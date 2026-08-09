const textEncoder = new TextEncoder()
const textDecoder = new TextDecoder("utf-8", { fatal: true })

export type SyncFrame =
  | { readonly type: "hello"; readonly proto: number; readonly documentId: string; readonly peer: bigint; readonly token: string; readonly lastVersionVector: Uint8Array }
  | { readonly type: "welcome"; readonly status: 0 | 1; readonly note: string; readonly catchup: { readonly type: "none" } | { readonly type: "updates" | "snapshot"; readonly bytes: Uint8Array } }
  | { readonly type: "update" | "snapshot" | "presence"; readonly bytes: Uint8Array }
  | { readonly type: "heartbeat" | "bye" }
  | { readonly type: "error"; readonly code: number; readonly message: string }

const syncTags = { hello: 1, welcome: 2, update: 3, snapshot: 4, presence: 5, heartbeat: 6, error: 7, bye: 8 } as const
const syncTagNames = new Map<number, SyncFrame["type"]>(Object.entries(syncTags).map(([name, tag]) => [tag, name as SyncFrame["type"]]))

/**
 * Encodes directly into a pre-sized `Uint8Array`. The previous implementation
 * accumulated a `number[]` and `out.push(...value)`-spread the payload — trash for
 * ~100k-element updates (argument-count limits and quadratic copying).
 */
class Encoder {
  readonly bytes: Uint8Array
  offset = 0
  constructor(size: number) {
    this.bytes = new Uint8Array(size)
  }
  u8(value: number): void {
    this.bytes[this.offset++] = value
  }
  u16(value: number): void {
    this.u8(value >>> 8)
    this.u8(value & 0xff)
  }
  u32(value: number): void {
    if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff) throw new Error("Sync field is too large")
    this.u8(value >>> 24)
    this.u8(value >>> 16 & 0xff)
    this.u8(value >>> 8 & 0xff)
    this.u8(value & 0xff)
  }
  u64(value: bigint): void {
    if (value < 0n || value > 0xffffffffffffffffn) throw new Error("Sync peer id is out of range")
    for (let shift = 56n; shift >= 0n; shift -= 8n) this.u8(Number(value >> shift & 0xffn))
  }
  data(value: Uint8Array): void {
    this.u32(value.byteLength)
    this.bytes.set(value, this.offset)
    this.offset += value.byteLength
  }
  string(value: string): void {
    this.data(textEncoder.encode(value))
  }
}

/** Byte size of the encoded frame, so the encoder can allocate exactly once. */
function frameSize(frame: SyncFrame): number {
  const str = (value: string): number => 4 + textEncoder.encode(value).byteLength
  switch (frame.type) {
    case "hello": return 1 + 1 + str(frame.documentId) + 8 + str(frame.token) + 4 + frame.lastVersionVector.byteLength
    case "welcome": return 1 + 1 + str(frame.note) + 1 + (frame.catchup.type === "none" ? 0 : 4 + frame.catchup.bytes.byteLength)
    case "update": case "snapshot": case "presence": return 1 + 4 + frame.bytes.byteLength
    case "error": return 1 + 2 + str(frame.message)
    case "heartbeat": case "bye": return 1
  }
}

/** Encode one frame of the sync service's versioned binary protocol. */
export function encodeSyncFrame(frame: SyncFrame): Uint8Array {
  const out = new Encoder(frameSize(frame))
  out.u8(syncTags[frame.type])
  switch (frame.type) {
    case "hello": out.u8(frame.proto); out.string(frame.documentId); out.u64(frame.peer); out.string(frame.token); out.data(frame.lastVersionVector); break
    case "welcome": out.u8(frame.status); out.string(frame.note); out.u8(frame.catchup.type === "none" ? 0 : frame.catchup.type === "updates" ? 1 : 2); if (frame.catchup.type !== "none") out.data(frame.catchup.bytes); break
    case "update": case "snapshot": case "presence": out.data(frame.bytes); break
    case "error": out.u16(frame.code); out.string(frame.message); break
    case "heartbeat": case "bye": break
  }
  return out.bytes
}

class SyncCursor {
  private offset = 0
  constructor(private readonly bytes: Uint8Array, private readonly maxBlob = 16 * 1024 * 1024) {}
  readU8(): number { this.require(1); return this.bytes[this.offset++]! }
  readU16(): number { this.require(2); return (this.readU8() << 8) | this.readU8() }
  readU32(): number { return this.readU8() * 0x1000000 + (this.readU8() << 16) + (this.readU8() << 8) + this.readU8() }
  readU64(): bigint { let value = 0n; for (let i = 0; i < 8; i += 1) value = value << 8n | BigInt(this.readU8()); return value }
  readBytes(): Uint8Array { const length = this.readU32(); if (length > this.maxBlob) throw new Error("Sync field exceeds the size limit"); this.require(length); const value = this.bytes.slice(this.offset, this.offset + length); this.offset += length; return value }
  readString(): string { return textDecoder.decode(this.readBytes()) }
  finish(): void { if (this.offset !== this.bytes.byteLength) throw new Error("Trailing bytes in sync frame") }
  private require(length: number): void { if (this.offset + length > this.bytes.byteLength) throw new Error("Truncated sync frame") }
}

/** Decode a complete binary frame, rejecting unknown, truncated, or extra data. */
export function decodeSyncFrame(input: ArrayBuffer | Uint8Array): SyncFrame {
  const cursor = new SyncCursor(input instanceof Uint8Array ? input : new Uint8Array(input))
  const tag = cursor.readU8()
  const type = syncTagNames.get(tag)
  if (!type) throw new Error(`Unknown sync frame tag ${tag}`)
  let frame: SyncFrame
  switch (type) {
    case "hello": { const proto = cursor.readU8(); const documentId = cursor.readString(); const peer = cursor.readU64(); const token = cursor.readString(); frame = { type, proto, documentId, peer, token, lastVersionVector: cursor.readBytes() }; break }
    case "welcome": { const status = cursor.readU8(); if (status !== 0 && status !== 1) throw new Error("Invalid sync welcome status"); const note = cursor.readString(); const catchupTag = cursor.readU8(); const catchup = catchupTag === 0 ? { type: "none" as const } : catchupTag === 1 ? { type: "updates" as const, bytes: cursor.readBytes() } : catchupTag === 2 ? { type: "snapshot" as const, bytes: cursor.readBytes() } : (() => { throw new Error("Invalid sync catch-up tag") })(); frame = { type, status: status as 0 | 1, note, catchup }; break }
    case "update": case "snapshot": case "presence": frame = { type, bytes: cursor.readBytes() }; break
    case "heartbeat": case "bye": frame = { type }; break
    case "error": frame = { type, code: cursor.readU16(), message: cursor.readString() }; break
  }
  cursor.finish()
  return frame
}
