#!/usr/bin/env bash
# Restore Nisaba data from a backup directory produced by backup.sh.
#
#   Usage: just restore artifacts/backups/20240101T120000Z
#          (or)  ./deploy/backup/restore.sh path/to/snapshot
#
#   - Postgres : restores the gzip'd SQL dump (drop+recreate via --clean).
#   - MinIO    : mirrors the local bucket copies back into MinIO.
#   - sync fs  : extracts the op-log + snapshot tar back into the sync-data volume.
#
# This OVERWRITES current data. It is intended for local/dev recovery and
# disaster-recovery drills. Production restore procedure: docs/operations.md.
set -euo pipefail

if [ "$#" -lt 1 ]; then
    echo "usage: $0 <backup-snapshot-dir>" >&2
    exit 64
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
# shellcheck disable=SC1091
[ -f .env ] && set -a && . ./.env && set +a

SRC="$1"
if [ ! -d "$SRC" ]; then
    echo "restore: snapshot directory not found: $SRC" >&2
    exit 66
fi
SRC="$(cd "$SRC" && pwd)"
echo "[restore] <- ${SRC}"

# ---- Postgres ----
DUMP="${SRC}/postgres/nisaba.sql.gz"
if [ -f "$DUMP" ]; then
    echo "[restore] loading Postgres dump into '${NISABA_DB_NAME:-nisaba}'..."
    gunzip -c "$DUMP" | docker compose exec -T postgres \
        psql -v ON_ERROR_STOP=1 -U "${NISABA_DB_USER:-nisaba_app}" \
        -d "${NISABA_DB_NAME:-nisaba}" \
        || { echo "[restore] Postgres restore failed." >&2; exit 1; }
else
    echo "[restore] no Postgres dump at ${DUMP}; skipping database."
fi

# ---- MinIO ----
if [ -d "${SRC}/minio" ]; then
    echo "[restore] restoring MinIO buckets..."
    obj_net="$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' "$(docker compose ps -q minio)" 2>/dev/null | awk '{print $1}')"
    obj_net="${obj_net:-nisaba_obj-net}"
    docker run --rm -i --network "$obj_net" \
        -e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD \
        -v "${SRC}/minio:/in:ro" \
        minio/mc:RELEASE.2024-10-02T08-27-28Z sh -c '
            mc alias set local http://minio:9000 "$MINIO_ROOT_USER" "$MINIO_ROOT_PASSWORD" >/dev/null
            for b in '"${NISABA_S3_BUCKET_BLOBS:-nisaba-blobs}"' '"${NISABA_S3_BUCKET_OPLOG:-nisaba-oplog}"'; do
                if [ -d "/in/${b}" ]; then
                    mc mirror --overwrite --watch=false "/in/${b}" "local/${b}" 2>/dev/null \
                        || echo "[restore] WARN: restore of ${b} had errors"
                fi
            done
        ' || echo "[restore] WARN: minio restore step failed."
else
    echo "[restore] no MinIO snapshot at ${SRC}/minio; skipping objects."
fi

# ---- sync filesystem ----
# Restored via a one-shot sync container (mounts the sync-data volume), so it
# works even after `just down` stopped the app tier. Requires the sync image.
TAR="${SRC}/sync/sync.tar.gz"
if [ -f "$TAR" ]; then
    echo "[restore] restoring sync filesystem (op-log + snapshots)..."
    gunzip -c "$TAR" | docker compose --profile app run --rm -T --no-deps \
        --entrypoint /bin/sh sync \
        -c 'rm -rf /data/oplog /data/snapshots && tar -xf - -C /data' \
        || { echo "[restore] sync-fs restore failed." >&2; exit 1; }
else
    echo "[restore] no sync snapshot at ${TAR}; skipping sync filesystem."
fi

echo "[restore] done."
