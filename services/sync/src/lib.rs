//! Nisaba sync service — Loro CRDT authority, relay, presence, and snapshots.
//!
//! This crate implements the Nisaba sync layer:
//!
//! * a WebSocket **authority/relay** keyed by document ids,
//! * **binary CRDT update import/export** (the relay path is opaque: peers'
//!   update bytes are never inspected or re-serialised),
//! * an **internal, service-token whole-state read** (`GET
//!   /internal/docs/{doc_id}/state`): a document's current state exported as
//!   an opaque snapshot for authenticated callers (the app's export path),
//!   still without interpretation,
//! * **reconnect catch-up** via version vectors, with a full-snapshot fallback,
//! * ephemeral **presence/awareness** with heartbeat expiry,
//! * a **role-aware access seam** (`author` / `reviewer` / `read-only`),
//! * an **append-only op log** and a **pluggable snapshot store** (filesystem
//!   and S3-backed implementations behind one trait; feature `s3`),
//! * **periodic snapshots**.
//!
//! The pure CRDT core ([`authority`], [`op_log`], [`snapshot`], [`presence`],
//! [`room`], [`registry`]) has no server dependency; the HTTP/WebSocket server
//! lives behind the `server` feature (on by default).
//!
//! Review-layer concerns (soft deletes, marks, accept/reject) are deliberately
//! out of scope for the relay: peers' updates are transported as opaque bytes
//! that may carry pending-suggestion marks over CRDT positions, and the relay
//! never filters or rewrites them.

#![forbid(unsafe_code)]

pub mod auth;
pub mod authority;
pub mod config;
pub mod error;
pub mod http;
pub mod oidc;
pub mod op_log;
pub mod presence;
pub mod protocol;
pub mod registry;
pub mod room;
#[cfg(feature = "s3")]
pub mod s3;
pub mod seed;
pub mod snapshot;
pub mod time;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod session;

pub use auth::{
    AccessResolver, AuthError, CapabilitySet, Identity, Role, RoleCapabilities,
    StaticAccessResolver,
};
pub use authority::AuthorityDoc;
pub use config::{Config, DocId, PeerId};
pub use error::{ProtoError, SyncError, SyncResult};
pub use oidc::{
    DenyAllAuthorizer, DocumentAuthorizer, HttpDocumentAuthorizer, JwksCache, JwtConfig,
    JwtValidator, OidcAccessResolver, TokenCache, run_jwks_refresher,
};
pub use op_log::{FsOpLogStore, MemoryOpLogStore, OpLogStore};
pub use presence::{Presence, PresenceEntry};
pub use protocol::{CatchUp, Frame, MsgType, PROTOCOL_VERSION, WelcomeStatus};
pub use registry::DocRegistry;
pub use room::{
    CLOSE_NORMAL, CLOSE_RESYNC_REQUIRED, CloseSignal, DocRoom, JoinOutcome, close_signal,
};
#[cfg(feature = "s3")]
pub use s3::{S3EnvConfig, S3OpLogStore, S3SnapshotStore, S3Stores};
pub use seed::{DenyAllSeedVerifier, HttpSeedVerifier, SeedVerifier};
pub use snapshot::{FsSnapshotStore, MemorySnapshotStore, Snapshot, SnapshotStore};
pub use time::{Clock, ManualClock, SystemClock};

/// Crate version (kept in sync with the workspace package version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
