/**
 * User settings: editor typography and other local preferences.
 *
 * Deliberately client-side (localStorage): these are reader/writer comforts
 * for this browser, not project data — a collaborator's serif preference
 * should not leak into anyone else's editor or into the compiled output.
 * Server-owned settings (a project's default file, sharing policy) live on
 * the API instead and never pass through here.
 *
 * Every field is validated on load: unknown or out-of-range values fall back
 * per-field to the defaults, so a corrupt record (or a future schema) can
 * never wedge the editor — worst case is the default look.
 */

export type TypefaceId = "mono" | "serif" | "sans"

export interface Settings {
  /** Editor typeface; the stacks themselves are the stylesheet's variables. */
  readonly typeface: TypefaceId
  /** Editor font size in px. */
  readonly fontSize: number
  /** Editor line height (unitless). */
  readonly lineHeight: number
}

export const DEFAULT_SETTINGS: Settings = {
  typeface: "mono",
  fontSize: 14,
  lineHeight: 1.75,
}

export const MIN_FONT_SIZE = 12
export const MAX_FONT_SIZE = 24
export const MIN_LINE_HEIGHT = 1.2
export const MAX_LINE_HEIGHT = 2.2

/** The CSS font stack each typeface maps to (kept in sync with styles.css). */
export const TYPEFACE_STACKS: Readonly<Record<TypefaceId, string>> = {
  mono: "var(--mono)",
  serif: "var(--serif)",
  sans: "var(--sans)",
}

export const TYPEFACE_LABELS: Readonly<Record<TypefaceId, string>> = {
  mono: "Mono",
  serif: "Serif",
  sans: "Sans",
}

const SETTINGS_KEY = "nisaba.settings"

const clampNumber = (value: unknown, min: number, max: number, fallback: number): number => {
  const n = typeof value === "number" && Number.isFinite(value) ? value : fallback
  return Math.min(max, Math.max(min, n))
}

/** Validates an unknown parsed record into safe settings, field by field. */
export function clampSettings(raw: unknown): Settings {
  const record = typeof raw === "object" && raw !== null ? (raw as Record<string, unknown>) : {}
  const typeface = record["typeface"]
  return {
    typeface: typeface === "serif" || typeface === "sans" || typeface === "mono" ? typeface : DEFAULT_SETTINGS.typeface,
    fontSize: clampNumber(record["fontSize"], MIN_FONT_SIZE, MAX_FONT_SIZE, DEFAULT_SETTINGS.fontSize),
    lineHeight: clampNumber(record["lineHeight"], MIN_LINE_HEIGHT, MAX_LINE_HEIGHT, DEFAULT_SETTINGS.lineHeight),
  }
}

export function loadSettings(): Settings {
  try {
    return clampSettings(JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? "null"))
  } catch {
    return DEFAULT_SETTINGS
  }
}

export function saveSettings(settings: Settings): void {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings))
  } catch {
    /* storage may be unavailable — the session still uses the values */
  }
}

/**
 * Applies the settings to the document as CSS variables. The editor's
 * stylesheet consumes them (`.cm-scroller` in styles.css); applying on the
 * root keeps the variables available no matter when the editor mounts.
 */
export function applySettings(settings: Settings): void {
  const root = document.documentElement
  root.style.setProperty("--ed-font", TYPEFACE_STACKS[settings.typeface])
  root.style.setProperty("--ed-size", `${settings.fontSize}px`)
  root.style.setProperty("--ed-line", String(settings.lineHeight))
}

// ---------------------------------------------------------------------------
// Per-project default file: which document opens when entering a project
// with no per-tab target (see main.ts openProject / readLastOpen). Local to
// this browser by the same rule as the typography settings — the project's
// shared "main document", if it ever matters server-side, is API data and
// does not belong here.
// ---------------------------------------------------------------------------

const DEFAULT_FILES_KEY = "nisaba.defaultFiles"

type DefaultFileMap = Record<string, string>

const readDefaultFileMap = (): DefaultFileMap => {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(DEFAULT_FILES_KEY) ?? "{}")
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {}
    const map: DefaultFileMap = {}
    for (const [projectId, path] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof path === "string" && path !== "") map[projectId] = path
    }
    return map
  } catch {
    return {}
  }
}

/** The file path this browser opens on entering the project, if set. */
export function loadDefaultFile(projectId: string): string | undefined {
  return readDefaultFileMap()[projectId]
}

/** Sets (or clears, with undefined/"") the project's default opening file. */
export function saveDefaultFile(projectId: string, path: string | undefined): void {
  try {
    const map = readDefaultFileMap()
    if (path === undefined || path === "") delete map[projectId]
    else map[projectId] = path
    localStorage.setItem(DEFAULT_FILES_KEY, JSON.stringify(map))
  } catch {
    /* storage may be unavailable — the session still works without it */
  }
}
