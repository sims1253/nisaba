/**
 * External-tool capability detection.
 *
 * Every command that shells out to LibreOffice / Poppler / ImageMagick / qpdf /
 * typst first checks here. If a required capability is missing we fail with a
 * {@link MissingToolError} that names the tool and gives an install hint —
 * never a silent fallback, never a confusing downstream stack trace.
 *
 * The report is intentionally timestamp-free so CI diffs stay clean.
 */
import { Effect } from "effect";
import { Shell, type ShellShape } from "./shell.js";
import { toPosix } from "../paths.js";
import { TOOL_NAME, VERSION } from "../version.js";

export const TOOL_IDS = [
  "typst",
  "libreoffice",
  "pdftoppm",
  "pdfinfo",
  "pdftotext",
  "qpdf",
  "magick",
  "convert",
  "compare",
  "gs",
] as const;

export type ToolId = (typeof TOOL_IDS)[number];

export interface ToolCapability {
  readonly id: ToolId;
  readonly available: boolean;
  readonly path: string | null;
  readonly version: string | null;
  readonly error: string | null;
}

/** Hints shown to the operator when a tool is missing. */
export const INSTALL_HINTS: Readonly<Record<ToolId, string>> = {
  typst: "Install from https://typst.app (snap install typst; brew install typst). Required for compiling Typst skeletons.",
  libreoffice:
    "Install LibreOffice (headless). Debian/Ubuntu: apt-get install libreoffice. Required to render DOCX→PDF for visual diff.",
  pdftoppm:
    "Install Poppler. Debian/Ubuntu: apt-get install poppler-utils. macOS: brew install poppler. Required to rasterise PDF pages.",
  pdfinfo: "Install Poppler (poppler-utils). Used for PDF metadata in compliance checks.",
  pdftotext: "Install Poppler (poppler-utils). Required for text-extractability and index-label checks.",
  qpdf: "Install qpdf. Debian/Ubuntu: apt-get install qpdf. Used to inspect PDF encryption/annotations/links.",
  magick: "Install ImageMagick 7 (provides `magick`). Used for page comparison when `compare` is absent.",
  convert: "Install ImageMagick 6 (provides `convert`). Used for raster fallback.",
  compare:
    "Install ImageMagick (provides `compare`). Debian/Ubuntu: apt-get install imagemagick. Required for per-pixel page diff.",
  gs: "Install Ghostscript (optional). Debian/Ubuntu: apt-get install ghostscript.",
};

interface DetectSpec {
  readonly id: ToolId;
  /** Commands to try, in order (e.g. soffice then libreoffice). */
  readonly commands: readonly string[];
  readonly args: readonly string[];
  readonly versionFrom: "stdout" | "stderr";
  readonly versionRegex: RegExp;
}

const SPECS: readonly DetectSpec[] = [
  { id: "typst", commands: ["typst"], args: ["--version"], versionFrom: "stdout", versionRegex: /typst\s+(\S+)/ },
  {
    id: "libreoffice",
    commands: ["soffice", "libreoffice"],
    args: ["--version"],
    versionFrom: "stdout",
    versionRegex: /LibreOffice\s+(\S+)/,
  },
  { id: "pdftoppm", commands: ["pdftoppm"], args: ["-v"], versionFrom: "stderr", versionRegex: /pdftoppm version\s+(\S+)/ },
  { id: "pdfinfo", commands: ["pdfinfo"], args: ["-v"], versionFrom: "stderr", versionRegex: /pdfinfo version\s+(\S+)/ },
  { id: "pdftotext", commands: ["pdftotext"], args: ["-v"], versionFrom: "stderr", versionRegex: /pdftotext version\s+(\S+)/ },
  { id: "qpdf", commands: ["qpdf"], args: ["--version"], versionFrom: "stdout", versionRegex: /qpdf version\s+(\S+)/ },
  { id: "magick", commands: ["magick"], args: ["--version"], versionFrom: "stdout", versionRegex: /ImageMagick\s+(\S+)/ },
  { id: "convert", commands: ["convert"], args: ["--version"], versionFrom: "stdout", versionRegex: /ImageMagick\s+(\S+)/ },
  { id: "compare", commands: ["compare"], args: ["-version"], versionFrom: "stdout", versionRegex: /ImageMagick\s+(\S+)/ },
  { id: "gs", commands: ["gs"], args: ["--version"], versionFrom: "stdout", versionRegex: /^(\S+)$/ },
];

export interface DerivedCapabilities {
  /** DOCX → PDF rendering (LibreOffice headless). */
  readonly docxToPdf: boolean;
  /** PDF → page images (pdftoppm, or ImageMagick as fallback). */
  readonly pdfToImage: boolean;
  /** Per-pixel image comparison (compare, or `magick compare`). */
  readonly imageCompare: boolean;
  /** PDF text extraction (pdftotext). */
  readonly textExtract: boolean;
  /** PDF structural inspection (pdfinfo + qpdf). */
  readonly pdfInspect: boolean;
  /** Index/heading-label presence checks (pdftotext). */
  readonly indexCheck: boolean;
}

export interface CapabilitiesReport {
  readonly schemaVersion: "1";
  readonly generatedBy: string;
  readonly tools: Readonly<Record<ToolId, ToolCapability>>;
  readonly derived: DerivedCapabilities;
}

function detectOne(shell: ShellShape, spec: DetectSpec): Effect.Effect<ToolCapability> {
  return Effect.gen(function* () {
    for (const cmd of spec.commands) {
      const resolved = yield* shell.which(cmd);
      if (!resolved) continue;
      // `-v` on Poppler tools exits non-zero but still prints the version to
      // stderr, so we probe with `run` (any exit code) rather than `runSuccess`.
      // The only failure we tolerate here is a spawn error (MissingToolError).
      const probe = yield* shell.run(cmd, spec.args, { timeoutMs: 10_000 }).pipe(
        Effect.catchTag("MissingToolError", (e) =>
          Effect.succeed({ __missing: true as const, error: e.hint ?? `spawn failed for ${e.tool}` })),
      );
      if ("__missing" in probe) {
        return {
          id: spec.id,
          available: true,
          path: toPosix(resolved),
          version: null,
          error: `version probe failed: ${probe.error}`,
        };
      }
      const stream = spec.versionFrom === "stdout" ? probe.stdout : probe.stderr;
      const m = stream.match(spec.versionRegex);
      return {
        id: spec.id,
        available: true,
        path: toPosix(resolved),
        version: m?.[1] ?? null,
        error: null,
      };
    }
    return {
      id: spec.id,
      available: false,
      path: null,
      version: null,
      error: `not found on PATH. ${INSTALL_HINTS[spec.id]}`,
    };
  });
}

/**
 * Probe the environment for all supported external tools and derive the
 * composite capabilities the pipeline cares about.
 */
export function detectCapabilities(): Effect.Effect<CapabilitiesReport, never, Shell> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    const tools = {} as Record<ToolId, ToolCapability>;
    for (const spec of SPECS) {
      tools[spec.id] = yield* detectOne(shell, spec);
    }
    const avail = (id: ToolId) => tools[id].available;
    const derived: DerivedCapabilities = {
      docxToPdf: avail("libreoffice"),
      pdfToImage: avail("pdftoppm") || avail("magick") || avail("convert"),
      imageCompare: avail("compare") || avail("magick"),
      textExtract: avail("pdftotext"),
      pdfInspect: avail("pdfinfo") && avail("qpdf"),
      indexCheck: avail("pdftotext"),
    };
    return {
      schemaVersion: "1",
      generatedBy: `${TOOL_NAME}@${VERSION}`,
      tools,
      derived,
    };
  });
}
