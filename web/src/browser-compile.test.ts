import { afterEach, describe, expect, it, vi } from "vitest"
import { BrowserCompiler, BrowserCompileUnavailable } from "./browser-compile"

const input = { project_id: "p", entry: "main.typ", sources: { "main.typ": "Hello" }, view: "proposed" as const }
const response = { pdf_base64: "pdf", span_map: [], diagnostics: [], outline: [], build_id: "b" }

function setup(timeoutMs = 120_000) {
  const worker = { postMessage: vi.fn(), terminate: vi.fn(), onmessage: null, onerror: null, onmessageerror: null } as unknown as Worker
  const createWorker = vi.fn(() => worker)
  const compiler = new BrowserCompiler({ present: () => true, createWorker, timeoutMs })
  const reply = (data: unknown): void => worker.onmessage?.(new MessageEvent("message", { data }))
  return { compiler, worker, createWorker, reply }
}

afterEach(() => vi.useRealTimers())

describe("browser compiler worker", () => {
  it("correlates replies and reuses the worker for warm compiles", async () => {
    const { compiler, worker, createWorker, reply } = setup()
    const first = compiler.compile(input)
    const second = compiler.compile(input)
    expect(worker.postMessage).toHaveBeenCalledWith({ id: 1, input })
    reply({ id: 2, response: { ...response, build_id: "second" } })
    reply({ id: 1, response })
    expect(await first).toEqual(response)
    expect((await second).build_id).toBe("second")
    expect(createWorker).toHaveBeenCalledTimes(1)
  })

  it("disables a worker after artifact initialization fails", async () => {
    const { compiler, worker, reply } = setup()
    const job = compiler.compile(input)
    reply({ id: 1, unavailable: true, error: "WASM could not load" })
    await expect(job).rejects.toBeInstanceOf(BrowserCompileUnavailable)
    expect(worker.terminate).toHaveBeenCalled()
    expect(compiler.available()).toBe(false)
  })

  it("terminates a crashed worker and rejects its outstanding jobs", async () => {
    const { compiler, worker } = setup()
    const job = compiler.compile(input)
    worker.onerror?.(new ErrorEvent("error", { message: "worker crashed" }))
    await expect(job).rejects.toBeInstanceOf(BrowserCompileUnavailable)
    expect(worker.terminate).toHaveBeenCalled()
  })

  it("terminates a hung compiler so the UI can fall back", async () => {
    vi.useFakeTimers()
    const { compiler, worker } = setup(10)
    const job = compiler.compile(input)
    const rejected = expect(job).rejects.toBeInstanceOf(BrowserCompileUnavailable)
    await vi.advanceTimersByTimeAsync(10)
    await rejected
    expect(worker.terminate).toHaveBeenCalled()
  })

  it("keeps compiler failures distinct from environment failures", async () => {
    const { compiler, reply } = setup()
    const job = compiler.compile(input)
    reply({ id: 1, error: "invalid source path" })
    await expect(job).rejects.toThrow("invalid source path")
    expect(compiler.available()).toBe(true)
  })

  it("rejects a response that does not match the shared API schema", async () => {
    const { compiler, reply } = setup()
    const job = compiler.compile(input)
    reply({ id: 1, response: { pdf_base64: 123 } })
    await expect(job).rejects.toThrow()
  })

  it("does not start a worker when artifacts are absent", async () => {
    const createWorker = vi.fn()
    const compiler = new BrowserCompiler({ present: () => false, createWorker })
    await expect(compiler.compile(input)).rejects.toBeInstanceOf(BrowserCompileUnavailable)
    expect(createWorker).not.toHaveBeenCalled()
  })

  it("falls back when worker creation is blocked", async () => {
    const compiler = new BrowserCompiler({ present: () => true, createWorker: () => { throw new Error("workers blocked") } })
    await expect(compiler.compile(input)).rejects.toBeInstanceOf(BrowserCompileUnavailable)
    expect(compiler.available()).toBe(false)
  })
})
