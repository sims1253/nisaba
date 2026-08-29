/**
 * Projects-screen list shaping: search filtering plus the two sort orders.
 *
 * Pure functions over the fetched project array — the landing page can list
 * hundreds of projects (shared, imported, QA leftovers), so the shaping is
 * client-side and synchronous; the API's own order is the "recent" default
 * (projects.updated_at DESC, bumped by document activity).
 */

export type ProjectSort = "recent" | "name"

export interface SortableProject {
  readonly id: string
  readonly name: string
  /** RFC 3339 timestamp; lexicographic order equals chronological order. */
  readonly updated_at: string
}

/** Normalizes a search query: trimmed, case-insensitive substring match. */
export function matchesQuery(name: string, query: string): boolean {
  const needle = query.trim().toLowerCase()
  return needle === "" || name.toLowerCase().includes(needle)
}

/**
 * Filter by name query and sort. `recent` = newest updated_at first (ties by
 * id for determinism, mirroring the API); `name` = locale-aware A–Z. The
 * input array is never mutated.
 */
export function filterAndSortProjects<T extends SortableProject>(
  projects: readonly T[],
  query: string,
  sort: ProjectSort,
): T[] {
  const filtered = query.trim() === "" ? [...projects] : projects.filter((p) => matchesQuery(p.name, query))
  if (sort === "name") {
    return filtered.sort((a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }))
  }
  return filtered.sort((a, b) => {
    const byTime = b.updated_at.localeCompare(a.updated_at)
    return byTime !== 0 ? byTime : a.id.localeCompare(b.id)
  })
}
