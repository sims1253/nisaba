# Production Release Checklist

This checklist applies to a production deployment that may hold sensitive data;
it is not a checklist for publishing the source repository. Every item requires evidence from
the target release and deployment environment. Unchecked items are known gaps, not implied
capabilities.

## Pre-release verification

### Fresh install
- [ ] Clean host: `docker compose up -d` brings up all infrastructure
- [ ] Migrations run successfully against a fresh database
- [ ] MinIO buckets are created and accessible
- [ ] Keycloak realm imports and the test user can authenticate
- [ ] App service healthcheck passes (`/health/ready`)
- [ ] Compile service healthcheck passes (`/healthz`)
- [ ] Sync service healthcheck passes (`/health/ready`)

### Upgrade path
- [ ] Forward migration: previous version → current migrations apply cleanly
- [ ] Rolling upgrade: services restart without data loss
- [ ] Rollback: previous version can read the current schema

### Backup and restore
- [ ] `just backup` produces a valid snapshot
- [ ] `just verify-backup` validates the snapshot structure
- [ ] `just restore` recovers from a backup on a clean volume
- [ ] PITR (point-in-time recovery) works for Postgres

### Failure drills
- [ ] Kill the app process mid-write: no data loss (op log + sync recovery)
- [ ] Kill the sync process mid-update: client reconnects and catches up
- [ ] Kill the compile process mid-compile: permit released, job retried
- [ ] Database disconnect: app returns 503 on `/health/ready`
- [ ] Object store outage: compile results cached, export deferred
- [ ] Corrupt snapshot: recovery via op-log replay

### Security
- [ ] Secret rotation: tokens, JWKS, and S3 credentials can be rotated
- [ ] Audit log: tamper evidence (hash chain or append-only)
- [ ] Log redaction: no secrets in structured logs
- [ ] Upload quarantine: file type and size validation enforced

### Supply chain
- [x] `cargo deny check` passes (licenses, advisories, bans, sources)
- [x] `cargo audit` passes (vulnerability database)
- [x] Docker images are digest-pinned
- [ ] SBOM/provenance attestation generated
- [x] `--locked` builds verify Cargo.lock integrity

### Performance
- [ ] Cold compile benchmark recorded (no warm cache)
- [ ] Warm compile benchmark recorded (cached project)
- [ ] 1000-page document compiles within timeout
- [ ] Large file tree loads without blocking UI

## Post-release monitoring

- [ ] Metrics: compile_ms, pdf_ms, cache hit rate, RSS memory
- [ ] Tracing: spans exported to OTLP collector
- [ ] Alerting: healthcheck failures, high latency, OOM
- [ ] Capacity: compile queue depth, worker pool utilization
