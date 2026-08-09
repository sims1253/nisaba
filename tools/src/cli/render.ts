/**
 * CLI run helpers: deterministic JSON envelopes and uniform error rendering.
 *
 * Every command prints exactly one JSON object to stdout:
 *
 *   { "ok": true, "result": <report>, "exitCode": 0|2 }
 *   { "ok": false, "error": { "kind": <tag>, ...fields }, "exitCode": 1 }
 *
 * `exitCode` is 2 when the tool ran but found violations (`passed === false`),
 * so CI can distinguish a tool failure from a failing check.
 */
import { Cause, Effect, Exit, Option } from "effect";
import { stableStringify } from "../json.js";
import {
  FsError,
  InvalidInputError,
  MalformedDocxError,
  MissingToolError,
  ToolFailedError,
  UnsafePathError,
} from "../errors.js";
import { UsageError } from "./args.js";

export interface OkEnvelope<T> {
  readonly ok: true;
  readonly result: T;
  readonly exitCode: 0 | 2;
}
export interface ErrEnvelope {
  readonly ok: false;
  readonly error: {
    readonly kind: string;
    readonly message: string;
    readonly [k: string]: unknown;
  };
  readonly exitCode: 1;
}

export function renderError(e: unknown): ErrEnvelope["error"] {
  if (e instanceof MissingToolError) {
    return {
      kind: "MissingToolError",
      message: `required external tool "${e.tool}" is not available`,
      tool: e.tool,
      hint: e.hint ?? null,
      command: e.command ?? null,
    };
  }
  if (e instanceof ToolFailedError) {
    return {
      kind: "ToolFailedError",
      message: `external tool "${e.tool}" failed (exit ${e.exitCode})`,
      tool: e.tool,
      exitCode: e.exitCode,
      args: e.args,
      stdout: truncate(e.stdout, 4000),
      stderr: truncate(e.stderr, 4000),
    };
  }
  if (e instanceof MalformedDocxError) {
    return { kind: "MalformedDocxError", message: e.reason, path: e.path, missingPart: e.missingPart ?? null };
  }
  if (e instanceof InvalidInputError) {
    return { kind: "InvalidInputError", message: e.reason, path: e.path };
  }
  if (e instanceof UnsafePathError) {
    return { kind: "UnsafePathError", message: e.reason, path: e.path };
  }
  if (e instanceof FsError) {
    return { kind: "FsError", message: e.message, path: e.path, code: e.code };
  }
  if (e instanceof UsageError) {
    return { kind: "UsageError", message: e.message };
  }
  return { kind: "UnknownError", message: e instanceof Error ? e.message : String(e) };
}

function truncate(s: string, max: number): string {
  return s.length <= max ? s : `${s.slice(0, max)}… (+${s.length - max} bytes)`;
}

/**
 * Run a command effect, print the JSON envelope, and resolve to the exit code.
 * `passed` governs exit 0 vs 2; any failure channel error is rendered uniformly.
 */
export async function runCli<E>(
  program: Effect.Effect<{ report: unknown; passed: boolean }, E, never>,
): Promise<number> {
  const exit = await Effect.runPromiseExit(program);
  if (Exit.isFailure(exit)) {
    const errOpt = Cause.findErrorOption(exit.cause);
    const err = Option.isSome(errOpt) ? errOpt.value : Cause.pretty(exit.cause);
    const body: ErrEnvelope = { ok: false, error: renderError(err), exitCode: 1 };
    process.stdout.write(stableStringify(body, 2) + "\n");
    return 1;
  }
  const { report, passed } = exit.value;
  const body: OkEnvelope<unknown> = { ok: true, result: report, exitCode: passed ? 0 : 2 };
  process.stdout.write(stableStringify(body, 2) + "\n");
  return body.exitCode;
}
