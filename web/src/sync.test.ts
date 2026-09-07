import { afterEach, describe, expect, it, vi } from "vitest"
import { LoroDoc } from "loro-crdt"
import { decodeSyncFrame, encodeSyncFrame } from "./protocol"
import { connectSync } from "./sync"

afterEach(() => vi.unstubAllGlobals())

describe("sync access failures", () => {
  it.each([
    [4003, "project access was revoked", true],
    [4003, "project access changed", true],
    [4003, "project access could not be verified: unauthenticated: membership removed", true],
    [4003, "reviewer cannot modify document text", false],
    [4002, "project access could not be verified", false]
  ])("handles error %i: %s", (code, message, losesAccess) => {
    const socket = Object.assign(new EventTarget(), { close: vi.fn() })
    vi.stubGlobal("WebSocket", class { constructor() { return socket } })
    const onAccessRevoked = vi.fn()
    const connection = connectSync(new LoroDoc(), { documentId: "doc-1", onAccessRevoked })
    try {
      socket.dispatchEvent(new MessageEvent("message", {
        data: encodeSyncFrame({ type: "error", code, message })
      }))
      expect(onAccessRevoked).toHaveBeenCalledTimes(losesAccess ? 1 : 0)
      if (losesAccess) expect(onAccessRevoked).toHaveBeenCalledWith(message)
      expect(socket.close).toHaveBeenCalled()
    } finally {
      connection.close()
    }
  })
})


describe("sync reconnect", () => {
  it.each([false, true])("resumes streaming and retries interrupted sends (drop first catch-up: %s)", (dropFirstCatchup) => {
    vi.useFakeTimers()
    const sockets: FakeSocket[] = []
    class FakeSocket extends EventTarget {
      static readonly OPEN = 1
      readyState = 1
      send = vi.fn()
      close = vi.fn()
      constructor() { super(); sockets.push(this) }
      welcome(): void {
        this.dispatchEvent(new Event("open"))
        this.dispatchEvent(new MessageEvent("message", { data: encodeSyncFrame({
          type: "welcome", status: 1, note: "", catchup: { type: "none" }
        }) }))
      }
    }
    vi.stubGlobal("WebSocket", FakeSocket)
    const doc = new LoroDoc()
    const relay = new LoroDoc()
    const ready = vi.fn()
    const connection = connectSync(doc, { documentId: "doc-1", onReady: ready })
    const edit = (text: string): void => {
      doc.getText("text").insert(doc.getText("text").length, text)
      doc.commit()
    }
    const deliver = (socket: FakeSocket): void => {
      for (const [bytes] of socket.send.mock.calls) {
        const frame = decodeSyncFrame(bytes)
        if (frame.type === "update") relay.import(frame.bytes)
      }
      socket.send.mockClear()
    }
    try {
      sockets[0]!.welcome()
      edit("start")
      deliver(sockets[0]!)
      expect(relay.getText("text").toString()).toBe("start")
      for (let i = 0; i < 2; i++) {
        const previous = sockets.at(-1)!
        previous.readyState = 3
        previous.dispatchEvent(new Event("close"))
        edit(" gap")
        vi.advanceTimersByTime(1000)
        const next = sockets.at(-1)!
        expect(next).not.toBe(previous)
        next.welcome()
        if (dropFirstCatchup && i === 0) {
          // send() succeeded locally, but the connection died before persistence.
          next.send.mockClear()
          continue
        }
        deliver(next)
        expect(relay.getText("text").toString()).toBe(doc.getText("text").toString())
        edit(" live")
        deliver(next)
        expect(relay.getText("text").toString()).toBe(doc.getText("text").toString())
      }
      expect(ready).toHaveBeenCalledTimes(1)
    } finally {
      connection.close()
      vi.useRealTimers()
    }
  })
})
