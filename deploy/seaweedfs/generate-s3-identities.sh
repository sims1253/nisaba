#!/bin/sh
# Generate the SeaweedFS S3 identity config from the NISABA_S3_* environment
# variables. docker-compose.yml runs this inside the seaweedfs container before
# `weed server` starts, so no real credentials live in the repository: the
# identities are rendered at container start-up from the operator's `.env` and
# written to an ephemeral file (regenerated on every start, never persisted in
# a volume or committed). Rotating a secret therefore only requires editing
# `.env` and recreating the container.
#
# Privilege model (see docs/security.md §3):
#   * nisaba-app   (NISABA_S3_ACCESS_KEY / NISABA_S3_SECRET_KEY):
#                  Read/Write/List/Tagging — the application's own account.
#   * nisaba-admin (NISABA_S3_ADMIN_KEY / NISABA_S3_ADMIN_SECRET):
#                  Admin — bootstrap only (deploy/seaweedfs/init-buckets.sh),
#                  never used by the app.
#
# There is deliberately NO anonymous identity: every S3 request must be signed.
# The gateway is published at 127.0.0.1:<port>, and an anonymous-read identity
# would let any host process read every blob (including full-text PDFs)
# unauthenticated.
set -eu

out=${1:?usage: generate-s3-identities.sh <output-file>}

: "${NISABA_S3_ACCESS_KEY:?NISABA_S3_ACCESS_KEY must be set}"
: "${NISABA_S3_SECRET_KEY:?NISABA_S3_SECRET_KEY must be set}"
: "${NISABA_S3_ADMIN_KEY:?NISABA_S3_ADMIN_KEY must be set}"
: "${NISABA_S3_ADMIN_SECRET:?NISABA_S3_ADMIN_SECRET must be set}"

# Reject non-printable characters outright (they are never legitimate in
# generated secrets and the simple escaper below only handles \ and ").
# NB: validated via `case` classes because busybox tr does not support
# [:print:] and swallows the failure inside command substitution.
require_printable() {
    case $1 in
        *[![:print:]]*)
            echo "generate-s3-identities: $2 must be printable ASCII" >&2
            return 1 ;;
    esac
}

# JSON-encode one (already-validated) value: quote-wrap and escape backslash
# and double quote.
json_str() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/.*/"&"/'
}

require_printable "$NISABA_S3_ACCESS_KEY" NISABA_S3_ACCESS_KEY || exit 1
require_printable "$NISABA_S3_SECRET_KEY" NISABA_S3_SECRET_KEY || exit 1
require_printable "$NISABA_S3_ADMIN_KEY" NISABA_S3_ADMIN_KEY || exit 1
require_printable "$NISABA_S3_ADMIN_SECRET" NISABA_S3_ADMIN_SECRET || exit 1

# Compute the encoded values up front so a failure aborts BEFORE anything is
# written (a failed command substitution inside the output block would
# otherwise leave a truncated, unparsable config file behind).
app_key=$(json_str "$NISABA_S3_ACCESS_KEY") || exit 1
app_secret=$(json_str "$NISABA_S3_SECRET_KEY") || exit 1
admin_key=$(json_str "$NISABA_S3_ADMIN_KEY") || exit 1
admin_secret=$(json_str "$NISABA_S3_ADMIN_SECRET") || exit 1

# Restrictive umask: the file carries the S3 secrets.
umask 027
{
    printf '%s\n' '{'
    printf '%s\n' '  "identities": ['
    printf '%s\n' '    {'
    printf '%s\n' '      "name": "nisaba-app",'
    printf '%s\n' '      "credentials": ['
    printf '%s\n' '        {'
    printf '          "accessKey": %s,\n' "$app_key"
    printf '          "secretKey": %s\n' "$app_secret"
    printf '%s\n' '        }'
    printf '%s\n' '      ],'
    printf '%s\n' '      "actions": ["Read", "Write", "List", "Tagging"]'
    printf '%s\n' '    },'
    printf '%s\n' '    {'
    printf '%s\n' '      "name": "nisaba-admin",'
    printf '%s\n' '      "credentials": ['
    printf '%s\n' '        {'
    printf '          "accessKey": %s,\n' "$admin_key"
    printf '          "secretKey": %s\n' "$admin_secret"
    printf '%s\n' '        }'
    printf '%s\n' '      ],'
    printf '%s\n' '      "actions": ["Admin", "Read", "Write", "List", "Tagging"]'
    printf '%s\n' '    }'
    printf '%s\n' '  ]'
    printf '%s\n' '}'
} > "${out}.tmp"

# Make the file readable by the non-root `seaweed` user that the image's
# /entrypoint.sh drops to via su-exec (chown fails harmlessly when this script
# already runs as a non-root user).
if ! chown seaweed:seaweed "${out}.tmp" 2>/dev/null; then
    chmod 640 "${out}.tmp" 2>/dev/null || true
fi
mv "${out}.tmp" "$out"

echo "generate-s3-identities: wrote $out (identities: nisaba-app, nisaba-admin)"
