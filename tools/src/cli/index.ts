/**
 * Unified CLI entry point. Subcommands:
 *
 *   capabilities        — external-tool capability report
 *   docx-introspect     — DOCX → deterministic manifest JSON
 *   typst-skeleton      — manifest → Typst template skeleton
 *   validate-schema     — required-placeholder / schema preservation check
 *   visual-diff         — page-image visual diff (Poppler + ImageMagick)
 *   pdf-compliance      — PDF compatibility checks
 *   ris-roundtrip       — RIS lossless round-trip + field coverage
 *   fixtures-gen        — generate the sample DOCX fixture
 *
 * Every command prints one deterministic JSON envelope to stdout (see
 * {@link ./render.ts}) and exits 0 (pass), 2 (ran but violations), or 1 (error).
 */
import { Effect, Layer, Schema } from "effect";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { FileSystem, FileSystemLive } from "../externals/fs.js";
import { Shell, ShellLive } from "../externals/shell.js";
import { detectCapabilities } from "../externals/capabilities.js";
import { introspectDocx } from "../docx/introspect.js";
import { ManifestSchema, type Manifest } from "../docx/manifest.js";
import { generateTypstSkeleton } from "../typst/skeleton.js";
import { validateAgainstManifest, validateTypstSource } from "../typst/schema.js";
import { checkPdfCompliance } from "../pdf/compliance.js";
import { runVisualDiff, DEFAULT_VISUAL_DIFF_OPTIONS, type Provenance } from "../visualdiff/harness.js";
import { checkRoundTrip } from "../ris/roundtrip.js";
import { buildSampleDocumentDocx } from "../fixtures/generate.js";
import { hashValue } from "../json.js";
import { toPosix } from "../paths.js";
import { InvalidInputError, FsError } from "../errors.js";
import { parseArgs, requireOption, UsageError } from "./args.js";
import { runCli } from "./render.js";

const Live = Layer.merge(ShellLive, FileSystemLive);

type CmdResult = Effect.Effect<{ report: unknown; passed: boolean }, unknown, Shell | FileSystem>;

function loadManifest(file: string): Effect.Effect<Manifest, InvalidInputError | FsError, FileSystem> {
  return Effect.gen(function* () {
    const fs = yield* FileSystem;
    const text = yield* fs.readText(file);
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch (e) {
      yield* Effect.fail(new InvalidInputError({ path: file, reason: `invalid JSON: ${(e as Error).message}` }));
      return undefined as never;
    }
    const decoded = yield* Effect.try({
      try: () => Schema.decodeUnknownSync(ManifestSchema)(json),
      catch: (e) => new InvalidInputError({ path: file, reason: `not a valid manifest: ${(e as Error).message}` }),
    });
    return decoded;
  });
}

function tempWorkdir(prefix: string): string {
  return mkdtempSync(path.join(tmpdir(), `nisaba-${prefix}-`));
}

/**
 * Removes a workdir we created ourselves. Only called for temp dirs, never for
 * a user-supplied `--workdir`. Best-effort: a leftover dir on failure is
 * preferable to failing the command over cleanup.
 */
function cleanupWorkdir(workdir: string): void {
  try {
    rmSync(workdir, { recursive: true, force: true });
  } catch {
    // best-effort
  }
}

function numOpt(args: ReturnType<typeof parseArgs>, key: string): number | undefined {
  const v = args.options.get(key);
  if (v === undefined) return undefined;
  const n = Number(v);
  return Number.isFinite(n) ? n : undefined;
}

function dispatch(args: ReturnType<typeof parseArgs>): CmdResult {
  switch (args.command) {
    case "capabilities":
      return detectCapabilities().pipe(Effect.map((report) => ({ report, passed: true })));

    case "docx-introspect": {
      const input = requireOption(args, "input");
      const output = args.options.get("output");
      return Effect.gen(function* () {
        const fs = yield* FileSystem;
        const bytes = yield* fs.readBytes(input);
        const manifest = introspectDocx(bytes, path.basename(input));
        let writtenTo: string | undefined;
        if (output) {
          yield* fs.writeJson(output, manifest);
          writtenTo = toPosix(output);
        }
        return {
          report: {
            manifest,
            manifestSha256: hashValue(manifest),
            writtenTo,
          },
          passed: true,
        };
      });
    }

    case "typst-skeleton": {
      const manifestFile = requireOption(args, "manifest");
      const output = args.options.get("output");
      return Effect.gen(function* () {
        const fs = yield* FileSystem;
        const manifest = yield* loadManifest(manifestFile);
        const skeleton = generateTypstSkeleton(manifest);
        let writtenTo: string | undefined;
        if (output) {
          yield* fs.writeText(output, skeleton);
          writtenTo = toPosix(output);
        }
        return {
          report: { skeleton, sha256: hashValue(skeleton), manifestHash: hashValue(manifest), writtenTo },
          passed: true,
        };
      });
    }

    case "validate-schema": {
      const manifestFile = requireOption(args, "manifest");
      const typstFile = args.options.get("typst");
      const againstManifest = args.options.get("against-manifest");
      return Effect.gen(function* () {
        const fs = yield* FileSystem;
        const manifest = yield* loadManifest(manifestFile);
        if (typstFile) {
          const source = yield* fs.readText(typstFile);
          const report = validateTypstSource(manifest, source, toPosix(typstFile));
          return { report, passed: report.passed };
        }
        if (againstManifest) {
          const candidate = yield* loadManifest(againstManifest);
          const report = validateAgainstManifest(manifest, candidate, toPosix(againstManifest));
          return { report, passed: report.passed };
        }
        yield* Effect.fail(new UsageError("validate-schema requires --typst <file> or --against-manifest <file>"));
        return undefined as never;
      });
    }

    case "pdf-compliance": {
      const input = requireOption(args, "input");
      const userWorkdir = args.options.get("workdir");
      const workdir = userWorkdir ?? tempWorkdir("pdf");
      return Effect.gen(function* () {
        yield* (yield* FileSystem).ensureDir(workdir);
        const report = yield* checkPdfCompliance(toPosix(path.resolve(input)), workdir);
        return { report, passed: report.passed };
      }).pipe(
        Effect.onExit(() =>
          Effect.sync(() => {
            if (userWorkdir === undefined) cleanupWorkdir(workdir);
          }),
        ),
      );
    }

    case "visual-diff": {
      const reference = requireOption(args, "reference");
      const candidate = requireOption(args, "candidate");
      const userWorkdir = args.options.get("workdir");
      const workdir = userWorkdir ?? tempWorkdir("vdiff");
      const dpi = numOpt(args, "dpi") ?? DEFAULT_VISUAL_DIFF_OPTIONS.dpi;
      const maxNorm = numOpt(args, "max-normalized-rmse") ?? DEFAULT_VISUAL_DIFF_OPTIONS.maxNormalizedRmse;
      const maxFrac = numOpt(args, "max-diff-page-fraction") ?? DEFAULT_VISUAL_DIFF_OPTIONS.maxDiffPageFraction;
      const fuzz = numOpt(args, "fuzz-percent") ?? DEFAULT_VISUAL_DIFF_OPTIONS.fuzzPercent;
      const referenceProvenance = (args.options.get("reference-provenance") ?? "unknown") as Provenance;
      const candidateProvenance = (args.options.get("candidate-provenance") ?? "unknown") as Provenance;
      return Effect.gen(function* () {
        const report = yield* runVisualDiff(
          toPosix(path.resolve(reference)),
          toPosix(path.resolve(candidate)),
          workdir,
          { dpi, maxNormalizedRmse: maxNorm, maxDiffPageFraction: maxFrac, fuzzPercent: fuzz, referenceProvenance, candidateProvenance },
        );
        return { report, passed: report.passed };
      }).pipe(
        Effect.onExit(() =>
          Effect.sync(() => {
            if (userWorkdir === undefined) cleanupWorkdir(workdir);
          }),
        ),
      );
    }

    case "ris-roundtrip": {
      const input = requireOption(args, "input");
      const required = args.options.get("required")?.split(",").map((s) => s.trim()).filter(Boolean);
      return Effect.gen(function* () {
        const fs = yield* FileSystem;
        const text = yield* fs.readText(input);
        const report = checkRoundTrip(text, toPosix(input), required);
        return { report, passed: report.lossless };
      });
    }

    case "fixtures-gen": {
      const output = requireOption(args, "output");
      return Effect.gen(function* () {
        const fs = yield* FileSystem;
        const docxPath = path.join(output, "sample-document.docx");
        const bytes = buildSampleDocumentDocx();
        yield* fs.writeBytes(docxPath, bytes);
        return {
          report: {
            files: [toPosix(docxPath)],
            docxSha256: hashValue(bytes),
            bytes: bytes.length,
            note: "Deterministic sample DOCX fixture. Not a real DOCX template — visual fidelity is asserted only against a real DOCX render.",
          },
          passed: true,
        };
      });
    }

    default:
      return Effect.fail(new UsageError(USAGE));
  }
}

const USAGE = `Usage: nisaba-tools <command> [options]

Commands:
  capabilities                 Probe external tools (typst, libreoffice, poppler, qpdf, imagemagick).
  docx-introspect --input <f> [--output <m.json>]   DOCX → manifest.
  typst-skeleton --manifest <m.json> [--output <f>] Manifest → Typst skeleton.
  validate-schema --manifest <m.json> (--typst <f> | --against-manifest <m2.json>)
  pdf-compliance --input <pdf> [--workdir <dir>]
  visual-diff --reference <pdf> --candidate <pdf> [--reference-provenance docx-render|typst-compile|pdf|unknown] [--dpi N] [--max-normalized-rmse F] [--max-diff-page-fraction F] [--fuzz-percent N] [--workdir <dir>]
  ris-roundtrip --input <f.ris> [--required TY,AU,PY,TI,JO]
  fixtures-gen --output <dir>   Generate the sample DOCX fixture.

Exit codes: 0 = pass, 2 = ran but violations found, 1 = error.`;

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const program = Effect.provide(dispatch(args), Live);
  const exitCode = await runCli(program);
  process.exitCode = exitCode;
}

export { dispatch, main, USAGE, Live };
