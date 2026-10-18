import { beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import { EditorState } from "@codemirror/state"
import type { EditorView } from "@codemirror/view"
import type { LoroDoc } from "loro-crdt"
import type { VirtualPdfViewer } from "./pdf-viewer"
import type { CompileHost, CompileWorkspace } from "./compile"
import { compileCurrent, compileForDiagnostics, downloadPreview, initCompile, markPreviewStale, resetBuildSummary } from "./compile"
import * as api from "./api"
import { downloadBase64 } from "./effects"
import { browserCompiler, BrowserCompileUnavailable } from "./browser-compile"

vi.mock("./api", async (original) => ({ ...await original<typeof api>(), previewProject: vi.fn(), preparePreview: vi.fn() }))
vi.mock("./browser-compile", () => ({
  browserCompiler: { available: vi.fn(), compile: vi.fn() },
  BrowserCompileUnavailable: class extends Error {}
}))
vi.mock("./effects", () => ({ decodeBase64Pdf: () => new Uint8Array([1]), downloadBase64: vi.fn() }))

describe("project preview", () => {
  let state: CompileWorkspace
  let jobs: Promise<unknown>[]
  const result = (pdf = "first-pdf"): api.ProjectPreview => ({
    entry: "main.typ", view: "proposed", compile: {
      pdf_base64: pdf, diagnostics: [], span_map: [], outline: [], build_id: pdf
    }
  })
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(browserCompiler.available).mockReturnValue(false)
    jobs = []
    document.body.innerHTML = '<button id="compile-button"></button><button id="download-preview"></button><div id="build-label"></div><div id="build-health"></div><div id="pdf-viewer"></div>'
    state = {
      project: { id: "p", name: "Paper", created_at: "", updated_at: "", entry_document_id: "main" },
      selected: { id: "chapter", project_id: "p", path: "chapters/results.typ", title: "Results", body: "draft", data: {}, revision: 0, updated_at: "" },
      view: "proposed", review: { items: [], suggesting: false, capability: "available" }, diagnostics: []
    }
    const run: CompileHost["run"] = (effect, success, failure) => {
      jobs.push(Effect.runPromise(effect).then(success, failure))
    }
    initCompile({
      state, run,
      editor: { state: EditorState.create({ doc: "Current chapter" }) } as EditorView,
      pdfViewer: { load: async () => {}, pageCount: 1 } as unknown as VirtualPdfViewer,
      getActiveLoro: () => ({} as LoroDoc),
      el: (selector) => document.querySelector(selector),
      setText: (selector, text) => { const node = document.querySelector(selector); if (node) node.textContent = text },
      status: vi.fn(), timeAgo: () => "just now", renderDiagnostics: vi.fn(), logBuild: vi.fn(), setDrawerOpen: vi.fn(), updateZoomLabel: vi.fn(), renderPagePosition: vi.fn()
    })
    resetBuildSummary()
    vi.mocked(api.previewProject).mockReturnValue(Effect.succeed(result()))
  })

  it("previews the project with the open chapter as a draft", async () => {
    compileCurrent()
    await Promise.all(jobs)
    expect(api.previewProject).toHaveBeenCalledWith("p", "proposed", { document_id: "chapter", body: "Current chapter", marks: [] })
    expect(document.querySelector("#build-label")?.textContent).toContain("main.typ")
  })

  it("uses the browser compiler for the prepared whole project", async () => {
    vi.mocked(browserCompiler.available).mockReturnValue(true)
    const input: api.PreviewInputs = { project_id: "p", entry: "main.typ", view: "redline", sources: {
      "main.typ": '#include "chapters/results.typ"', "chapters/results.typ": "projected draft", "review.typ": "review support"
    } }
    state.view = "redline"
    vi.mocked(api.preparePreview).mockReturnValue(Effect.succeed(input))
    vi.mocked(browserCompiler.compile).mockResolvedValue(result("browser-pdf").compile)
    compileCurrent()
    await Promise.all(jobs)
    expect(api.preparePreview).toHaveBeenCalledWith("p", "redline", { document_id: "chapter", body: "Current chapter", marks: [] })
    expect(browserCompiler.compile).toHaveBeenCalledWith(input)
    expect(api.previewProject).not.toHaveBeenCalled()
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("browser-pdf", "Paper.pdf", "application/pdf")
    expect(document.querySelector("#build-label")?.textContent).toContain("All markup")
  })

  it("falls back with the same draft if the browser compiler cannot start", async () => {
    vi.mocked(browserCompiler.available).mockReturnValue(true)
    vi.mocked(api.preparePreview).mockReturnValue(Effect.succeed({ project_id: "p", entry: "main.typ", view: "proposed", sources: {} }))
    vi.mocked(browserCompiler.compile).mockRejectedValue(new BrowserCompileUnavailable("worker failed"))
    compileCurrent()
    await Promise.all(jobs)
    expect(api.previewProject).toHaveBeenCalledWith("p", "proposed", { document_id: "chapter", body: "Current chapter", marks: [] })
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("first-pdf", "Paper.pdf", "application/pdf")
  })

  it("reports compiler errors without running another compiler", async () => {
    vi.mocked(browserCompiler.available).mockReturnValue(true)
    vi.mocked(api.preparePreview).mockReturnValue(Effect.succeed({ project_id: "p", entry: "main.typ", view: "proposed", sources: {} }))
    vi.mocked(browserCompiler.compile).mockRejectedValue(new Error("invalid compiler input"))
    compileCurrent()
    await Promise.all(jobs)
    expect(api.previewProject).not.toHaveBeenCalled()
    downloadPreview()
    expect(downloadBase64).not.toHaveBeenCalled()
  })

  it("downloads the displayed bytes after further edits, without compiling again", async () => {
    compileCurrent()
    await Promise.all(jobs)
    markPreviewStale()
    state.view = "baseline"
    downloadPreview()
    expect(api.previewProject).toHaveBeenCalledTimes(1)
    expect(downloadBase64).toHaveBeenCalledWith("first-pdf", "Paper.pdf", "application/pdf")
    expect(document.querySelector("#build-label")?.textContent).toContain("Final")
    expect(document.querySelector("#build-label")?.textContent).toContain("Edited since preview")
  })

  it("finishes applying a result before starting a queued build", async () => {
    let complete!: (result: api.ProjectPreview) => void
    vi.mocked(api.previewProject).mockReturnValueOnce(Effect.tryPromise({ try: () => new Promise<api.ProjectPreview>((resolve) => { complete = resolve }), catch: () => new Error("failed") as never }))
    vi.mocked(api.previewProject).mockReturnValueOnce(Effect.succeed(result("second-pdf")))
    compileCurrent()
    await Promise.resolve()
    compileCurrent()
    complete(result())
    await jobs[0]
    await Promise.all(jobs)
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("second-pdf", "Paper.pdf", "application/pdf")
  })

  it("keeps the last downloadable PDF when a manual update has errors", async () => {
    compileCurrent()
    await Promise.all(jobs)
    markPreviewStale()
    const failed = result("invalid-pdf")
    vi.mocked(api.previewProject).mockReturnValue(Effect.succeed({ ...failed, compile: {
      ...failed.compile, diagnostics: [{ severity: "error", message: "unknown variable" }]
    } }))
    compileCurrent()
    await Promise.all(jobs)
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("first-pdf", "Paper.pdf", "application/pdf")
    expect((document.querySelector("#download-preview") as HTMLButtonElement).disabled).toBe(false)
  })

  it("applies a project build after the author switches chapters", async () => {
    compileCurrent()
    state.selected = { ...state.selected!, id: "other", path: "other.typ" }
    await Promise.all(jobs)
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("first-pdf", "Paper.pdf", "application/pdf")
  })

  it("discards a result after leaving and reopening the project", async () => {
    compileCurrent()
    resetBuildSummary()
    await Promise.all(jobs)
    downloadPreview()
    expect(downloadBase64).not.toHaveBeenCalled()
  })

  it("runs a background update queued during an earlier build", async () => {
    compileCurrent()
    vi.mocked(api.previewProject).mockReturnValue(Effect.succeed(result("latest-pdf")))
    markPreviewStale()
    compileForDiagnostics()
    await jobs[0]
    await Promise.all(jobs)
    downloadPreview()
    expect(downloadBase64).toHaveBeenCalledWith("latest-pdf", "Paper.pdf", "application/pdf")
  })

})
