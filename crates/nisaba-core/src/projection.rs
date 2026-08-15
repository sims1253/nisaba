//! The projection function.
//!
//! ```text
//! project(text, marks, view) -> String
//! ```
//!
//! The projection is a **pure** function over a concrete text snapshot and its marks. It
//! is the single mechanism that makes tracked changes, the redline PDF, the redacted
//! public variant and the editor surface all fall out of one place.
//!
//! ## Views
//!
//! | View | Rule |
//! |---|---|
//! | [`View::Baseline`] | Drop `insert`-marked spans, keep `delete`-marked spans. The last agreed version. |
//! | [`View::Proposed`] | Keep `insert`-marked spans, drop `delete`-marked spans. What it becomes if all accepted. |
//! | [`View::Redline`]  | Emit the full text with change markers injected, for the review show-rules package. |
//! | [`View::Public`]   | `proposed` minus `secret`-marked spans (the redacted variant). |
//! | [`View::Editor`]   | The canonical source: all characters, unchanged. Decorations are a separate concern of the editor. |
//!
//! ## Character semantics
//!
//! A character is *inserted* if covered by any [`MarkKind::Insert`] mark, *deleted* if
//! covered by any [`MarkKind::Delete`] mark, and *secret* if covered by any
//! [`MarkKind::Secret`] mark. Comment marks never affect visibility. The mapping from
//! these per-character properties to keep/drop is exactly the table above.
//!
//! A character covered by both an insert and a delete mark is a model inconsistency (see
//! [`crate::validation`]); until it is resolved the projection is still deterministic and
//! well-defined (delete takes precedence for the redline view; for filtering views such a
//! character is dropped from both `baseline` and `proposed`).

use crate::mark::{MarkKind, MarkSet};
use crate::position::char_len;
use crate::redline::{self, RedlineStyle};

/// Bit flag: character is covered by an `insert` mark.
pub const FLAG_INSERT: u8 = 1 << 0;
/// Bit flag: character is covered by a `delete` mark.
pub const FLAG_DELETE: u8 = 1 << 1;
/// Bit flag: character is covered by a `secret` mark.
pub const FLAG_SECRET: u8 = 1 << 2;

/// The projection view to compute.
///
/// The five supported views are `baseline`, `proposed`, `redline`, `public`, and
/// `editor`. `Public` is kept even though its *use* is post-MVP, so the enum does not
/// need to change later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    /// Reject all pending changes: drop insertions, keep deletions. The last agreed
    /// version.
    Baseline,
    /// Accept all pending changes: keep insertions, drop deletions. What it becomes if
    /// all are accepted.
    Proposed,
    /// Full text with change markers injected (see [`redline`]).
    Redline,
    /// The redacted variant: `proposed` minus secret-marked spans.
    Public,
    /// The canonical source string (all characters). Editor decorations are derived
    /// separately and never alter the text.
    Editor,
}

impl View {
    /// All views, in spec order.
    pub const ALL: [View; 5] = [
        View::Baseline,
        View::Proposed,
        View::Redline,
        View::Public,
        View::Editor,
    ];
}

impl std::fmt::Display for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            View::Baseline => "baseline",
            View::Proposed => "proposed",
            View::Redline => "redline",
            View::Public => "public",
            View::Editor => "editor",
        })
    }
}

/// Compute, for every character of `text`, a bitmask of [`FLAG_INSERT`] / [`FLAG_DELETE`]
/// / [`FLAG_SECRET`] according to which marks cover it.
///
/// Ranges are clamped to the text length so projection never panics on out-of-bounds
/// input; use [`crate::validation::validate`] to detect such input. Comment marks do not
/// contribute flags.
#[must_use]
pub fn char_flags(text: &str, marks: &MarkSet) -> Vec<u8> {
    let n = char_len(text);
    let mut flags = vec![0u8; n];
    for m in marks.iter() {
        let bit = match m.kind {
            MarkKind::Insert => FLAG_INSERT,
            MarkKind::Delete => FLAG_DELETE,
            MarkKind::Secret => FLAG_SECRET,
            MarkKind::Comment => continue,
        };
        let s = m.range.start.to_char_idx().min(n);
        let e = m.range.end.to_char_idx().min(n);
        if s >= e {
            continue;
        }
        for f in &mut flags[s..e] {
            *f |= bit;
        }
    }
    flags
}

/// The canonical projection function.
///
/// Uses the default [`RedlineStyle`] for the `Redline` view. For a custom redline style,
/// use [`project_with`].
#[must_use]
pub fn project(text: &str, marks: &MarkSet, view: View) -> String {
    project_with(text, marks, view, &RedlineStyle::default())
}

/// Project with an explicit redline marker style (used only for [`View::Redline`]).
#[must_use]
pub fn project_with(text: &str, marks: &MarkSet, view: View, style: &RedlineStyle) -> String {
    match view {
        View::Baseline => filter(text, marks, |f| f & FLAG_INSERT == 0),
        View::Proposed => filter(text, marks, |f| f & FLAG_DELETE == 0),
        View::Public => filter(text, marks, |f| {
            (f & FLAG_DELETE == 0) && (f & FLAG_SECRET == 0)
        }),
        View::Editor => text.to_string(),
        View::Redline => redline::redline(text, marks, style),
    }
}

fn filter(text: &str, marks: &MarkSet, mut keep: impl FnMut(u8) -> bool) -> String {
    let flags = char_flags(text, marks);
    let mut out = String::with_capacity(text.len());
    for (i, ch) in text.chars().enumerate() {
        if keep(flags[i]) {
            out.push(ch);
        }
    }
    out
}

/// Character-level visibility classification, useful for editor decorations and tests.
///
/// For a given character position this reports which visibility-affecting marks cover it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CharVisibility {
    /// Covered by at least one `insert` mark.
    pub inserted: bool,
    /// Covered by at least one `delete` mark.
    pub deleted: bool,
    /// Covered by at least one `secret` mark.
    pub secret: bool,
}

impl CharVisibility {
    /// Classify a single character position against the precomputed flags.
    #[inline]
    #[must_use]
    pub const fn from_flags(f: u8) -> CharVisibility {
        CharVisibility {
            inserted: f & FLAG_INSERT != 0,
            deleted: f & FLAG_DELETE != 0,
            secret: f & FLAG_SECRET != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;
    use crate::mark::{AuthorId, Mark, MarkId, Timestamp};
    use crate::position::{TextRange, slice_chars};

    fn mark(id: u64, kind: MarkKind, s: u32, e: u32) -> Mark {
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
    fn baseline_drops_inserts_keeps_deletes() {
        // Text: "ABCDE", insert over B (1..2), delete over D (3..4).
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 1, 2),
            mark(2, MarkKind::Delete, 3, 4),
        ]);
        assert_eq!(project(text, &marks, View::Baseline), "ACDE"); // drop B, keep D
    }

    #[test]
    fn proposed_keeps_inserts_drops_deletes() {
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 1, 2),
            mark(2, MarkKind::Delete, 3, 4),
        ]);
        assert_eq!(project(text, &marks, View::Proposed), "ABCE"); // keep B, drop D
    }

    #[test]
    fn public_strips_secret_from_proposed() {
        // Text: "ABCDE"; insert B; delete D; secret over C (2..3).
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 1, 2),
            mark(2, MarkKind::Delete, 3, 4),
            mark(3, MarkKind::Secret, 2, 3),
        ]);
        // proposed = A B C E (drop D); public = drop D and drop secret(C) => A B E
        assert_eq!(project(text, &marks, View::Proposed), "ABCE");
        assert_eq!(project(text, &marks, View::Public), "ABE");
    }

    #[test]
    fn editor_is_full_text() {
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 1, 2),
            mark(2, MarkKind::Delete, 3, 4),
            mark(3, MarkKind::Comment, 0, 1),
        ]);
        assert_eq!(project(text, &marks, View::Editor), "ABCDE");
    }

    #[test]
    fn comments_never_affect_visibility() {
        let text = "ABCDE";
        let marks = set(&[mark(9, MarkKind::Comment, 1, 3)]);
        assert_eq!(project(text, &marks, View::Baseline), "ABCDE");
        assert_eq!(project(text, &marks, View::Proposed), "ABCDE");
        assert_eq!(project(text, &marks, View::Public), "ABCDE");
    }

    #[test]
    fn multibyte_text_positions_are_scalar_correct() {
        // "aéäXY": scalars a(0) é(1) ä(2) X(3) Y(4). Insert over é..ä (1..3), delete Y (4..5).
        let text = "aéäXY";
        let marks = set(&[
            mark(1, MarkKind::Insert, 1, 3),
            mark(2, MarkKind::Delete, 4, 5),
        ]);
        // baseline: drop é,ä => "aXY"; keep Y => "aXY"
        assert_eq!(project(text, &marks, View::Baseline), "aXY");
        // proposed: keep é,ä; drop Y => "aéäX"
        assert_eq!(project(text, &marks, View::Proposed), "aéäX");
        // sanity: slice the inserted region by scalar range
        assert_eq!(
            slice_chars(
                text,
                TextRange::new(Position::from_char_idx(1), Position::from_char_idx(3))
            ),
            Some("éä")
        );
    }

    #[test]
    fn adjacent_same_kind_marks_form_one_redline_run() {
        let text = "ABCDE";
        // two adjacent insert marks [0,2) and [2,4) -> one inserted run "ABCD"
        let marks = set(&[
            mark(1, MarkKind::Insert, 0, 2),
            mark(2, MarkKind::Insert, 2, 4),
        ]);
        let rl = project(text, &marks, View::Redline);
        // default style: #review.add[ABCD]E  — single wrapping, not two.
        assert_eq!(rl, "#review.add[ABCD]E");
    }

    #[test]
    fn char_flags_aggregate_multiple_marks() {
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 0, 2),
            mark(2, MarkKind::Secret, 1, 3),
        ]);
        let flags = char_flags(text, &marks);
        // 0: insert, 1: insert|secret, 2: secret, 3:0, 4:0
        assert_eq!(
            flags,
            vec![FLAG_INSERT, FLAG_INSERT | FLAG_SECRET, FLAG_SECRET, 0, 0]
        );
        let cv = CharVisibility::from_flags(flags[1]);
        assert!(cv.inserted && cv.secret && !cv.deleted);
    }

    #[test]
    fn out_of_bounds_ranges_are_clamped_not_panicked() {
        let text = "ABC";
        // range 2..99 clamps to 2..3
        let marks = set(&[mark(1, MarkKind::Delete, 2, 99)]);
        assert_eq!(project(text, &marks, View::Proposed), "AB");
    }
}
