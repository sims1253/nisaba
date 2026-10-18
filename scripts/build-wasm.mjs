#!/usr/bin/env node
// Honor Cargo's configured target directory, including a shared build cache.
import { execFileSync } from "node:child_process"
import { readFileSync, mkdirSync } from "node:fs"
import { join } from "node:path"
import { fileURLToPath } from "node:url"

const root = fileURLToPath(new URL("../", import.meta.url))
const version = readFileSync(join(root, "Cargo.lock"), "utf8")
  .match(/name = "wasm-bindgen"\nversion = "([^"]+)"/)[1]
const installed = execFileSync("wasm-bindgen", ["--version"], { encoding: "utf8" }).trim()
if (installed !== `wasm-bindgen ${version}`) {
  throw new Error(`Install wasm-bindgen-cli ${version}; found ${installed}`)
}
execFileSync("cargo", ["build", "--locked", "--release", "--target", "wasm32-unknown-unknown", "-p", "nisaba-compile-wasm"], { cwd: root, stdio: "inherit" })
const { target_directory: target } = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { cwd: root, encoding: "utf8" }))
const output = join(root, "web/src/wasm-generated")
mkdirSync(output, { recursive: true })
execFileSync("wasm-bindgen", [join(target, "wasm32-unknown-unknown/release/nisaba_compile_wasm.wasm"), "--target", "web", "--out-dir", output], { stdio: "inherit" })
