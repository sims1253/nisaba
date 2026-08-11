/**
 * Fuzzy subsequence matcher, shared by the citation completer and the command
 * palette so "type a few letters of what you want" behaves identically wherever
 * the user does it.
 *
 * Matches when every character of the query appears in the haystack in order
 * (not necessarily contiguously). Scoring rewards consecutive runs and matches
 * that land on a word boundary, which is what makes `intr` rank
 * `chapters/introduction.typ` above `printer-instructions.typ`.
 *
 * Returns a score (higher is better) or -1 when the query does not match.
 */
export function fuzzyScore(query: string, haystack: string): number {
  if (!query) return 0
  const q = query.toLowerCase()
  const h = haystack.toLowerCase()
  let qi = 0
  let score = 0
  let streak = 0
  for (let hi = 0; hi < h.length && qi < q.length; hi++) {
    if (h[hi] === q[qi]) {
      qi++
      streak++
      score += streak // consecutive matches score higher
      const previous = h[hi - 1]
      if (hi === 0 || previous === " " || previous === "-" || previous === "/" || previous === "_" || previous === ".") {
        score += 2 // word-boundary bonus
      }
    } else {
      streak = 0
    }
  }
  return qi === q.length ? score : -1
}
