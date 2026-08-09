//! The authoritative Loro replica for one document.
//!
//! [`AuthorityDoc`] is a thin, deep component around [`loro::LoroDoc`]:
//!
//! * **import** — accept opaque update bytes from a client (the relay forwards
//!   exactly those bytes to other peers; it never re-serialises state).
//! * **catch-up** — given a peer's last version vector, produce either the
//!   incremental updates since then or a full snapshot. This is what makes
//!   reconnect-after-offline converge.
//! * **snapshot** — export a full snapshot for the periodic snapshotter.
//!
//! The authority itself never creates local edits, so it has no opinion about the
//! document's structure (text/marks/data layers). That separation is
//! what lets the sync transport stay opaque to Loro state.

use std::borrow::Cow;
use std::sync::Arc;

use loro::{ExportMode, LoroDoc, VersionVector};

use crate::error::SyncResult;
use crate::protocol::CatchUp;

/// Wraps an authoritative [`LoroDoc`] for one document.
///
/// Cloning is cheap: the inner doc lives behind an [`Arc`].
#[derive(Clone)]
pub struct AuthorityDoc {
    doc: Arc<LoroDoc>,
}

impl AuthorityDoc {
    /// Create a fresh, empty authority document. Peer id is left at Loro's
    /// default — the authority produces no ops, so it needs no distinct peer.
    #[must_use]
    pub fn new() -> Self {
        let doc = LoroDoc::new();
        // The authority only imports remote ops; keeping it attached is the
        // correct default and makes `get_text` etc. operate on the live state.
        doc.attach();
        Self { doc: Arc::new(doc) }
    }

    /// Build an authority doc from a previously persisted snapshot.
    pub fn from_snapshot(bytes: &[u8]) -> SyncResult<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        Ok(Self { doc: Arc::new(doc) })
    }

    /// Access the underlying Loro doc. Exposed for tests that need to assert on
    /// CRDT state (e.g. read the `text` container); production code should go
    /// through the methods on this type.
    #[must_use]
    pub fn inner(&self) -> &LoroDoc {
        &self.doc
    }

    /// Import opaque update bytes produced by some peer's local edits.
    ///
    /// Returns the resulting authority version vector (the oplog VV), which the
    /// caller can use for snapshot thresholds.
    pub fn import_update(&self, bytes: &[u8]) -> SyncResult<VersionVector> {
        self.doc.import(bytes)?;
        Ok(self.doc.oplog_vv())
    }

    /// The authority's current version vector.
    #[must_use]
    pub fn version_vector(&self) -> VersionVector {
        self.doc.oplog_vv()
    }

    /// Produce the bytes a reconnecting peer needs to reach the current state.
    ///
    /// `peer_vv` is the peer's last-known version vector (encoded with
    /// [`VersionVector::encode`]); pass an empty slice to request a full snapshot
    /// (e.g. a brand-new peer).
    pub fn catchup(&self, peer_vv: &[u8]) -> SyncResult<CatchUp> {
        if peer_vv.is_empty() {
            return Ok(CatchUp::Snapshot(self.export_snapshot()?));
        }
        let from = VersionVector::decode(peer_vv)?;
        let auth_vv = self.doc.oplog_vv();
        // If the authority has no op the peer lacks, there is nothing to send.
        // (We compare version vectors rather than the exported byte length, since
        // Loro may emit a non-empty frame even when no new ops are present.)
        if from.includes_vv(&auth_vv) {
            return Ok(CatchUp::None);
        }
        // `Updates { from }` exports every op the authority has that `from` lacks.
        let bytes = self.doc.export(ExportMode::Updates {
            from: Cow::Borrowed(&from),
        })?;
        if bytes.is_empty() {
            Ok(CatchUp::None)
        } else {
            Ok(CatchUp::Updates(bytes))
        }
    }

    /// Export a full snapshot (state + history).
    pub fn export_snapshot(&self) -> SyncResult<Vec<u8>> {
        Ok(self.doc.export(ExportMode::Snapshot)?)
    }
}

impl Default for AuthorityDoc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edits(peer: u64, text: &str) -> (LoroDoc, Vec<u8>) {
        let d = LoroDoc::new();
        d.set_peer_id(peer).unwrap();
        d.get_text("text").insert(0, text).unwrap();
        d.commit();
        let bytes = d.export(ExportMode::Snapshot).unwrap();
        (d, bytes)
    }

    #[test]
    fn catchup_for_fresh_peer_is_snapshot() {
        let auth = AuthorityDoc::new();
        match auth.catchup(&[]).unwrap() {
            CatchUp::Snapshot(_) => {}
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[test]
    fn incremental_catchup_advances_a_stale_peer() {
        // Peer 1 makes the initial edit; authority imports it.
        let (_, snap) = edits(1, "hello");
        let auth = AuthorityDoc::from_snapshot(&snap).unwrap();

        // Peer 2 bootstraps from the snapshot and records its vv.
        let peer2 = LoroDoc::new();
        peer2.set_peer_id(2).unwrap();
        peer2.import(&snap).unwrap();
        let peer2_vv = peer2.oplog_vv().encode();

        // While peer 2 is "offline", peer 3 edits through the authority.
        let p3 = LoroDoc::new();
        p3.set_peer_id(3).unwrap();
        p3.import(&snap).unwrap(); // peer 3 has the current "hello"
        p3.get_text("text").insert(5, " world").unwrap();
        p3.commit();
        let p3_update = p3
            .export(ExportMode::Updates {
                from: Cow::Borrowed(&VersionVector::new()),
            })
            .unwrap();
        auth.import_update(&p3_update).unwrap();

        // Reconnecting peer 2 requests catch-up since its recorded vv.
        match auth.catchup(&peer2_vv).unwrap() {
            CatchUp::Updates(bytes) => {
                peer2.import(&bytes).unwrap();
            }
            other => panic!("expected updates, got {other:?}"),
        }

        // Both peers and the authority now agree, and the text contains both edits.
        assert_eq!(
            peer2.get_text("text").to_string(),
            auth.inner().get_text("text").to_string()
        );
        assert_eq!(peer2.get_text("text").to_string(), "hello world");
        assert_eq!(peer2.oplog_vv(), auth.version_vector());
    }

    #[test]
    fn catchup_none_when_already_current() {
        let (_, snap) = edits(1, "hi");
        let auth = AuthorityDoc::from_snapshot(&snap).unwrap();
        let vv = auth.version_vector().encode();
        assert_eq!(auth.catchup(&vv).unwrap(), CatchUp::None);
    }
}
