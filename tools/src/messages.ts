/**
 * Centralised, English-language report messages.
 *
 * Every user-facing prose string the JSON reports emit (check names, evidence
 * strings, notes) lives here, so the tools' default language is defined in
 * exactly one place and a future localisation pass only has to swap this
 * module. Report *data* — ids, statuses, matched watermark tokens, index-label
 * patterns, style-name regexes — stays with its owning module: those are
 * machine values, not prose.
 *
 * Static messages are plain string constants; interpolated ones are functions
 * so callers cannot mix display wording into their logic.
 */
import type { Provenance } from "./visualdiff/harness.js";

// ---------------------------------------------------------------------------
// pdf-compliance
// ---------------------------------------------------------------------------

export const PDF_COMPLIANCE = {
  /** Check: encryption must be absent. */
  checkEncryptionName: "No password protection / no encryption",
  checkEncryptionShortName: "No password protection",
  /** Check: text layer must be extractable. */
  checkTextExtractableName: "Text is extractable",
  evidenceExtractedChars: (chars: number) => `extracted: ${chars} non-whitespace characters`,
  /** Check: text-based watermark scan (heuristic). */
  checkWatermarkName: "Watermark heuristic (text)",
  evidenceSuspiciousTokens: (matched: readonly string[]) => `suspicious tokens in text: ${matched.join(", ")}`,
  evidenceNoWatermarkTokens: "no watermark tokens found in the extracted text",
  advisoryWatermarkTextOnly:
    "Watermark detection is purely text-based; no visual page comparison (pdftoppm/compare) available.",
  /** Check: cross-references and links must be clickable. */
  checkLinksName: "Cross-references/links clickable",
  evidenceQpdfDecodeFailed: (note: string) => `qpdf decoding failed: ${note}`,
  evidenceLinksSummary: (linkAnnotations: number, externalUris: number, outlinesPresent: boolean) =>
    `${linkAnnotations} link annotations, ${externalUris} external URIs, Outlines=${outlinesPresent ? "yes" : "no"}`,
  /** Check: index headings must be present in the text. */
  checkIndexLabelsName: "Indexes present (table of contents / list of tables / list of figures)",
  checkIndexLabelsShortName: "Indexes present",
  evidenceAllIndexHeadingsFound: "all index headings found in the text",
  evidenceMissingIndexLabels: (missing: readonly string[]) => `missing: ${missing.join(", ")}`,
  /** Hard check downgraded because a required tool is unavailable. */
  failureSkippedMissingTool: (requires: readonly string[] | undefined) =>
    `skipped (missing tool: ${requires?.join(", ") ?? "unknown"})`,
  evidenceSkippedToolMissing: "skipped — required tool missing (see capabilities)",
} as const;

// ---------------------------------------------------------------------------
// visual-diff
// ---------------------------------------------------------------------------

export const VISUAL_DIFF = {
  noteNonDocxRenderReference: (provenance: Provenance) =>
    `Reference provenance is "${provenance}", not "docx-render". Metrics are computed, but NO visual fidelity is claimed.`,
  notePageCountDiffers: (referencePages: number, candidatePages: number) =>
    `Page counts differ (${referencePages} vs ${candidatePages}); only common pages compared.`,
  notePagesNotComparable: (errorPages: number) => `${errorPages} page(s) could not be compared (compare error).`,
} as const;

// ---------------------------------------------------------------------------
// ris-roundtrip
// ---------------------------------------------------------------------------

export const RIS_ROUNDTRIP = {
  noteNotLossless: "Round-trip is NOT lossless — canonicalisation changes the data.",
  noteMissingRequiredFields: "At least one record is missing required fields.",
} as const;
