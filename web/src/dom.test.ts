import { beforeAll, describe, expect, it, vi } from "vitest"

class SmokeWebSocket {
  static readonly OPEN = 1
  readonly readyState = SmokeWebSocket.OPEN
  binaryType = "arraybuffer"
  private readonly listeners = new Map<string, EventListener[]>()
  addEventListener(type: string, listener: EventListener): void { this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]) }
  removeEventListener(): void { /* smoke transport */ }
  send = vi.fn()
  close = vi.fn()
}

beforeAll(async () => {
  vi.stubGlobal("WebSocket", SmokeWebSocket)
  vi.stubGlobal("IntersectionObserver", class { disconnect(): void {} observe(): void {} unobserve(): void {} })
  // jsdom has no layout engine. CodeMirror still expects the Range geometry
  // methods to exist while mounting, so provide deterministic empty geometry.
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList
  Range.prototype.getBoundingClientRect = () => new DOMRect()
  document.body.innerHTML = '<main id="app"></main>'
  await import("./main")
})

describe("editor DOM smoke", () => {
  it("mounts the editor, compile action, and preview", () => {
    expect(document.querySelector("#compile-button")?.textContent).toContain("Compile")
    expect(document.querySelector("#editor")).toBeTruthy()
    expect(document.querySelector("#pdf-viewer")).toBeTruthy()
  })
})
