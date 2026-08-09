#!/usr/bin/env sh
# MinIO bootstrap: create Nisaba buckets and a least-privilege service account.
#
# Runs as a one-shot sidecar (minio-init) after MinIO is healthy. Idempotent:
# every command tolerates "already exists" so the stack can be re-upped.
#
# Privilege model (see docs/security.md):
#   - root credentials            : bootstrap + admin only.
#   - NISABA_S3_ACCESS_KEY        : scoped to nisaba-* buckets via a custom
#                                   policy; used by app/sync/compile. Never root.
set -eu

: "${MINIO_ROOT_USER:?MINIO_ROOT_USER must be set}"
: "${MINIO_ROOT_PASSWORD:?MINIO_ROOT_PASSWORD must be set}"
: "${NISABA_S3_ACCESS_KEY:?NISABA_S3_ACCESS_KEY must be set}"
: "${NISABA_S3_SECRET_KEY:?NISABA_S3_SECRET_KEY must be set}"
: "${NISABA_S3_BUCKET_BLOBS:?NISABA_S3_BUCKET_BLOBS must be set}"
: "${NISABA_S3_BUCKET_OPLOG:?NISABA_S3_BUCKET_OPLOG must be set}"

ALIAS=local
mc alias set "${ALIAS}" "http://minio:9000" "${MINIO_ROOT_USER}" "${MINIO_ROOT_PASSWORD}"

# Buckets (object keys are opaque ids, never citation numbers — PLAN.md §6.3).
mc mb -p "${ALIAS}/${NISABA_S3_BUCKET_BLOBS}" 2>/dev/null || true
mc mb -p "${ALIAS}/${NISABA_S3_BUCKET_OPLOG}" 2>/dev/null || true
mc anonymous set none "${ALIAS}/${NISABA_S3_BUCKET_BLOBS}" 2>/dev/null || true
mc anonymous set none "${ALIAS}/${NISABA_S3_BUCKET_OPLOG}" 2>/dev/null || true
# Versioning protects against overwrite/delete of full-text PDFs and op-log snapshots.
mc version enable "${ALIAS}/${NISABA_S3_BUCKET_BLOBS}" 2>/dev/null || true
mc version enable "${ALIAS}/${NISABA_S3_BUCKET_OPLOG}" 2>/dev/null || true

# Least-privilege policy: scoped to nisaba-* buckets only.
POLICY_FILE="$(mktemp)"
cat > "${POLICY_FILE}" <<'EOF'
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:ListBucket", "s3:GetBucketLocation", "s3:GetObjectVersion", "s3:ListBucketVersions"],
      "Resource": ["arn:aws:s3:::nisaba-*"]
    },
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:AbortMultipartUpload", "s3:ListMultipartUploadParts"],
      "Resource": ["arn:aws:s3:::nisaba-*/*"]
    }
  ]
}
EOF
mc admin policy create "${ALIAS}" nisaba-app "${POLICY_FILE}" 2>/dev/null \
  || mc admin policy add "${ALIAS}" nisaba-app "${POLICY_FILE}" 2>/dev/null || true

# Service account (create-or-skip; re-up keeps the existing secret).
mc admin user add "${ALIAS}" "${NISABA_S3_ACCESS_KEY}" "${NISABA_S3_SECRET_KEY}" 2>/dev/null || true

# Attach policy across mc versions (attach -> set legacy fallback).
mc admin policy attach "${ALIAS}" nisaba-app --user "${NISABA_S3_ACCESS_KEY}" 2>/dev/null \
  || mc admin policy set "${ALIAS}" nisaba-app "user=${NISABA_S3_ACCESS_KEY}" 2>/dev/null || true

echo "[init-buckets] buckets + nisaba-app policy ready"
