/**
 * Unicode scalar ↔ UTF-16 code unit conversion.
 *
 * Rust `Position` counts Unicode scalar values; CodeMirror offsets are UTF-16
 * code units. Astral-plane characters (e.g. emoji, mathematical symbols) take
 * TWO UTF-16 code units (a surrogate pair) but ONE Unicode scalar value.
 *
 * `Position::to_utf16` and `Position::from_utf16` bridge this gap. This test verifies the conversion logic at the JavaScript level, matching the
 * Rust implementation's semantics.
 */
import { describe, it, expect } from "vitest"

describe("Unicode scalar ↔ UTF-16 conversion", () => {
  /**
   * Helper: convert a Unicode scalar position to a UTF-16 code unit offset,
   * matching the Rust Position::to_utf16 semantics.
   */
  function scalarToUtf16(text: string, scalarPos: number): number {
    let utf16 = 0
    let scalars = 0
    for (const ch of text) {
      if (scalars === scalarPos) return utf16
      scalars++
      utf16 += ch.length // JS string length per char = UTF-16 code units
    }
    return utf16
  }

  it("astral characters: scalar position 2 maps to UTF-16 offset 3", () => {
    const text = "a𝕏b" // 𝕏 is U+1D54F (supplementary plane)
    expect(scalarToUtf16(text, 0)).toBe(0) // 'a'
    expect(scalarToUtf16(text, 1)).toBe(1) // start of 𝕏
    expect(scalarToUtf16(text, 2)).toBe(3) // 'b' — not 2
    expect(scalarToUtf16(text, 3)).toBe(4) // trailing
  })

  it("naive passthrough (scalar == utf16) is wrong for astral text", () => {
    const text = "a𝕏b"
    const naiveOffset = 2
    const correctOffset = scalarToUtf16(text, 2)
    expect(correctOffset).not.toBe(naiveOffset) // a naive scalar offset points into the surrogate pair
    expect(text.charCodeAt(correctOffset)).toBe(text.charCodeAt(text.indexOf("b")))
  })

  it("multiple astral characters compound the drift", () => {
    const text = "😀hello😀"
    expect([...text].length).toBe(7) // 7 scalars
    expect(text.length).toBe(9) // 9 UTF-16 code units
    expect(scalarToUtf16(text, 1)).toBe(2) // 'h'
    expect(scalarToUtf16(text, 6)).toBe(7) // second 😀
  })

  it("scalar-to-utf16 is identity for BMP-only text", () => {
    const text = "café" // all BMP characters
    for (let i = 0; i <= [...text].length; i++) {
      expect(scalarToUtf16(text, i)).toBe(i)
    }
  })

  it("combining marks do not cause drift (each is its own scalar)", () => {
    const decomposed = "e\u0301" // 'e' + combining acute — two scalars
    expect([...decomposed].length).toBe(2)
    expect(scalarToUtf16(decomposed, 0)).toBe(0)
    expect(scalarToUtf16(decomposed, 1)).toBe(1)
    expect(scalarToUtf16(decomposed, 2)).toBe(2)
  })
})
