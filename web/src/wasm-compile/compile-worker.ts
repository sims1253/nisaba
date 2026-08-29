/**
 * The in-browser compile Web Worker (issue #20 stage 2c).
 *
 * One worker per tab, created lazily by the host
 * (web/src/wasm-compile/host.ts) on the first wasm-path compile (or the idle
 * prefetch) and then held for the session: the wasm boundary's warm `comemo`
 * caches live in this worker, and a replacement would start every compile
 * cold. Boot instantiates both artifacts — the multi-megabyte compiler module
 * plus the small projection module — then reports `ready` (with the crate
 * versions, making the wasm-vs-server compiler lockstep observable) or
 * `boot-failed` (the host then falls the tab back to server compiles).
 *
 * Compile jobs arrive as {@link CompileWorkerRequest}s and are answered in
 * order. Each job runs the app's `api_compile` pipeline (see ./pipeline.ts):
 * project the sources for the view, convert markdown headings, inject the
 * bibliography and redline support, then compile through a per-project worker
 * pool (`new_compile_workers`) — the browser-tab equivalent of the compile
 * service's worker cache, so switching documents or projects keeps the
 * previously warm compilers warm.
 */
/// <reference lib="webworker" />

import { loadCompileArtifacts, type JsCompileWorkers } from "./artifact"
import {
  buildWasmBoundaryRequest,
  toClientCompileResponse,
  type PipelineDeps,
  type WasmBoundaryResponse
} from "./pipeline"
import type { CompileWorkerMessage, CompileWorkerRequest } from "./protocol"

/**
 * How many project workers the tab-side pool may keep warm. The compile
 * service's default is 256 — server scale. A tab edits a handful of projects,
 * and every worker holds a Typst world plus `comemo` caches, so memory (the
 * issue's "compile-pool memory relocates to per-user clients" risk), not
 * throughput, is the binding constraint here.
 */
const MAX_PROJECT_WORKERS = 4

/**
 * The service's default idle TTL (30 min), carried over for symmetry. On
 * wasm32 the boundary has no wall clock, so the TTL sweep is inert there
 * (documented in crates/nisaba-compile-wasm); the capacity bound is what
 * actually limits the pool.
 */
const WORKER_IDLE_TTL_MS = 30 * 60 * 1000

let pool: JsCompileWorkers | undefined
let deps: PipelineDeps | undefined
let versions: { readonly compile: string; readonly core: string } | undefined

function post(message: CompileWorkerMessage): void {
  self.postMessage(message)
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

// Boot runs once, immediately: the host gates every compile on the ready/boot
// message this produces, and the idle prefetch exists precisely to pay the
// artifact download before the first compile wants it.
const booted: Promise<void> = (async () => {
  const artifacts = await loadCompileArtifacts()
  deps = {
    projectSource: artifacts.core.project_source,
    bibliographyYaml: artifacts.core.bibliography_yaml
  }
  pool = artifacts.compile.new_compile_workers(MAX_PROJECT_WORKERS, WORKER_IDLE_TTL_MS)
  versions = { compile: artifacts.compile.version(), core: artifacts.core.version() }
})()

booted.then(
  () => post({ type: "ready", compileVersion: versions?.compile ?? "?", coreVersion: versions?.core ?? "?" }),
  (error: unknown) => post({ type: "boot-failed", message: messageOf(error) })
)

self.onmessage = (event: MessageEvent<CompileWorkerRequest>): void => {
  const request = event.data
  if (request === null || typeof request !== "object" || request.type !== "compile") return
  // The host never posts before its boot promise resolves, but waiting on
  // `booted` anyway keeps this handler correct independent of that contract.
  void booted.then(
    () => {
      if (deps === undefined || pool === undefined) {
        post({ type: "failed", id: request.id, message: "the in-browser compiler finished booting without its artifacts" })
        return
      }
      try {
        const boundary = buildWasmBoundaryRequest(request.job, deps)
        const responseJson = pool.compile(JSON.stringify(boundary))
        post({
          type: "compiled",
          id: request.id,
          response: toClientCompileResponse(JSON.parse(responseJson) as WasmBoundaryResponse)
        })
      } catch (error) {
        // A JS Error with the core's message strings (stage 2b boundary):
        // the host re-throws it on the main thread as the compile failure.
        post({ type: "failed", id: request.id, message: messageOf(error) })
      }
    },
    (error: unknown) => post({ type: "failed", id: request.id, message: messageOf(error) })
  )
}
