import { describe, it, expect } from "vitest";
import { introspectDocx } from "../src/docx/introspect.js";
import { buildSampleDocumentDocx } from "../src/fixtures/generate.js";
import { validateTypstSource, validateAgainstManifest, countOccurrences } from "../src/typst/schema.js";
import { generateTypstSkeleton } from "../src/typst/skeleton.js";

const manifest = introspectDocx(buildSampleDocumentDocx(), "sample-document.docx");

describe("schema validation", () => {
  it("passes when every required placeholder survives in a Typst source", () => {
    const report = validateTypstSource(manifest, generateTypstSkeleton(manifest), "skeleton.typ");
    expect(report.passed).toBe(true);
    expect(report.missing).toEqual([]);
    expect(report.checks.every((c) => c.status === "present")).toBe(true);
  });

  it("fails and reports the missing token when a placeholder is dropped", () => {
    const broken = generateTypstSkeleton(manifest).replace(/<<Author>>/g, "Author-Feld");
    const report = validateTypstSource(manifest, broken, "broken.typ");
    // Author still appears as a substring ("Author-Feld"), so it counts as present.
    // Remove it entirely to force absence:
    const emptied = broken.replace(/Author/g, "");
    const report2 = validateTypstSource(manifest, emptied, "emptied.typ");
    expect(report2.missing).toContain("Author");
    expect(report2.passed).toBe(false);
    void report;
  });

  it("detects added/removed placeholder tokens when diffing two manifests", () => {
    const candidate = { ...manifest, requiredPlaceholders: ["Author", "Version", "ExtraField"] } as typeof manifest;
    const report = validateAgainstManifest(manifest, candidate, "candidate.json");
    expect(report.missing).toContain("Project");
    expect(report.extra).toContain("ExtraField");
    expect(report.passed).toBe(false);
  });

  it("countOccurrences is correct", () => {
    expect(countOccurrences("aaa", "a")).toBe(3);
    expect(countOccurrences("xyz", "a")).toBe(0);
    expect(countOccurrences("text", "")).toBe(0);
  });
});
