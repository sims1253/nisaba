import { describe, expect, it } from "vitest"
import { reviewReducer, type ReviewState } from "./review"

describe("review protocol reducer", () => {
  const baseItems = [
    { id: "s1", kind: "suggestion" as const, from: 0, to: 2, change: "insert" as const, author: "reviewer", status: "open" as const, createdAt: 1_000 },
    { id: "c1", kind: "comment" as const, from: 5, to: 7, body: "Check this", author: "reviewer", status: "open" as const, createdAt: 2_000 }
  ]
  const state: ReviewState = { suggesting: true, capability: "available", items: baseItems }

  it("accepts one suggestion and bulk rejects another selection, stamping resolver + time", () => {
    const accepted = reviewReducer(state, { type: "accept", id: "s1", by: "editor", at: 5_000 })
    expect(accepted.items[0]?.status).toBe("accepted")
    expect(accepted.items[0]?.resolvedBy).toBe("editor")
    expect(accepted.items[0]?.resolvedAt).toBe(5_000)
    // createdAt must survive a status change (setStatus spreads the item).
    expect(accepted.items[0]?.createdAt).toBe(1_000)
    // bulk-reject on a comment is now a no-op (comments are resolved, not
    // rejected). Verify the comment stays open and is not stamped.
    const rejectedComment = reviewReducer(accepted, { type: "bulk-reject", ids: ["c1"], by: "editor", at: 6_000 })
    expect(rejectedComment.items[1]?.status).toBe("open")
    expect(rejectedComment.items[1]?.resolvedBy).toBeUndefined()
    // Resolving the comment IS valid and stamps correctly.
    const resolvedComment = reviewReducer(accepted, { type: "resolve", id: "c1", by: "editor", at: 6_000 })
    expect(resolvedComment.items[1]?.status).toBe("resolved")
    expect(resolvedComment.items[1]?.resolvedBy).toBe("editor")
    expect(resolvedComment.items[1]?.resolvedAt).toBe(6_000)
  })

  it("rejects a cross-kind accept/reject on a comment (no-op, status stays open)", () => {
    const result = reviewReducer(state, { type: "accept", id: "c1", by: "editor", at: 5_000 })
    expect(result.items[1]?.status).toBe("open")
    expect(result.items[1]?.resolvedBy).toBeUndefined()
    const result2 = reviewReducer(state, { type: "reject", id: "c1", by: "editor", at: 5_000 })
    expect(result2.items[1]?.status).toBe("open")
    expect(result2.items[1]?.resolvedBy).toBeUndefined()
  })

  it("rejects a cross-kind resolve on a suggestion (no-op, status stays open)", () => {
    const result = reviewReducer(state, { type: "resolve", id: "s1", by: "editor", at: 5_000 })
    expect(result.items[0]?.status).toBe("open")
    expect(result.items[0]?.resolvedBy).toBeUndefined()
  })

  it("bulk-reject skips comments (only suggestions are rejectable)", () => {
    const result = reviewReducer(state, { type: "bulk-reject", ids: ["s1", "c1"], by: "editor", at: 6_000 })
    // s1 is a suggestion → rejected
    expect(result.items[0]?.status).toBe("rejected")
    expect(result.items[0]?.resolvedBy).toBe("editor")
    // c1 is a comment → unchanged
    expect(result.items[1]?.status).toBe("open")
    expect(result.items[1]?.resolvedBy).toBeUndefined()
  })

  it("marks anchors outside the document as orphaned", () => {
    expect(reviewReducer(state, { type: "mark-orphans", textLength: 6 }).items[1]?.orphaned).toBe(true)
  })

  it("resolve stamps resolvedBy/resolvedAt without disturbing createdAt", () => {
    const resolved = reviewReducer(state, { type: "resolve", id: "c1", by: "lead", at: 9_000 })
    const comment = resolved.items[1]
    expect(comment?.status).toBe("resolved")
    expect(comment?.resolvedBy).toBe("lead")
    expect(comment?.resolvedAt).toBe(9_000)
    expect(comment?.createdAt).toBe(2_000)
  })
})
