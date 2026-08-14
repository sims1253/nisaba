//! Per-connection WebSocket session.
//!
//! One async task per connected peer. It runs a `select!` loop that concurrently
//! reads inbound [`Frame`]s from the socket and drains the outbound channel the
//! [`crate::room::DocRoom`] fans frames onto. axum 0.8's `WebSocket` has no
//! `split()`, so a single task multiplexes both directions — there is no `.await`
//! held across a room mutation (those are synchronous), so this never stalls.
//!
//! The session enforces the protocol contract:
//!
//! * the first inbound frame must be a [`Frame::Hello`] that matches the path's
//!   document id;
//! * only `Binary` frames are accepted on the wire (our framing lives entirely
//!   in binary);
//! * the role is resolved through the injected [`crate::auth::AccessResolver`] and
//!   every mutating frame is checked against it.
//!
//! **Backpressure / server-initiated disconnect.** The room owns the outbound
//! channel's sender. When the room evicts a peer (slow channel / presence
//! timeout) it drops that sender and writes a close code into the shared
//! [`crate::room::CloseSignal`]; this task observes the receiver going `None`,
//! ships a protocol `Error` frame explaining the disconnect, and closes the
//! socket with that code. A voluntary close (client `Bye` / socket error) closes
//! with the normal code.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures_util::sink::SinkExt;
use tokio::sync::mpsc;

use crate::auth::Role;
use crate::config::{Config, DocId, PeerId};
use crate::protocol::{Frame, PROTOCOL_VERSION};
use crate::registry::DocRegistry;
use crate::room::{CLOSE_NORMAL, CLOSE_RESYNC_REQUIRED, CloseSignal, DocRoom, close_signal};

/// Shared application state handed to every handler.
#[derive(Clone)]
pub struct SessionState {
    pub registry: DocRegistry,
    pub config: Arc<Config>,
    /// Readiness probes served by `GET /health/ready` (see `server::Readiness`).
    pub readiness: crate::server::Readiness,
}

/// Application-level close codes (WS 4000–4999 are safe for apps to define).
pub mod codes {
    pub const PROTOCOL: u16 = 4000;
    pub const BAD_DOC_ID: u16 = 4001;
    pub const FORBIDDEN: u16 = 4003;
    pub const LIMIT: u16 = 4029;
    pub const TOO_LARGE: u16 = 4130;
    pub const INTERNAL: u16 = 4500;
}

/// Run a WebSocket connection to completion.
///
/// `doc_id` is the path parameter (already validated); the HELLO frame's doc id
/// must match it.
pub async fn run_socket(mut socket: WebSocket, state: SessionState, doc_id: DocId) {
    // The room is the sole owner of `tx`; dropping it from the session map is
    // observable here as `rx` going `None` and is how a server-initiated
    // disconnect terminates this task.
    let (tx, mut rx) = mpsc::channel::<Frame>(256);
    let close = close_signal(CLOSE_NORMAL);

    // ---- handshake: wait for the first binary HELLO frame --------------------
    // Bounded by a timeout: a peer that never speaks (or stalls mid-hello) cannot
    // hold this task and its channel allocation alive forever.
    let timeout = std::time::Duration::from_millis(state.config.handshake_timeout_ms);
    let hello_bytes = match tokio::time::timeout(timeout, first_binary(&mut socket)).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return, // socket closed before any HELLO
        Err(_) => {
            let _ = send_error(
                &mut socket,
                codes::LIMIT,
                &format!(
                    "handshake timed out after {}ms: no valid HELLO received",
                    state.config.handshake_timeout_ms
                ),
            )
            .await;
            let _ = socket.close().await;
            return;
        }
    };
    // The HELLO arrived within budget; the rest of the handshake (token
    // resolution, possibly a network call to the authorizer) is also bounded so a
    // slow/hung authorizer cannot hold the session hostage either.
    let outcome = tokio::time::timeout(
        timeout,
        handshake(
            &mut socket,
            &state,
            &doc_id,
            &hello_bytes,
            tx,
            close.clone(),
        ),
    )
    .await;
    let (peer, role, token, room, generation) = match outcome {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(_) => {
            let _ = send_error(
                &mut socket,
                codes::LIMIT,
                &format!(
                    "handshake timed out after {}ms: token resolution did not complete",
                    state.config.handshake_timeout_ms
                ),
            )
            .await;
            let _ = socket.close().await;
            return;
        }
    };

    // After a successful handshake the room owns the sender; the welcome is
    // already queued as the first outbound frame.

    // ---- main loop: read ↔ write multiplex ----------------------------------
    let mut rate = FrameRateLimiter::new(state.config.max_frames_per_second);
    let session = InboundSession {
        room: &room,
        peer,
        role,
        token: &token,
        state: &state,
        doc_id: &doc_id,
    };
    loop {
        tokio::select! {
            biased;
            frame = rx.recv() => match frame {
                Some(f) => {
                    if socket
                        .send(Message::Binary(bytes::Bytes::from(f.encode())))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                None => break, // room dropped our sender (eviction) or shutdown
            },
            msg = socket.recv() => match msg {
                Some(Ok(message)) => {
                    if !handle_message(&message, &session, &mut socket, &mut rate).await {
                        break;
                    }
                }
                Some(Err(_)) | None => break,
            },
        }
    }

    // Tear down. If the room set a close code (e.g. resync-required for a slow
    // peer), surface it both as a protocol Error frame and as the WS close code
    // so a well-behaved client knows to reconnect with a full resync.
    let code = close.load(std::sync::atomic::Ordering::Acquire);
    if code != CLOSE_NORMAL {
        let _ = send_error(&mut socket, code, close_reason(code)).await;
    }
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code,
            reason: close_reason(code).into(),
        })))
        .await;

    // Generation-fenced leave: a late close for a session that has since been
    // re-admitted must not evict the replacement.
    room.leave(peer, generation);
}

/// Human-readable close reason for a close code (sent both as a protocol Error
/// message and as the WS close-frame reason).
fn close_reason(code: u16) -> &'static str {
    match code {
        CLOSE_RESYNC_REQUIRED => "resync required: peer fell behind; reconnect to catch up",
        _ => "connection closed",
    }
}

/// Fixed-window inbound frame rate limiter for one session.
///
/// Counts processed inbound binary frames per one-second window and rejects
/// further frames once [`Config::max_frames_per_second`] is hit in that window,
/// signalling the room to evict the peer. This is the aggregate backstop under
/// the per-frame byte caps: it bounds the CPU cost of decode + op-log append +
/// fan-out that a chatty-byte-limited peer could otherwise spin at.
struct FrameRateLimiter {
    limit: u32,
    window_start: std::time::Instant,
    frames: u32,
}

impl FrameRateLimiter {
    fn new(limit: u32) -> Self {
        Self {
            limit,
            window_start: std::time::Instant::now(),
            frames: 0,
        }
    }

    /// Return true if a frame is within budget (and count it).
    fn allow(&mut self) -> bool {
        let now = std::time::Instant::now();
        if now.saturating_duration_since(self.window_start) >= std::time::Duration::from_secs(1) {
            self.window_start = now;
            self.frames = 0;
        }
        self.frames += 1;
        self.frames <= self.limit
    }
}

/// Wait for the first inbound binary frame (the HELLO). Returns `None` if the
/// socket closes first.
async fn first_binary(socket: &mut WebSocket) -> Option<Vec<u8>> {
    loop {
        match socket.recv().await? {
            Ok(Message::Binary(b)) => return Some(b.to_vec()),
            Ok(Message::Close(_)) | Err(_) => return None,
            // Ignore Text/Ping/Pong until we see the HELLO binary frame.
            Ok(_) => {}
        }
    }
}

/// Validate the HELLO frame and join the room. Returns the peer, role, bearer,
/// room, and admission generation on success. The bearer is retained so
/// mutating frames can be re-authorized after membership changes.
async fn handshake(
    socket: &mut WebSocket,
    state: &SessionState,
    doc_id: &DocId,
    hello_bytes: &[u8],
    tx: mpsc::Sender<Frame>,
    close: CloseSignal,
) -> Option<(PeerId, Role, String, Arc<DocRoom>, u64)> {
    let frame = match Frame::decode(hello_bytes, state.config.max_update_bytes) {
        Ok(f) => f,
        Err(e) => {
            let _ = send_error(socket, codes::PROTOCOL, &e.to_string()).await;
            return None;
        }
    };
    let Frame::Hello {
        proto,
        doc_id: hello_doc,
        peer,
        token,
        last_vv,
    } = frame
    else {
        let _ = send_error(socket, codes::PROTOCOL, "expected HELLO first").await;
        return None;
    };

    if proto != PROTOCOL_VERSION {
        let _ = send_error(
            socket,
            codes::PROTOCOL,
            &format!("protocol mismatch: client={proto}, server={PROTOCOL_VERSION}"),
        )
        .await;
        return None;
    }
    if hello_doc != doc_id.as_str() {
        let _ = send_error(
            socket,
            codes::BAD_DOC_ID,
            "HELLO doc id does not match path",
        )
        .await;
        return None;
    }
    let peer = match PeerId::new(peer) {
        Ok(p) => p,
        Err(e) => {
            let _ = send_error(socket, codes::BAD_DOC_ID, &e.to_string()).await;
            return None;
        }
    };
    // Bound the two unbounded HELLO fields beyond the coarse 4 MiB frame cap:
    // frame decode alone lets a multi-megabyte token or version vector through,
    // and the token is about to be handed to the (potentially remote) access
    // resolver while the vv is fed to the authority.
    for (kind, len, max) in [
        ("HELLO token", token.len(), crate::config::MAX_TOKEN_BYTES),
        (
            "HELLO version vector",
            last_vv.len(),
            crate::config::MAX_VV_BYTES,
        ),
    ] {
        if let Err(e) = state.config.check_hello_field_size(kind, len, max) {
            let _ = send_error(socket, codes::TOO_LARGE, &e.to_string()).await;
            return None;
        }
    }
    let role = match state.registry.access().resolve(doc_id, &token).await {
        Ok(r) => r,
        Err(e) => {
            let _ = send_error(socket, codes::FORBIDDEN, &e.to_string()).await;
            return None;
        }
    };
    let room = match state.registry.get_or_open(doc_id).await {
        Ok(r) => r,
        Err(e) => {
            let _ = send_error(socket, codes::INTERNAL, &e.to_string()).await;
            return None;
        }
    };
    let initial_state = Vec::new();
    match room.join(peer, role, &last_vv, initial_state, tx, close) {
        Ok(outcome) => Some((peer, role, token, room, outcome.generation)),
        Err(e) => {
            let code = match &e {
                crate::error::SyncError::Limit(_) => codes::LIMIT,
                crate::error::SyncError::Access(_) => codes::FORBIDDEN,
                _ => codes::INTERNAL,
            };
            let _ = send_error(socket, code, &e.to_string()).await;
            None
        }
    }
}

struct InboundSession<'a> {
    room: &'a Arc<DocRoom>,
    peer: PeerId,
    role: Role,
    token: &'a str,
    state: &'a SessionState,
    doc_id: &'a DocId,
}

/// Handle one inbound WebSocket message. Returns `false` to terminate the session.
async fn handle_message(
    message: &Message,
    session: &InboundSession<'_>,
    socket: &mut WebSocket,
    rate: &mut FrameRateLimiter,
) -> bool {
    let bytes = match message {
        Message::Binary(b) => b,
        Message::Close(_) => return false,
        // Ping/Pong are auto-answered by axum; Text is not part of the protocol.
        _ => return true,
    };
    // Rate gate before decode: a peer exceeding the frame budget is evicted by
    // the room rather than burning CPU on frame decode + op-log append + fan-out.
    if !rate.allow() {
        let _ = session.room.evict_for_rate_limit(session.peer);
        let _ = send_error(
            socket,
            codes::LIMIT,
            &format!(
                "inbound frame rate exceeded (max {} frames/second)",
                session.state.config.max_frames_per_second
            ),
        )
        .await;
        return false;
    }
    let frame = match Frame::decode(bytes, session.state.config.max_update_bytes) {
        Ok(f) => f,
        Err(e) => {
            let _ = send_error(socket, codes::PROTOCOL, &e.to_string()).await;
            return true;
        }
    };
    match frame {
        Frame::Update(b) => {
            // Memberships can be changed while a socket is open. Re-authorize
            // every mutating frame so removal/downgrade takes effect immediately
            // instead of leaving a stale author session live until reconnect.
            let Some(current_role) = refresh_access(
                session.role,
                session.token,
                session.state,
                session.doc_id,
                socket,
            )
            .await
            else {
                return false;
            };
            if let Err(e) = session
                .room
                .handle_update(session.peer, current_role, &b)
                .await
            {
                let code = match &e {
                    // A review-policy violation is a permission-style denial.
                    crate::error::SyncError::Access(_)
                    | crate::error::SyncError::ReviewPolicy(_) => codes::FORBIDDEN,
                    crate::error::SyncError::Limit(_) => codes::TOO_LARGE,
                    // Undecodable Loro bytes are a *client* input problem, not
                    // a server fault: report it as a protocol error (4000)
                    // instead of the misleading 4500 "internal error".
                    crate::error::SyncError::Loro(_) => codes::PROTOCOL,
                    _ => codes::INTERNAL,
                };
                let _ = send_error(socket, code, &e.to_string()).await;
            }
            true
        }
        Frame::Presence(state) => {
            if let Err(e) = session.room.handle_presence(session.peer, state) {
                let _ = send_error(socket, codes::TOO_LARGE, &e.to_string()).await;
            }
            true
        }
        Frame::Heartbeat => {
            // Heartbeats make revocation proactive even when the removed user
            // is idle. Without this check the stale editor was only locked after
            // their next edit had already been accepted locally.
            if refresh_access(
                session.role,
                session.token,
                session.state,
                session.doc_id,
                socket,
            )
            .await
            .is_none()
            {
                return false;
            }
            let _ = session.room.handle_heartbeat(session.peer);
            true
        }
        Frame::Bye => false,
        // Any other inbound type is a protocol error but not fatal.
        other => {
            let _ = send_error(
                socket,
                codes::PROTOCOL,
                &format!("unexpected inbound {:?}", other.msg_type()),
            )
            .await;
            true
        }
    }
}

async fn refresh_access(
    role: Role,
    token: &str,
    state: &SessionState,
    doc_id: &DocId,
    socket: &mut WebSocket,
) -> Option<Role> {
    match state.registry.access().resolve(doc_id, token).await {
        Ok(current) if current == role => Some(current),
        Ok(current) => {
            let _ = send_error(
                socket,
                codes::FORBIDDEN,
                &format!("project access changed from {role} to {current}; reopen the project"),
            )
            .await;
            None
        }
        Err(_) => {
            let _ = send_error(socket, codes::FORBIDDEN, "project access was revoked").await;
            None
        }
    }
}

async fn send_error(socket: &mut WebSocket, code: u16, msg: &str) -> Result<(), axum::Error> {
    socket
        .send(Message::Binary(bytes::Bytes::from(
            Frame::Error {
                code,
                msg: msg.to_string(),
            }
            .encode(),
        )))
        .await
}
