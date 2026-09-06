# Contributing

Discuss large changes in an issue before implementing them.

## Development setup

Install Rust (the repository's `rust-toolchain.toml` selects the version),
[Bun](https://bun.sh/) 1.3 or newer, and [just](https://github.com/casey/just).
The local service stack also needs Docker with Compose v2.

1. Read the [architecture and package map](docs/architecture.md) and [domain vocabulary](CONTEXT.md).
2. Run `bun install --frozen-lockfile` at the repository root.
3. Follow the [local stack setup](docs/operations.md#1-quick-start) when you need running services. Never commit `.env`.
4. Run the narrow test suite for the area you change, then the broader checks below.

## Required checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --all-targets --locked
bun run --cwd web lint
bun run --cwd web test
bun run --cwd web build
bun run --cwd tools lint
bun run --cwd tools typecheck
bun run --cwd tools test
```

See [testing](docs/testing.md) for suite selection. `just ci-local` also runs
dependency-policy and advisory checks. The external PDF test suite has
additional system prerequisites documented in [`tools/README.md`](tools/README.md).

Run the web checks without generated WASM artifacts. To work on browser
compilation, use `just wasm-web`; it requires `wasm-bindgen-cli 0.2.127` and
the `wasm32-unknown-unknown` target. Output goes to the ignored
`web/src/wasm-generated/` directory. The app uses server compilation when
these artifacts are absent.

## Change guidelines

See the [UI design](docs/ui-design.md) for interface conventions and
[dependency policy](docs/dependency-security.md) before changing dependencies.

- Use Bun for every TypeScript workspace; do not add another lockfile.
- Preserve `unsafe_code = "forbid"` and the workspace lint policy.
- Keep `nisaba-core` free of I/O, CRDT, and web-framework dependencies.
- Add tests for behavior changes, especially Unicode offsets, synchronization, authorization,
  persistence, exports, and document projections.
- Do not update golden fixtures merely to make a failing test pass; explain the intended output
  change in the pull request.
- Never commit credentials, `.env` files, build output, editor state, QA transcripts, or generated
  reports outside their documented fixture/output directories.
- Keep public documentation factual. Planned behavior must be labeled as planned.

## Licensing

The project is licensed under **AGPL-3.0-only** ([`LICENSE`](LICENSE)). By
contributing, you agree that your contribution is licensed under AGPL-3.0-only
as part of this repository. No per-file copyright or
SPDX-License-Identifier headers are used; the repository relies on the
top-level `LICENSE` file. Do not add headers to new files.

## Commits and pull requests

Use focused commits and describe:

- the problem and chosen behavior,
- migration or compatibility impact,
- tests run,
- security, durability, or compatibility implications where relevant.
