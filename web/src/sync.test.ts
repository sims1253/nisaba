import { afterEach, describe, expect, it, vi } from "vitest"
import { LoroDoc } from "loro-crdt"
import { encodeSyncFrame } from "./protocol"
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
