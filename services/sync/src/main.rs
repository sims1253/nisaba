//! `nisaba-sync` binary entry point.
//!
//! Boots the sync service: filesystem op-log + snapshot stores under a data
//! directory, a [`nisaba_sync::DocRegistry`], periodic presence-sweep and
//! snapshot maintenance tasks, and the axum HTTP/WebSocket server.
//!
//! Configuration is by environment variable + a data dir. Sync does **not**
//! build identity/login; it *does* validate the bearer tokens `app`
//! mints: in production it resolves each token through
//! [`nisaba_sync::OidcAccessResolver`] (JWT/JWKS) and asks `app` to authorize
//! the subject for the specific document ([`nisaba_sync::HttpDocumentAuthorizer`]).
//!
//! ## Security defaults
//!
//! By default **no token is accepted** — every HELLO is denied. This is safe by
//! default.
//!
//! * Local dev sets `NISABA_SYNC_DEV_ALLOW_ALL=1` to grant `author` to any
//!   non-empty token. Never use in production.
//! * Production sets the OIDC env vars (see [`build_access_resolver`]); sync
//!   validates the JWT against JWKS and **authorizes each document separately**
//!   — a valid global token alone never opens an arbitrary document.
//! * With nothing configured, [`nisaba_sync::StaticAccessResolver`] denies every
//!   token (fail-closed). Partial OIDC configuration is a **fatal** startup
//!   error rather than a silent insecure default.

use std::collections::HashSet;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::Algorithm;
use nisaba_sync::http::{HttpFetch, ReqwestHttpFetch};
use nisaba_sync::{
    AccessResolver, Clock, Config, DenyAllAuthorizer, DocRegistry, DocumentAuthorizer,
    FsOpLogStore, FsSnapshotStore, HttpDocumentAuthorizer, JwksCache, JwtConfig, JwtValidator,
    OidcAccessResolver, Role, StaticAccessResolver, SystemClock, TokenCache, run_jwks_refresher,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let data_dir =
        PathBuf::from(env::var("NISABA_SYNC_DATA_DIR").unwrap_or_else(|_| "data".into()));
    let op_log = Arc::new(FsOpLogStore::new(data_dir.join("oplog"))?);
    let snapshots = Arc::new(FsSnapshotStore::new(data_dir.join("snapshots"))?);
    let config = Arc::new(Config::default());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    let access = build_access_resolver(clock.clone())?;

    let registry = DocRegistry::new(op_log, snapshots, config.clone(), clock, access);
    let router = nisaba_sync::server::build(registry.clone(), config.clone());

    let interval = nisaba_sync::server::maintenance_interval(&config);
    spawn_maintenance(registry.clone(), interval);
    let addr: SocketAddr = nisaba_sync::server::resolve_bind_addr()?;
    tracing::info!(%addr, "nisaba-sync bind address resolved");
    nisaba_sync::server::serve(router, addr).await?;
    Ok(())
}

/// Build the access policy from environment variables.
///
/// Resolution order — every path fails **closed** (no token is implicitly
/// trusted):
///
/// 1. `NISABA_SYNC_DEV_ALLOW_ALL` set → every non-empty token is granted
///    `Author`. Dev escape hatch; logs a loud warning. Never use in production.
/// 2. OIDC mode: all of `NISABA_SYNC_OIDC_ISSUER`,
///    `NISABA_SYNC_OIDC_AUDIENCE`, `NISABA_SYNC_OIDC_JWKS_URL` set →
///    [`OidcAccessResolver`] validates the JWT against JWKS and authorizes each
///    document via [`HttpDocumentAuthorizer`] (calling the app authorization
///    endpoint). If `NISABA_SYNC_AUTHZ_URL` is unset, [`DenyAllAuthorizer`]
///    denies every document (the JWT is still validated, but no document is
///    reachable) — fail-closed. A background task refreshes the JWKS.
/// 3. Otherwise → [`StaticAccessResolver::new`] denies every token.
///
/// Partial OIDC configuration (some-but-not-all of the three required vars) is
/// a **fatal** error: the operator intended OIDC but misconfigured it, and a
/// silent fall-through to deny-all would hide the mistake.
///
/// `clock` is shared with the [`DocRegistry`] (presence TTL); the JWKS
/// freshness bookkeeping reuses it.
fn build_access_resolver(
    clock: Arc<dyn Clock>,
) -> Result<Arc<dyn AccessResolver>, Box<dyn std::error::Error>> {
    if env::var("NISABA_SYNC_DEV_ALLOW_ALL").is_ok() {
        tracing::warn!(
            "NISABA_SYNC_DEV_ALLOW_ALL is set: every non-empty token is granted author role. \
             Never use in production."
        );
        return Ok(Arc::new(StaticAccessResolver::allow_all(Role::Author)));
    }

    let issuer = env::var("NISABA_SYNC_OIDC_ISSUER").ok();
    let audience = env::var("NISABA_SYNC_OIDC_AUDIENCE").ok();
    let jwks_url = env::var("NISABA_SYNC_OIDC_JWKS_URL").ok();
    let present = [&issuer, &audience, &jwks_url]
        .iter()
        .filter(|o| o.is_some())
        .count();
    if present != 0 && present != 3 {
        return Err(
            "partial OIDC configuration: set ALL of NISABA_SYNC_OIDC_ISSUER, \
             NISABA_SYNC_OIDC_AUDIENCE, and NISABA_SYNC_OIDC_JWKS_URL (or none, for deny-all)"
                .into(),
        );
    }

    let Some((issuer, audience, jwks_url)) = (match (issuer, audience, jwks_url) {
        (Some(i), Some(a), Some(u)) => Some((i, a, u)),
        _ => None,
    }) else {
        tracing::warn!(
            "no access resolver configured (NISABA_SYNC_DEV_ALLOW_ALL unset and no OIDC vars): \
             every token will be denied"
        );
        return Ok(Arc::new(StaticAccessResolver::new()));
    };

    // Keycloak maps roles under realm_access.roles (see deploy/keycloak realm +
    // docs/architecture.md §6); that is the safe default. Overridable for a
    // provider that emits a flat roles claim.
    let roles_claim =
        env::var("NISABA_SYNC_OIDC_ROLES_CLAIM").unwrap_or_else(|_| "realm_access.roles".into());
    let allowed_algorithms = match env::var("NISABA_SYNC_OIDC_ALGORITHMS") {
        Ok(spec) => parse_algorithms(&spec)?,
        Err(_) => JwtConfig::default_algorithms(),
    };
    let validator = JwtValidator::new(JwtConfig {
        issuer,
        audience,
        allowed_algorithms,
        roles_claim,
        leeway_secs: env_secs("NISABA_SYNC_OIDC_LEEWAY_SECS", 60)?,
    })?;

    // JWKS: empty until the background refresher succeeds — every key lookup is
    // denied in the meantime (fail-closed during startup / IdP outage).
    let jwks_max_age = Duration::from_secs(env_secs("NISABA_SYNC_OIDC_JWKS_MAX_AGE_SECS", 3600)?);
    let jwks = Arc::new(JwksCache::empty(jwks_max_age, clock));

    let http = build_http_transport()?;

    // Per-document authorization. No URL wired → deny every document (the JWT is
    // still validated; the document gate simply never opens). Production MUST set
    // NISABA_SYNC_AUTHZ_URL or collaboration is impossible — the warning makes
    // that visible instead of failing open.
    let documents: Arc<dyn DocumentAuthorizer> = if let Ok(url) = env::var("NISABA_SYNC_AUTHZ_URL")
    {
        let Ok(token) = env::var("NISABA_SYNC_AUTHZ_TOKEN") else {
            return Err(
                "NISABA_SYNC_AUTHZ_URL is set but NISABA_SYNC_AUTHZ_TOKEN is missing".into(),
            );
        };
        let timeout = Duration::from_secs(env_secs("NISABA_SYNC_AUTHZ_TIMEOUT_SECS", 5)?);
        Arc::new(HttpDocumentAuthorizer::new(
            Arc::clone(&http),
            url,
            token,
            timeout,
        ))
    } else {
        tracing::warn!(
            "no document authorizer configured (NISABA_SYNC_AUTHZ_URL unset): every \
             document will be denied. Set it in production so validated tokens are still \
             checked per-document."
        );
        Arc::new(DenyAllAuthorizer)
    };

    let tokens = TokenCache::new(env_secs("NISABA_SYNC_OIDC_TOKEN_CACHE_TTL_SECS", 60)?, 4096);
    let resolver = Arc::new(OidcAccessResolver::new(
        validator,
        Arc::clone(&jwks),
        documents,
        tokens,
    ));

    // Background JWKS refresh: fetch once on entry, then on a schedule. Failures
    // are logged and the previous keys are retained; the cache's max-age guard
    // eventually fails closed if the outage is prolonged.
    let refresh_interval =
        Duration::from_secs(env_secs("NISABA_SYNC_OIDC_JWKS_REFRESH_SECS", 900)?);
    let jwks_refresher = Arc::clone(&jwks);
    let http_refresher = http;
    tokio::spawn(async move {
        run_jwks_refresher(jwks_refresher, http_refresher, jwks_url, refresh_interval).await;
    });

    tracing::info!("OIDC access resolver configured (JWT/JWKS + document authorizer)");
    Ok(resolver)
}

/// Build the shared outbound HTTP transport (rustls; never openssl — see
/// `deny.toml`). `https_only` unless `NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME`
/// is set, for local dev against an `http://` Keycloak or app authz endpoint.
fn build_http_transport() -> Result<Arc<dyn HttpFetch>, Box<dyn std::error::Error>> {
    let connect = Duration::from_secs(env_secs("NISABA_SYNC_HTTP_CONNECT_TIMEOUT_SECS", 5)?);
    let request = Duration::from_secs(env_secs("NISABA_SYNC_HTTP_REQUEST_TIMEOUT_SECS", 10)?);
    let client = if env::var_os("NISABA_SYNC_HTTP_ALLOW_INSECURE_SCHEME").is_some() {
        ReqwestHttpFetch::new_insecure_scheme(connect, request)
    } else {
        ReqwestHttpFetch::new(connect, request)?
    };
    Ok(Arc::new(client))
}

/// Parse a comma-separated list of JWT algorithms (e.g. `RS256,ES256`) into the
/// allow-set. Empty / blank entries are skipped; an all-empty spec is an error
/// (an empty allow-list would deny every token, but loudly rejecting the
/// misconfiguration is clearer).
fn parse_algorithms(spec: &str) -> Result<HashSet<Algorithm>, Box<dyn std::error::Error>> {
    let mut out = HashSet::new();
    for raw in spec.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let alg = Algorithm::from_str(raw).map_err(|_| {
            format!("unknown OIDC algorithm {raw:?} in NISABA_SYNC_OIDC_ALGORITHMS")
        })?;
        out.insert(alg);
    }
    if out.is_empty() {
        return Err("NISABA_SYNC_OIDC_ALGORITHMS resolved to no algorithms".into());
    }
    Ok(out)
}

/// Read a duration-in-seconds env var, falling back to `default`. A
/// non-numeric value is a fatal configuration error.
fn env_secs(var: &str, default: u64) -> Result<u64, Box<dyn std::error::Error>> {
    match env::var(var) {
        Ok(s) => Ok(s.parse()?),
        Err(_) => Ok(default),
    }
}

/// Periodic presence sweep + opportunistic snapshotting.
///
/// Runs until the runtime shuts down. Snapshot cadence is primarily event-driven
/// (every N updates — see [`Config::snapshot_every_updates`]); this task adds a
/// time-based floor so an idle-but-dirty document is still snapshotted, and is
/// where the presence TTL sweeper lives.
fn spawn_maintenance(registry: DocRegistry, interval: std::time::Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            for room in registry.rooms() {
                let evicted = room.sweep_presence();
                if !evicted.is_empty() {
                    tracing::info!(
                        doc = %room.doc_id(),
                        count = evicted.len(),
                        "evicted expired presence"
                    );
                }
                // The time-based snapshot floor: without this, a document that took a
                // few updates and went idle keeps replaying its whole op log on restart.
                match room.snapshot_if_dirty().await {
                    Ok(true) => tracing::debug!(doc = %room.doc_id(), "periodic snapshot written"),
                    Ok(false) => {}
                    // Best-effort: the op log still holds every update, so a failed
                    // snapshot costs replay time, never data.
                    Err(error) => {
                        tracing::warn!(error = %error, doc = %room.doc_id(), "periodic snapshot failed");
                    }
                }
            }
            // Reclaim empty rooms idle past the TTL, releasing their op-log file
            // handles. This caps long-term memory/descriptor growth from rooms
            // that joined and then went quiet.
            let evicted = registry.evict_idle_rooms().await;
            if evicted > 0 {
                tracing::info!(evicted, "evicted idle document rooms");
            }
        }
    });
}
