//! Convergence: three replicas editing one document through the room converge
//! to identical state, and the authority is the single source of truth.
//!
//! Exercises three clients editing one
//! document converge".

mod common;

use common::SimPeer;
use nisaba_sync::Role;

#[tokio::test]
async fn three_peers_converge() {
    let room = common::room("conv").await;
    let mut a = SimPeer::new(1, Role::Author);
    let mut b = SimPeer::new(2, Role::Reviewer);
    let mut c = SimPeer::new(3, Role::Author);
    a.connect(&room, &[]).await;
    b.connect(&room, &[]).await;
    c.connect(&room, &[]).await;

    // Peer A seeds the document, then everyone drains to converge on the seed.
    a.insert(0, "Hello");
    a.submit(&room).await;
    b.drain();
    c.drain();
    a.drain();
    assert_eq!(a.text(), "Hello");
    assert_eq!(b.text(), "Hello");
    assert_eq!(c.text(), "Hello");

    // Concurrent edits from different peers at different positions.
    a.insert(5, " world");
    b.insert(0, ">> ");
    c.insert(0, "!!! ");
    a.submit(&room).await;
    b.submit(&room).await;
    c.submit(&room).await;
    for p in [&mut a, &mut b, &mut c] {
        p.drain();
    }

    // CRDT guarantee: all replicas (and the authority) agree byte-for-byte.
    assert_eq!(a.text(), b.text());
    assert_eq!(b.text(), c.text());
    assert_eq!(
        a.text(),
        room.authority().inner().get_text("text").to_string()
    );
    assert_eq!(a.doc.oplog_vv(), room.authority().inner().oplog_vv());
}

#[tokio::test]
async fn interleaved_inserts_and_deletes_converge() {
    let room = common::room("mix").await;
    let mut a = SimPeer::new(10, Role::Author);
    let mut b = SimPeer::new(20, Role::Author);
    a.connect(&room, &[]).await;
    b.connect(&room, &[]).await;

    a.insert(0, "ABCDEFGH");
    a.submit(&room).await;
    b.drain();

    // A deletes a span while B inserts concurrently.
    a.delete(2, 3); // remove "CDE"
    b.insert(8, "XYZ");
    a.submit(&room).await;
    b.submit(&room).await;
    a.drain();
    b.drain();

    assert_eq!(a.text(), b.text());
    assert_eq!(
        a.text(),
        room.authority().inner().get_text("text").to_string()
    );
}

#[tokio::test]
async fn no_op_update_is_not_relayed() {
    // An update the authority already has should not be fanned out (avoids
    // echo storms). We verify by checking the room does not error and replicas
    // stay converged.
    let room = common::room("noop").await;
    let mut a = SimPeer::new(1, Role::Author);
    let mut b = SimPeer::new(2, Role::Author);
    a.connect(&room, &[]).await;
    b.connect(&room, &[]).await;

    a.insert(0, "seed");
    a.submit(&room).await;
    b.drain();
    a.drain();

    // Re-submit the exact same captured update (already applied). The room must
    // treat it as a no-op and not fan it out, so B's queue stays empty.
    let dup = a.snapshot();
    room.handle_update(a.peer, a.role, &dup).await.unwrap();
    assert!(b.rx.try_recv().is_err());
    assert_eq!(a.text(), b.text());
}
