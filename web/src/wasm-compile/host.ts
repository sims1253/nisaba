/**
 * Main-thread host for the in-browser compile worker.
 *
 * Owns the one Worker per tab (lazy: nothing is created — and nothing of the
 * multi-megabyte artifact is fetched — until the first wasm-path compile or
 * the idle prefetch), correlates requests to replies, decodes responses
 * through the same schema the server path uses, and defines what counts as
 * "unavailable" (missing artifacts, no Worker API, boot failure, worker
 * crash) versus a real compile failure. Unavailability is sticky for the
 * session: the dispatcher then routes compiles to the server, because an
 * environment that cannot boot the worker will not boot it on the next try
 * either, and a crashed worker's warm caches cannot be revived.
 */
import { Schema } from "effect"
import * as api from "../api"
import { wasmCompileArtifactsPresent } from "./artifact"
import type { WasmCompileJob } from "./pipeline"
import type { CompileWorkerMessage, CompileWorkerRequest } from "./protocol"

/**
 * The toggle asked for wasm but the engine could not be brought up (artifact
 * missing, import/instantiation failure, worker crash). Distinct from a
 * compile failure so the dispatcher can silently fall back to the server path
 * instead of surfacing an error the writer cannot act on.
 */
export class WasmCompileUnavailableError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "WasmCompileUnavailableError"
  }
}

/** What the worker reports once its artifacts are live. */
export interface WasmCompileBootInfo {
  readonly compileVersion: string
  readonly coreVersion: string
}

/** Injectable seams; all exist so the correlation and lifecycle logic is
 *  unit-testable in jsdom, which has no Worker and never has the artifacts. */
export interface WasmCompileHostDeps {
  /** Creates the worker. Default: the bundled compile-worker module. */
  readonly createWorker?: () => Worker
  /** The artifacts-present probe. Default: the real glob lookup. */
  readonly present?: () => boolean
  /**
   * How long a compile may run before the host stops waiting. The wasm engine
   * cannot be interrupted (no JS API cancels a running compile), so this only
   * releases the UI — the same role the server path's request timeout plays —
   * and a late worker reply for an already-abandoned id is ignored. Defaults
   * to outwaiting the compile service's own 120 s timeout, like the app's
   * compile client does.
   */
  readonly responseTimeoutMs?: number
}

const DEFAULT_RESPONSE_TIMEOUT_MS = 150_000
/** Generous: real boots download ~18 MB gzipped, and a slow link is not a
 *  failure. This guard exists for a worker that hangs silently. */
const BOOT_TIMEOUT_MS = 120_000

/** Decodes through the client's compile contract — the identical decode the
 *  server path applies, so a wasm/server response drift fails loudly. */
const decodeResponse = (value: unknown): api.CompileResponse =>
  Schema.decodeUnknownSync(api.CompileResponse)(value)

interface PendingCompile {
  readonly resolve: (response: api.CompileResponse) => void
  readonly reject: (error: Error) => void
  readonly timer: ReturnType<typeof setTimeout>
}

export class WasmCompileHost {
  private readonly createWorker: () => Worker
  private readonly present: () => boolean
  private readonly responseTimeoutMs: number
  private worker: Worker | undefined
  private boot: Promise<WasmCompileBootInfo> | undefined
  private unavailableReason: string | undefined
  private nextId = 1
  private readonly pending = new Map<number, PendingCompile>()
  private bootResolve: ((info: WasmCompileBootInfo) => void) | undefined
  private bootReject: ((error: WasmCompileUnavailableError) => void) | undefined
  private bootTimer: ReturnType<typeof setTimeout> | undefined

  constructor(deps: WasmCompileHostDeps = {}) {
    this.createWorker =
      deps.createWorker ?? (() => new Worker(new URL("./compile-worker.ts", import.meta.url), { type: "module" }))
    this.present = deps.present ?? wasmCompileArtifactsPresent
    this.responseTimeoutMs = deps.responseTimeoutMs ?? DEFAULT_RESPONSE_TIMEOUT_MS
  }

  /**
   * Whether the host could ever serve a compile right now: artifacts present,
   * the Worker API available (jsdom and very old browsers have neither), and
   * not marked unavailable. Cheap and synchronous — the dispatcher consults
   * it before every compile.
   */
  available(): boolean {
    return this.unavailabilityReason() === undefined
  }

  /**
   * Why the host cannot serve, computed from the current environment: a
   * sticky boot/crash reason if one was recorded, else the first blocking
   * precondition (no Worker API, artifacts not built). Undefined when the
   * host could serve right now. The dispatcher turns this into its
   * one-per-session fallback log line, so it must read as a standalone
   * explanation (including the `just wasm-web` hint where that is the fix).
   */
  unavailabilityReason(): string | undefined {
    if (this.unavailableReason !== undefined) return this.unavailableReason
    if (typeof Worker === "undefined") return "this browser has no Web Worker support"
    if (!this.present()) return "the wasm compile artifacts are not built (run `just wasm-web`, then reload)"
    return undefined
  }

  /** Boots the worker (once) and resolves when its artifacts are live. */
  start(): Promise<WasmCompileBootInfo> {
    this.boot ??= new Promise<WasmCompileBootInfo>((resolve, reject) => {
      let worker: Worker
      try {
        worker = this.createWorker()
      } catch (error) {
        const message = error instanceof Error ? error.message : "the compile worker could not be created"
        this.markUnavailable(message)
        reject(new WasmCompileUnavailableError(message))
        return
      }
      this.worker = worker
      this.bootResolve = resolve
      this.bootReject = reject
      worker.addEventListener("message", (event: MessageEvent<CompileWorkerMessage>) => {
        this.onMessage(event.data)
      })
      worker.addEventListener("error", () => {
        // Before ready this is a load failure; after, a crash. Either way the
        // worker is gone: fail the boot if pending, release every in-flight
        // compile, and leave the tab on the server path for the session.
        this.failWorker("the in-browser compile worker stopped unexpectedly")
      })
      // Cleared by whichever of ready/boot-failed/error settles the boot
      // first; every one of those paths runs settleBoot exactly once.
      this.bootTimer = setTimeout(() => {
        this.failWorker("the in-browser compile worker did not start in time")
      }, BOOT_TIMEOUT_MS)
    })
    return this.boot
  }

  /**
   * Runs one compile job and resolves with the decoded client response. No
   * queueing is needed: the worker applies jobs in message order (the wasm
   * engine is single-threaded), and the editor's `compiling` guard already
   * prevents overlapping jobs from the UI side.
   *
   * @throws {@link WasmCompileUnavailableError} when the engine cannot boot;
   *         `Error` with the core's message strings when the compile itself
   *         fails.
   */
  async compile(job: WasmCompileJob): Promise<api.CompileResponse> {
    await this.start()
    const worker = this.worker
    if (worker === undefined) {
      throw new WasmCompileUnavailableError(this.unavailableReason ?? "the in-browser compile worker is gone")
    }
    return await new Promise<api.CompileResponse>((resolve, reject) => {
      const id = this.nextId++
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`the in-browser compile did not finish within ${this.responseTimeoutMs / 1000} seconds`))
      }, this.responseTimeoutMs)
      this.pending.set(id, { resolve, reject, timer })
      const request: CompileWorkerRequest = { type: "compile", id, job }
      try {
        worker.postMessage(request)
      } catch (error) {
        clearTimeout(timer)
        this.pending.delete(id)
        reject(new Error(error instanceof Error ? error.message : "the compile worker request could not be sent"))
      }
    })
  }

  private onMessage(message: CompileWorkerMessage): void {
    if (message === null || typeof message !== "object") return
    if (message.type === "ready") {
      clearTimeout(this.bootTimer)
      this.bootResolve?.({ compileVersion: message.compileVersion, coreVersion: message.coreVersion })
      return
    }
    if (message.type === "boot-failed") {
      this.markUnavailable(message.message)
      clearTimeout(this.bootTimer)
      this.bootReject?.(new WasmCompileUnavailableError(message.message))
      return
    }
    const entry = message.type === "compiled" || message.type === "failed" ? this.pending.get(message.id) : undefined
    if (entry === undefined) return
    this.pending.delete(message.id)
    clearTimeout(entry.timer)
    if (message.type === "compiled") {
      try {
        entry.resolve(decodeResponse(message.response))
      } catch {
        entry.reject(new Error("the in-browser compiler returned an unexpected response"))
      }
    } else {
      entry.reject(new Error(message.message))
    }
  }

  /** Marks the host unusable for the rest of the session and fails everything pending. */
  private failWorker(reason: string): void {
    this.markUnavailable(reason)
    // The worker is gone; clearing it makes a compile dispatched after the
    // crash fail fast as unavailable (the dispatcher falls back to the
    // server) instead of posting into a dead worker and waiting out the
    // response timeout.
    this.worker = undefined
    clearTimeout(this.bootTimer)
    this.bootReject?.(new WasmCompileUnavailableError(reason))
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer)
      // Unavailable, not a compile failure: the dispatcher falls these back
      // to the server path, so a worker that dies mid-compile costs the
      // writer one transparent retry rather than a hard error.
      entry.reject(new WasmCompileUnavailableError(reason))
    }
    this.pending.clear()
  }

  private markUnavailable(reason: string): void {
    if (this.unavailableReason === undefined) this.unavailableReason = reason
  }
}
