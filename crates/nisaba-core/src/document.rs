//! The three-layer document.
//!
//! A collaborative document is one CRDT doc with three layers:
//!
//! | Layer | Type | Contents |
//! |---|---|---|
//! | `text`  | sequence | Every character ever typed, **including characters marked deleted** by a pending suggestion. Nothing is removed until accept/reject. |
//! | `marks` | Peritext range marks | `{ id, range, kind, author, ts }`, `kind ∈ {insert, delete, comment, secret}`. |
//! | `data`  | map | Structured template field values. Never markup. |
//!
//! [`Document`] owns all three. It is a pure value — no Loro, no async, no I/O — so the
//! same document can be projected, validated and resolved in tests and in the editor
//! alike. The CRDT binding is responsible for keeping a
//! [`Document`] in sync with a replica; the semantics here are expressed over the
//! concrete snapshot.

use std::collections::BTreeMap;

use crate::mark::{Mark, MarkId, MarkSet};
use crate::projection::{View, project_with};
use crate::redline::RedlineStyle;
use crate::resolution::{self, Resolution, ResolutionError, ResolutionOutcome};
use crate::validation::{self, ValidationIssue};

/// A structured template-field value.
///
/// The `data` layer is "never markup": it carries the typed values that the
/// template renders as form controls (dates, enums, PZN, controlled vocabularies — see
/// draft-2 "Template fields"). This enum is the small, pure value type those controls
/// read and write.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// Free-form text.
    Text(String),
    /// A whole number.
    Integer(i64),
    /// A decimal number.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A value drawn from a controlled vocabulary (the variant string is opaque to the
    /// core; the template owns its meaning).
    Enum(String),
}

/// The `data` layer: a flat map of field name → [`FieldValue`].
///
/// Stored sorted by key so serialization and equality are stable. Nested structure, when
/// the template needs it, is expressed by convention in the key (e.g.
/// `patient.age_years`) rather than by a recursive type — keeping the pure model simple.
///
/// `Eq` is deliberately not implemented: [`FieldValue::Float`] carries an `f64`, which is
/// not `Eq`. Structural [`PartialEq`] is still available.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    fields: BTreeMap<String, FieldValue>,
}

impl Data {
    /// An empty data layer.
    #[inline]
    #[must_use]
    pub fn new() -> Data {
        Data {
            fields: BTreeMap::new(),
        }
    }

    /// Set a field value.
    pub fn set(&mut self, key: impl Into<String>, value: FieldValue) {
        self.fields.insert(key.into(), value);
    }

    /// Get a field value.
    #[inline]
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&FieldValue> {
        self.fields.get(key)
    }

    /// Remove a field.
    pub fn remove(&mut self, key: &str) -> Option<FieldValue> {
        self.fields.remove(key)
    }

    /// Whether the layer is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Number of fields.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Iterate over `(key, value)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &FieldValue)> {
        self.fields.iter()
    }
}

/// The three-layer document.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    text: String,
    marks: MarkSet,
    data: Data,
}

impl Document {
    /// Construct an empty document.
    #[inline]
    #[must_use]
    pub fn new() -> Document {
        Document::from_text(String::new())
    }

    /// Construct a document from initial text, with no marks and no data.
    #[must_use]
    pub fn from_text(text: impl Into<String>) -> Document {
        Document {
            text: text.into(),
            marks: MarkSet::new(),
            data: Data::new(),
        }
    }

    /// Construct a document from all three layers.
    #[must_use]
    pub fn from_layers(text: impl Into<String>, marks: MarkSet, data: Data) -> Document {
        Document {
            text: text.into(),
            marks,
            data,
        }
    }

    /// The text layer (full text including delete-marked characters).
    #[inline]
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The mark layer.
    #[inline]
    #[must_use]
    pub fn marks(&self) -> &MarkSet {
        &self.marks
    }

    /// Mutable access to the mark layer.
    #[inline]
    #[must_use]
    pub fn marks_mut(&mut self) -> &mut MarkSet {
        &mut self.marks
    }

    /// The data layer.
    #[inline]
    #[must_use]
    pub fn data(&self) -> &Data {
        &self.data
    }

    /// Mutable access to the data layer.
    #[inline]
    #[must_use]
    pub fn data_mut(&mut self) -> &mut Data {
        &mut self.data
    }

    /// Character length of the text layer.
    #[inline]
    #[must_use]
    pub fn char_len(&self) -> usize {
        crate::position::char_len(&self.text)
    }

    /// Set the text layer (replacing it wholesale). The mark layer is always cleared —
    /// including for an empty replacement — because wholesale replacement invalidates
    /// every mark anchor (otherwise stale marks would point past the end of the new
    /// text); callers that edit text should go through the CRDT binding which maps
    /// changes to mark remapping.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.marks = MarkSet::new();
    }

    /// Insert a mark.
    pub fn add_mark(&mut self, mark: Mark) {
        self.marks.insert(mark);
    }

    /// Remove a mark by id.
    pub fn remove_mark(&mut self, id: MarkId) -> Option<Mark> {
        self.marks.remove(id)
    }

    /// Compute a projection of this document.
    #[must_use]
    pub fn project(&self, view: View) -> String {
        project_with(&self.text, &self.marks, view, &RedlineStyle::default())
    }

    /// Compute a projection with a custom redline style (used for [`View::Redline`]).
    #[must_use]
    pub fn project_with(&self, view: View, style: &RedlineStyle) -> String {
        project_with(&self.text, &self.marks, view, style)
    }

    /// Validate the document's mark model against its text.
    #[must_use]
    pub fn validate(&self) -> Vec<ValidationIssue> {
        validation::validate(&self.text, &self.marks)
    }

    /// Whether the document's mark model is clean.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        validation::is_valid(&self.text, &self.marks)
    }

    /// Accept or reject a single change mark in place. See [`resolution::resolve`].
    ///
    /// On success the document's text and marks are updated and the [`ResolutionOutcome`]
    /// is returned (by value, for inspection).
    pub fn resolve(
        &mut self,
        id: MarkId,
        resolution: Resolution,
    ) -> Result<ResolutionOutcome, ResolutionError> {
        let outcome = resolution::resolve(&self.text, &self.marks, id, resolution)?;
        // Return a clone for inspection, then move the original into the document.
        let report = outcome.clone();
        self.text = outcome.text;
        self.marks = outcome.marks;
        Ok(report)
    }

    /// Convenience: accept a change.
    pub fn accept(&mut self, id: MarkId) -> Result<ResolutionOutcome, ResolutionError> {
        self.resolve(id, Resolution::Accept)
    }

    /// Convenience: reject a change.
    pub fn reject(&mut self, id: MarkId) -> Result<ResolutionOutcome, ResolutionError> {
        self.resolve(id, Resolution::Reject)
    }

    /// Consume into the three layers.
    #[must_use]
    pub fn into_layers(self) -> (String, MarkSet, Data) {
        (self.text, self.marks, self.data)
    }
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;
    use crate::mark::{AuthorId, MarkKind, Timestamp};
    use crate::position::TextRange;

    fn change(id: u64, kind: MarkKind, s: u32, e: u32) -> Mark {
        Mark::new(
            MarkId::new(id),
            TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
            kind,
            AuthorId::new("alice"),
            Timestamp::new(id),
            None,
        )
    }

    #[test]
    fn document_round_trips_layers() {
        let mut doc = Document::from_text("ABCDE");
        doc.add_mark(change(1, MarkKind::Insert, 1, 2));
        doc.data_mut()
            .set("wirkstoff", FieldValue::Text("Rivaroxaban".into()));
        doc.data_mut()
            .set("audience", FieldValue::Enum("internal".into()));
        let (text, marks, data) = doc.into_layers();
        assert_eq!(text, "ABCDE");
        assert_eq!(marks.len(), 1);
        assert_eq!(data.len(), 2);
        assert!(matches!(data.get("wirkstoff"), Some(FieldValue::Text(_))));
    }

    #[test]
    fn document_project_and_validate() {
        let mut doc = Document::from_text("ABCDE");
        doc.add_mark(change(1, MarkKind::Insert, 1, 2));
        doc.add_mark(change(2, MarkKind::Delete, 3, 4));
        assert!(doc.is_valid());
        assert_eq!(doc.project(View::Baseline), "ACDE");
        assert_eq!(doc.project(View::Proposed), "ABCE");
        assert_eq!(doc.project(View::Editor), "ABCDE");
    }

    #[test]
    fn document_accept_in_place() {
        let mut doc = Document::from_text("ABCDE");
        doc.add_mark(change(1, MarkKind::Delete, 3, 4));
        doc.accept(MarkId::new(1)).unwrap();
        assert_eq!(doc.text(), "ABCE");
        assert_eq!(doc.marks().len(), 0);
    }

    #[test]
    fn char_len_is_scalar() {
        let doc = Document::from_text("aéä𝕏");
        assert_eq!(doc.char_len(), 4);
    }

    #[test]
    fn out_of_bounds_mark_is_reported_not_panicked() {
        let mut doc = Document::from_text("ABC");
        doc.add_mark(change(1, MarkKind::Delete, 2, 99));
        assert!(!doc.is_valid());
        // projection still works (clamped).
        assert_eq!(doc.project(View::Proposed), "AB");
    }

    #[test]
    fn set_text_clears_marks_even_for_empty_replacement() {
        // A wholesale replacement always invalidates mark anchors; keeping marks around
        // an empty text would leave them anchored past the end.
        let mut doc = Document::from_text("ABCDE");
        doc.add_mark(change(1, MarkKind::Insert, 1, 3));
        doc.set_text("");
        assert_eq!(doc.text(), "");
        assert!(
            doc.marks().is_empty(),
            "marks must not survive a wholesale replacement"
        );
        assert!(doc.is_valid());
        // And a non-empty replacement clears them too.
        doc.add_mark(change(2, MarkKind::Delete, 0, 1));
        doc.set_text("xyz");
        assert!(doc.marks().is_empty());
    }
}
