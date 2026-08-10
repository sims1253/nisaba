/**
 * Navigator structure: the file tree and the open document's section outline.
 *
 * Both are derived, never stored. A project is a flat set of path-addressed
 * documents (see PLAN.md, "Projects contain files"), so folders exist only
 * because paths contain slashes — `buildFileTree` recovers that hierarchy for
 * display without the model growing a folder concept. Section headings are
 * parsed from the live source so the outline tracks typing, rather than only
 * refreshing when the compile service returns its own outline.
 *
 * Everything here is pure so it can be unit-tested without a DOM.
 */
import { findConstructs } from "./model"

// ---------------------------------------------------------------------------
// File tree
// ---------------------------------------------------------------------------

export interface TreeFile<T> {
  readonly type: "file"
  /** Last path segment, e.g. `intro.typ`. */
  readonly name: string
  /** Full project-relative path, e.g. `chapters/intro.typ`. */
  readonly path: string
  readonly item: T
}

export interface TreeFolder<T> {
  readonly type: "folder"
  /** Folder segment name, e.g. `chapters`. */
  readonly name: string
  /** Path of the folder itself, e.g. `chapters` or `parts/appendix`. */
  readonly path: string
  readonly children: readonly TreeNode<T>[]
}

export type TreeNode<T> = TreeFile<T> | TreeFolder<T>

interface MutableFolder<T> {
  readonly type: "folder"
  readonly name: string
  readonly path: string
  readonly children: TreeNode<T>[]
}

/**
 * Sort order inside a folder: folders first, then files, each case-insensitively
 * and numerically aware (`part2` before `part10`), so a tree with `chapter-2`
 * and `chapter-10` reads the way an author numbered it.
 */
function compareNodes<T>(a: TreeNode<T>, b: TreeNode<T>): number {
  if (a.type !== b.type) return a.type === "folder" ? -1 : 1
  return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" })
}

function sortTree<T>(nodes: TreeNode<T>[]): TreeNode<T>[] {
  nodes.sort(compareNodes)
  for (const node of nodes) {
    if (node.type === "folder") sortTree(node.children as TreeNode<T>[])
  }
  return nodes
}

/**
 * Groups path-addressed entries into a folder tree.
 *
 * Empty segments (`a//b.typ`), leading slashes, and `.` segments are ignored so
 * a sloppy path still lands somewhere sensible instead of producing a nameless
 * folder. An entry whose path is only a folder-ish string (`chapters/`) is
 * treated as a file named after its last real segment — the model has no
 * directories, so there is nothing else it could mean.
 */
export function buildFileTree<T>(entries: readonly { readonly path: string; readonly item: T }[]): readonly TreeNode<T>[] {
  const roots: TreeNode<T>[] = []
  const folders = new Map<string, MutableFolder<T>>()

  const folderAt = (segments: readonly string[]): TreeNode<T>[] => {
    let parent = roots
    let prefix = ""
    for (const segment of segments) {
      prefix = prefix === "" ? segment : `${prefix}/${segment}`
      let folder = folders.get(prefix)
      if (!folder) {
        folder = { type: "folder", name: segment, path: prefix, children: [] }
        folders.set(prefix, folder)
        parent.push(folder as TreeFolder<T>)
      }
      parent = folder.children
    }
    return parent
  }

  for (const entry of entries) {
    const segments = entry.path.split("/").filter((segment) => segment !== "" && segment !== ".")
    const name = segments.at(-1)
    if (name === undefined) continue
    folderAt(segments.slice(0, -1)).push({ type: "file", name, path: entry.path, item: entry.item })
  }
  return sortTree(roots)
}

/** Every folder path in a tree, for expand/collapse bookkeeping. */
export function folderPaths<T>(nodes: readonly TreeNode<T>[]): readonly string[] {
  const out: string[] = []
  const walk = (list: readonly TreeNode<T>[]): void => {
    for (const node of list) {
      if (node.type === "folder") {
        out.push(node.path)
        walk(node.children)
      }
    }
  }
  walk(nodes)
  return out
}

// ---------------------------------------------------------------------------
// Section outline
// ---------------------------------------------------------------------------

export interface Heading {
  /** 1 for `= Title`, 2 for `== Title`, … */
  readonly level: number
  readonly title: string
  /** Character offset of the heading line's start. */
  readonly from: number
  /** Character offset just past the heading line. */
  readonly to: number
}

/**
 * The open document's headings, in document order.
 *
 * Parsed via `findConstructs` (the same parse the editor decorations use, and
 * memoised on the source string) so the outline can never disagree with what the
 * editor renders as a heading.
 */
export function documentHeadings(source: string): readonly Heading[] {
  const headings: Heading[] = []
  for (const construct of findConstructs(source)) {
    if (construct.kind !== "heading") continue
    const text = construct.label ?? source.slice(construct.from, construct.to)
    const level = text.length - text.replace(/^=+/, "").length
    const title = text.replace(/^=+/, "").trim()
    if (level === 0 || title === "") continue
    headings.push({ level, title, from: construct.from, to: construct.to })
  }
  return headings
}

/**
 * Index of the heading the caret sits under, or -1 above the first heading.
 * Used by the outline highlight, the sticky heading, and the breadcrumb.
 */
export function activeHeadingIndex(headings: readonly Heading[], position: number): number {
  let index = -1
  for (let i = 0; i < headings.length; i++) {
    const heading = headings[i]
    if (heading === undefined || heading.from > position) break
    index = i
  }
  return index
}

/**
 * The heading trail for a position — the active heading plus each shallower
 * ancestor above it, outermost first, e.g. `[Results, Acoustic analysis]`.
 */
export function headingTrail(headings: readonly Heading[], position: number): readonly Heading[] {
  const index = activeHeadingIndex(headings, position)
  if (index < 0) return []
  const trail: Heading[] = []
  let level = Number.POSITIVE_INFINITY
  for (let i = index; i >= 0; i--) {
    const heading = headings[i]
    if (heading === undefined || heading.level >= level) continue
    trail.unshift(heading)
    level = heading.level
    if (level === 1) break
  }
  return trail
}

// ---------------------------------------------------------------------------
// Word count
// ---------------------------------------------------------------------------

/**
 * Approximate word count of Typst source, for the status bar.
 *
 * Markup that is not prose is stripped first — set rules and other `#…` lines,
 * heading/list markers, math, code, and the delimiters around emphasis — so the
 * number tracks what a writer would count rather than the source's token count.
 * It is deliberately an estimate: an exact count would need the projected text
 * from the compile service, which is not available while typing.
 */
export function wordCount(source: string): number {
  const prose = source
    // Fenced raw blocks and inline code carry no prose.
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`[^`\n]*`/g, " ")
    // Math is read, not counted as words.
    .replace(/\$[^$]*\$/g, " ")
    // Whole-line Typst directives (`#set …`, `#import …`, `#figure(…)`).
    .replace(/^[ \t]*#[^\n]*$/gm, " ")
    // Leading heading and list markers.
    .replace(/^[ \t]*(=+|[-+*]|\d+\.)[ \t]+/gm, " ")
    // Label and reference tokens.
    .replace(/[<@][\w:.-]+>?/g, " ")
    // Emphasis delimiters, leaving the words they wrap.
    .replace(/[*_]/g, " ")
  const words = prose.match(/[\p{L}\p{N}][\p{L}\p{N}'’-]*/gu)
  return words ? words.length : 0
}
