import { Schema } from "effect"
import { CompileResponse } from "./api"
import type { PreviewInputs } from "./api"
import { compilerArtifactPresent } from "./compile-artifact"

export class BrowserCompileUnavailable extends Error {}

interface Pending {
  resolve(response: CompileResponse): void
  reject(error: Error): void
  timer: ReturnType<typeof setTimeout>
}

/** One worker per tab. Only environment failures trigger server fallback. */
export class BrowserCompiler {
  private worker: Worker | undefined
  private failed = false
  private nextId = 0
  private pending = new Map<number, Pending>()

  constructor(private readonly deps: {
    present?: () => boolean
    createWorker?: () => Worker
    timeoutMs?: number
  } = {}) {}

  available(): boolean {
    return !this.failed && (this.deps.present ?? compilerArtifactPresent)()
  }

  private disable(message: string): void {
    this.failed = true
    this.worker?.terminate()
    this.worker = undefined
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer)
      pending.reject(new BrowserCompileUnavailable(message))
    }
    this.pending.clear()
  }

  compile(input: PreviewInputs): Promise<CompileResponse> {
    if (!this.available()) return Promise.reject(new BrowserCompileUnavailable("Browser compiler unavailable"))
    if (!this.worker) {
      try {
        this.worker = this.deps.createWorker?.() ?? new Worker(new URL("./compile-worker.ts", import.meta.url), { type: "module" })
        this.worker.onmessage = (event: MessageEvent<{
          id: number; response?: unknown; error?: string; unavailable?: boolean
        }>): void => {
          const message = event.data
          const pending = this.pending.get(message.id)
          if (!pending) return
          if (message.unavailable) {
            this.disable(message.error ?? "Browser compiler could not start")
            return
          }
          clearTimeout(pending.timer)
          this.pending.delete(message.id)
          if (message.error) pending.reject(new Error(message.error))
          else {
            try { pending.resolve(Schema.decodeUnknownSync(CompileResponse)(message.response)) }
            catch (error) { pending.reject(error instanceof Error ? error : new Error(String(error))) }
          }
        }
        this.worker.onerror = (event): void => {
          event.preventDefault()
          this.disable(event.message || "Browser compiler worker failed")
        }
        this.worker.onmessageerror = (): void => this.disable("Browser compiler response could not be read")
      } catch (error) {
        this.disable(error instanceof Error ? error.message : "Browser workers unavailable")
        return Promise.reject(new BrowserCompileUnavailable("Browser compiler could not start"))
      }
    }
    return new Promise((resolve, reject) => {
      const id = ++this.nextId
      const timer = setTimeout(() => this.disable("Browser compiler timed out"), this.deps.timeoutMs ?? 120_000)
      this.pending.set(id, { resolve, reject, timer })
      try { this.worker!.postMessage({ id, input }) }
      catch { this.disable("Browser compiler request could not be sent") }
    })
  }
}

export const browserCompiler = new BrowserCompiler()
