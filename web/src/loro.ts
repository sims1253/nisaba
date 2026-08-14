/**
 * Browser initialization boundary for Loro.
 *
 * The package's implicit bundler entry can publish its JS classes before Vite
 * has completed WASM initialization. Awaiting the explicit web initializer
 * here gives the app and loro-codemirror one shared, ready module.
 */
import initializeLoro from "loro-crdt/web/loro_wasm.js"

await initializeLoro()

export * from "loro-crdt/web"
