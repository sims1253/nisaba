# Roadmap

Nisaba is under active development and is **not yet ready for production use**. This page tracks the remaining product work as public engineering priorities.

## What works today

- Rust domain model for projections, review marks, resolution, and validation
- Loro-based collaborative editing, reconnect, presence, and filesystem-backed recovery
- Project, document, reference, sharing, history, and export interfaces backed by PostgreSQL and S3
- In-process Typst compilation with embedded fonts and PDF/A-2b output by default
- CodeMirror editor, PDF preview, review UI, and OIDC integration
- One Bun workspace and a pinned Rust toolchain with CI for Rust, web, tools, infrastructure,
  dependency policy, and advisory scanning

The test suite covers Unicode position conversion, concurrent sync, projection goldens,
reference/RIS handling, PDF generation, authorization, persistence, and browser-facing models.

## Before a production deployment

### Document authority and review durability

- Add a fenced, single-writer document actor backed by the PostgreSQL CRDT WAL.
- Acknowledge updates only after durable persistence, and prove crash/reconnect recovery.
- Store marks, threads, and decisions as structured CRDT entities rather than JSON values.
- Finish stable cursor anchoring and schema migration for existing review entities.
- Make immutable checkpoints the input to preview, history, restore, build, and export.

### Compiler isolation and reproducibility

- Replace the mock Tinymist world with the production project/package/font world.
- Run compilation in supervised worker processes so timeout and cancellation hard-kill work.
- Add offline package locks, resource limits, deterministic artifact storage, and independent
  PDF-standard validation.
- Stream large artifacts instead of returning base64 payloads in JSON.

### General projects and generated contracts

- Finish the generic versioned file tree, including binary assets, rename/move, trash, and
  multiple entrypoints.
- Generate OpenAPI and TypeScript/Effect contracts and fail CI on contract drift.
- Split the frontend into lifecycle-scoped services and accessible components without
  `innerHTML` rendering.
- Move browser authentication to same-origin, HttpOnly sessions with short-lived sync tickets.

### Operations and release evidence

- Add production observability, worker/process failure drills, upgrade/rollback testing,
  point-in-time recovery, SBOM/provenance, and signed release artifacts.
- Record reproducible cold/warm build benchmarks against representative large projects.

## Near-term contribution areas

Small, reviewable contributions are welcome in tests, documentation, accessibility, generated
contract tooling, and operational hardening. Architectural changes to document authority,
checkpoints, or the compiler model should start with an issue so their migration and durability
requirements are explicit.
