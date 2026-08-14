# Nisaba tools

Repeatable, CI-friendly tooling for importing and validating document templates:
DOCX introspection → a deterministic intermediate manifest → a Typst template
skeleton, with required-placeholder/schema preservation validation, a page-image
visual-diff harness, and PDF compatibility checks.

- **Language:** TypeScript + [Effect 4](https://effect.website) (core `effect`
  package). Runtime: Bun (Node ≥20 also works). No Python required — DOCX is a
  ZIP of XML, parsed in-process.
- **External tools:** LibreOffice, Poppler (`pdftoppm`/`pdfinfo`/`pdftotext`),
  ImageMagick (`compare`), qpdf, and optionally Typst. All detected with clear
  failure when missing.
- **Determinism:** every report is key-sorted, timestamp-free, and stable across
  platforms; fixtures and golden files are byte-stable.

See [`../docs/template-pipeline.md`](../docs/template-pipeline.md) for the full
contract, and [`../fixtures/templates`](../fixtures/templates) for the sample DOCX
fixtures and the golden manifest/skeleton outputs.

## Quick start

```sh
cd tools
bun install
bun bin/nisaba-tools.ts capabilities
./verify.sh                 # typecheck + tests + golden-stability + skeleton compile
```

## Commands

Every command prints one deterministic JSON envelope
(`{ ok, result|error, exitCode }`) and exits `0` pass / `2` violation / `1` error.

```
capabilities                Probe external tools.
docx-introspect   --input <docx> [--output <m.json>]      DOCX → manifest (IR).
typst-skeleton    --manifest <m.json> [--output <f>]      Manifest → Typst skeleton.
validate-schema   --manifest <m.json> (--typst <f> | --against-manifest <m2.json>)
pdf-compliance    --input <pdf> [--workdir <dir>]         compatibility checks.
visual-diff       --reference <a.pdf> --candidate <b.pdf> [--reference-provenance docx-render|typst-compile|pdf|unknown]
                                                            [--dpi N] [--max-normalized-rmse F] [--max-diff-page-fraction F] [--fuzz-percent N]
ris-roundtrip     --input <f.ris> [--required TY,AU,PY,TI,JO]
fixtures-gen      --output <dir>                          Regenerate the sample DOCX.
```

### `visual-diff` provenance

Visual fidelity is **only** asserted when the reference is a real DOCX render:
pass `--reference-provenance docx-render`. Any other provenance still computes
per-page RMSE but reports `visualFidelityAssertable: false` and `passed: false`.
This encodes the rule: *do not claim visual fidelity without a real DOCX*.

## Layout

```
src/
  docx/        ordered XML parser, manifest Schema, introspection
  typst/       skeleton generator, schema/placeholder validation
  pdf/         poppler/qpdf inspection, compliance checks
  visualdiff/  pdftoppm + ImageMagick page-image diff harness
  ris/         lossless RIS round-trip checker
  externals/   capability detection, shell + filesystem Effect services
  fixtures/    deterministic minimal-DOCX generator
  cli/         dispatcher, arg parser, JSON envelope rendering
test/          vitest suite (unit + external-gated)
bin/nisaba-tools.ts          CLI entry
verify.sh                   full local verification (CI-ready)
```

## Testing

`bunx vitest run` runs the unit suite plus external-gated tests (PDF compliance,
visual diff) that render the fixture DOCX with LibreOffice. The gated tests
`console.warn`-skip when a tool is absent, so the suite is portable.
