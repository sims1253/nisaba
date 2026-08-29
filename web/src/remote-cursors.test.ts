import { describe, expect, it } from "vitest"
import { EditorState } from "@codemirror/state"
import { Decoration } from "@codemirror/view"
import { remoteCursors, remoteCursorsField, setRemoteCursors, type RemoteCursor } from "./remote-cursors.js"

const doc = "one\ntwo words\nthree\n"

const cursor = (over: Partial<RemoteCursor> & { peer: bigint }): RemoteCursor => ({
  name: "Ada",
  line: 1,
  column: 1,
  hue: 210,
  ...over,
})

/** Decoration start offsets of the current cursor set, in document order. */
const caretPositions = (state: EditorState): number[] => {
  const out: number[] = []
  const cursor = state.field(remoteCursorsField).decorations.iter()
  while (cursor.value !== null) {
    out.push(cursor.from)
    cursor.next()
  }
  return out
}

const stateWith = (cursors: readonly RemoteCursor[]): EditorState =>
  EditorState.create({ doc, extensions: remoteCursors }).update({
    effects: setRemoteCursors.of(cursors),
  }).state

describe("remote cursors", () => {
  it("places a caret widget at the peer's line and column", () => {
    // "two words": column 5 lands on the 'w' → doc offset 4 + 4 = 8.
    expect(caretPositions(stateWith([cursor({ peer: 1n, line: 2, column: 5 })]))).toEqual([8])
  })

  it("renders multiple peers sorted by position", () => {
    expect(
      caretPositions(
        stateWith([
          cursor({ peer: 2n, name: "Bo", line: 3, column: 1 }),
          cursor({ peer: 1n, line: 1, column: 1 }),
        ]),
      ),
    ).toEqual([0, 14])
  })

  it("clamps positions beyond the document instead of throwing", () => {
    // Line 99 of a 4-line doc clamps to the final (empty) line, start offset 20.
    expect(caretPositions(stateWith([cursor({ peer: 1n, line: 99, column: 50 })]))).toEqual([20])
  })

  it("clears when an empty roster arrives", () => {
    expect(caretPositions(stateWith([]))).toEqual([])
  })

  it("keeps carets glued to their line when text above changes", () => {
    let state = stateWith([cursor({ peer: 1n, line: 2, column: 5 })])
    expect(caretPositions(state)).toEqual([8])
    // Lengthening line 1 shifts line 2 down by 2: the same line-2/column-5
    // caret renders at its line's new start (6) + 4.
    state = state.update({ changes: { from: 3, insert: "XX" } }).state
    expect(caretPositions(state)).toEqual([10])
  })

  it("builds widget decorations", () => {
    const state = stateWith([cursor({ peer: 1n })])
    const iter = state.field(remoteCursorsField).decorations.iter()
    expect(iter.value).toBeInstanceOf(Decoration)
    expect(iter.from).toBe(0)
  })
})
