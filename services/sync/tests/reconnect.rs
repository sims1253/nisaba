//! Reconnect catch-up: a peer that was offline converges using its last version
//! vector, and a brand-new peer bootstraps from a snapshot.
//!
//! Exercises a client that goes offline, edits, and
//! merges cleanly on reconnect".

mod common;

use common::SimPeer;
use nisaba_sync::Role;
use nisaba_sync::protocol::{CatchUp, Frame};

#[tokio::test]
async fn offline_peer_catches_up_on_reconnect() {
    let room = common::room("reconnect").await;

    // Two peers online; A seeds the document.
    let mut a = SimPeer::new(1, Role::Author);
    let mut b = SimPeer::new(2, Role::Reviewer);
    a.connect(&room, &[]).await;
    b.connect(&room, &[]).await;
    a.insert(0, "base");
    a.submit(&room).await;
    b.drain();
    a.drain();
    assert_eq!(b.text(), "base");

    // Peer B "goes offline": record its last version vector, then leave the room
    // (but keep the replica — a real offline client keeps its local state).
    let b_vv = b.vv_bytes();
    b.leave_room(&room);

    // While B is offline, A and a new peer C make several edits through the room.
    let mut c = SimPeer::new(3, Role::Author);
    c.connect(&room, &[]).await;
    c.drain(); // C picks up "base"
    a.drain(); // A observes C's join roster
    a.insert(4, "-A");
    a.submit(&room).await;
    c.drain();
    c.insert(0, "C:");
    c.submit(&room).await;
    a.drain();

    // B reconnects with its stale version vector; its replica still has "base",
    // so the incremental catch-up applies cleanly on top.
    b.connect(&room, &b_vv).await;
    b.drain();

    assert_eq!(b.text(), a.text());
    assert_eq!(
        b.text(),
        room.authority().inner().get_text("text").to_string()
    );
    assert_eq!(b.doc.oplog_vv(), room.authority().inner().oplog_vv());
}

#[tokio::test]
async fn fresh_peer_bootstraps_from_snapshot() {
    // A document with existing history; a brand-new peer (empty vv) must receive
    // a full snapshot rather than an incremental update.
    let room = common::room("bootstrap").await;
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.insert(0, "the quick brown fox");
    a.submit(&room).await;
    a.drain();

    let mut fresh = SimPeer::new(99, Role::ReadOnly);
    fresh.connect(&room, &[]).await; // empty vv → snapshot catch-up
    assert_eq!(fresh.text(), "the quick brown fox");
    assert_eq!(fresh.doc.oplog_vv(), room.authority().inner().oplog_vv());
}

#[tokio::test]
async fn already_current_peer_gets_no_catchup() {
    let room = common::room("current").await;
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.insert(0, "hi");
    a.submit(&room).await;
    a.drain();

    // A reconnects with its current vv — the welcome must report no catch-up.
    // Leave first, since the peer is still connected.
    a.leave_room(&room);
    let vv = a.vv_bytes();
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let close = nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL);
    room.join(a.peer, Role::Author, &vv, Vec::new(), tx, close)
        .expect("join");
    let welcome = rx.recv().await.expect("welcome");
    let Frame::Welcome { catchup, .. } = welcome else {
        panic!("expected welcome");
    };
    assert!(matches!(catchup, CatchUp::None), "expected no catch-up");
}
