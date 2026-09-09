# Operations

Start, monitor, back up, and restore the local stack. For self-hosting, see
[deployment](deployment.md), [security](security.md), and
[configuration](configuration.md).

The Compose stack is for local development. Production deployment and restore
drills have not been verified. Metrics and tracing exporters are planned.

## 1. Quick start

Install Docker with Compose v2 and [just](https://github.com/casey/just).
Run these commands from the repository root; Docker builds the application:

```bash
cp .env.example .env          # edit the change-me-* secrets
just up-all                  # build and start infrastructure and app services
docker compose ps
```

After the services start, open http://127.0.0.1:8103 and sign in with
`demo` / `demo`. Follow the [user guide](user-guide.md) to create a document.
The bundled demo accounts and identity-provider realm are for local testing only.
Published ports bind to `127.0.0.1`; see [deployment](deployment.md) before
exposing the stack. Use `just down` to stop it and keep its data.

Keycloak admin is
at http://127.0.0.1:8090 (`admin` / `KEYCLOAK_ADMIN_PASSWORD`). The S3 API is
at http://127.0.0.1:9100.

`just up-all` fetches the development realm's signing keys when
`NISABA_OIDC_JWKS_JSON` is unset or empty. Use `just up` for infrastructure
only. `just smoke` checks infrastructure; `just e2e` checks the full app stack.

For code changes and checks, see [Contributing](../CONTRIBUTING.md).

---

## 2. Health & readiness

Every HTTP service exposes **`GET /healthz` → `200 ok`** (the Compose
`HEALTHCHECK` contract and the `depends_on: condition: service_healthy` gate).

| Service   | Probe                                              | notes |
|-----------|----------------------------------------------------|-------|
| postgres  | `pg_isready`                                       | live |
| seaweedfs | `GET /healthz` (port 8333, the S3 port)            | live |
| keycloak  | `GET /health/ready` (mgmt :9000, container-internal) | live; the mgmt port is NOT published to the host (probe from inside the compose network, e.g. `docker compose exec keycloak curl ...`, or rely on the container healthcheck) |
| app       | `GET /healthz` + `GET /health/ready` (port 8080)   | live + DB ready |
| sync      | `GET /healthz` + `GET /health/ready` (port 8080)   | live + store ready |
| compile   | `GET /healthz` (port 8080)                         | live |
| web       | `GET /healthz` (nginx)                             | needs the built SPA |

`/healthz` is a **liveness** probe. The app readiness endpoint performs a PostgreSQL
check. sync readiness verifies its durable store: with the S3 stores configured
(compose default) it issues a `HeadBucket` against the `nisaba-oplog` bucket, so
orchestration never routes traffic to a sync that cannot persist; with the
filesystem stores it checks the data directory is writable. App readiness does
not currently probe S3, and compile exposes liveness only; a passing readiness
check does not show that S3 is healthy (app) or anything beyond liveness
(compile).

For the app/sync collaboration path, configure the same non-empty
`NISABA_SYNC_AUTHZ_TOKEN` in both containers. Production app startup rejects a
missing or blank value; development/test app modes intentionally permit omission
but the internal authorization endpoint remains deny-all.

---

## 3. Observability

Rust services log through `tracing`; `RUST_LOG` controls verbosity. Compose
rotates each container's logs at 10 MB and retains three files. Logs can contain
author identities, so restrict access when collecting them centrally.

No service exports OTLP or serves `/metrics`. The `OTEL_*` variables in
`.env.example` are reserved and have no effect. Compile responses include
worker/cache counters and RSS when available; use those when sizing the
[worker pool](../services/compile/README.md#runtime-configuration).

## 4. Backup & restore

Scripts: [`deploy/backup/backup.sh`](../deploy/backup/backup.sh),
[`deploy/backup/restore.sh`](../deploy/backup/restore.sh). Run via
`just backup` / `just restore <dir>`.

### What is backed up

- The `nisaba` PostgreSQL database, as a gzipped logical dump. Back up the
  Keycloak database separately.
- Current objects in the `nisaba-*` S3 buckets, copied with `aws s3 sync`. This
  includes sync history under `oplog/` and `snapshot/` in `nisaba-oplog`.
  Bucket version history is not copied.

A failed database dump or bucket copy aborts the backup and reports it as
incomplete. Filesystem-backed sync deployments must also back up
`NISABA_SYNC_DATA_DIR`; the scripts cover the Compose S3 backend.

### Local rotation
`BACKUP_RETENTION_DAYS` (default 7) prunes local snapshots older than N days.
Backups land under `BACKUP_LOCAL_DIR` (default `./artifacts/backups`, gitignored).

### Restore (overwrites current data)
```bash
just down                       # stop app tier first
just restore artifacts/backups/<timestamp>
```

### Verify a backup (no restore)
```bash
just verify-backup artifacts/backups/<timestamp>
```
Asserts the snapshot is structurally sound (SQL dump is a valid PostgreSQL
backup, both bucket dirs exist — including the op-log bucket that holds
sync's durable history). A real restore drill
restores into an **isolated** throwaway stack (`-p nisaba-restore-drill`) and
checks row/object counts — schedule it as part of release acceptance.

### Production deltas
- **Off-host:** stream `pg_dump` (or use WAL archiving / point-in-time recovery)
  and SeaweedFS bucket replication to object storage in a different failure domain.
- **Tested restores:** schedule a restore drill into an isolated environment.
- **Immutability:** write backups to a WORM/object-lock target so ransomware or
  a compromised app role cannot delete them.
- **Cadence:** daily snapshots + continuous WAL; retention per the organization’s
  audit-trail horizon.

---

## 5. Production deployment deltas

The local Compose stack is deliberately close to production shape; the deltas
below are turned into a step-by-step self-hosting guide (TLS, secrets,
Keycloak, upgrade/rollback) in [`deployment.md`](deployment.md):

| Concern            | Local                                   | Production                          |
|--------------------|-----------------------------------------|-------------------------------------|
| TLS                | plain HTTP on `127.0.0.1`               | TLS-terminating reverse proxy; HSTS |
| Ingress            | per-service `127.0.0.1` ports           | single hostname behind the proxy    |
| OIDC issuer        | split (browser vs container)            | one URL resolves both sides         |
| Secrets            | `.env`                                  | secrets manager / orchestrator      |
| Keycloak mode      | `start-dev`                             | `start --optimized`, TLS, managed DB |
| Resource limits    | none                                    | `mem_limit`/`pids_limit` per service |
| Root filesystem    | writable                                | `read_only: true` + `tmpfs`         |
| Images             | built locally                           | signed, scanned, pinned-by-digest   |
| Backups            | local dir                               | off-host, immutable, tested restores|
| Observability      | env vars only                           | metrics + traces + centralized logs |

### OIDC issuer in production
Behind a single TLS fronted hostname (e.g. `https://nisaba.example`) the reverse
proxy routes `/realms/nisaba` to Keycloak internally and the browser uses the
same hostname externally. `NISABA_OIDC_ISSUER` and
`VITE_OIDC_ISSUER` must match that URL. Sync validates the same issuer but
may fetch JWKS through an internal URL. `NISABA_OIDC_DISCOVERY_URL` is reserved
and has no effect.

### Compile worker sizing

The server bounds its worker cache and concurrent compiles, with idle and LRU
eviction. Adjust the [compile limits](../services/compile/README.md#runtime-configuration)
using measured memory and latency for representative projects. An HTTP timeout
does not stop a running Typst compile.

---

## 6. Runbook (common ops)

| Task                  | Command                                   |
|-----------------------|-------------------------------------------|
| View status           | `docker compose ps`                        |
| Tail logs             | `just logs` / `just logs app`              |
| psql (app role)       | `just psql`                                |
| psql (admin)          | `just psql-admin`                          |
| SeaweedFS shell       | `just s3 ls s3://nisaba-blobs`             |
| Recreate infra only   | `just down && just up`                     |
| Nuke all data         | `just down-volumes`                        |
| Validate compose (your .env) | `just compose-check`                       |
| Validate compose (.env.example, temp env) | `just compose-validate`    |
| Smoke-test the infra tier    | `just smoke`                               |
| Backup / restore      | `just backup` / `just restore <dir>`       |
| Verify a backup       | `just verify-backup <dir>`                 |
| Full app smoke        | `just e2e`                                 |
| Build one service img | `just image nisaba-compile`                |
