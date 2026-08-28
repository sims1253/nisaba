#!/usr/bin/env bash
# Smoke-test the Nisaba INFRASTRUCTURE tier (Postgres + SeaweedFS + Keycloak) end
# to end against a throwaway project + temp env, then tear everything down.
#
# What it checks:
#   * `docker compose up -d` brings the three infra services to healthy.
#   * Postgres answers `pg_isready` as the least-privilege role.
#   * SeaweedFS answers /healthz on the host port.
#   * Keycloak serves the realm (issuer) on the host port.
#
# What it deliberately does NOT check:
#   * The `app` profile images (app/sync/compile/web) — those need the service
#     binaries and their /healthz contract. Use `just up-all` for that.
#
# Side-effect-free: it never reads or writes the repo's .env, uses a unique
# COMPOSE_PROJECT_NAME, and removes its own volumes on exit.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_EXAMPLE="$ROOT/.env.example"
if [ ! -f "$ENV_EXAMPLE" ]; then
    echo "smoke: .env.example not found at $ENV_EXAMPLE" >&2
    exit 1
fi
if ! command -v docker >/dev/null 2>&1; then
    echo "smoke: docker is required" >&2
    exit 1
fi

# Temp env + throwaway project name so we never touch real volumes/state.
TMP_ENV="$(mktemp)"
cp "$ENV_EXAMPLE" "$TMP_ENV"

PROJECT="nisaba-smoke-$$"
# All docker compose invocations go through this prefix.
COMPOSE=(docker compose --env-file "$TMP_ENV" -p "$PROJECT")

cleanup() {
    echo "[smoke] tearing down (project=$PROJECT)"
    "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
    rm -f "$TMP_ENV"
}
trap cleanup EXIT

echo "[smoke] bringing up the infra tier (project=$PROJECT)"
"${COMPOSE[@]}" up -d --quiet-pull

# Wait until a service's container healthcheck reports "healthy".
wait_healthy() {
    local svc="$1" tries="${2:-90}" delay="${3:-2}"
    local i cid status
    for ((i = 1; i <= tries; i++)); do
        cid="$("${COMPOSE[@]}" ps -q "$svc" 2>/dev/null || true)"
        if [ -n "$cid" ]; then
            status="$(docker inspect -f '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo none)"
            if [ "$status" = "healthy" ]; then
                echo "[smoke] $svc healthy (after ${i} polls)"
                return 0
            fi
        fi
        sleep "$delay"
    done
    echo "[smoke] $svc did not become healthy in time" >&2
    return 1
}

echo "[smoke] waiting for health (postgres/seaweedfs/keycloak)"
wait_healthy postgres 60
wait_healthy seaweedfs 60
wait_healthy keycloak 90

# Postgres role/db for the pg_isready probe (from the resolved env).
PG_USER="$(grep -E '^NISABA_DB_USER=' "$TMP_ENV" | cut -d= -f2- || true)"; PG_USER="${PG_USER:-nisaba_app}"
PG_DB="$(grep -E '^NISABA_DB_NAME=' "$TMP_ENV" | cut -d= -f2- || true)"; PG_DB="${PG_DB:-nisaba}"

# Probes run INSIDE the running containers (via `compose exec`) rather than
# against the published host ports. This keeps the smoke test portable: in some
# environments (e.g. Docker-in-Docker) the 127.0.0.1 host bindings are not
# reachable from the outer shell even though the services are healthy.

# Postgres: pg_isready as the least-privilege app role/db.
echo "[smoke] postgres pg_isready (as ${PG_USER}/${PG_DB})"
"${COMPOSE[@]}" exec -T postgres pg_isready -U "$PG_USER" -d "$PG_DB" >/dev/null

# SeaweedFS: liveness probe (the image ships curl).
echo "[smoke] seaweedfs /healthz"
"${COMPOSE[@]}" exec -T seaweedfs curl -fsS http://127.0.0.1:8333/healthz >/dev/null

# Keycloak: the mounted readiness script probes /health/ready, and we also
# assert the realm was imported by hitting GET /realms/nisaba on the main port
# (the keycloak image has no curl, so we use bash /dev/tcp).
echo "[smoke] keycloak readiness + realm import"
"${COMPOSE[@]}" exec -T keycloak bash /usr/local/bin/kc-healthcheck.sh >/dev/null
"${COMPOSE[@]}" exec -T keycloak bash -c \
    'exec 3<>/dev/tcp/127.0.0.1/8090 || exit 1; printf "GET /realms/nisaba HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n" >&3; head -n1 <&3 | grep -q " 200 "'

echo "[smoke] OK"
