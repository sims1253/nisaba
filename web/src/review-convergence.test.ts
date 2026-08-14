/**
 * Review state as per-item CRDT entries.
 *
 * Previously the entire review collection was stored as a single JSON string in
 * one LoroMap key ("items"), so concurrent reviewers overwrote each other
 * wholesale via last-writer-wins semantics. Each item is now keyed by its ID
 * (schema v2; see review-persistence.ts).
 *
 * These tests exercise the PRODUCTION read/write functions (imported from
 * review-persistence.ts — the same code main.ts's persistReview/loadPersistedReview
 * delegate to), not a hand-rolled container layout.
 */
import { describe, it, expect } from "vitest"
import { LoroDoc } from "loro-crdt"
import type { Comment, ReviewItem } from "./review"
import { readReviewItemsFromMap, writeReviewItemsToMap } from "./review-persistence"

const REVIEW_CONTAINER = "review"

function comment(id: string, author: string, createdAt: number): Comment {
  return { id, kind: "comment", from: 0, to: 1, body: `note from ${author}`, author, status: "open", createdAt }
}

describe("per-item review CRDT entities", () => {
  it("two concurrent reviewers adding different items coexist (no whole-list clobber)", () => {
    const doc1 = new LoroDoc()
    const doc2 = new LoroDoc()

    // Sync initial state
    doc2.import(new Uint8Array(doc1.export({ mode: "update" })))

    // Alice adds c1 on doc1, Bob adds c2 on doc2 (concurrent, offline from each other)
    writeReviewItemsToMap(doc1, [comment("c1", "alice", 1000)])
    doc1.commit({ origin: "review" })
    writeReviewItemsToMap(doc2, [comment("c2", "bob", 2000)])
    doc2.commit({ origin: "review" })

    // Sync both directions
    doc2.import(new Uint8Array(doc1.export({ mode: "update" })))
    doc1.import(new Uint8Array(doc2.export({ mode: "update" })))

    // Both items survive — no clobbering!
    const final1 = readReviewItemsFromMap(doc1)
    const final2 = readReviewItemsFromMap(doc2)

    expect(final1.map((item) => item.id)).toEqual(["c1", "c2"])
    expect(final2.map((item) => item.id)).toEqual(["c1", "c2"])
  })

  it("writes the schema marker and one map key per item id", () => {
    const doc = new LoroDoc()
    writeReviewItemsToMap(doc, [comment("a", "alice", 1000), comment("b", "alice", 2000)])
    doc.commit()

    const map = doc.getMap(REVIEW_CONTAINER)
    expect(map.get("__schema")).toBe(2)
    // Each item is its own map entry (JSON payload keyed by item id).
    expect(JSON.parse(String(map.get("a"))).body).toBe("note from alice")
    expect(JSON.parse(String(map.get("b"))).id).toBe("b")
    // The legacy whole-list blob must be gone.
    expect(map.get("items")).toBeUndefined()
  })

  it("reads come back in a deterministic creation order regardless of key iteration order", () => {
    const doc = new LoroDoc()
    // Write in an order that would sort differently by id (b created before a)
    // and includes a same-millisecond tie broken by id.
    writeReviewItemsToMap(doc, [comment("z", "alice", 3000), comment("b", "alice", 1000), comment("a", "alice", 1000)])
    doc.commit()

    const ids = readReviewItemsFromMap(doc).map((item) => item.id)
    expect(ids).toEqual(["a", "b", "z"])
    // Stable across repeated reads.
    expect(readReviewItemsFromMap(doc).map((item) => item.id)).toEqual(ids)
  })

  it("migrates a legacy single-blob layout: items are re-keyed per id and the blob is removed", () => {
    const doc = new LoroDoc()
    // Legacy v1 format: one JSON string under "items".
    const legacy = [comment("old-1", "alice", 1000), comment("old-2", "bob", 2000)]
    doc.getMap(REVIEW_CONTAINER).set("items", JSON.stringify(legacy))
    doc.commit()

    // A v1 reader (pre-upgrade build) still reads the legacy layout.
    expect(readReviewItemsFromMap(doc).map((item) => item.id)).toEqual(["old-1", "old-2"])

    // The next production write migrates: per-item keys carry the union, the
    // blob is deleted so a stale legacy read cannot resurrect old items.
    writeReviewItemsToMap(doc, [comment("new", "carol", 3000)])
    doc.commit()

    const map = doc.getMap(REVIEW_CONTAINER)
    expect(map.get("__schema")).toBe(2)
    expect(map.get("items")).toBeUndefined()
    expect(readReviewItemsFromMap(doc).map((item) => item.id)).toEqual(["old-1", "old-2", "new"])
  })

  it("a local update to an item wins over the persisted copy without dropping peer items", () => {
    const doc = new LoroDoc()
    writeReviewItemsToMap(doc, [comment("peer", "bob", 1000)])
    doc.commit()

    // A local session that has not seen "peer" writes its own item: the merge
    // (union by id) must keep both.
    writeReviewItemsToMap(doc, [comment("mine", "alice", 2000)])
    doc.commit()

    expect(readReviewItemsFromMap(doc).map((item) => item.id)).toEqual(["peer", "mine"])

    // Status changes (accept/reject/resolve tombstones) update the same key.
    writeReviewItemsToMap(doc, [{ ...comment("peer", "bob", 1000), status: "resolved", resolvedBy: "alice", resolvedAt: 5000 }])
    doc.commit()
    const resolved = readReviewItemsFromMap(doc).find((item) => item.id === "peer")
    expect(resolved?.status).toBe("resolved")
    expect(resolved?.resolvedBy).toBe("alice")
  })
})
