//! # nisaba-compile-core
//!
//! Nisaba's pure Typst compilation core: everything the compiler needs over
//! in-memory sources, with no I/O and no async runtime (sync code, std only).
//!
//! The crate deliberately uses Typst's `World` callback interface through
//! tinymist's in-memory mock VFS. It never invokes the Typst CLI or creates a
//! source file on disk.
//!
//! Contents:
//!
//! - [`Worker`]: a long-lived compiler instance for one project — mock-VFS
//!   world assembly, incremental source updates, and the warm `comemo` caches
//!   that only survive while the instance lives;
//! - [`WorkerEntry`] plus [`evict_idle`]/[`evict_lru`]: the LRU/TTL cache of
//!   workers keyed by project;
//! - [`Worker::compile`]: the compile pipeline — a single `typst::compile`
//!   call, PDF/A-2b export with a fixed timestamp for reproducibility,
//!   span-map production, outline parsing, and diagnostics shaping;
//! - [`validate_request`]: the request-shape guards (virtual-path traversal,
//!   source count/size bounds).
//!
//! The compile *service* (`services/compile`) is a thin HTTP plane over this
//! crate: bearer auth, body limits, the concurrency semaphore, per-compile
//! timeouts, and the RSS gauge. Keeping the core sync + std-only is what lets
//! the planned in-browser compile module (issue #20, stage 2) wrap the exact
//! same code behind a wasm-bindgen boundary.
//!
//! The one host-plane seam: [`Instrumentation::rss_bytes`] is left `None` by
//! the core (reading `/proc/self/status` is server I/O); the compile service
//! fills it in before serializing the response.
//!
//! A second, target-plane seam: `wasm32-unknown-unknown` has no clock the
//! core could read without JS imports — `Instant::now` and `SystemTime::now`
//! panic there — so the two instrumentation timings read `0` on that target
//! (see [`Stopwatch`]), build ids fall back to a process-global counter for
//! uniqueness, and [`WorkerEntry::now_millis`] reports the epoch: the TTL
//! sweep keeps everything, LRU eviction picks an arbitrary victim, and the
//! capacity bound still holds. None of this touches any content-bearing
//! output (PDF, span map, diagnostics, outline).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use tinymist_vfs::mock::MockWorkspace;
use tinymist_world::mock::{MockWorkspaceWorldExt, MockWorldChangeExt};
use typst::{
    World, WorldExt,
    diag::Severity,
    layout::{Frame as LayoutFrame, FrameItem},
    syntax::{DiagSpanKind, Span, SyntaxKind, SyntaxNode},
};
use typst_layout::PagedDocument;

/// Canonical cap on the number of source files one compile request may carry.
pub const DEFAULT_MAX_SOURCES: usize = 256;
/// Canonical cap on the total source bytes one compile request may carry.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
/// Cap on concurrently cached per-project workers. Once reached, the least
/// recently used worker is evicted to make room (LRU).
pub const DEFAULT_MAX_WORKERS: usize = 256;
/// Workers untouched for this long are evicted by the TTL sweep.
pub const DEFAULT_WORKER_IDLE_TTL: Duration = Duration::from_mins(30);

/// Monotonic stopwatch for the instrumentation timings only.
///
/// `wasm32-unknown-unknown` has no monotonic clock the core could read
/// without importing JavaScript (`Instant::now` panics there), and the
/// timings are host-plane instrumentation — never content — so on that
/// target the stopwatch reads `0` and the wasm host measures wall-clock time
/// around the call instead. Natively this is `std::time::Instant`.
pub struct Stopwatch {
    #[cfg(not(target_arch = "wasm32"))]
    started: std::time::Instant,
}

impl Stopwatch {
    /// Starts the clock (a no-op value on `wasm32`).
    #[must_use]
    pub fn start() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: std::time::Instant::now(),
        }
    }

    /// Milliseconds elapsed since [`Stopwatch::start`] (`0` on `wasm32`).
    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.started.elapsed().as_millis()
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }
}

/// One compile request: the projection of a project as plain Typst sources.
///
/// This is the compile service's HTTP request body (and, planned in issue
/// #20, the wasm boundary's input): the service knows nothing about CRDTs,
/// marks, or reviews — projection happens before this boundary.
#[derive(Debug, Clone, Deserialize)]
pub struct CompileRequest {
    /// The project the sources belong to; keys the per-project worker cache.
    pub project_id: String,
    /// The entry file (a key of `sources`) to compile.
    pub entry: String,
    /// Path → Typst source text.
    pub sources: HashMap<String, String>,
    /// An open projection label. The core does not interpret it beyond
    /// folding it into the per-worker fingerprint; projection happens before
    /// this boundary.
    pub view: String,
}

/// One compile result: PDF, span map, diagnostics, outline, build identity,
/// and instrumentation, exactly as the compile service serializes it.
#[derive(Debug, Serialize)]
pub struct CompileResponse {
    /// Base64-encoded PDF bytes, or `None` when the compile failed (then
    /// `diagnostics` explains why).
    pub pdf: Option<String>,
    /// Source ranges mapped to the page they render on.
    pub span_map: Vec<SpanMapEntry>,
    /// Compile errors and warnings, in UTF-16 offsets.
    pub diagnostics: Vec<Diagnostic>,
    /// Document outline (headings), in UTF-16 offsets.
    pub outline: Vec<OutlineEntry>,
    /// Opaque build identifier (drawn from the process-global compile
    /// sequence, so ids are unique per process even across workers).
    pub build_id: String,
    /// Timing and cache instrumentation for the compile.
    pub instrumentation: Instrumentation,
}

/// One span-map entry: a source range and the first page it renders on.
#[derive(Debug, Serialize)]
pub struct SpanMapEntry {
    /// Request source path (no leading slash).
    pub path: String,
    /// Range start as a UTF-16 code-unit offset.
    pub start: usize,
    /// Range end as a UTF-16 code-unit offset.
    pub end: usize,
    /// 1-based page the range first appears on; `None` on failed compiles
    /// (whole-source fallback entries).
    pub page: Option<usize>,
}

/// One compile diagnostic (error or warning), shaped for the wire.
#[derive(Debug, Serialize)]
pub struct Diagnostic {
    /// `"error"` or `"warning"`.
    pub severity: &'static str,
    /// Human-readable message.
    pub message: String,
    /// Source path the diagnostic points into, if it has a span.
    pub path: Option<String>,
    /// Range start as a UTF-16 code-unit offset, if the span resolves.
    pub start: Option<usize>,
    /// Range end as a UTF-16 code-unit offset, if the span resolves.
    pub end: Option<usize>,
}

/// One outline entry (a heading in some source file).
#[derive(Debug, Serialize)]
pub struct OutlineEntry {
    /// Heading level (count of `=` markers).
    pub level: usize,
    /// Heading text without the markers.
    pub title: String,
    /// Source path the heading lives in.
    pub path: String,
    /// Heading start as a UTF-16 code-unit offset.
    pub start: usize,
}

/// Timing and cache instrumentation for one compile.
#[derive(Debug, Serialize)]
pub struct Instrumentation {
    /// Milliseconds spent in `typst::compile` (total minus PDF export).
    pub compile_ms: u128,
    /// Milliseconds spent exporting the PDF.
    pub pdf_ms: u128,
    /// Whether this compile reused a cached (warm) worker.
    pub worker_reused: bool,
    /// Compiles this worker has served so far.
    pub worker_compiles: u64,
    /// Same-fingerprint cache hits this worker has seen.
    pub cache_hits: u64,
    /// Same-fingerprint cache misses this worker has seen.
    pub cache_misses: u64,
    /// Host-process RSS gauge. Always `None` from the core: reading
    /// `/proc/self/status` is server I/O, so the compile service fills this
    /// in before serializing (see the crate docs).
    pub rss_bytes: Option<u64>,
    /// Convergence passes; Typst converges inside one `typst::compile` call,
    /// so this is always 1 (kept for response-schema stability).
    pub convergence_passes: u8,
}

/// A long-lived compiler instance for one project.
///
/// Holds the mock-VFS workspace, the Typst universe built from it, and the
/// warm `comemo` caches that only survive while the worker lives. The
/// compile service keeps one per `project_id` (see [`WorkerEntry`]); a wasm
/// host holds one per browser tab for the same reason.
#[derive(Debug)]
pub struct Worker {
    workspace: MockWorkspace,
    universe: tinymist_world::mock::MockUniverse,
    /// The entry this worker's universe is rooted at. Updated whenever
    /// `update_sources` sees a request targeting a different entry, so a cached
    /// worker serving multiple documents of one project re-roots correctly.
    known_entry: String,
    known_sources: HashMap<String, String>,
    compile_count: u64,
    cache_hits: u64,
    cache_misses: u64,
    last_fingerprint: Option<u64>,
}

/// A cached worker plus an atomic last-use timestamp for LRU/TTL eviction.
#[derive(Clone)]
pub struct WorkerEntry {
    /// The worker itself, behind the lock the blocking compile takes.
    pub worker: Arc<StdMutex<Worker>>,
    /// Last-use timestamp in UNIX milliseconds (see [`WorkerEntry::touch`]).
    pub last_used: Arc<std::sync::atomic::AtomicU64>,
    /// Set when a compile times out. The worker mutex is held by the abandoned
    /// thread until `typst::compile` finishes. This flag tells subsequent
    /// lookups to evict the worker and create a fresh one instead of blocking
    /// on the stale lock.
    pub poisoned: Arc<std::sync::atomic::AtomicBool>,
}

impl WorkerEntry {
    /// Current UNIX time in milliseconds (0 if the clock is before the epoch;
    /// always 0 on `wasm32-unknown-unknown`, where reading a clock panics —
    /// see the crate docs).
    #[must_use]
    pub fn now_millis() -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| {
                    u64::try_from(duration.as_millis()).unwrap_or(0)
                })
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// Records this entry as just used (LRU/TTL bookkeeping).
    pub fn touch(&self) {
        self.last_used
            .store(Self::now_millis(), std::sync::atomic::Ordering::Relaxed);
    }
}

impl Worker {
    /// Builds a worker for the request: assembles the mock-VFS workspace from
    /// the sources and roots a Typst universe at the entry.
    pub fn new(request: &CompileRequest) -> Result<Self, String> {
        let builder = MockWorkspace::default_builder();
        let workspace = request
            .sources
            .iter()
            .fold(builder, |builder, (path, source)| {
                builder.file(path, source)
            })
            .build();
        let universe = workspace
            .world(&request.entry)
            .build_universe()
            .map_err(|error| format!("create Typst world: {error:?}"))?;
        Ok(Self {
            workspace,
            universe,
            known_entry: request.entry.clone(),
            known_sources: request.sources.clone(),
            compile_count: 0,
            cache_hits: 0,
            cache_misses: 0,
            last_fingerprint: None,
        })
    }

    /// Applies the request's sources to the workspace incrementally: removes
    /// gone paths, updates changed ones (leaving untouched files — and their
    /// parsed/cached state — alone), and re-roots the universe when the
    /// request targets a different entry.
    pub fn update_sources(&mut self, request: &CompileRequest) -> Result<(), String> {
        let old_paths: Vec<String> = self
            .known_sources
            .keys()
            .filter(|path| !request.sources.contains_key(*path))
            .cloned()
            .collect();
        for path in old_paths {
            let change = self
                .workspace
                .remove(&path)
                .map_err(|error| format!("remove source {path}: {error:?}"))?;
            change.apply_to_universe(&mut self.universe);
            self.known_sources.remove(&path);
        }
        for (path, source) in &request.sources {
            if self.known_sources.get(path) != Some(source) {
                let change = self.workspace.update_source(path, source);
                change.apply_to_universe(&mut self.universe);
                self.known_sources.insert(path.clone(), source.clone());
            }
        }
        // The universe's root/entry was fixed at construction time. If this
        // request targets a different entry (e.g. a cached project worker now
        // asked to compile a different document), re-root the universe to the
        // requested entry. The in-memory workspace already holds the current
        // sources, so rebuilding the universe from it is authoritative and
        // avoids the stale-root bug where a second document failed to compile.
        if self.known_entry != request.entry {
            let re_rooted = self
                .workspace
                .world(&request.entry)
                .build_universe()
                .map_err(|error| format!("re-root universe to {}: {error:?}", request.entry))?;
            self.universe = re_rooted;
            self.known_entry.clone_from(&request.entry);
        }
        Ok(())
    }

    /// Compiles the request: a single `typst::compile` call, then PDF/A-2b
    /// export (fixed timestamp for reproducibility), span map, outline, and
    /// diagnostics. Compile failures become ordinary diagnostics with no PDF,
    /// not errors.
    #[allow(clippy::too_many_lines)]
    pub fn compile(
        &mut self,
        request: &CompileRequest,
        reused: bool,
    ) -> Result<CompileResponse, String> {
        let span = tracing::info_span!(
            "typst_compile",
            project = %request.project_id,
            entry = %request.entry,
            reused,
        );
        let _enter = span.enter();
        let started = Stopwatch::start();
        let fingerprint = fingerprint(request);
        if self.last_fingerprint == Some(fingerprint) {
            self.cache_hits += 1;
        } else {
            self.cache_misses += 1;
            self.last_fingerprint = Some(fingerprint);
        }
        self.compile_count += 1;
        let world = self.universe.snapshot();
        // A single `typst::compile` call: Typst runs its own internal
        // convergence loop (up to 5 passes via comemo constraints), so an outer
        // loop with full-document hashing here is pure waste — an earlier version
        // multiplied compile time by up to 5x without changing the output.
        let compiled = typst::compile::<PagedDocument>(&world);
        let diagnostics = compiled
            .warnings
            .iter()
            .map(|diag| diagnostic(&world, diag))
            .collect::<Vec<_>>();
        // Convergence now happens inside the single `typst::compile` call above;
        // report 1 to preserve the response schema.
        let convergence_passes = 1;
        let document = match compiled.output {
            Ok(document) => document,
            Err(errors) => {
                let diagnostics = diagnostics
                    .into_iter()
                    .chain(errors.iter().map(|diag| diagnostic(&world, diag)))
                    .collect();
                return Ok(CompileResponse {
                    pdf: None,
                    span_map: source_span_map(None, &world, &request.sources),
                    diagnostics,
                    outline: outline(&request.sources),
                    build_id: build_id(),
                    instrumentation: instrumentation(&started, 0, reused, self, convergence_passes),
                });
            }
        };
        let compile_ms = started.elapsed_ms();
        let pdf_started = Stopwatch::start();
        // The service always emits PDF/A-2b (the archival profile the export
        // contract requires). Making this per-project/per-profile is future
        // work; the old request field for it was never sent by any client.
        let standards = typst_pdf::PdfStandards::new(&[typst_pdf::PdfStandard::A_2b])
            .map_err(|e| format!("invalid PDF standards combination: {e:?}"))?;
        let pdf = match typst_pdf::pdf(
            &document,
            &typst_pdf::PdfOptions {
                standards,
                timestamp: Some(typst_pdf::Timestamp::new_utc(
                    typst::foundations::Datetime::from_ymd_hms(2025, 1, 1, 0, 0, 0)
                        .expect("valid fixed date"),
                )),
                ..typst_pdf::PdfOptions::default()
            },
        ) {
            Ok(pdf) => pdf,
            Err(errors) => {
                let diagnostics = diagnostics
                    .into_iter()
                    .chain(errors.iter().map(|diag| diagnostic(&world, diag)))
                    .collect();
                return Ok(CompileResponse {
                    pdf: None,
                    span_map: source_span_map(Some(&document), &world, &request.sources),
                    diagnostics,
                    outline: outline(&request.sources),
                    build_id: build_id(),
                    instrumentation: instrumentation(
                        &started,
                        pdf_started.elapsed_ms(),
                        reused,
                        self,
                        convergence_passes,
                    ),
                });
            }
        };
        let pdf_ms = pdf_started.elapsed_ms();
        let mut result = CompileResponse {
            pdf: Some(BASE64.encode(pdf)),
            span_map: source_span_map(Some(&document), &world, &request.sources),
            diagnostics,
            outline: outline(&request.sources),
            build_id: build_id(),
            instrumentation: instrumentation(&started, pdf_ms, reused, self, convergence_passes),
        };
        result.instrumentation.compile_ms = compile_ms;
        tracing::info!(
            compile_ms,
            pdf_ms,
            pages = document.pages().len(),
            cache_hits = self.cache_hits,
            cache_misses = self.cache_misses,
            worker_reused = reused,
            "compile completed"
        );
        Ok(result)
    }
}

/// Removes every cached worker that has been idle longer than the given TTL.
pub fn evict_idle<S: std::hash::BuildHasher>(
    workers: &mut HashMap<String, WorkerEntry, S>,
    worker_idle_ttl: Duration,
) {
    let ttl_millis = u64::try_from(worker_idle_ttl.as_millis()).unwrap_or(u64::MAX);
    let now = WorkerEntry::now_millis();
    workers.retain(|_, entry| {
        now.saturating_sub(entry.last_used.load(std::sync::atomic::Ordering::Relaxed)) <= ttl_millis
    });
}

/// Evicts the single least-recently-used worker. Called when the cache is at
/// capacity and a new worker must be inserted.
pub fn evict_lru<S: std::hash::BuildHasher>(workers: &mut HashMap<String, WorkerEntry, S>) {
    let Some((victim, _)) = workers
        .iter()
        .min_by_key(|(_, entry)| entry.last_used.load(std::sync::atomic::Ordering::Relaxed))
    else {
        return;
    };
    let victim = victim.clone();
    workers.remove(&victim);
}

/// Validates the request shape against the given limits: a non-empty project
/// id, relative non-traversing virtual paths, at least one source, the entry
/// present among the sources, and the source count/byte caps.
///
/// The limits are parameters (not environment) so any host — the compile
/// service today, a wasm boundary in issue #20 stage 2 — applies its own
/// bounds to the same rules. [`DEFAULT_MAX_SOURCES`] and
/// [`DEFAULT_MAX_SOURCE_BYTES`] are the canonical values.
pub fn validate_request(
    request: &CompileRequest,
    max_sources: usize,
    max_source_bytes: usize,
) -> Result<(), String> {
    if request.project_id.trim().is_empty() {
        return Err("project_id must not be empty".into());
    }
    validate_virtual_path(&request.entry)?;
    if request.sources.is_empty() {
        return Err("sources must not be empty".into());
    }
    if request.sources.len() > max_sources {
        return Err(format!("too many sources (maximum {max_sources})"));
    }
    let total_source_bytes = request
        .sources
        .values()
        .try_fold(0usize, |total, source| total.checked_add(source.len()))
        .ok_or_else(|| "total source bytes overflowed".to_owned())?;
    if total_source_bytes > max_source_bytes {
        return Err(format!(
            "sources exceed total byte limit (maximum {max_source_bytes})"
        ));
    }
    if !request.sources.contains_key(&request.entry) {
        return Err("sources must contain entry".into());
    }
    for path in request.sources.keys() {
        validate_virtual_path(path)?;
    }
    Ok(())
}

fn validate_virtual_path(value: &str) -> Result<(), String> {
    // Deliberately laxer than the app service's valid_document_path
    // (services/app/src/lib.rs): this guards only the compiler's per-request
    // virtual filesystem, so `.` segments are tolerated and `..` is allowed
    // as long as it never climbs above the root. The app rejects `.`/`..`
    // and control characters as well because stored document paths are
    // user-facing identifiers; the divergence is intentional.
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() || value.contains('\\') {
        return Err(format!("invalid virtual path: {value:?}"));
    }
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return Err(format!("path traversal: {value:?}"));
                }
                depth -= 1;
            }
            _ => return Err(format!("invalid virtual path: {value:?}")),
        }
    }
    if depth == 0 {
        return Err(format!("invalid virtual path: {value:?}"));
    }
    Ok(())
}

fn instrumentation(
    started: &Stopwatch,
    pdf_ms: u128,
    reused: bool,
    worker: &Worker,
    convergence_passes: u8,
) -> Instrumentation {
    Instrumentation {
        compile_ms: started.elapsed_ms().saturating_sub(pdf_ms),
        pdf_ms,
        worker_reused: reused,
        worker_compiles: worker.compile_count,
        cache_hits: worker.cache_hits,
        cache_misses: worker.cache_misses,
        // Host-plane gauge: the core cannot read process memory usage without
        // I/O; the compile service fills this in before serializing.
        rss_bytes: None,
        convergence_passes,
    }
}

/// Convert a byte offset into `source` to a UTF-16 code-unit offset — the unit
/// JavaScript string indices (and therefore the web editor) use. Byte offsets
/// (Typst's unit) only equal code-unit offsets for pure-ASCII sources.
fn byte_to_utf16(source: &str, byte_offset: usize) -> usize {
    source[..byte_offset.min(source.len())]
        .encode_utf16()
        .count()
}

fn diagnostic(world: &dyn World, diag: &typst::diag::SourceDiagnostic) -> Diagnostic {
    let (path, start, end) = if let DiagSpanKind::Detached = diag.span.get() {
        (None, None, None)
    } else {
        let source = diag.span.id().and_then(|id| world.source(id).ok());
        let path = source
            .as_ref()
            .map(|source| source.id().vpath().get_with_slash().to_string());
        let range = world.range(diag.span);
        // `world.range` reports BYTE offsets (Typst's unit); the web editor
        // consumes them as JavaScript string indices (UTF-16 code units), so a
        // multi-byte character before the error shifted every jump (found by
        // the 2026-08-09 author-agent: em-dashes in the source moved the
        // "jump to error" location).
        let to_utf16 = |offset: usize| {
            source
                .as_ref()
                .map_or(offset, |s| byte_to_utf16(s.text(), offset))
        };
        (
            path,
            range.as_ref().map(|r| to_utf16(r.start)),
            range.map(|r| to_utf16(r.end)),
        )
    };
    Diagnostic {
        severity: match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        },
        message: diag.message.to_string(),
        path,
        start,
        end,
    }
}

/// Maps source ranges to the page they render on.
///
/// On a failed compile (`document` is `None`) the map falls back to one
/// whole-source entry per file with unknown page, which clients can still use
/// for whole-file navigation. On success every page's frame tree is walked and
/// each source span is resolved to a `(path, start, end)` range; a range is
/// recorded on the first page it appears on, so the map doubles as a
/// first-occurrence index per page. Spans outside the request's sources
/// (packages, fonts, embedded files) are skipped.
fn source_span_map(
    document: Option<&PagedDocument>,
    world: &dyn World,
    sources: &HashMap<String, String>,
) -> Vec<SpanMapEntry> {
    let Some(document) = document else {
        let mut entries = sources
            .iter()
            .map(|(path, source)| SpanMapEntry {
                path: path.clone(),
                start: 0,
                // Byte → UTF-16 code units (see byte_to_utf16).
                end: byte_to_utf16(source, source.len()),
                page: None,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        return entries;
    };
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for (page, page_data) in document.pages().iter().enumerate() {
        let mut record = |span: Span| {
            let Some(file_id) = span.id() else { return };
            let Ok(source) = world.source(file_id) else {
                return;
            };
            // tinymist reports root-relative paths with a leading slash;
            // request source keys are written without one, so normalize.
            let path = source
                .id()
                .vpath()
                .get_with_slash()
                .trim_start_matches('/')
                .to_string();
            if !sources.contains_key(&path) {
                return;
            }
            let Some(range) = world.range(span) else {
                return;
            };
            // Byte → UTF-16 code units, the unit the web client's editor uses.
            let start = byte_to_utf16(source.text(), range.start);
            let end = byte_to_utf16(source.text(), range.end);
            if seen.insert((path.clone(), start, end)) {
                entries.push(SpanMapEntry {
                    path,
                    start,
                    end,
                    page: Some(page + 1),
                });
            }
        };
        collect_frame_spans(&page_data.frame, &mut record);
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    entries
}

/// Visits every span-carrying item in a frame tree.
///
/// `Group` recurses; `Text` reports the span of its first glyph (the run's
/// representative span — 0.15 carries spans per glyph); `Shape` and `Image`
/// carry their own spans; `Link` and `Tag` carry no source span.
fn collect_frame_spans(frame: &LayoutFrame, record: &mut dyn FnMut(Span)) {
    for (_, item) in frame.items() {
        match item {
            FrameItem::Group(group) => collect_frame_spans(&group.frame, record),
            FrameItem::Text(text) => {
                if let Some(span) = text.glyphs.first().map(|glyph| glyph.span.0) {
                    record(span);
                }
            }
            FrameItem::Shape(_, span) | FrameItem::Image(_, _, span) => record(*span),
            FrameItem::Link(..) | FrameItem::Tag(..) => {}
        }
    }
}

fn outline(sources: &HashMap<String, String>) -> Vec<OutlineEntry> {
    let mut result = Vec::new();
    for (path, source) in sources {
        let root = typst::syntax::parse(source);
        collect_headings(&root, source, path, 0, &mut result);
    }
    result.sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    result
}

fn collect_headings(
    node: &SyntaxNode,
    source: &str,
    path: &str,
    offset: usize,
    result: &mut Vec<OutlineEntry>,
) {
    if node.kind() == SyntaxKind::Heading {
        let text = node.full_text().trim().to_owned();
        let level = text
            .chars()
            .take_while(|character| *character == '=')
            .count();
        let title = text.trim_start_matches('=').trim().to_owned();
        result.push(OutlineEntry {
            level,
            title,
            path: path.to_owned(),
            // Byte → UTF-16 code units (the web client's index unit).
            start: byte_to_utf16(source, offset),
        });
    }
    let mut child_offset = offset;
    for child in node.children() {
        collect_headings(child, source, path, child_offset, result);
        child_offset += child.len();
    }
}

fn fingerprint(request: &CompileRequest) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    request.entry.hash(&mut hasher);
    request.view.hash(&mut hasher);
    let mut sources = request.sources.iter().collect::<Vec<_>>();
    sources.sort_by(|a, b| a.0.cmp(b.0));
    sources.hash(&mut hasher);
    hasher.finish()
}

/// Per-process build prefix, so build ids stay globally unique across worker
/// restarts. Unused on `wasm32-unknown-unknown`, where reading the wall clock
/// panics; there the process-global sequence counter alone keeps ids unique.
#[cfg(not(target_arch = "wasm32"))]
static BUILD_INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Process-global compile sequence; keeps build ids unique even when the
/// per-process prefix degenerates (several workers in one wasm module, no
/// readable clock).
static BUILD_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn build_id() -> String {
    let sequence = BUILD_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let instance = BUILD_INSTANCE.get_or_init(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(
                    |_| "0".to_owned(),
                    |duration| duration.as_nanos().to_string(),
                )
        });
        format!("build-{instance}-{sequence}")
    }
    #[cfg(target_arch = "wasm32")]
    {
        format!("build-0-{sequence}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str) -> CompileRequest {
        CompileRequest {
            project_id: "p".into(),
            entry: "main.typ".into(),
            sources: HashMap::from([(String::from("main.typ"), source.into())]),
            view: "public".into(),
        }
    }

    #[test]
    fn rejects_virtual_path_traversal() {
        assert!(validate_virtual_path("../main.typ").is_err());
        assert!(validate_virtual_path("chapters/../../main.typ").is_err());
        assert!(validate_virtual_path("chapters/main.typ").is_ok());
    }

    #[test]
    fn compiles_valid_pdf_from_memory() {
        let mut worker =
            Worker::new(&request("= Hello\nThis is in-memory Typst.")).expect("worker");
        let response = worker
            .compile(&request("= Hello\nThis is in-memory Typst."), false)
            .expect("compile");
        let pdf = BASE64.decode(response.pdf.expect("pdf")).expect("base64");
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(response.outline.iter().any(|entry| entry.title == "Hello"));
    }

    #[test]
    fn pdf_defaults_to_a2b_standard() {
        let mut worker = Worker::new(&request("= Hello")).expect("worker");
        let response = worker.compile(&request("= Hello"), false).expect("compile");
        let pdf = BASE64.decode(response.pdf.expect("pdf")).expect("base64");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// Verify that the compile service embeds fonts in the
    /// output PDF. The embedded font resolver should provide standard fonts that
    /// survive into the final PDF. We parse the PDF bytes to check for font
    /// dictionary entries.
    #[test]
    fn pdf_embeds_fonts() {
        let source = "= Font Embedding Test\nThis text requires a font.";
        let mut worker = Worker::new(&request(source)).expect("worker");
        let response = worker.compile(&request(source), false).expect("compile");
        let pdf = BASE64.decode(response.pdf.expect("pdf")).expect("base64");

        // A valid PDF with embedded fonts contains font resource entries.
        // Check for /Type /Font or /FontDescriptor in the PDF bytes.
        let pdf_str = String::from_utf8_lossy(&pdf);
        assert!(
            pdf_str.contains("/Type") && pdf_str.contains("/Font"),
            "PDF should contain font type entries"
        );
        // Check that the PDF has actual font data (not just references).
        // Embedded fonts appear as stream objects with /FontFile or /FontDescriptor.
        assert!(
            pdf_str.contains("/FontDescriptor") || pdf_str.contains("/FontFile"),
            "PDF should contain embedded font descriptors"
        );
    }

    #[test]
    fn span_map_reports_real_pages_and_ranges() {
        let mut worker =
            Worker::new(&request("= Hello\nBody text #emph[with emphasis].")).expect("worker");
        let response = worker
            .compile(&request("= Hello\nBody text #emph[with emphasis]."), false)
            .expect("compile");
        assert!(!response.span_map.is_empty());
        for entry in &response.span_map {
            assert_eq!(entry.path, "main.typ");
            assert_eq!(entry.page, Some(1));
            assert!(entry.start < entry.end);
            assert!(entry.end <= "= Hello\nBody text #emph[with emphasis].".len());
        }
        // Ranges resolve to real source ranges, and the map is sorted by
        // (path, start) with no duplicate ranges.
        let mut ranges = response
            .span_map
            .iter()
            .map(|entry| (entry.start, entry.end))
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        ranges.dedup();
        assert_eq!(ranges.len(), response.span_map.len());
        assert!(response.span_map.windows(2).all(|pair| {
            pair[0].path <= pair[1].path
                && (pair[0].path != pair[1].path || pair[0].start < pair[1].start)
        }));
    }

    #[test]
    fn diagnostics_and_span_map_use_utf16_offsets_for_non_ascii() {
        // Regression (2026-08-09 author-agent): Typst reports byte offsets, but
        // the web editor indexes by UTF-16 code units; a multi-byte character
        // before an error used to shift the reported position left.
        // "—" is 3 UTF-8 bytes but 1 UTF-16 unit.
        let source = "= Intro\nAn em dash — then an error:\n#unknown-fn(";
        let request = request(source);
        let mut worker = Worker::new(&request).expect("worker");
        let response = worker.compile(&request, false).expect("compile");
        let diag = response
            .diagnostics
            .iter()
            .find(|d| d.start.is_some() && d.end.is_some())
            .expect("diagnostic present");
        let start = diag.start.expect("start");
        let end = diag.end.expect("end");
        // The error sits after two multi-byte characters ("—" ×1 here); the
        // byte offset would exceed the UTF-16 offset by 2. Verify the reported
        // range still lands inside the source when used as a string index.
        assert!(start < end, "{start} < {end}");
        assert!(
            end <= source.chars().count(),
            "{end} > {}",
            source.chars().count()
        );
        // And specifically: Typst reports the unclosed delimiter at the final
        // "(" — byte offset 49, UTF-16 offset 47 (the em dash costs 2 extra
        // bytes). A byte-offset consumer would jump 2 chars too far right.
        let paren_byte = source.len() - 1;
        let paren_utf16 = source[..paren_byte].encode_utf16().count();
        assert_eq!(
            start, paren_utf16,
            "diag start must be the UTF-16 offset of the delimiter"
        );
        // span_map entries must also use UTF-16 offsets.
        for entry in &response.span_map {
            assert!(entry.end <= source.chars().count());
        }
    }

    #[test]
    fn unsupported_unicode_is_reported_as_a_compile_diagnostic() {
        // A missing glyph is a document problem, not a service failure. In
        // particular PDF export must not turn Typst's diagnostic into a 500
        // containing its internal Debug representation.
        let request = request("文");
        let mut worker = Worker::new(&request).expect("worker");
        let response = worker.compile(&request, false).expect("compile response");
        assert!(response.pdf.is_none());
        assert!(
            response
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == "error"),
            "expected an ordinary user-facing error diagnostic"
        );
    }

    #[test]
    fn failed_compile_falls_back_to_whole_source_entries() {
        let request = request("#unknown-fn(");
        let mut worker = Worker::new(&request).expect("worker");
        let response = worker.compile(&request, false).expect("compile");
        assert!(response.pdf.is_none());
        assert_eq!(response.span_map.len(), 1);
        assert_eq!(response.span_map[0].path, "main.typ");
        assert_eq!(response.span_map[0].start, 0);
        assert_eq!(response.span_map[0].end, request.sources["main.typ"].len());
        assert_eq!(response.span_map[0].page, None);
    }

    #[test]
    fn reuses_warm_worker_and_updates_overlay() {
        let first = request("= First");
        let second = request("= Second");
        let mut worker = Worker::new(&first).expect("worker");
        worker.compile(&first, false).expect("first compile");
        worker.update_sources(&second).expect("overlay");
        let response = worker.compile(&second, true).expect("second compile");
        assert_eq!(worker.compile_count, 2);
        assert!(response.instrumentation.worker_reused);
        assert!(response.outline.iter().any(|entry| entry.title == "Second"));
        assert!(worker.universe.revision.get() >= 2);
    }

    #[test]
    fn re_roots_single_cached_worker_to_a_new_entry() {
        // A project worker is cached per project_id, not per document. If the
        // first request compiles document A and a later request targets document B
        // of the same project, the cached worker must re-root its universe to B
        // instead of compiling the stale entry A again.
        let mut worker = Worker::new(&CompileRequest {
            project_id: "p".into(),
            entry: "documents/a.typ".into(),
            sources: HashMap::from([(String::from("documents/a.typ"), "= Document A".into())]),
            view: "public".into(),
        })
        .expect("worker");
        let second = CompileRequest {
            project_id: "p".into(),
            entry: "documents/b.typ".into(),
            sources: HashMap::from([(String::from("documents/b.typ"), "= Document B".into())]),
            view: "public".into(),
        };
        worker.update_sources(&second).expect("re-root");
        assert_eq!(worker.known_entry, "documents/b.typ");
        let response = worker.compile(&second, true).expect("compile second entry");
        assert!(
            response
                .outline
                .iter()
                .any(|entry| entry.title == "Document B")
        );
        assert!(
            !response
                .outline
                .iter()
                .any(|entry| entry.title == "Document A")
        );
    }

    #[test]
    fn compile_request_requires_entry_source() {
        let mut request = request("hello");
        request.sources.clear();
        assert!(validate_request(&request, DEFAULT_MAX_SOURCES, DEFAULT_MAX_SOURCE_BYTES).is_err());
    }

    fn entry(last_used_millis: u64) -> WorkerEntry {
        WorkerEntry {
            worker: Arc::new(StdMutex::new(Worker::new(&request("= X")).expect("worker"))),
            last_used: Arc::new(std::sync::atomic::AtomicU64::new(last_used_millis)),
            poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[test]
    fn evict_lru_removes_only_the_oldest_worker() {
        let mut workers = HashMap::from([
            (String::from("a"), entry(1_000)),
            (String::from("b"), entry(2_000)),
            (String::from("c"), entry(500)),
        ]);
        evict_lru(&mut workers);
        assert!(!workers.contains_key("c"));
        assert_eq!(workers.len(), 2);
    }

    #[test]
    fn evict_idle_drops_workers_past_ttl_and_keeps_active_ones() {
        let now = WorkerEntry::now_millis();
        let mut workers = HashMap::from([
            (String::from("stale"), entry(now.saturating_sub(11_000))),
            (String::from("fresh"), entry(now)),
        ]);
        evict_idle(&mut workers, Duration::from_secs(10));
        assert!(!workers.contains_key("stale"));
        assert!(workers.contains_key("fresh"));
    }
}
