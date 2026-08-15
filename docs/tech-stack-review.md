# Nisaba — Tech Stack Review (August 2026)

A component-by-component assessment of every choice in the stack, with live
status checks against GitHub, crates.io, npm, and Docker Hub as of
**2026-08-10**.

---

## Summary verdict

| Layer              | Verdict        | Notes                                                |
|--------------------|----------------|------------------------------------------------------|
| Rust backend       | ✅ Excellent   | Best-in-class crate choices, all actively maintained |
| TS frontend        | ✅ Good        | Effect 4.0 committed (beta → stable); loro-codemirror is a thin dep |
| Infrastructure     | ✅ Resolved    | MinIO (archived) replaced by **SeaweedFS**            |
| Dev tooling        | ✅ Excellent   | Modern, fast, well-integrated                        |
| Security posture   | ✅ Good        | cargo-deny/audit, pinned digests, network isolation  |

The MinIO→SeaweedFS migration (the former P0) is **done**; the rest of the
stack is sound. Two lower-priority risks remain worth monitoring (Effect
beta, loro-codemirror).

---

## 1. Rust backend services

### Rust 1.94 / Edition 2024
**✅ Right choice.** Edition 2024 is stable; 1.94 is a recent, well-supported
toolchain. The `unsafe_code = "forbid"` workspace lint and strict clippy
pedantic policy are excellent for a security-focused project.

### Axum 0.8 (web framework) — used by `app`, `sync`, `compile`
**✅ Right choice.** The most popular async Rust web framework (26.8K stars,
last pushed 2026-08-07). Tower middleware ecosystem, first-class WebSocket
support (needed by `sync`), and native Tokio integration. No concerns.

### Tokio 1.x (async runtime)
**✅ Right choice.** The de facto standard. 32.9K stars, active daily. There is
no credible alternative for a production async Rust service.

### SQLx 0.9 (PostgreSQL)
**✅ Right choice.** Compile-time checked queries, `migrate` feature for the
migration directory, async, and native Rust TLS (rustls). 17.4K stars, active.
The project pins `postgres` + `tls-rustls` features, avoiding OpenSSL — correct
for reproducible Docker builds.

### aws-sdk-s3 1.x (S3-compatible blob client)
**✅ Right choice.** The official AWS Rust SDK talks to any S3-compatible
endpoint (the project points it at SeaweedFS). This is the correct decoupling:
the app code uses the S3 API, so the storage backend is swapped with an
env-var change, no code changes — as proven by the MinIO → SeaweedFS
migration. The `dependency-security.md` notes transitive rustls advisories from
this stack — these are tracked and mitigated correctly.

### Loro 1.13 (CRDT)
**✅ Right choice.** Loro is the foundation of the entire collaboration layer.
6K stars, updated daily (last push 2026-08-10). The project uses it both
server-side (`loro` Rust crate in `sync`) and client-side (`loro-crdt` WASM in
the browser). Version 1.13.9 is current on crates.io. The WASM package is at
1.13.9 in `package.json` while 1.14.1 is available on npm — a minor upgrade
gap worth closing when convenient.

### Typst 0.15.1 + Tinymist 0.15.0 (document compilation)
**✅ Right choice.** Typst is the most actively developed modern typesetting
engine (55K stars, updated 2026-08-09). The project uses the in-process Typst
library (not shelling out to the CLI), which is critical for keeping `comemo`
caches warm. Tinymist provides the `World`/VFS adapters. The architecture doc
correctly identifies this as the reason `compile` must be a long-lived Rust
process.

### Hayagriva 0.10.1 (bibliography / citations)
**✅ Right choice.** The native Typst bibliography engine, maintained by the
Typst team. Used in `nisaba-references` for RIS round-trip. 2.3M downloads,
updated 2026-06-14. The `dependency-security.md` correctly documents transitive
`quick-xml` advisories.

### jsonwebtoken 9 (JWT validation)
**✅ Right choice.** The standard Rust JWT crate. Used for OIDC token
validation in both `app` and `sync`. 2K stars, active.

### reqwest 0.12 (HTTP client)
**✅ Right choice.** Standard, used with `rustls-tls` (never OpenSSL), matching
the project-wide TLS policy.

### thiserror / tracing / tracing-subscriber
**✅ Right choice.** Standard Rust ecosystem staples. The tracing integration
across all services is well-structured.

---

## 2. TypeScript frontend (`web`)

### CodeMirror 6 (editor)
**✅ Right choice.** The npm packages (`@codemirror/view` 6.43.8,
`@codemirror/state` 6.7.1, etc.) are actively published (last update 2026-08-04).
**Note:** the GitHub *development monorepo* `codemirror/dev` is archived — it
moved to a self-hosted forge at `code.haverbeke.berlin`. This does **not** affect
the npm packages or the editor itself; CodeMirror 6 remains the best-in-class
code editor for the web. The move is worth noting in case of future source-level
contributions.

### TypeScript 7.0.2
**✅ Right choice.** TypeScript 7.0 is the "Corsa" native port (released
2026-07-08). It ships a Go-based compiler with dramatically faster type
checking. A major version with a runtime change — but it's stable, released,
and the right time to be on it.

### Vite 8.2.1 (build tooling)
**✅ Right choice.** Latest stable (released 2026-08-06). Vite is the standard
modern frontend build tool. 82K stars, actively maintained.

### pdfjs-dist 6.2.108 (PDF preview)
**✅ Right choice.** Mozilla's PDF.js is the only credible client-side PDF
renderer. 1,564 versions published, last update 2026-07-28.

### Effect 4.0.0-beta.105 (functional effects / TS framework)
**⚠️ Risk — pre-release dependency.** The `effect` library is at version
`4.0.0-beta.105` in both `web` and `tools`. The **stable** release is `3.22.1`;
the `beta` dist-tag is at `4.0.0-beta.107`. Effect is an ambitious framework
(15K stars, very active), but:

- You are **two beta versions behind** the latest beta and on a **major-version
  pre-release**. Breaking changes between beta releases are expected.
- Both `web` and `tools` depend on it, so a breaking upstream change affects
  two packages simultaneously.
- `@effect/vitest` and `@effect/language-service` are also on matching betas.

**Decision (2026-08-10):** The team is committed to Effect 4.0, including the
beta track and eventual stable release. The practical action is to stay current
with betas (update to beta.107) and add a renovate/dependabot rule so the
`effect` + `@effect/vitest` + `@effect/language-service` triplet is bumped in
lockstep. This removes the pre-release as an open question and makes it an
intentional, tracked dependency rather than a risk.

### loro-crdt 1.13.9 + loro-codemirror 0.3.3 (CRDT + editor binding)
**⚠️ Minor risk.** `loro-crdt` itself is healthy (updated daily). But
`loro-codemirror` is a **thin glue package** (41 stars, 10 versions, last
published 2025-10-07, last GitHub push 2026-05-25). If Loro makes a breaking
CRDT protocol change and `loro-codemirror` doesn't update, the editor binding
becomes the upgrade bottleneck. Mitigations: keep an eye on releases, and
consider whether the binding is thin enough to vendor/fork if it stalls.

### oxlint (linter)
**✅ Right choice.** Modern, fast Rust-based linter from the Oxc project.
Updated daily (pinned at 1.77.0 in `bun.lock`). Good replacement for ESLint
with less configuration overhead.

### Vitest 4 / Playwright 1.52 (testing)
**✅ Right choice.** Vitest is the standard Vite-native test runner. Playwright
for E2E. Both actively maintained.

---

## 3. Infrastructure

### PostgreSQL 17
**✅ Right choice.** The compose file pins `postgres:17-bookworm` by digest.
PostgreSQL 18 and 19-beta exist now; 17 is a safe, well-supported choice.
The project correctly uses **least-privilege roles** (separate `nisaba_app` and
`keycloak` users, not the superuser). SQLx `migrate` feature manages the
migration directory. No concerns.

### SeaweedFS (S3-compatible object storage) — **✅ Replaces archived MinIO**
**✅ Done.** MinIO's open-source repositories were archived on 2026-04-24, so
MinIO has been replaced by SeaweedFS (`chrislusf/seaweedfs:4.41`). SeaweedFS
runs `server -s3` (master + volume + filer + S3 gateway in a single process),
which fits the one-machine self-hosting baseline. The app keeps using the
standard S3 API via `aws-sdk-s3`, so the swap required **no application code
changes** — only infrastructure: the compose service, the `seaweedfs-init`
bootstrap (now `amazon/aws-cli` instead of `minio/mc`), identities generated
at container start (`deploy/seaweedfs/generate-s3-identities.sh`), and `.env` values
(`NISABA_S3_*` / `SEAWEEDFS_HOST_S3_PORT` replacing `MINIO_ROOT_*`).

Why MinIO had to go (the problem that motivated the swap):

| Repo              | Status    | Last activity   |
|-------------------|-----------|-----------------|
| `minio/minio`     | Archived  | 2026-04-24      |
| `minio/mc`        | Archived  | 2025-11-20      |
| `minio/operator`  | Archived  | 2026-03-20      |

The archived `minio/minio` image was frozen at `RELEASE.2025-09-07` and would
never receive security patches. SeaweedFS (34K stars, actively maintained,
18M Docker pulls) was the best drop-in: fully S3-compatible, the simplest
single-node Docker deployment, and the same S3 API so `aws-sdk-s3` works
unchanged. (Alternatives considered: Garage, a lighter Rust-based
geo-distributed S3 store; Ceph, enterprise-grade but operationally heavy for a
single-machine baseline.)

### Keycloak 26.0 (OIDC identity provider)
**✅ Right choice for dev.** 36K stars, active daily. The compose file
correctly labels it **dev-only** with a demo realm. For production, the docs
direct operators to bring their own IdP. Keycloak is the standard self-hosted
OIDC provider. The app's token validation (inline JWKS, PKCE, role mapping) is
well-designed and IdP-agnostic.

### Docker Compose (orchestration)
**✅ Right choice.** The compose file is exemplary: digest-pinned images,
network segmentation (db-net, obj-net, oidc-net, svc-net), health-gated
dependencies, `no-new-privileges`, and localhost-only port bindings. For a
"self-hosting is ordinary" baseline, this is the right tool.

### Nginx (web proxy)
**✅ Right choice.** The `web` Dockerfile builds a non-root nginx that proxies
SPA, API, and WebSocket (`/sync`). Standard, reliable, well-understood.

### Bun 1.3+ (package manager / JS runtime)
**✅ Right choice.** Fast, reliable workspace support, lockfile-based
reproducible installs. 95K stars, active.

---

## 4. Dev tooling

### just (task runner)
**✅ Right choice.** 35K stars, active. The `justfile` orchestrates the full
build/test/deploy workflow. Better than Make for this use case.

### cargo-deny / cargo-audit (dependency security)
**✅ Excellent practice.** `deny.toml` and `scripts/cargo-audit.sh` enforce
blocking advisories with documented, narrow exceptions. The
`docs/dependency-security.md` table tracks every advisory, its dependency path,
and its removal condition. This is a model for how to handle transitive
vulnerabilities in a Rust project.

### Deny list / linting policy
**✅ Good.** `unsafe_code = "forbid"`, clippy pedantic, `rustfmt.toml`,
oxlint for TS. Comprehensive quality gates.

---

## 5. Priority action items

| Priority | Item                                    | Effort  | Impact                              |
|----------|-----------------------------------------|---------|-------------------------------------|
| **P0**   | ✅ **Done** — replaced MinIO with SeaweedFS | —       | Archived/unpatched infra removed    |
| **P1**   | Add renovate/dependabot rule for Effect beta triplet | Trivial | Keeps `effect` + `@effect/*` in lockstep |
| **P2**   | Track loro-codemirror releases          | Monitor | Prevents CRDT upgrade bottleneck    |
| **P2**   | Upgrade loro-crdt 1.13.9 → 1.14.1       | Trivial | Minor version currency              |
| **P3**   | Note CodeMirror dev-repo relocation     | None    | Awareness for upstream contribution |
