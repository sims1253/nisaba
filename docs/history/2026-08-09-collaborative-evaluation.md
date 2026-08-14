# Collaborative Evaluation Report — 2026-08-09

> **Historical QA snapshot — kept for context only.** This report records the
> state of the stack on 2026-08-09, before the MinIO → SeaweedFS migration (it
> therefore describes a MinIO-based stack), and names the internal agent setup
> used for the evaluation. Every bug listed below has since been fixed; see the
> regression assertions in `deploy/e2e-app.sh` for the current proofs. It is not
> a description of current behaviour.

> Automated browser-based evaluation conducted by six `zai/glm-5.2` agents
> against the full local stack (Postgres, MinIO, Keycloak, app, sync, compile,
> web). Raw agent reports and evidence are preserved under
> `artifacts/agent-evaluation-2026-08-09/` (gitignored).

## Environment

- Docker Compose full stack with all seven services
- Keycloak dev realm with `demo`/`demo`, `reviewer`/`reviewer`, `reader`/`reader`
- OIDC JWKS injected from Keycloak at startup
- Six isolated `agent-browser` Chromium sessions

## Summary

The evaluation confirmed that the application shell loads quickly and that
document creation, editing, version history, projection views, and review
comments work for authenticated authors. However, several **critical bugs**
were found that break core collaboration, deployment, and permission
boundaries. The sync WebSocket is non-functional, concurrent edits silently
lose data, the committed nginx config returns HTTP 500 for all API calls, and
read-only users see enabled destructive controls.

---

## P0 — Critical

### BUG-01: nginx `/api/*` returns HTTP 500 (committed config bug)

**Found by:** eval-concurrency, eval-a11y

**Root cause:** In `deploy/web/nginx.conf`, the `location /api/` block places
`rewrite ^/api/(.*)$ /$1 break;` *before* `set $app_upstream "app:8080";`. The
`break` flag halts further `ngx_http_rewrite_module` processing, so the `set`
never executes, leaving `$app_upstream` empty and `proxy_pass http://` evaluates
to an invalid URL prefix.

**Impact:** Every browser CRUD call under `/api/*` (GET /api/projects, etc.)
returns HTTP 500 before reaching the app. The UI is stuck on "Loading
projects…". Health and `/api/compile` (exact match, unaffected) still work.

**Fix:** Move `set $app_upstream` before the `rewrite ... break;` line.
**Status: fixed in this commit.**

---

### BUG-02: Concurrent edits cause silent data loss

**Found by:** eval-concurrency, eval-coauthor (independently confirmed)

**Reproduction:**
1. Two users open the same document in separate sessions.
2. Both make edits at different positions within the same save interval.
3. Both `PATCH /projects/{id}/documents/{doc_id}` calls return HTTP 200.
4. The first editor's changes are silently overwritten by the second.

**Root cause:** Document persistence uses last-write-wins with no CRDT merge or
conflict detection. The sync WebSocket is down (BUG-03), so the Loro CRDT layer
never participates in real-time convergence. The REST save path has no
optimistic-concurrency guard (no `If-Match` / revision precondition).

**Impact:** Total loss of user work in any concurrent scenario — the primary use
case for a collaborative editor.

**Recommendation:** Wire the sync WebSocket so Loro replicas converge; enforce
revision preconditions on the REST save path as a fallback; surface a conflict
state in the UI rather than silently discarding changes.

---

### BUG-03: Sync WebSocket is non-functional (JWT claim mismatch)

**Found by:** eval-concurrency, eval-reader, eval-coauthor

**Reproduction:**
1. Authenticated user opens a document.
2. The browser attempts a WebSocket upgrade to `/sync/{doc_id}`.
3. The sync service returns a typed `ERROR` frame with code `4003` ("JWT claim
   mismatch").

**Root cause:** The sync service's OIDC issuer/audience/JWKS configuration does
not match what Keycloak actually emits. The audience claim in the token
(`nisaba-web`) does not match what the sync service validates, or the JWKS URL
is unreachable, or the issuer string differs between browser-facing and
container-internal hostnames.

**Impact:** No real-time collaboration, presence, cursor sharing, or CRDT
convergence. This is the root cause enabling BUG-02 (silent data loss), because
without sync there is no Loro convergence between replicas.

**Recommendation:** Align the sync service's OIDC configuration (issuer,
audience, JWKS URL) with Keycloak's actual token claims. Test the full sync
handshake end-to-end with a real token.

---

### BUG-04: Sharing API stores username instead of OIDC subject

**Found by:** eval-coauthor, eval-concurrency

**Reproduction:**
1. Project owner opens the Share panel and invites a user by username.
2. The membership is stored with the username string as `subject`.
3. Keycloak access tokens carry the UUID `sub`, not the username.
4. The invited user's `get_membership` lookup fails → 403 Forbidden.

**Impact:** Users invited through the UI sharing flow can never access the
project. The only workaround is to invite by OIDC UUID directly.

**Recommendation:** Resolve usernames to OIDC subjects before storing, or change
the membership lookup to match on `preferred_username` / a configurable claim.

---

## P1 — High

### BUG-05: PDF preview fails to render

**Found by:** eval-coauthor, eval-concurrency

**Reproduction:**
1. Authenticated user opens a document and clicks Compile.
2. The compile API returns HTTP 200 with PDF data.
3. The browser-side `pdf.worker-CLesOks4.mjs` dynamic import fails.

**Impact:** Users cannot preview compiled output despite a successful backend
compilation.

**Recommendation:** Fix the PDF worker module loading path in the Vite build
configuration; verify the worker URL resolves correctly under the nginx static
root.

---

### BUG-06: Read-only users see enabled destructive controls

**Found by:** eval-reader

**Reproduction:**
1. Sign in as `reader/reader` (role: `read-only`).
2. Open a shared project.

**Observed:** Delete project button, Delete document button, Add document
button, Compile button, Track changes toggle, Add reference, and Add comment are
all visible and enabled. The editor is fully editable
(`contentEditable="true"`). Only Share and Export are correctly hidden by
`applyRoleGates()`.

**Backend behavior:** The server correctly returns 403 on compile and other
write attempts, but the UI provides no client-side guard.

**Impact:** Read-only users can attempt destructive actions, receive confusing
403 errors, and believe they can edit (their input is silently discarded on
save).

**Recommendation:** Apply `readOnly: true` to the CodeMirror editor for
read-only roles; hide or disable all write controls based on membership role.

---

### BUG-07: JWT token expiry (5 min) with no proactive refresh

**Found by:** eval-reader, eval-concurrency, eval-a11y

**Impact:** After 5 minutes the token silently expires, causing unexplained 401
errors on all API calls. The user receives no indication their session expired
and must manually sign in again — repeatedly during a long editing session.

**Recommendation:** Implement silent token refresh using the OIDC refresh token
flow before the access token expires; surface a re-authentication prompt on 401.

---

## P2 — Medium

### PERF-01: No HTTP compression

**Found by:** eval-a11y

The first paint transfers ~3.8 MB uncompressed: the `loro_wasm.wasm` is 3.0 MB
(84% of payload), plus 630 KB JS and 27 KB CSS. With brotli/gzip this could be
~1.2–1.5 MB. No `Content-Encoding` header is returned for any asset.

**Recommendation:** Enable `gzip`/`brotli` in nginx for text-based assets.

---

### PERF-02: No `Cache-Control` on hash-named assets

**Found by:** eval-a11y

Hash-named assets (e.g., `index-cyBKdr_5.js`) use only `ETag`, causing a full
revalidation on every navigation. Since the hash changes when content changes,
these should be served with `Cache-Control: public, max-age=31536000,
immutable`.

---

### UX-01: History revision labels malformed

**Found by:** eval-concurrency

Version history shows labels like "Rev 9ec6b702a-..." which concatenate the
revision number with the UUID. The label should show a clean revision number.

---

## P3 — Low / Improvements

### A11Y-01: No `<h1>` on the page

The app shell uses only `<h2>` headings. axe reports `page-has-heading-one`
(moderate).

### A11Y-02: Editor textbox is unlabeled

The CodeMirror `.cm-content` element with `role=textbox` has no `aria-label`.

### A11Y-03: Low-contrast "Hide outline" glyph

The `‹` collapse glyph has 2.76:1 contrast, failing WCAG AA (minimum 3:1 for
UI components).

### A11Y-04: Duplicate accessible names

Two "Track changes: off" buttons have the same accessible name.

### A11Y-05: No skip-to-content link

### SEC-01: No security headers

CSP, HSTS, X-Frame-Options, X-Content-Type-Options, and Referrer-Policy are all
absent from nginx responses.

### SEC-02: In-memory auth token

The OIDC token is stored in memory only; a full page reload drops the session
and forces a Keycloak round-trip.

---

## Performance measurements (app shell)

| Metric | Value |
|--------|-------|
| FCP | 80–104 ms |
| LCP | 80–104 ms |
| CLS | 0 |
| TTFB | ~0.5 ms |
| Long tasks | 0 |
| Main-thread busy | ~94 ms |
| WASM compile | ~3 ms |
| First paint payload | ~3.8 MB (uncompressed) |

---

## What worked well

- **Application shell** loads fast and is responsive on LAN.
- **Document CRUD** (create, edit, save, switch) works correctly for authors.
- **Version history** with author attribution and timestamps.
- **Projection views** (Proposed, Baseline, Redline, Public) render correctly.
- **Review comments** (add, resolve, jump-to) work.
- **Backend permission enforcement**: server correctly returns 403 for
  read-only write attempts.
- **Responsive layout**: no horizontal scroll at 390 px / 320 px viewports.
- **Clean 3-pane layout** with good ARIA landmark roles.
- **OIDC sign-in flow** works end-to-end once nginx/JWKS are correct.
