//! Error model for the sync service.
//!
//! Every fallible operation in the library returns [`SyncError`] (or a narrow
//! domain-specific error that converts into it). The transport layer maps these
//! to [`crate::protocol::Frame::Error`] messages so clients receive actionable
//! codes rather than opaque panics.

use crate::protocol::MsgType;

/// Top-level error for the sync service.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// A frame did not conform to the wire protocol.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtoError),
    /// The document id was rejected by the validation rules in [`crate::config`].
    #[error("invalid document id: {0}")]
    InvalidDocId(String),
    /// Authentication or authorization failed.
    #[error("access denied: {0}")]
    Access(#[from] crate::auth::AuthError),
    /// A configured limit (size, peer count, ...) was exceeded.
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// The underlying Loro CRDT rejected an operation.
    #[error("loro error: {0}")]
    Loro(String),
    /// A reviewer update violated the review-layer policy (e.g. an attempt to
    /// overwrite the document text without a corresponding review record).
    #[error("review policy: {0}")]
    ReviewPolicy(String),
    /// A storage backend (op log, snapshot store) failed.
    #[error("storage error: {0}")]
    Storage(String),
    /// The connection was closed or the peer misbehaved during the handshake.
    #[error("handshake error: {0}")]
    Handshake(String),
}

/// Errors produced by the wire codec.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("unexpected end of input")]
    Truncated,
    #[error("invalid utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("unknown message tag {0}")]
    UnknownTag(u8),
    #[error("unexpected message type {0:?}")]
    UnexpectedMsg(MsgType),
    #[error("length prefix {0} exceeds configured maximum")]
    TooLong(usize),
}

impl From<loro::LoroError> for SyncError {
    fn from(err: loro::LoroError) -> Self {
        Self::Loro(err.to_string())
    }
}

impl From<loro::LoroEncodeError> for SyncError {
    fn from(err: loro::LoroEncodeError) -> Self {
        Self::Loro(err.to_string())
    }
}

impl From<std::io::Error> for SyncError {
    fn from(err: std::io::Error) -> Self {
        Self::Storage(err.to_string())
    }
}

/// Shorthand result alias.
pub type SyncResult<T> = Result<T, SyncError>;
