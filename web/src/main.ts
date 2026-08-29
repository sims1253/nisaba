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
import { Annotation, Compartment, EditorState, Prec, StateEffect, StateField } from "@codemirror/state"
import { Decoration, EditorView, keymap, placeholder, type DecorationSet, type ViewUpdate } from "@codemirror/view"
import { LoroExtensions, loroSyncAnnotation, redo as loroRedo } from "loro-codemirror"
import { LoroDoc, LoroText, UndoManager } from "loro-crdt"
import { Effect, Layer } from "effect"
import { findConstructs, type Construct } from "./model"
import { hybridEditorField, revealConstruct, reviewEditorField, setReviewItems, type ReferenceDisplay } from "./decorations"
import { downloadBase64 } from "./effects"
import { connectSync, isImportingRemote, type SyncConnection, type SyncStatus } from "./sync"
import { filterAndSortProjects, type ProjectSort } from "./projects-list"
import {
  DEFAULT_SETTINGS, TYPEFACE_LABELS, applySettings, clampSettings, loadDefaultFile,
  loadSettings, saveDefaultFile, saveSettings,
  type Settings, type TypefaceId,
} from "./settings"
import { VirtualPdfViewer } from "./pdf-viewer"
import * as api from "./api"
import type { CompileView, Fulltext, MembershipRole, NisabaDocument, Project, Reference } from "./api"
import { AuthTokenLive, OidcClient, OidcClientLive, onAuthFailure, readStoredAccessToken, currentUserDisplayName, decodedTokenPayload, isOidcCallback, oidcConfigFromEnv, scheduleTokenRefresh } from "./auth"
import { emptyReviewState, reviewReducer, type ReviewItem, type ReviewState } from "./review"
import { mergeReviewItems, readReviewItemsFromMap, writeReviewItemsToMap } from "./review-persistence"
import { createCursorAt } from "./cursor"
import { SHELL_HTML } from "./shell"
import { activeHeadingIndex, buildFileTree, documentHeadings, headingTrail, wordCount, type Heading, type TreeNode } from "./outline"
import { initialsOf, peerLocation, type PresencePeer } from "./presence"
import { remoteCursors, setRemoteCursors, type RemoteCursor } from "./remote-cursors"
import { createPalette, type PaletteItem } from "./palette"
import { fuzzyScore } from "./fuzzy"
import {
  clearPreview,
  compileCurrent,
  compileForDiagnostics,
  initCompile,
  renderBuildHealth,
  renderBuildLabel,
  resetBuildSummary,
  isCompiling,
  VIEW_LABELS,
  type BuildLogLevel,
  type CompileDiagnostic
} from "./compile"
import "./styles.css"

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
/** A fresh replica with a UNIQUE CRDT peer id. */
function newReplica(): LoroDoc {
  const doc = new LoroDoc()
  // CRDT peers MUST have distinct ids: every client previously used Loro's
  // default peer id, so a second collaborator's ops collided with the first
  // client's and the relay silently dropped them — reviewer suggestions never
  // reached the relay once another session had the document open (2026-08-09
  // collaboration finding, reproduced e2e).
  const buf = new Uint32Array(2)
  crypto.getRandomValues(buf)
  const hi = buf[0] ?? 0
  const lo = buf[1] ?? 0
  doc.setPeerId((BigInt(hi) << 32n) | BigInt(lo))
  return doc
}

let activeLoro = newReplica()

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

root.innerHTML = SHELL_HTML

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
// Workspace columns: drag-resize gutters, hide/show, dock, focus mode
// ---------------------------------------------------------------------------

/**
 * Per-column widths in pixels. `-1` means "flex" (rendered as 1fr): the document
 * and preview absorb the leftover space, while the navigator and the dock keep a
 * fixed pixel size. The widths persist for the session, which is enough for a
 * writing tool where one layout is kept for hours.
 */
interface ColumnWidths {
  navigator: number
  doc: number
  dock: number
  preview: number
}

const columnWidths: ColumnWidths = { navigator: 250, doc: -1, dock: 340, preview: -1 }

/** Hidden panes (and their gutter) collapse to 0; the document is always visible. */
interface HiddenPanes { navigator: boolean; preview: boolean }

const hiddenPanes: HiddenPanes = { navigator: false, preview: false }

/**
 * The tools that can occupy the dock, one at a time (docs/ui-design.md §4).
 * Review is the only one that also re-renders on state changes, so the rest are
 * plain "render once when opened" panels.
 */
type DockTool = "review" | "references" | "history" | "share" | "export" | "settings"

const DOCK_TITLES: Record<DockTool, string> = {
  review: "Review",
  references: "References",
  history: "History",
  share: "Share",
  export: "Export",
  settings: "Settings"
}

/** Which tool is docked, or undefined when the dock is closed. */
let dockTool: DockTool | undefined

/**
 * Below this width the navigator, the text, the dock, and the preview cannot all
 * be useful at once — each ends up too narrow to read. Opening a dock on a narrow
 * window therefore takes the preview's place, and closing it gives the preview
 * back. Only an automatic collapse is undone; a preview the writer hid on purpose
 * stays hidden.
 */
const FOUR_COLUMN_MIN_WIDTH = 1320
let previewCollapsedForDock = false

function makeRoomForDock(): void {
  if (window.innerWidth >= FOUR_COLUMN_MIN_WIDTH || hiddenPanes.preview) return
  hiddenPanes.preview = true
  previewCollapsedForDock = true
}

function restorePreviewAfterDock(): void {
  if (!previewCollapsedForDock) return
  previewCollapsedForDock = false
  hiddenPanes.preview = false
}

/**
 * Re-applies the same rule when the window is resized rather than only when the
 * dock opens, so dragging a window narrower (or working on a laptop after a
 * desktop session) does not leave four unusable columns.
 */
function reflowForWidth(): void {
  if (dockTool === undefined) return
  if (window.innerWidth < FOUR_COLUMN_MIN_WIDTH) makeRoomForDock()
  else restorePreviewAfterDock()
  applyWorkspaceGrid()
}

let reflowFrame: number | undefined
window.addEventListener("resize", () => {
  if (reflowFrame !== undefined) return
  reflowFrame = requestAnimationFrame(() => {
    reflowFrame = undefined
    reflowForWidth()
    renderPagePosition()
  })
})

const GUTTER = 4

const workspaceEl = root.querySelector<HTMLElement>(".workspace")!

/**
 * Writes the grid template from the current widths + hidden/visible flags.
 *
 * Tracks are: navigator · gutter · document · gutter · dock · gutter · preview.
 * The dock sits between the text and the page so a reviewer reads threads next to
 * the source with the artefact beyond them. A hidden pane and its adjacent gutter
 * both collapse to 0px (and the gutter is also `hidden`), which removes the column
 * without disturbing the others. When the dock is closed its two tracks are left
 * out of the template entirely — `display:none` would pull the remaining children
 * into the wrong tracks.
 */
function applyWorkspaceGrid(): void {
  const px = (value: number): string => (value === -1 ? "1fr" : `${value}px`)
  const g = (visible: boolean): string => (visible ? `${GUTTER}px` : "0px")
  const navigatorWidth = hiddenPanes.navigator ? "0px" : px(columnWidths.navigator)
  // A hidden pane's track must be 0px, not 1fr: the pane itself is display:none,
  // so a flexible track would simply hold dead space beside the text.
  const previewWidth = hiddenPanes.preview ? "0px" : px(columnWidths.preview)
  const dockOpen = dockTool !== undefined
  // A hidden pane is `display:none`, which removes it from grid flow entirely —
  // so a closed dock must not merely get a 0px track, or every child after it
  // would slide one track to the left and the preview would end up 0px wide.
  // Switch templates instead: 7 tracks with the dock, 5 without.
  workspaceEl.style.gridTemplateColumns = dockOpen
    ? `${navigatorWidth} ${g(!hiddenPanes.navigator)} ${px(columnWidths.doc)} ${g(true)} ${px(columnWidths.dock)} ${g(!hiddenPanes.preview)} ${previewWidth}`
    : `${navigatorWidth} ${g(!hiddenPanes.navigator)} ${px(columnWidths.doc)} ${g(!hiddenPanes.preview)} ${previewWidth}`
  const navigator = el<HTMLElement>("#navigator")
  if (navigator) navigator.hidden = hiddenPanes.navigator
  const preview = el<HTMLElement>(".preview-pane")
  if (preview) preview.hidden = hiddenPanes.preview
  const dock = el<HTMLElement>("#dock")
  if (dock) dock.hidden = !dockOpen
  for (const bar of document.querySelectorAll<HTMLElement>(".gutter")) {
    const key = bar.dataset.gutter
    bar.hidden = key === "navigator" ? hiddenPanes.navigator
      : key === "dock" ? !dockOpen
        : hiddenPanes.preview
  }
  // A collapsed pane leaves an edge tab on the side it lives on, so the way back
  // is where the user would reach for it.
  const showNavigatorTab = el<HTMLButtonElement>("#show-navigator-tab")
  if (showNavigatorTab) showNavigatorTab.hidden = !hiddenPanes.navigator
  const showPreviewTab = el<HTMLButtonElement>("#show-preview-tab")
  if (showPreviewTab) showPreviewTab.hidden = !hiddenPanes.preview
}

/**
 * Which pane a gutter resizes. The document is always a 1fr track, so a drag only
 * ever assigns a concrete width to the fixed pane on the other side; the document
 * flexes into whatever is left. Minimums keep a pane readable (navigator ≥ 150px,
 * dock/preview ≥ 260px).
 */
function gutterPane(gutter: string): keyof ColumnWidths | undefined {
  if (gutter === "navigator") return "navigator"
  if (gutter === "dock") return "dock"
  if (gutter === "preview") return "preview"
  return undefined
}

/**
 * Starts a column-resize drag on mousedown of a gutter.
 *
 * The handler measures the gutter's own bounding box (its left edge is the drag
 * origin) and tracks mousemove on `document` so the cursor can leave the 4px bar
 * without losing the grab. Each move rewrites the fixed pane's width and calls
 * applyWorkspaceGrid, which redraws the template synchronously. A `.dragging`
 * class on the workspace disables text selection during the gesture.
 */
function startGutterDrag(event: MouseEvent): void {
  const bar = event.currentTarget as HTMLElement
  const key = bar.dataset.gutter
  if (!key) return
  // A double-click reaches here as a second mousedown-up with no move; suppress a
  // zero-delta drag so it does not fight the dblclick hide/show handler.
  if (event.detail > 1) return
  const pane = gutterPane(key)
  if (!pane) return
  // Do not start a drag for a gutter whose pane is not on screen.
  if (pane === "navigator" && hiddenPanes.navigator) return
  if (pane === "preview" && hiddenPanes.preview) return
  if (pane === "dock" && dockTool === undefined) return
  event.preventDefault()
  workspaceEl.classList.add("dragging")
  const selector = pane === "navigator" ? "#navigator" : pane === "preview" ? ".preview-pane" : "#dock"
  const paneEl = el<HTMLElement>(selector)
  const originX = bar.getBoundingClientRect().left
  const startWidth = paneEl?.getBoundingClientRect().width ?? columnWidths[pane]
  const min = pane === "navigator" ? 150 : 260
  const onMove = (moveEvent: MouseEvent): void => {
    // The navigator's gutter is its RIGHT edge (drag right grows it); the dock and
    // preview gutters are their pane's LEFT edge, so dragging right shrinks them.
    // Inverting keeps "the boundary follows the cursor" true for all three.
    let delta = moveEvent.clientX - originX
    if (pane === "dock" || pane === "preview") delta = -delta
    columnWidths[pane] = Math.max(min, Math.round(startWidth + delta))
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

/** Collapses/restores a pane. Double-clicking its gutter does the same. */
function togglePane(pane: "navigator" | "preview"): void {
  hiddenPanes[pane] = !hiddenPanes[pane]
  // An explicit show/hide takes the decision away from the dock's auto-collapse.
  if (pane === "preview") previewCollapsedForDock = false
  applyWorkspaceGrid()
}

for (const bar of root.querySelectorAll<HTMLElement>(".gutter")) {
  bar.addEventListener("mousedown", startGutterDrag)
  bar.addEventListener("dblclick", () => {
    const key = bar.dataset.gutter
    if (key === "navigator") togglePane("navigator")
    else if (key === "preview") togglePane("preview")
  })
}

el("#hide-navigator")?.addEventListener("click", () => togglePane("navigator"))
el("#hide-preview")?.addEventListener("click", () => togglePane("preview"))
el("#show-navigator-tab")?.addEventListener("click", () => { hiddenPanes.navigator = false; applyWorkspaceGrid() })
el("#show-preview-tab")?.addEventListener("click", () => { hiddenPanes.preview = false; applyWorkspaceGrid() })

/**
 * Focus mode: nothing but the text (⌘⇧F).
 *
 * The panes are hidden by a body class rather than by mutating `hiddenPanes`, so
 * leaving focus mode restores exactly the layout the writer had — including a
 * deliberately collapsed preview or an open dock.
 */
let focusMode = false
function toggleFocusMode(): void {
  focusMode = !focusMode
  document.body.classList.toggle("focus-mode", focusMode)
  status(focusMode ? "Focus mode — press ⌘⇧F to bring the panels back" : "Ready")
  editor.focus()
}

// PDF zoom controls. The controls are hidden until a PDF is loaded; when shown,
// each button drives the VirtualPdfViewer's discrete zoom level and updates the
// label. Middle-click panning is handled inside the viewer itself.
function updateZoomLabel(): void { setText("#zoom-level", pdfViewer.zoomPercent) }
el("#zoom-in")?.addEventListener("click", () => { pdfViewer.zoomIn(); updateZoomLabel() })
el("#zoom-out")?.addEventListener("click", () => { pdfViewer.zoomOut(); updateZoomLabel() })
el("#zoom-reset")?.addEventListener("click", () => { pdfViewer.resetZoom(); updateZoomLabel() })

applyWorkspaceGrid()

/**
 * Switches between the two screens and the empty/loaded states within them.
 *
 * Picking a project and working in one are different jobs, so they are different
 * screens: the projects screen owns the window until a project is open, and the
 * workspace only exists once there is something to work on. Within the workspace,
 * the document chrome (bar, editor, sticky heading) is replaced by a placeholder
 * until a file is open, so an empty editor never implies an open document.
 */
function renderWorkspaceState(): void {
  const inProject = state.project !== undefined
  const open = state.document !== undefined
  document.body.classList.toggle("has-project", inProject)
  document.body.classList.toggle("has-document", open)
  const screen = el<HTMLElement>("#projects-screen")
  if (screen) screen.hidden = inProject
  const workspace = el<HTMLElement>("#workspace")
  if (workspace) workspace.hidden = !inProject
  for (const node of document.querySelectorAll<HTMLElement>(".doc-chrome, .preview-chrome")) node.hidden = !open
  const previewPlaceholder = el<HTMLElement>("#preview-placeholder")
  if (previewPlaceholder) previewPlaceholder.hidden = open
  const stickyHeading = el<HTMLElement>("#sticky-heading")
  if (stickyHeading && !open) stickyHeading.hidden = true
  const editorPlaceholder = el<HTMLElement>("#editor-placeholder")
  if (editorPlaceholder) editorPlaceholder.hidden = open
  if (!inProject) {
    closeDock()
    setDrawerOpen(false)
  }
  renderCrumbs()
}

/**
 * The breadcrumb: `Projects › project › file › §section`, each segment a jump
 * target. It is the answer to "where am I?" for a writer three folders deep in a
 * sixty-page document, and it doubles as the way back out.
 */
function renderCrumbs(): void {
  const host = el<HTMLElement>("#crumbs")
  if (!host) return
  host.replaceChildren()
  const push = (node: HTMLElement): void => {
    if (host.childElementCount > 0) {
      const separator = document.createElement("span")
      separator.className = "sep"
      separator.textContent = "›"
      separator.setAttribute("aria-hidden", "true")
      host.append(separator)
    }
    host.append(node)
  }
  if (!state.project) return
  const project = document.createElement("button")
  project.type = "button"
  project.textContent = state.project.name
  project.title = "Back to all projects"
  project.addEventListener("click", leaveProject)
  push(project)
  const selected = state.selected
  if (!selected) return
  const file = document.createElement("button")
  file.type = "button"
  file.className = "current"
  file.textContent = selected.document.path
  file.title = "Reveal in the sidebar"
  file.addEventListener("click", () => {
    hiddenPanes.navigator = false
    applyWorkspaceGrid()
    el<HTMLElement>("#file-tree")?.querySelector<HTMLElement>(".tree-row.active")?.scrollIntoView({ block: "nearest" })
  })
  push(file)
  const trail = headingTrail(currentHeadings, editor.state.selection.main.head)
  const leaf = trail.at(-1)
  if (!leaf) return
  const section = document.createElement("button")
  section.type = "button"
  section.textContent = `§${leaf.title}`
  section.title = "Scroll to this section"
  section.addEventListener("click", () => revealPosition(leaf.from))
  push(section)
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

// ---------------------------------------------------------------------------
// Dock and modal
//
// Two different things were previously the same 385px <dialog>: standing
// workflows (References, History, Share, Export) that you use *while* writing,
// and one-question prompts (rename, confirm, comment text). Standing workflows
// now open in the dock at full height; the modal is reserved for prompts, which
// is the one case where taking over the screen is the right answer.
// ---------------------------------------------------------------------------

/**
 * Renders a standing workflow into the dock, opening it if closed. Returns the
 * content host so the caller can wire up the controls it just rendered.
 */
function showPanel(tool: DockTool, content: string): HTMLElement | undefined {
  dockTool = tool
  makeRoomForDock()
  setText("#dock-title", DOCK_TITLES[tool])
  applyWorkspaceGrid()
  const foot = el<HTMLElement>("#dock-foot")
  if (foot) { foot.hidden = true; foot.replaceChildren() }
  const host = el<HTMLElement>("#dock-content")
  if (host) {
    host.innerHTML = content
    host.scrollTop = 0
  }
  syncDockButtons()
  return host ?? undefined
}

/** Reflects the open tool on the buttons that open it. */
function syncDockButtons(): void {
  const buttons: Record<DockTool, string> = {
    review: "#review-button",
    references: "#references-button",
    history: "#history-button",
    share: "#share-button",
    export: "#export-button",
    settings: "#settings-button"
  }
  for (const [tool, selector] of Object.entries(buttons)) {
    el<HTMLElement>(selector)?.setAttribute("aria-expanded", String(dockTool === tool))
  }
}

function closeDock(): void {
  if (dockTool === undefined) return
  dockTool = undefined
  restorePreviewAfterDock()
  applyWorkspaceGrid()
  el<HTMLElement>("#dock-content")?.replaceChildren()
  syncDockButtons()
  // The floating selection-comment affordance belongs to the review dock; with
  // the dock closed there is nothing to receive the comment.
  selectionCommentButton?.remove()
  selectionCommentButton = undefined
}

/** Opens a tool, or closes the dock when that tool is already showing. */
function toggleDock(tool: DockTool, open: () => void): void {
  if (dockTool === tool) { closeDock(); return }
  open()
}

/** Opens the shared modal for a prompt or a short form. */
function showModal(eyebrow: string, title: string, content: string): HTMLElement | undefined {
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
  showModal(eyebrow, title, `
    <label class="field">${escapeHtml(label)}<input id="prompt-input" type="text" placeholder="${escapeHtml(options.placeholder ?? "")}" autocomplete="off" /></label>
    <p class="prompt-error" id="prompt-error" hidden></p>
    <div class="modal-actions">
      <button class="btn" id="prompt-cancel" type="button">Cancel</button>
      <button class="btn btn-primary" id="prompt-ok" type="button" disabled>OK</button>
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
// Projects screen
// ---------------------------------------------------------------------------

// Projects-screen shaping state: the search query lives per session, the sort
// mode persists (the same storage-survives-reload treatment as last-open).
// The pure filter/sort logic lives in projects-list.ts and is unit-tested there.
let projectSearch = ""
let projectSort: ProjectSort = readProjectSort()

function readProjectSort(): ProjectSort {
  try { return localStorage.getItem("nisaba.projectSort") === "name" ? "name" : "recent" } catch { /* storage may be unavailable */ return "recent" }
}

function persistProjectSort(): void {
  try { localStorage.setItem("nisaba.projectSort", projectSort) } catch { /* storage may be unavailable */ }
}

/** Syncs the segmented control's pressed state with the current sort mode. */
function syncProjectToolState(): void {
  el<HTMLButtonElement>("#sort-recent")?.setAttribute("aria-pressed", String(projectSort === "recent"))
  el<HTMLButtonElement>("#sort-name")?.setAttribute("aria-pressed", String(projectSort === "name"))
}

/**
 * The projects screen: one row per project, name first, metadata right-aligned.
 *
 * Rows rather than cards — a card grid looks generous at six projects and
 * becomes a scavenger hunt at sixty, whereas a row list stays scannable and
 * shows more per screen. The rows are the search-filtered, sorted view of the
 * fetched list (recent-first by default, matching the API's order).
 */
function renderProjects(): void {
  const list = el<HTMLElement>("#project-list")
  if (!list) return
  const tools = el<HTMLElement>("#project-tools")
  if (tools) tools.hidden = state.projects.length === 0
  if (state.projects.length === 0) {
    list.innerHTML = `<div class="empty-note"><p>No projects yet. A project holds the files, references, and history of one document.</p><p><button id="empty-create-project" class="btn btn-primary" type="button">Create your first project</button></p></div>`
    el("#empty-create-project")?.addEventListener("click", createProject)
    applyRoleGates()
    return
  }
  const visible = filterAndSortProjects(state.projects, projectSearch, projectSort)
  const countNote = projectSearch.trim() === ""
    ? ""
    : `<p class="screen-note num">${visible.length} of ${state.projects.length} projects</p>`
  list.innerHTML = `${countNote}<div class="project-rows">${visible
    .map((project) => `<div class="project-row">
        <button class="project-open" data-project="${escapeHtml(project.id)}" type="button">
          <span class="name">${escapeHtml(project.name)}</span>
          <span class="meta">${escapeHtml(projectTimestamp(project))}</span>
        </button>
        <button class="btn-icon btn-danger" data-delete-project="${escapeHtml(project.id)}" type="button" title="Delete this project" aria-label="Delete ${escapeHtml(project.name)}">×</button>
      </div>`)
    .join("")}</div>`
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
        `This cannot be undone. Type "${project.name}" to confirm.`,
        (confirmText) => {
          if (confirmText.trim() !== project.name.trim()) {
            status("That did not match the project name — nothing was deleted")
            return
          }
          const reconcileDeletedProject = (): void => {
            status("Project deleted")
            state.projects = state.projects.filter((item) => item.id !== project.id)
            // If the deleted project was this tab's restore target, clear it
            // so a future tab-return doesn't try to reopen a project that no
            // longer exists; the global new-tab default drops it too, but
            // only when the global record actually points there.
            if (readLastOpen().projectId === project.id) persistLastOpen({})
            clearGlobalLastOpen(project.id)
            renderProjects()
          }
          run(api.deleteProject(project.id), reconcileDeletedProject, (error) => {
            if (!(error instanceof api.ApiError) || (error.status !== 403 && error.status !== 404)) {
              status(error instanceof Error ? error.message : "The API request failed")
              return
            }
            // Project deletion also removes its memberships, so the loser of a
            // same-project delete race can receive 403 rather than 404. Re-list
            // before treating it as success: an existing inaccessible project
            // remains a real permission error, while an absent target confirms
            // that another tab already completed this deletion.
            run(api.listProjects(), (projects) => {
              state.projects = projects
              if (projects.some((item) => item.id === project.id)) {
                status(error.message)
                renderProjects()
                return
              }
              reconcileDeletedProject()
            }, () => status(error.message))
          })
        },
        { placeholder: project.name }
      )
    })
  }
  applyRoleGates()
}

/** "edited 2 h ago" for a project row; falls back to nothing on a bad date. */
function projectTimestamp(project: Project): string {
  const parsed = Date.parse(project.updated_at)
  return Number.isFinite(parsed) ? `edited ${timeAgo(parsed)}` : ""
}

/** Leaves the open project and returns to the projects screen. */
function leaveProject(): void {
  if (failedSave?.projectId === state.project?.id && failedSave?.context.documentId === state.selected?.document.id) {
    status("Unsaved offline changes — reconnect before leaving this project")
    return
  }
  closeOpenDocument()
  const departing = state.project?.id
  state.project = undefined
  state.role = undefined
  currentHeadings = []
  persistLastOpen({})
  // The global new-tab default keeps another tab's more recent project; only
  // a default pointing at the departed project is dropped.
  if (departing !== undefined) clearGlobalLastOpen(departing)
  renderWorkspaceState()
  renderProjects()
  applyRoleGates()
}

/**
 * Tears down whatever document is open: sync connection, editor content, review
 * state, preview, and the derived UI. Shared by "leave the project" and "the open
 * document was just deleted", which previously duplicated this sequence.
 */
function closeOpenDocument(): void {
  syncConnection?.close()
  syncConnection = undefined
  flushPendingSave()
  cancelDiagnosticsCompile()
  // Detach the binding before clearing the editor. Otherwise the clear is
  // committed into the document we are closing, corrupting its CRDT and its
  // undo history while the next file is loading.
  editor.dispatch({ effects: loroCompartment.reconfigure([]) })
  state.selected = undefined
  state.document = undefined
  reviewerSyncReady = false
  state.review = emptyReviewState
  applyPresenceRoster([])
  editor.dispatch({ effects: setReviewItems.of([]) })
  closeReviewPopover()
  if (editor.state.doc.length > 0) {
    editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: "" } })
  }
  setSyncStatus("disconnected", "Open a document to work with other people on it")
  setText("#document-name", "No document open")
  setText("#document-path", "")
  setText("#revision-label", "")
  clearPreview()
  renderDiagnostics([])
  renderWorkspaceState()
  renderSectionOutline()
}

// ---------------------------------------------------------------------------
// Navigator: file tree + section outline
// ---------------------------------------------------------------------------

/**
 * The file tree.
 *
 * Folders are derived from the documents' paths (see outline.ts) because that is
 * what the project model actually is — a flat set of path-addressed files. The
 * previous flat list with a truncated path suffix hid the structure authors put
 * there. The entrypoint carries a MAIN tag: it is the file the preview builds
 * from, and that is the only question the tag needs to answer.
 */
function renderFileTree(): void {
  const host = el<HTMLElement>("#file-tree")
  if (!host || !state.project) return
  setText("#file-count", state.outline.length === 0 ? "" : String(state.outline.length))
  if (state.outline.length === 0) {
    host.innerHTML = `<div class="nav-empty">No files yet.<br /><button id="add-document-empty" class="btn btn-primary btn-small" type="button" style="margin-top:8px">Add a file</button></div>`
    el("#add-document-empty")?.addEventListener("click", addDocument)
    applyRoleGates()
    return
  }
  const tree = buildFileTree(state.outline.map((entry) => ({ path: entry.document.path, item: entry })))
  host.replaceChildren(...renderTreeNodes(tree, 0))
  applyRoleGates()
}

/** Folders the user has collapsed; everything is expanded until they say otherwise. */
const collapsedFolders = new Set<string>()

function renderTreeNodes(nodes: readonly TreeNode<OutlineEntry>[], depth: number): HTMLElement[] {
  const out: HTMLElement[] = []
  for (const node of nodes) {
    const item = document.createElement("div")
    item.className = "tree-item"
    for (let level = 0; level < depth; level++) {
      const indent = document.createElement("span")
      indent.className = "indent"
      item.append(indent)
    }
    if (node.type === "folder") {
      const collapsed = collapsedFolders.has(node.path)
      const row = document.createElement("button")
      row.type = "button"
      row.className = "tree-row folder"
      row.setAttribute("aria-expanded", String(!collapsed))
      row.innerHTML = `<span class="twist" aria-hidden="true">${collapsed ? "▸" : "▾"}</span><span class="label"></span>`
      const label = row.querySelector<HTMLElement>(".label")
      if (label) label.textContent = node.name
      row.addEventListener("click", () => {
        if (collapsed) collapsedFolders.delete(node.path)
        else collapsedFolders.add(node.path)
        renderFileTree()
      })
      item.append(row)
      out.push(item)
      if (!collapsed) out.push(...renderTreeNodes(node.children, depth + 1))
      continue
    }
    const entry = node.item
    const wrap = document.createElement("div")
    wrap.className = "tree-row-wrap"
    const row = document.createElement("button")
    row.type = "button"
    row.className = "tree-row"
    if (state.selected?.document.id === entry.document.id) row.classList.add("active")
    row.dataset.document = entry.document.id
    row.title = `${entry.document.path} — double-click to rename`
    row.innerHTML = `<span class="twist" aria-hidden="true"></span><span class="label"></span>`
    const label = row.querySelector<HTMLElement>(".label")
    if (label) label.textContent = node.name
    if (isEntrypoint(entry.document.path)) {
      const tag = document.createElement("span")
      tag.className = "tag"
      tag.textContent = "MAIN"
      tag.title = "The preview is built from this file"
      row.append(tag)
    }
    row.addEventListener("click", () => openDocument(entry))
    row.addEventListener("dblclick", () => renameDocument(entry))
    const remove = document.createElement("button")
    remove.type = "button"
    remove.className = "btn-icon btn-danger"
    remove.dataset.deleteDocument = entry.document.id
    remove.title = "Delete this file"
    remove.setAttribute("aria-label", `Delete ${entry.document.path}`)
    remove.textContent = "×"
    remove.addEventListener("click", (event) => { event.stopPropagation(); deleteDocument(entry) })
    wrap.append(row, remove)
    item.append(wrap)
    out.push(item)
  }
  return out
}

/**
 * Which file the preview builds from. The project model has no explicit
 * entrypoint field yet, so the convention is `main.typ` at the root, falling back
 * to the first file — the same choice compileCurrent makes, kept in one place so
 * the tag and the build can never disagree.
 */
function entrypointPath(): string | undefined {
  const paths = state.outline.map((entry) => entry.document.path)
  return paths.find((path) => path === "main.typ") ?? paths[0]
}

function isEntrypoint(path: string): boolean {
  return entrypointPath() === path
}

function renameDocument(entry: OutlineEntry): void {
  const project = state.project
  if (!project) return
  promptInPanel("File", "Rename file", "New name", (title) => {
    run(api.updateDocument(project.id, entry.document.id, { title }), () => {
      status("File renamed")
      loadOutline()
    })
  }, { placeholder: entry.document.title })
}

function deleteDocument(entry: OutlineEntry): void {
  const project = state.project
  if (!project) return
  promptInPanel("File", "Delete file", `This cannot be undone. Type "${entry.document.title}" to confirm.`, (confirmText) => {
    if (confirmText.trim() !== entry.document.title.trim()) {
      status("That did not match the file name — nothing was deleted")
      return
    }
    const reconcileDeletedDocument = (): void => {
      status("File deleted")
      if (state.selected?.document.id === entry.document.id) closeOpenDocument()
      loadOutline()
    }
    run(api.deleteDocument(project.id, entry.document.id), reconcileDeletedDocument, (error) => {
      // Deletes are idempotent from the editor's point of view. Another tab or
      // collaborator may win the race; a 404 then confirms the desired final
      // state and must close the stale document/relay instead of leaving a
      // ghost row that is later misdiagnosed as revoked project access.
      if (error instanceof api.ApiError && error.status === 404) {
        reconcileDeletedDocument()
        return
      }
      status(error instanceof Error ? error.message : "The API request failed")
    })
  }, { placeholder: entry.document.title })
}

/** Kept for callers that still speak in terms of "the outline of the project". */
function renderOutline(): void {
  renderFileTree()
  renderCrumbs()
}

/**
 * The section outline: the open document's headings, live.
 *
 * This is the navigation writers actually use in a long text, and it did not
 * exist before. It is rebuilt from the source on every edit (the parse is
 * memoised) and highlights the heading the caret is under.
 */
let currentHeadings: readonly Heading[] = []

function renderSectionOutline(): void {
  const host = el<HTMLElement>("#section-outline")
  if (!host) return
  if (!state.document) {
    host.innerHTML = `<div class="nav-empty">Open a file to see its sections.</div>`
    return
  }
  if (currentHeadings.length === 0) {
    host.innerHTML = `<div class="nav-empty">No headings yet. Start a line with <code>=</code> to make one.</div>`
    return
  }
  const active = activeHeadingIndex(currentHeadings, editor.state.selection.main.head)
  host.replaceChildren(...currentHeadings.map((heading, index) => {
    const row = document.createElement("button")
    row.type = "button"
    row.className = index === active ? "outline-row active" : "outline-row"
    row.dataset.level = String(Math.min(heading.level, 6))
    if (index === active) row.setAttribute("aria-current", "true")
    const title = document.createElement("span")
    title.className = "title"
    title.textContent = heading.title
    const line = document.createElement("span")
    line.className = "ln"
    line.textContent = String(editor.state.doc.lineAt(Math.min(heading.from, editor.state.doc.length)).number)
    row.append(title, line)
    row.addEventListener("click", () => revealPosition(heading.from))
    return row
  }))
}

/** Scrolls the editor to an offset, selects the line start, and focuses. */
function revealPosition(position: number): void {
  const target = Math.min(position, editor.state.doc.length)
  editor.dispatch({ selection: { anchor: target }, scrollIntoView: true, effects: EditorView.scrollIntoView(target, { y: "start", yMargin: 40 }) })
  editor.focus()
}

/**
 * The sticky heading: which section you are inside, pinned above the text.
 * Borrowed from VS Code's sticky scroll; in a long section it answers "where am
 * I?" without a glance at the sidebar.
 */
function renderStickyHeading(): void {
  const host = el<HTMLElement>("#sticky-heading")
  if (!host) return
  if (!state.document) { host.hidden = true; return }
  const trail = headingTrail(currentHeadings, editor.state.selection.main.head)
  if (trail.length === 0) { host.hidden = true; return }
  host.hidden = false
  host.replaceChildren()
  trail.forEach((heading, index) => {
    if (index > 0) {
      const separator = document.createElement("span")
      separator.className = "crumb-sep"
      separator.textContent = "›"
      host.append(separator)
    }
    const span = document.createElement("span")
    if (index === trail.length - 1) span.className = "leaf"
    span.textContent = heading.title
    host.append(span)
  })
}

/** Re-derives everything that depends on the document text. */
function refreshDocumentStructure(): void {
  currentHeadings = state.document ? documentHeadings(editor.state.doc.toString()) : []
  renderSectionOutline()
  renderStickyHeading()
  const words = wordCount(editor.state.doc.toString())
  setText("#word-count", `${words.toLocaleString()} ${words === 1 ? "word" : "words"}`)
}

function createProject(): void {
  promptInPanel("Projects", "New project", "What is it called?", (name) => {
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
 * selected", losing their place. Two storage tiers keep that convenience
 * without collapsing multi-tab sessions into one project (live-session
 * finding: two tabs on two projects both reloaded into whichever tab wrote
 * last):
 *
 * - sessionStorage — THIS tab's restore target. Not shared between tabs, so
 *   each tab reloads into the project it actually had open.
 * - localStorage — the most recent project-bearing entry anywhere, used only
 *   when a tab has no session record of its own (a brand-new tab), so it
 *   still lands in the last project instead of the project list.
 *
 * The restore runs after the project list loads, reopening the project and
 * (if its outline still contains it) the document.
 */
const LAST_OPEN_KEY = "nisaba.lastOpen"
interface LastOpen { readonly projectId?: string; readonly documentId?: string }

function persistLastOpen(entry: LastOpen): void {
  try {
    const encoded = JSON.stringify(entry)
    // This tab's record always reflects the write; the global record only
    // gains project-bearing entries — clearing it is a separate, guarded
    // operation (clearGlobalLastOpen) so one tab leaving a project cannot
    // wipe another tab's more recent project.
    sessionStorage.setItem(LAST_OPEN_KEY, encoded)
    if (entry.projectId) localStorage.setItem(LAST_OPEN_KEY, encoded)
  } catch { /* storage may be unavailable */ }
}

/** Drops the global new-tab default only when it points at `projectId`. */
function clearGlobalLastOpen(projectId: string): void {
  try {
    const global = JSON.parse(localStorage.getItem(LAST_OPEN_KEY) ?? "{}") as LastOpen
    if (global.projectId === projectId) localStorage.removeItem(LAST_OPEN_KEY)
  } catch { /* storage may be unavailable */ }
}

function readLastOpen(): LastOpen {
  try {
    // This tab's own record first; a fresh tab falls back to the global one.
    const stored = sessionStorage.getItem(LAST_OPEN_KEY) ?? localStorage.getItem(LAST_OPEN_KEY)
    return JSON.parse(stored ?? "{}") as LastOpen
  } catch { return {} }
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
  // With a remembered document, that document wins; without one, the
  // project's default file (if any) gets its chance inside openProject.
  openProject(project, { fromLastOpen: last.documentId !== undefined })
  if (!last.documentId) return
  // openProject loads the outline asynchronously; wait for it, then open the document.
  const tryOpen = (attempts: number): void => {
    // Abort if the user has already manually opened a document during the
    // polling window — don't override their choice. (Same still-open guard
    // as the default-file poller: ids are globally unique so a stale
    // outline from a previous project can't produce a false match here,
    // but a switch away must stop the polling.)
    if (state.project?.id !== last.projectId) return
    if (state.selected) return
    const entry = state.outline.find((e) => e.document.id === last.documentId)
    if (entry) { openDocument(entry); return }
    if (attempts > 0) setTimeout(() => tryOpen(attempts - 1), 200)
  }
  setTimeout(() => tryOpen(20), 200) // up to ~4s for the outline to load
}

function openProject(project: Project, options: { readonly fromLastOpen?: boolean } = {}): void {
  state.project = project
  state.selected = undefined
  state.document = undefined
  // Drop the outgoing project's outline immediately: until loadOutline's
  // fetch resolves, any consumer reading state.outline (file tree, the
  // default-file poller below) would otherwise see the previous project's
  // entries — and paths like main.typ collide across projects.
  state.outline = []
  // Role is unknown until the membership fetch resolves; reset so a stale role
  // from a previous project can't leak into the reviewer UX gates.
  state.role = undefined
  // M5: remember which project is open so a tab-away/return restores it instead
  // of dropping the user back on the project list.
  persistLastOpen({ projectId: project.id })
  renderWorkspaceState()
  renderFileTree()
  loadOutline()
  // The per-project default file (Settings dock) opens only when this entry
  // carries no more specific target of its own — a restore WITH a last-open
  // document arms its own poller instead, so exactly one poller runs. A
  // manual pick during the window overrides either (abort-if-selected).
  if (!options.fromLastOpen) {
    const defaultPath = loadDefaultFile(project.id)
    if (defaultPath !== undefined) {
      const tryDefault = (attempts: number): void => {
        if (state.project?.id !== project.id) return
        if (state.selected) return
        // Match by path AND owning project: the outline is per-project by
        // construction, but this guard keeps the poller correct even if the
        // outline is ever populated cross-project again.
        const entry = state.outline.find((e) => e.document.project_id === project.id && e.document.path === defaultPath)
        if (entry) { openDocument(entry); return }
        if (attempts > 0) setTimeout(() => tryDefault(attempts - 1), 200)
      }
      setTimeout(() => tryDefault(20), 200) // same ~4s outline window as restoreLastOpen
    }
  }
  // Each of these callbacks writes project-scoped state (references, fulltexts,
  // role). Rapidly opening project A then B delivers responses out of order, so
  // without the same still-open guard loadOutline uses, a late response for A
  // would hand B the wrong citation completer entries or — worse for the
  // reviewer UX — the wrong role.
  run(api.listReferences(project.id), (references) => {
    if (state.project?.id !== project.id) return
    state.references = references
    renderProjectFacts()
  })
  run(api.listFulltexts(project.id), (fulltexts) => {
    if (state.project?.id !== project.id) return
    state.fulltexts = new Map(fulltexts.map((item) => [item.reference_id, item]))
    renderProjectFacts()
  })
  // Fetch the caller's project-scoped role to gate reviewer UX: a reviewer is
  // locked into suggesting mode (H1) and has Export hidden (M4). On failure,
  // default to read-only (least privilege) so a transient error does not grant
  // author-level UI powers to non-authors. The failure path needs the guard
  // too: A's failed membership resolving after B opened would lock B's UI to
  // read-only on A's behalf.
  run(api.getMembership(project.id), (membership) => {
    if (state.project?.id !== project.id) return
    state.role = membership.role
    applyRoleGates()
  }, () => {
    if (state.project?.id !== project.id) return
    state.role = "read-only"
    applyRoleGates()
  })
  // The app-bar roster shows everyone with access — members were previously
  // visible only inside the Share/Invite dock, which non-managing members
  // (and new users) had no reason to open. Any member may read the list.
  run(api.listMembers(project.id), (members) => {
    if (state.project?.id !== project.id) return
    renderPeopleStrip(members)
  }, () => hidePeopleStrip())
}

/** Renders the who-has-access chips into the app bar (all members can see it). */
function renderPeopleStrip(members: readonly api.Membership[]): void {
  const host = el<HTMLElement>("#project-people")
  if (!host) return
  if (!state.project) { host.hidden = true; return }
  host.innerHTML = members
    .map((m) => {
      const label = ROLE_LABELS[m.role] ?? m.role
      return `<span class="person" title="${escapeHtml(m.subject)} · ${escapeHtml(label)}"><strong>${escapeHtml(m.subject)}</strong><span class="role-tag">${escapeHtml(label)}</span></span>`
    })
    .join("")
  host.hidden = false
}

function hidePeopleStrip(): void {
  const host = el<HTMLElement>("#project-people")
  if (host) { host.hidden = true; host.innerHTML = "" }
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
    renderProjectFacts()
  })
}

/**
 * The navigator's footer: the project's standing facts, as facts rather than
 * buttons. Files, how many references still lack an attached PDF (which is what
 * blocks an export), and which file the preview builds from.
 */
function renderProjectFacts(): void {
  const host = el<HTMLElement>("#nav-foot")
  if (!host) return
  const files = state.outline.length
  const references = state.references.length
  const withFulltext = state.references.filter((reference) => state.fulltexts.has(reference.id)).length
  const entry = entrypointPath()
  host.replaceChildren()
  const line = (label: string, value: string): void => {
    const row = document.createElement("div")
    row.append(`${label} `)
    const strong = document.createElement("b")
    strong.textContent = value
    row.append(strong)
    host.append(row)
  }
  line("files", `${files}`)
  line("references", references === 0 ? "none yet" : `${withFulltext} of ${references} with a PDF`)
  if (entry !== undefined) line("preview builds", entry)
}

function addDocument(): void {
  const project = state.project
  if (!project) return
  promptInPanel("Files", "New file", "Name and folder", (pathValue) => {
    const documentPath = pathValue.endsWith(".typ") ? pathValue : `${pathValue}.typ`
    const title = documentPath.split("/").pop()?.replace(/\.typ$/i, "") || "Untitled"
    run(api.createDocument(project.id, { path: documentPath, title }), () => {
      if (state.project?.id !== project.id) return
      status("File created")
      loadOutline()
    })
  }, { placeholder: "chapters/introduction.typ" })
}

/**
 * Adds a demo document with substantial Typst content. The seeded references
 * and the generated body live in ./demo-content, pulled via dynamic import()
 * so ~250 lines of demo material stay out of the critical bundle and load only
 * when the (role-gated) button is clicked.
 */
function addDemoFile(): void {
  const project = state.project
  if (!project) return
  void import("./demo-content").then(({ DEMO_REFERENCE_TITLES: refTitles, generateDemoBody }) => {
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
    // A failed chunk load (offline first click, cache eviction) should tell the
    // user something rather than die as an unhandled rejection.
  })
    .catch(() => status("The demo document could not be loaded"))
}



// ---------------------------------------------------------------------------
// Document loading, sync, autosave
// ---------------------------------------------------------------------------

let syncConnection: SyncConnection | undefined
let documentAccessRevoked = false
// Reviewer changes have no REST baseline fallback: they are durable only after
// the CRDT binding is live. Authors can keep working through relay outages
// because autosave persists their baseline, but a reviewer must stay locked
// until the initial welcome succeeds (and after a fatal protocol/auth failure).
let reviewerSyncReady = false

function openDocument(entry: OutlineEntry): void {
  const project = state.project
  if (!project) return
  if (entry.document.id !== state.selected?.document.id && failedSave?.projectId === project.id && failedSave.context.documentId === state.selected?.document.id) {
    status("Unsaved offline changes — reconnect before switching files")
    return
  }
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
  syncConnection = undefined
  // A socket close does not detach CodeMirror's Loro plugin. Remove the old
  // binding before any clear/load transaction so rapid file switches cannot
  // write the next editor state into the previous document's replica.
  editor.dispatch({ effects: loroCompartment.reconfigure([]) })
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
  documentAccessRevoked = false
  reviewerSyncReady = false
  editor.dispatch({ effects: editableComp.reconfigure(EditorView.editable.of(false)) })
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
  setText("#document-path", entry.document.path)
  status("Opening…")
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
      state.document = document
      // Re-apply the role gate after the review reset: openDocument wipes review
      // state (including a reviewer's forced-suggesting) so the lock must be
      // re-established here, not only in openProject (H1).
      applyRoleGates()
      renderWorkspaceState()
      // The replica starts empty; loadIntoEditor seeds the persisted body into both
      // the replica and CodeMirror (so the user sees content immediately AND the
      // loro-codemirror binding's init reconcile does not blank the editor). On
      // connect, connectSync resolves this seeded body to a single authoritative
      // origin to avoid CRDT duplication (bug N1): the first client to reach an
      // empty relay pushes its seed; later clients CLEAR their local seed and adopt
      // the relay's snapshot.
      const replica = newReplica()
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
      setText("#revision-label", `v${document.revision}`)
      status("Ready")
      // Everything derived from the text: outline, sticky heading, word count,
      // breadcrumb, and the review surfaces for the document just opened.
      refreshDocumentStructure()
      renderCrumbs()
      renderReviewBanner()
      renderReviewDock()
      // A newly opened document has no build yet; the compile module owns
      // lastBuild and this is the one write it accepts from outside.
      resetBuildSummary()
      // History is document-scoped. If the dock stayed open while switching
      // files, replace its pending/previous request with the newly-opened file's
      // timeline. The response guard in openHistory still discards the old one.
      if (dockTool === "history") openHistory()
      connectDocument(document, replica)
      // A reload can race a keepalive PATCH from the page being replaced: this
      // GET may return the old revision even though that save commits moments
      // later. Recheck once after the unload-save window. Only adopt a newer
      // revision while the new page is still pristine; local or peer edits make
      // the editor diverge from its loaded REST baseline and must never be
      // overwritten by this recovery path.
      setTimeout(() => {
        void Effect.runPromise(api.getDocument(project.id, documentId)).then((latest) => {
          const current = state.document
          if (state.selected?.document.id !== documentId || !current) return
          if (latest.revision <= current.revision) return
          if (pendingSave || saveTimer !== undefined || saveInFlight) return
          if (editor.state.doc.toString() !== current.body) return
          state.document = latest
          state.selected = { document: latest }
          state.outline = state.outline.map((item) => item.document.id === documentId ? { document: latest } : item)
          loadIntoEditor(latest.body)
          setText("#revision-label", `v${latest.revision}`)
          refreshDocumentStructure()
          renderOutline()
          renderCrumbs()
          status("Saved")
        }, () => undefined)
      }, 2_500)
    }
  )
}

function loadIntoEditor(body: string): void {
  // Seed ONLY the CodeMirror doc with the persisted body for immediate display.
  // The Loro replica is NOT seeded here — it stays empty until connectSync's
  // WELCOME handler either imports the relay's snapshot or seeds it as the
  // origin. This prevents the 2026-08-09 collaboration bug where a locally-
  // seeded private text container made delta exports arrive as "pending" ops
  // at the relay (the container's creation was below the syncFrom baseline).
  //
  // The CRDT binding (LoroExtensions) is also NOT attached here — it is
  // attached in the onReady callback after the WELCOME, when the replica has
  // its definitive content. The editor is a plain CodeMirror instance until
  // then; this is fine because the WELCOME arrives in milliseconds.
  isLoadingDocument = true
  try {
    editor.dispatch({
      changes: { from: 0, to: editor.state.doc.length, insert: body },
      effects: [
        setReviewItems.of(state.review.items),
        setDiagnostics.of([]),
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
    setSyncStatus("disconnected", "Sign in to work with other people; your edits are still saved to the project")
    syncConnection = undefined
    return
  }
  syncConnection = connectSync(replica, {
    documentId: document.id,
    token,
    seedBody: document.body,
    // Called after the WELCOME handler has given the replica its definitive
    // content (either imported from the relay or seeded as the origin). The
    // CRDT binding is attached HERE so the replica never carries a stale local
    // seed whose container creation would sit below the export baseline (the
    // 2026-08-09 "pending ops" collaboration bug). At this point CM and the
    // replica already agree (both have the same body), so the binding's init
    // reconcile is a no-op (no editor blank).
    onReady: () => {
      // Attach the CRDT binding AND sync CM to the replica's text in one guarded
      // transaction. Without the explicit CM sync, the binding's init-reconcile
      // (a microtask) would detect a mismatch (CM has the REST body, the replica
      // has the relay-imported body — they can differ by whitespace) and dispatch
      // a CM replacement. That replacement flows through the binding into the
      // replica as a text-touching local update, which the relay's reviewer gate
      // rejects (4003). By syncing CM here (under isLoadingDocument so no
      // listener fires), the init-reconcile sees CM == replica and is a no-op.
      isLoadingDocument = true
      try {
        const replicaText = getTextFromDoc(activeLoro).toString()
        editor.dispatch({
          changes: { from: 0, to: editor.state.doc.length, insert: replicaText },
          effects: [
            loroCompartment.reconfigure(
              LoroExtensions(activeLoro, undefined, new UndoManager(activeLoro, { excludeOriginPrefixes: ["load"] }), getTextFromDoc, stageReviewUpdate)
            )
          ]
        })
      } finally {
        isLoadingDocument = false
      }
      // Tell the room who we are and where we are as soon as the handshake is
      // complete, so peers see us without waiting for the next caret move.
      publishPresence()
      reviewerSyncReady = true
      applyRoleGates()
    },
    onStatus: (value, detail) => {
      if (value === "unsupported") {
        reviewerSyncReady = false
        applyRoleGates()
      }
      setSyncStatus(value, detail)
    },
    onAccessRevoked: (message) => {
      // Stop edits immediately when a membership/project disappears underneath
      // an open socket. Keeping the stale binding editable accepted local text
      // that could never be persisted and often surfaced an unrelated review
      // policy error instead.
      documentAccessRevoked = true
      if (saveTimer !== undefined) clearTimeout(saveTimer)
      saveTimer = undefined
      pendingSave = undefined
      isLoadingDocument = true
      try {
        editor.dispatch({
          changes: state.document
            ? { from: 0, to: editor.state.doc.length, insert: state.document.body }
            : undefined,
          effects: [
            loroCompartment.reconfigure([]),
            editableComp.reconfigure(EditorView.editable.of(false))
          ]
        })
      } finally {
        isLoadingDocument = false
      }
      status(message.includes("changed")
        ? "Project access changed — reopen the project"
        : "Project access revoked — this document is now read-only")
    },
    onPresence: (peers) => {
      applyPresenceRoster(peers)
    }
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

/**
 * The short status word for the sync cell. One mapping shared by setSyncStatus
 * and renderPresence — the two previously re-derived it independently and their
 * fallbacks drifted ("Local" vs "No document"). `status` is undefined before the
 * first relay callback: with no document open the label stays "No document"
 * (the shell's initial text); with one open, the editor works locally.
 */
function syncShortLabel(status: SyncStatus | undefined): string {
  if (browserOffline) return "Offline"
  if (status === "connected") return "Live"
  if (status === "connecting") return "Connecting…"
  if (status === "unsupported") return "Sync off"
  return state.document ? "Local" : "No document"
}

function setSyncStatus(value: SyncStatus, detail?: string): void {
  // Remember the latest relay status so the offline listener can restore it on
  // reconnect without a redundant status callback.
  lastSyncStatus = value
  lastSyncDetail = detail
  const cell = el<HTMLElement>("#connection-state")
  const dot = el<HTMLElement>("#status-dot")
  const effective: SyncStatus = browserOffline ? "disconnected" : value
  if (dot) dot.dataset.state = effective
  // The short word is the state; the sentence is the explanation, and it goes in
  // the tooltip so the bar stays scannable. "Live" is only ever claimed when the
  // relay says so — going offline dims it immediately, without waiting for the
  // WebSocket to notice.
  const short = syncShortLabel(value)
  const explanation = browserOffline ? "You are offline — your work is still saved to this device and syncs when you reconnect"
    : value === "connected" ? "Connected: other people see your edits as you type"
      : value === "connecting" ? "Reconnecting to the collaboration server…"
        : value === "unsupported" ? `Collaboration unavailable${detail ? ` · ${detail}` : ""}`
          : detail ?? "Not connected — your edits are still saved to the project"
  setText("#sync-label", presenceSuffix(short))
  if (cell) cell.title = explanation
}

// ---------------------------------------------------------------------------
// Presence: who else is here, and where
//
// The relay has always kept a roster with heartbeats; the client never read it,
// so the UI could only say "2 collaborators online". Now every peer publishes
// their name, file, section, and line, and the header shows them as avatars.
// ---------------------------------------------------------------------------

let presencePeers: readonly PresencePeer[] = []

/**
 * Applies one roster frame everywhere it matters: the avatar row, the "N
 * here" status suffix, and the remote-cursor decorations in the editor (a
 * peer renders a caret only while they are in THIS document's room and their
 * published path matches — a stale path from a just-switched peer is dropped
 * rather than drawn in the wrong file).
 */
function applyPresenceRoster(peers: readonly PresencePeer[]): void {
  presencePeers = peers
  renderPresence()
  const openPath = state.selected?.document.path
  const cursors: RemoteCursor[] = []
  for (const peer of peers) {
    if (peer.line === undefined) continue
    if (openPath !== undefined && peer.path !== undefined && peer.path !== openPath) continue
    cursors.push({
      peer: peer.peer,
      name: peer.name,
      line: peer.line,
      column: peer.column ?? 1,
      hue: authorHue(peer.name || String(peer.peer)),
    })
  }
  editor.dispatch({ effects: setRemoteCursors.of(cursors) })
}

/** The status bar's sync cell reads "Live · 3 here" once other people are present. */
function presenceSuffix(short: string): string {
  return presencePeers.length === 0 ? short : `${short} · ${presencePeers.length + 1} here`
}

function renderPresence(): void {
  const host = el<HTMLElement>("#presence")
  if (!host) return
  host.replaceChildren()
  // Cap the stack: beyond four avatars the row stops being scannable, and the
  // remainder is more useful as a count.
  const shown = presencePeers.slice(0, 4)
  for (const peer of shown) {
    const avatar = document.createElement("span")
    avatar.className = "avatar"
    avatar.style.setProperty("--hue", String(authorHue(peer.name || String(peer.peer))))
    avatar.textContent = initialsOf(peer.name)
    const where = peerLocation(peer)
    avatar.title = where === "" ? peer.name || "Someone else" : `${peer.name || "Someone else"} — ${where}`
    host.append(avatar)
  }
  if (presencePeers.length > shown.length) {
    const more = document.createElement("span")
    more.className = "more"
    more.textContent = `+${presencePeers.length - shown.length}`
    host.append(more)
  }
  // Re-rendering the avatar row also refreshes the "N here" suffix on the sync
  // label, using the same shared short-word mapping as setSyncStatus.
  setText("#sync-label", presenceSuffix(syncShortLabel(lastSyncStatus)))
}

/**
 * Publishes where this client is working. Called on caret moves, so it is
 * throttled to one frame's worth of change and the connection itself drops
 * frames that would repeat the state already on the wire.
 */
function publishPresence(): void {
  const connection = syncConnection
  const document_ = state.selected?.document
  if (!connection || !document_) return
  const head = editor.state.selection.main.head
  const caretLine = editor.state.doc.lineAt(Math.min(head, editor.state.doc.length))
  const line = caretLine.number
  const column = Math.min(head, editor.state.doc.length) - caretLine.from + 1
  const trail = headingTrail(currentHeadings, head)
  connection.publishPresence({
    name: currentUserDisplayName(),
    path: document_.path,
    section: trail.at(-1)?.title,
    line,
    column
  })
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
/** Completion of the currently-running baseline PATCH, used by dependent actions. */
let saveCompletion: Promise<void> | undefined
/** A baseline write that failed at the transport boundary and must not be discarded by navigation. */
let failedSave: { readonly projectId: string; readonly context: SaveContext } | undefined

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
  // Reviewers/read-only viewers have no baseline to persist (the server 403s
  // their PATCH); their edits are synced through the review layer instead, so
  // the "Unsaved changes" autosave dance would only produce a stuck error.
  if (state.role !== undefined && state.role !== "owner" && state.role !== "author") {
    if (state.role === "reviewer") status("Suggestion tracked")
    return
  }
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
  // Reviewers and read-only viewers have no baseline to save: the server
  // rejects their PATCH (403), which used to leave the status bar permanently
  // stuck on the permission error after every suggestion. Their edits live in
  // the shared review layer (synced via the relay), not in the app database.
  if (state.role !== undefined && state.role !== "owner" && state.role !== "author") {
    if (context.body !== state.document?.body) {
      status(state.role === "reviewer" ? "Suggestion tracked (synced via review)" : "View-only")
    }
    return
  }
  if (context.body === state.document?.body && state.selected?.document.id === context.documentId) { status("Saved"); return }
  status("Saving…")
  // Mark the request as in-flight so the beforeunload guard can warn about an
  // unsaved PATCH — saveTimer/pendingSave are already cleared by this point.
  saveInFlight = true
  let completeSave!: () => void
  const completion = new Promise<void>((resolve) => { completeSave = resolve })
  saveCompletion = completion
  void completion.finally(() => {
    if (saveCompletion === completion) saveCompletion = undefined
  })
  run(
    api.saveDocument(projectId, context.documentId, context.body, context.revision),
    (saved) => {
      saveInFlight = false
      const recoveringFailedSave = failedSave?.projectId === projectId && failedSave.context.documentId === context.documentId
      if (failedSave?.context.documentId === context.documentId) failedSave = undefined
      // The PATCH body is an immutable snapshot captured when local typing was
      // scheduled. Peer suggestions may arrive while that request is in flight;
      // never promote the then-current CRDT/editor text to the REST baseline.
      // Keep `state.document.body` aligned with exactly what this PATCH wrote,
      // while preserving server-owned metadata/revision from the response.
      const persisted = { ...saved, body: context.body }
      // Only update the open document if we are still editing the document this
      // save was for.
      if (state.selected?.document.id === context.documentId) {
        state.document = persisted
        setText("#revision-label", `v${persisted.revision}`)
      }
      status("Saved")
      // Reconnecting the CRDT socket can deliver an empty relay catch-up in the
      // same window as the successful retry and clear CodeMirror after REST has
      // durably accepted the offline body. Repair only that impossible-looking
      // state (non-empty recovered save, empty pristine editor); never replace a
      // non-empty editor, which may already include legitimate peer edits.
      if (recoveringFailedSave && persisted.body.length > 0) {
        setTimeout(() => {
          if (state.selected?.document.id !== context.documentId || editor.state.doc.length !== 0) return
          loadIntoEditor(persisted.body)
          refreshDocumentStructure()
          status("Saved")
        }, 500)
      }
      completeSave()
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
          completeSave()
          return
        }
        void Effect.runPromise(
          api.getDocument(projectId, context.documentId)
        ).then(
          (latest) => {
            if (state.selected?.document.id !== context.documentId) { status("Saved elsewhere"); completeSave(); return }
            state.document = latest
            setText("#revision-label", `v${latest.revision}`)
            const localBody = editor.state.doc.toString()
            if (localBody === latest.body) {
              // Already in sync (the CRDT merged the peer edit); nothing to write.
              status("Saved")
              completeSave()
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
            completeSave()
          },
          () => { status("Save conflicted; couldn't reload the latest revision"); completeSave() }
        )
        return
      }
      if (!(error instanceof api.ApiError) || error.status === undefined) failedSave = { projectId, context }
      status(error instanceof Error ? error.message : "The API request failed")
      completeSave()
    }
  )
}

/**
 * Establishes the REST baseline required by server-side project operations.
 * Collaboration keeps the live editor current, but export/history APIs read the
 * persisted document, so they must not race the autosave debounce or an active
 * PATCH. Failure rejects and the caller must not continue with stale content.
 */
async function saveBeforeServerSnapshot(): Promise<void> {
  if (saveTimer !== undefined) {
    clearTimeout(saveTimer)
    saveTimer = undefined
  }
  pendingSave = undefined
  if (saveCompletion) await saveCompletion

  const project = state.project
  const context = captureSaveContext()
  if (!project || !context || context.body === state.document?.body) return
  status("Saving before export…")
  saveInFlight = true
  try {
    const saved = await Effect.runPromise(
      api.saveDocument(project.id, context.documentId, context.body, context.revision)
    )
    if (state.selected?.document.id === context.documentId) {
      state.document = saved
      setText("#revision-label", `v${saved.revision}`)
    }
    status("Saved")
  } catch (error) {
    status(error instanceof Error ? error.message : "Couldn't save before export")
    throw error
  } finally {
    saveInFlight = false
  }
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
  showModal(
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
      // Remote peers' carets (Overleaf-style): colored caret + name flag per
      // presence-roster entry, hue-matched to the avatar chips. The cursor set
      // is driven by applyPresenceRoster; it is empty until a roster arrives.
      ...remoteCursors,
      // Typst command + reference/citation completions. Placed after basicSetup
      // (which already pulls in default autocompletion) so these sources augment
      // the defaults rather than replacing them.
      autocompletion({ override: [typstCompletions, referenceCompletions] }),
      // Tab accepts the current autocomplete suggestion (muscle memory from most editors).
      keymap.of([{ key: "Tab", run: acceptCompletion }]),
      // On Chromium/Linux the adapter's generic Mod-z binding also matched
      // Ctrl+Shift+Z, turning redo into a second undo. Intercept the exact
      // conventional redo chord at the highest precedence; Ctrl+Y remains
      // supported by the adapter itself.
      Prec.highest(EditorView.domEventHandlers({
        keydown(event, view) {
          if (!(event.shiftKey && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "z")) return false
          event.preventDefault()
          return loroRedo(view)
        }
      })),
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
            // searchAndScroll awaits page loads that reject when the document
            // is replaced mid-search (a rapid double-click during a recompile);
            // swallow it like the load() call sites rather than surface an
            // unhandled rejection.
            void pdfViewer.searchAndScroll(selected).catch(() => undefined)
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
          // Where the caret is drives four surfaces: the sticky heading, the
          // outline highlight, the breadcrumb's section, and what peers see of us.
          renderStickyHeading()
          renderSectionOutline()
          renderCrumbs()
          publishPresence()
        }
        if (update.docChanged) {
          // Re-parse the document only when text actually changed, then reuse the
          // result for all subsequent cursor moves until the next edit.
          cachedConstructs = findConstructs(update.state.doc.toString())
          // The outline, sticky heading, and word count are all functions of the
          // text, so they refresh here and nowhere else.
          refreshDocumentStructure()
          renderCrumbs()
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
          // `isImportingRemote()` covers synchronous Loro callbacks, while the
          // adapter annotation survives if Loro delivers its subscriber on a
          // later microtask. The annotation is therefore the durable source of
          // truth for peer text. Missing it caused an author's browser to treat
          // an imported reviewer proposal as local typing and PATCH that still-
          // open suggestion into the agreed REST baseline.
          const remote = isImportingRemote() || update.transactions.some((transaction) =>
            transaction.annotation(loroSyncAnnotation) !== undefined)
          const resolution = resolvingSuggestions || update.transactions.some((transaction) =>
            transaction.annotation(resolveAnnotation) === true)
          const localIntent = update.transactions.some((transaction) =>
            transaction.isUserEvent("input") || transaction.isUserEvent("delete") ||
            transaction.annotation(loroSyncAnnotation) === "undo")
          if (!isLoadingDocument) {
            // A suggesting-mode edit is a PROPOSAL: it is persisted through the
            // Loro review map (persistReview) and must never be written into the
            // baseline body — that would silently apply the suggestion, and for
            // reviewers the server correctly rejects baseline PATCHes. Only
            // non-suggestion edits (including accept/reject resolutions, which
            // apply a suggestion to the text) are flushed to the baseline.
            // Local edits are staged by loro-codemirror's before-commit hook so a
            // suggestion record joins the text in the same CRDT transaction. A
            // remote/imported update does not pass through that hook and is
            // reconciled here as before.
            const staged = stagedReviewUpdate?.update === update ? stagedReviewUpdate : undefined
            if (staged) stagedReviewUpdate = undefined
            const recordedSuggestion = staged?.recordedSuggestion ?? updateReviewItems(update)
            if (staged) renderReviewUpdate(update)
            if (!remote && localIntent && !recordedSuggestion && !resolution) {
              scheduleSave()
              // Live error checking: after the typing pause, recompile in the
              // background for fresh diagnostic underlines (no PDF update). Same
              // remote/load exclusions as save — peer imports and the load seed
              // are not user typing, so they must not trigger a diagnostics build.
              scheduleDiagnosticsCompile()
            } else if (!remote && recordedSuggestion) {
              status("Suggestion tracked")
            }
          }
        }
      }),
      loroCompartment.of([])
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

/**
 * Unsubscribe handle for the active document's review-container subscription.
 * One per document: torn down in openDocument before the next document subscribes,
 * so the listener never fires against a stale/closed replica.
 */
let reviewSyncUnsubscribe: (() => void) | undefined

/** Review result prepared inside loro-codemirror immediately before it commits. */
let stagedReviewUpdate: { update: ViewUpdate; recordedSuggestion: boolean } | undefined

/**
 * Attach review metadata while the editor's text mutation is still pending in
 * Loro. The adapter commits immediately after this hook returns, producing one
 * relay frame that contains both the suggestion and the text it annotates.
 */
function stageReviewUpdate(update: ViewUpdate): void {
  stagedReviewUpdate = {
    update,
    recordedSuggestion: updateReviewItems(update, { render: false, commit: false })
  }
}

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
function updateReviewItems(update: ViewUpdate, options: { render?: boolean; commit?: boolean } = {}): boolean {
  // Returns true when this transaction recorded one or more new suggestions (a
  // suggesting-mode edit that must be persisted through the Loro review map and
  // must NOT be written into the baseline body via the REST PATCH).
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
        recordedNew = true
      }
      if (insert.length > 0) {
        const insFromCursor = createCursorAt(activeLoro, fromB)
        const insToCursor = toB > fromB ? createCursorAt(activeLoro, toB) : insFromCursor
        const next = addCoalesced(review, { id: crypto.randomUUID(), kind: "suggestion", from: fromB, to: toB, fromCursor: insFromCursor, toCursor: insToCursor, change: "insert", text: insert.toString(), author, status: "open", createdAt }, fromB, lastPreFrom)
        lastPreFrom = fromB
        review = next
        recordedNew = true
      }
    })
  }
  state.review = review
  if (options.render !== false) renderReviewUpdate(update)
  if (recordedNew) persistReview(options.commit !== false)
  return recordedNew
}

/** Refresh review decorations and surfaces after state.review has been updated. */
function renderReviewUpdate(update: ViewUpdate): void {
  update.view.dispatch({ effects: setReviewItems.of(state.review.items) })
  renderReviewBanner()
  renderReviewSidebar()
}

// ---------------------------------------------------------------------------
// Review persistence + sync (JSON-in-LoroMap)
// ---------------------------------------------------------------------------

/**
 * Writes the current review items into the active replica's "review" LoroMap
 * (one JSON item per map key — see review-persistence.ts for the layout and
 * why per-item keys replaced the single-blob write), so they survive reload
 * and sync to every collaborator through the existing WebSocket relay.
 *
 * It is guarded so it does NOT run while:
 *   * applying a remote review update (applyingRemoteReview) — would echo back;
 *   * seeding a document (isLoadingDocument) — the seed dispatch, not a real change;
 *   * importing remote text (isImportingRemote) — peer edits, not local review edits;
 *   * no replica is active yet (before openDocument) — there is nowhere to write.
 */
function persistReview(commit = true): void {
  if (applyingRemoteReview || isLoadingDocument || isImportingRemote()) return
  const doc = activeLoro
  try {
    if (writeReviewItemsToMap(doc, state.review.items) && commit) {
      doc.commit({ origin: "review" })
    }
  } catch { /* replica torn down mid-document-switch: silently skip */ }
}

/**
 * Replaces local review items from a JSON payload read off the Loro map, guarding
 * against the write feedback loop with applyingRemoteReview and the save/retrack
 * guards with the existing flags. Called for both initial load (a prior session's
 * snapshot) and live remote updates.
 *
 * The remote list is MERGED with the current local list (local wins per id): a
 * suggestion the user typed before this catch-up arrived must not be dropped
 * from the UI (it is already persisted in the map via persistReview's merge).
 */
function applyRemoteReview(items: readonly ReviewItem[]): void {
  applyingRemoteReview = true
  try {
    const merged = mergeReviewItems(items, state.review.items)
    state.review = { ...state.review, items: merged, capability: "available" }
    editor.dispatch({ effects: setReviewItems.of(merged) })
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
    const items = readReviewItemsFromMap(doc)
    return items.length > 0 ? items : undefined
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
  if (!project) { showPanel("references", `<p class="empty-note">Open a project first.</p>`); return }
  showPanel(
    "references",
    `<label class="field">Search the library<input id="reference-filter" type="search" placeholder="Title, author, DOI or PMID" /></label>
     <button id="add-reference" class="btn" type="button">Add reference</button>
     <div id="reference-results" class="list"></div>
     <p class="dock-note">Every reference the text cites needs an attached PDF before the project can be exported.</p>`
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
  showModal("Project library", "Add reference", `
    <label>Title<input id="ref-title" type="text" placeholder="Reference title" autocomplete="off" /></label>
    <label>Authors<textarea id="ref-authors" placeholder="Comma- or newline-separated" autocomplete="off"></textarea></label>
    <label>Year<input id="ref-year" type="number" min="0" placeholder="2024" autocomplete="off" /></label>
    <label>DOI<input id="ref-doi" type="text" placeholder="10.xxxx/xxxxx" autocomplete="off" /></label>
    <label>Journal<input id="ref-journal" type="text" placeholder="Journal" autocomplete="off" /></label>
    <label>PMID<input id="ref-pmid" type="text" placeholder="PMID" autocomplete="off" /></label>
    <p class="prompt-error" id="prompt-error" hidden></p>
    <div class="modal-actions">
      <button class="btn" id="ref-cancel" type="button">Cancel</button>
      <button class="btn btn-primary" id="ref-add" type="button" disabled>Add</button>
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
    target.innerHTML = `<p class="empty-note">${state.references.length === 0 ? "No references yet. Add one to cite it from the text." : "Nothing matches that search."}</p>`
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
      // The PDF state is a fact about the entry (and the thing that blocks an
      // export), so it sits with the metadata; the actions share one row beneath.
      const attachment = fulltext
        ? `<span class="state-ok">PDF attached · ${escapeHtml(fulltext.filename)}</span>`
        : `<span class="state-warn">No PDF yet — an export that cites this will be blocked</span>`
      const upload = fulltext ? "" : `<button type="button" class="btn btn-small" data-upload="${escapeHtml(reference.id)}">Attach PDF</button>`
      return `<article class="list-item">
        <strong>${escapeHtml(reference.metadata.title)}</strong>
        <span class="meta">${authors}${reference.metadata.year === null ? "" : ` · ${reference.metadata.year}`}${journal} · ${escapeHtml(identifier)}</span>
        <span class="meta">${attachment}</span>
        <span class="row">
          <button type="button" data-cite="${escapeHtml(reference.id)}" class="btn btn-small">Insert citation</button>
          ${upload}
          <button type="button" class="btn btn-small btn-danger" data-delete="${escapeHtml(reference.id)}" title="Remove this reference">Delete</button>
        </span>
      </article>`
    })
    .join("")

  for (const button of target.querySelectorAll<HTMLButtonElement>("[data-cite]")) {
    button.addEventListener("click", () => {
      // `#cite(<id>)` is the label form the exporter's citation scanner reads.
      editor.dispatch({ changes: { from: editor.state.selection.main.head, insert: `#cite(<${button.dataset.cite}>)` } })
      editor.focus()
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
/** Role names as the interface says them. */
const ROLE_LABELS: Record<string, string> = {
  owner: "Owner",
  author: "Author",
  reviewer: "Reviewer",
  "read-only": "Read-only"
}

// ---------------------------------------------------------------------------
// Settings (editor typography; see web/src/settings.ts for the storage model)
// ---------------------------------------------------------------------------

let settings: Settings = loadSettings()

function openSettings(): void {
  const typefaces: readonly TypefaceId[] = ["mono", "serif", "sans"]
  const project = state.project
  const defaultFile = project ? loadDefaultFile(project.id) : undefined
  const defaultFileRow = project
    ? `<div class="settings-row">
        <label id="settings-default-file-label">Opening file</label>
        <select id="settings-default-file" aria-labelledby="settings-default-file-label">
          <option value="" ${defaultFile === undefined ? "selected" : ""}>Last file you had open</option>
          ${state.outline
            .map((entry) => `<option value="${escapeHtml(entry.document.path)}" ${entry.document.path === defaultFile ? "selected" : ""}>${escapeHtml(entry.document.path)}</option>`)
            .join("")}
          ${defaultFile !== undefined && !state.outline.some((e) => e.document.path === defaultFile)
            ? `<option value="${escapeHtml(defaultFile)}" selected disabled>${escapeHtml(defaultFile)} — not in this project</option>`
            : ""}
        </select>
      </div>
      <p class="settings-note">“Opening file” applies when this project is entered without a more recent file in this tab. It is this browser's choice, not the project's.</p>`
    : ""
  showPanel(
    "settings",
    `<div class="settings-body">
      ${defaultFileRow}
      <div class="settings-row">
        <label id="settings-typeface-label">Typeface</label>
        <div class="segmented" role="group" aria-labelledby="settings-typeface-label" id="settings-typeface">
          ${typefaces
            .map((id) => `<button class="seg" type="button" data-typeface="${id}" aria-pressed="${settings.typeface === id}">${TYPEFACE_LABELS[id]}</button>`)
            .join("")}
        </div>
      </div>
      <div class="settings-row">
        <label for="settings-font-size">Font size</label>
        <input id="settings-font-size" type="range" min="12" max="24" step="1" value="${settings.fontSize}" aria-label="Editor font size">
        <output id="settings-font-size-out" for="settings-font-size">${settings.fontSize}px</output>
      </div>
      <div class="settings-row">
        <label for="settings-line-height">Line spacing</label>
        <input id="settings-line-height" type="range" min="1.2" max="2.2" step="0.05" value="${settings.lineHeight}" aria-label="Editor line spacing">
        <output id="settings-line-height-out" for="settings-line-height">${settings.lineHeight.toFixed(2)}</output>
      </div>
      <p class="settings-note">Editor look only — your choices live in this browser and never affect collaborators or compiled output. <button type="button" class="btn" id="settings-reset">Reset to defaults</button></p>
    </div>`
  )
  const commit = (next: Settings): void => {
    settings = next
    saveSettings(settings)
    applySettings(settings)
  }
  for (const button of document.querySelectorAll<HTMLButtonElement>("#settings-typeface [data-typeface]")) {
    button.addEventListener("click", () => {
      commit(clampSettings({ ...settings, typeface: button.dataset.typeface as TypefaceId }))
      openSettings()
    })
  }
  const fontSize = el<HTMLInputElement>("#settings-font-size")
  fontSize?.addEventListener("input", () => {
    const size = clampSettings({ ...settings, fontSize: Number(fontSize.value) }).fontSize
    setText("#settings-font-size-out", `${size}px`)
    commit({ ...settings, fontSize: size })
  })
  const lineHeight = el<HTMLInputElement>("#settings-line-height")
  lineHeight?.addEventListener("input", () => {
    const height = clampSettings({ ...settings, lineHeight: Number(lineHeight.value) }).lineHeight
    setText("#settings-line-height-out", height.toFixed(2))
    commit({ ...settings, lineHeight: height })
  })
  el("#settings-reset")?.addEventListener("click", () => {
    commit(DEFAULT_SETTINGS)
    openSettings()
  })
  const defaultFileSelect = el<HTMLSelectElement>("#settings-default-file")
  defaultFileSelect?.addEventListener("change", () => {
    if (!project) return
    const path = defaultFileSelect.value === "" ? undefined : defaultFileSelect.value
    saveDefaultFile(project.id, path)
    status(path === undefined ? "Opening file cleared — this project opens where you left it" : `“${path}” opens when this project is entered`)
  })
}

function openShare(): void {
  const project = state.project
  if (!project) { showPanel("share", `<p class="empty-note">Open a project first.</p>`); return }
  showPanel(
    "share",
    `<p class="dock-note">Invite a collaborator by their username (e.g. <code>alice@nisaba.local</code>). They'll see this project after their next sign-in.</p>
     <div id="share-invite-form" class="inline-form">
       <input id="share-subject" type="text" placeholder="Username to invite" autocomplete="off" />
       <select id="share-role" aria-label="Role">
         <option value="author">Author — can edit the text</option>
         <option value="reviewer">Reviewer — can suggest and comment</option>
         <option value="read-only">Read-only — can read and preview</option>
       </select>
       <button id="share-invite" class="btn btn-primary" type="button">Invite</button>
     </div>
     <h3>Who has access</h3>
     <div id="share-members" class="list"><p class="dock-note">Loading…</p></div>
     <h3>Links</h3>
     <p class="dock-note">Generate a link that grants access to this project at a chosen role. Anyone who opens the link while signed in gets that role.</p>
     <div class="inline-form">
       <select id="share-link-role" aria-label="Link role">
         <option value="reviewer">Reviewer</option>
         <option value="author">Author</option>
         <option value="read-only">Read-only</option>
       </select>
       <button id="create-share-link" class="btn" type="button">Create link</button>
     </div>
     <div id="share-links" class="list"><p class="dock-note">No shareable links yet.</p></div>`
  )
  const canManageMembers = state.role === "owner" || state.role === "author"
  const renderMembers = (members: readonly api.Membership[]) => {
    renderPeopleStrip(members)
    const host = el<HTMLElement>("#share-members")
    if (!host) return
    host.innerHTML = members.length === 0
      ? `<p class="dock-note">No members yet.</p>`
      : members.map((m) => `<article class="list-item"><span class="row"><strong>${escapeHtml(m.subject)}</strong><span class="role-tag">${escapeHtml(ROLE_LABELS[m.role] ?? m.role)}</span>${canManageMembers && m.role !== "owner" ? `<button type="button" class="btn btn-small btn-danger" style="margin-left:auto" data-remove-member="${escapeHtml(m.subject)}">Remove</button>` : ""}</span></article>`).join("")
    for (const button of host.querySelectorAll<HTMLButtonElement>("[data-remove-member]")) {
      button.addEventListener("click", () => {
        const subject = button.dataset.removeMember ?? ""
        run(api.removeMember(project.id, subject), () => {
          status(`Removed ${subject} from the project`)
          run(api.listMembers(project.id), renderMembers)
        }, (error: unknown) => {
          status(error instanceof Error ? error.message : "Couldn't remove member")
        })
      })
    }
  }
  run(api.listMembers(project.id), renderMembers, () => { const h = el("#share-members"); if (h) h.innerHTML = `<p class="dock-note">Couldn't load members (you may not have permission).</p>` })
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
  let knownShareLinks: readonly api.ShareLink[] = []
  const renderShareLinks = (links: readonly api.ShareLink[]): void => {
    knownShareLinks = links
    const host = el<HTMLElement>("#share-links")
    if (!host) return
    host.innerHTML = links.length === 0
      ? `<p class="dock-note">No shareable links yet.</p>`
      : links.map((link) => {
          if (!link.token) {
            return `<div class="link-row"><span>${escapeHtml(ROLE_LABELS[link.role] ?? link.role)} link${link.label ? ` · ${escapeHtml(link.label)}` : ""}</span><span class="dock-note">Secret hidden</span><button type="button" class="btn" data-revoke="${escapeHtml(link.token_hash)}">Revoke</button></div>`
          }
          const url = `${window.location.origin}/?share=${encodeURIComponent(link.token)}`
          return `<div class="link-row"><code>${escapeHtml(url)}</code><button type="button" class="btn" data-copy="${escapeHtml(url)}">Copy</button><button type="button" class="btn" data-revoke="${escapeHtml(link.token)}">Revoke</button></div>`
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
      (created) => {
        status("Share link created — copy it now; the secret is shown once")
        if (createButton) { createButton.disabled = false; createButton.textContent = "Create link" }
        renderShareLinks([created, ...knownShareLinks])
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
  if (!project || !selected) { showPanel("history", `<p class="empty-note">Open a document first.</p>`); return }
  const documentId = selected.document.id
  showPanel("history", `<p class="dock-note">Loading earlier versions…</p>`)
  run(
    api.listDocumentHistory(project.id, selected.document.id),
    (revisions) => {
      // A history response belongs to the document that requested it. Switching
      // files while the request is in flight must not populate the dock with a
      // stale timeline under the newly-selected document.
      if (state.selected?.document.id !== documentId || dockTool !== "history") return
      const host = el<HTMLElement>("#dock-content")
      if (!host) return
      if (revisions.length === 0) {
        host.innerHTML = `<p class="empty-note">No saved revisions yet. Edits are snapshotted automatically on save.</p>`
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
        <p class="dock-note">Pick a version to read it. Pick a second one to see what changed between them.</p>
        <div class="history">
          <ul class="history-timeline" id="history-timeline">
            ${entries.map((entry) => {
              const at = Date.parse(entry.created_at)
              const when = entry.isCurrent ? "Working copy" : Number.isFinite(at) ? timeAgo(at) : `version ${entry.revision}`
              const detail = entry.isCurrent
                ? "your unsaved text"
                : `${escapeHtml(entry.author ?? "someone")} · ${Number.isFinite(at) ? new Date(at).toLocaleString() : `version ${entry.revision}`}`
              return `<li><button type="button" class="history-entry" data-rev="${escapeHtml(entry.id)}"><span class="history-entry-rev">${escapeHtml(when)}</span><span class="history-entry-meta">${detail}</span></button></li>`
            }).join("")}
          </ul>
          <div class="history-diff-pane" id="history-diff-pane"><div class="history-empty">Pick a version above.</div></div>
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
          pane.innerHTML = `<div class="history-empty">Pick a version above.</div>`
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
      if (state.selected?.document.id !== documentId || dockTool !== "history") return
      const host = el<HTMLElement>("#dock-content")
      if (host) host.innerHTML = `<p class="empty-note">Couldn't load history: ${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`
    }
  )
}

function openExport(): void {
  const project = state.project
  if (!project) { showPanel("export", `<p class="empty-note">Open a project first.</p>`); return }
  const entries = state.outline.map(({ document }) => `<option value="${escapeHtml(document.path)}">${escapeHtml(document.title)} — ${escapeHtml(document.path)}</option>`).join("")
  showPanel("export", `
    <label class="field">Which document<select id="export-entry">${entries}</select></label>
    <p class="dock-note">Exports the <b>${escapeHtml(VIEW_LABELS[state.view])}</b> version — the one the preview is showing — as a PDF, together with the reference files it cites.</p>
    <button id="run-export" class="btn btn-primary" type="button" ${entries ? "" : "disabled"}>Prepare download</button>
    <div id="export-result" class="dock-note"></div>`)
  el("#run-export")?.addEventListener("click", () => {
    const entry = el<HTMLSelectElement>("#export-entry")?.value
    if (!entry) return
    const exportButton = el<HTMLButtonElement>("#run-export")
    if (exportButton) exportButton.disabled = true
    setText("#export-result", "Saving current edits…")
    void saveBeforeServerSnapshot().then(() => {
      setText("#export-result", "Exporting…")
      run(api.exportProject(project.id, entry, state.view), (result) => {
      if (exportButton) exportButton.disabled = false
      const files = result.references.files
      const pdf = result.compile.pdf_base64
      const zip = result.zip_base64
      const zipName = result.zip_filename ?? `${project.name}.zip`
      const host = el<HTMLElement>("#export-result")
      if (!host) return
      host.innerHTML = `<p>Ready — ${files.length} reference file${files.length === 1 ? "" : "s"} included.</p>
        ${zip ? `<p><button id="download-zip" class="btn btn-primary" type="button">Download everything (.zip)</button> <code>${escapeHtml(zipName)}</code></p>` : ""}
        ${pdf ? `<button id="download-pdf" class="btn" type="button">Download PDF</button>` : `<p class="state-warn">The build produced no PDF — check the problems panel.</p>`}
        <ul class="export-files list">${files.map((file, index) => `<li><button type="button" class="btn btn-small" data-file="${index}">${escapeHtml(file.path)}</button></li>`).join("")}</ul>`
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
        if (host) host.innerHTML = `<p class="state-warn">Export failed: ${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`
      })
    }, (error: unknown) => {
      if (exportButton) exportButton.disabled = false
      const host = el<HTMLElement>("#export-result")
      if (host) host.innerHTML = `<p class="state-warn">Export cancelled because the current edits could not be saved: ${escapeHtml(error instanceof Error ? error.message : String(error))}</p>`
    })
  })
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

/**
 * Review state, stated in exactly two places.
 *
 * The old client announced the same "3 open items" in an amber banner, a toolbar
 * badge, AND the sidebar, plus a duplicate track-changes toggle in two of them —
 * redundant surfaces that drift apart and teach people to trust none of them.
 * Now: the count sits on the Review button (the door to the queue), and the queue
 * itself is the room. Track changes is one switch, in the document bar, which
 * states its own state.
 *
 * The function keeps its old name because it is called from a dozen places after
 * every review mutation.
 */
function renderReviewBanner(): void {
  const open = state.review.items.filter((item) => item.status === "open").length
  const reviewButton = el<HTMLButtonElement>("#review-button")
  const badge = el<HTMLElement>("#review-count")
  if (badge) {
    badge.textContent = String(open)
    badge.hidden = open === 0
  }
  if (reviewButton) {
    reviewButton.disabled = !state.selected
    reviewButton.title = open === 0
      ? "Comments and suggested changes"
      : `${open} open item${open === 1 ? "" : "s"}`
  }
  // Reviewers are locked into suggesting mode: every edit they make is recorded
  // as a proposal rather than silently changing the text. The switch shows the
  // state but refuses to move, and says why on hover.
  const suggesting = el<HTMLButtonElement>("#suggesting-button")
  if (suggesting) {
    const locked = state.role === "reviewer"
    suggesting.disabled = !state.selected || locked
    suggesting.setAttribute("aria-checked", String(state.review.suggesting))
    suggesting.textContent = `Track changes: ${state.review.suggesting ? "on" : "off"}`
    suggesting.title = locked
      ? "As a reviewer your edits are always recorded as suggestions"
      : state.review.suggesting
        ? "Your edits are recorded as suggestions for someone to accept"
        : "Record your edits as suggestions instead of changing the text"
  }
}

/** Flips track changes and re-renders every surface that shows it. */
function toggleSuggesting(): void {
  state.review = reviewReducer(state.review, { type: "toggle-suggesting" })
  renderReviewBanner()
  renderReviewDock()
}

/**
 * Apply project-role UI gates. Called when the membership fetch resolves
 * (openProject) and whenever role-dependent chrome may need re-rendering.
 *
 * - H1: a reviewer is forced into suggesting mode and cannot turn it off, so
 *   every body edit they make is recorded as a suggestion rather than a silent
 *   overwrite. The lock is enforced here (UI) and relies on the server already
 *   permitting reviewer document writes (needed for suggestion-mode edits).
 * - M4: read-only viewers cannot export (the server returns 403), so the
 *   Export button is hidden up-front rather than failing on click. Reviewers
 *   CAN export (the server grants them Permission::Document for review
 *   copies), so the button is shown to them.
 */
function applyRoleGates(): void {
  // Outside a project the membership role is unknown; gate the project list
  // (＋, row deletes) on the IdP roles claim from the token instead — the same
  // source the server authorizes. Inside a project, the membership role wins.
  const tokenRoles = decodedTokenPayload()?.roles
  const globalCanManage = Array.isArray(tokenRoles) && tokenRoles.includes("author")
  const canManage = state.role === undefined
    ? globalCanManage
    : state.role === "owner" || state.role === "author"
  const readOnly = documentAccessRevoked
    || !state.document
    || (state.role === "reviewer" && !reviewerSyncReady)
    || (state.role === undefined ? !globalCanManage && !(Array.isArray(tokenRoles) && tokenRoles.includes("reviewer")) : state.role === "read-only")
  const exportButton = el<HTMLElement>("#export-button")
  if (exportButton) exportButton.hidden = !(canManage || state.role === "reviewer")
  // Share/Invite is available to owners and authors (the roles the server allows
  // to manage members). Reviewers and read-only viewers can't add members, so
  // the button stays hidden for them (L3).
  const shareButton = el<HTMLElement>("#share-button")
  if (shareButton) shareButton.hidden = !canManage
  // The roster strip itself is for every member — visibility only depends on
  // a project being open (content is rendered by the listMembers fetch).
  const peopleStrip = el<HTMLElement>("#project-people")
  if (peopleStrip && !state.project) peopleStrip.hidden = true
  // History is read-only and useful for all members.
  const historyButton = el<HTMLElement>("#history-button")
  if (historyButton) historyButton.hidden = !state.selected

  // Read-only viewers: disable all write controls and make the editor read-only
  // so they cannot type into it and believe their edits are saved. The backend
  // also rejects these (403), but the UI should not expose the controls at all.
  if (editor) editor.dispatch({ effects: editableComp.reconfigure(EditorView.editable.of(!readOnly)) })

  // Create/delete/rename are owner/author actions server-side (reviewers are
  // blocked from baseline writes and deletions), so the controls are hidden for
  // reviewers too instead of surfacing a confusing 403 after the fact.
  // NOTE: the selectors here must match the real DOM ids/classes rendered by
  // renderProjects/renderFileTree (regression: they used to target non-existent
  // ids, leaving the destructive controls visible to every role — the e2e
  // permissions spec asserts the real selectors).
  const deleteProjectBtns = document.querySelectorAll<HTMLButtonElement>("[data-delete-project]")
  deleteProjectBtns.forEach((btn) => { btn.hidden = !canManage; btn.disabled = !canManage })
  const addDocBtn = el<HTMLButtonElement>("#add-document")
  if (addDocBtn) { addDocBtn.hidden = !canManage; addDocBtn.disabled = !canManage }
  const addDocEmptyBtn = el<HTMLButtonElement>("#add-document-empty")
  if (addDocEmptyBtn) { addDocEmptyBtn.hidden = !canManage; addDocEmptyBtn.disabled = !canManage }
  const addDemoBtn = el<HTMLButtonElement>("#add-demo")
  if (addDemoBtn) { addDemoBtn.hidden = !canManage; addDemoBtn.disabled = !canManage }
  const deleteDocBtns = document.querySelectorAll<HTMLButtonElement>("[data-delete-document]")
  deleteDocBtns.forEach((btn) => { btn.hidden = !canManage; btn.disabled = !canManage })
  // Project creation is an owner/author action (the server 403s reviewers and
  // read-only users); hide the ＋ button and the empty-state CTA for them.
  const newProjectBtn = el<HTMLButtonElement>("#new-project")
  if (newProjectBtn) { newProjectBtn.hidden = !canManage; newProjectBtn.disabled = !canManage }
  const emptyStateBtn = el<HTMLButtonElement>("#empty-create-project")
  if (emptyStateBtn) { emptyStateBtn.hidden = !canManage; emptyStateBtn.disabled = !canManage }
  // Updating the preview is a read action every role may take (see the roles
  // table in the user guide), so the primary action is never gated — only the
  // in-flight compile disables it.
  const compileBtn = el<HTMLButtonElement>("#compile-button")
  if (compileBtn && !isCompiling()) compileBtn.disabled = false
  const addRefBtn = el<HTMLButtonElement>("#add-reference")
  if (addRefBtn) { addRefBtn.hidden = !canManage; addRefBtn.disabled = !canManage }

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
  // The kind chip already shows the change word ("insert"/"delete"); repeating
  // it in the label glued the two together ("insertSuggestion · insert…").
  const kindLabel = item.kind === "comment" ? "Comment" : "Suggestion"
  const change = item.kind === "suggestion"
    ? `<span class="review-popover-kind review-popover-kind-${item.change}">${escapeHtml(item.change)}</span>`
    : `<span class="review-popover-kind review-popover-kind-comment">comment</span>`
  // Avatar chip carries the author's initials on a colour derived from their name
  // (authorHue), so distinct reviewers are visually separable at a glance; the name
  // and timeAgo sit beside it. Resolved items also show who resolved and when.
  const hue = authorHue(item.author)
  const avatar = `<span class="review-avatar" style="--hue:${hue}" aria-hidden="true">${escapeHtml(initialsOf(item.author))}</span>`
  const resolved = item.resolvedAt !== undefined && item.resolvedBy
    ? `<span class="review-resolved-line">Resolved by ${escapeHtml(item.resolvedBy)} · ${escapeHtml(timeAgo(item.resolvedAt))}</span>`
    : ""
  // Comments resolve; suggestions accept/reject. The buttons reuse the same actions as
  // the dialog so behaviour (authoritative text mutation for suggestions) is identical.
  const action = item.kind === "comment"
    ? `<button class="btn btn-primary" type="button" data-popover-resolve="${escapeHtml(item.id)}">Resolve</button>`
    : `<button class="btn btn-primary" type="button" data-popover-accept="${escapeHtml(item.id)}">Accept</button> <button class="btn" type="button" data-popover-reject="${escapeHtml(item.id)}">Reject</button>`
  popover.innerHTML = `
    <div class="review-popover-head">
      <div class="review-popover-meta">${change} <strong>${escapeHtml(kindLabel)}</strong><span class="review-popover-author">${escapeHtml(item.author)} · ${escapeHtml(timeAgo(item.createdAt))}</span></div>
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
  if (item.kind === "suggestion" && !state.review.items.some((candidate) =>
    candidate.kind === "suggestion" && candidate.status === "open")) {
    scheduleSave()
  }
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
// Review dock — the queue
// ---------------------------------------------------------------------------

/**
 * The review dock: every open comment and suggested change, as a triage queue.
 *
 * Two audiences, one surface. An author dips in to see what people proposed; a
 * reviewer works through forty items in a sitting. That second job is why the
 * queue is a keyboard listbox rather than a list of cards: ↑/↓ moves, Enter jumps
 * to the text, A accepts, R rejects, C comments, Esc goes back to writing. The
 * shortcuts are printed in the dock's footer and bind ONLY while focus is inside
 * the dock, so a single letter can never fire into the document.
 *
 * Rendering is idempotent and called after every review mutation; when the dock
 * is showing something else it is a no-op, so review state still mutates
 * correctly with the queue closed.
 */
type ReviewFilter = "all" | "suggestions" | "comments" | "mine"

let reviewFilter: ReviewFilter = "all"
/** Id of the queue row with roving focus, so re-renders keep the caret's place. */
let activeReviewId: string | undefined

function openReviewItems(): readonly ReviewItem[] {
  return state.review.items.filter((item) => item.status === "open")
}

function filteredReviewItems(): readonly ReviewItem[] {
  const me = currentUserDisplayName()
  return openReviewItems().filter((item) => {
    if (reviewFilter === "suggestions") return item.kind === "suggestion"
    if (reviewFilter === "comments") return item.kind === "comment"
    if (reviewFilter === "mine") return item.author === me
    return true
  })
}

function renderReviewDock(): void {
  if (dockTool !== "review") return
  const host = el<HTMLElement>("#dock-content")
  if (!host) return
  const open = openReviewItems()
  const items = filteredReviewItems()
  const suggestions = open.filter((item) => item.kind === "suggestion").length
  const comments = open.length - suggestions

  host.replaceChildren()

  const chips = document.createElement("div")
  chips.className = "chips"
  const chipSpec: readonly { key: ReviewFilter; label: string }[] = [
    { key: "all", label: `All ${open.length}` },
    { key: "suggestions", label: `Changes ${suggestions}` },
    { key: "comments", label: `Comments ${comments}` },
    { key: "mine", label: "Mine" }
  ]
  for (const spec of chipSpec) {
    const chip = document.createElement("button")
    chip.type = "button"
    chip.className = "chip"
    chip.textContent = spec.label
    chip.setAttribute("aria-pressed", String(reviewFilter === spec.key))
    chip.addEventListener("click", () => { reviewFilter = spec.key; renderReviewDock() })
    chips.append(chip)
  }
  host.append(chips)

  const addComment = document.createElement("button")
  addComment.type = "button"
  addComment.className = "btn btn-small"
  addComment.id = "sidebar-comment"
  addComment.textContent = "Add a comment here"
  addComment.title = "Comment on the selected text, or at the cursor"
  addComment.addEventListener("click", () => addCommentAtSelection())
  const toolbar = document.createElement("div")
  toolbar.className = "dock-toolbar"
  toolbar.append(addComment)
  host.append(toolbar)

  if (items.length === 0) {
    const empty = document.createElement("p")
    empty.className = "empty-note"
    empty.textContent = open.length === 0
      ? "Nothing to review yet. Turn on track changes to record your edits as suggestions, or select some text and add a comment."
      : "Nothing matches this filter."
    host.append(empty)
  } else {
    const queue = document.createElement("ul")
    queue.className = "review-queue"
    queue.id = "review-queue"
    queue.setAttribute("role", "listbox")
    queue.setAttribute("aria-label", "Open review items")
    if (activeReviewId === undefined || !items.some((item) => item.id === activeReviewId)) {
      activeReviewId = items[0]?.id
    }
    for (const item of items) queue.append(reviewRow(item, item.id === activeReviewId))
    queue.addEventListener("keydown", (event) => handleQueueKey(event, items))
    host.append(queue)
  }

  renderReviewFoot(open)
}

/** The dock footer: bulk actions on the left, the shortcut legend on the right. */
function renderReviewFoot(open: readonly ReviewItem[]): void {
  const foot = el<HTMLElement>("#dock-foot")
  if (!foot) return
  foot.replaceChildren()
  const openSuggestions = open.filter((item): item is Extract<ReviewItem, { kind: "suggestion" }> => item.kind === "suggestion")
  if (openSuggestions.length > 0) {
    const acceptAll = document.createElement("button")
    acceptAll.type = "button"
    acceptAll.className = "btn btn-small"
    acceptAll.id = "sidebar-accept-all"
    acceptAll.textContent = `Accept all ${openSuggestions.length}`
    acceptAll.addEventListener("click", () => acceptAllSuggestions(openSuggestions))
    const rejectAll = document.createElement("button")
    rejectAll.type = "button"
    rejectAll.className = "btn btn-small"
    rejectAll.id = "sidebar-reject-all"
    rejectAll.textContent = "Reject all"
    rejectAll.addEventListener("click", () => rejectAllSuggestions(openSuggestions))
    foot.append(acceptAll, rejectAll)
  }
  const keys = document.createElement("span")
  keys.className = "keys"
  keys.textContent = "↑↓ move · ↵ show · A accept · R reject · C comment"
  foot.append(keys)
  foot.hidden = false
}

/**
 * One queue row: kind, who, when, where, and the text — plus the two actions that
 * resolve it. `role="option"` inside the listbox, with roving tabindex so Tab
 * enters the queue once and the arrows do the rest.
 */
function reviewRow(item: ReviewItem, selected: boolean): HTMLElement {
  const row = document.createElement("li")
  row.className = item.kind === "comment" ? "review-card review-card-comment" : "review-card review-card-suggestion"
  row.dataset.reviewId = item.id
  row.setAttribute("role", "option")
  row.setAttribute("aria-selected", String(selected))
  row.tabIndex = selected ? 0 : -1

  const head = document.createElement("div")
  head.className = "review-card-head"
  const chip = document.createElement("span")
  const kind = item.kind === "comment" ? "comment" : item.change
  chip.className = `review-chip review-chip-${kind}`
  chip.textContent = kind === "insert" ? "Added" : kind === "delete" ? "Deleted" : "Comment"
  const author = document.createElement("span")
  author.className = "review-card-author"
  author.textContent = item.author
  const time = document.createElement("span")
  time.className = "review-card-time"
  time.textContent = timeAgo(item.createdAt)
  author.append(" ", time)
  head.append(chip, author)
  if (item.orphaned) {
    const orphan = document.createElement("span")
    orphan.className = "review-card-orphan"
    orphan.textContent = "needs re-anchoring"
    orphan.title = "The text this was attached to has changed too much to locate"
    head.append(orphan)
  }
  const avatar = document.createElement("span")
  avatar.className = "avatar"
  avatar.style.setProperty("--hue", String(authorHue(item.author)))
  avatar.setAttribute("aria-hidden", "true")
  avatar.textContent = initialsOf(item.author)
  head.append(avatar)
  row.append(head)

  const text = document.createElement("p")
  text.className = "review-card-text"
  if (item.kind === "comment") {
    text.textContent = item.body
  } else {
    text.classList.add("review-card-quote")
    const snippet = item.text ?? ""
    text.textContent = `“${snippet.length > 200 ? `${snippet.slice(0, 200)}…` : snippet}”`
  }
  row.append(text)

  const location = document.createElement("span")
  location.className = "review-card-loc"
  location.textContent = reviewLocation(item)
  row.append(location)

  if (item.kind === "suggestion" && item.change === "delete") {
    const note = document.createElement("p")
    note.className = "review-card-note"
    note.textContent = "Already removed from the text — Reject puts it back."
    row.append(note)
  }

  const actions = document.createElement("div")
  actions.className = "review-card-actions"
  if (item.kind === "comment") {
    actions.append(actionButton("Resolve", "btn btn-primary btn-small", () => applyReviewItemAction(item, "resolve")))
  } else {
    actions.append(
      actionButton("Accept", "btn btn-primary btn-small", () => applyReviewItemAction(item, "accept")),
      actionButton("Reject", "btn btn-small", () => applyReviewItemAction(item, "reject"))
    )
  }
  row.append(actions)

  row.addEventListener("click", (event) => {
    if ((event.target as HTMLElement).closest("button")) return
    selectReviewItem(item.id)
    revealReviewItem(item)
  })
  row.addEventListener("focus", () => selectReviewItem(item.id, false))
  return row
}

function actionButton(label: string, className: string, onClick: () => void): HTMLButtonElement {
  const button = document.createElement("button")
  button.type = "button"
  button.className = className
  button.textContent = label
  button.addEventListener("click", (event) => { event.stopPropagation(); onClick() })
  return button
}

/** `main.typ · line 12` — where the item is, in the writer's terms. */
function reviewLocation(item: ReviewItem): string {
  const path = state.selected?.document.path ?? ""
  const line = editor.state.doc.lineAt(Math.min(item.from, editor.state.doc.length)).number
  return path === "" ? `line ${line}` : `${path} · line ${line}`
}

/** Moves the queue's roving selection, optionally moving DOM focus with it. */
function selectReviewItem(id: string, focus = true): void {
  activeReviewId = id
  const queue = el<HTMLElement>("#review-queue")
  if (!queue) return
  for (const row of queue.querySelectorAll<HTMLElement>(".review-card")) {
    const isActive = row.dataset.reviewId === id
    row.setAttribute("aria-selected", String(isActive))
    row.tabIndex = isActive ? 0 : -1
    if (isActive && focus) {
      row.focus()
      row.scrollIntoView({ block: "nearest" })
    }
  }
}

/**
 * Queue keyboard triage. Bound to the listbox, never to the document, so the
 * single-letter shortcuts cannot reach the editor.
 */
function handleQueueKey(event: KeyboardEvent, items: readonly ReviewItem[]): void {
  if (event.metaKey || event.ctrlKey || event.altKey) return
  const index = items.findIndex((item) => item.id === activeReviewId)
  const item = items[index]
  const move = (delta: number): void => {
    if (items.length === 0) return
    const next = items[(index + delta + items.length) % items.length]
    if (next) selectReviewItem(next.id)
  }
  switch (event.key) {
    case "ArrowDown": event.preventDefault(); move(1); break
    case "ArrowUp": event.preventDefault(); move(-1); break
    case "Home": event.preventDefault(); if (items[0]) selectReviewItem(items[0].id); break
    case "End": event.preventDefault(); { const last = items.at(-1); if (last) selectReviewItem(last.id) } break
    case "Enter": event.preventDefault(); if (item) revealReviewItem(item); break
    case "a": case "A":
      event.preventDefault()
      if (item?.kind === "suggestion") applyReviewItemAction(item, "accept")
      break
    case "r": case "R":
      event.preventDefault()
      if (item?.kind === "suggestion") applyReviewItemAction(item, "reject")
      break
    case "c": case "C":
      event.preventDefault()
      if (item?.kind === "comment") applyReviewItemAction(item, "resolve")
      else addCommentAtSelection()
      break
    case "Escape": event.preventDefault(); editor.focus(); break
    default: break
  }
}

/** Adds a comment on the current selection (or at the cursor). */
function addCommentAtSelection(): void {
  const selection = editor.state.selection.main
  const hasSelection = selection.from !== selection.to
  promptInPanel(
    "Review",
    hasSelection ? "Comment on the selected text" : "Comment here",
    "Your comment",
    (commentBody) => {
      const fromCursor = createCursorAt(activeLoro, selection.from)
      const toCursor = selection.to > selection.from ? createCursorAt(activeLoro, selection.to) : fromCursor
      state.review = reviewReducer(state.review, {
        type: "add",
        item: {
          id: crypto.randomUUID(),
          kind: "comment",
          from: selection.from,
          to: selection.to,
          fromCursor,
          toCursor,
          body: commentBody,
          author: currentUserDisplayName(),
          status: "open",
          createdAt: Date.now()
        }
      })
      editor.dispatch({ effects: setReviewItems.of(state.review.items) })
      renderReviewBanner()
      renderReviewDock()
      persistReview()
      if (dockTool !== "review") openReviewDock()
    },
    { placeholder: "What should change, and why?" }
  )
}

function acceptAllSuggestions(openSuggestions: readonly Extract<ReviewItem, { kind: "suggestion" }>[]): void {
  state.review = reviewReducer(state.review, { type: "bulk-accept", ids: openSuggestions.map((item) => item.id), by: currentUserDisplayName(), at: Date.now() })
  editor.dispatch({ effects: setReviewItems.of(state.review.items) })
  renderReviewBanner()
  renderReviewDock()
  persistReview()
  scheduleSave()
}

function rejectAllSuggestions(openSuggestions: readonly Extract<ReviewItem, { kind: "suggestion" }>[]): void {
  // One authoritative transaction for all rejects: without it the removal of each
  // insert would be re-tracked as a new delete suggestion, and "reject all" would
  // churn forever.
  const changes: { from: number; to?: number; insert?: string }[] = []
  for (const item of openSuggestions) {
    if (item.change === "delete" && item.text) {
      // Clamp the restore position to the current doc length so a stale offset
      // cannot throw a RangeError.
      changes.push({ from: Math.min(item.from, editor.state.doc.length), insert: item.text })
    } else if (item.change === "insert") {
      // Only remove the suggested text if it still matches what is in the doc; a
      // since-edited range is skipped rather than corrupting surrounding text.
      const from = item.from
      const to = Math.min(item.to, editor.state.doc.length)
      if (editor.state.doc.sliceString(from, to) === item.text) changes.push({ from, to, insert: "" })
    }
  }
  // CodeMirror requires the change array in a single transaction to be ordered
  // and non-overlapping, so each change maps to the right original offset.
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
  renderReviewDock()
  persistReview()
  if (!state.review.items.some((item) => item.kind === "suggestion" && item.status === "open")) scheduleSave()
}

/** Kept under its old name for the many call sites that fire after a mutation. */
function renderReviewSidebar(): void {
  renderReviewDock()
}

/**
 * Floating "Comment" affordance at the end of a text selection — the Google Docs
 * gesture, and the one people reach for before they find any panel. It appears
 * whenever text is selected in a document the user may comment on.
 */
let selectionCommentButton: HTMLButtonElement | undefined
function updateSelectionCommentButton(view: EditorView): void {
  const selection = view.state.selection.main
  const hasSelection = selection.from !== selection.to
  if (!hasSelection || !state.document || state.role === "read-only") {
    selectionCommentButton?.remove()
    selectionCommentButton = undefined
    return
  }
  const coords = view.coordsAtPos(selection.to)
  if (!coords) {
    selectionCommentButton?.remove()
    selectionCommentButton = undefined
    return
  }
  if (!selectionCommentButton) {
    selectionCommentButton = document.createElement("button")
    selectionCommentButton.className = "selection-comment-button"
    selectionCommentButton.type = "button"
    selectionCommentButton.textContent = "Comment"
    selectionCommentButton.addEventListener("click", () => {
      addCommentAtSelection()
      selectionCommentButton?.remove()
      selectionCommentButton = undefined
    })
  }
  selectionCommentButton.style.left = `${coords.right + 6}px`
  selectionCommentButton.style.top = `${coords.top - 6}px`
  if (!selectionCommentButton.isConnected) document.body.append(selectionCommentButton)
}

/** Opens the review queue in the dock. */
function openReviewDock(): void {
  dockTool = "review"
  makeRoomForDock()
  setText("#dock-title", "Review")
  applyWorkspaceGrid()
  syncDockButtons()
  renderReviewDock()
  // Move focus into the queue so keyboard triage starts immediately.
  requestAnimationFrame(() => el<HTMLElement>("#review-queue")?.querySelector<HTMLElement>('.review-card[aria-selected="true"]')?.focus())
}

function toggleReviewSidebar(): void {
  toggleDock("review", openReviewDock)
}

// ---------------------------------------------------------------------------
// Compile/preview wiring
//
// The compile/preview subsystem itself — both compile paths, the shared
// suggestion-marks/request/settle/PDF plumbing they run on, the build label and
// build-health cells, the preview pane's empty/failure/clean states, and the
// compiling/pendingCompile/lastBuild lifecycle state — lives in ./compile.ts.
// It owns none of the handles it drives, so everything it borrows is handed in
// here, once, before the first render: the workspace state (it only reads),
// the editor, the PDF viewer, the active Loro replica (a getter — it is swapped
// per document), and the renderers its results feed.
// ---------------------------------------------------------------------------

initCompile({
  state,
  editor,
  pdfViewer,
  getActiveLoro: () => activeLoro,
  el,
  setText,
  status,
  run,
  timeAgo,
  renderDiagnostics,
  logBuild,
  setDrawerOpen,
  updateZoomLabel,
  renderPagePosition
})

// ---------------------------------------------------------------------------
// The view switch: which version of the document the preview renders
//
// These are projections computed server-side (crates/nisaba-core): baseline
// rejects every pending change, proposed accepts every one, redline renders the
// text with change markers, and public is proposed minus redacted spans. The
// labels (VIEW_LABELS, imported from ./compile) are the writer's words for
// exactly those things (docs/ui-design.md §1).
// ---------------------------------------------------------------------------

const VIEW_HINTS: Record<CompileView, string> = {
  proposed: "Every suggested change applied — what the document becomes",
  baseline: "No suggested changes applied — the last agreed text",
  redline: "The text with insertions and deletions marked",
  public: "Final, with redacted passages removed"
}

const VIEW_ORDER: readonly CompileView[] = ["proposed", "baseline", "redline", "public"]

function renderViewSwitch(): void {
  const host = el<HTMLElement>("#view-switch")
  if (!host) return
  host.replaceChildren(...VIEW_ORDER.map((view) => {
    const button = document.createElement("button")
    button.type = "button"
    button.dataset.view = view
    button.textContent = VIEW_LABELS[view]
    button.title = VIEW_HINTS[view]
    button.setAttribute("aria-pressed", String(state.view === view))
    button.addEventListener("click", () => {
      if (state.view === view) return
      state.view = view
      renderViewSwitch()
      // Asking for a view means asking to see it: recompile now. A compile already
      // in flight queues this one via pendingCompile, so the choice is never lost.
      if (state.document) compileCurrent()
    })
    return button
  }))
}

// ---------------------------------------------------------------------------
// Build drawer: problems and log
//
// Errors used to be a list wedged above the PDF, stealing preview height on every
// failure and putting source-line errors as far from the source as the layout
// allows. They now live in a drawer that opens itself when a build fails, with
// the chronological build log beside them.
// ---------------------------------------------------------------------------

type DrawerTab = "problems" | "log"

let drawerTab: DrawerTab = "problems"
let drawerOpen = false

interface BuildLogEntry {
  readonly at: number
  readonly level: BuildLogLevel
  readonly text: string
}

const buildLog: BuildLogEntry[] = []
const MAX_LOG_ENTRIES = 60

function logBuild(level: BuildLogEntry["level"], text: string): void {
  buildLog.unshift({ at: Date.now(), level, text })
  if (buildLog.length > MAX_LOG_ENTRIES) buildLog.length = MAX_LOG_ENTRIES
  renderBuildLog()
}

function renderBuildLog(): void {
  const host = el<HTMLElement>("#build-log")
  if (!host) return
  if (buildLog.length === 0) {
    host.innerHTML = `<p class="log-empty">Nothing built yet in this session.</p>`
    return
  }
  host.replaceChildren(...buildLog.map((entry) => {
    const line = document.createElement("div")
    line.className = "log-line"
    const time = document.createElement("span")
    time.className = "t"
    time.textContent = new Date(entry.at).toLocaleTimeString()
    const level = document.createElement("span")
    level.className = `k k-${entry.level}`
    level.textContent = entry.level
    const text = document.createElement("span")
    text.textContent = entry.text
    line.append(time, level, text)
    return line
  }))
}

function setDrawerOpen(open: boolean, tab?: DrawerTab): void {
  drawerOpen = open
  if (tab) drawerTab = tab
  const drawer = el<HTMLElement>("#build-drawer")
  if (drawer) drawer.hidden = !open
  const problems = el<HTMLElement>("#diagnostics-list")
  const log = el<HTMLElement>("#build-log")
  if (problems) problems.hidden = drawerTab !== "problems"
  if (log) log.hidden = drawerTab !== "log"
  el<HTMLElement>("#drawer-tab-problems")?.setAttribute("aria-selected", String(drawerTab === "problems"))
  el<HTMLElement>("#drawer-tab-log")?.setAttribute("aria-selected", String(drawerTab === "log"))
}

/**
 * Renders the problems list, the counts on the tab and the status bar, and the
 * editor's underline decorations. Each problem is clickable: it selects the
 * offending span in the source, which is the only thing anyone wants to do with
 * a compile error.
 */
function renderDiagnostics(diagnostics: readonly CompileDiagnostic[]): void {
  state.diagnostics = diagnostics
  editor.dispatch({ effects: setDiagnostics.of(diagnostics) })
  const host = el<HTMLElement>("#diagnostics-list")
  const errors = diagnostics.filter((item) => item.severity !== "warning").length
  const warnings = diagnostics.length - errors
  const countNode = el<HTMLElement>("#problem-count")
  if (countNode) {
    countNode.textContent = String(diagnostics.length)
    if (errors > 0) countNode.dataset.tone = "error"
    else if (warnings > 0) countNode.dataset.tone = "warn"
    else delete countNode.dataset.tone
  }
  if (!host) return
  if (diagnostics.length === 0) {
    host.innerHTML = `<p class="log-empty">No problems.</p>`
    // A clean build has nothing to say: close the drawer unless the writer pinned
    // it open on the log tab.
    if (drawerOpen && drawerTab === "problems") setDrawerOpen(false)
    return
  }
  const entry = state.selected ? state.selected.document.path : ""
  host.replaceChildren(...diagnostics.map((item, index) => {
    const severity = item.severity === "warning" ? "warning" : "error"
    const row = document.createElement("button")
    row.type = "button"
    row.className = "problem"
    row.dataset.diag = String(index)
    const badge = document.createElement("span")
    badge.className = `sev sev-${severity}`
    badge.textContent = severity
    const body = document.createElement("div")
    const message = document.createElement("div")
    message.textContent = item.message
    body.append(message)
    const location = locationLabel(item, entry)
    if (location !== "") {
      const loc = document.createElement("div")
      loc.className = "loc"
      loc.textContent = location
      body.append(loc)
    }
    row.append(badge, body)
    row.addEventListener("click", () => {
      const length = editor.state.doc.length
      const from = Math.min(item.start ?? 0, length)
      const to = Math.min(item.end ?? from, length)
      // A zero-width selection still scrolls the spot into view; a real range also
      // highlights the underlined span.
      editor.dispatch({ selection: { anchor: from, head: Math.max(to, from) }, scrollIntoView: true })
      editor.focus()
    })
    return row
  }))
}

/** `main.typ · line 12` — the writer's coordinates, not a character offset. */
function locationLabel(item: CompileDiagnostic, entry: string): string {
  const file = item.path ?? entry
  if (item.start === null || item.start === undefined) return file
  const line = editor.state.doc.lineAt(Math.min(item.start, editor.state.doc.length)).number
  return file === "" ? `line ${line}` : `${file} · line ${line}`
}

/**
 * "Page 3 of 12" on the preview bar, derived from the scroll position: the page
 * whose top edge is nearest the top of the viewport is the one being read.
 */
function renderPagePosition(): void {
  const viewer = el<HTMLElement>("#pdf-viewer")
  const label = el<HTMLElement>("#page-position")
  if (!viewer || !label) return
  const total = pdfViewer.pageCount
  if (total === 0) { label.textContent = ""; return }
  const pages = viewer.querySelectorAll<HTMLElement>(".pdf-page")
  let current = 1
  let best = Number.POSITIVE_INFINITY
  const top = viewer.getBoundingClientRect().top
  for (const page of pages) {
    const distance = Math.abs(page.getBoundingClientRect().top - top)
    if (distance < best) {
      best = distance
      current = Number(page.dataset.page) || current
    }
  }
  label.textContent = `page ${current} of ${total}`
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
  const callbackUrl = window.location.href
  const callback = isOidcCallback(callbackUrl)
  const current = new URL(callbackUrl)
  const hasOidcResponse = current.searchParams.has("code") || current.searchParams.has("error")
  if (hasOidcResponse) {
    for (const key of ["code", "state", "session_state", "iss", "error", "error_description"]) {
      current.searchParams.delete(key)
    }
    window.history.replaceState({}, "", `${current.pathname}${current.search}${current.hash}`)
  }
  // A spent callback can be reached through Back after the pending PKCE state
  // has been consumed. It still must be scrubbed, but must not be exchanged.
  if (!callback) return Promise.resolve()
  // Capture the response for validation, then remove its authorization code and
  // state synchronously. The token exchange is asynchronous; leaving secrets in
  // the address bar until it finishes lets the browser snapshot that entry.
  return Effect.runPromise(Effect.provide(OidcClient.use((client) => client.completeCallback(callbackUrl)), authLayer)).then(
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

// Project-scope actions. Each toggles its dock: clicking the open one closes it,
// which is what a person expects from a button that is visibly "on".
el("#new-project")?.addEventListener("click", createProject)
el("#add-document")?.addEventListener("click", addDocument)
el("#add-demo")?.addEventListener("click", addDemoFile)
el("#references-button")?.addEventListener("click", () => toggleDock("references", openReferences))
el("#history-button")?.addEventListener("click", () => toggleDock("history", openHistory))
el("#share-button")?.addEventListener("click", () => toggleDock("share", openShare))
el("#settings-button")?.addEventListener("click", () => toggleDock("settings", openSettings))

// Projects screen: search-as-you-type and the Recent/Name sort toggle. The
// sort choice persists; both re-render the list through the pure shaper.
const projectSearchInput = el<HTMLInputElement>("#project-search")
projectSearchInput?.addEventListener("input", () => {
  projectSearch = projectSearchInput.value
  renderProjects()
})
el("#sort-recent")?.addEventListener("click", () => {
  projectSort = "recent"
  persistProjectSort()
  syncProjectToolState()
  renderProjects()
})
el("#sort-name")?.addEventListener("click", () => {
  projectSort = "name"
  persistProjectSort()
  syncProjectToolState()
  renderProjects()
})
syncProjectToolState()
// Editor typography from saved settings before the first paint of text.
applySettings(settings)
el("#export-button")?.addEventListener("click", () => toggleDock("export", openExport))
el("#review-button")?.addEventListener("click", toggleReviewSidebar)
el("#dock-close")?.addEventListener("click", closeDock)
el("#go-projects")?.addEventListener("click", () => { if (state.project) leaveProject() })
el("#compile-button")?.addEventListener("click", compileCurrent)
el("#suggesting-button")?.addEventListener("click", toggleSuggesting)
el("#open-palette")?.addEventListener("click", () => palette.open())

// Build drawer: the tabs, the close button, and the status-bar cell that opens it
// at whichever tab explains the current state.
for (const tab of document.querySelectorAll<HTMLElement>("[data-drawer-tab]")) {
  tab.addEventListener("click", () => setDrawerOpen(true, tab.dataset.drawerTab === "log" ? "log" : "problems"))
}
el("#drawer-close")?.addEventListener("click", () => setDrawerOpen(false))
el("#build-health")?.addEventListener("click", () => {
  const hasProblems = state.diagnostics.length > 0
  if (drawerOpen && ((hasProblems && drawerTab === "problems") || (!hasProblems && drawerTab === "log"))) setDrawerOpen(false)
  else setDrawerOpen(true, hasProblems ? "problems" : "log")
})

// The preview bar's page readout follows the scroll position.
el("#pdf-viewer")?.addEventListener("scroll", () => renderPagePosition(), { passive: true })

// ---------------------------------------------------------------------------
// Command palette
//
// Everything the interface can do is reachable here, which is what lets the
// permanent chrome stay small. Candidates are rebuilt on every keystroke from
// live state, so a file added a second ago is already findable.
// ---------------------------------------------------------------------------

const palette = createPalette((): readonly PaletteItem[] => {
  const items: PaletteItem[] = []
  for (const entry of state.outline) {
    items.push({
      id: `file:${entry.document.id}`,
      group: "Files",
      kind: "file",
      label: entry.document.title,
      hint: entry.document.path,
      search: entry.document.path,
      run: () => openDocument(entry)
    })
  }
  for (const heading of currentHeadings) {
    items.push({
      id: `section:${heading.from}`,
      group: "Sections",
      kind: "§",
      label: heading.title,
      hint: `line ${editor.state.doc.lineAt(Math.min(heading.from, editor.state.doc.length)).number}`,
      run: () => revealPosition(heading.from)
    })
  }
  for (const reference of state.references) {
    const authors = reference.metadata.authors.join(", ")
    items.push({
      id: `ref:${reference.id}`,
      group: "References",
      kind: "cite",
      label: reference.metadata.title,
      hint: [authors, reference.metadata.year].filter(Boolean).join(" · "),
      search: `${authors} ${reference.metadata.doi ?? ""}`,
      run: () => insertCitation(reference.id)
    })
  }
  const command = (id: string, label: string, hint: string, runCommand: () => void): PaletteItem =>
    ({ id: `cmd:${id}`, group: "Commands", kind: "run", label, hint, run: runCommand })
  items.push(
    command("compile", "Update preview", "⌘↵", compileCurrent),
    command("review", "Review: comments and suggested changes", "", toggleReviewSidebar),
    command("track", `Track changes: turn ${state.review.suggesting ? "off" : "on"}`, "", toggleSuggesting),
    command("comment", "Add a comment here", "", () => addCommentAtSelection()),
    command("focus", focusMode ? "Leave focus mode" : "Focus mode: hide everything but the text", "⌘⇧F", toggleFocusMode),
    command("sidebar", hiddenPanes.navigator ? "Show the sidebar" : "Hide the sidebar", "⌘B", () => togglePane("navigator")),
    command("preview", hiddenPanes.preview ? "Show the preview" : "Hide the preview", "", () => togglePane("preview")),
    command("problems", "Problems and build log", "", () => setDrawerOpen(!drawerOpen, state.diagnostics.length > 0 ? "problems" : "log")),
    command("references", "References library", "", () => toggleDock("references", openReferences)),
    command("history", "Earlier versions of this file", "", () => toggleDock("history", openHistory)),
    command("share", "Share with people", "", () => toggleDock("share", openShare)),
    command("settings", "Settings: typeface, font size, line spacing", "", () => toggleDock("settings", openSettings)),
    command("export", "Export and download", "", () => toggleDock("export", openExport)),
    command("newfile", "New file", "", addDocument),
    command("projects", "All projects", "", leaveProject)
  )
  for (const view of VIEW_ORDER) {
    items.push(command(`view:${view}`, `Preview: ${VIEW_LABELS[view]}`, VIEW_HINTS[view], () => {
      state.view = view
      renderViewSwitch()
      renderBuildLabel()
      if (state.document) compileCurrent()
    }))
  }
  return items
})

/** Inserts a citation for a reference at the caret — the palette's cite action. */
function insertCitation(referenceId: string): void {
  const at = editor.state.selection.main.head
  editor.dispatch({ changes: { from: at, insert: `#cite(<${referenceId}>)` }, selection: { anchor: at + referenceId.length + 9 } })
  editor.focus()
}

// True while the pointer is over the preview pane: the zoom chords are
// browser defaults, so they are intercepted only where the preview is the
// active context — everywhere else ⌘+/⌘− zoom the page as the browser intends.
let pointerOverPreview = false
el<HTMLElement>(".preview-pane")?.addEventListener("pointerenter", () => { pointerOverPreview = true })
el<HTMLElement>(".preview-pane")?.addEventListener("pointerleave", () => { pointerOverPreview = false })

document.addEventListener("keydown", (event) => {
  const modifier = event.metaKey || event.ctrlKey
  // Policy: never intercept browser-essential chords — the reload family
  // (⌘R, ⌘⇧R, F5), devtools, history. ⌘⇧R used to open the Review dock and
  // silently swallowed hard reload; the Review dock is a button and a palette
  // command instead. ⌘S/⌘K/⌘B/⌘⇧F are editor-standard overrides (the
  // browser action under them — save page — is meaningless here); zoom is
  // scoped to the preview pane below.
  // ⌘K reaches everything, including from inside a dialog-free overlay, so it is
  // handled before the modal guard below.
  if (modifier && event.key.toLowerCase() === "k") { event.preventDefault(); palette.open(); return }
  // Skip the remaining global shortcuts while a <dialog> is modal: keyboard events
  // still bubble to document inside showModal()'s top layer, so ⌘↵ would compile
  // (or ⌘S save) while the user is typing into a prompt.
  if (el<HTMLDialogElement>("#workspace-panel")?.open) return
  if (modifier && event.key === "Enter") { event.preventDefault(); compileCurrent() }
  // ⌘S saves and then updates the preview: a writer pressing save expects both.
  if (modifier && !event.shiftKey && event.key.toLowerCase() === "s") { event.preventDefault(); saveNow(); compileCurrent() }
  if (modifier && event.shiftKey && event.key.toLowerCase() === "f") { event.preventDefault(); toggleFocusMode() }
  if (modifier && !event.shiftKey && event.key.toLowerCase() === "b") { event.preventDefault(); togglePane("navigator") }
  // ⌘= / ⌘− zoom the preview — but only while the pointer is over the preview
  // pane, so the same chords still zoom the page everywhere else (they are
  // browser zoom defaults; a global intercept stole them).
  if (pointerOverPreview && modifier && (event.key === "=" || event.key === "+")) { event.preventDefault(); pdfViewer.zoomIn(); updateZoomLabel() }
  if (pointerOverPreview && modifier && event.key === "-") { event.preventDefault(); pdfViewer.zoomOut(); updateZoomLabel() }
  // Esc backs out of the current surface, innermost first. The review popover
  // handles its own Esc (capture phase), so by the time we get here it is closed.
  if (event.key === "Escape" && !modifier) {
    if (focusMode) { toggleFocusMode(); return }
    if (dockTool !== undefined && document.activeElement?.closest("#dock")) { closeDock(); editor.focus() }
  }
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
  // HIGH #4: Do NOT destroy the editor or close the sync connection here.
  // beforeunload can fire and then the user clicks "Stay on page", which
  // would leave the editor permanently destroyed with no recovery path. The
  // destructive teardown now runs on "pagehide", which only fires on a real unload.
})
// HIGH #4: Real teardown belongs here. "pagehide" fires only when the document is
// genuinely being unloaded (navigation/close), unlike "beforeunload" which the
// user can cancel — so destroying the editor here can never strand a staying user.
window.addEventListener("pagehide", (event) => {
  // bfcache freeze: the browser snapshots the page for back-forward cache
  // and restores it without reloading when the user navigates back. Destroying
  // the editor or closing sync here would leave a
  // visually-intact but completely dead page. Skip teardown on persisted freeze;
  // the handlers run only on a genuine unload.
  if (event.persisted) return
  syncConnection?.close()
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
  // A failed offline PATCH has already left the debounce queue. Recreate it
  // from the editor when connectivity returns so the status and REST baseline
  // recover without requiring another keystroke or a reload.
  if ((state.role === "owner" || state.role === "author") && editor.state.doc.toString() !== state.document?.body) {
    scheduleSave()
  }
  // One-shot actions such as create/delete are intentionally not replayed:
  // automatically repeating a destructive request after reconnect would be
  // surprising. Do clear the stale browser transport error and tell the user
  // exactly what remains to do.
  if (el<HTMLElement>("#save-status")?.textContent === "Failed to fetch") {
    status("Back online — retry the action")
  }
})

renderAuth()
renderReviewBanner()
renderViewSwitch()
renderBuildLabel()
renderBuildHealth()
renderBuildLog()
renderSectionOutline()
clearPreview()
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
  applyPresenceRoster([])
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
    applyRoleGates()
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
