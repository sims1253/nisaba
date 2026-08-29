/**
 * The public surface of the in-browser compile path (issue #20 stage 2c),
 * consumed by web/src/compile.ts's request builder.
 *
 * `createWasmCompile` wires the pieces (toggle → availability → worker host)
 * into one Effect with exactly the server path's signature — the same input
 * object api.compile takes, the same `Effect<CompileResponse, ApiError>` out —
 * so choosing an engine is a one-call substitution, and falling back to the
 * server when the wasm path cannot serve is the dispatcher's job, not the
 * caller's:
 *
 * - toggle off (the default) → always the server;
 * - toggle on + engine unavailable (artifacts not built, no Worker API, boot
 *   failure, crashed worker) → the server, recorded as the fallback;
 * - toggle on + engine up → the worker; only a *compile* failure after a
 *   healthy boot surfaces as an error, because that is a real reportable
 *   failure (the same strings the core produces for the server path), not an
 *   environment problem to paper over.
 *
 * Which path served the latest compile is exposed for the build log line, so
 * the choice is visible where builds are (docs/ui-design.md §4.6).
 */
import { Data, Effect } from "effect"
import * as api from "../api"
import type { CompileView } from "../api"
import { wasmCompileArtifactsPresent } from "./artifact"
import { WasmCompileHost, WasmCompileUnavailableError } from "./host"
import type { WasmCompileJob } from "./pipeline"
import { compilePath, type CompilePath } from "./toggle"

/** The request shape api.compile takes; the dispatcher accepts it verbatim. */
export interface WasmCompileInput {
  readonly projectId: string
  readonly entry: string
  readonly sources: Readonly<Record<string, string>>
  readonly marks?: Readonly<Record<string, readonly api.MarkInput[]>>
  readonly view?: CompileView
}

/** Boot-type unavailability, as a tagged error so only it triggers fallback. */
class WasmCompileUnavailable extends Data.TaggedError("WasmCompileUnavailable")<{ readonly message: string }> {}

export interface WasmCompile {
  /**
   * Compiles via the selected engine. Same contract as api.compile; never
   * fails with `WasmCompileUnavailable` (that always becomes the server path).
   */
  readonly compile: (
    input: WasmCompileInput,
    references: readonly api.Reference[]
  ) => Effect.Effect<api.CompileResponse, api.ApiError>
  /**
   * Why an opted-in tab is still building on the server: the toggle selected
   * wasm but the engine cannot serve (artifacts not built, no Worker API,
   * boot failure, crashed worker). Undefined when the toggle selected the
   * server path or the wasm engine can serve. compile.ts logs this once per
   * session so the fallback is visible, not silent.
   */
  readonly fallbackReason: () => string | undefined
  /** Which engine served the most recent compile ("server" before any). */
  readonly lastServedBy: () => CompilePath
  /** Boots the worker when the toggle selected wasm — on idle, so the first
   *  compile does not also pay the artifact download. A no-op otherwise. */
  readonly prefetch: () => void
}

export interface WasmCompileDeps {
  /** The toggle read; overridable for tests. */
  readonly path?: () => CompilePath
  /** The Worker factory; overridable for tests. */
  readonly createWorker?: () => Worker
  /** The artifacts-present probe; overridable for tests. */
  readonly present?: () => boolean
}

export function createWasmCompile(deps: WasmCompileDeps = {}): WasmCompile {
  const { path = compilePath, present = wasmCompileArtifactsPresent, createWorker } = deps
  const host = new WasmCompileHost({ createWorker, present })
  let servedBy: CompilePath = "server"

  const serverCompile = (input: WasmCompileInput): Effect.Effect<api.CompileResponse, api.ApiError> =>
    Effect.suspend(() => {
      servedBy = "server"
      return api.compile(input)
    })

  return {
    compile: (input, references) => {
      if (path() !== "wasm" || !host.available()) return serverCompile(input)
      const job: WasmCompileJob = {
        project_id: input.projectId,
        entry: input.entry,
        sources: input.sources,
        marks: input.marks ?? {},
        view: input.view ?? "proposed",
        references
      }
      // Recorded at dispatch, not completion: compiles are serialized by the
      // editor's `compiling` guard, so when a callback reads the value it
      // names the engine that served (or, after a fallback below, the one
      // that actually did) — and a FAILED wasm compile still reads "wasm",
      // which is the truth the writer needs when it fails.
      servedBy = "wasm"
      return Effect.tryPromise({
        try: () => host.compile(job),
        catch: (error) => {
          if (error instanceof WasmCompileUnavailableError) {
            return new WasmCompileUnavailable({ message: error.message })
          }
          return error instanceof api.ApiError
            ? error
            : new api.ApiError({ message: error instanceof Error ? error.message : "the in-browser compile failed" })
        }
      }).pipe(Effect.catchTag("WasmCompileUnavailable", () => serverCompile(input)))
    },
    fallbackReason: () => (path() === "wasm" ? host.unavailabilityReason() : undefined),
    lastServedBy: () => servedBy,
    prefetch: () => {
      if (path() !== "wasm" || !host.available()) return
      const boot = (): void => {
        // Boot failure needs no handling here: start() marks the host
        // unavailable, and the next compile takes the server path.
        void host.start().catch(() => undefined)
      }
      if (typeof requestIdleCallback === "function") requestIdleCallback(boot)
      else setTimeout(boot, 2000)
    }
  }
}

/** The app-wide instance compile.ts drives. */
export const wasmCompile: WasmCompile = createWasmCompile()
