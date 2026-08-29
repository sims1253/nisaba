import { describe, it, expect, beforeAll, type TestContext } from "vitest";
import { Effect, Layer } from "effect";
import { mkdtempSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { checkPdfCompliance } from "../src/pdf/compliance.js";
import { runVisualDiff } from "../src/visualdiff/harness.js";
import { detectCapabilities, type CapabilitiesReport, type ToolId } from "../src/externals/capabilities.js";
import { Shell, ShellLive } from "../src/externals/shell.js";
import { FileSystem, FileSystemLive } from "../src/externals/fs.js";
import { buildSampleDocumentDocx } from "../src/fixtures/generate.js";
import { execFile } from "../src/externals/process.js";

const Live = Layer.merge(ShellLive, FileSystemLive);
const run = <A, E>(eff: Effect.Effect<A, E, Shell | FileSystem>) =>
  Effect.runPromise(Effect.provide(eff, Live));

const capsP = run(detectCapabilities());

/** Render the fixture DOCX to PDF with LibreOffice, if available. */
let pdfPath: string | null = null;
let reason: string | null = null;

beforeAll(async () => {
  const caps = await capsP;
  if (!caps.tools.libreoffice.available) {
    reason = "libreoffice not available";
    return;
  }
  const dir = mkdtempSync(path.join(tmpdir(), "nisaba-ext-"));
  const docxPath = path.join(dir, "sample-document.docx");
  writeFileSync(docxPath, buildSampleDocumentDocx());
  try {
    await execFile(caps.tools.libreoffice.path ?? "soffice", [
      "--headless", "--convert-to", "pdf", "--outdir", dir, docxPath,
    ], { timeoutMs: 90_000 });
    const candidate = path.join(dir, "sample-document.pdf");
    pdfPath = existsSync(candidate) ? candidate : null;
    if (!pdfPath) reason = "soffice produced no PDF";
  } catch (e) {
    reason = `soffice failed: ${(e as Error).message}`;
  }
}, 120_000);

/** Names of `tools` that the probed environment does not provide. */
const missingTools = (caps: CapabilitiesReport, tools: readonly ToolId[]): string[] =>
  tools.filter((id) => !caps.tools[id].available);

/**
 * Skip the current test unless the fixture DOCX rendered to PDF and none of
 * `tools` (or anything in `extraMissing`) is unavailable. Skipping goes
 * through the Vitest test context so the reporter lists the test as skipped —
 * an early `return` here would report it as passed on machines without the
 * external tools, hiding that nothing was tested.
 */
const gateOn = (
  ctx: TestContext,
  caps: CapabilitiesReport,
  tools: readonly ToolId[],
  extraMissing: readonly string[] = [],
): void => {
  const missing = [...missingTools(caps, tools), ...extraMissing];
  if (pdfPath === null || missing.length > 0) {
    const note = [
      reason,
      missing.length > 0 ? `missing tools: ${missing.join(", ")}` : null,
    ]
      .filter((s): s is string => s !== null)
      .join("; ");
    ctx.skip(true, note);
  }
};

describe("pdf compliance (gated on libreoffice + poppler + qpdf)", () => {
  it("passes the full battery on the rendered fixture", async (ctx) => {
    const caps = await capsP;
    gateOn(ctx, caps, ["pdfinfo", "pdftotext", "qpdf"]);
    const work = mkdtempSync(path.join(tmpdir(), "nisaba-comp-"));
    const report = await run(checkPdfCompliance(pdfPath!, work));
    const byId = Object.fromEntries(report.checks.map((c) => [c.id, c])) as Record<string, (typeof report.checks)[number]>;
    expect(byId.encryption!.status).toBe("pass");
    expect(byId["text-extractable"]!.status).toBe("pass");
    expect(byId["index-labels"]!.status).toBe("pass");
    expect(report.passed).toBe(true);
  });
});

describe("visual diff (gated on pdftoppm + compare)", () => {
  it("self-diff is zero and asserts fidelity only with docx-render provenance", async (ctx) => {
    const caps = await capsP;
    // Per-pixel comparison needs `compare`, or `magick compare` as fallback.
    const comparator =
      caps.tools.compare.available || caps.tools.magick.available ? [] : ["compare/magick"];
    gateOn(ctx, caps, ["pdftoppm"], comparator);
    const work = mkdtempSync(path.join(tmpdir(), "nisaba-vd-"));
    const report = await run(
      runVisualDiff(pdfPath!, pdfPath!, work, {
        dpi: 100,
        referenceProvenance: "docx-render",
        candidateProvenance: "pdf",
      }),
    );
    expect(report.samePageCount).toBe(true);
    expect(report.meanNormalizedRmse).toBe(0);
    expect(report.visualFidelityAssertable).toBe(true);
    expect(report.passed).toBe(true);

    const reportUnknown = await run(
      runVisualDiff(pdfPath!, pdfPath!, mkdtempSync(path.join(tmpdir(), "nisaba-vd2-")), {
        dpi: 100,
        referenceProvenance: "unknown",
      }),
    );
    expect(reportUnknown.visualFidelityAssertable).toBe(false);
    expect(reportUnknown.passed).toBe(false);
  });
});
