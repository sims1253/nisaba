//! Parity guard for the wasm compile boundary (issue #20, stage 2b).
//!
//! The same compile requests are fed through the wasm boundary
//! (`new_compile_worker` → `compile`, the code a browser runs) and through
//! the native core directly (`nisaba_compile_core::Worker`, the code the
//! compile service drives), and both are compared against the committed
//! golden responses — PDF bytes included, which the core's fixed PDF
//! timestamp makes byte-comparable across processes and targets.
//!
//! The scenarios mirror `crates/nisaba-compile-core/src/lib.rs`'s inline
//! test fixtures (same texts, same request shapes). Each golden file holds
//! the compile response with the volatile fields (`build_id`,
//! `instrumentation` timings) stripped; it is regenerated natively with
//! `UPDATE_GOLDEN=1` and embedded with `include_str!` on `wasm32`, where
//! filesystem access is unavailable.
//!
//! The chain of equalities — native core call == golden files == wasm
//! boundary == wasm core call — is what guarantees the client-side compile
//! cannot drift from the server-side one. Every test below runs natively
//! (`cargo test -p nisaba-compile-wasm`) and on
//! `wasm32-unknown-unknown` (`cargo test -p nisaba-compile-wasm --target
//! wasm32-unknown-unknown`, requires `wasm-bindgen-cli` matching the
//! `wasm-bindgen` version), except the `JsError` mapping itself, which can
//! only run on wasm32 — constructing a `JsError` calls a JS import, which
//! panics on non-wasm targets.

// On wasm32 the attribute re-export turns every `#[test]` below into a
// `#[wasm_bindgen_test]`, so the identical suite runs under the wasm-bindgen
// test runner on the wasm target.
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test as test;

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use nisaba_compile_core::{CompileRequest, Worker};
use nisaba_compile_wasm::{CompileWorker, CompileWorkers};
use serde_json::Value;

/// One golden scenario: its name (the golden file stem) and the compile
/// request in the service's HTTP body shape.
struct Scenario {
    name: &'static str,
    request_json: String,
}

fn request(entry: &str, sources: &[(&str, &str)]) -> String {
    let map = sources
        .iter()
        .map(|(path, source)| (path, *source))
        .collect::<HashMap<_, _>>();
    serde_json::to_string(&serde_json::json!({
        "project_id": "parity",
        "entry": entry,
        "sources": map,
        "view": "public",
    }))
    .expect("serialize request")
}

/// Mirrors `compiles_valid_pdf_from_memory` in nisaba-compile-core: the
/// minimal in-memory compile.
fn hello() -> Scenario {
    Scenario {
        name: "hello",
        request_json: request(
            "main.typ",
            &[("main.typ", "= Hello\nThis is in-memory Typst.")],
        ),
    }
}

/// Mirrors `span_map_reports_real_pages_and_ranges` in nisaba-compile-core:
/// a document whose spans resolve to page 1 ranges.
fn emphasis() -> Scenario {
    Scenario {
        name: "emphasis",
        request_json: request(
            "main.typ",
            &[("main.typ", "= Hello\nBody text #emph[with emphasis].")],
        ),
    }
}

/// Multibyte text (Latin-1 + a supplementary-plane scalar), locking
/// scalar-correct positions through the JSON boundary — mirrors the golden
/// scenario family of `nisaba-core`'s projection suite.
fn multibyte() -> Scenario {
    Scenario {
        name: "multibyte",
        request_json: request(
            "main.typ",
            &[("main.typ", "= Über 𝕏 und ä\n\nDer Nutzen ist — belegt.")],
        ),
    }
}

/// Mirrors `diagnostics_and_span_map_use_utf16_offsets_for_non_ascii` in
/// nisaba-compile-core: a failing compile (unclosed call) after an em dash,
/// pinning the UTF-16 offset conversion and the whole-source span-map
/// fallback.
fn failure() -> Scenario {
    Scenario {
        name: "failure",
        request_json: request(
            "main.typ",
            &[(
                "main.typ",
                "= Intro\nAn em dash — then an error:\n#unknown-fn(",
            )],
        ),
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![hello(), emphasis(), multibyte(), failure()]
}

/// Removes the fields that legitimately differ between runs and hosts: the
/// per-process `build_id` prefix and the timing/cache instrumentation. Every
/// content-bearing field — `pdf`, `span_map`, `diagnostics`, `outline` —
/// stays in and must match byte for byte.
fn strip_volatile(response_json: &str) -> Value {
    let mut value: Value = serde_json::from_str(response_json).expect("parse compile response");
    let map = value
        .as_object_mut()
        .expect("compile response is a JSON object");
    map.remove("build_id");
    map.remove("instrumentation");
    value
}

/// The golden text for a scenario. On wasm32 it is embedded at compile time;
/// natively it is read from `tests/golden/` so `UPDATE_GOLDEN=1` can (re)write
/// it.
#[cfg(target_arch = "wasm32")]
fn golden_text(scenario: &Scenario) -> String {
    match scenario.name {
        "hello" => include_str!("golden/hello.json").to_owned(),
        "emphasis" => include_str!("golden/emphasis.json").to_owned(),
        "multibyte" => include_str!("golden/multibyte.json").to_owned(),
        "failure" => include_str!("golden/failure.json").to_owned(),
        other => panic!("unknown golden scenario: {other}"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn golden_path(scenario: &Scenario) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{}.json", scenario.name))
}

#[cfg(not(target_arch = "wasm32"))]
fn golden_text(scenario: &Scenario) -> String {
    let path = golden_path(scenario);
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file missing: {}; run UPDATE_GOLDEN=1 to create",
            path.display()
        )
    })
}

/// Natively regenerates the golden file for a scenario from the actual
/// (stripped) response, mirroring `crates/nisaba-core`'s golden workflow:
/// run `UPDATE_GOLDEN=1 cargo test -p nisaba-compile-wasm` after an
/// intentional output change and explain the change in the pull request.
#[cfg(not(target_arch = "wasm32"))]
fn maybe_update_golden(scenario: &Scenario, stripped: &Value) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let mut pretty = serde_json::to_string_pretty(stripped).expect("serialize golden pretty");
        pretty.push('\n');
        std::fs::write(golden_path(scenario), pretty).expect("write golden");
    }
}

#[cfg(target_arch = "wasm32")]
fn maybe_update_golden(_scenario: &Scenario, _stripped: &Value) {}

/// The same request through the native core, exactly the way the compile
/// service drives it (`Worker::new` → `compile`, response serialized).
fn direct_core(request_json: &str) -> Value {
    let request: CompileRequest =
        serde_json::from_str(request_json).expect("parse request for direct core path");
    let mut worker = Worker::new(&request).expect("direct core worker");
    let response = worker
        .compile(&request, false)
        .expect("direct core compile");
    let response_json = serde_json::to_string(&response).expect("serialize direct core response");
    strip_volatile(&response_json)
}

/// The core parity requirement of issue #20 stage 2b: for every golden
/// scenario, the wasm boundary's response is byte-identical (PDF included) to
/// the committed golden files and to the native core call the compile
/// service performs.
#[test]
fn golden_compile_parity() {
    for scenario in scenarios() {
        let mut worker = CompileWorker::new(&scenario.request_json)
            .unwrap_or_else(|error| panic!("{}: boundary worker failed: {error}", scenario.name));
        let actual = worker
            .compile(&scenario.request_json)
            .unwrap_or_else(|error| panic!("{}: boundary compile failed: {error}", scenario.name));
        let stripped = strip_volatile(&actual);
        maybe_update_golden(&scenario, &stripped);

        let golden: Value = serde_json::from_str(&golden_text(&scenario))
            .unwrap_or_else(|error| panic!("{}: golden is not valid JSON: {error}", scenario.name));
        assert_eq!(
            stripped, golden,
            "{}: wasm boundary response != golden file",
            scenario.name
        );

        // The boundary adds nothing: the same request through the core
        // directly produces the identical response.
        assert_eq!(
            stripped,
            direct_core(&scenario.request_json),
            "{}: boundary response != direct core call",
            scenario.name
        );

        // The PDF bytes themselves: identical to the golden bytes, a real
        // PDF, and one with fonts embedded (the typst-assets fonts the mock
        // universe embeds — the wasm build shares them with the service).
        let pdf = stripped["pdf"].as_str().map_or_else(Vec::new, |encoded| {
            BASE64.decode(encoded).expect("decode golden pdf")
        });
        if scenario.name == "failure" {
            assert!(
                stripped["pdf"].is_null(),
                "{}: failed compile must have no pdf",
                scenario.name
            );
            assert!(
                stripped["diagnostics"]
                    .as_array()
                    .is_some_and(|diagnostics| diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic["severity"] == "error")),
                "{}: failed compile must carry error diagnostics",
                scenario.name
            );
        } else {
            assert!(pdf.starts_with(b"%PDF-"), "{}: not a PDF", scenario.name);
            let pdf_str = String::from_utf8_lossy(&pdf);
            assert!(
                pdf_str.contains("/FontDescriptor") || pdf_str.contains("/FontFile"),
                "{}: PDF does not embed fonts",
                scenario.name
            );
            let golden_pdf = BASE64
                .decode(golden["pdf"].as_str().expect("golden pdf"))
                .expect("decode golden pdf");
            assert_eq!(
                pdf, golden_pdf,
                "{}: boundary PDF bytes != golden PDF bytes",
                scenario.name
            );
        }
    }
}

/// A warm worker produces the identical stripped response on recompile (the
/// comemo caches must not change the output), and the instrumentation flags
/// move the way the service's do: cold compile, then reused.
#[test]
fn warm_recompile_is_stable_and_reused_flag_matches_the_service() {
    let mut scenario = hello();
    let mut worker = CompileWorker::new(&scenario.request_json).expect("worker");
    let first = worker
        .compile(&scenario.request_json)
        .expect("first compile");
    let first_value: Value = serde_json::from_str(&first).expect("parse first");
    assert_eq!(first_value["instrumentation"]["worker_reused"], false);
    let second = worker
        .compile(&scenario.request_json)
        .expect("second compile");
    let second_value: Value = serde_json::from_str(&second).expect("parse second");
    assert_eq!(second_value["instrumentation"]["worker_reused"], true);
    assert_eq!(
        second_value["instrumentation"]["worker_compiles"], 2,
        "worker must count both compiles"
    );
    assert_eq!(
        strip_volatile(&first),
        strip_volatile(&second),
        "warm recompile changed the response"
    );

    // The build id advances per compile (same process prefix, counter up).
    assert_ne!(first_value["build_id"], second_value["build_id"]);
    // The core's host-plane seam: rss_bytes stays None on the wasm side.
    assert!(second_value["instrumentation"]["rss_bytes"].is_null());

    // A source edit through update_sources changes the outline, mirroring
    // `reuses_warm_worker_and_updates_overlay` in nisaba-compile-core.
    scenario.request_json = request("main.typ", &[("main.typ", "= Second")]);
    worker
        .update_sources(&scenario.request_json)
        .expect("overlay");
    let third = worker
        .compile(&scenario.request_json)
        .expect("third compile");
    let third_value: Value = serde_json::from_str(&third).expect("parse third");
    let titles: Vec<&str> = third_value["outline"]
        .as_array()
        .expect("outline array")
        .iter()
        .map(|entry| entry["title"].as_str().expect("title"))
        .collect();
    assert!(titles.contains(&"Second"), "outline must follow the update");
}

/// A single cached worker re-roots to a different entry of the same project
/// (mirrors `re_roots_single_cached_worker_to_a_new_entry` in
/// nisaba-compile-core) — through the boundary.
#[test]
fn boundary_re_roots_a_cached_worker_to_a_new_entry() {
    let first = request("documents/a.typ", &[("documents/a.typ", "= Document A")]);
    let second = request("documents/b.typ", &[("documents/b.typ", "= Document B")]);
    let mut worker = CompileWorker::new(&first).expect("worker");
    let _ = worker.compile(&first).expect("compile a");
    worker.update_sources(&second).expect("re-root");
    let response = worker.compile(&second).expect("compile b");
    let value: Value = serde_json::from_str(&response).expect("parse");
    let titles: Vec<&str> = value["outline"]
        .as_array()
        .expect("outline array")
        .iter()
        .map(|entry| entry["title"].as_str().expect("title"))
        .collect();
    assert!(titles.contains(&"Document B"));
    assert!(!titles.contains(&"Document A"));
}

/// The worker pool keeps the service's per-project semantics: cold first
/// compile, warm recompile, and LRU eviction at capacity (clock-independent:
/// with one cached entry, eviction's choice is forced).
#[test]
fn worker_pool_matches_the_service_cache_semantics() {
    let a = request("main.typ", &[("main.typ", "= A")]);
    // Same project id, different sources: the pool must key on project_id.
    let a2 = request("main.typ", &[("main.typ", "= A again")]);
    let b = serde_json::json!({
        "project_id": "project-b",
        "entry": "main.typ",
        "sources": {"main.typ": "= B"},
        "view": "public",
    });
    let b = b.to_string();

    let mut pool = CompileWorkers::new(1, std::time::Duration::from_mins(30));
    let cold = pool.compile(&a).expect("cold compile");
    assert_eq!(
        serde_json::from_str::<Value>(&cold).expect("parse")["instrumentation"]["worker_reused"],
        false
    );
    let warm = pool.compile(&a2).expect("warm compile");
    assert_eq!(
        serde_json::from_str::<Value>(&warm).expect("parse")["instrumentation"]["worker_reused"],
        true
    );
    let warm_value: Value = serde_json::from_str(&warm).expect("parse");
    let titles: Vec<&str> = warm_value["outline"]
        .as_array()
        .expect("outline")
        .iter()
        .map(|entry| entry["title"].as_str().expect("title"))
        .collect();
    assert!(
        titles.contains(&"A again"),
        "warm worker must see the update"
    );

    // Capacity 1: compiling project B must evict A; recompiling A is cold.
    let _ = pool.compile(&b).expect("compile b");
    let re_cold = pool.compile(&a).expect("compile a after eviction");
    assert_eq!(
        serde_json::from_str::<Value>(&re_cold).expect("parse")["instrumentation"]["worker_reused"],
        false,
        "evicted project must compile cold"
    );

    // Zero-capacity pool refuses without panicking (the service answers 429).
    let mut empty = CompileWorkers::new(0, std::time::Duration::from_mins(30));
    let error = empty.compile(&a).expect_err("capacity must be enforced");
    assert_eq!(error, "worker cache at capacity (0)");
}

/// The boundary mirrors the service's request-shape guards (its `400`
/// conditions) through the plain halves, with the core's exact messages.
#[test]
fn request_shape_errors_match_the_core_guards() {
    let error = CompileWorker::new("nope").expect_err("bad JSON must fail");
    assert!(
        error.starts_with("invalid compile request JSON:"),
        "unexpected error: {error}"
    );

    let bad_ids = [
        (
            r#"{"project_id":"","entry":"main.typ","sources":{"main.typ":"x"},"view":"public"}"#,
            "project_id must not be empty",
        ),
        (
            r#"{"project_id":"p","entry":"../main.typ","sources":{"../main.typ":"x"},"view":"public"}"#,
            "path traversal",
        ),
        (
            r#"{"project_id":"p","entry":"main.typ","sources":{"other.typ":"x"},"view":"public"}"#,
            "sources must contain entry",
        ),
    ];
    for (body, expected) in bad_ids {
        let error = CompileWorker::new(body)
            .err()
            .unwrap_or_else(|| panic!("request must be rejected: {body}"));
        assert!(
            error.contains(expected),
            "expected {error:?} to contain {expected:?}"
        );
        // The pool applies the same guards.
        let mut pool = CompileWorkers::new(4, std::time::Duration::from_mins(30));
        let pooled = pool.compile(body).expect_err("pool must reject too");
        assert!(pooled.contains(expected));
    }

    // A valid worker also revalidates every request (update and compile).
    let good = request("main.typ", &[("main.typ", "= X")]);
    let mut worker = CompileWorker::new(&good).expect("worker");
    assert!(worker.update_sources("nope").is_err());
    assert!(worker.compile("nope").is_err());
}

/// `version()` exports the crate (workspace) version so the client can log
/// which compile build it loaded.
#[test]
fn version_is_exported() {
    assert_eq!(nisaba_compile_wasm::version(), env!("CARGO_PKG_VERSION"));
}

/// The `#[wasm_bindgen]` wrappers map the plain halves' errors onto
/// `JsError`s. Constructing a `JsError` calls a JS import (it panics on
/// non-wasm targets), so this half of the boundary runs on wasm32 only; the
/// message strings themselves are pinned natively above.
#[cfg(target_arch = "wasm32")]
#[test]
fn boundary_errors_reach_js() {
    use nisaba_compile_wasm::{JsCompileWorkers, new_compile_worker, new_compile_workers};

    assert!(new_compile_worker("nope").is_err());
    assert!(new_compile_worker("[]").is_err());
    let worker = new_compile_worker(&hello().request_json).expect("worker");
    let mut worker = worker;
    assert!(worker.compile("nope").is_err());
    assert!(
        worker
            .compile(
                r#"{"project_id":"","entry":"main.typ","sources":{"main.typ":"x"},"view":"v"}"#
            )
            .is_err()
    );

    let mut pool: JsCompileWorkers = new_compile_workers(4, 30.0 * 60.0 * 1000.0);
    assert!(pool.compile("nope").is_err());
}

/// `Duration::from_secs_f64(NaN)` panics and `f64::clamp` propagates NaN —
/// the documented "`NaN` counts as zero" TTL contract must hold without
/// trapping the module (a JS `Number(undefined)` or failed `parseInt` is
/// exactly this shape).
#[test]
fn nan_idle_ttl_counts_as_zero() {
    let _: nisaba_compile_wasm::JsCompileWorkers =
        nisaba_compile_wasm::new_compile_workers(4, f64::NAN);
    // Same for infinity: clamped to the ~100-year ceiling, not a panic.
    let _: nisaba_compile_wasm::JsCompileWorkers =
        nisaba_compile_wasm::new_compile_workers(4, f64::INFINITY);
}

/// The single-worker half applies the request's sources before compiling,
/// like the service handler and the pool half — compiling edited sources on
/// a warm worker must reflect the edit, not the stale universe. Before the
/// fix this compiled `hello` while shaping outline/span map from `emphasis`.
#[test]
fn compile_applies_the_requests_sources_on_a_warm_worker() {
    let cold = hello();
    let warm = emphasis();
    let mut worker = CompileWorker::new(&cold.request_json)
        .unwrap_or_else(|error| panic!("boundary worker failed: {error}"));
    worker.compile(&cold.request_json).expect("cold compile");

    let actual = worker
        .compile(&warm.request_json)
        .expect("warm compile with edited sources");
    let stripped = strip_volatile(&actual);
    maybe_update_golden(&warm, &stripped);
    let golden: Value = serde_json::from_str(&golden_text(&warm))
        .unwrap_or_else(|error| panic!("golden is not valid JSON: {error}"));
    assert_eq!(
        stripped, golden,
        "warm compile must reflect the edited sources (emphasis), not the stale universe (hello)"
    );
}
