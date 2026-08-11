/**
 * Command palette (⌘K).
 *
 * This is the one concession to code-editor conventions in an interface built
 * for writers, and it earns its place twice over: it is the fastest way to open
 * a file or jump to a section in a long document, and it is what lets the
 * permanent chrome stay small — every occasional action lives here instead of
 * becoming another button.
 *
 * Behaviour:
 *   * opens on ⌘K / Ctrl-K, closes on Esc or a click outside;
 *   * fuzzy-matches over whatever the caller supplies (files, headings,
 *     references, commands), grouped by kind and ranked by score;
 *   * `>` at the start filters to commands only, for people arriving from VS Code;
 *   * ↑/↓ move, Enter runs, and the list is an ARIA combobox+listbox so screen
 *     readers announce the active option.
 *
 * The palette owns no application state: `createPalette` takes a provider that
 * returns the current candidate set, so results are always live.
 */
import { fuzzyScore } from "./fuzzy"

export interface PaletteItem {
  /** Stable id, used for the aria-activedescendant target. */
  readonly id: string
  /** Group heading, e.g. "Files", "Sections", "Commands". */
  readonly group: string
  /** Four-or-so character kind label shown at the left of the row. */
  readonly kind: string
  readonly label: string
  /** Right-aligned secondary text: a path, a line number, a shortcut. */
  readonly hint?: string
  /** Extra text matched against the query but not displayed. */
  readonly search?: string
  readonly run: () => void
}

export interface Palette {
  readonly open: (initialQuery?: string) => void
  readonly close: () => void
  readonly isOpen: () => boolean
}

const MAX_RESULTS = 40

/**
 * Groups are shown in a fixed order and never interleave: ranking purely by
 * score scattered files above and below the commands, so the same heading
 * appeared twice in one list. Score still decides the order *within* a group.
 */
const GROUP_ORDER = ["Files", "Sections", "References", "Commands"]

function groupRank(group: string): number {
  const index = GROUP_ORDER.indexOf(group)
  return index === -1 ? GROUP_ORDER.length : index
}

export function createPalette(provider: () => readonly PaletteItem[]): Palette {
  let backdrop: HTMLElement | undefined
  let input: HTMLInputElement | undefined
  let list: HTMLElement | undefined
  let results: PaletteItem[] = []
  let active = 0

  const rank = (query: string): PaletteItem[] => {
    const commandsOnly = query.startsWith(">")
    const needle = (commandsOnly ? query.slice(1) : query).trim()
    const candidates = provider().filter((item) => !commandsOnly || item.group === "Commands")
    if (needle === "") {
      return [...candidates].sort((a, b) => groupRank(a.group) - groupRank(b.group)).slice(0, MAX_RESULTS)
    }
    return candidates
      .map((item) => ({ item, score: Math.max(fuzzyScore(needle, item.label), fuzzyScore(needle, `${item.search ?? ""} ${item.hint ?? ""}`) - 1) }))
      .filter((scored) => scored.score >= 0)
      .sort((a, b) => groupRank(a.item.group) - groupRank(b.item.group) || b.score - a.score)
      .slice(0, MAX_RESULTS)
      .map((scored) => scored.item)
  }

  const render = (): void => {
    if (!list || !input) return
    list.replaceChildren()
    if (results.length === 0) {
      const empty = document.createElement("div")
      empty.className = "palette-empty"
      empty.textContent = "Nothing matches that."
      list.append(empty)
      input.removeAttribute("aria-activedescendant")
      return
    }
    let group: string | undefined
    results.forEach((item, index) => {
      if (item.group !== group) {
        group = item.group
        const heading = document.createElement("div")
        heading.className = "palette-group"
        heading.textContent = group
        list?.append(heading)
      }
      const row = document.createElement("li")
      row.className = "palette-item"
      row.id = `palette-item-${index}`
      row.setAttribute("role", "option")
      row.setAttribute("aria-selected", String(index === active))
      const kind = document.createElement("span")
      kind.className = "kind"
      kind.textContent = item.kind
      const label = document.createElement("span")
      label.className = "label"
      label.textContent = item.label
      row.append(kind, label)
      if (item.hint !== undefined && item.hint !== "") {
        const hint = document.createElement("span")
        hint.className = "hint"
        hint.textContent = item.hint
        row.append(hint)
      }
      // mousedown, not click: the input must not lose focus before we run.
      row.addEventListener("mousedown", (event) => {
        event.preventDefault()
        active = index
        commit()
      })
      row.addEventListener("mousemove", () => {
        if (active === index) return
        active = index
        for (const node of list?.querySelectorAll<HTMLElement>(".palette-item") ?? []) {
          node.setAttribute("aria-selected", String(node === row))
        }
        input?.setAttribute("aria-activedescendant", row.id)
      })
      list?.append(row)
    })
    input.setAttribute("aria-activedescendant", `palette-item-${active}`)
  }

  const refresh = (): void => {
    results = rank(input?.value ?? "")
    active = 0
    render()
  }

  const move = (delta: number): void => {
    if (results.length === 0) return
    active = (active + delta + results.length) % results.length
    render()
    list?.querySelector<HTMLElement>(`#palette-item-${active}`)?.scrollIntoView({ block: "nearest" })
  }

  const commit = (): void => {
    const item = results[active]
    close()
    item?.run()
  }

  const close = (): void => {
    backdrop?.remove()
    backdrop = undefined
    input = undefined
    list = undefined
    results = []
  }

  const open = (initialQuery = ""): void => {
    if (backdrop) {
      input?.focus()
      input?.select()
      return
    }
    backdrop = document.createElement("div")
    backdrop.className = "palette-backdrop"
    backdrop.addEventListener("mousedown", (event) => {
      if (event.target === backdrop) close()
    })

    const panel = document.createElement("div")
    panel.className = "palette"
    panel.setAttribute("role", "dialog")
    panel.setAttribute("aria-label", "Search files, sections and commands")

    input = document.createElement("input")
    input.type = "text"
    input.placeholder = "Search files, sections, references — or type > for commands"
    input.setAttribute("role", "combobox")
    input.setAttribute("aria-expanded", "true")
    input.setAttribute("aria-controls", "palette-results")
    input.setAttribute("aria-autocomplete", "list")
    input.autocomplete = "off"
    input.value = initialQuery

    list = document.createElement("ul")
    list.className = "palette-results"
    list.id = "palette-results"
    list.setAttribute("role", "listbox")

    const foot = document.createElement("div")
    foot.className = "palette-foot"
    for (const hint of ["↑↓ move", "↵ open", "esc close", "> commands"]) {
      const span = document.createElement("span")
      span.textContent = hint
      foot.append(span)
    }

    panel.append(input, list, foot)
    backdrop.append(panel)
    document.body.append(backdrop)

    input.addEventListener("input", refresh)
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || (event.key === "n" && event.ctrlKey)) { event.preventDefault(); move(1) }
      else if (event.key === "ArrowUp" || (event.key === "p" && event.ctrlKey)) { event.preventDefault(); move(-1) }
      else if (event.key === "Enter") { event.preventDefault(); commit() }
      else if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close() }
      else if (event.key === "Tab") { event.preventDefault() }
    })

    refresh()
    input.focus()
  }

  return { open, close, isOpen: () => backdrop !== undefined }
}
