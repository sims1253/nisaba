//! # nisaba-core-wasm
//!
//! WebAssembly projection wrapper for the Nisaba web client.
//!
//! This crate exposes the pure projection in [`nisaba-core`] (plus the
//! bibliography-YAML renderer from [`nisaba-references`]) over a small
//! wasm-bindgen boundary so the browser can project document sources with
//! exactly the code the `app` service uses server-side today. `web/src/model.ts`
//! documents why that must be the same implementation: a second, simpler
//! projection in the browser would drift from the server and quietly disagree
//! about what a reviewer sees. This wrapper keeps `nisaba-core` the single
//! source of truth for projection (issue #20, stage 1).
//!
//! ## Boundary design
//!
//! Each feature is a pair: a plain-Rust function returning
//! `Result<String, String>` that carries the exact semantics (and error
//! strings) of the app's compile path, and a `#[wasm_bindgen]` wrapper that
//! maps the error onto a JS `Error`. The plain functions are the ones the
//! native parity suite can exercise fully — constructing a [`JsError`] calls a
//! JS import, which panics on non-wasm targets — while the wasm32 suite runs
//! the identical tests through the boundary.
//!
//! The boundary is deliberately boring: strings in, strings out. Inputs are
//! JSON-encoded with serde DTOs that mirror the `app` service's public wire
//! contract exactly, so the TypeScript side (stage 2) can pass the objects it
//! already has — `web/src/api.ts`'s `MarkInput` and the `GET /references` rows —
//! through `JSON.stringify` without adaptation:
//!
//! - [`projected_source`] (JS: [`project_source`]) mirrors the app's compile
//!   path (`services/app/src/lib.rs::projected_source`): a source string, the
//!   per-document mark list, and a view name; same DTO shape, same fallbacks
//!   (a mark without `id` uses its `timestamp`), same rejection of unknown
//!   kinds and views.
//! - [`references_bibliography_yaml`] (JS: [`bibliography_yaml`]) mirrors the
//!   app's `references_bibliography_yaml` helper, including the wire-metadata →
//!   `nisaba-references` mapping (`core_metadata` in the app: authors become
//!   family names, `year` becomes the issued date, `journal` the container
//!   title).
//!
//! ## Parity
//!
//! `tests/parity.rs` feeds the golden-file scenarios from
//! `crates/nisaba-core/tests/projection_golden.rs` through this boundary and
//! asserts the output is byte-identical to the committed golden files and to
//! the direct native `nisaba_core::project` call. The suite runs natively and
//! on `wasm32-unknown-unknown` (wasm-bindgen-test), so the wasm build is pinned
//! to the same contract the native suite pins.
//!
//! ## Gaps left to stage 2 (tracked in issue #20)
//!
//! The markdown-heading conversion (`markdown_headings_to_typst`) and the
//! bibliography/redline-support *injection* helpers are private functions
//! inside `services/app` and are not reachable from a pure crate; stage 1 does
//! not touch the server, so they are not exposed here. When the client takes
//! over projection they must either move into a pure crate or be reimplemented
//! behind the same parity guard.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]

use wasm_bindgen::prelude::*;

mod dto {
    //! Serde DTOs mirroring the app service's public wire contract.
    use nisaba_core::MarkKind;
    use nisaba_core::View;
    use serde::Deserialize;

    /// A review mark over a document source, in the app's wire shape
    /// (`services/app/src/types.rs::MarkInput`, mirrored by
    /// `web/src/api.ts`'s `MarkInput`). `start`/`end` are character
    /// (Unicode scalar) indices.
    #[derive(Debug, Clone, Deserialize)]
    pub struct Mark {
        /// Stable mark id; falls back to `timestamp` when absent (app rule).
        #[serde(default)]
        pub id: Option<u64>,
        /// First covered character index.
        pub start: u32,
        /// One past the last covered character index.
        pub end: u32,
        /// One of `insert` / `delete` / `comment` / `secret`.
        pub kind: String,
        /// Author identifier.
        pub author: String,
        /// Logical timestamp.
        pub timestamp: u64,
    }

    /// Parses a view name exactly as the app's wire enum accepts them
    /// (`#[serde(rename_all = "lowercase")]` on `CompileView`).
    pub fn parse_view(view: &str) -> Result<View, String> {
        match view {
            "baseline" => Ok(View::Baseline),
            "proposed" => Ok(View::Proposed),
            "redline" => Ok(View::Redline),
            "public" => Ok(View::Public),
            _ => Err(format!("unknown view: {view}")),
        }
    }

    /// Parses a mark kind exactly as the app's `projected_source` does.
    pub fn parse_mark_kind(kind: &str) -> Result<MarkKind, String> {
        match kind {
            "insert" => Ok(MarkKind::Insert),
            "delete" => Ok(MarkKind::Delete),
            "comment" => Ok(MarkKind::Comment),
            "secret" => Ok(MarkKind::Secret),
            _ => Err(format!("unknown mark kind: {kind}")),
        }
    }

    /// Reference metadata in the app's wire shape
    /// (`services/app/src/types.rs::ReferenceMetadata`, mirrored by
    /// `web/src/api.ts`). Unknown fields (e.g. `extra`) are ignored, matching
    /// serde's default tolerance for forward compatibility.
    #[derive(Debug, Clone, Deserialize)]
    pub struct ReferenceMetadata {
        /// Work title; entries without a title are skipped by the renderer.
        pub title: String,
        /// Author family names in source order.
        #[serde(default)]
        pub authors: Vec<String>,
        /// Publication year.
        pub year: Option<u16>,
        /// DOI.
        pub doi: Option<String>,
        /// `PubMed` id.
        pub pmid: Option<String>,
        /// Journal or book title.
        pub journal: Option<String>,
    }

    /// One reference row in the app's wire shape (`GET /references`).
    #[derive(Debug, Clone, Deserialize)]
    pub struct Reference {
        /// Stable reference id (used verbatim as the bibliography key).
        pub id: String,
        /// Descriptive metadata.
        pub metadata: ReferenceMetadata,
    }
}

/// Project one document source through `nisaba-core`'s projection, exactly as
/// the `app` service does before sending sources to the compile service
/// (plain-Rust half of the pair; the JS-facing wrapper is [`project_source`]).
///
/// # Arguments
/// - `source` — the raw document text (all characters, marks unapplied).
/// - `marks_json` — a JSON array of marks in the app's `MarkInput` shape
///   (`[{"id":1,"start":15,"end":21,"kind":"insert","author":"alice",
///   "timestamp":1}, ...]`); `[]` projects an unmarked document.
/// - `view` — one of `"baseline"`, `"proposed"`, `"redline"`, `"public"`.
///
/// # Errors
/// Returns an error string when `marks_json` is not valid JSON for the DTO, or
/// when a mark kind or view name is unknown — the same conditions and message
/// strings for which the app answers `400 Bad Request` (`invalid marks JSON:
/// ...`, `unknown mark kind: ...`, `unknown view: ...`).
pub fn projected_source(source: &str, marks_json: &str, view: &str) -> Result<String, String> {
    let view = dto::parse_view(view)?;
    let marks: Vec<dto::Mark> =
        serde_json::from_str(marks_json).map_err(|error| format!("invalid marks JSON: {error}"))?;
    // Same construction the app's `projected_source` uses: a Document with one
    // mark per input row, ids falling back to timestamps, notes dropped.
    let mut document = nisaba_core::Document::from_text(source);
    for mark in &marks {
        let kind = dto::parse_mark_kind(&mark.kind)?;
        document.add_mark(nisaba_core::Mark::new(
            nisaba_core::MarkId::new(mark.id.unwrap_or(mark.timestamp)),
            nisaba_core::TextRange::new(
                nisaba_core::Position::from_char_idx(mark.start),
                nisaba_core::Position::from_char_idx(mark.end),
            ),
            kind,
            nisaba_core::AuthorId::new(mark.author.clone()),
            nisaba_core::Timestamp::new(mark.timestamp),
            None,
        ));
    }
    Ok(document.project(view))
}

/// JS-facing wrapper for [`projected_source`]; errors surface as JS `Error`
/// objects carrying the same message strings.
///
/// # Errors
/// See [`projected_source`].
#[wasm_bindgen]
pub fn project_source(source: &str, marks_json: &str, view: &str) -> Result<String, JsError> {
    projected_source(source, marks_json, view).map_err(|error| JsError::new(&error))
}

/// Render a hayagriva bibliography YAML for Typst's `#bibliography`, mirroring
/// the app's compile path (`references_bibliography_yaml` in
/// `services/app/src/lib.rs`): the reference id is used verbatim as the key so
/// `@<id>` citations resolve, entries without a title are skipped, and the
/// wire-metadata mapping matches the app's `core_metadata` (authors become
/// family names, `year` the issued date, `journal` the container title).
/// Plain-Rust half of the pair; the JS-facing wrapper is [`bibliography_yaml`].
///
/// # Arguments
/// - `references_json` — a JSON array of `GET /references` rows
///   (`[{"id":"<uuid>","metadata":{"title":..,"authors":[..],"year":..,
///   "doi":..,"pmid":..,"journal":..}}, ...]`). Extra fields are ignored;
///   rows with empty or path-separator ids are skipped, as in the app.
///
/// # Errors
/// Returns an error string when `references_json` is not valid JSON for the
/// DTO (`invalid references JSON: ...`).
pub fn references_bibliography_yaml(references_json: &str) -> Result<String, String> {
    let rows: Vec<dto::Reference> = serde_json::from_str(references_json)
        .map_err(|error| format!("invalid references JSON: {error}"))?;
    let entries: Vec<nisaba_references::ReferenceEntry> = rows
        .iter()
        .filter_map(|row| {
            let id = nisaba_references::ReferenceId::new(row.id.clone()).ok()?;
            Some(nisaba_references::ReferenceEntry {
                id,
                metadata: nisaba_references::Metadata {
                    title: row.metadata.title.clone(),
                    authors: row
                        .metadata
                        .authors
                        .iter()
                        .map(|author| nisaba_references::Person::family(author.clone()))
                        .collect(),
                    issued: row.metadata.year.map(|year| nisaba_references::IssuedDate {
                        year: i32::from(year),
                        ..nisaba_references::IssuedDate::default()
                    }),
                    container_title: row.metadata.journal.clone(),
                    doi: row.metadata.doi.clone(),
                    pmid: row.metadata.pmid.clone(),
                    ..nisaba_references::Metadata::default()
                },
                unknown_ris: Vec::new(),
                fulltext: None,
                provenance: Vec::new(),
            })
        })
        .collect();
    Ok(nisaba_references::bibliography_yaml(&entries))
}

/// JS-facing wrapper for [`references_bibliography_yaml`]; errors surface as JS
/// `Error` objects carrying the same message strings.
///
/// # Errors
/// See [`references_bibliography_yaml`].
#[wasm_bindgen]
pub fn bibliography_yaml(references_json: &str) -> Result<String, JsError> {
    references_bibliography_yaml(references_json).map_err(|error| JsError::new(&error))
}

/// The crate version, so the client can log which projection build it loaded
/// (the wasm and server compiler versions must move in lockstep; issue #20
/// lists version skew as a risk this makes observable).
#[must_use]
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
