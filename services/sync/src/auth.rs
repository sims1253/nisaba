//! Role-aware access seam.
//!
//! The supported roles are `author`, `reviewer`, and `read-only` (the vocabulary
//! lives in the shared `nisaba-auth` crate so both services parse the same
//! spellings);
//! "Do not build auth" — OIDC integration is the `app` service's job. This component
//! therefore defines only the **seam**: how the sync transport learns a peer's
//! role for a document, and what that role permits at the transport layer.
//!
//! The transport policy is intentionally narrow and explicit (see [`CapabilitySet`]):
//! the only thing the *sync* layer restricts is mutation (pushing CRDT updates).
//! Everything about suggesting-vs-editing, accept/reject, and marks lives in the
//! review logic of the projection layer and is **not** the sync
//! service's concern — sync transports opaque Loro state.
//!
//! Concrete OIDC/JWT resolution is injected via [`AccessResolver`]: a static,
//! in-process resolver ([`StaticAccessResolver`]) stands in for it in local dev
//! and tests; the production [`crate::oidc::OidcAccessResolver`] validates a
//! JWT against JWKS and delegates per-document authorization to a narrow
//! [`crate::oidc::DocumentAuthorizer`] (HTTP verifier). See [`crate::oidc`].

use std::collections::HashSet;

use async_trait::async_trait;

use crate::config::DocId;

pub use nisaba_auth::Role;

/// Sync-transport extensions to the shared [`Role`]: what a role permits at the
/// transport layer, and how roles combine when clamping a document grant with
/// the bearer's `IdP` roles. These stay in the sync service (as an extension
/// trait on the shared type) because they are coupled to [`CapabilitySet`] and
/// the sync plane's clamp policy — the shared crate owns only the vocabulary.
pub trait RoleCapabilities {
    /// The set of capabilities this role has at the transport layer.
    #[must_use]
    fn capabilities(self) -> CapabilitySet;

    /// Whether this role may push CRDT updates.
    #[must_use]
    fn can_push_updates(self) -> bool;

    /// Rank used to combine roles: `author` (2) > `reviewer` (1) > `read-only` (0).
    #[must_use]
    fn rank(self) -> u8;

    /// The least privileged of two roles. Used to clamp the role a document
    /// authorizer grants (membership/share-link derived) with the bearer's `IdP`
    /// roles claim: a `read-only` `IdP` user who redeems an `author` share link
    /// must not gain author capabilities on the sync plane.
    #[must_use]
    fn least_privileged(self, other: Role) -> Role;

    /// The highest role in a set of `IdP` roles (empty set → `None`).
    #[must_use]
    fn max_role(roles: &HashSet<Role>) -> Option<Role>;
}

impl RoleCapabilities for Role {
    fn capabilities(self) -> CapabilitySet {
        let mut caps = CapabilitySet::RECEIVE_STATE | CapabilitySet::PRESENCE;
        if matches!(self, Self::Author | Self::Reviewer) {
            caps |= CapabilitySet::PUSH_UPDATES;
        }
        caps
    }

    fn can_push_updates(self) -> bool {
        self.capabilities().contains(CapabilitySet::PUSH_UPDATES)
    }

    fn rank(self) -> u8 {
        match self {
            Self::Author => 2,
            Self::Reviewer => 1,
            Self::ReadOnly => 0,
        }
    }

    fn least_privileged(self, other: Role) -> Role {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    fn max_role(roles: &HashSet<Role>) -> Option<Role> {
        roles.iter().copied().max_by_key(|role| role.rank())
    }
}

bitflags::bitflags! {
    /// Transport-layer capabilities. Kept as a bitflags so new capabilities
    /// (e.g. `MANAGE_PRESENCE`) compose without touching call sites.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CapabilitySet: u8 {
        /// Receive CRDT updates and snapshots.
        const RECEIVE_STATE = 1 << 0;
        /// Send CRDT updates that mutate the document.
        const PUSH_UPDATES  = 1 << 1;
        /// Participate in presence (heartbeat + state).
        const PRESENCE      = 1 << 2;
    }
}

/// Errors from access resolution / enforcement.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The token could not be resolved to any role.
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),
    /// Resolved, but the role is not permitted to do what was attempted.
    #[error("forbidden: role {role} lacks capability {cap}")]
    Forbidden { role: Role, cap: &'static str },
}

/// The verified identity a token resolves to. Production OIDC/JWT resolvers
/// populate this from validated token claims (subject + explicit `roles`
/// claim only — scopes are never inferred into roles, see [`crate::oidc`]);
/// per-document authorization is then a separate decision made by a
/// [`crate::oidc::DocumentAuthorizer`].
#[derive(Debug, Clone)]
pub struct Identity {
    /// The token subject (`sub` claim). Unique, stable principal identifier.
    pub subject: String,
    /// Global roles read from the explicit roles claim. These are *identity
    /// context only* — they never, by themselves, grant access to any document
    ///.
    pub roles: HashSet<Role>,
}

impl Identity {
    /// The strongest (most permissive) global role the identity holds, or `None`.
    #[must_use]
    pub fn strongest_role(&self) -> Option<Role> {
        if self.roles.contains(&Role::Author) {
            Some(Role::Author)
        } else if self.roles.contains(&Role::Reviewer) {
            Some(Role::Reviewer)
        } else if self.roles.contains(&Role::ReadOnly) {
            Some(Role::ReadOnly)
        } else {
            None
        }
    }
}

/// Resolves an opaque role token to a [`Role`] for a given document.
///
/// Implementations are injected by the deployment: [`StaticAccessResolver`] for
/// local dev and tests, [`crate::oidc::OidcAccessResolver`] (JWT/JWKS + document
/// authorizer) in production. The sync service never assumes how a token is
/// minted.
///
/// `async` because the production resolver may call an external authorization
/// endpoint per document; it is held as `Arc<dyn AccessResolver>` in the
/// [`crate::registry::DocRegistry`], hence `#[async_trait]`.
#[async_trait]
pub trait AccessResolver: Send + Sync {
    /// Resolve `token` to the [`Role`] the bearer has on `doc`. A returned
    /// `Err(AuthError)` is a hard denial — the caller must drop the peer.
    async fn resolve(&self, doc: &DocId, token: &str) -> Result<Role, AuthError>;
}

/// A trivial map-based resolver for local dev and tests.
#[derive(Debug, Default, Clone)]
pub struct StaticAccessResolver {
    /// Maps `(doc_id, token)` → role. An empty map denies everything.
    grants: std::collections::HashMap<(String, String), Role>,
    /// If set, any non-empty token resolves to this role (broad local-dev grant).
    allow_all: Option<Role>,
}

impl StaticAccessResolver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant `role` for `token` on `doc`.
    #[must_use]
    pub fn grant(mut self, doc: impl Into<String>, token: impl Into<String>, role: Role) -> Self {
        self.grants.insert((doc.into(), token.into()), role);
        self
    }

    /// Allow any non-empty token as `role`. Use only in local dev / tests.
    #[must_use]
    pub fn allow_all(role: Role) -> Self {
        Self {
            grants: std::collections::HashMap::new(),
            allow_all: Some(role),
        }
    }
}

#[async_trait]
impl AccessResolver for StaticAccessResolver {
    async fn resolve(&self, doc: &DocId, token: &str) -> Result<Role, AuthError> {
        if let Some(role) = self.grants.get(&(doc.to_string(), token.to_string())) {
            return Ok(*role);
        }
        if let Some(role) = self.allow_all.filter(|_| !token.is_empty()) {
            return Ok(role);
        }
        Err(AuthError::Unauthenticated(format!(
            "no grant for doc {doc}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_cannot_push() {
        assert!(!Role::ReadOnly.can_push_updates());
        assert!(Role::Reviewer.can_push_updates());
        assert!(Role::Author.can_push_updates());
    }

    #[tokio::test]
    async fn static_resolver_grants_and_denies() {
        let r = StaticAccessResolver::new().grant("d1", "tok-author", Role::Author);
        let d = DocId::new("d1").unwrap();
        assert_eq!(r.resolve(&d, "tok-author").await.unwrap(), Role::Author);
        assert!(r.resolve(&d, "wrong").await.is_err());
    }

    #[tokio::test]
    async fn allow_all_grants_for_nonempty_token() {
        let r = StaticAccessResolver::allow_all(Role::Reviewer);
        let d = DocId::new("d1").unwrap();
        assert_eq!(r.resolve(&d, "anything").await.unwrap(), Role::Reviewer);
        assert!(r.resolve(&d, "").await.is_err());
    }
}
