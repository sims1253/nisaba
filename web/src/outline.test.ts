import { describe, expect, it } from "vitest"
import { activeHeadingIndex, buildFileTree, documentHeadings, folderPaths, headingTrail, wordCount } from "./outline"

const paths = (list: readonly string[]) => list.map((path) => ({ path, item: path }))

describe("buildFileTree", () => {
  it("derives folders from path segments", () => {
    const tree = buildFileTree(paths(["main.typ", "chapters/intro.typ", "chapters/results.typ"]))
    expect(tree.map((node) => `${node.type}:${node.name}`)).toEqual(["folder:chapters", "file:main.typ"])
    const chapters = tree[0]
    if (chapters?.type !== "folder") throw new Error("expected a folder")
    expect(chapters.children.map((node) => node.name)).toEqual(["intro.typ", "results.typ"])
  })

  it("nests deeply and keeps full paths on files", () => {
    const tree = buildFileTree(paths(["parts/one/a.typ"]))
    const parts = tree[0]
    if (parts?.type !== "folder") throw new Error("expected a folder")
    const one = parts.children[0]
    if (one?.type !== "folder") throw new Error("expected a nested folder")
    expect(one.path).toBe("parts/one")
    expect(one.children[0]).toMatchObject({ type: "file", name: "a.typ", path: "parts/one/a.typ" })
  })

  it("sorts folders before files, numerically", () => {
    const tree = buildFileTree(paths(["chapter-10.typ", "chapter-2.typ", "zz/a.typ"]))
    expect(tree.map((node) => node.name)).toEqual(["zz", "chapter-2.typ", "chapter-10.typ"])
  })

  it("ignores empty and dot segments instead of creating nameless folders", () => {
    const tree = buildFileTree(paths(["/a.typ", "./b.typ", "x//y.typ"]))
    expect(tree.map((node) => `${node.type}:${node.name}`)).toEqual(["folder:x", "file:a.typ", "file:b.typ"])
  })

  it("lists every folder path", () => {
    const tree = buildFileTree(paths(["a/b/c.typ", "d/e.typ"]))
    expect([...folderPaths(tree)].sort()).toEqual(["a", "a/b", "d"])
  })
})

describe("documentHeadings", () => {
  const source = [
    "#set page(paper: \"a4\")",
    "= Title",
    "Some prose.",
    "== Methods",
    "More prose.",
    "=== Stations",
    "== Results"
  ].join("\n")

  it("extracts level and title", () => {
    const headings = documentHeadings(source)
    expect(headings.map((heading) => `${heading.level}:${heading.title}`)).toEqual([
      "1:Title",
      "2:Methods",
      "3:Stations",
      "2:Results"
    ])
  })

  it("points at the heading line offsets", () => {
    const headings = documentHeadings(source)
    const first = headings[0]
    if (!first) throw new Error("expected a heading")
    expect(source.slice(first.from, first.to)).toBe("= Title")
  })

  it("finds the heading a position sits under", () => {
    const headings = documentHeadings(source)
    expect(activeHeadingIndex(headings, 0)).toBe(-1)
    const methods = headings[1]
    if (!methods) throw new Error("expected a heading")
    expect(activeHeadingIndex(headings, methods.from + 3)).toBe(1)
    expect(activeHeadingIndex(headings, source.length)).toBe(3)
  })

  it("builds the ancestor trail for the sticky heading", () => {
    const headings = documentHeadings(source)
    const stations = headings[2]
    if (!stations) throw new Error("expected a heading")
    expect(headingTrail(headings, stations.from).map((heading) => heading.title)).toEqual([
      "Title",
      "Methods",
      "Stations"
    ])
    expect(headingTrail(headings, 0)).toEqual([])
  })
})

describe("wordCount", () => {
  it("counts prose words", () => {
    expect(wordCount("The honey badger attended the rave.")).toBe(6)
  })

  it("ignores Typst directives, markers, math and code", () => {
    const source = [
      "#set text(size: 10pt)",
      "= Heading here",
      "- one item",
      "Body with $x + y$ math and `code` and *bold* words."
    ].join("\n")
    // "Heading here" (2) + "one item" (2) + "Body with math and and bold words" (7)
    expect(wordCount(source)).toBe(11)
  })

  it("counts hyphenated and apostrophed words once", () => {
    expect(wordCount("well-known don't cases")).toBe(3)
  })

  it("is zero for an empty document", () => {
    expect(wordCount("")).toBe(0)
  })
})
