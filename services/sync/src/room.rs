//! A document room: the coordination point for one document.
//!
//! [`DocRoom`] is the deep component that ties together every other component for a
//! single document id:
//!
//! * the [`AuthorityDoc`] (the CRDT authority),
//! * the set of live sessions and the fan-out relay,
//! * the [`Presence`] roster (ephemeral, heartbeat-expiring),
//! * the [`OpLogStore`] (append-only) and [`SnapshotStore`] (periodic).
//!
//! Design notes:
//!
//! * **Opaque relay.** Inbound update bytes are imported into the authority and
//!   forwarded verbatim to other sessions. The room never inspects Loro state —
//!   this is the "sync transports opaque Loro state" invariant.
//! * **Atomic admission under the gate.** The capacity check, the duplicate-peer
//!   check, and the session insert all happen while holding [`Self::gate`], so
//!   two concurrent joins can neither blow past the peer cap nor both believe
//!   they admitted the same peer. Each admitted session additionally receives a
//!   unique **generation** (an incarnation number); [`DocRoom::leave`] only
//!   removes a session when the caller presents the generation it was admitted
//!   with, so a *stale* leave (a late socket-close for a session that has since
//!   been replaced) can never evict the replacement.
//! * **Backpressure evicts the slow peer, never drops the update.** Fan-out uses
//!   `try_send`; when a peer's outbound channel is full the room disconnects that
//!   peer explicitly (a resync-required close signal + roster departure) while
//!   leaving the authority and op log untouched, so a reconnect catches up.
//! * **Serialised handshake/fan-out.** A reconnecting peer must receive its
//!   `WELCOME` before any concurrently-arriving update. We guarantee this by
//!   holding [`Self::gate`] (a plain `Mutex`, no `.await` inside) across:
//!   (a) computing catch-up + admitting the session + queueing the welcome, and
//!   (b) every fan-out. Because imports run *outside* the gate, any update fanned
//!   out before a join is already in the authority when the join computes its
//!   catch-up, and any update imported after is fanned out after the welcome.
//! * **No physical deletion.** The room treats every update as opaque bytes, so
//!   review-layer "soft deletes" (marks over CRDT positions,
//!   trap #3) pass through untouched.

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use loro::VersionVector;
use tokio::sync::mpsc;

use crate::auth::{AuthError, Role, RoleCapabilities};
use crate::authority::AuthorityDoc;
use crate::config::{Config, DocId, PeerId};
use crate::error::{SyncError, SyncResult};
use crate::op_log::OpLogStore;
use crate::presence::{Presence, encode_roster};
use crate::protocol::{CatchUp, Frame, WelcomeStatus};
use crate::seed::SeedVerifier;
use crate::snapshot::{Snapshot, SnapshotStore};
use crate::time::{Clock, SystemClock};

/// A shared, atomic close-code the room writes when it disconnects a session and
/// the session task reads on its way out, so a server-initiated close carries a
/// meaningful code (e.g. resync-required) instead of a generic one.
pub type CloseSignal = Arc<AtomicU16>;

/// Construct a fresh close signal seeded with `code`.
#[must_use]
pub fn close_signal(code: u16) -> CloseSignal {
    Arc::new(AtomicU16::new(code))
}

/// The "normal" close code used for voluntary leaves (WS 1000).
pub const CLOSE_NORMAL: u16 = 1000;
/// Application close code: the peer fell behind and must resync from scratch.
/// Room-initiated; the authority/op log are preserved so reconnect catches up.
pub const CLOSE_RESYNC_REQUIRED: u16 = 4091;

/// What [`DocRoom::join`] hands back to the session: the welcome frame that was
/// queued first on the peer's channel, plus the unique **generation** that fences
/// stale leaves for this admission.
#[derive(Debug)]
pub struct JoinOutcome {
    /// The welcome frame (already queued on the peer's outbound channel).
    pub welcome: Frame,
    /// The admission generation. Pass it back to [`DocRoom::leave`] so a stale
    /// leave cannot evict a newer admission of the same peer id.
    pub generation: u64,
}

/// One admitted session's outbound channel plus its fencing generation and the
/// close signal shared with the session task.
#[derive(Clone)]
struct SessionSlot {
    tx: mpsc::Sender<Frame>,
    generation: u64,
    close: CloseSignal,
}

/// Coalescer for presence roster re-broadcasts.
///
/// Presence frames can arrive in rapid bursts; re-encoding and fanning the full
/// roster to every peer on each frame is wasteful. This throttles real
/// broadcasts to at most one per `interval`, keeping the *latest* roster pending
/// so it is delivered by the next due broadcast or an explicit
/// [`DocRoom::flush_pending_presence`] call.
struct PresenceBroadcaster {
    last: Option<Instant>,
    interval: Duration,
    pending: bool,
}

impl PresenceBroadcaster {
    fn new(interval_ms: u64) -> Self {
        Self {
            last: None,
            interval: Duration::from_millis(interval_ms),
            pending: false,
        }
    }

    /// Whether a broadcast is due now. The first change always broadcasts; bursts
    /// within `interval` are coalesced into a pending flag.
    fn due(&mut self, now: Instant) -> bool {
        let overdue = match self.last {
            Some(last) => now.saturating_duration_since(last) >= self.interval,
            None => true,
        };
        if overdue {
            self.last = Some(now);
            self.pending = false;
            true
        } else {
            self.pending = true;
            false
        }
    }
}

/// One document's coordination state.
pub struct DocRoom {
    doc_id: DocId,
    authority: AuthorityDoc,
    /// Every mutation of this map happens under [`Self::gate`]; reads (e.g.
    /// [`Self::session_count`]) are best-effort stats and need no lock.
    sessions: DashMap<PeerId, SessionSlot>,
    /// Serialises handshake + fan-out ordering (see component documentation). Also guards the
    /// presence roster.
    gate: Mutex<Presence>,
    /// Monotonic admission counter; each join draws the next value.
    next_gen: AtomicU64,
    updates_since_snapshot: AtomicU64,
    /// Time of the last activity (join / leave / update / presence / heartbeat),
    /// read under the clock for idle-eviction decisions. Accessed only while
    /// holding [`Self::gate`], except by the maintenance-read-only stats.
    last_active: Mutex<Option<Instant>>,
    op_log: Arc<dyn OpLogStore>,
    snapshots: Arc<dyn SnapshotStore>,
    config: Arc<Config>,
    clock: Arc<dyn Clock>,
    presence_bcast: Mutex<PresenceBroadcaster>,
    /// Verifies that a reviewer's seed of an empty room matches the app's
    /// authoritative document body (fail-closed when unconfigured).
    seed_verifier: Arc<dyn SeedVerifier>,
    /// Monotonic time of each peer's most recent review-container update, used
    /// by the reviewer text gate to correlate suggestion records with the
    /// separate text frames the web client emits.
    review_touched: Mutex<std::collections::HashMap<PeerId, Instant>>,
}

impl DocRoom {
    /// Lock the presence gate.
    ///
    /// Poisoning is unreachable: no code path taken while the gate is held
    /// panics (fallible work — decode, import, awaits — happens before or
    /// after the critical section; the nested `last_active` lock recovers
    /// poisoning, and `presence_bcast` maps it to an error, so neither can
    /// cascade a panic into a held gate), so the `expect` documents that
    /// invariant once instead of repeating it at every call site. If it ever
    /// fires, the panic unwinds only the connection task that hit it.
    fn lock_gate(&self) -> std::sync::MutexGuard<'_, Presence> {
        self.gate
            .lock()
            .expect("room gate poisoned (invariant: no gate-held section panics)")
    }

    /// Build a room. If a prior snapshot + op log exist for `doc_id`, the
    /// authority is hydrated from them; otherwise it starts empty.
    pub async fn open(
        doc_id: DocId,
        op_log: Arc<dyn OpLogStore>,
        snapshots: Arc<dyn SnapshotStore>,
        config: Arc<Config>,
        clock: Arc<dyn Clock>,
        seed_verifier: Arc<dyn SeedVerifier>,
    ) -> SyncResult<Self> {
        let authority = match snapshots.latest(&doc_id).await? {
            Some(snap) => {
                let auth = AuthorityDoc::from_snapshot(&snap.bytes)?;
                // Replay any op-log entries recorded after the snapshot. Re-importing
                // already-applied ops is a no-op in Loro, so this is correct even if
                // the log was not truncated at snapshot time. A record that fails to
                // import (a torn tail we could not preview, or a rejected update) is
                // skipped with a warning rather than failing the whole room open —
                // this is what makes corrupt trailing records recoverable.
                for update in op_log.read_all(&doc_id).await? {
                    if let Err(e) = auth.import_update(&update) {
                        tracing::warn!(
                            error = %e,
                            doc = %doc_id,
                            "skipping op-log record that fails to import during replay"
                        );
                    }
                }
                auth
            }
            None => AuthorityDoc::new(),
        };

        let presence_coalesce_ms = config.presence_coalesce_ms;
        let now = clock.now();
        Ok(Self {
            doc_id,
            authority,
            sessions: DashMap::new(),
            gate: Mutex::new(Presence::new(config.presence_ttl_ms, clock.clone())),
            next_gen: AtomicU64::new(0),
            updates_since_snapshot: AtomicU64::new(0),
            last_active: Mutex::new(Some(now)),
            op_log,
            snapshots,
            config,
            clock,
            presence_bcast: Mutex::new(PresenceBroadcaster::new(presence_coalesce_ms)),
            seed_verifier,
            review_touched: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Convenience constructor for tests/dev: in-memory stores, system clock.
    pub async fn in_memory(doc_id: DocId) -> SyncResult<Self> {
        Self::open(
            doc_id,
            Arc::new(crate::op_log::MemoryOpLogStore::default()),
            Arc::new(crate::snapshot::MemorySnapshotStore::default()),
            Arc::new(crate::config::Config::default()),
            Arc::new(SystemClock),
            Arc::new(crate::seed::DenyAllSeedVerifier),
        )
        .await
    }

    /// Constructor for tests that need a controllable seed verifier.
    pub async fn with_seed_verifier(
        doc_id: DocId,
        seed_verifier: Arc<dyn SeedVerifier>,
    ) -> SyncResult<Self> {
        Self::open(
            doc_id,
            Arc::new(crate::op_log::MemoryOpLogStore::default()),
            Arc::new(crate::snapshot::MemorySnapshotStore::default()),
            Arc::new(crate::config::Config::default()),
            Arc::new(SystemClock),
            seed_verifier,
        )
        .await
    }

    /// The document id.
    #[must_use]
    pub fn doc_id(&self) -> &DocId {
        &self.doc_id
    }

    /// Read-only access to the authority (tests + health stats).
    #[must_use]
    pub fn authority(&self) -> &AuthorityDoc {
        &self.authority
    }

    /// Number of live sessions.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the room has any live sessions. An evictable room must have none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Whether the room has had no activity for at least `ttl`. Only meaningful
    /// for rooms with no live sessions (the registry never evicts a room that
    /// still has peers). Reads [`Self::last_active`] under its lock.
    #[must_use]
    pub fn is_idle(&self, ttl: Duration) -> bool {
        let last = match self.last_active.lock() {
            Ok(g) => *g,
            Err(poisoned) => *poisoned.into_inner(),
        };
        match last {
            Some(t) => self.clock.now().saturating_duration_since(t) >= ttl,
            None => true,
        }
    }

    /// The last-activity timestamp, or the room-open time if never touched. Used
    /// by the registry to pick eviction victims. Never blocks long: a read of a
    /// poisoned lock falls back to the current clock time.
    #[must_use]
    pub fn last_active_hint(&self) -> Instant {
        let last = match self.last_active.lock() {
            Ok(g) => *g,
            Err(_) => None,
        };
        last.unwrap_or_else(|| self.clock.now())
    }

    /// Record that the room just saw activity (a join, leave, update, presence
    /// frame, or heartbeat). Called with the gate already held in the mutating
    /// paths, and independently by the read-only stats paths that only read the
    /// activity clock.
    fn touch(&self) {
        let now = self.clock.now();
        match self.last_active.lock() {
            Ok(mut g) => *g = Some(now),
            Err(poisoned) => *poisoned.into_inner() = Some(now),
        }
    }

    /// A peer joins the room.
    ///
    /// `tx` is the channel the session task drains to its WebSocket. Ownership of
    /// the sender moves into the room (the room is the sole sender holder, so
    /// dropping it from the map is observable as `None` on the receiver — this is
    /// how server-initiated disconnects terminate the session task). The first
    /// message queued on the channel is guaranteed to be the `WELCOME` (see component
    /// docs).
    ///
    /// `close` is shared with the session task: the room writes a close code into
    /// it when it disconnects the peer, and the session task reads it when it
    /// tears down the socket.
    pub fn join(
        &self,
        peer: PeerId,
        role: Role,
        peer_vv: &[u8],
        initial_state: Vec<u8>,
        tx: mpsc::Sender<Frame>,
        close: CloseSignal,
    ) -> SyncResult<JoinOutcome> {
        let mut presence = self.lock_gate();

        // Atomic admission: capacity + duplicate check + insert all under the
        // gate, so concurrent joins cannot exceed the peer cap or double-admit.
        if presence.len() >= self.config.max_peers_per_doc {
            return Err(SyncError::Limit(format!(
                "document {} is full (max {} peers)",
                self.doc_id, self.config.max_peers_per_doc
            )));
        }
        if self.sessions.contains_key(&peer) {
            return Err(SyncError::Handshake(format!(
                "peer {} already connected to document {}",
                peer.get(),
                self.doc_id
            )));
        }

        // Catch-up is computed inside the gate so it observes any update already
        // fanned out by a concurrent handle_update (which also holds this gate).
        let catchup = self.authority.catchup(peer_vv)?;
        // Bound the catch-up payload (incremental updates or a full snapshot):
        // without a cap, `catchup` output was queued verbatim on the peer's
        // channel no matter how large. Reject the join — never truncate, since
        // silently modifying CRDT state would corrupt the peer.
        if let CatchUp::Updates(bytes) | CatchUp::Snapshot(bytes) = &catchup
            && bytes.len() > self.config.max_snapshot_bytes
        {
            return Err(SyncError::Limit(format!(
                "catch-up payload of {} bytes exceeds the {} byte limit for document {}",
                bytes.len(),
                self.config.max_snapshot_bytes,
                self.doc_id
            )));
        }
        let status = match &catchup {
            CatchUp::None => WelcomeStatus::OkNoCatchUp,
            _ => WelcomeStatus::Ok,
        };
        let welcome = Frame::Welcome {
            status,
            note: format!("joined document {} as {role}", self.doc_id),
            catchup: catchup.clone(),
        };

        let generation = self.next_gen.fetch_add(1, Ordering::Relaxed) + 1;
        self.sessions.insert(
            peer,
            SessionSlot {
                tx,
                generation,
                close,
            },
        );

        // Queue the welcome while still holding the gate. A failure here means the
        // channel is already full/closed before the peer even starts — admit-then
        // immediately evict so the peer gets a clean error rather than a silent
        // stall.
        if let Err(e) = self
            .sessions
            .get(&peer)
            .expect("just inserted")
            .tx
            .try_send(welcome.clone())
        {
            let failed = vec![peer];
            self.evict_failed(
                &mut presence,
                &failed,
                matches!(e, mpsc::error::TrySendError::Full(_)),
            );
            return Err(SyncError::Limit(format!(
                "peer {} outbound channel unavailable at welcome: {e}",
                peer.get()
            )));
        }

        presence.upsert(peer, initial_state);
        let roster = encode_roster(&presence.roster());
        // Broadcast the new roster to everyone (including the joiner).
        self.fanout(&mut presence, &Frame::Presence(roster));
        self.touch();

        drop(presence);
        Ok(JoinOutcome {
            welcome,
            generation,
        })
    }

    /// Remove a peer from the room (socket closed or `Bye`).
    ///
    /// **Generation-fenced:** the removal only takes effect if `generation` is the
    /// one this peer was admitted with. A *stale* leave — e.g. a late close for an
    /// old session whose peer id has since been re-admitted with a new generation —
    /// is a no-op and cannot evict the replacement. Returns whether the removal
    /// actually happened.
    pub fn leave(&self, peer: PeerId, generation: u64) -> bool {
        let mut presence = self.lock_gate();
        let mine = self
            .sessions
            .get(&peer)
            .is_some_and(|slot| slot.generation == generation);
        if !mine {
            // Not present, or superseded by a newer admission: do nothing.
            return false;
        }
        self.sessions.remove(&peer);
        presence.remove(peer);
        let roster = encode_roster(&presence.roster());
        self.fanout(&mut presence, &Frame::Presence(roster));
        self.touch();
        drop(presence);
        true
    }

    /// Handle an inbound CRDT update from `sender`.
    ///
    /// Imports into the authority (advancing the version vector), appends to the
    /// op log, maybe writes a snapshot, then fans the *same opaque bytes* out to
    /// every other session. A peer that cannot keep up is disconnected with a
    /// resync-required signal; the import/append already happened, so the update
    /// is never lost and the slow peer catches up on reconnect.
    pub async fn handle_update(&self, sender: PeerId, role: Role, bytes: &[u8]) -> SyncResult<()> {
        if !role.can_push_updates() {
            return Err(SyncError::Access(AuthError::Forbidden {
                role,
                cap: "PUSH_UPDATES",
            }));
        }
        self.config.check_update_size(bytes.len())?;

        // Review-layer gate: reviewers must not overwrite the document text
        // without a corresponding review record (see enforce_reviewer_policy).
        if role == Role::Reviewer {
            self.enforce_reviewer_policy(sender, bytes).await?;
        }

        // Import + persist run outside the gate (Loro is internally synchronised
        // and the op log has its own mutex). Fan-out below acquires the gate so it
        // is ordered relative to any concurrent join.
        let advanced = self.import_and_persist(bytes).await?;
        if advanced {
            let mut presence = self.lock_gate();
            self.fanout_except(&mut presence, &Frame::Update(bytes.to_vec()), sender);
            self.touch();
        }
        Ok(())
    }

    /// Handle an inbound presence update from `sender` (opaque client state).
    pub fn handle_presence(&self, sender: PeerId, state: Vec<u8>) -> SyncResult<()> {
        if state.len() > crate::config::MAX_PRESENCE_BYTES {
            return Err(SyncError::Limit(format!(
                "presence payload of {} bytes exceeds limit",
                state.len()
            )));
        }
        let mut presence = self.lock_gate();
        presence.upsert(sender, state);
        // Coalesce roster re-broadcasts: a burst of presence frames must not
        // re-encode + fan the full roster per frame. The latest roster is kept
        // pending and delivered on the next due broadcast or the maintenance
        // flush, so peers still converge (at most one broadcast per interval).
        // A poisoned lock maps to an error rather than panicking under the
        // gate (which would poison the gate in turn).
        if self
            .presence_bcast
            .lock()
            .map_err(|_| SyncError::Internal("presence broadcaster lock poisoned".into()))?
            .due(self.clock.now())
        {
            let roster = encode_roster(&presence.roster());
            self.fanout(&mut presence, &Frame::Presence(roster));
        }
        self.touch();
        drop(presence);
        Ok(())
    }

    /// Flush a coalesced (pending) presence roster update, delivering the latest
    /// roster once the coalesce interval has elapsed. Returns whether a broadcast
    /// was sent. Called by the periodic maintenance task so a peer that stops
    /// sending presence frames still receives the final roster state.
    pub fn flush_pending_presence(&self) -> SyncResult<bool> {
        let broadcast_now = {
            let mut b = self
                .presence_bcast
                .lock()
                .map_err(|_| SyncError::Internal("presence broadcaster lock poisoned".into()))?;
            if !b.pending {
                return Ok(false);
            }
            let now = self.clock.now();
            if b.last
                .is_some_and(|last| now.saturating_duration_since(last) < b.interval)
            {
                return Ok(false);
            }
            b.last = Some(now);
            b.pending = false;
            true
        };
        if !broadcast_now {
            return Ok(false);
        }
        let mut presence = self.lock_gate();
        let roster = encode_roster(&presence.roster());
        self.fanout(&mut presence, &Frame::Presence(roster));
        Ok(true)
    }

    /// Record a heartbeat from `sender`.
    pub fn handle_heartbeat(&self, sender: PeerId) -> SyncResult<()> {
        let mut presence = self.lock_gate();
        presence.heartbeat(sender);
        self.touch();
        drop(presence);
        Ok(())
    }

    /// Disconnect a peer that exceeded its inbound frame rate. The peer's close
    /// signal is set to resync-required so it reconnects with a full catch-up
    /// rather than silently losing sync. Returns a sync error (or the empty
    /// result) so the session can log which peer was evicted.
    pub fn evict_for_rate_limit(&self, peer: PeerId) -> SyncResult<()> {
        let mut presence = self.lock_gate();
        if self.sessions.contains_key(&peer) {
            self.evict_failed(&mut presence, &[peer], true);
        }
        drop(presence);
        Ok(())
    }

    /// Run the presence expiry sweeper. Returns the evicted peers and, if any,
    /// disconnects their sessions (resync-required) and broadcasts an updated
    /// roster.
    pub fn sweep_presence(&self) -> Vec<PeerId> {
        let mut presence = self.lock_gate();
        let evicted = presence.sweep();
        if evicted.is_empty() {
            return evicted;
        }
        // Disconnect the evicted peers (their session may still be open) and drop
        // their sessions, all under the gate so a concurrent rejoin cannot be
        // torn down by this sweep.
        self.evict_failed(&mut presence, &evicted, true);
        drop(presence);
        evicted
    }

    /// Snapshot only if there are unsnapshotted updates.
    ///
    /// This is the time-based floor under the update-count trigger: a document that
    /// received fewer than `snapshot_every_updates` updates and then went idle would
    /// otherwise never be snapshotted, and would replay its whole op log on restart.
    /// Returns `true` when a snapshot was written.
    pub async fn snapshot_if_dirty(&self) -> SyncResult<bool> {
        if self.updates_since_snapshot.load(Ordering::Relaxed) == 0 {
            return Ok(false);
        }
        self.snapshot_now().await?;
        Ok(true)
    }

    /// Force a snapshot now (used by the periodic task and by tests).
    pub async fn snapshot_now(&self) -> SyncResult<VersionVector> {
        let bytes = self.authority.export_snapshot()?;
        let vv = self.authority.version_vector();
        self.snapshots
            .put(
                &self.doc_id,
                Snapshot {
                    vv: vv.clone(),
                    bytes,
                },
            )
            .await?;
        self.updates_since_snapshot.store(0, Ordering::Relaxed);
        Ok(vv)
    }

    /// Milliseconds a reviewer's text-touching update may follow a
    /// review-container update from the same peer. The web client emits the
    /// suggestion record and the text it annotates as *separate* CRDT frames,
    /// so the gate correlates them by peer + recency instead of requiring one
    /// combined update.
    const REVIEWER_TEXT_WINDOW_MS: u64 = 30_000;

    /// Enforce the review-layer policy for reviewer pushes.
    ///
    /// Reviewers may push updates that change the `review` container freely
    /// (suggestions, comments, accept/reject records). Updates that change the
    /// `text` container are only accepted when:
    ///
    /// 1. the room is empty — the update is (or claims to be) the initial seed,
    ///    and the resulting text must match the app's authoritative document
    ///    body ([`SeedVerifier`], fail-closed); or
    /// 2. the update also changes the `review` container in the same frame, or
    ///    the same peer changed it within [`Self::REVIEWER_TEXT_WINDOW_MS`]
    ///    (the web client's separate-frame suggestion flow).
    ///
    /// This blocks a custom client from silently replacing the document text
    /// (the QA-reported baseline overwrite) while keeping every documented
    /// reviewer flow working. A client that also forges review records can
    /// still bypass the transport gate — that residual requires a semantic
    /// review validator and is documented in docs/security.md.
    async fn enforce_reviewer_policy(&self, sender: PeerId, bytes: &[u8]) -> SyncResult<()> {
        let delta = self.authority.review_delta(bytes)?;
        if delta.touches_review {
            let now = self.clock.now();
            self.review_touched
                .lock()
                .map_err(|_| SyncError::Internal("review-touched lock poisoned".into()))?
                .insert(sender, now);
            // A combined text + review update is a suggestion regardless of
            // whether the document was previously empty. Check this before the
            // seed branch so a reviewer's first suggestion in an empty document
            // is not mistaken for an unsigned baseline seed.
            if delta.touches_text {
                return Ok(());
            }
        }
        if !delta.touches_text {
            return Ok(());
        }
        if self.authority.text_is_empty() {
            let ok = self
                .seed_verifier
                .verify(&self.doc_id, &delta.text_after)
                .await
                .map_err(SyncError::ReviewPolicy)?;
            if !ok {
                return Err(SyncError::ReviewPolicy(
                    "reviewer seed does not match the document body (suggest only; use the review layer)"
                        .into(),
                ));
            }
            return Ok(());
        }
        // A reviewer may touch text only if the update also carries review
        // state (a genuine suggestion mark) or the reviewer recently touched
        // the review container (init-reconcile echo within the grace window).
        // The previous ≤256-char net-change allowance was an attack surface
        // that let a reviewer make small unauthorized text edits without a
        // review record.
        let window_ok = {
            let touched = self
                .review_touched
                .lock()
                .map_err(|_| SyncError::Internal("review-touched lock poisoned".into()))?;
            touched.get(&sender).is_some_and(|at| {
                self.clock.now().saturating_duration_since(*at)
                    <= Duration::from_millis(Self::REVIEWER_TEXT_WINDOW_MS)
            })
        };
        if window_ok {
            return Ok(());
        }
        Err(SyncError::ReviewPolicy(
            "reviewer text change without a matching review record (suggest only; use the review layer)"
                .into(),
        ))
    }

    // ---- internals ---------------------------------------------------------

    /// Append to the op log **before** importing into the authority, then write a
    /// snapshot if the update threshold is crossed. Returns whether the import
    /// advanced the authority (so the caller knows whether to fan out).
    ///
    /// Decode-gate-then-append-then-import: undecodable bytes are rejected at
    /// ingest (never persisted), decodable bytes are made durable in the op log
    /// before the authority consumes them, so a crash between append and import
    /// loses nothing, and replay never sees a record the authority cannot apply.
    async fn import_and_persist(&self, bytes: &[u8]) -> SyncResult<bool> {
        // Validate BEFORE persisting: an update that Loro cannot decode must
        // never enter the op log. Append-then-import (the previous ordering)
        // kept garbage in the log whenever the authority rejected an update,
        // and replay then re-logged a warning for the same bad record forever.
        // A throwaway replica costs one extra import of the (bounded) update
        // and keeps the crash-durability property intact: only decodable bytes
        // are appended, so a crash between append and authority-import can
        // still be recovered from the log.
        if let Err(e) = loro::LoroDoc::new().import(bytes) {
            tracing::warn!(
                error = %e,
                doc = %self.doc_id,
                "rejecting undecodable CRDT update at ingest"
            );
            return Err(SyncError::Loro(format!("update failed to decode: {e}")));
        }
        self.op_log.append(&self.doc_id, bytes).await?;
        let before = self.authority.version_vector();
        let after = match self.authority.import_update(bytes) {
            Ok(after) => after,
            Err(e) => {
                // Defensive fallback: a validated update should import; if it
                // still fails, skip without poisoning the op log.
                tracing::warn!(
                    error = %e,
                    doc = %self.doc_id,
                    "authority rejected a validated update; skipping import"
                );
                return Ok(false);
            }
        };
        let advanced = after != before;
        if advanced {
            let n = self.updates_since_snapshot.fetch_add(1, Ordering::Relaxed) + 1;
            if n >= self.config.snapshot_every_updates {
                // Best-effort snapshot; a failure to persist must not drop the
                // update (the op log still has it).
                if let Err(e) = self.snapshot_now().await {
                    tracing::warn!(error = %e, doc = %self.doc_id, "snapshot failed; op log retains update");
                }
            }
        }
        Ok(advanced)
    }

    /// Send `frame` to every session, evicting any that cannot keep up. **Caller
    /// must hold [`Self::gate`]** and pass the locked presence guard so evicted
    /// peers can be dropped from the roster and the updated roster re-broadcast.
    fn fanout(&self, presence: &mut Presence, frame: &Frame) {
        let failed = self.try_send_all(frame, None);
        if failed.is_empty() {
            return;
        }
        self.evict_failed(presence, &failed, true);
    }

    /// Send `frame` to every session except `sender`, evicting slow ones. **Caller
    /// must hold [`Self::gate`].**
    fn fanout_except(&self, presence: &mut Presence, frame: &Frame, sender: PeerId) {
        let failed = self.try_send_all(frame, Some(sender));
        if failed.is_empty() {
            return;
        }
        self.evict_failed(presence, &failed, true);
    }

    /// Best-effort send to every session (optionally skipping `except`), returning
    /// the peer ids that could not accept the frame (full or closed channel).
    /// **Caller must hold [`Self::gate`]** (iterates the session map).
    fn try_send_all(&self, frame: &Frame, except: Option<PeerId>) -> Vec<PeerId> {
        let mut failed = Vec::new();
        for entry in &self.sessions {
            if Some(*entry.key()) == except {
                continue;
            }
            if entry.tx.try_send(frame.clone()).is_err() {
                failed.push(*entry.key());
            }
        }
        failed
    }

    /// Disconnect every peer in `peers`: set their close signal, drop their
    /// sessions, remove them from presence, and re-broadcast the roster. When
    /// `resync` is true the close signal carries resync-required (backpressure or
    /// presence-timeout); set it false only for voluntary paths. **Caller must
    /// hold [`Self::gate`].**
    ///
    /// The roster re-broadcast is best-effort and does not recurse: every peer
    /// that was full is removed in this pass, so the re-broadcast targets only
    /// peers that just accepted a frame and have capacity.
    fn evict_failed(&self, presence: &mut Presence, peers: &[PeerId], resync: bool) {
        if peers.is_empty() {
            return;
        }
        tracing::warn!(doc = %self.doc_id, peers = ?peers, resync, "evicting peers whose outbound channel is full");
        let code = if resync {
            CLOSE_RESYNC_REQUIRED
        } else {
            CLOSE_NORMAL
        };
        for p in peers {
            // Only disconnect the session we actually targeted: if the peer was
            // re-admitted with a new generation between the failed send and here,
            // the slot we hold is gone and we must not touch the new one. (The gate
            // makes this window impossible for the common paths, but the check is
            // cheap and keeps the invariant local to this method.)
            if let Some(slot) = self.sessions.get(p) {
                slot.close.store(code, Ordering::Release);
            }
            self.sessions.remove(p);
            presence.remove(*p);
        }
        tracing::warn!(
            doc = %self.doc_id,
            count = peers.len(),
            resync,
            "disconnecting slow/stale peers (authority and op log retained)"
        );
        let roster = encode_roster(&presence.roster());
        let _ = self.try_send_all(&Frame::Presence(roster), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;
    use crate::op_log::MemoryOpLogStore;
    use crate::snapshot::MemorySnapshotStore;
    use crate::time::SystemClock;
    use std::sync::Arc;

    async fn room() -> (DocRoom, Arc<dyn OpLogStore>) {
        let doc = DocId::new("integration_doc").unwrap();
        let op_log: Arc<dyn OpLogStore> = Arc::new(MemoryOpLogStore::default());
        let snapshots: Arc<dyn SnapshotStore> = Arc::new(MemorySnapshotStore::default());
        let room = DocRoom::open(
            doc,
            Arc::clone(&op_log),
            snapshots,
            Arc::new(Config::default()),
            Arc::new(SystemClock),
            Arc::new(crate::seed::DenyAllSeedVerifier),
        )
        .await
        .unwrap();
        (room, op_log)
    }

    #[tokio::test]
    async fn undecodable_update_is_rejected_and_not_persisted() {
        let (room, op_log) = room().await;
        // Random bytes are never a valid Loro update.
        let garbage: Vec<u8> = (0..64u8)
            .map(|i| i.wrapping_mul(7).wrapping_add(13))
            .collect();
        let result = room.handle_update(PeerId(1), Role::Author, &garbage).await;
        assert!(
            result.is_err(),
            "undecodable CRDT bytes must be rejected at ingest"
        );
        let persisted = op_log.read_all(&room.doc_id).await.unwrap();
        assert!(
            persisted.is_empty(),
            "rejected update must never reach the op log (got {} records)",
            persisted.len()
        );
        // And a *valid* update still round-trips (the gate must not be leaky).
        let valid = loro::LoroDoc::new();
        valid.get_text("text").insert(0, "hello").unwrap();
        let encoded = valid.export(loro::ExportMode::Snapshot).unwrap();
        room.handle_update(PeerId(2), Role::Author, &encoded)
            .await
            .unwrap();
        let persisted = op_log.read_all(&room.doc_id).await.unwrap();
        assert_eq!(persisted.len(), 1, "valid update must be persisted");
    }

    #[tokio::test]
    async fn poisoned_presence_bcast_maps_to_error_not_panic() {
        // A panic while the coalescer is held poisons it: presence frames must
        // return an error instead of panicking — and, because that error
        // returns *before* any gate-held panic, the gate itself stays usable.
        let (room, _op_log) = room().await;
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = room.presence_bcast.lock().unwrap();
            panic!("poison the presence broadcaster");
        }));
        assert!(poisoned.is_err());
        let result = room.handle_presence(PeerId(1), vec![1, 2, 3]);
        assert!(matches!(result, Err(SyncError::Internal(_))));
        // The gate is not poisoned by the contained failure.
        room.handle_heartbeat(PeerId(1)).unwrap();
    }
}
