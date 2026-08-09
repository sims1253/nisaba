/**
 * Page-image visual-diff harness.
 *
 * Rasterises two PDFs page-by-page with Poppler's `pdftoppm` at a fixed DPI,
 * then compares each page pair with ImageMagick's `compare` to get a normalised
 * RMSE metric. Output is a deterministic report.
 *
 * Fidelity honesty: the report records each input's *provenance* and only allows
 * `passed` when the reference is tagged as a real DOCX render. A skeleton-only
 * comparison can still be *run* and produce metrics, but it is reported as
 * non-authoritative.
 *
 * Required external tools are gated: a missing `pdftoppm` or `compare`/`magick`
 * fails fast with a {@link MissingToolError} that names the tool and gives an
 * install hint — never a silent zero-diff.
 */
import { Effect } from "effect";
import { Shell } from "../externals/shell.js";
import { FileSystem } from "../externals/fs.js";
import { detectCapabilities, type CapabilitiesReport, type ToolId } from "../externals/capabilities.js";
import { hashBytes } from "../json.js";
import { toPosix } from "../paths.js";
import { TOOL_NAME, VERSION } from "../version.js";
import { MissingToolError, ToolFailedError, FsError, InvalidInputError } from "../errors.js";

export type Provenance = "docx-render" | "typst-compile" | "pdf" | "unknown";

export interface VisualDiffOptions {
  readonly dpi: number;
  /** Normalised RMSE above which a page is considered different. */
  readonly maxNormalizedRmse: number;
  /** Maximum fraction of pages allowed to differ before the run fails. */
  readonly maxDiffPageFraction: number;
  readonly referenceProvenance: Provenance;
  readonly candidateProvenance: Provenance;
  readonly fuzzPercent: number;
}

export const DEFAULT_VISUAL_DIFF_OPTIONS: VisualDiffOptions = {
  dpi: 150,
  maxNormalizedRmse: 0.01,
  maxDiffPageFraction: 0.0,
  referenceProvenance: "unknown",
  candidateProvenance: "unknown",
  fuzzPercent: 5,
};

export interface PageComparison {
  readonly page: number;
  readonly normalizedRmse: number | null;
  readonly rawMetric: string;
  readonly status: "match" | "diff" | "error";
}

export interface VisualDiffReport {
  readonly schemaVersion: "1";
  readonly generatedBy: string;
  readonly reference: { readonly path: string; readonly provenance: Provenance; readonly sha256: string };
  readonly candidate: { readonly path: string; readonly provenance: Provenance; readonly sha256: string };
  readonly rendering: { readonly tool: string; readonly dpi: number; readonly referencePages: number; readonly candidatePages: number };
  readonly comparisons: readonly PageComparison[];
  readonly samePageCount: boolean;
  readonly thresholds: { readonly maxNormalizedRmse: number; readonly maxDiffPageFraction: number };
  /** True only when the reference is a real DOCX render. */
  readonly visualFidelityAssertable: boolean;
  readonly meanNormalizedRmse: number | null;
  readonly passed: boolean;
  readonly notes: readonly string[];
  readonly capabilities: CapabilitiesReport;
}

function requireCapability(caps: CapabilitiesReport, id: ToolId): Effect.Effect<void, MissingToolError> {
  if (caps.tools[id].available) return Effect.void;
  return Effect.fail(
    new MissingToolError({
      tool: id,
      hint: caps.tools[id].error ?? `install ${id}`,
    }),
  );
}

function renderPdf(
  pdfPath: string,
  outDir: string,
  prefix: string,
  dpi: number,
): Effect.Effect<string[], MissingToolError | ToolFailedError | InvalidInputError | FsError, Shell | FileSystem> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    const fs = yield* FileSystem;
    yield* fs.ensureDir(outDir);
    const base = `${outDir}/${prefix}`;
    yield* shell.runSuccess("pdftoppm", ["-r", String(dpi), "-png", pdfPath, base], { timeoutMs: 180_000 });
    const entries = yield* fs.listDir(outDir);
    return entries
      .filter((n) => n.startsWith(`${prefix}-`) && n.endsWith(".png"))
      .sort()
      .map((n) => toPosix(`${outDir}/${n}`));
  });
}

function comparePages(
  refPng: string,
  candPng: string,
  diffPng: string,
  fuzzPercent: number,
): Effect.Effect<{ normalized: number | null; raw: string; status: "match" | "diff" | "error" }, MissingToolError, Shell> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    const useMagick = (yield* shell.which("compare")) === null && (yield* shell.which("magick")) !== null;
    const cmd = useMagick ? "magick" : "compare";
    const args = useMagick
      ? ["compare", "-metric", "RMSE", "-fuzz", `${fuzzPercent}%`, refPng, candPng, diffPng]
      : ["-metric", "RMSE", "-fuzz", `${fuzzPercent}%`, refPng, candPng, diffPng];
    // `compare` exits 1 when images differ (which is expected) and 0 when equal;
    // both are success from our point of view. Only a real error (>1 / spawn fail) rejects.
    const res = yield* shell.run(cmd, args, { timeoutMs: 60_000 });
    if (res.exitCode > 1) {
      return { normalized: null, raw: res.stderr.trim(), status: "error" };
    }
    const metricLine = res.stderr.trim();
    const m = metricLine.match(/\(([-+0-9.eE]+)\)/);
    const normalized = m ? Number(m[1]) : null;
    return { normalized, raw: metricLine, status: "diff" };
  });
}

/** Run a page-image visual diff between two PDFs. */
export function runVisualDiff(
  referencePdf: string,
  candidatePdf: string,
  workDir: string,
  optsIn: Partial<VisualDiffOptions> = {},
): Effect.Effect<VisualDiffReport, MissingToolError | ToolFailedError | InvalidInputError | FsError, Shell | FileSystem> {
  const opts = { ...DEFAULT_VISUAL_DIFF_OPTIONS, ...optsIn };
  return Effect.gen(function* () {
    const caps = yield* detectCapabilities();
    yield* requireCapability(caps, "pdftoppm");
    if (!(caps.tools.compare.available || caps.tools.magick.available)) {
      yield* requireCapability(caps, "compare"); // fails with a clear hint
    }

    const fs = yield* FileSystem;
    const refBytes = yield* fs.readBytes(referencePdf);
    const candBytes = yield* fs.readBytes(candidatePdf);

    const refPages = yield* renderPdf(referencePdf, `${workDir}/reference`, "page", opts.dpi);
    const candPages = yield* renderPdf(candidatePdf, `${workDir}/candidate`, "page", opts.dpi);
    yield* fs.ensureDir(`${workDir}/diff`);
    const pageCount = Math.min(refPages.length, candPages.length);

    const comparisons: PageComparison[] = [];
    let sum = 0;
    let counted = 0;
    for (let i = 0; i < pageCount; i++) {
      const refPng = refPages[i]!;
      const candPng = candPages[i]!;
      const diffPng = `${workDir}/diff/page-${i + 1}.png`;
      const c = yield* comparePages(refPng, candPng, diffPng, opts.fuzzPercent);
      let status: "match" | "diff" | "error" = c.status;
      if (c.normalized !== null) {
        if (c.normalized <= opts.maxNormalizedRmse) status = "match";
        else status = "diff";
        sum += c.normalized;
        counted++;
      }
      comparisons.push({
        page: i + 1,
        normalizedRmse: c.normalized,
        rawMetric: c.raw,
        status,
      });
    }

    const samePageCount = refPages.length === candPages.length;
    const diffPages = comparisons.filter((c) => c.status === "diff").length;
    const errorPages = comparisons.filter((c) => c.status === "error").length;
    const mean = counted > 0 ? sum / counted : null;
    const diffFraction = pageCount > 0 ? diffPages / pageCount : 1;

    const visualFidelityAssertable = opts.referenceProvenance === "docx-render";
    const notes: string[] = [];
    if (!visualFidelityAssertable) {
      notes.push(
        `Referenz-Provenienz ist "${opts.referenceProvenance}", nicht "docx-render". Metriken werden berechnet, aber es wird KEINE visuelle Fidelity behauptet.`,
      );
    }
    if (!samePageCount) {
      notes.push(`Seitenzahlen unterscheiden sich (${refPages.length} vs ${candPages.length}); nur gemeinsame Seiten verglichen.`);
    }
    if (errorPages > 0) {
      notes.push(`${errorPages} Seite(n) konnten nicht verglichen werden (compare-Fehler).`);
    }

    const metricsPass = errorPages === 0 && diffFraction <= opts.maxDiffPageFraction;
    const passed = visualFidelityAssertable && metricsPass && samePageCount;

    return {
      schemaVersion: "1",
      generatedBy: `${TOOL_NAME}@${VERSION}`,
      reference: { path: toPosix(referencePdf), provenance: opts.referenceProvenance, sha256: hashBytes(refBytes) },
      candidate: { path: toPosix(candidatePdf), provenance: opts.candidateProvenance, sha256: hashBytes(candBytes) },
      rendering: { tool: "pdftoppm", dpi: opts.dpi, referencePages: refPages.length, candidatePages: candPages.length },
      comparisons,
      samePageCount,
      thresholds: { maxNormalizedRmse: opts.maxNormalizedRmse, maxDiffPageFraction: opts.maxDiffPageFraction },
      visualFidelityAssertable,
      meanNormalizedRmse: mean,
      passed,
      notes,
      capabilities: caps,
    };
  });
}
