# Nisaba web

Browser editor and paginated preview for the Nisaba app service.

## Interface

The workspace layout, the vocabulary it uses, and the reasoning behind each
surface are documented in [`docs/ui-design.md`](../docs/ui-design.md).
See the [user guide](../docs/user-guide.md) for editing and review instructions.

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

## API

The client uses the app service's [project and document APIs](../docs/architecture.md#43-app--rest-under-api).
Conditional saves send `expected_revision` to prevent stale edits from
overwriting concurrent changes.

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
is set. It is unavailable in production.

Sync connects to `GET /sync/{doc_id}` with the stored access token and the
[versioned binary framing](../fixtures/sync/PROTOCOL.md).

Run checks with:

```sh
bun run lint
bun run test
bun run build
```

The Playwright e2e suite (`bun run e2e`) needs the full Docker stack — the
real Keycloak, app, and relay that `just e2e-up` starts and `just e2e-test`
runs the suite against (see the `justfile`).
