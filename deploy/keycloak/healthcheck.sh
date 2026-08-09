#!/usr/bin/env bash
# Keycloak readiness probe — no curl/wget needed (the Keycloak image ships only
# bash + java). Performs a real HTTP GET against the management health endpoint
# using bash's /dev/tcp.
#
# Mounted read-only into the keycloak container and invoked by the Compose
# HEALTHCHECK. Exits 0 only when /health/ready returns 200.
set -eu

MGMT_HOST="127.0.0.1"
MGMT_PORT="${KC_HTTP_MANAGEMENT_PORT:-9000}"

exec 3<>"/dev/tcp/${MGMT_HOST}/${MGMT_PORT}" || { echo "no mgmt port"; exit 1; }
printf 'GET /health/ready HTTP/1.0\r\nHost: %s\r\n\r\n' "${MGMT_HOST}" >&3
status="$(head -n1 <&3 || true)"
exec 3>&-

case "${status}" in
    *" 200 "*) echo "keycloak ready"; exit 0 ;;
    *) echo "keycloak not ready: ${status}"; exit 1 ;;
esac
