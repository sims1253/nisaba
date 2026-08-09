/**
 * Loro cursor utilities for stable mark anchoring.
 *
 * Marks anchored at integer offsets become invalid when text before them
 * is edited. Loro cursors are stable identifiers that survive concurrent
 * edits. These helpers encode/decode cursors for JSON-safe storage.
 */
import { Cursor, type LoroDoc } from "loro-crdt"

/**
 * Encode a Loro cursor as a base64 string for JSON storage.
 */
export function encodeCursor(cursor: Cursor): string {
  const bytes = cursor.encode()
  return btoa(String.fromCharCode(...bytes))
}

/**
 * Decode a base64-encoded cursor back to a Loro Cursor.
 */
export function decodeCursor(encoded: string): Cursor | undefined {
  try {
    const bytes = new Uint8Array(atob(encoded).split("").map((c) => c.charCodeAt(0)))
    return Cursor.decode(bytes)
  } catch {
    return undefined
  }
}

/**
 * Create a cursor at the given offset in the document's text container,
 * encode it as base64. Returns undefined if the cursor cannot be created.
 */
export function createCursorAt(doc: LoroDoc, offset: number): string | undefined {
  const text = doc.getText("text")
  const cursor = text.getCursor(offset, 0) // side=0 = before
  if (!cursor) return undefined
  return encodeCursor(cursor)
}

/**
 * Resolve a base64-encoded cursor to its current offset in the document.
 * Returns undefined if the cursor cannot be resolved (e.g., the position
 * was deleted and the cursor is orphaned).
 */
export function resolveCursor(doc: LoroDoc, encoded: string): number | undefined {
  const cursor = decodeCursor(encoded)
  if (!cursor) return undefined
  const pos = doc.getCursorPos(cursor)
  return pos?.offset
}
