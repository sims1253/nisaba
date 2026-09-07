# Architecture

Service boundaries, authoring data flow, storage, and APIs. See
[operations](operations.md) for running the stack and [security](security.md)
for its trust boundaries.

## 1. Service inventory

The following table is validated by CI against the Cargo workspace members
(`Cargo.toml` `[workspace] members`) and the bun workspace members
(`package.json` `workspaces`).

| Service / Package  | Language    | Owns                                              | Status        |
|--------------------|-------------|---------------------------------------------------|---------------|
| `nisaba-compile`   | **Rust**    | Typst compilation (in-memory sources → PDF)       | impl. (`/healthz`) |
| `nisaba-sync`      | Rust        | Loro CRDT authority, relay, presence, op-log      | impl. (`/healthz`, `/health/ready`) |
| `nisaba-app`       | Rust        | CRUD, references, export orchestration, auth      | impl. (`/healthz`, `/health/ready`; Postgres + S3, inline JWKS) |
| `nisaba-core`      | Rust (lib)  | Position model, projection, marks, reference types | impl. (pure, no I/O) |
| `nisaba-core-wasm` | Rust (lib, wasm-bindgen) | Projection and bibliography wrapper for the web client | impl. (pure; experimental WASM library) |
| `nisaba-compile-core` | Rust (lib)  | Typst workers, compilation, span map, outline, and diagnostics | impl. (pure, no I/O, no async) |
| `nisaba-compile-wasm` | Rust (lib, wasm-bindgen) | Compile wrapper for the web client | impl. (experimental WASM library) |
| `nisaba-auth`      | Rust (lib)  | Shared role vocabulary (`Role` spellings for tokens and the app/sync authz contract) | impl. |
| `nisaba-references`| Rust (lib)  | RIS reference format round-trip                   | impl. |
| `nisaba-export`    | Rust (lib)  | Export utilities                                  | impl. |
| `@nisaba/web`      | TypeScript  | CodeMirror 6 editor, paginated preview            | impl. |
| `@nisaba/tools`    | TypeScript  | DOCX→Typst pipeline, visual-diff, PDF compliance  | impl. |
| `postgres`         | —           | Metadata (projects, users, references)            | **live (infra)** |
| `seaweedfs`        | —           | S3-compatible reference full-text blobs           | **live (infra)** |
| `keycloak`         | Java        | OIDC identity provider                            | **live (infra, dev-only)** |

`nisaba-compile-core` hosts Typst in-process and keeps project workers warm
between requests. The compile service adds HTTP authentication, request limits,
concurrency control, and timeouts. The experimental WASM libraries expose the same compilation core
through `nisaba-compile-wasm` and projections through `nisaba-core-wasm`. Golden tests compare native and WASM output, including PDFs.

## 2. Topology

```
                          ┌──────────────────────────────┐
   browser (CodeMirror 6, │  web  (nginx, non-root :8080) │
   Loro replica, viewer)  │  / → SPA   /api → app   /sync │
         HTTP/WS          │            → sync (WebSocket) │
         OIDC (redirect)  └──────────────┬───────────────┘
                 ▲                        │ svc-net
                 │           ┌────────────┼─────────────┐
                 │           ▼            ▼             ▼
        ┌────────────────┐ ┌────────┐ ┌────────┐
        │   keycloak     │ │  app   │ │ sync   │   (compile called by app
        │   (OIDC :8090) │ │ (CRUD) │ │ (CRDT) │    over svc-net on demand)
        └───────┬────────┘ └───┬────┘ └────┬───┘
        db-net  │          db+obj│       obj│
                │                │          │
        ┌───────▼──────┐   ┌─────▼──────────▼──────┐
        │  postgres    │   │ seaweedfs (full-text │
        │ nisaba + kc  │   │ blobs + sync oplog)  │
        └──────────────┘   └───────────────────────┘
                segmented networks and named volumes
```

Network segmentation is defined in `docker-compose.yml` and explained in
[`security.md`](security.md) §"Network model". Membership separates database,
object-store, OIDC, and application-service traffic. Published developer ports
bind only to `127.0.0.1`; production deployments should add outbound firewall
policy where egress restriction is required.

---

## 3. Data flow — the core authoring loop

1. **Edit (web).** A writer types in CodeMirror 6. Hybrid inline decorations
   render allowlisted constructs. The edit is applied to the local
   Loro replica (WASM) and sent to `sync` over a WebSocket.
2. **Collaborate (sync).** `sync` is the Loro authority: it relays ops to other
   replicas, computes presence/awareness, and persists its op log and snapshots
   in S3 (the Compose default) or a configured filesystem directory. The editor
   reparses on each keystroke to check syntax.
3. **Project (app).** `app` owns path-addressed documents and references in Postgres,
   authorizes the request against the OIDC token, and orchestrates compiles and
   exports.
4. **Compile (compile).** `app` sends the **projection** of the document to `compile` as plain Typst sources. `compile` knows nothing
   about CRDTs, marks or reviews; it returns PDF, diagnostics, outline, and span
   map. Warm state is keyed by `project_id`.
   Workspace previews use the server project pipeline (§4.1.1).
5. **Store reference files (seaweedfs).** Uploaded full-text PDFs land in
   `nisaba-blobs`. Object keys are opaque ids — **never citation numbers**. Compile/export artifacts are still returned directly;
   content-addressed artifact storage is future work.

The projection is the seam that keeps the compiler pure: `project(text, marks,
view) -> String`. It is golden-file tested.

---

## 4. Service APIs

### 4.1 `compile` — HTTP `POST /compile`

```
POST /compile
Content-Type: application/json
{
  "project_id": "uuid",
  "entry": "m3/3-2-1.typ",
  "sources": { "<path>": "<typst source>", ... },   // the projection, not the CRDT
  "view": "baseline" | "proposed" | "redline"
}
→ 200 {
  "pdf"?:       "<base64 bytes>",
  "span_map":   [ ... ],
  "diagnostics":[ ... ],
  "outline":    [ ... ],
  "build_id":   "<opaque id>",
  "instrumentation": { ... }
}
```

- This is the app→compile wire; the app's own public `POST /api/compile` accepts
  marks alongside these fields, applies the `view` projection server-side, and
  sends only the projected sources here.
- Warm `comemo` caches persist across calls for the same `project_id`.

#### 4.1.1 Project previews

The workspace calls `POST /projects/{project_id}/preview` with a view and an
optional draft of the open document. The app captures each document's collaborative
text and review marks together, falling back to its stored body only when sync has
no state for that document. An unavailable sync service fails the request.

The project's `entry_document_id` selects the compilation entrypoint. It follows
renames and resets when that document is deleted. Without an explicit selection,
the app uses `main.typ`, then the first path in lexical order.

Preview and export share source projection, bibliography injection, and redline
support. The response includes the actual entrypoint and view. The browser keeps
the resulting PDF for direct download without another compile.

Each request captures document states independently. It does not yet create a
durable project checkpoint or claim an atomic read across documents. The browser
still uses REST autosave; replacing that second write path remains work to do.

The workspace uses server compilation. The WASM compiler libraries and experimental
client dispatcher remain available for development, but the workspace does not
use the `nisaba.compilePath` preference.

### 4.2 `sync` — WebSocket (+ an internal state read)

Path convention: `wss://<host>/sync/{doc_id}` (the `web` nginx upgrades
`/sync/` to the `sync` service). Framing is Loro's update protocol plus a
presence channel.

`sync` also serves a service-token-only HTTP read. Nginx does not proxy it;
the development stack also exposes sync on a loopback-only host port:

```
GET /internal/docs/{doc_id}/state
Authorization: Bearer <NISABA_SYNC_AUTHZ_TOKEN>
→ 200 application/octet-stream   # whole current CRDT state, opaque Loro snapshot
→ 204                            # the document has no state anywhere (NOT 404 —
                                 # a routing miss must stay distinguishable)
```

Review records live in each document's CRDT `review` container. The app reads
the snapshot and interprets those records for export. Sync returns the snapshot
without applying export projections. Its reviewer-update validation is
described in the [wire protocol](../fixtures/sync/PROTOCOL.md#update-handling).

### 4.3 `app` — REST under `/api`

The browser-facing CRUD API lives under `/api/*`; the `web` nginx strips the
`/api` prefix and forwards to `app` (`/api/projects` → `/projects`). The
exception is the exact `/api/compile` route, which nginx forwards verbatim.
Routes (the machine-readable truth is `GET /openapi.json` on the app service):

- `GET|POST /projects`, `GET|PATCH|DELETE /projects/{project_id}`
- `GET|POST /projects/{project_id}/members`,
  `DELETE /projects/{project_id}/members/{subject}` (member removal; the owner
  row cannot be removed), `GET /projects/{project_id}/membership` (own role)
- `GET|POST /projects/{project_id}/documents`,
  `GET|PATCH|DELETE /projects/{project_id}/documents/{document_id}`
- `GET /projects/{project_id}/documents/{document_id}/history`,
  `GET .../history/{revision_id}`
- `GET|POST /projects/{project_id}/references`,
  `GET|PATCH|DELETE /projects/{project_id}/references/{reference_id}`
- `GET|PUT|DELETE .../references/{reference_id}/fulltext`,
  `GET /projects/{project_id}/fulltexts`
- `POST /projects/{project_id}/exports` — portable archive (see below)
- `POST /projects/{project_id}/share-links`,
  `DELETE /projects/{project_id}/share-links/{token}` (revocation),
  `POST /share/{token}/redeem`
- `GET /projects/{project_id}/audit`
- `POST /api/compile` (proxied verbatim by nginx; also reachable on the app
  port), `GET /healthz`, `GET /health/ready`, `GET /openapi.json`
- Internal (machine-token only, never proxied): `POST /internal/sync/authorize`,
  `GET /internal/document/{document_id}/body`
- Internal on the sync service (machine-token only, never proxied):
  `GET /internal/docs/{document_id}/state` — see §4.2

Reference payloads are structured JSON (`metadata` with `title`, `authors`,
`year`, `doi`, `pmid`, `journal`, and a mandatory `extra` object) — the API does
not parse RIS text. Project-scoped DOIs must be unique (409 on duplicates).

Document-body persistence: the web client persists the editor's text with a
debounced `PATCH /projects/{p}/documents/{d}` (autosave) — that is the write
path of record for document bodies. The sync relay carries live collaborative
edits between peers and *reads* the authoritative body from the app (via
`GET /internal/document/{document_id}/body`) to seed/verify rooms; it never
writes document bodies back to the database.

Preview and export read text and review marks from the same per-document sync
snapshot. They never project cursor positions from that snapshot over the REST
body. Documents without collaborative state use their stored body and no marks.
A sync failure fails the request rather than silently dropping review state.

Ordinary export packages the compiled sources and PDF. `include_fulltexts: true`
adds the evidence bundle and requires the cited reference attachments.

Export compiles the selected entrypoint with the requested review projection.
The entrypoint controls which other files contribute to the PDF through Typst
imports and includes. The ZIP contains that PDF, all saved document sources
under `documents/`, and per-document RIS bibliographies with full-text PDFs
under `references-<n>/`. Saved sources keep their directory structure with sanitized filenames and
are not replaced by the projected compile inputs. Exports require every cited
reference to have an uploaded full-text PDF (409 otherwise). Owners, authors,
and reviewers may export; read-only members may compile but not export.

### 4.4 Health — `GET /healthz` (all HTTP services)

Every HTTP service exposes `GET /healthz` returning `200 ok`. This is the
contract used by the Docker `HEALTHCHECK` directives. `app` and `sync` also serve
`GET /health/ready`.

---

## 5. Storage model

| Store      | What lives here                                              | Owner role    |
|------------|--------------------------------------------------------------|---------------|
| Postgres `nisaba`   | projects, documents, references, audit | `nisaba_app`  |
| Postgres `keycloak` | Keycloak realm, users, sessions                           | `keycloak`    |
| SeaweedFS `nisaba-blobs` | uploaded reference full-text PDFs                   | `nisaba-app` (scoped) |
| SeaweedFS `nisaba-oplog` | sync's durable CRDT history: op-log parts (`oplog/`) and snapshots (`snapshot/`) | `nisaba-app` (scoped) |

- `app` always uses `PostgresRepository` and `S3BlobStore` in the service binary.
  In-memory adapters are available only to unit tests.
- Postgres and SeaweedFS use **separate, least-privilege roles**.
- SeaweedFS buckets are **versioned** for recoverability.
- Citation numbers are **never stored**; they are derived at build time.

---

## 6. Authentication & OIDC flow

```
browser ──(1) SPA loads, discovers unauthenticated state ─▶ app (401 on first API call)
browser ──(2) redirect ──────────────────────────────────▶ keycloak /realms/nisaba (login)
browser ◀──(3) authorization code ──────────────────────── keycloak
browser ──(4) code → app (or direct token exchange) ─────▶ tokens (access/refresh/id)
browser ──(5) GET /api/... Authorization: Bearer <access> ─▶ app (validates, routes by role)
```

- Realm `nisaba`, client `nisaba-web` (**public**, authorization-code +
  PKCE `S256` — no client secret is exposed to the browser), roles `author` /
  `reviewer` / `read-only`.
- Roles are mapped into the access token as a **top-level `roles`** claim.
- Today the app validates tokens against a JWKS read inline from
  `NISABA_OIDC_JWKS_JSON` at startup. An **empty** value is the safe deny-all
  default (the app boots and rejects every token); populate it with the realm
  JWKS to accept tokens.
- Access tokens are short-lived (Keycloak's 5-minute default in the dev realm);
  the SPA stores `expiresAt` and refreshes proactively. API clients must handle
  silent 401s by refreshing.

### 6.1 Role model

Capabilities come from the **IdP role claim** (`author` / `reviewer` /
`read-only`) AND the **project membership role** (`owner` / `author` /
`reviewer` / `read-only`); both must permit an action. In practice:

| Action | owner | author | reviewer | read-only |
|--------|:-----:|:------:|:--------:|:---------:|
| Read documents / history / audit / members | ✓ | ✓ | ✓ | ✓ |
| Edit baseline (PATCH body) | ✓ | ✓ | — (suggest only) | — |
| Create / rename / delete documents | ✓ | ✓ | — | — |
| Accept / reject / comment (review layer) | ✓ | ✓ | ✓ | — |
| Compile / see diagnostics | ✓ | ✓ | ✓ | ✓ |
| Export project | ✓ | ✓ | ✓ | — |
| Manage members / share links / delete project | ✓ | ✓ | — | — |

Reviewers are locked into suggesting mode: their edits become tracked
suggestions in the CRDT review layer. The REST API rejects reviewer document
body writes with 403, while the UI allows review actions. The web UI hides
create/delete controls from reviewers.

---

## 7. Constraints

Keep document paths independent of any domain-specific hierarchy. Derive
citation numbers when building rather than storing them with references.
Tracked deletions must retain enough text to undo a rejected suggestion.
CRDT convergence does not guarantee valid Typst syntax.

Warm Typst caches consume memory on the server and, when enabled, in the
browser. The server bounds worker count and concurrency; HTTP timeouts do not
stop a running compile. See the [compile service](../services/compile/README.md).

## 8. Wire contracts

- [Sync WebSocket framing](../fixtures/sync/PROTOCOL.md)
- App REST schema: `GET /openapi.json` on the app service
- [Compile request and response](../services/compile/README.md)
