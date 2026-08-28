//! Registry of live document rooms.
//!
//! [`DocRegistry`] maps a [`DocId`] to its [`DocRoom`], creating one lazily on
//! first join and hydrating it from the snapshot + op-log stores. It is the unit
//! the HTTP/WebSocket layer holds by handle.

use std::sync::Arc;

use dashmap::DashMap;

use crate::auth::AccessResolver;
use crate::authority::AuthorityDoc;
use crate::config::{Config, DocId};
use crate::error::{SyncError, SyncResult};
use crate::op_log::OpLogStore;
use crate::room::DocRoom;
use crate::seed::SeedVerifier;
use crate::snapshot::SnapshotStore;
use crate::time::Clock;

/// The live set of document rooms, plus the shared stores and config.
#[derive(Clone)]
pub struct DocRegistry {
    rooms: Arc<DashMap<DocId, Arc<DocRoom>>>,
    op_log: Arc<dyn OpLogStore>,
    snapshots: Arc<dyn SnapshotStore>,
    config: Arc<Config>,
    clock: Arc<dyn Clock>,
    access: Arc<dyn AccessResolver>,
    seed_verifier: Arc<dyn SeedVerifier>,
}

impl DocRegistry {
    #[must_use]
    pub fn new(
        op_log: Arc<dyn OpLogStore>,
        snapshots: Arc<dyn SnapshotStore>,
        config: Arc<Config>,
        clock: Arc<dyn Clock>,
        access: Arc<dyn AccessResolver>,
    ) -> Self {
        Self::with_seed_verifier(
            op_log,
            snapshots,
            config,
            clock,
            access,
            Arc::new(crate::seed::DenyAllSeedVerifier),
        )
    }

    /// Like [`Self::new`] but with an explicit seed verifier (production wires
    /// the app body endpoint; deny-all is the fail-closed default).
    #[must_use]
    pub fn with_seed_verifier(
        op_log: Arc<dyn OpLogStore>,
        snapshots: Arc<dyn SnapshotStore>,
        config: Arc<Config>,
        clock: Arc<dyn Clock>,
        access: Arc<dyn AccessResolver>,
        seed_verifier: Arc<dyn SeedVerifier>,
    ) -> Self {
        Self {
            rooms: Arc::new(DashMap::new()),
            op_log,
            snapshots,
            config,
            clock,
            access,
            seed_verifier,
        }
    }

    /// The configured access resolver.
    #[must_use]
    pub fn access(&self) -> &Arc<dyn AccessResolver> {
        &self.access
    }

    /// Get the room for `doc_id`, creating + hydrating it on first access.
    ///
    /// Creating a new room is gated by [`Config::max_rooms`]: once the cap is
    /// reached, this first evicts currently-empty rooms (oldest last-touched
    /// first); if there are still no empty rooms to evict, the new join is
    /// rejected with a `Limit` error rather than growing unbounded. This keeps an
    /// unprivileged flood of distinct document ids from pinning an unbounded
    /// number of rooms (and op-log file handles) forever.
    pub async fn get_or_open(&self, doc_id: &DocId) -> SyncResult<Arc<DocRoom>> {
        if let Some(r) = self.rooms.get(doc_id) {
            return Ok(Arc::clone(&r));
        }
        if self.rooms.len() >= self.config.max_rooms {
            // Try to make room by evicting currently-empty rooms. `DocRoom::is_empty`
            // means "no live sessions"; an empty room is safe to drop at any time.
            let evicted = self.evict_empty().await;
            if evicted == 0 {
                return Err(SyncError::Limit(format!(
                    "document room limit reached ({}); try again later",
                    self.config.max_rooms
                )));
            }
        }
        // `entry` is atomic: only one inserter wins. The pre-built `Arc` is
        // dropped if another thread won the race — a cheap wasted clone.
        let room = Arc::new(
            DocRoom::open(
                doc_id.clone(),
                Arc::clone(&self.op_log),
                Arc::clone(&self.snapshots),
                Arc::clone(&self.config),
                Arc::clone(&self.clock),
                Arc::clone(&self.seed_verifier),
            )
            .await?,
        );
        let r = self.rooms.entry(doc_id.clone()).or_insert(room);
        Ok(Arc::clone(&r))
    }

    /// Number of live rooms.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Whether any rooms are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    /// All live room handles, for the periodic sweep/snapshot task.
    #[must_use]
    pub fn rooms(&self) -> Vec<Arc<DocRoom>> {
        self.rooms
            .iter()
            .map(|entry| Arc::clone(entry.value()))
            .collect()
    }

    /// The live room for `doc_id`, if one is currently registered.
    #[must_use]
    pub fn room(&self, doc_id: &DocId) -> Option<Arc<DocRoom>> {
        self.rooms
            .get(doc_id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// The document's whole current state as an opaque Loro snapshot, for the
    /// internal read API (`GET /internal/docs/{doc_id}/state`).
    ///
    /// Resolution order:
    ///
    /// 1. a **live room** — its authority holds every applied update, including
    ///    any not yet snapshotted to the store;
    /// 2. otherwise the **persisted stores** (latest snapshot + op-log replay),
    ///    hydrated into a throwaway authority *without registering a room*, so
    ///    an internal read never pins an op-log handle or grows the room map;
    /// 3. `Ok(None)` when the document has no state anywhere (no live room, no
    ///    snapshot, empty op log) — the HTTP layer answers 204, keeping 404
    ///    reserved for a routing miss so the caller can tell the two apart.
    ///
    /// The bytes are exported without interpretation: this is the same opaque
    /// whole-state export a joining peer receives, surfaced on an authenticated
    /// service-to-service path instead of the public WebSocket relay.
    pub async fn export_state(&self, doc_id: &DocId) -> SyncResult<Option<Vec<u8>>> {
        if let Some(room) = self.room(doc_id) {
            return room.export_state().map(Some);
        }
        // No live room: hydrate from the persisted stores. A snapshot is the
        // cheap path; a document that only ever received updates below the
        // snapshot threshold still has state in the op log alone.
        let authority = match self.snapshots.latest(doc_id).await? {
            Some(snapshot) => AuthorityDoc::from_snapshot(&snapshot.bytes)?,
            None if self.op_log.is_empty(doc_id).await? => return Ok(None),
            None => AuthorityDoc::new(),
        };
        replay_op_log(&*self.op_log, doc_id, &authority).await?;
        authority.export_snapshot().map(Some)
    }

    /// Evict every room that is currently empty (no live sessions) and idle
    /// beyond [`Config::evict_idle_ttl_ms`] — oldest-touched first, up to the cap
    /// surplus. Returns the number of rooms evicted.
    ///
    /// Empty-but-recently-touched rooms are skipped so a burst of opens that then
    /// idle briefly is not thrashed; only rooms idle past the TTL are reclaimed.
    /// This is what releases an idle room's op-log file handle.
    #[allow(clippy::unused_async)] // close() stays sync; kept async for API consistency
    pub async fn evict_idle_rooms(&self) -> usize {
        let ttl = std::time::Duration::from_millis(self.config.evict_idle_ttl_ms);
        let mut idle: Vec<(DocId, std::time::Instant)> = Vec::new();
        for entry in self.rooms.iter() {
            let room = entry.value();
            if room.is_empty() && room.is_idle(ttl) {
                let last = room.last_active_hint();
                idle.push((entry.key().clone(), last));
            }
        }
        // Oldest-last-touched first so we reclaim the most-stale rooms first.
        idle.sort_by_key(|(_, last)| *last);
        let mut count = 0;
        for (doc, _) in idle {
            if self.rooms.remove(&doc).is_some() {
                // Release the op-log file handle the room no longer pins.
                if let Err(e) = self.op_log.close(&doc) {
                    tracing::warn!(error = %e, doc = %doc, "failed to close evicted room op log");
                }
                tracing::info!(doc = %doc, "evicted idle document room");
                count += 1;
            }
        }
        count
    }

    // ---- internals ---------------------------------------------------------

    /// Evict currently-empty rooms until the map is under [`Config::max_rooms`] or
    /// no empty room remains. Returns the number evicted.
    #[allow(clippy::unused_async)] // close() stays sync; kept async for API consistency
    async fn evict_empty(&self) -> usize {
        let mut count = 0;
        while self.rooms.len() >= self.config.max_rooms {
            let mut victim: Option<(DocId, std::time::Instant)> = None;
            for entry in self.rooms.iter() {
                if !entry.value().is_empty() {
                    continue;
                }
                let last = entry.value().last_active_hint();
                let candidate = (entry.key().clone(), last);
                victim = match victim {
                    Some((_, tl)) if tl <= last => victim,
                    _ => Some(candidate),
                };
            }
            let Some((doc, _)) = victim else {
                break;
            };
            if self.rooms.remove(&doc).is_some() {
                if let Err(e) = self.op_log.close(&doc) {
                    tracing::warn!(error = %e, doc = %doc, "failed to close evicted room op log");
                }
                count += 1;
            }
        }
        count
    }
}

/// Replay every op-log record for `doc` into `authority` (internal state reads).
///
/// Re-importing an already-applied op is a no-op in Loro, so this is correct
/// wherever the snapshot boundary sits. A record that fails to import (a torn
/// tail) is skipped with a warning rather than failing the read — the same
/// recovery posture as [`DocRoom::open`].
async fn replay_op_log(
    op_log: &dyn OpLogStore,
    doc: &DocId,
    authority: &AuthorityDoc,
) -> SyncResult<()> {
    for update in op_log.read_all(doc).await? {
        if let Err(error) = authority.import_update(&update) {
            tracing::warn!(
                error = %error,
                doc = %doc,
                "skipping op-log record that fails to import during internal state read"
            );
        }
    }
    Ok(())
}
