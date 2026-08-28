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
import { runVisualDiff, DEFAULT_VISUAL_DIFF_OPTIONS, PROVENANCES, type Provenance } from "../visualdiff/harness.js";
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
    const json = yield* Effect.try({
      try: () => JSON.parse(text) as unknown,
      catch: (e) => new InvalidInputError({ path: file, reason: `invalid JSON: ${(e as Error).message}` }),
    });
    return yield* Effect.try({
      try: () => Schema.decodeUnknownSync(ManifestSchema)(json),
      catch: (e) => new InvalidInputError({ path: file, reason: `not a valid manifest: ${(e as Error).message}` }),
    });
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

/**
 * Parse a numeric option. Absent → `fallback`; present-but-unparseable
 * (e.g. `--dpi 15O`, or an empty value) → UsageError — a present-but-invalid
 * value must never silently fall back to the default, consistent with the
 * validated-CLI-input policy of `parseProvenanceOption`.
 */
function numOpt(
  args: ReturnType<typeof parseArgs>,
  key: string,
  fallback: number,
): Effect.Effect<number, UsageError> {
  const raw = args.options.get(key);
  if (raw === undefined) return Effect.succeed(fallback);
  const n = raw.trim() === "" ? Number.NaN : Number(raw);
  return Number.isFinite(n)
    ? Effect.succeed(n)
    : Effect.fail(new UsageError(`invalid --${key} value "${raw}"; expected a finite number`));
}

/**
 * Parse a `--reference-provenance` / `--candidate-provenance` option. The raw
 * CLI string is untrusted: casting it unchecked let a typo silently become a
 * bogus provenance in the diff report. Mirrors the role narrowing in
 * `web/src/api.ts`, but rejects (with the valid values listed) instead of
 * falling back — an unknown value must fail loudly. A valueless flag (the
 * option given without an argument) is rejected too: parseArgs files it under
 * `flags`, where an unchecked lookup would silently default to "unknown".
 */
export function parseProvenanceOption(
  args: ReturnType<typeof parseArgs>,
  key: "reference-provenance" | "candidate-provenance",
): Effect.Effect<Provenance, UsageError> {
  if (args.flags.has(key)) {
    return Effect.fail(new UsageError(`--${key} requires a value`));
  }
  const raw = args.options.get(key) ?? "unknown";
  return (PROVENANCES as readonly string[]).includes(raw)
    ? Effect.succeed(raw as Provenance)
    : Effect.fail(
        new UsageError(`invalid --${key} value "${raw}"; valid values: ${PROVENANCES.join(", ")}`),
      );
}

function dispatch(args: ReturnType<typeof parseArgs>): CmdResult {
  switch (args.command) {
    case "capabilities":
      return detectCapabilities().pipe(Effect.map((report) => ({ report, passed: true })));

    case "docx-introspect": {
      const output = args.options.get("output");
      return Effect.gen(function* () {
        const input = yield* requireOption(args, "input");
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
      const output = args.options.get("output");
      return Effect.gen(function* () {
        const manifestFile = yield* requireOption(args, "manifest");
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
      const typstFile = args.options.get("typst");
      const againstManifest = args.options.get("against-manifest");
      return Effect.gen(function* () {
        const manifestFile = yield* requireOption(args, "manifest");
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
        return yield* Effect.fail(new UsageError("validate-schema requires --typst <file> or --against-manifest <file>"));
      });
    }

    case "pdf-compliance": {
      const userWorkdir = args.options.get("workdir");
      const workdir = userWorkdir ?? tempWorkdir("pdf");
      return Effect.gen(function* () {
        const input = yield* requireOption(args, "input");
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
      const userWorkdir = args.options.get("workdir");
      const workdir = userWorkdir ?? tempWorkdir("vdiff");
      return Effect.gen(function* () {
        const reference = yield* requireOption(args, "reference");
        const candidate = yield* requireOption(args, "candidate");
        const dpi = yield* numOpt(args, "dpi", DEFAULT_VISUAL_DIFF_OPTIONS.dpi);
        const maxNorm = yield* numOpt(args, "max-normalized-rmse", DEFAULT_VISUAL_DIFF_OPTIONS.maxNormalizedRmse);
        const maxFrac = yield* numOpt(args, "max-diff-page-fraction", DEFAULT_VISUAL_DIFF_OPTIONS.maxDiffPageFraction);
        const fuzz = yield* numOpt(args, "fuzz-percent", DEFAULT_VISUAL_DIFF_OPTIONS.fuzzPercent);
        const referenceProvenance = yield* parseProvenanceOption(args, "reference-provenance");
        const candidateProvenance = yield* parseProvenanceOption(args, "candidate-provenance");
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
      const required = args.options.get("required")?.split(",").map((s) => s.trim()).filter(Boolean);
      return Effect.gen(function* () {
        const input = yield* requireOption(args, "input");
        const fs = yield* FileSystem;
        const text = yield* fs.readText(input);
        const report = checkRoundTrip(text, toPosix(input), required);
        return { report, passed: report.lossless };
      });
    }

    case "fixtures-gen":
      return Effect.gen(function* () {
        const output = yield* requireOption(args, "output");
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
