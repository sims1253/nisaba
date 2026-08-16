# `nisaba-sync`

The sync service for Nisaba: a Loro 1.13.x CRDT **authority and relay** keyed by
document id, with presence/awareness, an append-only op log, periodic snapshots,
and a pluggable snapshot store. It implements the collaborative-editing core of
the product.

## Capabilities

- **WebSocket authority/relay** keyed by document id (`GET /sync/{doc_id}`).
- **Binary CRDT import/export** — opaque Loro update bytes; the relay never
  inspects CRDT state.
- **Reconnect catch-up** via version vectors (`ExportMode::Updates { from }`),
  with a full-snapshot fallback for fresh peers or unrecoverable gaps.
- **Presence/awareness** with heartbeat expiry (injectable clock; TTL sweeper).
- **Role-aware access seam** — `author` / `reviewer` / `read-only`, resolved
  through a pluggable `AccessResolver`. Read-only peers cannot push updates.
- **Append-only op log** (`OpLogStore`) and **pluggable snapshot store**
  (`SnapshotStore`); filesystem implementations stand in for the S3-compatible
  blob boundary. A room hydrates from the latest snapshot + op-log replay.
- **Periodic snapshots** — event-driven (every N updates) plus a time-based
  maintenance floor.
- **Health endpoints** — `GET /health`, `GET /healthz` (k8s liveness alias),
  `GET /health/ready`.
- **Limits/security** — document-id validation (path-traversal-safe for the FS
  stores), update-size cap, per-document peer cap, presence-size cap.

## Layout

```
src/
  protocol.rs   binary wire framing (the versioned contract; see fixtures/sync/PROTOCOL.md)
  config.rs     limits, security validation, DocId/PeerId
  auth.rs       roles, capability set, AccessResolver seam (StaticAccessResolver for dev)
  http.rs       injectable outbound HTTP transport (HttpFetch; ReqwestHttpFetch under `server`)
  oidc.rs       production OIDC/JWT resolver: JWKS cache, JwtValidator, document authorizer
  authority.rs  AuthorityDoc: LoroDoc wrapper (import / catch-up / snapshot)
  op_log.rs     OpLogStore trait + FsOpLogStore + MemoryOpLogStore (append-only)
  snapshot.rs   SnapshotStore trait + FsSnapshotStore + MemorySnapshotStore
  presence.rs   ephemeral roster + heartbeat expiry + roster codec
  room.rs       DocRoom: authority + relay + presence + persistence (coordination)
  registry.rs   DocRegistry: live rooms + shared stores
  session.rs    per-connection WebSocket session (server feature)
  server.rs     axum app, health, ws upgrade (server feature)
  main.rs       binary: FS stores, maintenance tasks, serve
tests/          convergence, reconnect, presence, persistence, limits, e2e
```

The pure CRDT core has no server dependency; the HTTP/WebSocket server lives
behind the `server` feature (on by default). `cargo test --no-default-features`
exercises the headless core; `cargo test` runs the full suite including the
end-to-end WebSocket tests.

## Run

```sh
# Local dev: grant author to any non-empty token (NEVER in production).
NISABA_SYNC_DEV_ALLOW_ALL=1 \
NISABA_SYNC_DATA_DIR=./data \
PORT=8080 \
cargo run -p nisaba-sync
```

By default **no token is accepted** — every HELLO is denied with `FORBIDDEN`
(safe by default). Local dev sets `NISABA_SYNC_DEV_ALLOW_ALL=1` to grant `author`
to any non-empty token. Production enables **OIDC mode**: the service validates
the bearer JWT against JWKS and then asks the `app` service to authorize the
subject for the specific document (see
[Authentication & authorization](#authentication--authorization)). Sync does
not build identity/login itself.

**Bind address** — resolved in order: `NISABA_SYNC_ADDR` (full `host:port`), then
`PORT` (bare port, bound on `0.0.0.0`), then the default `0.0.0.0:8080`.

Environment (connectivity): `NISABA_SYNC_ADDR`, `PORT`, `NISABA_SYNC_DATA_DIR`,
`NISABA_SYNC_DEV_ALLOW_ALL`, `RUST_LOG`. Authentication variables are listed in
[Authentication & authorization](#authentication--authorization).

## Authentication & authorization

Every WebSocket `HELLO` carries a document id and a bearer token. Sync resolves
the peer's role through a single `AccessResolver`, but the production resolver
(`OidcAccessResolver`) applies **two independent checks** so that a globally
valid token can never open an arbitrary document:

1. **JWT validation** (`JwtValidator`). Header `kid` → JWKS key lookup → the
   token algorithm must be in the allow-list **and** equal the matched JWK's
   configured algorithm (defeats RS↔HMAC confusion) → `iss`, `aud`, `exp` are
   verified. Only the explicit roles claim is read (default
   `realm_access.roles`, the Keycloak mapping); **scopes are never interpreted
   as roles**.
2. **Document authorization** (`DocumentAuthorizer`). Even after a valid JWT,
   the `(subject, document)` pair must be affirmatively allowed. The HTTP
   verifier (`HttpDocumentAuthorizer`) asks the `app` service; if no verifier is
   wired, `DenyAllAuthorizer` denies every document.

### Fail-closed guarantees

- Missing/empty/stale JWKS → deny (never an empty allow; no "try all keys").
- Any signature / claim / transport / timeout error → deny.
- No document authorizer wired → deny every document.
- JWKS refresh failure retains the previous keys (rotation overlap) but the
  `max_age` guard eventually fails closed once they go stale.
- Partial OIDC configuration (some-but-not-all of the three required vars) is a
  **fatal** startup error, not a silent deny-all.

### Modes (selected at startup, see `main.rs`)

| Mode | Trigger | Behaviour |
|------|---------|----------|
| **Deny-all** (default) | nothing configured | every token denied |
| **Dev allow-all** | `NISABA_SYNC_DEV_ALLOW_ALL` set | any non-empty token → `author`. **Never in production.** |
| **OIDC production** | `ISSUER`+`AUDIENCE`+`JWKS_URL` all set | JWT/JWKS validation + per-document verifier |

### Configuration

| Variable | Default | Meaning |
|----------|---------|---------|
| `NISABA_SYNC_OIDC_ISSUER` | — | expected `iss` claim (required for OIDC) |
| `NISABA_SYNC_OIDC_AUDIENCE` | — | expected `aud` claim (required for OIDC) |
| `NISABA_SYNC_OIDC_JWKS_URL` | — | JWKS endpoint reachable from the sync container (required for OIDC) |
| `NISABA_SYNC_OIDC_ROLES_CLAIM` | `realm_access.roles` | dotted path of the roles claim |
| `NISABA_SYNC_OIDC_ALGORITHMS` | `RS256,ES256` | comma-separated allow-list (HMAC discouraged) |
| `NISABA_SYNC_OIDC_LEEWAY_SECS` | `60` | `exp`/`nbf` leeway |
| `NISABA_SYNC_OIDC_JWKS_MAX_AGE_SECS` | `3600` | deny keys older than this without a refresh |
| `NISABA_SYNC_OIDC_JWKS_REFRESH_SECS` | `900` | background JWKS refresh interval |
| `NISABA_SYNC_OIDC_TOKEN_CACHE_TTL_SECS` | `60` | verified-token cache TTL (capped at token `exp`; `0` disables) |
| `NISABA_SYNC_AUTHZ_URL` | — | app document-authorization endpoint (unset → deny-all documents) |
| `NISABA_SYNC_AUTHZ_TOKEN` | — | service token for the authz endpoint (required if `AUTHZ_URL` set) |
| `NISABA_SYNC_AUTHZ_TIMEOUT_SECS` | `5` | per-call timeout (timeout → deny) |
| `NISABA_SYNC_HTTP_CONNECT_TIMEOUT_SECS` | `5` | outbound TCP+TLS handshake bound |
| `NISABA_SYNC_HTTP_REQUEST_TIMEOUT_SECS` | `10` | outbound whole-call bound |
| `NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME` | unset | permit `http://` (local dev only) |

### Document-authorization wire contract (the `app` side)

`HttpDocumentAuthorizer` issues exactly one request per authorized `HELLO`; the
`app` service must implement the server side. **Any** non-2xx status,
unparseable body, unknown role, transport error, or timeout is a **denial**.

```text
POST <NISABA_SYNC_AUTHZ_URL>
Authorization: Bearer <NISABA_SYNC_AUTHZ_TOKEN>
Content-Type: application/json
{ "subject": "<jwt sub>", "document": "<doc_id>" }

→ 200 { "role": "author" | "reviewer" | "read-only" }   // allow
→ 401 | 403 | 4xx | 5xx                                       // deny
```

The role strings mirror the `app` service mapping. The service token
is a machine credential injected into the `sync` and `app` containers only; it
is **not** the end-user's access token (sync validates that separately in stage 1).

## Design invariants

- **Opaque transport** — sync never inspects or re-serialises Loro state (a
  core-model principle). In particular, review-layer soft deletes (marks over CRDT positions,
  review semantics) pass through untouched: **no physical deletion assumptions**.
- **Presence is ephemeral** — never written to the op log or snapshots; it
  expires without a heartbeat.
- **Append-only** — the op log exposes no mutation other than `append`.
- **No eviction engineered yet** — the op log is not compacted after
  a snapshot; replay re-imports already-applied ops, which is a no-op in Loro.
