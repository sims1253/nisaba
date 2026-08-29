#!/usr/bin/env bash
# Fetch the dev Keycloak realm's JWKS document (what the app service needs in
# NISABA_OIDC_JWKS_JSON), starting the infra tier first if Keycloak is not up.
# Prints the JWKS JSON on stdout. Shared by the justfile `up-all` and `e2e-up`
# recipes; extracted from e2e-up so the probe/poll logic exists exactly once.
set -euo pipefail

# KEYCLOAK_HTTP_PORT is normally already in the environment (just's
# dotenv-load exports .env values, and a caller-provided value wins, matching
# compose interpolation precedence). The .env fallback keeps the script
# correct under `just --no-dotenv` and when run by hand. A missing .env is not
# an error — the default port matches the compose/.env.example default.
if [ -z "${KEYCLOAK_HTTP_PORT:-}" ] && [ -f .env ]; then
    # shellcheck disable=SC1091
    set -a && . ./.env && set +a
fi
kc_port="${KEYCLOAK_HTTP_PORT:-8090}"
certs_url="http://127.0.0.1:${kc_port}/realms/nisaba/protocol/openid-connect/certs"

# Fetch the JWKS from the running Keycloak (or start infra first and wait).
if ! curl -fsS "$certs_url" >/dev/null 2>&1; then
    # Both call sites capture this script's stdout as the JWKS value, and the
    # app aborts startup on a JWKS that is not valid JSON — so the compose
    # bring-up's stdout goes to stderr unconditionally. Compose sends `up -d`
    # status lines to stderr by default, but honors COMPOSE_STATUS_STDOUT; a
    # truthy value in the operator's environment would otherwise pollute the
    # captured JWKS on the cold-start path.
    docker compose up -d 1>&2
    echo "[fetch-jwks] waiting for Keycloak on port ${kc_port}..." >&2
    for _ in $(seq 1 60); do
        if curl -fsS "$certs_url" >/dev/null 2>&1; then
            break
        fi
        sleep 5
    done
fi
if ! jwks="$(curl -fsS "$certs_url")"; then
    echo "[fetch-jwks] Keycloak did not come up (no JWKS at ${certs_url})" >&2
    exit 1
fi
printf '%s\n' "$jwks"
