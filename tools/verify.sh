#!/usr/bin/env bash
# Nisaba tools verification script.
#
# Runs the full local verification suite for the tools package:
#   1. dependency install (bun) — only if node_modules is absent
#   2. lint (oxlint)
#   3. typecheck (tsc --noEmit)
#   4. unit + external-gated tests (vitest)
#   5. golden-file stability (re-introspect fixture == committed golden)
#   6. skeleton compiles with Typst (if typst present)
#
# Safe to run in CI: no network after install, deterministic outputs, exits
# non-zero on any failure. Usage: ./tools/verify.sh [--no-install]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLS="$HERE"
ROOT="$(cd "$TOOLS/.." && pwd)"
NO_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --no-install) NO_INSTALL=1 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

cd "$TOOLS"

if [[ "$NO_INSTALL" -eq 0 && ! -d node_modules ]]; then
  echo ":: installing dependencies (bun)"
  bun install --frozen-lockfile
fi

echo ":: lint"
bunx oxlint .

echo ":: typecheck"
bunx tsc --noEmit

echo ":: tests"
bunx vitest run

echo ":: golden stability (re-introspect fixture == committed golden)"
VERIFY_TMP="$(mktemp -d)"
trap 'rm -rf "$VERIFY_TMP"' EXIT
"$TOOLS/bin/nisaba-tools.ts" docx-introspect \
  --input "$ROOT/fixtures/templates/sample-document.docx" \
  --output "$VERIFY_TMP/manifest.fresh.json" >/dev/null
# Compare parsed values; object key order does not change the manifest.
bun -e '
const fs = require("node:fs");
const { isDeepStrictEqual } = require("node:util");
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
if (!isDeepStrictEqual(read(process.argv[1]), read(process.argv[2]))) {
  console.error("golden manifest drift");
  process.exit(1);
}
console.log("golden manifest stable");
' "$VERIFY_TMP/manifest.fresh.json" "$ROOT/fixtures/templates/golden/sample-document.manifest.json"

echo ":: golden skeleton stable (typst-skeleton == committed golden)"
"$TOOLS/bin/nisaba-tools.ts" typst-skeleton \
  --manifest "$ROOT/fixtures/templates/golden/sample-document.manifest.json" \
  --output "$VERIFY_TMP/skeleton.fresh.typ" >/dev/null
if ! diff -q "$ROOT/fixtures/templates/golden/sample-document.skeleton.typ" "$VERIFY_TMP/skeleton.fresh.typ" >/dev/null; then
  echo "golden skeleton drift:" >&2
  diff "$ROOT/fixtures/templates/golden/sample-document.skeleton.typ" "$VERIFY_TMP/skeleton.fresh.typ" >&2 || true
  exit 1
fi
echo "golden skeleton stable"

if command -v typst >/dev/null 2>&1; then
  echo ":: skeleton compiles (typst)"
  typst compile "$ROOT/fixtures/templates/golden/sample-document.skeleton.typ" "$VERIFY_TMP/out.pdf"
  echo "compiled ok: $(stat -c%s "$VERIFY_TMP/out.pdf" 2>/dev/null || stat -f%z "$VERIFY_TMP/out.pdf") bytes"
else
  echo ":: skeleton compiles — skipped (typst not on PATH)"
fi

echo ":: capabilities"
"$TOOLS/bin/nisaba-tools.ts" capabilities | bun -e '
const d = JSON.parse(require("fs").readFileSync(0, "utf8"));
const c = d.result;
const need = [["libreoffice","docxToPdf"],["pdftoppm","pdfToImage"],["compare","imageCompare"],["pdftotext","textExtract"],["pdfinfo","pdfInspect"]];
for (const [t, k] of need) {
  const ok = c.tools[t].available && (k ? c.derived[k] : true);
  console.log((ok ? "  [ok]   " : "  [MISS] ") + t + (c.tools[t].version ? " " + c.tools[t].version : "") + (c.tools[t].available ? "" : " — " + (c.tools[t].error ?? "")));
}
'

echo ":: verify.sh OK"
