/**
 * DOCX (OOXML) low-level parsing.
 *
 * A `.docx` is a ZIP of XML parts. We unzip it with `fflate` (pure JS, no native
 * deps) and parse the parts with the order-preserving parser in
 * {@link ./xmlparser.ts}. Order preservation is essential: a Word body
 * interleaves paragraphs and tables, and that order is semantically meaningful.
 *
 * Namespace prefixes are stripped at parse time (`w:val` → `val`), so callers
 * use bare tag names. Accessors are defensive: a missing part or unexpected
 * shape yields an empty/neutral value, never a throw — introspection must be
 * able to run on a real, slightly-non-conformant DOCX template without
 * abandoning the whole manifest.
 */
import { unzipSync, strFromU8 } from "fflate";
import { MalformedDocxError } from "../errors.js";
import { parseXml, type XmlElement } from "./xmlparser.js";

export type XmlEl = XmlElement;

export interface ParsedDocx {
  /** ZIP entry name → decoded UTF-8 text (for XML parts). */
  readonly parts: ReadonlyMap<string, string>;
  /** Original ZIP entry names, sorted. */
  readonly names: readonly string[];
  /** Raw bytes for integrity hashing. */
  readonly bytes: Uint8Array;
}

/** Unzip a DOCX and decode the XML parts as UTF-8. Binary parts are skipped. */
export function parseDocxBytes(bytes: Uint8Array, sourcePath: string): ParsedDocx {
  let unzipped: Record<string, Uint8Array>;
  try {
    unzipped = unzipSync(bytes, {});
  } catch (e) {
    throw new MalformedDocxError({
      path: sourcePath,
      reason: `not a readable ZIP/OOXML package: ${(e as Error).message}`,
    });
  }

  const xmlParts = new Map<string, string>();
  const names = Object.keys(unzipped).sort();
  const xmlish = (n: string) =>
    n.endsWith(".xml") || n.endsWith(".rels") || n === "[Content_Types].xml";
  for (const name of names) {
    if (xmlish(name)) {
      try {
        xmlParts.set(name, strFromU8(unzipped[name]!));
      } catch {
        // skip undecodable
      }
    }
  }
  if (!xmlParts.has("word/document.xml")) {
    throw new MalformedDocxError({
      path: sourcePath,
      missingPart: "word/document.xml",
      reason: "package is missing word/document.xml — not a valid DOCX",
    });
  }
  return { parts: xmlParts, names, bytes };
}

/** Parse an XML string into an ordered element tree. */
export function parseXmlDocument(text: string): XmlElement {
  return parseXml(text);
}

/** Read an attribute value (namespace already stripped). */
export function attr(el: XmlEl | undefined, name: string): string | undefined {
  if (!el) return undefined;
  const v = el.attribs[name];
  return v !== undefined ? v : undefined;
}

/** Read the direct text content of an element. */
export function textOf(el: XmlEl | undefined): string {
  return el ? el.text : "";
}

/** Get the first direct child element named `name`. */
export function child(el: XmlEl | undefined, name: string): XmlEl | undefined {
  if (!el) return undefined;
  return el.children.find((c) => c.tag === name);
}

/** Get all direct child elements named `name`, in document order. */
export function children(el: XmlEl | undefined, name: string): XmlEl[] {
  if (!el) return [];
  return el.children.filter((c) => c.tag === name);
}

/** Numeric attribute, or undefined if absent/non-numeric. */
export function numAttr(el: XmlEl | undefined, name: string): number | undefined {
  const v = attr(el, name);
  if (v === undefined) return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

export interface Rel {
  readonly id: string;
  readonly type: string; // last path segment of the relationship type
  readonly target: string;
  readonly targetMode?: string;
}

/** Parse a `.rels` part into a Map of Id → {@link Rel}. */
export function parseRels(text: string | undefined): Map<string, Rel> {
  const out = new Map<string, Rel>();
  if (!text) return out;
  const root = parseXml(text);
  for (const r of children(child(root, "Relationships"), "Relationship")) {
    const id = attr(r, "Id") ?? "";
    const type = (attr(r, "Type") ?? "").split("/").pop() ?? "";
    const target = attr(r, "Target") ?? "";
    const targetMode = attr(r, "TargetMode");
    if (id) out.set(id, { id, type, target, targetMode });
  }
  return out;
}
