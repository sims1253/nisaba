# Nisaba

Nisaba is an experimental, self-hostable collaborative authoring platform for general
document projects. Projects contain path-addressed Typst documents, Loro supports collaborative
text and review state, and the compile service produces reproducible PDF artifacts.

The core model is deliberately flat: a project contains documents identified by relative paths.
Folders are derived from those paths, so Nisaba does not impose a content-specific hierarchy.

> [!WARNING]
> Nisaba is under active development. It is not ready for production data. See the
> [roadmap](ROADMAP.md) and [release checklist](docs/release-checklist.md) for the known gaps.

## Capabilities

- collaborative Typst editing with reconnect, protocol-level presence/awareness
  (roster + heartbeat), and review marks
- flat project/document APIs plus references, sharing, history, and portable export
- in-process Typst compilation and PDF preview
- PostgreSQL persistence, S3-compatible blob storage, and OIDC authentication
- deterministic projection, reference, export, and PDF-oriented test tooling

## Repository layout

- `crates/nisaba-core` — document projections, marks, review, and validation
- `crates/nisaba-references` — RIS, bibliography numbering, and reference validation
- `crates/nisaba-export` — deterministic project-archive assembly
- `services/app` — authorization, project/document/reference APIs, and export orchestration
- `services/compile` — Rust/Typst compilation service
- `services/sync` — Loro WebSocket authority, presence, and recovery
- `web` — CodeMirror editor and PDF preview client
- `tools` — optional DOCX introspection, template derivation, visual diff, and PDF checks

## Local development

Prerequisites:

- Docker with Compose v2
- Rust (the exact toolchain is selected by `rust-toolchain.toml`)
- [Bun](https://bun.sh/) 1.3 or newer
- [just](https://github.com/casey/just)

Start the local stack:

```bash
cp .env.example .env
# Change every change-me-* value before using the stack outside localhost.
just up-all
```

The default Compose configuration binds published ports to `127.0.0.1` only. It includes a
**development-only** Keycloak realm with documented demo accounts; never import that realm into
a production identity provider. See [`docs/operations.md`](docs/operations.md) for service URLs,
health checks, backup/restore, and deployment deltas.

Run the main checks:

```bash
bun install --frozen-lockfile
cargo test --workspace --all-targets --locked
bun run --cwd web test
bun run --cwd tools test
```

`just ci-local` runs the broader formatting, linting, dependency, build, and test suite. Some
operational and external-tool checks require Docker, LibreOffice, Poppler, qpdf, ImageMagick, or
Typst; [`docs/testing.md`](docs/testing.md) lists the suites and prerequisites.

## Documentation

- [User guide](docs/user-guide.md)
- [Domain vocabulary](CONTEXT.md)
- [Architecture](docs/architecture.md)
- [Operations](docs/operations.md)
- [Security model](docs/security.md)
- [Testing](docs/testing.md)
- [Roadmap](ROADMAP.md)
- [Product direction](PLAN.md)

## Contributing and security

See [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a change. Please report security issues
privately as described in [`SECURITY.md`](SECURITY.md), not in a public issue.

## License

Nisaba is licensed under **AGPL-3.0-only**. See [`LICENSE`](LICENSE).
