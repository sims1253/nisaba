/**
 * Low-level, shell-free process execution and PATH lookup.
 *
 * Nothing here ever invokes a shell (`shell: false`), so a filename or argument
 * can never be interpreted as a command. This is the foundation that lets us
 * run LibreOffice / Poppler / ImageMagick / qpdf safely with user-influenced
 * paths.
 */
import { spawn } from "node:child_process";
import { access, constants } from "node:fs/promises";
import path from "node:path";
import { ToolFailedError, MissingToolError } from "../errors.js";

export interface ExecOptions {
  /** Working directory. Defaults to `process.cwd()`. */
  readonly cwd?: string;
  /** Environment. Defaults to `process.env`. */
  readonly env?: NodeJS.ProcessEnv;
  /** Hard timeout in milliseconds. On expiry the child is killed and we fail. */
  readonly timeoutMs?: number;
  /** Stdin bytes to write, if any. */
  readonly stdin?: Uint8Array | string;
  /** Max stdout/stderr bytes to buffer. Defaults to 16 MiB each. */
  readonly maxBuffer?: number;
}

export interface ExecResult {
  readonly command: string;
  readonly args: readonly string[];
  readonly exitCode: number;
  readonly stdout: string;
  readonly stderr: string;
  readonly durationMs: number;
  readonly timedOut: boolean;
}

/**
 * Look up an executable on PATH without a shell. Returns the resolved absolute
 * path, or `null` if not found. On Windows also appends common extensions.
 */
export async function which(command: string, env: NodeJS.ProcessEnv = process.env): Promise<string | null> {
  if (path.isAbsolute(command) || command.includes(path.sep)) {
    try {
      await access(command, constants.X_OK);
      return path.resolve(command);
    } catch {
      return null;
    }
  }
  const pathVar = env.PATH ?? env.Path;
  if (!pathVar) return null;
  const exts = process.platform === "win32" ? (env.PATHEXT ?? ".EXE").split(path.delimiter) : [""];
  for (const dir of pathVar.split(path.delimiter)) {
    if (!dir) continue;
    for (const ext of exts) {
      const candidate = path.resolve(dir, command + ext);
      try {
        await access(candidate, constants.X_OK);
        return candidate;
      } catch {
        // try next
      }
    }
  }
  return null;
}

/**
 * Spawn `command` with `args`, never through a shell. Captures stdout/stderr as
 * UTF-8 strings up to `maxBuffer` bytes. Resolves on clean exit (any code),
 * rejects only on spawn failure or timeout.
 *
 * Callers decide what exit codes mean; this function reports them faithfully.
 */
export function execFile(command: string, args: readonly string[], opts: ExecOptions = {}): Promise<ExecResult> {
  return new Promise((resolve, reject) => {
    const start = Date.now();
    const child = spawn(command, args as string[], {
      cwd: opts.cwd,
      env: opts.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
      shell: false,
      windowsHide: true,
    });
    const maxBuffer = opts.maxBuffer ?? 16 * 1024 * 1024;
    let stdout = "";
    let stderr = "";
    let stdoutDone = false;
    let stderrDone = false;
    let timedOut = false;
    let timer: NodeJS.Timeout | undefined;

    const settle = (exitCode: number | null) => {
      if (timer) clearTimeout(timer);
      resolve({
        command,
        args,
        exitCode: exitCode ?? -1,
        stdout,
        stderr,
        durationMs: Date.now() - start,
        timedOut,
      });
    };

    const trySettle = (code: number | null) => {
      // Wait for both streams to close before resolving, so we capture all output.
      if ((stdoutDone || !child.stdout) && (stderrDone || !child.stderr)) settle(code ?? 0);
    };

    if (opts.timeoutMs && Number.isFinite(opts.timeoutMs)) {
      timer = setTimeout(() => {
        timedOut = true;
        child.kill("SIGKILL");
      }, opts.timeoutMs);
    }

    if (child.stdout) {
      child.stdout.setEncoding("utf8");
      child.stdout.on("data", (chunk: string) => {
        if (stdout.length < maxBuffer) stdout += chunk.slice(0, maxBuffer - stdout.length);
      });
      child.stdout.on("end", () => {
        stdoutDone = true;
        if (child.exitCode !== null && child.exitCode !== undefined) trySettle(child.exitCode);
      });
    } else {
      stdoutDone = true;
    }
    if (child.stderr) {
      child.stderr.setEncoding("utf8");
      child.stderr.on("data", (chunk: string) => {
        if (stderr.length < maxBuffer) stderr += chunk.slice(0, maxBuffer - stderr.length);
      });
      child.stderr.on("end", () => {
        stderrDone = true;
        if (child.exitCode !== null && child.exitCode !== undefined) trySettle(child.exitCode);
      });
    } else {
      stderrDone = true;
    }

    child.on("error", (err) => {
      if (timer) clearTimeout(timer);
      reject(new MissingToolError({ tool: command, hint: `failed to spawn: ${err.message}`, command }));
    });

    child.on("close", (code) => {
      trySettle(code);
    });

    if (opts.stdin !== undefined && child.stdin) {
      const data = typeof opts.stdin === "string" ? Buffer.from(opts.stdin, "utf8") : Buffer.from(opts.stdin);
      child.stdin.end(data);
    } else if (child.stdin) {
      child.stdin.end();
    }
  });
}

/**
 * Run a command that is *expected* to succeed (exit 0). Non-zero exit, timeout,
 * or a missing binary is surfaced as {@link ToolFailedError} /
 * {@link MissingToolError} — the exact contract the higher-level Effect service
 * relies on.
 */
export async function requireSuccess(
  command: string,
  args: readonly string[],
  opts: ExecOptions = {},
): Promise<ExecResult> {
  const resolved = (await which(command, opts.env)) ?? command;
  const res = await execFile(resolved, args, opts);
  if (res.timedOut) {
    throw new ToolFailedError({
      tool: command,
      exitCode: res.exitCode,
      stdout: res.stdout,
      stderr: `(timed out after ${opts.timeoutMs}ms)\n${res.stderr}`,
      args,
    });
  }
  if (res.exitCode !== 0) {
    throw new ToolFailedError({
      tool: command,
      exitCode: res.exitCode,
      stdout: res.stdout,
      stderr: res.stderr,
      args,
    });
  }
  return res;
}
