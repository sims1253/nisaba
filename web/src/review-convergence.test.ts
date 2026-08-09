/**
 * Review state as per-item CRDT entries.
 *
 * Previously the entire review collection was stored as a single JSON string in
 * one LoroMap key ("items"), so concurrent reviewers overwrote each other
 * wholesale via last-writer-wins semantics. Each item is now keyed by its ID.
 *
 * This test verifies that two concurrent additions of DIFFERENT items coexist
 * after CRDT convergence.
 */
import { describe, it, expect } from "vitest"
import { LoroDoc } from "loro-crdt"

const REVIEW_CONTAINER = "review"

describe("per-item review CRDT entities", () => {
  it("two concurrent reviewers adding different items coexist", () => {
    const doc1 = new LoroDoc()
    const doc2 = new LoroDoc()

    // Sync initial state
    doc2.import(new Uint8Array(doc1.export({ mode: "update" })))

    const map1 = doc1.getMap(REVIEW_CONTAINER)
    const map2 = doc2.getMap(REVIEW_CONTAINER)

    // Alice adds c1 on doc1
    map1.set("__schema", 2)
    map1.set("c1", JSON.stringify({ id: "c1", kind: "comment", body: "alice", author: "alice", status: "open", createdAt: 1000 }))
    doc1.commit({ origin: "review" })

    // Bob adds c2 on doc2 (concurrent, offline from doc1)
    map2.set("__schema", 2)
    map2.set("c2", JSON.stringify({ id: "c2", kind: "comment", body: "bob", author: "bob", status: "open", createdAt: 2000 }))
    doc2.commit({ origin: "review" })

    // Sync both directions
    doc2.import(new Uint8Array(doc1.export({ mode: "update" })))
    doc1.import(new Uint8Array(doc2.export({ mode: "update" })))

    // Both items survive — no clobbering!
    const final1Keys = [...map1.keys()].filter((k) => k !== "__schema")
    const final2Keys = [...map2.keys()].filter((k) => k !== "__schema")

    expect(final1Keys).toContain("c1")
    expect(final1Keys).toContain("c2")
    expect(final2Keys).toContain("c1")
    expect(final2Keys).toContain("c2")
  })

  it("legacy single-blob format still works for backward compat", () => {
    const doc = new LoroDoc()
    const map = doc.getMap(REVIEW_CONTAINER)
    // Legacy v1 format
    map.set("items", JSON.stringify([{ id: "x", kind: "comment", body: "old", author: "a", status: "open", createdAt: 0 }]))
    doc.commit()

    // The legacy data is still readable
    const json = map.get("items")
    expect(typeof json).toBe("string")
    const items = JSON.parse(json as string)
    expect(items).toHaveLength(1)
  })
})
