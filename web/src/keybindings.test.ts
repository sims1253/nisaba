import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  DEFAULT_BINDINGS,
  bindingLabel,
  bindingRefusal,
  chordFromEvent,
  clampBindings,
  isBrowserEssential,
  isChord,
  loadBindings,
  prettyChord,
  saveBindings,
} from "./keybindings.js"

const keydown = (key: string, modifiers: { mod?: boolean; alt?: boolean; shift?: boolean } = {}): KeyboardEvent =>
  new KeyboardEvent("keydown", {
    key,
    metaKey: modifiers.mod === true,
    ctrlKey: modifiers.mod === true,
    altKey: modifiers.alt === true,
    shiftKey: modifiers.shift === true,
  })

describe("chordFromEvent", () => {
  it("unifies cmd and ctrl as Mod and orders modifiers", () => {
    expect(chordFromEvent(keydown("k", { mod: true }))).toBe("Mod+K")
    expect(chordFromEvent(keydown("F", { mod: true, shift: true }))).toBe("Mod+Shift+F")
    expect(chordFromEvent(keydown("r", { mod: true, alt: true, shift: true }))).toBe("Mod+Alt+Shift+R")
  })

  it("normalizes letters to uppercase and keeps named keys", () => {
    expect(chordFromEvent(keydown("b", { mod: true }))).toBe("Mod+B")
    expect(chordFromEvent(keydown("Enter", { mod: true }))).toBe("Mod+Enter")
    expect(chordFromEvent(keydown("F12"))).toBe("F12")
  })

  it("ignores pure modifier presses and unusable keys", () => {
    expect(chordFromEvent(keydown("Shift"))).toBeUndefined()
    expect(chordFromEvent(keydown("Meta"))).toBeUndefined()
    expect(chordFromEvent(keydown("+", { mod: true }))).toBeUndefined()
    expect(chordFromEvent(keydown("Escape"))).toBeUndefined()
  })
})

describe("isChord", () => {
  it("accepts well-formed chords and rejects malformed ones", () => {
    expect(isChord("Mod+K")).toBe(true)
    expect(isChord("F9")).toBe(true)
    expect(isChord("Mod+Shift+Enter")).toBe(true)
    expect(isChord("Mod+Mod+K")).toBe(false)
    expect(isChord("Hyper+K")).toBe(false)
    expect(isChord("Mod+")).toBe(false)
    expect(isChord("NotAKey")).toBe(false)
  })
})

describe("browser-essential policy", () => {
  it("lists the reload, devtools, history, and zoom chords", () => {
    for (const chord of ["Mod+R", "Mod+Shift+R", "F5", "F12", "Mod+Shift+I", "Alt+ArrowLeft", "Mod+=", "Mod+-"]) {
      expect(isBrowserEssential(chord)).toBe(true)
    }
    expect(isBrowserEssential("Mod+K")).toBe(false)
  })

  it("bindingRefusal explains essential and duplicate chords", () => {
    expect(bindingRefusal(DEFAULT_BINDINGS, "palette", "Mod+Shift+R")).toContain("browser")
    expect(bindingRefusal(DEFAULT_BINDINGS, "palette", "Mod+S")).toContain("Save")
    expect(bindingRefusal(DEFAULT_BINDINGS, "palette", "Mod+Shift+K")).toBeUndefined()
  })
})

describe("persistence", () => {
  beforeEach(() => {
    vi.stubGlobal("localStorage", memoryStorage())
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it("round-trips valid bindings", () => {
    const next = { ...DEFAULT_BINDINGS, palette: "Mod+Shift+K" }
    saveBindings(next)
    expect(loadBindings()).toEqual(next)
  })

  it("drops essential, duplicate, and malformed entries on load", () => {
    localStorage.setItem("nisaba.keybindings", JSON.stringify({
      palette: "Mod+Shift+R", // browser-essential
      save: "Mod+B",          // duplicate of navigator
      compile: "!!",          // malformed
      focus: "Mod+Shift+L",   // fine
    }))
    expect(loadBindings()).toEqual({ ...DEFAULT_BINDINGS, focus: "Mod+Shift+L" })
  })

  it("returns defaults for corrupt storage", () => {
    localStorage.setItem("nisaba.keybindings", "{nope")
    expect(loadBindings()).toEqual(DEFAULT_BINDINGS)
  })
})

describe("presentation", () => {
  it("labels every action", () => {
    expect(bindingLabel("palette")).toBe("Command palette")
    expect(bindingLabel("navigator")).toContain("sidebar")
  })

  it("prettifies chords without changing the stored form", () => {
    const stored = "Mod+Shift+F"
    const shown = prettyChord(stored)
    expect(["⌘⇧F", "Ctrl+Shift+F"]).toContain(shown)
  })
})

/** Minimal localStorage stand-in (same pattern as settings.test.ts). */
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
