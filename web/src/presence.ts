/**
 * Presence: who else is in this document, and where they are working.
 *
 * The sync relay has always maintained a per-document roster with heartbeat
 * expiry (`services/sync/src/presence.rs`) and broadcast it as `presence`
 * frames; the web client simply never sent or read one, so the UI could only say
 * "2 collaborators online". This module is the client half: a tiny JSON state
 * payload per peer, and a decoder for the relay's roster encoding.
 *
 * Two rules shape the code:
 *
 * 1. **Presence must never break the document.** `connectSync` treats a thrown
 *    error while handling a frame as a fatal protocol failure and tears the
 *    socket down. So `decodeRoster` never throws: a truncated or malformed
 *    roster degrades to the peers it could read, and a peer whose state is not
 *    parseable still counts as present, just unnamed.
 * 2. **Payloads stay small.** The relay caps a presence payload (16 KiB) and
 *    rate-limits frames; an oversized one is answered with an error frame, which
 *    the client treats as fatal. Names and section titles are truncated so the
 *    encoded state cannot approach the cap.
 */

/** Maximum characters kept for any single string field in a presence payload. */
const MAX_FIELD = 64

/** Ephemeral state a peer publishes about itself. */
export interface PresenceState {
  /** Display name, from the OIDC token. */
  readonly name: string
  /** Project-relative path of the document they have open. */
  readonly path?: string
  /** Title of the section their caret is in. */
  readonly section?: string
  /** 1-based caret line. */
  readonly line?: number
  /** 1-based caret column (UTF-16 offset within the line + 1). */
  readonly column?: number
}

/** A roster entry: presence state plus the CRDT peer id that published it. */
export interface PresencePeer extends PresenceState {
  readonly peer: bigint
}

const clip = (value: string): string => (value.length > MAX_FIELD ? `${value.slice(0, MAX_FIELD - 1)}…` : value)

/**
 * Encodes presence state as compact JSON. Keys are single letters because this
 * payload is re-broadcast to every peer in the room on each change.
 */
export function encodePresenceState(state: PresenceState): Uint8Array {
  const payload: Record<string, string | number> = { n: clip(state.name) }
  if (state.path !== undefined && state.path !== "") payload["p"] = clip(state.path)
  if (state.section !== undefined && state.section !== "") payload["s"] = clip(state.section)
  if (state.line !== undefined && Number.isFinite(state.line)) payload["l"] = Math.max(1, Math.trunc(state.line))
  if (state.column !== undefined && Number.isFinite(state.column)) payload["c"] = Math.max(1, Math.trunc(state.column))
  return new TextEncoder().encode(JSON.stringify(payload))
}

/** Parses one peer's opaque state payload; undefined when it is not ours to read. */
export function decodePresenceState(bytes: Uint8Array): PresenceState | undefined {
  if (bytes.byteLength === 0) return undefined
  try {
    const parsed: unknown = JSON.parse(new TextDecoder().decode(bytes))
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return undefined
    const record = parsed as Record<string, unknown>
    const name = typeof record["n"] === "string" ? record["n"] : ""
    const path = typeof record["p"] === "string" ? record["p"] : undefined
    const section = typeof record["s"] === "string" ? record["s"] : undefined
    const line = typeof record["l"] === "number" && Number.isFinite(record["l"]) ? record["l"] : undefined
    const column = typeof record["c"] === "number" && Number.isFinite(record["c"]) ? record["c"] : undefined
    return { name, path, section, line, column }
  } catch {
    return undefined
  }
}

/**
 * Decodes the relay's roster frame.
 *
 * Wire format (`encode_roster`, big-endian):
 * `[u32 count]( [u64 peer][u32 len][len bytes state] )*`
 *
 * Returns the entries it could read; a truncated tail is dropped rather than
 * raised, because a partial roster is a cosmetic problem and a thrown error
 * would close the document's sync connection.
 */
export function decodeRoster(bytes: Uint8Array): readonly PresencePeer[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  if (bytes.byteLength < 4) return []
  const count = view.getUint32(0, false)
  const peers: PresencePeer[] = []
  let offset = 4
  for (let index = 0; index < count; index++) {
    if (offset + 12 > bytes.byteLength) break
    const peer = view.getBigUint64(offset, false)
    offset += 8
    const length = view.getUint32(offset, false)
    offset += 4
    if (offset + length > bytes.byteLength) break
    const state = decodePresenceState(bytes.subarray(offset, offset + length))
    offset += length
    peers.push({ peer, name: state?.name ?? "", path: state?.path, section: state?.section, line: state?.line, column: state?.column })
  }
  return peers
}

/**
 * Initials (max 2) for an avatar chip; `?` when there is no usable name.
 * Splitting on `@` as well as whitespace/`.`/`_`/`-` matters because OIDC
 * display names may be email-shaped ("first.last@example.org") — without it
 * the local part and domain fuse into one "word". `"anonymous"` (the no-token
 * display name) and empty names degrade to `?` so the chip always has a glyph.
 */
export function initialsOf(name: string): string {
  const trimmed = name.trim()
  if (!trimmed || trimmed === "anonymous") return "?"
  const parts = trimmed.split(/[\s._@-]+/).filter(Boolean)
  const first = parts[0]
  if (first === undefined) return "?"
  const second = parts[1]
  if (second === undefined) return first.slice(0, 2).toUpperCase()
  return (first.slice(0, 1) + second.slice(0, 1)).toUpperCase()
}

/** Where a peer is, phrased for a tooltip: `main.typ · §Results, line 12`. */
export function peerLocation(peer: PresencePeer): string {
  const parts: string[] = []
  if (peer.path) parts.push(peer.path)
  if (peer.section) parts.push(`§${peer.section}`)
  if (peer.line !== undefined) parts.push(`line ${peer.line}`)
  return parts.join(" · ")
}
