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
  it.each([4029, 4091, 4500])("retries transient relay error %s", (code) => {
    vi.useFakeTimers()
    const sockets: FakeSocket[] = []
    class FakeSocket extends EventTarget {
      static readonly OPEN = 1
      readyState = 1
      send = vi.fn()
      constructor() { super(); sockets.push(this) }
      close(): void { this.readyState = 3; this.dispatchEvent(new Event("close")) }
    }
    vi.stubGlobal("WebSocket", FakeSocket)
    const onStatus = vi.fn()
    const connection = connectSync(new LoroDoc(), { documentId: "doc-1", onStatus })
    try {
      sockets[0]!.dispatchEvent(new Event("open"))
      sockets[0]!.dispatchEvent(new MessageEvent("message", { data: encodeSyncFrame({
        type: "error", code, message: "temporary relay failure"
      }) }))
      vi.advanceTimersByTime(1000)
      expect(sockets).toHaveLength(2)
      sockets[1]!.dispatchEvent(new Event("open"))
      sockets[1]!.dispatchEvent(new MessageEvent("message", { data: encodeSyncFrame({
        type: "welcome", status: 1, note: "", catchup: { type: "none" }
      }) }))
      expect(onStatus).toHaveBeenLastCalledWith("connected", undefined)
      expect(onStatus.mock.calls.some(([status]) => status === "unsupported")).toBe(false)
    } finally { connection.close(); vi.useRealTimers() }
  })

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
        if (frame.type === "update") {
          relay.import(frame.bytes)
          socket.dispatchEvent(new MessageEvent("message", { data: encodeSyncFrame(frame) }))
        }
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

describe("durable update queue", () => {
  it("drains more than 4 MiB of offline edits as original batches, then forgets receipts", () => {
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
        this.receive({ type: "welcome", status: 1, note: "", catchup: { type: "none" } })
      }
      receive(frame: Parameters<typeof encodeSyncFrame>[0]): void {
        this.dispatchEvent(new MessageEvent("message", { data: encodeSyncFrame(frame) }))
      }
      takeUpdates(): Uint8Array[] {
        const frames = this.send.mock.calls.map(([bytes]) => decodeSyncFrame(bytes))
        this.send.mockClear()
        return frames.flatMap(frame => frame.type === "update" ? [frame.bytes] : [])
      }
    }
    vi.stubGlobal("WebSocket", FakeSocket)
    const doc = new LoroDoc()
    const relay = new LoroDoc()
    const connection = connectSync(doc, { documentId: "doc-1" })
    try {
      sockets[0]!.welcome()
      sockets[0]!.readyState = 3
      sockets[0]!.dispatchEvent(new Event("close"))
      // Independent random content prevents compression from hiding the size cap.
      for (let i = 0; i < 140; i++) {
        const random = crypto.getRandomValues(new Uint8Array(32768))
        doc.getText("text").insert(doc.getText("text").length, btoa(String.fromCharCode(...random)))
        doc.commit()
      }
      vi.advanceTimersByTime(1000)
      const socket = sockets.at(-1)!
      socket.welcome()
      let total = 0
      for (let i = 0; i < 140; i++) {
        const updates = socket.takeUpdates()
        expect(updates).toHaveLength(1)
        const bytes = updates[0]!
        expect(bytes.length).toBeLessThan(4 * 1024 * 1024)
        total += bytes.length
        relay.import(bytes)
        socket.receive({ type: "update", bytes })
      }
      expect(total).toBeGreaterThan(4 * 1024 * 1024)
      expect(relay.getText("text").toString()).toBe(doc.getText("text").toString())
      expect(socket.takeUpdates()).toEqual([])
      // Peer history is received state, not an outgoing local batch.
      relay.getText("text").insert(0, "peer edit")
      relay.commit()
      socket.receive({ type: "update", bytes: relay.export({ mode: "update", from: doc.oplogVersion() }) })
      socket.readyState = 3
      socket.dispatchEvent(new Event("close"))
      vi.advanceTimersByTime(1000)
      const reconnected = sockets.at(-1)!
      reconnected.welcome()
      expect(reconnected.takeUpdates()).toEqual([])
    } finally { connection.close(); vi.useRealTimers() }
  }, 20000)
})
