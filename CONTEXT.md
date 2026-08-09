# Domain Context

This document fixes the repository's ubiquitous language. Code, interfaces, schema names,
tests, and user-facing text should use these terms consistently.

## Terms

- **Organization** — security and billing boundary containing members and projects.
- **Project** — collaborative workspace containing files, references, settings, checkpoints,
  and build artifacts.
- **Document** — editable text file at a stable project-relative path. It owns collaborative
  text, review state, structured data, and history.
- **Asset** — immutable binary file referenced by a document, stored by content hash.
- **Path** — normalized, project-relative address such as `chapters/introduction.typ`.
  A path is presentation and build structure, not a product-specific hierarchy.
- **Entrypoint** — document selected as the root of a build.
- **Reference** — structured citation metadata attached to a project.
- **Review mark** — typed range annotation representing a suggestion or other review state.
- **Thread** — anchored discussion with messages and resolution state.
- **Checkpoint** — immutable record of exact project inputs at a point in time.
- **Build** — isolated compiler execution over checkpointed inputs.
- **Artifact** — immutable output of a build, with provenance and diagnostics.
- **Share link** — revocable, scoped capability granting read-only access to selected output.

## Boundaries

- `nisaba-core` owns pure document, review, projection, and resolution behavior.
- `app` owns authorization and project metadata.
- `sync` owns durable collaborative state and presence.
- `compile` owns isolated compiler execution, not project semantics.
- PostgreSQL holds relationships and indexed read models; object storage holds large immutable
  blobs; the collaboration store holds canonical editable state.

The core model deliberately has no industry-specific hierarchy. Optional templates and future
extensions may validate content, but they must operate on general projects and paths rather than
adding required levels to every project.
