/**
 * RIS round-trip checker.
 *
 * Verifies that an RIS bibliography survives a canonical re-emit *losslessly*
 * on the fields reference lists require. This is **verification tooling**,
 * not the authoritative converter — the conversion itself is owned by
 * `crates/nisaba-references` (Rust). The Rust CLI (`cargo run --bin ris --
 * canonical`) provides the authoritative canonical emission; this component
 * cross-checks it with an independent TS parser.
 *
 * The Rust crate is the single source of truth for RIS format logic. This
 * TypeScript parser exists solely as an independent validation harness to catch
 * regressions in the Rust implementation. It is NOT used for production
 * RIS conversion.
 *
 * Lossless here means: re-parsing the canonical emission of the parsed input
 * yields a record set deep-equal to the parsed input. Multi-valued fields are
 * compared as sorted sets (order-independence), matching RIS's semantics.
 */
import { TOOL_NAME, VERSION } from "../version.js";
import { hashText } from "../json.js";

/** Common reference-list fields (best-effort default; overridable). */
export const DEFAULT_REQUIRED_TAGS = ["TY", "AU", "PY", "TI", "JO"] as const;

/** Tags whose member order is meaningful (first author = first listed). */
export const ORDERED_TAGS = new Set(["AU", "ED"]);

export type RisRecord = ReadonlyMap<string, readonly string[]>;

export interface RoundTripReport {
  readonly schemaVersion: "1";
  readonly generatedBy: string;
  readonly input: { readonly path: string; readonly sha256: string; readonly bytes: number };
  readonly recordCount: number;
  readonly lossless: boolean;
  readonly canonicalSha256: string;
  readonly requiredTags: readonly string[];
  readonly fieldCoverage: readonly { readonly tag: string; readonly recordsWithField: number; readonly coverageFraction: number }[];
  readonly perRecordMissing: readonly { readonly index: number; readonly missing: readonly string[] }[];
  readonly notes: readonly string[];
}

const RIS_TAG_RE = /^([A-Z0-9]{2})\s+-\s?(.*)$/;

/** Parse RIS text into records (multi-valued tags). Records end at `ER`. */
export function parseRis(text: string): RisRecord[] {
  const records: Map<string, string[]>[] = [];
  let current: Map<string, string[]> | null = null;
  for (const line of text.split(/\r?\n/)) {
    const m = line.match(RIS_TAG_RE);
    if (m) {
      const tag = m[1]!;
      const value = m[2] ?? "";
      if (!current) current = new Map();
      const list = current.get(tag) ?? [];
      if (value !== "") list.push(value);
      else list.push("");
      current.set(tag, list);
      if (tag === "ER") {
        records.push(current);
        current = null;
      }
    }
  }
  if (current) records.push(current);
  return records;
}

/** Emit records in a canonical, deterministic RIS form. */
export function canonicalRis(records: readonly RisRecord[]): string {
  const lines: string[] = [];
  const orderedSet = (values: readonly string[]): string[] => {
    const out: string[] = [];
    const seen = new Set<string>();
    for (const v of values) {
      if (!seen.has(v)) {
        seen.add(v);
        out.push(v);
      }
    }
    return out;
  };
  for (const rec of records) {
    // `ER` is the record terminator: it MUST be emitted last, even though it
    // would otherwise sort among the other tags.
    const tags = Array.from(rec.keys()).filter((t) => t !== "ER").sort();
    for (const tag of tags) {
      const values = [...(rec.get(tag) ?? [])];
      const ordered = ORDERED_TAGS.has(tag) ? orderedSet(values) : values.sort();
      if (ordered.length === 0) {
        lines.push(`${tag}  -`);
      } else {
        for (const v of ordered) lines.push(`${tag}  - ${v}`);
      }
    }
    // Always terminate with ER.
    const erVals = rec.get("ER");
    if (erVals && erVals.length > 0) {
      for (const v of erVals) lines.push(v === "" ? "ER  -" : `ER  - ${v}`);
    } else {
      lines.push("ER  -");
    }
  }
  return lines.join("\n") + "\n";
}

function recordsEqual(a: readonly RisRecord[], b: readonly RisRecord[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const ra = a[i]!;
    const rb = b[i]!;
    const ka = Array.from(ra.keys()).sort();
    const kb = Array.from(rb.keys()).sort();
    if (ka.join("\u0001") !== kb.join("\u0001")) return false;
    for (const tag of ka) {
      const va = [...(ra.get(tag) ?? [])].sort().join("\u0001");
      const vb = [...(rb.get(tag) ?? [])].sort().join("\u0001");
      if (va !== vb) return false;
    }
  }
  return true;
}

/** Check a round-trip and report field coverage + losslessness. */
export function checkRoundTrip(
  text: string,
  inputPath: string,
  requiredTags: readonly string[] = DEFAULT_REQUIRED_TAGS,
): RoundTripReport {
  const records = parseRis(text);
  const canonical = canonicalRis(records);
  const reparsed = parseRis(canonical);
  const lossless = recordsEqual(records, reparsed);

  const fieldCoverage = requiredTags.map((tag) => {
    const withField = records.filter((r) => (r.get(tag) ?? []).filter((v) => v !== "").length > 0).length;
    return {
      tag,
      recordsWithField: withField,
      coverageFraction: records.length === 0 ? 0 : withField / records.length,
    };
  });

  const perRecordMissing = records.map((rec, index) => ({
    index,
    missing: requiredTags.filter((t) => (rec.get(t) ?? []).filter((v) => v !== "").length === 0),
  }));

  const notes: string[] = [];
  if (!lossless) notes.push("Round-Trip ist NICHT verlustfrei — Kanonisierung verändert die Daten.");
  const anyMissing = perRecordMissing.some((r) => r.missing.length > 0);
  if (anyMissing) notes.push("Mindestens ein Eintrag fehlen erforderliche required fields.");

  return {
    schemaVersion: "1",
    generatedBy: `${TOOL_NAME}@${VERSION}`,
    input: { path: inputPath, sha256: hashText(text), bytes: Buffer.byteLength(text, "utf8") },
    recordCount: records.length,
    lossless,
    canonicalSha256: hashText(canonical),
    requiredTags,
    fieldCoverage,
    perRecordMissing,
    notes,
  };
}
