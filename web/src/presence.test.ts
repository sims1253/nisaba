import { describe, expect, it } from "vitest"
import { decodePresenceState, decodeRoster, encodePresenceState, initialsOf, peerLocation, type PresencePeer } from "./presence"

/** Mirrors `encode_roster` in services/sync/src/presence.rs. */
function encodeRoster(entries: readonly { peer: bigint; state: Uint8Array }[]): Uint8Array {
  const size = 4 + entries.reduce((total, entry) => total + 12 + entry.state.byteLength, 0)
  const bytes = new Uint8Array(size)
  const view = new DataView(bytes.buffer)
  view.setUint32(0, entries.length, false)
  let offset = 4
  for (const entry of entries) {
    view.setBigUint64(offset, entry.peer, false)
    offset += 8
    view.setUint32(offset, entry.state.byteLength, false)
    offset += 4
    bytes.set(entry.state, offset)
    offset += entry.state.byteLength
  }
  return bytes
}

describe("presence state", () => {
  it("round-trips through the compact JSON encoding", () => {
    const state = { name: "twinkleburst", path: "chapters/intro.typ", section: "Results", line: 12, column: 34 }
    expect(decodePresenceState(encodePresenceState(state))).toEqual(state)
  })

  it("encodes the caret column under the c key next to the line's l key", () => {
    const encoded = new TextDecoder().decode(
      encodePresenceState({ name: "sparkletoes", line: 3, column: 9 }),
    )
    expect(encoded).toBe('{"n":"sparkletoes","l":3,"c":9}')
  })

  it("omits empty optional fields", () => {
    const encoded = new TextDecoder().decode(encodePresenceState({ name: "sparkletoes", path: "" }))
    expect(encoded).toBe('{"n":"sparkletoes"}')
  })

  it("clips long fields so the payload stays far below the relay cap", () => {
    const encoded = encodePresenceState({ name: "x".repeat(500), section: "y".repeat(500) })
    expect(encoded.byteLength).toBeLessThan(256)
  })

  it("returns undefined for empty or malformed state", () => {
    expect(decodePresenceState(new Uint8Array())).toBeUndefined()
    expect(decodePresenceState(new TextEncoder().encode("not json"))).toBeUndefined()
    expect(decodePresenceState(new TextEncoder().encode("[1,2]"))).toBeUndefined()
  })
})

describe("decodeRoster", () => {
  it("decodes the relay's roster encoding", () => {
    const bytes = encodeRoster([
      { peer: 1n, state: encodePresenceState({ name: "twinkleburst", path: "main.typ", line: 3 }) },
      { peer: 2n ** 63n, state: encodePresenceState({ name: "sparkletoes", section: "Methods" }) }
    ])
    const roster = decodeRoster(bytes)
    expect(roster).toHaveLength(2)
    expect(roster[0]).toMatchObject({ peer: 1n, name: "twinkleburst", path: "main.typ", line: 3 })
    expect(roster[1]).toMatchObject({ peer: 2n ** 63n, name: "sparkletoes", section: "Methods" })
  })

  it("keeps a peer whose state is empty or unreadable, so the count stays right", () => {
    const bytes = encodeRoster([
      { peer: 7n, state: new Uint8Array() },
      { peer: 8n, state: new TextEncoder().encode("{{{") }
    ])
    const roster = decodeRoster(bytes)
    expect(roster.map((peer) => peer.peer)).toEqual([7n, 8n])
    expect(roster.every((peer) => peer.name === "")).toBe(true)
  })

  it("never throws on truncated input — presence must not kill the sync socket", () => {
    const bytes = encodeRoster([{ peer: 1n, state: encodePresenceState({ name: "clawson" }) }])
    for (let length = 0; length < bytes.byteLength; length++) {
      expect(() => decodeRoster(bytes.subarray(0, length))).not.toThrow()
    }
    // A header claiming more peers than the buffer holds yields what it could read.
    const lying = bytes.slice()
    new DataView(lying.buffer).setUint32(0, 99, false)
    expect(decodeRoster(lying)).toHaveLength(1)
  })

  it("decodes an empty roster", () => {
    expect(decodeRoster(encodeRoster([]))).toEqual([])
  })

  it("reads a roster that is a view into a larger buffer", () => {
    const inner = encodeRoster([{ peer: 5n, state: encodePresenceState({ name: "clawson" }) }])
    const framed = new Uint8Array(inner.byteLength + 8)
    framed.set(inner, 8)
    expect(decodeRoster(framed.subarray(8))[0]).toMatchObject({ peer: 5n, name: "clawson" })
  })
})

describe("presence display", () => {
  it("builds initials from names, emails, and handles", () => {
    expect(initialsOf("Ada Lovelace")).toBe("AL")
    expect(initialsOf("twinkleburst")).toBe("TW")
    expect(initialsOf("first.last@example.org")).toBe("FL")
    expect(initialsOf("  ")).toBe("?")
  })

  it("degrades anonymous and separator-only names to a glyph", () => {
    expect(initialsOf("anonymous")).toBe("?")
    expect(initialsOf("")).toBe("?")
    expect(initialsOf("...")).toBe("?")
    expect(initialsOf("a@b")).toBe("AB")
  })

  it("phrases a peer's location", () => {
    const peer: PresencePeer = { peer: 1n, name: "sparkletoes", path: "main.typ", section: "Results", line: 12 }
    expect(peerLocation(peer)).toBe("main.typ · §Results · line 12")
    expect(peerLocation({ peer: 2n, name: "x" })).toBe("")
  })
})
