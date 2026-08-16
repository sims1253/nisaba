# Architecture

This document describes how the Nisaba services fit together: the service
boundaries, the data flow for the core authoring loop, the storage model, and
the externally-visible service APIs. It is the operational complement to
[`docs/security.md`](security.md) / [`docs/operations.md`](operations.md)
(the *how to run it safely*).

> **Regenerated from the actual codebase.** A CI check (see `.github/workflows/`)
> validates that the service inventory below matches the workspace members.

---

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
| `nisaba-auth`      | Rust (lib)  | Shared role vocabulary (`Role` spellings for tokens and the app/sync authz contract) | impl. |
| `nisaba-references`| Rust (lib)  | RIS reference format round-trip                   | impl. |
| `nisaba-export`    | Rust (lib)  | Export utilities                                  | impl. |
| `@nisaba/web`      | TypeScript  | CodeMirror 6 editor, paginated preview            | impl. |
| `@nisaba/tools`    | TypeScript  | DOCX→Typst pipeline, visual-diff, PDF compliance  | impl. |
| `postgres`         | —           | Metadata (projects, users, references)            | **live (infra)** |
| `seaweedfs`        | —           | S3-compatible reference full-text blobs           | **live (infra)** |
| `keycloak`         | Java        | OIDC identity provider                            | **live (infra, dev-only)** |

`nisaba-compile` **must** be Rust: the Typst compiler is a callback `World` and
warm `comemo` caches only survive if the process stays alive. The
other services are Rust for crate sharing, but the boundary is a process
boundary regardless.

### Rust workspace members

```
crates/nisaba-auth        — shared role vocabulary (Role spellings)
crates/nisaba-core        — pure domain: Position, projection, marks
crates/nisaba-references  — RIS reference format
crates/nisaba-export      — export utilities
services/app              — CRUD, references, export orchestration, auth
services/compile          — Typst compilation
services/sync             — Loro CRDT authority, relay, presence
```

### Bun workspace members

```
web     (@nisaba/web)   — CodeMirror 6 SPA
tools   (@nisaba/tools) — template pipeline, visual-diff, PDF tooling
```

---

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
        db-net  │          db+obj│          │ sync-data volume
                │                │          │
        ┌───────▼──────┐   ┌─────▼──────────▼──────┐
        │  postgres    │   │ seaweedfs (full-text) │
        │ nisaba + kc  │   │ + local sync store    │
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
   in the configured filesystem data directory. Convergence is a CRDT property; syntactic
   validity is not, so the editor reparses on every keystroke.
3. **Project (app).** `app` owns path-addressed documents and references in Postgres,
   authorizes the request against the OIDC token, and orchestrates compiles and
   exports.
4. **Compile (compile).** `app` sends the **projection** of the document to `compile` as plain Typst sources. `compile` knows nothing
   about CRDTs, marks or reviews; it returns PDF, diagnostics, outline, and span
   map. Warm state is keyed by `project_id`.
5. **Store reference files (seaweedfs).** Uploaded full-text PDFs land in
   `nisaba-blobs`. Object keys are opaque ids — **never citation numbers**. Compile/export artifacts are still returned directly;
   content-addressed artifact storage is future work.

The projection is the seam that keeps the compiler pure: `project(text, marks,
view) -> String`. It is golden-file tested.

---

## 4. Service APIs

### 4.1 `compile` — HTTP `POST /compile`

The most important interface in the system. Narrow and stable.

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

### 4.2 `sync` — WebSocket

Path convention: `wss://<host>/sync/{doc_id}` (the `web` nginx upgrades
`/sync/` to the `sync` service). Framing is Loro's update protocol plus a
presence channel.

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

Reference payloads are structured JSON (`metadata` with `title`, `authors`,
`year`, `doi`, `pmid`, `journal`, and a mandatory `extra` object) — the API does
not parse RIS text. Project-scoped DOIs must be unique (409 on duplicates).

Document-body persistence: the web client persists the editor's text with a
debounced `PATCH /projects/{p}/documents/{d}` (autosave) — that is the write
path of record for document bodies. The sync relay carries live collaborative
edits between peers and *reads* the authoritative body from the app (via
`GET /internal/document/{document_id}/body`) to seed/verify rooms; it never
writes document bodies back to the database.

Export layout: the archive contains the compiled PDF, the projected document
sources (paths flattened with `/` → `_`), and per-document RIS bibliographies +
full-text PDFs under `references-<n>/`. The generated master `main.typ`
includes every document by its full project-relative path, so documents in
subdirectories export and compile correctly. Exports require every cited
reference to have an uploaded full-text PDF (409 otherwise). Owners, authors,
and reviewers may export (reviewers need it to export review copies;
read-only members may compile but not export).

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
| Named volume `sync-data` | Loro op log and snapshots                           | `nisaba-sync` |
| SeaweedFS `nisaba-oplog` | reserved by local bootstrap; no service uses it today | none |

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
suggestions in the review layer (synced over the CRDT relay) and are **never
written into the baseline** until an owner/author accepts them. The REST API
rejects reviewer baseline writes with 403; the web UI hides the create/delete
controls for reviewers.

---

## 7. Failure modes the architecture must not silently paper over

1. **Shelling out to the Typst CLI** — kills warm caches. `compile` is a process.
2. **Storing citation numbers** — corrupts export filenames on renumber.
3. **Physically deleting tracked-deletion text** — makes reject impossible.
4. **CRDT convergence ≠ syntactic validity** — reparse every keystroke.
5. **Compile-pool memory** — warm caches for large projects are big.
6. **Leaking product-specific hierarchy into storage** — keep the file model general.

---

## 8. Open contracts

These are tracked as open questions in the implementation plan (§7).

- Exact `sync` WebSocket message envelope.
- `app` REST resource shapes (paths under `/api`).
- Which PDF standards should be offered by default (for example PDF/A-2b or PDF/UA-1)?
