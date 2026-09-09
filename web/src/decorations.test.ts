import { EditorState, StateEffect } from "@codemirror/state"
import { describe, expect, it } from "vitest"
import { hybridEditorField } from "./decorations"

describe("hybrid source visibility", () => {
  const field = hybridEditorField(() => {}, () => [])
  const source = 'Before #figure(image("a.png")) after'
  const inside = source.indexOf("image")

  function create(anchor = 0): EditorState {
    return EditorState.create({ doc: source, selection: { anchor }, extensions: [field] })
  }

  function widgets(state: EditorState): number {
    let count = 0
    state.field(field).between(0, state.doc.length, (_from, _to, decoration) => {
      if (decoration.spec.widget) count++
    })
    return count
  }

  it("reveals on entry and restores the widget on exit in the selection transaction", () => {
    let state = create()
    expect(widgets(state)).toBe(1)
    state = state.update({ selection: { anchor: inside } }).state
    expect(widgets(state)).toBe(0)
    state = state.update({ selection: { anchor: source.length } }).state
    expect(widgets(state)).toBe(1)
  })

  it("reveals the source at the initial cursor", () => {
    expect(widgets(create(inside))).toBe(0)
  })

  it("keeps source visible while typing and after an unrelated effect", () => {
    let state = create(inside)
    state = state.update({ changes: { from: inside, insert: "new" }, selection: { anchor: inside + 3 } }).state
    expect(widgets(state)).toBe(0)
    state = state.update({ effects: StateEffect.define<null>().of(null) }).state
    expect(widgets(state)).toBe(0)
  })

  it("uses the new ranges when text and selection change together", () => {
    const prefix = "A long inserted paragraph. "
    const state = create().update({
      changes: { from: 0, insert: prefix },
      selection: { anchor: prefix.length + inside }
    }).state
    expect(widgets(state)).toBe(0)
  })

  it("tracks the mapped cursor after an edit before the construct", () => {
    const state = create(inside).update({ changes: { from: 0, insert: "More text. " } }).state
    expect(widgets(state)).toBe(0)
  })
})
