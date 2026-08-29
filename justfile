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
# The app verifies tokens against the INLINE JWKS in NISABA_OIDC_JWKS_JSON, and
# .env.example ships it empty (deny-all: every user-token API call 401s). When
# the variable is unset/empty here, fetch the dev realm's JWKS from Keycloak
# (scripts/fetch-jwks.sh, which starts infra first if needed) and inject it into
# this invocation — a value the operator set explicitly (.env or the
# environment, with the usual env-wins precedence) is never clobbered.
up-all:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${NISABA_OIDC_JWKS_JSON:-}" ]; then
        echo "[up-all] NISABA_OIDC_JWKS_JSON empty; fetching the dev realm JWKS..."
        export NISABA_OIDC_JWKS_JSON="$(./scripts/fetch-jwks.sh)"
    else
        echo "[up-all] NISABA_OIDC_JWKS_JSON set; using it as-is."
    fi
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

# Build the OPTIONAL in-browser compile WASM artifacts (issue #20 stage 2c)
# into web/src/wasm-generated/ (gitignored — never committed; the compile
# module is tens of megabytes, mostly embedded typst fonts). Without these
# files the web client builds and runs exactly as shipped: compiles go to the
# server, and an opted-in tab (localStorage nisaba.compilePath=wasm) logs one
# line saying why it fell back. With them, an opted-in tab compiles in a Web
# Worker instead (docs/architecture.md §4.1).
#
# Prerequisites (NOT needed for any other web work): the wasm32 target
# (`rustup target add wasm32-unknown-unknown`) and wasm-bindgen-cli matching
# the crates' pinned wasm-bindgen 0.2.127
# (`cargo install wasm-bindgen-cli --version 0.2.127`) — a mismatched CLI
# fails against the generated glue, so the version is checked here.
wasm-web:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v wasm-bindgen >/dev/null || { echo "wasm-bindgen-cli is required: cargo install wasm-bindgen-cli --version 0.2.127"; exit 1; }
    if [ "$(wasm-bindgen --version | awk '{print $2}')" != "0.2.127" ]; then
        echo "wasm-bindgen-cli 0.2.127 is required (matches the crates' wasm-bindgen); found: $(wasm-bindgen --version)"
        exit 1
    fi
    rustup target list --installed | grep -q '^wasm32-unknown-unknown$' || { echo "missing target: rustup target add wasm32-unknown-unknown"; exit 1; }
    cargo build -p nisaba-core-wasm -p nisaba-compile-wasm --release --locked --target wasm32-unknown-unknown
    rm -rf web/src/wasm-generated
    mkdir -p web/src/wasm-generated
    wasm-bindgen --target web --out-dir web/src/wasm-generated "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/nisaba_core_wasm.wasm"
    wasm-bindgen --target web --out-dir web/src/wasm-generated "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/nisaba_compile_wasm.wasm"
    echo "Built in-browser compile artifacts (gitignored):"
    ls -lh web/src/wasm-generated

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
# The web image must be rebuilt with OIDC env vars baked in. Always injects the
# live dev realm JWKS via scripts/fetch-jwks.sh (same helper as `up-all`),
# overriding any NISABA_OIDC_JWKS_JSON from .env: the tests must validate
# against the Keycloak that is actually running.
e2e-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -f .env ]; then
        echo "[e2e] .env not found. Copy .env.example and generate secrets first." >&2
        exit 1
    fi
    NISABA_OIDC_JWKS_JSON="$(./scripts/fetch-jwks.sh)" docker compose --profile app up -d --build

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
        ./scripts/wait-healthy.sh "$svc"
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
