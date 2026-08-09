/**
 * Shell Effect service.
 *
 * Wraps {@link requireSuccess} / {@link execFile} from `./process.ts` so that
 * external tools can be invoked from inside `Effect.gen` and composed with the
 * rest of the pipeline. Two entry points:
 *
 *   - {@link ShellShape.run}        resolves with the raw result (any exit code)
 *   - {@link ShellShape.runSuccess} fails on non-zero exit or timeout
 *
 * Both honour a hard timeout and capture stdout/stderr verbatim.
 */
import { Effect, Context, Layer } from "effect";
import { ToolFailedError, MissingToolError } from "../errors.js";
import { execFile, requireSuccess, which, type ExecOptions, type ExecResult } from "./process.js";

export type { ExecOptions, ExecResult };

export interface ShellShape {
  /** Resolve an executable path on PATH without a shell. `null` if not found. */
  readonly which: (command: string) => Effect.Effect<string | null>;
  /** Run a command, resolving with the full result regardless of exit code. */
  readonly run: (
    command: string,
    args: readonly string[],
    opts?: ExecOptions,
  ) => Effect.Effect<ExecResult, MissingToolError>;
  /** Run a command that must exit 0; otherwise fail with {@link ToolFailedError}. */
  readonly runSuccess: (
    command: string,
    args: readonly string[],
    opts?: ExecOptions,
  ) => Effect.Effect<ExecResult, ToolFailedError | MissingToolError>;
}

export class Shell extends Context.Service<Shell, ShellShape>()("nisaba/Shell") {}

export const ShellLive = Layer.succeed(
  Shell,
  Shell.of({
    which: (command) => Effect.promise(() => which(command)),
    run: (command, args, opts) =>
      Effect.tryPromise({
        try: async () => {
          const resolved = (await which(command, opts?.env)) ?? command;
          return await execFile(resolved, args, opts ?? {});
        },
        catch: (e) => (e instanceof MissingToolError ? e : new MissingToolError({ tool: command, hint: String(e) })),
      }),
    runSuccess: (command, args, opts) =>
      Effect.tryPromise({
        try: () => requireSuccess(command, args, opts ?? {}),
        catch: (e) => (e instanceof ToolFailedError || e instanceof MissingToolError ? e : new ToolFailedError({
          tool: command, exitCode: null, stdout: "", stderr: String(e), args,
        })),
      }),
  }),
);
