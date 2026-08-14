import { afterEach, describe, expect, it } from "vitest"
import { EditorState } from "@codemirror/state"
import { EditorView } from "@codemirror/view"
import { LoroExtensions, redo, undo } from "loro-codemirror"
import { LoroDoc, LoroText, UndoManager } from "loro-crdt"

const views: EditorView[] = []
afterEach(() => {
  while (views.length > 0) views.pop()?.destroy()
})

function boundEditor(beforeCommit?: () => void): { view: EditorView; doc: LoroDoc; undoManager: UndoManager } {
  const doc = new LoroDoc()
  const undoManager = new UndoManager(doc, { mergeInterval: 0 })
  const view = new EditorView({
    state: EditorState.create({
      extensions: [LoroExtensions(
        doc,
        undefined,
        undoManager,
        (current) => current.getText("text") as LoroText,
        beforeCommit
      )]
    }),
    parent: document.body.appendChild(document.createElement("div"))
  })
  views.push(view)
  return { view, doc, undoManager }
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0))
}

describe("collaborative undo adapter", () => {
  it("redo restores a text edit committed with review metadata", async () => {
    let doc!: LoroDoc
    const bound = boundEditor(() => {
      doc.getMap("review").set("item", "metadata")
    })
    doc = bound.doc
    await Promise.resolve()
    bound.view.dispatch({ selection: { anchor: 0 } })

    bound.view.dispatch({ changes: { from: 0, insert: "redo me" } })
    await settle()
    expect(bound.view.state.doc.toString()).toBe("redo me")
    expect(bound.undoManager.canUndo()).toBe(true)

    undo(bound.view)
    await settle()
    expect(bound.view.state.doc.toString()).toBe("")

    redo(bound.view)
    await settle()
    expect(bound.view.state.doc.toString()).toBe("redo me")
    expect(doc.getText("text").toString()).toBe("redo me")
  })
})
