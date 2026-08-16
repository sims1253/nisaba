#!/usr/bin/env bash
# Audit the exact dependency graph recorded in Cargo.lock. The advisory-ignore
# list has one source of truth — the [advisories] ignore array in deny.toml
# (documented per entry in docs/dependency-security.md) — and is extracted here
# so cargo-audit and cargo-deny can never drift apart.
set -euo pipefail

mapfile -t ignored < <(sed -n '/^\[advisories\]/,/^\[licenses\]/p' deny.toml | grep -oE 'RUSTSEC-[0-9]{4}-[0-9]+' | sort -u)

args=()
for advisory in "${ignored[@]}"; do
  args+=(--ignore "$advisory")
done

cargo audit "${args[@]}"
