//! Persistence boundary: the op log + snapshot store reconstruct the authority
//! document after a restart — the "op log → snapshots" contract; the
//! filesystem stores stand in for the S3-compatible blob boundary.

mod common;

use std::sync::Arc;

use common::SimPeer;
use nisaba_sync::{Config, DocId, DocRoom, FsOpLogStore, FsSnapshotStore, Role, SystemClock};
use tempfile::tempdir;

async fn open(
    doc: &str,
    oplog_dir: &std::path::Path,
    snap_dir: &std::path::Path,
    snapshot_every: u64,
) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::open(
            DocId::new(doc).unwrap(),
            Arc::new(FsOpLogStore::new(oplog_dir).unwrap()),
            Arc::new(FsSnapshotStore::new(snap_dir).unwrap()),
            Arc::new(Config {
                snapshot_every_updates: snapshot_every,
                ..Config::default()
            }),
            Arc::new(SystemClock),
            Arc::new(nisaba_sync::DenyAllSeedVerifier),
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn snapshot_then_reopen_restores_state() {
    let oplog = tempdir().unwrap();
    let snap = tempdir().unwrap();

    let room = open("doc", oplog.path(), snap.path(), 1_000_000).await;
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.insert(0, "persisted state");
    a.submit(&room).await;
    a.drain();

    // Force a snapshot, then drop the in-memory room (simulate restart).
    room.snapshot_now().await.unwrap();
    let expected = a.text();
    drop(room);

    let room2 = open("doc", oplog.path(), snap.path(), 1_000_000).await;
    assert_eq!(
        room2.authority().inner().get_text("text").to_string(),
        expected
    );
}

#[tokio::test]
async fn op_log_replays_edits_after_snapshot() {
    let oplog = tempdir().unwrap();
    let snap = tempdir().unwrap();

    // Snapshot every single update so a snapshot exists after the first edit.
    let room = open("doc", oplog.path(), snap.path(), 1).await;
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.insert(0, "first");
    a.submit(&room).await;
    a.drain();
    // A snapshot is now persisted (snapshot_every_updates = 1).
    a.insert(5, " second");
    a.submit(&room).await;
    a.drain();
    a.insert(0, "pre ");
    a.submit(&room).await;
    a.drain();

    let expected = a.text();
    drop(room);

    // Reopen: load latest snapshot, then replay the append-only op log. Re-importing
    // already-applied ops is a no-op in Loro, so the replay is correct even though
    // the log was not truncated at snapshot time.
    let room2 = open("doc", oplog.path(), snap.path(), 1_000_000).await;
    assert_eq!(
        room2.authority().inner().get_text("text").to_string(),
        expected
    );
}

#[tokio::test]
async fn fresh_doc_has_no_snapshot() {
    let oplog = tempdir().unwrap();
    let snap = tempdir().unwrap();
    let room = open("fresh", oplog.path(), snap.path(), 1_000_000).await;
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        String::new()
    );
}

#[tokio::test]
async fn duplicate_retries_do_not_grow_the_durable_log() {
    use nisaba_sync::{MemoryOpLogStore, MemorySnapshotStore, OpLogStore};
    let log = Arc::new(MemoryOpLogStore::default());
    let id = DocId::new("retries").unwrap();
    let room = Arc::new(
        DocRoom::open(
            id.clone(),
            log.clone(),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(nisaba_sync::DenyAllSeedVerifier),
        )
        .await
        .unwrap(),
    );
    let mut peer = SimPeer::new(1, Role::Author);
    peer.connect(&room, &[]).await;
    peer.insert(0, "persist once");
    let update = peer.captured_updates().remove(0);
    let (a, b) = tokio::join!(
        room.handle_update(peer.peer, peer.role, &update),
        room.handle_update(peer.peer, peer.role, &update)
    );
    a.unwrap();
    b.unwrap();
    room.handle_update(peer.peer, peer.role, &update)
        .await
        .unwrap();
    assert_eq!(log.len(&id).await.unwrap(), 1);
}

#[tokio::test]
async fn updates_waiting_for_dependencies_are_still_durable() {
    use nisaba_sync::{MemoryOpLogStore, MemorySnapshotStore, OpLogStore};
    let log = Arc::new(MemoryOpLogStore::default());
    let snapshots = Arc::new(MemorySnapshotStore::default());
    let id = DocId::new("dependencies").unwrap();
    let room = Arc::new(
        DocRoom::open(
            id.clone(),
            log.clone(),
            snapshots.clone(),
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(nisaba_sync::DenyAllSeedVerifier),
        )
        .await
        .unwrap(),
    );
    let mut peer = SimPeer::new(1, Role::Author);
    peer.connect(&room, &[]).await;
    peer.insert(0, "first");
    peer.insert(5, " second");
    let updates = peer.captured_updates();
    room.handle_update(peer.peer, peer.role, &updates[1])
        .await
        .unwrap();
    assert_eq!(log.len(&id).await.unwrap(), 1);
    drop(room);
    let room = DocRoom::open(
        id.clone(),
        log.clone(),
        snapshots,
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        Arc::new(nisaba_sync::DenyAllSeedVerifier),
    )
    .await
    .unwrap();
    room.handle_update(peer.peer, peer.role, &updates[0])
        .await
        .unwrap();
    let restored = loro::LoroDoc::new();
    restored
        .import(&room.export_state().unwrap().unwrap())
        .unwrap();
    assert_eq!(restored.get_text("text").to_string(), "first second");
}
