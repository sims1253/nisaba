/**
 * Deterministic sample-DOCX fixture generator.
 *
 * Produces a small but *valid* OOXML package that exercises every code path the
 * introspector cares about: A4 section geometry, heading styles (outlineLvl), a
 * `<<…>>` placeholder, a mandatory `<<Project>>` block, a hyperlink, an inline
 * image, a table, a numbered list, a page break, and a TOC field.
 *
 * The ZIP is byte-stable: fixed compression level and a fixed mtime on every
 * entry, so re-running yields an identical file (determinism). This is a
 * *fixture*, not a real DOCX template — visual fidelity is only ever claimed
 * against a real DOCX render (see `docs/template-pipeline.md`).
 */
import { zipSync, strToU8 } from "fflate";
import { base64ToBytes } from "./base64.js";

/** A 1×1 transparent PNG used as the inline-image fixture. */
const PNG_1x1 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

export interface FixtureParts {
  readonly "[Content_Types].xml": string;
  readonly "_rels/.rels": string;
  readonly "word/_rels/document.xml.rels": string;
  readonly "word/document.xml": string;
  readonly "word/styles.xml": string;
  readonly "word/numbering.xml": string;
  readonly "word/header1.xml": string;
  readonly "docProps/core.xml": string;
  readonly "docProps/app.xml": string;
  readonly "word/media/logo.png": Uint8Array;
}

/** All XML string parts of the fixture, before zipping. */
export function fixtureParts(): FixtureParts {
  return {
    "[Content_Types].xml": CONTENT_TYPES,
    "_rels/.rels": ROOT_RELS,
    "word/_rels/document.xml.rels": DOC_RELS,
    "word/document.xml": DOCUMENT_XML,
    "word/styles.xml": STYLES_XML,
    "word/numbering.xml": NUMBERING_XML,
    "word/header1.xml": HEADER_XML,
    "docProps/core.xml": CORE_XML,
    "docProps/app.xml": APP_XML,
    "word/media/logo.png": base64ToBytes(PNG_1x1),
  };
}

/** Build the fixture as deterministic ZIP bytes. */
export function buildSampleDocumentDocx(): Uint8Array {
  const parts = fixtureParts();
  const zippable: Record<string, [Uint8Array, { level: 0 | 9; mtime: number }]> = {};
  const mtime = Date.UTC(2020, 0, 1, 0, 0, 0);
  (Object.keys(parts) as (keyof FixtureParts)[]).sort().forEach((name) => {
    const v = parts[name];
    const bytes = typeof v === "string" ? strToU8(v) : v;
    zippable[name] = [bytes, { level: 9, mtime }];
  });
  return zipSync(zippable);
}

// ---------------------------------------------------------------------------

// (no text escaping helpers needed — fixture XML is authored literally above)

const CONTENT_TYPES = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>`;

const ROOT_RELS = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>`;

const DOC_RELS = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/sample" TargetMode="External"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/logo.png"/></Relationships>`;

const HEADER_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>Nisaba Fixture Header</w:t></w:r></w:p></w:hdr>`;

const CORE_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Nisaba Sample Document Fixture</dc:title><dc:creator>Nisaba Tools</dc:creator><dc:subject>sample document</dc:subject><dc:keywords>fixture, docx, template</dc:keywords><dc:language>en-US</dc:language><cp:revision>1</cp:revision><dcterms:created xsi:type="dcterms:W3CDTF">2020-01-01T00:00:00Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2020-01-02T00:00:00Z</dcterms:modified></cp:coreProperties>`;

const APP_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties"><Application>Nisaba Fixture Generator</Application><AppVersion>0.1.0</AppVersion><Template>NisabaSampleTemplate.dotx</Template><Company>Nisaba</Company><Pages>3</Pages><Words>42</Words><Characters>300</Characters></Properties>`;

const STYLES_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style><w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:pPr><w:outlineLvl w:val="2"/></w:pPr></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:uiPriority w:val="99"/></w:style><w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/></w:style></w:styles>`;

const NUMBERING_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%2."/><w:lvlJc w:val="left"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>`;

const DOCUMENT_XML = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Sample Document</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Table of Contents</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText> TOC \\o "1-3" \\h \\z \\u </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>(Table of Contents)</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:t>Author: </w:t></w:r><w:r><w:t>&lt;&lt;Author&gt;&gt;</w:t></w:r></w:p><w:p><w:r><w:t>&lt;&lt;Project&gt;&gt;</w:t></w:r></w:p><w:p><w:hyperlink r:id="rId2"><w:r><w:rPr><w:rStyle w:val="Hyperlink"/></w:rPr><w:t>External Link</w:t></w:r></w:hyperlink></w:p><w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="914400" cy="457200"/><wp:docPr id="1" name="Logo"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:blipFill><a:blip r:embed="rId3"/></pic:blipFill><pic:spPr/></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tr><w:trPr><w:tblHeader/></w:trPr><w:tc><w:p><w:r><w:t>Label</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>Version</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>&lt;&lt;Version&gt;&gt;</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>First item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Second item</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading3"/></w:pPr><w:r><w:t>List of Tables</w:t></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading3"/></w:pPr><w:r><w:t>List of Figures</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId1"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="720" w:footer="720" w:gutter="0"/><w:cols w:space="720"/></w:sectPr></w:body></w:document>`;
