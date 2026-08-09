//! Mark-semantics behavior matrix.
//!
//! These are the cases the PLAN says *"users produce all of these within a week"*, with
//! explicit, deterministic behaviour for each. They exercise only the pure model — the
//! slice of §6.2 that is correctly testable without a CRDT.
//!
//! | # | Case | Expected behaviour |
//! |---|---|---|
//! | 1 | Insertion inside another author's pending insertion | Both coexist; rejecting the outer insertion cascades onto the inner one. |
//! | 2 | Deletion overlapping a pending insertion partially | Flagged as an insert/delete conflict; projection stays deterministic. |
//! | 3 | Accepting one change that partially overlaps another | The overlapping change's range is clipped to its survivors, never corrupted. |
//! | 4 | Comment anchor whose entire range is deleted | Orphaned and surfaced — never silently relocated. |
//! | 5 | Concurrent accept and reject of the same change | The pure model resolves each operation deterministically; convergence of the *stream* is the CRDT's job (documented). |
//!
//! The resolution rules are ported in spirit from Overleaf's `ranges-tracker`:
//! a character removed by a resolution collapses every overlapping mark's range onto the
//! deletion's left edge, and a mark wholly inside the deletion becomes empty.

use nisaba_core::prelude::*;
use nisaba_core::validation::ValidationIssue;
use nisaba_core::{InlineSafety, View};

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

fn doc_with(text: &str, marks: &[Mark]) -> Document {
    let mut d = Document::from_text(text);
    for m in marks {
        d.add_mark(m.clone());
    }
    d
}

// ---- Case 1: insertion inside another author's pending insertion -----------------

#[test]
fn case1_nested_insertions_coexist_and_cascade_on_reject() {
    // Text "ABCDEFGH". Alice inserts [2,6) ("CDEF"); Bob inserts [3,4) ("D") inside Alice's.
    let text = "ABCDEFGH";
    let marks = vec![
        change(1, MarkKind::Insert, 2, 6, "alice"),
        change(2, MarkKind::Insert, 3, 4, "bob"),
    ];
    let doc = doc_with(text, &marks);

    // Both insertions coexist: nested inserts are NOT a conflict (only insert+delete is).
    let structural: Vec<_> = doc
        .validate()
        .into_iter()
        .filter(|i| matches!(i, ValidationIssue::InsertDeleteConflict { .. }))
        .collect();
    assert!(
        structural.is_empty(),
        "nested inserts must not conflict: {structural:?}"
    );

    // Proposed keeps both spans.
    assert_eq!(doc.project(View::Proposed), "ABCDEFGH");
    // Baseline drops both (all insert-marked chars gone).
    assert_eq!(doc.project(View::Baseline), "ABGH");

    // Rejecting Alice's outer insertion cascades: Bob's inner insertion loses its text too.
    let mut d2 = doc.clone();
    let out = d2.reject(MarkId::new(1)).unwrap();
    // Alice's [2,6) removed; Bob's [3,4) was inside → becomes empty.
    assert!(out.emptied.contains(&MarkId::new(2)));
    let bob = d2.marks().get(MarkId::new(2)).unwrap();
    assert!(
        bob.range.is_empty(),
        "Bob's insertion should collapse, not relocate"
    );
    assert_eq!(d2.text(), "ABGH");
}

// ---- Case 2: deletion overlapping a pending insertion partially ------------------

#[test]
fn case2_partial_delete_insert_overlap_is_flagged_and_deterministic() {
    // Alice inserts [2,6) ("CDEF"); Bob deletes [4,8) ("EFGH"). Overlap [4,6).
    let text = "ABCDEFGH";
    let marks = vec![
        change(1, MarkKind::Insert, 2, 6, "alice"),
        change(2, MarkKind::Delete, 4, 8, "bob"),
    ];
    let doc = doc_with(text, &marks);

    // The overlap is reported as a conflict.
    let conflicts: Vec<_> = doc
        .validate()
        .into_iter()
        .filter(|i| matches!(i, ValidationIssue::InsertDeleteConflict { .. }))
        .collect();
    assert_eq!(
        conflicts.len(),
        1,
        "expected one insert/delete conflict: {conflicts:?}"
    );

    // Projection is still deterministic. Proposed drops deletes (and the overlap, since
    // delete wins) and keeps the rest of the insertion: chars 2,3 kept; 4,5 overlap
    // dropped; 6,7 dropped (delete-only). So proposed = "AB" + "CD" = "ABCD".
    assert_eq!(doc.project(View::Proposed), "ABCD");
    // Baseline drops the insertion entirely and keeps the deletion: "AB" + "EFGH"? No —
    // baseline keeps delete-marked chars and drops insert-marked. Insert [2,6) dropped,
    // so chars 2,3,4,5 gone. Delete [4,8) kept. Result chars: 0,1,6,7 = "ABGH".
    assert_eq!(doc.project(View::Baseline), "ABGH");
}

// ---- Case 3: accepting one change that partially overlaps another ----------------

#[test]
fn case3_accept_overlapping_change_clips_the_other() {
    // Delete A [2,6) ("CDEF"); Insert B [4,8) ("EFGH"). Accept A.
    let text = "ABCDEFGH";
    let marks = vec![
        change(1, MarkKind::Delete, 2, 6, "alice"),
        change(2, MarkKind::Insert, 4, 8, "bob"),
    ];
    let mut doc = doc_with(text, &marks);

    let out = doc.accept(MarkId::new(1)).unwrap();
    // A removed chars [2,6) ("CDEF"); text now "ABGH".
    assert_eq!(doc.text(), "ABGH");
    // B's range [4,8): chars [4,6) inside deletion collapse, [6,8) shift left by 4 → [2,4).
    let b = doc.marks().get(MarkId::new(2)).unwrap();
    assert_eq!(
        b.range,
        TextRange::new(Position::from_char_idx(2), Position::from_char_idx(4)),
        "overlapping change must clip to survivors"
    );
    // Exactly one mark was remapped (B); A was removed.
    assert_eq!(out.remapped, 1);
    assert_eq!(doc.marks().len(), 1);
    // The document remains structurally valid.
    assert!(doc.is_valid());
}

// ---- Case 4: comment anchor whose entire range is deleted → orphaned -------------

#[test]
fn case4_comment_orphaned_not_relocated() {
    // Comment over [2,5) ("CDE"); a single delete covers exactly [2,5).
    let text = "ABCDEFGH";
    let marks = vec![
        Mark::new(
            MarkId::new(7),
            TextRange::new(Position::from_char_idx(2), Position::from_char_idx(5)),
            MarkKind::Comment,
            AuthorId::new("carol"),
            Timestamp::new(1),
            Some("Please reword.".to_string()),
        ),
        change(2, MarkKind::Delete, 2, 5, "bob"),
    ];
    let doc = doc_with(text, &marks);

    // Orphan is surfaced, not silently relocated.
    let orphans: Vec<_> = doc
        .validate()
        .into_iter()
        .filter(|i| matches!(i, ValidationIssue::OrphanedComment { .. }))
        .collect();
    assert_eq!(orphans.len(), 1, "comment should be orphaned: {orphans:?}");
    if let ValidationIssue::OrphanedComment { comment, range } = &orphans[0] {
        assert_eq!(*comment, MarkId::new(7));
        // The orphan keeps its original range — it is not moved.
        assert_eq!(
            *range,
            TextRange::new(Position::from_char_idx(2), Position::from_char_idx(5))
        );
    } else {
        panic!("expected OrphanedComment");
    }

    // Accepting the deletion collapses the comment to an empty range at the deletion's
    // left edge — it is still not relocated elsewhere.
    let mut d2 = doc.clone();
    let out = d2.accept(MarkId::new(2)).unwrap();
    assert!(out.emptied.contains(&MarkId::new(7)));
    let c = d2.marks().get(MarkId::new(7)).unwrap();
    assert!(c.range.is_empty());
    assert_eq!(
        c.range.start,
        Position::from_char_idx(2),
        "collapsed in place, not relocated"
    );
    // The comment body survives (still inspectable for the reviewer).
    assert_eq!(c.note.as_deref(), Some("Please reword."));
}

#[test]
fn case4b_partially_deleted_comment_is_not_orphaned() {
    // Comment [2,6); delete only [3,4) — the comment partially survives.
    let text = "ABCDEFGH";
    let marks = vec![
        change(7, MarkKind::Comment, 2, 6, "carol"),
        change(2, MarkKind::Delete, 3, 4, "bob"),
    ];
    let doc = doc_with(text, &marks);
    let orphans: Vec<_> = doc
        .validate()
        .into_iter()
        .filter(|i| matches!(i, ValidationIssue::OrphanedComment { .. }))
        .collect();
    assert!(
        orphans.is_empty(),
        "partially-deleted comment must not be orphaned: {orphans:?}"
    );
}

// ---- Case 5: concurrent accept and reject of the same change --------------------
//
// The pure model resolves a single operation deterministically against a snapshot. Two
// users issuing *different* operations on the same change from the same snapshot produce
// different results — that divergence is exactly what the CRDT layer resolves by picking a
// single winning operation. What the pure model guarantees is that whichever operation
// wins, it applies correctly and leaves the document valid.

#[test]
fn case5_concurrent_accept_and_reject_resolve_deterministically() {
    let text = "ABCDE";
    let marks = vec![change(1, MarkKind::Delete, 2, 4, "alice")];

    // Replica A accepts from the shared snapshot.
    let doc_a = doc_with(text, &marks);
    let mut a = doc_a.clone();
    a.accept(MarkId::new(1)).unwrap();
    assert_eq!(a.text(), "ABE");
    assert!(a.is_valid());

    // Replica B rejects from the *same* snapshot.
    let doc_b = doc_with(text, &marks);
    let mut b = doc_b.clone();
    b.reject(MarkId::new(1)).unwrap();
    assert_eq!(b.text(), "ABCDE");
    assert!(b.is_valid());

    // Each resolution is independently correct and deterministic. Convergence — which
    // operation wins — is the CRDT's responsibility, not the pure model's.
    assert_ne!(a.text(), b.text());

    // Repeating the same operation on the same snapshot is byte-identical (determinism).
    let mut a2 = doc_a;
    a2.accept(MarkId::new(1)).unwrap();
    assert_eq!(a.text(), a2.text());
}

// ---- Bonus: the redline structural trap is exercised end-to-end through Document --

#[test]
fn document_redline_falls_back_for_unbalanced_syntax() {
    let text = "= M3\n\n#figure(image(\"x\"))\n\nBody.";
    let target = "#figure(image(\"x\")"; // unbalanced
    let start = text.find(target).unwrap();
    let cstart = text[..start].chars().count();
    let clen = target.chars().count();
    let mut doc = Document::from_text(text);
    doc.add_mark(Mark::new(
        MarkId::new(1),
        TextRange::new(
            Position::from_char_idx(u32::try_from(cstart).unwrap()),
            Position::from_char_idx(u32::try_from(cstart + clen).unwrap()),
        ),
        MarkKind::Delete,
        AuthorId::new("reviewer"),
        Timestamp::new(1),
        None,
    ));
    let report =
        nisaba_core::redline_with_report(doc.text(), doc.marks(), &RedlineStyle::default());
    assert!(matches!(
        report.runs[0].safety,
        InlineSafety::BlockReplaced(_)
    ));
    let redline = doc.project(View::Redline);
    assert!(redline.contains("#review.rep-open[]"));
    assert!(redline.contains("#review.rep-close[]"));
    // The unbalanced span is never wrapped in an inline delete marker.
    assert!(!redline.contains(&format!("#review.del[{target}")));
}
