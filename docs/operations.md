# Operations

Day-2 operations for the Nisaba stack: bring-up, health, observability,
backup/restore, and the production deltas. Pairs with
[`security.md`](security.md) (least privilege), [`architecture.md`](architecture.md)
(service contracts), [`deployment.md`](deployment.md) (the self-hosting
walkthrough built on the §5 deltas), and
[`configuration.md`](configuration.md) (the environment-variable reference).

> **Status:** the local infrastructure and application tiers, health probes, and
> backup scripts run today. Metrics/OTLP export, high-availability deployment, and
> production failure drills remain planned. Treat the Compose
> configuration as a local development stack, not a production manifest.

---

## 1. Quick start

```bash
cp .env.example .env          # then edit any change-me-* secrets
just up                        # Postgres + SeaweedFS + Keycloak (+ seaweedfs-init)

# verify
just compose-validate          # validate compose against .env.example (temp env)
just smoke                     # bring up infra, probe health + realm, tear down
docker compose ps
open http://127.0.0.1:8103     # Nisaba web (sign in: demo / demo)
open http://127.0.0.1:8090     # Keycloak admin (admin / <KEYCLOAK_ADMIN_PASSWORD>)
open http://127.0.0.1:9100     # SeaweedFS S3 endpoint (<NISABA_S3_ADMIN_KEY>)

# app tier (templates; builds the Rust/web images)
just up-all                     # app-profile up --build; injects the dev realm
                                # JWKS when NISABA_OIDC_JWKS_JSON is unset/empty
just e2e                        # full app-profile smoke: build images, dev token,
                                # compile→PDF through nginx, sync WS reachability
```

Run the local CI-equivalent checks:

```bash
just ci-local     # fmt-check, clippy, test (incl. doctests), deny, audit,
                  # verify, web-install, web-test, web-lint, web-build
```

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

## 3. Observability plan

Three signals. Logging is implemented today; metrics and tracing are planned.
The `OTEL_*` variables in `.env.example` are **reserved** — no service ships an
OTLP exporter or a `/metrics` endpoint yet:

### Logs
- Structured (`tracing`/`tracing-subscriber` in Rust; `RUST_LOG` controls
  verbosity). Compose caps log size per container (`json-file`, 10m × 3).
- **Production:** ship to a central log store with retention aligned to the
  audit-trail requirement. PII is present (author identities) —
  restrict access and mask at ingest where possible.

### Metrics
- **Plan:** Prometheus scrape of `/metrics` on each HTTP service (port shared
  with the app, or a sidecar). Key metrics to instrument first:
  - `compile`: warm-cache hit rate, cold vs warm compile latency histogram,
    resident memory per pinned project, OOM/eviction count.
  - `sync`: connected replicas, ops/sec, snapshot lag.
  - `app`: request rate/latency, export job duration, DB pool saturation.
- The `compile` memory metric is the one to watch: warm caches for
  large projects are the failure mode. **Instrument before engineering
  eviction.**

### Traces
- **Plan:** OpenTelemetry OTLP export to `OTEL_EXPORTER_OTLP_ENDPOINT`
  (`.env.example`); spans tagged by `service.name` and `nisaba.project_id`.
  The `project → compile` hop is the critical path to trace.

### A collector for local dev (optional)
Drop in `otel/opentelemetry-collector` + `prom/prometheus` + `grafana/grafana`
as a `telemetry` profile when the service streams ship exporters. Not included
today to avoid unused moving parts.

---

## 4. Backup & restore

Scripts: [`deploy/backup/backup.sh`](../deploy/backup/backup.sh),
[`deploy/backup/restore.sh`](../deploy/backup/restore.sh). Run via
`just backup` / `just restore <dir>`.

### What is backed up
- **Postgres** `nisaba` database: logical dump (`pg_dump --clean --if-exists`,
  gzipped). **A failed dump aborts the backup** — a snapshot without the
  database dump is reported as INCOMPLETE and `backup.sh` exits non-zero
  instead of printing a warning and continuing. Keycloak's DB is
  *not* in the dev backup (it is stateful identity; back it up separately in
  production).
- **SeaweedFS** `nisaba-*` buckets: `aws s3 sync` (versioned) to a local dir. The
  sync runs inside the `amazon/aws-cli` image via `--entrypoint /bin/sh`. **A
  failed sync aborts the backup** — since 2026-08-09 a snapshot without object
  storage is reported as INCOMPLETE and `backup.sh` exits non-zero instead of
  printing a warning and continuing, so a silently-empty `seaweedfs/` directory
  can never be mistaken for a good backup.
- **sync CRDT history** lives in the `nisaba-oplog` bucket itself (the
  `oplog/` and `snapshot/` key prefixes): since the sync service moved its
  durable stores onto S3, the bucket sync above covers it — there is no
  separate sync data volume to archive any more (the compose `sync-data`
  volume is gone; the filesystem store remains available for bare-metal runs
  via `NISABA_SYNC_STORE_BACKEND=fs`).

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
`NISABA_OIDC_DISCOVERY_URL` then collapse to one value, eliminating the local-dev
split (`architecture.md` §6).

### Compile worker sizing (do not pre-engineer)
Start one warm worker per active project and **instrument**
memory, and only add eviction/caps when data shows it is needed. The failure
mode is gradual and visible, not sudden.

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

---

## 7. Things deliberately deferred (do not regress into building them)

The complexity budget goes to references, review and templates — **not** to these:

- Cached cross-reference index + staleness flag for document previews.
- Local WASM browser preview (a latency optimisation against Word, the baseline).
- Compile-pool eviction / warm-project caps (instrument memory first).
- DOCX import/export, billing, multi-tenancy, SSO/SAML, mobile.

If an ops change risks reintroducing any of these as accidental scope, push back.
