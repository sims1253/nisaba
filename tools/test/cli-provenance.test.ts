import { describe, it, expect } from "vitest";
import { Cause, Effect, Exit, Option } from "effect";
import { parseArgs, UsageError } from "../src/cli/args.js";
import { dispatch, parseProvenanceOption, Live } from "../src/cli/index.js";
import { PROVENANCES } from "../src/visualdiff/harness.js";

/** Extract the typed failure from an Exit, mirroring render.ts. */
function failureOf<E>(exit: Exit.Exit<unknown, E>): E {
  if (Exit.isSuccess(exit)) throw new Error("expected a failure");
  const errOpt = Cause.findErrorOption(exit.cause);
  return Option.isSome(errOpt) ? (errOpt.value as E) : (Cause.pretty(exit.cause) as unknown as E);
}

describe("visual-diff provenance option validation", () => {
  it("accepts every valid provenance value", async () => {
    for (const p of PROVENANCES) {
      const args = parseArgs(["visual-diff", "--reference-provenance", p]);
      const result = await Effect.runPromise(parseProvenanceOption(args, "reference-provenance"));
      expect(result).toBe(p);
    }
  });

  it("defaults to \"unknown\" when the option is absent", async () => {
    const args = parseArgs(["visual-diff", "--reference", "a.pdf", "--candidate", "b.pdf"]);
    expect(await Effect.runPromise(parseProvenanceOption(args, "reference-provenance"))).toBe("unknown");
    expect(await Effect.runPromise(parseProvenanceOption(args, "candidate-provenance"))).toBe("unknown");
  });

  it("rejects a typo with a UsageError listing the valid values", async () => {
    const args = parseArgs(["visual-diff", "--reference-provenance", "docx-rendr"]);
    const exit = await Effect.runPromiseExit(parseProvenanceOption(args, "reference-provenance"));
    expect(Exit.isFailure(exit)).toBe(true);
    const err = failureOf(exit);
    expect(err).toBeInstanceOf(UsageError);
    expect((err as UsageError).message).toContain("docx-rendr");
    expect((err as UsageError).message).toContain("docx-render, typst-compile, pdf, unknown");
  });

  it("rejects a bogus candidate provenance the same way", async () => {
    const args = parseArgs(["visual-diff", "--candidate-provenance", "pdf2"]);
    const exit = await Effect.runPromiseExit(parseProvenanceOption(args, "candidate-provenance"));
    expect(Exit.isFailure(exit)).toBe(true);
    expect(failureOf(exit)).toBeInstanceOf(UsageError);
  });

  it("fails the whole visual-diff command on an invalid provenance before touching services", async () => {
    const args = parseArgs([
      "visual-diff", "--reference", "a.pdf", "--candidate", "b.pdf",
      "--reference-provenance", "typst",
    ]);
    // The Live layers are provided but must never be touched: the command
    // fails on the invalid option before capability detection or file access.
    const exit = await Effect.runPromiseExit(Effect.provide(dispatch(args), Live));
    expect(Exit.isFailure(exit)).toBe(true);
    const err = failureOf(exit);
    expect(err).toBeInstanceOf(UsageError);
    expect((err as UsageError).message).toContain('invalid --reference-provenance value "typst"');
  });

  it("rejects a valueless flag instead of silently defaulting to \"unknown\"", async () => {
    // parseArgs files a valueless option under `flags` (the next token starts
    // with `-`); an unchecked lookup would quietly label the reference
    // "unknown" — the same silent-mislabel class this guard closes.
    const args = parseArgs(["visual-diff", "--reference-provenance", "--dpi", "100"]);
    const exit = await Effect.runPromiseExit(parseProvenanceOption(args, "reference-provenance"));
    expect(Exit.isFailure(exit)).toBe(true);
    const err = failureOf(exit);
    expect(err).toBeInstanceOf(UsageError);
    expect((err as UsageError).message).toBe("--reference-provenance requires a value");
  });

  it("also rejects a valueless flag at the end of argv", async () => {
    const args = parseArgs(["visual-diff", "--candidate-provenance"]);
    const exit = await Effect.runPromiseExit(parseProvenanceOption(args, "candidate-provenance"));
    expect(Exit.isFailure(exit)).toBe(true);
    expect(failureOf(exit)).toBeInstanceOf(UsageError);
  });
});
