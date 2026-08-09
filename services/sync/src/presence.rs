//! Ephemeral presence / awareness with heartbeat expiry.
//!
//! Presence is deliberately **not** stored in the CRDT or the op log: it is
//! transient state that must expire, not be replayed or snapshotted. This
//! component owns exactly that: a per-document roster of peers, each with a
//! last-seen timestamp read from an injectable [`Clock`].
//!
//! A background sweeper (or, in tests, an explicit [`Presence::sweep`] call)
//! removes peers whose heartbeat is older than the TTL.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::PeerId;
use crate::error::ProtoError;
use crate::time::Clock;

/// One peer's presence entry.
#[derive(Debug, Clone)]
pub struct PresenceEntry {
    /// Opaque client state (name, cursor, selection, colour, ...). JSON-encoded
    /// by the client; the server treats it as bytes.
    pub state: Vec<u8>,
    pub last_seen: Instant,
}

/// A per-document presence roster.
pub struct Presence {
    ttl: Duration,
    peers: HashMap<PeerId, PresenceEntry>,
    clock: Arc<dyn Clock>,
}

impl Presence {
    /// New roster with the given TTL (milliseconds) and clock.
    #[must_use]
    pub fn new(ttl_ms: u64, clock: Arc<dyn Clock>) -> Self {
        Self {
            ttl: Duration::from_millis(ttl_ms),
            peers: HashMap::new(),
            clock,
        }
    }

    /// Add or refresh a peer. `state` is the opaque presence payload (may be empty
    /// for a pure heartbeat).
    pub fn upsert(&mut self, peer: PeerId, state: Vec<u8>) {
        self.peers.insert(
            peer,
            PresenceEntry {
                state,
                last_seen: self.clock.now(),
            },
        );
    }

    /// Record a heartbeat (no state change) for `peer`. No-op if the peer is
    /// unknown — heartbeating a peer we never saw a Presence frame for is a
    /// protocol oddity, not an error.
    pub fn heartbeat(&mut self, peer: PeerId) {
        if let Some(entry) = self.peers.get_mut(&peer) {
            entry.last_seen = self.clock.now();
        }
    }

    /// Remove a peer (on graceful leave or socket close).
    pub fn remove(&mut self, peer: PeerId) -> bool {
        self.peers.remove(&peer).is_some()
    }

    /// Number of currently-present peers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// Whether any peers are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Whether `peer` is currently considered present.
    #[must_use]
    pub fn contains(&self, peer: PeerId) -> bool {
        self.peers.contains_key(&peer)
    }

    /// Remove peers whose heartbeat is older than the TTL. Returns the peers that
    /// were evicted, so callers can broadcast their departure.
    pub fn sweep(&mut self) -> Vec<PeerId> {
        let now = self.clock.now();
        let expired: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, e)| now.saturating_duration_since(e.last_seen) > self.ttl)
            .map(|(p, _)| *p)
            .collect();
        let mut expired = expired;
        expired.sort_unstable();
        for p in &expired {
            self.peers.remove(p);
        }
        expired
    }

    /// A snapshot of the roster as `(peer, state)` pairs, for broadcasting.
    #[must_use]
    pub fn roster(&self) -> Vec<(PeerId, Vec<u8>)> {
        let mut v: Vec<_> = self
            .peers
            .iter()
            .map(|(p, e)| (*p, e.state.clone()))
            .collect();
        v.sort_by_key(|(p, _)| p.get());
        v
    }
}

/// Encode a roster for the wire: `[u32 count][u64 peer][u32 len][bytes state]]`.
#[must_use]
pub fn encode_roster(roster: &[(PeerId, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    let count = u32::try_from(roster.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_be_bytes());
    for (peer, state) in roster {
        out.extend_from_slice(&peer.get().to_be_bytes());
        let len = u32::try_from(state.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(state);
    }
    out
}

/// Decode a roster produced by [`encode_roster`].
pub fn decode_roster(buf: &[u8]) -> Result<Vec<(PeerId, Vec<u8>)>, ProtoError> {
    const N: usize = 4;
    if buf.len() < N {
        return Err(ProtoError::Truncated);
    }
    let count = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let mut pos = N;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 8 + 4 > buf.len() {
            return Err(ProtoError::Truncated);
        }
        let mut p = [0u8; 8];
        p.copy_from_slice(&buf[pos..pos + 8]);
        pos += 8;
        let peer = u64::from_be_bytes(p);
        let len = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if pos + len > buf.len() {
            return Err(ProtoError::Truncated);
        }
        let state = buf[pos..pos + len].to_vec();
        pos += len;
        out.push((PeerId(peer), state));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::ManualClock;

    fn ro() -> (Presence, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        let p = Presence::new(1000, clock.clone());
        (p, clock)
    }

    #[test]
    fn upsert_adds_and_heartbeat_keeps_alive() {
        let (mut p, clock) = ro();
        p.upsert(PeerId(1), b"alice".to_vec());
        assert!(p.contains(PeerId(1)));
        assert_eq!(p.len(), 1);

        // Advance almost to the TTL and heartbeat; should still be present.
        clock.advance(Duration::from_millis(900));
        p.heartbeat(PeerId(1));
        clock.advance(Duration::from_millis(900));
        p.sweep();
        assert!(p.contains(PeerId(1)));
    }

    #[test]
    fn sweep_evicts_expired() {
        let (mut p, clock) = ro();
        p.upsert(PeerId(1), b"alice".to_vec());
        p.upsert(PeerId(2), b"bob".to_vec());

        // Cross the TTL without heartbeating peer 1.
        clock.advance(Duration::from_millis(1001));
        let evicted = p.sweep();
        assert_eq!(evicted, vec![PeerId(1), PeerId(2)]);
        assert!(p.is_empty());
    }

    #[test]
    fn selective_expiry() {
        let (mut p, clock) = ro();
        p.upsert(PeerId(1), b"alice".to_vec());
        clock.advance(Duration::from_millis(500));
        p.upsert(PeerId(2), b"bob".to_vec()); // bob added later
        clock.advance(Duration::from_millis(600)); // alice now 1100ms old, bob 600ms
        let evicted = p.sweep();
        assert_eq!(evicted, vec![PeerId(1)]);
        assert!(p.contains(PeerId(2)));
    }

    #[test]
    fn roster_is_sorted_and_stable() {
        let (mut p, _clock) = ro();
        p.upsert(PeerId(3), b"c".to_vec());
        p.upsert(PeerId(1), b"a".to_vec());
        p.upsert(PeerId(2), b"b".to_vec());
        let ids: Vec<_> = p.roster().into_iter().map(|(p, _)| p.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn remove_drops_peer() {
        let (mut p, _clock) = ro();
        p.upsert(PeerId(1), b"x".to_vec());
        assert!(p.remove(PeerId(1)));
        assert!(!p.remove(PeerId(1)));
    }

    #[test]
    fn roster_codec_roundtrip() {
        let roster = vec![
            (PeerId(1), b"alice".to_vec()),
            (PeerId(2), vec![]),
            (PeerId(7), b"{\"c\":3}".to_vec()),
        ];
        let bytes = encode_roster(&roster);
        let back = decode_roster(&bytes).unwrap();
        assert_eq!(back, roster);
    }

    #[test]
    fn roster_codec_rejects_truncated() {
        assert!(decode_roster(&[]).is_err());
        assert!(decode_roster(&[0, 0, 0, 1]).is_err());
    }
}
