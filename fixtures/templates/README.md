# Document fixtures

This directory contains synthetic, deterministic fixtures for the optional DOCX-to-Typst tools.

## `sample-document.docx`

A small valid OOXML package exercising page geometry, heading styles, `<<…>>` placeholders,
a hyperlink, an inline image, a table, a numbered list, a page break, and a table-of-contents
field. Code in `tools/src/fixtures/generate.ts` generates it with fixed ZIP timestamps and stable
part order.

Regenerate it from the repository root:

```bash
bun tools/bin/nisaba-tools.ts fixtures-gen --output fixtures/templates
```

## Golden outputs

- `golden/sample-document.manifest.json` — deterministic introspection output
- `golden/sample-document.skeleton.typ` — generated Typst starting point

Regenerate both:

```bash
bun tools/bin/nisaba-tools.ts docx-introspect \
  --input fixtures/templates/sample-document.docx \
  --output fixtures/templates/golden/sample-document.manifest.json
bun tools/bin/nisaba-tools.ts typst-skeleton \
  --manifest fixtures/templates/golden/sample-document.manifest.json \
  --output fixtures/templates/golden/sample-document.skeleton.typ
```

`pipeline-evidence.md` is a blank record for validating a private, real-world source template.
Do not commit proprietary source documents or rendered copies.
