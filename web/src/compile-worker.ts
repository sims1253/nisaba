/** The compiler and its warm caches stay off the editor thread. */
import { loadCompiler } from "./compile-artifact"
import type { PreviewInputs } from "./api"

let compiler: ReturnType<typeof loadCompiler> | undefined

self.onmessage = async (event: MessageEvent<{ id: number; input: PreviewInputs }>): Promise<void> => {
  const { id, input } = event.data
  let engine: Awaited<ReturnType<typeof loadCompiler>>
  try {
    compiler ??= loadCompiler()
    engine = await compiler
  } catch (error) {
    self.postMessage({ id, unavailable: true, error: String(error) })
    return
  }
  try {
    const response = JSON.parse(engine.compile(JSON.stringify(input))) as Record<string, unknown>
    self.postMessage({ id, response: { ...response, pdf_base64: response.pdf } })
  } catch (error) {
    self.postMessage({ id, error: String(error) })
  }
}
