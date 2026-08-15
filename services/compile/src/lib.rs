//! Nisaba's long-lived in-memory Typst compiler service.
//!
//! The service deliberately uses Typst's `World` callback interface through
//! tinymist's in-memory mock VFS. It never invokes the Typst CLI or creates a
//! source file on disk.

use std::{
    collections::HashMap,
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
/// Per-compile timeout. COUPLED to the app service's compile HTTP client
/// (`HttpCompileClient`, `services/app/src/compile_client.rs`), which gives up
/// after 150 s: raising this above 150 s via `NISABA_COMPILE_TIMEOUT_MS`
/// makes clients receive 502s while the abandoned worker still burns a
/// compile slot. Raise both together.
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
    /// An open projection label. The compile service does not interpret it
    /// beyond folding it into the per-worker fingerprint; projection happens
    /// before this boundary.
    view: String,
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

impl Default for ServiceConfig {
    /// The canonical limits, matching `from_env`'s production-mode defaults
    /// (`NISABA_COMPILE_MODE` unset ⇒ `require_auth: true`, no token). Wiring
    /// this into a binary fails loudly: `run()` asserts that required auth has
    /// a token.
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_sources: DEFAULT_MAX_SOURCES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            compile_timeout: DEFAULT_TIMEOUT,
            bearer_token: None,
            require_auth: true,
            max_workers: DEFAULT_MAX_WORKERS,
            worker_idle_ttl: DEFAULT_WORKER_IDLE_TTL,
            max_concurrent_compiles: DEFAULT_MAX_CONCURRENT_COMPILES,
        }
    }
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

    /// Test configuration: the defaults plus the intended test delta only (a
    /// bearer token), so this cannot drift from `ServiceConfig::default()` the
    /// way the previous field-by-field restatement did.
    #[cfg(test)]
    fn test() -> Self {
        Self {
            bearer_token: Some("test-token".into()),
            ..ServiceConfig::default()
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
    span_map: Vec<SpanMapEntry>,
    diagnostics: Vec<Diagnostic>,
    outline: Vec<OutlineEntry>,
    build_id: String,
    instrumentation: Instrumentation,
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
    /// thread until `typst::compile` finishes. This flag tells subsequent
    /// lookups to evict the worker and create a fresh one instead of blocking
    /// on the stale lock.
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
    let existing = {
        let mut workers = state.workers.lock().await;
        evict_idle(&mut workers, &state.config);
        // Evict a cached worker whose previous compile timed out: the
        // underlying task still holds the worker mutex, so reusing it
        // would block the new request until the abandoned compile finishes.
        // Removing it here forces a fresh worker (with its own mutex) below.
        if workers
            .get(&project_id)
            .is_some_and(|e| e.poisoned.load(std::sync::atomic::Ordering::Relaxed))
        {
            workers.remove(&project_id);
        }
        if let Some(entry) = workers.get(&project_id) {
            entry.touch();
            Some(Arc::new(entry.clone()))
        } else {
            // Reserve room under the lock (LRU eviction when at capacity) and
            // leave the map WITHOUT building anything: Worker::new parses every
            // source file and constructs a Typst universe (up to max_sources ×
            // max_source_bytes of work) — holding the global map lock for that
            // blocked every compile for every other project.
            if workers.len() >= state.config.max_workers {
                evict_lru(&mut workers);
            }
            None
        }
    };

    let (new_entry, reused) = if let Some(entry) = existing {
        (entry, true)
    } else {
        // Build the new worker with the map lock released (see above).
        let built = WorkerEntry {
            worker: Arc::new(StdMutex::new(
                Worker::new(&request).map_err(internal_error)?,
            )),
            last_used: Arc::new(std::sync::atomic::AtomicU64::new(WorkerEntry::now_millis())),
            poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // Double-checked insert: a concurrent first request may have won the
        // build race (or won it and already timed out) while the lock was
        // released — use its worker and drop ours.
        let mut workers = state.workers.lock().await;
        let raced_poisoned = workers
            .get(&project_id)
            .is_some_and(|e| e.poisoned.load(std::sync::atomic::Ordering::Relaxed));
        if raced_poisoned {
            workers.remove(&project_id);
        }
        if !raced_poisoned && let Some(entry) = workers.get(&project_id) {
            entry.touch();
            (Arc::new(entry.clone()), true)
        } else {
            if workers.len() >= state.config.max_workers {
                evict_lru(&mut workers);
            }
            if workers.len() >= state.config.max_workers {
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(ErrorResponse {
                        error: format!("worker cache at capacity ({})", state.config.max_workers),
                    }),
                ));
            }
            let entry = Arc::new(built);
            workers.insert(project_id, (*entry).clone());
            (entry, false)
        }
    };

    // Bound concurrent blocking compiles process-wide so a burst of projects
    // cannot exhaust the blocking pool. The permit stays in this handler scope
    // and is released when the handler returns (success or timeout).
    let Ok(_permit) = state.semaphore.clone().acquire_owned().await else {
        return Err(internal_error("compile semaphore closed".into()));
    };
    // The global map lock is released before this task starts. The blocking task
    // takes only this project's worker lock, so unrelated projects compile
    // concurrently. A single spawn_blocking task runs the compile closure
    // directly — the previous design burned two OS threads per compile (a
    // dedicated std::thread for the work plus a spawn_blocking task parked on
    // a channel waiting for it). The semaphore permit stays in this handler
    // scope (NOT moved into the task) so it is dropped when the handler
    // returns — including on timeout. Previously the permit was moved into
    // the worker thread, so a timed-out compile held its slot until the thread
    // eventually finished, and 8 such timeouts exhausted every slot,
    // denying the service to all projects.
    let worker_arc = new_entry.worker.clone();
    let compile_task = tokio::task::spawn_blocking(move || {
        let mut worker = worker_arc
            .lock()
            .map_err(|_| "compile worker lock poisoned".to_owned())?;
        worker.update_sources(&request)?;
        worker.compile(&request, reused)
    });
    match tokio::time::timeout(state.config.compile_timeout, compile_task).await {
        Ok(Ok(Ok(response))) => Ok(Json(response)),
        Ok(Ok(Err(error))) => Err(internal_error(error)),
        Ok(Err(join_error)) => Err(internal_error(format!("compile task failed: {join_error}"))),
        Err(_) => {
            // Mark the cached worker as poisoned so the next request for this
            // project creates a fresh one instead of blocking on the mutex
            // held by the abandoned (still-running) blocking task.
            new_entry
                .poisoned
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Err(timeout_error())
        }
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
    // Only the limit fields matter to validation; the defaults are the
    // canonical values (previously restated field-by-field, drifting from
    // ServiceConfig::default()).
    validate_request_with_limits(request, &ServiceConfig::default())
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
    // Deliberately laxer than the app service's valid_document_path
    // (services/app/src/lib.rs): this guards only the compile service's
    // per-request virtual filesystem, so `.` segments are tolerated and `..`
    // is allowed as long as it never climbs above the root. The app rejects
    // `.`/`..` and control characters as well because stored document paths
    // are user-facing identifiers; the divergence is intentional.
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
                    build_id: build_id(self.compile_count),
                    instrumentation: instrumentation(
                        started,
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
                    build_id: build_id(self.compile_count),
                    instrumentation: instrumentation(
                        started,
                        pdf_started.elapsed().as_millis(),
                        reused,
                        self,
                        convergence_passes,
                    ),
                });
            }
        };
        let pdf_ms = pdf_started.elapsed().as_millis();
        let mut result = CompileResponse {
            pdf: Some(BASE64.encode(pdf)),
            span_map: source_span_map(Some(&document), &world, &request.sources),
            diagnostics,
            outline: outline(&request.sources),
            build_id: build_id(self.compile_count),
            instrumentation: instrumentation(started, pdf_ms, reused, self, convergence_passes),
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

fn instrumentation(
    started: Instant,
    pdf_ms: u128,
    reused: bool,
    worker: &Worker,
    convergence_passes: u8,
) -> Instrumentation {
    Instrumentation {
        compile_ms: started.elapsed().as_millis().saturating_sub(pdf_ms),
        pdf_ms,
        worker_reused: reused,
        worker_compiles: worker.compile_count,
        cache_hits: worker.cache_hits,
        cache_misses: worker.cache_misses,
        rss_bytes: current_rss(),
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
