# `nisaba-sync`

The sync service for Nisaba: a Loro 1.13.x CRDT **authority and relay** keyed by
document id, with presence/awareness, an append-only op log, periodic snapshots,
and a pluggable snapshot store. It implements the collaborative-editing core of
the product.

## Capabilities

- **WebSocket authority/relay** keyed by document id (`GET /sync/{doc_id}`).
- **Binary CRDT import/export** — opaque Loro update bytes; the relay never
  inspects or re-serialises CRDT state.
- **Internal whole-state read** (`GET /internal/docs/{doc_id}/state`,
  service-token only): a document's current state as an opaque snapshot, for
  the app service's export path. Serving whole-state bytes is *not* the relay
  path — see [Design invariants](#design-invariants) for where the opacity
  line now sits.
- **Reconnect catch-up** via version vectors (`ExportMode::Updates { from }`),
  with a full-snapshot fallback for fresh peers or unrecoverable gaps.
- **Presence/awareness** with heartbeat expiry (injectable clock; TTL sweeper).
- **Role-aware access seam** — `author` / `reviewer` / `read-only`, resolved
  through a pluggable `AccessResolver`. Read-only peers cannot push updates.
- **Append-only op log** (`OpLogStore`) and **pluggable snapshot store**
  (`SnapshotStore`), each with filesystem, S3, and in-memory implementations.
  A room hydrates from the latest snapshot + op-log replay. The S3 stores are
  the production durability plane (see
  [Durable stores](#durable-stores-s3-key-layout)).
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

Every WebSocket `HELLO` carries a document id and a bearer token. Sync resolves
the peer's role through a single `AccessResolver`, but the production resolver
(`OidcAccessResolver`) applies **two independent checks** so that a globally
valid token can never open an arbitrary document:

1. **JWT validation** (`JwtValidator`). Header `kid` → JWKS key lookup → the
   token algorithm must be in the allow-list **and** equal the matched JWK's
   configured algorithm (defeats RS↔HMAC confusion) → `iss`, `aud`, `exp` are
   verified → the `sub` claim must be present **and non-empty** (it keys the
   per-document authorization below; an empty subject is rejected as
   unauthenticated). Only the explicit roles claim is read (default
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
| `NISABA_SYNC_AUTHZ_TOKEN` | — | shared service token: presented to the authz/seed endpoints **and** required by `GET /internal/docs/{id}/state` (unset → deny-all, fail-closed) |
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

The no-state answer is **204, not 404**: an unmatched route (version skew
against an older sync without `/internal/docs`, a misconfigured base URL in
the app) also answers 404, and the caller must distinguish "genuinely no
state — empty marks" from "wrong door — fail loudly".

- The credential is the SAME shared token as the authz hop above
  (`NISABA_SYNC_AUTHZ_TOKEN`); it is stored as a SHA-256 digest and compared in
  constant time, mirroring how the app checks it on its own `/internal/*`
  endpoints. Unset/empty → **deny-all** (fail-closed; the app's export then
  fails with a dependency error rather than reading unauthenticated state).
- The bytes are served **without interpretation**: no container is read, no
  entry decoded, nothing re-serialised — the authority is exported exactly as a
  joining peer would receive it.
- A live room answers from its in-memory authority (including unsnapshotted
  updates); with no live room the state is hydrated from the latest snapshot +
  op-log replay into a throwaway authority, without registering a room.
- The path is **never proxied by the web nginx** (only `/api/` and `/sync/`
  are forwarded), so it is reachable only inside the service network.
- The caller interprets the bytes (the app service decodes the `review`
  container for export marks); this service does not.

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

### Append protocol (no read-modify-write, no gaps)

S3 objects are immutable; the store never mutates or rewrites an existing
part. Every append allocates the **next** part number and `PutObject`s a fresh
key exactly once:

1. the per-document counter is seeded from a listing (`max(existing) + 1`),
   so it survives restarts;
2. allocate → PUT → increment happens while holding a per-document async
   mutex, so two appends can never be handed the same part number (the lock
   identity survives room eviction/`close`: a room can be evicted while an
   append is still in flight, and a fresh mutex would let the next append
   reuse the in-flight part number);
3. the counter increments **only after** the PUT succeeds — a failed or
   crashed PUT never created its object, and the next append reuses the same
   part number.

Because PUTs are atomic and part numbers are consumed only on success, the
parts present for a document are always the contiguous prefix `0..=n`;
readers verify contiguity and replay **only the contiguous prefix**, warning
and truncating if a gap ever appears (bucket tampering, or the unsupported
split-brain case of two sync processes sharing one bucket — the store assumes
a single writer, as does the filesystem store across hosts).

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

## Design invariants

- **Opacity is about the relay path** — the WebSocket relay transports opaque
  bytes: sync never inspects, filters, or re-serialises Loro state on behalf of
  a *peer*. In particular, review-layer soft deletes (marks over CRDT
  positions, review semantics) pass through untouched: **no physical deletion
  assumptions**. The internal read API serves whole-state snapshots **without
  interpretation** (no container is read, nothing re-encoded) — bytes in,
  the same state out — and interpretation is left to the authenticated caller.
- **Presence is ephemeral** — never written to the op log or snapshots; it
  expires without a heartbeat.
- **Append-only** — the op log exposes no mutation other than `append`.
- **No eviction engineered yet** — the op log is not compacted after
  a snapshot; replay re-imports already-applied ops, which is a no-op in Loro.
