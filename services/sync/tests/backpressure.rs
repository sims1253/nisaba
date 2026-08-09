//! Backpressure handling: a peer that cannot drain its outbound channel is
//! disconnected explicitly with a resync-required signal, while the authority and
//! op log retain the update so a reconnect catches up.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::SimPeer;
use nisaba_sync::protocol::Frame;
use nisaba_sync::{
    CLOSE_NORMAL, CLOSE_RESYNC_REQUIRED, DocId, DocRoom, PeerId, Role, close_signal,
};
use tokio::sync::mpsc;

#[tokio::test]
async fn slow_peer_is_evicted_with_resync_and_catches_up_on_reconnect() {
    let room: Arc<DocRoom> = Arc::new(DocRoom::in_memory(DocId::new("bp").unwrap()).await.unwrap());

    // Peer A: a well-behaved author that drains its channel.
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.drain();

    // Peer B: admitted with a deliberately tiny (capacity-2) channel that we never
    // drain, so the welcome + join-roster fill it exactly and the very next
    // fanned-out update cannot be delivered.
    let (btx, mut brx) = mpsc::channel::<Frame>(2);
    let bclose = close_signal(CLOSE_NORMAL);
    room.join(
        PeerId(2),
        Role::Author,
        &[],
        Vec::new(),
        btx,
        bclose.clone(),
    )
    .expect("B joins");
    assert_eq!(room.session_count(), 2);

    // A edits and submits. Fan-out to B's full channel must evict B rather than
    // silently dropping the update.
    a.insert(0, "hello");
    a.submit(&room).await;
    a.drain(); // A observes B's departure in the roster.

    // B was disconnected with the resync-required close code, not dropped silently.
    assert_eq!(room.session_count(), 1, "slow peer evicted");
    assert_eq!(
        bclose.load(Ordering::Acquire),
        CLOSE_RESYNC_REQUIRED,
        "evicted peer must be told to resync"
    );
    // B's receiver is closed once the buffered frames are consumed.
    while brx.try_recv().is_ok() {}
    assert!(matches!(
        brx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));

    // The update survived in the authority (and the op log) — it was never dropped.
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        "hello"
    );

    // B reconnects fresh (empty vv → snapshot catch-up) and converges.
    let mut b2 = SimPeer::new(2, Role::Author);
    b2.connect(&room, &[]).await;
    assert_eq!(b2.text(), "hello");
    assert_eq!(
        b2.text(),
        room.authority().inner().get_text("text").to_string()
    );
}

#[tokio::test]
async fn backpressure_does_not_drop_update_for_other_peers() {
    // When B is evicted for being slow, A and C (both healthy) still receive the
    // update — backpressure on one peer must not stall or drop delivery to others.
    let room: Arc<DocRoom> = Arc::new(
        DocRoom::in_memory(DocId::new("bp2").unwrap())
            .await
            .unwrap(),
    );

    let mut a = SimPeer::new(1, Role::Author);
    let mut c = SimPeer::new(3, Role::Author);
    a.connect(&room, &[]).await;
    c.connect(&room, &[]).await;
    a.drain();
    c.drain();

    // Slow B (capacity 2, never drained).
    let (btx, mut brx) = mpsc::channel::<Frame>(2);
    let bclose = close_signal(CLOSE_NORMAL);
    room.join(
        PeerId(2),
        Role::Author,
        &[],
        Vec::new(),
        btx,
        bclose.clone(),
    )
    .expect("B joins");
    a.drain();
    c.drain();

    a.insert(0, "shared-edit");
    a.submit(&room).await;

    // B evicted, but C still received the update.
    assert_eq!(room.session_count(), 2);
    assert_eq!(bclose.load(Ordering::Acquire), CLOSE_RESYNC_REQUIRED);
    while brx.try_recv().is_ok() {}

    c.drain();
    assert_eq!(c.text(), "shared-edit");
}
