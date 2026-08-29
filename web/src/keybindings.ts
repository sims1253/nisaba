/**
 * Rebindable app chords: which keys trigger the app-level actions.
 *
 * The policy from ui-design.md §6 is enforced here, not just documented:
 * chords the browser needs — the reload family, devtools, history — can
 * never be stored as a binding (isBrowserEssential), so neither the shipped
 * defaults nor a user's rebind can hijack them. The capture UI refuses them
 * with a reason; load-time validation drops any that slipped past (a corrupt
 * record degrades to the default chord, per the settings system's rule).
 *
 * Chord syntax: `Mod+Alt+Shift+Key` where `Mod` unifies Cmd (macOS) and
 * Ctrl (others) — a binding recorded on one platform works on the other.
 * Keys are single characters (letters stored uppercase) or named specials
 * (Enter, F1–F12, arrows). Pure modifier presses are not chords.
 */

export type BindingAction = "palette" | "compile" | "save" | "focus" | "navigator"

export interface Keybindings {
  readonly palette: string
  readonly compile: string
  readonly save: string
  readonly focus: string
  readonly navigator: string
}

export const DEFAULT_BINDINGS: Keybindings = {
  palette: "Mod+K",
  compile: "Mod+Enter",
  save: "Mod+S",
  focus: "Mod+Shift+F",
  navigator: "Mod+B",
}

const BINDINGS_KEY = "nisaba.keybindings"

export const BINDING_ACTIONS: readonly BindingAction[] = ["palette", "compile", "save", "focus", "navigator"]

/** Chords the browser owns; a binding may never take one (§6 policy). */
const BROWSER_ESSENTIAL: ReadonlySet<string> = new Set([
  // Reload family.
  "Mod+R", "Mod+Shift+R", "F5", "Mod+F5", "Shift+F5", "Mod+Shift+F5",
  // Devtools family.
  "F12", "Mod+Shift+I", "Mod+Shift+J", "Mod+Shift+C", "Mod+Alt+I",
  // History navigation.
  "Alt+ArrowLeft", "Alt+ArrowRight", "Mod+[", "Mod+]",
  // Browser zoom (the preview pane listens for these while the pointer is
  // over it; binding an app action to them would fight both the browser
  // and that scoped handler). "+" itself is unrepresentable in chord
  // strings (split ambiguity) — "=" is the same physical key.
  "Mod+=", "Mod+-",
])

export function isBrowserEssential(chord: string): boolean {
  return BROWSER_ESSENTIAL.has(chord)
}

/** Normalizes a key name: single characters uppercased, specials verbatim. */
const normalizeKey = (key: string): string | undefined => {
  // "+" cannot be encoded in a chord string ("Mod++" splits ambiguously);
  // "=" is the same physical key and validates normally.
  if (key === "+") return undefined
  if (key.length === 1) return key.toUpperCase()
  if (/^F([1-9]|1[0-2])$/.test(key)) return key
  if (["Enter", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Tab", "Backspace"].includes(key)) return key
  return undefined
}

/** Builds the chord string a keyboard event represents; undefined for noise. */
export function chordFromEvent(event: KeyboardEvent): string | undefined {
  // Pure modifier presses are chord fragments, not chords.
  if (["Shift", "Meta", "Ctrl", "Control", "Alt"].includes(event.key)) return undefined
  const key = normalizeKey(event.key)
  if (key === undefined) return undefined
  const mod = event.metaKey || event.ctrlKey
  const parts: string[] = []
  if (mod) parts.push("Mod")
  if (event.altKey) parts.push("Alt")
  if (event.shiftKey) parts.push("Shift")
  parts.push(key)
  return parts.join("+")
}

/** A well-formed stored chord: unique modifiers, then a valid key. Public
 * for tests: the validation the capture UI and load-time clamp share. */
export const isChord = (value: string): boolean => {
  const parts = value.split("+")
  const key = parts.pop()
  if (key === undefined || normalizeKey(key) !== key) return false
  const modifiers = new Set(parts)
  if (modifiers.size !== parts.length) return false
  for (const modifier of modifiers) {
    if (!["Mod", "Alt", "Shift"].includes(modifier)) return false
  }
  return true
}

/** Parses and validates a stored bindings record, falling back per action. */
export function clampBindings(raw: unknown): Keybindings {
  const record = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {}
  // Pass 1: keep the well-formed, non-essential customs. Modifierless
  // chords are invalid bindings for the same reason bindingRefusal gives:
  // a bare key as a global binding is untypeable app-wide.
  const customs = new Map<BindingAction, string>()
  for (const action of BINDING_ACTIONS) {
    const value = record[action]
    if (typeof value !== "string" || !isChord(value) || isBrowserEssential(value)) continue
    if (!value.startsWith("Mod") && !/^F([1-9]|1[0-2])$/.test(value)) continue
    customs.set(action, value)
  }
  const result = { ...DEFAULT_BINDINGS }
  for (const [action, chord] of customs) result[action] = chord
  // Pass 2: a custom must not collide with ANY other action's final chord —
  // custom or retained default (a stored "Mod+B" for save otherwise slips
  // past navigator's default). Dropping reverts to the default, which can
  // itself collide, so iterate to a fixpoint.
  for (let round = 0; round < BINDING_ACTIONS.length; round += 1) {
    const drops: BindingAction[] = []
    for (const [action, chord] of customs) {
      if (result[action] !== chord) continue
      if (BINDING_ACTIONS.some((other) => other !== action && result[other] === chord)) drops.push(action)
    }
    if (drops.length === 0) break
    for (const action of drops) {
      customs.delete(action)
      result[action] = DEFAULT_BINDINGS[action]
    }
  }
  return result
}

export function loadBindings(): Keybindings {
  try {
    return clampBindings(JSON.parse(localStorage.getItem(BINDINGS_KEY) ?? "null"))
  } catch {
    return DEFAULT_BINDINGS
  }
}

export function saveBindings(bindings: Keybindings): void {
  try {
    localStorage.setItem(BINDINGS_KEY, JSON.stringify(bindings))
  } catch {
    /* storage may be unavailable — the session still uses the values */
  }
}

// The live bindings for this session. Owned here (not in main.ts) so any
// module that renders a chord — the compile button's rebuilt kbd, palette
// hints — reads what is actually bound instead of a stale copy.
let liveBindings: Keybindings | undefined

/** The bindings in effect right now (loaded on first use). */
export function currentBindings(): Keybindings {
  liveBindings ??= loadBindings()
  return liveBindings
}

/** Updates the live bindings and persists them. */
export function commitBindings(next: Keybindings): void {
  liveBindings = next
  saveBindings(next)
}

/**
 * Whether a binding may be taken: not browser-essential, not modifierless
 * (a bare letter/space/enter as a global binding would make that key
 * untypeable app-wide — chords need Mod, with bare F-keys as the one
 * exception), and not already used by another action. Returns undefined
 * when allowed, else the refusal reason.
 */
export function bindingRefusal(bindings: Keybindings, action: BindingAction, chord: string): string | undefined {
  if (isBrowserEssential(chord)) {
    return "That chord belongs to the browser (reload, devtools, history, or zoom) and stays available"
  }
  if (!chord.startsWith("Mod") && !/^F([1-9]|1[0-2])$/.test(chord)) {
    return "Chords need Cmd/Ctrl (or a bare F-key) — a bare key would become untypeable"
  }
  for (const other of BINDING_ACTIONS) {
    if (other !== action && bindings[other] === chord) {
      return `Already used by “${bindingLabel(other)}”`
    }
  }
  return undefined
}

/** Action names as the interface says them. */
export function bindingLabel(action: BindingAction): string {
  switch (action) {
    case "palette": return "Command palette"
    case "compile": return "Update preview"
    case "save": return "Save and update preview"
    case "focus": return "Focus mode"
    case "navigator": return "Show or hide the sidebar"
  }
}

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent)

/** Displays a chord the way this platform writes it (⌘⇧F on macOS, Ctrl+Shift+F else). */
export function prettyChord(chord: string): string {
  const joiner = isMac ? "" : "+"
  return chord
    .split("+")
    .map((part) => {
      if (part === "Mod") return isMac ? "⌘" : "Ctrl"
      if (part === "Shift") return isMac ? "⇧" : "Shift"
      if (part === "Alt") return isMac ? "⌥" : "Alt"
      return part
    })
    .join(joiner)
}
