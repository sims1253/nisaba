/**
 * Remote cursors: where collaborators' carets are, drawn in the text.
 *
 * The presence roster (see presence.ts) already carries each peer's name,
 * document, and caret position; the avatars in the app bar show *who* is
 * here. This module is the other half of the Overleaf-style picture: a
 * CodeMirror decoration layer that draws each peer's caret as a thin colored
 * bar with a name flag, colored by the same per-author hue as the avatar
 * chips and review marks, so a peer is recognizably the same color
 * everywhere.
 *
 * The layer is deliberately dumb: it renders whatever cursor list it is
 * given (one {@link setRemoteCursors} dispatch per roster frame) and derives
 * nothing itself — presence semantics, name resolution, and hue assignment
 * stay in main.ts next to their consumers.
 */

import { StateEffect, StateField, type EditorState } from "@codemirror/state"
import { Decoration, type DecorationSet, EditorView, WidgetType } from "@codemirror/view"

/** One peer's caret, already resolved to display form (name + hue). */
export interface RemoteCursor {
  /** Stable identity (CRDT peer id) so widgets survive roster updates. */
  readonly peer: bigint
  /** Display name; may be empty for a peer that published no state. */
  readonly name: string
  /** 1-based line in the CURRENT document. */
  readonly line: number
  /** 1-based column (UTF-16 offset within the line + 1); default 1. */
  readonly column: number
  /** Per-author hue, shared with the avatar chips (0–360). */
  readonly hue: number
}

/** Replace the rendered cursor set. Dispatched once per presence roster. */
export const setRemoteCursors = StateEffect.define<readonly RemoteCursor[]>()
/** Widget: a zero-width caret bar with the peer's name flag above it. */
class RemoteCaretWidget extends WidgetType {
  constructor(
    readonly peer: bigint,
    readonly name: string,
    readonly hue: number,
  ) {
    super()
  }

  override eq(other: RemoteCaretWidget): boolean {
    // Same identity and placement: the DOM node can be reused.
    return other.peer === this.peer && other.name === this.name && other.hue === this.hue
  }

  toDOM(): HTMLElement {
    const host = document.createElement("span")
    host.className = "remote-caret"
    host.style.setProperty("--hue", String(this.hue))
    if (this.name) {
      const label = document.createElement("span")
      label.className = "remote-caret-label"
      label.textContent = this.name
      host.append(label)
    }
    return host
  }

  override ignoreEvent(): boolean {
    return true
  }
}

/**
 * Builds the decorations for a cursor list against a document. Positions are
 * clamped to the document: a peer whose caret is beyond the current text
 * (their copy is newer, or this tab is mid-catch-up) renders at the last
 * valid position rather than throwing the editor out of sync.
 */
function buildDecorations(state: EditorState, cursors: readonly RemoteCursor[]): DecorationSet {
  const ranges = []
  const lastLine = state.doc.lines
  for (const cursor of cursors) {
    const lineNumber = Math.min(Math.max(1, Math.trunc(cursor.line)), lastLine)
    const line = state.doc.line(lineNumber)
    const column = Math.min(Math.max(1, Math.trunc(cursor.column)), line.length + 1)
    const pos = Math.min(line.from + column - 1, line.to)
    ranges.push(
      Decoration.widget({
        widget: new RemoteCaretWidget(cursor.peer, cursor.name, cursor.hue),
        side: -1,
        block: false,
      }).range(pos),
    )
  }
  return Decoration.set(ranges, true)
}

/** Holds the current cursor list and the decorations derived from it. */
export const remoteCursorsField = StateField.define<{
  cursors: readonly RemoteCursor[]
  decorations: DecorationSet
}>({
  create: () => ({ cursors: [], decorations: Decoration.none }),
  update(value, transaction) {
    let cursors = value.cursors
    for (const effect of transaction.effects) {
      if (effect.is(setRemoteCursors)) cursors = effect.value
    }
    // Recompute when the cursor set changed, or when text moved under an
    // unchanged set so carets stay glued to their line/column instead of
    // drifting with shifted offsets.
    const cursorSetChanged = cursors !== value.cursors
    if (cursorSetChanged || (transaction.docChanged && value.cursors.length > 0)) {
      return { cursors, decorations: buildDecorations(transaction.state, cursors) }
    }
    return value
  },
  provide: (field) => EditorView.decorations.from(field, (value) => value.decorations),
})

/** The extension: state + derived decorations. Add once to the editor. */
export const remoteCursors = [remoteCursorsField]
