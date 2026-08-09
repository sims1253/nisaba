#!/usr/bin/env bash
# End-to-end smoke of the FULL Nisaba stack (infra + app profile) against a
# throwaway project, then tear everything down.
#
# What it checks (the integration the infra tier alone cannot prove):
#   * `docker compose --profile app up -d --build` brings ALL seven services
#     (postgres, minio, keycloak, app, sync, compile, web) to healthy.
#   * The OIDC path is real, not stubbed: deploy/dev-token.py mints an RSA key +
#     JWKS + JWT; the JWKS is injected as NISABA_OIDC_JWKS_JSON and the app
#     verifies the dev token BY SIGNATURE (iss/aud/kid/alg), not a client secret.
#   * web (nginx) serves /healthz and the SPA is built.
#   * sync completes a real HELLO handshake at GET /sync/{doc_id} — not just the
#     101 upgrade. The dev stack configures sync with no OIDC issuer/JWKS, which
#     means deny-all, so the assertion is that the relay answers a typed ERROR
#     frame: proof the fail-closed path runs rather than silently admitting a
#     peer. (Asserting a WELCOME needs sync pointed at a reachable JWKS URL for
#     the same key the token was minted with; the dev compose does not wire one.)
#   * The app half of the authorize loop is exercised for real: a project →
#     a path-addressed document is created, then POST /internal/sync/authorize
#     with the machine token resolves that document to the creator's role.
#   * A compile→PDF round trip succeeds: create a throwaway project, then
#     POST /api/compile with the dev bearer token → a non-null pdf_base64.
#     (compile is internal-only on svc-net; app calls it with the shared
#     NISABA_COMPILE_TOKEN. The probe hits the app's real /api/compile route.)
#
# Side-effect-free: never reads/writes the repo's .env, uses a unique
# COMPOSE_PROJECT_NAME, mints its own dev key into a temp dir, and removes its
# own volumes + images containers on exit.
#
# Heavy: this BUILDS the app-profile images (app/sync/compile/web). For the
# lightweight infra-only check use `just smoke`. See deploy/README.md.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_EXAMPLE="$ROOT/.env.example"
if [ ! -f "$ENV_EXAMPLE" ]; then
    echo "e2e: .env.example not found at $ENV_EXAMPLE" >&2
    exit 1
fi

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "e2e: '$1' is required" >&2
        exit 1
    fi
}
need docker
need jq
need uv       # runs deploy/dev-token.py + deploy/sync-handshake.py (PEP 723)
need openssl  # dev-token.py shells out to the openssl CLI

# Temp env + throwaway project so we never touch real volumes/state.
TMP_ENV="$(mktemp)"
TOKEN_DIR="$(mktemp -d -t nisaba-e2e-token-XXXXXX)"
PROJECT="nisaba-e2e-$$"
grep -vE '^(NISABA_SYNC_OIDC_ISSUER|NISABA_SYNC_OIDC_AUDIENCE|NISABA_SYNC_OIDC_JWKS_URL)=' "$ENV_EXAMPLE" > "$TMP_ENV"

COMPOSE=(docker compose --env-file "$TMP_ENV" -p "$PROJECT")

cleanup() {
    local rc=$?
    echo "[e2e] tearing down (project=$PROJECT)"
    "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$TOKEN_DIR" "$TMP_ENV"
    exit "$rc"
}
trap cleanup EXIT

# ---- Mint the dev token + inject its JWKS into the app's environment --------
# dev-token.py prints the output dir on stdout (single line); notes → stderr.
ISSUER="$(grep -E '^NISABA_OIDC_ISSUER=' "$TMP_ENV" | cut -d= -f2- || true)"
ISSUER="${ISSUER:-http://localhost:8090/realms/nisaba}"
echo "[e2e] minting dev OIDC token (issuer=${ISSUER})"
TOKEN_DIR="$(uv run deploy/dev-token.py --issuer "$ISSUER" \
    --out-dir "$TOKEN_DIR" 2>/dev/null)"
# Export so compose's ${NISABA_OIDC_JWKS_JSON:-} interpolation picks it up.
# (Shell export is more robust for a JSON value than embedding it in .env.)
NISABA_OIDC_JWKS_JSON="$(cat "${TOKEN_DIR}/jwks.json")"
export NISABA_OIDC_JWKS_JSON
TOKEN="$(cat "${TOKEN_DIR}/token")"

# ---- Build + bring up the full stack ---------------------------------------
echo "[e2e] building + bringing up the full stack (project=$PROJECT)"
"${COMPOSE[@]}" up -d --build --quiet-pull

# Wait until a service's container healthcheck reports "healthy".
wait_healthy() {
    local svc="$1" tries="${2:-120}" delay="${3:-3}"
    local i cid status
    for ((i = 1; i <= tries; i++)); do
        cid="$("${COMPOSE[@]}" ps -q "$svc" 2>/dev/null || true)"
        if [ -n "$cid" ]; then
            status="$(docker inspect -f '{{.State.Health.Status}}' "$cid" 2>/dev/null || echo none)"
            if [ "$status" = "healthy" ]; then
                echo "[e2e] $svc healthy (after ${i} polls)"
                return 0
            fi
        fi
        sleep "$delay"
    done
    echo "[e2e] $svc did not become healthy in time" >&2
    "${COMPOSE[@]}" logs --tail=40 "$svc" >&2 || true
    return 1
}

echo "[e2e] waiting for health (postgres/minio/keycloak/app/sync/compile/web)"
wait_healthy postgres 60
wait_healthy minio 60
wait_healthy keycloak 120
wait_healthy app 150
wait_healthy sync 120
wait_healthy compile 120
wait_healthy web 90

# Probes run INSIDE the running `app` container (it ships curl and sits on every
# network), so the smoke stays portable in Docker-in-Docker where 127.0.0.1 host
# bindings are not reachable from the outer shell. See deploy/smoke.sh.

# ---- web (nginx): liveness + SPA served ------------------------------------
echo "[e2e] web /healthz (nginx liveness)"
"${COMPOSE[@]}" exec -T app curl -fsS http://web:8080/healthz >/dev/null
echo "[e2e] web serves the SPA index (nginx → built dist)"
"${COMPOSE[@]}" exec -T app curl -fsS http://web:8080/ \
    | grep -qi '<html' || { echo "[e2e] web did not serve an HTML index" >&2; exit 1; }

# ---- compile → PDF (app /api/compile → compile over svc-net) ----------------
# 1) create a throwaway project (needs the Author role the dev token carries);
#    create_project auto-grants the creator Owner membership.
echo "[e2e] creating throwaway project (app POST /projects)"
proj_json="$(printf '%s' '{"name":"e2e-smoke"}' \
    | "${COMPOSE[@]}" exec -T app curl -sS -X POST http://127.0.0.1:8080/projects \
        -H "Authorization: Bearer ${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d @-)"
proj_id="$(printf '%s' "$proj_json" | jq -r '.id // empty')"
[ -n "$proj_id" ] \
    || { echo "[e2e] project creation failed; response: ${proj_json}" >&2; exit 1; }
echo "[e2e] created project ${proj_id}"

# 2) compile it. A minimal Typst source compiles to a PDF; assert pdf_base64.
payload="$(jq -nc --arg pid "$proj_id" \
    '{project_id:$pid, entry:"main.typ",
      sources:{"main.typ":"#set page(width: 10cm, height: auto)\nHello e2e"},
      mode:"full", view:"baseline"}')"
echo "[e2e] compile → PDF (app POST /api/compile → compile)"
compile_json="$(printf '%s' "$payload" \
    | "${COMPOSE[@]}" exec -T app curl -sS -X POST http://127.0.0.1:8080/api/compile \
        -H "Authorization: Bearer ${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d @-)"
pdf="$(printf '%s' "$compile_json" | jq -r '.pdf_base64 // empty')"
[ -n "$pdf" ] \
    || { echo "[e2e] compile returned no pdf_base64; response: ${compile_json}" >&2; exit 1; }
echo "[e2e] compiled PDF ok (build_id=$(printf '%s' "$compile_json" | jq -r '.build_id'))"

# ---- a real document, so sync has something to authorize --------------------
api() {
    local method="$1" path="$2"
    if [ "$#" -ge 3 ]; then
        printf '%s' "$3" | "${COMPOSE[@]}" exec -T app curl -sS -X "$method" \
            "http://127.0.0.1:8080${path}" \
            -H "Authorization: Bearer ${TOKEN}" -H 'Content-Type: application/json' -d @-
    else
        "${COMPOSE[@]}" exec -T app curl -sS -X "$method" "http://127.0.0.1:8080${path}" \
            -H "Authorization: Bearer ${TOKEN}"
    fi
}

echo "[e2e] creating document under project ${proj_id}"
doc_json="$(api POST "/projects/${proj_id}/documents" \
    '{"path":"main.typ","title":"Main","body":"= Hello","data":{}}')"
DOC="$(printf '%s' "$doc_json" | jq -r '.id // empty')"
[ -n "$DOC" ] || { echo "[e2e] document creation failed; response: ${doc_json}" >&2; exit 1; }
echo "[e2e] created document ${DOC}"

# ---- app half of the sync authorize loop ------------------------------------
# This is the call sync makes for every handshake. It must resolve the document
# to the caller's project membership; the project creator is auto-granted Owner,
# which maps to the "author" role.
AUTHZ_TOKEN="$(grep -E '^NISABA_SYNC_AUTHZ_TOKEN=' "$TMP_ENV" | cut -d= -f2- || true)"
[ -n "$AUTHZ_TOKEN" ] || { echo "[e2e] .env.example has no NISABA_SYNC_AUTHZ_TOKEN" >&2; exit 1; }
echo "[e2e] app POST /internal/sync/authorize (the call sync makes per handshake)"
subject="$(printf '%s' "$TOKEN" | cut -d. -f2 \
    | { read -r p; printf '%s' "$p$(printf '=%.0s' $(seq $(( (4 - ${#p} % 4) % 4 ))))"; } \
    | base64 -d 2>/dev/null | jq -r '.sub')"
authz_json="$(printf '{"document":"%s","subject":"%s"}' "$DOC" "$subject" \
    | "${COMPOSE[@]}" exec -T app curl -sS -X POST http://127.0.0.1:8080/internal/sync/authorize \
        -H "Authorization: Bearer ${AUTHZ_TOKEN}" -H 'Content-Type: application/json' -d @-)"
role="$(printf '%s' "$authz_json" | jq -r '.role // empty')"
[ "$role" = "author" ] \
    || { echo "[e2e] authorize did not return the author role; response: ${authz_json}" >&2; exit 1; }
echo "[e2e] authorize ok (subject=${subject} role=${role})"

# ---- sync: a real HELLO handshake, not just the upgrade ---------------------
# Runs from the host against the published sync port. The dev stack leaves sync's
# OIDC variables empty, which is deny-all, so a typed ERROR frame is the correct
# and asserted outcome — the fail-closed path actually executing. Point sync at a
# JWKS URL serving this token's key to turn this into `--expect welcome`.
SYNC_PORT="$(grep -E '^SYNC_HOST_PORT=' "$TMP_ENV" | cut -d= -f2- || true)"
SYNC_PORT="${SYNC_PORT:-8101}"
echo "[e2e] sync HELLO handshake (ws://127.0.0.1:${SYNC_PORT}/sync/${DOC})"
uv run deploy/sync-handshake.py \
    --url "ws://127.0.0.1:${SYNC_PORT}/sync/${DOC}" \
    --token "$TOKEN" \
    --expect error \
    || { echo "[e2e] sync handshake did not complete as expected" >&2; exit 1; }

echo "[e2e] OK"
