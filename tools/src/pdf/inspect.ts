/**
 * PDF inspection helpers over Poppler (`pdfinfo`, `pdftotext`) and `qpdf`.
 *
 * These are thin, typed wrappers around the external CLIs, used by
 * {@link ./compliance.ts}. Every call honours a hard timeout and captures
 * output verbatim. None of them interpret a non-zero exit as a thrown error —
 * callers decide what exit codes mean — but they do tag spawn failures.
 */
import { Effect } from "effect";
import { Shell } from "../externals/shell.js";
import { FileSystem } from "../externals/fs.js";
import { toPosix } from "../paths.js";
import { FsError, InvalidInputError, MissingToolError } from "../errors.js";

/** A parsed `pdfinfo` key/value map. */
export type PdfInfo = ReadonlyMap<string, string>;

/** Run `pdfinfo` and parse its `Key: value` output. */
export function pdfInfo(pdfPath: string): Effect.Effect<PdfInfo, MissingToolError, Shell> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    const res = yield* shell.run("pdfinfo", [pdfPath], { timeoutMs: 30_000 });
    const map = new Map<string, string>();
    for (const line of (res.stdout + "\n" + res.stderr).split("\n")) {
      const idx = line.indexOf(":");
      if (idx > 0) {
        const k = line.slice(0, idx).trim();
        const v = line.slice(idx + 1).trim();
        if (k) map.set(k, v);
      }
    }
    return map;
  });
}

/** Extract the document text with `pdftotext -layout`. */
export function pdfToText(pdfPath: string): Effect.Effect<string, MissingToolError, Shell> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    // -q for quiet; emit to stdout via "-".
    const res = yield* shell.run("pdftotext", ["-q", "-layout", pdfPath, "-"], { timeoutMs: 60_000, maxBuffer: 64 * 1024 * 1024 });
    return res.stdout;
  });
}

/** Decompress and linearise a PDF with `qpdf --qdf` so annotation/URI scanning is reliable. */
export function qpdfQdf(
  pdfPath: string,
  outPath: string,
): Effect.Effect<{ outPath: string; exitCode: number; stderr: string }, MissingToolError, Shell> {
  return Effect.gen(function* () {
    const shell = yield* Shell;
    const res = yield* shell.run("qpdf", ["--decode-level=all", "--qdf", pdfPath, outPath], {
      timeoutMs: 60_000,
    });
    return { outPath: toPosix(outPath), exitCode: res.exitCode, stderr: res.stderr };
  });
}

/** Read the QDF-decompressed PDF text for annotation/URI scanning. */
export function readQdfText(
  pdfPath: string,
  workDir: string,
  qdfName = "decoded.qdf.pdf",
): Effect.Effect<{ text: string | null; note: string }, MissingToolError | InvalidInputError | FsError, Shell | FileSystem> {
  return Effect.gen(function* () {
    const fs = yield* FileSystem;
    const outPath = `${workDir}/${qdfName}`;
    const q = yield* qpdfQdf(pdfPath, outPath);
    if (q.exitCode !== 0) {
      return { text: null, note: `qpdf --qdf exited ${q.exitCode}: ${q.stderr.trim()}` };
    }
    const bytes = yield* fs.readBytes(outPath);
    // QDF is mostly ASCII-parseable; decode as latin1 to preserve byte values.
    const text = Buffer.from(bytes).toString("latin1");
    return { text, note: "decoded" };
  });
}
