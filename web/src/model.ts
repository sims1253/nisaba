/**
 * Typst source analysis for the hybrid editor.
 *
 * Projection (baseline / proposed / redline / public) deliberately does **not**
 * live here. It is applied by the app service before compiling, using
 * `crates/nisaba-core`'s projection and redline logic, so there is exactly one
 * implementation of those rules. A second, simpler copy in the browser would
 * drift from it and quietly disagree about what a reviewer sees.
 */
export type ConstructKind = "strong" | "emphasis" | "heading" | "list" | "math" | "link" | "citation" | "figure" | "table" | "template" | "raw"
export interface Construct { readonly from: number; readonly to: number; readonly kind: ConstructKind; readonly label?: string; readonly name?: string }

const syntaxPatterns: readonly [ConstructKind, RegExp][] = [
  // `*…*` is strong only at a word boundary: the opening `*` must be preceded
  // by start or whitespace and immediately followed by a non-space character,
  // so spaced multiplication like `2 * 3 * 4` (or a lone `[a * b * c]`) is not
  // bolded. The leading `(^|\s)` is captured so the construct range can skip
  // it; the trailing lookahead keeps the span from running into following text.
  ["strong", /(^|\s)(\*[^*\s][^*\n]*\*)(?=$|\s|\p{P})/gu],
  // `_…_` is emphasis only when both delimiters sit at a word boundary — the
  // opening `_` is preceded by start or a non-word char and the closing `_` is
  // followed by one — so snake_case identifiers (`my_variable_name`) are not
  // italicised. The opening `_` must also be followed by a non-space character.
  ["emphasis", /(^|[^\w])(_[^_\s][^_\n]*_)(?=$|[^\w])/gu],
  ["heading", /^=+ .+$/gm],
  // List items may be indented (Typst nests lists with leading whitespace), so
  // the marker is matched after optional leading spaces/tabs. The leading
  // whitespace is captured as group 1 so the construct `from` can stay at the
  // line start (CodeMirror line decorations must point there) while the label
  // (group 2) begins at the `- `/`+ ` marker — `label[0]` reliably reports the
  // marker character even for indented items.
  ["list", /^([ \t]*)([-+] .+)$/gm],
  // `$…$` is math only when neither delimiter is adjacent to a digit, so plain
  // currency like `$5 and $10` is not rendered in math styling.
  ["math", /(^|\D)(\$[^$\n]+\$)(?=\D|$)/gu],
  ["link", /https?:\/\/[^\s\]]+/g],
]

/**
 * Detects `#cite(…)`, `#figure(…)`, and `#table(…)` function calls so they can
 * be rendered as **in-place rich-text marks** (not collapsed chips).
 *
 * Unlike the old chipping approach, which replaced the full call with a button
 * and blocked editing inside it, a mark decoration styles the source text in
 * place while keeping it fully visible and editable — the same hybrid model used
 * for `*bold*` and `_italic_`. The caller's cursor entering the construct's
 * range is irrelevant because the text is never hidden.
 *
 * The parser finds a balanced `#fn(…)` call on a single line. Multi-line calls
 * (common for large tables) are detected by scanning forward for the matching
 * close paren once the opening is found, so they are styled correctly too.
 */
function findFunctionConstructs(source: string): Construct[] {
  const constructs: Construct[] = []
  // Match `#cite`, `#figure`, or `#table` followed by an optional space and `(`.
  const re = /#(cite|figure|table)\s*\(/g
  for (const match of source.matchAll(re)) {
    const kind: ConstructKind = match[1] === "cite" ? "citation" : match[1] as ConstructKind
    const openParen = (match.index ?? 0) + match[0].length - 1
    // Walk forward from the opening paren to find the balanced close. Parens
    // nested inside Typst content blocks `[...]` and string literals `"..."` are
    // skipped, otherwise a caption like `[result f(x)]`, a smiley `[a :)]`, or
    // an image path `"a)b"` would prematurely close the call and truncate the
    // construct (dropping its trailing `])`). Brackets are tracked separately;
    // strings are tracked everywhere so a `]`/`(`/`)` inside one never derails
    // the walk.
    let depth = 0
    let bracketDepth = 0
    let inString = false
    let end = -1
    for (let i = openParen; i < source.length; i++) {
      const ch = source[i]
      if (inString) {
        if (ch === "\\") i++ // skip escaped char (e.g. \" inside a string)
        else if (ch === '"') inString = false
        continue
      }
      if (ch === '"') { inString = true; continue }
      if (ch === "[") { bracketDepth++; continue }
      if (ch === "]") { if (bracketDepth > 0) bracketDepth--; continue }
      if (bracketDepth > 0) continue // parens inside a content block don't count
      if (ch === "(") depth++
      else if (ch === ")") {
        depth--
        if (depth === 0) { end = i + 1; break }
      }
    }
    if (end === -1) continue // unbalanced; ignore rather than guess
    const from = match.index ?? 0
    // Store the full source text so the rich widget can parse it for display.
    constructs.push({ from, to: end, kind, name: kind, label: source.slice(from, end) })
  }
  return constructs
}

// Accepted for call-site compatibility but no longer selects anything for chipping.
const noFunctions = new Set<string>()

// Single-entry memo. `findConstructs` is a pure function of `source`, but it is
// invoked more than once per edit: the hybrid-editor StateField re-parses on
// every doc-change transaction, and main.ts's selection listener parses again
// to refresh its own cursor-reveal cache. A one-slot cache keyed on the source
// string makes those redundant calls O(1). The returned array is shared across
// callers and must be treated as read-only.
let findConstructsCacheKey: string | null = null
let findConstructsCacheValue: Construct[] = []

function findConstructsImpl(source: string): Construct[] {
  const constructs: Construct[] = []
  for (const [kind, pattern] of syntaxPatterns) for (const match of source.matchAll(pattern)) {
    const start = match.index ?? 0
    if (kind === "list") {
      // `from` stays at the line start (line decorations must point there) and
      // the label is the marker+text (group 2), so `label[0]` is the `- `/`+ `
      // marker even for indented items. The leading whitespace (group 1) is
      // re-derived by the decoration layer to place the marker widget.
      constructs.push({ from: start, to: start + match[0].length, kind, label: match[2] ?? match[0] })
      continue
    }
    // Inline patterns (strong/emphasis/math) carry a leading capture group for
    // the boundary character that lets the delimited span start only at a word
    // boundary. `match[1]` is that boundary (the empty string at start-of-text)
    // and `match[2]` is the real `*…*` / `_…_` / `$…$` span, so the construct
    // range skips the consumed boundary. Patterns without groups behave as before.
    const boundary = match[1] ?? ""
    const span = match[2]
    constructs.push({ from: start + boundary.length, to: start + match[0].length, kind, label: span ?? match[0] })
  }
  // Rich-text marks for citations, figures, and tables.
  for (const c of findFunctionConstructs(source)) constructs.push(c)
  return constructs.sort((a, b) => a.from - b.from || b.to - a.to)
}

export function findConstructs(source: string, _functions: ReadonlySet<string> = noFunctions): Construct[] {
  void _functions
  if (source === findConstructsCacheKey) return findConstructsCacheValue
  const result = findConstructsImpl(source)
  findConstructsCacheKey = source
  findConstructsCacheValue = result
  return result
}
