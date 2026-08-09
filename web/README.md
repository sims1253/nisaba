# Nisaba web

Browser editor and paginated preview for the Nisaba app service.

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
secret is accepted or embedded.

Sync connects to `GET /sync/{doc_id}` with the stored access token and the
versioned binary framing documented in `fixtures/sync/PROTOCOL.md`. PDF bytes
are decoded into an `application/pdf` Blob URL, and replaced URLs are revoked.

Run checks with:

```sh
bun run lint
bun run test
bun run build
```
