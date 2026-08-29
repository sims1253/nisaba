//! # nisaba-compile-wasm
//!
//! WebAssembly compile wrapper for the Nisaba web client (issue #20, stage
//! 2b).
//!
//! This crate drives the pure compilation core in [`nisaba_compile_core`]
//! exactly the way the `compile` service does, minus the HTTP plane: no axum
//! router, bearer auth, body limits, concurrency semaphore, per-compile
//! timeouts, or `/proc/self/status` RSS reader. The browser gets the same
//! `Worker::new` → `update_sources` → `compile` pipeline, the same span map,
//! outline, and diagnostics shaping, and — because the core pins the PDF
//! timestamp — the same bytes the server produces.
//!
//! ## Boundary design
//!
//! Like `nisaba-core-wasm` (the stage 1 projection wrapper), each feature is
//! a pair: a plain-Rust half whose errors are `Result<_, String>` (fully
//! exercisable by the native test suite) and a `#[wasm_bindgen]` half whose
//! errors surface as JS `Error` objects (constructing a [`JsError`] calls a
//! JS import, which panics on non-wasm targets, so those halves run under
//! wasm-bindgen-test only).
//!
//! The boundary is strings in, strings out: requests are the compile
//! service's HTTP body verbatim (`{"project_id", "entry", "sources",
//! "view"}` — the DTO lives in the core, shared by both hosts), and compile
//! results are the service's HTTP response verbatim (`pdf` base64, span map,
//! diagnostics, outline, `build_id`, `instrumentation`; serialized from the
//! core's `CompileResponse`). Stage 2c can therefore pass the objects the
//! client already builds for `POST /api/compile` straight through.
//!
//! Two shapes are exported, mirroring how the service holds workers:
//!
//! - [`CompileWorker`] (JS: created by [`new_compile_worker`]) is one
//!   long-lived project worker — the browser-tab equivalent of a cache hit on
//!   the server. Keep it alive across keystrokes; the warm `comemo` caches
//!   die with it.
//! - [`CompileWorkers`] (JS: created by [`new_compile_workers`]) is the
//!   per-project worker cache with the service's LRU/TTL eviction
//!   ([`WorkerEntry`], `evict_idle`/`evict_lru` from the core), sized by the
//!   host. One per Web Worker is enough for a browser tab.
//!
//! Requests are validated with the core's canonical limits
//! ([`DEFAULT_MAX_SOURCES`] / [`DEFAULT_MAX_SOURCE_BYTES`]); the server's
//! HTTP-only body limit has no analogue here (there is no body). The
//! `instrumentation.rss_bytes` field stays `None`: the core deliberately
//! leaves it to the host, and reading process memory is not portable to the
//! browser.
//!
//! ## Fonts
//!
//! The `tinymist-world` `mock` feature embeds `typst-assets` fonts in the
//! binary — the same mechanism the compile service uses — so no font I/O
//! happens at runtime and wasm compiles embed the same fonts as the server.
//! The cost is a wasm module of tens of megabytes; the PR carrying this crate
//! reports the measured size. Stage 2c loads it lazily in a Web Worker.
//!
//! ## Parity
//!
//! `tests/parity.rs` compiles the same fixture sources through the native
//! core (the code path the service drives) and through this boundary on
//! `wasm32-unknown-unknown` (wasm-bindgen-test), and asserts both are
//! byte-identical to the committed golden responses — PDF bytes included,
//! which the core's fixed PDF timestamp makes achievable. Native and wasm
//! suites run the identical assertions.
//!
//! ## Gaps left to stage 2c (tracked in issue #20)
//!
//! The Web Worker wiring, lazy loading, and the server-fallback toggle live
//! in the web client; this crate only provides the module. Per-compile
//! timeouts do not exist here (no JS API can interrupt a running wasm
//! compile), so the pool's `poisoned` flag — the server's mechanism for
//! abandoning timed-out compiles — can never be set and is not exposed.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use nisaba_compile_core::{
    CompileRequest, DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_SOURCES, Worker, WorkerEntry, evict_idle,
    evict_lru, validate_request,
};
use wasm_bindgen::prelude::*;

/// Parses and validates a compile request the way the service's handler does:
/// JSON in the core's DTO shape, then the core's request-shape guards under
/// the canonical limits.
fn checked_request(request_json: &str) -> Result<CompileRequest, String> {
    let request = serde_json::from_str(request_json)
        .map_err(|error| format!("invalid compile request JSON: {error}"))?;
    validate_request(&request, DEFAULT_MAX_SOURCES, DEFAULT_MAX_SOURCE_BYTES)?;
    Ok(request)
}

/// Serializes a core `CompileResponse` into the service's wire JSON.
fn response_json(response: &nisaba_compile_core::CompileResponse) -> Result<String, String> {
    serde_json::to_string(response).map_err(|error| format!("serialize compile response: {error}"))
}

/// Plain-Rust half of the single-worker boundary: one long-lived project
/// worker plus the warm/reused bookkeeping the service tracks in its worker
/// cache. Mirrors the service's per-request flow (validate, then drive the
/// worker) with the canonical limits.
///
/// The JS-facing half is [`JsCompileWorker`], created by
/// [`new_compile_worker`].
#[derive(Debug)]
pub struct CompileWorker {
    worker: Worker,
    /// Whether this worker has already served a compile — the boundary's
    /// equivalent of the service's cache-hit flag, reported as
    /// `instrumentation.worker_reused`.
    served: bool,
}

impl CompileWorker {
    /// Builds a worker for the request (validate, then `Worker::new`).
    ///
    /// # Errors
    /// Returns an error string when the JSON does not fit the request DTO,
    /// the request violates the core's shape rules (empty project id, virtual
    /// path traversal, missing entry, source caps), or the Typst universe
    /// cannot be created.
    pub fn new(request_json: &str) -> Result<Self, String> {
        let request = checked_request(request_json)?;
        Ok(Self {
            worker: Worker::new(&request)?,
            served: false,
        })
    }

    /// Applies the request's sources incrementally (`Worker::update_sources`):
    /// removes gone paths, updates changed ones, re-roots on a changed entry.
    ///
    /// # Errors
    /// Returns an error string on invalid requests or failed VFS updates.
    pub fn update_sources(&mut self, request_json: &str) -> Result<(), String> {
        let request = checked_request(request_json)?;
        self.worker.update_sources(&request)
    }

    /// Compiles the request (`Worker::compile`) and returns the serialized
    /// `CompileResponse`. The request's sources are applied first
    /// (`Worker::update_sources`), exactly as the service handler and the
    /// pool half below do before every compile — a caller passing edited
    /// sources straight here must not get a PDF from the stale universe; the
    /// update is a no-op when the sources are unchanged. The first compile on
    /// a fresh worker reports `worker_reused: false`, later ones `true` —
    /// the same values the service reports for a cold and a warm worker.
    ///
    /// # Errors
    /// Returns an error string on invalid requests or serialization failure;
    /// compile failures are ordinary diagnostics inside a successful
    /// response, exactly as in the service.
    pub fn compile(&mut self, request_json: &str) -> Result<String, String> {
        let request = checked_request(request_json)?;
        self.worker.update_sources(&request)?;
        let response = self.worker.compile(&request, self.served)?;
        self.served = true;
        response_json(&response)
    }
}

/// Plain-Rust half of the worker-pool boundary: the per-project worker cache
/// the compile service keeps, with its LRU/TTL eviction, driven the way the
/// service's handler drives it (minus the concurrency semaphore and the
/// timeout poisoning, neither of which applies to a single-threaded wasm
/// host that cannot abandon a running compile).
///
/// The JS-facing half is [`JsCompileWorkers`], created by
/// [`new_compile_workers`].
pub struct CompileWorkers {
    workers: HashMap<String, WorkerEntry>,
    max_workers: usize,
    worker_idle_ttl: Duration,
}

impl CompileWorkers {
    /// An empty pool bounded by `max_workers` (LRU eviction at capacity) and
    /// `worker_idle_ttl` (idle sweep before every compile).
    #[must_use]
    pub fn new(max_workers: usize, worker_idle_ttl: Duration) -> Self {
        Self {
            workers: HashMap::new(),
            max_workers,
            worker_idle_ttl,
        }
    }

    /// Compiles through the per-project worker cache, mirroring the service:
    /// TTL sweep, touch-or-insert (LRU eviction at capacity, capacity error
    /// when even eviction cannot make room), then `update_sources` and
    /// `compile` under the entry's worker lock.
    ///
    /// On platforms without a wall clock (`SystemTime::now` unavailable,
    /// i.e. `wasm32-unknown-unknown`) every entry's last-use timestamp stays
    /// at the epoch, so the TTL sweep keeps everything and LRU eviction picks
    /// an arbitrary entry; the capacity bound still holds. `rss_bytes` stays
    /// `None` (see the crate docs).
    ///
    /// # Errors
    /// Returns an error string on invalid requests, capacity overflow
    /// (`worker cache at capacity (n)`), or worker failures; compile
    /// failures are ordinary diagnostics inside a successful response.
    pub fn compile(&mut self, request_json: &str) -> Result<String, String> {
        let request = checked_request(request_json)?;
        let project_id = request.project_id.clone();
        // Opportunistic TTL sweep, exactly as the service does per request.
        evict_idle(&mut self.workers, self.worker_idle_ttl);
        let (entry, reused) = if let Some(entry) = self.workers.get(&project_id) {
            entry.touch();
            (Arc::new(entry.clone()), true)
        } else {
            if self.workers.len() >= self.max_workers {
                evict_lru(&mut self.workers);
            }
            if self.workers.len() >= self.max_workers {
                return Err(format!("worker cache at capacity ({})", self.max_workers));
            }
            let built = WorkerEntry {
                worker: Arc::new(StdMutex::new(Worker::new(&request)?)),
                last_used: Arc::new(std::sync::atomic::AtomicU64::new(WorkerEntry::now_millis())),
                poisoned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            };
            let entry = Arc::new(built);
            self.workers.insert(project_id, (*entry).clone());
            (entry, false)
        };
        let mut worker = entry
            .worker
            .lock()
            .map_err(|_| "compile worker lock poisoned".to_owned())?;
        worker.update_sources(&request)?;
        let response = worker.compile(&request, reused)?;
        response_json(&response)
    }
}

/// JS-facing single project worker; errors surface as JS `Error` objects
/// carrying the same message strings as the plain half ([`CompileWorker`]).
/// Create with [`new_compile_worker`].
#[derive(Debug)]
#[wasm_bindgen]
pub struct JsCompileWorker {
    inner: CompileWorker,
}

#[wasm_bindgen]
impl JsCompileWorker {
    /// See [`CompileWorker::update_sources`].
    pub fn update_sources(&mut self, request_json: &str) -> Result<(), JsError> {
        self.inner
            .update_sources(request_json)
            .map_err(|error| JsError::new(&error))
    }

    /// See [`CompileWorker::compile`].
    pub fn compile(&mut self, request_json: &str) -> Result<String, JsError> {
        self.inner
            .compile(request_json)
            .map_err(|error| JsError::new(&error))
    }
}

/// JS-facing worker pool; errors surface as JS `Error` objects carrying the
/// same message strings as the plain half ([`CompileWorkers`]). Create with
/// [`new_compile_workers`].
#[wasm_bindgen]
pub struct JsCompileWorkers {
    inner: CompileWorkers,
}

#[wasm_bindgen]
impl JsCompileWorkers {
    /// See [`CompileWorkers::compile`].
    pub fn compile(&mut self, request_json: &str) -> Result<String, JsError> {
        self.inner
            .compile(request_json)
            .map_err(|error| JsError::new(&error))
    }
}

/// Creates a single long-lived project worker from a compile request (the
/// service's HTTP body shape). Keep the returned [`JsCompileWorker`] alive
/// across keystrokes — the warm `comemo` caches die with it.
///
/// # Errors
/// See [`CompileWorker::new`].
#[wasm_bindgen]
pub fn new_compile_worker(request_json: &str) -> Result<JsCompileWorker, JsError> {
    CompileWorker::new(request_json)
        .map(|inner| JsCompileWorker { inner })
        .map_err(|error| JsError::new(&error))
}

/// Creates a per-project worker cache with the service's LRU/TTL eviction:
/// at most `max_workers` workers (least recently used evicted at capacity),
/// idle workers dropped after `idle_ttl_millis`.
///
/// The TTL is clamped to `[0, ~100 years]`; `NaN` counts as zero.
#[must_use]
#[wasm_bindgen]
pub fn new_compile_workers(max_workers: usize, idle_ttl_millis: f64) -> JsCompileWorkers {
    // f64::clamp propagates NaN and Duration::from_secs_f64(NaN) panics —
    // a JS `Number(undefined)` or failed parseInt must trap neither the
    // module nor the caller, so NaN is mapped to the zero-TTL documented
    // above before the clamp runs.
    let ttl_secs = if idle_ttl_millis.is_nan() {
        0.0
    } else {
        (idle_ttl_millis / 1000.0).clamp(0.0, 100.0 * 365.0 * 24.0 * 3600.0)
    };
    JsCompileWorkers {
        inner: CompileWorkers::new(max_workers, Duration::from_secs_f64(ttl_secs)),
    }
}

/// The crate version, so the client can log which compile build it loaded
/// (the wasm and server compiler versions must move in lockstep; issue #20
/// lists version skew as a risk this makes observable).
#[must_use]
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
