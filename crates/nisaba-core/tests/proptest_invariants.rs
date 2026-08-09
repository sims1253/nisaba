//! Property-based invariants.
//!
//! These properties express the load-bearing invariants of the pure model. They are
//! deliberately independent of any CRDT: they hold for *every* `(text, marks)` snapshot a
//! CRDT could converge on, so the M2 fuzz acceptance ("never produces a mark whose range is
//! invalid against the text") is satisfied compositionally — each single resolution
//! preserves it here.

use nisaba_core::position::slice_chars;
use nisaba_core::prelude::*;
use nisaba_core::validation::ValidationIssue as ResolutionIssue;
use nisaba_core::{
    InlineSafety, MarkKind, Resolution, View, accept, char_len, classify_inline_safety, project,
    redline, reject, remap_position_after_deletion, resolve, validate,
};
use proptest::prelude::*;

/// One character drawn from an alphabet that includes multibyte scalars and Typst
/// structural characters, so redline and slicing are exercised on realistic input.
fn any_char() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('a'),
        Just('b'),
        Just('c'),
        Just(' '),
        Just('\n'),
        Just('['),
        Just(']'),
        Just('('),
        Just(')'),
        Just('$'),
        Just('`'),
        Just('é'),
        Just('ä'),
        Just('𝕏'),
    ]
}

/// Random text from that alphabet.
fn any_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any_char(), 0..40).prop_map(|cs| cs.into_iter().collect())
}

/// A mark kind drawn uniformly from the four kinds.
fn any_kind() -> impl Strategy<Value = MarkKind> {
    prop_oneof![
        Just(MarkKind::Insert),
        Just(MarkKind::Delete),
        Just(MarkKind::Comment),
        Just(MarkKind::Secret),
    ]
}

/// Combined strategy: random text plus a vector of in-bounds, well-ordered marks with
/// unique ids. Generating the text first lets the mark ranges be bounded by the text's
/// length without borrowing.
fn any_text_marks(max_marks: usize) -> impl Strategy<Value = (String, Vec<Mark>)> {
    proptest::collection::vec(any_char(), 0..40).prop_flat_map(move |cs| {
        let len = u32::try_from(cs.len()).unwrap_or(0);
        let triple = (any_kind(), 0..=len, 0..=len);
        proptest::collection::vec(triple, 0..=max_marks).prop_map(move |raw| {
            let text: String = cs.iter().collect();
            let marks: Vec<Mark> = raw
                .into_iter()
                .enumerate()
                .map(|(i, (k, a, b))| {
                    let (s, e) = if a <= b { (a, b) } else { (b, a) };
                    Mark::new(
                        MarkId::new(u64::try_from(i).unwrap_or(u64::MAX) + 1),
                        TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
                        k,
                        AuthorId::new("a"),
                        Timestamp::new(u64::try_from(i).unwrap_or(u64::MAX) + 1),
                        None,
                    )
                })
                .collect();
            (text, marks)
        })
    })
}

/// Is `needle` a subsequence of `haystack` (same chars, same relative order)?
fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut it = haystack.chars();
    for nc in needle.chars() {
        if it.find(|&hc| hc == nc).is_none() {
            return false;
        }
    }
    true
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// char_len counts scalars; slicing the whole range returns the whole text.
    #[test]
    fn char_len_and_whole_slice(text in any_text()) {
        let n = char_len(&text);
        prop_assert_eq!(n, text.chars().count());
        let whole = slice_chars(
            &text,
            TextRange::new(Position::ZERO, Position::from_char_idx(u32::try_from(n).unwrap_or(u32::MAX))),
        );
        prop_assert_eq!(whole, Some(text.as_str()));
    }

    /// baseline / proposed / public are always subsequences of the full text (they only
    /// ever drop characters, never reorder or invent them).
    #[test]
    fn filtered_views_are_subsequences((text, marks) in any_text_marks(5)) {
        let marks: MarkSet = marks.into_iter().collect();
        for view in [View::Baseline, View::Proposed, View::Public] {
            let out = project(&text, &marks, view);
            prop_assert!(is_subsequence(&text, &out), "view {view:?} not a subsequence: text={text:?} out={out:?}");
        }
    }

    /// The redline output contains every source character in order (markers add, never
    /// remove).
    #[test]
    fn redline_preserves_source_chars((text, marks) in any_text_marks(5)) {
        let marks: MarkSet = marks.into_iter().collect();
        let rl = redline(&text, &marks, &RedlineStyle::default());
        prop_assert!(is_subsequence(&rl, &text), "redline lost a source char: text={text:?} rl={rl:?}");
    }

    /// Accepting every change yields exactly the `proposed` projection; rejecting every
    /// change yields exactly the `baseline` projection.
    #[test]
    fn accept_all_eq_proposed_reject_all_eq_baseline((text, marks) in any_text_marks(6)) {
        let marks: MarkSet = marks.into_iter().collect();
        let ids: Vec<MarkId> = marks.iter().map(|m| m.id).collect();

        // Accept all change marks in id order.
        let mut t = text.clone();
        let mut ms = marks.clone();
        for id in &ids {
            if ms.get(*id).is_some_and(|m| m.kind.is_change()) {
                let out = accept(&t, &ms, *id).unwrap();
                t = out.text;
                ms = out.marks;
            }
        }
        prop_assert_eq!(t, project(&text, &marks, View::Proposed));

        // Reject all change marks in id order.
        let mut t = text.clone();
        let mut ms = marks.clone();
        for id in &ids {
            if ms.get(*id).is_some_and(|m| m.kind.is_change()) {
                let out = reject(&t, &ms, *id).unwrap();
                t = out.text;
                ms = out.marks;
            }
        }
        prop_assert_eq!(t, project(&text, &marks, View::Baseline));
    }

    /// After any single accept or reject, every surviving mark range stays within the new
    /// text bounds and remains well ordered — the M2 invariant, one resolution at a time.
    #[test]
    fn ranges_stay_valid_after_resolution((text, marks) in any_text_marks(6)) {
        let marks: MarkSet = marks.into_iter().collect();
        let ids: Vec<MarkId> = marks.iter().map(|m| m.id).collect();
        for id in &ids {
            if marks.get(*id).is_some_and(|m| m.kind.is_change()) {
                for res in [Resolution::Accept, Resolution::Reject] {
                    let out = resolve(&text, &marks, *id, res).unwrap();
                    let new_len = char_len(&out.text);
                    for m in out.marks.iter() {
                        prop_assert!(m.range.is_well_ordered(), "inverted range after {res:?}: {m:?}");
                        prop_assert!(
                            m.range.end.to_char_idx() <= new_len,
                            "out-of-bounds range after {res:?}: {m:?} (new_len={new_len})"
                        );
                    }
                    for issue in validate(&out.text, &out.marks) {
                        prop_assert!(
                            !matches!(issue, ResolutionIssue::OutOfBounds { .. } | ResolutionIssue::InvertedRange { .. }),
                            "structural issue after {res:?}: {issue}"
                        );
                    }
                }
            }
        }
    }

    /// The inline-safety heuristic is sound: if it says `InlineSafe`, the span really is
    /// balanced (every bracket type nets to zero, math/raw counts are even).
    #[test]
    fn inline_safety_is_sound(span in any_text()) {
        if matches!(classify_inline_safety(&span), InlineSafety::InlineSafe) {
            let (mut sq, mut cu, mut pa) = (0i64, 0i64, 0i64);
            let (mut dollars, mut backticks) = (0u64, 0u64);
            for ch in span.chars() {
                match ch {
                    '[' => sq += 1, ']' => sq -= 1,
                    '{' => cu += 1, '}' => cu -= 1,
                    '(' => pa += 1, ')' => pa -= 1,
                    '$' => dollars += 1, '`' => backticks += 1,
                    _ => {}
                }
            }
            prop_assert_eq!((sq, cu, pa), (0, 0, 0));
            prop_assert_eq!(dollars % 2, 0);
            prop_assert_eq!(backticks % 2, 0);
        }
    }

    /// Position remapping is monotonic non-decreasing.
    #[test]
    fn remap_is_monotonic((s, e, a, b) in (0u32..20, 0u32..20, 0u32..20, 0u32..20)) {
        let (del_s, del_e) = if s <= e { (s, e) } else { (e, s) };
        let (p1, p2) = if a <= b { (a, b) } else { (b, a) };
        let r1 = remap_position_after_deletion(
            Position::from_char_idx(p1),
            Position::from_char_idx(del_s),
            Position::from_char_idx(del_e),
        );
        let r2 = remap_position_after_deletion(
            Position::from_char_idx(p2),
            Position::from_char_idx(del_s),
            Position::from_char_idx(del_e),
        );
        prop_assert!(r1 <= r2, "remap not monotonic: {r1} > {r2} for p1={p1} p2={p2} del=[{del_s},{del_e})");
    }

    /// In-bounds, well-ordered marks never produce structural validation issues.
    #[test]
    fn in_bounds_marks_have_no_structural_issues((text, marks) in any_text_marks(6)) {
        let marks: MarkSet = marks.into_iter().collect();
        for issue in validate(&text, &marks) {
            prop_assert!(
                !matches!(issue, ResolutionIssue::OutOfBounds { .. } | ResolutionIssue::InvertedRange { .. }),
                "unexpected structural issue for in-bounds marks: {issue}"
            );
        }
    }
}
