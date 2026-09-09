/** Project preview lifecycle, provenance, and PDF downloads. */
import { Effect } from "effect"
import type { EditorView } from "@codemirror/view"
import type { LoroDoc } from "loro-crdt"
import * as api from "./api"
import type { CompileView, MarkInput, NisabaDocument, Project } from "./api"
import { currentBindings, prettyChord } from "./keybindings"
import { resolveCursor } from "./cursor"
import { decodeBase64Pdf, downloadBase64 } from "./effects"
import type { VirtualPdfViewer } from "./pdf-viewer"
import type { ReviewItem, ReviewState } from "./review"

// ---------------------------------------------------------------------------
// Compile diagnostics (mirrors services/compile Diagnostic: severity/message/path/start/end)
// ---------------------------------------------------------------------------

export interface CompileDiagnostic {
  readonly severity: string
  readonly message: string
  readonly path?: string | null
  readonly start?: number | null
  readonly end?: number | null
}

/** Build-log severity levels; main.ts's BuildLogEntry reuses this vocabulary. */
export type BuildLogLevel = "ok" | "warn" | "err" | "info"

/** The writer's words for the compile views (docs/ui-design.md §1). Shared with
 *  main.ts's view switch and command palette; lives here because the build
 *  label and the build log line name the view a build was made under. */
export const VIEW_LABELS: Record<CompileView, string> = {
  proposed: "Final",
  baseline: "Original",
  redline: "All markup",
  public: "Public copy"
}

// ---------------------------------------------------------------------------
// Lifecycle state — owned here, written only by the code below
// ---------------------------------------------------------------------------

let compiling = false
type BuildMode = "manual" | "background"
let pendingCompile: BuildMode | undefined
let previewSession = 0

/** What the current preview was built from — shown on the preview bar. */
interface BuildSummary {
  readonly buildId: string
  readonly entry: string
  readonly view: CompileView
  readonly projectId: string
  readonly pdf: string | null
  readonly generation: number
  readonly at: number
  readonly ms: number
  pages?: number
}

let lastBuild: BuildSummary | undefined
let generation = 0

// ---------------------------------------------------------------------------
// The borrowed handles: main.ts owns them and passes them in via initCompile
// ---------------------------------------------------------------------------

/** The slice of the workspace the compile paths read. main.ts owns the whole
 *  Workspace object and may keep writing it; this interface is exactly what
 *  the compile subsystem demands of it (all reads, no writes). */
export interface CompileWorkspace {
  project?: Project
  selected?: NisabaDocument
  view: CompileView
  review: ReviewState
  diagnostics: readonly CompileDiagnostic[]
}

/** Everything the compile module needs but does not own. main.ts supplies these
 *  once, before the first render; every function below reaches them through
 *  requireHost(), so a missing initCompile fails loudly at the first call. */
export interface CompileHost {
  readonly state: CompileWorkspace
  readonly editor: EditorView
  readonly pdfViewer: VirtualPdfViewer
  /** The active document's Loro replica — a getter because main.ts swaps the
   *  replica on every document switch. */
  getActiveLoro(): LoroDoc
  el<T extends HTMLElement>(selector: string): T | null
  setText(selector: string, value: string): void
  status(message: string): void
  run<A>(effect: Effect.Effect<A, api.ApiError>, onSuccess: (value: A) => void, onError: (error: unknown) => void): void
  timeAgo(timestamp: number): string
  renderDiagnostics(diagnostics: readonly CompileDiagnostic[]): void
  logBuild(level: BuildLogLevel, text: string): void
  setDrawerOpen(open: boolean, tab?: "problems" | "log"): void
  updateZoomLabel(): void
  renderPagePosition(): void
}

let host: CompileHost | undefined

/** The one-time handoff from main.ts. Must be called before any other export. */
export function initCompile(handles: CompileHost): void {
  host = handles
}

function requireHost(): CompileHost {
  if (host === undefined) throw new Error("compile module used before initCompile()")
  return host
}

// ---------------------------------------------------------------------------
// Preview pane states
// ---------------------------------------------------------------------------

/** Restores the preview pane's never-compiled empty state. */
export function clearPreview(): void {
  const { el, setText } = requireHost()
  const viewer = el<HTMLElement>("#pdf-viewer")
  viewer?.replaceChildren()
  viewer?.classList.add("empty-preview")
  viewer?.append(makeEmptyPreviewNode("Nothing to show yet", "Choose Update preview to render the pages."))
  el<HTMLElement>("#pdf-zoom-controls")?.setAttribute("hidden", "")
  setText("#page-position", "")
}

function makeEmptyPreviewNode(title: string, body: string): HTMLElement {
  const node = document.createElement("div")
  node.className = "pane-empty"
  const heading = document.createElement("h2")
  heading.textContent = title
  const text = document.createElement("p")
  text.textContent = body
  node.append(heading, text)
  return node
}

function showPreviewFailure(message: string): void {
  const { el, setText } = requireHost()
  const viewer = el<HTMLElement>("#pdf-viewer")
  if (!viewer) return
  viewer.replaceChildren()
  viewer.classList.add("empty-preview")
  viewer.append(makeEmptyPreviewNode("The preview could not be built", message))
  el<HTMLElement>("#pdf-zoom-controls")?.setAttribute("hidden", "")
  setText("#page-position", "")
}

// ---------------------------------------------------------------------------
// Status-bar cells and the compile button
// ---------------------------------------------------------------------------

/** Keeps the compile button honest about what it is doing right now. */
function setCompileButtonBusy(busy: boolean): void {
  const { el } = requireHost()
  const button = el<HTMLButtonElement>("#compile-button")
  if (!button) return
  button.disabled = busy
  button.replaceChildren()
  button.append(busy ? "Building…" : "Update preview")
  if (!busy) {
    const key = document.createElement("kbd")
    // Rendered from the live bindings so a busy→idle re-creation shows the
    // current chord, and tagged data-chord so the rebind sweep can still
    // find this re-created kbd after a build (both directions are needed).
    key.dataset.chord = "compile"
    key.textContent = prettyChord(currentBindings().compile)
    button.append(" ", key)
  }
}

export function renderBuildLabel(): void {
  const { el, timeAgo } = requireHost()
  const label = el<HTMLElement>("#build-label")
  if (!label) return
  if (lastBuild === undefined) {
    label.textContent = "No preview yet"
    label.title = ""
    label.classList.remove("preview-stale")
    return
  }
  label.textContent = `${lastBuild.entry} · ${VIEW_LABELS[lastBuild.view]} · ${timeAgo(lastBuild.at)}${lastBuild.generation !== generation ? " · Edited since preview" : ""}`
  label.classList.toggle("preview-stale", lastBuild.generation !== generation)
  // Provenance is a first-class fact in Nisaba, but the build id is expert
  // metadata: it belongs in the tooltip and the log, not in the writer's line.
  label.title = lastBuild.ms > 0
    ? `Build ${lastBuild.buildId} · ${(lastBuild.ms / 1000).toFixed(2)} s`
    : `Build ${lastBuild.buildId}`
}

/**
 * The build cell in the status bar: the one-glance answer to "is the preview
 * good?". Clicking it opens the drawer at the tab that explains the answer.
 */
export function renderBuildHealth(): void {
  const { el, state } = requireHost()
  const cell = el<HTMLButtonElement>("#build-health")
  if (!cell) return
  const errors = state.diagnostics.filter((item) => item.severity !== "warning").length
  const warnings = state.diagnostics.length - errors
  cell.replaceChildren()
  const mark = document.createElement("span")
  if (errors > 0) {
    mark.className = "status-error"
    mark.textContent = `${errors} problem${errors === 1 ? "" : "s"}`
    cell.title = "Open the problems panel"
  } else if (lastBuild === undefined) {
    mark.textContent = "No preview yet"
    cell.title = "Choose Update preview to build the pages"
  } else {
    mark.className = warnings > 0 ? "status-warn" : "status-ok"
    const pages = lastBuild.pages === undefined ? "" : ` · ${lastBuild.pages} page${lastBuild.pages === 1 ? "" : "s"}`
    mark.textContent = warnings > 0
      ? `${warnings} warning${warnings === 1 ? "" : "s"}${pages}`
      : `${lastBuild.generation !== generation ? "Preview needs updating" : "Preview ready"}${pages}`
    cell.title = "Open the build log"
  }
  cell.append(mark)
}

function elDownloadDisabled(disabled: boolean): void {
  const button = requireHost().el<HTMLButtonElement>("#download-preview")
  if (button) button.disabled = disabled
}

/** Leave the previous project's build behind and invalidate its pending results. */
export function resetBuildSummary(): void {
  previewSession++
  pendingCompile = undefined
  lastBuild = undefined
  elDownloadDisabled(true)
  renderBuildLabel()
  renderBuildHealth()
}

// ---------------------------------------------------------------------------
// Project inputs and PDF rendering.
// ---------------------------------------------------------------------------

/**
 * The suggestion marks sent with a compile request. Projection happens
 * server-side over these marks: only open, non-orphaned suggestions affect the
 * body text — accepted/rejected ones are already reflected (or removed) in the
 * editor text, and comments never change visibility (see projection.rs).
 * Offsets are editor doc offsets, which match the compile source exactly; clamp
 * `end` to the doc length as a guard against any stale position that escaped
 * updateReviewItems' remapping.
 */
function collectOpenSuggestionMarks(): readonly MarkInput[] {
  const { editor, state, getActiveLoro } = requireHost()
  const activeLoro = getActiveLoro()
  const docLength = editor.state.doc.length
  return state.review.items
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
}

/** Capture all project files with the open editor's current draft. */
function compileRequest(projectId: string, marks: readonly MarkInput[]): Effect.Effect<api.CompileResponse & { entry: string; view: CompileView }, api.ApiError> {
  const { editor, state } = requireHost()
  return api.previewProject(projectId, state.view, {
    document_id: state.selected!.id,
    body: editor.state.doc.toString(),
    marks
  }).pipe(Effect.map((result) => ({ ...result.compile, entry: result.entry, view: result.view })))
}

/** Local and received edits make the displayed PDF older than the document. */
export function markPreviewStale(): void {
  generation++
  renderBuildLabel()
  renderBuildHealth()
}

/** Downloads the bytes of the displayed build without compiling again. */
export function downloadPreview(): void {
  const { state, status } = requireHost()
  if (!lastBuild?.pdf || lastBuild.projectId !== state.project?.id) {
    status("Update preview before downloading a PDF")
    return
  }
  downloadBase64(lastBuild.pdf, `${state.project.name}.pdf`, "application/pdf")
}

/**
 * Puts the PDF of a clean build on the canvas — the shared success path of
 * both compile paths; only the render-failure policy differs (`onRenderError`).
 *
 * Bytes are handed to PDF.js directly: object URLs can be revoked by a later
 * rapid compile while the worker is still fetching them, producing a
 * successful build with a broken preview.
 */
function loadCleanPdf(pdf: string, onRenderError?: (error: unknown) => void): void {
  const { el, pdfViewer, updateZoomLabel, renderPagePosition } = requireHost()
  const build = lastBuild
  const data = decodeBase64Pdf(pdf)
  // `empty-preview` is the empty-state marker; drop it once a real PDF is being
  // rendered (clearPreview/showPreviewFailure re-add it on clear/fail).
  el<HTMLElement>("#pdf-viewer")?.classList.remove("empty-preview")
  updateZoomLabel()
  el<HTMLElement>("#pdf-zoom-controls")?.removeAttribute("hidden")
  void pdfViewer.load(data).then(() => {
    if (lastBuild !== build) return
    if (build) build.pages = pdfViewer.pageCount
    renderPagePosition()
    renderBuildHealth()
  }).catch((error: unknown) => {
    console.error("PDF render failed", error)
    if (lastBuild === build) onRenderError?.(error)
  })
}

export function compileCurrent(): void { buildProject("manual") }
export function compileForDiagnostics(): void { buildProject("background") }

/** One project pipeline; explicit builds add progress and error reporting. */
function buildProject(mode: BuildMode): void {
  const { state, status, renderDiagnostics, setDrawerOpen, logBuild } = requireHost()
  const { project, selected } = state
  if (!project || !selected) {
    if (mode === "manual") status("Open a document first")
    return
  }
  if (compiling) {
    // A manual request takes priority, but a typing pause must also get a turn.
    if (pendingCompile !== "manual") pendingCompile = mode
    return
  }
  compiling = true
  const session = previewSession
  const buildGeneration = generation
  const startedAt = Date.now()
  const current = (): boolean => state.project?.id === project.id && previewSession === session
  if (mode === "manual") setCompileButtonBusy(true)
  const settle = (): void => {
    compiling = false
    if (mode === "manual") setCompileButtonBusy(false)
    const next = pendingCompile
    pendingCompile = undefined
    if (next) buildProject(next)
  }
  runCompile(settle, compileRequest(project.id, collectOpenSuggestionMarks()),
    (result) => {
      if (!current()) return
      const diagnostics = result.diagnostics as readonly CompileDiagnostic[]
      const errors = diagnostics.filter((item) => item.severity !== "warning").length
      const warnings = diagnostics.length - errors
      renderDiagnostics(diagnostics)
      if (result.pdf_base64 && errors === 0) {
        lastBuild = {
          buildId: result.build_id, entry: result.entry, view: result.view,
          projectId: project.id, pdf: result.pdf_base64, generation: buildGeneration,
          at: Date.now(), ms: Date.now() - startedAt
        }
        elDownloadDisabled(false)
        renderBuildLabel()
        loadCleanPdf(result.pdf_base64, (error) => {
          showPreviewFailure(error instanceof Error ? error.message : "The pages could not be rendered.")
        })
      } else if (!lastBuild) {
        showPreviewFailure(errors > 0 ? "Fix the problems listed below and try again." : "The build produced no pages.")
      }
      renderBuildHealth()
      if (mode === "manual") {
        if (errors > 0) setDrawerOpen(true, "problems")
        logBuild(errors > 0 ? "err" : warnings > 0 ? "warn" : "ok",
          `${result.entry} · ${VIEW_LABELS[result.view]} · ${((Date.now() - startedAt) / 1000).toFixed(2)} s · build ${result.build_id}`)
        status(errors > 0 ? `${errors} problem${errors === 1 ? "" : "s"} stopped the preview`
          : result.pdf_base64 ? "Preview updated" : "The build produced no pages")
      }
    },
    (error: unknown) => {
      if (!current() || mode !== "manual") return
      const message = error instanceof Error ? error.message : "The preview could not be built"
      if (!lastBuild) showPreviewFailure(message)
      logBuild("err", message)
      status(message)
    })
}

/** Whether a compile (manual or background) is in flight. Read-only accessor
 *  for main.ts: role gating re-enables the compile button for every role and
 *  must not stomp the disabled state an in-flight manual compile just set. */
export function isCompiling(): boolean {
  return compiling
}

function runCompile<A>(settle: () => void, effect: Effect.Effect<A, api.ApiError>, success: (result: A) => void, failure: (error: unknown) => void): void {
  requireHost().run(effect,
    (result) => { try { success(result) } finally { settle() } },
    (error) => { try { failure(error) } finally { settle() } })
}
