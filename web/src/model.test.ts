import { describe, expect, it } from "vitest"
import type { EditorView, WidgetType } from "@codemirror/view"
import { findConstructs } from "./model"

// The list/citation widgets ignore the `view` argument to toDOM; a bare object is
// enough to satisfy the type and let the widget build its DOM in jsdom.
const dummyView = {} as EditorView
import { computeOrderedListIndices, hybridDecorations, parseTable, type ReferenceDisplay } from "./decorations"

describe("hybrid allowlist", () => {
  it("recognises inline syntax and function-call constructs", () => {
    // `#figure(image)` is detected as a figure mark (in-place styling, not a
    // chip); `#unknown[visible]` is an unknown function call and left as raw text.
    const constructs = findConstructs("= Heading\n*strong* $x$ #figure(image) #unknown[visible]")
    expect(constructs.map((item) => item.kind)).toEqual(["heading", "strong", "math", "figure"])
  })

  describe("function-call constructs detected as in-place marks", () => {
    // Citations, figures, and tables are rendered as rich-text marks (like
    // *bold*) rather than collapsed chips. The source text stays visible and
    // editable. Other function calls (emph, unknown) are still left as raw text.
    it("detects a flat table call", () => {
      expect(findConstructs("#table(columns: 2, [a], [b])")).toEqual([
        expect.objectContaining({ kind: "table", from: 0, to: 28 })
      ])
    })

    it("detects a flat figure call", () => {
      expect(findConstructs("#figure(image, caption: [Beispiel])")).toEqual([
        expect.objectContaining({ kind: "figure", from: 0, to: 35 })
      ])
    })

    it("detects a nested-paren figure call", () => {
      expect(findConstructs("#figure(rect(width: 3cm, fill: red), caption: [Beispiel])")).toEqual([
        expect.objectContaining({ kind: "figure" })
      ])
    })

    it("does not mark an emph call", () => {
      expect(findConstructs("#emph([hi])")).toEqual([])
    })

    it("detects a cite call", () => {
      expect(findConstructs("#cite(<key>)")).toEqual([
        expect.objectContaining({ kind: "citation", from: 0, to: 12 })
      ])
    })

    it("does not chip a custom template-field call", () => {
      expect(findConstructs("#custom-field[value]", new Set(["custom-field"]))).toEqual([])
    })

    it("does not mark an incomplete call typed character-by-character", () => {
      expect(findConstructs("#table(")).toEqual([])
      expect(findConstructs("#figure(")).toEqual([])
    })

    it("detects a citation nested inside a figure caption (both kept)", () => {
      // findFunctionConstructs only matches #-prefixed calls, so a bare
      // `table(` inside `#figure(...)` is NOT separately detected. But a
      // `#cite(...)` that appears inside a figure's caption content block IS
      // detected as a separate construct. deduplicateNested (decorations.ts)
      // resolves the resulting overlap.
      const found = findConstructs('#figure(image("p"), caption: [#cite(<ref>) details])')
      const kinds = found.map((c) => c.kind)
      expect(kinds).toContain("figure")
      expect(kinds).toContain("citation")
      const fig = found.find((c) => c.kind === "figure")!
      const cite = found.find((c) => c.kind === "citation")!
      expect(cite.from).toBeGreaterThan(fig.from)
      expect(cite.to).toBeLessThan(fig.to)
    })

    it("detects a multi-line table call spanning several lines", () => {
      const source = "#table(\n  columns: 2,\n  [a], [b],\n)"
      const found = findConstructs(source)
      expect(found).toEqual([expect.objectContaining({ kind: "table", from: 0 })])
      // The range should cover the closing paren on the last line.
      expect(found[0]!.to).toBe(source.length)
    })

    it("detects multiple citation keys in one call", () => {
      const found = findConstructs("Text #cite(<key1>, <key2>) end")
      expect(found).toEqual([expect.objectContaining({ kind: "citation" })])
      expect(found[0]!.label).toContain("<key1>")
      expect(found[0]!.label).toContain("<key2>")
    })
  })

  describe("inline marks inside list items", () => {
    // deduplicateNested (decorations.ts) must NOT remove marks or rich
    // constructs that are nested inside a list item line. These tests verify
    // that findConstructs still produces them so the decoration layer has the
    // data to render bold/italic/citations inside lists.

    it("keeps strong inside a list item", () => {
      const kinds = findConstructs("- *bold item*").map((c) => c.kind)
      expect(kinds).toContain("list")
      expect(kinds).toContain("strong")
    })

    it("keeps emphasis inside a list item", () => {
      const kinds = findConstructs("- _italic item_").map((c) => c.kind)
      expect(kinds).toContain("list")
      expect(kinds).toContain("emphasis")
    })

    it("keeps a citation inside a list item", () => {
      const found = findConstructs("- See #cite(<key>) for details")
      const kinds = found.map((c) => c.kind)
      expect(kinds).toContain("list")
      expect(kinds).toContain("citation")
      const list = found.find((c) => c.kind === "list")!
      const cite = found.find((c) => c.kind === "citation")!
      expect(cite.from).toBeGreaterThan(list.from)
      expect(cite.to).toBeLessThan(list.to)
    })
  })

  describe("list detection", () => {
    it("detects unordered list items with dash", () => {
      const lists = findConstructs("- first\n- second").filter((c) => c.kind === "list")
      expect(lists).toHaveLength(2)
      expect(lists[0]!.label).toBe("- first")
      expect(lists[1]!.label).toBe("- second")
    })

    it("detects ordered list items with plus", () => {
      const lists = findConstructs("+ first\n+ second").filter((c) => c.kind === "list")
      expect(lists).toHaveLength(2)
      expect(lists.every((l) => l.label?.startsWith("+"))).toBe(true)
    })

    it("does not match a bare dash without a space", () => {
      expect(findConstructs("-dash").filter((c) => c.kind === "list")).toHaveLength(0)
    })

    it("distinguishes ordered from unordered in mixed lists", () => {
      const lists = findConstructs("+ ord1\n- unord1\n+ ord2").filter((c) => c.kind === "list")
      expect(lists).toHaveLength(3)
      expect(lists[0]!.label?.startsWith("+")).toBe(true)
      expect(lists[1]!.label?.startsWith("-")).toBe(true)
      expect(lists[2]!.label?.startsWith("+")).toBe(true)
    })
  })

  describe("construct ordering (the property main.ts's inline lookup relies on)", () => {
    it("sorts constructs outer-first for equal `from`", () => {
      // main.ts looks up "the construct at the cursor" with a plain
      // .find((item) => head >= item.from && head <= item.to), so the FIRST
      // construct covering a position must be the outermost one.
      const found = findConstructs("#figure(table(columns: 2, [a], [b]))")
      expect(found[0]?.kind).toBe("figure")
      expect(found.map((item) => item.from)).toEqual([...found.map((item) => item.from)].sort((a, b) => a - b))
    })
  })

  describe("inline construct boundaries (no false positives)", () => {
    // The `*`/`_`/`$` detection regexes must be context aware so that
    // multiplication, snake_case identifiers, and currency are not styled as
    // strong / emphasis / math, while genuine constructs still match.

    describe("strong", () => {
      it("does not bold spaced multiplication", () => {
        const kinds = findConstructs("2 * 3 * 4").map((c) => c.kind)
        expect(kinds).not.toContain("strong")
      })

      it("does not bold asterisks inside a content block", () => {
        const kinds = findConstructs("[a * b * c]").map((c) => c.kind)
        expect(kinds).not.toContain("strong")
      })

      it("does not bold glued asterisks without a word boundary", () => {
        expect(findConstructs("a*b*c").filter((c) => c.kind === "strong")).toHaveLength(0)
      })

      it("detects strong at the start of text", () => {
        expect(findConstructs("*bold*").map((c) => c.kind)).toContain("strong")
      })

      it("detects strong after whitespace with the correct span", () => {
        const found = findConstructs("see *bold item* here").filter((c) => c.kind === "strong")
        expect(found).toHaveLength(1)
        expect(found[0]!.label).toBe("*bold item*")
      })
    })

    describe("emphasis", () => {
      it("does not italicise snake_case identifiers", () => {
        const kinds = findConstructs("call my_variable_name now").map((c) => c.kind)
        expect(kinds).not.toContain("emphasis")
      })

      it("does not italicise a trailing snake_case token", () => {
        expect(findConstructs("value := my_var").filter((c) => c.kind === "emphasis")).toHaveLength(0)
      })

      it("detects emphasis with word boundaries", () => {
        const found = findConstructs("use _italic words_ here").filter((c) => c.kind === "emphasis")
        expect(found).toHaveLength(1)
        expect(found[0]!.label).toBe("_italic words_")
      })

      it("detects emphasis at the start of text", () => {
        expect(findConstructs("_italics_").map((c) => c.kind)).toContain("emphasis")
      })
    })

    describe("math", () => {
      it("does not render currency as math", () => {
        const kinds = findConstructs("It costs $5 and $10 total").map((c) => c.kind)
        expect(kinds).not.toContain("math")
      })

      it("does not render a single dollar amount as math", () => {
        expect(findConstructs("Price: $42 each").filter((c) => c.kind === "math")).toHaveLength(0)
      })

      it("detects a math span", () => {
        const found = findConstructs("Equation $x^2 + y$ here").filter((c) => c.kind === "math")
        expect(found).toHaveLength(1)
        expect(found[0]!.label).toBe("$x^2 + y$")
      })

      it("detects math that contains digits", () => {
        // Digits inside the span are fine; only a digit adjacent to a `$`
        // delimiter signals currency.
        const found = findConstructs("$2x + 3$").filter((c) => c.kind === "math")
        expect(found).toHaveLength(1)
        expect(found[0]!.label).toBe("$2x + 3$")
      })
    })
  })
})

// ---------------------------------------------------------------------------
// Round 3: edge cases in function-call parsing, nested lists, table parsing,
// citation re-rendering, and findConstructs memoization.
// ---------------------------------------------------------------------------

describe("function-call paren edge cases", () => {
  it("does not truncate on a close paren inside a content block", () => {
    // The `)` inside `[Smiley :)]` must not prematurely close the call and drop
    // the trailing `])`.
    const src = "#figure(caption: [Smiley :)])"
    const fig = findConstructs(src)[0]!
    expect(fig.kind).toBe("figure")
    expect(fig.to).toBe(src.length)
  })

  it("does not truncate on a close paren inside a string literal", () => {
    const src = '#figure(image("a)b"))'
    expect(findConstructs(src)[0]!.to).toBe(src.length)
  })

  it("handles a balanced paren inside a content block normally", () => {
    const src = "#figure(caption: [f(x) is fine])"
    expect(findConstructs(src)[0]!.to).toBe(src.length)
  })

  it("handles an escaped quote inside a string", () => {
    // The Typst string `"a\"b)c"` contains an escaped quote; the `)` after the
    // backslash-escaped quote must not be seen as the string's close.
    const src = '#figure(image("a\\"b)c"))'
    expect(findConstructs(src)[0]!.to).toBe(src.length)
  })

  it("still ignores a genuinely unbalanced call", () => {
    expect(findConstructs("#figure(caption: [oops")).toEqual([])
  })
})

describe("indented / nested list detection", () => {
  it("detects indented list items and strips indentation from the label", () => {
    const lists = findConstructs("- top\n  - inner\n  - inner2\n- top2").filter((c) => c.kind === "list")
    expect(lists).toHaveLength(4)
    expect(lists.map((l) => l.label)).toEqual(["- top", "- inner", "- inner2", "- top2"])
  })

  it("keeps construct.from at the line start even when indented", () => {
    // Line decorations must anchor at a line start, so `from` cannot skip the
    // leading whitespace.
    const src = "  - inner"
    const list = findConstructs(src).find((c) => c.kind === "list")!
    expect(list.from).toBe(0)
    expect(list.to).toBe(src.length)
  })

  it("detects indented ordered items", () => {
    const lists = findConstructs("+ a\n  + b").filter((c) => c.kind === "list")
    expect(lists).toHaveLength(2)
    expect(lists.every((l) => l.label?.startsWith("+"))).toBe(true)
  })
})

describe("computeOrderedListIndices", () => {
  it("numbers nested ordered lists per indentation level", () => {
    const src = "+ a\n  + b\n  + c\n+ d"
    const lists = findConstructs(src).filter((c) => c.kind === "list")
    const idx = computeOrderedListIndices(findConstructs(src), src)
    expect(idx.get(lists[0]!.from)).toBe(1) // a
    expect(idx.get(lists[1]!.from)).toBe(1) // b (new, deeper level)
    expect(idx.get(lists[2]!.from)).toBe(2) // c
    expect(idx.get(lists[3]!.from)).toBe(2) // d (resumes top level)
  })

  it("numbers a flat ordered list sequentially", () => {
    const src = "+ one\n+ two\n+ three"
    const lists = findConstructs(src).filter((c) => c.kind === "list")
    const idx = computeOrderedListIndices(findConstructs(src), src)
    expect(idx.get(lists[0]!.from)).toBe(1)
    expect(idx.get(lists[1]!.from)).toBe(2)
    expect(idx.get(lists[2]!.from)).toBe(3)
  })

  it("resets the counter after a heading", () => {
    const src = "+ one\n+ two\n= Heading\n+ three"
    const lists = findConstructs(src).filter((c) => c.kind === "list")
    const idx = computeOrderedListIndices(findConstructs(src), src)
    expect(idx.get(lists[2]!.from)).toBe(1)
  })

  it("resets the counter after a separating paragraph", () => {
    const src = "+ one\nparagraph\n+ two"
    const lists = findConstructs(src).filter((c) => c.kind === "list")
    const idx = computeOrderedListIndices(findConstructs(src), src)
    expect(idx.get(lists[1]!.from)).toBe(1)
  })

  it("does not index unordered items", () => {
    const src = "- a\n+ b"
    const lists = findConstructs(src).filter((c) => c.kind === "list")
    const idx = computeOrderedListIndices(findConstructs(src), src)
    expect(idx.has(lists[0]!.from)).toBe(false)
    expect(idx.get(lists[1]!.from)).toBe(1)
  })
})

describe("list marker rendering (integration)", () => {
  it("places the marker on the `+ ` characters and numbers per level", () => {
    const src = "+ a\n  + b\n  + c\n+ d"
    const decos = hybridDecorations(findConstructs(src), src, () => {}, [])
    const markers: { from: number; to: number; text: string }[] = []
    decos.between(0, src.length, (f, t, deco) => {
      const widget = deco.spec.widget
      if (!widget) return
      const dom = (widget as WidgetType).toDOM(dummyView)
      if (dom.className.includes("typst-list-marker")) markers.push({ from: f, to: t, text: dom.textContent ?? "" })
    })
    expect(markers).toEqual([
      { from: 0, to: 2, text: "1." }, // + a
      { from: 6, to: 8, text: "1." }, //   + b (indented; marker after 2 spaces)
      { from: 12, to: 14, text: "2." }, //   + c
      { from: 16, to: 18, text: "2." }, // + d
    ])
  })
})

describe("parseTable", () => {
  it("clamps columns: 0 to 1 instead of looping forever", () => {
    // Without the clamp the row-chunking loop steps by 0 and hangs the runner.
    const result = parseTable("#table(columns: 0, [a], [b])")
    expect(result).not.toBeNull()
    expect(result!.columns).toBe(1)
  })

  it("defaults to 2 columns when columns is absent", () => {
    const result = parseTable("#table([h1], [h2], [a], [b])")
    expect(result!.columns).toBe(2)
    expect(result!.headers).toEqual(["h1", "h2"])
    expect(result!.rows).toEqual([["a", "b"]])
  })

  it("captures a nested-bracket cell as a single cell", () => {
    const result = parseTable("#table(columns: 1, [#strong[bold]])")
    expect(result!.headers).toEqual(["#strong[bold]"])
    // No phantom "bold" row from the inner content block.
    expect(result!.rows).toEqual([])
  })

  it("does not create phantom cells from nested content blocks", () => {
    const result = parseTable("#table(columns: 2, [#emph[a]], [#emph[b]])")
    expect(result!.headers).toEqual(["#emph[a]", "#emph[b]"])
    expect(result!.rows).toEqual([])
  })
})

describe("CitationWidget.eq reflects references", () => {
  // The rendered "Author (Year)" chip depends on the references array, so eq
  // must return false when the references identity changes — otherwise an
  // updated bibliography leaves a stale chip in the editor.
  const src = "#cite(<key>)"
  const noop = (): void => {}
  const refs2020: ReferenceDisplay[] = [{ id: "key", authors: ["Doe"], year: 2020, title: "T" }]
  const refs2021: ReferenceDisplay[] = [{ id: "key", authors: ["Doe"], year: 2021, title: "T" }]

  function citationWidget(refs: readonly ReferenceDisplay[]): WidgetType {
    const decos = hybridDecorations(findConstructs(src), src, noop, refs)
    const widgets: WidgetType[] = []
    decos.between(0, src.length, (_f, _t, deco) => {
      if (deco.spec.widget) widgets.push(deco.spec.widget)
    })
    expect(widgets).toHaveLength(1)
    return widgets[0]!
  }

  it("treats identical references as equal", () => {
    expect(citationWidget(refs2020).eq(citationWidget(refs2020))).toBe(true)
  })

  it("treats different references as not equal", () => {
    expect(citationWidget(refs2020).eq(citationWidget(refs2021))).toBe(false)
  })

  it("renders the updated year after a reference change", () => {
    expect(citationWidget(refs2020).toDOM(dummyView).textContent).toContain("2020")
    expect(citationWidget(refs2021).toDOM(dummyView).textContent).toContain("2021")
  })
})

describe("findConstructs memoization", () => {
  it("returns the same array reference for an identical source", () => {
    const a = findConstructs("*bold* and _em_")
    const b = findConstructs("*bold* and _em_")
    expect(a).toBe(b)
  })

  it("returns a different reference when the source changes", () => {
    const a = findConstructs("*a*")
    const b = findConstructs("*b*")
    expect(a).not.toBe(b)
  })

  it("still returns correct results when served from the cache", () => {
    const src = "= H\n*bold* $x$ #cite(<k>)"
    findConstructs(src) // populate the cache
    const result = findConstructs(src)
    expect(result.map((x) => x.kind)).toEqual(["heading", "strong", "math", "citation"])
  })
})