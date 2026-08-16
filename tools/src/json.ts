/**
 * Deterministic JSON serialisation and hashing.
 *
 * "Deterministic" here means: the same logical value always produces byte-identical
 * output, regardless of key insertion order or which platform produced it. This is
 * what makes golden files and CI diffs trustworthy — generated Typst must be
 * deterministic or every unrelated compile produces spurious diffs.
 *
 * Rules implemented:
 *   - Object keys are sorted with a plain code-unit comparison (stable across
 *     platforms, no locale dependence).
 *   - `undefined` fields are dropped; `null` is kept.
 *   - `Date` → ISO 8601 (UTC).
 *   - `Map` → sorted-key object; `Set` → sorted array.
 *   - Arrays keep their order (order is meaningful for content).
 *   - No trailing whitespace, 2-space indent for human readability in files.
 */
import { createHash } from "node:crypto";

/** Convert a value into a canonical, key-sorted clone ready for `JSON.stringify`. */
export function toCanonical(value: unknown, seen: WeakMap<object, unknown> = new WeakMap()): unknown {
  if (value === null || typeof value !== "object") return value;

  // Guard against cycles.
  if (seen.has(value as object)) return seen.get(value as object);

  if (value instanceof Date) return value.toISOString();

  if (value instanceof Map) {
    const obj: Record<string, unknown> = {};
    for (const [k, v] of value) obj[String(k)] = toCanonical(v, seen);
    const sorted: Record<string, unknown> = {};
    for (const k of Object.keys(obj).sort()) sorted[k] = obj[k];
    seen.set(value as object, sorted);
    return sorted;
  }

  if (value instanceof Set) {
    const arr = Array.from(value).map((v) => toCanonical(v, seen));
    const sorted = (arr as { toString(): string }[]).map((v) => String(v)).sort();
    seen.set(value as object, sorted);
    return sorted;
  }

  if (Array.isArray(value)) {
    const arr = value.map((v) => toCanonical(v, seen));
    seen.set(value as object, arr);
    return arr;
  }

  // Plain object: drop undefined, sort keys.
  const out: Record<string, unknown> = {};
  const keys = Object.keys(value as Record<string, unknown>).sort();
  for (const k of keys) {
    const v = (value as Record<string, unknown>)[k];
    if (v !== undefined) out[k] = toCanonical(v, seen);
  }
  seen.set(value as object, out);
  return out;
}

/**
 * Serialise a value to a canonical JSON string.
 * @param indent indent width in spaces (default 2). Use 0 for the most compact form.
 */
export function stableStringify(value: unknown, indent: number | string = 2): string {
  return JSON.stringify(toCanonical(value), null, indent);
}

/** SHA-256 hex digest of raw bytes. */
export function hashBytes(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

/** SHA-256 hex digest of a UTF-8 string. */
export function hashText(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

/** SHA-256 hex digest of the *canonical* form of a value (order-independent). */
export function hashValue(value: unknown): string {
  return hashText(stableStringify(value, 0));
}
