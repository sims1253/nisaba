/**
 * Required-placeholder / schema preservation validation.
 *
 * Template placeholders (`<<…>>` tokens and
 * `<<…>>` placeholders, content controls)
 * MUST survive every transformation. This component is the enforcement point.
 *
 * Two modes:
 *   - against a Typst source: each required token must occur at least once;
 *   - against another manifest: token sets are diffed (added/removed) — this is
 *     the "re-derive from fresh DOCX and diff" check.
 *
 * Output is a deterministic {@link SchemaValidationReport}.
 */
import { TOOL_NAME, VERSION } from "../version.js";
import { hashText } from "../json.js";
import type { Manifest } from "../docx/manifest.js";

export type TokenStatus = "present" | "absent";

export interface TokenCheck {
  readonly token: string;
  readonly status: TokenStatus;
  readonly occurrences: number;
}

export interface SchemaValidationReport {
  readonly schemaVersion: "1";
  readonly generatedBy: string;
  readonly manifest: { readonly sha256: string; readonly requiredCount: number };
  readonly against: {
    readonly kind: "typst" | "manifest";
    readonly path: string;
    readonly sha256?: string;
  };
  readonly checks: readonly TokenCheck[];
  readonly missing: readonly string[];
  readonly extra: readonly string[]; // tokens in target manifest but not required (manifest mode)
  readonly passed: boolean;
}

/** Count case-sensitive occurrences of `token` inside `text`. */
export function countOccurrences(text: string, token: string): number {
  if (!token) return 0;
  let count = 0;
  let i = 0;
  while ((i = text.indexOf(token, i)) !== -1) {
    count++;
    i += token.length;
  }
  return count;
}

/**
 * Validate that every required placeholder of `manifest` survives in a Typst
 * source string.
 */
export function validateTypstSource(
  manifest: Manifest,
  source: string,
  sourcePath: string,
): SchemaValidationReport {
  const checks: TokenCheck[] = manifest.requiredPlaceholders.map((token) => {
    // Accept both the raw token and the bracketed <<token>> form.
    const occ =
      countOccurrences(source, token) +
      countOccurrences(source, `<<${token}>>`);
    return { token, status: occ > 0 ? "present" as const : "absent" as const, occurrences: occ };
  });
  const missing = checks.filter((c) => c.status === "absent").map((c) => c.token);
  return {
    schemaVersion: "1",
    generatedBy: `${TOOL_NAME}@${VERSION}`,
    manifest: { sha256: hashText(JSON.stringify(manifest)), requiredCount: manifest.requiredPlaceholders.length },
    against: { kind: "typst", path: sourcePath, sha256: hashText(source) },
    checks,
    missing,
    extra: [],
    passed: missing.length === 0,
  };
}

/**
 * Diff the required-placeholder sets of two manifests. Used to check that a
 * re-derived template preserves the schema of the original (refresh check).
 */
export function validateAgainstManifest(
  required: Manifest,
  candidate: Manifest,
  candidatePath: string,
): SchemaValidationReport {
  const req = new Set(required.requiredPlaceholders);
  const cand = new Set(candidate.requiredPlaceholders);
  const checks: TokenCheck[] = required.requiredPlaceholders.map((token) => ({
    token,
    status: cand.has(token) ? ("present" as const) : ("absent" as const),
    occurrences: cand.has(token) ? 1 : 0,
  }));
  const missing = Array.from(req).filter((t) => !cand.has(t)).sort();
  const extra = Array.from(cand).filter((t) => !req.has(t)).sort();
  return {
    schemaVersion: "1",
    generatedBy: `${TOOL_NAME}@${VERSION}`,
    manifest: { sha256: hashText(JSON.stringify(required)), requiredCount: req.size },
    against: { kind: "manifest", path: candidatePath, sha256: hashText(JSON.stringify(candidate)) },
    checks,
    missing,
    extra,
    passed: missing.length === 0,
  };
}
