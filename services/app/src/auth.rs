//! Authentication primitives: principals, roles, JWT verification, and access control.

use async_trait::async_trait;
use axum::{
    extract::{FromRequestParts, State},
    http::{HeaderMap, Request, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::Arc,
};
use uuid::Uuid;

use crate::AppState;
use crate::types::{AppError, MembershipRole};

#[derive(Debug, Clone)]
pub struct Principal {
    pub subject: String,
    pub roles: HashSet<Role>,
    /// The `preferred_username` claim (e.g. "demo"), used for membership
    /// lookups when the sharing UI invited by username rather than OIDC sub.
    #[allow(dead_code)]
    pub preferred_username: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Author,
    Reviewer,
    ReadOnly,
}
impl Role {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "author" => Some(Self::Author),
            "reviewer" => Some(Self::Reviewer),
            "read-only" | "readonly" | "read_only" => Some(Self::ReadOnly),
            _ => None,
        }
    }
}

#[async_trait]
pub trait JwksProvider: Send + Sync {
    async fn key(&self, kid: &str) -> Result<jsonwebtoken::jwk::Jwk, AppError>;
}
#[derive(Clone)]
pub struct StaticJwks {
    pub(crate) keys: Arc<HashMap<String, jsonwebtoken::jwk::Jwk>>,
}
impl StaticJwks {
    #[must_use]
    pub fn new(set: jsonwebtoken::jwk::JwkSet) -> Self {
        Self {
            keys: Arc::new(
                set.keys
                    .into_iter()
                    .filter_map(|k| k.common.key_id.clone().map(|id| (id, k)))
                    .collect(),
            ),
        }
    }
}
#[async_trait]
impl JwksProvider for StaticJwks {
    async fn key(&self, kid: &str) -> Result<jsonwebtoken::jwk::Jwk, AppError> {
        self.keys
            .get(kid)
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("unknown signing key".into()))
    }
}

#[derive(Clone)]
pub struct Authenticator {
    pub issuer: String,
    pub audience: String,
    pub jwks: Arc<dyn JwksProvider>,
}
impl Authenticator {
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Principal, AppError> {
        let value = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::Unauthorized("bearer token required".into()))?;
        let token = value
            .strip_prefix("Bearer ")
            .ok_or_else(|| AppError::Unauthorized("bearer token required".into()))?;
        let header =
            decode_header(token).map_err(|_| AppError::Unauthorized("invalid JWT".into()))?;
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| AppError::Unauthorized("JWT kid required".into()))?;
        let jwk = self.jwks.key(kid).await?;
        let key = DecodingKey::from_jwk(&jwk)
            .map_err(|_| AppError::Unauthorized("invalid JWKS key".into()))?;
        let configured_algorithm = jwk
            .common
            .key_algorithm
            .as_ref()
            .map(ToString::to_string)
            .and_then(|value| Algorithm::from_str(&value).ok())
            .ok_or_else(|| AppError::Unauthorized("JWKS key algorithm is required".into()))?;
        if configured_algorithm != header.alg {
            return Err(AppError::Unauthorized(
                "JWT algorithm does not match JWKS key".into(),
            ));
        }
        let mut validation = Validation::new(configured_algorithm);
        validation.set_issuer(std::slice::from_ref(&self.issuer));
        validation.set_audience(std::slice::from_ref(&self.audience));
        let token = decode::<Claims>(token, &key, &validation)
            .map_err(|_| AppError::Unauthorized("JWT validation failed".into()))?;
        let mut roles = HashSet::new();
        for role in &token.claims.roles {
            if let Some(role) = Role::parse(role) {
                roles.insert(role);
            }
        }
        Ok(Principal {
            subject: token
                .claims
                .sub
                .clone()
                .unwrap_or_else(|| token.claims.preferred_username.clone().unwrap_or_default()),
            roles,
            preferred_username: token.claims.preferred_username,
        })
    }
}
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Claims {
    #[serde(default)]
    sub: Option<String>,
    exp: usize,
    iss: String,
    aud: Value,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

#[derive(Clone, Copy)]
pub(crate) enum Permission {
    Read,
    Manage,
    Document,
}
impl Permission {
    /// Whether the principal's IdP roles satisfy this permission tier.
    fn allows(self, principal: &Principal) -> bool {
        match self {
            Permission::Read => !principal.roles.is_empty(),
            Permission::Manage => principal.roles.contains(&Role::Author),
            Permission::Document => {
                principal.roles.contains(&Role::Author) || principal.roles.contains(&Role::Reviewer)
            }
        }
    }
}

/// Enforce a permission tier against an already-verified principal. The JWT
/// signature is verified exactly once per request (see [`Auth`]); this only
/// checks the role claim.
pub(crate) fn permitted(
    principal: &Principal,
    permission: Permission,
) -> Result<(), AppError> {
    if permission.allows(principal) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

pub(crate) async fn project_access(
    state: &AppState,
    principal: &Principal,
    project_id: Uuid,
    permission: Permission,
) -> Result<(), AppError> {
    permitted(principal, permission)?;
    // Try the OIDC sub first; fall back to preferred_username so that
    // memberships created through the UI sharing flow (which sends the
    // human-typed username) also resolve.
    if state
        .repo
        .get_membership(project_id, &principal.subject)
        .await
        .is_ok()
    {
        return Ok(());
    }
    if let Some(ref username) = principal.preferred_username {
        state
            .repo
            .get_membership(project_id, username)
            .await
            .map_err(|_| AppError::Forbidden)?;
        return Ok(());
    }
    Err(AppError::Forbidden)
}

/// Extractor for the verified caller. The `project_acl` middleware verifies the
/// JWT once per request and stashes the resulting [`Principal`] in the request
/// extensions; this extractor reuses it when present and otherwise falls back
/// to authenticating from the `Authorization` header (for paths the middleware
/// does not gate). Handlers therefore never re-verify the signature: previously
/// `permitted`/`project_access` re-authenticated on every call, so a single
/// request could verify the same JWT up to three times.
pub(crate) struct Auth(pub Principal);

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(principal) = parts.extensions.get::<Principal>() {
            return Ok(Auth(principal.clone()));
        }
        Ok(Auth(state.auth.authenticate(&parts.headers).await?))
    }
}

pub(crate) async fn project_acl(
    State(state): State<AppState>,
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let mut segments = request.uri().path().split('/');
    let project_id = segments.nth(2).and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(project_id) = project_id else {
        // Not a project-scoped path: the handler performs (and answers for)
        // its own authentication. Verify anyway when a token is present so
        // downstream handlers can reuse the stashed principal instead of
        // verifying the signature a second time.
        if let Ok(principal) = state.auth.authenticate(request.headers()).await {
            request.extensions_mut().insert(principal);
        }
        return next.run(request).await;
    };
    let principal = match state.auth.authenticate(request.headers()).await {
        Ok(principal) => principal,
        Err(error) => return error.into_response(),
    };
    // Try the OIDC sub first; fall back to preferred_username so that
    // memberships created through the UI sharing flow (which sends the
    // human-typed username) also resolve.
    let membership = if let Ok(m) = state
        .repo
        .get_membership(project_id, &principal.subject)
        .await
    {
        m
    } else if let Some(ref username) = principal.preferred_username {
        let Ok(m) = state.repo.get_membership(project_id, username).await else {
            return AppError::Forbidden.into_response();
        };
        m
    } else {
        return AppError::Forbidden.into_response();
    };
    // Hand the verified identity to the handlers so they do not re-verify it.
    request.extensions_mut().insert(principal.clone());
    let path = request.uri().path();
    // Export is a read-only operation that generates a project archive from
    // existing data — no mutation. Reviewers need it to export review copies.
    // The handler additionally enforces Permission::Document.
    let is_export = path.ends_with("/exports") && request.method() == http::Method::POST;
    // Self-service leave: a member removing their own membership is allowed
    // regardless of role. The path is /projects/{id}/members/{subject}.
    let is_self_leave = request.method() == http::Method::DELETE
        && path.contains("/members/")
        && path.rsplit('/').next() == Some(&principal.subject);
    let can_write = match request.method() {
        &http::Method::GET | &http::Method::HEAD => true,
        _ if is_export => matches!(
            membership.role,
            MembershipRole::Owner | MembershipRole::Author | MembershipRole::Reviewer
        ),
        _ if is_self_leave => true,
        // Reviewers propose changes through the review layer, never by writing
        // the baseline directly (PATCH/DELETE would let them bypass track
        // changes and destroy author work). The sync relay still lets
        // reviewers push suggestions.
        _ => matches!(
            membership.role,
            MembershipRole::Owner | MembershipRole::Author
        ),
    };
    if !can_write {
        return AppError::Forbidden.into_response();
    }
    next.run(request).await
}
