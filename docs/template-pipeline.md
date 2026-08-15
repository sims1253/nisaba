# DOCX-to-Typst template pipeline

The `tools/` workspace contains deterministic utilities for inspecting the structure of a DOCX
file, generating a Typst starting point, and comparing rendered output. This is an optional
import aid for general document projects; DOCX is not part of the core storage model.

## Workflow

1. **Introspect** a source DOCX into a stable JSON manifest.
2. **Generate** a Typst skeleton containing detected headings, placeholders, tables, images,
   page geometry, headers, and numbering hints.
3. **Validate** a candidate source or manifest against the captured structure.
4. **Render and compare** reference and candidate PDFs using explicit thresholds.
5. **Record provenance** for source files, tool versions, commands, and output hashes.

The repository's `fixtures/templates/sample-document.docx` is synthetic and exists only to
exercise the pipeline. It is not a claim of visual parity with third-party templates.

## Commands

The CLI entry point is `bin/nisaba-tools.ts` (the `nisaba-tools` bin declared
in `tools/package.json`). From `tools/`:

```bash
bun install --frozen-lockfile
bun bin/nisaba-tools.ts capabilities
bun bin/nisaba-tools.ts docx-introspect \
  --input ../fixtures/templates/sample-document.docx \
  --output /tmp/manifest.json
bun bin/nisaba-tools.ts typst-skeleton \
  --manifest /tmp/manifest.json \
  --output /tmp/template.typ
bun bin/nisaba-tools.ts validate-schema \
  --manifest /tmp/manifest.json \
  --typst /tmp/template.typ
```

From the repository root, use `bun tools/bin/nisaba-tools.ts <command>` (see
[`fixtures/templates/README.md`](../fixtures/templates/README.md)).

For a visual comparison:

```bash
bun bin/nisaba-tools.ts visual-diff \
  --reference reference.pdf \
  --candidate candidate.pdf \
  --workdir /tmp/visual-diff
```

Thresholds are explicit inputs. A successful comparison means only that the configured checks
passed; it does not establish semantic equivalence or suitability for a particular use.

## Determinism

The synthetic DOCX uses fixed ZIP timestamps, stable part ordering, and fixed compression. JSON
output is canonically ordered. Re-running generation should produce the same bytes and hashes.
When output changes intentionally, regenerate and review both golden manifest and Typst skeleton.
