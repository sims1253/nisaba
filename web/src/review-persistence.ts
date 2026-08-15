/**
 * Review persistence layout: per-item CRDT entries.
 *
 * The review state lives in the active replica's "review" LoroMap so it survives
 * reload and syncs to every collaborator through the existing WebSocket relay
 * (the same path the text CRDT uses — no new endpoints).
 *
 * Each item is stored under its own map key (the item id) as a JSON payload.
 * Loro maps are last-writer-wins PER KEY, so two reviewers concurrently adding
 * DIFFERENT items write different keys and both survive convergence. (An
 * earlier dev layout stored the whole list as ONE JSON string under "items";
 * last-writer-wins on that single key let concurrent reviewers clobber each
 * other's whole lists. There is no reader for it — no release shipped it.)
 *
 * Writes MERGE with what is already persisted (union by id, local wins): a
 * session whose catch-up has not arrived yet must still not drop a peer's
 * item.
 *
 * Item removal: review items are never hard-deleted — accept/reject/resolve
 * flip `status` (a tombstone) and the item stays persisted for the audit trail
 * (resolvedBy/resolvedAt).
 */
import type { LoroDoc, LoroMap } from "loro-crdt"
import type { ReviewItem } from "./review"

/** Loro container key that holds the review state (one JSON item per entry). */
export const REVIEW_CONTAINER = "review"

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
 * Read order for items recovered from the map: by creation time, then id as
 * the tiebreaker (coalesced runs share the original createdAt, and two items
 * can be created within the same millisecond). This is deterministic — Loro
 * map key iteration order is not guaranteed.
 */
function compareReviewItems(a: ReviewItem, b: ReviewItem): number {
  if (a.createdAt !== b.createdAt) return a.createdAt - b.createdAt
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0
}

/** Minimal shape guard for a value read out of a map entry. */
function isReviewItem(value: unknown): value is ReviewItem {
  return typeof value === "object" && value !== null &&
    typeof (value as ReviewItem).id === "string" &&
    ((value as ReviewItem).kind === "suggestion" || (value as ReviewItem).kind === "comment")
}

/**
 * Reads the persisted review items from a replica's review map: one JSON item
 * per entry. Returns items in a deterministic order; an empty array when
 * nothing is persisted. Corrupt entries are skipped rather than failing the
 * whole read.
 */
export function readReviewItemsFromMap(doc: LoroDoc): ReviewItem[] {
  try {
    return readPerItemEntries(doc.getMap(REVIEW_CONTAINER))
  } catch { return [] }
}

/** Reads the per-item layout: one JSON item per map key, sorted deterministically. */
function readPerItemEntries(map: LoroMap): ReviewItem[] {
  const items: ReviewItem[] = []
  for (const key of map.keys()) {
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
 * Writes review items into a replica's review map: one map key per item id
 * (JSON payload). The write MERGES with what is already persisted (union by
 * id, local wins) — see the module comment for why items are never dropped
 * here.
 *
 * map.set() is a no-op when the value is unchanged (Loro dedups them), so
 * calling this after every review mutation is cheap. Returns true when
 * anything was written; the caller decides whether to commit.
 */
export function writeReviewItemsToMap(doc: LoroDoc, items: readonly ReviewItem[]): boolean {
  const merged = mergeReviewItems(readReviewItemsFromMap(doc), items)
  let changed = false
  const map = doc.getMap(REVIEW_CONTAINER)
  for (const item of merged) {
    const json = JSON.stringify(item)
    if (map.get(item.id) !== json) {
      map.set(item.id, json)
      changed = true
    }
  }
  return changed
}
