/**
 * DOCX → {@link Manifest} introspection.
 *
 * Pure and deterministic: the same bytes + filename always yield the same
 * manifest. Tolerant — a missing `styles.xml` or `numbering.xml` degrades to
 * fewer details rather than failing, because a real DOCX template that is
 * slightly non-conformant must still be introspectable. Only the absence of
 * `word/document.xml` is fatal.
 *
 * Body order is preserved exactly (see {@link ./xmlparser.ts}), so paragraphs
 * and tables appear in the manifest's `blocks` in their true document order.
 */
import { TOOL_NAME, VERSION } from "../version.js";
import { hashBytes, hashText } from "../json.js";
import { MalformedDocxError } from "../errors.js";
import {
  attr,
  child,
  children,
  numAttr,
  parseDocxBytes,
  parseRels,
  parseXmlDocument,
  textOf,
  type ParsedDocx,
  type Rel,
  type XmlEl,
} from "./parse.js";
import type { Block, Manifest } from "./manifest.js";

/** Strip `readonly` modifiers for mutable builder locals (arrays/objects). */
type Writable<T> = { -readonly [K in keyof T]: T[K] };

interface StyleDef {
  readonly type: string;
  readonly name?: string;
  readonly basedOn?: string;
  readonly outlineLvl?: number;
  readonly isDefault: boolean;
  readonly uiPriority?: number;
}

interface WalkCtx {
  readonly styles: Map<string, StyleDef>;
  readonly headingLevels: Map<string, number>;
  readonly rels: Map<string, Rel>;
  readonly numbering: Map<number, { abstractNumId: number; levels: number; styleLink?: string }>;
}

/** Introspect raw DOCX bytes into a deterministic manifest. */
export function introspectDocx(bytes: Uint8Array, fileName: string): Manifest {
  const doc = parseDocxBytes(bytes, fileName);
  // parseDocxBytes already throws when this part is absent; re-checking keeps
  // the invariant local instead of trusting it via a non-null assertion.
  const documentXml = doc.parts.get("word/document.xml");
  if (documentXml === undefined) {
    throw new MalformedDocxError({
      path: fileName,
      missingPart: "word/document.xml",
      reason: "package is missing word/document.xml — not a valid DOCX",
    });
  }
  const documentRoot = parseXmlDocument(documentXml);

  const styles = parseStyles(doc.parts.get("word/styles.xml"));
  const headingLevels = computeHeadingLevels(styles);
  const numbering = parseNumbering(doc.parts.get("word/numbering.xml"));
  const rels = parseRels(doc.parts.get("word/_rels/document.xml.rels"));

  const ctx: WalkCtx = { styles, headingLevels, rels, numbering };

  // Not guaranteed: parseXmlDocument returns a synthetic root, so an empty or
  // unparseable word/document.xml leaves it without any child element. A body
  // walk over `undefined` would die with a raw TypeError — fail typed instead.
  const docEl = child(documentRoot, "document") ?? documentRoot.children[0];
  if (docEl === undefined) {
    throw new MalformedDocxError({
      path: fileName,
      reason: "word/document.xml contains no XML root element",
    });
  }
  const body = child(docEl, "body") ?? docEl;

  const sections = extractSections(body, rels);
  const { blocks, placeholders, fields, tables, images } = walkBody(body, ctx);

  const headers = extractHeaderFooterRefs(doc, body, "header");
  const footers = extractHeaderFooterRefs(doc, body, "footer");
  const footnotes = countNotes(doc.parts.get("word/footnotes.xml"), "footnote");
  const endnotes = countNotes(doc.parts.get("word/endnotes.xml"), "endnote");
  const comments = countComments(doc.parts.get("word/comments.xml"));

  const metadata = extractMetadata(doc);
  const ooxmlNamespace = extractNamespace(documentXml);

  const requiredPlaceholders = Array.from(new Set(placeholders.map((p) => p.token))).sort();

  return {
    schemaVersion: "1",
    generatedBy: `${TOOL_NAME}@${VERSION}`,
    source: {
      fileName,
      sha256: hashBytes(doc.bytes),
      bytes: doc.bytes.length,
      format: "docx",
      ooxmlNamespace,
    },
    metadata,
    parts: doc.names.slice(),
    document: {
      sha256: hashText(documentXml),
      sections,
      blocks,
      placeholders,
      fields,
      tables,
      images,
      numbering: Array.from(numbering.entries()).map(([numId, v]) => ({
        numId,
        abstractNumId: v.abstractNumId,
        levels: v.levels,
        styleLink: v.styleLink,
      })),
      headers,
      footers,
      footnotes,
      endnotes,
      comments,
    },
    styles: summarizeStyles(styles),
    requiredPlaceholders,
  };
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

function parseStyles(xml: string | undefined): Map<string, StyleDef> {
  const out = new Map<string, StyleDef>();
  if (!xml) return out;
  const root = parseXmlDocument(xml);
  for (const s of children(child(root, "styles"), "style")) {
    const id = attr(s, "styleId") ?? "";
    const type = attr(s, "type") ?? "";
    const isDefault = attr(s, "default") === "1";
    const nameEl = child(s, "name");
    const name = attr(nameEl, "val") ?? textOf(nameEl);
    const basedOn = attr(child(s, "basedOn"), "val") ?? textOf(child(s, "basedOn"));
    const uiPriority = numAttr(child(s, "uiPriority"), "val");
    const outlineLvl = numAttr(child(child(s, "pPr"), "outlineLvl"), "val");
    if (id) {
      out.set(id, {
        type,
        name: name || undefined,
        basedOn: basedOn || undefined,
        outlineLvl,
        isDefault,
        uiPriority,
      });
    }
  }
  return out;
}

function headingLevelFromName(name: string | undefined): number | undefined {
  if (!name) return undefined;
  const m = name.match(/(?:heading|überschrift|titolo|titel|title)[\s\-_]*([1-9])/i);
  return m ? Number(m[1]) : undefined;
}

function computeHeadingLevels(styles: Map<string, StyleDef>): Map<string, number> {
  const out = new Map<string, number>();
  for (const [id, def] of styles) {
    if (def.type !== "paragraph") continue;
    let level = def.outlineLvl !== undefined ? def.outlineLvl + 1 : headingLevelFromName(def.name);
    if (level === undefined) {
      // follow basedOn chain
      const seen = new Set<string>();
      let cur: StyleDef | undefined = def;
      while (cur && !seen.has(id)) {
        seen.add(id);
        if (cur.outlineLvl !== undefined) {
          level = cur.outlineLvl + 1;
          break;
        }
        const named = headingLevelFromName(cur.name);
        if (named !== undefined) {
          level = named;
          break;
        }
        cur = cur.basedOn ? styles.get(cur.basedOn) : undefined;
      }
    }
    if (level !== undefined) out.set(id, level);
  }
  return out;
}

function summarizeStyles(styles: Map<string, StyleDef>): Manifest["styles"] {
  const toSummary = (type: string) =>
    Array.from(styles.entries())
      .filter(([, d]) => d.type === type)
      .map(([id, d]) => ({
        id,
        name: d.name,
        basedOn: d.basedOn,
        next: undefined,
        isDefault: d.isDefault,
        uiPriority: d.uiPriority,
      }))
      .sort((a, b) => a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
  let defaultParagraph: string | undefined;
  let defaultRun: string | undefined;
  for (const [id, d] of styles) {
    if (d.isDefault && d.type === "paragraph") defaultParagraph = id;
    if (d.isDefault && d.type === "character") defaultRun = id;
  }
  return {
    paragraph: toSummary("paragraph"),
    character: toSummary("character"),
    table: toSummary("table"),
    latentStyleCount: undefined,
    defaultParagraph,
    defaultRun,
  };
}

// ---------------------------------------------------------------------------
// Numbering
// ---------------------------------------------------------------------------

function parseNumbering(
  xml: string | undefined,
): Map<number, { abstractNumId: number; levels: number; styleLink?: string }> {
  const out = new Map<number, { abstractNumId: number; levels: number; styleLink?: string }>();
  if (!xml) return out;
  const root = parseXmlDocument(xml);
  const numberingEl = child(root, "numbering");
  const abstractLevels = new Map<number, number>();
  for (const a of children(numberingEl, "abstractNum")) {
    const aid = numAttr(a, "abstractNumId") ?? -1;
    abstractLevels.set(aid, children(a, "lvl").length);
  }
  for (const n of children(numberingEl, "num")) {
    const numId = numAttr(n, "numId") ?? -1;
    const abstractNumId = numAttr(child(n, "abstractNumId"), "val") ?? -1;
    const styleLink = attr(child(child(n, "numPrLink"), "styleLink"), "val");
    out.set(numId, {
      abstractNumId,
      levels: abstractLevels.get(abstractNumId) ?? 0,
      styleLink: styleLink || undefined,
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

function extractSections(
  body: XmlEl,
  rels: Map<string, Rel>,
): Manifest["document"]["sections"] {
  const sects: XmlEl[] = [];
  for (const p of children(body, "p")) {
    const sp = child(child(p, "pPr"), "sectPr");
    if (sp) sects.push(sp);
  }
  const finalSp = child(body, "sectPr");
  if (finalSp) sects.push(finalSp);

  return sects.map((sp, index) => {
    const pgSz = child(sp, "pgSz");
    const pgMar = child(sp, "pgMar");
    const cols = child(sp, "cols");
    const headerRefs = children(sp, "headerReference").map((h) => resolveRel(rels, attr(h, "id")));
    const footerRefs = children(sp, "footerReference").map((f) => resolveRel(rels, attr(f, "id")));
    const orient = attr(pgSz, "orient") === "landscape" ? "landscape" : "portrait";
    return {
      index,
      pageSize: {
        widthTwips: numAttr(pgSz, "w") ?? 11906,
        heightTwips: numAttr(pgSz, "h") ?? 16838,
        orientation: orient,
      },
      margins: {
        topTwips: numAttr(pgMar, "top") ?? 1440,
        bottomTwips: numAttr(pgMar, "bottom") ?? 1440,
        leftTwips: numAttr(pgMar, "left") ?? 1800,
        rightTwips: numAttr(pgMar, "right") ?? 1800,
        headerTwips: numAttr(pgMar, "header") ?? 720,
        footerTwips: numAttr(pgMar, "footer") ?? 720,
      },
      columns: {
        count: numAttr(cols, "num") ?? 1,
        spaceTwips: numAttr(cols, "space") ?? 720,
      },
      headerRefs: headerRefs.filter((x): x is string => !!x),
      footerRefs: footerRefs.filter((x): x is string => !!x),
    };
  });
}

function resolveRel(rels: Map<string, Rel>, id: string | undefined): string | undefined {
  if (!id) return undefined;
  return rels.get(id)?.target;
}

// ---------------------------------------------------------------------------
// Body walk → blocks + inventories
// ---------------------------------------------------------------------------

const BRACKET_PLACEHOLDER = /<<([^<>]+?)>>/g;

interface WalkResult {
  readonly blocks: Block[];
  readonly placeholders: Manifest["document"]["placeholders"];
  readonly fields: Manifest["document"]["fields"];
  readonly tables: Manifest["document"]["tables"];
  readonly images: Manifest["document"]["images"];
}

function walkBody(body: XmlEl, ctx: WalkCtx): WalkResult {
  const blocks: Block[] = [];
  const placeholders = new Map<string, { kind: string; count: number; sample?: string }>();
  const fields = new Map<string, { type: string; count: number }>();
  const tables: Manifest["document"]["tables"][number][] = [];
  const images: Manifest["document"]["images"][number][] = [];
  const sectionCounter = { i: 0 };

  const addPlaceholder = (token: string, kind: string) => {
    const key = `${kind}\u0000${token}`;
    const e = placeholders.get(key);
    placeholders.set(key, { kind, count: (e?.count ?? 0) + 1, sample: e?.sample ?? token });
  };
  const addField = (instruction: string, type: string) => {
    const norm = instruction.trim().replace(/\s+/g, " ");
    const key = `${type}\u0000${norm}`;
    const e = fields.get(key);
    fields.set(key, { type, count: (e?.count ?? 0) + 1 });
  };

  const paraHandlers = (): ParaHandlers => ({
    blocks,
    addPlaceholder,
    addField,
    sectionCounter,
    tables,
    images,
  });

  const visit = (container: XmlEl) => {
    for (const c of container.children) {
      switch (c.tag) {
        case "p":
          processParagraph(c, ctx, paraHandlers());
          break;
        case "tbl": {
          const idx = tables.length;
          tables.push(summarizeTable(c, idx));
          blocks.push({ kind: "table", ref: idx });
          // Descend into cells to capture inline placeholders/fields/images, but
          // do NOT emit cell paragraphs as body blocks (the table is one block).
          const cellDiscard: Block[] = [];
          for (const tr of children(c, "tr")) {
            for (const tc of children(tr, "tc")) {
              for (const p of children(tc, "p"))
                processParagraph(p, ctx, { ...paraHandlers(), blocks: cellDiscard });
            }
          }
          break;
        }
        case "sdt": {
          processSdt(c, addPlaceholder);
          const content = child(c, "sdtContent");
          if (content) visit(content);
          break;
        }
        default:
          break;
      }
    }
  };

  visit(body);

  return {
    blocks,
    placeholders: (Array.from(placeholders.entries())
      .map(([k, v]) => ({
        token: k.split("\u0000").slice(1).join("\u0000"),
        kind: v.kind,
        count: v.count,
        sample: v.sample,
      }))
      .sort((a, b) => a.token < b.token ? -1 : a.token > b.token ? 1 : a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0)) as Manifest["document"]["placeholders"],
    fields: (Array.from(fields.entries())
      .map(([k, v]) => ({
        type: v.type,
        instruction: k.split("\u0000").slice(1).join("\u0000"),
        count: v.count,
      }))
      .sort((a, b) => a.type < b.type ? -1 : a.type > b.type ? 1 : a.instruction < b.instruction ? -1 : a.instruction > b.instruction ? 1 : 0)) as Manifest["document"]["fields"],
    tables,
    images,
  };
}

interface ParaHandlers {
  readonly blocks: Block[];
  readonly addPlaceholder: (token: string, kind: string) => void;
  readonly addField: (instruction: string, type: string) => void;
  readonly sectionCounter: { i: number };
  readonly tables: Manifest["document"]["tables"][number][];
  readonly images: Manifest["document"]["images"][number][];
}

function processParagraph(p: XmlEl, ctx: WalkCtx, h: ParaHandlers): void {
  const pPr = child(p, "pPr");
  const styleId = attr(child(pPr, "pStyle"), "val");
  const hasList = !!child(pPr, "numPr");

  if (child(pPr, "sectPr")) {
    h.blocks.push({ kind: "sectionBreak", sectionIndex: h.sectionCounter.i });
    h.sectionCounter.i += 1;
  }

  const acc = collectRunText(p, ctx, h);

  const directLvl = numAttr(child(pPr, "outlineLvl"), "val");
  const level: number | undefined =
    directLvl !== undefined
      ? directLvl + 1
      : styleId
        ? ctx.headingLevels.get(styleId)
        : undefined;

  let m: RegExpExecArray | null;
  BRACKET_PLACEHOLDER.lastIndex = 0;
  while ((m = BRACKET_PLACEHOLDER.exec(acc.text)) !== null) {
    h.addPlaceholder(m[1]!.trim(), "bracket");
  }

  if (level !== undefined && level >= 1 && level <= 9) {
    h.blocks.push({ kind: "heading", level, text: acc.text.trim(), styleId });
    return;
  }
  if (acc.pageBreak && acc.text.trim() === "" && !acc.hasField && !acc.hasImage) {
    h.blocks.push({ kind: "pageBreak" });
    return;
  }
  h.blocks.push({
    kind: "paragraph",
    styleId,
    text: acc.text,
    flags: {
      hasHyperlink: acc.hasHyperlink,
      hasField: acc.hasField,
      hasImage: acc.hasImage,
      hasList,
    },
  });
}

function processSdt(
  sdt: XmlEl,
  addPlaceholder: (token: string, kind: string) => void,
): void {
  const pr = child(sdt, "sdtPr");
  const tag = attr(child(pr, "tag"), "val");
  const alias = attr(child(pr, "alias"), "val");
  if (tag) addPlaceholder(tag, "sdt");
  else if (alias) addPlaceholder(alias, "sdt");
  else {
    const content = child(sdt, "sdtContent");
    const trimmed = collectPlainText(content).trim();
    if (/^<<.+>>$/.test(trimmed)) {
      addPlaceholder(trimmed.replace(/^<<|>>$/g, "").trim(), "sdt");
    }
  }
}

function summarizeTable(tbl: XmlEl, index: number): Manifest["document"]["tables"][number] {
  const rows = children(tbl, "tr");
  const cols = rows.reduce((max, tr) => {
    const span = children(tr, "tc").reduce((s, tc) => s + (numAttr(child(child(tc, "tcPr"), "gridSpan"), "val") ?? 1), 0);
    return Math.max(max, span);
  }, 0);
  const styleId = attr(child(child(tbl, "tblPr"), "tblStyle"), "val");
  const firstRow = rows[0];
  const hasHeaderRow = !!firstRow && !!child(child(firstRow, "trPr"), "tblHeader");
  return { index, rows: rows.length, columns: cols, styleId: styleId || undefined, hasHeaderRow };
}

interface RunAccumulator {
  text: string;
  hasHyperlink: boolean;
  hasField: boolean;
  hasImage: boolean;
  pageBreak: boolean;
}

function collectRunText(p: XmlEl, ctx: WalkCtx, h: ParaHandlers): RunAccumulator {
  const acc: RunAccumulator = { text: "", hasHyperlink: false, hasField: false, hasImage: false, pageBreak: false };

  const walk = (el: XmlEl) => {
    for (const c of el.children) {
      switch (c.tag) {
        case "t":
          acc.text += c.text;
          break;
        case "hyperlink":
          acc.hasHyperlink = true;
          walk(c);
          break;
        case "br":
          if (attr(c, "type") === "page") acc.pageBreak = true;
          break;
        case "drawing":
        case "pict":
          acc.hasImage = true;
          recordImage(c, ctx, h);
          break;
        case "instrText": {
          const instr = c.text;
          const ft = classifyField(instr);
          acc.hasField = true;
          h.addField(instr, ft);
          if (ft === "mergefield") {
            const mm = instr.match(/MERGEFIELD\s+(\S+)/i);
            if (mm) h.addPlaceholder(mm[1]!, "mergefield");
          }
          break;
        }
        case "fldSimple": {
          const instr = attr(c, "instr") ?? "";
          if (instr) {
            h.addField(instr, classifyField(instr));
            acc.hasField = true;
          }
          walk(c);
          break;
        }
        case "fldChar":
          acc.hasField = true;
          break;
        case "sdt":
          processSdt(c, h.addPlaceholder);
          walkChildren(c, h, ctx, acc);
          break;
        default:
          walk(c);
      }
    }
  };
  walk(p);
  return acc;
}

function walkChildren(sdt: XmlEl, h: ParaHandlers, ctx: WalkCtx, acc: RunAccumulator): void {
  const content = child(sdt, "sdtContent");
  if (!content) return;
  // Treat SDT run content as plain run content for text/flags.
  for (const c of content.children) {
    switch (c.tag) {
      case "t":
        acc.text += c.text;
        break;
      case "instrText": {
        const instr = c.text;
        h.addField(instr, classifyField(instr));
        acc.hasField = true;
        break;
      }
      default:
        break;
    }
  }
  void ctx;
}

function collectPlainText(container: XmlEl | undefined): string {
  if (!container) return "";
  let out = "";
  const walk = (el: XmlEl) => {
    if (el.tag === "t") out += el.text;
    for (const c of el.children) walk(c);
  };
  walk(container);
  return out;
}

function recordImage(drawing: XmlEl, ctx: WalkCtx, h: ParaHandlers): void {
  const idx = h.images.length;
  const inline = child(drawing, "inline") ?? child(drawing, "anchor");
  const extent = child(drawing, "extent") ?? (inline ? child(inline, "extent") : undefined);
  const blip = findBlip(drawing);
  const embed = attr(blip, "embed");
  const rel = embed ? ctx.rels.get(embed) : undefined;
  h.images.push({
    index: idx,
    relId: embed || undefined,
    part: rel?.target ? `word/${rel.target}` : undefined,
    widthEmu: numAttr(extent, "cx"),
    heightEmu: numAttr(extent, "cy"),
  });
}

function findBlip(el: XmlEl | undefined): XmlEl | undefined {
  if (!el) return undefined;
  if (el.tag === "blip") return el;
  for (const c of el.children) {
    const found = findBlip(c);
    if (found) return found;
  }
  return undefined;
}

function classifyField(instruction: string): string {
  const head = instruction.trim().split(/\s+/)[0]?.toUpperCase() ?? "";
  switch (head) {
    case "HYPERLINK":
      return "hyperlink";
    case "TOC":
      return "toc";
    case "PAGEREF":
      return "pageref";
    case "REF":
      return "ref";
    case "NOTEREF":
      return "noteref";
    case "MERGEFIELD":
      return "mergefield";
    case "FORMTEXT":
    case "FORMCHECKBOX":
    case "FORMDROPDOWN":
      return "formfield";
    default:
      return "other";
  }
}

// ---------------------------------------------------------------------------
// Headers/footers/notes/comments
// ---------------------------------------------------------------------------

function extractHeaderFooterRefs(
  doc: ParsedDocx,
  body: XmlEl,
  kind: "header" | "footer",
): Manifest["document"]["headers"] {
  const refs: XmlEl[] = [];
  const tag = kind === "header" ? "headerReference" : "footerReference";
  for (const p of children(body, "p")) {
    for (const r of children(child(p, "pPr"), tag)) refs.push(r);
  }
  for (const r of children(child(body, "sectPr"), tag)) refs.push(r);
  const rels = parseRels(doc.parts.get("word/_rels/document.xml.rels"));
  return refs
    .map((r) => {
      const target = resolveRel(rels, attr(r, "id"));
      const type = (attr(r, "type") ?? "default") as "default" | "first" | "even";
      const part = target ? `word/${target}` : "";
      const bytes = part ? doc.parts.get(part) : undefined;
      return { part, type, sha256: bytes ? hashText(bytes) : "" };
    })
    .filter((r) => r.part)
    .sort((a, b) => a.part < b.part ? -1 : a.part > b.part ? 1 : a.type < b.type ? -1 : a.type > b.type ? 1 : 0);
}

function countNotes(xml: string | undefined, noteTag: "footnote" | "endnote"): { count: number } | undefined {
  if (!xml) return undefined;
  const root = parseXmlDocument(xml);
  const container = child(root, "footnotes") ?? child(root, "endnotes");
  if (!container) return undefined;
  return { count: children(container, noteTag).length };
}

function countComments(xml: string | undefined): { count: number } | undefined {
  if (!xml) return undefined;
  const root = parseXmlDocument(xml);
  return { count: children(child(root, "comments"), "comment").length };
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

function extractMetadata(doc: ParsedDocx): Manifest["metadata"] {
  const out: Writable<Manifest["metadata"]> = {};
  const coreXml = doc.parts.get("docProps/core.xml");
  if (coreXml) {
    const root = parseXmlDocument(coreXml);
    const props = child(root, "coreProperties") ?? root;
    const get = (n: string) => {
      const t = textOf(child(props, n));
      return t || undefined;
    };
    out.title = get("title");
    out.creator = get("creator");
    out.subject = get("subject");
    out.description = get("description");
    out.keywords = get("keywords");
    out.language = get("language");
    out.created = get("created");
    out.modified = get("modified");
    out.revision = get("revision");
  }
  const appXml = doc.parts.get("docProps/app.xml");
  if (appXml) {
    const root = parseXmlDocument(appXml);
    const props = child(root, "Properties") ?? root;
    const get = (n: string) => textOf(child(props, n)) || undefined;
    out.application = get("Application");
    out.appVersion = get("AppVersion");
    out.templateName = get("Template");
    out.company = get("Company");
    const pages = get("Pages");
    const words = get("Words");
    const chars = get("Characters");
    out.pages = pages ? Number(pages) : undefined;
    out.words = words ? Number(words) : undefined;
    out.characters = chars ? Number(chars) : undefined;
  }
  return out;
}

function extractNamespace(documentXml: string): string | undefined {
  const m = documentXml.match(/xmlns:w="([^"]+)"/);
  return m?.[1];
}
