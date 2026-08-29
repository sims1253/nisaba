/**
 * The compile/preview subsystem, extracted from the workspace shell (main.ts).
 *
 * Everything here is behaviour-identical to the code it replaces: same DOM ids,
 * same Effect pipes, same policies. The two compile paths are two policies over
 * one set of machinery (see the "Shared compile plumbing" banner below).
 *
 * Ownership: this module OWNS the compile lifecycle state (`compiling`,
 * `pendingCompile`, `lastBuild`) and renders the surfaces only it can interpret
 * (the build label, the build-health cell, the preview pane's empty/failure/
 * clean-PDF states, the compile button's busy state). It owns NONE of the
 * handles it drives: the workspace state, the editor, the PDF viewer, the
 * active Loro replica, and the renderers its results feed into all belong to
 * main.ts and are handed in once, via initCompile, before the first render.
 */
import { Effect } from "effect"
import type { EditorView } from "@codemirror/view"
import type { LoroDoc } from "loro-crdt"
import * as api from "./api"
import type { CompileView, MarkInput, NisabaDocument, Project } from "./api"
import { currentBindings, prettyChord } from "./keybindings"
import { resolveCursor } from "./cursor"
import { decodeBase64Pdf } from "./effects"
import type { VirtualPdfViewer } from "./pdf-viewer"
import type { ReviewItem, ReviewState } from "./review"
import { wasmCompile } from "./wasm-compile"

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
/** When a manual compile (button/Ctrl+S/view switch) arrives while a compile is
 *  in flight, we can't run it immediately. This flag ensures it runs as soon as
 *  the in-flight compile finishes, so the user's explicit action is never lost. */
let pendingCompile = false

/** What the current preview was built from — shown on the preview bar. */
interface BuildSummary {
  readonly buildId: string
  readonly at: number
  readonly ms: number
  pages?: number
}

let lastBuild: BuildSummary | undefined

// ---------------------------------------------------------------------------
// The borrowed handles: main.ts owns them and passes them in via initCompile
// ---------------------------------------------------------------------------

/** The slice of the workspace the compile paths read. main.ts owns the whole
 *  Workspace object and may keep writing it; this interface is exactly what
 *  the compile subsystem demands of it (all reads, no writes). */
export interface CompileWorkspace {
  project?: Project
  selected?: { readonly document: NisabaDocument }
  view: CompileView
  review: ReviewState
  diagnostics: readonly CompileDiagnostic[]
  /** The project's reference rows, as the library pane shows them. The
   *  in-browser compile path builds its bibliography from these (the app
   *  service reads them from the database on every server compile); the
   *  server path never looks at them — marks and references travel in the
   *  request it posts. */
  references: readonly api.Reference[]
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
  // When the writer opted into in-browser compiles, start paying the wasm
  // artifact download on idle instead of at their first build. A no-op for
  // everyone else (the default path), and boot failures surface later as the
  // fallback note below, not here.
  wasmCompile.prefetch()
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
  const { el, state, timeAgo } = requireHost()
  const label = el<HTMLElement>("#build-label")
  if (!label) return
  if (lastBuild === undefined) {
    label.textContent = "No preview yet"
    label.title = ""
    return
  }
  label.textContent = `${VIEW_LABELS[state.view]} · built ${timeAgo(lastBuild.at)}`
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
      : `Preview up to date${pages}`
    cell.title = "Open the build log"
  }
  cell.append(mark)
}

/** Clears the last-build summary and repaints the surfaces that show it — the
 *  document-switch path in main.ts (a newly opened document has no build yet).
 *  The one write to lastBuild from outside the compile paths. */
export function resetBuildSummary(): void {
  lastBuild = undefined
  renderBuildLabel()
  renderBuildHealth()
}

// ---------------------------------------------------------------------------
// Shared compile plumbing. compileCurrent (manual) and compileForDiagnostics
// (background) are two policies over this machinery: every line where they
// differ is a deliberate, commented choice, not drift.
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

/**
 * The compile request both paths send: the current document as a single
 * in-memory source, its open suggestion marks, and the active view. One
 * builder so the manual and background paths can never drift in what they
 * ask the engine to build.
 *
 * Which engine serves it is the wasm-compile dispatcher's call (issue #20
 * stage 2c): the server route by default, an in-browser Web Worker for users
 * who opted in via the localStorage toggle — and, when the opted-in engine
 * cannot serve (artifacts not built, worker failed), the server again, with
 * one log line saying so. Both paths consume the identical request and
 * produce the identical response contract, so nothing downstream can tell
 * (or needs to tell) the difference except the log line.
 */
function compileRequest(projectId: string, entry: string, marks: readonly MarkInput[]): Effect.Effect<api.CompileResponse, api.ApiError> {
  const { editor, state } = requireHost()
  const fallback = wasmCompile.fallbackReason()
  if (fallback !== undefined) noteWasmCompileFallback(fallback)
  return wasmCompile.compile(
    {
      projectId,
      entry,
      sources: { [entry]: editor.state.doc.toString() },
      marks: { [entry]: marks },
      view: state.view
    },
    state.references
  )
}

/** Whether the fallback note has been logged this session. The note exists so
 *  an opted-in writer learns why builds still hit the server (usually: the
 *  artifacts are not built — `just wasm-web`); once is enough. */
let notedWasmFallback = false

function noteWasmCompileFallback(reason: string): void {
  if (notedWasmFallback) return
  notedWasmFallback = true
  const { logBuild } = requireHost()
  logBuild("info", `In-browser compile is enabled but unavailable: ${reason}. Building on the server.`)
}

/** The engine suffix for the build log line: which path served the compile
 *  (issue #20 stage 2c). The log is the expert surface, so the writer's
 *  status line and the build label stay engine-agnostic. */
function engineLabel(): string {
  return wasmCompile.lastServedBy() === "wasm" ? "in-browser" : "server"
}

/** Release after a manual compile settles: free the guard, re-enable the
 *  compile button (only manual compiles disable it), and run any compile that
 *  was queued while this one was in flight. */
function settleManualCompile(): void {
  compiling = false
  setCompileButtonBusy(false)
  drainPendingCompile()
}

/** Release after a background compile settles: free the guard and drain the
 *  queue. The button was never touched, so there is nothing to re-enable. */
function settleBackgroundCompile(): void {
  compiling = false
  drainPendingCompile()
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
  const data = decodeBase64Pdf(pdf)
  // `empty-preview` is the empty-state marker; drop it once a real PDF is being
  // rendered (clearPreview/showPreviewFailure re-add it on clear/fail).
  el<HTMLElement>("#pdf-viewer")?.classList.remove("empty-preview")
  updateZoomLabel()
  el<HTMLElement>("#pdf-zoom-controls")?.removeAttribute("hidden")
  void pdfViewer.load(data).then(() => {
    if (lastBuild) lastBuild.pages = pdfViewer.pageCount
    renderPagePosition()
    renderBuildHealth()
  }).catch((error: unknown) => {
    console.error("PDF render failed", error)
    onRenderError?.(error)
  })
}

export function compileCurrent(): void {
  const { state, status, setText, run, renderDiagnostics, setDrawerOpen, logBuild } = requireHost()
  const { project, selected } = state
  if (!project || !selected) { status("Open a document first"); return }
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
  setCompileButtonBusy(true)
  setText("#build-label", "Building…")
  const startedAt = Date.now()
  // Clear the previous result on EVERY attempt so a failing compile can never
  // leave a stale PDF (or stale kept pages) on the canvas — the pane must reflect
  // the source being compiled, not the last successful build.
  clearPreview()
  renderDiagnostics([])
  run(
    compileRequest(project.id, entry, collectOpenSuggestionMarks()).pipe(
      Effect.tap(() => Effect.sync(settleManualCompile)),
      Effect.tapError(() => Effect.sync(settleManualCompile))
    ),
    (result) => {
      // Document-switch guard: discard the result if the user has moved to a
      // different document while this compile was in flight.
      if (state.selected?.document.id !== documentId) return
      const diagnostics = result.diagnostics as readonly CompileDiagnostic[]
      const errors = diagnostics.filter((item) => item.severity !== "warning").length
      const warnings = diagnostics.length - errors
      // A manual compile reports real elapsed time — the writer asked for this
      // build, so the label's tooltip shows what it cost.
      lastBuild = { buildId: result.build_id, at: Date.now(), ms: Date.now() - startedAt }
      renderBuildLabel()
      renderDiagnostics(diagnostics)
      // The PDF only matches the source when the build is clean; a build with
      // errors (or no PDF) is shown as an empty/failure state, never the stale
      // canvas from the previous successful compile.
      const pdf = result.pdf_base64
      if (pdf && errors === 0) {
        // A render failure of a successful manual build is the writer's problem
        // to know about: it goes on the pane, not just the console.
        loadCleanPdf(pdf, (error) => showPreviewFailure(error instanceof Error ? error.message : "The pages could not be rendered."))
      } else {
        showPreviewFailure(diagnostics.length > 0 ? "Fix the problems listed below and try again." : "The build produced no pages.")
      }
      // The drawer opens itself when a build fails: the writer needs the reason,
      // and the reason is one click away from the line that caused it.
      if (errors > 0) setDrawerOpen(true, "problems")
      renderBuildHealth()
      logBuild(
        errors > 0 ? "err" : warnings > 0 ? "warn" : "ok",
        `build ${result.build_id} — ${VIEW_LABELS[state.view].toLowerCase()} · ${((Date.now() - startedAt) / 1000).toFixed(2)} s${errors > 0 ? ` · ${errors} problem${errors === 1 ? "" : "s"}` : warnings > 0 ? ` · ${warnings} warning${warnings === 1 ? "" : "s"}` : ""} · ${engineLabel()}`
      )
      status(errors > 0
        ? `${errors} problem${errors === 1 ? "" : "s"} stopped the preview`
        : warnings > 0 ? `Preview updated with ${warnings} warning${warnings === 1 ? "" : "s"}` : "Preview updated")
    },
    (error: unknown) => {
      // Document-switch guard: don't clobber the new document's preview with the
      // old document's compile error.
      if (state.selected?.document.id !== documentId) return
      // A thrown/transport error leaves the pane empty with the reason rather
      // than the last good PDF.
      const message = error instanceof Error ? error.message : "The preview could not be built"
      showPreviewFailure(message)
      logBuild("err", message)
      renderBuildHealth()
      status(message)
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
export function compileForDiagnostics(): void {
  const { state, run, renderDiagnostics } = requireHost()
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
  run(
    compileRequest(project.id, selected.document.path, collectOpenSuggestionMarks()).pipe(
      Effect.tap(() => Effect.sync(settleBackgroundCompile)),
      Effect.tapError(() => Effect.sync(settleBackgroundCompile))
    ),
    (result) => {
      // Document-switch guard: if the user has switched documents while this
      // background compile was in flight, discard the result.
      if (state.selected?.document.id !== documentId) return
      const diagnostics = result.diagnostics as readonly CompileDiagnostic[]
      renderDiagnostics(diagnostics)
      // Update the PDF preview only on a clean build. A build with errors leaves
      // the last good PDF in place (better than clearing to an empty pane on
      // every transient typo). The zoom controls are managed the same way as a
      // manual compile.
      const errors = diagnostics.filter((item) => item.severity !== "warning").length
      const pdf = result.pdf_base64
      if (pdf && errors === 0) {
        // `ms: 0` is deliberate: renderBuildLabel only shows a timing in the
        // tooltip when ms > 0, and a background build the writer never asked
        // for should not claim one.
        lastBuild = { buildId: result.build_id, at: Date.now(), ms: 0 }
        renderBuildLabel()
        // A render failure is logged (in loadCleanPdf) but not surfaced:
        // showing it would clobber the last good PDF the writer is still
        // reading.
        loadCleanPdf(pdf)
      }
      // The status bar must reflect what the background build just found, or a
      // typo silently introduces problems the writer is never told about.
      renderBuildHealth()
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

/** Whether a compile (manual or background) is in flight. Read-only accessor
 *  for main.ts: role gating re-enables the compile button for every role and
 *  must not stomp the disabled state an in-flight manual compile just set. */
export function isCompiling(): boolean {
  return compiling
}
