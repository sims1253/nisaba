/**
 * The client-side compile pipeline: everything between the request the editor
 * builds (raw text + marks + view — the `POST /api/compile` body) and the
 * request the wasm compile boundary accepts (projected sources — the compile
 * service's body), plus the response mapping back to the client's shape.
 *
 * The heavy, parity-sensitive halves run in WASM built from the same crates
 * the server uses: projection and the bibliography YAML come from
 * `nisaba-core-wasm` (issue #20 stage 1), compilation from
 * `nisaba-compile-wasm` (stage 2b) — both byte-pinned by their parity suites.
 * What remains are the app service's three private string helpers
 * (`markdown_headings_to_typst`, `inject_bibliography`, `inject_redline_review`
 * in services/app/src/lib.rs::api_compile), which stage 1 deliberately did
 * not expose (see its crate docs: they "must either move into a pure crate or
 * be reimplemented behind the same parity guard"). They are ported here 1:1 —
 * each function names its Rust origin — with the app's test cases mirrored in
 * ./wasm-compile.test.ts. When they move into a crate, this module shrinks to
 * the request/response mapping.
 *
 * Everything here is pure (the wasm functions arrive as injectable deps), so
 * the whole pipeline is unit-testable without any real wasm.
 */
import type { CompileView, MarkInput } from "../api"

/**
 * One compile job: exactly what the editor sends to `POST /api/compile`,
 * plus the project's reference rows (the `GET /references` wire shape) so the
 * worker can build the bibliography the app builds server-side from the
 * database on every compile.
 */
export interface WasmCompileJob {
  readonly project_id: string
  readonly entry: string
  readonly sources: Readonly<Record<string, string>>
  readonly marks: Readonly<Record<string, readonly MarkInput[]>>
  readonly view: CompileView
  readonly references: readonly unknown[]
}

/** The wasm boundary's request: the compile service's body shape. Projection
 *  has already been applied (that is this module's job) and marks never cross
 *  this line — exactly as the app clears them before forwarding. */
export interface WasmBoundaryRequest {
  readonly project_id: string
  readonly entry: string
  readonly sources: Record<string, string>
  readonly view: string
}

/** The wasm boundary's response: the compile service's body shape, which
 *  names the PDF `pdf` where the app's public route says `pdf_base64`. */
export interface WasmBoundaryResponse {
  readonly pdf: string | null
  readonly span_map: readonly unknown[]
  readonly diagnostics: readonly unknown[]
  readonly outline: readonly unknown[]
  readonly build_id: string
  readonly instrumentation?: unknown
}

/** The app's public compile response shape (api.ts's `CompileResponse`). */
export interface ClientCompileResponse {
  readonly pdf_base64: string | null
  readonly span_map: readonly unknown[]
  readonly diagnostics: readonly unknown[]
  readonly outline: readonly unknown[]
  readonly build_id: string
}

/** The wasm-backed halves of the pipeline, passed in by the worker. */
export interface PipelineDeps {
  /** `nisaba-core-wasm` `project_source`: applies a view's projection. */
  readonly projectSource: (source: string, marksJson: string, view: string) => string
  /** `nisaba-core-wasm` `bibliography_yaml`: renders the references YAML. */
  readonly bibliographyYaml: (referencesJson: string) => string
}

// --- Ports of services/app/src/lib.rs's private compile helpers (keep 1:1) ---

/** Reserved bibliography basename (app `REFS_SOURCE_PATH`). */
const REFS_SOURCE_PATH = "refs.yml"
/** Reserved review support-file path (app `REVIEW_SUPPORT_PATH`). */
const REVIEW_SUPPORT_PATH = "review.typ"
/** The review support-file body (app `REVIEW_SUPPORT_SOURCE`). */
const REVIEW_SUPPORT_SOURCE = "#let add = it => text(fill: green)[+#it]\n#let del = it => text(fill: red)[#strike[#it]]\n#let rep-open = it => []\n#let rep-close = it => []\n"
/** The redline markers the projection emits (nisaba-core `RedlineStyle`
 *  defaults); their presence in any source is what triggers review-support
 *  injection, and the strings come from the style's own defaults so they
 *  cannot drift from what projection emits. */
const REDLINE_MARKERS: readonly string[] = ["#review.add[", "#review.del[", "#review.rep-open[]", "#review.rep-close[]"]

/**
 * Rust `str::lines` semantics: `\n` is a terminator (a trailing newline does
 * not produce a final empty line) and one `\r` directly before a `\n` is
 * stripped. The app's heading conversion is defined over `lines()`, so the
 * port must reproduce its edge behavior, not `split("\n")`'s.
 */
function rustLines(source: string): string[] {
  const lines = source.split("\n")
  const terminated = source.endsWith("\n")
  if (terminated && lines.length > 0 && lines[lines.length - 1] === "") lines.pop()
  return lines.map((line, index) =>
    (terminated || index < lines.length - 1) && line.endsWith("\r") ? line.slice(0, -1) : line)
}

/**
 * Port of the app's `markdown_headings_to_typst`: markdown headings become
 * Typst headings at compile time (`### Title` → `=== Title`), never mutating
 * the stored body, so a document that exports successfully also previews
 * successfully. A `#` run only counts when followed by a space; 1–6 levels;
 * leading spaces are preserved. Like the Rust original, the result is
 * `lines().join("\n")`, so a trailing newline is not reproduced.
 */
export function markdownHeadingsToTypst(source: string): string {
  return rustLines(source)
    .map((line) => {
      const trimmed = line.replace(/^ +/, "")
      let hashes = 0
      while (trimmed[hashes] === "#") hashes += 1
      if (hashes >= 1 && hashes <= 6 && trimmed[hashes] === " ") {
        const leadingSpaces = line.length - trimmed.length
        return `${" ".repeat(leadingSpaces)}${"=".repeat(hashes)} ${trimmed.slice(hashes + 1)}`
      }
      return line
    })
    .join("\n")
}

/** The directory part of a virtual path ("" for a root file) — the app's
 *  `entry.rsplit_once('/')` dir. */
function directoryOf(path: string): string {
  const slash = path.lastIndexOf("/")
  return slash === -1 ? "" : path.slice(0, slash)
}

/** Port of the app's `inject_bibliography`: writes the rendered YAML to the
 *  reserved `refs.yml` beside the entry and appends a `#bibliography` call to
 *  the entry when it does not already cite one. A whitespace-only YAML (no
 *  references) injects nothing. */
function injectBibliography(request: { entry: string; sources: Record<string, string> }, yaml: string): void {
  if (yaml.trim() === "") return
  const entryDir = directoryOf(request.entry)
  const bibPath = entryDir === "" ? REFS_SOURCE_PATH : `${entryDir}/${REFS_SOURCE_PATH}`
  if (request.sources[bibPath] !== undefined) return
  request.sources[bibPath] = yaml
  const entrySource = request.sources[request.entry]
  if (entrySource !== undefined && !entrySource.includes("#bibliography(")) {
    request.sources[request.entry] = `${entrySource}\n#bibliography("refs.yml")\n`
  }
}

/** Port of the app's `inject_redline_review`: for the redline view only, when
 *  any source carries the projection's markers, adds the `review.typ` support
 *  module beside the entry and imports it from the entry. Markers are checked
 *  in ALL sources, not just the entry — in a multi-file project the marks may
 *  live on an included file while the entry only `#include`s it. */
function injectRedlineReview(request: { entry: string; view: string; sources: Record<string, string> }): void {
  if (request.view !== "redline") return
  const hasMarkers = Object.values(request.sources).some((source) =>
    REDLINE_MARKERS.some((marker) => source.includes(marker)))
  if (!hasMarkers) return
  const entryDir = directoryOf(request.entry)
  const modulePath = entryDir === "" ? REVIEW_SUPPORT_PATH : `${entryDir}/${REVIEW_SUPPORT_PATH}`
  if (request.sources[modulePath] !== undefined) return
  request.sources[modulePath] = REVIEW_SUPPORT_SOURCE
  const entrySource = request.sources[request.entry]
  if (
    entrySource !== undefined &&
    !entrySource.includes('#import "review.typ"') &&
    !entrySource.includes("#import 'review.typ'")
  ) {
    request.sources[request.entry] = `#import "review.typ" as review\n\n${entrySource}`
  }
}

// --- The pipeline itself ------------------------------------------------------

/**
 * Builds the exact request the wasm boundary (and the compile service)
 * accepts, mirroring the app's `api_compile` step for step: project every
 * source for the view (marks applied here, never forwarded), convert markdown
 * headings, render and inject the bibliography, inject redline support.
 *
 * @throws Whatever the injected wasm functions throw (JS `Error`s carrying the
 *         core's message strings — e.g. an unknown mark kind), so the caller
 *         reports the same failures the server path would.
 */
export function buildWasmBoundaryRequest(job: WasmCompileJob, deps: PipelineDeps): WasmBoundaryRequest {
  const request: { project_id: string; entry: string; sources: Record<string, string>; view: string } = {
    project_id: job.project_id,
    entry: job.entry,
    sources: {},
    view: job.view
  }
  for (const [path, source] of Object.entries(job.sources)) {
    const projected = deps.projectSource(source, JSON.stringify(job.marks[path] ?? []), job.view)
    request.sources[path] = markdownHeadingsToTypst(projected)
  }
  injectBibliography(request, deps.bibliographyYaml(JSON.stringify(job.references)))
  injectRedlineReview(request)
  return request
}

/**
 * Maps the wasm boundary's response onto the app's public compile response:
 * renames `pdf` → `pdf_base64` and drops `instrumentation` (the client's
 * contract has no such field; on wasm its timings read 0 — the core is
 * deliberately clock-less there and the host measures wall-clock in JS — and
 * `rss_bytes` is null, since reading process memory is not portable to a
 * browser tab).
 */
export function toClientCompileResponse(response: WasmBoundaryResponse): ClientCompileResponse {
  return {
    pdf_base64: response.pdf,
    span_map: response.span_map,
    diagnostics: response.diagnostics,
    outline: response.outline,
    build_id: response.build_id
  }
}
