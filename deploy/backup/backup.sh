#!/usr/bin/env bash
# Back up the Nisaba data plane: Postgres (logical dump) + SeaweedFS (object sync).
#
#   - Postgres : pg_dump of the `nisaba` database as the least-privilege role.
#   - SeaweedFS: aws s3 sync of the nisaba-* buckets to a local dir.
#
# Tolerant by design: if a dependency (a bucket, the DB, the compose stack) is
# absent, that step is skipped with a clear message rather than aborting the
# whole backup. Production notes in docs/operations.md.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

# shellcheck disable=SC1091
[ -f .env ] && set -a && . ./.env && set +a

RETENTION_DAYS="${BACKUP_RETENTION_DAYS:-7}"
OUT_DIR="${BACKUP_LOCAL_DIR:-./artifacts/backups}"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
DEST="${OUT_DIR}/${TS}"

mkdir -p "${DEST}/postgres" "${DEST}/seaweedfs" "${DEST}/sync"
echo "[backup] -> ${DEST}"

# ---- Postgres ----
if docker compose ps postgres 2>/dev/null | grep -q "postgres"; then
    echo "[backup] dumping Postgres database '${NISABA_DB_NAME:-nisaba}'..."
    docker compose exec -T postgres \
        pg_dump --no-owner --no-privileges --clean --if-exists \
        -U "${NISABA_DB_USER:-nisaba_app}" "${NISABA_DB_NAME:-nisaba}" \
        > "${DEST}/postgres/nisaba.sql" 2>"${DEST}/postgres/pg_dump.stderr" \
        || { echo "[backup] WARN: pg_dump failed — see ${DEST}/postgres/pg_dump.stderr"; }
    gzip -f "${DEST}/postgres/nisaba.sql" 2>/dev/null || true
else
    echo "[backup] postgres container not running; skipping database dump."
fi

# ---- SeaweedFS ----
if docker compose ps seaweedfs 2>/dev/null | grep -q "seaweedfs"; then
    echo "[backup] syncing SeaweedFS buckets..."
    obj_net="$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' "$(docker compose ps -q seaweedfs)" 2>/dev/null | awk '{print $1}')"
    obj_net="${obj_net:-nisaba_obj-net}"
    if ! docker run --rm -i --network "$obj_net" --entrypoint /bin/sh         -e AWS_ACCESS_KEY_ID="${NISABA_S3_ADMIN_KEY}"         -e AWS_SECRET_ACCESS_KEY="${NISABA_S3_ADMIN_SECRET}"         -v "${DEST}/seaweedfs:/out:rw"         amazon/aws-cli:2.25.28 -c '
            export AWS_ENDPOINT_URL=http://seaweedfs:8333
            failed=0
            for b in '"${NISABA_S3_BUCKET_BLOBS:-nisaba-blobs}"' '"${NISABA_S3_BUCKET_OPLOG:-nisaba-oplog}"'; do
                if ! aws s3 sync --no-progress "s3://${b}" "/out/${b}"; then
                    echo "[backup] ERROR: sync of ${b} failed" >&2
                    failed=1
                fi
            done
            exit $failed
        '; then
        echo "[backup] ERROR: SeaweedFS sync step failed; snapshot is INCOMPLETE (no object storage)." >&2
        exit 1
    fi
else
    echo "[backup] seaweedfs container not running; skipping object sync."
fi

# ---- sync filesystem (op-log + snapshots) ----
# sync persists CRDT history to the sync-data volume (/data) today; the S3
# op-log bucket is the future integration surface (docs/operations.md §4).
if docker compose ps sync 2>/dev/null | grep -q "sync"; then
    echo "[backup] archiving sync filesystem (/data: op-log + snapshots)..."
    docker compose exec -T sync tar -czf - -C /data . \
        > "${DEST}/sync/sync.tar.gz" 2>"${DEST}/sync/tar.stderr" \
        || { echo "[backup] WARN: sync-fs tar failed — see ${DEST}/sync/tar.stderr"; }
else
    echo "[backup] sync container not running; skipping sync filesystem."
fi

# ---- Retention (local only) ----
echo "[backup] pruning local backups older than ${RETENTION_DAYS} days..."
find "${OUT_DIR}" -mindepth 1 -maxdepth 1 -type d -mtime "+${RETENTION_DAYS}" \
    -exec rm -rf {} \; 2>/dev/null || true

echo "[backup] done: ${DEST}"
