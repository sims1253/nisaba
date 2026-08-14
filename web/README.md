# Nisaba web

Browser editor and paginated preview for the Nisaba app service.

## Interface

The workspace layout, the vocabulary it uses, and the reasoning behind each
surface are documented in [`docs/ui-design.md`](../docs/ui-design.md). In source
terms:

| Module | Owns |
|--------|------|
| `shell.ts` | The static markup of every region (app bar, projects screen, navigator, document, dock, preview, build drawer, status bar) |
| `styles.css` | The design tokens and every component style |
| `main.ts` | State, rendering, and wiring |
| `outline.ts` | The file tree and heading outline derivations (pure) |
| `palette.ts` | The ⌘K command palette |
| `presence.ts` | Presence payloads and the relay's roster encoding (pure) |
| `decorations.ts` | In-editor review marks and Typst construct styling |
| `pdf-viewer.ts` | The virtualised page preview |

## Resource model

The client uses a flat project-to-document model. A document is available at
`/projects/{project_id}/documents/{document_id}` and has these wire fields:

- `id`
- `project_id`
- `path`
- `title`
- `body`
- `data`
- `revision`
- `updated_at`

List and create documents with `GET` and `POST /projects/{project_id}/documents`.
Read, update, and delete one document with `GET`, `PATCH`, and `DELETE` on its
resource URL. Conditional saves send `expected_revision` to prevent stale edits
from overwriting concurrent changes.

References, full-text attachments, membership, sharing, history, review, and
collaborative sync remain available. Project export remains the generic
`POST /projects/{project_id}/exports` endpoint. Compile requests use `document`
for a single document or `full` for a full project build.

## Development

```sh
bun install --frozen-lockfile
bun run dev
```

Vite serves the editor on `http://localhost:5173`. During development, `/api/*`
proxies to `http://localhost:8100` and `/sync/*` proxies WebSockets to
`ws://localhost:8101`. Override these with `VITE_APP_URL` and `VITE_SYNC_URL`.
The `/api` prefix is stripped before forwarding, except `/api/compile`, which is
forwarded verbatim. Requests use the user's OIDC bearer token.

Optional OIDC public-client configuration uses `VITE_OIDC_ISSUER`,
`VITE_OIDC_CLIENT_ID`, and optionally `VITE_OIDC_REDIRECT_URI` and
`VITE_OIDC_SCOPE`. Login uses Authorization Code with PKCE (S256); no client
secret is accepted or embedded. When the dev server is reached through a
tunnel (and so is listed in `VITE_ALLOWED_HOSTS`), the browser cannot address
the developer's localhost Keycloak; setting `VITE_OIDC_PROXY_TARGET` (for
example `http://127.0.0.1:8090`) adds a dev-only `/realms` proxy through Vite
so the issuer can be the tunnelled origin. The proxy is off unless the target
is set — nothing in the repo serves a default, and production has no
equivalent.

Sync connects to `GET /sync/{doc_id}` with the stored access token and the
versioned binary framing documented in `fixtures/sync/PROTOCOL.md`. PDF bytes
are decoded straight into a `Uint8Array` and handed to pdf.js — no Blob URL is
involved (object URLs are only created transiently for downloads, and revoked
on the next tick).

Run checks with:

```sh
bun run lint
bun run test
bun run build
```

The Playwright e2e suite (`bun run e2e`) needs the full Docker stack — the
real Keycloak, app, and relay that `just e2e-up` starts and `just e2e-test`
runs the suite against (see the `justfile`).
