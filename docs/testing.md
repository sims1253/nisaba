# Testing Strategy

Nisaba uses a **test pyramid** where fast, deterministic integration tests are
the primary CI signal, browser-driven end-to-end tests run nightly against the
full stack, and Docker-based smoke tests are reserved for infrastructure
validation only.

## Test layers

### 1. Integration & unit tests (primary CI signal — run on every PR)

These are fast (seconds), deterministic, and cover application logic:

| Suite | Command | Scope |
|-------|---------|-------|
| **Rust workspace** | `cargo test --workspace` | Sync convergence/reconnect/persistence, app CRUD/permissions/share-links, core projection golden + mark semantics + proptest |
| **Web (vitest)** | `cd web && bun run test` | API client, auth/PKCE, CRDT sync protocol, review state machine, PDF effects, model parsing, decorations, protocol encode/decode, in-browser compile pipeline/toggle/worker host (mocked worker; runs without the wasm artifacts) |
| **Tools (vitest)** | `cd tools && bunx vitest run` | DOCX introspection, schema validation, RIS round-trip, fixture stability |
| **Rust doctests** | `cargo test --workspace --doc` | API contract examples in rustdoc |

These tests run via the `rust.yml`, `web.yml`, and `tools.yml` GitHub Actions
workflows on every push to `main` and on every pull request.

### 2. Static analysis (runs alongside tests in CI)

- `cargo fmt --check` / `cargo clippy` / `cargo deny` / `cargo audit`
- [oxlint](https://oxc.rs/docs/guide/usage/linter.html) + `tsc` (web — `bun run
  lint` / `bun run build` in CI; oxlint replaces ESLint)
- `oxlint` + `tsc --noEmit` (tools — `bun run lint` / `bun run typecheck`)
- `shellcheck` (deploy scripts)
- `docker compose config` validation (`validate-compose.sh`)

### 3. Browser end-to-end tests (Playwright — nightly in CI, on demand locally)

Real-browser flows against the full Compose stack (infra + app profile), driven
by Playwright with Chromium. The spec files live in `web/e2e/` (configuration:
`web/e2e/playwright.config.ts`; tests run serially because they share state).
Coverage includes sign-in, collaboration and reviewer overlap, the review
workflow, permissions, sharing, export, history, sync, undo, offline drafts,
connectivity, startup, search, and deletion races.

| Aspect | Detail |
|--------|--------|
| CI | `.github/workflows/e2e.yml` — nightly schedule plus manual dispatch; not run per PR (the full stack is too heavy) |
| Local | `just e2e-suite` (start stack → wait healthy → run tests); `just e2e-up` / `just e2e-test` run the steps individually against an already-running stack |
| Direct | `cd web && bunx playwright test --config e2e/` (requires the stack up and `E2E_BASE_URL`, default `http://127.0.0.1:8103`) |

### 4. Docker-based smoke tests (infra validation only)

These are **not** the primary CI signal. They validate Docker Compose
configuration (healthchecks, realm import, port bindings) — not application
logic, which is covered by layers 1 and 3.

| Script | What it checks | When it runs |
|--------|----------------|--------------|
| `deploy/smoke.sh` | Infra tier: Postgres `pg_isready`, SeaweedFS liveness, Keycloak realm import | CI: only when infra files change (`deploy/**`, `docker-compose.yml`, `.env.example`) or on schedule/dispatch/main. Local: `just smoke`. |
| `deploy/e2e-app.sh` | Full stack: builds the four app-profile images (app/sync/compile/web) and pulls the pinned infra images, mints a dev OIDC token, compile→PDF round trip, sync WS handshake, app authorize loop | Local only (`just e2e`) — too heavy for per-PR CI. |

**Rationale:** smoke tests are slow (2–3 min for `smoke.sh`, 10+ min for
`e2e-app.sh` because it builds images), fragile (Docker daemon state, network
flakiness, container startup races), and test infrastructure configuration
rather than application correctness. The integration tests in layer 1 are
faster, more specific, and more reliable — they catch logic regressions that a
Docker smoke test would never surface.

## Guidelines for contributors and automation

1. **Prefer integration tests.** When adding a feature or fixing a bug, write
   a test in the appropriate `*.test.ts` (web) or `tests/` directory (Rust).
   These run on every PR and give immediate, specific feedback.

2. **Do not add smoke tests for application logic.** If a new feature needs
   end-to-end coverage, add it to the existing Rust integration test suites
   (`services/*/tests/`), the web vitest suite, or — for real-browser flows —
   a Playwright spec in `web/e2e/` — not as a new shell script that spins up
   Docker.

3. **Smoke scripts are for infra config only.** Use them to verify that a
   Dockerfile, compose service, or healthcheck change works. Do not use them
   to test API endpoints, CRDT behavior, or rendering — those belong in the
   integration test layer (or the Playwright tier for browser flows).

4. **`just ci-local`** runs the full integration test suite locally and
   mirrors what CI runs on every PR. Run it before pushing.
