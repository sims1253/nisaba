# Domain Context

This document fixes the repository's ubiquitous language. Code, interfaces, schema names,
tests, and user-facing text should use these terms consistently.

## Terms

- **Organization** *(planned)* — security and billing boundary containing members and
  projects. No code implements organizations today; multi-tenancy is deliberately
  deferred.
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
- **Checkpoint** *(planned)* — immutable record of exact project inputs at a point in time.
  Checkpoints as the build/history input remain future work.
- **Build** *(planned)* — isolated compiler execution over checkpointed inputs.
- **Artifact** *(planned)* — immutable output of a build, with provenance and diagnostics.
  Compilation returns PDFs directly today; content-addressed artifact storage is
  future work.
- **Share link** — revocable capability granting a chosen project role (up to `author`)
  to any signed-in user who redeems it. The effective role is the least privileged
  of the link's role and the redeemer's identity-provider role (see
  [`docs/security.md`](docs/security.md) §2).

## Boundaries

- `nisaba-core` owns pure document, review, projection, and resolution behavior.
- `nisaba-auth` owns the shared role vocabulary (the `author`/`reviewer`/`read-only`
  spellings parsed from tokens and the app/sync authorization contract); authorization
  decisions remain in each service.
- `app` owns authorization and project metadata.
- `sync` owns durable collaborative state and presence.
- `compile` owns isolated compiler execution, not project semantics.
- PostgreSQL holds relationships and indexed read models; object storage holds large immutable
  blobs; the collaboration store holds canonical editable state.

The core model deliberately has no industry-specific hierarchy. Optional templates and future
extensions may validate content, but they must operate on general projects and paths rather than
adding required levels to every project.
