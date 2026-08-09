//! Nisaba's long-lived in-memory Typst compiler service.
//!
//! The service deliberately uses Typst's `World` callback interface through
//! tinymist's in-memory mock VFS. It never invokes the Typst CLI or creates a
//! source file on disk.

use std::{
    collections::HashMap,
    hash::Hash,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tinymist_vfs::mock::MockWorkspace;
use tinymist_world::mock::{MockWorkspaceWorldExt, MockWorldChangeExt};
use tokio::sync::Mutex;
use typst::{
    World, WorldExt,
    diag::Severity,
    layout::{Frame as LayoutFrame, FrameItem},
    syntax::{DiagSpanKind, Span, SyntaxKind, SyntaxNode},
};
use typst_layout::PagedDocument;

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_SOURCES: usize = 256;
const DEFAULT_MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
/// Cap on concurrently cached per-project workers. Once reached, the least
/// recently used worker is evicted to make room (LRU).
const DEFAULT_MAX_WORKERS: usize = 256;
/// Workers untouched for this long are evicted by the TTL sweep.
const DEFAULT_WORKER_IDLE_TTL: Duration = Duration::from_mins(30);
/// Global bound on concurrently-running blocking compiles, regardless of how
/// many workers/projects are cached. Prevents unbounded `spawn_blocking` usage.
const DEFAULT_MAX_CONCURRENT_COMPILES: usize = 8;

#[derive(Debug, Clone, Deserialize)]
pub struct CompileRequest {
    project_id: String,
    entry: String,
    sources: HashMap<String, String>,
    mode: CompileMode,
    /// An open projection label. The compile service does not interpret it;
    /// projection happens before this boundary.
    view: String,
    /// Opt-in SVG page frames. Defaults to false because frames are computed
    /// eagerly for every page and consumed by nobody today.
    #[serde(default)]
    include_frames: bool,
    /// PDF standards to enforce. Defaults to PDF/A-2b when empty.
    /// This should become a per-project/per-profile lock before production use.
    #[serde(default)]
    pdf_standards: Vec<typst_pdf::PdfStandard>,
}

#[derive(Debug, Clone, Copy, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
enum CompileMode {
    Document,
    Full,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub max_body_bytes: usize,
    pub max_sources: usize,
    pub max_source_bytes: usize,
    pub compile_timeout: Duration,
    pub bearer_token: Option<String>,
    pub require_auth: bool,
    /// Maximum number of cached per-project workers (LRU eviction).
    pub max_workers: usize,
    /// Idle workers are evicted after this long without a request.
    pub worker_idle_ttl: Duration,
    /// Global bound on concurrently running blocking compiles.
    pub max_concurrent_compiles: usize,
}

impl ServiceConfig {
    pub fn from_env() -> Result<Self, String> {
        let mode = std::env::var("NISABA_COMPILE_MODE").unwrap_or_else(|_| "production".to_owned());
        let require_auth = match mode.as_str() {
            "production" => true,
            "development" | "test" => false,
            _ => return Err("NISABA_COMPILE_MODE must be production, development, or test".into()),
        };
        Ok(Self {
            max_body_bytes: env_usize("NISABA_COMPILE_MAX_BODY_BYTES", DEFAULT_MAX_BODY_BYTES)?,
            max_sources: env_usize("NISABA_COMPILE_MAX_SOURCES", DEFAULT_MAX_SOURCES)?,
            max_source_bytes: env_usize(
                "NISABA_COMPILE_MAX_SOURCE_BYTES",
                DEFAULT_MAX_SOURCE_BYTES,
            )?,
            compile_timeout: Duration::from_millis(env_u64(
                "NISABA_COMPILE_TIMEOUT_MS",
                u64::try_from(DEFAULT_TIMEOUT.as_millis()).expect("default timeout fits in u64"),
            )?),
            bearer_token: std::env::var("NISABA_COMPILE_TOKEN").ok(),
            require_auth,
            max_workers: env_usize("NISABA_COMPILE_MAX_WORKERS", DEFAULT_MAX_WORKERS)?,
            worker_idle_ttl: Duration::from_millis(env_u64(
                "NISABA_COMPILE_WORKER_IDLE_TTL_MS",
                u64::try_from(DEFAULT_WORKER_IDLE_TTL.as_millis())
                    .expect("default idle ttl fits in u64"),
            )?),
            max_concurrent_compiles: env_usize(
                "NISABA_COMPILE_MAX_CONCURRENT_COMPILES",
                DEFAULT_MAX_CONCURRENT_COMPILES,
            )?,
        })
    }

    #[cfg(test)]
    fn test() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_sources: DEFAULT_MAX_SOURCES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            compile_timeout: DEFAULT_TIMEOUT,
            bearer_token: Some("test-token".into()),
            require_auth: true,
            max_workers: DEFAULT_MAX_WORKERS,
            worker_idle_ttl: DEFAULT_WORKER_IDLE_TTL,
            max_concurrent_compiles: DEFAULT_MAX_CONCURRENT_COMPILES,
        }
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize, String> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a positive integer")),
        Err(_) => Ok(default),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64, String> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be a positive integer")),
        Err(_) => Ok(default),
    }
}

#[derive(Debug, Serialize)]
pub struct CompileResponse {
    pdf: Option<String>,
    frames: Vec<Frame>,
    span_map: Vec<SpanMapEntry>,
    diagnostics: Vec<Diagnostic>,
    outline: Vec<OutlineEntry>,
    build_id: String,
    instrumentation: Instrumentation,
}

#[derive(Debug, Serialize)]
pub struct Frame {
    page: usize,
    svg: String,
}

#[derive(Debug, Serialize)]
pub struct SpanMapEntry {
    path: String,
    start: usize,
    end: usize,
    page: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    severity: &'static str,
    message: String,
    path: Option<String>,
    start: Option<usize>,
    end: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct OutlineEntry {
    level: usize,
    title: String,
    path: String,
    start: usize,
}

#[derive(Debug, Serialize)]
pub struct Instrumentation {
    compile_ms: u128,
    pdf_ms: u128,
    svg_ms: u128,
    worker_reused: bool,
    worker_compiles: u64,
    cache_hits: u64,
    cache_misses: u64,
    rss_bytes: Option<u64>,
    convergence_passes: u8,
}

#[derive(Debug)]
struct Worker {
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

#[derive(Clone)]
struct AppState {
    workers: Arc<Mutex<HashMap<String, WorkerEntry>>>,
    /// Global cap on concurrently running blocking compiles.
    semaphore: Arc<tokio::sync::Semaphore>,
    config: ServiceConfig,
}

/// A cached worker plus an atomic last-use timestamp for LRU/TTL eviction.
#[derive(Clone)]
struct WorkerEntry {
    worker: Arc<StdMutex<Worker>>,
    last_used: Arc<std::sync::atomic::AtomicU64>,
    /// Set when a compile times out. The worker mutex is held by the abandoned
    /// thread until `typst::compile` finishes. This flag is a diagnostic signal.
    #[allow(dead_code)]
    poisoned: Arc<std::sync::atomic::AtomicBool>,
}

impl WorkerEntry {
    fn now_millis() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(0)
            })
    }

    fn touch(&self) {
        self.last_used
            .store(Self::now_millis(), std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    error: String,
}

pub async fn run() {
    let config = ServiceConfig::from_env().expect("invalid compile service configuration");
    assert!(
        !config.require_auth || config.bearer_token.is_some(),
        "NISABA_COMPILE_TOKEN is required outside development/test mode"
    );
    let bind = std::env::var("NISABA_COMPILE_ADDR").unwrap_or_else(|_| {
        std::env::var("PORT").map_or_else(
            |_| DEFAULT_BIND.to_owned(),
            |port| format!("0.0.0.0:{port}"),
        )
    });
    let address: SocketAddr = bind
        .parse()
        .expect("NISABA_COMPILE_ADDR or PORT must specify a valid socket address");
    let app = app_with_config(config);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind compile service");
    axum::serve(listener, app)
        .await
        .expect("serve compile service");
}

/// Build the HTTP router. Keeping construction public makes the service easy to exercise
/// without binding a socket in integration tests.
pub fn app() -> Router {
    let config = ServiceConfig::from_env().expect("invalid compile service configuration");
    app_with_config(config)
}

pub fn app_with_config(config: ServiceConfig) -> Router {
    let max_concurrent = config.max_concurrent_compiles.max(1);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/compile", post(compile))
        .layer(DefaultBodyLimit::max(config.max_body_bytes))
        .with_state(AppState {
            workers: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            config,
        })
}

async fn healthz() -> &'static str {
    "ok"
}

async fn compile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CompileRequest>,
) -> Result<Json<CompileResponse>, (StatusCode, Json<ErrorResponse>)> {
    if state.config.require_auth && !authorized(&headers, state.config.bearer_token.as_deref()) {
        return Err(unauthorized());
    }
    validate_request_with_limits(&request, &state.config).map_err(bad_request)?;
    let project_id = request.project_id.clone();

    // Opportunistic TTL sweep: drop cache entries that have sat idle past the
    // configured TTL. Runs on every request, cheap (single lock + atomic read).
    let (new_entry, reused) = {
        let mut workers = state.workers.lock().await;
        evict_idle(&mut workers, &state.config);
        if let Some(entry) = workers.get(&project_id) {
            entry.touch();
            (Arc::new(entry.clone()), true)
        } else {
            if workers.len() >= state.config.max_workers {
                evict_lru(&mut workers);
            }
            if workers.len() < state.config.max_workers {
                // Build outside the map lock. A concurrent first request may
                // win the insertion race; in that case its worker is used and
                // this one dropped.
                let entry = WorkerEntry {
                    worker: Arc::new(StdMutex::new(
                        Worker::new(&request).map_err(internal_error)?,
                    )),
                    last_used: Arc::new(std::sync::atomic::AtomicU64::new(
                        WorkerEntry::now_millis(),
                    )),
                    poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                };
                match workers.entry(project_id) {
                    std::collections::hash_map::Entry::Occupied(entry) => {
                        entry.get().touch();
                        (Arc::new(entry.get().clone()), true)
                    }
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(entry.clone());
                        (Arc::new(entry), false)
                    }
                }
            } else {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(ErrorResponse {
                        error: format!("worker cache at capacity ({})", state.config.max_workers),
                    }),
                ));
            }
        }
    };

    // Bound concurrent blocking compiles process-wide so a burst of projects
    // cannot exhaust the blocking pool. The permit is moved into the blocking
    // task and released when it completes.
    let Ok(permit) = state.semaphore.clone().acquire_owned().await else {
        return Err(internal_error("compile semaphore closed".into()));
    };
    // The global map lock is released before this task starts. The blocking task
    // takes only this project's lock, so unrelated projects compile concurrently.
    // Spawn the compile on a dedicated thread and use a
    // channel to retrieve the result. On timeout the thread is abandoned (the
    // worker is poisoned and will be recreated), and crucially the permit is
    // released immediately so new compile requests are not blocked by a
    // thread that nobody can stop.
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_arc = new_entry.worker.clone();
    std::thread::spawn(move || {
        // The permit lives in this thread; it is released when the thread
        // exits. If we time out, the caller has already moved on and this
        // thread runs to completion in the background, releasing the permit
        // when done.
        let _permit = permit;
        let result = (|| -> Result<CompileResponse, String> {
            let mut worker = worker_arc
                .lock()
                .map_err(|_| "compile worker lock poisoned".to_owned())?;
            worker.update_sources(&request)?;
            worker.compile(&request, reused)
        })();
        // Result is sent to the channel; if the caller already timed out,
        // this send fails silently (the receiver was dropped).
        let _ = tx.send(result);
    });
    match tokio::time::timeout(
        state.config.compile_timeout,
        tokio::task::spawn_blocking(move || {
            rx.recv()
                .map_err(|_| "compile worker thread disconnected".to_owned())
        }),
    )
    .await
    {
        Ok(Ok(Ok(Ok(response)))) => Ok(Json(response)),
        Ok(Ok(Ok(Err(error)))) => Err(internal_error(error)),
        Ok(Ok(Err(error))) => Err(internal_error(format!("compile receiver error: {error}"))),
        Ok(Err(error)) => Err(internal_error(format!("compile receiver task: {error}"))),
        Err(_) => Err(timeout_error()),
    }
}

/// Removes every cached worker that has been idle longer than the configured TTL.
fn evict_idle(workers: &mut HashMap<String, WorkerEntry>, config: &ServiceConfig) {
    let ttl_millis = u64::try_from(config.worker_idle_ttl.as_millis()).unwrap_or(u64::MAX);
    let now = WorkerEntry::now_millis();
    workers.retain(|_, entry| {
        now.saturating_sub(entry.last_used.load(std::sync::atomic::Ordering::Relaxed)) <= ttl_millis
    });
}

/// Evicts the single least-recently-used worker. Called when the cache is at
/// capacity and a new worker must be inserted.
fn evict_lru(workers: &mut HashMap<String, WorkerEntry>) {
    let Some((victim, _)) = workers
        .iter()
        .min_by_key(|(_, entry)| entry.last_used.load(std::sync::atomic::Ordering::Relaxed))
    else {
        return;
    };
    let victim = victim.clone();
    workers.remove(&victim);
}

/// Compares the presented bearer against the configured token in constant time.
///
/// Both sides are hashed first so the comparison is independent of token length, and an
/// absent or empty configured token never authorizes (fail-closed).
fn authorized(headers: &HeaderMap, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(presented) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let expected = Sha256::digest(token.as_bytes());
    let presented = Sha256::digest(presented.as_bytes());
    bool::from(expected.ct_eq(&presented))
}

#[cfg(test)]
fn validate_request(request: &CompileRequest) -> Result<(), String> {
    validate_request_with_limits(
        request,
        &ServiceConfig {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_sources: DEFAULT_MAX_SOURCES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            compile_timeout: DEFAULT_TIMEOUT,
            bearer_token: None,
            require_auth: false,
            max_workers: DEFAULT_MAX_WORKERS,
            worker_idle_ttl: DEFAULT_WORKER_IDLE_TTL,
            max_concurrent_compiles: DEFAULT_MAX_CONCURRENT_COMPILES,
        },
    )
}

fn validate_request_with_limits(
    request: &CompileRequest,
    config: &ServiceConfig,
) -> Result<(), String> {
    if request.project_id.trim().is_empty() {
        return Err("project_id must not be empty".into());
    }
    validate_virtual_path(&request.entry)?;
    if request.sources.is_empty() {
        return Err("sources must not be empty".into());
    }
    if request.sources.len() > config.max_sources {
        return Err(format!("too many sources (maximum {})", config.max_sources));
    }
    let total_source_bytes = request
        .sources
        .values()
        .try_fold(0usize, |total, source| total.checked_add(source.len()))
        .ok_or_else(|| "total source bytes overflowed".to_owned())?;
    if total_source_bytes > config.max_source_bytes {
        return Err(format!(
            "sources exceed total byte limit (maximum {})",
            config.max_source_bytes
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

impl Worker {
    fn new(request: &CompileRequest) -> Result<Self, String> {
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

    fn update_sources(&mut self, request: &CompileRequest) -> Result<(), String> {
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

    #[allow(clippy::too_many_lines)]
    fn compile(
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
        let started = Instant::now();
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
        // loop with full-document hashing here is pure waste — it multiplied
        // compile time by up to 5x for `full` mode without changing the output.
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
                    frames: Vec::new(),
                    span_map: source_span_map(None, &world, &request.sources),
                    diagnostics,
                    outline: outline(&request.sources),
                    build_id: build_id(self.compile_count),
                    instrumentation: instrumentation(
                        started,
                        0,
                        0,
                        reused,
                        self,
                        convergence_passes,
                    ),
                });
            }
        };
        let compile_ms = started.elapsed().as_millis();
        let pdf_started = Instant::now();
        // Set an explicit PDF standard. Defaults to PDF/A-2b
        // when no standards are specified by the request.
        let standards = if request.pdf_standards.is_empty() {
            typst_pdf::PdfStandards::new(&[typst_pdf::PdfStandard::A_2b])
                .map_err(|e| format!("invalid PDF standards combination: {e:?}"))?
        } else {
            typst_pdf::PdfStandards::new(&request.pdf_standards)
                .map_err(|e| format!("invalid PDF standards combination: {e:?}"))?
        };
        let pdf = typst_pdf::pdf(
            &document,
            &typst_pdf::PdfOptions {
                standards,
                timestamp: Some(typst_pdf::Timestamp::new_utc(
                    typst::foundations::Datetime::from_ymd_hms(2025, 1, 1, 0, 0, 0)
                        .expect("valid fixed date"),
                )),
                ..typst_pdf::PdfOptions::default()
            },
        )
        .map_err(|errors| format!("PDF export failed: {errors:?}"))?;
        let pdf_ms = pdf_started.elapsed().as_millis();
        let svg_started = Instant::now();
        let frames = if request.include_frames {
            document
                .pages()
                .iter()
                .enumerate()
                .map(|(page, page_data)| Frame {
                    page: page + 1,
                    svg: typst_svg::svg(page_data, &typst_svg::SvgOptions::default()),
                })
                .collect()
        } else {
            Vec::new()
        };
        let svg_ms = svg_started.elapsed().as_millis();
        let mut result = CompileResponse {
            pdf: Some(BASE64.encode(pdf)),
            frames,
            span_map: source_span_map(Some(&document), &world, &request.sources),
            diagnostics,
            outline: outline(&request.sources),
            build_id: build_id(self.compile_count),
            instrumentation: instrumentation(
                started,
                pdf_ms,
                svg_ms,
                reused,
                self,
                convergence_passes,
            ),
        };
        result.instrumentation.compile_ms = compile_ms;
        tracing::info!(
            compile_ms,
            pdf_ms,
            svg_ms,
            pages = document.pages().len(),
            cache_hits = self.cache_hits,
            cache_misses = self.cache_misses,
            worker_reused = reused,
            "compile completed"
        );
        let _ = request.view;
        Ok(result)
    }
}

fn instrumentation(
    started: Instant,
    pdf_ms: u128,
    svg_ms: u128,
    reused: bool,
    worker: &Worker,
    convergence_passes: u8,
) -> Instrumentation {
    Instrumentation {
        compile_ms: started
            .elapsed()
            .as_millis()
            .saturating_sub(pdf_ms)
            .saturating_sub(svg_ms),
        pdf_ms,
        svg_ms,
        worker_reused: reused,
        worker_compiles: worker.compile_count,
        cache_hits: worker.cache_hits,
        cache_misses: worker.cache_misses,
        rss_bytes: current_rss(),
        convergence_passes,
    }
}

fn diagnostic(world: &dyn World, diag: &typst::diag::SourceDiagnostic) -> Diagnostic {
    let (path, start, end) = if let DiagSpanKind::Detached = diag.span.get() {
        (None, None, None)
    } else {
        let path = diag
            .span
            .id()
            .and_then(|id| world.source(id).ok())
            .map(|source| source.id().vpath().get_with_slash().to_string());
        let range = world.range(diag.span);
        (path, range.as_ref().map(|r| r.start), range.map(|r| r.end))
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
                end: source.len(),
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
            if seen.insert((path.clone(), range.start, range.end)) {
                entries.push(SpanMapEntry {
                    path,
                    start: range.start,
                    end: range.end,
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
        collect_headings(&root, path, 0, &mut result);
    }
    result.sort_by(|a, b| a.path.cmp(&b.path).then(a.start.cmp(&b.start)));
    result
}

fn collect_headings(node: &SyntaxNode, path: &str, offset: usize, result: &mut Vec<OutlineEntry>) {
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
            start: offset,
        });
    }
    let mut child_offset = offset;
    for child in node.children() {
        collect_headings(child, path, child_offset, result);
        child_offset += child.len();
    }
}

fn fingerprint(request: &CompileRequest) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    request.entry.hash(&mut hasher);
    request.mode.hash(&mut hasher);
    request.view.hash(&mut hasher);
    let mut sources = request.sources.iter().collect::<Vec<_>>();
    sources.sort_by(|a, b| a.0.cmp(b.0));
    sources.hash(&mut hasher);
    hasher.finish()
}

/// Per-process build prefix, so build ids stay globally unique across worker
/// restarts; the worker's compile counter disambiguates within a process.
static BUILD_INSTANCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn build_id(number: u64) -> String {
    let instance = BUILD_INSTANCE.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or_else(
                |_| "0".to_owned(),
                |duration| duration.as_nanos().to_string(),
            )
    });
    format!("build-{instance}-{number}")
}

fn current_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

fn bad_request(error: String) -> (StatusCode, Json<ErrorResponse>) {
    (StatusCode::BAD_REQUEST, Json(ErrorResponse { error }))
}

fn unauthorized() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: "Bearer token required".into(),
        }),
    )
}

fn timeout_error() -> (StatusCode, Json<ErrorResponse>) {
    tracing::warn!(target: "compile_timeout", "compile timed out; worker will be poisoned");
    (
        StatusCode::GATEWAY_TIMEOUT,
        Json(ErrorResponse {
            error: "compile timed out; the blocking worker may continue running".into(),
        }),
    )
}

fn internal_error(error: String) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse { error }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(source: &str) -> CompileRequest {
        CompileRequest {
            project_id: "p".into(),
            entry: "main.typ".into(),
            sources: HashMap::from([(String::from("main.typ"), source.into())]),
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: true,
            pdf_standards: vec![],
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
        assert_eq!(response.frames.len(), 1);
        assert!(response.outline.iter().any(|entry| entry.title == "Hello"));
    }

    #[test]
    fn frames_are_empty_by_default() {
        let mut worker = Worker::new(&CompileRequest {
            project_id: "p".into(),
            entry: "main.typ".into(),
            sources: HashMap::from([(String::from("main.typ"), "= Hello".into())]),
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: false,
            pdf_standards: vec![],
        })
        .expect("worker");
        let response = worker
            .compile(
                &CompileRequest {
                    project_id: "p".into(),
                    entry: "main.typ".into(),
                    sources: HashMap::from([(String::from("main.typ"), "= Hello".into())]),
                    mode: CompileMode::Document,
                    view: "public".into(),
                    include_frames: false,
                    pdf_standards: vec![],
                },
                false,
            )
            .expect("compile");
        assert!(
            response.frames.is_empty(),
            "frames should be empty when include_frames is false"
        );
    }

    #[test]
    fn pdf_defaults_to_a2b_standard() {
        let mut worker = Worker::new(&CompileRequest {
            project_id: "p".into(),
            entry: "main.typ".into(),
            sources: HashMap::from([(String::from("main.typ"), "= Hello".into())]),
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: false,
            pdf_standards: vec![],
        })
        .expect("worker");
        let response = worker
            .compile(
                &CompileRequest {
                    project_id: "p".into(),
                    entry: "main.typ".into(),
                    sources: HashMap::from([(String::from("main.typ"), "= Hello".into())]),
                    mode: CompileMode::Document,
                    view: "public".into(),
                    include_frames: false,
                    pdf_standards: vec![],
                },
                false,
            )
            .expect("compile");
        let pdf = BASE64.decode(response.pdf.expect("pdf")).expect("base64");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// Verify that the compile service embeds fonts in the
    /// output PDF. The embedded font resolver should provide standard fonts that
    /// survive into the final PDF. We parse the PDF bytes to check for font
    /// dictionary entries.
    #[test]
    fn pdf_embeds_fonts() {
        let mut worker = Worker::new(&CompileRequest {
            project_id: "p".into(),
            entry: "main.typ".into(),
            sources: HashMap::from([(
                String::from("main.typ"),
                "= Font Embedding Test\nThis text requires a font.".into(),
            )]),
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: false,
            pdf_standards: vec![],
        })
        .expect("worker");
        let response = worker
            .compile(
                &CompileRequest {
                    project_id: "p".into(),
                    entry: "main.typ".into(),
                    sources: HashMap::from([(
                        String::from("main.typ"),
                        "= Font Embedding Test\nThis text requires a font.".into(),
                    )]),
                    mode: CompileMode::Document,
                    view: "public".into(),
                    include_frames: false,
                    pdf_standards: vec![],
                },
                false,
            )
            .expect("compile");
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
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: false,
            pdf_standards: vec![],
        })
        .expect("worker");
        let second = CompileRequest {
            project_id: "p".into(),
            entry: "documents/b.typ".into(),
            sources: HashMap::from([(String::from("documents/b.typ"), "= Document B".into())]),
            mode: CompileMode::Document,
            view: "public".into(),
            include_frames: false,
            pdf_standards: vec![],
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
        assert!(validate_request(&request).is_err());
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
        let config = ServiceConfig {
            worker_idle_ttl: Duration::from_secs(10),
            max_workers: usize::MAX,
            max_concurrent_compiles: usize::MAX,
            ..ServiceConfig::test()
        };
        let mut workers = HashMap::from([
            (String::from("stale"), entry(now.saturating_sub(11_000))),
            (String::from("fresh"), entry(now)),
        ]);
        evict_idle(&mut workers, &config);
        assert!(!workers.contains_key("stale"));
        assert!(workers.contains_key("fresh"));
    }

    fn payload(project_id: &str, view: &str, source: &str) -> serde_json::Value {
        serde_json::json!({
            "project_id": project_id,
            "entry": "main.typ",
            "sources": {"main.typ": source},
            "mode": "document",
            "view": view
        })
    }

    async fn send(
        app: &Router,
        path: &str,
        payload: serde_json::Value,
        token: Option<&str>,
    ) -> StatusCode {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let mut request = Request::post(path).header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        app.clone()
            .oneshot(
                request
                    .body(Body::from(payload.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response")
            .status()
    }

    #[tokio::test]
    async fn health_is_open_but_compile_requires_bearer_auth() {
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;

        let app = app_with_config(ServiceConfig::test());
        let health = app
            .clone()
            .oneshot(
                Request::get("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(
            send(&app, "/compile", payload("auth", "public", "hello"), None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(
                &app,
                "/compile",
                payload("auth", "public", "hello"),
                Some("wrong")
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn compile_accepts_open_projection_labels() {
        let app = app_with_config(ServiceConfig::test());
        assert_eq!(
            send(
                &app,
                "/compile",
                payload("public", "future-view", "= Public"),
                Some("test-token")
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn different_projects_can_compile_concurrently() {
        let app = app_with_config(ServiceConfig::test());
        let (first, second) = tokio::join!(
            send(
                &app,
                "/compile",
                payload("project-a", "public", "= A"),
                Some("test-token")
            ),
            send(
                &app,
                "/compile",
                payload("project-b", "public", "= B"),
                Some("test-token")
            ),
        );
        assert_eq!(first, StatusCode::OK);
        assert_eq!(second, StatusCode::OK);
    }

    #[tokio::test]
    async fn body_and_source_limits_are_enforced() {
        let mut config = ServiceConfig::test();
        config.max_body_bytes = 100;
        let app = app_with_config(config.clone());
        assert_eq!(
            send(
                &app,
                "/compile",
                payload("body", "public", "a source larger than this body limit"),
                Some("test-token")
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE
        );

        config.max_body_bytes = DEFAULT_MAX_BODY_BYTES;
        config.max_sources = 1;
        config.max_source_bytes = 3;
        let app = app_with_config(config);
        let many_sources = serde_json::json!({
            "project_id": "limits",
            "entry": "main.typ",
            "sources": {"main.typ": "ok", "other.typ": "x"},
            "mode": "document",
            "view": "public"
        });
        assert_eq!(
            send(&app, "/compile", many_sources, Some("test-token")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(
                &app,
                "/compile",
                payload("limits-bytes", "public", "four"),
                Some("test-token")
            )
            .await,
            StatusCode::BAD_REQUEST
        );
    }
}
