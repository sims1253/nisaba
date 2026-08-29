/**
 * Loader for the optional in-browser compile WASM artifacts.
 *
 * The artifacts are wasm-bindgen `--target web` output for
 * `crates/nisaba-compile-wasm` (the compiler; tens of megabytes, mostly the
 * embedded `typst-assets` fonts) and `crates/nisaba-core-wasm` (the
 * projection; small). They are NEVER committed: `just wasm-web` builds them
 * into the gitignored `web/src/wasm-generated/`.
 *
 * This module is the only code that references that directory, through
 * `import.meta.glob`: a glob that matches nothing yields no entries, so the
 * web app installs, lints, tests, and builds identically whether or not the
 * artifacts are present, and their absence is a plain runtime fact
 * (`wasmCompileArtifactsPresent()`) that the compile dispatcher turns into
 * "use the server path" instead of an error. When they are present, Vite
 * bundles them as lazily-imported chunks (dev serves them straight from
 * `src/`), and the first wasm compile — or the idle prefetch — downloads them.
 */

/** wasm-bindgen's `--target web` glue, as far as this client drives it. */
export interface WasmGlue {
  /** Fetches and instantiates the module; resolves when exports are live. */
  readonly default: (moduleOrPath?: string) => Promise<unknown>
}

/** The pool handle from `new_compile_workers` (crates/nisaba-compile-wasm). */
export interface JsCompileWorkers {
  /**
   * Compiles a request JSON (the compile service's body shape) and returns
   * the response JSON. Throws a JS `Error` carrying the core's message
   * strings; compile failures are ordinary diagnostics inside a successful
   * response, exactly as in the service.
   */
  readonly compile: (requestJson: string) => string
}

/** The generated compile wrapper's exports (crates/nisaba-compile-wasm). */
export interface CompileWasmModule extends WasmGlue {
  /** Per-project worker pool with the compile service's LRU/TTL cache. */
  readonly new_compile_workers: (maxWorkers: number, idleTtlMillis: number) => JsCompileWorkers
  /** The workspace crate version, for compiler-lockstep observability. */
  readonly version: () => string
}

/** The generated projection wrapper's exports (crates/nisaba-core-wasm). */
export interface CoreWasmModule extends WasmGlue {
  /** Projects one source's marks for a view (the app's `projected_source`). */
  readonly project_source: (source: string, marksJson: string, view: string) => string
  /** Renders the bibliography YAML for `#bibliography` (app's helper). */
  readonly bibliography_yaml: (referencesJson: string) => string
  /** The workspace crate version, for compiler-lockstep observability. */
  readonly version: () => string
}

// Exact filenames, not a directory glob: a wrongly named build must read as
// "absent" (server path), not accidentally load some other generated module.
// wasm-bindgen derives the names from the cdylib targets, so they carry the
// crates' underscores: nisaba_compile_wasm.js / nisaba_core_wasm.js.
const compileLoaders = import.meta.glob("../wasm-generated/nisaba_compile_wasm.js")
const coreLoaders = import.meta.glob("../wasm-generated/nisaba_core_wasm.js")

/** Whether both generated artifacts exist to be loaded. Cheap and
 *  synchronous; the compile dispatcher consults it before every compile. */
export function wasmCompileArtifactsPresent(): boolean {
  return Object.keys(compileLoaders).length > 0 && Object.keys(coreLoaders).length > 0
}

/**
 * Loads and instantiates both artifacts. Runs in the compile Web Worker (the
 * multi-megabyte instantiate would freeze the editor thread); the main thread
 * only ever calls {@link wasmCompileArtifactsPresent}.
 *
 * @throws When either artifact is missing (not built — run `just wasm-web`)
 *         or fails to import or instantiate; the caller decides whether that
 *         is fatal (worker boot) or a fallback trigger (host marks itself
 *         unavailable).
 */
export async function loadCompileArtifacts(): Promise<{
  readonly compile: CompileWasmModule
  readonly core: CoreWasmModule
}> {
  const compileLoader = Object.values(compileLoaders)[0]
  const coreLoader = Object.values(coreLoaders)[0]
  if (compileLoader === undefined || coreLoader === undefined) {
    throw new Error("in-browser compile artifacts are not built (run `just wasm-web`)")
  }
  const [compileModule, coreModule] = await Promise.all([compileLoader(), coreLoader()])
  const compile = compileModule as unknown as CompileWasmModule
  const core = coreModule as unknown as CoreWasmModule
  // The glue's default initializer fetches and instantiates the wasm binary;
  // until it resolves, the exports above are callable but would trap.
  await compile.default()
  await core.default()
  return { compile, core }
}
