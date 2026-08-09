//! Limits and the role-aware access seam at the transport layer.
//!
//! Covers: update-size cap, per-document peer cap, and read-only enforcement.
//! These are the security/validation invariants audited in one place
//! (`config.rs` / `auth.rs`).

mod common;

use std::sync::Arc;

use common::SimPeer;
use nisaba_sync::protocol::Frame;
use nisaba_sync::{
    AccessResolver, AuthError, CapabilitySet, Config, DocId, DocRoom, MemoryOpLogStore,
    MemorySnapshotStore, PeerId, Role, StaticAccessResolver, SyncError, SystemClock,
};
use tokio::sync::mpsc;

async fn room_with(config: Config) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::open(
            DocId::new("limits").unwrap(),
            Arc::new(MemoryOpLogStore::default()),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(config),
            Arc::new(SystemClock),
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn update_size_limit_rejects_oversized_blob() {
    let room = room_with(Config {
        max_update_bytes: 8,
        ..Config::default()
    })
    .await;
    let err = room
        .handle_update(PeerId(1), Role::Author, &[0u8; 64])
        .await
        .unwrap_err();
    assert!(matches!(err, SyncError::Limit(_)), "{err:?}");
}

#[tokio::test]
async fn read_only_role_cannot_push_updates() {
    let room = room_with(Config::default()).await;
    let mut ro = SimPeer::new(7, Role::ReadOnly);
    ro.connect(&room, &[]).await;

    ro.insert(0, "forbidden");
    for u in &ro.captured_updates() {
        let err = room
            .handle_update(ro.peer, Role::ReadOnly, u)
            .await
            .unwrap_err();
        assert!(
            matches!(err, SyncError::Access(AuthError::Forbidden { .. })),
            "{err:?}"
        );
    }
    // Nothing was relayed: the authority is still empty.
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        String::new()
    );
}

#[tokio::test]
async fn reviewer_can_push_updates() {
    let room = room_with(Config::default()).await;
    let mut rev = SimPeer::new(8, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.insert(0, "suggestion");
    rev.submit(&room).await;
    rev.drain();
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        "suggestion"
    );
}

#[tokio::test]
async fn peer_cap_rejects_extra_join() {
    let room = room_with(Config {
        max_peers_per_doc: 2,
        ..Config::default()
    })
    .await;
    let mut a = SimPeer::new(1, Role::Author);
    let mut b = SimPeer::new(2, Role::Author);
    a.connect(&room, &[]).await;
    b.connect(&room, &[]).await;
    a.drain();

    let (tx, _rx) = mpsc::channel::<Frame>(8);
    let close = nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL);
    let err = room
        .join(PeerId(3), Role::Author, &[], Vec::new(), tx, close)
        .unwrap_err();
    assert!(matches!(err, SyncError::Limit(_)), "{err:?}");
}

#[tokio::test]
async fn duplicate_peer_rejected() {
    let room = room_with(Config::default()).await;
    let mut a = SimPeer::new(1, Role::Author);
    a.connect(&room, &[]).await;
    a.drain();

    let (tx, _rx) = mpsc::channel::<Frame>(8);
    let close = nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL);
    let err = room
        .join(PeerId(1), Role::Author, &[], Vec::new(), tx, close)
        .unwrap_err();
    assert!(matches!(err, SyncError::Handshake(_)), "{err:?}");
}

#[test]
fn capabilities_distinguish_roles() {
    assert!(
        Role::Author
            .capabilities()
            .contains(CapabilitySet::PUSH_UPDATES)
    );
    assert!(
        Role::Reviewer
            .capabilities()
            .contains(CapabilitySet::PUSH_UPDATES)
    );
    assert!(
        !Role::ReadOnly
            .capabilities()
            .contains(CapabilitySet::PUSH_UPDATES)
    );
    // Every role can receive state and be present.
    for r in [Role::Author, Role::Reviewer, Role::ReadOnly] {
        assert!(r.capabilities().contains(CapabilitySet::RECEIVE_STATE));
        assert!(r.capabilities().contains(CapabilitySet::PRESENCE));
    }
}

#[tokio::test]
async fn static_resolver_denies_without_grant() {
    let r = StaticAccessResolver::new();
    let doc = DocId::new("d").unwrap();
    // No grant → unauthenticated.
    assert!(matches!(
        r.resolve(&doc, "anything").await,
        Err(AuthError::Unauthenticated(_))
    ));
}
