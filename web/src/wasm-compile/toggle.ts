/**
 * The compile-path toggle: which engine serves interactive compiles.
 *
 * Server compile is the default and stays the default for the first release
 * (issue #20 stage 2c): the in-browser wasm engine is experimental until it
 * has soaked in real use, and every build is verified against the server's.
 * Opting in is one documented localStorage line, so no settings surface (and
 * no workspace-shell wiring) is needed:
 *
 *   localStorage.setItem("nisaba.compilePath", "wasm")
 *
 * Anything else — unset, `"server"`, or a mistyped value — selects the server
 * path: the fail-safe direction, because the server engine is the reference
 * implementation. The build log names which path served each compile
 * (web/src/compile.ts), so the choice is observable, not a hidden fact.
 */
export type CompilePath = "server" | "wasm"

/** The localStorage key. It names the *choice*, not one of its options, so
 *  flipping the default later (issue #20) does not rename it. */
const STORAGE_KEY = "nisaba.compilePath"

/**
 * The path used before anyone opts in. Server compile is the shipped default
 * (issue #20 stage 2c keeps it that way for the first release); flipping this
 * constant — and the docs that state it — is the whole default change.
 */
export const DEFAULT_COMPILE_PATH: CompilePath = "server"

// Same defensive accessor web/src/auth.ts uses: localStorage can throw in
// locked-down embedding contexts (on access AND on use), and a broken storage
// must not break builds — every read/write is guarded there too.
function storage(): Storage | undefined {
  try {
    return globalThis.localStorage
  } catch {
    return undefined
  }
}

/** Which engine should serve compiles in this tab. */
export function compilePath(): CompilePath {
  let stored: string | null = null
  try {
    stored = storage()?.getItem(STORAGE_KEY) ?? null
  } catch {
    stored = null
  }
  return stored === "wasm" ? "wasm" : DEFAULT_COMPILE_PATH
}

/** Persists the choice. The parameter type admits only the two legal values,
 *  so a typo can never wedge the flag into an undefined state. */
export function setCompilePath(path: CompilePath): void {
  try {
    storage()?.setItem(STORAGE_KEY, path)
  } catch {
    /* storage unavailable: this tab keeps the default until it can store */
  }
}
