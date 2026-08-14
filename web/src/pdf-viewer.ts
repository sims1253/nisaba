import type { PDFDocumentLoadingTask, PDFDocumentProxy, RenderTask, TextLayer } from "pdfjs-dist"

// pdfjs-dist worker: the `new URL(..., import.meta.url)` pattern does not
// resolve reliably under all bundler/nginx combinations. Importing the worker
// entry directly lets Vite emit it as a hashed asset and wire the URL.
import PdfWorker from "pdfjs-dist/build/pdf.worker.min.mjs?worker"

// ONE worker port for the whole session, created lazily on first load.
// loadPdfDocument used to assign `GlobalWorkerOptions.workerPort = new
// PdfWorker()` per call: every assignment handed pdfjs a brand-new Worker that
// nothing ever terminated, and since compileForDiagnostics reloads the preview
// after every typing pause, a long session accumulated one dead worker (plus
// its message channel) per 2s-debounced recompile. Reusing a single port is
// the supported pattern: pdfjs caches one PDFWorker wrapper per port
// (PDFWorker.#workerPorts) and the underlying Worker stays alive across
// documents.
let pdfWorkerPort: Worker | undefined

async function loadPdfDocument(data: Uint8Array): Promise<PDFDocumentLoadingTask> {
  const { GlobalWorkerOptions, getDocument } = await import("pdfjs-dist")
  pdfWorkerPort ??= new PdfWorker()
  GlobalWorkerOptions.workerPort = pdfWorkerPort
  return getDocument({ data })
}

const ZOOM_LEVELS = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0]
const DEFAULT_ZOOM_INDEX = 3

/**
 * Virtualised PDF viewer with zoom, text selection, and bidirectional sync with
 * the editor.
 *
 * Each page is rendered as a canvas (the visual) plus an invisible text layer
 * (pdf.js `TextLayer`) positioned over it. The text layer makes the PDF
 * selectable — the user can highlight text just like in a native PDF viewer —
 * and its DOM nodes carry the position data we use for search highlighting and
 * the PDF→editor reverse search.
 *
 * Middle-click does whatever the browser does natively for the container (no
 * interception). Double-clicking a word in the PDF calls back to the editor so
 * it can search and highlight the source text.
 */
export class VirtualPdfViewer {
  private document?: PDFDocumentProxy
  /** The in-flight/completed loading task behind `document`; destroyed on the next load. */
  private loadingTask?: PDFDocumentLoadingTask
  private observer?: IntersectionObserver
  private generation = 0
  private zoomIndex = DEFAULT_ZOOM_INDEX
  private renderedPages = new Set<number>()
  private activeRenders = new Map<number, RenderTask>()
  private activeTextLayers = new Map<number, TextLayer>()
  private highlightEl: HTMLElement | null = null
  private highlightTimers: ReturnType<typeof setTimeout>[] = []
  private onDblClickText?: (text: string) => void

  constructor(
    private readonly container: HTMLElement,
    opts?: { onDblClickText?: (text: string) => void }
  ) {
    this.onDblClickText = opts?.onDblClickText
    // Double-click in the PDF text layer → extract the selected word and notify
    // the callback so the editor can jump to it.
    this.container.addEventListener("dblclick", () => {
      const sel = window.getSelection()
      const text = sel?.toString().trim()
      if (text && this.onDblClickText) this.onDblClickText(text)
    })
  }

  get zoom(): number { return ZOOM_LEVELS[this.zoomIndex] ?? 1.25 }
  get zoomPercent(): string { return `${Math.round(this.zoom * 100)}%` }
  /** Pages in the loaded document, or 0 when nothing is loaded. Drives the
   *  "page 3 of 12" readout on the preview bar and the build log's page count. */
  get pageCount(): number { return this.document?.numPages ?? 0 }

  zoomIn(): void {
    if (this.zoomIndex < ZOOM_LEVELS.length - 1) { this.zoomIndex++; this.rerenderVisible() }
  }
  zoomOut(): void {
    if (this.zoomIndex > 0) { this.zoomIndex--; this.rerenderVisible() }
  }
  resetZoom(): void {
    if (this.zoomIndex === DEFAULT_ZOOM_INDEX) return
    this.zoomIndex = DEFAULT_ZOOM_INDEX
    this.rerenderVisible()
  }

  private rerenderVisible(): void {
    const pdf = this.document
    if (!pdf || !this.observer) return
    // Bump generation so any renderPage() already in flight (awaiting getPage
    // or mid canvas/text-layer render) aborts instead of committing a render at
    // the previous zoom. Cancel the in-flight canvas + text-layer work too.
    this.generation++
    this.activeRenders.forEach((task) => task.cancel())
    this.activeRenders.clear()
    this.activeTextLayers.forEach((layer) => layer.cancel())
    this.activeTextLayers.clear()
    // Reset every page to its placeholder and re-observe it. Pages already in
    // view were unobserved after their first render, so observe() fires again
    // and re-renders them at the new zoom; out-of-view pages render on demand.
    for (const pageEl of this.container.querySelectorAll<HTMLElement>(".pdf-page")) {
      const num = Number(pageEl.dataset.page)
      pageEl.innerHTML = `<div class="pdf-loading" aria-label="Loading PDF page ${num}">Loading page ${num}\u2026</div>`
      this.observer.unobserve(pageEl)
      this.observer.observe(pageEl)
    }
    this.renderedPages.clear()
  }

  async load(data: Uint8Array): Promise<void> {
    const generation = ++this.generation
    this.observer?.disconnect()
    this.container.replaceChildren()
    this.renderedPages.clear()
    this.activeRenders.forEach((r) => r.cancel())
    this.activeRenders.clear()
    this.activeTextLayers.forEach((layer) => layer.cancel())
    this.activeTextLayers.clear()
    this.clearHighlight()
    // Tear down the PREVIOUS loading task/document before starting the next
    // one. load() used to replace this.document without destroying it, so every
    // recompile leaked the old PDFDocumentProxy and its transport. destroy()
    // must also SETTLE before the shared worker port can serve the next
    // document (pdfjs marks the port's worker _pendingDestroy until teardown
    // finishes and refuses new documents in that window), so it is awaited
    // rather than fire-and-forgotten; its errors are swallowed because a
    // half-dead document must not fail the fresh load.
    const previousTask = this.loadingTask
    this.loadingTask = undefined
    this.document = undefined
    try { await previousTask?.destroy() } catch { /* teardown of an already-dead document */ }
    const task = await loadPdfDocument(data)
    if (generation !== this.generation) {
      void task.destroy().catch(() => undefined)
      return
    }
    this.loadingTask = task
    let pdf: PDFDocumentProxy
    try {
      pdf = await task.promise
    } catch (error) {
      // A destroy issued by a newer load() rejects an in-flight promise
      // ("Worker was destroyed"); that is supersession, not a render failure,
      // so it bails silently instead of surfacing a bogus preview error.
      if (generation !== this.generation) return
      throw error
    }
    if (generation !== this.generation) {
      void pdf.destroy().catch(() => undefined)
      return
    }
    this.document = pdf
    const pages = Array.from({ length: pdf.numPages }, (_, index) => {
      const page = document.createElement("article")
      page.className = "pdf-page pdf-page-shell"
      page.dataset.page = String(index + 1)
      page.innerHTML = `<div class="pdf-loading" aria-label="Loading PDF page ${index + 1}">Loading page ${index + 1}\u2026</div>`
      this.container.append(page)
      return page
    })
    this.observer = new IntersectionObserver((entries) => {
      for (const entry of entries) if (entry.isIntersecting) {
        const page = entry.target as HTMLElement
        void this.renderPage(pdf, Number(page.dataset.page), page, this.generation)
        this.observer?.unobserve(page)
      }
    }, { root: this.container, rootMargin: "900px 0px" })
    pages.forEach((page) => this.observer?.observe(page))
  }

  private async renderPage(pdf: PDFDocumentProxy, pageNumber: number, host: HTMLElement, generation: number): Promise<void> {
    const page = await pdf.getPage(pageNumber)
    if (generation !== this.generation) return
    const viewport = page.getViewport({ scale: this.zoom })
    host.style.width = `${viewport.width}px`
    host.style.height = `${viewport.height}px`
    // --scale-factor drives the .pdf-text-layer span sizing/transform chain (see
    // styles.css). Without it, pdf.js v5 TextLayer spans inherit body font-size and
    // text selection rectangles don't line up with glyphs at non-default zoom.
    host.style.setProperty("--scale-factor", String(this.zoom))

    // Canvas (the visual render)
    const canvas = document.createElement("canvas")
    canvas.width = viewport.width
    canvas.height = viewport.height
    canvas.setAttribute("aria-label", `PDF page ${pageNumber}`)

    // Text layer container (invisible overlay for selection + search)
    const textLayerDiv = document.createElement("div")
    textLayerDiv.className = "pdf-text-layer"

    host.replaceChildren(canvas, textLayerDiv)
    this.renderedPages.add(pageNumber)

    // Render canvas
    const context = canvas.getContext("2d")
    if (!context) throw new Error(`Could not get 2D context for PDF page ${pageNumber}`)
    const renderTask = page.render({ canvas, canvasContext: context, viewport })
    this.activeRenders.set(pageNumber, renderTask)
    try {
      await renderTask.promise
    } catch (err: unknown) {
      if (err instanceof Error && err.name === "RenderingCancelledException") return
      throw err
    } finally {
      // Only drop our own entry: a newer render for the same page (zoom/load)
      // may have already overwritten it.
      if (this.activeRenders.get(pageNumber) === renderTask) this.activeRenders.delete(pageNumber)
    }
    if (generation !== this.generation) return

    // Render text layer using pdf.js TextLayer utility
    const textContent = await page.getTextContent()
    if (generation !== this.generation) return
    const { TextLayer } = await import("pdfjs-dist")
    // Cancel any previous text layer for this page (e.g. a mid-zoom re-render)
    // before starting a new one, and track this one so rerenderVisible()/load()
    // can cancel it if the generation changes mid-render.
    this.activeTextLayers.get(pageNumber)?.cancel()
    const textLayer = new TextLayer({
      textContentSource: textContent,
      container: textLayerDiv,
      viewport,
    })
    this.activeTextLayers.set(pageNumber, textLayer)
    try {
      await textLayer.render()
    } catch (err: unknown) {
      // TextLayer.cancel() rejects with an AbortException; treat that as a
      // normal abort (zoom/load) and bail without surfacing an error.
      if (!(err instanceof Error && err.name === "AbortException")) throw err
    } finally {
      if (this.activeTextLayers.get(pageNumber) === textLayer) this.activeTextLayers.delete(pageNumber)
    }
  }

  /**
   * Clears any existing search highlight overlay.
   */
  clearHighlight(): void {
    this.highlightTimers.forEach((t) => clearTimeout(t))
    this.highlightTimers = []
    this.highlightEl?.remove()
    this.highlightEl = null
  }

  /**
   * Searches the PDF text for a query string, scrolls to the first match, and
   * places a highlight overlay on the match position so the user can find it.
   *
   * The text layer DOM nodes (rendered by pdf.js TextLayer) carry the position
   * data we need: each <span> in the text layer corresponds to a text item with
   * known viewport coordinates.
   */
  async searchAndScroll(query: string): Promise<boolean> {
    const pdf = this.document
    if (!pdf || query.trim().length === 0) return false
    this.clearHighlight()
    const normalizedQuery = query.trim().toLowerCase()

    for (let pageNum = 1; pageNum <= pdf.numPages; pageNum++) {
      const page = await pdf.getPage(pageNum)
      const textContent = await page.getTextContent()

      // Reconstruct full text and track which item the match falls in.
      let fullText = ""
      const items: { str: string; transform: number[]; width: number; height: number }[] = []
      for (const item of textContent.items) {
        if (!("str" in item)) continue
        const ti = item as { str: string; transform: number[]; width: number; height: number }
        items.push(ti)
        fullText += ti.str
      }
      const matchIdx = fullText.toLowerCase().indexOf(normalizedQuery)
      if (matchIdx === -1) continue

      // Find the text item containing the match start.
      let cumLen = 0
      let matchItem: { str: string; transform: number[]; width: number; height: number } | null = null
      for (const item of items) {
        if (cumLen + item.str.length > matchIdx) { matchItem = item; break }
        cumLen += item.str.length
      }
      if (!matchItem) continue

      const viewport = page.getViewport({ scale: this.zoom })

      // The match may start part-way through a text item (e.g. the item is
      // "introduction" but the query is "duct"). Compute the character offset of
      // the match within its item, then assume uniform glyph advance to derive
      // the match's horizontal extent in PDF units.
      const offsetInItem = matchIdx - cumLen
      const charWidth = matchItem.str.length > 0 ? matchItem.width / matchItem.str.length : 0
      const matchStartX = (matchItem.transform[4] ?? 0) + offsetInItem * charWidth
      const matchWidth = Math.max(normalizedQuery.length * charWidth, 1)
      const baselineY = matchItem.transform[5] ?? 0

      // Build the match rectangle in PDF coordinates and convert it to viewport
      // coordinates via the viewport's transform (handles zoom, rotation and any
      // page transform) instead of hand-rolling the math. The baseline is the
      // bottom of the glyphs, so the rectangle rises `height` above it.
      // convertToViewportRectangle was removed in pdfjs-dist 6; convert
      // each corner individually via convertToViewportPoint.
      const [rx1, ry1] = viewport.convertToViewportPoint(matchStartX, baselineY - matchItem.height)
      const [rx2, ry2] = viewport.convertToViewportPoint(matchStartX + matchWidth, baselineY)
      const rect = [rx1, ry1, rx2, ry2]
      const hx = Math.min(rect[0], rect[2])
      const hy = Math.min(rect[1], rect[3])
      const hw = Math.abs(rect[2] - rect[0])
      const hh = Math.max(Math.abs(rect[3] - rect[1]), (matchItem.height || 10) * this.zoom)

      // Highlight overlay on the page element
      const pageEl = this.container.querySelector<HTMLElement>(`.pdf-page[data-page="${pageNum}"]`)
      if (!pageEl) continue

      const highlight = document.createElement("div")
      highlight.className = "pdf-search-highlight"
      highlight.style.left = `${hx - 2}px`
      highlight.style.top = `${hy - 2}px`
      highlight.style.width = `${hw + 4}px`
      highlight.style.height = `${hh + 4}px`
      pageEl.append(highlight)
      this.highlightEl = highlight

      // Fade out after 3s and remove after 5s. Track the timers so a new search
      // (or teardown) can cancel them instead of orphaning them.
      this.highlightTimers.push(
        setTimeout(() => { highlight.style.opacity = "0" }, 3000),
        setTimeout(() => { highlight.remove(); if (this.highlightEl === highlight) this.highlightEl = null }, 5000),
      )

      // Scroll the match into view within the .pdf-viewer container only (don't
      // touch ancestor scroll containers). Land the match ~40% from the top.
      const pageRect = pageEl.getBoundingClientRect()
      const containerRect = this.container.getBoundingClientRect()
      const pageTopInContent = pageRect.top - containerRect.top + this.container.scrollTop
      this.container.scrollTo({ top: Math.max(0, pageTopInContent + hy - this.container.clientHeight * 0.4), behavior: "smooth" })

      return true
    }
    return false
  }
}
