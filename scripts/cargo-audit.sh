#!/usr/bin/env bash
# Audit the exact dependency graph recorded in Cargo.lock. Advisory exceptions
# are documented in docs/dependency-security.md and duplicated in deny.toml for
# cargo-deny's independent policy check.
set -euo pipefail

ignored=(
  RUSTSEC-2026-0194 # quick-xml: quadratic duplicate-attribute check
  RUSTSEC-2026-0195 # quick-xml: namespace allocation DoS
  RUSTSEC-2026-0098 # rustls-webpki: URI name constraints
  RUSTSEC-2026-0099 # rustls-webpki: wildcard name constraints
  RUSTSEC-2026-0104 # rustls-webpki: CRL parsing panic
  RUSTSEC-2023-0089 # atomic-polyfill unmaintained
  RUSTSEC-2025-0141 # bincode 1.x unmaintained
  RUSTSEC-2025-0057 # fxhash unmaintained
  RUSTSEC-2024-0436 # paste unmaintained
  RUSTSEC-2026-0206 # rustybuzz unmaintained
  RUSTSEC-2026-0192 # ttf-parser unmaintained
  RUSTSEC-2024-0320 # yaml-rust unmaintained
)

args=()
for advisory in "${ignored[@]}"; do
  args+=(--ignore "$advisory")
done

cargo audit "${args[@]}"
