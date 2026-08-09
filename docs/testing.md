# Testing Strategy

Nisaba uses a **test pyramid** where fast, deterministic integration tests are
the primary CI signal, and Docker-based smoke tests are reserved for
infrastructure validation only.

## Test layers

### 1. Integration & unit tests (primary CI signal — run on every PR)

These are fast (seconds), deterministic, and cover application logic:

| Suite | Command | Scope |
|-------|---------|-------|
| **Rust workspace** | `cargo test --workspace` | Sync convergence/reconnect/persistence (10 test files, ~34 tests), app CRUD/permissions/share-links (25 tests), core projection golden + mark semantics + proptest (20 tests) |
| **Web (vitest)** | `cd web && bun run test` | 94 tests: API client, auth/PKCE, CRDT sync protocol, review state machine, PDF effects, model parsing, decorations, protocol encode/decode |
| **Tools (vitest)** | `cd tools && bunx vitest run` | DOCX introspection, schema validation, RIS round-trip, fixture stability |
| **Rust doctests** | `cargo test --workspace --doc` | API contract examples in rustdoc |

These tests run via the `rust.yml`, `web.yml`, and `tools.yml` GitHub Actions
workflows on every push to `main` and on every pull request.

### 2. Static analysis (runs alongside tests in CI)

- `cargo fmt --check` / `cargo clippy` / `cargo deny` / `cargo audit`
- ESLint + `tsc --noEmit` (web)
- `shellcheck` (deploy scripts)
- `docker compose config` validation (`validate-compose.sh`)

### 3. Docker-based smoke tests (infra validation only)

These are **not** the primary CI signal. They validate Docker Compose
configuration (healthchecks, realm import, port bindings) — not application
logic, which is covered by layer 1.

| Script | What it checks | When it runs |
|--------|----------------|--------------|
| `deploy/smoke.sh` | Infra tier: Postgres `pg_isready`, MinIO liveness, Keycloak realm import | CI: only when infra files change (`deploy/**`, `docker-compose.yml`, `.env.example`) or on schedule/dispatch/main. Local: `just smoke`. |
| `deploy/e2e-app.sh` | Full stack: builds all 7 images, mints dev OIDC token, compile→PDF round trip, sync WS handshake, app authorize loop | Local only (`just e2e`) — too heavy for per-PR CI. |

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
   (`services/*/tests/`) or the web vitest suite — not as a new shell script
   that spins up Docker.

3. **Smoke scripts are for infra config only.** Use them to verify that a
   Dockerfile, compose service, or healthcheck change works. Do not use them
   to test API endpoints, CRDT behaviour, or rendering — those belong in the
   integration test layer.

4. **`just ci-local`** runs the full integration test suite locally and
   mirrors what CI runs on every PR. Run it before pushing.
