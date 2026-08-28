#!/usr/bin/env bash
# Verify a backup snapshot directory WITHOUT restoring it.
#
#   Usage: just verify-backup artifacts/backups/<timestamp>
#          (or)  ./deploy/backup/verify.sh <backup-snapshot-dir>
#
# Structural checks only (no containers, no network, no data mutation):
#   * Postgres : nisaba.sql.gz exists, is a valid gzip, and its SQL carries
#                PostgreSQL dump markers.
#   * SeaweedFS: the nisaba-* bucket directories (the app's blobs and the
#                sync service's op-log + snapshots) are present in the snapshot.
#
# This complements (does not replace) a real restore drill: restoring into an
# isolated throwaway stack and checking row/object counts is the release
# acceptance. See docs/operations.md §4.
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
    echo "verify: snapshot directory not found: $SRC" >&2
    exit 66
fi
SRC="$(cd "$SRC" && pwd)"

fail=0
ok() { echo "  ok    $1"; }
bad() { echo "  FAIL  $1" >&2; fail=1; }

# Decompress to a bounded buffer without tripping pipefail on SIGPIPE: head
# closes the pipe early; the leading `var=$(... || true)` keeps the exit code.
peek() {
    # shellcheck disable=SC2155
    local out
    out="$(gunzip -c "$1" 2>/dev/null | head -n "$2" || true)"
    printf '%s' "$out"
}

echo "[verify] ${SRC}"

# ---- Postgres ----
DUMP="${SRC}/postgres/nisaba.sql.gz"
if [ -f "$DUMP" ]; then
    if gunzip -t "$DUMP" 2>/dev/null; then
        header="$(peek "$DUMP" 40)"
        if printf '%s' "$header" | grep -qiE 'PostgreSQL database dump|-- Name:|CREATE DATABASE'; then
            ok "postgres dump valid (gzip + SQL markers)"
        else
            bad "postgres dump decompresses but has no PostgreSQL markers"
        fi
    else
        bad "postgres dump is not a valid gzip: ${DUMP}"
    fi
else
    bad "missing postgres dump: ${DUMP}"
fi

# ---- SeaweedFS ----
# Both buckets matter: nisaba-blobs is the app's full-text store, nisaba-oplog
# is the sync service's durable CRDT history (oplog/ + snapshot/ prefixes).
SEAWEEDFS_DIR="${SRC}/seaweedfs"
blobs_found=0
if [ -d "$SEAWEEDFS_DIR" ]; then
    for b in "${NISABA_S3_BUCKET_BLOBS:-nisaba-blobs}" "${NISABA_S3_BUCKET_OPLOG:-nisaba-oplog}"; do
        if [ -d "${SEAWEEDFS_DIR}/${b}" ]; then
            ok "seaweedfs bucket snapshot present: ${b}"
            blobs_found=1
        else
            bad "seaweedfs bucket missing from snapshot: ${b}"
        fi
    done
    if [ "$blobs_found" -ne 1 ]; then
        bad "seaweedfs snapshot has no nisaba-* bucket dirs"
    fi
else
    bad "missing seaweedfs snapshot: ${SEAWEEDFS_DIR}"
fi

if [ "$fail" -ne 0 ]; then
    echo "[verify] FAILED — snapshot is incomplete or corrupt." >&2
    exit 1
fi
echo "[verify] OK — snapshot is structurally sound."
