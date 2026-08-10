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
  it("mounts the editor, the primary action, and the preview", () => {
    // The primary action is stated in the writer's words, not the compiler's
    // (docs/ui-design.md §1) — the tooltip still says "compile".
    expect(document.querySelector("#compile-button")?.textContent).toContain("Update preview")
    expect(document.querySelector("#editor")).toBeTruthy()
    expect(document.querySelector("#pdf-viewer")).toBeTruthy()
  })

  it("mounts every persistent region of the workspace", () => {
    for (const selector of [
      ".appbar",
      "#crumbs",
      "#projects-screen",
      "#navigator",
      "#file-tree",
      "#section-outline",
      "#dock",
      "#build-drawer",
      ".statusbar",
      "#workspace-panel"
    ]) {
      expect(document.querySelector(selector), selector).toBeTruthy()
    }
  })

  it("starts on the projects screen with the workspace put away", () => {
    expect(document.querySelector<HTMLElement>("#projects-screen")?.hidden).toBe(false)
    expect(document.querySelector<HTMLElement>("#workspace")?.hidden).toBe(true)
    expect(document.body.classList.contains("has-project")).toBe(false)
  })

  it("keeps review state on one door and one room: the dock starts closed", () => {
    expect(document.querySelector<HTMLElement>("#dock")?.hidden).toBe(true)
    expect(document.querySelector<HTMLElement>("#review-count")?.hidden).toBe(true)
    // The banner that used to duplicate the count is gone for good.
    expect(document.querySelector("#review-banner")).toBeNull()
  })

  it("states track changes on exactly one switch", () => {
    const switches = document.querySelectorAll('[id$="suggesting"], #suggesting-button')
    expect(switches).toHaveLength(1)
    expect(document.querySelector("#suggesting-button")?.textContent).toBe("Track changes: off")
  })

  it("offers the four projection views under writer-facing names", () => {
    const labels = [...document.querySelectorAll("#view-switch button")].map((node) => node.textContent)
    expect(labels).toEqual(["Final", "Original", "All markup", "Public copy"])
  })
})
