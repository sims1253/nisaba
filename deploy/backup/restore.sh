#!/usr/bin/env bash
# Restore Nisaba data from a backup directory produced by backup.sh.
#
#   Usage: just restore artifacts/backups/20240101T120000Z
#          (or)  ./deploy/backup/restore.sh path/to/snapshot
#
#   - Postgres : restores the gzip'd SQL dump (drop+recreate via --clean).
#   - SeaweedFS: syncs the local bucket copies back into SeaweedFS.
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

# ---- SeaweedFS ----
if [ -d "${SRC}/seaweedfs" ]; then
    echo "[restore] restoring SeaweedFS buckets..."
    obj_net="$(docker inspect -f '{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}' "$(docker compose ps -q seaweedfs)" 2>/dev/null | awk '{print $1}')"
    obj_net="${obj_net:-nisaba_obj-net}"
    # Digest-pinned to match docker-compose.yml (seaweedfs-init service).
    if ! docker run --rm -i \
            --network "$obj_net" \
            --entrypoint /bin/sh \
            -e AWS_ACCESS_KEY_ID="${NISABA_S3_ADMIN_KEY}" \
            -e AWS_SECRET_ACCESS_KEY="${NISABA_S3_ADMIN_SECRET}" \
            -v "${SRC}/seaweedfs:/in:ro" \
            amazon/aws-cli:2.36.20@sha256:8af59c0d96b104000cce4f11e211c06385240d72c515198159041f13ebe459fa \
            -c '
            export AWS_ENDPOINT_URL=http://seaweedfs:8333
            failed=0
            for b in '"${NISABA_S3_BUCKET_BLOBS:-nisaba-blobs}"' '"${NISABA_S3_BUCKET_OPLOG:-nisaba-oplog}"'; do
                if [ -d "/in/${b}" ]; then
                    if ! aws s3 sync --no-progress "/in/${b}" "s3://${b}"; then
                        echo "[restore] ERROR: restore of ${b} failed" >&2
                        failed=1
                    fi
                fi
            done
            exit $failed
        '; then
        echo "[restore] ERROR: SeaweedFS restore step failed." >&2
        exit 1
    fi
else
    echo "[restore] no SeaweedFS snapshot at ${SRC}/seaweedfs; skipping objects."
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
