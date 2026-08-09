# Dependency Security Exceptions

`cargo audit` and `cargo deny` are blocking checks, but the locked graph currently contains
advisories that cannot be upgraded independently of the Typst/Tinymist, Hayagriva, or AWS SDK
stacks. Exceptions are narrow advisory IDs, never crate-wide vulnerability suppression.

The authoritative exception list is enforced by `scripts/cargo-audit.sh` and `deny.toml`. CI
must fail if a new advisory appears. Review this page whenever the lockfile or any upstream stack
is upgraded, and at least before each release.

| Advisories | Dependency path | Current mitigation | Removal condition |
|---|---|---|---|
| RUSTSEC-2026-0194, RUSTSEC-2026-0195 | Hayagriva/Typst → `quick-xml` | Inputs are size-limited at service boundaries; XML-based imports remain untrusted and must not bypass those limits | Upstream accepts `quick-xml >=0.41` |
| RUSTSEC-2026-0098, -0099, -0104 | AWS SDK → rustls 0.21 → `rustls-webpki` 0.101 | S3 endpoints are operator-configured; production must use a trusted, controlled endpoint | AWS SDK graph removes rustls 0.21 |
| RUSTSEC-2023-0089, RUSTSEC-2025-0141, RUSTSEC-2025-0057, RUSTSEC-2024-0436, RUSTSEC-2026-0206, RUSTSEC-2026-0192, RUSTSEC-2024-0320 | Typst/Tinymist rendering and syntax stack | These are maintenance warnings rather than known exploitable vulnerabilities; compiler inputs remain bounded and isolated from credentials | The owning upstream stack migrates to maintained replacements |

Exceptions do not make the current compiler suitable for hostile multi-tenant use. Supervised
worker-process isolation and hard resource limits remain release blockers in
[`ROADMAP.md`](../ROADMAP.md).
