# =============================================================================
# Nisaba — developer task runner (https://github.com/casey/just)
# =============================================================================
# `just` with no argument lists all recipes. Recipes that touch files owned by
# sibling implementation streams (tools/verify.sh, web build) are tolerant: they
# run the real check when the prerequisite exists and skip cleanly otherwise.
# =============================================================================

# Project root (where this justfile lives).
export CARGO_TARGET_DIR := env_var_or_default("CARGO_TARGET_DIR", "target")

# Load .env (same file compose interpolates) so recipes like psql, s3, and
# migrate see NISABA_DB_USER / NISABA_S3_ADMIN_* / the NISABA_DB_* parts
# `migrate` builds its DATABASE_URL from. Values already present in the
# environment win, matching compose interpolation precedence.
# A missing .env is not an error.
set dotenv-load := true

# Default: list available recipes.
default:
    @just --list --unsorted

# ---------- Infra (Docker Compose) -----------------------------------------

# Bring up the local infrastructure tier (Postgres + SeaweedFS + Keycloak + init).
up:
    docker compose up -d

# Bring up everything, including application service images (re)built on demand.
up-all:
    docker compose --profile app up -d --build

# Stop and remove containers (named volumes are preserved).
down:
    docker compose --profile app down

# Stop and remove containers AND named volumes (postgres/seaweedfs/keycloak data).
down-volumes:
    docker compose --profile app down -v

# Tail all services, or follow specific ones: `just logs`, `just logs app`,
# `just logs app sync`. Uses a splat parameter so no argument expands to
# nothing — an interpolated empty string would make compose fail with
# "no such service".
logs *svc:
    docker compose --profile app logs -f {{svc}}

# Validate the resolved compose config against the current .env.
compose-check:
    docker compose config -q
    docker compose --profile app config -q

# Validate compose against .env.example via a TEMP env (no working-tree .env
# pollution). This is what CI runs. See deploy/validate-compose.sh.
compose-validate:
    ./deploy/validate-compose.sh

# Bring up the infra tier (Postgres+SeaweedFS+Keycloak) against a throwaway project,
# probe health + the Keycloak realm import, then tear down. See deploy/smoke.sh.
smoke:
    ./deploy/smoke.sh

# Full end-to-end smoke of the WHOLE stack (infra + app profile): builds the
# app/sync/compile/web images, mints a dev OIDC token (deploy/dev-token.py via
# uv), injects its JWKS, then probes web/nginx, a sync WebSocket upgrade, and a
# compile→PDF round trip — all against a throwaway project. See deploy/e2e-app.sh.
# (Heavy: it builds the app-profile images. Use `just smoke` for infra-only.)
e2e:
    ./deploy/e2e-app.sh

# ---------- Databases / object store ---------------------------------------

# Open a psql session to the Nisaba database (as the least-privilege role).
psql:
    docker compose exec postgres psql -U "${NISABA_DB_USER}" -d "${NISABA_DB_NAME}"

# Open a psql session as the maintenance superuser.
psql-admin:
    docker compose exec postgres psql -U "${POSTGRES_USER}" -d "${POSTGRES_DB}"

# Run migrations via sqlx-cli against the embedded migrations/ directory
# (the app stream owns it; sqlx-cli must be installed separately).
migrate dir='migrations':
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -d "{{dir}}" ]; then
        echo "[migrate] no '{{dir}}' directory yet (owned by the app stream); nothing to do."
        exit 0
    fi
    if command -v sqlx >/dev/null 2>&1; then
        # DATABASE_URL is synthesised from the NISABA_DB_* parts (the single
        # source compose also uses) against the host-published port — .env
        # deliberately ships no literal URL duplicating the password.
        # :? — a missing NISABA_DB_PASSWORD fails here, loudly, not in sqlx.
        # NB: a password with URI-reserved characters must be percent-encoded,
        # exactly like in the compose synthesis.
        export DATABASE_URL="postgres://${NISABA_DB_USER:-nisaba_app}:${NISABA_DB_PASSWORD:?NISABA_DB_PASSWORD is required}@127.0.0.1:${POSTGRES_HOST_PORT:-5433}/${NISABA_DB_NAME:-nisaba}"
        sqlx migrate run --source "{{dir}}"
    else
        echo "[migrate] sqlx-cli not installed; install with: cargo install sqlx-cli --no-default-features --features postgres"
        exit 1
    fi

# Open an AWS CLI shell against the local SeaweedFS S3 endpoint.
# Usage: just s3 ls, just s3 ls s3://nisaba-blobs, etc.
# Digest-pinned to match docker-compose.yml (seaweedfs-init service).
s3 *ARGS:
    docker run --rm -i --network nisaba_obj-net \
        -e AWS_ACCESS_KEY_ID="${NISABA_S3_ADMIN_KEY}" \
        -e AWS_SECRET_ACCESS_KEY="${NISABA_S3_ADMIN_SECRET}" \
        -e AWS_ENDPOINT_URL=http://seaweedfs:8333 \
        amazon/aws-cli:2.36.20@sha256:8af59c0d96b104000cce4f11e211c06385240d72c515198159041f13ebe459fa \
        s3 {{ARGS}}

# ---------- Rust workspace -------------------------------------------------

check:
    cargo check --workspace --all-targets

test:
    # DATABASE_URL in the environment (e.g. an older .env that still ships a
    # literal one) points at the Docker-internal hostname for the app
    # container. Drop it so the live_api tests fall back to their own .env
    # parse, which builds the host-reachable 127.0.0.1:{POSTGRES_HOST_PORT}
    # URL — otherwise dotenv-load would make them skip "no reachable database".
    env -u DATABASE_URL cargo test --workspace --all-targets
    env -u DATABASE_URL cargo test --workspace --doc

# Fast feedback build of all binaries.
build:
    cargo build --workspace --release

# Postgres-backed live API integration tests for the app service (the compose
# stack must be running; skips cleanly when no database is reachable).
# env -u DATABASE_URL: same reason as `test` — an exported value is likely
# container-facing.
test-live:
    env -u DATABASE_URL cargo test -p nisaba-app --test live_api

clippy:
    cargo clippy --workspace --all-targets

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# ---------- Web (bun, matching tools/) --------------------------------------

# Install from the committed bun.lock (fails if the lockfile would change).
web-install:
    bun install --frozen-lockfile

web-dev:
    bun install --frozen-lockfile && cd web && bun run dev

web-build:
    bun install --frozen-lockfile && cd web && bun run build

web-test:
    bun install --frozen-lockfile && cd web && bun run test

web-lint:
    bun install --frozen-lockfile && cd web && bun run lint

# ---------- Supply chain ---------------------------------------------------

# Run cargo-deny (licenses, advisories, bans, sources). The tool reads the
# committed Cargo.lock and never regenerates it. cargo-deny >=0.20 is required
# because older releases cannot parse CVSS 4.0 advisories.
deny:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! cargo deny --version >/dev/null 2>&1; then
        echo "[deny] cargo-deny >=0.20 required: cargo install cargo-deny --version 0.20.2 --locked" >&2
        exit 1
    fi
    cargo deny check

# Run cargo-audit with the reviewed exceptions documented in
# docs/dependency-security.md. cargo-audit reads Cargo.lock without modifying it.
audit:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-audit >/dev/null 2>&1 || cargo audit --version >/dev/null 2>&1; then
        ./scripts/cargo-audit.sh
    else
        echo "[audit] cargo-audit not installed: cargo install cargo-audit" >&2
        exit 1
    fi

# ---------- E2E (browser) --------------------------------------------------

# Start the full stack for e2e testing (requires .env with generated secrets).
# The web image must be rebuilt with OIDC env vars baked in.
e2e-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f .env ]; then
        echo "[e2e] .env not found. Copy .env.example and generate secrets first." >&2
        exit 1
    fi
    # Read KEYCLOAK_HTTP_PORT from .env (the same file compose interpolates), so
    # a non-default port works here too. dotenv-load already exports it; the
    # explicit sourcing keeps the script correct when just is invoked with
    # --no-dotenv.
    # shellcheck disable=SC1091
    set -a && . ./.env && set +a
    KC_PORT="${KEYCLOAK_HTTP_PORT:-8090}"
    CERTS_URL="http://127.0.0.1:${KC_PORT}/realms/nisaba/protocol/openid-connect/certs"
    # Fetch JWKS from the running Keycloak (or start infra first).
    if ! curl -fsS "$CERTS_URL" >/dev/null 2>&1; then
        docker compose up -d
        echo "[e2e] waiting for Keycloak on port ${KC_PORT}..."
        for i in $(seq 1 60); do
            if curl -fsS "$CERTS_URL" >/dev/null 2>&1; then
                break
            fi
            sleep 5
        done
    fi
    JWKS="$(curl -fsS "$CERTS_URL")"
    NISABA_OIDC_JWKS_JSON="$JWKS" docker compose --profile app up -d --build

# Run Playwright e2e tests against a running stack.
e2e-test:
    #!/usr/bin/env bash
    set -euo pipefail
    cd web && bunx playwright install chromium --with-deps 2>/dev/null || true
    bunx playwright test --config e2e/

# Full e2e lifecycle: start stack, run tests, tear down.
e2e-suite: e2e-up
    #!/usr/bin/env bash
    set -euo pipefail
    # Wait for all services to be healthy (helper lives in scripts/ because the
    # justfile template parser rejects the quoting needed inline).
    for svc in app sync compile web; do
        echo "[e2e] waiting for ${svc}..."
        ./scripts/wait-healthy.sh "$svc" || true
    done
    just e2e-test

# ---------- Tools (tolerant) -----------------------------------------------

# Run the tools verification suite (owned by the tools stream).
verify:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -x ./tools/verify.sh ]; then
        ./tools/verify.sh
    elif [ -f ./tools/verify.sh ]; then
        bash ./tools/verify.sh
    else
        echo "[verify] tools/verify.sh not found; nothing to do."
    fi

# ---------- Images ---------------------------------------------------------

# Build a single Rust service image. Usage: just image nisaba-compile
image service:
    docker build -f deploy/Dockerfile.rust -t "{{service}}" --build-arg "SERVICE={{service}}" .

image-web:
    docker build -f deploy/Dockerfile.web -t nisaba-web .

# ---------- Backups (see deploy/backup/ and docs/operations.md) ------------

backup:
    ./deploy/backup/backup.sh

restore file:
    ./deploy/backup/restore.sh "{{file}}"

# Structurally verify a backup snapshot WITHOUT restoring it (gzip + markers).
# Usage: just verify-backup artifacts/backups/<timestamp>
verify-backup dir:
    ./deploy/backup/verify.sh "{{dir}}"

# ---------- Cleanup --------------------------------------------------------

clean:
    cargo clean
    @echo "Note: docker volumes are preserved. Use 'just down-volumes' to remove data."

# Run a broad local check across everything that exists today.
# This mirrors what CI runs on every PR: Rust integration tests (sync, app,
# core), web vitest, tools, and all linters/formatters. Docker-based smoke
# tests (deploy/smoke.sh, deploy/e2e-app.sh) are NOT included — they validate
# infrastructure configuration, not application logic, and run separately via
# `just smoke` / `just e2e` or in CI only when infra files change.
ci-local: fmt-check clippy test deny audit verify web-install web-test web-lint web-build
    @echo "local CI checks complete"
