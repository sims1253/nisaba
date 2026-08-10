//! Review-layer policy for reviewer pushes (the QA-reported "reviewer
//! overwrites the baseline via WebSocket").
//!
//! The transport gate enforces: a reviewer update that changes the `text`
//! container is only accepted when (a) the room is empty and the resulting
//! text matches the app's authoritative body (seed verifier), or (b) the
//! update (or a recent update from the same peer) also changed the `review`
//! container — the web client emits suggestion records and the text they
//! annotate as separate frames.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::SimPeer;
use nisaba_sync::config::DocId;
use nisaba_sync::{
    DocRoom, MemoryOpLogStore, MemorySnapshotStore, Role, SeedVerifier, SyncError, SystemClock,
};

/// A scriptable seed verifier for the tests.
struct FakeSeedVerifier {
    /// Bodies the verifier accepts.
    allow: Vec<String>,
    /// Whether to fail closed (Err) instead of returning false.
    fail_closed: bool,
}

#[async_trait]
impl SeedVerifier for FakeSeedVerifier {
    async fn verify(&self, _doc: &DocId, text: &str) -> Result<bool, String> {
        if self.fail_closed {
            return Err("app unreachable".into());
        }
        Ok(self.allow.iter().any(|allowed| allowed == text))
    }
}

async fn room_with_verifier(allow: Vec<String>, fail_closed: bool) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::open(
            DocId::new("policy").unwrap(),
            Arc::new(MemoryOpLogStore::default()),
            Arc::new(MemorySnapshotStore::default()),
            Arc::new(nisaba_sync::Config::default()),
            Arc::new(SystemClock),
            Arc::new(FakeSeedVerifier { allow, fail_closed }),
        )
        .await
        .unwrap(),
    )
}

#[tokio::test]
async fn reviewer_seed_matching_app_body_is_accepted() {
    let room = room_with_verifier(vec!["= Seed\n".to_string()], false).await;
    let mut rev = SimPeer::new(1, Role::Reviewer);
    rev.connect(&room, &[]).await;
    // The web client seeds a fresh room with the body it loaded from the app.
    rev.insert(0, "= Seed\n");
    rev.submit(&room).await;
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        "= Seed\n"
    );
}

#[tokio::test]
async fn reviewer_seed_mismatching_app_body_is_rejected() {
    let room = room_with_verifier(vec!["the real body".to_string()], false).await;
    let mut rev = SimPeer::new(1, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.insert(0, "REVIEWER OVERWROTE THE BASELINE VIA SYNC");
    let err = rev
        .submit_result(&room)
        .await
        .expect_err("mismatched seed must be rejected");
    assert!(matches!(err, SyncError::ReviewPolicy(_)), "{err:?}");
    assert_eq!(room.authority().inner().get_text("text").to_string(), "");
}

#[tokio::test]
async fn reviewer_seed_denied_when_verifier_unreachable() {
    // Fail-closed: an app outage must not open the door for arbitrary seeds.
    let room = room_with_verifier(vec![], true).await;
    let mut rev = SimPeer::new(1, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.insert(0, "anything");
    let err = rev
        .submit_result(&room)
        .await
        .expect_err("verifier outage must deny");
    assert!(matches!(err, SyncError::ReviewPolicy(_)), "{err:?}");
}

#[tokio::test]
async fn reviewer_text_overwrite_of_existing_room_is_rejected() {
    let room = room_with_verifier(vec![], false).await;
    // An author seeds the room first.
    let mut author = SimPeer::new(2, Role::Author);
    author.connect(&room, &[]).await;
    author.insert(0, "= Real baseline\n");
    author.submit(&room).await;

    // A reviewer then tries to replace the text with a raw update (the QA
    // repro): no review record anywhere → rejected.
    let mut rev = SimPeer::new(3, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.drain();
    rev.doc.get_text("text").delete(0, 15).unwrap();
    rev.doc
        .get_text("text")
        .insert(0, "REVIEWER OVERWROTE")
        .unwrap();
    rev.doc.commit();
    let err = rev
        .submit_result(&room)
        .await
        .expect_err("text overwrite must be rejected");
    assert!(matches!(err, SyncError::ReviewPolicy(_)), "{err:?}");
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        "= Real baseline\n"
    );
}

#[tokio::test]
async fn reviewer_combined_text_and_review_update_is_accepted() {
    // The web client's suggesting flow can land the text frame and the review
    // record in one transaction (e.g. bulk reject). Both containers change in
    // the same update → accepted.
    let room = room_with_verifier(vec![], false).await;
    let mut author = SimPeer::new(2, Role::Author);
    author.connect(&room, &[]).await;
    author.insert(0, "base");
    author.submit(&room).await;

    let mut rev = SimPeer::new(3, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.drain();
    rev.doc.get_text("text").insert(4, " suggested").unwrap();
    rev.set_review(r#"[{"id":"r1","kind":"suggestion","change":"insert","text":" suggested","status":"open"}]"#);
    // set_review commits; the text insert above is in the same auto-commit
    // transaction, so the first captured update carries both changes.
    rev.submit_first(&room)
        .await
        .expect("combined update accepted");
    assert_eq!(
        room.authority().inner().get_text("text").to_string(),
        "base suggested"
    );
}

#[tokio::test]
async fn reviewer_review_only_update_is_accepted() {
    let room = room_with_verifier(vec![], false).await;
    let mut rev = SimPeer::new(1, Role::Reviewer);
    rev.connect(&room, &[]).await;
    rev.set_review(r"[]");
    rev.submit_result(&room)
        .await
        .expect("review-only update accepted");
    assert!(rev.review_items().is_some());
}
