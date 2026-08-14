//! Validation: is a `(text, marks)` pair well-formed, and are its marks sound?
//!
//! The projection function is total and never panics (out-of-bounds ranges are clamped),
//! but a model that contains out-of-bounds marks, insert+delete conflicts, or orphaned
//! comments is *semantically* broken. This component surfaces those conditions as a
//! deterministic list of [`ValidationIssue`]s so the editor and the export path can report
//! them — and so a fuzz test can assert the invariant "every mark range is valid against
//! the text".
//!
//! These rules cover the structural mark invariants that
//! does not require a CRDT:
//!
//! - ranges in bounds and well ordered;
//! - an insertion and a deletion covering the same character is a conflict;
//! - a comment whose entire range is covered by pending deletions is *orphaned* (never
//!   silently relocated).

use crate::Position;
use crate::mark::{Mark, MarkId, MarkKind, MarkSet};
use crate::position::{TextRange, char_len};

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationIssue {
    /// A mark range has `start > end`.
    InvertedRange {
        /// The offending mark id.
        id: crate::mark::MarkId,
        /// The offending range.
        range: TextRange,
    },
    /// A mark range extends past the end (or before the start) of the text.
    OutOfBounds {
        /// The offending mark id.
        id: crate::mark::MarkId,
        /// The offending range.
        range: TextRange,
        /// The character length of the text.
        text_len: u32,
    },
    /// An `insert` mark and a `delete` mark cover at least one common character.
    InsertDeleteConflict {
        /// The insert mark id.
        insert: crate::mark::MarkId,
        /// The delete mark id.
        delete: crate::mark::MarkId,
        /// The overlapping character range.
        overlap: TextRange,
    },
    /// A comment mark whose entire range is covered by pending deletions.
    OrphanedComment {
        /// The comment mark id.
        comment: crate::mark::MarkId,
        /// The comment's (now fully-deleted) range.
        range: TextRange,
    },
}

impl std::fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationIssue::InvertedRange { id, range } => {
                write!(f, "mark {id} has inverted range {range}")
            }
            ValidationIssue::OutOfBounds {
                id,
                range,
                text_len,
            } => {
                write!(
                    f,
                    "mark {id} range {range} is out of bounds (text is {text_len} chars)"
                )
            }
            ValidationIssue::InsertDeleteConflict {
                insert,
                delete,
                overlap,
            } => {
                write!(
                    f,
                    "insert {insert} and delete {delete} conflict over {overlap}"
                )
            }
            ValidationIssue::OrphanedComment { comment, range } => {
                write!(
                    f,
                    "comment {comment} over {range} is orphaned (entire range deleted)"
                )
            }
        }
    }
}

/// Validate `(text, marks)`. Returns every issue found, in a deterministic order.
///
/// Order: structural issues first (inverted, out-of-bounds), then conflicts (sorted by
/// insert id then delete id), then orphaned comments (by id).
#[must_use]
pub fn validate(text: &str, marks: &MarkSet) -> Vec<ValidationIssue> {
    let n = char_len(text);
    let n_u32 = u32::try_from(n).unwrap_or(u32::MAX);
    let mut issues = Vec::new();

    // 1. Structural per-mark issues.
    for m in marks.iter() {
        if !m.range.is_well_ordered() {
            issues.push(ValidationIssue::InvertedRange {
                id: m.id,
                range: m.range,
            });
            continue; // an inverted range can't be meaningfully bounds-checked
        }
        if m.range.start.to_char_idx() > n || m.range.end.to_char_idx() > n {
            issues.push(ValidationIssue::OutOfBounds {
                id: m.id,
                range: m.range,
                text_len: n_u32,
            });
        }
    }

    // 2. Insert/delete conflicts over shared characters. The per-character flag vector
    // (one pass) decides *whether* any character is both inserted and deleted; only then
    // are the (few) candidate marks paired up, instead of comparing every insert against
    // every delete up front.
    let flags = crate::projection::char_flags(text, marks);
    let has_conflict = flags.iter().any(|f| {
        f & crate::projection::FLAG_INSERT != 0 && f & crate::projection::FLAG_DELETE != 0
    });
    let deletes = marks.of_kind(MarkKind::Delete);
    if has_conflict {
        let mut conflicts: Vec<ValidationIssue> = Vec::new();
        let clamped = |m: &Mark| {
            // Mirrors `char_flags`: ranges are clamped to the text length, and a range
            // that covers nothing after clamping contributes no flags (and no conflict).
            let s = m.range.start.to_char_idx().min(n);
            let e = m.range.end.to_char_idx().min(n);
            (s < e).then_some((s, e))
        };
        let inserts: Vec<(usize, usize, MarkId)> = marks
            .iter()
            .filter(|m| m.kind == MarkKind::Insert)
            .filter_map(|m| clamped(m).map(|(s, e)| (s, e, m.id)))
            .collect();
        let deletes: Vec<(usize, usize, MarkId)> = deletes
            .iter()
            .filter_map(|m| clamped(m).map(|(s, e)| (s, e, m.id)))
            .collect();
        for (ins_s, ins_e, ins_id) in &inserts {
            for (del_s, del_e, del_id) in &deletes {
                let overlap_s = (*ins_s).max(*del_s);
                let overlap_e = (*ins_e).min(*del_e);
                if overlap_s < overlap_e {
                    conflicts.push(ValidationIssue::InsertDeleteConflict {
                        insert: *ins_id,
                        delete: *del_id,
                        overlap: TextRange::new(
                            Position::from_char_idx(u32::try_from(overlap_s).unwrap_or(u32::MAX)),
                            Position::from_char_idx(u32::try_from(overlap_e).unwrap_or(u32::MAX)),
                        ),
                    });
                }
            }
        }
        conflicts.sort_by_key(|c| match c {
            ValidationIssue::InsertDeleteConflict { insert, delete, .. } => {
                (insert.as_u64(), delete.as_u64())
            }
            _ => (0, 0),
        });
        // Deduplicate (shouldn't happen since pairs are unique, but defensive).
        conflicts.dedup();
        issues.extend(conflicts);
    }

    // 3. Orphaned comments: a comment whose entire range is covered by delete marks.
    let mut orphans: Vec<ValidationIssue> = Vec::new();
    for c in marks.of_kind(MarkKind::Comment) {
        if c.range.is_empty() {
            // An empty comment range over a point anchor is not "orphaned by deletion";
            // it is simply a point pin and is left to the editor to render.
            continue;
        }
        if is_range_fully_deleted(c.range, &deletes) {
            orphans.push(ValidationIssue::OrphanedComment {
                comment: c.id,
                range: c.range,
            });
        }
    }
    orphans.sort_by_key(|o| match o {
        ValidationIssue::OrphanedComment { comment, .. } => comment.as_u64(),
        _ => 0,
    });
    issues.extend(orphans);

    issues
}

/// Whether every character of `range` is covered by at least one delete mark.
///
/// This is the orphan predicate: a comment whose entire span would vanish from the
/// `proposed` view.
#[must_use]
pub fn is_range_fully_deleted(range: TextRange, deletes: &[&Mark]) -> bool {
    if range.is_empty() {
        return false;
    }
    // Merge delete coverage within `range` and check it spans the whole range.
    let mut covered: Vec<(u32, u32)> = Vec::new();
    for d in deletes {
        if let Some(inter) = d.range.intersect(range) {
            covered.push((
                u32::try_from(inter.start.to_char_idx()).unwrap_or(u32::MAX),
                u32::try_from(inter.end.to_char_idx()).unwrap_or(u32::MAX),
            ));
        }
    }
    if covered.is_empty() {
        return false;
    }
    covered.sort_unstable();
    // Sweep to check contiguous coverage from range.start to range.end.
    let end_u32 = u32::try_from(range.end.to_char_idx()).unwrap_or(u32::MAX);
    let mut cursor = u32::try_from(range.start.to_char_idx()).unwrap_or(u32::MAX);
    for (s, e) in covered {
        if s > cursor {
            return false; // gap
        }
        cursor = cursor.max(e);
        if cursor >= end_u32 {
            return true;
        }
    }
    cursor >= end_u32
}

/// Convenience: a model is clean iff [`validate`] finds nothing.
#[must_use]
pub fn is_valid(text: &str, marks: &MarkSet) -> bool {
    validate(text, marks).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;
    use crate::mark::{AuthorId, MarkId, Timestamp};

    fn m(id: u64, kind: MarkKind, s: u32, e: u32) -> Mark {
        Mark::new(
            MarkId::new(id),
            TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
            kind,
            AuthorId::new("alice"),
            Timestamp::new(id),
            None,
        )
    }

    fn set(marks: &[Mark]) -> MarkSet {
        marks.iter().cloned().collect()
    }

    #[test]
    fn clean_model_validates() {
        let marks = set(&[m(1, MarkKind::Insert, 0, 2), m(2, MarkKind::Delete, 3, 5)]);
        assert!(is_valid("ABCDE", &marks));
    }

    #[test]
    fn out_of_bounds_detected() {
        let marks = set(&[m(1, MarkKind::Insert, 0, 99)]);
        let issues = validate("ABC", &marks);
        assert!(issues.iter().any(
            |i| matches!(i, ValidationIssue::OutOfBounds { id, .. } if *id == MarkId::new(1))
        ));
    }

    #[test]
    fn inverted_range_detected() {
        let marks = set(&[m(1, MarkKind::Insert, 5, 2)]);
        let issues = validate("ABCDEF", &marks);
        assert!(issues.iter().any(
            |i| matches!(i, ValidationIssue::InvertedRange { id, .. } if *id == MarkId::new(1))
        ));
    }

    #[test]
    fn insert_delete_conflict_detected() {
        let marks = set(&[m(1, MarkKind::Insert, 0, 4), m(2, MarkKind::Delete, 2, 6)]);
        let issues = validate("ABCDEF", &marks);
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::InsertDeleteConflict { insert, delete, .. }
            if *insert == MarkId::new(1) && *delete == MarkId::new(2)
        )));
    }

    #[test]
    fn orphaned_comment_when_fully_deleted() {
        // Comment over [2,5), single delete over [2,5).
        let marks = set(&[m(9, MarkKind::Comment, 2, 5), m(2, MarkKind::Delete, 2, 5)]);
        let issues = validate("ABCDEF", &marks);
        assert!(issues.iter().any(|i| matches!(
            i,
            ValidationIssue::OrphanedComment { comment, .. } if *comment == MarkId::new(9)
        )));
    }

    #[test]
    fn orphaned_comment_when_covered_by_multiple_deletes() {
        // Comment over [2,6); deletes [2,4) and [4,6) together cover it.
        let marks = set(&[
            m(9, MarkKind::Comment, 2, 6),
            m(1, MarkKind::Delete, 2, 4),
            m(2, MarkKind::Delete, 4, 6),
        ]);
        let issues = validate("ABCDEFGH", &marks);
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, ValidationIssue::OrphanedComment { .. }))
        );
    }

    #[test]
    fn partially_deleted_comment_is_not_orphaned() {
        let marks = set(&[m(9, MarkKind::Comment, 2, 6), m(2, MarkKind::Delete, 3, 5)]);
        assert!(is_valid("ABCDEFGH", &marks));
    }

    #[test]
    fn empty_comment_not_flagged() {
        let marks = set(&[m(9, MarkKind::Comment, 3, 3)]);
        assert!(is_valid("ABCDEF", &marks));
    }
}
