import { StateField, StateEffect, RangeSet, type Range } from "@codemirror/state"
import { Decoration, EditorView, WidgetType, type DecorationSet } from "@codemirror/view"
import { findConstructs, type Construct } from "./model"
import type { ReviewItem } from "./review"

export const revealConstruct = StateEffect.define<{ readonly from: number; readonly to: number }>()

/**
 * Reference metadata for citation display. Passed from main.ts so the
 * CitationWidget can render "Author et al. (Year)" instead of a bare key.
 */
export interface ReferenceDisplay {
  readonly id: string
  readonly authors: readonly string[]
  readonly year: number | null
  readonly title: string
}

// ---------------------------------------------------------------------------
// Construct source parsing (extracts display data from Typst markup)
// ---------------------------------------------------------------------------

/** Extracts citation keys from `#cite(<key1>, <key2>)`. */
function parseCitationKeys(source: string): string[] {
  const matches = source.matchAll(/<([^>]+)>/g)
  return [...matches].map((m) => m[1] ?? "")
}

/**
 * Extracts the caption text from `#figure(..., caption: [text])`.
 *
 * Uses balanced bracket matching so a caption containing nested content
 * blocks like `caption: [See [@key] for details]` is captured in full
 * rather than truncated at the first inner `]`.
 */
function parseFigureCaption(source: string): string | null {
  const m = source.match(/caption:\s*\[/)
  if (!m || m.index === undefined) return null
  return extractBracketContent(source, m.index + m[0].length - 1)
}

/**
 * Returns the text between a `[` at `openPos` and its balanced `]`,
 * or null if unbalanced. The delimiters themselves are excluded.
 */
function extractBracketContent(source: string, openPos: number): string | null {
  if (source[openPos] !== "[") return null
  let depth = 0
  for (let i = openPos; i < source.length; i++) {
    if (source[i] === "[") depth++
    else if (source[i] === "]") {
      depth--
      if (depth === 0) return source.slice(openPos + 1, i)
    }
  }
  return null // unbalanced
}

/** Extracts dimensions hint from `#figure(image("path", width: ...), ...)`. */
function parseFigureImageHint(source: string): string | null {
  const m = source.match(/image\(["']([^"']+)["']/)
  return m ? (m[1] ?? "") : null
}

/**
 * Extracts a rough table grid from `#table(columns: n, ..cells)`.
 *
 * Parses content blocks `[cell]` and positional args after `columns:` to build
 * a grid preview. This is intentionally approximate — the purpose is to give a
 * visual cue, not a pixel-perfect table render (the PDF preview is the fidelity
 * authority).
 */
/** Exported for unit tests. */
export function parseTable(source: string): { columns: number; headers: string[]; rows: string[][] } | null {
  const colMatch = source.match(/columns:\s*(\d+)/)
  // Clamp to at least 1. `columns: 0` is syntactically valid Typst but would
  // make the row-chunking loop below step by 0 and spin forever, hanging the
  // editor the moment such a table is rendered.
  const columns = colMatch ? Math.max(1, Number(colMatch[1])) : 2
  // Extract content blocks [cell] using balanced bracket matching so cells
  // that themselves contain brackets (e.g. [#strong[bold]]) are captured in
  // full rather than truncated at the first inner `]`.
  const cells: string[] = []
  for (let i = 0; i < source.length; i++) {
    if (source[i] !== "[") continue
    const content = extractBracketContent(source, i)
    if (content === null) continue
    cells.push(content.trim())
    // Skip past this cell's closing `]` (open + 1 + content length) so a content
    // block nested inside the cell — e.g. `[#strong[bold]]` or `[#emph[x]]` — is
    // not also extracted as a phantom extra cell ("bold" / "x") that corrupts the
    // column grid.
    i += content.length + 1
  }
  if (cells.length === 0) return null
  const headers = cells.slice(0, columns)
  const bodyCells = cells.slice(columns)
  const rows: string[][] = []
  for (let i = 0; i < bodyCells.length; i += columns) {
    rows.push(bodyCells.slice(i, i + columns))
  }
  return { columns, headers, rows }
}

/**
 * Renders a citation key as a human-readable label: "Author et al. (Year)"
 * or "Author (Year)" or falls back to a shortened key.
 */
function citationLabel(key: string, references: readonly ReferenceDisplay[]): string {
  const ref = references.find((r) => r.id === key)
  if (!ref) return key.length > 12 ? key.slice(0, 8) + "…" : key
  const firstAuthor = ref.authors[0]
  const yearStr = ref.year ? ` (${ref.year})` : ""
  if (!firstAuthor) return ref.title.length > 30 ? ref.title.slice(0, 27) + "…" : ref.title + yearStr
  const surname = firstAuthor.includes(" ") ? firstAuthor.split(" ").slice(-1)[0] : firstAuthor
  const authorStr = ref.authors.length > 1 ? `${surname} et al.` : surname
  return `${authorStr}${yearStr}`
}

// ---------------------------------------------------------------------------
// Rich construct widgets
// ---------------------------------------------------------------------------

class CitationWidget extends WidgetType {
  constructor(
    private readonly construct: Construct,
    private readonly onOpen: (construct: Construct) => void,
    private readonly references: readonly ReferenceDisplay[]
  ) { super() }

  override toDOM(): HTMLElement {
    const keys = parseCitationKeys(this.construct.label ?? "")
    const span = document.createElement("span")
    span.className = "rich-citation"
    span.title = "Click to edit · move cursor here to see source"
    span.textContent = keys.length === 0
      ? "[?]"
      : keys.map((k) => citationLabel(k, this.references)).join("; ")
    span.addEventListener("click", () => this.onOpen(this.construct))
    return span
  }
  override eq(other: CitationWidget): boolean {
    // The rendered label ("Author et al. (Year)") depends on `references`, not
    // just the source span. Comparing by identity forces a re-render whenever
    // the field is rebuilt with a fresh references array (main.ts maps a new
    // array on every update), so an edited bibliography is reflected as soon as
    // any transaction recomputes the decorations; without this a changed author
    // or year would leave a stale chip.
    return other.construct.from === this.construct.from && other.construct.to === this.construct.to
      && other.construct.label === this.construct.label
      && other.references === this.references
  }
  override ignoreEvent(): boolean { return false }
}

class FigureWidget extends WidgetType {
  constructor(
    private readonly construct: Construct,
    private readonly onOpen: (construct: Construct) => void
  ) { super() }

  override toDOM(): HTMLElement {
    const caption = parseFigureCaption(this.construct.label ?? "")
    const imgHint = parseFigureImageHint(this.construct.label ?? "")
    const figure = document.createElement("figure")
    figure.className = "rich-figure"
    figure.title = "Click to edit · move cursor here to see source"

    const placeholder = document.createElement("div")
    placeholder.className = "rich-figure-body"
    if (imgHint) {
      const filename = imgHint.split("/").pop() ?? imgHint
      placeholder.innerHTML = `<span class="rich-figure-icon">\u{1F5BC}</span><span>${escapeText(filename)}</span>`
    } else {
      placeholder.innerHTML = `<span class="rich-figure-icon">\u{1F4CA}</span><span>Figure</span>`
    }
    figure.append(placeholder)

    if (caption) {
      const cap = document.createElement("figcaption")
      cap.className = "rich-figure-caption"
      cap.textContent = caption
      figure.append(cap)
    }
    figure.addEventListener("click", () => this.onOpen(this.construct))
    return figure
  }
  override eq(other: FigureWidget): boolean {
    return other.construct.from === this.construct.from && other.construct.to === this.construct.to
      && other.construct.label === this.construct.label
  }
  override ignoreEvent(): boolean { return false }
}

class TableWidget extends WidgetType {
  constructor(
    private readonly construct: Construct,
    private readonly onOpen: (construct: Construct) => void
  ) { super() }

  override toDOM(): HTMLElement {
    const table = document.createElement("table")
    table.className = "rich-table"
    table.title = "Click to edit · move cursor here to see source"

    const parsed = parseTable(this.construct.label ?? "")
    if (parsed) {
      const thead = table.createTHead()
      const headerRow = thead.insertRow()
      for (const h of parsed.headers) {
        const th = document.createElement("th")
        th.textContent = h
        headerRow.append(th)
      }
      const tbody = table.createTBody()
      for (const row of parsed.rows) {
        const tr = tbody.insertRow()
        for (const cell of row) {
          const td = tr.insertCell()
          td.textContent = cell
        }
      }
    } else {
      const row = table.insertRow()
      const cell = row.insertCell()
      cell.textContent = "Table"
    }
    table.addEventListener("click", () => this.onOpen(this.construct))
    return table
  }
  override eq(other: TableWidget): boolean {
    return other.construct.from === this.construct.from && other.construct.to === this.construct.to
      && other.construct.label === this.construct.label
  }
  override ignoreEvent(): boolean { return false }
}

function escapeText(text: string): string {
  return text.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;")
}

// ---------------------------------------------------------------------------
// Hybrid editor decorations
// ---------------------------------------------------------------------------

/** Constructs that get a rich visual widget (replaced in the editor). */
const richKinds = new Set(["citation", "figure", "table"])
/** Constructs styled in-place as marks (bold, italic, headings). */
const markKinds = new Set(["strong", "emphasis", "heading"])

/**
 * Filters out constructs whose replace decoration would overlap the
 * full-range replace of an enclosing **rich-kind** construct, so that
 * `#figure(table(...))` or a list line inside a figure does not produce
 * conflicting decorations.
 *
 * Mark kinds (`strong`, `emphasis`, `heading`) produce *mark* decorations,
 * which CodeMirror allows to overlap with anything — so they are never
 * filtered. This means `*bold*` inside a list item (`- *bold text*`) keeps
 * its bold styling, and `#cite(...)` inside a list line keeps its citation
 * chip, because the enclosing list construct only replaces its 2-char marker
 * prefix, not the rich construct's range.
 */
function deduplicateNested(constructs: readonly Construct[]): Construct[] {
  return constructs.filter((c) => {
    // Marks can safely overlap with any decoration — never remove them.
    if (markKinds.has(c.kind)) return true
    // For replace-producing constructs (rich kinds, list markers), remove any
    // that are strictly contained within a rich-kind construct whose
    // full-range replace decoration would conflict.
    return !constructs.some((other) =>
      other !== c &&
      richKinds.has(other.kind) &&
      other.from <= c.from &&
      other.to >= c.to &&
      (other.from < c.from || other.to > c.to)
    )
  })
}

/**
 * Computes sequential 1-based numbering for consecutive ordered (`+`) list
 * items, resetting the counter when the marker type changes or when a
 * non-whitespace gap (paragraph, heading, …) appears between items.
 *
 * The `source` text is used to inspect the characters between two list
 * constructs: if only whitespace separates them the items belong to the same
 * list (Typst allows blank lines inside a list); any other content breaks it.
 */
/** Exported for unit tests. */
export function computeOrderedListIndices(
  constructs: readonly Construct[],
  source: string
): Map<number, number> {
  const indices = new Map<number, number>()
  // Per-indentation-level running counters. Typst renders each indentation
  // level of an ordered list as its own 1-based sequence, so going deeper starts
  // at 1, staying on the same level increments, and returning to a shallower
  // level resumes that level's previous counter. Without this, a nested list
  // such as `+ a` / `  + b` / `+ c` would number 1, 2, 3 instead of 1, 1, 2.
  const counters = new Map<number, number>()
  let prevLevel = -1
  let prevOrdered = false
  let prevEnd = -1
  for (const c of constructs) {
    if (c.kind !== "list") {
      // A mark or other inline construct sitting *inside* a list line
      // (e.g. `*bold*` in `- *bold*`) does not break the list. Only reset when
      // the intervening construct is not contained within any list item.
      const insideListItem = constructs.some(
        (l) => l.kind === "list" && c.from >= l.from && c.to <= l.to
      )
      if (!insideListItem) {
        counters.clear()
        prevLevel = -1
        prevEnd = -1
        prevOrdered = false
      }
      continue
    }
    const isOrdered = c.label?.[0] === "+"
    // Indentation level = number of leading spaces/tabs on the item's line.
    let level = 0
    for (let i = c.from; i < source.length && (source[i] === " " || source[i] === "\t"); i++) level++
    const between = prevEnd >= 0 ? source.slice(prevEnd, c.from) : ""
    const hasGap = !/^\s*$/.test(between)
    if (hasGap || isOrdered !== prevOrdered) {
      counters.clear()
      prevLevel = -1
    }
    if (isOrdered) {
      const next = level > prevLevel ? 1 : (counters.get(level) ?? 0) + 1
      counters.set(level, next)
      indices.set(c.from, next)
    }
    prevLevel = level
    prevOrdered = isOrdered
    prevEnd = c.to
  }
  return indices
}

export function hybridDecorations(
  constructs: readonly Construct[],
  source: string,
  onOpen: (construct: Construct) => void,
  references: readonly ReferenceDisplay[],
  revealed: readonly { from: number; to: number }[] = []
): DecorationSet {
  const visible = deduplicateNested(constructs)
  const orderedIndices = computeOrderedListIndices(visible, source)
  const ranges = visible.flatMap((construct) => {
    // Lists: render the prefix marker (- or +) as a styled bullet using a
    // replace widget on just those two characters. The line text stays
    // visible behind the marker. A line decoration adds indentation.
    if (construct.kind === "list") {
      const markerLen = 2 // "- " or "+ "
      // Indented (nested) list items have leading whitespace before the marker.
      // The line decoration still anchors at the line start (CodeMirror requires
      // line decorations to point at a line start), but the marker replace must
      // cover the `- `/`+ ` itself, not the indentation, so skip the leading
      // spaces/tabs to find the marker.
      let markerStart = construct.from
      while (markerStart < source.length && (source[markerStart] === " " || source[markerStart] === "\t")) markerStart++
      const isOrdered = construct.label?.[0] === "+"
      const index = isOrdered ? (orderedIndices.get(construct.from) ?? 1) : 1
      return [
        Decoration.line({ class: "typst-list-line" }).range(construct.from),
        Decoration.replace({ widget: new ListMarkerWidget(isOrdered ? "ordered" : "unordered", index), inclusive: false }).range(markerStart, markerStart + markerLen),
      ]
    }
    // Stylistic constructs are always rendered as in-place marks.
    if (markKinds.has(construct.kind)) {
      return [Decoration.mark({ class: `typst-${construct.kind}` }).range(construct.from, construct.to)]
    }
    // Rich constructs (citation/figure/table) are replaced with a visual widget
    // UNLESS the cursor has entered their range, in which case the raw source is
    // revealed so the user can edit it.
    if (richKinds.has(construct.kind)) {
      if (revealed.some((item) => item.from === construct.from && item.to === construct.to)) return []
      const widget = construct.kind === "citation"
        ? new CitationWidget(construct, onOpen, references)
        : construct.kind === "figure"
          ? new FigureWidget(construct, onOpen)
          : new TableWidget(construct, onOpen)
      return [Decoration.replace({ widget, inclusive: false }).range(construct.from, construct.to)]
    }
    return []
  })
  return RangeSet.of(ranges.sort((a, b) => a.from - b.from))
}

/** Tiny widget that renders a list bullet or number in place of `- `/`+ `. */
class ListMarkerWidget extends WidgetType {
  constructor(
    private readonly variant: "ordered" | "unordered",
    private readonly index = 1
  ) { super() }
  override toDOM(): HTMLElement {
    const el = document.createElement("span")
    el.className = `typst-list-marker typst-list-marker-${this.variant}`
    el.textContent = this.variant === "ordered" ? `${this.index}.` : "\u2022"
    return el
  }
  override eq(other: ListMarkerWidget): boolean {
    return other.variant === this.variant && other.index === this.index
  }
  override ignoreEvent(): boolean { return true }
}

/**
 * Carries the hybrid editor's decoration set together with the set of
 * construct ranges the user has "revealed" (cursor-entered) so that editing
 * inside a revealed construct does not cause a one-frame flicker where the
 * widget reappears before the selection listener re-dispatches `revealConstruct`.
 */
interface HybridEditorValue {
  readonly decorations: DecorationSet
  readonly revealed: readonly { from: number; to: number }[]
}

export function hybridEditorField(
  onOpen: (construct: Construct) => void,
  references: () => readonly ReferenceDisplay[]
): StateField<HybridEditorValue> {
  return StateField.define<HybridEditorValue>({
    create: (state) => {
      const source = state.doc.toString()
      return { decorations: hybridDecorations(findConstructs(source), source, onOpen, references()), revealed: [] }
    },
    update: (value, transaction) => {
      if (!transaction.docChanged && transaction.effects.length === 0) return value
      const revealEffects = transaction.effects.filter((effect) => effect.is(revealConstruct))
      // When a revealConstruct effect is present it fully replaces the
      // revealed set. Otherwise carry forward the previous set, mapping
      // positions through the document change so the widget stays hidden
      // while the user edits inside the construct.
      const revealed: readonly { from: number; to: number }[] = revealEffects.length > 0
        ? revealEffects.map((effect) => ({ ...effect.value }))
        : value.revealed.map((r) => ({ from: transaction.changes.mapPos(r.from), to: transaction.changes.mapPos(r.to) }))
      const source = transaction.state.doc.toString()
      return { decorations: hybridDecorations(findConstructs(source), source, onOpen, references(), revealed), revealed }
    },
    provide: (field) => [
      EditorView.decorations.from(field, (value) => value.decorations),
      EditorView.atomicRanges.of((view) => {
        const decorations = view.state.field(field).decorations
        const replaceStamps: Range<Decoration>[] = []
        decorations.between(0, view.state.doc.length, (from, to, value) => {
          if (value.spec.widget && !(value.spec.widget instanceof ListMarkerWidget)) {
            replaceStamps.push(value.range(from, to))
          }
        })
        return RangeSet.of(replaceStamps)
      }),
    ]
  })
}

/** Replaces the review items the editor highlights. */
export const setReviewItems = StateEffect.define<readonly ReviewItem[]>()

export type ReviewOpener = (id: string, anchor: HTMLElement) => void

class CommentAnchorWidget extends WidgetType {
  constructor(private readonly id: string, private readonly onOpen: ReviewOpener) { super() }
  override toDOM(): HTMLElement {
    const marker = document.createElement("button")
    marker.type = "button"
    marker.className = "review-comment-anchor"
    marker.textContent = "\u25CF"
    marker.title = "Open comment"
    marker.setAttribute("data-review-id", this.id)
    marker.setAttribute("aria-label", "Open comment thread")
    marker.addEventListener("click", (event) => {
      event.stopPropagation()
      this.onOpen(this.id, marker)
    })
    return marker
  }
  override eq(other: CommentAnchorWidget): boolean { return other.id === this.id }
  override ignoreEvent(): boolean { return false }
}

export const reviewDecorations = (items: readonly ReviewItem[], length: number, onOpen: ReviewOpener): DecorationSet => {
  const ranges = items
    .filter((item) => item.status === "open" && !item.orphaned && item.from >= 0 && item.to <= length)
    .flatMap((item) => {
      if (item.kind === "suggestion") {
        return item.to > item.from ? [Decoration.mark({ class: "review-suggestion", attributes: { "data-review-id": item.id } }).range(item.from, item.to)] : []
      }
      return item.to > item.from
        ? [Decoration.mark({ class: "review-comment", attributes: { "data-review-id": item.id } }).range(item.from, item.to)]
        : [Decoration.widget({ widget: new CommentAnchorWidget(item.id, onOpen), side: 1 }).range(item.from)]
    })
    .sort((a, b) => a.from - b.from)
  return RangeSet.of(ranges)
}

export function reviewEditorField(onOpen: ReviewOpener): StateField<DecorationSet> {
  return StateField.define({
    create: () => RangeSet.empty as DecorationSet,
    update: (decorations, transaction) => {
      const replacement = transaction.effects.find((effect) => effect.is(setReviewItems))
      if (replacement) return reviewDecorations(replacement.value, transaction.state.doc.length, onOpen)
      return transaction.docChanged ? decorations.map(transaction.changes) : decorations
    },
    provide: (field) => EditorView.decorations.from(field)
  })
}