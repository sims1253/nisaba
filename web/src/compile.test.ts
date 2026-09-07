import { beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import { EditorState } from "@codemirror/state"
import type { EditorView } from "@codemirror/view"
import type { LoroDoc } from "loro-crdt"
import type { VirtualPdfViewer } from "./pdf-viewer"
import type { CompileHost, CompileWorkspace } from "./compile"
import { compileCurrent, downloadPreview, initCompile, markPreviewStale, resetBuildSummary } from "./compile"
import * as api from "./api"
import { downloadBase64 } from "./effects"

vi.mock("./api", () => ({ previewProject: vi.fn() }))
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
})
