/**
 * Manifest → Typst template skeleton.
 *
 * Emits a *deterministic* Typst file: the same manifest always produces a
 * byte-identical skeleton. It is a skeleton, not a fidelity claim —
 * the header comment says so explicitly, and visual fidelity is only ever
 * asserted by the page-image diff against a real DOCX render.
 *
 * What the skeleton captures from the manifest:
 *   - page geometry (twips → pt) and margins from the first section;
 *   - the heading outline (so the navigator + TOC have something to show);
 *   - the required placeholders, both as a `#let` field record and as literal
 *     `<<…>>` markers in the body, so {@link ./schema.ts} can verify they survive;
 *   - table/image/page-break positions as stubs;
 *   - the function allowlist generated templates declare.
 */
import type { Block, Manifest } from "../docx/manifest.js";
import { hashValue } from "../json.js";

const TWIPS_PER_PT = 20;

/** Sanitise an arbitrary placeholder token into a valid Typst identifier. */
export function toTypstIdent(token: string): string {
  const id = token
    .toLowerCase()
    .replace(/ä/g, "ae").replace(/ö/g, "oe").replace(/ü/g, "ue").replace(/ß/g, "ss")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  if (!id) return "feld";
  if (/^[0-9]/.test(id)) return `_${id}`;
  return id;
}

function pt(twips: number): string {
  // Typst accepts `pt`; emit a clean decimal.
  const v = twips / TWIPS_PER_PT;
  return Number.isInteger(v) ? `${v}pt` : `${v.toFixed(3).replace(/0+$/, "").replace(/\.$/, "")}pt`;
}

function escapeTypstText(s: string): string {
  // Escape characters that would be interpreted as Typst markup in body text.
  return s
    .replace(/\\/g, "\\\\")
    .replace(/#/g, "\\#")
    .replace(/=/g, "\\=")
    .replace(/\*/g, "\\*")
    .replace(/_/g, "\\_")
    .replace(/`/g, "\\`")
    .replace(/\[/g, "\\[")
    .replace(/\]/g, "\\]");
}

function headingPrefix(level: number): string {
  return "=".repeat(Math.min(Math.max(level, 1), 6));
}

function renderBlock(block: Block, _ctx: { tableCount: number }): string {
  switch (block.kind) {
    case "heading":
      return `${headingPrefix(block.level)} ${escapeTypstText(block.text) || "(ohne Überschrift)"}`;
    case "paragraph":
      return escapeTypstText(block.text) || "";
    case "table":
      return `#figure(table(columns: 2)[\n  // Tabelle ${block.ref + 1} aus dem DOCX\n][\n  <<Tabelle_Inhalt>>\n], caption: [Tabelle ${block.ref + 1}])`;
    case "image":
      return `#figure(image("media/bild-${block.ref + 1}.png"), caption: [Abbildung ${block.ref + 1}])`;
    case "placeholder":
      return `<<${block.token}>>`;
    case "field":
      if (block.fieldType === "toc") return "#outline(title: [Inhaltsverzeichnis], indent: auto)";
      return `// Feld (${block.fieldType}): ${escapeTypstText(block.instruction)}`;
    case "pageBreak":
      return "#pagebreak()";
    case "sectionBreak":
      return `// Abschnittswechsel (Sektion ${block.sectionIndex})`;
  }
}

/** Generate a deterministic Typst template skeleton from a manifest. */
export function generateTypstSkeleton(manifest: Manifest): string {
  const lines: string[] = [];
  const section = manifest.document.sections[0];
  const manifestHash = hashValue(manifest);

  lines.push(`// Auto-generiertes Typst-Template-Skeleton.`);
  lines.push(`// Quelle: ${manifest.source.fileName}`);
  lines.push(`// Manifest-Hash (sha256, kanonisch): ${manifestHash}`);
  lines.push(`// Generator: ${manifest.generatedBy}`);
  lines.push(`// HINWEIS: Dies ist ein Skeleton, keine Fidelity-Garantie. Visuelle`);
  lines.push(`// Übereinstimmung wird NUR durch den page-image-diff gegen eine echte`);
  lines.push(`// DOCX-Renderung behauptet (siehe docs/template-pipeline.md).`);
  lines.push("");

  // Required placeholder fields, as a data record.
  if (manifest.requiredPlaceholders.length > 0) {
    lines.push("#let felder = (");
    for (const token of manifest.requiredPlaceholders) {
      lines.push(`  ${toTypstIdent(token)}: "<<${token}>>",`);
    }
    lines.push(")");
    lines.push("");
  }

  // Page setup from first section.
  if (section) {
    const orient = section.pageSize.orientation === "landscape" ? " (Querformat)" : "";
    lines.push(`#set page(`);
    lines.push(`  width: ${pt(section.pageSize.widthTwips)},`);
    lines.push(`  height: ${pt(section.pageSize.heightTwips)},${orient}`);
    lines.push(`  margin: (top: ${pt(section.margins.topTwips)}, bottom: ${pt(section.margins.bottomTwips)}, left: ${pt(section.margins.leftTwips)}, right: ${pt(section.margins.rightTwips)}),`);
    lines.push(`  header: align(right)[Nisaba Skeleton],`);
    lines.push(`  numbering: \"1\",`);
    lines.push(`)`);
  }
  lines.push(`#set text(lang: \"de\", font: \"Libertinus Serif\")`);
  lines.push(`#set par(justify: true)`);
  lines.push("");

  // Heading styling.
  lines.push(`#set heading(numbering: \"1.1.1\")`);
  lines.push("#show heading.where(level: 1): it => pagebreak(weak: true) + it");
  lines.push("");

  // Function allowlist: template declares which function names are
  // recognised constructs. Declared here as no-op aliases for visibility.
  lines.push("// Erlaubte Funktionsnamen: figure, table, cite.");
  lines.push(`#let _allowlist = ("figure", "table", "cite")`);
  lines.push("");

  // Body, block by block.
  lines.push("=== Dokument ===");
  const ctx = { tableCount: manifest.document.tables.length };
  for (const block of manifest.document.blocks) {
    const rendered = renderBlock(block, ctx);
    if (rendered === "") continue;
    lines.push("");
    lines.push(rendered);
  }
  lines.push("");

  return lines.join("\n") + "\n";
}
