# Configuration reference

Every environment variable the stack understands, grouped by the container or
service that consumes it. Defaults are what the code or `docker-compose.yml`
falls back to when the variable is unset — the committed
[`.env.example`](../.env.example) shows the local-development values. For
production guidance (secrets sourcing, issuer collapse) see
[`deployment.md`](deployment.md); for day-2 operations see
[`operations.md`](operations.md).

Conventions:

- Compose hard-requires the variables marked **required** (`${VAR:?}` fails
  `docker compose up` when unset).
- "Reserved" means the variable is declared in `.env.example` but read by
  nothing yet — setting it has no effect.
- The web client is a build-time exception: `VITE_*` values are baked into the
  SPA bundle when the image is built, not read at runtime.

---

## Postgres container

| Variable | Default (compose) | What it does | Read by |
|----------|-------------------|--------------|---------|
| `POSTGRES_USER` | `postgres` | Maintenance superuser (bootstrap/migrations only) | `docker-compose.yml` |
| `POSTGRES_PASSWORD` | — **required** | Superuser password | `docker-compose.yml` |
| `POSTGRES_DB` | `postgres` | Bootstrap database | `docker-compose.yml` |
| `NISABA_DB_USER` | `nisaba_app` | Least-privilege app role (owns the `nisaba` DB) | init script `deploy/postgres/init/10-init-databases.sh` |
| `NISABA_DB_PASSWORD` | — **required** | App role password | same |
| `NISABA_DB_NAME` | `nisaba` | Application database | same |
| `KEYCLOAK_DB_USER` | `keycloak` | Dedicated Keycloak role | same |
| `KEYCLOAK_DB_PASSWORD` | — **required** | Keycloak role password | same |
| `POSTGRES_HOST_PORT` | `5433` | Host-side port (bound to `127.0.0.1`) | `docker-compose.yml` |

## SeaweedFS + bucket init

Identities are **generated at container start** from these variables
(`deploy/seaweedfs/generate-s3-identities.sh`) — changing the key material
means changing the environment and recreating the container; no identity
file is committed to the repo.

| Variable | Default (compose) | What it does | Read by |
|----------|-------------------|--------------|---------|
| `NISABA_S3_ADMIN_KEY` | `nisaba-admin` | Admin identity used only by the bucket bootstrap | `deploy/seaweedfs/init-buckets.sh` (feeds the generated admin identity) |
| `NISABA_S3_ADMIN_SECRET` | — **required** | Admin secret | same |
| `SEAWEEDFS_HOST_S3_PORT` | `9100` | Host-side S3 port (`127.0.0.1`) | `docker-compose.yml` |
| `NISABA_S3_BUCKET_BLOBS` | `nisaba-blobs` | Reference full-text bucket (created + versioned on first boot) | init script, `app` |
| `NISABA_S3_BUCKET_OPLOG` | `nisaba-oplog` | Sync durability bucket: CRDT op-log parts (`oplog/` prefix) + snapshots (`snapshot/` prefix); created + versioned on first boot | init script, `sync` |

## Keycloak container

| Variable | Default (compose) | What it does | Read by |
|----------|-------------------|--------------|---------|
| `KEYCLOAK_ADMIN` | `admin` | Initial admin-console account | `docker-compose.yml` |
| `KEYCLOAK_ADMIN_PASSWORD` | — **required** | Admin password | `docker-compose.yml` |
| `KEYCLOAK_HTTP_PORT` | `8090` | Container + host port (`127.0.0.1`) | `docker-compose.yml` |
| `KEYCLOAK_IMAGE` | `quay.io/keycloak/keycloak:26.0` (digest-pinned) | Overrides the Keycloak image | `docker-compose.yml` |

The `KC_*` variables (database URL/credentials, HTTP mode, hostname) are set
inline in `docker-compose.yml` for the dev profile — production Keycloak
replacement is documented in [`deploy/keycloak/README.md`](../deploy/keycloak/README.md).

## `app` service

| Variable | Default | What it does | Read by |
|----------|---------|--------------|---------|
| `DATABASE_URL` | synthesised by compose | Postgres connection string; migrations run at startup. **Not set in `.env`**: compose builds the container-facing URL (`postgres://…@postgres:5432/…`) from `NISABA_DB_USER` / `NISABA_DB_PASSWORD` / `NISABA_DB_NAME` so the password exists in one place only; the justfile `migrate` recipe synthesises the host-facing equivalent (`127.0.0.1:$POSTGRES_HOST_PORT`) for sqlx the same way | `PostgresRepository::from_env` (`services/app/src/persistence.rs`) |
| `NISABA_S3_ENDPOINT` | — **required** | S3 endpoint as seen from the container (`http://seaweedfs:8333`) | `S3BlobStore::from_env` |
| `NISABA_S3_ACCESS_KEY` / `NISABA_S3_SECRET_KEY` | — **required** | App S3 identity (generated at seaweedfs start) | same |
| `NISABA_S3_REGION` | `us-east-1` (code) / `local` (compose) | S3 region label | same |
| `NISABA_S3_BUCKET_BLOBS` | — **required** | Full-text bucket | same |
| `NISABA_OIDC_ISSUER` | `https://issuer.invalid` (code) | Expected `iss` claim; must match the token issuer exactly | `services/app/src/main.rs` |
| `NISABA_OIDC_AUDIENCE` | `nisaba` | Expected `aud` claim (the dev realm emits both `nisaba-web` and `nisaba`) | same |
| `NISABA_OIDC_JWKS_JSON` | empty → deny-all | Signing keys read **inline** at startup; empty/unset rejects every token | same |
| `NISABA_OIDC_CLIENT_ID` | `nisaba-web` | Passed to the web image build as the SPA's client id | `docker-compose.yml` (build arg) |
| `NISABA_OIDC_DISCOVERY_URL` | — | **Reserved**: the future JWKS-fetch URL; the app reads `NISABA_OIDC_JWKS_JSON` inline today | nothing yet |
| `NISABA_COMPILE_URL` | `http://compile:8080` | Compile service endpoint on `svc-net` | `services/app/src/main.rs` |
| `NISABA_COMPILE_TOKEN` | — **required**, non-empty | Shared secret for `app → compile` calls | same + compile |
| `NISABA_SYNC_AUTHZ_TOKEN` | — **required** in production | Machine secret for `sync → app` document authorization; also presented by `app → sync` internal state reads | same + sync |
| `NISABA_SYNC_STATE_URL` | `http://sync:8080` | Sync service base URL for the export path's review-state reads (`GET /internal/docs/{id}/state`) | same |
| `NISABA_APP_ADDR` | `0.0.0.0:8080` (compose) | Bind address (`PORT` is a fallback) | same |
| `APP_HOST_PORT` | `8100` | Host-side port (`127.0.0.1`) | `docker-compose.yml` |
| `RUST_LOG` / `RUST_BACKTRACE` | `info` / `1` | Log verbosity / backtraces (all Rust services) | `tracing_subscriber` |
| `TEST_DATABASE_URL` | — | **Reserved** for optional Postgres-backed adapter tests | `services/app` tests |

Role names are **not configurable**: the `author` / `reviewer` / `read-only`
vocabulary is hardcoded in `crates/nisaba-auth` (`Role::parse`), and roles are
read from the token's top-level `roles` claim as configured in the realm
mapper. (Legacy `NISABA_OIDC_ROLE_AUTHOR` / `_REVIEWER` / `_READONLY`
variables were removed from `.env.example` — nothing ever read them.)

## `sync` service

The complete table with per-variable semantics lives in
[`services/sync/README.md`](../services/sync/README.md) ("Configuration"). The
variables `.env.example` sets for the local stack:

| Variable | Default | What it does |
|----------|---------|--------------|
| `NISABA_SYNC_ADDR` / `PORT` | `0.0.0.0:8080` | Bind address resolution order |
| `SYNC_HOST_PORT` | `8101` | Host-side port (`127.0.0.1`), compose |
| `NISABA_SYNC_STORE_BACKEND` | `fs` (compose pins `s3`) | Durable stores: `s3` (op-log + snapshots in the `NISABA_S3_BUCKET_OPLOG` bucket) or `fs` (local data dir). A missing S3 variable in `s3` mode is a fatal startup error |
| `NISABA_S3_ENDPOINT` / `NISABA_S3_ACCESS_KEY` / `NISABA_S3_SECRET_KEY` | — **required** in `s3` mode | Same SeaweedFS endpoint + identity the app uses (`S3Stores` in `services/sync/src/s3.rs`) |
| `NISABA_S3_REGION` | `us-east-1` (code) / `local` (compose) | S3 region label |
| `NISABA_S3_BUCKET_OPLOG` | — **required** in `s3` mode | Bucket for the `oplog/` and `snapshot/` key prefixes |
| `NISABA_SYNC_DATA_DIR` | `data` | Op-log + snapshot directory (**`fs` backend only**; unused in compose) |
| `NISABA_SYNC_DEV_ALLOW_ALL` | unset | **Never in production**: grants `author` to any non-empty token |
| `NISABA_SYNC_OIDC_ISSUER` / `NISABA_SYNC_OIDC_AUDIENCE` / `NISABA_SYNC_OIDC_JWKS_URL` | unset (deny-all); `.env.example` sets all three | All three set together enable JWT/JWKS validation — the local stack validates real Keycloak JWTs; a partial set is a fatal startup error |
| `NISABA_SYNC_OIDC_ROLES_CLAIM` | `realm_access.roles` | Dotted path of the roles claim (set `roles` for this realm) |
| `NISABA_SYNC_AUTHZ_URL` | unset → deny-all documents | App's `/internal/sync/authorize` endpoint |
| `NISABA_SYNC_AUTHZ_TOKEN` | unset | Machine token for the authz endpoint (same value the app checks); also required by `GET /internal/docs/{id}/state` — unset → deny-all (fail-closed) |
| `NISABA_SYNC_SEED_VERIFY_URL` | unset → fail-closed | App endpoint verifying a reviewer's seed of an empty room |
| `NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME` | unset | Permit `http://` outbound (local dev only) |

Tuning variables (`NISABA_SYNC_OIDC_ALGORITHMS`, `..._LEEWAY_SECS`,
`..._JWKS_MAX_AGE_SECS`, `..._JWKS_REFRESH_SECS`, `..._TOKEN_CACHE_TTL_SECS`,
`NISABA_SYNC_AUTHZ_TIMEOUT_SECS`, `NISABA_SYNC_HTTP_CONNECT_TIMEOUT_SECS`,
`NISABA_SYNC_HTTP_REQUEST_TIMEOUT_SECS`) are listed in the sync README table.

## `compile` service

| Variable | Default | What it does |
|----------|---------|--------------|
| `NISABA_COMPILE_ADDR` / `PORT` | `0.0.0.0:8080` | Bind address (no published host port) |
| `NISABA_COMPILE_MODE` | `production` | `development`/`test` additionally disable auth — never use these modes in production |
| `NISABA_COMPILE_TOKEN` | — **required** in production | Bearer secret enforced on `POST /compile` |
| `NISABA_COMPILE_TIMEOUT_MS` | `120000` | Request compile timeout |
| `NISABA_COMPILE_MAX_WORKERS` | `256` | Maximum cached Typst workers (LRU + idle-TTL evicted) |
| `NISABA_COMPILE_WORKER_IDLE_TTL_MS` | `1800000` | Idle TTL before a cached worker is evicted |
| `NISABA_COMPILE_MAX_CONCURRENT_COMPILES` | `8` | Global cap on concurrently running compiles |
| `NISABA_COMPILE_MAX_BODY_BYTES` | `8388608` | Request body limit |
| `NISABA_COMPILE_MAX_SOURCES` | `256` | Maximum source files |
| `NISABA_COMPILE_MAX_SOURCE_BYTES` | `4194304` | Aggregate source bytes |

(Also documented in [`services/compile/README.md`](../services/compile/README.md).)

## `web` (SPA + nginx)

Build-time (baked into the bundle by `deploy/Dockerfile.web`; changing them
requires an image rebuild):

| Variable | Default | What it does |
|----------|---------|--------------|
| `VITE_OIDC_ISSUER` | — **required** (build fails if empty) | Browser-facing OIDC issuer |
| `VITE_OIDC_CLIENT_ID` | `nisaba-web` | Public PKCE client id |
| `VITE_OIDC_SCOPE` | `openid profile email` | Requested OIDC scope |
| `WEB_HOST_PORT` | `8103` | Host-side port for the nginx container (`127.0.0.1`) |

Runtime reads with fallbacks (rarely needed): `VITE_OIDC_REDIRECT_URI`
(defaults to the current origin).

Vite **dev-server only** (unused in the built image): `VITE_APP_URL`,
`VITE_SYNC_URL`, `VITE_ALLOWED_HOSTS`, `VITE_OIDC_PROXY_TARGET` — see
`web/vite.config.ts`.

## Backup scripts

| Variable | Default | What it does | Read by |
|----------|---------|--------------|---------|
| `BACKUP_RETENTION_DAYS` | `7` | Prune local snapshots older than N days | `deploy/backup/backup.sh` |
| `BACKUP_LOCAL_DIR` | `./artifacts/backups` | Snapshot destination (gitignored) | `deploy/backup/backup.sh` |

## Observability (reserved)

`OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_SERVICE_NAMESPACE` are declared in
`.env.example` but **no service ships an OTEL exporter or a `/metrics`
endpoint** — they are reserved for the observability plan in
[`operations.md`](operations.md) §3.
