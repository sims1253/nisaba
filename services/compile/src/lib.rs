//! Nisaba's compile service: the HTTP plane over the pure compilation core.
//!
//! This crate owns the server concerns only — the axum router, bearer auth,
//! request/body limits, the global concurrency semaphore, per-compile
//! timeouts, and the RSS gauge — and delegates the compilation itself (the
//! worker cache, Typst world assembly, span maps, outlines, diagnostics) to
//! [`nisaba_compile_core`], which is kept I/O-free and tokio-free so the same
//! code can later run behind a wasm boundary in the browser (issue #20,
//! stage 2).

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use nisaba_compile_core::{
    CompileRequest, CompileResponse, DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_SOURCES,
    DEFAULT_MAX_WORKERS, DEFAULT_WORKER_IDLE_TTL, Worker, WorkerEntry, evict_idle, evict_lru,
    validate_request,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

const DEFAULT_BIND: &str = "0.0.0.0:8080";
const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Per-compile timeout. COUPLED to the app service's compile HTTP client
/// (`HttpCompileClient`, `services/app/src/compile_client.rs`), which gives up
/// after 150 s: raising this above 150 s via `NISABA_COMPILE_TIMEOUT_MS`
/// makes clients receive 502s while the abandoned worker still burns a
/// compile slot. Raise both together.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(2);
/// Global bound on concurrently-running blocking compiles, regardless of how
/// many workers/projects are cached. Prevents unbounded `spawn_blocking` usage.
const DEFAULT_MAX_CONCURRENT_COMPILES: usize = 8;

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
    /// (`NISABA_COMPILE_MODE` unset ⇒ `require_auth: true`, no token). The
    /// source/worker limits are `nisaba_compile_core`'s canonical constants so
    /// the service and any future wasm host share one set of defaults (the
    /// values were previously restated field-by-field here, drifting from the
    /// validation defaults). Wiring this into a binary fails loudly: `run()`
    /// asserts that required auth has a token.
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

#[derive(Clone)]
struct AppState {
    workers: Arc<Mutex<HashMap<String, WorkerEntry>>>,
    /// Global cap on concurrently running blocking compiles.
    semaphore: Arc<tokio::sync::Semaphore>,
    config: ServiceConfig,
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
    validate_request(
        &request,
        state.config.max_sources,
        state.config.max_source_bytes,
    )
    .map_err(bad_request)?;
    let project_id = request.project_id.clone();

    // Opportunistic TTL sweep: drop cache entries that have sat idle past the
    // configured TTL. Runs on every request, cheap (single lock + atomic read).
    let existing = {
        let mut workers = state.workers.lock().await;
        evict_idle(&mut workers, state.config.worker_idle_ttl);
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
        Ok(Ok(Ok(mut response))) => {
            // Host-plane gauge: the core leaves rss_bytes empty (reading
            // /proc is server I/O); fill it in just before serializing so the
            // response schema is unchanged.
            response.instrumentation.rss_bytes = current_rss();
            Ok(Json(response))
        }
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
