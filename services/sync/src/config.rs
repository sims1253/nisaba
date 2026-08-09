//! Limits, security validation, and stable identifiers.
//!
//! This component is the single source of truth for every configured bound the
//! transport enforces. Keeping them here makes the `DoS` / fuzz surface auditable
//! in one place: any untrusted byte stream entering the server is bounded by one
//! of the constants below before it touches the `CRDT` or the filesystem.

use crate::error::SyncError;

/// Maximum length of a document id, in UTF-8 bytes.
pub const MAX_DOC_ID_LEN: usize = 128;
/// Maximum number of live peers (sessions) allowed in one document room.
pub const MAX_PEERS_PER_DOC: usize = 64;
/// Maximum size of a single CRDT update frame, in bytes (4 MiB).
pub const MAX_UPDATE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum size of an encoded version vector sent in a HELLO, in bytes.
pub const MAX_VV_BYTES: usize = 64 * 1024;
/// Maximum size of a presence payload, in bytes.
pub const MAX_PRESENCE_BYTES: usize = 16 * 1024;
/// Maximum size of the token carried in a HELLO frame, in bytes. Kept far below
/// [`MAX_UPDATE_BYTES`] so a pre-auth peer cannot force a large allocation or a
/// long token holding a resolver task alive.
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;
/// Minimum spacing (ms) between presence roster re-broadcasts per room; bursts
/// of presence frames are coalesced down to one broadcast per interval.
pub const PRESENCE_BROADCAST_COALESCE_MS: u64 = 250;
/// Hard cap on concurrently-open document rooms. Prevents an unprivileged flood
/// of distinct document ids from pinning an unbounded number of rooms (and op-log
/// file handles) forever.
pub const MAX_ROOMS: usize = 1024;
/// Default idle time (ms) after which an empty room + its op-log handle are
/// evicted.
pub const EVICT_IDLE_TTL_MS: u64 = 15 * 60 * 1000;
/// Default timeout (ms) a session must complete its handshake (send a HELLO that
/// passes validation) within before the connection is dropped. Bounds the cost of
/// never-speaking peers: a flood of idle sockets cannot hold a task alive forever.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 10_000;
/// Default cap on inbound frames processed per second per session. A wildly
/// chatty peer (busy-loop flooding Update/Presence frames) is throttled even
/// though each frame is individually size-bounded: the per-frame byte bounds
/// alone would still let an attacker burn CPU on frame decode + op-log append +
/// fan-out. Generous for real collaboration (typing bursts are far below this).
pub const MAX_FRAMES_PER_SECOND: u32 = 200;
/// Maximum size of a server→client snapshot, in bytes (64 MiB).
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
/// Presence entry is considered expired after this many milliseconds without a
/// heartbeat.
pub const PRESENCE_TTL_MS: u64 = 30_000;
/// How often the presence sweeper runs, in milliseconds.
pub const PRESENCE_SWEEP_MS: u64 = 5_000;
/// Number of inbound updates between automatic snapshots. Tuned small for tests;
/// production tunes this per §8.1 ("instrument, do not engineer").
pub const SNAPSHOT_EVERY_UPDATES: u64 = 256;

/// A validated document identifier.
///
/// Document ids double as filesystem keys for the op log and snapshot store, so
/// the validation rules exist to prevent path traversal and ambiguous routing:
/// they must be non-empty, ASCII, and limited to `[A-Za-z0-9._-]` with no leading
/// dot or ``..`` segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DocId(String);

impl DocId {
    /// Validate and wrap a document id.
    pub fn new(id: impl Into<String>) -> Result<Self, SyncError> {
        let id = id.into();
        validate_doc_id(&id)?;
        Ok(Self(id))
    }

    /// Returns the inner string. It is guaranteed to satisfy [`validate_doc_id`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for DocId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A Loro peer identifier. Must be non-zero; every replica uses a distinct one so
/// the CRDT can attribute ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub u64);

impl PeerId {
    /// Validate that the peer id is usable (non-zero).
    pub fn new(id: u64) -> Result<Self, SyncError> {
        if id == 0 {
            return Err(SyncError::Handshake("peer id must be non-zero".to_string()));
        }
        Ok(Self(id))
    }

    /// Raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Validate a document id against the security rules.
///
/// Returns the id on success so callers can chain.
pub fn validate_doc_id(id: &str) -> Result<&str, SyncError> {
    if id.is_empty() || id.len() > MAX_DOC_ID_LEN {
        return Err(SyncError::InvalidDocId(id.to_string()));
    }
    if id.starts_with('.') {
        return Err(SyncError::InvalidDocId(id.to_string()));
    }
    if !id.bytes().all(is_allowed_doc_id_byte) {
        return Err(SyncError::InvalidDocId(id.to_string()));
    }
    // Reject `..` as a whole segment — defence in depth against path traversal in
    // the filesystem stores, even though the byte allowlist already forbids `/`.
    for seg in id.split('_') {
        if seg == ".." {
            return Err(SyncError::InvalidDocId(id.to_string()));
        }
    }
    let _ = id;
    Ok(id)
}

#[inline]
const fn is_allowed_doc_id_byte(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')
}

/// Bounded configuration. All fields have safe defaults; production overrides via
/// [`Config::from_env`] (kept minimal for M2 — full config management is app's job).
#[derive(Debug, Clone)]
pub struct Config {
    /// Max inbound update size.
    pub max_update_bytes: usize,
    /// Max live peers per document.
    pub max_peers_per_doc: usize,
    /// Presence TTL (milliseconds).
    pub presence_ttl_ms: u64,
    /// Presence sweeper interval (milliseconds).
    pub presence_sweep_ms: u64,
    /// Updates between automatic snapshots.
    pub snapshot_every_updates: u64,
    /// Hard cap on live rooms; oldest empty room is evicted first when full.
    pub max_rooms: usize,
    /// Idle time (ms) after which an empty room is evicted (drops its op-log
    /// file handle).
    pub evict_idle_ttl_ms: u64,
    /// Timeout (ms) a session must complete its handshake within.
    pub handshake_timeout_ms: u64,
    /// Max inbound frames processed per second per session.
    pub max_frames_per_second: u32,
    /// Presence roster re-broadcast coalesce interval (ms).
    pub presence_coalesce_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_update_bytes: MAX_UPDATE_BYTES,
            max_peers_per_doc: MAX_PEERS_PER_DOC,
            presence_ttl_ms: PRESENCE_TTL_MS,
            presence_sweep_ms: PRESENCE_SWEEP_MS,
            snapshot_every_updates: SNAPSHOT_EVERY_UPDATES,
            max_rooms: MAX_ROOMS,
            evict_idle_ttl_ms: EVICT_IDLE_TTL_MS,
            handshake_timeout_ms: HANDSHAKE_TIMEOUT_MS,
            max_frames_per_second: MAX_FRAMES_PER_SECOND,
            presence_coalesce_ms: PRESENCE_BROADCAST_COALESCE_MS,
        }
    }
}

impl Config {
    /// Reject a payload length against the update bound.
    pub fn check_update_size(&self, len: usize) -> Result<(), SyncError> {
        if len > self.max_update_bytes {
            return Err(SyncError::Limit(format!(
                "update of {len} bytes exceeds limit of {}",
                self.max_update_bytes
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_ids() {
        assert!(DocId::new("chapters_introduction").is_ok());
        assert!(DocId::new("doc.001").is_ok());
        assert!(DocId::new("a").is_ok());
    }

    #[test]
    fn rejects_dangerous_ids() {
        for bad in [
            "",
            "..",
            "../x",
            "a/b",
            " a",
            "a b",
            ".hidden",
            &"x".repeat(200),
        ] {
            assert!(DocId::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn rejects_zero_peer() {
        assert!(PeerId::new(0).is_err());
        assert!(PeerId::new(1).is_ok());
    }
}
