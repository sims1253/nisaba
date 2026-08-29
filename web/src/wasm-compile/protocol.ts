/**
 * Wire protocol between the compile host (main thread) and the compile worker
 * (web/src/wasm-compile/compile-worker.ts). Plain JSON-cloneable messages
 * only: `Error` objects do not structured-clone, so failures travel as
 * message strings and the throwing side is re-thrown by the receiver.
 */
import type { ClientCompileResponse, WasmCompileJob } from "./pipeline"

/** Main → worker: run one compile job. `id` correlates the reply. */
export interface CompileWorkerRequest {
  readonly type: "compile"
  readonly id: number
  readonly job: WasmCompileJob
}

/** Worker → main. */
export type CompileWorkerMessage =
  /** Boot succeeded: both artifacts are instantiated; the versions make the
   *  wasm-vs-server compiler lockstep (issue #20's version-skew risk)
   *  observable in logs. */
  | { readonly type: "ready"; readonly compileVersion: string; readonly coreVersion: string }
  /** Boot failed (artifact missing/import failure): the host marks itself
   *  unavailable and compiles fall back to the server path. */
  | { readonly type: "boot-failed"; readonly message: string }
  /** The compile finished; `response` is already in the client's wire shape. */
  | { readonly type: "compiled"; readonly id: number; readonly response: ClientCompileResponse }
  /** The compile threw before producing a response. */
  | { readonly type: "failed"; readonly id: number; readonly message: string }
