//! Production OIDC/JWT access resolution.
//!
//! [`StaticAccessResolver`](crate::StaticAccessResolver) is the dev/test seam.
//! This component is the production resolver: it validates an OIDC access token
//! (JWT) and, **separately**, authorizes the bearer for a specific document.
//! The two are deliberately decoupled so that **a valid global token alone never
//! grants access to an arbitrary document**.
//!
//! # Pipeline
//!
//! `OidcAccessResolver::resolve(doc, token)`:
//! 1. **Token cache** hit? Skip re-decoding (still runs step 3).
//! 2. **JWT validation** ([`JwtValidator`]): header → `kid` lookup in the JWKS
//!    cache → algorithm allow-list (never `none`) → algorithm must equal the
//!    JWK's configured algorithm (defeats RS/HMAC confusion) → signature,
//!    `iss`, `aud`, `exp` verified via `jsonwebtoken`. Only the explicit
//!    `roles` claim is read; **scopes are never interpreted as roles**.
//! 3. **Document authorization** ([`DocumentAuthorizer`]): the per-document
//!    gate. [`DenyAllAuthorizer`] (default, fail-closed) or
//!    [`HttpDocumentAuthorizer`] (calls the app authorization endpoint).
//!
//! # Fail-closed everywhere
//!
//! * No/empty/stale JWKS → deny (never an empty allow).
//! * Any signature / claim / transport error → deny.
//! * No document authorizer wired → [`DenyAllAuthorizer`] denies every doc.
//! * JWKS refresh failure keeps the last good keys (rotation overlap) but a
//!   `max_age` guard eventually denies once they go stale.
//!
//! # Caching
//!
//! [`JwksCache`] caches signing keys with a periodic background refresh (see
//! [`run_jwks_refresher`]). [`TokenCache`] caches the verified [`Identity`] for
//! a token until `min(cache_ttl, token exp)` to avoid re-verifying the signature
//! on every request; document authorization still runs on every request.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::auth::{AccessResolver, AuthError, Identity, Role};
use crate::config::DocId;
use crate::http::{HttpFetch, HttpFetchError, HttpMethod, HttpRequest};
use crate::time::Clock;

/// Wall-clock seconds since the Unix epoch. Used only for the verified-token
/// cache TTL and JWKS freshness bookkeeping; JWT `exp` itself is checked by
/// `jsonwebtoken` against the real clock.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// JWT validation
// ---------------------------------------------------------------------------

/// Configuration for JWT validation. All fields are required and validated by
/// [`JwtValidator::new`]; production sets them from environment variables (see
/// `main.rs`).
#[derive(Debug, Clone)]
pub struct JwtConfig {
    /// Expected `iss` claim. The token's issuer must match exactly.
    pub issuer: String,
    /// Expected `aud` claim. The token's audience must contain this value.
    pub audience: String,
    /// Algorithms the resolver will accept. Must be non-empty. The token's `alg`
    /// header must (a) be in this set and (b) equal the matched JWK's configured
    /// algorithm. Tokens with `alg: none` are rejected: `none` is never a
    /// permitted value (and `jsonwebtoken` cannot parse such a header).
    pub allowed_algorithms: HashSet<Algorithm>,
    /// Dotted JSON path of the roles claim (e.g. `"roles"` or, for Keycloak,
    /// `"realm_access.roles"`). Only this claim is read; scopes are ignored.
    pub roles_claim: String,
    /// Leeway (seconds) applied to `exp`/`nbf` checks.
    pub leeway_secs: u64,
}

impl JwtConfig {
    /// A reasonable production default: RSASSA-PKCS1-v1_5 + ECDSA, SHA-256.
    /// HMAC (`HS*`) is intentionally excluded — symmetric keys in a public JWKS
    /// are the classic algorithm-confusion vector.
    #[must_use]
    pub fn default_algorithms() -> HashSet<Algorithm> {
        [Algorithm::RS256, Algorithm::ES256].into_iter().collect()
    }
}

/// Validates OIDC/JWT access tokens. Stateless apart from config; the JWKS
/// (key material) live in a shared [`JwksCache`].
#[derive(Debug, Clone)]
pub struct JwtValidator {
    issuer: String,
    audience: String,
    allowed_algorithms: HashSet<Algorithm>,
    roles_claim: Vec<String>,
    leeway_secs: u64,
}

impl JwtValidator {
    /// Construct from validated config.
    ///
    /// # Errors
    /// Returns a descriptive static message if the configuration is unsafe
    /// (empty issuer/audience/roles claim, empty allow-list, or `none` allowed).
    pub fn new(config: JwtConfig) -> Result<Self, &'static str> {
        if config.issuer.trim().is_empty() {
            return Err("OIDC issuer must be set");
        }
        if config.audience.trim().is_empty() {
            return Err("OIDC audience must be set");
        }
        if config.roles_claim.trim().is_empty() {
            return Err("OIDC roles_claim path must be set");
        }
        if config.allowed_algorithms.is_empty() {
            return Err("OIDC allowed_algorithms must be non-empty");
        }
        let roles_claim = config
            .roles_claim
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Ok(Self {
            issuer: config.issuer,
            audience: config.audience,
            allowed_algorithms: config.allowed_algorithms,
            roles_claim,
            leeway_secs: config.leeway_secs,
        })
    }

    /// Validate `token` and return the verified [`Identity`] plus its `exp`
    /// (unix seconds, for capping the token-cache TTL). Reads signing keys from
    /// `jwks`. **Fail-closed**: any error → [`AuthError::Unauthenticated`].
    fn validate(&self, token: &str, jwks: &JwksCache) -> Result<(Identity, usize), AuthError> {
        // 1. Header: alg allow-list + kid required. Never trust the header's alg
        //    without also confirming it matches the *key's* alg (step 4) — this
        //    is the defense against RS↔HMAC algorithm-confusion attacks.
        let header = decode_header(token)
            .map_err(|_| AuthError::Unauthenticated("malformed JWT header".into()))?;
        if !self.allowed_algorithms.contains(&header.alg) {
            return Err(AuthError::Unauthenticated(format!(
                "JWT algorithm {:?} is not allowed",
                header.alg
            )));
        }
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| AuthError::Unauthenticated("JWT 'kid' header is required".into()))?;

        // 2. Key lookup. An empty/stale JWKS denies (no fail-open to "try all keys").
        let jwk = jwks
            .key_by_kid(kid)
            .ok_or_else(|| AuthError::Unauthenticated(format!("no JWKS key for kid {kid:?}")))?;

        // 3. The header alg must equal the JWK's configured algorithm. Prevents
        //    e.g. an attacker forging an RS256 header onto an HMAC signature.
        let key_alg = jwk
            .common
            .key_algorithm
            .as_ref()
            .map(ToString::to_string)
            .and_then(|s| Algorithm::from_str(&s).ok())
            .ok_or_else(|| AuthError::Unauthenticated("JWKS key has no algorithm".into()))?;
        if key_alg != header.alg {
            return Err(AuthError::Unauthenticated(
                "JWT algorithm does not match its JWKS key".into(),
            ));
        }

        // 4. Signature + iss + aud + exp. `jsonwebtoken` does the crypto.
        let key = DecodingKey::from_jwk(&jwk)
            .map_err(|_| AuthError::Unauthenticated("JWKS key is not usable".into()))?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(std::slice::from_ref(&self.issuer));
        validation.set_audience(std::slice::from_ref(&self.audience));
        validation.leeway = self.leeway_secs;
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Unauthenticated(jwt_error(&e)))?;

        // 5. Read the explicit roles claim only.
        let roles = extract_roles(&data.claims.extra, &self.roles_claim);
        Ok((
            Identity {
                subject: data.claims.sub,
                roles,
            },
            data.claims.exp,
        ))
    }
}

/// Map a `jsonwebtoken` error to a short, non-leaky denial message. We never
/// surface the raw error to peers (it can echo token bytes).
fn jwt_error(e: &jsonwebtoken::errors::Error) -> String {
    let kind = e.kind();
    if matches!(kind, jsonwebtoken::errors::ErrorKind::ExpiredSignature) {
        "JWT expired".into()
    } else if matches!(
        kind,
        jsonwebtoken::errors::ErrorKind::InvalidIssuer
            | jsonwebtoken::errors::ErrorKind::InvalidAudience
    ) {
        "JWT claim mismatch".into()
    } else if matches!(kind, jsonwebtoken::errors::ErrorKind::InvalidSignature) {
        "JWT signature invalid".into()
    } else {
        "JWT validation failed".into()
    }
}

/// The claims we deserialize. `iss`/`aud`/`exp`/`sub` are validated by
/// `jsonwebtoken`; everything else lands in `extra` for the roles-path lookup.
/// `iss`/`aud` are kept as required fields so a token lacking them fails to
/// deserialize (an additional, structural denial), even though we never read
/// their values here.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Claims {
    sub: String,
    exp: usize,
    iss: String,
    aud: Value,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// Navigate `extra` by a dotted path (e.g. `realm_access.roles`) and collect the
/// explicit roles. Scopes and any other claim are deliberately ignored.
fn extract_roles(extra: &Map<String, Value>, path: &[String]) -> HashSet<Role> {
    let mut out = HashSet::new();
    if path.is_empty() {
        return out;
    }
    let mut node: Option<&Value> = extra.get(&path[0]);
    for seg in &path[1..] {
        node = node.and_then(|v| v.get(seg));
    }
    let Some(node) = node else {
        return out;
    };
    match node {
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str()
                    && let Some(role) = Role::parse(s)
                {
                    out.insert(role);
                }
            }
        }
        Value::String(s) => {
            if let Some(role) = Role::parse(s) {
                out.insert(role);
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// JWKS cache (periodically refreshed, fail-closed)
// ---------------------------------------------------------------------------

/// A refreshable cache of JWKS signing keys.
///
/// - Construct from [`JwksCache::from_static`] (inline JSON) or
///   [`JwksCache::empty`] (URL-refreshed; populated by [`run_jwks_refresher`]).
/// - [`JwksCache::key_by_kid`] returns `None` when there are no keys or the
///   last successful fetch is older than `max_age` (stale → deny).
/// - [`JwksCache::refresh`] replaces the keys; on error it keeps the previous
///   keys so a transient network blip does not mass-deny, but the `max_age`
///   guard still eventually fails them closed.
pub struct JwksCache {
    inner: RwLock<JwksState>,
    max_age: Duration,
    clock: Arc<dyn Clock>,
}

struct JwksState {
    /// kid → key.
    keys: HashMap<String, jsonwebtoken::jwk::Jwk>,
    /// When the keys were last successfully fetched.
    last_ok: Option<std::time::Instant>,
}

impl JwksCache {
    /// A cache seeded from inline (static) JWKS JSON. `last_ok` is set to now,
    /// so the keys are immediately usable and stay fresh for `max_age` (refresh
    /// is not required for the static case).
    #[must_use]
    pub fn from_static(set: JwkSet, max_age: Duration, clock: Arc<dyn Clock>) -> Self {
        let keys = index_keys(set);
        Self {
            inner: RwLock::new(JwksState {
                keys,
                last_ok: Some(clock.now()),
            }),
            max_age,
            clock,
        }
    }

    /// An empty cache awaiting a URL refresh. Until the first successful
    /// refresh, [`Self::key_by_kid`] denies every key (fail-closed).
    #[must_use]
    pub fn empty(max_age: Duration, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: RwLock::new(JwksState {
                keys: HashMap::new(),
                last_ok: None,
            }),
            max_age,
            clock,
        }
    }

    /// Look up a key by `kid`. Returns `None` if the cache is empty or stale.
    fn key_by_kid(&self, kid: &str) -> Option<jsonwebtoken::jwk::Jwk> {
        let state = self.inner.read().expect("jwks cache poisoned");
        let fresh = state
            .last_ok
            .is_some_and(|t| self.clock.now().duration_since(t) <= self.max_age);
        if !fresh {
            return None;
        }
        state.keys.get(kid).cloned()
    }

    /// Whether the cached keys are absent or stale (used by health/logging).
    #[must_use]
    pub fn is_stale(&self) -> bool {
        let state = self.inner.read().expect("jwks cache poisoned");
        state
            .last_ok
            .is_none_or(|t| self.clock.now().duration_since(t) > self.max_age)
    }

    /// Refresh the cache by fetching `url` over `http` and parsing a JWKS JSON
    /// document. On success the keys + `last_ok` are replaced; on error the old
    /// keys are retained (see type docs).
    ///
    /// # Errors
    /// Propagates the transport error; the cache is left unchanged.
    pub async fn refresh(
        &self,
        url: &str,
        http: &Arc<dyn HttpFetch>,
    ) -> Result<(), HttpFetchError> {
        let resp = http
            .fetch(HttpRequest {
                method: HttpMethod::Get,
                url: url.to_owned(),
                headers: Vec::new(),
                body: None,
            })
            .await?;
        if !resp.is_success() {
            return Err(HttpFetchError::Transport(format!(
                "JWKS endpoint returned HTTP {}",
                resp.status
            )));
        }
        let set: JwkSet = serde_json::from_slice(&resp.body)
            .map_err(|e| HttpFetchError::Transport(format!("invalid JWKS JSON: {e}")))?;
        let keys = index_keys(set);
        let mut state = self.inner.write().expect("jwks cache poisoned");
        state.keys = keys;
        state.last_ok = Some(self.clock.now());
        Ok(())
    }
}

/// Index a `JwkSet` into a kid→key map. Keys without a `kid` are dropped (we
/// route by `kid`; a keyless entry cannot be selected safely).
fn index_keys(set: JwkSet) -> HashMap<String, jsonwebtoken::jwk::Jwk> {
    set.keys
        .into_iter()
        .filter_map(|k| k.common.key_id.clone().map(|id| (id, k)))
        .collect()
}

/// Run a JWKS URL refresher forever (spawn it from the binary). Fetches once on
/// entry, then every `interval`. Before the first successful fetch, retry once a
/// second: dependency health can change between Compose's readiness check and
/// this task's first request, and waiting the normal 15-minute rotation interval
/// would leave every live session denied. After keys have loaded, failures keep
/// the previous keys and the cache's `max_age` guard handles prolonged outages.
pub async fn run_jwks_refresher(
    jwks: Arc<JwksCache>,
    http: Arc<dyn HttpFetch>,
    url: String,
    interval: Duration,
) {
    loop {
        match jwks.refresh(&url, &http).await {
            Ok(()) => {
                tracing::info!(url = %url, keys = "loaded", "JWKS refreshed");
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, url = %url, "initial JWKS refresh failed; retrying in one second (denying until success)");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` yields immediately once; the successful initial fetch above
    // already covered that tick.
    tick.tick().await;
    loop {
        tick.tick().await;
        if let Err(e) = jwks.refresh(&url, &http).await {
            tracing::warn!(error = %e, url = %url, "periodic JWKS refresh failed; retaining previous keys");
        }
    }
}

// ---------------------------------------------------------------------------
// Verified-token cache
// ---------------------------------------------------------------------------

/// A bounded cache of verified tokens → [`Identity`], to avoid re-verifying the
/// JWT signature on every request. Entries expire at `min(cache_ttl, token exp)`,
/// so the cache cannot outlive the token.
pub struct TokenCache {
    entries: Mutex<HashMap<String, CachedIdentity>>,
    ttl_secs: u64,
    max_entries: usize,
}

#[derive(Clone)]
struct CachedIdentity {
    identity: Identity,
    expires_at_unix: u64,
}

impl TokenCache {
    /// `ttl_secs == 0` disables the cache (every token is re-verified).
    #[must_use]
    pub fn new(ttl_secs: u64, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl_secs,
            max_entries,
        }
    }

    fn get(&self, token: &str) -> Option<Identity> {
        if self.ttl_secs == 0 {
            return None;
        }
        let mut entries = self.entries.lock().expect("token cache poisoned");
        let now = unix_now();
        let expired = entries.get(token).is_some_and(|c| c.expires_at_unix <= now);
        if expired {
            entries.remove(token);
        }
        entries.get(token).map(|c| c.identity.clone())
    }

    fn insert(&self, token: &str, identity: &Identity, token_exp_unix: usize) {
        if self.ttl_secs == 0 {
            return;
        }
        let mut entries = self.entries.lock().expect("token cache poisoned");
        let now = unix_now();
        let ttl_cap = now.saturating_add(self.ttl_secs);
        // `token_exp_unix` is a JWT `exp` (usize); widen to u64 without a
        // truncating cast so the lint is satisfied on 32-bit pointer targets.
        let token_exp = u64::try_from(token_exp_unix).unwrap_or(u64::MAX);
        let exp_cap = token_exp.min(ttl_cap.max(1));
        // Bound the cache: drop expired entries, and if still over capacity, clear
        // the oldest half. This is a coarse LRU; the cache is a perf optimization,
        // not a correctness mechanism.
        if entries.len() >= self.max_entries {
            entries.retain(|_, c| c.expires_at_unix > now);
        }
        if entries.len() >= self.max_entries {
            entries.clear();
        }
        entries.insert(
            token.to_owned(),
            CachedIdentity {
                identity: identity.clone(),
                expires_at_unix: exp_cap,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Document authorization (the per-document gate)
// ---------------------------------------------------------------------------

/// Decides whether a verified [`Identity`] may access `doc`, and with which
/// [`Role`]. This is the gate that ensures **a globally-valid token alone never
/// grants an arbitrary document** — it must affirmatively allow the specific
/// (subject, document) pair.
///
/// `raw_token` is available for authorizers that forward the token onward (e.g.
/// to a passthrough endpoint that re-validates it).
#[async_trait]
pub trait DocumentAuthorizer: Send + Sync {
    /// Authorize `identity` for `doc`. `Err` is a hard denial.
    async fn authorize(
        &self,
        identity: &Identity,
        doc: &DocId,
        raw_token: &str,
    ) -> Result<Role, AuthError>;
}

/// Denies every document. The safe default when no verifier is configured —
/// production must explicitly wire [`HttpDocumentAuthorizer`] (or another impl).
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAllAuthorizer;

#[async_trait]
impl DocumentAuthorizer for DenyAllAuthorizer {
    async fn authorize(
        &self,
        _identity: &Identity,
        doc: &DocId,
        _raw_token: &str,
    ) -> Result<Role, AuthError> {
        Err(AuthError::Unauthenticated(format!(
            "no document grant for {doc} (deny-all)"
        )))
    }
}

/// Request body sent to the app authorization endpoint.
#[derive(Debug, Serialize)]
struct AuthzRequest<'a> {
    subject: &'a str,
    document: &'a str,
}

/// Response body expected from the app authorization endpoint.
#[derive(Debug, Deserialize)]
struct AuthzResponse {
    role: String,
}

/// Calls an external (app) authorization endpoint to decide per-document access.
///
/// Wire contract (the app service implements the server side):
///
/// ```text
/// POST <NISABA_SYNC_AUTHZ_URL>
/// Authorization: Bearer <NISABA_SYNC_AUTHZ_TOKEN>
/// Content-Type: application/json
/// { "subject": "<sub>", "document": "<doc_id>" }
///
/// → 200 { "role": "author" | "reviewer" | "read-only" }   // allow
/// → 401 | 403 | 4xx | 5xx                                   // deny
/// ```
///
/// Any transport error, timeout, non-2xx status, or unparseable role is a
/// **denial** — never an allow.
pub struct HttpDocumentAuthorizer {
    http: Arc<dyn HttpFetch>,
    url: String,
    service_token: String,
    timeout: Duration,
}

impl HttpDocumentAuthorizer {
    #[must_use]
    pub fn new(
        http: Arc<dyn HttpFetch>,
        url: impl Into<String>,
        service_token: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            http,
            url: url.into(),
            service_token: service_token.into(),
            timeout,
        }
    }
}

#[async_trait]
impl DocumentAuthorizer for HttpDocumentAuthorizer {
    async fn authorize(
        &self,
        identity: &Identity,
        doc: &DocId,
        _raw_token: &str,
    ) -> Result<Role, AuthError> {
        let body = serde_json::to_vec(&AuthzRequest {
            subject: &identity.subject,
            document: doc.as_str(),
        })
        .map_err(|e| AuthError::Unauthenticated(format!("authz encode failed: {e}")))?;
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: self.url.clone(),
            headers: vec![
                (
                    "authorization".into(),
                    format!("Bearer {}", self.service_token),
                ),
                ("content-type".into(), "application/json".into()),
            ],
            body: Some(body),
        };
        let resp = match tokio::time::timeout(self.timeout, self.http.fetch(req)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "document authz transport error; denying");
                return Err(AuthError::Unauthenticated(
                    "document authz unavailable".into(),
                ));
            }
            Err(_) => {
                tracing::warn!(timeout = ?self.timeout, "document authz timed out; denying");
                return Err(AuthError::Unauthenticated(
                    "document authz timed out".into(),
                ));
            }
        };
        if !resp.is_success() {
            return Err(AuthError::Unauthenticated(format!(
                "document authz denied (HTTP {})",
                resp.status
            )));
        }
        let parsed: AuthzResponse = serde_json::from_slice(&resp.body).map_err(|_| {
            AuthError::Unauthenticated("document authz returned malformed body".into())
        })?;
        Role::parse(&parsed.role).ok_or_else(|| {
            AuthError::Unauthenticated(format!(
                "document authz returned unknown role {:?}",
                parsed.role
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// The resolver
// ---------------------------------------------------------------------------

/// Production [`AccessResolver`]: validate the JWT, then authorize the document.
///
/// Construct with [`OidcAccessResolver::try_new`]; the binary wires env vars and
/// the HTTP transport (see `main.rs`).
pub struct OidcAccessResolver {
    jwks: Arc<JwksCache>,
    validator: JwtValidator,
    documents: Arc<dyn DocumentAuthorizer>,
    tokens: TokenCache,
}

impl OidcAccessResolver {
    #[must_use]
    pub fn new(
        validator: JwtValidator,
        jwks: Arc<JwksCache>,
        documents: Arc<dyn DocumentAuthorizer>,
        tokens: TokenCache,
    ) -> Self {
        Self {
            jwks,
            validator,
            documents,
            tokens,
        }
    }
}

#[async_trait]
impl AccessResolver for OidcAccessResolver {
    async fn resolve(&self, doc: &DocId, token: &str) -> Result<Role, AuthError> {
        // JWT verification (cached). The document authorizer always runs after.
        let identity = if let Some(id) = self.tokens.get(token) {
            id
        } else {
            let (id, exp) = self.validator.validate(token, &self.jwks)?;
            self.tokens.insert(token, &id, exp);
            id
        };
        let membership_role = self.documents.authorize(&identity, doc, token).await?;
        // Clamp the membership-derived role with the IdP roles claim verified
        // from the JWT. A share link can grant membership at "author", but a
        // read-only IdP user must never gain author capabilities on the sync
        // plane (the REST plane correctly 403s them; this keeps both planes
        // consistent). Missing roles in the token deny access (fail-closed).
        let idp_role = Role::max_role(&identity.roles)
            .ok_or_else(|| AuthError::Unauthenticated("token carries no roles claim".into()))?;
        Ok(Role::least_privileged(membership_role, idp_role))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DocId;
    use crate::http::{HttpFetch, HttpFetchError, HttpRequest, HttpResponse};
    use crate::time::ManualClock;
    use jsonwebtoken::jwk::{AlgorithmParameters, CommonParameters, Jwk, OctetKeyParameters};
    use jsonwebtoken::{EncodingKey, Header};
    use serde_json::json;

    const ISSUER: &str = "https://idp.example/realms/nisaba";
    const AUDIENCE: &str = "nisaba-sync";
    const KID: &str = "test-key";
    const SECRET: &[u8] = b"sync-unit-test-secret";

    /// Build an HS256 JWKS with one key, plus the matching encoding key.
    fn hs_jwks() -> (JwkSet, EncodingKey) {
        let jwk = Jwk {
            common: CommonParameters {
                key_id: Some(KID.into()),
                key_algorithm: Some(jsonwebtoken::jwk::KeyAlgorithm::HS256),
                ..Default::default()
            },
            algorithm: AlgorithmParameters::OctetKey(OctetKeyParameters {
                value: base64::Engine::encode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    SECRET,
                ),
                ..Default::default()
            }),
        };
        let set = JwkSet { keys: vec![jwk] };
        (set, EncodingKey::from_secret(SECRET))
    }

    fn validator(algs: &[Algorithm], roles_claim: &str) -> JwtValidator {
        JwtValidator::new(JwtConfig {
            issuer: ISSUER.into(),
            audience: AUDIENCE.into(),
            allowed_algorithms: algs.iter().copied().collect(),
            roles_claim: roles_claim.into(),
            leeway_secs: 0,
        })
        .expect("validator config")
    }

    fn jwks_cache(set: JwkSet) -> Arc<JwksCache> {
        Arc::new(JwksCache::from_static(
            set,
            Duration::from_hours(1),
            Arc::new(ManualClock::new()),
        ))
    }

    /// Mint a signed JWT with the given `claims` (merged over the required
    /// registered claims) and `header` overrides.
    fn mint(extra: Value, header_alg: Algorithm, header_kid: Option<&str>) -> String {
        let (_, enc) = hs_jwks();
        let exp = unix_now() + 3600;
        let mut claims = json!({
            "sub": "alice",
            "exp": exp,
            "iss": ISSUER,
            "aud": AUDIENCE,
        });
        if let Value::Object(source) = extra
            && let Some(target) = claims.as_object_mut()
        {
            target.extend(source);
        }
        jsonwebtoken::encode(
            &Header {
                alg: header_alg,
                kid: header_kid.map(str::to_owned),
                ..Default::default()
            },
            &claims,
            &enc,
        )
        .expect("encode token")
    }

    fn valid_token() -> String {
        mint(json!({"roles": ["author"]}), Algorithm::HS256, Some(KID))
    }

    // ---- config validation -------------------------------------------------

    #[tokio::test]
    async fn rejects_alg_none_token() {
        // A hand-crafted `alg: none` token (empty signature). `jsonwebtoken`
        // cannot parse a `none` header, so `decode_header` fails → denial.
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let header = b"{\"alg\":\"none\",\"typ\":\"JWT\"}";
        let payload_bytes = serde_json::to_vec(&json!({
            "sub": "alice",
            "exp": unix_now() + 3600,
            "iss": ISSUER,
            "aud": AUDIENCE,
            "roles": ["author"],
        }))
        .unwrap();
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = format!(
            "{}.{}.",
            base64::Engine::encode(&enc, header),
            base64::Engine::encode(&enc, payload_bytes),
        );
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    #[test]
    fn rejects_empty_algorithms() {
        assert!(
            JwtValidator::new(JwtConfig {
                issuer: ISSUER.into(),
                audience: AUDIENCE.into(),
                allowed_algorithms: HashSet::new(),
                roles_claim: "roles".into(),
                leeway_secs: 0,
            })
            .is_err()
        );
    }

    // ---- happy path --------------------------------------------------------

    #[tokio::test]
    async fn resolves_when_jwt_and_document_grant_both_ok() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("chapters_introduction").unwrap();
        assert_eq!(r.resolve(&doc, &valid_token()).await.unwrap(), Role::Author);
    }

    // ---- invalid alg / alg confusion --------------------------------------

    #[tokio::test]
    async fn rejects_algorithm_not_in_allowlist() {
        let (set, _) = hs_jwks();
        // Configured allow-list is RS256-only; token is HS256.
        let v = validator(&[Algorithm::RS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        let err = r.resolve(&doc, &valid_token()).await.unwrap_err();
        assert!(matches!(err, AuthError::Unauthenticated(_)));
    }

    #[tokio::test]
    async fn rejects_alg_mismatch_with_jwk() {
        // Algorithm-confusion defense: even though HS384 is in the allow-list and
        // the kid matches, the selected JWK's configured algorithm is HS256. The
        // header alg must equal the key's alg, so this is rejected — an attacker
        // cannot choose the verification algorithm by tampering the header.
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256, Algorithm::HS384], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let token = mint(json!({"roles": ["author"]}), Algorithm::HS384, Some(KID));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    // ---- invalid kid -------------------------------------------------------

    #[tokio::test]
    async fn rejects_unknown_kid() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        // kid points at a key the JWKS does not contain.
        let token = mint(
            json!({"roles": ["author"]}),
            Algorithm::HS256,
            Some("missing"),
        );
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_missing_kid() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let token = mint(json!({"roles": ["author"]}), Algorithm::HS256, None);
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    // ---- invalid issuer / audience ----------------------------------------

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let token = mint(
            json!({"iss": "https://attacker.example/realms/evil"}),
            Algorithm::HS256,
            Some(KID),
        );
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_wrong_audience() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let token = mint(json!({"aud": "someone-else"}), Algorithm::HS256, Some(KID));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    // ---- expired -----------------------------------------------------------

    #[tokio::test]
    async fn rejects_expired_token() {
        let (set, enc) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let claims = json!({
            "sub": "alice",
            "exp": unix_now().saturating_sub(3600), // long expired
            "iss": ISSUER,
            "aud": AUDIENCE,
            "roles": ["author"],
        });
        let token = jsonwebtoken::encode(
            &Header {
                alg: Algorithm::HS256,
                kid: Some(KID.into()),
                ..Default::default()
            },
            &claims,
            &enc,
        )
        .unwrap();
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &token).await.is_err());
    }

    // ---- scope escalation must NOT escalate -------------------------------

    #[tokio::test]
    async fn scope_claim_does_not_escalate_roles() {
        // A malicious token carries an admin-y scope but an explicit read-only
        // role. The resolver must give read-only — scopes are never read.
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        // Authorizer grants exactly the identity's strongest global role, proving
        // the role came from the `roles` claim, not the scope.
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(GlobalRoleAuthorizer);
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(0, 0));
        let token = mint(
            json!({"roles": ["read-only"], "scope": "admin docs:write profile"}),
            Algorithm::HS256,
            Some(KID),
        );
        let doc = DocId::new("d1").unwrap();
        assert_eq!(r.resolve(&doc, &token).await.unwrap(), Role::ReadOnly);
    }

    #[tokio::test]
    async fn roles_under_nested_path_keycloak_style() {
        // Keycloak maps roles under realm_access.roles; the dotted path must work.
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "realm_access.roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(GlobalRoleAuthorizer);
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(0, 0));
        let token = mint(
            json!({"realm_access": {"roles": ["reviewer"]}}),
            Algorithm::HS256,
            Some(KID),
        );
        let doc = DocId::new("d1").unwrap();
        assert_eq!(r.resolve(&doc, &token).await.unwrap(), Role::Reviewer);
    }

    // ---- document deny / allow via mock authorizer ------------------------

    #[tokio::test]
    async fn document_denial_overrides_valid_token() {
        // Token is perfectly valid, but the document authorizer denies this doc.
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "bob" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        // alice's token, but only bob is allowed → denial.
        assert!(r.resolve(&doc, &valid_token()).await.is_err());
    }

    #[tokio::test]
    async fn deny_all_authorizer_denies_every_document() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(DenyAllAuthorizer);
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &valid_token()).await.is_err());
    }

    // ---- HTTP verifier (mocked transport) ---------------------------------

    #[tokio::test]
    async fn http_verifier_allows_on_200_role() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let http: Arc<dyn HttpFetch> = Arc::new(CannedHttp::ok(r#"{"role":"reviewer"}"#));
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(HttpDocumentAuthorizer::new(
            http,
            "https://app/internal/sync/authorize",
            "svc-token",
            Duration::from_secs(2),
        ));
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        assert_eq!(
            r.resolve(&doc, &valid_token()).await.unwrap(),
            Role::Reviewer
        );
    }

    #[tokio::test]
    async fn http_verifier_denies_on_403() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let http: Arc<dyn HttpFetch> = Arc::new(CannedHttp::status(403));
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(HttpDocumentAuthorizer::new(
            http,
            "https://app/internal/sync/authorize",
            "svc-token",
            Duration::from_secs(2),
        ));
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &valid_token()).await.is_err());
    }

    #[tokio::test]
    async fn http_verifier_denies_on_transport_error() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let http: Arc<dyn HttpFetch> = Arc::new(CannedHttp::err());
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(HttpDocumentAuthorizer::new(
            http,
            "https://app/internal/sync/authorize",
            "svc-token",
            Duration::from_secs(2),
        ));
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &valid_token()).await.is_err());
    }

    #[tokio::test]
    async fn http_verifier_denies_on_unknown_role_body() {
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let http: Arc<dyn HttpFetch> = Arc::new(CannedHttp::ok(r#"{"role":"superuser"}"#));
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(HttpDocumentAuthorizer::new(
            http,
            "https://app/internal/sync/authorize",
            "svc-token",
            Duration::from_secs(2),
        ));
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(60, 16));
        let doc = DocId::new("d1").unwrap();
        assert!(r.resolve(&doc, &valid_token()).await.is_err());
    }

    // ---- JWKS staleness ----------------------------------------------------

    #[tokio::test]
    async fn stale_jwks_denies() {
        let clock = Arc::new(ManualClock::new());
        let (set, _) = hs_jwks();
        let jwks = Arc::new(JwksCache::from_static(
            set,
            Duration::from_mins(1),
            clock.clone(),
        ));
        // Immediately, the key is present.
        assert!(jwks.key_by_kid(KID).is_some());
        // Advance past max_age → stale → deny.
        clock.advance(Duration::from_secs(61));
        assert!(jwks.key_by_kid(KID).is_none());
        assert!(jwks.is_stale());
    }

    #[tokio::test]
    async fn empty_jwks_denies_until_refreshed() {
        let clock = Arc::new(ManualClock::new());
        let jwks = Arc::new(JwksCache::empty(Duration::from_hours(1), clock.clone()));
        assert!(jwks.key_by_kid(KID).is_none());
        // A mocked JWKS endpoint populates the cache.
        let (set, _) = hs_jwks();
        let body = serde_json::to_vec(&set).unwrap();
        let http: Arc<dyn HttpFetch> = Arc::new(CannedHttp::ok_body(body));
        jwks.refresh("https://idp/.well-known/jwks.json", &http)
            .await
            .unwrap();
        assert!(jwks.key_by_kid(KID).is_some());
    }

    // ---- token cache -------------------------------------------------------

    #[tokio::test]
    async fn token_cache_skips_revalidation_within_ttl() {
        // A validating authorizer that counts calls; a cache hit must not
        // re-validate the JWT (it only re-runs the authorizer).
        let (set, _) = hs_jwks();
        let v = validator(&[Algorithm::HS256], "roles");
        let jwks = jwks_cache(set);
        let docs: Arc<dyn DocumentAuthorizer> = Arc::new(AllowAuthorizer { subject: "alice" });
        let r = OidcAccessResolver::new(v, jwks, docs, TokenCache::new(600, 16));
        let doc = DocId::new("d1").unwrap();
        let token = valid_token();
        // Resolve twice; both succeed.
        assert!(r.resolve(&doc, &token).await.is_ok());
        assert!(r.resolve(&doc, &token).await.is_ok());
    }

    // ---- helpers / mocks ---------------------------------------------------

    /// Allows the doc iff the identity subject matches; grants Author.
    struct AllowAuthorizer {
        subject: &'static str,
    }

    #[async_trait]
    impl DocumentAuthorizer for AllowAuthorizer {
        async fn authorize(
            &self,
            identity: &Identity,
            _doc: &DocId,
            _raw_token: &str,
        ) -> Result<Role, AuthError> {
            if identity.subject == self.subject {
                Ok(Role::Author)
            } else {
                Err(AuthError::Forbidden {
                    role: Role::ReadOnly,
                    cap: "subject not allowed for document",
                })
            }
        }
    }

    /// Grants the identity's strongest global role (used to prove scopes do not
    /// escalate roles).
    struct GlobalRoleAuthorizer;

    #[async_trait]
    impl DocumentAuthorizer for GlobalRoleAuthorizer {
        async fn authorize(
            &self,
            identity: &Identity,
            _doc: &DocId,
            _raw_token: &str,
        ) -> Result<Role, AuthError> {
            identity.strongest_role().ok_or(AuthError::Forbidden {
                role: Role::ReadOnly,
                cap: "no global role",
            })
        }
    }

    /// A canned HTTP transport returning a fixed response or a transport error.
    enum CannedHttp {
        Ok(Vec<u8>),
        Status(u16),
        Err,
    }

    impl CannedHttp {
        fn ok(body: &str) -> Self {
            Self::Ok(body.as_bytes().to_vec())
        }
        fn ok_body(body: Vec<u8>) -> Self {
            Self::Ok(body)
        }
        fn status(code: u16) -> Self {
            Self::Status(code)
        }
        fn err() -> Self {
            Self::Err
        }
    }

    #[async_trait]
    impl HttpFetch for CannedHttp {
        async fn fetch(&self, _req: HttpRequest) -> Result<HttpResponse, HttpFetchError> {
            match self {
                Self::Ok(body) => Ok(HttpResponse {
                    status: 200,
                    body: body.clone(),
                }),
                Self::Status(code) => Ok(HttpResponse {
                    status: *code,
                    body: Vec::new(),
                }),
                Self::Err => Err(HttpFetchError::Transport("mock failure".into())),
            }
        }
    }
}
