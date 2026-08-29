import { describe, expect, it } from "vitest"
import { filterAndSortProjects, matchesQuery, type SortableProject } from "./projects-list.js"

const mk = (id: string, name: string, updated_at: string): SortableProject => ({ id, name, updated_at })

const projects = [
  mk("c", "Tidal Notes", "2026-08-29T10:00:00Z"),
  mk("a", "Salt Roads", "2026-08-28T10:00:00Z"),
  mk("b", "Zebra Annex", "2026-08-29T12:00:00Z"),
]

describe("matchesQuery", () => {
  it("matches case-insensitively and ignores surrounding whitespace", () => {
    expect(matchesQuery("Tidal Notes", "  tidal ")).toBe(true)
    expect(matchesQuery("Tidal Notes", "SALT")).toBe(false)
  })

  it("treats an empty query as matching everything", () => {
    expect(matchesQuery("anything", "")).toBe(true)
    expect(matchesQuery("anything", "   ")).toBe(true)
  })
})

describe("filterAndSortProjects", () => {
  it("sorts by updated_at descending (recent first) with id as tiebreaker", () => {
    expect(filterAndSortProjects(projects, "", "recent").map((p) => p.name)).toEqual([
      "Zebra Annex",
      "Tidal Notes",
      "Salt Roads",
    ])
  })

  it("orders same-second pairs with variable subsecond width by instant, not string", () => {
    // `.500Z` vs `.501123Z`: lexicographic compare puts `.501123Z` BEFORE
    // `.500Z` is fine here, but the mirrored case `.500Z` vs `.500123Z`
    // (see below) is where strings lie. A full millisecond of difference
    // must order by instant.
    const half = mk("a", "Half a second", "2026-08-29T10:00:00.500Z")
    const more = mk("b", "Half a second plus", "2026-08-29T10:00:00.501123Z")
    expect(filterAndSortProjects([half, more], "", "recent").map((p) => p.name)).toEqual([
      "Half a second plus",
      "Half a second",
    ])
  })

  it("breaks sub-millisecond ties deterministically by id", () => {
    // JS Dates have millisecond precision: `.500Z` and `.500123Z` parse to
    // the same instant, so the pair is a tie and the id tiebreaker decides
    // (mirroring the API's deterministic ORDER BY …, id).
    const half = mk("a", "Half a second", "2026-08-29T10:00:00.500Z")
    const more = mk("b", "Half a second plus", "2026-08-29T10:00:00.500123Z")
    expect(filterAndSortProjects([more, half], "", "recent").map((p) => p.id)).toEqual(["a", "b"])
  })

  it("breaks updated_at ties deterministically by id", () => {
    const tied = [mk("b", "Late id", "2026-08-29T10:00:00Z"), mk("a", "Early id", "2026-08-29T10:00:00Z")]
    expect(filterAndSortProjects(tied, "", "recent").map((p) => p.id)).toEqual(["a", "b"])
  })

  it("sorts by name, locale-aware and numeric", () => {
    const named = [mk("1", "Chapter 10", "2026-01-01T00:00:00Z"), mk("2", "Chapter 2", "2026-01-02T00:00:00Z")]
    expect(filterAndSortProjects(named, "", "name").map((p) => p.name)).toEqual(["Chapter 2", "Chapter 10"])
  })

  it("filters by query before sorting", () => {
    expect(filterAndSortProjects(projects, "salt", "recent").map((p) => p.name)).toEqual(["Salt Roads"])
    expect(filterAndSortProjects(projects, "NOTES", "name").map((p) => p.name)).toEqual(["Tidal Notes"])
  })

  it("returns an empty array for a query that matches nothing", () => {
    expect(filterAndSortProjects(projects, "atlantis", "recent")).toEqual([])
  })

  it("does not mutate the input array", () => {
    const input = [...projects]
    filterAndSortProjects(input, "", "name")
    expect(input).toEqual(projects)
  })
})
