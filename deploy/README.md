# deploy/

Deployment definitions, templates, and bootstrap scripts for Nisaba. These are
root-level **developer-experience / operations** files; application source lives
in `crates/`, `services/`, `web/`, `tools/` (owned by those workspaces).

## Layout

```
deploy/
  Dockerfile.rust                      # multi-stage Rust service image (template)
  Dockerfile.rust.dockerignore         # BuildKit sidecar context filter
  Dockerfile.web                       # multi-stage web image (bun → nginx)
  Dockerfile.web.dockerignore
  web/nginx.conf                       # non-root nginx: SPA + /api→app + /sync→sync + /healthz
  web/nginx-security-headers.conf      # security headers + CSP Report-Only (included by nginx.conf)
  postgres/init/10-init-databases.sh   # least-privilege roles + databases
  seaweedfs/generate-s3-identities.sh  # S3 identities generated from env at container start
  seaweedfs/init-buckets.sh            # buckets + versioning (one-shot)
  keycloak/nisaba-realm.json           # OIDC realm (DEV-ONLY): client, roles, demo users
  keycloak/README.md                   # incl. production replacement checklist
  keycloak/healthcheck.sh              # /health/ready probe (bash /dev/tcp; no curl in image)
  backup/backup.sh                     # pg_dump + aws s3 sync + sync-fs tar (local)
  backup/restore.sh                    # restore a snapshot (overwrites data)
  backup/verify.sh                     # structural check of a snapshot (no restore)
  validate-compose.sh                  # `docker compose config` against .env.example (temp env)
  smoke.sh                             # bring up infra, probe health + realm, tear down
  e2e-app.sh                           # full app-profile smoke: dev token, compile→PDF via
                                       #   nginx, sync WS handshake (throwaway project)
  dev-token.py                         # mints a dev RSA key + JWKS + JWT (stdlib + openssl; `uv run`)
  sync-handshake.py                    # drives one real sync HELLO over a raw WebSocket and
                                       #   asserts the returned frame (stdlib; `uv run`)
```

## Two tiers

1. **Infrastructure** — brought up by `docker compose up` (default profile):
   Postgres, SeaweedFS, Keycloak, and the one-shot `seaweedfs-init`.
2. **Application** — brought up by `docker compose --profile app up --build`:
   `app`, `sync`, `compile`, `web`. These build from `Dockerfile.rust` /
   `Dockerfile.web`.

## The app images build and run with the durable adapters

The Rust service images build and the binaries launch correctly — each binds
`0.0.0.0:8080` via its own address env var (`NISABA_APP_ADDR` /
`NISABA_SYNC_ADDR` / `NISABA_COMPILE_ADDR`) and answers `GET /healthz`
(sync also serves `/health/ready`). The `app` binary always constructs the
**durable** adapters — `PostgresRepository` and `S3BlobStore`
(`services/app/src/main.rs`) — and runs the embedded SQLx migrations at
startup; an unreachable database or object store is fatal, not degraded.
In-memory adapters are compiled only for unit tests. OIDC tokens are verified
against the JWKS read inline from `NISABA_OIDC_JWKS_JSON` (empty → deny all).
This is why the app tier lives behind the `app` profile: `just up` gives you
working infra; `just up-all` exercises the image build/run pipeline; `just e2e`
adds a full app-profile smoke (dev token + compile-through-nginx + sync WS).

## Building a single image

```bash
just image nisaba-compile        # = docker build -f deploy/Dockerfile.rust \
                                 #         --build-arg SERVICE=nisaba-compile -t nisaba-compile .
just image-web                   # builds the SPA image
```

## Health checks

Defined on every service in `docker-compose.yml` and (for built images) in the
Dockerfiles. Contract: `GET /healthz` → `200 ok`. See
[`docs/operations.md`](../docs/operations.md) §2.

## Routing & isolation (nginx)

The `web` nginx reverse-proxies two internal services and **nothing else**:

- `/api/*` → `app`; exact `/api/compile` is forwarded verbatim because the app
  owns that route, while the general `/api/` rule strips the prefix for CRUD
  calls (`/api/projects` → `/projects`).
- `/sync/*` → `sync` (WebSocket upgrade; `sync` serves `/sync/{doc_id}`).
- The `app` sync authorization endpoint is internal-only at
  `/internal/sync/authorize`; it accepts only the machine Bearer secret and is
  not exposed through an nginx `/api` route.
- `compile` is **never** proxied by nginx. It is internal-only on `svc-net`,
  reachable solely by `app` at `compile:8080`, with the shared
  `NISABA_COMPILE_TOKEN` authorising each call. It has no published host port.

## Network model & least privilege

Four segmented Compose bridge networks restrict lateral access through
membership. None of them is marked `internal: true` — deliberately: recent
Docker releases suppress published host ports for containers attached only to
internal networks, which would break the documented loopback-only access to
Postgres, SeaweedFS, and Keycloak. Isolation instead comes from network
membership, scoped credentials, and explicit `127.0.0.1` port bindings.
[`docs/security.md`](../docs/security.md) §3 is the normative description
(member table, published ports, and the consequences for `web` and `compile`).

## Conventions

- All host ports bind to `127.0.0.1` only (`compile` has none at all).
- Every container runs as a non-root user and sets `no-new-privileges`.
- Bootstrap scripts are idempotent (`|| true` on "already exists") so the stack
  can be re-upped cleanly.
- The Keycloak realm is a dev-only fixture; production must replace every
  default (see `deploy/keycloak/README.md`).
