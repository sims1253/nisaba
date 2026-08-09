//! # nisaba-core
//!
//! Pure domain model for Nisaba, a collaborative authoring platform
//! built on Typst.
//!
//! This crate contains the deterministic document behavior shared by every adapter:
//!
//! - a general project of path-addressed documents ([`project`]);
//! - three-layer documents containing text, review marks, and structured data
//!   ([`document`], [`mark`]);
//! - projections for baseline, proposed, redline, public, and editor views
//!   ([`projection`], [`redline` mod](mod@redline));
//! - deterministic accept/reject resolution, range remapping, and orphan detection
//!   ([`resolution`], [`validation`]).
//!
//! ## Design principles (enforced)
//!
//! - **Unicode character positions, not byte offsets.** See [`position`]: [`Position`] is a
//!   newtype over a Unicode-scalar count and there is no public constructor that takes a
//!   byte offset.
//! - **Independent of the CRDT.** There is no Loro type anywhere in this crate. Marks are
//!   expressed over a concrete `&str` and integer positions; the sync service translates
//!   CRDT identifiers to and from [`Position`].
//! - **Pure and deterministic.** Every function here is total over its inputs and produces
//!   stable output regardless of insertion order.
//! - **Graceful, never panicking.** Out-of-bounds marks are clamped by the projection and
//!   surfaced by [`validation`]; a tracked deletion spanning unbalanced Typst syntax falls
//!   back to a block-level "replaced" region rather than emitting broken markup.
//!
//! ## Example
//!
//! ```
//! use nisaba_core::prelude::*;
//!
//! // A document with one pending insertion and one pending deletion.
//! let mut doc = Document::from_text("ABCDE");
//! doc.add_mark(Mark::new(
//!     MarkId::new(1),
//!     TextRange::new(Position::from_char_idx(1), Position::from_char_idx(2)),
//!     MarkKind::Insert,
//!     AuthorId::new("alice"),
//!     Timestamp::new(1),
//!     None,
//! ));
//! doc.add_mark(Mark::new(
//!     MarkId::new(2),
//!     TextRange::new(Position::from_char_idx(3), Position::from_char_idx(4)),
//!     MarkKind::Delete,
//!     AuthorId::new("alice"),
//!     Timestamp::new(2),
//!     None,
//! ));
//!
//! assert_eq!(doc.project(View::Baseline), "ACDE"); // drop insert, keep delete
//! assert_eq!(doc.project(View::Proposed), "ABCE"); // keep insert, drop delete
//! assert_eq!(doc.project(View::Editor), "ABCDE");  // full source
//! assert!(doc.is_valid());
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

pub mod document;
pub mod mark;
pub mod position;
pub mod project;
pub mod projection;
pub mod redline;
pub mod resolution;
pub mod review;
pub mod validation;

// Primary re-exports for convenience.
pub use document::{Data, Document, FieldValue};
pub use mark::{AuthorId, Mark, MarkId, MarkKind, MarkSet, Timestamp};
pub use position::{Position, TextRange, char_len};
pub use project::{DocumentPath, InvalidDocumentPath, Project, ProjectDocument, ProjectId};
pub use projection::{CharVisibility, View, project, project_with};
pub use redline::{
    InlineSafety, RedlineReport, RedlineRunRecord, RedlineStyle, ReplacedReason,
    classify_inline_safety, redline, redline_with_report,
};
pub use resolution::{
    Resolution, ResolutionError, ResolutionOutcome, accept, reject, remap_position_after_deletion,
    remap_range_after_deletion, remove_char_range, resolve,
};
pub use review::{
    BulkResolutionOutcome, CommentMessage, CommentState, CommentThread, CommentThreadId, Milestone,
    MilestoneComparison, ProjectedSnapshot, SnapshotChange, SnapshotPath, compare_milestones,
    refresh_comment_threads, resolve_bulk,
};
pub use validation::{ValidationIssue, is_valid, validate};

/// A prelude bringing the most-used types into scope.
pub mod prelude {
    pub use crate::document::{Data, Document, FieldValue};
    pub use crate::mark::{AuthorId, Mark, MarkId, MarkKind, MarkSet, Timestamp};
    pub use crate::position::{Position, TextRange, char_len};
    pub use crate::project::{
        DocumentPath, InvalidDocumentPath, Project, ProjectDocument, ProjectId,
    };
    pub use crate::projection::{View, project, project_with};
    pub use crate::redline::{RedlineStyle, redline};
    pub use crate::resolution::{Resolution, accept, reject};
    pub use crate::review::{
        CommentMessage, CommentState, CommentThread, CommentThreadId, Milestone,
        MilestoneComparison, ProjectedSnapshot, SnapshotChange, SnapshotPath, compare_milestones,
        refresh_comment_threads, resolve_bulk,
    };
}
