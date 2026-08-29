//! Parity guard for the wasm boundary (issue #20, stage 1).
//!
//! The same inputs are fed through the wasm boundary (`project_source`,
//! `bibliography_yaml`) and through the native path (direct
//! `nisaba_core::project` / `nisaba_references::bibliography_yaml` calls —
//! exactly what the app service's compile path ends up calling), and both are
//! compared against the committed golden projection fixtures in
//! `crates/nisaba-core/tests/golden/`.
//!
//! The scenarios mirror `crates/nisaba-core/tests/projection_golden.rs`
//! (same texts, same marks, timestamps equal to ids); the golden files are
//! embedded with `include_str!` so this suite also runs on
//! `wasm32-unknown-unknown`, where filesystem access is unavailable. The chain
//! of equalities — native golden files == native core call == wasm boundary —
//! is what guarantees the client-side projection cannot drift from the
//! server-side one.
//!
//! Error paths split by target: the plain `projected_source` /
//! `references_bibliography_yaml` functions (which the `#[wasm_bindgen]`
//! wrappers delegate to) are checked natively for the exact app error strings,
//! while the `JsError` mapping itself can only run on wasm32 — constructing a
//! `JsError` calls a JS import, which panics on non-wasm targets.
//!
//! Run natively: `cargo test -p nisaba-core-wasm`.
//! Run on wasm:  `cargo test -p nisaba-core-wasm --target wasm32-unknown-unknown`
//! (requires `wasm-bindgen-cli` matching the `wasm-bindgen` version).

// On wasm32 the attribute re-export turns every `#[test]` below into a
// `#[wasm_bindgen_test]`, so the identical suite runs under the wasm-bindgen
// test runner on the wasm target.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test as test;

use nisaba_core::prelude::*;
use nisaba_core_wasm::{
    bibliography_yaml, project_source, projected_source, references_bibliography_yaml, version,
};

/// One golden scenario: text, its marks, the wire JSON for those marks, and
/// the expected output per view (embedded from nisaba-core's golden files).
struct Scenario {
    name: &'static str,
    text: &'static str,
    marks: Vec<Mark>,
    marks_json: String,
    /// `(view name, expected output)` for the four views the wire contract
    /// accepts (the `editor` view is the raw source and is not a wire view).
    goldens: [(&'static str, &'static str); 4],
}

/// Build a mark the same way the native golden test does: id and timestamp both
/// derive from the same number so the wire JSON round-trips losslessly.
fn mark(id: u64, kind: MarkKind, start: u32, end: u32, author: &str) -> Mark {
    Mark::new(
        MarkId::new(id),
        TextRange::new(Position::from_char_idx(start), Position::from_char_idx(end)),
        kind,
        AuthorId::new(author),
        Timestamp::new(id),
        None,
    )
}

/// Encode marks in the app's `MarkInput` wire shape.
fn marks_json(marks: &[Mark]) -> String {
    let rows: Vec<serde_json::Value> = marks
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id.as_u64(),
                "start": m.range.start.to_char_idx(),
                "end": m.range.end.to_char_idx(),
                "kind": m.kind.to_string(),
                "author": m.author.to_string(),
                "timestamp": m.timestamp.as_u64(),
            })
        })
        .collect();
    serde_json::to_string(&rows).expect("serialize marks JSON")
}

fn core_view(view: &str) -> View {
    match view {
        "baseline" => View::Baseline,
        "proposed" => View::Proposed,
        "redline" => View::Redline,
        "public" => View::Public,
        _ => panic!("unknown golden view: {view}"),
    }
}

fn simple_insert_delete() -> Scenario {
    // Mirrors `golden_simple_insert_and_delete`: insert "belegt" (pending
    // addition by alice), delete "ist" (pending removal by bob).
    let text = "Der Nutzen ist belegt.";
    let ist_start = text.find("ist").expect("ist");
    let belegt_start = text.find("belegt").expect("belegt");
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let cs = c(ist_start);
    let ce = cs + 3; // "ist"
    let bs = c(belegt_start);
    let be = bs + u32::try_from("belegt".chars().count()).unwrap();
    let marks = vec![
        mark(1, MarkKind::Insert, bs, be, "alice"),
        mark(2, MarkKind::Delete, cs, ce, "bob"),
    ];
    Scenario {
        name: "simple_insert_delete",
        text,
        marks_json: marks_json(&marks),
        marks,
        goldens: [
            (
                "baseline",
                include_str!("../../nisaba-core/tests/golden/simple_insert_delete-baseline.txt"),
            ),
            (
                "proposed",
                include_str!("../../nisaba-core/tests/golden/simple_insert_delete-proposed.txt"),
            ),
            (
                "redline",
                include_str!("../../nisaba-core/tests/golden/simple_insert_delete-redline.txt"),
            ),
            (
                "public",
                include_str!("../../nisaba-core/tests/golden/simple_insert_delete-public.txt"),
            ),
        ],
    }
}

fn secret_redaction() -> Scenario {
    // Mirrors `golden_secret_redaction`: "X" is secret, "ist" is deleted; the
    // public view drops both.
    let text = "Wirkstoff X ist geheim.";
    let xs = text.find('X').expect("X");
    let ist = text.find("ist").expect("ist");
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let marks = vec![
        mark(1, MarkKind::Secret, c(xs), c(xs) + 1, "alice"),
        mark(2, MarkKind::Delete, c(ist), c(ist) + 3, "bob"),
    ];
    Scenario {
        name: "secret_redaction",
        text,
        marks_json: marks_json(&marks),
        marks,
        goldens: [
            (
                "baseline",
                include_str!("../../nisaba-core/tests/golden/secret_redaction-baseline.txt"),
            ),
            (
                "proposed",
                include_str!("../../nisaba-core/tests/golden/secret_redaction-proposed.txt"),
            ),
            (
                "redline",
                include_str!("../../nisaba-core/tests/golden/secret_redaction-redline.txt"),
            ),
            (
                "public",
                include_str!("../../nisaba-core/tests/golden/secret_redaction-public.txt"),
            ),
        ],
    }
}

fn redline_figure_trap() -> Scenario {
    // Mirrors `golden_redline_figure_trap`: a deletion spanning half of a
    // `#figure(...)` call must fall back to the block-level "replaced" region.
    let text = "= Chapter Three\n\n#figure(image(\"logo.svg\"))\n\nText darunter.";
    let target = "#figure(image(\"logo.svg\")"; // unbalanced: missing one `)`
    let start = text.find(target).expect("figure");
    let cstart = text[..start].chars().count();
    let clen = target.chars().count();
    let marks = vec![mark(
        1,
        MarkKind::Delete,
        u32::try_from(cstart).unwrap(),
        u32::try_from(cstart + clen).unwrap(),
        "reviewer",
    )];
    Scenario {
        name: "redline_figure_trap",
        text,
        marks_json: marks_json(&marks),
        marks,
        goldens: [
            (
                "baseline",
                include_str!("../../nisaba-core/tests/golden/redline_figure_trap-baseline.txt"),
            ),
            (
                "proposed",
                include_str!("../../nisaba-core/tests/golden/redline_figure_trap-proposed.txt"),
            ),
            (
                "redline",
                include_str!("../../nisaba-core/tests/golden/redline_figure_trap-redline.txt"),
            ),
            (
                "public",
                include_str!("../../nisaba-core/tests/golden/redline_figure_trap-public.txt"),
            ),
        ],
    }
}

fn multibyte() -> Scenario {
    // Mirrors `golden_multibyte_text`: ASCII + Latin-1 + a supplementary-plane
    // scalar, locking in scalar-correct positions through the JSON boundary.
    let text = "Über 𝕏 und ä";
    let xs = text.find('𝕏').expect("X");
    let ae = text.find('ä').expect("ä");
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let marks = vec![
        mark(1, MarkKind::Insert, c(xs), c(xs) + 1, "a"),
        mark(2, MarkKind::Delete, c(ae), c(ae) + 1, "b"),
    ];
    Scenario {
        name: "multibyte",
        text,
        marks_json: marks_json(&marks),
        marks,
        goldens: [
            (
                "baseline",
                include_str!("../../nisaba-core/tests/golden/multibyte-baseline.txt"),
            ),
            (
                "proposed",
                include_str!("../../nisaba-core/tests/golden/multibyte-proposed.txt"),
            ),
            (
                "redline",
                include_str!("../../nisaba-core/tests/golden/multibyte-redline.txt"),
            ),
            (
                "public",
                include_str!("../../nisaba-core/tests/golden/multibyte-public.txt"),
            ),
        ],
    }
}

fn comment_only() -> Scenario {
    // Mirrors `golden_comment_only_does_not_filter`: a comment anchor never
    // changes which characters any view emits.
    let text = "Kommentierter Satz.";
    let mid = text.find("Satz").expect("Satz");
    let c = |b: usize| u32::try_from(text[..b].chars().count()).unwrap();
    let marks = vec![mark(7, MarkKind::Comment, c(mid), c(mid) + 4, "carol")];
    Scenario {
        name: "comment_only",
        text,
        marks_json: marks_json(&marks),
        marks,
        goldens: [
            (
                "baseline",
                include_str!("../../nisaba-core/tests/golden/comment_only-baseline.txt"),
            ),
            (
                "proposed",
                include_str!("../../nisaba-core/tests/golden/comment_only-proposed.txt"),
            ),
            (
                "redline",
                include_str!("../../nisaba-core/tests/golden/comment_only-redline.txt"),
            ),
            (
                "public",
                include_str!("../../nisaba-core/tests/golden/comment_only-public.txt"),
            ),
        ],
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        simple_insert_delete(),
        secret_redaction(),
        redline_figure_trap(),
        multibyte(),
        comment_only(),
    ]
}

/// The core parity requirement of issue #20 stage 1: for every golden scenario
/// and every wire view, the wasm boundary's output is byte-identical to the
/// native `nisaba_core::project` call and to the committed golden files.
#[test]
fn golden_projection_parity() {
    for scenario in scenarios() {
        let marks: MarkSet = scenario.marks.iter().cloned().collect();
        for (view, expected) in &scenario.goldens {
            let through_boundary = project_source(scenario.text, &scenario.marks_json, view)
                .unwrap_or_else(|e| {
                    panic!("{}: boundary failed for view {view}: {e:?}", scenario.name)
                });
            assert_eq!(
                through_boundary, *expected,
                "{}/{}: wasm boundary output != golden file",
                scenario.name, view
            );
            let native = project(scenario.text, &marks, core_view(view));
            assert_eq!(
                through_boundary, native,
                "{}/{}: wasm boundary output != native core call",
                scenario.name, view
            );
        }
    }
}

/// The boundary mirrors the app service's `projected_source` semantics beyond
/// the happy path: the id-falls-back-to-timestamp rule, and the exact error
/// conditions the app answers with `400 Bad Request`.
#[test]
fn app_wire_semantics() {
    // Missing `id` falls back to `timestamp` (app rule), so these two requests
    // project identically.
    let with_id = r#"[{"id":42,"start":0,"end":3,"kind":"insert","author":"a","timestamp":42}]"#;
    let without_id = r#"[{"start":0,"end":3,"kind":"insert","author":"a","timestamp":42}]"#;
    let a = project_source("abcdef", with_id, "proposed").expect("with id");
    let b = project_source("abcdef", without_id, "proposed").expect("without id");
    assert_eq!(a, b);
    // `insert` over `abc`: proposed keeps it, baseline drops it.
    assert_eq!(a, "abcdef");
    assert_eq!(
        project_source("abcdef", with_id, "baseline").expect("baseline"),
        "def"
    );

    // Unknown mark kind: the one error string byte-identical to the app's
    // BadRequest body (malformed marks JSON / unknown views fail in axum's
    // extractor there, with different strings). The plain
    // function carries the message the JS wrapper wraps in a `JsError`.
    let err = projected_source(
        "abc",
        r#"[{"id":1,"start":0,"end":1,"kind":"squiggle","author":"a","timestamp":1}]"#,
        "baseline",
    )
    .expect_err("unknown kind must fail");
    assert_eq!(err, "unknown mark kind: squiggle");

    // Unknown view.
    let err = projected_source("abc", "[]", "sideways").expect_err("unknown view must fail");
    assert_eq!(err, "unknown view: sideways");

    // Malformed marks JSON.
    let err = projected_source("abc", "[{", "baseline").expect_err("bad JSON must fail");
    assert!(
        err.starts_with("invalid marks JSON:"),
        "unexpected error: {err}"
    );

    // Malformed references JSON (bibliography wrapper).
    let err = references_bibliography_yaml("nope").expect_err("bad refs JSON must fail");
    assert!(
        err.starts_with("invalid references JSON:"),
        "unexpected error: {err}"
    );

    // Out-of-bounds marks are clamped by the projection, never panic (the
    // graceful-degradation rule in nisaba-core's crate docs).
    let clamped = project_source(
        "abc",
        r#"[{"id":1,"start":2,"end":99,"kind":"delete","author":"a","timestamp":1}]"#,
        "proposed",
    )
    .expect("clamped");
    assert_eq!(clamped, "ab");

    // Empty marks project the identity for filtering views.
    assert_eq!(project_source("abc", "[]", "baseline").unwrap(), "abc");
}

/// The bibliography wrapper matches the native `nisaba_references` renderer
/// for the same rows, mirrors the app's metadata mapping, and keeps the
/// renderer's skip rules (no title, invalid id).
#[test]
fn bibliography_yaml_parity() {
    let rows = serde_json::json!([
        {
            "id": "11111111-1111-1111-1111-111111111111",
            "metadata": {
                "title": "A study: \"efficacy\"",
                "authors": ["Mustermann", "Doe"],
                "year": 2024,
                "doi": "10.1000/abc",
                "pmid": "123456",
                "journal": "Journal of Typst Studies",
                "extra": {"ignored": "wire field the mapping does not use"}
            }
        },
        {"id": "22222222-2222-2222-2222-222222222222", "metadata": {"title": "No authors"}},
        {"id": "", "metadata": {"title": "invalid id: skipped"}},
        {"id": "has/slash", "metadata": {"title": "path separator: skipped"}}
    ]);
    let through_boundary =
        bibliography_yaml(&rows.to_string()).expect("bibliography through boundary");

    // The same rows through the app's mapping into the native renderer.
    let native_entries: Vec<nisaba_references::ReferenceEntry> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            let id = nisaba_references::ReferenceId::new(
                row.get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
            )
            .ok()?;
            let m = row.get("metadata").unwrap();
            Some(nisaba_references::ReferenceEntry {
                id,
                metadata: nisaba_references::Metadata {
                    title: m.get("title").unwrap().as_str().unwrap().to_owned(),
                    authors: m
                        .get("authors")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|p| nisaba_references::Person::family(p.as_str().unwrap()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    issued: m
                        .get("year")
                        .and_then(serde_json::Value::as_u64)
                        .map(|year| nisaba_references::IssuedDate {
                            year: i32::try_from(year).unwrap(),
                            ..nisaba_references::IssuedDate::default()
                        }),
                    container_title: m.get("journal").and_then(|v| v.as_str()).map(str::to_owned),
                    doi: m.get("doi").and_then(|v| v.as_str()).map(str::to_owned),
                    pmid: m.get("pmid").and_then(|v| v.as_str()).map(str::to_owned),
                    ..nisaba_references::Metadata::default()
                },
                unknown_ris: Vec::new(),
                fulltext: None,
                provenance: Vec::new(),
            })
        })
        .collect();
    let native = nisaba_references::bibliography_yaml(&native_entries);
    assert_eq!(
        through_boundary, native,
        "bibliography: wasm boundary != native renderer"
    );

    // Anchor the rendered shape: the id is the key, values are quoted YAML
    // scalars (colons and quotes cannot break out), and skipped rows are
    // absent.
    assert!(through_boundary.contains("\"11111111-1111-1111-1111-111111111111\":"));
    assert!(through_boundary.contains("  title: \"A study: \\\"efficacy\\\"\""));
    assert!(through_boundary.contains("    - \"Mustermann\""));
    assert!(through_boundary.contains("  date: 2024"));
    assert!(through_boundary.contains("    doi: \"10.1000/abc\""));
    // The journal nests under `parent:` (a periodical) at a four-space indent.
    assert!(
        through_boundary
            .contains("  parent:\n    type: periodical\n    title: \"Journal of Typst Studies\"")
    );
    assert!(through_boundary.contains("\"22222222-2222-2222-2222-222222222222\":"));
    assert!(!through_boundary.contains("invalid id"));
    assert!(!through_boundary.contains("path separator"));
}

/// `version()` exports the crate (workspace) version so the client can log
/// which projection build it loaded.
#[test]
fn version_is_exported() {
    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
}

/// The `#[wasm_bindgen]` wrappers map the plain functions' errors onto
/// `JsError`s. Constructing a `JsError` calls a JS import (it panics on
/// non-wasm targets), so this half of the boundary runs on wasm32 only; the
/// message strings themselves are pinned natively above.
#[cfg(target_arch = "wasm32")]
#[test]
fn boundary_errors_reach_js() {
    assert!(project_source("abc", "[]", "sideways").is_err());
    assert!(project_source("abc", "[{", "baseline").is_err());
    assert!(
        project_source(
            "abc",
            r#"[{"id":1,"start":0,"end":1,"kind":"squiggle","author":"a","timestamp":1}]"#,
            "baseline"
        )
        .is_err()
    );
    assert!(bibliography_yaml("nope").is_err());
}
