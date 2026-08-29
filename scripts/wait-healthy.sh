#!/usr/bin/env bash
# Wait until a nisaba service container is healthy (or at least running).
# Usage: ./scripts/wait-healthy.sh <service> [attempts]
set -euo pipefail

svc="$1"
attempts="${2:-60}"
container="nisaba-${svc}-1"
for i in $(seq 1 "${attempts}"); do
    # Read the container state through python3 (docker inspect --format is not
    # usable from the justfile, whose template parser rejects `{{`).
    status=$(docker inspect "${container}" 2>/dev/null | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    s = d[0]["State"]
    print(s.get("Health", {}).get("Status", s.get("Status", "none")))
except Exception:
    print("none")
' 2>/dev/null || echo none)
    if [ "${status}" = "healthy" ]; then
        echo "${container}: healthy"
        exit 0
    fi
    sleep 3
done
echo "timed out waiting for ${container} to become healthy after ${i} attempts" >&2
# Same diagnosability as the sibling waiters (deploy/e2e-app.sh, nightly e2e):
# dump the container tail so a local e2e-suite failure is immediately readable.
docker logs --tail 50 "${container}" >&2 || true
exit 1
