//! HTTP/WebSocket server: routing, the health endpoints, and graceful startup.
//!
//! Only the binary needs this; the pure CRDT core links without the `server`
//! feature. The public surface is [`build`] (a [`axum::Router`]) and [`serve`].
//!
//! Routes:
//!
//! * `GET /health` — liveness: always 200, reports version + live room count.
//! * `GET /health/ready` — readiness: 200 only when the wired probes pass
//!   (JWKS freshness, data-dir writability for the fs stores, or storage
//!   reachability for the S3 stores); 503 with the failing reasons otherwise.
//!   Probes that do not apply (no OIDC resolver, no configured storage) are
//!   skipped, so a bare `build` is always ready.
//! * `GET /sync/{doc_id}` — WebSocket upgrade. The document id is validated here
//!   (a 400 on a bad id, before any upgrade) and re-checked against the HELLO
//!   frame inside the session.
//! * `GET /internal/docs/{doc_id}/state` — service-to-service read of a
//!   document's whole current CRDT state as opaque snapshot bytes. Guarded by
//!   [`InternalAuth`] (the shared `NISABA_SYNC_AUTHZ_TOKEN` machine credential,
//!   the same secret the app and sync already exchange). Never proxied by the
//!   web nginx (only `/api/` and `/sync/` are forwarded), so it is reachable
//!   only inside the service network.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower_http::trace::TraceLayer;

use crate::config::{Config, DocId};
use crate::oidc::JwksCache;
use crate::registry::DocRegistry;
use crate::session::{SessionState, run_socket};

/// The default bind address when neither [`resolve_bind_addr`] nor the environment
/// overrides it.
pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";

/// Resolve the bind address from the environment.
///
/// Precedence (matches common deployment conventions):
/// 1. `NISABA_SYNC_ADDR` — a full `host:port` socket address.
/// 2. `PORT` — a bare port number, bound on `0.0.0.0`.
/// 3. [`DEFAULT_BIND_ADDR`] (`0.0.0.0:8080`).
pub fn resolve_bind_addr() -> Result<std::net::SocketAddr, std::net::AddrParseError> {
    resolve_bind_addr_from(|k| std::env::var(k).ok())
}

/// Pure, injectable core of [`resolve_bind_addr`]. `var` looks up an environment
/// variable by name (returning `None` if unset). Exposed for deterministic tests
/// that must not mutate the process-wide environment.
pub fn resolve_bind_addr_from<V>(var: V) -> Result<std::net::SocketAddr, std::net::AddrParseError>
where
    V: Fn(&str) -> Option<String>,
{
    if let Some(addr) = var("NISABA_SYNC_ADDR").filter(|s| !s.trim().is_empty()) {
        return addr.trim().parse();
    }
    if let Some(port) = var("PORT").filter(|s| !s.trim().is_empty()) {
        return format!("0.0.0.0:{}", port.trim()).parse();
    }
    DEFAULT_BIND_ADDR.parse()
}

/// Readiness probes for [`build_with_readiness`]. A `None` field means the
/// probe does not apply (e.g. the deny-all dev resolver has no JWKS cache) and
/// is not checked.
#[derive(Clone, Default)]
pub struct Readiness {
    /// When set, readiness fails while the JWKS cache is stale or empty: token
    /// validation is fail-closed, so every HELLO would be denied until keys
    /// load — a state orchestrators should wait out, not route traffic into.
    pub jwks: Option<Arc<JwksCache>>,
    /// When set, readiness probes that this directory is writable (the
    /// filesystem op-log and snapshot stores live under it; a read-only
    /// volume fails every durable join). Not wired when the S3 stores are
    /// selected — see [`Self::storage`].
    pub data_dir: Option<std::path::PathBuf>,
    /// When set, readiness probes the configured durable store backend (the
    /// S3 stores answer with a `HeadBucket` against the op-log bucket). Wired
    /// instead of [`Self::data_dir`] when `NISABA_SYNC_STORE_BACKEND=s3`, so
    /// orchestration does not route traffic to a sync that cannot persist.
    pub storage: Option<Arc<dyn StorageProbe>>,
}

/// An async reachability probe for the durable storage backend, wired into
/// [`Readiness::storage`]. Implemented by the S3 stores (feature `s3`); kept
/// as a trait so the server depends on no SDK and any future backend (or a
/// test double) can be probed the same way.
#[async_trait::async_trait]
pub trait StorageProbe: Send + Sync {
    /// `Ok(())` when the backend accepts reads/writes; `Err(reason)` names the
    /// failure for the readiness body.
    async fn probe(&self) -> Result<(), String>;
}

/// Build the application router.
///
/// Exposed so tests can drive the router directly or bind it to an ephemeral
/// listener. Readiness has no probes wired (always ready) and the internal
/// read API is **deny-all** (see [`InternalAuth`]); the binary uses
/// [`build_with_readiness`].
pub fn build(registry: DocRegistry, config: Arc<Config>) -> Router {
    build_with_readiness(
        registry,
        config,
        Readiness::default(),
        InternalAuth::default(),
    )
}

/// Like [`build`], with real readiness probes (see [`Readiness`]) and the
/// credential for the `/internal/*` read API (see [`InternalAuth`]).
pub fn build_with_readiness(
    registry: DocRegistry,
    config: Arc<Config>,
    // Named `probes` so it cannot shadow the `readiness` handler below at the
    // route registration site.
    probes: Readiness,
    internal_auth: InternalAuth,
) -> Router {
    let state = SessionState {
        registry,
        config,
        readiness: probes,
        internal_auth,
    };
    Router::new()
        .route("/health", get(health))
        // `/healthz` is the conventional k8s liveness path; alias of `/health`.
        .route("/healthz", get(health))
        .route("/health/ready", get(readiness))
        .route("/sync/{doc_id}", get(sync_handler))
        .route("/internal/docs/{doc_id}/state", get(internal_doc_state))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Serve the application on `addr` until shutdown.
///
/// Presence sweeping and periodic snapshotting run in a separate task spawned by the
/// binary (`spawn_maintenance`), not here.
pub async fn serve(router: Router, addr: SocketAddr) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "nisaba-sync listening");
    axum::serve(listener, router).await
}

async fn health(State(st): State<SessionState>) -> Json<serde_json::Value> {
    let rooms = st.registry.len();
    Json(json!({
        "status": "ok",
        "service": "nisaba-sync",
        "version": crate::VERSION,
        "protocol": crate::PROTOCOL_VERSION,
        "rooms": rooms,
    }))
}

/// Readiness: report every failing dependency and return 503 until all wired
/// probes pass (mirrors the app service's `/health/ready`, which returns a
/// non-200 with a reason while its database is unreachable). The per-document
/// authz endpoint is deliberately NOT probed here — a network call per
/// readiness check is not cheap, and a blip there degrades individual joins
/// without making the service unusable.
async fn readiness(State(st): State<SessionState>) -> Response {
    let mut reasons: Vec<String> = Vec::new();
    if let Some(jwks) = &st.readiness.jwks
        && jwks.is_stale()
    {
        reasons.push("jwks cache is empty or stale; token validation is fail-closed".into());
    }
    if let Some(dir) = &st.readiness.data_dir {
        // A create+write+remove of one probe file (orchestrators poll this
        // endpoint every few seconds; the synchronous fs calls are µs-scale).
        if let Err(error) = probe_writable(dir) {
            reasons.push(format!(
                "data dir {} is not writable: {error}",
                dir.display()
            ));
        }
    }
    if let Some(storage) = &st.readiness.storage {
        // One round-trip to the durable backend (e.g. HeadBucket): a sync that
        // cannot reach its op-log bucket must not be routed into, since every
        // accepted update would fail to persist.
        if let Err(reason) = storage.probe().await {
            reasons.push(format!("storage backend unreachable: {reason}"));
        }
    }
    let ready = reasons.is_empty();
    let body = json!({
        "status": if ready { "ready" } else { "unavailable" },
        "rooms": st.registry.len(),
        "reasons": reasons,
    });
    if ready {
        (StatusCode::OK, Json(body)).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

/// Verify `dir` exists and accepts writes by touching (and removing) a probe
/// file inside it.
fn probe_writable(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".readiness-probe");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

/// The shared service credential guarding the `/internal/*` read API.
///
/// It is the SAME machine credential the sync service presents when it calls
/// the app (`NISABA_SYNC_AUTHZ_TOKEN`): one shared secret authorises both
/// directions of the app↔sync hop. The token is stored as a SHA-256 digest and
/// presented tokens are hashed and compared in constant time — exactly how the
/// app checks it on `/internal/sync/authorize` and
/// `/internal/document/{id}/body`.
///
/// **Fail-closed:** the default (no token configured) denies every internal
/// request.
#[derive(Clone, Default)]
pub struct InternalAuth {
    token_hash: Option<[u8; 32]>,
}

impl InternalAuth {
    /// Configure the accepted credential from its plaintext. An empty/blank
    /// token yields the deny-all state.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        Self {
            token_hash: (!token.trim().is_empty()).then(|| Sha256::digest(token.as_bytes()).into()),
        }
    }

    /// Check a request's `Authorization: Bearer …` header against the
    /// configured credential. `Ok(())` allows the read; an `Err` says which
    /// denial to send (401 missing bearer, 403 wrong credential or none
    /// configured — all fail-closed).
    fn authorize(&self, headers: &HeaderMap) -> Result<(), Denial> {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .filter(|token| !token.is_empty());
        let Some(expected) = self.token_hash.as_ref() else {
            // No credential configured: fail closed rather than open the read.
            return Err(Denial::Forbidden);
        };
        let Some(presented) = presented else {
            return Err(Denial::Unauthorized);
        };
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        if bool::from(expected.as_slice().ct_eq(digest.as_slice())) {
            Ok(())
        } else {
            Err(Denial::Forbidden)
        }
    }
}

/// Why an internal read was denied (see [`InternalAuth::authorize`]).
enum Denial {
    /// No `Authorization: Bearer …` header on the request.
    Unauthorized,
    /// Wrong credential, or no credential configured at all (fail-closed).
    Forbidden,
}

impl Denial {
    /// The ready-to-send denial response.
    fn response(self) -> Response {
        let status = match self {
            Denial::Unauthorized => StatusCode::UNAUTHORIZED,
            Denial::Forbidden => StatusCode::FORBIDDEN,
        };
        (
            status,
            Json(json!({
                "error": "forbidden",
                "detail": "internal service endpoints require the shared service token"
            })),
        )
            .into_response()
    }
}

/// Internal (service-token) read: a document's whole current CRDT state as
/// opaque Loro snapshot bytes.
///
/// The bytes are served **uninterpreted** — the same whole-state export a
/// joining peer would receive, on an authenticated internal path instead of
/// the public relay. Answers:
///
/// * `200` `application/octet-stream` — the snapshot bytes;
/// * `204` — the document has no state anywhere (never seeded). Deliberately
///   NOT `404`: an unmatched route (version skew against an older sync, a
///   misconfigured base URL in the app) also answers 404, and the caller must
///   be able to tell "genuinely no state — empty marks" apart from "wrong
///   door — fail loudly";
/// * `400` — invalid document id (same validation as the WS path);
/// * `401` / `403` — missing / wrong service token (see [`InternalAuth`]);
/// * `500` — a store or export failure.
async fn internal_doc_state(
    State(st): State<SessionState>,
    Path(doc_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(denial) = st.internal_auth.authorize(&headers) {
        return denial.response();
    }
    let doc = match DocId::new(doc_id) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_document_id", "detail": e.to_string() })),
            )
                .into_response();
        }
    };
    match st.registry.export_state(&doc).await {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/octet-stream"),
            )],
            bytes,
        )
            .into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal", "detail": e.to_string() })),
        )
            .into_response(),
    }
}

/// WebSocket upgrade handler. A bad document id is rejected with HTTP 400 before
/// any upgrade, so an invalid path can never open a socket.
async fn sync_handler(
    ws: axum::extract::WebSocketUpgrade,
    State(st): State<SessionState>,
    Path(doc_id): Path<String>,
) -> Response {
    let doc = match DocId::new(doc_id) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid_document_id", "detail": e.to_string() })),
            )
                .into_response();
        }
    };
    ws.on_upgrade(move |socket| async move {
        run_socket(socket, st, doc).await;
    })
}

/// A maintenance interval used by the periodic sweep/snapshot tasks.
#[must_use]
pub fn maintenance_interval(config: &Config) -> Duration {
    Duration::from_millis(config.presence_sweep_ms.max(1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |k| map.get(k).map(|v| (*v).to_string())
    }

    #[test]
    fn defaults_to_8080_when_nothing_set() {
        let addr = resolve_bind_addr_from(env_of(&[])).unwrap();
        assert_eq!(addr.to_string(), DEFAULT_BIND_ADDR);
    }

    #[test]
    fn nisaba_sync_addr_takes_precedence_over_port() {
        let addr = resolve_bind_addr_from(env_of(&[
            ("NISABA_SYNC_ADDR", "127.0.0.1:9090"),
            ("PORT", "7777"),
        ]))
        .unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:9090");
    }

    #[test]
    fn port_binds_all_interfaces_when_addr_absent() {
        let addr = resolve_bind_addr_from(env_of(&[("PORT", "5000")])).unwrap();
        assert_eq!(addr.ip().to_string(), "0.0.0.0");
        assert_eq!(addr.port(), 5000);
    }

    #[test]
    fn empty_strings_fall_through() {
        // Empty values must not be treated as set; fall through to PORT then default.
        let addr = resolve_bind_addr_from(env_of(&[("NISABA_SYNC_ADDR", "  "), ("PORT", "8081")]))
            .unwrap();
        assert_eq!(addr.port(), 8081);
    }

    // ---- internal state read API ---------------------------------------------

    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    use crate::auth::{Role, StaticAccessResolver};
    use crate::op_log::{MemoryOpLogStore, OpLogStore};
    use crate::snapshot::{MemorySnapshotStore, SnapshotStore};
    use crate::time::SystemClock;

    fn memory_registry() -> DocRegistry {
        DocRegistry::new(
            Arc::new(MemoryOpLogStore::default()),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(StaticAccessResolver::new()),
        )
    }

    /// Seed a document with text through the registry's own room (the same
    /// ingest path production uses), so the read API serves real state.
    async fn seed(registry: &DocRegistry, doc: &str, text: &str) {
        let doc = DocId::new(doc).unwrap();
        let room = registry.get_or_open(&doc).await.unwrap();
        let peer = loro::LoroDoc::new();
        peer.set_peer_id(7).unwrap();
        peer.get_text("text").insert(0, text).unwrap();
        peer.commit();
        let bytes = peer.export(loro::ExportMode::Snapshot).unwrap();
        room.handle_update(crate::config::PeerId(7), Role::Author, &bytes)
            .await
            .unwrap();
    }

    async fn get(router: &Router, path: &str, token: Option<&str>) -> axum::response::Response {
        let mut builder = Request::get(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn internal_state_requires_the_service_token() {
        // A registry seeded with one document, served with a configured token.
        let registry = memory_registry();
        seed(&registry, "doc_with_state", "hello").await;
        let router = build_with_readiness(
            registry,
            Arc::new(Config::default()),
            Readiness::default(),
            InternalAuth::from_token("machine-secret"),
        );

        // No credential → 401.
        let response = get(&router, "/internal/docs/doc_with_state/state", None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // Wrong credential → 403.
        let response = get(
            &router,
            "/internal/docs/doc_with_state/state",
            Some("wrong"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        // Correct credential → 200 with importable snapshot bytes.
        let response = get(
            &router,
            "/internal/docs/doc_with_state/state",
            Some("machine-secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream")
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let doc = loro::LoroDoc::new();
        doc.import(&bytes).unwrap();
        assert_eq!(doc.get_text("text").to_string(), "hello");

        // Unknown document (no state anywhere) → 204, distinct from a routing
        // miss's 404 so the caller can tell "no state" from "wrong door".
        let response = get(
            &router,
            "/internal/docs/never_seeded/state",
            Some("machine-secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Invalid document id → 400 (same validation as the WS path).
        let response = get(
            &router,
            "/internal/docs/.hidden/state",
            Some("machine-secret"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn internal_state_is_deny_all_without_a_configured_token() {
        let router = build_with_readiness(
            memory_registry(),
            Arc::new(Config::default()),
            Readiness::default(),
            InternalAuth::from_token(""),
        );
        let response = get(&router, "/internal/docs/anything/state", Some("whatever")).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    // ---- readiness: the storage probe ---------------------------------------

    /// Test double for [`StorageProbe`]: ready or not, with a fixed reason.
    struct FixedProbe(Result<(), &'static str>);

    #[async_trait::async_trait]
    impl StorageProbe for FixedProbe {
        async fn probe(&self) -> Result<(), String> {
            self.0.map_err(str::to_string)
        }
    }

    async fn readiness_status(readiness: Readiness) -> StatusCode {
        let router = build_with_readiness(
            memory_registry(),
            Arc::new(Config::default()),
            readiness,
            InternalAuth::default(),
        );
        get(&router, "/health/ready", None).await.status()
    }

    #[tokio::test]
    async fn readiness_fails_while_the_storage_backend_is_unreachable() {
        let failing = Readiness {
            storage: Some(Arc::new(FixedProbe(Err("HeadBucket nisaba-oplog failed")))),
            ..Readiness::default()
        };
        let response = get(
            &build_with_readiness(
                memory_registry(),
                Arc::new(Config::default()),
                failing,
                InternalAuth::default(),
            ),
            "/health/ready",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "unavailable");
        assert_eq!(
            json["reasons"][0],
            "storage backend unreachable: HeadBucket nisaba-oplog failed"
        );
    }

    #[tokio::test]
    async fn readiness_passes_with_a_healthy_storage_backend() {
        let healthy = Readiness {
            storage: Some(Arc::new(FixedProbe(Ok(())))),
            ..Readiness::default()
        };
        assert_eq!(readiness_status(healthy).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn readiness_with_no_storage_probe_stays_ready() {
        // The fs dev path wires only the data dir; no storage probe must mean
        // "not checked", never a permanent 503.
        assert_eq!(readiness_status(Readiness::default()).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn internal_state_serves_persisted_state_without_a_live_room() {
        // State that only exists in the stores (no live room anywhere, e.g.
        // after a restart) must still read correctly: the endpoint hydrates
        // from the latest snapshot + op-log replay without registering a room.
        let op_log: Arc<MemoryOpLogStore> = Arc::new(MemoryOpLogStore::default());
        let snapshots: Arc<MemorySnapshotStore> = Arc::new(MemorySnapshotStore::default());
        let first = DocRegistry::new(
            Arc::clone(&op_log) as Arc<dyn OpLogStore>,
            Arc::clone(&snapshots) as Arc<dyn SnapshotStore>,
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(StaticAccessResolver::new()),
        );
        seed(&first, "persisted", "body text").await;
        // Persist the state, then drop every live room (the registry with it).
        let room = first
            .get_or_open(&DocId::new("persisted").unwrap())
            .await
            .unwrap();
        room.snapshot_now().await.unwrap();
        drop(room);
        drop(first);
        assert_eq!(
            persisted_counts(&op_log, &snapshots).await,
            (1, 1),
            "the op log and snapshot store each hold the seeded state"
        );

        // A fresh registry over the same stores has no live rooms.
        let second = DocRegistry::new(
            Arc::clone(&op_log) as Arc<dyn OpLogStore>,
            Arc::clone(&snapshots) as Arc<dyn SnapshotStore>,
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(StaticAccessResolver::new()),
        );
        assert!(second.is_empty());
        let bytes = second
            .export_state(&DocId::new("persisted").unwrap())
            .await
            .unwrap()
            .expect("persisted state must read back");
        let doc = loro::LoroDoc::new();
        doc.import(&bytes).unwrap();
        assert_eq!(doc.get_text("text").to_string(), "body text");
        // And the read did not register a room.
        assert!(second.is_empty());
    }

    /// (op-log records, snapshots) held for `persisted` — proves the fixtures
    /// above actually persisted something before the registries were dropped.
    async fn persisted_counts(
        op_log: &MemoryOpLogStore,
        snapshots: &MemorySnapshotStore,
    ) -> (u64, usize) {
        let doc = DocId::new("persisted").unwrap();
        let records = op_log.read_all(&doc).await.unwrap().len() as u64;
        let snaps = snapshots.list(&doc).await.unwrap().len();
        (records, snaps)
    }
}
