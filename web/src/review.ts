export type ReviewItem = Suggestion | Comment
export interface Suggestion {
  readonly id: string; readonly kind: "suggestion"; readonly from: number; readonly to: number; readonly change: "insert" | "delete"; readonly text?: string; readonly author: string
  /** Base64-encoded Loro cursor anchoring the `from` position. */
  readonly fromCursor?: string
  /** Base64-encoded Loro cursor anchoring the `to` position. */
  readonly toCursor?: string
  readonly status: "open" | "accepted" | "rejected"; readonly orphaned?: boolean
  /** Epoch millis when the item was first created (coalesced runs keep the original). */
  readonly createdAt: number
  /** Epoch millis when the item was accepted/rejected/resolved, if it has been. */
  readonly resolvedAt?: number
  /** Display name of the user who accepted/rejected/resolved this item, if anyone has. */
  readonly resolvedBy?: string
}
export interface Comment {
  readonly id: string; readonly kind: "comment"; readonly from: number; readonly to: number; readonly body: string; readonly author: string
  /** Base64-encoded Loro cursor anchoring the `from` position. */
  readonly fromCursor?: string
  /** Base64-encoded Loro cursor anchoring the `to` position. */
  readonly toCursor?: string
  readonly status: "open" | "resolved"; readonly orphaned?: boolean
  readonly createdAt: number
  readonly resolvedAt?: number
  readonly resolvedBy?: string
}
export interface ReviewState { readonly items: readonly ReviewItem[]; readonly suggesting: boolean; readonly capability: "available" | "unsupported" }
export type ReviewAction =
  | { readonly type: "toggle-suggesting" }
  | { readonly type: "add"; readonly item: ReviewItem }
  // `by`/`at` stamp resolvedBy/resolvedAt so the audit trail (who accepted/rejected/resolved,
  // and when) is carried by the action — the reducer is pure and cannot ask auth for the user.
  | { readonly type: "accept" | "reject" | "resolve"; readonly id: string; readonly by: string; readonly at: number }
  | { readonly type: "bulk-accept" | "bulk-reject"; readonly ids: readonly string[]; readonly by: string; readonly at: number }
  | { readonly type: "mark-orphans"; readonly textLength: number }

export function reviewReducer(state: ReviewState, action: ReviewAction): ReviewState {
  if (action.type === "toggle-suggesting") return { ...state, suggesting: !state.suggesting }
  if (action.type === "add") return { ...state, items: [...state.items, action.item] }
  // setStatus spreads the item, so createdAt is preserved across a status change; only
  // resolvedBy/resolvedAt are added alongside the new status. A cross-kind
  // transition (accept/reject on a comment, resolve on a suggestion) is a no-op
  // so the item keeps its valid status instead of entering one the UI cannot
  // represent (a Comment has only "open" | "resolved"; a Suggestion has only
  // "open" | "accepted" | "rejected").
  const setStatus = (item: ReviewItem, status: ReviewItem["status"], id: string, by: string, at: number): ReviewItem => {
    if (item.id !== id) return item
    if (status === "resolved" && item.kind !== "comment") return item
    if ((status === "accepted" || status === "rejected") && item.kind !== "suggestion") return item
    return { ...item, status, resolvedBy: by, resolvedAt: at } as ReviewItem
  }
  if (action.type === "accept" || action.type === "reject" || action.type === "resolve") {
    const status = action.type === "resolve" ? "resolved" : action.type === "accept" ? "accepted" : "rejected"
    return { ...state, items: state.items.map((item) => setStatus(item, status, action.id, action.by, action.at)) }
  }
  if (action.type === "bulk-accept" || action.type === "bulk-reject") {
    const ids = new Set(action.ids)
    const status = action.type === "bulk-accept" ? "accepted" : "rejected"
    return { ...state, items: state.items.map((item) => ids.has(item.id) && item.kind === "suggestion" ? { ...item, status, resolvedBy: action.by, resolvedAt: action.at } as ReviewItem : item) }
  }
  if (action.type === "mark-orphans") return { ...state, items: state.items.map((item) => ({ ...item, orphaned: isOrphaned(item, action.textLength) })) }
  return state
}

/**
 * An item is orphaned when its anchor can no longer be drawn: an insert whose range
 * collapsed (the text under it was deleted) or a range that reaches past the end of
 * the document. Zero-width delete suggestions are drawn as anchors, not orphans,
 * because their target text is already gone by design.
 */
export function isOrphaned(item: ReviewItem, textLength: number): boolean {
  return (
    (item.kind === "suggestion" && item.change === "insert" && item.to <= item.from) ||
    item.from > textLength ||
    item.to > textLength
  )
}

export const emptyReviewState: ReviewState = { items: [], suggesting: false, capability: "unsupported" }
