import { describe, expect, it } from "vitest"
import { decodeSyncFrame, encodeSyncFrame } from "./protocol"

describe("sync frame protocol", () => {
  it("round trips the server's binary hello frame", () => {
    const frame = { type: "hello" as const, proto: 1, documentId: "doc/1", peer: 42n, token: "", lastVersionVector: Uint8Array.from([1, 2]) }
    expect(decodeSyncFrame(encodeSyncFrame(frame))).toEqual(frame)
  })
  it("rejects malformed binary frames", () => expect(() => decodeSyncFrame(Uint8Array.from([3, 0, 0, 0]))).toThrow(/truncated/i))

  it("round trips welcome, update, error, heartbeat, and bye frames", () => {
    const cases = [
      { type: "welcome" as const, status: 0 as const, note: "hi", catchup: { type: "snapshot" as const, bytes: Uint8Array.from([9, 9]) } },
      { type: "welcome" as const, status: 1 as const, note: "", catchup: { type: "none" as const } },
      { type: "update" as const, bytes: Uint8Array.from([1, 2, 3]) },
      { type: "snapshot" as const, bytes: Uint8Array.from([]) },
      { type: "presence" as const, bytes: Uint8Array.from([0xff]) },
      { type: "error" as const, code: 403, message: "forbidden" },
      { type: "heartbeat" as const },
      { type: "bye" as const },
    ]
    for (const frame of cases) expect(decodeSyncFrame(encodeSyncFrame(frame))).toEqual(frame)
  })

  it("rejects empty input", () => expect(() => decodeSyncFrame(new Uint8Array(0))).toThrow(/truncated/i))

  it("rejects an unknown tag byte", () => expect(() => decodeSyncFrame(Uint8Array.from([0]))).toThrow(/unknown/i))

  it("rejects an unknown tag byte (tag 9)", () => expect(() => decodeSyncFrame(Uint8Array.from([9]))).toThrow(/unknown/i))

  it("rejects trailing bytes after a valid frame", () => {
    const frame = encodeSyncFrame({ type: "heartbeat" })
    expect(() => decodeSyncFrame(Uint8Array.from([...frame, 0]))).toThrow(/trailing/i)
  })

  it("rejects an invalid welcome status", () => {
    // welcome tag = 2, status byte = 5 (invalid)
    const bytes = encodeSyncFrame({ type: "welcome", status: 0, note: "", catchup: { type: "none" } })
    bytes[1] = 5 // corrupt the status byte
    expect(() => decodeSyncFrame(bytes)).toThrow(/invalid.*status/i)
  })

  it("rejects an invalid catch-up tag in a welcome frame", () => {
    // Build a welcome with catchup type "none" (tag 0), then corrupt to tag 3
    const bytes = encodeSyncFrame({ type: "welcome", status: 0, note: "", catchup: { type: "none" } })
    bytes[2 + 4] = 3 // position: tag(1) + status(1) + note-length(4) = offset 6 is the catchup tag
    expect(() => decodeSyncFrame(bytes)).toThrow(/catch.?up/i)
  })
})
