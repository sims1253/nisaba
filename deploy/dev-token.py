#!/usr/bin/env python3
# PEP 723 inline metadata block below declares this as a standalone,
# dependency-free script (stdlib + the `openssl` CLI). uv reads the block,
# pins an interpreter, and runs the file in a throwaway environment
# (`uv run deploy/dev-token.py`); plain `python3 deploy/dev-token.py` also
# works if uv is unavailable. See deploy/README.md.
# /// script
# requires-python = ">=3.9"
# ///
"""Mint a Nisaba dev OIDC token + matching JWKS (stdlib + openssl only).

The app service verifies bearer tokens by signature against the JWKS read
**inline** from ``NISABA_OIDC_JWKS_JSON`` at startup (it does not fetch a
discovery URL today). To exercise the authenticated app endpoints in local dev
or e2e without a Keycloak round-trip, this script:

  1. generates a 2048-bit RSA keypair (openssl);
  2. builds a JWKS document from the public key (``kty/use/alg/kid/n/e``);
  3. mints an RS256 JWT signed by the private key with ``iss``/``aud``/``roles``
     matching what the app validates (``roles`` is top-level — the app reads
     ``roles``, not ``realm_access.roles``).

Output (into ``--out-dir``, default a fresh temp dir): ``key.pem``,
``jwks.json``, ``token``. The directory path is printed to stdout (single
line); human notes go to stderr. The app must be started with
``NISABA_OIDC_JWKS_JSON`` set to the contents of ``jwks.json``.

  uv run deploy/dev-token.py --issuer http://localhost:8090/realms/nisaba
  export NISABA_OIDC_JWKS_JSON="$(cat <outdir>/jwks.json)"
  curl -H "Authorization: Bearer $(cat <outdir>/token)" ...

DEV ONLY: the private key is unencrypted and ships no expiry hygiene. Never use
this token or key outside local development.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def b64url(data: bytes) -> str:
    """Base64url without padding (RFC 7515)."""
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def require_openssl() -> None:
    try:
        subprocess.run(
            ["openssl", "version"], capture_output=True, check=True
        )
    except (FileNotFoundError, subprocess.CalledProcessError) as exc:
        sys.stderr.write("[dev-token] openssl is required but unavailable: "
                         f"{exc}\n")
        sys.exit(127)


def gen_keypair(out_dir: Path) -> Path:
    key_pem = out_dir / "key.pem"
    subprocess.run(
        [
            "openssl", "genpkey",
            "-algorithm", "RSA",
            "-pkeyopt", "rsa_keygen_bits:2048",
            "-out", str(key_pem),
        ],
        check=True,
        capture_output=True,
    )
    os.chmod(key_pem, 0o600)
    return key_pem


def modulus_hex(key_pem: Path) -> str:
    proc = subprocess.run(
        ["openssl", "rsa", "-in", str(key_pem), "-noout", "-modulus"],
        check=True,
        capture_output=True,
        text=True,
    )
    line = proc.stdout.strip()
    prefix = "Modulus="
    if not line.startswith(prefix):
        raise RuntimeError(f"unexpected openssl modulus output: {line!r}")
    return line[len(prefix):]


def jwk_for_public_key(kid: str, key_pem: Path) -> dict:
    # RSA public exponent for openssl-generated keys is 65537 (0x010001).
    exp = 65537
    n = b64url(bytes.fromhex(modulus_hex(key_pem)))
    e = b64url(exp.to_bytes(3, "big"))
    return {
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": kid,
        "n": n,
        "e": e,
    }


def rs256_sign(signing_input: bytes, key_pem: Path) -> bytes:
    proc = subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", str(key_pem)],
        input=signing_input,
        capture_output=True,
        check=True,
    )
    return proc.stdout


def mint_jwt(kid: str, key_pem: Path, claims: dict) -> str:
    header = {"alg": "RS256", "typ": "JWT", "kid": kid}
    segment = lambda obj: b64url(  # noqa: E731 (local, short-lived)
        json.dumps(obj, separators=(",", ":")).encode("utf-8")
    )
    signing_input = f"{segment(header)}.{segment(claims)}".encode("ascii")
    signature = b64url(rs256_sign(signing_input, key_pem))
    return f"{signing_input.decode('ascii')}.{signature}"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Mint a Nisaba dev OIDC token + JWKS (openssl only).",
    )
    parser.add_argument(
        "--issuer",
        default=os.environ.get(
            "NISABA_OIDC_ISSUER", "http://localhost:8090/realms/nisaba"
        ),
        help="iss claim (default: $NISABA_OIDC_ISSUER)",
    )
    parser.add_argument(
        "--audience",
        default=os.environ.get("NISABA_OIDC_AUDIENCE", "nisaba"),
        help="aud claim (default: $NISABA_OIDC_AUDIENCE, usually 'nisaba')",
    )
    parser.add_argument(
        "--subject", default="dev@nisaba.local", help="sub claim"
    )
    parser.add_argument(
        "--roles",
        default="author,reviewer",
        help="comma-separated top-level roles claim",
    )
    parser.add_argument(
        "--ttl", type=int, default=3600, help="token lifetime in seconds"
    )
    parser.add_argument("--kid", default="nisaba-dev-key", help="JWK kid")
    parser.add_argument(
        "--out-dir", help="output directory (default: a fresh temp dir)"
    )
    args = parser.parse_args()

    require_openssl()
    out_dir = Path(args.out_dir) if args.out_dir else Path(
        tempfile.mkdtemp(prefix="nisaba-dev-token-")
    )
    out_dir.mkdir(parents=True, exist_ok=True)

    key_pem = gen_keypair(out_dir)
    jwk = jwk_for_public_key(args.kid, key_pem)
    jwks = {"keys": [jwk]}
    (out_dir / "jwks.json").write_text(
        json.dumps(jwks, separators=(",", ":")), encoding="utf-8"
    )

    now = int(time.time())
    roles = [r.strip() for r in args.roles.split(",") if r.strip()]
    claims = {
        "sub": args.subject,
        "iss": args.issuer,
        "aud": [args.audience],
        "azp": "nisaba-web",
        "iat": now,
        "exp": now + args.ttl,
        "roles": roles,
        "realm_access": {"roles": roles},
        "preferred_username": args.subject,
        "email": args.subject,
    }
    token = mint_jwt(args.kid, key_pem, claims)
    (out_dir / "token").write_text(token, encoding="utf-8")

    sys.stderr.write(
        f"[dev-token] wrote key.pem, jwks.json, token to {out_dir}\n"
        "[dev-token] start the app with: "
        f'NISABA_OIDC_JWKS_JSON="$(cat {out_dir}/jwks.json)"\n'
        f"[dev-token] bearer token: {out_dir}/token\n"
    )
    # Single-line stdout: the output directory (for scripts to consume).
    print(out_dir)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
