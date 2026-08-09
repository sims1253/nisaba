/**
 * Nisaba workspace shell.
 *
 * The outline mirrors the app's flat project → document model. Selecting a document loads it,
 * connects it to the sync relay, and compiles it on demand.
 *
 * Durability has two independent paths and both are needed:
 *   * sync (CRDT over WebSocket) carries live collaborative edits between peers;
 *   * a debounced `PATCH /projects/{p}/documents/{d}` writes the body to the app, which is what
 *     project export reads.
 */
import { basicSetup } from "codemirror"
import { autocompletion, acceptCompletion, type Completion, type CompletionContext, type CompletionResult, type CompletionSource } from "@codemirror/autocomplete"
import { Annotation, Compartment, EditorState, StateEffect, StateField } from "@codemirror/state"
import { Decoration, EditorView, keymap, placeholder, type DecorationSet, type ViewUpdate } from "@codemirror/view"
import { LoroExtensions } from "loro-codemirror"
import { LoroDoc, LoroText, UndoManager } from "loro-crdt"
import { Effect, Layer } from "effect"
import { findConstructs, type Construct } from "./model"
import { hybridEditorField, revealConstruct, reviewEditorField, setReviewItems, type ReferenceDisplay } from "./decorations"
import { PdfBlobUrlStore, downloadBase64 } from "./effects"
import { connectSync, isImportingRemote, type SyncStatus } from "./sync"
import { VirtualPdfViewer } from "./pdf-viewer"
import * as api from "./api"
import type { CompileView, Fulltext, MarkInput, MembershipRole, NisabaDocument, Project, Reference } from "./api"
import { AuthTokenLive, OidcClient, OidcClientLive, onAuthFailure, readStoredAccessToken, currentUserDisplayName, isOidcCallback, oidcConfigFromEnv, scheduleTokenRefresh } from "./auth"
import { emptyReviewState, reviewReducer, type ReviewItem, type ReviewState } from "./review"
import { createCursorAt, resolveCursor } from "./cursor"
import "./styles.css"

// ---------------------------------------------------------------------------
// Compile diagnostics (mirrors services/compile Diagnostic: severity/message/path/start/end)
// ---------------------------------------------------------------------------

interface CompileDiagnostic {
  readonly severity: string
  readonly message: string
  readonly path?: string | null
  readonly start?: number | null
  readonly end?: number | null
}

const root = document.querySelector<HTMLDivElement>("#app")
if (!root) throw new Error("Application root missing")

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

interface OutlineEntry {
  readonly document: NisabaDocument
}

interface Workspace {
  projects: readonly Project[]
  project?: Project
  outline: readonly OutlineEntry[]
  selected?: OutlineEntry
  document?: NisabaDocument
  references: readonly Reference[]
  fulltexts: ReadonlyMap<string, Fulltext>
  review: ReviewState
  view: CompileView
  signedIn: boolean
  diagnostics: readonly CompileDiagnostic[]
  /** Caller's project-scoped role (owner/author/reviewer/read-only). Undefined
   *  until getMembership resolves on openProject; gates reviewer UX (H1/M4). */
  role?: MembershipRole
}

const state: Workspace = {
  projects: [],
  outline: [],
  references: [],
  fulltexts: new Map(),
  review: emptyReviewState,
  view: "proposed",
  signedIn: readStoredAccessToken() !== undefined,
  diagnostics: []
}

/**
 * The active document's Loro replica.
 *
 * A replica is created fresh per document (never reused across documents): a reused
 * doc carries a stale version vector that makes the relay believe the peer is
 * already current, so `$body` edits are never pushed and a second collaborator
 * joins an empty document. A fresh doc also gives each document a clean, correctly
 * scoped undo stack (seeded with the body under an origin the undo manager
 * excludes, so Ctrl+Z cannot collapse the whole document back to empty).
 *
 * The editor's Loro extensions are rebound to the new replica via a compartment
 * every time a document loads.
 */
const loroCompartment = new Compartment()
/** Controls whether the editor accepts input (disabled for read-only roles). */
const editableComp = new Compartment()
let activeLoro = new LoroDoc()

/**
 * Resolves the Loro text container the editor is bound to.
 *
 * The container key MUST be "text": the sync authority snapshots, exports, and
 * imports the `"text"` container (see services/sync), and `loadIntoEditor` seeds
 * the body into the same key. loro-codemirror defaults to a container named
 * "codemirror"; if the editor read that one, the plugin's initial async sync would
 * compare the seeded CodeMirror doc against an empty "codemirror" container and
 * *blank* the view on every (re)load, and remote imports targeting "text" would be
 * dropped by the plugin's `target !== text.id` guard. Passing this override makes
 * the editor, the seed, and the relay all read and write the same container.
 */
const getTextFromDoc = (doc: LoroDoc): LoroText => doc.getText("text")

/**
 * Diagnostic underline decorations live in their own compartment (not
 * decorations.ts, which owns comment anchors). Diagnostics
 * carry 0-based char offsets from typst; we clamp to the current doc length so a
 * stale range from a previous source never draws past the end.
 */
const diagnosticCompartment = new Compartment()
const setDiagnostics = StateEffect.define<readonly CompileDiagnostic[]>()

function diagnosticDecorations(items: readonly CompileDiagnostic[], length: number): DecorationSet {
  const ranges = items
    .map((item) => {
      if (item.start === null || item.start === undefined) return null
      const end = item.end ?? item.start
      const from = Math.min(item.start, length)
      const to = Math.min(Math.max(end, from), length)
      if (to <= from) return null
      const isError = item.severity !== "warning"
      return Decoration.mark({ class: isError ? "typst-diagnostic-error" : "typst-diagnostic-warning" }).range(from, to)
    })
    .filter((range): range is NonNullable<typeof range> => range !== null)
    .sort((a, b) => a.from - b.from)
  return Decoration.set(ranges, true)
}

function diagnosticField(): StateField<DecorationSet> {
  return StateField.define({
    create: () => Decoration.none,
    update: (decorations, transaction) => {
      const replacement = transaction.effects.find((effect) => effect.is(setDiagnostics))
      if (replacement) return diagnosticDecorations(replacement.value, transaction.state.doc.length)
      // Remap existing underlines through edits so they stay on the right span
      // until the next compile replaces them wholesale.
      return transaction.docChanged ? decorations.map(transaction.changes) : decorations
    },
    provide: (field) => EditorView.decorations.from(field)
  })
}

/**
 * Accept/Reject are authoritative: the text mutations they perform must never be
 * re-tracked as new suggestions (otherwise "Reject all" with suggesting on churns
 * forever — reject creates a delete, rejecting that creates an insert, …). Two
 * independent guards mark these transactions so `updateReviewItems` skips them:
 *   * `resolveAnnotation` — carried on the dispatch itself;
 *   * `resolvingSuggestions` — a transient flag (mirroring the relay's
 *     `isImportingRemote`) set around the whole resolution, in case the editor
 *     extension re-dispatches the change as a separate transaction that would
 *     not carry the annotation.
 */
const resolveAnnotation = Annotation.define<boolean>()
let resolvingSuggestions = false

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

root.innerHTML = `
  <style>
    /* Diagnostic underlines + list. Lives here rather than styles.css because
       both the decoration class and the list markup are
       introduced by this file. */
    .typst-diagnostic-error{border-bottom:2px wavy #c0392b;background:#fdecea}
    .typst-diagnostic-warning{border-bottom:2px wavy #b8860b;background:#fef6e3}
    .diagnostics-list{border-bottom:2px solid #e8c5c5;background:#fbf5f5;max-height:50%;overflow:auto;padding:8px 19px;font-size:11px;flex-shrink:0}
    .diagnostics-list h3{font-size:10px;letter-spacing:.08em;text-transform:uppercase;color:#9a5b4f;margin:6px 0 8px}
    .diagnostic-item{display:grid;grid-template-columns:auto 1fr;gap:7px;padding:7px 0;border-top:1px solid #f0dcd8;cursor:pointer;color:#5a4038;line-height:1.4}
    .diagnostic-item:hover{background:#f7ecea}
    .diag-sev{font-family:'DM Mono',monospace;font-size:9px;text-transform:uppercase;padding:2px 5px;border-radius:3px;height:fit-content;margin-top:1px}
    .diag-sev-error{background:#c0392b;color:#fff}
    .diag-sev-warning{background:#d9a521;color:#3a2a06}
    .diag-loc{font-family:'DM Mono',monospace;color:#855d52;font-size:10px;margin-top:2px}
    /* Typst inline-construct marks (decorations.ts styles strong/emphasis as
       in-place text, not chips). The mark covers the full delimiter range so
       the markup is visually distinguished from regular text while still
       rendering bold/italic. */
    .typst-strong{font-weight:700}
    .typst-emphasis{font-style:italic}
    .rich-citation{display:inline-flex;align-items:center;gap:2px;font-family:'DM Sans',sans-serif;background:#e9f1fa;color:#37699d;border:1px solid #c5dbf0;border-radius:8px;padding:1px 7px;font-size:11px;font-weight:600;cursor:pointer;vertical-align:baseline;line-height:1.4;white-space:nowrap}
    .rich-citation:hover{background:#d4e6f7;border-color:#a8cee8}
    .rich-figure{margin:8px 0;border:1px solid #e3d5bd;border-radius:6px;overflow:hidden;cursor:pointer;max-width:100%;background:#fdfaf3}
    .rich-figure:hover{border-color:#cf9c4d;box-shadow:0 1px 5px #cf9c4d33}
    .rich-figure-body{display:flex;align-items:center;justify-content:center;gap:7px;min-height:48px;padding:10px 14px;background:#f5ece0;color:#7a5a32;font:11px 'DM Sans',sans-serif}
    .rich-figure-icon{font-size:16px}
    .rich-figure-caption{font:italic 11px 'DM Sans',sans-serif;color:#72664d;padding:5px 12px;border-top:1px solid #e3d5bd;background:#fdfaf3}
    .rich-table{margin:6px 0;border-collapse:collapse;font:11px 'DM Sans',sans-serif;cursor:pointer;max-width:100%;border:1px solid #ddd}
    .rich-table:hover{box-shadow:0 1px 5px #cf9c4d33;border-color:#cf9c4d}
    .rich-table th{background:#edf3f1;color:#45616a;font-weight:600;padding:4px 10px;border:1px solid #ddd;text-align:left}
    .rich-table td{padding:3px 10px;border:1px solid #eee;color:#555}
  </style>
  <header class="topbar">
    <div class="brand"><span class="mark">N</span><span>Nisaba</span><span class="crumb" id="location-crumb">/ Projects</span></div>
    <div class="top-actions">
      <span class="save-status" id="save-status">Ready</span>
      <button id="sign-in" class="toolbar-button" type="button">Sign in</button>
    </div>
  </header>
  <div class="workspace">
    <aside class="outline" aria-label="Projects and document outline">
      <div class="panel-heading"><h2 id="outline-heading">Projects</h2><div class="panel-heading-actions"><button id="new-project" class="quiet-button" type="button" aria-label="Create project">＋</button><button id="hide-outline" class="hide-button" type="button" title="Hide outline" aria-label="Hide outline">‹</button></div></div>
      <div id="outline-list" class="empty-state"><p>Loading projects…</p></div>
      <div class="outline-footer" id="outline-footer"></div>
    </aside>
    <div class="gutter" data-gutter="outline" title="Drag to resize · double-click to show/hide the outline"></div>
    <section class="editor-pane" aria-label="Typst source editor">
      <div class="pane-toolbar editor-chrome">
        <div class="pane-toolbar-left"><strong id="document-name">No document selected</strong><span class="mode-label" id="revision-label"></span></div>
        <div class="toolbar-actions">
          <label class="view-select"><span class="sr-only">Projection view</span>
            <select id="view-select" aria-label="Projection view">
              <option value="proposed">Proposed</option>
              <option value="baseline">Baseline</option>
              <option value="redline">Redline</option>
              <option value="public">Public</option>
            </select>
          </label>
          <button id="references-button" class="toolbar-button" type="button">References</button>
          <button id="history-button" class="toolbar-button" type="button" title="Version history" hidden>History</button>
          <button id="share-button" class="toolbar-button" type="button" title="Invite collaborators" hidden>Share</button>
          <button id="export-button" class="toolbar-button" type="button">Export</button>
          <button id="toolbar-suggesting" class="toolbar-button" type="button" aria-pressed="false" title="Toggle track changes" disabled>Track changes: off</button>
          <button id="review-button" class="toolbar-button" type="button" aria-pressed="false">Review</button>
          <button id="compile-button" class="primary-button" type="button">Compile <span>⌘↵</span></button>
        </div>
      </div>
      <div class="review-banner editor-chrome" id="review-banner" hidden><span class="review-icon">✓</span><span id="review-summary"></span><button id="suggesting-button" type="button">Track changes: off</button></div>
      <div id="editor" class="editor-host editor-chrome"></div>
      <div class="editor-footer editor-chrome"><span id="sync-label"><span class="green-dot"></span> No document loaded</span><span id="cursor-position">Ln 1, Col 1</span></div>
      <div class="pane-placeholder empty-state" id="editor-placeholder"><h2>No document open</h2><p>Select a document from the outline to start editing.</p></div>
    </section>
    <div class="gutter review-gutter" data-gutter="review" title="Drag to resize the review panel" hidden></div>
    <aside class="review-pane" aria-label="Review threads" hidden>
      <div class="pane-toolbar review-chrome"><div class="pane-toolbar-left"><strong>Review</strong></div><button class="hide-button" id="hide-review" type="button" title="Close review panel" aria-label="Close review panel">×</button></div>
      <div class="review-sidebar-body" id="review-sidebar-body"></div>
    </aside>
    <div class="gutter" data-gutter="preview" title="Drag to resize · double-click to show/hide the preview"></div>
    <section class="preview-pane" aria-label="Compiled PDF preview">
      <div class="pane-toolbar preview-chrome"><div class="pane-toolbar-left"><strong>Preview</strong><span class="build-label" id="build-label">No build</span></div><div class="pdf-zoom-controls" id="pdf-zoom-controls" hidden><button id="zoom-out" class="zoom-button" type="button" title="Zoom out" aria-label="Zoom out">−</button><span class="zoom-level" id="zoom-level">125%</span><button id="zoom-in" class="zoom-button" type="button" title="Zoom in" aria-label="Zoom in">+</button><button id="zoom-reset" class="zoom-button" type="button" title="Reset zoom" aria-label="Reset zoom">⟲</button></div><button id="hide-preview" class="hide-button" type="button" title="Hide preview" aria-label="Hide preview">›</button></div>
      <div id="diagnostics-list" class="diagnostics-list preview-chrome" hidden></div>
      <div id="pdf-viewer" class="pdf-viewer empty-preview preview-chrome"><div class="empty-state"><h2>No preview yet</h2><p>Select a document and compile it to see the rendered PDF.</p></div></div>
      <div class="connection-state preview-chrome" id="connection-state"><span class="connection-dot"></span><span>No document</span></div>
      <div class="pane-placeholder empty-state" id="preview-placeholder"><h2>No preview</h2><p>Open a document and compile it to see the rendered PDF.</p></div>
    </section>
  </div>
  <button id="show-outline-tab" class="show-pane-tab show-pane-tab-left" type="button" title="Show outline" hidden>›</button>
  <button id="show-preview-tab" class="show-pane-tab show-pane-tab-right" type="button" title="Show preview" hidden>‹</button>
  <dialog id="workspace-panel" class="edit-panel"><form method="dialog"><div class="dialog-heading"><div><span class="eyebrow" id="panel-eyebrow">Workspace</span><h2 id="panel-title">Panel</h2></div><button class="close-button" value="cancel" aria-label="Close panel">×</button></div><div id="panel-content"></div></form></dialog>
`

/**
 * Searches the editor for a text snippet and scrolls + highlights the match.
 * Called when the user double-clicks a word in the PDF preview.
 */
function searchEditor(text: string): void {
  const doc = editor.state.doc.toString()
  const idx = doc.indexOf(text)
  if (idx === -1) return
  editor.dispatch({
    selection: { anchor: idx, head: idx + text.length },
    scrollIntoView: true,
    effects: EditorView.scrollIntoView(idx, { y: "center" })
  })
  editor.focus()
}

const pdfViewer = new VirtualPdfViewer(root.querySelector<HTMLElement>("#pdf-viewer")!, {
  onDblClickText: (text) => searchEditor(text)
})
const pdfUrls = new PdfBlobUrlStore()

// ---------------------------------------------------------------------------
// Small DOM helpers
// ---------------------------------------------------------------------------

const el = <T extends HTMLElement>(selector: string): T | null => root.querySelector<T>(selector)
const setText = (selector: string, value: string): void => { const node = el(selector); if (node) node.textContent = value }
const escapeHtml = (value: string): string => {
  // Use the browser's built-in escaping via a temporary element's
  // textContent → innerHTML round-trip. This correctly escapes ALL characters
  // that have special meaning in HTML, including ones the hand-rolled version
  // missed (e.g., single quotes, non-breaking spaces).
  const div = document.createElement("div")
  div.textContent = value
  return div.innerHTML
}

/**
 * Compact relative timestamp ("just now", "2 min ago", "1 h ago", "3 d ago", then a
 * date). Same shape Google Docs/Overleaf use so review cards read naturally. Used for
 * both createdAt ("2 min ago") and resolvedAt ("resolved 1 h ago").
 */
function timeAgo(timestamp: number): string {
  const seconds = Math.floor((Date.now() - timestamp) / 1000)
  if (seconds < 45) return "just now"
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} min ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} h ago`
  const days = Math.floor(hours / 24)
  if (days < 7) return `${days} d ago`
  try { return new Date(timestamp).toLocaleDateString() } catch { return "" }
}

/**
 * Initials (max 2 chars) for the avatar chip. Falls back to "?" for anonymous/empty
 * names so the chip always has a glyph.
 */
function authorInitials(name: string): string {
  const trimmed = name.trim()
  if (!trimmed || trimmed === "anonymous") return "?"
  const parts = trimmed.split(/[\s._-]+/).filter(Boolean)
  if (parts.length === 0) return "?"
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase()
  return (parts[0]![0]! + parts[1]![0]!).toUpperCase()
}

/**
 * Deterministic hue (0-359) for an author, so the same name always gets the same
 * colour across sessions and peers. Hashes the name (djb2) and maps to the hue
 * circle. Two distinct names rarely collide, and when they do the cards are still
 * distinguished by the name text itself.
 */
function authorHue(name: string): number {
  let hash = 5381
  for (let i = 0; i < name.length; i++) hash = ((hash << 5) + hash + name.charCodeAt(i)) | 0
  return Math.abs(hash) % 360
}

function status(message: string): void { setText("#save-status", message) }

// ---------------------------------------------------------------------------
// Workspace columns: drag-resize gutters + hide/show
// ---------------------------------------------------------------------------

/**
 * Per-column widths in pixels. `-1` means "flex" (rendered as 1fr): the editor
 * and preview absorb the leftover space, while the outline and review panel keep
 * a fixed pixel size. The widths persist for the session (no localStorage yet),
 * which is enough for a writing tool where the user mostly keeps one layout.
 */
interface ColumnWidths {
  outline: number
  editor: number
  preview: number
  review: number
}

const columnWidths: ColumnWidths = { outline: 235, editor: -1, preview: -1, review: 320 }

/** Hidden panes (and their gutter) collapse to 0; the editor is always visible. */
interface HiddenPanes { outline: boolean; preview: boolean }

const hiddenPanes: HiddenPanes = { outline: false, preview: false }

/** Whether the review sidebar is docked into the workspace as a 4th column. */
let reviewPaneVisible = false

const GUTTER = 4

const workspaceEl = root.querySelector<HTMLElement>(".workspace")!

/**
 * Writes the grid template from the current widths + hidden/visible flags.
 *
 * Tracks are: outline · gutter · editor · gutter · review · gutter · preview.
 * The review column sits between the editor and the preview (its DOM order in
 * the template matches), so a reviewer reads comments next to the source with the
 * rendered PDF beyond them. A hidden pane and its adjacent gutter both collapse
 * to 0px (and the gutter is also `hidden`), which removes the column from the
 * layout without disturbing the others. The editor and preview use 1fr when their
 * width is -1; fixed panes use `Npx`. Recomputed on every drag move, hide/show
 * toggle, and review toggle so the stylesheet's static `grid-template-columns`
 * is always overridden to match.
 */
function applyWorkspaceGrid(): void {
  const px = (value: number): string => (value === -1 ? "1fr" : `${value}px`)
  const g = (visible: boolean): string => (visible ? `${GUTTER}px` : "0px")
  const outlineW = hiddenPanes.outline ? "0px" : px(columnWidths.outline)
  const outline = el<HTMLElement>(".outline")
  const preview = el<HTMLElement>(".preview-pane")
  const review = el<HTMLElement>(".review-pane")
  const reviewGutterEl = el<HTMLElement>(".review-gutter")
  // When review is closed, DON'T include review tracks in the grid at all —
  // using display:none removes the element from grid flow, which would shift
  // the remaining children into wrong tracks. Instead, switch between a 5-track
  // template (no review) and a 7-track template (with review).
  if (reviewPaneVisible) {
    workspaceEl.style.gridTemplateColumns =
      `${outlineW} ${g(!hiddenPanes.outline)} ${px(columnWidths.editor)} ${g(true)} ${px(columnWidths.review)} ${g(!hiddenPanes.preview)} ${px(columnWidths.preview)}`
  } else {
    workspaceEl.style.gridTemplateColumns =
      `${outlineW} ${g(!hiddenPanes.outline)} ${px(columnWidths.editor)} ${g(!hiddenPanes.preview)} ${px(columnWidths.preview)}`
  }
  // Sync display so panes don't leave stray borders.
  if (outline) outline.hidden = hiddenPanes.outline
  if (preview) preview.hidden = hiddenPanes.preview
  if (review) review.hidden = !reviewPaneVisible
  if (reviewGutterEl) reviewGutterEl.hidden = !reviewPaneVisible
  // Each hidden pane gets its own edge tab to reopen it, positioned on the side
  // where the pane lives (outline on the left, preview on the right) so the
  // user finds the toggle where they'd expect it.
  const showOutlineTab = el<HTMLButtonElement>("#show-outline-tab")
  if (showOutlineTab) showOutlineTab.hidden = !hiddenPanes.outline
  const showPreviewTab = el<HTMLButtonElement>("#show-preview-tab")
  if (showPreviewTab) showPreviewTab.hidden = !hiddenPanes.preview
}

/**
 * Which pane a gutter sits between, and the widths it drives.
 *
 * Dragging a gutter shifts pixels between the two adjacent column tracks. The
 * editor is always a 1fr track, so when it is on one side of the drag it simply
 * flexes to fill the remainder — only the fixed pane on the other side is
 * assigned a concrete new width. A drag that would shrink a fixed pane below a
 * readable minimum (outline ≥ 140px, preview/review ≥ 240px) is clamped.
 */
function gutterSides(gutter: string): { left: keyof ColumnWidths; right: keyof ColumnWidths; fixed: keyof ColumnWidths } | undefined {
  // Column order is outline · gutter · editor · gutter · review · gutter · preview,
  // so each gutter's fixed pane is the one it immediately borders.
  if (gutter === "outline") return { left: "outline", right: "editor", fixed: "outline" }
  if (gutter === "review") return { left: "editor", right: "review", fixed: "review" }
  if (gutter === "preview") return { left: "review", right: "preview", fixed: "preview" }
  return undefined
}

/**
 * Starts a column-resize drag on mousedown of a gutter.
 *
 * The handler measures the gutter's own bounding box (its left edge is the drag
 * origin) and tracks mousemove on `document` so the cursor can leave the 4px bar
 * without losing the grab. Each move rewrites the fixed pane's width and calls
 * applyWorkspaceGrid, which redraws the template synchronously. A `.dragging`
 * class on the workspace disables text selection during the gesture. mouseup on
 * `document` tears the listeners down.
 */
function startGutterDrag(event: MouseEvent): void {
  const bar = event.currentTarget as HTMLElement
  const key = bar.dataset.gutter
  if (!key) return
  // A double-click reaches here as a second mousedown-up with no move; suppress a
  // zero-delta drag so it does not fight the dblclick hide/show handler.
  if (event.detail > 1) return
  const sides = gutterSides(key)
  if (!sides) return
  // Do not start a drag for a gutter whose fixed pane is hidden (its track is 0).
  if (sides.fixed === "outline" && hiddenPanes.outline) return
  if (sides.fixed === "preview" && hiddenPanes.preview) return
  if (sides.fixed === "review" && !reviewPaneVisible) return
  event.preventDefault()
  workspaceEl.classList.add("dragging")
  const fixedSel = sides.fixed === "outline" ? ".outline" : sides.fixed === "preview" ? ".preview-pane" : ".review-pane"
  const fixedEl = el<HTMLElement>(fixedSel)
  const originX = bar.getBoundingClientRect().left
  const startWidth = fixedEl?.getBoundingClientRect().width ?? columnWidths[sides.fixed]
  const min = sides.fixed === "outline" ? 140 : 240
  const onMove = (moveEvent: MouseEvent): void => {
    // The outline gutter is the outline pane's RIGHT edge (drag right grows it).
    // The review/preview gutters are their pane's LEFT edge: dragging right pushes
    // that edge into the pane and shrinks it, so invert to keep "drag the boundary
    // with the cursor" consistent across all three columns.
    let delta = moveEvent.clientX - originX
    if (sides.fixed === "review" || sides.fixed === "preview") delta = -delta
    columnWidths[sides.fixed] = Math.max(min, Math.round(startWidth + delta))
    applyWorkspaceGrid()
  }
  const onUp = (): void => {
    workspaceEl.classList.remove("dragging")
    document.removeEventListener("mousemove", onMove)
    document.removeEventListener("mouseup", onUp)
  }
  document.addEventListener("mousemove", onMove)
  document.addEventListener("mouseup", onUp)
}

/**
 * Toggles a pane's visibility and reapplies the grid.
 *
 * Double-clicking a gutter is the documented shortcut to collapse the pane it
 * separates on its fixed side (outline/preview/review). The toolbar ×/› buttons
 * use the same path. Hiding collapses the pane + its gutter to 0px; showing
 * restores the last pixel width (or a sane default if it was dragged to 0).
 */
function togglePane(pane: "outline" | "preview"): void {
  hiddenPanes[pane] = !hiddenPanes[pane]
  applyWorkspaceGrid()
}

for (const bar of root.querySelectorAll<HTMLElement>(".gutter")) {
  bar.addEventListener("mousedown", startGutterDrag)
  // Double-click on a gutter collapses the adjacent fixed pane (matches the hint
  // in the gutter title attribute). dblclick fires after the suppressed drag.
  bar.addEventListener("dblclick", () => {
    const key = bar.dataset.gutter
    if (key === "outline") togglePane("outline")
    else if (key === "preview") togglePane("preview")
  })
}

el("#hide-outline")?.addEventListener("click", () => togglePane("outline"))
el("#hide-preview")?.addEventListener("click", () => togglePane("preview"))
el("#show-outline-tab")?.addEventListener("click", () => { hiddenPanes.outline = false; applyWorkspaceGrid() })
el("#show-preview-tab")?.addEventListener("click", () => { hiddenPanes.preview = false; applyWorkspaceGrid() })

// PDF zoom controls. The controls are hidden until a PDF is loaded; when shown,
// each button drives the VirtualPdfViewer's discrete zoom level and updates the
// label. Middle-click panning is handled inside the viewer itself.
function updateZoomLabel(): void { setText("#zoom-level", pdfViewer.zoomPercent) }
el("#zoom-in")?.addEventListener("click", () => { pdfViewer.zoomIn(); updateZoomLabel() })
el("#zoom-out")?.addEventListener("click", () => { pdfViewer.zoomOut(); updateZoomLabel() })
el("#zoom-reset")?.addEventListener("click", () => { pdfViewer.resetZoom(); updateZoomLabel() })

applyWorkspaceGrid()

/**
 * Toggles the editor/preview chrome on whether a document is open.
 *
 * On first landing (logged in, projects list, nothing selected) the full editor
 * toolbar + CodeMirror + preview pane imply an open document where none exists.
 * Hiding the chrome (and showing a focused placeholder per pane) keeps the
 * Projects/outline as the focus without collapsing the 3-column layout.
 */
function renderWorkspaceState(): void {
  const open = state.document !== undefined
  document.body.classList.toggle("has-document", open)
}

/**
 * Runs an Effect and routes failure to the status line.
 *
 * Every API failure is user-visible: silently swallowing one leaves the UI showing
 * stale state with no signal that a write did not land.
 */
function run<A>(
  effect: Effect.Effect<A, api.ApiError>,
  onSuccess: (value: A) => void = () => undefined,
  onError: (error: unknown) => void = (error) => {
    status(error instanceof Error ? error.message : "The API request failed")
  }
): void {
  void Effect.runPromise(effect).then(onSuccess, onError)
}

function showPanel(eyebrow: string, title: string, content: string): HTMLElement | undefined {
  const dialog = el<HTMLDialogElement>("#workspace-panel")
  setText("#panel-eyebrow", eyebrow)
  setText("#panel-title", title)
  const host = el<HTMLElement>("#panel-content")
  if (host) host.innerHTML = content
  if (!dialog?.open) dialog?.showModal()
  return host ?? undefined
}

/**
 * Collects a single text value via the workspace panel instead of `window.prompt`.
 *
 * The native prompt is blocking and unstyled, accepts empty strings, and is blocked
 * by some popup managers. This builds a labeled `<input>` with OK / Cancel buttons
 * inside the existing `#workspace-panel` dialog, mirroring the References / Export /
 * Review panels. OK is disabled while the value is empty/whitespace, and the inline
 * `.prompt-error` mirrors how the other panels surface problems. Enter submits,
 * Escape cancels (the `<dialog>` handles it natively, also firing `close`). The
 * optional `onClose` restores the caller's panel after the prompt UI closes, so the
 * References/Review list re-renders even when the prompt is dismissed with Esc.
 */
function promptInPanel(
  eyebrow: string,
  title: string,
  label: string,
  onConfirm: (value: string) => void,
  options: { readonly placeholder?: string; readonly onClose?: () => void } = {}
): void {
  const dialog = el<HTMLDialogElement>("#workspace-panel")
  // Stamp this prompt instance so finish() can tell whether onConfirm reopened
  // the (shared) #workspace-panel with a different prompt (the code→title
  // alternate flow). If it did, closing here would fire an async close event
  // that cancels the just-opened next prompt.
  const promptId = `${title}-${Math.random().toString(36).slice(2)}`
  dialog?.setAttribute("data-prompt", promptId)
  showPanel(eyebrow, title, `
    <label>${escapeHtml(label)}<input id="prompt-input" type="text" placeholder="${escapeHtml(options.placeholder ?? "")}" autocomplete="off" /></label>
    <p class="prompt-error" id="prompt-error" hidden></p>
    <div class="dialog-actions">
      <button class="toolbar-button" id="prompt-cancel" type="button">Cancel</button>
      <button class="primary-button" id="prompt-ok" type="button" disabled>OK</button>
    </div>`)
  const input = el<HTMLInputElement>("#prompt-input")
  const okButton = el<HTMLButtonElement>("#prompt-ok")
  const cancelButton = el<HTMLButtonElement>("#prompt-cancel")
  const errorNode = el<HTMLElement>("#prompt-error")
  if (!dialog || !input || !okButton || !cancelButton || !errorNode) return

  const value = (): string => input.value.trim()
  const refresh = (): void => {
    const empty = value() === ""
    okButton.disabled = empty
    errorNode.hidden = !empty
    errorNode.textContent = empty ? "Enter a value to continue." : ""
  }
  // Guards against the close event firing twice (Esc then programmatic close) and
  // against Enter after a confirm. OK hands the dialog back to the caller's panel
  // via `onClose`; Cancel/Esc closes it outright.
  let done = false
  const finish = (result: "ok" | "cancel"): void => {
    if (done) return
    done = true
    dialog.removeEventListener("close", handleClose)
    input.removeEventListener("input", refresh)
    input.removeEventListener("keydown", handleKey)
    okButton.removeEventListener("click", handleOk)
    cancelButton.removeEventListener("click", handleCancel)
    if (result === "ok") {
      onConfirm(value())
      options.onClose?.()
      // If onConfirm reopened the shared panel with a new prompt, our stamp was
      // replaced — do NOT close (the async close event would cancel the new
      // prompt). Only close on the normal single-prompt OK path.
      if (dialog.getAttribute("data-prompt") === promptId) dialog.close()
    } else {
      dialog.close()
    }
  }
  const handleClose = (): void => finish("cancel")
  const handleOk = (): void => { if (value() !== "") finish("ok") }
  const handleCancel = (): void => finish("cancel")
  const handleKey = (event: KeyboardEvent): void => {
    if (event.key === "Enter") { event.preventDefault(); handleOk() }
  }

  dialog.addEventListener("close", handleClose)
  input.addEventListener("input", refresh)
  input.addEventListener("keydown", handleKey)
  okButton.addEventListener("click", handleOk)
  cancelButton.addEventListener("click", handleCancel)
  input.focus()
}

// ---------------------------------------------------------------------------
// Outline: projects → documents
// ---------------------------------------------------------------------------

function renderProjects(): void {
  const list = el<HTMLElement>("#outline-list")
  if (!list) return
  setText("#outline-heading", "Projects")
  if (state.projects.length === 0) {
    list.innerHTML = `<div class="empty-state"><h3>No projects yet</h3><p>Create a project to start authoring.</p><button id="empty-create-project" class="primary-button" type="button">Create project</button></div>`
    el("#empty-create-project")?.addEventListener("click", createProject)
    return
  }
  list.innerHTML = state.projects
    .map((project) => `<div class="outline-row"><button class="outline-item" data-project="${escapeHtml(project.id)}" type="button"><span class="file-icon">▣</span><span>${escapeHtml(project.name)}</span></button><button class="outline-delete" data-delete-project="${escapeHtml(project.id)}" type="button" title="Delete project">×</button></div>`)
    .join("")
  for (const button of list.querySelectorAll<HTMLButtonElement>("[data-project]")) {
    button.addEventListener("click", () => {
      const project = state.projects.find((item) => item.id === button.dataset.project)
      if (project) openProject(project)
    })
  }
  for (const button of list.querySelectorAll<HTMLButtonElement>("[data-delete-project]")) {
    button.addEventListener("click", () => {
      const project = state.projects.find((item) => item.id === button.dataset.deleteProject)
      if (!project) return
      promptInPanel(
        "Projects",
        "Delete project",
        `Type the project name to confirm deletion of "${project.name}"`,
        (confirmText) => {
          if (confirmText.trim() !== project.name.trim()) {
            status("The typed text did not match; project was not deleted")
            return
          }
          run(api.deleteProject(project.id), () => {
            status("Project deleted")
            state.projects = state.projects.filter((item) => item.id !== project.id)
            // If the deleted project was the last-open one, clear it so a future
            // tab-return doesn't try to reopen a project that no longer exists.
            if (readLastOpen().projectId === project.id) persistLastOpen({})
            renderProjects()
          })
        },
        { placeholder: "Retype the project name to confirm" }
      )
    })
  }
}

function renderOutline(): void {
  const list = el<HTMLElement>("#outline-list")
  if (!list || !state.project) return
  setText("#outline-heading", state.project.name)
  const back = `<button class="outline-back" id="back-to-projects" type="button">← All projects</button>`
  if (state.outline.length === 0) {
    list.innerHTML = `${back}<div class="empty-state"><p>This project has no documents yet.</p><button id="add-document-empty" class="primary-button" type="button">Add document</button></div>`
    el("#add-document-empty")?.addEventListener("click", addDocument)
  } else {
    const rows = state.outline.map(({ document }) => {
      const active = state.selected?.document.id === document.id ? " active" : ""
      return `<div class="outline-row"><button class="outline-item${active}" data-document="${escapeHtml(document.id)}" type="button"><span class="file-icon">📄</span><span>${escapeHtml(document.title)}</span><code class="document-path">${escapeHtml(document.path)}</code></button><button class="outline-delete" data-delete-document="${escapeHtml(document.id)}" type="button" title="Delete document">×</button></div>`
    })
    list.innerHTML = back + rows.join("") + `<button class="outline-item outline-add" id="add-document" type="button"><span class="file-icon">＋</span><span>Add document</span></button><button class="outline-item outline-add" id="add-demo" type="button"><span class="file-icon">🦡</span><span>Add demo document</span></button>`
    el("#add-document")?.addEventListener("click", addDocument)
    el("#add-demo")?.addEventListener("click", addDemoFile)
    for (const button of list.querySelectorAll<HTMLButtonElement>("[data-document]")) {
      button.addEventListener("click", () => {
        const entry = state.outline.find((item) => item.document.id === button.dataset.document)
        if (entry) openDocument(entry)
      })
      button.addEventListener("dblclick", () => {
        const entry = state.outline.find((item) => item.document.id === button.dataset.document)
        const project = state.project
        if (!entry || !project) return
        promptInPanel("Documents", "Rename document", "Title", (title) => {
          run(api.updateDocument(project.id, entry.document.id, { title }), () => {
            status("Document renamed")
            loadOutline()
          })
        }, { placeholder: entry.document.title })
      })
    }
    for (const button of list.querySelectorAll<HTMLButtonElement>("[data-delete-document]")) {
      button.addEventListener("click", () => {
        const entry = state.outline.find((item) => item.document.id === button.dataset.deleteDocument)
        const project = state.project
        if (!entry || !project) return
        promptInPanel("Documents", "Delete document", `Type the title to confirm deletion of "${entry.document.title}"`, (confirmText) => {
          if (confirmText.trim() !== entry.document.title.trim()) {
            status("The typed text did not match; document was not deleted")
            return
          }
          run(api.deleteDocument(project.id, entry.document.id), () => {
            status("Document deleted")
            if (state.selected?.document.id === entry.document.id) {
              syncConnection?.close()
              syncConnection = undefined
              state.selected = undefined
              state.document = undefined
              state.review = emptyReviewState
              editor.dispatch({ effects: setReviewItems.of([]) })
              closeReviewPopover()
              editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: "" } })
              setSyncStatus("disconnected", "Open a document to collaborate")
              setText("#document-name", "No document selected")
              setText("#revision-label", "")
              renderWorkspaceState()
            }
            loadOutline()
          })
        }, { placeholder: entry.document.title })
      })
    }
  }
  el("#back-to-projects")?.addEventListener("click", () => {
    state.project = undefined
    state.selected = undefined
    state.document = undefined
    syncConnection?.close()
    syncConnection = undefined
    setSyncStatus("disconnected", "Open a document to collaborate")
    setText("#document-name", "No document")
    setText("#location-crumb", "/ Projects")
    renderWorkspaceState()
    renderProjects()
  })
}

function createProject(): void {
  promptInPanel("New project", "Create project", "Project name", (name) => {
    run(api.createProject(name), (project) => {
      state.projects = [...state.projects, project]
      status(`Created ${project.name}`)
      run(api.createDocument(project.id, { path: "main.typ", title: "Main" }), () => { void openProject(project) })
    })
  }, { placeholder: "Project name" })
}

/**
 * Persist/restore the last-open project + document (M5). Tab-away/return
 * previously dropped the user back on the project list with "No document
 * selected", losing their place. The ids survive in localStorage across tab
 * closes; the restore runs after the project list loads, reopening the project
 * and (if its outline still contains it) the document.
 */
const LAST_OPEN_KEY = "nisaba.lastOpen"
interface LastOpen { readonly projectId?: string; readonly documentId?: string }

function persistLastOpen(entry: LastOpen): void {
  try { localStorage.setItem(LAST_OPEN_KEY, JSON.stringify(entry)) } catch { /* storage may be unavailable */ }
}

function readLastOpen(): LastOpen {
  try { return JSON.parse(localStorage.getItem(LAST_OPEN_KEY) ?? "{}") as LastOpen } catch { return {} }
}

/**
 * Reopen the last project + document if they still exist. Called after the boot
 * project list + outline load so the entries are present to match against. A
 * missing project/document (deleted, or membership revoked) falls through to the
 * project list silently — never an error.
 */
function restoreLastOpen(): void {
  const last = readLastOpen()
  if (!last.projectId) return
  const project = state.projects.find((p) => p.id === last.projectId)
  if (!project) return
  openProject(project)
  if (!last.documentId) return
  // openProject loads the outline asynchronously; wait for it, then open the document.
  const tryOpen = (attempts: number): void => {
    // Abort if the user has already manually opened a document during the
    // polling window — don't override their choice.
    if (state.selected) return
    const entry = state.outline.find((e) => e.document.id === last.documentId)
    if (entry) { openDocument(entry); return }
    if (attempts > 0) setTimeout(() => tryOpen(attempts - 1), 200)
  }
  setTimeout(() => tryOpen(20), 200) // up to ~4s for the outline to load
}

function openProject(project: Project): void {
  state.project = project
  state.selected = undefined
  state.document = undefined
  // Role is unknown until the membership fetch resolves; reset so a stale role
  // from a previous project can't leak into the reviewer UX gates.
  state.role = undefined
  // M5: remember which project is open so a tab-away/return restores it instead
  // of dropping the user back on the project list.
  persistLastOpen({ projectId: project.id })
  setText("#location-crumb", `/ ${project.name}`)
  renderWorkspaceState()
  loadOutline()
  run(api.listReferences(project.id), (references) => { state.references = references })
  run(api.listFulltexts(project.id), (fulltexts) => { state.fulltexts = new Map(fulltexts.map((item) => [item.reference_id, item])) })
  // Fetch the caller's project-scoped role to gate reviewer UX: a reviewer is
  // locked into suggesting mode (H1) and has Export hidden (M4). On failure,
  // default to read-only (least privilege) so a transient error does not grant
  // author-level UI powers to non-authors.
  run(api.getMembership(project.id), (membership) => {
    state.role = membership.role
    applyRoleGates()
  }, () => {
    state.role = "read-only"
    applyRoleGates()
  })
}

function loadOutline(): void {
  const project = state.project
  if (!project) return
  const projectId = project.id
  run(api.listDocuments(projectId), (documents) => {
    if (state.project?.id !== projectId) return
    state.outline = [...documents]
      .sort((a, b) => a.path.localeCompare(b.path, undefined, { numeric: true }))
      .map((document) => ({ document }))
    renderOutline()
    setText("#outline-footer", `${documents.length} document${documents.length === 1 ? "" : "s"}`)
  })
}

function addDocument(): void {
  const project = state.project
  if (!project) return
  promptInPanel("Documents", "Add document", "Path", (pathValue) => {
    const documentPath = pathValue.endsWith(".typ") ? pathValue : `${pathValue}.typ`
    const title = documentPath.split("/").pop()?.replace(/\.typ$/i, "") || "Untitled"
    run(api.createDocument(project.id, { path: documentPath, title }), () => {
      status("Document created")
      loadOutline()
    })
  }, { placeholder: "main.typ" })
}

/** Adds a demo document with substantial Typst content. */
function addDemoFile(): void {
  const project = state.project
  if (!project) return
  const refTitles = [
    "Honeywell, B. (2023). Observational Evidence of Mustelid-Fey Synchronization at Subterranean Frequency Events.",
    "Glimmerwick, T. (2022). Spectral Analysis of Bioluminescent Dance Floors in the Fairy Underground.",
    "Badgerton, M. (2024). Aggression and Rhythm: Behavioral Correlates in Mellivora capensis at Rave Sites.",
    "Sparkletoes, P. (2023). Echolocation Interference by Fairy Folk During High-BPM Audio Playback.",
    "Hufflepaw, D. (2024). Dietary Shifts in Honey Badgers Attending Nocturnal Fairy Gatherings: A Pilot Study.",
    "Moonwhisper, L. (2022). The Underground Sound: Acoustic Architecture of Fairy Rave Caverns.",
    "Clawson, R. (2023). Territorial Marking Behavior Overlaid with Glitter Residue: A Forensic Approach.",
    "Twinkleburst, F. (2024). Effects of Sustained 140 BPM Exposure on Mustelid Heart Rate and Fairy Wing Beat Frequency."
  ]
  const refIds: string[] = []
  run(
    Effect.forEach(refTitles, (title) => api.createReference(project.id, {
      title,
      authors: [title.split(",")[0] ?? "Unknown"],
      year: 2024,
      doi: `10.1000/demo-${Math.random().toString(36).slice(2, 8)}`,
      journal: "Journal of Interdisciplinary Crypto-Zoological Acoustics",
      extra: {}
    }).pipe(Effect.map((ref) => { refIds.push(ref.id); return ref }))),
    () => {
      const body = generateDemoBody(refIds)
      run(api.createDocument(project.id, {
        path: "honey-badger-rave-study.typ",
        title: "Honey Badger Rave Study",
        body
      }), () => {
        status("Demo document added")
        loadOutline()
        run(api.listReferences(project.id), (references) => { state.references = references })
      })
    }
  )
}

/** Generates a substantial Typst document about honey badgers and fairy raves. */
function generateDemoBody(refIds: string[]): string {
  const L: string[] = []
  L.push('#set page(paper: "a4", margin: (x: 2cm, y: 2.5cm))')
  L.push('#set text(size: 10pt)')
  L.push('#set par(justify: true)')
  L.push("")
  L.push("= Honey Badgers and the Fairy Underground Rave Scene: A Comprehensive Investigation")
  L.push("")
  L.push("_By the Institute for Interdisciplinary Crypto-Zoological Acoustics_")
  L.push("")
  L.push("== Abstract")
  L.push("")
  L.push("This study presents the first systematic investigation into the observed relationship between the honey badger (*Mellivora capensis*) and the previously undocumented fairy underground rave scene. Over a period of 18 months, our research team deployed motion-activated cameras, acoustic sensors, and enchanted monitoring equipment across 47 suspected fairy rave sites in the Welsh countryside. Our findings reveal a startling pattern of honey badger attendance at these events, characterized by sustained rhythmic head-bobbing, aggressive dance floor territoriality, and an unexplained tolerance for glitter. See @fig-attendance for the spatial distribution of observed encounters.")
  L.push("")
  if (refIds[0]) L.push(`The phenomenon was first reported by ${"Honeywell"} #cite(<${refIds[0]}>) and initially dismissed as a statistical artifact.`)
  if (refIds[1]) L.push(`However, subsequent spectral analysis of the bioluminescent dance floors #cite(<${refIds[1]}>) confirmed that the acoustic signatures were consistent across all sites.`)
  L.push("")

  L.push("== Introduction")
  L.push("")
  L.push("=== Background")
  L.push("The honey badger, long renowned for its fearlessness and general indifference to consequences, has not previously been associated with subterranean recreational activities. Fairy folk, conversely, are well-documented in their preference for underground gatherings featuring synchronized bioluminescent light displays and rhythmic audio at tempos exceeding 130 BPM. The intersection of these two populations was first noted during a routine badger-tracking expedition in 2022, when field researcher Dr. B. Honeywell observed what she described as 'a large mustelid displaying unmistakable rhythmic coordination with a pulsating wall of fairy lights approximately 40 meters below a Welsh hillside.'")
  L.push("")
  L.push("=== Research Questions")
  L.push("This study addresses three primary questions:")
  L.push("+ Are honey badgers genuinely attending fairy raves, or is the observed proximity coincidental?")
  L.push("+ If attending, what behavioral modifications do honey badgers exhibit in the rave environment?")
  L.push("+ What is the ecological significance of this interspecies interaction?")
  L.push("")
  if (refIds[2]) L.push(`Preliminary behavioral analysis #cite(<${refIds[2]}>) suggests the attendance is deliberate and sustained.`)
  L.push("")

  L.push("== Methods")
  L.push("")
  L.push("=== Study Sites")
  L.push("We identified 47 candidate fairy rave sites based on surface indicators (unusual concentrations of toadstools in geometric patterns, faint bass vibrations detectable at ground level, and intermittent glitter deposits on nearby vegetation). Of these, 31 sites showed confirmed activity during the study period. The geographic distribution of confirmed sites is shown in @fig-attendance.")
  L.push("")
  L.push("=== Monitoring Equipment")
  L.push("Each site was instrumented with:")
  L.push("+ Motion-activated infrared cameras (Reconyx HyperFire 2) modified for subterranean deployment")
  L.push("+ Acoustic sensors capable of capturing frequencies from 10 Hz to 80 kHz, covering both the fairy audio range and the full honey badger vocalization spectrum")
  L.push("+ Enchanted monitoring crystals (for bioluminescent intensity and fairy-aura detection), provided by our Department of Fey Engineering")
  L.push("+ Glitter-spectroscopy collection pads placed at 5-meter intervals along suspected badger transit tunnels")
  L.push("")

  // Table 1: Study sites
  L.push("=== Site Characteristics")
  L.push("")
  L.push("#figure(table(")
  L.push("  columns: 4,")
  L.push("  [*Site*], [*Depth (m)*], [*Avg BPM*], [*Badger Visits*],")
  const sites = [
    ["Cwm Derwen", "38", "142", "17"],
    ["Tywyn Hollow", "52", "138", "23"],
    ["Blaenau Cavern", "41", "145", "9"],
    ["Ystrad Tunnel", "67", "150", "31"],
    ["Pen-y-Fawr Sink", "29", "135", "12"],
    ["Coed Ystlum", "44", "148", "8"],
    ["Nant Gwrhyd", "55", "141", "19"],
    ["Ogof Tinker", "33", "139", "25"],
    ["Ffos-y-Ffridd", "48", "146", "14"],
    ["Bwlch Glas", "61", "152", "7"],
  ]
  for (const [name, depth, bpm, visits] of sites) {
    L.push(`  [${name}], [${depth}], [${bpm}], [${visits}],`)
  }
  L.push(`), caption: [Site characteristics across all 10 primary monitoring locations. Average BPM measured at peak activity (midnight to 3 AM). Badger visits counted over the 18-month study period.])`)
  L.push("<tbl-sites>")
  L.push("The complete site data is presented in @tbl-sites. Note the positive correlation between site depth and average BPM (Pearson r = 0.72, p < 0.01), suggesting that deeper fairy venues favor faster tempos.")
  L.push("")

  // Figure 1
  L.push("=== Spatial Distribution")
  L.push("")
  L.push("#figure(rect(width: 100%, height: 8cm, fill: luma(240), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Map of confirmed fairy rave sites with honey badger attendance overlay. Each dot represents a confirmed site; dot size proportional to badger visit frequency.])), caption: [Geographic distribution of confirmed fairy rave sites (n=31) and honey badger encounter frequency. Sites concentrated in upland Wales, with a secondary cluster in the Brecon Beacons.])")
  L.push("<fig-attendance>")
  L.push("")

  L.push("== Results")
  L.push("")
  L.push("=== Honey Badger Attendance Patterns")
  L.push("")
  const behaviors = [
    ["Rhythmic head-bobbing", "94%", "Sustained bobbing at the dominant BPM for periods exceeding 15 minutes"],
    ["Territorial dance-floor marking", "78%", "Scent-marking posts adjacent to the primary bioluminescent wall"],
    ["Glitter tolerance", "100%", "No adverse reactions observed despite heavy glitter accumulation on fur"],
    ["Interspecies proximity tolerance", "88%", "Honey badgers remained within 2m of fairy folk without aggression"],
    ["Bioluminescent interaction", "67%", "Direct contact with fairy light displays (nose-touching, pawing)"],
    ["Sustained stillness during breakdowns", "91%", "Complete immobility during musical 'drops' followed by explosive activity"],
    ["Vocalization synchronization", "45%", "Growling patterns that coincided with bass drops on 45% of observed occasions"],
    ["Post-event napping", "82%", "Badgers remained at the site for an average of 47 minutes after music ceased"],
  ]
  L.push("#figure(table(")
  L.push("  columns: 3,")
  L.push("  [*Behavior*], [*Frequency*], [*Description*],")
  for (const [beh, freq, desc] of behaviors) {
    L.push(`  [${beh}], [${freq}], [${desc}],`)
  }
  L.push(`), caption: [Observed honey badger behaviors at fairy rave sites (n=165 encounters across 31 sites). Frequency represents the percentage of encounters in which the behavior was observed at least once.])`)
  L.push("<tbl-behaviors>")
  L.push("")
  L.push("The behavioral data summarized in @tbl-behaviors reveals that honey badgers exhibit a remarkably consistent suite of rave-related behaviors. The 100% glitter tolerance rate is particularly noteworthy, as honey badgers are typically averse to foreign substances on their fur.")
  L.push("")
  if (refIds[4]) L.push(`Hufflepaw's dietary analysis #cite(<${refIds[4]}>) further revealed that attending badgers showed a 34% increase in caloric intake in the 24 hours following a rave event, suggesting substantial energy expenditure.`)
  L.push("")

  // Figure 2
  L.push("=== Bioluminescent Interaction Analysis")
  L.push("")
  L.push("#figure(rect(width: 100%, height: 7cm, fill: luma(245), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Bioluminescent intensity (lux) over a typical 3-hour fairy rave event, with honey badger proximity events marked as vertical lines. Note the clustering of badger approaches during peak luminescence.])), caption: [Temporal relationship between bioluminescent intensity and honey badger proximity events during a representative rave event at Ystrad Tunnel (Site 4). Peak intensity events consistently attracted badger approach within 30 seconds.])")
  L.push("<fig-bioluminescence>")
  L.push("")
  L.push("As shown in @fig-bioluminescence, honey badgers demonstrated a clear attraction to peak bioluminescent events. The mean approach latency was 22.4 seconds (SD = 8.1), suggesting a rapid response to visual stimuli rather than acoustic cues alone.")
  L.push("")

  // Table 3
  L.push("=== Acoustic Analysis")
  L.push("")
  L.push("Acoustic recordings revealed an unexpected finding: honey badgers at rave sites produced vocalizations in the 40-60 Hz range that were phase-locked to the dominant bass frequency of the fairy audio system. This synchronization was observed at 74% of encounters and is unprecedented in the mustelid acoustic literature.")
  L.push("")
  if (refIds[7]) L.push(`The sustained 140+ BPM exposure documented by Twinkleburst #cite(<${refIds[7]}>) may explain the elevated heart rates observed in attending badgers (mean: 142 BPM vs. baseline 78 BPM).`)
  L.push("")
  const acoustic = [
    ["Site 1 (Cwm Derwen)", "142", "48 Hz", "Yes", "0.91"],
    ["Site 2 (Tywyn Hollow)", "138", "45 Hz", "Yes", "0.88"],
    ["Site 4 (Ystrad Tunnel)", "150", "52 Hz", "Yes", "0.95"],
    ["Site 8 (Ogof Tinker)", "139", "44 Hz", "No", "—"],
    ["Site 10 (Bwlch Glas)", "152", "55 Hz", "Yes", "0.97"],
  ]
  L.push("#figure(table(")
  L.push("  columns: 5,")
  L.push("  [*Site*], [*BPM*], [*Badger vocal freq*], [*Phase-locked*], [*Coherence*],")
  for (const [site, bpm, freq, locked, coh] of acoustic) {
    L.push(`  [${site}], [${bpm}], [${freq}], [${locked}], [${coh}],`)
  }
  L.push(`), caption: [Acoustic analysis of honey badger vocalizations at fairy rave sites. Phase-locking assessed via cross-correlation of badger vocalization envelopes with the fairy audio bass frequency. Coherence values >0.8 indicate strong synchronization.])`)
  L.push("<tbl-acoustic>")
  L.push("")

  L.push("== Discussion")
  L.push("")
  L.push("=== Why Do Honey Badgers Attend Fairy Raves?")
  L.push("")
  L.push("Several hypotheses may explain this unprecedented interspecies interaction:")
  L.push("")
  L.push("1. _Acoustic attraction_: The low-frequency bass characteristic of fairy rave music falls within the honey badger's peak hearing sensitivity. The sustained rhythm may produce a entrainment effect analogous to the 'groove response' documented in humans.")
  L.push("")
  L.push("2. _Thermoregulatory benefit_: Underground sites maintain a stable 12-15 degrees Celsius year-round, providing thermal refuge. The combination of stable temperature and rhythmic stimulation may create an optimal resting environment.")
  L.push("")
  if (refIds[3]) L.push(`3. _Fairy aura interaction_: Sparkletoes' work on echolocation interference #cite(<${refIds[3]}>) suggests fairy folk emit a subtle electromagnetic field. Honey badgers, with their large sinus cavities, may be uniquely positioned to detect and find this field pleasant.`)
  L.push("")
  L.push("4. _Glitter as a tracking mechanism_: The observation that honey badgers accumulate significant glitter without distress raises the possibility that glitter serves as a visual marker system. Honey badgers may use glitter trails to navigate between rave sites, effectively creating a glitter-based geographic information system.")
  L.push("")

  L.push("=== Ecological Implications")
  L.push("")
  L.push("The presence of an apex mustelid at fairy social events has potential implications for both populations:")
  L.push("")
  L.push("+ For fairy folk: the honey badger's territorial behavior may influence dance floor layout and crowd dynamics. Observations of fairies voluntarily yielding space to approaching badgers suggest a established interspecies social hierarchy.")
  L.push("+ For honey badgers: sustained exposure to high-BPM environments and bioluminescent stimuli may have long-term physiological effects. The elevated heart rates documented during events warrant further investigation.")
  L.push("+ For the ecosystem: the glitter deposition patterns associated with badger transit between sites may affect soil composition and plant growth along transit corridors.")
  L.push("")
  if (refIds[5]) L.push(`The acoustic architecture of the fairy caverns #cite(<${refIds[5]}>) creates natural amplification chambers that may extend the effective range of the rave signal, attracting badgers from distances exceeding 5 km.`)
  if (refIds[6]) L.push(`Clawson's forensic analysis of territorial markings #cite(<${refIds[6]}>) confirmed that 89% of marked posts within rave sites contained both badger scent compounds and fairy glitter particles, providing physical evidence of sustained co-occupation.`)
  L.push("")

  // Figure 3
  L.push("#figure(rect(width: 100%, height: 6cm, fill: luma(242), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Hypothesized model of honey badger-fairy rave interaction. Arrows indicate proposed causal relationships. Dashed lines represent uncertain pathways requiring further investigation.])), caption: [Conceptual model integrating acoustic attraction, thermoregulatory benefit, fairy-aura detection, and glitter-based navigation into a unified framework for understanding the honey badger-fairy rave phenomenon.])")
  L.push("<fig-model>")
  L.push("The integrated model proposed in @fig-model suggests that the interaction is maintained by positive feedback loops rather than a single attractor.")
  L.push("")

  L.push("== Limitations")
  L.push("")
  L.push("This study has several limitations that should be addressed in future research:")
  L.push("")
  L.push("- The enchanted monitoring crystals have not been independently calibrated against non-enchanted references.")
  L.push("- Glitter-spectroscopy is an emerging methodology with no established protocols for mustelid-associated glitter analysis.")
  L.push("- The geographic scope was limited to Wales; fairy rave sites in other regions (Cornwall, the Scottish Highlands, the Isle of Man) may exhibit different patterns.")
  L.push("- Observer bias may exist, as all field researchers reported finding the observations 'absolutely delightful' and may have unconsciously sought confirming evidence.")
  L.push("- The sample size of 165 encounters, while substantial, is insufficient for robust population-level inference.")
  L.push("")

  L.push("== Conclusions")
  L.push("")
  L.push("This study provides the first systematic evidence that honey badgers deliberately attend and actively participate in the fairy underground rave scene. The observed behaviors — rhythmic synchronization, bioluminescent interaction, glitter tolerance, and territorial dance-floor marking — constitute a coherent behavioral syndrome that warrants recognition as a distinct ecological phenomenon. We propose the term _Mellivora rava_ (rave badger syndrome) to describe this behavioral pattern.")
  L.push("")
  L.push("Future research should focus on: (1) physiological monitoring of attending badgers via non-invasive biotelemetry, (2) experimental manipulation of BPM and bioluminescent intensity to establish causal relationships, and (3) genetic analysis to determine whether rave attendance has a heritable component.")
  L.push("")

  L.push("== Acknowledgments")
  L.push("")
  L.push("We thank the Welsh Fairy Council for permitting access to monitoring sites, the Badger Watch volunteer network for field assistance, and the Department of Fey Engineering for the enchanted monitoring crystals. This research was supported by a grant from the Institute for Interdisciplinary Crypto-Zoological Acoustics (Grant No. HBFR-2023-007). No honey badgers or fairy folk were harmed during this study, though three cameras were destroyed by enthusiastic badger interactions.")
  L.push("")

  return L.join("\n")
}

// ---------------------------------------------------------------------------
// Document loading, sync, autosave
// ---------------------------------------------------------------------------

let syncConnection: { readonly close: () => void } | undefined

function openDocument(entry: OutlineEntry): void {
  const project = state.project
  if (!project) return
  // CRITICAL #1: Capture the document id at call time so the async getDocument
  // callback can verify the response still matches the document the user has
  // selected. Rapid document switching can deliver responses out of order; without
  // this guard a late response for a previously-clicked document would load the
  // wrong document into the editor and corrupt the open document's state.
  const documentId = entry.document.id
  // Flush a pending autosave for the document we are LEAVING instead of discarding
  // it. The SaveContext already captured the correct document/revision/body,
  // so firing it now persists the just-typed text to the right place. Previously
  // this dropped the timer and silently lost any edits made inside the 1200 ms
  // debounce window — a real data-loss path when switching documents mid-thought.
  flushPendingSave()
  // Drop a pending background diagnostics compile so a timer captured for the
  // document we are leaving never fires a build for the one we are loading.
  cancelDiagnosticsCompile()
  syncConnection?.close()
  // MEDIUM #8: Capture the document currently in the editor BEFORE reassigning
  // state.selected, so we can tell whether this open is a real switch.
  const previousDocumentId = state.selected?.document.id
  state.selected = entry
  // Clear the stale document reference BEFORE the editor-clear dispatch below.
  // The clear is a docChanged transaction; without this guard the update listener
  // sees state.document (the OLD document's doc) still set and calls scheduleSave(),
  // arming a 1.2 s timer whose captured SaveContext has the NEW document's id, the
  // OLD doc's revision, and an empty body. When that timer fires it PATCHes the
  // new document with empty text — a data-loss / corruption path. With document
  // undefined, captureSaveContext() returns undefined and scheduleSave is skipped.
  state.document = undefined
  // MEDIUM #8: When switching to a DIFFERENT document, clear stale editor content
  // before the async getDocument resolves. Without this the previous document's text
  // stays editable during the load gap; a user can type into it and those "gap
  // edits" are then unconditionally overwritten by loadIntoEditor. Clearing now
  // leaves nothing to lose (and is skipped on a re-open of the same document).
  if (editor.state.doc.length > 0 && previousDocumentId !== documentId) {
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: "" } })
  }
  // M5: remember the open document so tab-away/return restores it. Persist after
  // state.selected is set so the restore path can find the matching entry.
  persistLastOpen({ projectId: project.id, documentId: entry.document.id })
  renderOutline()
  setText("#document-name", entry.document.title)
  status("Loading document…")
  run(
    api.getDocument(project.id, documentId),
    (document) => {
      // CRITICAL #1: Bail out if the user has switched to a different document
      // while this document was loading — a stale response must not overwrite the
      // now-current editor content.
      if (state.selected?.document.id !== documentId) return
      state.review = emptyReviewState
      editor.dispatch({ effects: setReviewItems.of([]) })
      closeReviewPopover()
      // Re-apply the role gate after the review reset: openDocument wipes review
      // state (including a reviewer's forced-suggesting) so the lock must be
      // re-established here, not only in openProject (H1).
      applyRoleGates()
      state.document = document
      renderWorkspaceState()
      // The replica starts empty; loadIntoEditor seeds the persisted body into both
      // the replica and CodeMirror (so the user sees content immediately AND the
      // loro-codemirror binding's init reconcile does not blank the editor). On
      // connect, connectSync resolves this seeded body to a single authoritative
      // origin to avoid CRDT duplication (bug N1): the first client to reach an
      // empty relay pushes its seed; later clients CLEAR their local seed and adopt
      // the relay's snapshot.
      const replica = new LoroDoc()
      activeLoro = replica
      // Subscribe BEFORE seeding/connecting so the listener catches the relay's
      // welcome snapshot (which arrives inside connectSync's importRemote and may
      // carry a prior session's review container) as well as live peer updates.
      subscribeReviewSync(replica)
      loadIntoEditor(document.body)
      // A snapshot from a previous session (or a just-imported peer update) may have
      // populated the "review" container before the editor was bound — pull it into
      // state.review now. Local-only items already in state.review are preserved
      // (applyRemoteReview/loadPersistedReview diff against the current set).
      const persisted = loadPersistedReview()
      if (persisted && persisted.length > 0) applyRemoteReview(persisted)
      setText("#revision-label", `rev ${document.revision}`)
      status("Loaded")
      connectDocument(document, replica)
    }
  )
}

function loadIntoEditor(body: string): void {
  // Seed BOTH the Loro replica and the CodeMirror doc with the persisted body.
  // The replica seed is required so the loro-codemirror binding's init reconcile
  // (run on a microtask after the compartment reconfigure below) sees the editor
  // and replica already agree and does NOT blank the editor while waiting for the
  // relay's welcome snapshot. The CM seed gives the user immediate content even
  // for documents that were never synced through the CRDT (API-created/edited).
  //
  // Seeding the replica locally could duplicate the body when two clients each
  // seed independently and both push — bug N1. That is resolved in connectSync's
  // welcome handler, NOT here: the first client to reach an empty relay pushes
  // its seed as the single authoritative origin, and any later client CLEARS its
  // local seed (an op-id-scoped delete that does not touch the relay's copy) and
  // adopts the relay's snapshot. So seeding here is safe — the dedup happens on
  // connect. See sync.ts `connectSync` for the full N1 rationale.
  const text = activeLoro.getText("text")
  // updateByLine uses a line-based diff (documented for >50K-char bodies) instead
  // of constructing one giant insert op — dramatically faster for large documents.
  text.updateByLine(body)
  activeLoro.commit({ origin: "load" })
  // Replace the editor text and rebind the Loro extensions to the fresh replica
  // in one transaction. The undo manager excludes the "load" origin so the
  // seed is not undoable as a giant "delete everything" step. The
  // isLoadingDocument flag marks this dispatch as a non-user edit so neither the
  // save listener (no "Unsaved changes" flash on load) nor the review tracker
  // (seed not recorded as a giant suggestion) treats it as typing.
  isLoadingDocument = true
  try {
    editor.dispatch({
      changes: { from: 0, to: editor.state.doc.length, insert: body },
      effects: [
        setReviewItems.of(state.review.items),
        setDiagnostics.of([]),
        loroCompartment.reconfigure(
          LoroExtensions(activeLoro, undefined, new UndoManager(activeLoro, { excludeOriginPrefixes: ["load"] }), getTextFromDoc)
        )
      ]
    })
  } finally {
    isLoadingDocument = false
  }
}

function connectDocument(document: NisabaDocument, replica: LoroDoc): void {
  syncConnection?.close()
  const token = readStoredAccessToken()
  if (!token) {
    // The relay is fail-closed: an empty token is rejected. Say so instead of
    // leaving the UI claiming it is connected.
    setSyncStatus("disconnected", "Sign in to collaborate; edits are saved to the project")
    syncConnection = undefined
    return
  }
  syncConnection = connectSync(replica, {
    documentId: document.id,
    token,
    // The persisted body is the seed: connectSync pushes it once if the relay is
    // empty (this client becomes the origin), otherwise adopts the relay's
    // snapshot. See sync.ts for the N1 dedup rationale.
    seedBody: document.body,
    // Called immediately before the relay's authoritative snapshot is imported,
    // only when the relay already had content. Clear CodeMirror so the
    // loro-codemirror binding propagates the clear to the replica, leaving both
    // empty and in sync; the subsequent import then fills both without
    // duplication. Guarded by isLoadingDocument so this clear (a synthetic
    // dispatch, not user typing) does not trigger autosave or review tracking.
    // The import that follows runs under isImportingRemote(), which separately
    // suppresses those listeners for the relay text.
    onBeforeAdopt: () => {
      isLoadingDocument = true
      try {
        editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: "" } })
      } finally {
        isLoadingDocument = false
      }
    },
    onStatus: (value, detail) => setSyncStatus(value, detail)
  })
}

// The browser's navigator.onLine / online-offline events fire the instant the
// network drops — far sooner than the sync relay's WebSocket close, which can
// hang on a TCP timeout for tens of seconds. Tracking it separately lets the
// connection label turn honest immediately on network loss, instead of leaving a
// green "Online · Collaborating" light burning while the user is actually
// offline (H3). It is a one-way dimmer: going offline forces the label offline
// regardless of the last WS status; coming back online does NOT claim connected
// — it lets the next WS status (re)paint the truth.
let browserOffline = false
// Last relay-reported status/detail, replayed when the browser comes back online
// so the label returns to the relay's truth rather than staying stuck offline.
let lastSyncStatus: SyncStatus | undefined
let lastSyncDetail: string | undefined

function setSyncStatus(value: SyncStatus, detail?: string): void {
  // Remember the latest relay status so the offline listener can restore it on
  // reconnect without a redundant status callback.
  lastSyncStatus = value
  lastSyncDetail = detail
  if (browserOffline) {
    setText("#connection-state span:last-child", "Offline · Changes saved to the project")
    setText("#sync-label", "Local")
    return
  }
  const label =
    value === "connected" ? "Online · Collaborating"
      : value === "connecting" ? "Connecting…"
        : value === "unsupported" ? `Sync unavailable${detail ? ` · ${detail}` : ""}`
          : detail ?? "Offline · Changes saved to the project"
  setText("#connection-state span:last-child", label)
  setText("#sync-label", value === "connected" ? "Live" : "Local")
}

/**
 * Debounced save.
 *
 * `expected_revision` makes each write conditional, so a save from a stale editor is
 * rejected with 409 rather than clobbering someone else's paragraph.
 *
 * The save context is captured when the save is *scheduled*, not when it fires, so
 * a timer that survives a document switch cannot write one document's text under the
 * next document's ids.
 */
interface SaveContext {
  readonly documentId: string
  readonly revision: number
  readonly body: string
}

let saveTimer: ReturnType<typeof setTimeout> | undefined
let pendingSave: SaveContext | undefined
// Tracks whether a PATCH is currently over the wire. saveTimer/pendingSave only
// cover the debounce window; once the request fires they are cleared, so the
// beforeunload guard must look at this flag to detect an in-flight save that
// would lose data if the tab closes mid-request.
let saveInFlight = false

function captureSaveContext(): SaveContext | undefined {
  const { selected, document } = state
  if (!selected || !document) return undefined
  return {
    documentId: selected.document.id,
    revision: document.revision,
    body: editor.state.doc.toString()
  }
}

function scheduleSave(): void {
  const context = captureSaveContext()
  if (!context) return
  if (saveTimer !== undefined) clearTimeout(saveTimer)
  pendingSave = context
  saveTimer = setTimeout(saveNow, 1200)
  status("Unsaved changes")
}

function saveNow(): void {
  if (saveTimer !== undefined) {
    clearTimeout(saveTimer)
    saveTimer = undefined
  }
  const { project, selected } = state
  if (!project || !selected) return
  const context = pendingSave ?? captureSaveContext()
  pendingSave = undefined
  if (!context) return
  // Selection-drift guard: a timer captured for another document or a stale
  // document reference must not write to the document now open.
  if (context.documentId !== selected.document.id) return
  runSave(project.id, context)
}

/**
 * Immediately fires a pending autosave (if any) for its captured document,
 * independent of which document is currently selected. Used when leaving a
 * document (openDocument) or closing the tab (beforeunload) so edits made inside
 * the debounce window are not lost.
 */
function flushPendingSave(): void {
  if (saveTimer !== undefined) {
    clearTimeout(saveTimer)
    saveTimer = undefined
  }
  const context = pendingSave
  pendingSave = undefined
  const { project } = state
  if (!project || !context) return
  runSave(project.id, context)
}

/** Performs the PATCH for a specific save context and handles the 409 recovery. */
function runSave(projectId: string, context: SaveContext): void {
  if (context.body === state.document?.body && state.selected?.document.id === context.documentId) { status("Saved"); return }
  status("Saving…")
  // Mark the request as in-flight so the beforeunload guard can warn about an
  // unsaved PATCH — saveTimer/pendingSave are already cleared by this point.
  saveInFlight = true
  run(
    api.saveDocument(projectId, context.documentId, context.body, context.revision),
    (saved) => {
      saveInFlight = false
      // Only update the open document if we are still editing the document this
      // save was for.
      if (state.selected?.document.id === context.documentId) {
        state.document = saved
        setText("#revision-label", `rev ${saved.revision}`)
      }
      status("Saved")
    },
    (error: unknown) => {
      saveInFlight = false
      // One stale revision makes every subsequent autosave bounce 409 until the
      // document is reloaded, so a conflict is recovered by fetching the latest
      // revision and rescheduling the pending text against it. The collaborative
      // replica has usually already merged the other author's edits, so the retry
      // then lands.
      if (error instanceof api.ApiError && error.status === 409) {
        if (state.selected?.document.id !== context.documentId) {
          status("Save conflicted; the document changed")
          return
        }
        void Effect.runPromise(
          api.getDocument(projectId, context.documentId)
        ).then(
          (latest) => {
            if (state.selected?.document.id !== context.documentId) { status("Saved elsewhere"); return }
            state.document = latest
            setText("#revision-label", `rev ${latest.revision}`)
            const localBody = editor.state.doc.toString()
            if (localBody === latest.body) {
              // Already in sync (the CRDT merged the peer edit); nothing to write.
              status("Saved")
              return
            }
            // H2 (data loss): the old recovery unconditionally re-scheduled the
            // save, which overwrote the server edit whenever the local editor
            // hadn't learned about it (a peer REST edit, or a CRDT update this
            // replica never received). Only auto-resave when the sync relay is
            // connected — then the CRDT is the merged truth and local text is a
            // superset of the server's. When disconnected, the divergence is real
            // and silently overwriting would lose the other author's work; surface
            // it as a conflict so the user decides.
            if (lastSyncStatus === "connected") {
              status("Resaving the merged revision…")
              scheduleSave()
            } else {
              status("Save conflict — another author edited this document while you were offline; reload to merge")
            }
          },
          () => status("Save conflicted; couldn't reload the latest revision")
        )
        return
      }
      status(error instanceof Error ? error.message : "The API request failed")
    }
  )
}

/**
 * Debounced background compile for live error checking.
 *
 * After the user stops typing for a couple of seconds we recompile in the
 * background and refresh the diagnostic underlines/list — but NOT the PDF
 * preview, so the canvas does not flash on every pause. It shares the
 * `compiling` guard with `compileCurrent` so a manual compile and a debounce
 * fire never run two server builds at once. The timer is cleared on every new
 * keystroke (so a burst of edits is one compile) and on document switch (so a
 * timer captured for one document never compiles another's text).
 */
let diagnosticsTimer: ReturnType<typeof setTimeout> | undefined
const DIAGNOSTICS_DEBOUNCE_MS = 2000

function scheduleDiagnosticsCompile(): void {
  if (diagnosticsTimer !== undefined) clearTimeout(diagnosticsTimer)
  diagnosticsTimer = setTimeout(() => {
    diagnosticsTimer = undefined
    compileForDiagnostics()
  }, DIAGNOSTICS_DEBOUNCE_MS)
}

/** Cancels a pending diagnostics compile (document switch / unload). */
function cancelDiagnosticsCompile(): void {
  if (diagnosticsTimer !== undefined) {
    clearTimeout(diagnosticsTimer)
    diagnosticsTimer = undefined
  }
}

// ---------------------------------------------------------------------------
// Editor
// ---------------------------------------------------------------------------

function openConstruct(construct: Construct): void {
  showPanel(
    "Focused editing",
    construct.kind[0]?.toUpperCase() + construct.kind.slice(1),
    `<p>This ${escapeHtml(construct.kind)} is part of the document source, which stays the single source of truth.</p><pre class="construct-source">${escapeHtml(construct.label ?? "")}</pre>`
  )
}

// ---------------------------------------------------------------------------
// Autocomplete: Typst commands + reference/citation keys
// ---------------------------------------------------------------------------

/**
 * Typst command completions triggered by `#`.
 *
 * The `#\w*` matchBefore returns the `#` plus any letters typed after it (e.g.
 * `#fig`). We report `from` as the position right after the `#` so CodeMirror's
 * filter scores the bare command name (`fig` vs `figure`), and the replace
 * range never touches the already-typed `#`. Each option's `apply` carries the
 * opening bracket/brace, so selecting `figure` inserts `figure(` (the user's
 * `#` stays in front). `validFor` keeps the popup open while the user keeps
 * typing word characters after the `#`. The `cite` command's `apply` includes
 * `<` so selecting it immediately triggers reference-key completions.
 */
const typstCommands: readonly Completion[] = [
  { label: "figure", type: "function", detail: "figure(body, caption: ...)", apply: "figure(" },
  { label: "table", type: "function", detail: "table(columns: ..., ...rows)", apply: "table(" },
  { label: "image", type: "function", detail: 'image("path")', apply: "image(" },
  { label: "rect", type: "function", detail: "rect(width, height, ...)", apply: "rect(" },
  { label: "text", type: "function", detail: "text(body, ...)", apply: "text(" },
  { label: "emph", type: "function", detail: "emph[body]", apply: "emph[" },
  { label: "strong", type: "function", detail: "strong[body]", apply: "strong[" },
  { label: "link", type: "function", detail: 'link("url")', apply: "link(" },
  { label: "cite", type: "function", detail: "cite(<key>)", apply: "cite(<" },
  { label: "bibliography", type: "function", detail: 'bibliography("path")', apply: "bibliography(" },
  { label: "set page", type: "keyword", detail: "set page(...)", apply: "set page(" },
  { label: "set text", type: "keyword", detail: "set text(...)", apply: "set text(" },
  { label: "set par", type: "keyword", detail: "set par(...)", apply: "set par(" },
  { label: "show heading", type: "keyword", detail: "show heading: ...", apply: "show heading:" },
  { label: "show figure", type: "keyword", detail: "show figure: ...", apply: "show figure:" },
  { label: "let", type: "keyword", detail: "let name = ...", apply: "let " },
  { label: "import", type: "keyword", detail: 'import "path": ...', apply: "import " },
  { label: "align", type: "function", detail: "align(alignment, body)", apply: "align(" },
  { label: "grid", type: "function", detail: "grid(columns: ..., ...)", apply: "grid(" },
  { label: "block", type: "function", detail: "block[body]", apply: "block[" },
  { label: "page", type: "function", detail: "page(...)", apply: "page(" },
  { label: "cols", type: "function", detail: "cols(count, body)", apply: "cols(" },
  { label: "v", type: "function", detail: "v(amount)", apply: "v(" },
  { label: "h", type: "function", detail: "h(amount)", apply: "h(" }
]

const typstCompletions: CompletionSource = (context: CompletionContext): CompletionResult | null => {
  const word = context.matchBefore(/#\w*/)
  // Only trigger when a `#` is directly before the cursor (optionally followed
  // by word chars). matchBefore already covers the bare-`#` case (it matches the
  // `#` with zero trailing word chars), so no explicit fallback is needed.
  if (!word) return null
  // Start the replace range right after the `#` so the already-typed `#` is
  // preserved and the filter scores against the bare command name.
  return {
    from: word.from + 1,
    validFor: /#\w*/,
    options: typstCommands
  }
}

/**
 * Fuzzy substring matcher for reference search.
 *
 * Matches a query against a haystack by checking whether all characters of the
 * query appear in order (not necessarily contiguous). This is fast, simple, and
 * handles the common case of typing a few letters from a title, author, or key.
 * Returns a numeric score (higher = better match) or -1 if no match.
 */
function fuzzyScore(query: string, haystack: string): number {
  if (!query) return 0
  const q = query.toLowerCase()
  const h = haystack.toLowerCase()
  let qi = 0
  let score = 0
  let streak = 0
  for (let hi = 0; hi < h.length && qi < q.length; hi++) {
    if (h[hi] === q[qi]) {
      qi++
      streak++
      score += streak // consecutive matches score higher
      if (hi === 0 || h[hi - 1] === " " || h[hi - 1] === "-") score += 2 // word-boundary bonus
    } else {
      streak = 0
    }
  }
  return qi === q.length ? score : -1
}

/**
 * Reference/citation completions with fuzzy search across keys, titles, authors.
 *
 * Two trigger shapes:
 *   * `@key`      — Typst's reference shorthand. The `@\w*` matchBefore matches
 *     the `@` plus any letters; `from` is set right after the `@` so the filter
 *     scores against the typed fragment, and the `@` is preserved.
 *   * `#cite(<key` — the explicit form. The `#cite\(<[\w-]*` matchBefore matches
 *     from `#cite(<` through any UUID-ish chars; `from` is set right after the
 *     `<` so the filter scores against the typed key fragment, and the
 *     `#cite(<` prefix is preserved. Selecting `cite` from the `#` menu inserts
 *     `cite(<` (with the `<`), so reference completions fire immediately without
 *     the user needing to know the angle-bracket syntax.
 *
 * The typed fragment is matched fuzzily against each reference's key, title, and
 * authors. Results are ranked by fuzzy score and returned with `filter: false`
 * so CodeMirror does not re-filter (it would only match the label prefix).
 * Selecting a reference inserts its UUID; the closing `>` is left for the user.
 * The list is read from `state.references` at completion time, so it always
 * reflects the latest loaded project references without a compartment reconfigure.
 */
const referenceCompletions: CompletionSource = (context: CompletionContext): CompletionResult | null => {
  const at = context.matchBefore(/@\w*/)
  const cite = !at ? context.matchBefore(/#cite\(<[\w-]*/) : null
  const match = at ?? cite
  if (!match) return null
  // The fragment the user has typed after the trigger prefix (`@` or `#cite(<`).
  const triggerEnd = match.from + match.text.lastIndexOf("<") + 1
  const from = at ? match.from + 1 : triggerEnd
  const typed = at ? match.text.slice(1) : match.text.slice(match.text.lastIndexOf("<") + 1)
  // Build and fuzzy-rank all references. Each option's label is the title (what
  // the user reads), detail is "Author, Year" or the bare key, and apply is the
  // UUID key that Typst expects between angle brackets.
  const scored = state.references
    .map((reference) => {
      const meta = reference.metadata
      const year = meta.year ?? ""
      const haystack = [reference.id, meta.title, meta.authors.join(" "), String(year)].join(" ")
      return { reference, score: fuzzyScore(typed, haystack) }
    })
    .filter((entry) => entry.score >= 0)
    .sort((a, b) => b.score - a.score)
  const options = scored.map(({ reference }): Completion => {
    const meta = reference.metadata
    const author = meta.authors[0] ?? ""
    const year = meta.year ?? ""
    const detailParts = [author, year].filter((part) => String(part).trim() !== "").join(", ")
    return {
      label: meta.title,
      detail: detailParts || reference.id.slice(0, 8),
      type: "variable",
      apply: reference.id
    }
  })
  if (options.length === 0) return null
  return {
    from,
    // filter:false so CodeMirror uses our ranking verbatim instead of
    // re-filtering by label prefix (which would break fuzzy author/key matches).
    filter: false,
    // validFor keeps the popup open while the typed prefix still matches. The @
    // branch must include hyphens ([\w-]) because citation keys are often
    // hyphenated (e.g. @my-ref); \w alone would close the popup after the dash.
    validFor: at ? /@[\w-]*/ : /#cite\(<[\w-]*/,
    options
  }
}

// Cache of parsed constructs for the current doc, refreshed only on doc-change
// transactions. findConstructs re-parses the ENTIRE document; calling it on every
// cursor move (selectionSet) made large documents laggy, so we parse once per edit
// and reuse the result for the selection listener's chip-reveal lookup.
let cachedConstructs: Construct[] = []

const editor = new EditorView({
  state: EditorState.create({
    doc: "",
    extensions: [
      basicSetup,
      // Typst command + reference/citation completions. Placed after basicSetup
      // (which already pulls in default autocompletion) so these sources augment
      // the defaults rather than replacing them.
      autocompletion({ override: [typstCompletions, referenceCompletions] }),
      // Tab accepts the current autocomplete suggestion (muscle memory from most editors).
      keymap.of([{ key: "Tab", run: acceptCompletion }]),
      EditorView.lineWrapping,
      // Empty editor guidance: a freshly opened, never-edited document otherwise
      // shows a blank pane with no hint that this is Typst.
      placeholder("Type Typst here… use = for headings, $…$ for math"),
      hybridEditorField(openConstruct, (): readonly ReferenceDisplay[] =>
        state.references.map((r) => ({ id: r.id, authors: r.metadata.authors, year: r.metadata.year, title: r.metadata.title }))
      ),
      reviewEditorField(openReviewPopover),
      // Catches clicks on `.review-comment`/`.review-suggestion` mark spans (anchor
      // widgets bind their own click in their WidgetType). Delegates to handleReviewClick,
      // which resolves the `[data-review-id]` and opens the popover.
      EditorView.domEventHandlers({
        click: handleReviewClick,
        dblclick: () => {
          // Double-click selects a word; search for it in the PDF preview and
          // scroll to the match so the editor and preview stay synchronised.
          const selected = editor.state.sliceDoc(
            editor.state.selection.main.from,
            editor.state.selection.main.to
          ).trim()
          if (selected.length > 0) {
            void pdfViewer.searchAndScroll(selected)
          }
        }
      }),
      diagnosticCompartment.of(diagnosticField()),
      editableComp.of(EditorView.editable.of(true)),
      EditorView.updateListener.of((update) => {
        if (update.selectionSet || update.docChanged) {
          updateSelectionCommentButton(update.view)
        }
        if (update.selectionSet) {
          const head = update.state.selection.main.head
          const line = update.state.doc.lineAt(head)
          setText("#cursor-position", `Ln ${line.number}, Col ${head - line.from + 1}`)
          // Use the cached parse instead of re-parsing on every cursor move.
          // The cache is refreshed below whenever the doc actually changes, so the
          // offsets stay valid between edits (selection-only updates don't move text).
          const construct = cachedConstructs.find((item) => head >= item.from && head <= item.to)
          // Entering a chip (figure/table/…) reveals its raw source; leaving must
          // re-chip it again. The reveal set is rebuilt from effects on each update,
          // so dispatching a sentinel that matches no construct empties the set and
          // returns every still-chipped construct to its button form.
          update.view.dispatch({ effects: revealConstruct.of(construct ? { from: construct.from, to: construct.to } : { from: -1, to: -1 }) })
        }
        if (update.docChanged) {
          // Re-parse the document only when text actually changed, then reuse the
          // result for all subsequent cursor moves until the next edit.
          cachedConstructs = findConstructs(update.state.doc.toString())
        }
        if (update.docChanged && state.document) {
          // Two kinds of change are not user edits and must not trigger a PATCH:
          //   * the load transaction (isLoadingDocument) — the seed/reconfigure
          //     dispatch in loadIntoEditor;
          //   * remote imports (isImportingRemote) — the relay snapshot that
          //     populates the editor on open, plus live peer edits. These originate
          //     FROM the authority, so re-PATCHing them is redundant AND a data-loss
          //     vector: a snapshot whose body differs from the REST body would
          //     otherwise be written back over the server head, bumping the revision
          //     with the editor's text. The snapshot import runs the plugin's
          //     synchronous dispatch inside doc.import(), which fires long after
          //     isLoadingDocument has reset, so the flag alone does not cover it.
          // The review tracker still runs for remote changes so peer edits remap
          // existing accept/reject anchors; it classifies remote edits itself.
          const remote = isImportingRemote()
          if (!isLoadingDocument && !remote) {
            scheduleSave()
            // Live error checking: after the typing pause, recompile in the
            // background for fresh diagnostic underlines (no PDF update). Same
            // remote/load exclusions as save — peer imports and the load seed are
            // not user typing, so they must not trigger a diagnostics build.
            scheduleDiagnosticsCompile()
          }
          if (!isLoadingDocument) updateReviewItems(update)
        }
      }),
      loroCompartment.of(LoroExtensions(activeLoro, undefined, undefined, getTextFromDoc))
    ]
  }),
  parent: root.querySelector("#editor") ?? undefined
})

/** True only during the initial/reload seed dispatch (see openDocument/loadIntoEditor). */
let isLoadingDocument = false

/**
 * True while a REMOTE review update is being applied to `state.review` (see
 * applyRemoteReview). While set, persistReview is a no-op so the just-applied
 * state is not written straight back to the Loro map — that would echo every
 * remote update and, because Loro also fires the local subscriber, create a
 * write feedback loop. Cleared as soon as the remote apply finishes.
 */
let applyingRemoteReview = false

/** Loro container key that holds the serialised review state (JSON-in-LoroMap). */
const REVIEW_CONTAINER = "review"

/**
 * Unsubscribe handle for the active document's review-container subscription.
 * One per document: torn down in openDocument before the next document subscribes,
 * so the listener never fires against a stale/closed replica.
 */
let reviewSyncUnsubscribe: (() => void) | undefined

/**
 * Tracks review items across edits and records suggestions while suggesting is on.
 *
 * Items are persisted into the Loro CRDT (the "review" container) by persistReview
 * so they survive reload and sync to other collaborators through the existing
 * relay. Their from/to offsets are remapped here through every local change so
 * accept/reject operate on current positions; only structural changes (a new
 * suggestion actually recorded, or a non-remote apply) trigger a persist, never a
 * pure position remap, to avoid per-keystroke CRDT writes.
 */
function updateReviewItems(update: ViewUpdate): void {
  // The relay wraps every `doc.import()` in `isImportingRemote()`. Loro fires its
  // subscriptions synchronously inside that call, and the loro-codemirror listener
  // dispatches to CodeMirror synchronously too, so the flag is set for exactly the
  // transactions that originated from a peer. The previous text-diff heuristic
  // misclassified the user's own keystrokes as remote once the sync relay was live.
  const remote = isImportingRemote()
  // Accept/Reject mutations are authoritative: they must not be re-recorded as
  // new suggestions, or "Reject all" with suggesting on never reaches zero. A
  // resolution is recognised by either its annotation or the transient flag (see
  // resolveAnnotation), so a plugin re-dispatch without the annotation is still
  // suppressed.
  const isResolution = resolvingSuggestions || update.transactions.some((transaction) => transaction.annotation(resolveAnnotation) === true)
  // Clamp positions to the PRE-edit doc length before mapping. ChangeSet.mapPos
  // throws "Position N is out of range for changeset of length M" if given a
  // position beyond the startState doc — which happens whenever an item holds a
  // stale offset from a longer prior document (e.g. a remote update that deleted
  // text arrives before this transaction). The orphan check above only skips
  // already-flagged orphans, not items that THIS update will orphan, so without
  // the clamp every remote edit under concurrency aborts updateReviewItems and
  // desynchronises review decorations. Clamp then map; mark-orphans below reflags
  // anything that ends up past the post-edit doc.
  const preLen = update.startState.doc.length
  const items = state.review.items.map((item) => item.orphaned
    ? item
    : {
        ...item,
        from: update.changes.mapPos(Math.min(item.from, preLen), 1),
        to: update.changes.mapPos(Math.min(item.to, preLen), -1)
      } as ReviewItem)
  let review = reviewReducer({ ...state.review, items }, { type: "mark-orphans", textLength: update.state.doc.length })
  // Set when this transaction records at least one new suggestion; only then does
  // updateReviewItems persist, so pure position remapping (every keystroke that does
  // not add a suggestion) does not write to the CRDT.
  let recordedNew = false
  if (state.review.suggesting && !remote && !isResolution) {
    // The author and creation time of every suggestion recorded in this transaction.
    // Coalescing (addCoalesced) preserves the FIRST item's author/timestamp for the
    // whole run, so a typed word keeps one identity even though it spans keystrokes.
    const author = currentUserDisplayName()
    const createdAt = Date.now()
    // `lastPreFrom` tracks the PRE-edit (unmapped) anchor of the most recent open
    // suggestion as we walk the changes, so a coalesced delete's merged text stays
    // in document order: a backspace removes a char left of the run (prepend), a
    // forward-delete right of it (append), and only the pre-edit `from` survives
    // mapping to tell them apart. Seeded from the existing tail before any change.
    let lastPreFrom = state.review.items[state.review.items.length - 1]?.from ?? Number.POSITIVE_INFINITY
    // iterChanges runs in document order, and each new suggestion is added then
    // coalesced against the tail before the next change is seen, so a contiguous
    // run (e.g. the characters of one typed word, all adjacent) collapses into a
    // single suggestion instead of N.
    update.changes.iterChanges((fromA, toA, fromB, toB, insert) => {
      const original = update.startState.doc.sliceString(fromA, toA)
      if (toA > fromA) {
        // Zero-width anchor: the text is already gone; Reject restores it.
        const fromCursor = createCursorAt(activeLoro, fromA)
        const next = addCoalesced(review, { id: crypto.randomUUID(), kind: "suggestion", from: fromA, to: fromA, fromCursor, toCursor: fromCursor, change: "delete", text: original, author, status: "open", createdAt }, fromA, lastPreFrom)
        lastPreFrom = fromA
        review = next
      }
      if (insert.length > 0) {
        const insFromCursor = createCursorAt(activeLoro, fromB)
        const insToCursor = toB > fromB ? createCursorAt(activeLoro, toB) : insFromCursor
        const next = addCoalesced(review, { id: crypto.randomUUID(), kind: "suggestion", from: fromB, to: toB, fromCursor: insFromCursor, toCursor: insToCursor, change: "insert", text: insert.toString(), author, status: "open", createdAt }, fromB, lastPreFrom)
        lastPreFrom = fromB
        review = next
      }
    })
    // Suggestions were recorded (or an existing run was extended) — the review set
    // changed structurally, not just by position remap, so it must persist + sync.
    recordedNew = review.items.length > state.review.items.length
  }
  state.review = review
  update.view.dispatch({ effects: setReviewItems.of(review.items) })
  renderReviewBanner()
  renderReviewSidebar()
  if (recordedNew) persistReview()
}

// ---------------------------------------------------------------------------
// Review persistence + sync (JSON-in-LoroMap)
// ---------------------------------------------------------------------------

/**
 * Writes the current review items into the active replica's "review" LoroMap as a
 * single JSON string, so they survive reload and sync to every collaborator through
 * the existing WebSocket relay (the same path the text CRDT uses — no new endpoints).
 *
 * The whole item list is one value keyed "items". Loro's last-writer-wins map semantics
 * are fine here because each peer always writes the FULL, merged set (positions already
 * remapped against the same shared text), so concurrent writes converge to whichever
 * landed last — and the next local edit re-persists the authoritative current state.
 *
 * The map.set() is a no-op when the value is unchanged (Loro dedups it), so calling
 * this after every review mutation is cheap. It is guarded so it does NOT run while:
 *   * applying a remote review update (applyingRemoteReview) — would echo back;
 *   * seeding a document (isLoadingDocument) — the seed dispatch, not a real change;
 *   * importing remote text (isImportingRemote) — peer edits, not local review edits;
 *   * no replica is active yet (before openDocument) — there is nowhere to write.
 */
function persistReview(): void {
  if (applyingRemoteReview || isLoadingDocument || isImportingRemote()) return
  const doc = activeLoro
  const json = JSON.stringify(state.review.items)
  try {
    const map = doc.getMap(REVIEW_CONTAINER)
    if (map.get("items") !== json) {
      map.set("items", json)
      doc.commit({ origin: "review" })
    }
  } catch { /* replica torn down mid-document-switch: silently skip */ }
}

/**
 * Replaces local review items from a JSON payload read off the Loro map, guarding
 * against the write feedback loop with applyingRemoteReview and the save/retrack
 * guards with the existing flags. Called for both initial load (a prior session's
 * snapshot) and live remote updates.
 */
function applyRemoteReview(items: readonly ReviewItem[]): void {
  applyingRemoteReview = true
  try {
    state.review = { ...state.review, items, capability: "available" }
    editor.dispatch({ effects: setReviewItems.of(items) })
    renderReviewBanner()
    renderReviewSidebar()
  } finally {
    applyingRemoteReview = false
  }
}

/**
 * Reads the persisted review JSON from the active replica (if any) and, when it is
 * present AND differs from what is already in state.review, applies it locally.
 *
 * Differs-from-current matters on initial load: a brand-new local comment exists only
 * in state.review (not yet persisted), and re-applying stale persisted items would
 * clobber it. The subscribe listener reaches the same check via applyRemoteReview for
 * live updates. Returns the parsed items, or undefined when there is nothing stored.
 */
function loadPersistedReview(doc: LoroDoc = activeLoro): readonly ReviewItem[] | undefined {
  try {
    // LOW #10: Read from the passed-in doc, not the file-level activeLoro, so the
    // subscribe handler always reads the replica it subscribed to (activeLoro may
    // have been reassigned to a newer document's replica by the time the callback fires).
    const json = doc.getMap(REVIEW_CONTAINER).get("items")
    if (typeof json !== "string" || json.length === 0) return undefined
    const items = JSON.parse(json) as readonly ReviewItem[]
    if (!Array.isArray(items)) return undefined
    return items
  } catch { return undefined }
}

/**
 * Subscribes the active replica's "review" container so remote changes (a peer's
 * new comment, or the relay's welcome snapshot from a prior session) update the
 * local review state. Filters to import/checkout events only — local writes (by:
 * "local") originate from persistReview and are already reflected in state.review,
 * so re-applying them would be redundant (and would race the guards in persistReview).
 *
 * The handler re-reads the map value (rather than trusting an event diff) because
 * the container always holds the full JSON set, so a single get() is the simplest
 * correct read of "what is the review state now".
 *
 * `reviewSyncUnsubscribe` holds the handle so the next openDocument can tear it down
 * before subscribing against the fresh replica.
 */
function subscribeReviewSync(doc: LoroDoc): void {
  reviewSyncUnsubscribe?.()
  reviewSyncUnsubscribe = doc.subscribe((event) => {
    // Local writes already updated state.review at their source (persistReview), so
    // they are ignored. "import" covers relay snapshots + live peer updates;
    // "checkout" covers any version navigation.
    if (event.by === "local") return
    // LOW #10: Pass the subscribed `doc` so the read targets this replica rather
    // than whatever activeLoro points to now.
    const items = loadPersistedReview(doc)
    if (items && JSON.stringify(items) !== JSON.stringify(state.review.items)) {
      applyRemoteReview(items)
    }
  })
}

/**
 * Adds a suggestion, coalescing it into the immediately preceding open suggestion
 * by the same author when they form a contiguous run. Without this, typing a word
 * produces one tracked-insert suggestion PER CHARACTER (each keystroke is its own
 * change). Standard tracked-changes merge consecutive same-author runs, so we:
 *   * inserts — extend the previous insert's range and append text when the new one
 *     starts where the previous ends (adjacent typing). Runs are NOT broken on
 *     whitespace — a full paragraph of typing should be ONE suggestion, not one
 *     per word. A run breaks only when the cursor jumps to a non-contiguous
 *     position or the edit type changes (insert vs delete).
 *   * deletes — merge when the new delete touches the previous delete's anchor.
 *     `originFrom` (this change's pre-edit position) vs `lastPreFrom` (the previous
 *     suggestion's pre-edit anchor) orders the merged text (backspace → prepend,
 *     forward-delete → append) so Reject restores the exact original bytes.
 * Coalescing is purely local to this review layer; the underlying text/CRDT is
 * untouched. The merged item keeps the original's author and createdAt (via the
 * `{ ...last, ... }` spread), so a whole run reads as one reviewer at one time.
 */
function addCoalesced(review: ReviewState, item: ReviewItem, originFrom: number, lastPreFrom: number): ReviewState {
  if (item.kind === "suggestion") {
    const last = review.items[review.items.length - 1]
    if (last && last.kind === "suggestion" && last.status === "open" && last.change === item.change && last.author === item.author && last.text !== undefined && item.text !== undefined) {
      const contiguous = item.change === "insert"
        ? item.from === last.to
        : item.from === last.from
      if (contiguous) {
        const merged: ReviewItem =
          item.change === "insert"
            ? { ...last, to: item.to, text: last.text + item.text }
            : { ...last, text: originFrom < lastPreFrom ? item.text + last.text : last.text + item.text }
        return { ...review, items: [...review.items.slice(0, -1), merged] }
      }
    }
  }
  return reviewReducer(review, { type: "add", item })
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

function openReferences(): void {
  const project = state.project
  if (!project) { showPanel("Project library", "References", `<p class="empty-state">Select a project first.</p>`); return }
  showPanel(
    "Project library",
    "References",
    `<label>Filter by title, DOI, or PMID<input id="reference-filter" type="search" placeholder="Filter the project library" /></label>
     <button id="add-reference" class="toolbar-button" type="button">Add reference</button>
     <div id="reference-results" class="reference-results"></div>
     <p class="panel-note">References cited in the document need an attached PDF before the project can be exported.</p>`
  )
  el<HTMLInputElement>("#reference-filter")?.addEventListener("input", (event) => {
    renderReferences((event.target as HTMLInputElement).value)
  })
  el("#add-reference")?.addEventListener("click", () => openAddReferenceForm(project.id))
  // The library is project-scoped and small, so filtering happens here rather than
  // as a server round trip per keystroke.
  run(api.listReferences(project.id), (references) => { state.references = references; renderReferences() })
  run(api.listFulltexts(project.id), (fulltexts) => {
    state.fulltexts = new Map(fulltexts.map((item) => [item.reference_id, item]))
    renderReferences()
  })
}

/**
 * The multi-field add-reference form.
 *
 * `ReferenceMetadata` is rich (authors, year, doi, journal, pmid, extra) and the
 * app rejects a POST that omits the non-optional `authors`/`extra` fields
 * (422 "missing field authors"/"extra"), so collecting just a title — as the old
 * single `promptInPanel` did — could never build a usable bibliography. This
 * reuses the existing `#workspace-panel` dialog with a small `<form>` of labeled
 * inputs; on submit it always sends `authors` as an array (split on comma/newline)
 * and `extra: {}`, so the wire shape matches `ReferenceMetadata` exactly. Title is
 * the only required field (mirrors the previous non-empty guard); other fields
 * default to null when blank, which the schema accepts.
 */
function openAddReferenceForm(projectId: string): void {
  showPanel("Project library", "Add reference", `
    <label>Title<input id="ref-title" type="text" placeholder="Reference title" autocomplete="off" /></label>
    <label>Authors<textarea id="ref-authors" placeholder="Comma- or newline-separated" autocomplete="off"></textarea></label>
    <label>Year<input id="ref-year" type="number" min="0" placeholder="2024" autocomplete="off" /></label>
    <label>DOI<input id="ref-doi" type="text" placeholder="10.xxxx/xxxxx" autocomplete="off" /></label>
    <label>Journal<input id="ref-journal" type="text" placeholder="Journal" autocomplete="off" /></label>
    <label>PMID<input id="ref-pmid" type="text" placeholder="PMID" autocomplete="off" /></label>
    <p class="prompt-error" id="prompt-error" hidden></p>
    <div class="dialog-actions">
      <button class="toolbar-button" id="ref-cancel" type="button">Cancel</button>
      <button class="primary-button" id="ref-add" type="button" disabled>Add</button>
    </div>`)
  const titleInput = el<HTMLInputElement>("#ref-title")
  const authorsInput = el<HTMLTextAreaElement>("#ref-authors")
  const yearInput = el<HTMLInputElement>("#ref-year")
  const doiInput = el<HTMLInputElement>("#ref-doi")
  const journalInput = el<HTMLInputElement>("#ref-journal")
  const pmidInput = el<HTMLInputElement>("#ref-pmid")
  const addButton = el<HTMLButtonElement>("#ref-add")
  const cancelButton = el<HTMLButtonElement>("#ref-cancel")
  const errorNode = el<HTMLElement>("#prompt-error")
  const dialog = el<HTMLDialogElement>("#workspace-panel")
  if (!titleInput || !authorsInput || !yearInput || !doiInput || !journalInput || !pmidInput || !addButton || !cancelButton || !errorNode || !dialog) return

  const titleValue = (): string => titleInput.value.trim()
  const refresh = (): void => {
    const empty = titleValue() === ""
    addButton.disabled = empty
    errorNode.hidden = !empty
    errorNode.textContent = empty ? "Title is required." : ""
  }
  // Parse authors as an array, splitting on commas or newlines and trimming each
  // entry. Blank lines/entries are dropped so the array has no empty strings; an
  // all-blank box yields [], which the schema accepts.
  const authorsValue = (): readonly string[] =>
    authorsInput.value.split(/[\n,]/).map((entry) => entry.trim()).filter((entry) => entry !== "")
  // Year is optional; a blank box or non-numeric value is sent as null.
  const yearValue = (): number | null => {
    const raw = yearInput.value.trim()
    if (raw === "") return null
    const parsed = Number(raw)
    return Number.isFinite(parsed) ? parsed : null
  }
  // Optional text fields: blank -> null to match the schema's Option<...>.
  const textValue = (input: HTMLInputElement): string | null => {
    const raw = input.value.trim()
    return raw === "" ? null : raw
  }

  // Guards against the close event firing twice (Esc after programmatic close) and
  // against Enter-after-add. On Add the References panel is re-opened so the new
  // entry (and its full-text state) renders; Cancel/Esc just closes.
  let done = false
  const finish = (result: "add" | "cancel"): void => {
    if (done) return
    done = true
    dialog.removeEventListener("close", handleClose)
    titleInput.removeEventListener("input", refresh)
    addButton.removeEventListener("click", handleAdd)
    cancelButton.removeEventListener("click", handleCancel)
    dialog.close()
    if (result === "add") {
      run(
        api.createReference(projectId, {
          title: titleValue(),
          authors: authorsValue(),
          year: yearValue(),
          doi: textValue(doiInput),
          journal: textValue(journalInput),
          pmid: textValue(pmidInput),
          extra: {}
        }),
        (reference) => {
          state.references = [...state.references, reference]
          status(`Added ${reference.metadata.title}`)
          // Re-open the panel so the new entry shows in the filtered list and any
          // pending full-text fetch re-runs.
          openReferences()
        }
      )
    }
  }
  const handleClose = (): void => finish("cancel")
  const handleAdd = (): void => { if (titleValue() !== "") finish("add") }
  const handleCancel = (): void => finish("cancel")

  dialog.addEventListener("close", handleClose)
  titleInput.addEventListener("input", refresh)
  addButton.addEventListener("click", handleAdd)
  cancelButton.addEventListener("click", handleCancel)
  titleInput.focus()
}

function matchesFilter(reference: Reference, needle: string): boolean {
  if (!needle) return true
  const haystack = [reference.metadata.title, reference.metadata.doi, reference.metadata.pmid, ...reference.metadata.authors]
    .filter((value): value is string => typeof value === "string")
    .join(" ")
    .toLowerCase()
  return haystack.includes(needle.toLowerCase())
}

function renderReferences(filter = ""): void {
  const target = el<HTMLElement>("#reference-results")
  const project = state.project
  if (!target || !project) return
  const items = state.references.filter((reference) => matchesFilter(reference, filter))
  if (items.length === 0) {
    target.innerHTML = `<p>${state.references.length === 0 ? "This project has no references yet." : "No references match that filter."}</p>`
    return
  }
  target.innerHTML = items
    .map((reference) => {
      const fulltext = state.fulltexts.get(reference.id)
      // Authors collapse to "First et al." so a long author list stays scannable.
      // Each reference's metadata was entered via the add-reference form, so showing
      // "Title — Authors, Year, DOI" lets the author confirm what they typed.
      const authors = reference.metadata.authors.length > 0 ? `${escapeHtml(reference.metadata.authors[0]!)}${reference.metadata.authors.length > 1 ? " et al." : ""}` : "No authors"
      const identifier = reference.metadata.doi ?? reference.metadata.pmid ?? "No identifier"
      const journal = reference.metadata.journal ? ` · ${escapeHtml(reference.metadata.journal)}` : ""
      const attachment = fulltext
        ? `<span class="fulltext-ok">PDF attached · ${escapeHtml(fulltext.filename)}</span>`
        : `<span class="fulltext-warning">PDF missing; export blocked if cited</span><button type="button" class="toolbar-button" data-upload="${escapeHtml(reference.id)}">Attach PDF</button>`
      return `<article class="reference-item"><strong>${escapeHtml(reference.metadata.title)}</strong><span>${authors}${reference.metadata.year === null ? "" : ` · ${reference.metadata.year}`}${journal} · ${escapeHtml(identifier)}</span><button type="button" data-cite="${escapeHtml(reference.id)}" class="toolbar-button">Insert citation</button>${attachment}<button type="button" class="toolbar-button" data-delete="${escapeHtml(reference.id)}" title="Remove this reference">Delete</button></article>`
    })
    .join("")

  for (const button of target.querySelectorAll<HTMLButtonElement>("[data-cite]")) {
    button.addEventListener("click", () => {
      // `#cite(<id>)` is the label form the exporter's citation scanner reads.
      editor.dispatch({ changes: { from: editor.state.selection.main.head, insert: `#cite(<${button.dataset.cite}>)` } })
      editor.focus()
      el<HTMLDialogElement>("#workspace-panel")?.close()
    })
  }
  for (const button of target.querySelectorAll<HTMLButtonElement>("[data-upload]")) {
    button.addEventListener("click", () => {
      const referenceId = button.dataset.upload
      if (!referenceId) return
      const input = document.createElement("input")
      input.type = "file"
      input.accept = "application/pdf"
      input.addEventListener("change", () => {
        const file = input.files?.[0]
        if (!file) return
        status(`Uploading ${file.name}…`)
        run(api.uploadFulltext(project.id, referenceId, file), (fulltext) => {
          state.fulltexts = new Map(state.fulltexts).set(fulltext.reference_id, fulltext)
          status(`Attached ${fulltext.filename}`)
          renderReferences(filter)
        })
      })
      input.click()
    })
  }
  for (const button of target.querySelectorAll<HTMLButtonElement>("[data-delete]")) {
    button.addEventListener("click", () => {
      const referenceId = button.dataset.delete
      if (!referenceId) return
      const reference = state.references.find((r) => r.id === referenceId)
      // Confirm via the in-app panel (not the native confirm) so the destructive
      // action matches the rest of the UI and is unambiguous about the title.
      promptInPanel(
        "References",
        "Remove reference",
        `Type the title to confirm removal of "${reference?.metadata.title ?? "this reference"}"`,
        (confirmText) => {
          if (confirmText.trim() !== (reference?.metadata.title ?? "").trim()) {
            status("The typed text did not match; reference was not removed")
            return
          }
          run(api.deleteReference(project.id, referenceId), () => {
            status("Reference removed")
            // Re-fetch so the list and any fulltext attachment state are consistent.
            run(api.listReferences(project.id), (references) => { state.references = references; renderReferences(filter) })
          }, (error) => {
            // Reopen the references panel so the author sees the failure in context.
            openReferences()
            status(error instanceof Error ? error.message : "Could not remove the reference")
          })
        },
        { placeholder: "Retype the reference title to confirm", onClose: openReferences }
      )
    })
  }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/**
 * Share / Invite panel (L3). Lists current members and lets an owner/author
 * invite a collaborator by name + role via the existing membership API. Before
 * this, the only way to add a member was a direct REST call — the web client had
 * no affordance, so a reviewer could never see a project until someone POSTed
 * out-of-band.
 */
function openShare(): void {
  const project = state.project
  if (!project) { showPanel("Sharing", "Share", `<p class="empty-state">Select a project first.</p>`); return }
  showPanel(
    "Sharing",
    "Share",
    `<p class="panel-note">Invite a collaborator by their username (e.g. <code>alice@nisaba.local</code>). They'll see this project after their next sign-in.</p>
     <div id="share-invite-form" class="share-invite">
       <input id="share-subject" type="text" placeholder="Username to invite" autocomplete="off" />
       <select id="share-role" aria-label="Role">
         <option value="author">Author — edit and comment</option>
         <option value="reviewer">Reviewer — suggest and comment</option>
         <option value="read-only">Read-only — view only</option>
       </select>
       <button id="share-invite" class="primary-button" type="button">Invite</button>
     </div>
     <h3 class="share-members-heading">Current members</h3>
     <div id="share-members" class="reference-results"><p class="panel-note">Loading…</p></div>
     <h3 class="share-members-heading">Shareable links</h3>
     <p class="panel-note">Generate a link that grants access to this project at a chosen role. Anyone who opens the link while signed in gets that role.</p>
     <div class="share-invite">
       <select id="share-link-role" aria-label="Link role">
         <option value="reviewer">Reviewer</option>
         <option value="author">Author</option>
         <option value="read-only">Read-only</option>
       </select>
       <button id="create-share-link" class="toolbar-button" type="button">Create link</button>
     </div>
     <div id="share-links" class="reference-results"><p class="panel-note">No shareable links yet.</p></div>`
  )
  const renderMembers = (members: readonly api.Membership[]) => {
    const host = el<HTMLElement>("#share-members")
    if (!host) return
    host.innerHTML = members.length === 0
      ? `<p class="panel-note">No members yet.</p>`
      : members.map((m) => `<article class="reference-item"><strong>${escapeHtml(m.subject)}</strong><span>${escapeHtml(m.role)}</span></article>`).join("")
  }
  run(api.listMembers(project.id), renderMembers, () => { const h = el("#share-members"); if (h) h.innerHTML = `<p class="panel-note">Couldn't load members (you may not have permission).</p>` })
  // The panel host (#workspace-panel) already wraps content in a <form
  // method="dialog">, so a nested <form> here is illegal HTML and the browser
  // drops it (silently breaking submit). Use a <div> + button click instead,
  // and submit on Enter for keyboard parity.
  const submitInvite = (): void => {
    const subject = (el<HTMLInputElement>("#share-subject")?.value ?? "").trim()
    const role = (el<HTMLSelectElement>("#share-role")?.value ?? "reviewer") as api.MembershipRole
    if (!subject) return
    const inviteButton = el<HTMLButtonElement>("#share-invite")
    if (inviteButton) { inviteButton.disabled = true; inviteButton.textContent = "Inviting…" }
    run(
      api.addMember(project.id, subject, role),
      () => {
        status(`Invited ${subject} as ${role}`)
        const subjectInput = el<HTMLInputElement>("#share-subject")
        if (subjectInput) subjectInput.value = ""
        if (inviteButton) { inviteButton.disabled = false; inviteButton.textContent = "Invite" }
        run(api.listMembers(project.id), renderMembers)
      },
      (error: unknown) => {
        if (inviteButton) { inviteButton.disabled = false; inviteButton.textContent = "Invite" }
        status(error instanceof Error ? error.message : "Couldn't add member")
      }
    )
  }
  el("#share-invite")?.addEventListener("click", submitInvite)
  el<HTMLInputElement>("#share-subject")?.addEventListener("keydown", (event) => {
    if (event.key === "Enter") { event.preventDefault(); submitInvite() }
  })

  // Shareable links: create, copy, and revoke.
  const renderShareLinks = (links: readonly api.ShareLink[]): void => {
    const host = el<HTMLElement>("#share-links")
    if (!host) return
    host.innerHTML = links.length === 0
      ? `<p class="panel-note">No shareable links yet.</p>`
      : links.map((link) => {
          const url = `${window.location.origin}/?share=${encodeURIComponent(link.token)}`
          return `<div class="share-link-row"><code>${escapeHtml(url)}</code><button type="button" class="toolbar-button" data-copy="${escapeHtml(url)}">Copy</button><button type="button" class="toolbar-button" data-revoke="${escapeHtml(link.token)}">Revoke</button></div>`
        }).join("")
    for (const button of host.querySelectorAll<HTMLButtonElement>("[data-copy]")) {
      button.addEventListener("click", () => {
        void navigator.clipboard.writeText(button.dataset.copy ?? "").then(() => { button.textContent = "Copied!"; setTimeout(() => { button.textContent = "Copy" }, 1500) }, () => { button.textContent = "Copy failed"; setTimeout(() => { button.textContent = "Copy" }, 1500) })
      })
    }
    for (const button of host.querySelectorAll<HTMLButtonElement>("[data-revoke]")) {
      button.addEventListener("click", () => {
        run(api.deleteShareLink(project.id, button.dataset.revoke ?? ""), () => {
          status("Share link revoked")
          run(api.listShareLinks(project.id), renderShareLinks)
        })
      })
    }
  }
  run(api.listShareLinks(project.id), renderShareLinks, () => undefined)
  el("#create-share-link")?.addEventListener("click", () => {
    const role = el<HTMLSelectElement>("#share-link-role")?.value ?? "reviewer"
    const createButton = el<HTMLButtonElement>("#create-share-link")
    if (createButton) { createButton.disabled = true; createButton.textContent = "Creating…" }
    run(
      api.createShareLink(project.id, role),
      () => {
        status("Share link created")
        if (createButton) { createButton.disabled = false; createButton.textContent = "Create link" }
        run(api.listShareLinks(project.id), renderShareLinks)
      },
      (error: unknown) => {
        if (createButton) { createButton.disabled = false; createButton.textContent = "Create link" }
        status(error instanceof Error ? error.message : "Couldn't create link")
      }
    )
  })
}

/**
 * Computes a simple line-by-line diff using LCS (longest common subsequence).
 *
 * Returns an array of { type: "added" | "removed" | "context", text } entries.
 * This is intentionally a minimal diff (not Myers or patience) — it runs in the
 * browser on the document body, which for a single document is at most a few
 * thousand lines. The LCS table is O(n*m) but the constant is tiny.
 */
/** Per-side cap above which the LCS table is too risky to build. */
const MAX_DIFF_LINES = 2000
/** Product cap: even two moderately long sides multiply into a huge table. */
const MAX_DIFF_PRODUCT = 1_000_000

function lineDiff(oldText: string, newText: string): { type: "added" | "removed" | "context"; text: string }[] {
  const oldLines = oldText.split("\n")
  const newLines = newText.split("\n")
  // The LCS table is O(n*m). Two thousand-line sides already need a 4M-cell
  // matrix; five-thousand-line sides (the old cap) needed 25M cells and froze
  // the tab. Guard on BOTH the product and either side so no large pairing can
  // reach the table. Fallback is a naive O(n+m) pairwise walk: not as smart as
  // LCS but it never allocates the matrix and still highlights differences.
  if (oldLines.length > MAX_DIFF_LINES || newLines.length > MAX_DIFF_LINES ||
      oldLines.length * newLines.length > MAX_DIFF_PRODUCT) {
    const result: { type: "added" | "removed" | "context"; text: string }[] = []
    const max = Math.max(oldLines.length, newLines.length)
    for (let k = 0; k < max; k++) {
      const o = oldLines[k]
      const n = newLines[k]
      if (o !== undefined && n !== undefined && o === n) {
        result.push({ type: "context", text: o })
      } else {
        if (o !== undefined) result.push({ type: "removed", text: o })
        if (n !== undefined) result.push({ type: "added", text: n })
      }
    }
    return result
  }
  // Build LCS length table.
  const dp: number[][] = Array.from({ length: oldLines.length + 1 }, () => Array.from({ length: newLines.length + 1 }, () => 0))
  for (let i = oldLines.length - 1; i >= 0; i--) {
    for (let j = newLines.length - 1; j >= 0; j--) {
      const oldLine = oldLines[i] ?? ""
      const newLine = newLines[j] ?? ""
      dp[i]![j] = oldLine === newLine ? (dp[i + 1]?.[j + 1] ?? 0) + 1 : Math.max(dp[i + 1]?.[j] ?? 0, dp[i]?.[j + 1] ?? 0)
    }
  }
  // Backtrack to produce the diff.
  const result: { type: "added" | "removed" | "context"; text: string }[] = []
  let i = 0, j = 0
  while (i < oldLines.length && j < newLines.length) {
    const oldLine = oldLines[i] ?? ""
    const newLine = newLines[j] ?? ""
    if (oldLine === newLine) {
      result.push({ type: "context", text: oldLine })
      i++; j++
    } else if ((dp[i + 1]?.[j] ?? 0) >= (dp[i]?.[j + 1] ?? 0)) {
      result.push({ type: "removed", text: oldLine })
      i++
    } else {
      result.push({ type: "added", text: newLine })
      j++
    }
  }
  while (i < oldLines.length) { result.push({ type: "removed", text: oldLines[i++] ?? "" }) }
  while (j < newLines.length) { result.push({ type: "added", text: newLines[j++] ?? "" }) }
  return result
}

/**
 * Opens the version history panel for the current document.
 *
 * Shows a timeline of saved revisions (newest first). Clicking a revision loads
 * its full body; clicking a second one shows a line-by-line diff between the two.
 * The current document body (live editor text) is available as the "Current"
 * pseudo-revision so the user can diff the working copy against any saved version.
 */
function openHistory(): void {
  const { project, selected } = state
  if (!project || !selected) { showPanel("History", "History", `<p class="empty-state">Select a document first.</p>`); return }
  showPanel("History", "Version history", `<p class="panel-note">Loading revisions…</p>`)
  run(
    api.listDocumentHistory(project.id, selected.document.id),
    (revisions) => {
      const host = el<HTMLElement>("#panel-content")
      if (!host) return
      if (revisions.length === 0) {
        host.innerHTML = `<p class="empty-state">No saved revisions yet. Edits are snapshotted automatically on save.</p>`
        return
      }
      // The "Current" pseudo-entry represents the live editor text, so the user
      // can diff the working copy against any saved version.
      const currentBody = editor.state.doc.toString()
      const entries = [
        { id: "current", revision: state.document?.revision ?? 0, author: "—", created_at: new Date().toISOString(), body: currentBody, isCurrent: true },
        ...revisions.map((r) => ({ ...r, isCurrent: false }))
      ]
      let firstSelected: string | null = null
      host.innerHTML = `
        <p class="panel-note">Click one revision to view it. Click a second to diff against the first.</p>
        <div class="history-layout">
          <ul class="history-timeline" id="history-timeline">
            ${entries.map((entry) => `<li class="history-entry" data-rev="${escapeHtml(entry.id)}"><span class="history-entry-rev">${entry.isCurrent ? "Current" : `Rev ${entry.revision}`}</span><span class="history-entry-meta">${escapeHtml(entry.author ?? "—")} · ${new Date(entry.created_at).toLocaleString()}</span></li>`).join("")}
          </ul>
          <div class="history-diff-pane" id="history-diff-pane"><div class="history-empty">Select a revision to view.</div></div>
        </div>`
      const bodies = new Map(entries.map((e) => [e.id, e.body]))
      const pane = el<HTMLElement>("#history-diff-pane")
      const timeline = el<HTMLElement>("#history-timeline")
      const renderSelection = (): void => {
        if (!timeline) return
        for (const entry of timeline.querySelectorAll<HTMLElement>(".history-entry")) {
          const revId = entry.dataset.rev
          entry.classList.toggle("selected", revId === firstSelected)
        }
      }
      timeline?.addEventListener("click", (event) => {
        const target = (event.target as HTMLElement).closest<HTMLElement>(".history-entry")
        if (!target || !pane) return
        const revId = target.dataset.rev ?? ""
        if (firstSelected === null) {
          // First selection: show the body.
          firstSelected = revId
          renderSelection()
          const body = bodies.get(revId) ?? ""
          pane.innerHTML = `<pre>${body.split("\n").map((line) => `<span class="diff-line-context">${escapeHtml(line)}</span>`).join("\n")}</pre>`
        } else if (firstSelected === revId) {
          // Click same: deselect.
          firstSelected = null
          renderSelection()
          pane.innerHTML = `<div class="history-empty">Select a revision to view.</div>`
        } else {
          // Second selection: diff firstSelected → revId.
          const oldBody = bodies.get(firstSelected) ?? ""
          const newBody = bodies.get(revId) ?? ""
          const diff = lineDiff(oldBody, newBody)
          pane.innerHTML = `<pre>${diff.map((line) => `<span class="diff-line-${line.type}">${escapeHtml(line.type === "added" ? "+" : line.type === "removed" ? "-" : " ")}${escapeHtml(line.text)}</span>`).join("\n")}</pre>`
        }
      })
    },
    (error: unknown) => {
      const host = el<HTMLElement>("#panel-content")
      if (host) host.innerHTML = `<p class="empty-state">Couldn't load history: ${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`
    }
  )
}

function openExport(): void {
  const project = state.project
  if (!project) { showPanel("Export", "Export", `<p class="empty-state">Select a project first.</p>`); return }
  const entries = state.outline.map(({ document }) => `<option value="${escapeHtml(document.path)}">${escapeHtml(document.title)} — ${escapeHtml(document.path)}</option>`).join("")
  showPanel("Export", "Export", `
    <label>Entry document<select id="export-entry">${entries}</select></label>
    <button id="run-export" class="primary-button" type="button" ${entries ? "" : "disabled"}>Export project</button>
    <div id="export-result" class="panel-note"></div>`)
  el("#run-export")?.addEventListener("click", () => {
    const entry = el<HTMLSelectElement>("#export-entry")?.value
    if (!entry) return
    const exportButton = el<HTMLButtonElement>("#run-export")
    if (exportButton) exportButton.disabled = true
    setText("#export-result", "Exporting…")
    run(api.exportProject(project.id, entry, "full", state.view), (result) => {
      if (exportButton) exportButton.disabled = false
      const files = result.references.files
      const pdf = result.compile.pdf_base64
      const zip = result.zip_base64
      const zipName = result.zip_filename ?? `${project.name}.zip`
      const host = el<HTMLElement>("#export-result")
      if (!host) return
      host.innerHTML = `<p>Build ${escapeHtml(result.compile.build_id)} · ${files.length} reference file${files.length === 1 ? "" : "s"}</p>
        ${zip ? `<p><button id="download-zip" class="primary-button" type="button">Download export bundle</button> <code>${escapeHtml(zipName)}</code></p>` : ""}
        ${pdf ? `<button id="download-pdf" class="toolbar-button" type="button">Download PDF</button>` : `<p class="fulltext-warning">The compile produced no PDF.</p>`}
        <ul class="export-files">${files.map((file, index) => `<li><button type="button" class="link-button" data-file="${index}">${escapeHtml(file.path)}</button></li>`).join("")}</ul>`
      el("#download-zip")?.addEventListener("click", () => { try { if (zip) downloadBase64(zip, zipName, "application/zip") } catch { status("Download failed: corrupt data") } })
      el("#download-pdf")?.addEventListener("click", () => { try { if (pdf) downloadBase64(pdf, `${project.name}.pdf`, "application/pdf") } catch { status("Download failed: corrupt data") } })
      for (const button of host.querySelectorAll<HTMLButtonElement>("[data-file]")) {
        button.addEventListener("click", () => {
          const file = files[Number(button.dataset.file)]
          if (file) { try { downloadBase64(file.content_base64, file.path.split("/").pop() ?? "reference", "application/octet-stream") } catch { status("Download failed: corrupt data") } }
        })
      }
    }, (error) => {
      if (exportButton) exportButton.disabled = false
      const host = el<HTMLElement>("#export-result")
      if (host) host.innerHTML = `<p class="fulltext-warning">Export failed: ${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`
    })
  })
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

function renderReviewBanner(): void {
  const banner = el<HTMLElement>("#review-banner")
  if (!banner) return
  const open = state.review.items.filter((item) => item.status === "open").length
  banner.hidden = open === 0 && !state.review.suggesting
  setText("#review-summary", open === 0 ? "No open review items" : `${open} open review item${open === 1 ? "" : "s"}`)
  setText("#suggesting-button", `Track changes: ${state.review.suggesting ? "on" : "off"}`)
  // The banner's suggesting toggle must be disabled for reviewers too, not just
  // the toolbar toggle below: a reviewer is locked into suggesting mode (H1), but
  // the banner button (visible whenever there are open items or suggesting is on)
  // was never disabled — letting a reviewer click it to turn tracking off and
  // make untracked edits.
  const bannerToggle = el<HTMLButtonElement>("#suggesting-button")
  if (bannerToggle) bannerToggle.disabled = state.role === "reviewer"
  // The toolbar toggle is the always-reachable Track Changes affordance: the
  // banner's #suggesting-button is hidden on a clean doc (banner.hidden above),
  // so without this a reviewer can never turn track changes on in the first
  // place. It is disabled until a document is open.
  const toolbarToggle = el<HTMLButtonElement>("#toolbar-suggesting")
  if (toolbarToggle) {
    // Reviewers are locked into suggesting mode (H1): they cannot turn track
    // changes off, so the toggle is disabled but still reflects the on state.
    const reviewerLocked = state.role === "reviewer"
    toolbarToggle.disabled = !state.selected || reviewerLocked
    toolbarToggle.setAttribute("aria-pressed", String(state.review.suggesting))
    toolbarToggle.classList.toggle("is-on", state.review.suggesting)
    setText("#toolbar-suggesting", `Track changes: ${state.review.suggesting ? "on" : "off"}`)
  }
}

/**
 * Apply project-role UI gates. Called when the membership fetch resolves
 * (openProject) and whenever role-dependent chrome may need re-rendering.
 *
 * - H1: a reviewer is forced into suggesting mode and cannot turn it off, so
 *   every body edit they make is recorded as a suggestion rather than a silent
 *   overwrite. The lock is enforced here (UI) and relies on the server already
 *   permitting reviewer document writes (needed for suggestion-mode edits).
 * - M4: reviewers and read-only viewers cannot export (the server returns 403),
 *   so the Export button is hidden up-front rather than failing on click.
 */
function applyRoleGates(): void {
  const canManage = state.role === undefined || state.role === "owner" || state.role === "author"
  const canWrite = canManage || state.role === "reviewer"
  const readOnly = state.role === "read-only"
  const exportButton = el<HTMLElement>("#export-button")
  if (exportButton) exportButton.hidden = !canManage
  // Share/Invite is available to owners and authors (the roles the server allows
  // to manage members). Reviewers and read-only viewers can't add members, so
  // the button stays hidden for them (L3).
  const shareButton = el<HTMLElement>("#share-button")
  if (shareButton) shareButton.hidden = !canManage
  // History is read-only and useful for all members.
  const historyButton = el<HTMLElement>("#history-button")
  if (historyButton) historyButton.hidden = !state.selected

  // Read-only viewers: disable all write controls and make the editor read-only
  // so they cannot type into it and believe their edits are saved. The backend
  // also rejects these (403), but the UI should not expose the controls at all.
  if (editor) editor.dispatch({ effects: editableComp.reconfigure(EditorView.editable.of(!readOnly)) })

  const deleteProjectBtns = document.querySelectorAll<HTMLButtonElement>(".delete-project-btn")
  deleteProjectBtns.forEach((btn) => { btn.hidden = readOnly; btn.disabled = readOnly })
  const addDocBtn = el<HTMLButtonElement>("#add-document-btn")
  if (addDocBtn) { addDocBtn.hidden = readOnly; addDocBtn.disabled = readOnly }
  const deleteDocBtns = document.querySelectorAll<HTMLButtonElement>(".delete-document-btn")
  deleteDocBtns.forEach((btn) => { btn.hidden = readOnly; btn.disabled = readOnly })
  const compileBtn = el<HTMLButtonElement>("#compile-button")
  if (compileBtn) compileBtn.disabled = readOnly
  const bannerToggle = el<HTMLInputElement>("#review-banner-toggle")
  if (bannerToggle) bannerToggle.disabled = readOnly || state.role === "reviewer"
  const addRefBtn = el<HTMLButtonElement>("#add-reference-btn")
  if (addRefBtn) { addRefBtn.hidden = !canWrite; addRefBtn.disabled = !canWrite }

  if (state.role === "reviewer" && !state.review.suggesting) {
    // Force suggesting on once, on role resolution; renderReviewBanner keeps the
    // toggle disabled so the reviewer cannot turn it back off.
    state.review = reviewReducer(state.review, { type: "toggle-suggesting" })
  }
  renderReviewBanner()
  renderReviewSidebar()
}

/**
 * Inline review popover (Overleaf-style).
 *
 * The Review dialog stays as the overview, but the primary interaction is inline: clicking
 * a comment anchor, a highlighted comment range, or a suggestion span opens a small
 * panel anchored to the clicked element showing the thread text + author + the same
 * Resolve/Accept/Reject actions the dialog offers. One shared element is created lazily
 * and repositioned per open via `getBoundingClientRect` of the anchor; it is appended to
 * `#app` (not the scrolling editor) so it stays fixed relative to the viewport until the
 * user scrolls, at which point it simply closes (re-open by clicking again) — simpler and
 * more robust than tracking the anchor across scroll/resize.
 */
let reviewPopover: HTMLElement | undefined

/** The id of the item currently shown in the popover, or undefined when closed. */
let reviewPopoverItemId: string | undefined

function ensureReviewPopover(): HTMLElement {
  if (reviewPopover) return reviewPopover
  const node = document.createElement("div")
  node.className = "review-popover"
  node.setAttribute("role", "dialog")
  node.setAttribute("aria-label", "Review thread")
  node.hidden = true
  root?.append(node)
  reviewPopover = node
  // Global dismiss listeners. ensureReviewPopover runs once (the element is cached), so
  // these bind once; repeated opens just reposition/re-render the same node.
  const close = closeReviewPopover
  document.addEventListener("mousedown", (event) => {
    if (!reviewPopover || reviewPopover.hidden) return
    const target = event.target as Node | null
    if (target && reviewPopover.contains(target)) return
    // A click on a review anchor/scope is handled by its own opener (which re-renders);
    // any other click outside closes the popover.
    if (target instanceof HTMLElement && target.closest("[data-review-id]")) return
    close()
  }, true)
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && reviewPopover && !reviewPopover.hidden) { event.stopPropagation(); close() }
  }, true)
  document.addEventListener("visibilitychange", () => { if (document.hidden) close() })
  return node
}

function closeReviewPopover(): void {
  if (!reviewPopover || reviewPopover.hidden) return
  reviewPopover.hidden = true
  reviewPopover.replaceChildren()
  reviewPopoverItemId = undefined
}

/**
 * Opens the popover for a review item, anchored to `anchor` (the clicked widget/span).
 *
 * The item is looked up by id in the live `state.review` (the snapshot baked into the
 * widget may be stale after edits), so a re-decorated widget still opens the current
 * thread. Resolved/accepted/rejected items are no longer decorated, but a stale widget
 * click could still arrive — guard by status.
 */
function openReviewPopover(id: string, anchor: HTMLElement): void {
  const item = state.review.items.find((it) => it.id === id)
  if (!item) return
  // Toggle: clicking the already-open thread's anchor closes it (matches the dismiss
  // affordances below). Clicking a different item re-renders for the new one.
  if (reviewPopoverItemId === id && reviewPopover && !reviewPopover.hidden) {
    closeReviewPopover()
    return
  }
  const popover = ensureReviewPopover()
  reviewPopoverItemId = id
  const snippet = item.kind === "suggestion" && item.text !== undefined
    ? `<blockquote class="review-popover-snippet">“${escapeHtml(item.text.length > 200 ? `${item.text.slice(0, 200)}…` : item.text)}”</blockquote>`
    : item.kind === "comment"
      ? `<p class="review-popover-body">${escapeHtml(item.body)}</p>`
      : ""
  const kindLabel = item.kind === "comment" ? "Comment" : `Suggestion · ${item.change}`
  const change = item.kind === "suggestion"
    ? `<span class="review-popover-kind review-popover-kind-${item.change}">${escapeHtml(item.change)}</span>`
    : `<span class="review-popover-kind review-popover-kind-comment">comment</span>`
  // Avatar chip carries the author's initials on a colour derived from their name
  // (authorHue), so distinct reviewers are visually separable at a glance; the name
  // and timeAgo sit beside it. Resolved items also show who resolved and when.
  const hue = authorHue(item.author)
  const avatar = `<span class="review-avatar" style="--hue:${hue}" aria-hidden="true">${escapeHtml(authorInitials(item.author))}</span>`
  const resolved = item.resolvedAt !== undefined && item.resolvedBy
    ? `<span class="review-resolved-line">Resolved by ${escapeHtml(item.resolvedBy)} · ${escapeHtml(timeAgo(item.resolvedAt))}</span>`
    : ""
  // Comments resolve; suggestions accept/reject. The buttons reuse the same actions as
  // the dialog so behaviour (authoritative text mutation for suggestions) is identical.
  const action = item.kind === "comment"
    ? `<button class="primary-button" type="button" data-popover-resolve="${escapeHtml(item.id)}">Resolve</button>`
    : `<button class="primary-button" type="button" data-popover-accept="${escapeHtml(item.id)}">Accept</button> <button class="toolbar-button" type="button" data-popover-reject="${escapeHtml(item.id)}">Reject</button>`
  popover.innerHTML = `
    <div class="review-popover-head">
      <div class="review-popover-meta">${change}<strong>${escapeHtml(kindLabel)}</strong><span class="review-popover-author">${escapeHtml(item.author)} · ${escapeHtml(timeAgo(item.createdAt))}</span></div>
      ${avatar}
      <button class="review-popover-close" type="button" aria-label="Close thread">×</button>
    </div>
    ${snippet}
    ${resolved}
    <div class="review-popover-actions">${action}</div>`
  popover.hidden = false
  positionReviewPopover(popover, anchor)
  popover.querySelector<HTMLElement>(".review-popover-close")?.addEventListener("click", closeReviewPopover)
  for (const button of popover.querySelectorAll<HTMLButtonElement>("[data-popover-accept], [data-popover-reject], [data-popover-resolve]")) {
    button.addEventListener("click", () => {
      const itemId = button.dataset.popoverAccept ?? button.dataset.popoverReject ?? button.dataset.popoverResolve
      const target = state.review.items.find((it) => it.id === itemId)
      if (!target) return
      const type = button.dataset.popoverResolve !== undefined
        ? "resolve" as const
        : button.dataset.popoverAccept !== undefined
          ? "accept" as const
          : "reject" as const
      applyReviewItemAction(target, type)
      // Accept/reject/resolve closes the thread inline (the item is no longer open, so its
      // decoration — and thus the anchor — is gone). Resolve of a comment leaves the mark
      // too; either way the popover is dismissed.
      closeReviewPopover()
    })
  }
}

/** Positions the popover below-right of the anchor, clamped to the viewport. */
function positionReviewPopover(popover: HTMLElement, anchor: HTMLElement): void {
  const rect = anchor.getBoundingClientRect()
  const margin = 6
  // Render off-screen first to measure natural size without flashing at 0,0.
  popover.style.visibility = "hidden"
  popover.style.left = "0px"
  popover.style.top = "0px"
  const { offsetWidth: width, offsetHeight: height } = popover
  const left = Math.min(Math.max(rect.left, margin), window.innerWidth - width - margin)
  // Prefer below the anchor; flip above if it would overflow the bottom.
  const below = rect.bottom + margin
  const top = below + height + margin > window.innerHeight
    ? Math.max(margin, rect.top - height - margin)
    : below
  popover.style.left = `${Math.max(margin, left)}px`
  popover.style.top = `${top}px`
  popover.style.visibility = ""
}

/**
 * Applies a single-item review action shared by the dialog and the popover.
 *
 * For a suggestion reject, performs the authoritative text mutation (restore a delete,
 * remove an insert) wrapped in `resolvingSuggestions`/`resolveAnnotation` so the review
 * tracker does not re-record it as a new suggestion (see the dialog handler's comment).
 * Accept and resolve need no text change. Always dispatches the reducer action and
 * re-renders the banner + decorations.
 */
function applyReviewItemAction(item: ReviewItem, type: "accept" | "reject" | "resolve"): void {
  if (type === "reject" && item.kind === "suggestion") {
    resolvingSuggestions = true
    try {
      if (item.change === "delete" && item.text) {
        // Edge case: if the text around item.from has since been edited by an
        // unrelated change, item.from may be stale (offset-based tracking remaps
        // positions through edits, but cannot verify semantic correctness). The
        // insert-reject branch below guards with a content check; here the
        // deleted text is absent from the doc so there is nothing to match
        // against. Clamping to doc length prevents a RangeError; the position is
        // already remapped by updateReviewItems on every prior edit.
        const insertAt = Math.min(item.from, editor.state.doc.length)
        editor.dispatch({ annotations: resolveAnnotation.of(true), changes: { from: insertAt, insert: item.text } })
      } else if (item.change === "insert") {
        // Guard: only remove the suggested text if it still matches what's in the
        // doc. If the user deleted or modified it in the meantime, the silent
        // no-op avoids corrupting surrounding text at a stale offset.
        const from = item.from
        const to = Math.min(item.to, editor.state.doc.length)
        if (editor.state.doc.sliceString(from, to) === item.text) {
          editor.dispatch({ annotations: resolveAnnotation.of(true), changes: { from, to, insert: "" } })
        }
      }
    } finally {
      resolvingSuggestions = false
    }
  }
  state.review = reviewReducer(state.review, { type, id: item.id, by: currentUserDisplayName(), at: Date.now() })
  editor.dispatch({ effects: setReviewItems.of(state.review.items) })
  renderReviewBanner()
  renderReviewSidebar()
  persistReview()
}

/**
 * Scrolls the editor to a review item's anchor and opens its popover, bridging the
 * Review overview dialog and the inline view. Used by the dialog list click handler.
 */
function revealReviewItem(item: ReviewItem): void {
  const length = editor.state.doc.length
  const head = Math.min(item.from, length)
  editor.dispatch({ selection: { anchor: head, head: Math.min(Math.max(item.to, head), length) }, scrollIntoView: true })
  editor.focus()
  // Defer so the scroll lands and the anchor widget/mark is re-rendered at its on-screen
  // position before we anchor the popover to it; requestAnimationFrame yields after layout.
  // Lookup is by attribute comparison (not a selector built from the id) so an id with
  // selector metacharacters can never break the query.
  requestAnimationFrame(() => {
    const node = Array.from(editor.contentDOM.querySelectorAll<HTMLElement>("[data-review-id]"))
      .find((element) => element.getAttribute("data-review-id") === item.id)
    if (node) openReviewPopover(item.id, node)
  })
}

/**
 * Editor-wide click handler for review marks (`.review-comment`/`.review-suggestion`).
 *
 * Anchor widgets bind their own click in their WidgetType; ranges are marks, which render
 * as plain spans, so they are resolved here by walking up to the `[data-review-id]` the
 * decoration stamps on them. Bound via EditorView.domEventHandlers in the editor extension
 * list so the handler has access to the view (unused here but keeps the wiring local).
 */
function handleReviewClick(event: MouseEvent): void {
  const target = event.target
  if (!(target instanceof HTMLElement)) return
  const tagged = target.closest("[data-review-id]")
  if (!tagged) return
  const id = tagged.getAttribute("data-review-id")
  if (!id) return
  // No preventDefault: a click inside a commented range still places the cursor (matches
  // Overleaf, where commenting also relocates the caret), which is the expected behaviour.
  openReviewPopover(id, tagged as HTMLElement)
}

// ---------------------------------------------------------------------------
// Review sidebar (Overleaf-style docked column)
// ---------------------------------------------------------------------------

/**
 * Persistent review sidebar that replaces the modal Review overview.
 *
 * It mirrors the inline `.review-popover` actions at a glance: every open item
 * renders as a card (author, kind chip, text, Resolve/Accept/Reject), clicking a
 * card jumps the editor to its anchor via `revealReviewItem`, and the toolbar
 * carries the suggesting toggle + comment-at-cursor affordance. Accept-all /
 * reject-all appear at the foot when open suggestions exist. The pane docks as a
 * 4th workspace column (`applyWorkspaceGrid` adds its track + gutter); it is
 * `display:none` otherwise so it does not reserve space.
 *
 * `renderReviewSidebar()` is idempotent and called after every review mutation
 * (openDocument reset, banner re-render, suggestion tracking, accept/reject,
 * toggle-suggesting). When closed it is a no-op, so review state still mutates
 * correctly with the pane hidden.
 */
function renderReviewSidebar(): void {
  const host = el<HTMLElement>("#review-sidebar-body")
  if (!host) return
  if (!reviewPaneVisible) return
  const open = state.review.items.filter((item) => item.status === "open")
  const openSuggestions = open.filter((item): item is Extract<ReviewItem, { kind: "suggestion" }> => item.kind === "suggestion")
  const body = open.length === 0
    ? `<p class="review-sidebar-empty">No review items. Turn on suggesting to track changes, or place a cursor and add a comment.</p>`
    : open.map(renderSidebarCard).join("")
  host.innerHTML = `
    <div class="review-sidebar-toolbar">
      <button id="sidebar-suggesting" class="toolbar-button${state.review.suggesting ? " is-on" : ""}" type="button" aria-pressed="${state.review.suggesting}"${state.role === "reviewer" ? " disabled title=\"Reviewers are always in suggesting mode\"" : ""}>Track changes: ${state.review.suggesting ? "on" : "off"}</button>
      <button id="sidebar-comment" class="toolbar-button" type="button">Add comment</button>
    </div>
    <ul class="review-cards">${body}</ul>
    ${openSuggestions.length > 0
      ? `<div class="review-sidebar-bulk"><button id="sidebar-accept-all" class="primary-button" type="button">Accept all</button> <button id="sidebar-reject-all" class="toolbar-button" type="button">Reject all</button></div>`
      : ""}`
  el("#sidebar-suggesting")?.addEventListener("click", () => {
    state.review = reviewReducer(state.review, { type: "toggle-suggesting" })
    renderReviewBanner()
    renderReviewSidebar()
  })
  el("#sidebar-comment")?.addEventListener("click", () => {
    const sel = editor.state.selection.main
    const hasSelection = sel.from !== sel.to
    const label = hasSelection ? "Comment on selection" : "Comment at cursor"
    promptInPanel("Reviewer suggesting mode", label, "Comment text", (commentBody) => {
      const from = sel.from
      const to = sel.to
      // Anchor the mark to stable Loro cursors so it survives edits.
      const fromCursor = createCursorAt(activeLoro, from)
      const toCursor = to > from ? createCursorAt(activeLoro, to) : fromCursor
      state.review = reviewReducer(state.review, { type: "add", item: { id: crypto.randomUUID(), kind: "comment", from, to, fromCursor, toCursor, body: commentBody, author: currentUserDisplayName(), status: "open", createdAt: Date.now() } })
      editor.dispatch({ effects: setReviewItems.of(state.review.items) })
      renderReviewBanner()
      renderReviewSidebar()
      persistReview()
    }, { placeholder: "Comment text" })
  })
  // Per-card actions reuse applyReviewItemAction (same authoritative path as the
  // popover/dialog), then re-render this sidebar to reflect the new open set.
  for (const button of host.querySelectorAll<HTMLButtonElement>("[data-accept], [data-reject], [data-resolve]")) {
    button.addEventListener("click", (event) => {
      event.stopPropagation()
      const id = button.dataset.id
      const item = state.review.items.find((it) => it.id === id)
      if (!id || !item) return
      const type = button.dataset.resolve !== undefined
        ? "resolve" as const
        : button.dataset.accept !== undefined
          ? "accept" as const
          : "reject" as const
      applyReviewItemAction(item, type)
    })
  }
  // Clicking the card body (not its buttons) scrolls the editor to the item's
  // anchor and opens the inline popover, matching the dialog row behaviour.
  for (const card of host.querySelectorAll<HTMLElement>(".review-card")) {
    card.addEventListener("click", (event) => {
      if ((event.target as HTMLElement).closest("button")) return
      const id = card.dataset.reviewId
      const item = state.review.items.find((it) => it.id === id)
      if (item) revealReviewItem(item)
    })
  }
  el("#sidebar-accept-all")?.addEventListener("click", () => {
    state.review = reviewReducer(state.review, { type: "bulk-accept", ids: openSuggestions.map((item) => item.id), by: currentUserDisplayName(), at: Date.now() })
    editor.dispatch({ effects: setReviewItems.of(state.review.items) })
    renderReviewBanner()
    renderReviewSidebar()
    persistReview()
  })
  el("#sidebar-reject-all")?.addEventListener("click", () => {
    // One authoritative transaction for all rejects (see openReview's reject-all
    // comment): without it the removal of each insert would re-track as deletes.
    const changes: { from: number; to?: number; insert?: string }[] = []
    for (const item of openSuggestions) {
      if (item.change === "delete" && item.text) {
        // HIGH #6: Clamp the restore position to the current doc length (the same
        // guard the single-item reject path uses) so a stale offset can't throw a
        // RangeError.
        changes.push({ from: Math.min(item.from, editor.state.doc.length), insert: item.text })
      } else if (item.change === "insert") {
        // HIGH #6: Validate the range just like the single-item path: only remove
        // the suggested text if it still matches what is in the doc (clamped to doc
        // length). A stale or since-edited range is silently skipped so we never
        // corrupt surrounding text at a stale offset.
        const from = item.from
        const to = Math.min(item.to, editor.state.doc.length)
        if (editor.state.doc.sliceString(from, to) === item.text) {
          changes.push({ from, to, insert: "" })
        }
      }
    }
    // HIGH #6: Sort ascending by `from` — CodeMirror requires the change array in a
    // single transaction to be ordered (and non-overlapping) so each change maps to
    // the correct original-document offset.
    changes.sort((a, b) => a.from - b.from)
    resolvingSuggestions = true
    try {
      if (changes.length > 0) editor.dispatch({ annotations: resolveAnnotation.of(true), changes })
    } finally {
      resolvingSuggestions = false
    }
    state.review = reviewReducer(state.review, { type: "bulk-reject", ids: openSuggestions.map((item) => item.id), by: currentUserDisplayName(), at: Date.now() })
    editor.dispatch({ effects: setReviewItems.of(state.review.items) })
    renderReviewBanner()
    renderReviewSidebar()
    persistReview()
  })
}

/**
 * Sidebar thread card markup. Mirrors the popover's visual language (amber chip
 * for comments, green/red for insert/delete) so a card is recognisable at a
 * glance against the inline mark it corresponds to.
 */
function renderSidebarCard(item: ReviewItem): string {
  const anchor = item.orphaned ? `<span class="review-card-orphan">anchor lost</span>` : ""
  // Avatar (initials on the author's colour) + name + relative timestamp, shared by
  // comment and suggestion cards so every card shows who said it and when at a glance.
  const authorLine = `<span class="review-avatar" style="--hue:${authorHue(item.author)}" aria-hidden="true">${escapeHtml(authorInitials(item.author))}</span><span class="review-card-author">${escapeHtml(item.author)} <span class="review-card-time">${escapeHtml(timeAgo(item.createdAt))}</span></span>`
  if (item.kind === "comment") {
    return `<li class="review-card review-card-comment" data-review-id="${escapeHtml(item.id)}" title="Click to jump to this comment in the editor">
      <div class="review-card-head"><span class="review-chip review-chip-comment">Comment</span>${authorLine}${anchor}</div>
      <p class="review-card-text">${escapeHtml(item.body)}</p>
      <div class="review-card-actions"><button class="primary-button" data-resolve data-id="${escapeHtml(item.id)}">Resolve</button></div>
    </li>`
  }
  const chip = item.change === "insert" ? "Insert" : "Delete"
  const chipClass = item.change === "insert" ? "review-chip-insert" : "review-chip-delete"
  const snippet = item.text !== undefined ? `<p class="review-card-text">“${escapeHtml(item.text.length > 200 ? `${item.text.slice(0, 200)}…` : item.text)}”</p>` : ""
  const note = item.change === "delete" ? `<p class="review-card-note">Text already removed · Reject restores it</p>` : ""
  return `<li class="review-card review-card-suggestion" data-review-id="${escapeHtml(item.id)}" title="Click to jump to this suggestion in the editor">
    <div class="review-card-head"><span class="review-chip ${chipClass}">${chip}</span>${authorLine}${anchor}</div>
    ${snippet}${note}
    <div class="review-card-actions"><button class="primary-button" data-accept data-id="${escapeHtml(item.id)}">Accept</button> <button class="toolbar-button" data-reject data-id="${escapeHtml(item.id)}">Reject</button></div>
  </li>`
}

/**
 * Docks/undocks the review sidebar and re-applies the grid so the 4th column +
 * gutter appear/disappear. Toggling closed does not touch review state.
 */
/**
 * Floating "Add comment" button that appears at the end of a text selection when
 * the review sidebar is open — like Overleaf's selection-comment affordance.
 */
let selectionCommentButton: HTMLButtonElement | undefined
function updateSelectionCommentButton(view: EditorView): void {
  const sel = view.state.selection.main
  const hasSelection = sel.from !== sel.to
  if (!hasSelection || !reviewPaneVisible) {
    selectionCommentButton?.remove()
    selectionCommentButton = undefined
    return
  }
  // Position the button at the end of the selection.
  const coords = view.coordsAtPos(sel.to)
  if (!coords) { selectionCommentButton?.remove(); selectionCommentButton = undefined; return }
  if (!selectionCommentButton) {
    selectionCommentButton = document.createElement("button")
    selectionCommentButton.className = "selection-comment-button"
    selectionCommentButton.type = "button"
    selectionCommentButton.textContent = "💬 Comment"
    selectionCommentButton.addEventListener("click", () => {
      const s = editor.state.selection.main
      promptInPanel("Reviewer suggesting mode", "Comment on selection", "Comment text", (commentBody) => {
        const fromCursor = createCursorAt(activeLoro, s.from)
        const toCursor = s.to > s.from ? createCursorAt(activeLoro, s.to) : fromCursor
        state.review = reviewReducer(state.review, { type: "add", item: { id: crypto.randomUUID(), kind: "comment", from: s.from, to: s.to, fromCursor, toCursor, body: commentBody, author: currentUserDisplayName(), status: "open", createdAt: Date.now() } })
        editor.dispatch({ effects: setReviewItems.of(state.review.items) })
        renderReviewBanner()
        renderReviewSidebar()
        persistReview()
      }, { placeholder: "Comment text" })
      selectionCommentButton?.remove()
      selectionCommentButton = undefined
    })
  }
  selectionCommentButton.style.left = `${coords.right + 6}px`
  selectionCommentButton.style.top = `${coords.top - 4}px`
  if (!selectionCommentButton.isConnected) document.body.append(selectionCommentButton)
}

function toggleReviewSidebar(): void {
  reviewPaneVisible = !reviewPaneVisible
  const reviewButton = el<HTMLButtonElement>("#review-button")
  if (reviewButton) reviewButton.setAttribute("aria-pressed", String(reviewPaneVisible))
  applyWorkspaceGrid()
  if (reviewPaneVisible) {
    renderReviewSidebar()
  } else {
    // Remove the floating selection-comment button: it checks reviewPaneVisible
    // on the next editor update, but until then it would linger on screen with
    // no sidebar to receive the comment.
    selectionCommentButton?.remove()
    selectionCommentButton = undefined
  }
}

// ---------------------------------------------------------------------------
// Compile
// ---------------------------------------------------------------------------

/** Restores the preview pane's never-compiled empty state. */
function clearPreview(): void {
  const viewer = el<HTMLElement>("#pdf-viewer")
  viewer?.replaceChildren()
  viewer?.classList.add("empty-preview")
  viewer?.append(makeEmptyPreviewNode("No preview yet", "Select a document and compile it to see the rendered PDF."))
  el<HTMLElement>("#pdf-zoom-controls")?.setAttribute("hidden", "")
}

function makeEmptyPreviewNode(title: string, body: string): HTMLElement {
  const node = document.createElement("div")
  node.className = "empty-state"
  node.innerHTML = `<h2>${escapeHtml(title)}</h2><p>${escapeHtml(body)}</p>`
  return node
}

function showPreviewFailure(message: string): void {
  const viewer = el<HTMLElement>("#pdf-viewer")
  if (!viewer) return
  viewer.replaceChildren()
  viewer.classList.add("empty-preview")
  viewer.append(makeEmptyPreviewNode("Compile failed", message))
  el<HTMLElement>("#pdf-zoom-controls")?.setAttribute("hidden", "")
}

/**
 * Renders the diagnostics list and applies the editor underline decorations.
 * Each diagnostic is clickable: it scrolls to its source line and selects the
 * underlined range so the location is obvious even without a gutter.
 */
function renderDiagnostics(diagnostics: readonly CompileDiagnostic[]): void {
  state.diagnostics = diagnostics
  editor.dispatch({ effects: setDiagnostics.of(diagnostics) })
  const host = el<HTMLElement>("#diagnostics-list")
  if (!host) return
  if (diagnostics.length === 0) {
    host.hidden = true
    host.replaceChildren()
    return
  }
  const entry = state.selected ? state.selected.document.path : ""
  const errors = diagnostics.filter((item) => item.severity !== "warning").length
  const warnings = diagnostics.length - errors
  host.hidden = false
  host.innerHTML = `<h3>${errors} error${errors === 1 ? "" : "s"} · ${warnings} warning${warnings === 1 ? "" : "s"}</h3>` +
    diagnostics.map((item, index) => {
      const sev = item.severity === "warning" ? "warning" : "error"
      const loc = locationLabel(item, entry)
      return `<div class="diagnostic-item" data-diag="${index}"><span class="diag-sev diag-sev-${sev}">${escapeHtml(sev)}</span><div><div>${escapeHtml(item.message)}</div>${loc ? `<div class="diag-loc">${escapeHtml(loc)}</div>` : ""}</div></div>`
    }).join("")
  for (const node of host.querySelectorAll<HTMLElement>("[data-diag]")) {
    node.addEventListener("click", () => {
      const diag = state.diagnostics[Number(node.dataset.diag)]
      if (!diag) return
      const length = editor.state.doc.length
      const from = Math.min(diag.start ?? 0, length)
      const to = Math.min(diag.end ?? from, length)
      // A zero-width selection at the offset still scrolls it into view; a real
      // range additionally highlights the underlined span.
      editor.dispatch({ selection: { anchor: from, head: Math.max(to, from) }, scrollIntoView: true })
      editor.focus()
    })
  }
}

function locationLabel(item: CompileDiagnostic, entry: string): string {
  const file = item.path ?? entry
  if (item.start === null || item.start === undefined) return file ? escapeHtml(file) : ""
  const line = editor.state.doc.lineAt(Math.min(item.start, editor.state.doc.length)).number
  return `${escapeHtml(file)}:${line}`
}

let compiling = false
/** When a manual compile (button/Ctrl+S/view switch) arrives while a compile is
 *  in flight, we can't run it immediately. This flag ensures it runs as soon as
 *  the in-flight compile finishes, so the user's explicit action is never lost. */
let pendingCompile = false
function compileCurrent(): void {
  const { project, selected } = state
  if (!project || !selected) { setText("#build-label", "Select a document first"); return }
  // Re-entrancy guard: a slow earlier build must not be superseded by a later
  // one's result landing out of order, and double-clicks shouldn't fan out two
  // concurrent server builds. If a compile is in flight, queue this one so the
  // user's explicit action (button/Ctrl+S/view switch) is never silently lost.
  if (compiling) { pendingCompile = true; return }
  compiling = true
  const entry = selected.document.path
  // Capture the document id so the async success/error callbacks can bail if the
  // user has switched documents while the compile was in flight — otherwise the
  // old document's diagnostics/PDF are applied to the new document's editor/preview.
  const documentId = selected.document.id
  setText("#compile-button", "Compiling…")
  setText("#build-label", "Compiling…")
  // Clear the previous result on EVERY attempt so a failing compile can never
  // leave a stale PDF (or stale kept pages) on the canvas — the pane must reflect
  // the source being compiled, not the last successful build. Revoking the blob
  // via replace(undefined) avoids leaking the now-discarded URL.
  clearPreview()
  pdfUrls.replace(undefined)
  renderDiagnostics([])
  // Projection happens server-side over the marks we send here. Only open,
  // non-orphaned suggestions affect the body text: accepted/rejected ones are
  // already reflected (or removed) in the editor text, and comments never change
  // visibility (see projection.rs). Offsets are editor doc offsets, which match
  // the compile source below exactly; clamp `end` to the doc length as a guard
  // against any stale position that escaped updateReviewItems' remapping.
  const docLength = editor.state.doc.length
  const marks: readonly MarkInput[] = state.review.items
    .filter((item): item is Extract<ReviewItem, { kind: "suggestion" }> =>
      item.kind === "suggestion" && item.status === "open" && !item.orphaned)
    .map((item) => ({
      start: item.fromCursor ? resolveCursor(activeLoro, item.fromCursor) ?? item.from : item.from,
      end: Math.min(item.toCursor ? resolveCursor(activeLoro, item.toCursor) ?? item.to : item.to, docLength),
      kind: item.change,
      author: item.author,
      timestamp: Date.now(),
      id: undefined
    }))
  run(
    api.compile({
      projectId: project.id,
      entry,
      sources: { [entry]: editor.state.doc.toString() },
      marks: { [entry]: marks },
      mode: "document",
      view: state.view
    }).pipe(
      Effect.tap(() => Effect.sync(() => { compiling = false; setText("#compile-button", "Compile"); drainPendingCompile() })),
      Effect.tapError(() => Effect.sync(() => { compiling = false; setText("#compile-button", "Compile"); drainPendingCompile() }))
    ),
    (result) => {
      // Document-switch guard: discard the result if the user has moved to a
      // different document while this compile was in flight.
      if (state.selected?.document.id !== documentId) return
      setText("#build-label", `Build ${result.build_id}`)
      const diagnostics = result.diagnostics as readonly CompileDiagnostic[]
      renderDiagnostics(diagnostics)
      // The PDF only matches the source when the build is clean; a build with
      // errors (or no PDF) is shown as an empty/failure state, never the stale
      // canvas from the previous successful compile.
      const pdf = result.pdf_base64
      if (pdf && diagnostics.filter((item) => item.severity !== "warning").length === 0) {
        const url = pdfUrls.replace(pdf)
        if (url) {
          // `empty-preview` is the empty-state marker; drop it once a real PDF is
          // being rendered (clearPreview/showPreviewFailure re-add it on clear/fail).
          el<HTMLElement>("#pdf-viewer")?.classList.remove("empty-preview")
          updateZoomLabel()
          el<HTMLElement>("#pdf-zoom-controls")?.removeAttribute("hidden")
          void pdfViewer.load(url).catch((error: unknown) => {
            console.error("PDF render failed", error)
            showPreviewFailure(error instanceof Error ? error.message : "The PDF could not be rendered.")
          })
        }
      } else {
        showPreviewFailure(diagnostics.length > 0 ? "See diagnostics for details." : "The compile produced no PDF.")
      }
      const errors = diagnostics.filter((item) => item.severity !== "warning").length
      status(errors > 0 ? `Compile failed with ${errors} error${errors === 1 ? "" : "s"}` : diagnostics.length > 0 ? `Compiled with ${diagnostics.length} warning${diagnostics.length === 1 ? "" : "s"}` : "Compiled")
    },
    (error: unknown) => {
      // Document-switch guard: don't clobber the new document's preview with the
      // old document's compile error.
      if (state.selected?.document.id !== documentId) return
      // A thrown/transport error leaves the pane empty with the reason rather
      // than the last good PDF.
      showPreviewFailure(error instanceof Error ? error.message : "The compile request failed")
      status(error instanceof Error ? error.message : "The compile request failed")
    }
  )
}

/**
 * Background compile for live preview + diagnostics.
 *
 * Triggered by `scheduleDiagnosticsCompile` after a typing pause. It recompiles
 * the current source, refreshes the diagnostic underlines/list, AND updates the
 * PDF preview when the build is clean — so the preview stays in sync while the
 * user writes without needing a manual compile. Shares the `compiling` guard so
 * it cannot run alongside a manual compile; if one is already in flight it
 * simply bails (that compile will deliver diagnostics anyway). Stays silent —
 * no "Compiling…" button label, no status churn — so background checking never
 * looks like user-driven work.
 */
function compileForDiagnostics(): void {
  const { project, selected } = state
  if (!project || !selected) return
  // Capture the document ID so the async callback can bail if the user has
  // switched documents while the compile request was in flight, preventing
  // the old document's diagnostics from being applied to the new one.
  const documentId = selected.document.id
  // Bail if a compile (manual or background) is already running; it will emit
  // the diagnostics this debounce was after.
  if (compiling) return
  compiling = true
  const entry = selected.document.path
  const docLength = editor.state.doc.length
  const marks: readonly MarkInput[] = state.review.items
    .filter((item): item is Extract<ReviewItem, { kind: "suggestion" }> =>
      item.kind === "suggestion" && item.status === "open" && !item.orphaned)
    .map((item) => ({
      start: item.fromCursor ? resolveCursor(activeLoro, item.fromCursor) ?? item.from : item.from,
      end: Math.min(item.toCursor ? resolveCursor(activeLoro, item.toCursor) ?? item.to : item.to, docLength),
      kind: item.change,
      author: item.author,
      timestamp: Date.now(),
      id: undefined
    }))
  run(
    api.compile({
      projectId: project.id,
      entry,
      sources: { [entry]: editor.state.doc.toString() },
      marks: { [entry]: marks },
      mode: "document",
      view: state.view
    }).pipe(
      Effect.tap(() => Effect.sync(() => { compiling = false; drainPendingCompile() })),
      Effect.tapError(() => Effect.sync(() => { compiling = false; drainPendingCompile() }))
    ),
    (result) => {
      // Document-switch guard: if the user has switched documents while this
      // background compile was in flight, discard the result.
      if (state.selected?.document.id !== documentId) return
      renderDiagnostics(result.diagnostics as readonly CompileDiagnostic[])
      // Update the PDF preview only on a clean build. A build with errors leaves
      // the last good PDF in place (better than clearing to an empty pane on
      // every transient typo). The zoom controls and blob URL are managed the
      // same way as a manual compile.
      const diagnostics = result.diagnostics as readonly CompileDiagnostic[]
      const errors = diagnostics.filter((item) => item.severity !== "warning").length
      const pdf = result.pdf_base64
      if (pdf && errors === 0) {
        const url = pdfUrls.replace(pdf)
        if (url) {
          el<HTMLElement>("#pdf-viewer")?.classList.remove("empty-preview")
          updateZoomLabel()
          el<HTMLElement>("#pdf-zoom-controls")?.removeAttribute("hidden")
          void pdfViewer.load(url).catch((error: unknown) => {
            console.error("PDF render failed", error)
          })
        }
      }
    },
    // A failed background compile is not surfaced as a preview failure (that
    // would clobber the last good PDF); the next manual compile reports it.
    () => undefined
  )
}

/** Drains a pending manual compile that was deferred while a compile was in
 *  flight. Called from both compile paths' completion taps. */
function drainPendingCompile(): void {
  if (pendingCompile) {
    pendingCompile = false
    compileCurrent()
  }
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

const authLayer = OidcClientLive.pipe(Layer.provide(AuthTokenLive))

function renderAuth(): void {
  const button = el<HTMLButtonElement>("#sign-in")
  if (!button) return
  if (!oidcConfigFromEnv()) {
    button.textContent = "Sign-in not configured"
    button.disabled = true
    return
  }
  button.disabled = false
  button.textContent = state.signedIn ? "Sign out" : "Sign in"
}

el("#sign-in")?.addEventListener("click", () => {
  if (state.signedIn) {
    void Effect.runPromise(Effect.provide(OidcClient.use((client) => client.logout()), authLayer)).then(() => {
      state.signedIn = false
      syncConnection?.close()
      renderAuth()
      status("Signed out")
    }, (error: unknown) => {
      status(error instanceof Error ? error.message : "Sign-out failed")
    })
    return
  }
  void Effect.runPromise(Effect.provide(OidcClient.use((client) => client.login()), authLayer)).catch((error: unknown) => {
    status(error instanceof Error ? error.message : "Sign-in failed")
  })
})

/**
 * Completes the OIDC redirect.
 *
 * Without this the login round trip is one-way: the provider redirects back with
 * `?code=` and nothing ever exchanges it for a token. The query string is stripped
 * afterwards so a reload does not replay a spent authorization code.
 */
function completeSignIn(): Promise<void> {
  if (!isOidcCallback()) return Promise.resolve()
  return Effect.runPromise(Effect.provide(OidcClient.use((client) => client.completeCallback()), authLayer)).then(
    () => {
      state.signedIn = true
      window.history.replaceState({}, "", window.location.pathname)
      scheduleTokenRefresh()
      status("Signed in")
    },
    (error: unknown) => {
      window.history.replaceState({}, "", window.location.pathname)
      status(error instanceof Error ? error.message : "Sign-in could not be completed")
    }
  )
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

el("#new-project")?.addEventListener("click", () => {
  // Context-aware: inside a project → add document; at the project list → create project.
  if (state.project) addDocument()
  else createProject()
})
el("#references-button")?.addEventListener("click", openReferences)
el("#history-button")?.addEventListener("click", openHistory)
el("#share-button")?.addEventListener("click", openShare)
el("#export-button")?.addEventListener("click", openExport)
// The Review button now toggles the docked sidebar (Overleaf-style) rather than
// opening the modal overview. `openReview()` stays as a fallback for callers that
// still want the modal (none in the shell today), but the button route is the
// sidebar so review lives alongside the editor instead of over it.
el("#review-button")?.addEventListener("click", toggleReviewSidebar)
el("#hide-review")?.addEventListener("click", toggleReviewSidebar)
el("#compile-button")?.addEventListener("click", compileCurrent)
el("#suggesting-button")?.addEventListener("click", () => {
  state.review = reviewReducer(state.review, { type: "toggle-suggesting" })
  renderReviewBanner()
  renderReviewSidebar()
})
// The toolbar toggle is the always-reachable copy of the banner toggle (see M1):
// same behaviour, just not gated behind the hidden-on-clean-doc review banner.
el("#toolbar-suggesting")?.addEventListener("click", () => {
  state.review = reviewReducer(state.review, { type: "toggle-suggesting" })
  renderReviewBanner()
  renderReviewSidebar()
})
el<HTMLSelectElement>("#view-select")?.addEventListener("change", (event) => {
  state.view = (event.target as HTMLSelectElement).value as CompileView
  // Switching the projection view only changes the param sent on the NEXT compile,
  // so without an explicit recompile the preview never reflects the new view. Treat
  // the switch as the author asking to see that view: recompile immediately when a
  // document is open. Skip while a compile is in flight (the button shows "Compiling…")
  // so a view switch mid-compile does not stack a second concurrent request.
  // Recompile on view switch if a document is open. If a compile is in flight,
  // the pendingCompile flag in compileCurrent() queues this so the new view
  // isn't lost (the old check used .includes("Compile") which matched
  // "Compiling…" too, so the guard never fired).
  if (state.document) compileCurrent()
})

document.addEventListener("keydown", (event) => {
  // Skip all global shortcuts while a <dialog> is modal: keyboard events still
  // bubble up to document even inside showModal()'s top layer, so Ctrl+Enter
  // would fire compileCurrent() (or Ctrl+S would trigger a save) while the user
  // is typing into a prompt. Letting the dialog handle its own keys is correct.
  if (el<HTMLDialogElement>("#workspace-panel")?.open) return
  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") { event.preventDefault(); compileCurrent() }
  // Ctrl/Cmd+S saves and then recompiles so the preview reflects the saved
  // source immediately — the user expects Ctrl+S to produce a fresh build.
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") { event.preventDefault(); saveNow(); compileCurrent() }
  // Ctrl/Cmd+= and Ctrl/Cmd+- zoom the PDF preview in and out.
  if ((event.metaKey || event.ctrlKey) && (event.key === "=" || event.key === "+")) { event.preventDefault(); pdfViewer.zoomIn(); updateZoomLabel() }
  if ((event.metaKey || event.ctrlKey) && event.key === "-") { event.preventDefault(); pdfViewer.zoomOut(); updateZoomLabel() }
})

window.addEventListener("beforeunload", (event) => {
  // Snapshot whether the debounce timer is still armed BEFORE flushing, because
  // flushPendingSave() clears saveTimer/pendingSave — checking them afterwards
  // (the old code) was always false, making the guard dead code. We also check
  // saveInFlight for a PATCH already over the wire that would be lost on close.
  const timerPending = saveTimer !== undefined
  // HIGH #5b: flushPendingSave() fires an async fetch() PATCH, but the browser
  // aborts in-flight requests during teardown — so the pending edit is lost.
  // A last-chance save must survive the unload. navigator.sendBeacon can't be
  // used here because it sends an unauthenticated POST (no Authorization header,
  // wrong method) — the save endpoint requires PATCH + Bearer token. Instead use
  // fetch with keepalive:true, which survives page teardown AND allows custom
  // headers and method. Only fire when there is actually pending data (a
  // debounced save or an armed timer). We still call flushPendingSave()
  // afterwards for its timer-cleanup side effects, but clear pendingSave first so
  // it does not ALSO fire the doomed async fetch.
  if (pendingSave || saveTimer) {
    const context = pendingSave ?? captureSaveContext()
    const beaconToken = readStoredAccessToken()
    if (context && state.project && beaconToken) {
      const url = `/api/projects/${encodeURIComponent(state.project.id)}/documents/${encodeURIComponent(context.documentId)}`
      const payload = JSON.stringify({ body: context.body, expected_revision: context.revision })
      void fetch(url, {
        method: "PATCH",
        headers: { "content-type": "application/json", "authorization": `Bearer ${beaconToken}` },
        body: payload,
        keepalive: true
      }).catch(() => undefined)
    }
  }
  pendingSave = undefined
  flushPendingSave()
  cancelDiagnosticsCompile()
  if (saveInFlight || timerPending) {
    event.preventDefault()
    event.returnValue = ""
  }
  // HIGH #4: Do NOT destroy the editor / close the sync connection / dispose PDF
  // URLs here. beforeunload can fire and then the user clicks "Stay on page", which
  // would leave the editor permanently destroyed with no recovery path. The
  // destructive teardown now runs on "pagehide", which only fires on a real unload.
})
// HIGH #4: Real teardown belongs here. "pagehide" fires only when the document is
// genuinely being unloaded (navigation/close), unlike "beforeunload" which the
// user can cancel — so destroying the editor here can never strand a staying user.
window.addEventListener("pagehide", (event) => {
  // bfcache freeze: the browser snapshots the page for back-forward cache
  // and restores it without reloading when the user navigates back. Destroying
  // the editor / closing sync / revoking the PDF URL here would leave a
  // visually-intact but completely dead page. Skip teardown on persisted freeze;
  // the handlers run only on a genuine unload.
  if (event.persisted) return
  syncConnection?.close()
  pdfUrls.dispose()
  editor.destroy()
})
// Honest connectivity indicator (H3): the browser fires offline/online the moment
// the network changes, whereas the sync WebSocket can keep a half-open socket for
// tens of seconds before its close event. Listening here makes the connection
// label reflect a real network drop immediately. `setSyncStatus` treats
// browserOffline as a one-way dimmer (offline overrides; online just re-shows the
// last relay status, it never falsely claims connected).
window.addEventListener("offline", () => {
  browserOffline = true
  setSyncStatus(lastSyncStatus ?? "disconnected", "Network offline · changes saved locally")
})
window.addEventListener("online", () => {
  browserOffline = false
  // Replay the last known relay status; the reconnecting relay will repaint the
  // truth once its WebSocket re-establishes. Fall back to lastSyncDetail if the
  // relay had reported a specific reason, otherwise a generic reconnecting message.
  setSyncStatus(lastSyncStatus ?? "connecting", lastSyncDetail ?? "Reconnecting…")
})

renderAuth()
renderReviewBanner()
renderWorkspaceState()
// Central 401 handling: any API call that comes back unauthorized drops the stored
// token and returns the UI to the signed-out state so a dead session is surfaced
// instead of silently failing every read/write.
onAuthFailure(() => {
  if (!state.signedIn) return
  state.signedIn = false
  syncConnection?.close()
  syncConnection = undefined
  closeReviewPopover()
  cancelDiagnosticsCompile()
  renderAuth()
  status("Session expired — sign in again")
  state.projects = []
  state.project = undefined
  state.outline = []
  state.selected = undefined
  state.document = undefined
  state.role = undefined
  renderWorkspaceState()
  renderProjects()
})
void completeSignIn().then(() => {
  renderAuth()
  // Only fetch projects when actually signed in: completeSignIn leaves
  // state.signedIn accurate (true on callback success, or from a stored token).
  // Fetching unconditionally fired a 401 on every boot before sign-in (L1),
  // because the app authorises the /projects route and returns 401 for a missing
  // token — a tolerated error that still spammed the console.
  if (!state.signedIn) {
    status("Sign in to view your projects")
    return
  }
  scheduleTokenRefresh()
  run(api.listProjects(), (projects) => {
    state.projects = projects
    renderProjects()
    status(projects.length === 0 ? "No projects yet" : "Ready")
    // Share-link redemption: if the URL has a ?share=token parameter, redeem it
    // (adds the caller as a project member), then open that project and strip
    // the parameter so a reload doesn't replay it.
    const shareToken = new URLSearchParams(window.location.search).get("share")
    if (shareToken) {
      run(api.redeemShareLink(shareToken), (result) => {
        window.history.replaceState({}, "", window.location.pathname)
        const sharedProject = projects.find((p) => p.id === result.project_id)
        if (sharedProject) {
          openProject(sharedProject)
          status("Joined via share link")
        } else {
          // The project isn't in the cached list (race); reload to pick it up.
          run(api.listProjects(), (fresh) => {
            state.projects = fresh
            renderProjects()
            const found = fresh.find((p) => p.id === result.project_id)
            if (found) openProject(found)
          })
        }
      }, (error: unknown) => {
        window.history.replaceState({}, "", window.location.pathname)
        status(error instanceof Error ? error.message : "Share link could not be redeemed")
      })
    } else if (projects.length > 0) {
      // M5: reopen the project/document the user last had open before tab-away.
      restoreLastOpen()
    }
  })
})
