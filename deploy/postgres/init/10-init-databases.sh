#!/usr/bin/env bash
# PostgreSQL bootstrap: create dedicated, least-privilege roles + databases.
#
# Runs once, as the POSTGRES superuser, during the postgres container's first
# initialisation (scripts in /docker-entrypoint-initdb.d run on an empty data
# dir only). Idempotent guards make re-import safe.
#
# Privilege model (see docs/security.md):
#   - `postgres`      : maintenance superuser, bootstrap + migrations only.
#   - `nisaba_app`    : owns the `nisaba` database; used by the `app` service.
#   - `keycloak`      : owns the `keycloak` database; used by Keycloak only.
# No application ever uses the superuser.
set -euo pipefail

: "${POSTGRES_USER:?POSTGRES_USER must be set}"
: "${NISABA_DB_USER:?NISABA_DB_USER must be set}"
: "${NISABA_DB_PASSWORD:?NISABA_DB_PASSWORD must be set}"
: "${NISABA_DB_NAME:?NISABA_DB_NAME must be set}"
: "${KEYCLOAK_DB_USER:?KEYCLOAK_DB_USER must be set}"
: "${KEYCLOAK_DB_PASSWORD:?KEYCLOAK_DB_PASSWORD must be set}"

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
    -- Roles (LOGIN, no SUPERUSER/CREATEDB/CREATEROLE).
    DO \$do\$
    BEGIN
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${KEYCLOAK_DB_USER}') THEN
            CREATE ROLE "${KEYCLOAK_DB_USER}" LOGIN PASSWORD '${KEYCLOAK_DB_PASSWORD}';
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '${NISABA_DB_USER}') THEN
            CREATE ROLE "${NISABA_DB_USER}" LOGIN PASSWORD '${NISABA_DB_PASSWORD}';
        END IF;
    END
    \$do\$;

    -- Databases (created only if absent; \gexec is idempotent).
    SELECT 'CREATE DATABASE "${KEYCLOAK_DB_USER}" OWNER "${KEYCLOAK_DB_USER}"'
      WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${KEYCLOAK_DB_USER}')\gexec
    SELECT 'CREATE DATABASE "${NISABA_DB_NAME}" OWNER "${NISABA_DB_USER}"'
      WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = '${NISABA_DB_NAME}')\gexec

    GRANT ALL PRIVILEGES ON DATABASE "${KEYCLOAK_DB_USER}" TO "${KEYCLOAK_DB_USER}";
    GRANT ALL PRIVILEGES ON DATABASE "${NISABA_DB_NAME}"   TO "${NISABA_DB_USER}";

    -- Future tables created by the owning role keep ownership to themselves.
    ALTER DATABASE "${KEYCLOAK_DB_USER}" OWNER TO "${KEYCLOAK_DB_USER}";
    ALTER DATABASE "${NISABA_DB_NAME}"   OWNER TO "${NISABA_DB_USER}";
EOSQL

echo "[10-init-databases] roles and databases ready: ${NISABA_DB_NAME}, ${KEYCLOAK_DB_USER}"
