//! Presence roster + heartbeat expiry.
//!
//! Uses an injectable manual clock so expiry is deterministic (no sleeps).

mod common;

use std::sync::Arc;
use std::time::Duration;

use nisaba_sync::presence::decode_roster;
use nisaba_sync::protocol::Frame;
use nisaba_sync::time::ManualClock;
use nisaba_sync::{Config, DocId, DocRoom, MemoryOpLogStore, MemorySnapshotStore, PeerId, Role};
use tokio::sync::mpsc;

async fn room_with_clock(clock: Arc<ManualClock>) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::open(
            DocId::new("presence").unwrap(),
            Arc::new(MemoryOpLogStore::default()),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(Config {
                presence_ttl_ms: 100,
                presence_sweep_ms: 10,
                ..Config::default()
            }),
            clock,
        )
        .await
        .unwrap(),
    )
}

/// Join `peer`, returning the receiver and the admission generation (needed to
/// leave without evicting a re-admitted replacement).
fn join(
    room: &Arc<DocRoom>,
    peer: u64,
    role: Role,
    state: Vec<u8>,
) -> (mpsc::Receiver<Frame>, u64) {
    let (tx, rx) = mpsc::channel(64);
    let close = nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL);
    let outcome = room
        .join(PeerId(peer), role, &[], state, tx, close)
        .unwrap();
    (rx, outcome.generation)
}

/// Drain all queued presence frames and return the last roster decoded.
fn roster(rx: &mut mpsc::Receiver<Frame>) -> Vec<(PeerId, Vec<u8>)> {
    let mut last = Vec::new();
    while let Ok(f) = rx.try_recv() {
        if let Frame::Presence(b) = f {
            last = decode_roster(&b).unwrap();
        }
    }
    last
}

#[tokio::test]
async fn join_and_leave_updates_roster() {
    let clock = Arc::new(ManualClock::new());
    let room = room_with_clock(clock.clone()).await;

    let (mut ra, gen_a) = join(&room, 1, Role::Author, b"alice".to_vec());
    assert!(
        roster(&mut ra)
            .iter()
            .any(|(p, s)| p.get() == 1 && s == b"alice")
    );

    let (mut rb, _) = join(&room, 2, Role::Reviewer, b"bob".to_vec());
    let roster_a = roster(&mut ra);
    let roster_b = roster(&mut rb);
    assert_eq!(roster_a.len(), 2);
    assert_eq!(roster_b.len(), 2);

    room.leave(PeerId(1), gen_a);
    let roster_b = roster(&mut rb);
    assert!(roster_b.iter().all(|(p, _)| p.get() != 1));
    assert_eq!(roster_b.len(), 1);
}

#[tokio::test]
async fn heartbeat_keeps_peer_alive_past_ttl() {
    let clock = Arc::new(ManualClock::new());
    let room = room_with_clock(clock.clone()).await;

    let (mut ra, _) = join(&room, 1, Role::Author, b"alice".to_vec());
    let _ = roster(&mut ra);

    // Cross the TTL once: would expire, but we heartbeat first.
    clock.advance(Duration::from_millis(60));
    room.handle_heartbeat(PeerId(1)).unwrap();
    clock.advance(Duration::from_millis(60));
    let evicted = room.sweep_presence();
    assert!(evicted.is_empty(), "heartbeat should have kept peer alive");
    assert_eq!(room.session_count(), 1);
}

#[tokio::test]
async fn sweep_evicts_silent_peer_and_drops_session() {
    let clock = Arc::new(ManualClock::new());
    let room = room_with_clock(clock.clone()).await;

    let (mut ra, _) = join(&room, 1, Role::Author, b"alice".to_vec());
    let _ = roster(&mut ra);
    assert_eq!(room.session_count(), 1);

    // No heartbeat; cross the TTL.
    clock.advance(Duration::from_millis(101));
    let evicted = room.sweep_presence();
    assert_eq!(evicted, vec![PeerId(1)]);
    assert_eq!(room.session_count(), 0);
}

#[tokio::test]
async fn presence_update_broadcasts_new_state() {
    let clock = Arc::new(ManualClock::new());
    let room = room_with_clock(clock.clone()).await;

    let (mut ra, _) = join(&room, 1, Role::Author, vec![]);
    let _ = roster(&mut ra);

    room.handle_presence(PeerId(1), b"editing".to_vec())
        .unwrap();
    let r = roster(&mut ra);
    assert!(r.iter().any(|(p, s)| p.get() == 1 && s == b"editing"));
}
