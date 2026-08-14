#!/usr/bin/env bash
# Validate docker-compose.yml resolves cleanly using .env.example as the env
# source, WITHOUT touching the working-tree .env.
#
# Copies .env.example to a temporary file and runs `docker compose config`
# against it for both the infra profile and the `app` profile. Exits non-zero
# if either fails to interpolate/validate. No containers are started.
#
# This is the CI gate (and `just compose-validate`). For validating your local
# .env instead, run:  docker compose config -q && docker compose --profile app config -q
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_EXAMPLE="$ROOT/.env.example"
if [ ! -f "$ENV_EXAMPLE" ]; then
    echo "validate-compose: .env.example not found at $ENV_EXAMPLE" >&2
    exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
    echo "validate-compose: docker is required" >&2
    exit 1
fi

# Temp env file — never writes to the repo's .env.
TMP_ENV="$(mktemp)"
trap 'rm -f "$TMP_ENV"' EXIT
cp "$ENV_EXAMPLE" "$TMP_ENV"

echo "[validate-compose] infra profile (default)"
docker compose --env-file "$TMP_ENV" config -q

echo "[validate-compose] app profile (--profile app)"
docker compose --env-file "$TMP_ENV" --profile app config -q

# `cargo chef cook` runs before the full source tree is copied into the Rust
# builder. Local path patches must therefore be copied into that stage first.
vendor_copy_line="$(grep -n '^COPY vendor/im vendor/im$' deploy/Dockerfile.rust | cut -d: -f1)"
chef_cook_line="$(grep -n 'cargo chef cook --release' deploy/Dockerfile.rust | head -n1 | cut -d: -f1)"
if [ -z "$vendor_copy_line" ] || [ -z "$chef_cook_line" ] || [ "$vendor_copy_line" -ge "$chef_cook_line" ]; then
    echo "validate-compose: deploy/Dockerfile.rust must copy vendor/im before cargo chef cook" >&2
    exit 1
fi

echo "[validate-compose] OK"
