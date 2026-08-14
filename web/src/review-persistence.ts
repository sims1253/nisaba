/**
 * Review persistence layout: per-item CRDT entries (schema v2).
 *
 * The review state lives in the active replica's "review" LoroMap so it survives
 * reload and syncs to every collaborator through the existing WebSocket relay
 * (the same path the text CRDT uses — no new endpoints).
 *
 * Schema v2 (current) stores each item under its own map key (the item id),
 * alongside a `__schema: 2` marker. Loro maps are last-writer-wins PER KEY, so
 * two reviewers concurrently adding DIFFERENT items write different keys and
 * both survive convergence — the reason this layout replaced v1.
 *
 * Schema v1 (legacy) stored the whole item list as ONE JSON string under the
 * "items" key. Last-writer-wins per key meant a peer writing a stale or empty
 * list (a fresh session whose catch-up had not arrived, or a reload racing the
 * WELCOME) clobbered the shared items wholesale — the 2026-08-09 collaboration
 * finding: reviewer suggestions lost, snapshots showed the container reset to
 * []. Writes under v1 therefore had to MERGE the current map value with the
 * local list (union by id, local wins per id). v2 keeps that merge for the
 * individual item payloads (a not-yet-caught-up session still must not drop a
 * peer's item), but concurrent item ADDITIONS can no longer clobber each other.
 *
 * Item removal: review items are never hard-deleted — accept/reject/resolve
 * flip `status` (a tombstone) and the item stays persisted for the audit trail
 * (resolvedBy/resolvedAt). Because writes merge persisted ∪ local, this module
 * only ever ADDS or UPDATES per-item keys; the only key it deletes is the
 * legacy "items" blob during the v1 → v2 migration (after its contents have
 * been re-keyed per item, so no data is lost).
 */
import type { LoroDoc, LoroMap } from "loro-crdt"
import type { ReviewItem } from "./review"

/** Loro container key that holds the serialised review state (JSON-in-LoroMap). */
export const REVIEW_CONTAINER = "review"
/** Map key marking the per-item layout; guards readers against mixed layouts. */
const REVIEW_SCHEMA_KEY = "__schema"
/** The v1 layout's single whole-list key, kept readable for migration. */
const REVIEW_LEGACY_ITEMS_KEY = "items"

/**
 * Union of two review item lists by id, with `local` winning for duplicate ids
 * (the local session is authoritative for items it created or just acted on).
 * Prevents a stale/empty session from clobbering shared review state.
 */
export function mergeReviewItems(remote: readonly ReviewItem[], local: readonly ReviewItem[]): ReviewItem[] {
  const byId = new Map<string, ReviewItem>()
  for (const item of remote) byId.set(item.id, item)
  for (const item of local) byId.set(item.id, item) // local wins
  return [...byId.values()]
}

/**
 * Read order for items recovered from per-item map keys: by creation time, then
 * id as the tiebreaker (coalesced runs share the original createdAt, and two
 * items can be created within the same millisecond). The legacy blob preserved
 * insertion order, which IS creation order, so this reproduces the v1 ordering
 * consumers (the review dock queue, `openReviewItems`) were built against — but
 * deterministically, since Loro map key iteration order is not guaranteed.
 */
function compareReviewItems(a: ReviewItem, b: ReviewItem): number {
  if (a.createdAt !== b.createdAt) return a.createdAt - b.createdAt
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0
}

/** Minimal shape guard for a value read out of a per-item map key. */
function isReviewItem(value: unknown): value is ReviewItem {
  return typeof value === "object" && value !== null &&
    typeof (value as ReviewItem).id === "string" &&
    ((value as ReviewItem).kind === "suggestion" || (value as ReviewItem).kind === "comment")
}

/**
 * Reads the persisted review items from a replica's review map, handling BOTH
 * layouts: per-item keys when the `__schema: 2` marker is present, and the
 * legacy single-"items" blob otherwise (a document last touched by an older
 * build, before it is re-written). Returns items in a deterministic order;
 * an empty array when nothing is persisted.
 */
export function readReviewItemsFromMap(doc: LoroDoc): ReviewItem[] {
  try {
    const map = doc.getMap(REVIEW_CONTAINER)
    if (map.get(REVIEW_SCHEMA_KEY) === 2) return readPerItemEntries(map)
    const value = map.get(REVIEW_LEGACY_ITEMS_KEY)
    if (value === undefined) return []
    const parsed = JSON.parse(String(value))
    return Array.isArray(parsed) ? parsed.filter(isReviewItem) : []
  } catch { return [] }
}

/** Reads the schema v2 layout: one JSON item per map key, sorted deterministically. */
function readPerItemEntries(map: LoroMap): ReviewItem[] {
  const items: ReviewItem[] = []
  for (const key of map.keys()) {
    if (key === REVIEW_SCHEMA_KEY || key === REVIEW_LEGACY_ITEMS_KEY) continue
    const raw = map.get(key)
    if (typeof raw !== "string") continue
    try {
      const parsed = JSON.parse(raw)
      if (isReviewItem(parsed)) items.push(parsed)
    } catch { /* corrupt entry: skip rather than fail the whole read */ }
  }
  return items.sort(compareReviewItems)
}

/**
 * Writes review items into a replica's review map using the per-item layout:
 * sets `__schema: 2`, one map key per item id (JSON payload), and deletes the
 * legacy "items" blob so a stale v1 read cannot resurrect pre-migration items.
 * The write MERGES with what is already persisted (union by id, local wins) —
 * see the module comment for why items are never dropped here.
 *
 * map.set()/map.delete() are no-ops when the value is unchanged (Loro dedups
 * them), so calling this after every review mutation is cheap. Returns true
 * when anything was written; the caller decides whether to commit.
 */
export function writeReviewItemsToMap(doc: LoroDoc, items: readonly ReviewItem[]): boolean {
  const merged = mergeReviewItems(readReviewItemsFromMap(doc), items)
  let changed = false
  const map = doc.getMap(REVIEW_CONTAINER)
  if (map.get(REVIEW_SCHEMA_KEY) !== 2) {
    map.set(REVIEW_SCHEMA_KEY, 2)
    changed = true
  }
  for (const item of merged) {
    const json = JSON.stringify(item)
    if (map.get(item.id) !== json) {
      map.set(item.id, json)
      changed = true
    }
  }
  // Migration cleanup: the blob's contents were unioned into the per-item keys
  // above, so removing it loses nothing and prevents a legacy read from
  // resurrecting items v2 no longer carries.
  if (map.get(REVIEW_LEGACY_ITEMS_KEY) !== undefined) {
    map.delete(REVIEW_LEGACY_ITEMS_KEY)
    changed = true
  }
  return changed
}
