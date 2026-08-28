import { describe, it, expect } from "vitest";
import { Schema } from "effect";
import { zipSync, strToU8 } from "fflate";
import { buildSampleDocumentDocx } from "../src/fixtures/generate.js";
import { introspectDocx } from "../src/docx/introspect.js";
import { ManifestSchema } from "../src/docx/manifest.js";
import { stableStringify, hashBytes, hashValue } from "../src/json.js";
import { MalformedDocxError } from "../src/errors.js";

const docx = buildSampleDocumentDocx();
const manifest = introspectDocx(docx, "sample-document.docx");

describe("fixture + manifest", () => {
  it("fixture ZIP is byte-deterministic", () => {
    expect(hashBytes(buildSampleDocumentDocx())).toBe(hashBytes(docx));
  });

  it("introspection is deterministic (same bytes → same manifest)", () => {
    const again = introspectDocx(buildSampleDocumentDocx(), "sample-document.docx");
    expect(hashValue(again)).toBe(hashValue(manifest));
  });

  it("manifest round-trips through the Schema contract", () => {
    const decoded = Schema.decodeUnknownSync(ManifestSchema)(JSON.parse(stableStringify(manifest, 0)));
    expect(hashValue(decoded)).toBe(hashValue(manifest));
  });
});

describe("introspected content", () => {
  it("captures A4 portrait geometry", () => {
    const s = manifest.document.sections[0]!;
    expect(s.pageSize.widthTwips).toBe(11906);
    expect(s.pageSize.heightTwips).toBe(16838);
    expect(s.pageSize.orientation).toBe("portrait");
  });

  it("preserves body order: heading, toc-field, placeholder paragraphs, table, page break, headings", () => {
    const kinds = manifest.document.blocks.map((b) => b.kind);
    // Title heading, contents heading, TOC field paragraph,
    // then content paragraphs, a table, the page break, and two headings.
    expect(kinds[0]).toBe("heading");
    expect(kinds).toContain("table");
    expect(kinds).toContain("pageBreak");
    expect(kinds.indexOf("table")).toBeLessThan(kinds.indexOf("pageBreak"));
  });

  it("records all three required placeholders", () => {
    expect(manifest.requiredPlaceholders).toEqual([
      "Author",
      "Project",
      "Version",
    ]);
  });

  it("detects the TOC field and the external hyperlink field", () => {
    const types = manifest.document.fields.map((f) => f.type);
    expect(types).toContain("toc");
    // Hyperlink is detected via w:hyperlink → paragraph flag, not a field instruction.
    const hasHyperlinkParagraph = manifest.document.blocks.some(
      (b) => b.kind === "paragraph" && "flags" in b && b.flags.hasHyperlink,
    );
    expect(hasHyperlinkParagraph).toBe(true);
  });

  it("summarises the table (2×2, header row) and the inline image", () => {
    expect(manifest.document.tables).toEqual([
      expect.objectContaining({ rows: 2, columns: 2, hasHeaderRow: true, styleId: "TableGrid" }),
    ]);
    expect(manifest.document.images.length).toBe(1);
    expect(manifest.document.images[0]!.part).toBe("word/media/logo.png");
  });

  it("records the numbering definition and the header reference", () => {
    expect(manifest.document.numbering).toEqual([
      expect.objectContaining({ numId: 1, abstractNumId: 0, levels: 2 }),
    ]);
    expect(manifest.document.headers.map((h) => h.part)).toContain("word/header1.xml");
  });

  it("extracts core + app metadata", () => {
    expect(manifest.metadata.title).toBe("Nisaba Sample Document Fixture");
    expect(manifest.metadata.templateName).toBe("NisabaSampleTemplate.dotx");
    expect(manifest.metadata.pages).toBe(3);
  });
});

describe("malformed-package guards", () => {
  // The body walk used to reach for `documentRoot.children[0]!`; an empty
  // word/document.xml has no root element at all, so that must fail typed.
  const docxWithDocumentXml = (documentXml: string): Uint8Array =>
    zipSync({ "word/document.xml": strToU8(documentXml) });

  it("throws MalformedDocxError for an empty word/document.xml", () => {
    let thrown: unknown;
    try {
      introspectDocx(docxWithDocumentXml("   "), "empty.docx");
    } catch (e) {
      thrown = e;
    }
    expect(thrown).toBeInstanceOf(MalformedDocxError);
    expect((thrown as MalformedDocxError).reason).toContain("no XML root element");
  });

  it("still throws MalformedDocxError when the part is missing outright", () => {
    expect(() => introspectDocx(zipSync({ "word/styles.xml": strToU8("<styles/>") }), "no-doc.docx")).toThrow(
      MalformedDocxError,
    );
  });
});
