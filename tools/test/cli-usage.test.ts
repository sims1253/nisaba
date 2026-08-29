import { describe, it, expect, vi } from "vitest";
import { Effect } from "effect";
import { mkdtempSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { parseArgs } from "../src/cli/args.js";
import { dispatch, Live } from "../src/cli/index.js";
import { runCli } from "../src/cli/render.js";

/**
 * Usage-error regression tests: a missing or invalid option must surface as
 * the documented JSON error envelope with exit code 1 — never as an exception
 * escaping `dispatch` (which bin/nisaba-tools.ts would report as a raw stack
 * trace instead of the envelope).
 */
interface CapturedRun {
  readonly exitCode: number;
  readonly envelope: {
    ok: boolean;
    error?: { kind: string; message: string };
    exitCode: number;
  };
}

/** Dispatch argv through the real runCli, capturing the printed envelope. */
async function runCommand(argv: readonly string[]): Promise<CapturedRun> {
  const chunks: string[] = [];
  const spy = vi.spyOn(process.stdout, "write").mockImplementation(((chunk: string | Uint8Array) => {
    chunks.push(typeof chunk === "string" ? chunk : Buffer.from(chunk).toString("utf8"));
    return true;
  }) as typeof process.stdout.write);
  try {
    const exitCode = await runCli(Effect.provide(dispatch(parseArgs(argv)), Live));
    return { exitCode, envelope: JSON.parse(chunks.join("")) };
  } finally {
    spy.mockRestore();
  }
}

describe("CLI usage errors render the JSON error envelope", () => {
  it("dispatch no longer throws synchronously on a missing required option", () => {
    expect(() => dispatch(parseArgs(["docx-introspect"]))).not.toThrow();
  });

  it.each([
    [["docx-introspect"], "missing required option --input"],
    [["typst-skeleton"], "missing required option --manifest"],
    [["validate-schema"], "missing required option --manifest"],
    [["ris-roundtrip"], "missing required option --input"],
    [["fixtures-gen"], "missing required option --output"],
    [["visual-diff"], "missing required option --reference"],
    [["visual-diff", "--reference", "a.pdf"], "missing required option --candidate"],
  ])("missing required option: %j", async (argv, message) => {
    const { exitCode, envelope } = await runCommand(argv);
    expect(exitCode).toBe(1);
    expect(envelope).toEqual({
      ok: false,
      error: { kind: "UsageError", message },
      exitCode: 1,
    });
  });

  it("rejects a valueless required flag instead of reporting it missing", async () => {
    // parseArgs files a bare `--input` under `flags`; requireOption must say
    // the option needs a value, mirroring parseProvenanceOption.
    const { exitCode, envelope } = await runCommand(["docx-introspect", "--input"]);
    expect(exitCode).toBe(1);
    expect(envelope.ok).toBe(false);
    expect(envelope.error).toEqual({ kind: "UsageError", message: "--input requires a value" });
  });

  it.each([
    ["--dpi", "15O"],
    ["--fuzz-percent", "abc"],
    ["--max-normalized-rmse", ""],
    ["--max-diff-page-fraction", "1e999"], // parses to Infinity, not finite
  ])("invalid numeric option %s %j fails loudly, not silently", async (key, value) => {
    const { exitCode, envelope } = await runCommand([
      "visual-diff", "--reference", "a.pdf", "--candidate", "b.pdf", key, value,
    ]);
    expect(exitCode).toBe(1);
    expect(envelope.ok).toBe(false);
    expect(envelope.error!.kind).toBe("UsageError");
    expect(envelope.error!.message).toBe(`invalid ${key} value "${value}"; expected a finite number`);
  });

  it("rejects a valueless numeric option instead of silently using the default", async () => {
    // parseArgs files a bare `--dpi` (next token is an option) under `flags`;
    // numOpt must reject it rather than treating the option as absent and
    // running under default thresholds.
    const { exitCode, envelope } = await runCommand([
      "visual-diff", "--reference", "a.pdf", "--candidate", "b.pdf", "--dpi", "--fuzz-percent", "5",
    ]);
    expect(exitCode).toBe(1);
    expect(envelope.ok).toBe(false);
    expect(envelope.error).toEqual({ kind: "UsageError", message: "--dpi requires a value" });
  });

  it("accepts a valid numeric option and keeps the default when absent", async () => {
    // A valid --dpi (and, separately, no --dpi at all) must pass validation:
    // the failure below is the invalid provenance that follows the numeric
    // options inside the command effect, proving they were consumed.
    for (const extra of [["--dpi", "100"], []] as const) {
      const { envelope } = await runCommand([
        "visual-diff", "--reference", "a.pdf", "--candidate", "b.pdf",
        ...extra,
        "--reference-provenance", "typst",
      ]);
      expect(envelope.error!.kind).toBe("UsageError");
      expect(envelope.error!.message).toContain('invalid --reference-provenance value "typst"');
    }
  });

  it("keeps happy paths on the ok envelope with exit 0", async () => {
    const out = mkdtempSync(path.join(tmpdir(), "nisaba-usage-ok-"));
    try {
      const { exitCode, envelope } = await runCommand(["fixtures-gen", "--output", out]);
      expect(exitCode).toBe(0);
      expect(envelope.ok).toBe(true);
      expect(envelope.exitCode).toBe(0);
      expect(existsSync(path.join(out, "sample-document.docx"))).toBe(true);
    } finally {
      try {
        rmSync(out, { recursive: true, force: true });
      } catch {
        // best-effort
      }
    }
  });
});
