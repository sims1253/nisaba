/**
 * Tagged errors used across the tooling.
 *
 * Every failure that can escape to a CLI boundary has a `_tag` so callers (and
 * tests) can recover with {@link Effect.catchTag}. The string payloads are kept
 * structured so they can be serialised to deterministic JSON without stack
 * traces leaking absolute paths or timestamps.
 */
import { Data } from "effect";

/** A required external tool (LibreOffice, Poppler, ImageMagick, qpdf, typst) is missing. */
export class MissingToolError extends Data.TaggedError("MissingToolError")<{
  readonly tool: string;
  readonly hint?: string;
  readonly command?: string;
}> {}

/** An external tool ran but exited non-zero or produced unusable output. */
export class ToolFailedError extends Data.TaggedError("ToolFailedError")<{
  readonly tool: string;
  readonly exitCode: number | null;
  readonly stdout: string;
  readonly stderr: string;
  readonly args: readonly string[];
}> {}

/** The user gave us a path that is not safe to use (escapes a sandbox root, etc.). */
export class UnsafePathError extends Data.TaggedError("UnsafePathError")<{
  readonly path: string;
  readonly reason: string;
}> {}

/** A file the user pointed us at does not exist or is not the expected kind. */
export class InvalidInputError extends Data.TaggedError("InvalidInputError")<{
  readonly path: string;
  readonly reason: string;
}> {}

/** A filesystem operation failed (permission denied, disk full, etc.). */
export class FsError extends Data.TaggedError("FsError")<{
  readonly path: string;
  readonly code: string;
  readonly message: string;
}> {}

/** A DOCX could be read as a zip but is missing a part we consider mandatory. */
export class MalformedDocxError extends Data.TaggedError("MalformedDocxError")<{
  readonly path: string;
  readonly missingPart?: string;
  readonly reason: string;
}> {}

/** Generic, non-tagged fallback for programmer errors. Never escapes to JSON. */
export class InternalError extends Data.TaggedError("InternalError")<{
  readonly message: string;
}> {}
