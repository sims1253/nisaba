//! Golden-file tests for the projection function.
//!
//! Each scenario pins the exact output of [`nisaba_core::project`] for all five views.
//! The expected outputs live in `tests/golden/` as UTF-8 text files. Set the
//! `UPDATE_GOLDEN=1` environment variable to regenerate them (use after an intentional
//! output change, then review the diff).
//!
//! Because the projection is a pure, deterministic function, these files are the
//! contract: an unrelated change that alters any of them is a real regression, not noise
//!.

use std::fs;
use std::path::PathBuf;

use nisaba_core::prelude::*;

/// One golden scenario.
struct Case {
    /// Basename of the golden files (e.g. `simple_insert`).
    name: &'static str,
    text: &'static str,
    marks: Vec<Mark>,
}

fn mk(id: u64, kind: MarkKind, s: u32, e: u32, author: &str) -> Mark {
    Mark::new(
        MarkId::new(id),
        TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
        kind,
        AuthorId::new(author),
        Timestamp::new(id),
        None,
    )
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check_or_update(name: &str, view: View, actual: &str) {
    let file = golden_dir().join(format!("{name}-{view}.txt"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&file, actual).expect("write golden");
        return;
    }
    let expected = fs::read_to_string(&file).unwrap_or_else(|_| {
        panic!(
            "golden file missing: {}; run UPDATE_GOLDEN=1 to create",
            file.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "golden mismatch for {name}/{view} ({}):\n--- expected ---\n{}\n--- actual ---\n{}\n",
        file.display(),
        expected,
        actual
    );
}

fn run(case: &Case) {
    let marks: MarkSet = case.marks.iter().cloned().collect();
    for view in View::ALL {
        let actual = project(case.text, &marks, view);
        check_or_update(case.name, view, &actual);
    }
}

#[test]
fn golden_simple_insert_and_delete() {
    // German test sentence. Insert "nachgewiesen" (proposed) over "belegt"; delete "ist".
    //      D e r   N u t z e n   i s t   b e l e g t .
    // idx: 0 1 2 3 4 5 6 7 8 9 ...
    // "ist" starts at index 12, "belegt" at index 16.
    let text = "Der Nutzen ist belegt.";
    let ist_start = text.find("ist").unwrap();
    let belegt_start = text.find("belegt").unwrap();
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let cs = c(ist_start);
    let ce = cs + 3; // "ist"
    let bs = c(belegt_start);
    let be = bs + u32::try_from("belegt".chars().count()).unwrap();
    let case = Case {
        name: "simple_insert_delete",
        text,
        marks: vec![
            mk(1, MarkKind::Insert, bs, be, "alice"), // insert "belegt" (pending addition)
            mk(2, MarkKind::Delete, cs, ce, "bob"),   // delete "ist"
        ],
    };
    run(&case);
}

#[test]
fn golden_secret_redaction() {
    // Public view strips secret spans from the proposed text.
    // "Wirkstoff X ist geheim." — mark "X" as secret, delete "ist".
    let text = "Wirkstoff X ist geheim.";
    let xs = text.find('X').unwrap();
    let ist = text.find("ist").unwrap();
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let case = Case {
        name: "secret_redaction",
        text,
        marks: vec![
            mk(1, MarkKind::Secret, c(xs), c(xs) + 1, "alice"),
            mk(2, MarkKind::Delete, c(ist), c(ist) + 3, "bob"),
        ],
    };
    run(&case);
}

#[test]
fn golden_redline_figure_trap() {
    // The PLAN's canonical structural trap: a deletion that spans half of a `#figure(`
    // call must fall back to the block-level "replaced" region rather than wrapping the
    // unbalanced span in an inline marker.
    let text = "= Chapter Three\n\n#figure(image(\"logo.svg\"))\n\nText darunter.";
    let target = "#figure(image(\"logo.svg\")"; // unbalanced: missing one `)`
    let start = text.find(target).unwrap();
    let cstart = text[..start].chars().count();
    let clen = target.chars().count();
    let case = Case {
        name: "redline_figure_trap",
        text,
        marks: vec![mk(
            1,
            MarkKind::Delete,
            u32::try_from(cstart).unwrap(),
            u32::try_from(cstart + clen).unwrap(),
            "reviewer",
        )],
    };
    run(&case);
    // Extra assertion: the trap really triggered the block fallback.
    let marks: MarkSet = case.marks.iter().cloned().collect();
    let report = nisaba_core::redline_with_report(text, &marks, &RedlineStyle::default());
    assert_eq!(report.runs.len(), 1);
    assert!(matches!(
        report.runs[0].safety,
        nisaba_core::InlineSafety::BlockReplaced(_)
    ));
}

#[test]
fn golden_multibyte_text() {
    // Mix ASCII, Latin-1 extensions and a supplementary-plane scalar to lock in
    // scalar-correct projection positions.
    let text = "Über 𝕏 und ä";
    // Mark "𝕏" (index 5) for insert and "ä" (index 10) for delete.
    let xs = text.find('𝕏').unwrap();
    let ae = text.find('ä').unwrap();
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let case = Case {
        name: "multibyte",
        text,
        marks: vec![
            mk(1, MarkKind::Insert, c(xs), c(xs) + 1, "a"),
            mk(2, MarkKind::Delete, c(ae), c(ae) + 1, "b"),
        ],
    };
    run(&case);
}

#[test]
fn golden_comment_only_does_not_filter() {
    // A comment anchor never changes which characters are emitted by any view.
    let text = "Kommentierter Satz.";
    let mid = text.find("Satz").unwrap();
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let case = Case {
        name: "comment_only",
        text,
        marks: vec![mk(7, MarkKind::Comment, c(mid), c(mid) + 4, "carol")],
    };
    run(&case);
}
