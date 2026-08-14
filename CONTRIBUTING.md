# Contributing

Thanks for helping improve Nisaba. The project is still experimental, so discuss large changes
in an issue before investing in an implementation.

## Development setup

1. Install the prerequisites listed in [`README.md`](README.md).
2. Run `bun install --frozen-lockfile` at the repository root.
3. Copy `.env.example` to `.env` only when you need the local Docker stack. Never commit it.
4. Run the narrow test suite for the area you change, then the broader checks below.

## Required checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --all-targets --locked
bun run --cwd web lint
bun run --cwd web test
bun run --cwd web build
bun run --cwd tools typecheck
bun run --cwd tools test
```

`just ci-local` also runs dependency-policy and advisory checks. The external PDF test suite has
additional system prerequisites documented in [`docs/testing.md`](docs/testing.md).

## Change guidelines

- Use Bun for every TypeScript workspace; do not add another lockfile.
- Preserve `unsafe_code = "forbid"` and the workspace lint policy.
- Keep `nisaba-core` free of I/O, CRDT, and web-framework dependencies.
- Add tests for behavior changes, especially Unicode offsets, synchronization, authorization,
  persistence, exports, and document projections.
- Do not update golden fixtures merely to make a failing test pass; explain the intended output
  change in the pull request.
- Never commit credentials, `.env` files, build output, editor state, QA transcripts, or generated
  reports outside their documented fixture/output directories.
- Keep public documentation factual. Planned behavior must be labelled as planned.

## Licensing

The project is licensed under **AGPL-3.0-only** ([`LICENSE`](LICENSE)). By
contributing, you agree that your contribution is licensed under AGPL-3.0-only
as part of this repository. No per-file copyright or
SPDX-License-Identifier headers are used — the repository relies on the
top-level `LICENSE` file — so do not add them to new files.

## Commits and pull requests

Use focused commits and describe:

- the problem and chosen behavior,
- migration or compatibility impact,
- tests run,
- security, durability, or compatibility implications where relevant.

By contributing, you agree that your contribution is licensed under AGPL-3.0-only.
