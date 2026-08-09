//! Marks: Peritext-style range annotations over the text layer.
//!
//! A mark is the unit of the attribution layer:
//!
//! ```text
//! { id, range, kind, author, ts }
//!   kind ∈ { insert, delete, comment, secret }
//! ```
//!
//! Marks carry every non-textual fact about a document: tracked changes (`insert` /
//! `delete`), comment anchors (`comment`) and trade-secret redaction (`secret`). They are
//! *range* marks — they annotate a span of characters rather than mutating them — so the
//! text layer keeps every character ever typed, including those a pending suggestion marks
//! deleted. That is what makes reject always possible.
//!
//! ## Independence from the CRDT
//!
//! The semantics here are expressed purely over a concrete `&str` and a character
//! [`crate::TextRange`]. There is no Loro type, no op log, no CRDT identifier in this
//! component. The CRDT stays swappable: a future binding translates stable CRDT
//! positions to [`Position`](crate::Position)s against a snapshot, and everything below —
//! validation, projection, accept/reject — keeps working unchanged.

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt;

use crate::position::TextRange;

/// The four kinds of mark the attribution layer carries.
///
/// The supported kinds are `insert`, `delete`, `comment`, and `secret`;
/// each kind drives one projection rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkKind {
    /// A pending insertion. The spanned characters are *proposed* content: present in the
    /// `proposed` view, absent from the `baseline` view.
    Insert,
    /// A pending deletion. The spanned characters are still physically present in the text
    /// layer (so the deletion can be rejected) but absent from the `proposed` view and
    /// struck through in the `redline` view.
    Delete,
    /// A comment anchor. Comment marks never affect which characters a projection emits;
    /// they attach a discussion to a span. A comment whose entire range becomes covered by
    /// pending deletions is *orphaned* (see [`crate::resolution`]).
    Comment,
    /// A trade-secret / business-secret span (Kennzeichnung von Betriebs- und
    /// Geschäftsgeheimnissen). Secret marks drive the `public` (redacted) projection and
    /// never affect insert/delete visibility.
    Secret,
}

impl MarkKind {
    /// Whether this kind participates in insert/delete change semantics.
    #[inline]
    #[must_use]
    pub const fn is_change(self) -> bool {
        matches!(self, MarkKind::Insert | MarkKind::Delete)
    }
}

impl fmt::Display for MarkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MarkKind::Insert => "insert",
            MarkKind::Delete => "delete",
            MarkKind::Comment => "comment",
            MarkKind::Secret => "secret",
        })
    }
}

/// A stable, document-unique identifier for a mark.
///
/// In the pure model this is an opaque `u64`; the only requirement is uniqueness within a
/// document. In production these originate from a proper generator (a ULID/snowflake, or
/// the mark id assigned by the CRDT) so they survive replication and merge — but that is a
/// concern of the storage layer, not of this component.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkId(u64);

impl MarkId {
    /// Construct a mark id from a raw value.
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> MarkId {
        MarkId(raw)
    }

    /// The raw underlying value.
    #[inline]
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for MarkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MarkId({})", self.0)
    }
}

impl fmt::Display for MarkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "m{}", self.0)
    }
}

/// The author of a mark. An opaque, comparable identifier (typically a user id).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorId(std::sync::Arc<str>);

impl AuthorId {
    /// Construct an author id from anything string-like.
    #[inline]
    #[must_use]
    pub fn new(id: impl Into<String>) -> AuthorId {
        AuthorId(std::sync::Arc::from(id.into().into_boxed_str()))
    }

    /// The author identifier as a string slice.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for AuthorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AuthorId {
    fn from(s: &str) -> Self {
        AuthorId::new(s)
    }
}

impl Borrow<str> for AuthorId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A logical timestamp for a mark.
///
/// Opaque and totally ordered together with the author and mark id (see
/// [`Mark::occurrence_order`]). The storage layer is free to populate this with wall-clock
/// milliseconds, a hybrid logical clock, or a Lamport stamp; the pure model only requires
/// monotonicity within an author.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Construct a timestamp from a raw value.
    #[inline]
    #[must_use]
    pub const fn new(raw: u64) -> Timestamp {
        Timestamp(raw)
    }

    /// The raw underlying value.
    #[inline]
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ts({})", self.0)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A single Peritext-style range mark.
///
/// The stable shape is `{ id, range, kind, author, ts }`, plus an optional `note`
/// carrying human-readable payload (a comment body, a change rationale, or a secret
/// classification label). The `note` is the only extension over the spec and is required
/// to make comment marks useful in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Stable, document-unique identifier.
    pub id: MarkId,
    /// Character range this mark annotates.
    pub range: TextRange,
    /// What this mark means.
    pub kind: MarkKind,
    /// Who created it.
    pub author: AuthorId,
    /// When it was created (logical timestamp).
    pub timestamp: Timestamp,
    /// Optional payload: comment body, rationale, classification label, etc.
    pub note: Option<String>,
}

impl Mark {
    /// Construct a mark with all fields. `note` is optional and may be empty.
    #[must_use]
    pub fn new(
        id: MarkId,
        range: TextRange,
        kind: MarkKind,
        author: AuthorId,
        timestamp: Timestamp,
        note: Option<String>,
    ) -> Mark {
        Mark {
            id,
            range,
            kind,
            author,
            timestamp,
            note,
        }
    }

    /// Builder: attach a note.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Mark {
        self.note = Some(note.into());
        self
    }

    /// A deterministic, total ordering for concurrent operations: timestamps first, then
    /// author, then mark id. This is the tie-break the projection and accept/reject
    /// resolution use so that two replicas with the same marks always resolve overlaps the
    /// same way.
    #[must_use]
    pub fn occurrence_order(&self, other: &Mark) -> std::cmp::Ordering {
        self.timestamp
            .cmp(&other.timestamp)
            .then_with(|| self.author.cmp(&other.author))
            .then_with(|| self.id.cmp(&other.id))
    }
}

// ---------------------------------------------------------------------------
// MarkSet
// ---------------------------------------------------------------------------

/// An ordered collection of marks keyed by [`MarkId`].
///
/// Iteration order is stable (sorted by mark id) which keeps projections and redline
/// output deterministic regardless of insertion order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkSet {
    marks: BTreeMap<MarkId, Mark>,
}

impl MarkSet {
    /// An empty set.
    #[inline]
    #[must_use]
    pub const fn new() -> MarkSet {
        MarkSet {
            marks: BTreeMap::new(),
        }
    }

    /// Insert or replace a mark by its id.
    pub fn insert(&mut self, mark: Mark) {
        self.marks.insert(mark.id, mark);
    }

    /// Remove a mark by id, returning it if present.
    pub fn remove(&mut self, id: MarkId) -> Option<Mark> {
        self.marks.remove(&id)
    }

    /// Look up a mark by id.
    #[inline]
    #[must_use]
    pub fn get(&self, id: MarkId) -> Option<&Mark> {
        self.marks.get(&id)
    }

    /// Number of marks.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.marks.len()
    }

    /// Whether there are no marks.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// Iterate over all marks (in id order).
    pub fn iter(&self) -> impl Iterator<Item = &Mark> {
        self.marks.values()
    }

    /// Iterate mutably over all marks.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Mark> {
        self.marks.values_mut()
    }

    /// Marks of a single kind, sorted by range start then occurrence order.
    #[must_use]
    pub fn of_kind(&self, kind: MarkKind) -> Vec<&Mark> {
        let mut v: Vec<&Mark> = self.marks.values().filter(|m| m.kind == kind).collect();
        sort_by_range(v.as_mut_slice());
        v
    }

    /// All marks whose range overlaps `range`, sorted by range start.
    #[must_use]
    pub fn overlapping(&self, range: TextRange) -> Vec<&Mark> {
        let mut v: Vec<&Mark> = self
            .marks
            .values()
            .filter(|m| m.range.overlaps(range))
            .collect();
        sort_by_range(v.as_mut_slice());
        v
    }

    /// Marks whose range is fully contained within `range`.
    #[must_use]
    pub fn contained_in(&self, range: TextRange) -> Vec<&Mark> {
        let mut v: Vec<&Mark> = self
            .marks
            .values()
            .filter(|m| range.contains_range(m.range))
            .collect();
        sort_by_range(v.as_mut_slice());
        v
    }

    /// Consume into the inner map (id → mark).
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> BTreeMap<MarkId, Mark> {
        self.marks
    }
}

fn sort_by_range(marks: &mut [&Mark]) {
    marks.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then_with(|| a.range.end.cmp(&b.range.end))
            .then_with(|| a.occurrence_order(b))
    });
}

impl IntoIterator for MarkSet {
    type Item = Mark;
    type IntoIter = std::collections::btree_map::IntoValues<MarkId, Mark>;
    fn into_iter(self) -> Self::IntoIter {
        self.marks.into_values()
    }
}

impl FromIterator<Mark> for MarkSet {
    fn from_iter<I: IntoIterator<Item = Mark>>(iter: I) -> Self {
        let mut set = MarkSet::new();
        for m in iter {
            set.insert(m);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Position;

    fn mk(id: u64, kind: MarkKind, s: u32, e: u32) -> Mark {
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
    fn insert_replace_by_id() {
        let mut set = MarkSet::new();
        set.insert(mk(1, MarkKind::Insert, 0, 3));
        set.insert(mk(2, MarkKind::Delete, 2, 5));
        assert_eq!(set.len(), 2);
        // Same id replaces.
        set.insert(mk(1, MarkKind::Comment, 7, 9));
        assert_eq!(set.len(), 2);
        assert_eq!(set.get(MarkId::new(1)).unwrap().kind, MarkKind::Comment);
    }

    #[test]
    fn overlapping_and_contained() {
        let mut set = MarkSet::new();
        set.insert(mk(1, MarkKind::Insert, 0, 4));
        set.insert(mk(2, MarkKind::Delete, 3, 7));
        set.insert(mk(3, MarkKind::Comment, 9, 12));
        let r = TextRange::new(Position::from_char_idx(2), Position::from_char_idx(5));
        assert_eq!(set.overlapping(r).len(), 2);
        let outer = TextRange::new(Position::from_char_idx(0), Position::from_char_idx(8));
        assert_eq!(set.contained_in(outer).len(), 2);
    }

    #[test]
    fn of_kind_filters() {
        let mut set = MarkSet::new();
        set.insert(mk(1, MarkKind::Insert, 0, 2));
        set.insert(mk(2, MarkKind::Delete, 1, 3));
        set.insert(mk(3, MarkKind::Insert, 5, 7));
        assert_eq!(set.of_kind(MarkKind::Insert).len(), 2);
        assert_eq!(set.of_kind(MarkKind::Delete).len(), 1);
        assert_eq!(set.of_kind(MarkKind::Comment).len(), 0);
    }

    #[test]
    fn occurrence_order_is_deterministic_total() {
        // Same timestamp, different authors and ids — order must be reproducible.
        let a = Mark::new(
            MarkId::new(5),
            TextRange::point(Position::ZERO),
            MarkKind::Insert,
            AuthorId::new("bob"),
            Timestamp::new(10),
            None,
        );
        let b = Mark::new(
            MarkId::new(3),
            TextRange::point(Position::ZERO),
            MarkKind::Insert,
            AuthorId::new("alice"),
            Timestamp::new(10),
            None,
        );
        let c = Mark::new(
            MarkId::new(1),
            TextRange::point(Position::ZERO),
            MarkKind::Insert,
            AuthorId::new("alice"),
            Timestamp::new(10),
            None,
        );
        let mut v = [a, b, c];
        v.sort_by(Mark::occurrence_order);
        assert_eq!(
            v.iter().map(|m| m.id.as_u64()).collect::<Vec<_>>(),
            vec![1, 3, 5]
        );
    }
}
