# Deployment (self-hosting)

A production deployment walkthrough for the Docker Compose stack: how it
differs from local development, how to front it with TLS, how to source
secrets, and how to upgrade and roll back. It pairs with
[`operations.md`](operations.md) (day-2 runbook) and
[`security.md`](security.md) (threat model); the consolidated
environment-variable reference is [`configuration.md`](configuration.md).

> **Status — read this first.** The Compose stack is a *local development*
> stack that is deliberately close to production shape. Everything below is
> written from the actual configuration, but the **full procedure has not been
> exercised on a production host**; the upgrade/rollback and failure-drill
> items in [`release-checklist.md`](release-checklist.md) are still unchecked.
> Nisaba is pre-release software — do not trust it with irreplaceable data.

---

## 1. What you are deploying

Two tiers, both from `docker-compose.yml`:

- **Infrastructure** (`docker compose up -d`): Postgres 17, SeaweedFS
  (S3-compatible object storage), Keycloak 26 (OIDC), one-shot bucket init.
  These images are pinned by digest and pulled, not built.
- **Application** (`--profile app`): `app`, `sync`, `compile` (Rust, built from
  `deploy/Dockerfile.rust`) and `web` (SPA + nginx, built from
  `deploy/Dockerfile.web`). The `app` binary runs SQLx migrations at startup
  and exits if Postgres or the object store is unreachable.

All services bind published ports to `127.0.0.1` only, so a reverse proxy on
the same host is the single ingress. `compile` has no published port at all.

## 2. Differences from local development

The authoritative table is [`operations.md`](operations.md) §5 ("Production
deployment deltas"). The ones that change what you do:

| Concern | Local dev | Self-hosting |
|---------|-----------|--------------|
| TLS | none (loopback HTTP) | TLS-terminating reverse proxy, single hostname, HSTS |
| OIDC issuer | split browser/container hostnames | one external URL (§5 below) |
| Keycloak | `start-dev --import-realm`, demo realm | `start --optimized`, own realm, no demo users |
| Secrets | `.env` beside the checkout | generated secrets, kept outside the repo (§4) |
| Restart policy | infra: `unless-stopped`; app tier: none | add `restart: unless-stopped` for the app tier (§7) |
| Backups | local dir | off-host copies of every snapshot (§8) |

## 3. Host prerequisites

- Docker Engine with Compose v2, and `just` (the commands below use it; the
  equivalent `docker compose` invocations are in the [`justfile`](../justfile)).
- A DNS name for the deployment (e.g. `nisaba.example`) — the OIDC issuer and
  the browser both need one stable origin.
- Ports 80/443 free for the reverse proxy; nothing else needs to be exposed.
- Root is only needed for the proxy (Let's Encrypt binding :80/:443); the
  containers themselves run non-root.

## 4. Secrets: keep them out of the repo checkout

Local dev copies `.env.example` to `.env` in the repo. For a deployment,
generate every secret and store the file **outside** the working tree (or in a
secret manager), then point Compose at it:

```bash
cp .env.example /etc/nisaba/env    # or wherever you keep it, NOT in the repo
# generate real secrets for at least:
#   POSTGRES_PASSWORD NISABA_DB_PASSWORD KEYCLOAK_DB_PASSWORD
#   KEYCLOAK_ADMIN_PASSWORD NISABA_COMPILE_TOKEN NISABA_SYNC_AUTHZ_TOKEN
#   NISABA_S3_ADMIN_SECRET NISABA_S3_SECRET_KEY
# e.g.  openssl rand -hex 32
docker compose --env-file /etc/nisaba/env --profile app up -d --build
```

Notes:

- `POSTGRES_PASSWORD`, `NISABA_DB_PASSWORD`, `KEYCLOAK_DB_PASSWORD`,
  `NISABA_COMPILE_TOKEN` are hard-required (`${VAR:?}` in `docker-compose.yml`);
  a blank `NISABA_SYNC_AUTHZ_TOKEN` aborts app startup in production mode.
- The machine secrets (`NISABA_COMPILE_TOKEN`, `NISABA_SYNC_AUTHZ_TOKEN`,
  S3 keys) must be different from any local-dev value.
- SeaweedFS identities are generated at seaweedfs container start from the
  `NISABA_S3_*` values (`deploy/seaweedfs/generate-s3-identities.sh`); set the
  generated secrets in the environment file before first boot — they take
  effect on the next container start.
- Docker/orchestrator secrets (`secrets: external: true`) are future work
  (see [`security.md`](security.md) §5); environment files are today's mechanism.
- The `just` recipes and `deploy/backup/*.sh` scripts source the repo-root
  `.env` when it exists and call `docker compose` without `--env-file`. If you
  keep the environment file elsewhere, either export the variables in your
  shell before running them or invoke `docker compose --env-file …` directly,
  as shown above and in §7–§9.

## 5. Keycloak for production

The bundled realm is a **dev fixture** (public PKCE client with demo users,
`sslRequired: "none"`). You must replace it — the complete checklist is in
[`deploy/keycloak/README.md`](../deploy/keycloak/README.md). In short:

- Build and run Keycloak in production mode: `kc.sh build` once, then
  `start --optimized` (instead of the compose file's `start-dev
  --import-realm`), behind the TLS proxy on your single hostname. Manage the
  realm (client, roles, mappers, users) through the admin console or your own
  realm export — do not import `nisaba-realm.json`.
- Recreate, on your production client, the mappers the app depends on: the
  top-level `roles` claim and the audience mapper (`aud` containing
  `nisaba-web` and/or `nisaba` — see
  [`deploy/keycloak/README.md`](../deploy/keycloak/README.md)).
- Set `NISABA_OIDC_ISSUER` (and `NISABA_OIDC_DISCOVERY_URL`) to the external
  `https://…/realms/<realm>` URL. Behind one TLS hostname the local-dev
  browser/container issuer split collapses to this single value
  ([`operations.md`](operations.md) §5).
- Populate `NISABA_OIDC_JWKS_JSON` from the production realm's
  `.../protocol/openid-connect/certs` endpoint (the app reads it inline at
  startup; empty rejects every token). Rotate it when the realm's signing
  keys rotate.
- Give `sync` its production OIDC variables pointing at the same realm
  (`NISABA_SYNC_OIDC_ISSUER`, `NISABA_SYNC_OIDC_AUDIENCE`,
  `NISABA_SYNC_OIDC_JWKS_URL` — see [`configuration.md`](configuration.md));
  unset `NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME`.

## 6. TLS reverse proxy

One hostname fronts everything: `/` (SPA), `/api/*` (app) and `/sync/*`
(WebSocket) are already wired inside the `web` container's nginx, so the outer
proxy only needs `web` plus the Keycloak paths for the browser OIDC redirects.

Caddy example (automatic Let's Encrypt certificates):

```caddyfile
nisaba.example {
    encode zstd gzip

    # Keycloak browser-facing paths (login pages, JS, certificates)
    handle /realms/*     { reverse_proxy 127.0.0.1:8090 }
    handle /resources/*  { reverse_proxy 127.0.0.1:8090 }
    handle /js/*         { reverse_proxy 127.0.0.1:8090 }
    handle /admin/*      { reverse_proxy 127.0.0.1:8090 }

    # Everything else: SPA + /api + /sync (already proxied inside the web image)
    reverse_proxy 127.0.0.1:8103
}
```

nginx equivalent (certificate management omitted):

```nginx
server {
    listen 443 ssl;
    http2 on;
    server_name nisaba.example;
    ssl_certificate     /etc/ssl/nisaba/fullchain.pem;
    ssl_certificate_key /etc/ssl/nisaba/privkey.pem;
    add_header Strict-Transport-Security "max-age=31536000" always;

    location /realms/     { proxy_pass http://127.0.0.1:8090; }
    location /resources/  { proxy_pass http://127.0.0.1:8090; }
    location /js/         { proxy_pass http://127.0.0.1:8090; }
    location /admin/      { proxy_pass http://127.0.0.1:8090; }

    location / {
        proxy_pass http://127.0.0.1:8103;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;     # /sync WebSocket
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto https;
    }
}
```

Two build-time consequences (the web image bakes its OIDC settings — see
[`configuration.md`](configuration.md)):

- `VITE_OIDC_ISSUER` must be the **external** `https://…` issuer, so the image
  must be (re)built after changing it.
- The Keycloak client's redirect URIs must include `https://nisaba.example`
  (the dev realm only lists localhost origins).

Both examples are **untested templates**: exact Keycloak fronting (hostname
settings, admin exposure, backchannel URLs) varies by Keycloak version —
verify against the Keycloak production documentation for your release, and
keep the admin console off the public hostname if you do not need it.

## 7. First boot checklist

```bash
docker compose --env-file /etc/nisaba/env up -d            # infra
docker compose --env-file /etc/nisaba/env --profile app up -d --build
docker compose --env-file /etc/nisaba/env ps               # wait for "healthy"
```

- Every service reports `GET /healthz` → 200 (`app`/`sync` also serve
  `/health/ready`); `app` readiness includes a database check
  ([`operations.md`](operations.md) §2).
- Log in through the browser once, create a throwaway project, compile it, and
  confirm a second user can open it — that exercises OIDC, Postgres, S3, and
  the sync WebSocket end to end. (`just e2e` does the same against a
  throwaway stack with a dev token.)
- Take a first backup (§8) and **verify** it while the system is still small.
- For unattended hosts: the app-tier services currently define no `restart:`
  policy in `docker-compose.yml` (the infra services use `unless-stopped`).
  Add `restart: unless-stopped` for app/sync/compile/web via a Compose overlay
  until the default changes upstream.

## 8. Upgrade procedure

```bash
# 1. Back up FIRST, and verify the snapshot
just backup
just verify-backup artifacts/backups/<timestamp>

# 2. Move to the new release
git fetch --tags
git checkout <new-tag>

# 3. Rebuild and restart; SQLx migrations run automatically at app startup
docker compose --env-file /etc/nisaba/env --profile app up -d --build

# 4. Watch the app logs through the migration and confirm health
docker compose --env-file /etc/nisaba/env ps
docker compose --env-file /etc/nisaba/env logs -f app
```

- Copy every backup snapshot off the host (the scripts write locally under
  `BACKUP_LOCAL_DIR`; see [`operations.md`](operations.md) §4 for the
  production backup deltas — off-host, immutable targets, scheduled restore
  drills).
- Tabs open across the upgrade keep running the previous build's code (the SPA
  is cached in the browser) and can write stale-shaped data through the sync
  relay. There is deliberately no cross-version compatibility before the first
  release: tell users to reload open tabs after an upgrade lands.
- The upgrade path (forward migration on existing data) is a release-checklist
  item that has **not** been evidence-verified; test the upgrade on a copy of
  production data before doing it live.

## 9. Rollback procedure

```bash
# 1. Stop the app tier (volumes are preserved by `down`)
docker compose --env-file /etc/nisaba/env --profile app down

# 2. Return to the previous release
git checkout <previous-tag>

# 3. Rebuild and start
docker compose --env-file /etc/nisaba/env --profile app up -d --build
```

If the new release already ran migrations the old code cannot read, restore
the pre-upgrade snapshot taken in §8 (this **overwrites current data**):

```bash
docker compose --env-file /etc/nisaba/env --profile app down
just restore /path/to/artifacts/backups/<pre-upgrade-timestamp>
docker compose --env-file /etc/nisaba/env --profile app up -d
```

`just restore` runs [`deploy/backup/restore.sh`](../deploy/backup/restore.sh),
which reloads the Postgres dump, re-syncs the SeaweedFS buckets, and unpacks
the sync op-log/snapshot tar. Expect to lose everything written between the
snapshot and the rollback — which is why the backup in §8 step 1 is not
optional. A tested rollback is itself an open release-checklist item; treat
this as the procedure to rehearse, not a proven one.

## 10. Deliberately not covered here

- High-availability or multi-node deployment (the baseline is one machine).
- Metrics/traces/alerting — no exporters ship yet; see
  [`operations.md`](operations.md) §3.
- Zero-downtime upgrades (Compose `--build` recreates containers; brief
  downtime is expected and acceptable at this scale).
