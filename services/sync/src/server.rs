//! HTTP/WebSocket server: routing, the health endpoints, and graceful startup.
//!
//! Only the binary needs this; the pure CRDT core links without the `server`
//! feature. The public surface is [`build`] (a [`axum::Router`]) and [`serve`].
//!
//! Routes:
//!
//! * `GET /health` — liveness: always 200, reports version + live room count.
//! * `GET /health/ready` — readiness: 200 only when the wired probes pass
//!   (JWKS freshness, data-dir writability); 503 with the failing reasons
//!   otherwise. Probes that do not apply (no OIDC resolver, no data dir) are
//!   skipped, so a bare `build` is always ready.
//! * `GET /sync/{doc_id}` — WebSocket upgrade. The document id is validated here
//!   (a 400 on a bad id, before any upgrade) and re-checked against the HELLO
//!   frame inside the session.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
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
    /// When set, readiness probes that this directory is writable (the op-log
    /// and snapshot stores live under it; a read-only volume fails every
    /// durable join).
    pub data_dir: Option<std::path::PathBuf>,
}

/// Build the application router.
///
/// Exposed so tests can drive the router directly or bind it to an ephemeral
/// listener. Readiness has no probes wired (always ready); the binary uses
/// [`build_with_readiness`].
pub fn build(registry: DocRegistry, config: Arc<Config>) -> Router {
    build_with_readiness(registry, config, Readiness::default())
}

/// Like [`build`], with real readiness probes (see [`Readiness`]).
pub fn build_with_readiness(
    registry: DocRegistry,
    config: Arc<Config>,
    // Named `probes` so it cannot shadow the `readiness` handler below at the
    // route registration site.
    probes: Readiness,
) -> Router {
    let state = SessionState {
        registry,
        config,
        readiness: probes,
    };
    Router::new()
        .route("/health", get(health))
        // `/healthz` is the conventional k8s liveness path; alias of `/health`.
        .route("/healthz", get(health))
        .route("/health/ready", get(readiness))
        .route("/sync/{doc_id}", get(sync_handler))
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
}
