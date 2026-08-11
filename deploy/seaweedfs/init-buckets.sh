#!/usr/bin/env sh
# SeaweedFS bootstrap: create Nisaba buckets and verify the least-privilege
# service account.
#
# Runs as a one-shot sidecar after SeaweedFS is healthy. Idempotent: every
# command tolerates "already exists" so the stack can be re-upped.
#
# Privilege model (see docs/security.md):
#   - nisaba-admin credentials : bootstrap + admin only (s3.json).
#   - NISABA_S3_ACCESS_KEY      : scoped to Read/Write/List actions; used by
#                                 app. Never admin.
#
# Unlike MinIO (which created service accounts at runtime via `mc admin`),
# SeaweedFS identities are declared statically in deploy/seaweedfs/s3.json.
# This script only creates buckets and enables versioning.
set -eu

: "${NISABA_S3_ADMIN_KEY:?NISABA_S3_ADMIN_KEY must be set}"
: "${NISABA_S3_ADMIN_SECRET:?NISABA_S3_ADMIN_SECRET must be set}"
: "${NISABA_S3_BUCKET_BLOBS:?NISABA_S3_BUCKET_BLOBS must be set}"
: "${NISABA_S3_BUCKET_OPLOG:?NISABA_S3_BUCKET_OPLOG must be set}"

ENDPOINT="${NISABA_S3_ENDPOINT:-http://seaweedfs:8333}"

# SeaweedFS supports the standard S3 API, so we use aws-cli instead of mc.
# --no-sign-request falls back to the anonymous identity when the bucket does
# not exist yet; after creation we use admin creds for versioning config.

export AWS_ACCESS_KEY_ID="${NISABA_S3_ADMIN_KEY}"
export AWS_SECRET_ACCESS_KEY="${NISABA_S3_ADMIN_SECRET}"
export AWS_ENDPOINT_URL="${ENDPOINT}"

# Buckets (object keys are opaque ids, never citation numbers — PLAN.md §6.3).
for bucket in "${NISABA_S3_BUCKET_BLOBS}" "${NISABA_S3_BUCKET_OPLOG}"; do
    # Create the bucket if it doesn't exist (ignore BucketAlreadyOwnedByYou).
    aws s3api create-bucket --bucket "${bucket}" 2>/dev/null || true
    # Enable versioning to protect against overwrite/delete of full-text PDFs
    # and op-log snapshots.
    aws s3api put-bucket-versioning \
        --bucket "${bucket}" \
        --versioning-configuration Status=Enabled 2>/dev/null || true
    # Block public access (defense-in-depth alongside the anonymous Read-only identity).
    aws s3api put-public-access-block \
        --bucket "${bucket}" \
        --public-access-block-configuration \
            BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true \
        2>/dev/null || true
done

echo "[init-buckets] buckets + versioning ready at ${ENDPOINT}"
