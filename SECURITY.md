# Security Policy

## Project status

Nisaba is pre-production software. Do not use it for production documents or sensitive data
until the applicable items in [`docs/release-checklist.md`](docs/release-checklist.md) have been
verified for your deployment.

## Reporting a vulnerability

Please do **not** open a public issue for a suspected vulnerability. Use GitHub's private
vulnerability reporting for this repository (**Security → Advisories → Report a vulnerability**).
If that feature is unavailable, contact a repository maintainer privately and request a secure
reporting channel.

Include, where possible:

- affected commit or version,
- impact and prerequisites,
- reproduction steps or a minimal proof of concept,
- any suggested mitigation,
- whether credentials or personal data may have been exposed.

Maintainers will acknowledge a report through the same private channel and coordinate disclosure
and remediation there. No fixed response-time SLA is offered while the project is experimental.

## Supported versions

Only the current `main` branch receives security fixes before the first tagged release. A version
support table will be added when stable releases exist.

## Deployment warning

The Compose stack and Keycloak realm are designed for local development. They include placeholder
secrets, demo users, HTTP-only endpoints on private/local networks, and development defaults.
Follow [`docs/security.md`](docs/security.md) and [`docs/operations.md`](docs/operations.md) and
replace all such defaults before any non-local deployment.
