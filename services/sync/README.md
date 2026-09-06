# `nisaba-sync`

Loro CRDT authority and WebSocket relay, with ephemeral presence and durable
update logs and snapshots. Rooms are keyed by document ID.

- `GET /sync/{doc_id}` connects a peer using the
  [binary protocol](../../fixtures/sync/PROTOCOL.md). Reconnecting peers catch
  up by version vector, with a full-snapshot fallback.
- `GET /internal/docs/{doc_id}/state` supplies a snapshot for app exports and
  requires the shared service token.
- Filesystem and S3 backends persist updates and periodic snapshots. In-memory
  stores support tests.
- The service checks roles, reviewer updates, document IDs, payload sizes,
  peer counts, and frame rates.
- Health endpoints are `/health`, `/healthz`, and `/health/ready`.

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
  s3.rs         S3OpLogStore + S3SnapshotStore + S3Stores (feature `s3`)
  presence.rs   ephemeral roster + heartbeat expiry + roster codec
  room.rs       DocRoom: authority + relay + presence + persistence (coordination)
  registry.rs   DocRegistry: live rooms + shared stores
  session.rs    per-connection WebSocket session (server feature)
  server.rs     axum app, health, ws upgrade (server feature)
  main.rs       binary: store selection (fs/s3), maintenance tasks, serve
tests/          convergence, reconnect, presence, persistence, limits, e2e
```

The pure CRDT core has no server dependency; the HTTP/WebSocket server lives
behind the `server` feature (on by default). `cargo test --no-default-features`
exercises the headless core; `cargo test` runs the full suite including the
end-to-end WebSocket tests.

## Run

```sh
# Local dev: grant author to any non-empty token (NEVER in production),
# filesystem stores under ./data (the default backend).
NISABA_SYNC_DEV_ALLOW_ALL=1 \
NISABA_SYNC_DATA_DIR=./data \
PORT=8080 \
cargo run -p nisaba-sync

# Against the compose stack's SeaweedFS instead (what compose runs with):
NISABA_SYNC_DEV_ALLOW_ALL=1 \
NISABA_SYNC_STORE_BACKEND=s3 \
NISABA_S3_ENDPOINT=http://127.0.0.1:9100 \
NISABA_S3_ACCESS_KEY=nisaba-app NISABA_S3_SECRET_KEY=... \
NISABA_S3_BUCKET_OPLOG=nisaba-oplog \
PORT=8080 cargo run -p nisaba-sync
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
`NISABA_SYNC_DEV_ALLOW_ALL`, `RUST_LOG`. Storage variables
(`NISABA_SYNC_STORE_BACKEND` + the `NISABA_S3_*` set) are listed in
[Durable stores](#durable-stores-s3-key-layout). Authentication variables are
listed in [Authentication & authorization](#authentication--authorization).

## Authentication & authorization

Each `HELLO` contains a document ID and bearer token. The production
`OidcAccessResolver` validates the JWT, then asks the app service to authorize
the subject for that document. Mutating frames recheck access.

JWT validation checks the signing key, allowed algorithm, issuer, audience,
expiry, and non-empty subject. The algorithm must match the key. Roles come
from the configured roles claim, never from scopes. Missing or stale keys,
invalid claims, and authorization errors deny access. A failed JWKS refresh
retains previous keys until their maximum age.

### Startup modes

| Mode | Trigger | Behavior |
|------|---------|----------|
| Deny-all | No authentication configuration | Denies every token |
| Development | `NISABA_SYNC_DEV_ALLOW_ALL` set | Grants author to any non-empty token; local development only |
| OIDC | Issuer, audience, and JWKS URL all set | Validates JWT and document access |

Partial OIDC configuration is a startup error. Without a document authorizer,
OIDC mode denies access to every document.

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
| `NISABA_SYNC_AUTHZ_TOKEN` | — | shared service token: presented to the authz/seed endpoints **and** required by `GET /internal/docs/{id}/state` (unset → deny-all, fail-closed) |
| `NISABA_SYNC_AUTHZ_TIMEOUT_SECS` | `5` | per-call timeout (timeout → deny) |
| `NISABA_SYNC_HTTP_CONNECT_TIMEOUT_SECS` | `5` | outbound TCP+TLS handshake bound |
| `NISABA_SYNC_HTTP_REQUEST_TIMEOUT_SECS` | `10` | outbound whole-call bound |
| `NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME` | unset | permit `http://` (local dev only) |

### Document-authorization wire contract (the `app` side)

`HttpDocumentAuthorizer` calls the app service during authorization. A non-2xx
status, invalid body, unknown role, transport error, or timeout denies access.

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
is separate from the end-user access token that sync validates.

### Internal state read API (the `app` → `sync` direction)

Exports need each document's review marks, and review state lives in the CRDT
this service relays — so the app reads a document's whole state back on an
authenticated internal path:

```text
GET /internal/docs/{doc_id}/state
Authorization: Bearer <NISABA_SYNC_AUTHZ_TOKEN>

→ 200 application/octet-stream   // whole current state as an opaque Loro snapshot
→ 204                            // the document has no state anywhere
→ 400                            // invalid document id
→ 401 | 403                      // missing / wrong service token
→ 500                            // store or export failure
```

A 204 means the document has no synced state. A 404 is an error: it may mean
the caller reached an incorrect route or an incompatible service version.

The endpoint compares a SHA-256 digest of the shared token in constant time;
an unset or empty token denies access. Live rooms export their current state.
Otherwise, the service reconstructs state from snapshots and the update log
without registering a room. The app interprets review records from the snapshot.

The web nginx does not proxy `/internal/`. The sync service also has a
loopback-only published port in the development Compose stack.

## Durable stores (S3 key layout)

The op log and snapshots are the authority for every collaborative document.
Two interchangeable implementations back the same traits:

- **`fs`** (default outside compose): one append-only file per document plus a
  per-document snapshot directory under `NISABA_SYNC_DATA_DIR`.
- **`s3`** (what compose runs, `NISABA_SYNC_STORE_BACKEND=s3`): immutable
  objects in the `NISABA_S3_BUCKET_OPLOG` bucket of the same SeaweedFS
  endpoint the app service uses.

| Variable | Default | Meaning |
|----------|---------|---------|
| `NISABA_SYNC_STORE_BACKEND` | `fs` | `s3` or `fs`; anything else is a fatal startup error |
| `NISABA_S3_ENDPOINT` | — **required** in `s3` mode | S3 endpoint as seen from the sync process |
| `NISABA_S3_ACCESS_KEY` / `NISABA_S3_SECRET_KEY` | — **required** in `s3` mode | The shared `nisaba-app` S3 identity (read/write/list/tag) |
| `NISABA_S3_REGION` | `us-east-1` | Region label (SeaweedFS accepts any) |
| `NISABA_S3_BUCKET_OPLOG` | — **required** in `s3` mode | Bucket holding both stores (prefixes below) |
| `NISABA_SYNC_DATA_DIR` | `data` | `fs` mode only: root of the op-log/snapshot directory tree |

A missing variable in `s3` mode is a **fatal startup error** — sync pointed at
S3 durability must not silently fall back to a local disk.

### Key layout

```text
oplog/{doc_id}/{part}.part      one immutable object per appended update
snapshot/{doc_id}/{seq}.snap    one immutable object per persisted snapshot
```

`{part}`/`{seq}` are zero-padded to 12 digits, so S3's lexicographic listing
order equals numeric order and readers replay by listing alone. Document ids
are validated to `[A-Za-z0-9._-]` (no `/`), so one document's prefix can never
collide with another's namespace.

### Append protocol

Each append uses the next numbered object key. A per-document mutex covers
allocation, PUT, and counter advancement; it survives room eviction. The
counter starts at `max(existing) + 1` and advances only after PUT succeeds.
Readers replay a contiguous prefix and warn and stop if they find a gap.

The store assumes a single writer. Do not run two sync processes against the
same bucket or filesystem directory.

### Snapshot latest resolution

Snapshots are immutable, monotonically numbered objects; there is no index
object and no "latest" pointer to rewrite. *Latest* is resolved by **version
vector**, never by key: sequence numbers say nothing about coverage — the two
snapshot writers (the update-threshold path and the maintenance floor) export
before taking the document lock, so a stale export can land a higher sequence
than a newer one. The store fetches the candidates and picks the greatest VV
with the same comparison the filesystem store uses; unreadable objects are
skipped with a warning. Snapshot bodies use the same
`[u32 be vv_len][vv bytes][snapshot bytes]` framing as the filesystem store.

### Readiness

With the S3 stores configured, `GET /health/ready` issues a `HeadBucket`
against the configured bucket (endpoint + credentials + bucket existence in
one round-trip) instead of the filesystem backend's data-dir-writable check,
so orchestration never routes traffic to a sync that cannot persist. See
`StorageProbe` in `src/server.rs`.

## Update and retention rules

Accepted updates are relayed as their original bytes. The authority imports
updates and inspects reviewer changes before accepting them; see the
[protocol](../../fixtures/sync/PROTOCOL.md#update-handling) and
[security model](../../docs/security.md).

Presence expires without a heartbeat and is never persisted. The update log is
append-only and is not compacted after snapshots. Replaying already-applied
updates is a no-op in Loro.
