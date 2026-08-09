//! Wire protocol: binary framing for the sync transport.
//!
//! Every WebSocket message is exactly one [`Frame`]. The codec is deliberately
//! self-describing and length-prefixed so that:
//!
//! * the relay can forward CRDT updates as **opaque bytes** (it never inspects or
//!   re-serialises Loro state),
//! * catch-up can carry either an incremental update or a full snapshot,
//! * presence travels out-of-band from CRDT history (it is ephemeral and must not
//!   be persisted into the append-only op log).
//!
//! ## Frame layout
//!
//! ```text
//! [u8 tag][tag-specific payload]
//! ```
//! Variable-length fields are `[u32 be length][bytes]`. Integers are big-endian.
//! Numbers below never appear in released order without a version bump, matching
//! [`PROTOCOL_VERSION`].

use crate::error::ProtoError;

/// Wire protocol version. Bumped on any breaking change to this component.
pub const PROTOCOL_VERSION: u8 = 1;

/// A message type tag. Stable across versions; reordering is a breaking change.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    /// Client → server: join a document. Carries the client's last version vector
    /// so the server can compute a catch-up payload.
    Hello = 1,
    /// Server → client: handshake result plus the catch-up payload.
    Welcome = 2,
    /// Bidirectional: an opaque Loro CRDT update.
    Update = 3,
    /// Server → client: a full Loro snapshot (catch-up fallback).
    Snapshot = 4,
    /// Bidirectional: ephemeral presence state (JSON).
    Presence = 5,
    /// Client → server: heartbeat. Server → client: roster refresh marker.
    Heartbeat = 6,
    /// Server → client: structured error.
    Error = 7,
    /// Client → server: graceful leave.
    Bye = 8,
}

impl MsgType {
    fn from_u8(b: u8) -> Result<Self, ProtoError> {
        Ok(match b {
            1 => Self::Hello,
            2 => Self::Welcome,
            3 => Self::Update,
            4 => Self::Snapshot,
            5 => Self::Presence,
            6 => Self::Heartbeat,
            7 => Self::Error,
            8 => Self::Bye,
            other => return Err(ProtoError::UnknownTag(other)),
        })
    }
}

/// Handshake outcome reported in [`Frame::Welcome`].
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WelcomeStatus {
    /// Joined and catch-up payload attached.
    Ok = 0,
    /// Joined but no catch-up payload (already up to date).
    OkNoCatchUp = 1,
}

/// The catch-up strategy the server chose for a reconnecting peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatchUp {
    /// Nothing to send.
    None,
    /// Incremental Loro updates since the peer's last version vector.
    Updates(Vec<u8>),
    /// A full snapshot, used when the history gap cannot be filled incrementally.
    Snapshot(Vec<u8>),
}

impl CatchUp {
    fn tag(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::Updates(_) => 1,
            Self::Snapshot(_) => 2,
        }
    }
}

/// A decoded protocol frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// `[u8 proto][str doc_id][u64 peer][str token][bytes last_vv]`
    Hello {
        proto: u8,
        doc_id: String,
        peer: u64,
        token: String,
        last_vv: Vec<u8>,
    },
    /// `[u8 status][str note][catchup]`
    Welcome {
        status: WelcomeStatus,
        note: String,
        catchup: CatchUp,
    },
    /// `[bytes]`
    Update(Vec<u8>),
    /// `[bytes]`
    Snapshot(Vec<u8>),
    /// `[bytes json]`
    Presence(Vec<u8>),
    /// `[]` (optionally understood as a ping by higher layers)
    Heartbeat,
    /// `[u16 code][str msg]`
    Error { code: u16, msg: String },
    /// `[]`
    Bye,
}

impl Frame {
    /// Returns the message type for this frame.
    #[must_use]
    pub fn msg_type(&self) -> MsgType {
        match self {
            Self::Hello { .. } => MsgType::Hello,
            Self::Welcome { .. } => MsgType::Welcome,
            Self::Update(_) => MsgType::Update,
            Self::Snapshot(_) => MsgType::Snapshot,
            Self::Presence(_) => MsgType::Presence,
            Self::Heartbeat => MsgType::Heartbeat,
            Self::Error { .. } => MsgType::Error,
            Self::Bye => MsgType::Bye,
        }
    }

    /// Encode this frame to its binary representation.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.msg_type() as u8);
        match self {
            Self::Hello {
                proto,
                doc_id,
                peer,
                token,
                last_vv,
            } => {
                out.push(*proto);
                write_str(&mut out, doc_id);
                write_u64(&mut out, *peer);
                write_str(&mut out, token);
                write_bytes(&mut out, last_vv);
            }
            Self::Welcome {
                status,
                note,
                catchup,
            } => {
                out.push(*status as u8);
                write_str(&mut out, note);
                out.push(catchup.tag());
                match catchup {
                    CatchUp::None => {}
                    CatchUp::Updates(b) | CatchUp::Snapshot(b) => write_bytes(&mut out, b),
                }
            }
            Self::Update(b) | Self::Snapshot(b) | Self::Presence(b) => write_bytes(&mut out, b),
            Self::Heartbeat | Self::Bye => {}
            Self::Error { code, msg } => {
                write_u16(&mut out, *code);
                write_str(&mut out, msg);
            }
        }
        out
    }

    /// Decode a frame from a complete binary message.
    ///
    /// `max_blob` bounds every length-prefixed blob read, defending against memory
    /// exhaustion from a hostile or buggy peer before the caller ever allocates.
    pub fn decode(buf: &[u8], max_blob: usize) -> Result<Self, ProtoError> {
        let mut cur = Cursor::new(buf, max_blob);
        let tag = cur.read_u8()?;
        let mt = MsgType::from_u8(tag)?;
        Ok(match mt {
            MsgType::Hello => {
                let proto = cur.read_u8()?;
                let doc_id = cur.read_str()?;
                let peer = cur.read_u64()?;
                let token = cur.read_str()?;
                let last_vv = cur.read_bytes()?;
                Self::Hello {
                    proto,
                    doc_id,
                    peer,
                    token,
                    last_vv,
                }
            }
            MsgType::Welcome => {
                let status = match cur.read_u8()? {
                    0 => WelcomeStatus::Ok,
                    1 => WelcomeStatus::OkNoCatchUp,
                    other => return Err(ProtoError::UnknownTag(other)),
                };
                let note = cur.read_str()?;
                let ctag = cur.read_u8()?;
                let catchup = match ctag {
                    0 => CatchUp::None,
                    1 => CatchUp::Updates(cur.read_bytes()?),
                    2 => CatchUp::Snapshot(cur.read_bytes()?),
                    other => return Err(ProtoError::UnknownTag(other)),
                };
                Self::Welcome {
                    status,
                    note,
                    catchup,
                }
            }
            MsgType::Update => Self::Update(cur.read_bytes()?),
            MsgType::Snapshot => Self::Snapshot(cur.read_bytes()?),
            MsgType::Presence => Self::Presence(cur.read_bytes()?),
            MsgType::Heartbeat => Self::Heartbeat,
            MsgType::Error => {
                let code = cur.read_u16()?;
                let msg = cur.read_str()?;
                Self::Error { code, msg }
            }
            MsgType::Bye => Self::Bye,
        })
    }
}

// ---- low-level codec helpers ------------------------------------------------

fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn write_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn write_str(out: &mut Vec<u8>, s: &str) {
    write_bytes(out, s.as_bytes());
}
fn write_bytes(out: &mut Vec<u8>, b: &[u8]) {
    let len = u32::try_from(b.len()).expect("blob length fits in u32");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(b);
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    max_blob: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8], max_blob: usize) -> Self {
        Self {
            buf,
            pos: 0,
            max_blob,
        }
    }

    fn read_u8(&mut self) -> Result<u8, ProtoError> {
        let b = self
            .buf
            .get(self.pos)
            .copied()
            .ok_or(ProtoError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u16(&mut self) -> Result<u16, ProtoError> {
        if self.pos + 2 > self.buf.len() {
            return Err(ProtoError::Truncated);
        }
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn read_u64(&mut self) -> Result<u64, ProtoError> {
        const N: usize = 8;
        if self.pos + N > self.buf.len() {
            return Err(ProtoError::Truncated);
        }
        let mut arr = [0u8; N];
        arr.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(u64::from_be_bytes(arr))
    }

    fn read_len(&mut self) -> Result<usize, ProtoError> {
        const N: usize = 4;
        if self.pos + N > self.buf.len() {
            return Err(ProtoError::Truncated);
        }
        let mut arr = [0u8; N];
        arr.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        let len = u32::from_be_bytes(arr) as usize;
        if len > self.max_blob {
            return Err(ProtoError::TooLong(len));
        }
        Ok(len)
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>, ProtoError> {
        let len = self.read_len()?;
        if self.pos + len > self.buf.len() {
            return Err(ProtoError::Truncated);
        }
        let v = self.buf[self.pos..self.pos + len].to_vec();
        self.pos += len;
        Ok(v)
    }

    fn read_str(&mut self) -> Result<String, ProtoError> {
        let bytes = self.read_bytes()?;
        Ok(std::str::from_utf8(&bytes)?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(f: &Frame) {
        let bytes = f.encode();
        let back = Frame::decode(&bytes, 1 << 20).expect("decode");
        assert_eq!(f, &back, "roundtrip mismatch");
    }

    #[test]
    fn frames_roundtrip() {
        roundtrip(&Frame::Hello {
            proto: PROTOCOL_VERSION,
            doc_id: "mod3_3-2-1".into(),
            peer: 42,
            token: "tok".into(),
            last_vv: vec![1, 2, 3],
        });
        roundtrip(&Frame::Welcome {
            status: WelcomeStatus::Ok,
            note: "welcome".into(),
            catchup: CatchUp::Updates(vec![9, 9]),
        });
        roundtrip(&Frame::Welcome {
            status: WelcomeStatus::OkNoCatchUp,
            note: String::new(),
            catchup: CatchUp::None,
        });
        roundtrip(&Frame::Welcome {
            status: WelcomeStatus::Ok,
            note: "snap".into(),
            catchup: CatchUp::Snapshot(vec![0xff; 10]),
        });
        roundtrip(&Frame::Update(vec![1, 2, 3, 4]));
        roundtrip(&Frame::Snapshot(vec![]));
        roundtrip(&Frame::Presence(b"{\"name\":\"alice\"}".to_vec()));
        roundtrip(&Frame::Heartbeat);
        roundtrip(&Frame::Error {
            code: 403,
            msg: "forbidden".into(),
        });
        roundtrip(&Frame::Bye);
    }

    #[test]
    fn truncated_frame_is_error() {
        let bytes = Frame::Update(vec![1, 2]).encode();
        // Drop the payload: leave tag + length only.
        let truncated = &bytes[..5];
        assert!(Frame::decode(truncated, 1 << 20).is_err());
    }

    #[test]
    fn oversized_blob_is_rejected() {
        let bytes = Frame::Update(vec![0; 100]).encode();
        assert!(Frame::decode(&bytes, 16).is_err());
    }

    #[test]
    fn unknown_tag_is_error() {
        assert!(Frame::decode(&[99], 16).is_err());
    }
}
