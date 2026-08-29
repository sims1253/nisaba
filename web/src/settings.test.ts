import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  DEFAULT_SETTINGS,
  MAX_FONT_SIZE,
  MAX_LINE_HEIGHT,
  MIN_FONT_SIZE,
  MIN_LINE_HEIGHT,
  TYPEFACE_STACKS,
  applySettings,
  clampSettings,
  loadSettings,
  saveSettings,
} from "./settings.js"

describe("clampSettings", () => {
  it("returns the defaults for garbage input", () => {
    expect(clampSettings(null)).toEqual(DEFAULT_SETTINGS)
    expect(clampSettings("nope")).toEqual(DEFAULT_SETTINGS)
    expect(clampSettings(42)).toEqual(DEFAULT_SETTINGS)
  })

  it("keeps valid values and falls back per field", () => {
    expect(clampSettings({ typeface: "serif", fontSize: 18, lineHeight: 2 })).toEqual({
      typeface: "serif",
      fontSize: 18,
      lineHeight: 2,
    })
    expect(clampSettings({ typeface: "comic-sans", fontSize: 18, lineHeight: 2 })).toEqual({
      typeface: DEFAULT_SETTINGS.typeface,
      fontSize: 18,
      lineHeight: 2,
    })
    expect(clampSettings({ typeface: "serif", fontSize: "big", lineHeight: 2 })).toEqual({
      typeface: "serif",
      fontSize: DEFAULT_SETTINGS.fontSize,
      lineHeight: 2,
    })
  })

  it("clamps out-of-range numbers to the limits", () => {
    expect(clampSettings({ fontSize: 4, lineHeight: 0.5 }).fontSize).toBe(MIN_FONT_SIZE)
    expect(clampSettings({ fontSize: 200, lineHeight: 9 }).fontSize).toBe(MAX_FONT_SIZE)
    expect(clampSettings({ fontSize: 14, lineHeight: 0.5 }).lineHeight).toBe(MIN_LINE_HEIGHT)
    expect(clampSettings({ fontSize: 14, lineHeight: 9 }).lineHeight).toBe(MAX_LINE_HEIGHT)
  })

  it("treats non-finite numbers as missing", () => {
    expect(clampSettings({ fontSize: Number.NaN }).fontSize).toBe(DEFAULT_SETTINGS.fontSize)
    expect(clampSettings({ fontSize: Number.POSITIVE_INFINITY }).fontSize).toBe(DEFAULT_SETTINGS.fontSize)
  })
})

describe("persistence", () => {
  // The suite's jsdom under Bun leaves the storage global undefined (same
  // caveat as auth.test.ts), so these tests run against a memory stub.
  beforeEach(() => {
    vi.stubGlobal("localStorage", memoryStorage())
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it("round-trips through localStorage", () => {
    const settings = clampSettings({ typeface: "sans", fontSize: 20, lineHeight: 1.4 })
    saveSettings(settings)
    expect(loadSettings()).toEqual(settings)
  })

  it("returns defaults when nothing is stored", () => {
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS)
  })

  it("sanitizes corrupt storage on load", () => {
    localStorage.setItem("nisaba.settings", "{not json")
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS)
    localStorage.setItem("nisaba.settings", JSON.stringify({ typeface: "zap", fontSize: 999 }))
    expect(loadSettings()).toEqual({ ...DEFAULT_SETTINGS, fontSize: MAX_FONT_SIZE })
  })
})

/** Minimal localStorage stand-in: enough for get/set/remove by key. */
function memoryStorage(): Storage {
  const map = new Map<string, string>()
  return {
    get length() { return map.size },
    clear: () => map.clear(),
    getItem: (key: string) => map.get(key) ?? null,
    key: (index: number) => [...map.keys()][index] ?? null,
    removeItem: (key: string) => { map.delete(key) },
    setItem: (key: string, value: string) => { map.set(key, String(value)) },
  } as Storage
}

describe("applySettings", () => {
  it("sets the editor typography variables on the document root", () => {
    applySettings(clampSettings({ typeface: "serif", fontSize: 17, lineHeight: 1.5 }))
    const root = document.documentElement
    expect(root.style.getPropertyValue("--ed-font")).toBe(TYPEFACE_STACKS.serif)
    expect(root.style.getPropertyValue("--ed-size")).toBe("17px")
    expect(root.style.getPropertyValue("--ed-line")).toBe("1.5")
  })
})
