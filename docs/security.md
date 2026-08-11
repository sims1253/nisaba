# Security model

Threat-informed defaults for the local stack and the production posture it is
shaped toward. This complements [`architecture.md`](architecture.md) (data flow)
and [`operations.md`](operations.md) (day-2 hardening/backups).

> **Status:** the controls below are **implemented in the local stack** unless
> marked *future*. The application services themselves are skeletons; their
> in-process security (authorisation enforcement, input validation) is the
> responsibility of the service streams and is out of scope here.

---

## 1. Guiding principles

1. **Least privilege by default.** Every principal gets the narrowest access
   that satisfies its job: separate DB roles, scoped S3 credentials, non-root
   container users, and segmented service networks.
2. **Fail closed.** Missing config is fatal (`${VAR:?}` in Compose), not a
   silent insecure default.
3. **No secrets in the repo.** `.env` is gitignored; only `.env.example`
   (placeholders) is committed.
4. **Defence in depth, but proportional.** Nisaba is not multi-tenant and does
   not hold payment data. Controls target an honest, authenticated
   user base and private-document integrity — not a public SaaS perimeter.

---

## 2. Identity & access

| Principal              | Scope                                         | How created                         |
|------------------------|-----------------------------------------------|-------------------------------------|
| `postgres` (superuser) | bootstrap + migrations only; never used by app | Compose `POSTGRES_USER`             |
| `nisaba_app`           | owns `nisaba` DB; no SUPERUSER/CREATEDB       | `deploy/postgres/init/10-init-databases.sh` |
| `keycloak`             | owns `keycloak` DB only                       | same init script                    |
| SeaweedFS admin (`nisaba-admin`) | bootstrap + admin only; never used by app | `deploy/seaweedfs/s3.json` (static identity) |
| `nisaba-app` S3 account| read/write/list/tag on `nisaba-*` buckets only | `deploy/seaweedfs/s3.json` (static identity) |
| Keycloak demo users    | local dev only (`demo`/`reviewer`/`reader`)   | `deploy/keycloak/nisaba-realm.json` |

- OIDC roles `author` / `reviewer` / `read-only` are mapped into access tokens
  and are the authorisation input for `app`.
- Brute-force protection is enabled in the realm (`bruteForceProtected: true`).
- **Future:** rotate demo credentials; enforce MFA for `reviewer` in trials.

### Application authorization (two layers)

Every `/projects/{id}/*` request passes two gates, both of which must allow it:

1. **IdP role claim** (`auth.rs::permitted`): `author` for manage/project-level
   actions, `author` **or** `reviewer` for nothing that mutates the baseline —
   since 2026-08-09 document PATCH/DELETE require the `author` role. Baseline
   writes are owner/author only; reviewers propose through the review layer.
2. **Project membership role** (`auth.rs::project_acl`): document-path writes
   are owner/author only; reads are open to every member; project-level writes
   (members, references, exports, share links, project metadata) are
   owner/author only. Non-members get 403.

Consequences that are intentional:

- A reviewer **cannot** delete or overwrite documents (previously possible via
  the REST API, allowing silent destruction of author work).
- A reviewer **can** read documents, history, audit, and the members list.
- Share-link tokens are stored **hashed** (SHA-256); the plaintext token is
  returned exactly once, at creation, and revocation works by hashing the
  presented token.

### The sync plane (WebSocket relay)

The sync service enforces the same role boundary on the CRDT transport:

- **Role intersection.** The role the sync relay grants is the *least
  privileged* of the membership role and the bearer's IdP roles claim. A
  `read-only` IdP user who redeems an `author` share link therefore stays
  `read-only` on the sync plane — the same answer the REST plane gives — and
  cannot push updates. (Previously the relay trusted the membership role alone,
  so share-link redemption could escalate an IdP `read-only` user to `author`
  capabilities over WebSocket.)
- **Reviewer text gate.** A reviewer may push updates that change the `review`
  container freely (suggestions, comments, accept/reject records), but an
  update that changes the `text` container is only accepted when:
  1. the room is empty — the update is the initial seed, and the resulting text
     must match the app's authoritative document body
     (`NISABA_SYNC_SEED_VERIFY_URL`; verification failures and verifier
     outages deny the seed, fail-closed), or
  2. the same update — or a recent update from the same peer (30 s window) —
     also changed the `review` container, matching how the web client emits
     suggestion records and the text they annotate as separate CRDT frames.
  This blocks a custom client from silently replacing the document text (the
  2026-08-09 QA finding "reviewer overwrites the baseline via WebSocket").
  Residual limitation: a client that *also* forges review records (a fake
  accept/suggestion item) can still change text through the transport gate;
  fully closing that requires a semantic review validator inside the sync
  service and is tracked in the roadmap.
- The sync service decodes every CRDT update **before** persisting it to the
  op log, so undecodable bytes are rejected at ingest (protocol error 4000,
  not the misleading 4500 "internal error").

### Client token storage

The web client stores the OIDC access token in `localStorage` (shared across
tabs) rather than `sessionStorage` (per-tab): the collaborative editor must let
a user work in several tabs/windows of the same project without signing in
again (found by the 2026-08-09 author-agent's two-tab sync test). The token is
short-lived (5 minutes in local dev) and refreshed in the background; the OIDC
PKCE pending state remains in `sessionStorage`. Trade-off: `localStorage`
survives tab/window close on shared machines — sign out (which clears the
token) before leaving a shared computer.

### Input validation

- NUL bytes and control characters are rejected (400) in project names,
  document paths/titles, member subjects, and reference metadata; document
  bodies reject NUL only (tabs/newlines are legitimate prose).
- Document paths must be safe project-relative paths (no `/`-prefix, no `\`,
  no `.`/`..` segments, no control characters, no surrounding whitespace).
- Reference DOIs are unique per project (409 on duplicates); metadata fields
  are length-capped so a single bad record cannot bloat every compile's
  injected `refs.yml`.
- Document paths are length-capped (1024 chars) like the other user-facing
  text fields, so a runaway path cannot bloat compile/export include lists.
- Fulltext uploads must actually look like PDFs (magic `%PDF-` header + `%%EOF`
  trailer), not merely declare `application/pdf`.

---

## 3. Network model

Four segmented Compose bridge networks restrict lateral access through explicit
membership. They are not marked `internal: true`: Docker 29 suppresses published
host ports for containers attached only to internal networks, which would make the
documented loopback-only Postgres, SeaweedFS, and browser OIDC endpoints unreachable.

| Network     | Members                         | Purpose                              |
|-------------|---------------------------------|--------------------------------------|
| `db-net`    | postgres, keycloak, app         | SQL traffic                          |
| `obj-net`   | seaweedfs, seaweedfs-init, app  | S3-compatible object traffic         |
| `oidc-net`  | keycloak, app                   | app ↔ Keycloak backchannel           |
| `svc-net`   | app, sync, compile, web         | application service mesh             |

Consequences:

- **Postgres and SeaweedFS are unreachable from `web`** (web is only on
  `svc-net`). A browser compromise cannot talk to the data stores directly.
- **`compile` is on `svc-net` only** — it has no route to Postgres (not on
  `db-net`), no route to Keycloak (not on `oidc-net`), and no S3 credentials or
  `obj-net` membership. It is a pure function of the sources `app` sends it,
  reachable solely by `app` at `compile:8080`, and never proxied by nginx. The
  shared `NISABA_COMPILE_TOKEN` authorises `app → compile` calls and is injected
  into those two containers only.
- Network membership, credentials, and explicit loopback port bindings—not an
  `internal` network flag—enforce the local isolation boundaries. Production
  should additionally apply outbound firewall policy where egress restriction is required.

All published ports bind to `127.0.0.1` (loopback) only — the stack is not
exposed on other interfaces. In production, a TLS-terminating reverse proxy is
the only ingress (see `operations.md`).

| Published (host)    | Bound to     | Purpose                     |
|---------------------|--------------|-----------------------------|
| `127.0.0.1:5433`    | postgres     | dev DB access               |
| `127.0.0.1:9100`    | seaweedfs    | S3 API (no console port)    |
| `127.0.0.1:8090`    | keycloak     | browser OIDC + admin        |
| `127.0.0.1:8100/8101/8103` | app/sync/web | dev access to services |

`compile` intentionally has **no** published host port — it is internal-only.

---

## 4. Container hardening

- **Non-root users.** Rust services run as uid `65532` (Dockerfile.rust); the
  web container uses `nginxinc/nginx-unprivileged` (uid `101`). No service runs
  as root.
- **`no-new-privileges:true`** is set on every Compose service.
- **Minimal runtime image.** Rust runtime is `debian:bookworm-slim` carrying
  only the binary, `tini`, `ca-certificates` and `curl` (for `/healthz`). No
  toolchain, no shell tooling beyond what the healthcheck needs.
- **tini** as PID 1 for correct signal handling and zombie reaping.
- **Resource caps** *future*: add `mem_limit`/`pids_limit` and a read-only
  rootfs (`read_only: true` + `tmpfs` for `/tmp` and the home dir) once the
  warm-worker memory envelope is measured. Do not
  pre-engineer eviction; instrument first.

### Distroless as a future hardening
Switching the Rust runtime to `gcr.io/distroless/cc-debian12:nonroot` removes
the shell and `curl` at the cost of a compiled-in or sidecar healthcheck. Tracked
as a hardening task; not MVP-blocking.

---

## 5. Secrets handling

- `.env` is the single source for local secrets and is gitignored
  (`.gitignore`: `.env`, `.env.*`, `!.env.example`).
- **No web client secret exists.** `nisaba-web` is a **public** OIDC client
  using authorisation-code + PKCE (`S256`); the browser proves possession of
  the code verifier instead of a shared secret. The app verifies tokens by
  **signature** against the JWKS in `NISABA_OIDC_JWKS_JSON` (read inline at
  startup; empty → deny all), never by client secret.
- `NISABA_COMPILE_TOKEN` is the shared secret for `app → compile` calls. It is
  injected into the `app` and `compile` containers only — never `sync`, `web`,
  or the public nginx surface. `compile` enforces it on `POST /compile` (bearer
  token) in production mode (`NISABA_COMPILE_MODE=production`, the default).
- `NISABA_SYNC_AUTHZ_TOKEN` is a separate machine secret for `sync → app`
  document authorization. It is injected into `sync` and `app` only. The app
  compares SHA-256 digests in constant time, looks up the document UUID before
  checking membership, and returns `403` for unknown documents or members to
  avoid enumeration. Production app startup requires this secret; development
  and test modes may omit it, but then the endpoint denies all requests.
- **The Keycloak realm is dev-only** (`sslRequired: "none"`, demo users, public
  client) — production must replace every default; see the production checklist
  in [`deploy/keycloak/README.md`](../deploy/keycloak/README.md).
- **Future (production):** secrets via Docker Swarm/Kubernetes secrets, Vault,
  or cloud secret manager; Compose `secrets:` with `external: true`; never bake
  production secrets into images.

---

## 6. Supply chain

- [`deny.toml`](../deny.toml) enforces an allowlist of OSI/FSF-approved licences
  compatible with AGPL-3.0, denies the `openssl` crate in favour of
  rustls, forbids wildcard/git dependencies, and warns on duplicate versions.
- `cargo audit` runs in CI (`.github/workflows/security.yml`) against the
  RustSec advisory DB.
- CI uses pinned, reviewed base images; application images are built
  multi-stage so the toolchain never ships.
- **Future:** `cargo vet`, reproducible builds, SBOM generation (`cargo cyclonedx`).

---

## 7. Data protection & integrity

- Full-text PDFs and op-log snapshots are stored in **versioned** SeaweedFS
  buckets, so an accidental overwrite/delete is recoverable — important because
  export filenames are derived from reference entries and must remain stable.
- Backups are local-only for dev (`deploy/backup/`); production backup/restore
  is in [`operations.md`](operations.md).
- **Privacy note (out of scope for infra):** Nisaba may process unpublished
  private documents. At-rest encryption and a full data-impact review are product/ops
  responsibilities called out here so they are not lost.

---

## 8. CI gating (what blocks a merge)

See `.github/workflows/`. The intended gates:

- `rust`: `cargo fmt --check`, `cargo clippy --workspace`, `cargo test --workspace`,
  `cargo deny check`.
- `web`: build + test + lint (once the web stream provides configs).
- `security`: `cargo audit` on schedule and on PRs.
- `tools`: `tools/verify.sh` when present.

These are real gates for Rust today; the web/tools gates are tolerant until the
respective streams land their artifacts.
