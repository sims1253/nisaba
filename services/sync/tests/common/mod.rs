//! Shared test harness for integration tests.
//!
//! Models each "peer" as its own [`loro::LoroDoc`] that collects its local
//! updates via [`loro::LoroDoc::subscribe_local_update`] (exactly how a real
//! client would), and drives them through a [`nisaba_sync::DocRoom`] the way the
//! WebSocket session does. This lets convergence / reconnect / presence be
//! asserted deterministically without a network, while an end-to-end test in
//! `e2e.rs` exercises the same logic over a real socket.
//!
//! NB: the [`loro::Subscription`] returned by `subscribe_local_update` MUST be
//! kept alive for the callback to fire; it is stored on each peer.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use loro::{ExportMode, LoroDoc, Subscription};
use nisaba_sync::protocol::{CatchUp, Frame};
use nisaba_sync::{CLOSE_NORMAL, CloseSignal, DocId, DocRoom, PeerId, Role, close_signal};
use tokio::sync::mpsc;

/// A simulated peer: its own CRDT replica plus the outbound channel the room
/// fans frames onto.
pub struct SimPeer {
    pub peer: PeerId,
    pub role: Role,
    pub doc: LoroDoc,
    pub rx: mpsc::Receiver<Frame>,
    /// The admission generation returned by the last join — needed to leave
    /// without evicting a re-admitted replacement.
    pub generation: u64,
    close: CloseSignal,
    pending: Arc<Mutex<Vec<Vec<u8>>>>,
    _sub: Subscription,
}

impl SimPeer {
    /// Create a fresh peer with the given Loro peer id and role.
    pub fn new(peer: u64, role: Role) -> Self {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).unwrap();
        let pending = Arc::new(Mutex::new(Vec::new()));
        let pending_cb = Arc::clone(&pending);
        let sub = doc.subscribe_local_update(Box::new(move |update: &Vec<u8>| {
            pending_cb.lock().unwrap().push(update.clone());
            true
        }));
        Self {
            peer: PeerId(peer),
            role,
            doc,
            rx: unreachable_rx(),
            generation: 0,
            close: close_signal(CLOSE_NORMAL),
            pending,
            _sub: sub,
        }
    }

    /// Join `room`, importing the catch-up payload into this peer's doc.
    pub async fn connect(&mut self, room: &Arc<DocRoom>, peer_vv: &[u8]) {
        let (tx, rx) = mpsc::channel(64);
        self.rx = rx;
        self.close = close_signal(CLOSE_NORMAL);
        let outcome = room
            .join(
                self.peer,
                self.role,
                peer_vv,
                Vec::new(),
                tx,
                self.close.clone(),
            )
            .expect("join");
        self.generation = outcome.generation;
        // The room queued the WELCOME first; apply its catch-up payload.
        if let Some(frame) = self.rx.recv().await {
            if let Frame::Welcome { catchup, .. } = frame {
                self.apply_catchup(catchup);
            } else {
                panic!("expected WELCOME first, got {frame:?}");
            }
        }
    }

    /// Leave the room using this peer's current admission generation, so the
    /// leave is correctly fenced (a stale leave for an older generation is a
    /// no-op).
    pub fn leave_room(&self, room: &Arc<DocRoom>) -> bool {
        room.leave(self.peer, self.generation)
    }

    fn apply_catchup(&mut self, catchup: CatchUp) {
        match catchup {
            CatchUp::None => {}
            CatchUp::Updates(b) | CatchUp::Snapshot(b) => {
                self.doc.import(&b).expect("import catchup");
            }
        }
    }

    /// Drain and apply every fanned-out frame (updates imported; presence and
    /// heartbeats ignored for state purposes).
    pub fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Frame::Update(b)) => {
                    self.doc.import(&b).expect("import fanned update");
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    /// Submit every locally-captured update through the room and clear the queue.
    pub async fn submit(&self, room: &Arc<DocRoom>) {
        for u in self.captured_updates() {
            room.handle_update(self.peer, self.role, &u)
                .await
                .expect("update");
        }
    }

    /// Submit every locally-captured update through the room, returning the
    /// first error instead of panicking.
    pub async fn submit_result(&self, room: &Arc<DocRoom>) -> Result<(), nisaba_sync::SyncError> {
        for u in self.captured_updates() {
            room.handle_update(self.peer, self.role, &u).await?;
        }
        Ok(())
    }

    /// Submit only the FIRST captured update (for tests that need to exercise
    /// one specific frame, e.g. a combined text+review transaction).
    pub async fn submit_first(&self, room: &Arc<DocRoom>) -> Result<(), nisaba_sync::SyncError> {
        if let Some(u) = self.captured_updates().into_iter().next() {
            room.handle_update(self.peer, self.role, &u).await?;
        }
        Ok(())
    }

    /// Drain and return the locally-captured updates without submitting them.
    pub fn captured_updates(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut *self.pending.lock().unwrap())
    }

    /// Write review items the way the web client's persistence does: one JSON
    /// item per map key (the item id). Commits. Mirrors
    /// `web/src/review-persistence.ts` `writeReviewItemsToMap`.
    pub fn set_review(&self, items: &[(&str, &str)]) {
        let map = self.doc.get_map(nisaba_sync::authority::REVIEW_CONTAINER);
        for (id, json) in items {
            map.insert(id, json.to_string()).unwrap();
        }
        self.doc.commit();
    }

    /// The peer's persisted review item with `id`, if any (the JSON payload).
    pub fn review_entry(&self, id: &str) -> Option<String> {
        self.doc
            .get_map(nisaba_sync::authority::REVIEW_CONTAINER)
            .get(id)
            .and_then(|value| match value.get_deep_value() {
                loro::LoroValue::String(s) => Some(s.to_string()),
                _ => None,
            })
    }

    /// The peer's current text content (the `text` container).
    pub fn text(&self) -> String {
        self.doc.get_text("text").to_string()
    }

    /// Insert text at `pos`, committing immediately.
    pub fn insert(&self, pos: usize, s: &str) {
        self.doc.get_text("text").insert(pos, s).unwrap();
        self.doc.commit();
    }

    /// Delete `len` chars at `pos`, committing immediately.
    pub fn delete(&self, pos: usize, len: usize) {
        self.doc.get_text("text").delete(pos, len).unwrap();
        self.doc.commit();
    }

    /// Export a full snapshot of this peer's doc (for seeding the authority).
    pub fn snapshot(&self) -> Vec<u8> {
        self.doc.export(ExportMode::Snapshot).unwrap()
    }

    /// Encode this peer's current version vector.
    pub fn vv_bytes(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }
}

/// Build an in-memory room for `doc_id`.
pub async fn room(doc_id: &str) -> Arc<DocRoom> {
    Arc::new(
        DocRoom::in_memory(DocId::new(doc_id).unwrap())
            .await
            .unwrap(),
    )
}

fn unreachable_rx<T>() -> mpsc::Receiver<T> {
    let (_tx, rx) = mpsc::channel(1);
    rx
}
