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
