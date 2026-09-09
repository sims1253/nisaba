# Testing

Nisaba uses a **test pyramid** where fast, deterministic integration tests are
the primary CI signal, browser-driven end-to-end tests run nightly against the
full stack, and Docker-based smoke tests are reserved for infrastructure
validation only.

## Test layers

### 1. Integration & unit tests (primary CI signal — run on every PR)

These suites cover application logic:

| Suite | Command | Scope |
|-------|---------|-------|
| **Rust workspace** | `cargo test --workspace` | Sync convergence/reconnect/persistence, app CRUD/permissions/share-links, core projection golden + mark semantics + proptest |
| **Web (vitest)** | `cd web && bun run test` | API client, auth/PKCE, CRDT sync protocol, review state machine, PDF effects, model parsing, decorations, protocol encode/decode, project preview lifecycle and PDF download identity |
| **Tools (vitest)** | `cd tools && bunx vitest run` | DOCX introspection, schema validation, RIS round-trip, fixture stability |
| **PostgreSQL API and adapter tests** | `just test-live` (local API tests; see [app test setup](../services/app/README.md)) | Real repository migrations, persistence, and authorization through HTTP |
| **Rust doctests** | `cargo test --workspace --doc` | API contract examples in rustdoc |

These tests run via the `rust.yml`, `web.yml`, and `tools.yml` GitHub Actions
workflows when relevant paths change on `main` or a pull request. Database tests
are marked ignored in the default Rust run. The Rust CI test job starts a
PostgreSQL service and invokes them explicitly with `--ignored`; configuration,
connection, or migration failures fail the job. It also checks that an invalid
database URL cannot pass the live API tests.

### 2. Static analysis (runs alongside tests in CI)

- `cargo fmt --check` / `cargo clippy` / `cargo deny` / `cargo audit`
- [oxlint](https://oxc.rs/docs/guide/usage/linter.html) + `tsc` (web — `bun run
  lint` / `bun run build` in CI)
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

## Adding tests

Use Rust integration tests or web/tools Vitest tests for application logic.
Add browser flows to the existing Playwright suite and infrastructure checks
to the existing smoke scripts. Avoid adding Docker startup scripts for tests
that fit these suites.

Run `just ci-local` before pushing; see [CONTRIBUTING.md](../CONTRIBUTING.md)
for the required checks.
