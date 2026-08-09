//! Accept / reject resolution and range remapping.
//!
//! This component implements mark semantics: the
//! deterministic, sequential resolution of a single tracked change against a concrete
//! `(text, marks)` snapshot.
//!
//! ## What is here
//!
//! - [`remap_position_after_deletion`] and [`remap_range_after_deletion`]: the core range
//!   transformation that keeps every other mark valid after a span of characters is
//!   removed. This is the rule Overleaf's `ranges-tracker` rebases onto integer
//!   positions: characters before the deletion are unaffected; characters after shift left
//!   by the deleted length; characters inside collapse onto the deletion's left edge.
//! - [`resolve`]: accept or reject one change mark. Deleting text (accept-delete /
//!   reject-insert) removes the spanned characters and remaps every surviving mark.
//!   Non-destructive resolutions (accept-insert / reject-delete) simply drop the mark.
//!
//! ## What is deliberately not here
//!
//! *Concurrent* accept/reject — two users resolving the same change at once — is a CRDT
//! concern and belongs to the Loro binding, not the pure model. The pure model resolves a
//! single operation deterministically; the CRDT layer guarantees the *operations*
//! themselves converge, so every replica applying the same operations in causal order
//! reaches the same text and marks. The PLAN's "fuzz test of concurrent random edits +
//! marks never produces a mark whose range is invalid against the text" (§8 M2) is
//! satisfied compositionally: each single resolution preserves the invariant
//! ([`resolve`] never produces an out-of-bounds mark), and convergence of the op stream is
//! the CRDT's job.

use crate::Position;
use crate::mark::{MarkId, MarkKind, MarkSet};
use crate::position::{TextRange, char_len};

/// Which way to resolve a pending change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Accept the change: insertions become permanent, deletions are applied.
    Accept,
    /// Reject the change: insertions are discarded, deletions are cancelled.
    Reject,
}

/// Why a resolution could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    /// No mark with the requested id exists.
    MarkNotFound(MarkId),
    /// The mark is not a tracked change (it is a comment or secret). Those are dismissed
    /// via [`MarkSet::remove`](crate::mark::MarkSet::remove), not accept/reject.
    NotAChange {
        /// The id that was requested.
        id: MarkId,
        /// The kind that was found.
        kind: MarkKind,
    },
}

impl std::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolutionError::MarkNotFound(id) => write!(f, "no mark with id {id}"),
            ResolutionError::NotAChange { id, kind } => {
                write!(f, "mark {id} is a {kind}, not a tracked change")
            }
        }
    }
}

impl std::error::Error for ResolutionError {}

/// Outcome of resolving a single change.
#[derive(Debug, Clone)]
pub struct ResolutionOutcome {
    /// The new text.
    pub text: String,
    /// The new mark set (the resolved mark removed, all others remapped).
    pub marks: MarkSet,
    /// Whether characters were removed from the text.
    pub text_changed: bool,
    /// The character range that was removed from the text, if any.
    pub removed_range: Option<TextRange>,
    /// Number of surviving marks whose range was adjusted.
    pub remapped: usize,
    /// Ids of marks whose range became empty as a result (e.g. a comment fully inside an
    /// accepted deletion). The caller surfaces these as orphaned.
    pub emptied: Vec<MarkId>,
}

/// Remap a single position after the characters `[del_start, del_end)` were removed.
///
/// Positions at or before `del_start` are unchanged; positions at or after `del_end` shift
/// left by `del_end - del_start`; positions inside the deleted region collapse onto
/// `del_start`.
#[must_use]
pub fn remap_position_after_deletion(
    pos: Position,
    del_start: Position,
    del_end: Position,
) -> Position {
    if pos <= del_start {
        pos
    } else if pos >= del_end {
        // Shift left by the deleted length.
        let d = del_end
            .to_char_idx()
            .saturating_sub(del_start.to_char_idx());
        Position::from_char_idx(
            u32::try_from(pos.to_char_idx().saturating_sub(d)).unwrap_or(u32::MAX),
        )
    } else {
        // Inside the deleted region: collapse to its left edge.
        del_start
    }
}

/// Remap a range after the characters `[del_start, del_end)` were removed.
///
/// The resulting range covers exactly the surviving characters the original range covered,
/// contiguous and in order. A range entirely inside the deletion collapses to the empty
/// range `[del_start, del_start)`.
#[must_use]
pub fn remap_range_after_deletion(
    range: TextRange,
    del_start: Position,
    del_end: Position,
) -> TextRange {
    let new_start = remap_position_after_deletion(range.start, del_start, del_end);
    let new_end = remap_position_after_deletion(range.end, del_start, del_end);
    // The position map is monotonic non-decreasing, so new_start <= new_end always.
    TextRange::new(new_start, new_end)
}

/// Remove the characters `[start, end)` (by Unicode scalar index) from `text`, returning a
/// new `String`.
#[must_use]
pub fn remove_char_range(text: &str, start: usize, end: usize) -> String {
    if start >= end || start >= char_len(text) {
        return text.to_string();
    }
    let end = end.min(char_len(text));
    let mut out = String::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        if !(start..end).contains(&i) {
            out.push(ch);
        }
    }
    out
}

/// Resolve (accept or reject) a single change mark against `(text, marks)`.
///
/// Returns the new text and mark set together with a report. See the component documentation for the
/// remapping rule and the scope of "concurrent" resolution.
pub fn resolve(
    text: &str,
    marks: &MarkSet,
    id: MarkId,
    resolution: Resolution,
) -> Result<ResolutionOutcome, ResolutionError> {
    let mark = marks
        .get(id)
        .ok_or(ResolutionError::MarkNotFound(id))?
        .clone();
    if !mark.kind.is_change() {
        return Err(ResolutionError::NotAChange {
            id,
            kind: mark.kind,
        });
    }

    // Determine whether this resolution removes text: rejecting an insertion discards
    // its characters, and accepting a deletion applies it. All other combos leave the
    // text in place.
    let removes_text = matches!(
        (mark.kind, resolution),
        (MarkKind::Insert, Resolution::Reject) | (MarkKind::Delete, Resolution::Accept)
    );

    if !removes_text {
        // Drop the mark; nothing else moves.
        let mut new_marks = marks.clone();
        new_marks.remove(id);
        return Ok(ResolutionOutcome {
            text: text.to_string(),
            marks: new_marks,
            text_changed: false,
            removed_range: None,
            remapped: 0,
            emptied: Vec::new(),
        });
    }

    // Destructive path: remove the mark's characters and remap every surviving mark.
    let n = char_len(text);
    let del_start = mark.range.start.to_char_idx().min(n);
    let del_end = mark.range.end.to_char_idx().min(n);
    if del_start >= del_end {
        let mut new_marks = marks.clone();
        new_marks.remove(id);
        return Ok(ResolutionOutcome {
            text: text.to_string(),
            marks: new_marks,
            text_changed: false,
            removed_range: None,
            remapped: 0,
            emptied: Vec::new(),
        });
    }
    let new_text = remove_char_range(text, del_start, del_end);
    let del_s = Position::from_char_idx(u32::try_from(del_start).unwrap_or(u32::MAX));
    let del_e = Position::from_char_idx(u32::try_from(del_end).unwrap_or(u32::MAX));

    let mut new_marks = MarkSet::new();
    let mut remapped = 0usize;
    let mut emptied = Vec::new();
    for m in marks.iter() {
        if m.id == id {
            continue;
        }
        let mut nm = m.clone();
        let before = nm.range;
        nm.range = remap_range_after_deletion(nm.range, del_s, del_e);
        if nm.range != before {
            remapped += 1;
        }
        if !before.is_empty() && nm.range.is_empty() {
            emptied.push(nm.id);
        }
        new_marks.insert(nm);
    }

    Ok(ResolutionOutcome {
        text: new_text,
        marks: new_marks,
        text_changed: true,
        removed_range: Some(TextRange::new(del_s, del_e)),
        remapped,
        emptied,
    })
}

/// Accept the change identified by `id`. See [`resolve`].
pub fn accept(
    text: &str,
    marks: &MarkSet,
    id: MarkId,
) -> Result<ResolutionOutcome, ResolutionError> {
    resolve(text, marks, id, Resolution::Accept)
}

/// Reject the change identified by `id`. See [`resolve`].
pub fn reject(
    text: &str,
    marks: &MarkSet,
    id: MarkId,
) -> Result<ResolutionOutcome, ResolutionError> {
    resolve(text, marks, id, Resolution::Reject)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mark::{AuthorId, Mark, Timestamp};
    use crate::projection::{View, project};

    fn change(id: u64, kind: MarkKind, s: u32, e: u32, author: &str) -> Mark {
        Mark::new(
            MarkId::new(id),
            TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
            kind,
            AuthorId::new(author),
            Timestamp::new(id),
            None,
        )
    }

    fn set(marks: &[Mark]) -> MarkSet {
        marks.iter().cloned().collect()
    }

    #[test]
    fn accept_insert_keeps_text_drops_mark() {
        let text = "ABCDE"; // insert over B (1..2)
        let marks = set(&[change(1, MarkKind::Insert, 1, 2, "alice")]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        assert!(!out.text_changed);
        assert_eq!(out.text, "ABCDE");
        assert_eq!(out.marks.len(), 0);
    }

    #[test]
    fn reject_insert_removes_text() {
        let text = "ABCDE"; // insert over B
        let marks = set(&[change(1, MarkKind::Insert, 1, 2, "alice")]);
        let out = reject(text, &marks, MarkId::new(1)).unwrap();
        assert!(out.text_changed);
        assert_eq!(out.text, "ACDE");
        assert_eq!(out.marks.len(), 0);
    }

    #[test]
    fn accept_delete_removes_text() {
        let text = "ABCDE"; // delete over D (3..4)
        let marks = set(&[change(1, MarkKind::Delete, 3, 4, "alice")]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        assert_eq!(out.text, "ABCE");
        assert_eq!(out.marks.len(), 0);
    }

    #[test]
    fn reject_delete_keeps_text_drops_mark() {
        let text = "ABCDE";
        let marks = set(&[change(1, MarkKind::Delete, 3, 4, "alice")]);
        let out = reject(text, &marks, MarkId::new(1)).unwrap();
        assert_eq!(out.text, "ABCDE");
        assert_eq!(out.marks.len(), 0);
    }

    #[test]
    fn remap_shifts_marks_after_deletion() {
        // delete [2,4); a later comment at [5,7) should shift to [3,5).
        let text = "ABCDEFGH";
        let marks = set(&[
            change(1, MarkKind::Delete, 2, 4, "alice"),
            Mark::new(
                MarkId::new(9),
                TextRange::new(Position::from_char_idx(5), Position::from_char_idx(7)),
                MarkKind::Comment,
                AuthorId::new("bob"),
                Timestamp::new(1),
                None,
            ),
        ]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        assert_eq!(out.text, "ABEFGH");
        let c = out.marks.get(MarkId::new(9)).unwrap();
        assert_eq!(
            c.range,
            TextRange::new(Position::from_char_idx(3), Position::from_char_idx(5))
        );
    }

    #[test]
    fn remap_keeps_marks_before_deletion() {
        let text = "ABCDEFGH";
        let marks = set(&[
            change(1, MarkKind::Delete, 4, 6, "alice"),
            change(2, MarkKind::Insert, 0, 1, "alice"),
        ]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        let ins = out.marks.get(MarkId::new(2)).unwrap();
        assert_eq!(
            ins.range,
            TextRange::new(Position::from_char_idx(0), Position::from_char_idx(1))
        );
    }

    #[test]
    fn remap_overlapping_change_clips_to_survivors() {
        // delete A:[2,6); insert B:[4,8). Accept A. B's chars in [4,6) are removed; B's
        // chars [6,8) shift left by 4 to [2,4). So B becomes [2,4).
        let text = "ABCDEFGH";
        let marks = set(&[
            change(1, MarkKind::Delete, 2, 6, "alice"),
            change(2, MarkKind::Insert, 4, 8, "bob"),
        ]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        assert_eq!(out.text, "ABGH"); // removed CDEF
        let b = out.marks.get(MarkId::new(2)).unwrap();
        assert_eq!(
            b.range,
            TextRange::new(Position::from_char_idx(2), Position::from_char_idx(4))
        );
    }

    #[test]
    fn comment_inside_accepted_deletion_becomes_empty_and_orphaned() {
        // delete [2,5); comment [3,4) entirely inside. Accept delete → comment emptied.
        let text = "ABCDEFGH";
        let marks = set(&[
            change(1, MarkKind::Delete, 2, 5, "alice"),
            Mark::new(
                MarkId::new(9),
                TextRange::new(Position::from_char_idx(3), Position::from_char_idx(4)),
                MarkKind::Comment,
                AuthorId::new("bob"),
                Timestamp::new(1),
                None,
            ),
        ]);
        let out = accept(text, &marks, MarkId::new(1)).unwrap();
        assert!(out.emptied.contains(&MarkId::new(9)));
        let c = out.marks.get(MarkId::new(9)).unwrap();
        assert!(c.range.is_empty());
    }

    #[test]
    fn comment_reject_is_error() {
        let text = "ABCDE";
        let marks = set(&[Mark::new(
            MarkId::new(9),
            TextRange::new(Position::from_char_idx(1), Position::from_char_idx(2)),
            MarkKind::Comment,
            AuthorId::new("bob"),
            Timestamp::new(1),
            None,
        )]);
        assert!(matches!(
            reject(text, &marks, MarkId::new(9)),
            Err(ResolutionError::NotAChange { .. })
        ));
    }

    #[test]
    fn resolution_then_projection_matches_proposed_baseline() {
        // Accepting every change should make baseline == proposed == the resulting text.
        let text = "ABCDE";
        let marks = set(&[
            change(1, MarkKind::Insert, 1, 2, "a"), // insert B
            change(2, MarkKind::Delete, 3, 4, "a"), // delete D
        ]);
        let mut t = text.to_string();
        let mut ms = marks.clone();
        for id in [MarkId::new(1), MarkId::new(2)] {
            let out = accept(&t, &ms, id).unwrap();
            t = out.text;
            ms = out.marks;
        }
        // After accepting both: text = "ABCE".
        assert_eq!(t, "ABCE");
        // Proposed view of the original should equal the fully-accepted text.
        assert_eq!(project(text, &marks, View::Proposed), "ABCE");
        // Baseline of the original should equal the fully-rejected text.
        let mut t2 = text.to_string();
        let mut ms2 = marks.clone();
        for id in [MarkId::new(1), MarkId::new(2)] {
            let out = reject(&t2, &ms2, id).unwrap();
            t2 = out.text;
            ms2 = out.marks;
        }
        assert_eq!(t2, project(text, &marks, View::Baseline));
    }

    #[test]
    fn remap_position_table() {
        let s = Position::from_char_idx(3);
        let e = Position::from_char_idx(6);
        // before
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(0), s, e),
            Position::from_char_idx(0)
        );
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(3), s, e),
            Position::from_char_idx(3)
        );
        // inside → collapse to s
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(4), s, e),
            Position::from_char_idx(3)
        );
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(5), s, e),
            Position::from_char_idx(3)
        );
        // at/after e → shift by 3
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(6), s, e),
            Position::from_char_idx(3)
        );
        assert_eq!(
            remap_position_after_deletion(Position::from_char_idx(9), s, e),
            Position::from_char_idx(6)
        );
    }

    #[test]
    fn remove_char_range_multibyte() {
        // "aéäXY": remove [1,3) (é, ä)
        assert_eq!(remove_char_range("aéäXY", 1, 3), "aXY");
    }
}
