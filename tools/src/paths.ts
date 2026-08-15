/**
 * Safe, platform-normalised path handling.
 *
 * All paths in tool output are emitted POSIX-style (forward slashes) so reports
 * are identical whether produced on Linux CI or a developer's macOS machine.
 * The helpers here also enforce an optional sandbox root so a user-supplied
 * output path can never escape the directory a caller intends to write into.
 */
import path from "node:path";
import { UnsafePathError } from "./errors.js";

/** Normalise any path to POSIX forward slashes (no leading/backslash translation). */
export function toPosix(p: string): string {
  return p.split(path.sep).join("/");
}

/** POSIX-style relative path from `from` to `to`, both absolute. */
export function relPosix(from: string, to: string): string {
  return toPosix(path.relative(path.resolve(from), path.resolve(to)));
}

/**
 * Ensure `target` resolves to a path inside `root`.
 * Returns the POSIX-style absolute target on success, otherwise fails with
 * {@link UnsafePathError}. This is the single chokepoint for user-supplied
 * output paths.
 */
export function ensureWithin(root: string, target: string): string {
  const absRoot = path.resolve(root);
  const absTarget = path.resolve(absRoot, target);
  const rel = path.relative(absRoot, absTarget);
  if (rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel))) {
    return toPosix(absTarget);
  }
  throw new UnsafePathError({
    path: target,
    reason: `resolves outside of the allowed root '${toPosix(absRoot)}'`,
  });
}

/**
 * Safe join: each segment is treated as a single path component. Segments may
 * not contain path separators or `..` that escapes. Used when constructing
 * output paths from pieces that could otherwise be path-injected.
 */
export function safeJoin(base: string, ...segments: string[]): string {
  const resolved = path.resolve(base, ...segments);
  // Re-check containment to reject any segment like "../../../etc/passwd".
  return ensureWithin(base, resolved);
}
