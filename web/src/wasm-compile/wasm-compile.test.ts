/**
 * Tests for the in-browser compile path (issue #20 stage 2c): the toggle, the
 * pipeline ports (each mirrors the app-service Rust test cases it must stay
 * 1:1 with), the worker host's lifecycle, and the dispatcher's fallback.
 *
 * Everything runs without any real wasm: jsdom has no Worker and the
 * artifacts are never built here, which is exactly the environment the
 * graceful-degradation path has to survive. The host and dispatcher seams
 * (createWorker, present, path) exist for this file.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import * as api from "../api"
import { createWasmCompile } from "./index"
import { WasmCompileHost, WasmCompileUnavailableError } from "./host"
import { buildWasmBoundaryRequest, markdownHeadingsToTypst, toClientCompileResponse, type WasmCompileJob } from "./pipeline"
import { compilePath, setCompilePath } from "./toggle"

// ---------------------------------------------------------------------------
// Helpers: a fake Worker the host can boot, reply to, and crash
// ---------------------------------------------------------------------------

/** The host's Worker surface: message events in, postMessage out. */
class FakeWorker extends EventTarget {
  /** Every request the main thread posted, in order. */
  readonly posted: unknown[] = []
  postMessage(message: unknown): void {
    this.posted.push(message)
  }
  /** The worker → main direction: a `ready`/`boot-failed`/`compiled`/`failed`. */
  emit(message: unknown): void {
    this.dispatchEvent(new MessageEvent("message", { data: message }))
  }
  /** A worker that died (uncaught error / OOM): the `error` event. */
  crash(): void {
    this.dispatchEvent(new Event("error"))
  }
}

const asWorker = (worker: FakeWorker): Worker => worker as unknown as Worker

/** The client compile contract's minimal valid body (api.ts CompileResponse). */
const okResponse = (buildId = "b1"): api.CompileResponse => ({
  pdf_base64: "JVBERi0xLjQ=",
  span_map: [],
  diagnostics: [],
  outline: [],
  build_id: buildId
})

const okFetch = (body: unknown): Response =>
  new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } })

const job: WasmCompileJob = {
  project_id: "p1",
  entry: "main.typ",
  sources: { "main.typ": "= Hello" },
  marks: { "main.typ": [] },
  view: "proposed",
  references: []
}

/** A posted request, once it exists (waitFor has already confirmed it). */
function requestAt(worker: FakeWorker, index = 0): { id: number } {
  const request = worker.posted[index] as { id: number } | undefined
  if (request === undefined) throw new Error(`no request posted at ${index}`)
  return request
}

/** An in-memory Storage: the test environment's `localStorage` global is
 *  genuinely undefined (Node without --localstorage-file), which is itself
 *  the degraded case the toggle must survive — so the tests stub a working
 *  storage instead of assuming one exists. */
function memoryStorage(): Storage {
  const map = new Map<string, string>()
  return {
    get length(): number {
      return map.size
    },
    clear: () => map.clear(),
    getItem: (key: string) => map.get(key) ?? null,
    key: (index: number) => [...map.keys()][index] ?? null,
    removeItem: (key: string) => map.delete(key),
    setItem: (key: string, value: string) => map.set(key, value)
  } as Storage
}

beforeEach(() => {
  vi.unstubAllGlobals()
  vi.stubGlobal("localStorage", memoryStorage())
  // jsdom has no Worker constructor at all; the host tests inject their own
  // factories but `available()` still probes the global, so give it one.
  vi.stubGlobal("Worker", FakeWorker)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

// ---------------------------------------------------------------------------
// The toggle
// ---------------------------------------------------------------------------

describe("compile path toggle", () => {
  it("defaults to the server path", () => {
    expect(compilePath()).toBe("server")
  })

  it("opts in to the wasm path through localStorage", () => {
    setCompilePath("wasm")
    expect(compilePath()).toBe("wasm")
    setCompilePath("server")
    expect(compilePath()).toBe("server")
  })

  it("treats any other stored value as the server path (fail-safe direction)", () => {
    localStorage.setItem("nisaba.compilePath", "WASM")
    expect(compilePath()).toBe("server")
    localStorage.setItem("nisaba.compilePath", "wasm ")
    expect(compilePath()).toBe("server")
  })

  it("survives a storage that throws (locked-down embedding contexts)", () => {
    vi.stubGlobal("localStorage", {
      get getItem() {
        throw new Error("denied")
      },
      setItem: () => undefined
    })
    expect(compilePath()).toBe("server")
    expect(() => setCompilePath("wasm")).not.toThrow()
  })
})

// ---------------------------------------------------------------------------
// The pipeline: markdown heading conversion (mirrors the app's
// compile_proxy_converts_markdown_headings_like_export and the Rust helper's
// own edge cases — keep 1:1 with services/app markdown_headings_to_typst)
// ---------------------------------------------------------------------------

describe("markdownHeadingsToTypst", () => {
  it("converts markdown headings to Typst headings (the app's test case)", () => {
    expect(markdownHeadingsToTypst("# Hello\n### Sub")).toBe("= Hello\n=== Sub")
  })

  it("converts all six levels", () => {
    expect(markdownHeadingsToTypst("###### Deep")).toBe("====== Deep")
  })

  it("preserves leading spaces", () => {
    expect(markdownHeadingsToTypst("  ## Nested")).toBe("  == Nested")
  })

  it("leaves Typst code syntax alone (no space after the hash run)", () => {
    expect(markdownHeadingsToTypst("#figure(image)\n#emph[hi]")).toBe("#figure(image)\n#emph[hi]")
  })

  it("leaves seven hashes alone (markdown caps at six)", () => {
    expect(markdownHeadingsToTypst("####### seven")).toBe("####### seven")
  })

  it("applies Rust str::lines semantics: CRLF stripped, trailing newline dropped", () => {
    expect(markdownHeadingsToTypst("# One\r\n## Two\r\n")).toBe("= One\n== Two")
  })
})

// ---------------------------------------------------------------------------
// The pipeline: request building and response mapping
// ---------------------------------------------------------------------------

/** Records what the wasm halves were asked; returns canned answers. */
function recordingDeps(answers: { projected?: string; yaml?: string } = {}) {
  const calls: { source: string; marks: string; view: string }[] = []
  const yamlCalls: string[] = []
  return {
    deps: {
      projectSource: (source: string, marks: string, view: string): string => {
        calls.push({ source, marks, view })
        return answers.projected ?? source
      },
      bibliographyYaml: (references: string): string => {
        yamlCalls.push(references)
        return answers.yaml ?? ""
      }
    },
    calls,
    yamlCalls
  }
}

describe("buildWasmBoundaryRequest", () => {
  it("projects every source with its own marks and converts headings (the app's api_compile order)", () => {
    const { deps, calls } = recordingDeps()
    const request = buildWasmBoundaryRequest(
      {
        ...job,
        sources: { "main.typ": "# Hello", "ch/intro.typ": "### Sub" },
        marks: { "main.typ": [{ start: 0, end: 1, kind: "insert", author: "a", timestamp: 1 }], "ch/intro.typ": [] }
      },
      deps
    )
    expect(calls).toEqual([
      { source: "# Hello", marks: JSON.stringify([{ start: 0, end: 1, kind: "insert", author: "a", timestamp: 1 }]), view: "proposed" },
      { source: "### Sub", marks: "[]", view: "proposed" }
    ])
    expect(request.sources["main.typ"]).toBe("= Hello")
    expect(request.sources["ch/intro.typ"]).toBe("=== Sub")
  })

  it("forwards the projection inputs and never the marks", () => {
    const { deps } = recordingDeps()
    const request = buildWasmBoundaryRequest({ ...job, view: "redline" }, deps)
    expect(request).toEqual({
      project_id: "p1",
      entry: "main.typ",
      sources: { "main.typ": "= Hello" },
      view: "redline"
    })
    expect("marks" in request).toBe(false)
  })

  it("injects the bibliography beside the entry and cites it (refs.yml)", () => {
    const { deps } = recordingDeps({ yaml: "title: X\n" })
    const request = buildWasmBoundaryRequest(job, deps)
    expect(request.sources["refs.yml"]).toBe("title: X\n")
    expect(request.sources["main.typ"]).toBe('= Hello\n#bibliography("refs.yml")\n')
  })

  it("injects the bibliography relative to a nested entry", () => {
    const { deps } = recordingDeps({ yaml: "title: X\n" })
    const request = buildWasmBoundaryRequest(
      { ...job, entry: "chapters/main.typ", sources: { "chapters/main.typ": "x" } },
      deps
    )
    expect(request.sources["chapters/refs.yml"]).toBe("title: X\n")
    expect(request.sources["chapters/main.typ"]).toBe('x\n#bibliography("refs.yml")\n')
  })

  it("injects nothing when the bibliography renders empty (no references)", () => {
    const { deps, yamlCalls } = recordingDeps({ yaml: "  \n" })
    const request = buildWasmBoundaryRequest(job, deps)
    expect(yamlCalls).toEqual(["[]"])
    expect(Object.keys(request.sources)).toEqual(["main.typ"])
  })

  it("never overwrites an existing refs.yml, and skips the citation then too", () => {
    const { deps } = recordingDeps({ yaml: "title: X\n" })
    const request = buildWasmBoundaryRequest(
      // The trailing newline is gone from BOTH sources: like the Rust
      // original, the heading pass is lines().join("\n"), which does not
      // reproduce a final terminator.
      { ...job, sources: { "main.typ": "x", "refs.yml": "keep: me\n" } },
      deps
    )
    expect(request.sources["refs.yml"]).toBe("keep: me")
    expect(request.sources["main.typ"]).toBe("x")
  })

  it("respects a #bibliography call the entry already makes", () => {
    const { deps } = recordingDeps({ yaml: "title: X\n" })
    const request = buildWasmBoundaryRequest(
      { ...job, sources: { "main.typ": '#bibliography("mine.yml")\n' } },
      deps
    )
    expect(request.sources["main.typ"]).toBe('#bibliography("mine.yml")')
  })

  it("adds review support for the redline view when any source carries markers (the app's multi-file rule)", () => {
    const { deps } = recordingDeps()
    const request = buildWasmBoundaryRequest(
      {
        ...job,
        entry: "chapters/main.typ",
        view: "redline",
        sources: { "chapters/main.typ": "#include \"intro.typ\"", "chapters/intro.typ": "#review.add[more]" }
      },
      deps
    )
    expect(request.sources["chapters/review.typ"]).toContain("#let add")
    expect(request.sources["chapters/main.typ"]).toBe('#import "review.typ" as review\n\n#include "intro.typ"')
  })

  it("does not add review support for other views, or without markers", () => {
    const { deps } = recordingDeps()
    const withMarkers = { ...job, view: "proposed" as const, sources: { "main.typ": "#review.del[gone]" } }
    expect(buildWasmBoundaryRequest(withMarkers, deps).sources["review.typ"]).toBeUndefined()
    const redlineClean = { ...job, view: "redline" as const }
    expect(buildWasmBoundaryRequest(redlineClean, deps).sources["review.typ"]).toBeUndefined()
  })

  it("never overwrites an existing review.typ or re-imports it", () => {
    const { deps } = recordingDeps()
    const request = buildWasmBoundaryRequest(
      { ...job, view: "redline", sources: { "main.typ": '#import "review.typ" as review\n#review.add[x]', "review.typ": "#let add = it => it" } },
      deps
    )
    expect(request.sources["review.typ"]).toBe("#let add = it => it")
    expect(request.sources["main.typ"]).toBe('#import "review.typ" as review\n#review.add[x]')
  })
})

describe("toClientCompileResponse", () => {
  it("maps the boundary's service shape onto the client contract", () => {
    expect(
      toClientCompileResponse({
        pdf: "JVBERi0xLjQ=",
        span_map: [1],
        diagnostics: [2],
        outline: [3],
        build_id: "b7",
        instrumentation: { compile_ms: 0, pdf_ms: 0, rss_bytes: null }
      })
    ).toEqual({
      pdf_base64: "JVBERi0xLjQ=",
      span_map: [1],
      diagnostics: [2],
      outline: [3],
      build_id: "b7"
    })
  })
})

// ---------------------------------------------------------------------------
// The worker host (jsdom has no Worker, so one is stubbed in)
// ---------------------------------------------------------------------------

/** A host whose environment says wasm could work: Worker API + artifacts. */
function capableHost(overrides: { createWorker?: () => Worker; responseTimeoutMs?: number } = {}): WasmCompileHost {
  return new WasmCompileHost({
    present: () => true,
    createWorker: overrides.createWorker ?? (() => asWorker(new FakeWorker())),
    responseTimeoutMs: overrides.responseTimeoutMs
  })
}

/**
 * Boots a host on the given worker. The `ready` must be emitted while the
 * boot promise is pending, and the worker only exists once start() has run
 * its executor — this helper sequences the two correctly.
 */
async function bootedHost(worker: FakeWorker, responseTimeoutMs?: number): Promise<WasmCompileHost> {
  const host = capableHost({ createWorker: () => asWorker(worker), responseTimeoutMs })
  const boot = host.start()
  worker.emit({ type: "ready", compileVersion: "0.1.0", coreVersion: "0.1.0" })
  await boot
  return host
}

/** A request's id, once it has been posted. host.compile awaits the (already
 *  resolved) boot before posting, so the post lands a microtask later. */
async function postedId(worker: FakeWorker, index = 0): Promise<number> {
  await vi.waitFor(() => expect(worker.posted.length).toBeGreaterThan(index))
  return requestAt(worker, index).id
}

describe("WasmCompileHost", () => {
  it("reports the artifacts as missing when they were not built", () => {
    const host = new WasmCompileHost({ present: () => false, createWorker: () => asWorker(new FakeWorker()) })
    expect(host.available()).toBe(false)
    expect(host.unavailabilityReason()).toContain("just wasm-web")
  })

  it("reports the missing Worker API as unavailable (the jsdom reality)", () => {
    vi.stubGlobal("Worker", undefined)
    const host = new WasmCompileHost({ present: () => true, createWorker: () => asWorker(new FakeWorker()) })
    expect(host.available()).toBe(false)
    expect(host.unavailabilityReason()).toContain("Web Worker")
  })

  it("boots once, reports the crate versions, and serves compiles in order", async () => {
    const worker = new FakeWorker()
    const host = capableHost({ createWorker: () => asWorker(worker) })
    const boot = host.start()
    worker.emit({ type: "ready", compileVersion: "0.1.0", coreVersion: "0.1.0" })
    await expect(boot).resolves.toEqual({ compileVersion: "0.1.0", coreVersion: "0.1.0" })
    expect(host.available()).toBe(true)
    expect(host.unavailabilityReason()).toBeUndefined()

    const first = host.compile(job)
    const second = host.compile(job)
    await vi.waitFor(() => expect(worker.posted).toHaveLength(2))
    expect((worker.posted as { type: string }[]).map((request) => request.type)).toEqual(["compile", "compile"])
    worker.emit({ type: "compiled", id: requestAt(worker, 0).id, response: okResponse("one") })
    worker.emit({ type: "compiled", id: requestAt(worker, 1).id, response: okResponse("two") })
    await expect(first).resolves.toMatchObject({ build_id: "one" })
    await expect(second).resolves.toMatchObject({ build_id: "two" })
  })

  it("surfaces a compile failure with the core's message strings and stays usable", async () => {
    const worker = new FakeWorker()
    const host = await bootedHost(worker)
    const failing = host.compile(job)
    const id = await postedId(worker)
    worker.emit({ type: "failed", id, message: "unknown mark kind: strike" })
    await expect(failing).rejects.toThrow("unknown mark kind: strike")
    expect(host.available()).toBe(true)
  })

  it("rejects a response that does not fit the client contract", async () => {
    const worker = new FakeWorker()
    const host = await bootedHost(worker)
    const compile = host.compile(job)
    const id = await postedId(worker)
    worker.emit({ type: "compiled", id, response: { nope: true } })
    await expect(compile).rejects.toThrow("unexpected response")
  })

  it("ignores replies for abandoned ids (a timed-out compile)", async () => {
    vi.useFakeTimers()
    const worker = new FakeWorker()
    const host = await bootedHost(worker, 10)
    const timedOut = host.compile(job)
    // Attach the assertion before advancing the clock, so the rejection is
    // never momentarily unhandled when the fake timer fires.
    const expectation = expect(timedOut).rejects.toThrow("did not finish within")
    await vi.advanceTimersByTimeAsync(10) // the response timeout fires
    await expectation
    // The late reply for the abandoned id must not crash or resolve anything.
    worker.emit({ type: "compiled", id: 1, response: okResponse() })
    const next = host.compile(job)
    await vi.advanceTimersByTimeAsync(1) // the post lands, well short of the timeout
    expect(worker.posted).toHaveLength(2)
    worker.emit({ type: "compiled", id: requestAt(worker, 1).id, response: okResponse("next") })
    await expect(next).resolves.toMatchObject({ build_id: "next" })
  })

  it("falls back for the session when boot fails, with the worker's reason", async () => {
    const worker = new FakeWorker()
    const host = capableHost({ createWorker: () => asWorker(worker) })
    const boot = host.start()
    worker.emit({ type: "boot-failed", message: "the artifacts are not built" })
    await expect(boot).rejects.toBeInstanceOf(WasmCompileUnavailableError)
    expect(host.available()).toBe(false)
    expect(host.unavailabilityReason()).toBe("the artifacts are not built")
    await expect(host.compile(job)).rejects.toBeInstanceOf(WasmCompileUnavailableError)
  })

  it("treats a crashed worker as unavailable and fails in-flight compiles as unavailable", async () => {
    const worker = new FakeWorker()
    const host = await bootedHost(worker)
    const inFlight = host.compile(job)
    await postedId(worker)
    worker.crash()
    await expect(inFlight).rejects.toBeInstanceOf(WasmCompileUnavailableError)
    expect(host.available()).toBe(false)
    expect(host.unavailabilityReason()).toContain("stopped unexpectedly")
    // A compile dispatched after the crash fails fast as unavailable (the
    // dispatcher falls back) instead of posting into the dead worker.
    await expect(host.compile(job)).rejects.toBeInstanceOf(WasmCompileUnavailableError)
    expect(worker.posted).toHaveLength(1)
  })

  it("marks itself unavailable when the worker cannot even be created", async () => {
    const host = capableHost({
      createWorker: () => {
        throw new Error("quota exceeded")
      }
    })
    await expect(host.start()).rejects.toBeInstanceOf(WasmCompileUnavailableError)
    expect(host.available()).toBe(false)
  })
})

// ---------------------------------------------------------------------------
// The dispatcher: engine choice and server fallback
// ---------------------------------------------------------------------------

describe("createWasmCompile", () => {
  const input = {
    projectId: "p1",
    entry: "main.typ",
    sources: { "main.typ": "= Hello" } as Readonly<Record<string, string>>,
    marks: { "main.typ": [] } as Readonly<Record<string, readonly api.MarkInput[]>>,
    view: "proposed" as const
  }

  it("serves from the server by default and never reports a fallback", async () => {
    const fetchMock = vi.fn(async () => okFetch(okResponse("s1")))
    vi.stubGlobal("fetch", fetchMock)
    vi.stubGlobal("Worker", FakeWorker)
    const engine = createWasmCompile({ path: () => "server" })
    expect(engine.fallbackReason()).toBeUndefined()
    const response = await Effect.runPromise(engine.compile(input, []))
    expect(response.build_id).toBe("s1")
    expect(fetchMock).toHaveBeenCalledWith("/api/compile", expect.anything())
    expect(engine.lastServedBy()).toBe("server")
  })

  it("explains itself and falls back when the artifacts are not built", async () => {
    const fetchMock = vi.fn(async () => okFetch(okResponse("s1")))
    vi.stubGlobal("fetch", fetchMock)
    vi.stubGlobal("Worker", FakeWorker)
    const engine = createWasmCompile({ path: () => "wasm", present: () => false })
    expect(engine.fallbackReason()).toContain("just wasm-web")
    const response = await Effect.runPromise(engine.compile(input, []))
    expect(response.build_id).toBe("s1")
    expect(fetchMock).toHaveBeenCalledWith("/api/compile", expect.anything())
    expect(engine.lastServedBy()).toBe("server")
  })

  it("runs the job in the worker when the wasm engine is up", async () => {
    const fetchMock = vi.fn(async () => okFetch(okResponse()))
    vi.stubGlobal("fetch", fetchMock)
    vi.stubGlobal("Worker", FakeWorker)
    const worker = new FakeWorker()
    const engine = createWasmCompile({ path: () => "wasm", present: () => true, createWorker: () => asWorker(worker) })
    const reference = {
      id: "r1",
      project_id: "p1",
      metadata: { title: "T", authors: [], year: null, doi: null, pmid: null, journal: null, extra: {} },
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z"
    }
    const compile = Effect.runPromise(engine.compile(input, [reference]))
    // The worker exists after the effect starts; ready releases the boot the
    // compile is waiting on, then the job is posted.
    worker.emit({ type: "ready", compileVersion: "0", coreVersion: "0" })
    await vi.waitFor(() => expect(worker.posted).toHaveLength(1))
    const request = requestAt(worker) as unknown as {
      type: string
      id: number
      job: { project_id: string; references: { id?: string }[] }
    }
    expect(request.type).toBe("compile")
    expect(request.job.project_id).toBe("p1")
    expect(request.job.references[0]?.id).toBe("r1")
    worker.emit({ type: "compiled", id: request.id, response: okResponse("w1") })
    await expect(compile).resolves.toMatchObject({ build_id: "w1" })
    expect(fetchMock).not.toHaveBeenCalled()
    expect(engine.lastServedBy()).toBe("wasm")
  })

  it("falls back to the server when the worker cannot boot, and reports why", async () => {
    const fetchMock = vi.fn(async () => okFetch(okResponse("s1")))
    vi.stubGlobal("fetch", fetchMock)
    vi.stubGlobal("Worker", FakeWorker)
    const engine = createWasmCompile({
      path: () => "wasm",
      present: () => true,
      createWorker: () => {
        throw new Error("no workers left")
      }
    })
    const response = await Effect.runPromise(engine.compile(input, []))
    expect(response.build_id).toBe("s1")
    expect(fetchMock).toHaveBeenCalledWith("/api/compile", expect.anything())
    expect(engine.lastServedBy()).toBe("server")
    expect(engine.fallbackReason()).toContain("no workers left")
  })

  it("surfaces a real compile failure instead of papering over it", async () => {
    vi.stubGlobal("Worker", FakeWorker)
    const worker = new FakeWorker()
    const engine = createWasmCompile({ path: () => "wasm", present: () => true, createWorker: () => asWorker(worker) })
    const compile = Effect.runPromise(engine.compile(input, []))
    worker.emit({ type: "ready", compileVersion: "0", coreVersion: "0" })
    await vi.waitFor(() => expect(worker.posted).toHaveLength(1))
    const request = worker.posted[0] as { id: number }
    worker.emit({ type: "failed", id: request.id, message: "unknown mark kind: note" })
    await expect(compile).rejects.toThrow("unknown mark kind: note")
    expect(engine.lastServedBy()).toBe("wasm")
  })

  it("prefetch boots the worker only when the wasm path is selected", async () => {
    vi.useFakeTimers()
    vi.stubGlobal("Worker", FakeWorker)
    const created: FakeWorker[] = []
    const factory = (): Worker => {
      const worker = new FakeWorker()
      created.push(worker)
      return asWorker(worker)
    }
    createWasmCompile({ path: () => "server", present: () => true, createWorker: factory }).prefetch()
    await vi.advanceTimersByTimeAsync(5000)
    expect(created).toHaveLength(0)
    createWasmCompile({ path: () => "wasm", present: () => true, createWorker: factory }).prefetch()
    await vi.advanceTimersByTimeAsync(5000)
    expect(created).toHaveLength(1)
  })
})
