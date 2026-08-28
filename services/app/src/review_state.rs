//! Review-state bridge: sync snapshots → compile marks.
//!
//! Review state (suggestions, comments, accept/reject records) is durable and
//! replicated in each document's Loro CRDT — the web client persists one JSON
//! item per entry of the `review` map container (web/src/review-persistence.ts
//! is the canonical layout) and the sync service relays it like any other CRDT
//! state. The app service stores only the document *body*, so any server-side
//! consumer of review marks must read the CRDT back from sync.
//!
//! This module is the app service's only Loro dependency. `nisaba-core` is
//! deliberately CRDT-independent (its marks are expressed over plain offsets),
//! so the translation from CRDT-stored review items to [`MarkInput`]s lives
//! here, in the HTTP service that already owns the compile/export contracts.
//!
//! Semantics mirror the web compile path exactly (web/src/main.ts,
//! `collectOpenSuggestionMarks`): only `kind === "suggestion"` items that are
//! `status === "open"` and not orphaned become marks; Loro cursors are
//! resolved to current offsets with a fallback to the stored raw offsets; the
//! end offset is clamped to the document length; each mark keeps the item's
//! author and creation timestamp.

use async_trait::async_trait;
use base64::Engine as _;
use loro::LoroDoc;
use loro::cursor::Cursor;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

use crate::types::{AppError, MarkInput};

/// The CRDT container keys. Bound to the web client's layout
/// (web/src/review-persistence.ts; the sync service's authority.rs documents
/// the same keys).
const TEXT_CONTAINER: &str = "text";
const REVIEW_CONTAINER: &str = "review";

/// One persisted review item, as written by the web client into the `review`
/// map container (see web/src/review.ts for the authoritative shape).
///
/// Every field beyond `kind` is optional on the wire: the item JSON is
/// client-authored, and an unexpected/corrupt entry must be skipped, never
/// crash the export.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewItemWire {
    kind: String,
    from: u32,
    to: u32,
    change: Option<String>,
    author: Option<String>,
    status: Option<String>,
    orphaned: Option<bool>,
    created_at: Option<u64>,
    /// Base64-encoded Loro cursor anchoring `from`.
    from_cursor: Option<String>,
    /// Base64-encoded Loro cursor anchoring `to`.
    to_cursor: Option<String>,
}

/// Extract the compile marks for a document from its whole CRDT state.
///
/// `snapshot` is the opaque snapshot bytes served by the sync service's
/// internal read API. Returns the marks in the same semantics the web client
/// sends with an interactive compile; an empty vector is normal (no review
/// items, or none that are open suggestions). Corrupt individual entries are
/// skipped, mirroring the web reader; a snapshot that cannot be imported at
/// all is an `Err` (infrastructure fault, surfaced by the caller as a
/// dependency error).
pub fn review_marks_from_snapshot(snapshot: &[u8]) -> Result<Vec<MarkInput>, String> {
    let doc = LoroDoc::new();
    doc.import(snapshot)
        .map_err(|error| format!("snapshot does not import: {error}"))?;
    // The length marks are clamped against: the CRDT text container is the
    // document the cursors were taken over (the editor's source of truth).
    let text_len =
        u32::try_from(doc.get_text(TEXT_CONTAINER).to_string().chars().count()).unwrap_or(u32::MAX);

    let mut marks = Vec::new();
    doc.get_map(REVIEW_CONTAINER).for_each(|_key, value| {
        // Entries are JSON strings; a child container (or any other value
        // type) is a foreign/corrupt entry — skip it.
        let raw = match value.into_value() {
            Ok(loro::LoroValue::String(s)) => s.as_str().to_string(),
            _ => return,
        };
        let Ok(item) = serde_json::from_str::<ReviewItemWire>(&raw) else {
            return; // corrupt entry: skip rather than fail the whole read
        };
        if let Some(mark) = mark_from_item(&doc, &item, text_len) {
            marks.push(mark);
        }
    });
    // Deterministic order regardless of Loro map iteration order.
    marks.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.end.cmp(&b.end))
            .then_with(|| a.timestamp.cmp(&b.timestamp))
    });
    Ok(marks)
}

/// Convert one persisted review item into a mark, applying exactly the web
/// compile path's semantics (`collectOpenSuggestionMarks`):
///
/// * only `kind === "suggestion"` with `status === "open"` and not flagged
///   `orphaned` (the web filter; comments never affect projection and
///   resolved items are done). Like the web, the persisted `orphaned` flag is
///   trusted as-is — the web client re-derives it on every edit but only
///   re-persists it alongside the next review write, so the flag can lag the
///   CRDT by design;
/// * cursors resolve to current offsets, falling back to the raw offsets when
///   a cursor is absent, undecodable, or unresolvable (`resolveCursor(x) ??
///   offset`);
/// * `end` is clamped to the document length (the web builder's
///   `Math.min(..., docLength)` guard against stale positions).
fn mark_from_item(doc: &LoroDoc, item: &ReviewItemWire, text_len: u32) -> Option<MarkInput> {
    if item.kind != "suggestion" || item.status.as_deref() != Some("open") {
        return None;
    }
    if item.orphaned == Some(true) {
        return None;
    }
    let kind = item.change.as_deref()?;
    if kind != "insert" && kind != "delete" {
        return None; // a suggestion must be an insert or a delete
    }
    let start = item
        .from_cursor
        .as_deref()
        .and_then(|encoded| resolve_cursor(doc, encoded))
        .unwrap_or(item.from);
    let end = item
        .to_cursor
        .as_deref()
        .and_then(|encoded| resolve_cursor(doc, encoded))
        .unwrap_or(item.to)
        .min(text_len);
    Some(MarkInput {
        id: None,
        start,
        end,
        kind: kind.to_owned(),
        author: item.author.clone().unwrap_or_default(),
        // The item's own creation time — provenance of the suggestion, not the
        // moment of the export.
        timestamp: item.created_at.unwrap_or_default(),
    })
}

/// Resolve a base64-encoded Loro cursor to its current offset, or `None` when
/// it cannot be resolved (absent/undecodable/orphaned) — the caller falls back
/// to the stored raw offset, mirroring the web `resolveCursor(x) ?? offset`.
fn resolve_cursor(doc: &LoroDoc, encoded: &str) -> Option<u32> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let cursor = Cursor::decode(&bytes).ok()?;
    let pos = doc.get_cursor_pos(&cursor).ok()?;
    u32::try_from(pos.current.pos).ok()
}

/// Client for the sync service's internal whole-state read API
/// (`GET /internal/docs/{doc_id}/state`), authenticated with the shared
/// service machine credential (`NISABA_SYNC_AUTHZ_TOKEN`) exactly like the
/// other app↔sync hop.
#[async_trait]
pub trait SyncStateClient: Send + Sync {
    /// The document's whole current CRDT state as opaque snapshot bytes, or
    /// `None` when sync holds no state for the document (a document nobody
    /// has ever collaborated on).
    ///
    /// # Errors
    /// A transport failure or an unexpected answer is an
    /// [`AppError::Dependency`] — callers fail the operation rather than
    /// degrade to a marks-less result.
    async fn document_state(&self, document_id: Uuid) -> Result<Option<Vec<u8>>, AppError>;
}

/// HTTP adapter for [`SyncStateClient`] against the sync service.
pub struct HttpSyncStateClient {
    client: reqwest::Client,
    endpoint: String,
    internal_token: String,
}

impl HttpSyncStateClient {
    /// `endpoint` is the sync service base URL (e.g. `http://sync:8080`);
    /// `internal_token` is the shared service credential.
    pub fn new(endpoint: impl Into<String>, internal_token: impl Into<String>) -> Self {
        Self {
            // Whole-state snapshots can be large; the bound matches the sync
            // service's own max outbound snapshot size (64 MiB) with headroom
            // for the transport.
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client builder is infallible"),
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            internal_token: internal_token.into(),
        }
    }
}

#[async_trait]
impl SyncStateClient for HttpSyncStateClient {
    async fn document_state(&self, document_id: Uuid) -> Result<Option<Vec<u8>>, AppError> {
        let response = self
            .client
            .get(format!(
                "{}/internal/docs/{}/state",
                self.endpoint, document_id
            ))
            .bearer_auth(&self.internal_token)
            .send()
            .await
            .map_err(|error| {
                AppError::Dependency(format!(
                    "sync state read for document {document_id} failed: {error}"
                ))
            })?;
        match response.status() {
            status if status.is_success() => response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map(Some)
                .map_err(|error| {
                    AppError::Dependency(format!(
                        "sync state read for document {document_id} returned an unreadable body: {error}"
                    ))
                }),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status => {
                let detail = response.text().await.unwrap_or_default();
                Err(AppError::Dependency(format!(
                    "sync state read for document {document_id} returned {status}: {detail}"
                )))
            }
        }
    }
}

/// Fail-closed stand-in used when no sync client is wired: every read is a
/// dependency error, so the export path refuses to run marks-less.
pub struct UnconfiguredSyncState;

#[async_trait]
impl SyncStateClient for UnconfiguredSyncState {
    async fn document_state(&self, _document_id: Uuid) -> Result<Option<Vec<u8>>, AppError> {
        Err(AppError::Dependency(
            "sync state client is not configured".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro::ExportMode;

    /// Build a Loro snapshot in-test: text plus review entries, laid out
    /// exactly like the web client writes them (one JSON string per item id).
    fn snapshot(text: &str, items: &[serde_json::Value]) -> Vec<u8> {
        let doc = LoroDoc::new();
        doc.set_peer_id(42).unwrap();
        doc.get_text(TEXT_CONTAINER).insert(0, text).unwrap();
        let review = doc.get_map(REVIEW_CONTAINER);
        for item in items {
            review
                .insert(item["id"].as_str().unwrap(), item.to_string())
                .unwrap();
        }
        doc.commit();
        doc.export(ExportMode::Snapshot).unwrap()
    }

    /// A cursor anchored at `offset`, encoded like the web client encodes it
    /// (base64 of the Loro cursor bytes).
    fn cursor_at(doc: &LoroDoc, offset: usize) -> String {
        let cursor = doc
            .get_text(TEXT_CONTAINER)
            .get_cursor(offset, loro::cursor::Side::Left)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(cursor.encode())
    }

    fn marks(text: &str, items: &[serde_json::Value]) -> Vec<MarkInput> {
        review_marks_from_snapshot(&snapshot(text, items)).unwrap()
    }

    #[test]
    fn empty_and_absent_review_states_are_empty_marks() {
        // No review container at all (a plain text document).
        assert!(marks("hello", &[]).is_empty());
        // A review container holding only non-suggestion entries.
        let comment = serde_json::json!({
            "id": "c1", "kind": "comment", "from": 0, "to": 2, "body": "hi",
            "author": "bea", "status": "open", "createdAt": 7
        });
        assert!(marks("hello", &[comment]).is_empty());
    }

    #[test]
    fn only_open_non_orphaned_suggestions_become_marks() {
        let open = serde_json::json!({
            "id": "s1", "kind": "suggestion", "from": 0, "to": 2, "change": "delete",
            "author": "bea", "status": "open", "createdAt": 100
        });
        let accepted = serde_json::json!({
            "id": "s2", "kind": "suggestion", "from": 3, "to": 4, "change": "insert",
            "author": "bea", "status": "accepted", "createdAt": 101
        });
        let rejected = serde_json::json!({
            "id": "s3", "kind": "suggestion", "from": 3, "to": 4, "change": "insert",
            "author": "bea", "status": "rejected", "createdAt": 102
        });
        let orphaned = serde_json::json!({
            "id": "s4", "kind": "suggestion", "from": 1, "to": 2, "change": "insert",
            "author": "bea", "status": "open", "orphaned": true, "createdAt": 103
        });
        let comment = serde_json::json!({
            "id": "c1", "kind": "comment", "from": 1, "to": 2, "body": "hi",
            "author": "bea", "status": "open", "createdAt": 104
        });
        let got = marks("hello", &[open, accepted, rejected, orphaned, comment]);
        assert_eq!(
            got.len(),
            1,
            "only the one open suggestion survives: {got:?}"
        );
        assert_eq!(got[0].start, 0);
        assert_eq!(got[0].end, 2);
        assert_eq!(got[0].kind, "delete");
        assert_eq!(got[0].author, "bea");
        assert_eq!(got[0].timestamp, 100);
    }

    #[test]
    fn cursors_resolve_over_stale_raw_offsets_with_fallback() {
        // Build the snapshot, then keep a live doc over the same text so the
        // cursors encoded in items are valid for it.
        let doc = LoroDoc::new();
        doc.set_peer_id(42).unwrap();
        doc.get_text(TEXT_CONTAINER).insert(0, "hello").unwrap();
        doc.commit();

        let with_cursors = serde_json::json!({
            "id": "s1", "kind": "suggestion", "from": 99, "to": 99,
            "fromCursor": cursor_at(&doc, 1), "toCursor": cursor_at(&doc, 3),
            "change": "delete", "author": "bea", "status": "open", "createdAt": 1
        });
        // The raw from/to (99/99) are stale offsets from a longer revision;
        // the cursors are the authoritative anchors and must win over them.
        let got = marks("hello", &[with_cursors]);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].start, got[0].end), (1, 3));

        let garbage_cursor = serde_json::json!({
            "id": "s2", "kind": "suggestion", "from": 1, "to": 3,
            "fromCursor": "!!!not-base64!!!", "toCursor": "AAAA",
            "change": "insert", "author": "bea", "status": "open", "createdAt": 2
        });
        let got = marks("hello", &[garbage_cursor]);
        assert_eq!(got.len(), 1);
        assert_eq!(
            (got[0].start, got[0].end),
            (1, 3),
            "fallback to raw offsets"
        );
    }

    #[test]
    fn end_is_clamped_to_the_document_length() {
        // A cursor-less item whose stored `to` is past the text length (a
        // stale offset from a longer revision): the end clamps to the length,
        // exactly like the web builder's `Math.min(..., docLength)` guard.
        let stale = serde_json::json!({
            "id": "s1", "kind": "suggestion", "from": 1, "to": 500,
            "change": "delete", "author": "bea", "status": "open", "createdAt": 1
        });
        let got = marks("hello", &[stale]);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].start, got[0].end), (1, 5));
    }

    #[test]
    fn corrupt_entries_are_skipped_not_fatal() {
        let doc = LoroDoc::new();
        doc.get_map(REVIEW_CONTAINER)
            .insert("junk", "not json")
            .unwrap();
        doc.get_map(REVIEW_CONTAINER)
            .insert("not-a-string", 42)
            .unwrap();
        doc.get_text(TEXT_CONTAINER).insert(0, "hello").unwrap();
        doc.commit();
        let bytes = doc.export(ExportMode::Snapshot).unwrap();
        assert!(review_marks_from_snapshot(&bytes).unwrap().is_empty());
    }

    #[test]
    fn undecodable_snapshot_is_an_error() {
        assert!(review_marks_from_snapshot(b"definitely not a loro snapshot").is_err());
    }
}
