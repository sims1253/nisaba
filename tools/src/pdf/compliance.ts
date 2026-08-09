/**
 * PDF compatibility checks.
 *
 * Implements navigability and portable-document checks, including
 * Checks encryption (must be absent), text extractability (must be present), link
 * presence (cross-refs + indexes must be clickable), index labels
 * (Inhalts-/Tabellen-/Abbildungsverzeichnis must be rendered), and a *heuristic*
 * watermark scan.
 *
 * Each check is self-contained and reports a status + severity, so the operator
 * can tell a hard failure from a heuristic warning. Required external tools are
 * gated by capability detection: a missing tool downgrades its checks to
 * `skipped` with an install hint rather than crashing.
 */
import { Effect } from "effect";
import { Shell } from "../externals/shell.js";
import { FileSystem } from "../externals/fs.js";
import { detectCapabilities, type CapabilitiesReport, type ToolId } from "../externals/capabilities.js";
import { pdfInfo, pdfToText, readQdfText } from "./inspect.js";
import { TOOL_NAME, VERSION } from "../version.js";
import { MissingToolError, FsError, InvalidInputError } from "../errors.js";

export type CheckStatus = "pass" | "fail" | "warn" | "skipped";
export type Severity = "hard" | "soft" | "heuristic";

export interface ComplianceCheck {
  readonly id: string;
  readonly name: string;
  readonly status: CheckStatus;
  readonly severity: Severity;
  readonly evidence: string;
  readonly detail?: Readonly<Record<string, unknown>>;
  /** Tools this check needs; populated when status is `skipped`. */
  readonly requires?: readonly ToolId[];
}

export interface PdfComplianceReport {
  readonly schemaVersion: "1";
  readonly generatedBy: string;
  readonly input: { readonly path: string };
  readonly capabilities: CapabilitiesReport;
  readonly checks: readonly ComplianceCheck[];
  readonly passed: boolean;
  /** Human-readable summary of the hard failures, for CI logs. */
  readonly failures: readonly string[];
}

const WATERMARK_TOKENS = [
  "entwurf",
  "draft",
  "wasserzeichen",
  "confidential",
  "vertraulich",
  "kopie",
  "copy",
  "nicht freigegeben",
  "not approved",
  "bearbeitungsversion",
  "sample",
  "muster",
];

const INDEX_LABELS = [
  { id: "inhaltsverzeichnis", patterns: ["inhaltsverzeichnis", "table of contents", "inhalt"] },
  { id: "tabellenverzeichnis", patterns: ["tabellenverzeichnis", "list of tables"] },
  { id: "abbildungsverzeichnis", patterns: ["abbildungsverzeichnis", "list of figures", "bilderverzeichnis"] },
];

function norm(s: string): string {
  return s.toLowerCase().replace(/\s+/g, " ").trim();
}

/** Run the full compliance battery against `pdfPath`. */
export function checkPdfCompliance(
  pdfPath: string,
  workDir: string,
): Effect.Effect<PdfComplianceReport, MissingToolError | InvalidInputError | FsError, Shell | FileSystem> {
  return Effect.gen(function* () {
    const caps = yield* detectCapabilities();
    const checks: ComplianceCheck[] = [];

    // ---- Encryption (hard) ---------------------------------------------------
    const avail = (id: ToolId) => caps.tools[id].available;
    const hasQpdf = avail("qpdf");

    if (avail("pdfinfo")) {
      const info = yield* pdfInfo(pdfPath);
      const enc = info.get("Encrypted") ?? "";
      const encrypted = /yes/i.test(enc) && !/no\b/i.test(enc);
      checks.push({
        id: "encryption",
        name: "Kein Passwortschutz / keine Verschlüsselung",
        status: encrypted ? "fail" : "pass",
        severity: "hard",
        evidence: `pdfinfo: Encrypted=${enc || "(n/a)"}`,
      });
    } else {
      checks.push(skipCheck("encryption", "Kein Passwortschutz", ["pdfinfo"]));
    }

    // ---- Text extractability (hard) -----------------------------------------
    let extractedText: string | null = null;
    if (avail("pdftotext")) {
      const text = yield* pdfToText(pdfPath);
      extractedText = text;
      const chars = text.replace(/\s+/g, "").length;
      checks.push({
        id: "text-extractable",
        name: "Text ist extrahierbar",
        status: chars > 0 ? "pass" : "fail",
        severity: "hard",
        evidence: `extrahiert: ${chars} nicht-Leerzeichen`,
        detail: { characters: chars, sample: text.slice(0, 120).replace(/\s+/g, " ").trim() },
      });
    } else {
      checks.push(skipCheck("text-extractable", "Text ist extrahierbar", ["pdftotext"]));
    }

    // ---- Watermark heuristic (heuristic) ------------------------------------
    if (extractedText !== null) {
      const n = norm(extractedText);
      const matched = WATERMARK_TOKENS.filter((t) => n.includes(t));
      const visualPageCheck = caps.derived.pdfToImage && caps.derived.imageCompare;
      checks.push({
        id: "watermark-heuristic",
        name: "Wasserzeichen-Heuristik (Text)",
        status: matched.length > 0 ? "warn" : "pass",
        severity: "heuristic",
        evidence:
          matched.length > 0
            ? `verdächtige Token im Text: ${matched.join(", ")}`
            : "keine Wasserzeichen-Tokens im extrahierten Text gefunden",
        detail: {
          matched,
          ...(visualPageCheck
            ? {}
            : { advisory: "Wasserzeichen-Erkennung ist rein textbasiert; kein visueller Seitenvergleich (pdftoppm/compare) verfügbar." }),
        },
      });
    } else {
      checks.push(skipCheck("watermark-heuristic", "Wasserzeichen-Heuristik (Text)", ["pdftotext"]));
    }

    // ---- Links + outlines (hard) --------------------------------------------
    if (hasQpdf) {
      const qdf = yield* readQdfText(pdfPath, workDir);
      if (qdf.text === null) {
        checks.push({
          id: "links",
          name: "Querverweise/Links anklickbar",
          status: "fail",
          severity: "hard",
          evidence: `qpdf-Dekodierung fehlgeschlagen: ${qdf.note}`,
        });
      } else {
        const t = qdf.text;
        const uriMatches = [...t.matchAll(/\/URI\s*\(([^)]*)\)/g)].map((m) => m[1] ?? "");
        const linkAnnots = (t.match(/\/Subtype\s*\/Link/g) ?? []).length;
        const annots = (t.match(/\/Type\s*\/Annot/g) ?? []).length;
        const hasOutlines = /\/Outlines\b/.test(t) && /\/Count\s+([1-9]\d*)/.test(t);
        const external = uriMatches.filter((u) => /^https?:/i.test(u));
        const status: CheckStatus = linkAnnots > 0 ? "pass" : "fail";
        checks.push({
          id: "links",
          name: "Querverweise/Links anklickbar",
          status,
          severity: "hard",
          evidence: `${linkAnnots} Link-Annotationen, ${external.length} externe URIs, Outlines=${hasOutlines ? "ja" : "nein"}`,
          detail: {
            linkAnnotations: linkAnnots,
            totalAnnotations: annots,
            externalUris: external.slice(0, 10),
            outlinesPresent: hasOutlines,
          },
        });
      }
    } else {
      checks.push(skipCheck("links", "Querverweise/Links anklickbar", ["qpdf"]));
    }

    // ---- Index labels (hard) -------------------------------------------------
    if (extractedText !== null) {
      const n = norm(extractedText);
      const perLabel = INDEX_LABELS.map((label) => {
        const found = label.patterns.some((p) => n.includes(norm(p)));
        return { id: label.id, found };
      });
      const missing = perLabel.filter((l) => !l.found).map((l) => l.id);
      checks.push({
        id: "index-labels",
        name: "Verzeichnisse vorhanden (Inhalts-/Tabellen-/Abbildungsverzeichnis)",
        status: missing.length === 0 ? "pass" : "fail",
        severity: "hard",
        evidence:
          missing.length === 0
            ? "alle Verzeichnis-Überschriften im Text gefunden"
            : `fehlend: ${missing.join(", ")}`,
        detail: { labels: perLabel },
      });
    } else {
      checks.push(skipCheck("index-labels", "Verzeichnisse vorhanden", ["pdftotext"]));
    }

    const failures = checks
      .filter((c) => c.severity === "hard" && c.status === "fail")
      .map((c) => `${c.id}: ${c.evidence}`);
    // A missing tool downgrades a hard check to `skipped`; that must not count
    // as a pass — the report stays honest until every hard check actually ran.
    // Skipped checks are listed in `failures` so CI logs explain the non-pass.
    const skipped = checks
      .filter((c) => c.severity === "hard" && c.status === "skipped")
      .map((c) => `${c.id}: übersprungen (fehlendes Werkzeug: ${c.requires?.join(", ") ?? "unbekannt"})`);
    failures.push(...skipped);
    const passed = failures.length === 0;

    return {
      schemaVersion: "1",
      generatedBy: `${TOOL_NAME}@${VERSION}`,
      input: { path: pdfPath },
      capabilities: caps,
      checks,
      passed,
      failures,
    };
  });
}

function skipCheck(id: string, name: string, requires: readonly ToolId[]): ComplianceCheck {
  return {
    id,
    name,
    status: "skipped",
    severity: "hard",
    evidence: `übersprungen — benötigtes Werkzeug fehlt (siehe capabilities)`,
    requires,
  };
}
