import { describe, it, expect } from "vitest";
import { generateTypstSkeleton, toTypstIdent } from "../src/typst/skeleton.js";
import { introspectDocx } from "../src/docx/introspect.js";
import { buildSampleDocumentDocx } from "../src/fixtures/generate.js";
import { hashValue } from "../src/json.js";

const manifest = introspectDocx(buildSampleDocumentDocx(), "sample-document.docx");
const skeleton = generateTypstSkeleton(manifest);

describe("typst skeleton", () => {
  it("is deterministic", () => {
    expect(generateTypstSkeleton(manifest)).toBe(skeleton);
  });

  it("states it is a skeleton, not a fidelity claim", () => {
    expect(skeleton).toMatch(/Skeleton/);
    expect(skeleton.toLowerCase()).toMatch(/keine Fidelity-Garantie|fidelity/i);
  });

  it("emits A4 page geometry from twips→pt", () => {
    expect(skeleton).toContain("width: 595.3pt");
    expect(skeleton).toContain("height: 841.9pt");
  });

  it("declares the function allowlist", () => {
    expect(skeleton).toMatch(/"figure", "table", "cite"/);
  });

  it("preserves every required placeholder as a #let field and a literal marker", () => {
    for (const token of manifest.requiredPlaceholders) {
      expect(skeleton).toContain(`<<${token}>>`);
    }
  });

  it("sanitises tokens to Typst identifiers", () => {
    expect(toTypstIdent("Author")).toBe("author");
    expect(toTypstIdent("Project Name")).toBe(
      "project_name",
    );
  });

  it("includes a manifest hash for traceability", () => {
    expect(skeleton).toContain(hashValue(manifest).slice(0, 16));
  });
});
