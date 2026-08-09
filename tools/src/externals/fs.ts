/**
 * Filesystem Effect service.
 *
 * A thin, mockable wrapper over `node:fs/promises`. All write methods create
 * parent directories; path outputs are POSIX-normalised at the boundaries that
 * emit them. Errors are tagged so the CLI layer can render them uniformly.
 */
import { Effect, Context, Layer } from "effect";
import { mkdir, readFile, writeFile, stat, readdir, rm, access } from "node:fs/promises";
import { dirname } from "node:path";
import { FsError, InvalidInputError } from "../errors.js";
import { stableStringify } from "../json.js";

export interface FileStat {
  readonly path: string;
  readonly size: number;
  readonly mtimeMs: number;
  readonly isFile: boolean;
  readonly isDirectory: boolean;
}

/** The contract the rest of the code depends on. */
export interface FileSystemShape {
  readonly readBytes: (path: string) => Effect.Effect<Uint8Array, InvalidInputError | FsError>;
  readonly readText: (path: string) => Effect.Effect<string, InvalidInputError | FsError>;
  readonly writeBytes: (path: string, bytes: Uint8Array) => Effect.Effect<void, InvalidInputError | FsError>;
  readonly writeText: (path: string, text: string) => Effect.Effect<void, InvalidInputError | FsError>;
  readonly writeJson: (path: string, value: unknown) => Effect.Effect<void, InvalidInputError | FsError>;
  readonly ensureDir: (path: string) => Effect.Effect<void, InvalidInputError | FsError>;
  readonly exists: (path: string) => Effect.Effect<boolean, never>;
  readonly stat: (path: string) => Effect.Effect<FileStat, InvalidInputError | FsError>;
  readonly listDir: (path: string) => Effect.Effect<readonly string[], InvalidInputError | FsError>;
  readonly rm: (path: string) => Effect.Effect<void, InvalidInputError | FsError>;
}

/** Service key for {@link FileSystemShape}. */
export class FileSystem extends Context.Service<FileSystem, FileSystemShape>()("nisaba/FileSystem") {}

function wrapErr(p: string, e: unknown): InvalidInputError | FsError {
  const code = (e as NodeJS.ErrnoException)?.code ?? "EUNKNOWN";
  const message = (e as Error)?.message ?? String(e);
  if (code === "ENOENT" || code === "ENOTDIR") {
    return new InvalidInputError({ path: p, reason: `no such file or directory (${code})` });
  }
  return new FsError({ path: p, code, message });
}

export const FileSystemLive = Layer.succeed(
  FileSystem,
  FileSystem.of({
    readBytes: (p) =>
      Effect.tryPromise({ try: () => readFile(p), catch: (e) => wrapErr(p, e) }),
    readText: (p) =>
      Effect.tryPromise({ try: () => readFile(p, "utf8"), catch: (e) => wrapErr(p, e) }),
    writeBytes: (p, bytes) =>
      Effect.tryPromise({
        try: async () => {
          await mkdir(dirname(p), { recursive: true });
          await writeFile(p, bytes);
        },
        catch: (e) => wrapErr(p, e),
      }),
    writeText: (p, text) =>
      Effect.tryPromise({
        try: async () => {
          await mkdir(dirname(p), { recursive: true });
          await writeFile(p, text, "utf8");
        },
        catch: (e) => wrapErr(p, e),
      }),
    writeJson: (p, value) =>
      Effect.tryPromise({
        try: async () => {
          await mkdir(dirname(p), { recursive: true });
          await writeFile(p, stableStringify(value, 2) + "\n", "utf8");
        },
        catch: (e) => wrapErr(p, e),
      }),
    ensureDir: (p) =>
      Effect.tryPromise({ try: () => mkdir(p, { recursive: true }), catch: (e) => wrapErr(p, e) }),
    exists: (p) =>
      Effect.promise(() =>
        access(p)
          .then(() => true)
          .catch(() => false),
      ),
    stat: (p) =>
      Effect.tryPromise({
        try: async () => {
          const s = await stat(p);
          return {
            path: p,
            size: s.size,
            mtimeMs: s.mtimeMs,
            isFile: s.isFile(),
            isDirectory: s.isDirectory(),
          } satisfies FileStat;
        },
        catch: (e) => wrapErr(p, e),
      }),
    listDir: (p) =>
      Effect.tryPromise({ try: () => readdir(p), catch: (e) => wrapErr(p, e) }),
    rm: (p) =>
      Effect.tryPromise({
        try: () => rm(p, { recursive: true, force: true }),
        catch: (e) => wrapErr(p, e),
      }),
  }),
);
