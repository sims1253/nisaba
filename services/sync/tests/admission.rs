//! Atomic peer admission and stale-leave fencing.
//!
//! These cover the two correctness invariants added to [`DocRoom`]:
//!
//! * **Atomic admission under the gate** — capacity and duplicate-peer checks
//!   happen together with the insert, so concurrent joins can neither exceed the
//!   peer cap nor both admit the same peer.
//! * **Generation-fenced leave** — a stale leave (a late close for a session
//!   whose peer id has since been re-admitted) must not evict the replacement.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use nisaba_sync::protocol::Frame;
use nisaba_sync::{
    CLOSE_NORMAL, Config, DocId, DocRoom, MemoryOpLogStore, MemorySnapshotStore, PeerId, Role,
    SyncError, SystemClock, close_signal,
};
use tokio::sync::mpsc;

async fn room_with(config: Config) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::open(
            DocId::new("admission").unwrap(),
            Arc::new(MemoryOpLogStore::default()),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(config),
            Arc::new(SystemClock),
            Arc::new(nisaba_sync::DenyAllSeedVerifier),
        )
        .await
        .unwrap(),
    )
}

/// One join attempt result for the concurrent tests.
#[derive(Debug)]
#[allow(dead_code)]
enum Attempt {
    Admitted,
    Handshake,
    Limit,
    Other(SyncError),
}

/// Try to admit `peer`. The returned receiver must be kept alive until every
/// contender has been measured — otherwise the room (correctly) evicts the peer
/// on the next fan-out because its channel is closed, which would defeat the test.
fn attempt(room: &Arc<DocRoom>, peer: PeerId) -> (Attempt, mpsc::Receiver<Frame>) {
    // A large capacity so admission-broadcast rosters never cause backpressure
    // eviction during the test (the thing under test is the cap, not backpressure).
    let (tx, rx) = mpsc::channel::<Frame>(256);
    let close = close_signal(CLOSE_NORMAL);
    let res = match room.join(peer, Role::Author, &[], Vec::new(), tx, close) {
        Ok(_) => Attempt::Admitted,
        Err(SyncError::Handshake(_)) => Attempt::Handshake,
        Err(SyncError::Limit(_)) => Attempt::Limit,
        Err(e) => Attempt::Other(e),
    };
    (res, rx)
}

#[tokio::test]
async fn concurrent_joins_for_same_peer_admit_exactly_once() {
    // Many threads race to admit peer 7. With atomic admission exactly one must
    // win and the rest must get a Handshake error — no two threads can both
    // believe they admitted the peer.
    const N: usize = 16;
    let room = room_with(Config::default()).await;
    let start = Arc::new(Barrier::new(N));
    let hold = Arc::new(Barrier::new(N));
    let admitted = Arc::new(AtomicU64::new(0));
    let handshake = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let room = Arc::clone(&room);
        let start = Arc::clone(&start);
        let hold = Arc::clone(&hold);
        let admitted = Arc::clone(&admitted);
        let handshake = Arc::clone(&handshake);
        handles.push(thread::spawn(move || {
            start.wait();
            let (res, rx) = attempt(&room, PeerId(7));
            match res {
                Attempt::Admitted => {
                    admitted.fetch_add(1, Ordering::Relaxed);
                }
                Attempt::Handshake => {
                    handshake.fetch_add(1, Ordering::Relaxed);
                }
                other => panic!("unexpected outcome for duplicate peer: {other:?}"),
            }
            // Keep this peer's receiver alive until every contender has joined,
            // so the room does not evict the single admitted peer mid-race.
            hold.wait();
            drop(rx);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(admitted.load(Ordering::Relaxed), 1, "exactly one admission");
    assert_eq!(
        handshake.load(Ordering::Relaxed),
        u64::try_from(N - 1).unwrap(),
        "the rest are rejected as duplicates"
    );
    assert_eq!(room.session_count(), 1);
}

#[tokio::test]
async fn concurrent_joins_respect_peer_cap() {
    // Cap of 4; 10 distinct peers race to join. Exactly 4 must be admitted and
    // the room must never transiently hold more than the cap.
    const CAP: usize = 4;
    const CONTENDERS: usize = 10;
    let room = room_with(Config {
        max_peers_per_doc: CAP,
        ..Config::default()
    })
    .await;
    let start = Arc::new(Barrier::new(CONTENDERS));
    let hold = Arc::new(Barrier::new(CONTENDERS));
    let admitted = Arc::new(AtomicU64::new(0));
    let limited = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::with_capacity(CONTENDERS);
    for i in 0..CONTENDERS {
        let room = Arc::clone(&room);
        let start = Arc::clone(&start);
        let hold = Arc::clone(&hold);
        let admitted = Arc::clone(&admitted);
        let limited = Arc::clone(&limited);
        handles.push(thread::spawn(move || {
            start.wait();
            // Distinct, non-zero peer ids.
            let (res, rx) = attempt(&room, PeerId(u64::try_from(i + 1).unwrap()));
            match res {
                Attempt::Admitted => {
                    admitted.fetch_add(1, Ordering::Relaxed);
                }
                Attempt::Limit => {
                    limited.fetch_add(1, Ordering::Relaxed);
                }
                other => panic!("unexpected outcome under cap: {other:?}"),
            }
            // Keep admitted peers' receivers alive until every contender has been
            // measured, so none are evicted for a closed channel mid-race.
            hold.wait();
            drop(rx);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        admitted.load(Ordering::Relaxed),
        u64::try_from(CAP).unwrap(),
        "cap is an absolute ceiling even under contention"
    );
    assert_eq!(
        limited.load(Ordering::Relaxed),
        u64::try_from(CONTENDERS - CAP).unwrap()
    );
    assert_eq!(room.session_count(), CAP);
}

#[tokio::test]
async fn stale_leave_does_not_evict_replacement() {
    // The core fencing property: after a peer is re-admitted with a new
    // generation, a leave carrying the *old* generation is a no-op and must not
    // remove the replacement.
    let room = room_with(Config::default()).await;

    // First admission (generation 1). Keep the receiver alive so the room does
    // not evict this peer on the next join's fan-out.
    let (tx1, rx1) = mpsc::channel::<Frame>(8);
    let close1 = close_signal(CLOSE_NORMAL);
    let first = room
        .join(PeerId(7), Role::Author, &[], Vec::new(), tx1, close1)
        .unwrap();
    assert_eq!(first.generation, 1);
    assert_eq!(room.session_count(), 1);

    // Voluntary leave with the matching generation removes it.
    drop(rx1);
    assert!(room.leave(PeerId(7), first.generation));
    assert_eq!(room.session_count(), 0);

    // Re-admission gets a fresh generation (2).
    let (tx2, rx2) = mpsc::channel::<Frame>(8);
    let close2 = close_signal(CLOSE_NORMAL);
    let second = room
        .join(PeerId(7), Role::Author, &[], Vec::new(), tx2, close2)
        .unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(room.session_count(), 1);

    // A stale leave for generation 1 must NOT touch the replacement.
    assert!(
        !room.leave(PeerId(7), first.generation),
        "stale leave must be a no-op"
    );
    assert_eq!(
        room.session_count(),
        1,
        "replacement must survive the stale leave"
    );

    // The current generation's leave does remove it.
    drop(rx2);
    assert!(room.leave(PeerId(7), second.generation));
    assert_eq!(room.session_count(), 0);
}

#[tokio::test]
async fn leave_for_unknown_peer_is_noop() {
    let room = room_with(Config::default()).await;
    // No session was ever admitted for peer 9; leaving must not panic or insert.
    assert!(!room.leave(PeerId(9), 1));
    assert_eq!(room.session_count(), 0);
}
