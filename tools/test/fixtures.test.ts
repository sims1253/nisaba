import { describe, it, expect } from "vitest";
import { buildSampleDocumentDocx } from "../src/fixtures/generate.js";
import { unzipSync, strFromU8 } from "fflate";
import { hashBytes } from "../src/json.js";

describe("fixture generator", () => {
  it("produces a valid OOXML package with the mandatory parts", () => {
    const bytes = buildSampleDocumentDocx();
    const parts = Object.keys(unzipSync(bytes));
    expect(parts).toContain("[Content_Types].xml");
    expect(parts).toContain("word/document.xml");
    expect(parts).toContain("word/styles.xml");
    expect(parts).toContain("docProps/core.xml");
    expect(parts).toContain("docProps/app.xml");
  });

  it("is byte-deterministic across invocations", () => {
    const a = buildSampleDocumentDocx();
    const b = buildSampleDocumentDocx();
    expect(hashBytes(a)).toBe(hashBytes(b));
  });

  it("embeds the required-placeholder markers in document.xml (XML-escaped)", () => {
    const bytes = buildSampleDocumentDocx();
    const parts = unzipSync(bytes);
    const doc = strFromU8(parts["word/document.xml"]!);
    expect(doc).toContain("&lt;&lt;Author&gt;&gt;");
    expect(doc).toContain("&lt;&lt;Project&gt;&gt;");
  });
});
