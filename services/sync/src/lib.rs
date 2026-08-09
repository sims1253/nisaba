//! Nisaba sync service — Loro CRDT authority, relay, presence, and snapshots.
//!
//! This crate implements the M2 sync layer described in `PLAN.md` §4 / §8:
//!
//! * a WebSocket **authority/relay** keyed by document ids,
//! * **binary CRDT update import/export** (opaque bytes; sync never inspects
//!   Loro state),
//! * **reconnect catch-up** via version vectors, with a full-snapshot fallback,
//! * ephemeral **presence/awareness** with heartbeat expiry,
//! * a **role-aware access seam** (`author` / `reviewer` / `read-only`),
//! * an **append-only op log** and a **pluggable snapshot store** (filesystem
//!   implementation standing in for the S3-compatible blob boundary),
//! * **periodic snapshots**.
//!
//! The pure CRDT core ([`authority`], [`op_log`], [`snapshot`], [`presence`],
//! [`room`], [`registry`]) has no server dependency; the HTTP/WebSocket server
//! lives behind the `server` feature (on by default).
//!
//! Review-layer concerns (soft deletes, marks, accept/reject) are deliberately
//! out of scope here: "sync transports opaque Loro state". In
//! particular this service makes **no physical deletion assumptions** — every
//! update is opaque bytes that may carry pending-suggestion marks over CRDT
//! positions.

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
pub mod snapshot;
pub mod time;

#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub mod session;

pub use auth::{AccessResolver, AuthError, CapabilitySet, Identity, Role, StaticAccessResolver};
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
pub use snapshot::{FsSnapshotStore, MemorySnapshotStore, Snapshot, SnapshotStore};
pub use time::{Clock, ManualClock, SystemClock};

/// Crate version (kept in sync with the workspace package version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
