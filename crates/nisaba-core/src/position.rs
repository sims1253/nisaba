//! Unicode character positions and ranges.
//!
//! The PLAN is explicit: *"Use Unicode character positions explicitly, not byte
//! offsets."* Every offset in the Nisaba mark and projection model is a count of
//! [Unicode scalar values](https://www.unicode.org/glossary/#scalar_value) from the
//! start of the text, **never** a byte index into the UTF-8 representation.
//!
//! This is enforced at the type level: [`Position`] is a newtype around a character
//! index, and there is no public constructor that accepts a byte offset. Converting a
//! [`Position`] to the byte offset a `&str` slice needs is an explicit, fallible-looking
//! operation ([`Position::to_byte`]) so byte offsets can never leak in by accident.
//!
//! The model is deliberately independent of any CRDT. CRDT libraries such as Loro use
//! stable per-character identifiers internally; the layer that bridges a CRDT replica to
//! this pure model is responsible for translating those identifiers to and from
//! [`Position`]s against a concrete text snapshot.

use core::fmt;

/// A character position: the number of Unicode scalar values before this point in the
/// text.
///
/// A `Position` of zero points before the first character; a `Position` equal to the
/// text's character length points after the last character. Like byte indices into a
/// `&str`, positions are *gap* indices — they live between characters — which makes
/// half-open ranges behave naturally.
///
/// Internally a `u32`, which accommodates any realistic document (a `u32` indexes over
/// four billion characters). Constructing a `Position` from a larger `usize` fails
/// gracefully via [`TryFrom`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position(u32);

impl Position {
    /// The position before the first character.
    pub const ZERO: Position = Position(0);

    /// Construct a position from a character index.
    ///
    /// Returns `None` if the index does not fit in a `u32`.
    #[inline]
    #[must_use]
    pub const fn from_char_idx(idx: u32) -> Position {
        Position(idx)
    }

    /// The position as a `usize` character index.
    #[inline]
    #[must_use]
    pub const fn to_char_idx(self) -> usize {
        self.0 as usize
    }

    /// Saturating addition of a character count.
    #[inline]
    #[must_use]
    pub const fn saturating_add(self, n: u32) -> Position {
        Position(self.0.saturating_add(n))
    }

    /// Saturating subtraction of a character count.
    #[inline]
    #[must_use]
    pub const fn saturating_sub(self, n: u32) -> Position {
        Position(self.0.saturating_sub(n))
    }

    /// Translate this character position into the byte offset of the same gap in `text`.
    ///
    /// Returns [`None`] if this position is out of range for `text` (strictly greater
    /// than the text's character length). This is the single sanctioned way to turn a
    /// [`Position`] into something `&str` slicing understands.
    #[inline]
    #[must_use]
    pub fn to_byte(self, text: &str) -> Option<usize> {
        let target = self.0 as usize;
        let mut seen = 0usize;
        for (bytes, _ch) in text.char_indices() {
            if seen == target {
                return Some(bytes);
            }
            seen += 1;
        }
        // `seen == target` here means the position points past the last character, which
        // is valid (the trailing gap).
        (seen == target).then_some(text.len())
    }

    /// Translate this character position into the UTF-16 code unit offset of the
    /// same gap in `text`.
    ///
    /// `CodeMirror` (and JavaScript strings generally) index by UTF-16 code units.
    /// A supplementary-plane character occupies two UTF-16 code units (a surrogate
    /// pair) but counts as one Unicode scalar value in the [`Position`] model.
    /// This method bridges that gap so marks anchored at a [`Position`] can be
    /// projected to the correct `CodeMirror` offset.
    ///
    /// Returns [`None`] if this position is out of range for `text`.
    #[must_use]
    pub fn to_utf16(self, text: &str) -> Option<usize> {
        let target = self.0 as usize;
        let mut seen = 0usize;
        let mut utf16 = 0usize;
        for ch in text.chars() {
            if seen == target {
                return Some(utf16);
            }
            seen += 1;
            utf16 += ch.len_utf16();
        }
        (seen == target).then_some(utf16)
    }

    /// Construct a [`Position`] from a UTF-16 code unit offset in `text`.
    ///
    /// This is the inverse of [`Position::to_utf16`]: given a `CodeMirror` offset,
    /// it returns the corresponding Unicode scalar value position.
    ///
    /// Returns [`None`] if the offset is out of range or points into the middle
    /// of a surrogate pair.
    #[must_use]
    pub fn from_utf16(text: &str, utf16_offset: usize) -> Option<Position> {
        let mut seen_utf16 = 0usize;
        let mut seen_scalars = 0u32;
        for ch in text.chars() {
            if seen_utf16 == utf16_offset {
                return Some(Position(seen_scalars));
            }
            seen_utf16 += ch.len_utf16();
            seen_scalars += 1;
        }
        if seen_utf16 == utf16_offset {
            Some(Position(seen_scalars))
        } else {
            None
        }
    }

    /// Construct a [`Position`] from a UTF-8 byte offset in `text`.
    ///
    /// This is the inverse of [`Position::to_byte`].
    ///
    /// Returns [`None`] if the offset is out of range or does not land on a
    /// scalar boundary.
    #[must_use]
    pub fn from_byte(text: &str, byte_offset: usize) -> Option<Position> {
        let mut seen_scalars = 0u32;
        for (bytes, _ch) in text.char_indices() {
            if bytes == byte_offset {
                return Some(Position(seen_scalars));
            }
            seen_scalars += 1;
        }
        if text.len() == byte_offset {
            Some(Position(seen_scalars))
        } else {
            None
        }
    }
}

impl TryFrom<usize> for Position {
    type Error = PositionTooLarge;

    #[inline]
    fn try_from(idx: usize) -> Result<Self, Self::Error> {
        u32::try_from(idx)
            .map(Position)
            .map_err(|_| PositionTooLarge { requested: idx })
    }
}

/// Error returned when a character index exceeds the `u32` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionTooLarge {
    /// The offending index.
    pub requested: usize,
}

impl fmt::Display for PositionTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "character index {} exceeds the u32 Position range",
            self.requested
        )
    }
}

impl std::error::Error for PositionTooLarge {}

impl fmt::Debug for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Position({})", self.0)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The number of Unicode scalar values in a string.
///
/// This is the unit all [`Position`]s and [`TextRange`]s are measured in.
#[inline]
#[must_use]
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// A half-open character range `[start, end)`.
///
/// `start` is inclusive, `end` is exclusive. The range `[p, p)` is empty and valid; it
/// denotes a point anchor (used, for example, by a collapsed comment pin). Ranges are the
/// currency of marks and of every projection rule in [`crate::projection`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    /// Inclusive start position.
    pub start: Position,
    /// Exclusive end position.
    pub end: Position,
}

impl TextRange {
    /// A new range `[start, end)`.
    #[inline]
    #[must_use]
    pub const fn new(start: Position, end: Position) -> TextRange {
        TextRange { start, end }
    }

    /// The empty range at a single point `[p, p)`.
    #[inline]
    #[must_use]
    pub const fn point(p: Position) -> TextRange {
        TextRange { start: p, end: p }
    }

    /// Collapse to the empty range at `start`.
    #[inline]
    #[must_use]
    pub const fn collapsed_to_start(self) -> TextRange {
        TextRange::point(self.start)
    }

    /// Number of characters spanned. Always `0` for a valid range.
    #[inline]
    #[must_use]
    pub const fn char_len(self) -> u32 {
        self.end.0.saturating_sub(self.start.0)
    }

    /// Whether the range covers no characters.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 >= self.end.0
    }

    /// Whether `start <= end` (the only structural validity condition).
    #[inline]
    #[must_use]
    pub const fn is_well_ordered(self) -> bool {
        self.start.0 <= self.end.0
    }

    /// True when this range covers the given position. An empty range covers no position.
    #[inline]
    #[must_use]
    pub fn contains_pos(self, p: Position) -> bool {
        !self.is_empty() && self.start <= p && p < self.end
    }

    /// True when this range fully contains `other` (inclusive of the empty range at
    /// either boundary).
    #[inline]
    #[must_use]
    pub fn contains_range(self, other: TextRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// True when the two ranges share at least one character. Empty ranges never overlap.
    #[inline]
    #[must_use]
    pub fn overlaps(self, other: TextRange) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        self.start < other.end && other.start < self.end
    }

    /// The intersection of the two ranges, or `None` if they share no characters.
    #[must_use]
    pub fn intersect(self, other: TextRange) -> Option<TextRange> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        if start < end {
            Some(TextRange::new(start, end))
        } else {
            None
        }
    }

    /// The smallest range containing both inputs. Well-defined even for empty ranges.
    #[must_use]
    pub fn union(self, other: TextRange) -> TextRange {
        // Treat empty ranges as their point location for the purposes of union so that
        // unioning with an empty range is a no-op on extent.
        let (s1, e1) = if self.is_empty() {
            (self.start, self.start)
        } else {
            (self.start, self.end)
        };
        let (s2, e2) = if other.is_empty() {
            (other.start, other.start)
        } else {
            (other.start, other.end)
        };
        TextRange::new(s1.min(s2), e1.max(e2))
    }

    /// Clamp this range to lie within `[min, max]`, returning a (possibly empty) range.
    #[must_use]
    pub fn clamp(self, min: Position, max: Position) -> TextRange {
        let start = self.start.max(min);
        let end = self.end.min(max);
        if start <= end {
            TextRange::new(start, end)
        } else {
            TextRange::point(start)
        }
    }
}

impl fmt::Debug for TextRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {})", self.start.0, self.end.0)
    }
}

impl fmt::Display for TextRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start.0, self.end.0)
    }
}

/// Slice `text` by a character range, returning `None` if the range is out of bounds or
/// not well ordered.
#[must_use]
pub fn slice_chars(text: &str, range: TextRange) -> Option<&str> {
    if !range.is_well_ordered() {
        return None;
    }
    let start = range.start.to_byte(text)?;
    let end = range.end.to_byte(text)?;
    // Bound check against the resolved byte offsets (defence in depth).
    if end < start || end > text.len() {
        return None;
    }
    text.get(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_len_counts_scalars_not_bytes() {
        // "aéä" mixes ASCII and two-byte scalars. Bytes: 1+2+2 = 5. Scalars: 3.
        assert_eq!(char_len("aéä"), 3);
        assert_eq!("aéä".len(), 5);
        // A supplementary-plane scalar (4 bytes) is still one position.
        assert_eq!(char_len("𝕏"), 1);
        assert_eq!("𝕏".len(), 4);
    }

    #[test]
    fn position_to_byte_handles_multibyte() {
        let text = "aéä𝕏"; // scalars at byte offsets 0, 1, 3, 5
        assert_eq!(Position::from_char_idx(0).to_byte(text), Some(0));
        assert_eq!(Position::from_char_idx(1).to_byte(text), Some(1));
        assert_eq!(Position::from_char_idx(2).to_byte(text), Some(3));
        assert_eq!(Position::from_char_idx(3).to_byte(text), Some(5));
        assert_eq!(Position::from_char_idx(4).to_byte(text), Some(9)); // trailing gap
        assert_eq!(Position::from_char_idx(5).to_byte(text), None); // out of range
    }

    #[test]
    fn slice_chars_uses_scalar_boundaries() {
        let text = "aéä𝕏";
        assert_eq!(
            slice_chars(
                text,
                TextRange::new(Position::from_char_idx(1), Position::from_char_idx(3))
            ),
            Some("éä")
        );
        assert_eq!(
            slice_chars(
                text,
                TextRange::new(Position::from_char_idx(3), Position::from_char_idx(4))
            ),
            Some("𝕏")
        );
        // Scalar-based slicing never splits a code point; a naive byte slice can. Here byte
        // range 1..4 lands in the middle of the two-byte `ä` and is therefore not valid
        // UTF-8, whereas the equivalent scalar slice is fine.
        assert!(text.get(1..3).is_some()); // "é" (bytes 1..3 are exactly one scalar)
        assert!(text.get(1..4).is_none()); // splits the ä scalar
        assert_eq!(
            slice_chars(
                text,
                TextRange::new(Position::from_char_idx(0), Position::from_char_idx(4))
            ),
            Some(text)
        );
    }

    #[test]
    fn try_from_usize_rejects_huge_indices() {
        assert_eq!(Position::try_from(7usize), Ok(Position::from_char_idx(7)));
        let huge = usize::try_from(u64::MAX).unwrap_or(usize::MAX);
        assert!(Position::try_from(huge).is_err());
    }

    #[test]
    fn range_overlap_and_containment() {
        let a = TextRange::new(Position::from_char_idx(0), Position::from_char_idx(5));
        let b = TextRange::new(Position::from_char_idx(3), Position::from_char_idx(7));
        let c = TextRange::new(Position::from_char_idx(5), Position::from_char_idx(9));
        assert!(a.overlaps(b));
        assert!(!a.overlaps(c)); // touching at 5 is not overlapping (half-open)
        assert_eq!(
            a.intersect(b),
            Some(TextRange::new(
                Position::from_char_idx(3),
                Position::from_char_idx(5)
            ))
        );
        assert_eq!(a.intersect(c), None);
        assert!(a.contains_range(TextRange::new(
            Position::from_char_idx(1),
            Position::from_char_idx(4)
        )));
        assert!(!a.contains_range(c));
        assert_eq!(
            a.union(c),
            TextRange::new(Position::from_char_idx(0), Position::from_char_idx(9))
        );
    }

    #[test]
    fn empty_ranges_never_overlap() {
        let p = TextRange::point(Position::from_char_idx(3));
        let r = TextRange::new(Position::from_char_idx(0), Position::from_char_idx(3));
        assert!(!p.overlaps(r));
        assert!(!p.overlaps(p));
        assert!(r.contains_range(TextRange::point(Position::from_char_idx(3))));
    }

    #[test]
    fn position_to_utf16_handles_ascii() {
        let text = "hello";
        assert_eq!(Position::from_char_idx(0).to_utf16(text), Some(0));
        assert_eq!(Position::from_char_idx(3).to_utf16(text), Some(3));
        assert_eq!(Position::from_char_idx(5).to_utf16(text), Some(5));
        assert_eq!(Position::from_char_idx(6).to_utf16(text), None);
    }

    #[test]
    fn position_to_utf16_handles_astral() {
        // "\u{1D54F}" = MATHEMATICAL DOUBLE-STRUCK CAPITAL X (supplementary plane)
        let text = "a\u{1D54F}b";
        assert_eq!(Position::from_char_idx(0).to_utf16(text), Some(0));
        assert_eq!(Position::from_char_idx(1).to_utf16(text), Some(1));
        assert_eq!(Position::from_char_idx(2).to_utf16(text), Some(3));
        assert_eq!(Position::from_char_idx(3).to_utf16(text), Some(4));
    }

    #[test]
    fn position_to_utf16_multiple_astral() {
        // Each emoji is U+1F600 (2 UTF-16 code units)
        let text = "\u{1F600}hello\u{1F600}";
        assert_eq!(Position::from_char_idx(0).to_utf16(text), Some(0));
        assert_eq!(Position::from_char_idx(1).to_utf16(text), Some(2));
        assert_eq!(Position::from_char_idx(6).to_utf16(text), Some(7));
        assert_eq!(Position::from_char_idx(7).to_utf16(text), Some(9));
    }

    #[test]
    fn position_from_utf16_round_trips() {
        let text = "a\u{1D54F}b\u{1F600}c";
        for i in 0..=char_len(text) {
            let pos = Position::from_char_idx(u32::try_from(i).unwrap());
            let utf16 = pos.to_utf16(text).unwrap();
            let back = Position::from_utf16(text, utf16).unwrap();
            assert_eq!(pos, back, "round trip failed at scalar {i}");
        }
    }

    #[test]
    fn position_from_utf16_rejects_mid_surrogate() {
        let text = "a\u{1D54F}b";
        // Offset 2 is inside the surrogate pair for the astral char
        assert_eq!(Position::from_utf16(text, 2), None);
        assert_eq!(
            Position::from_utf16(text, 1),
            Some(Position::from_char_idx(1))
        );
    }

    #[test]
    fn position_from_byte_round_trips() {
        let text = "a\u{1D54F}b\u{1F600}c";
        for i in 0..=char_len(text) {
            let pos = Position::from_char_idx(u32::try_from(i).unwrap());
            let byte = pos.to_byte(text).unwrap();
            let back = Position::from_byte(text, byte).unwrap();
            assert_eq!(pos, back, "byte round trip failed at scalar {i}");
        }
    }

    #[test]
    fn position_from_byte_rejects_mid_scalar() {
        let text = "a\u{1D54F}b";
        assert_eq!(Position::from_byte(text, 2), None);
        assert_eq!(
            Position::from_byte(text, 1),
            Some(Position::from_char_idx(1))
        );
    }

    #[test]
    fn position_conversions_with_combining_marks() {
        let precomposed = "\u{00E9}";
        let decomposed = "e\u{0301}";

        assert_eq!(char_len(precomposed), 1);
        assert_eq!(Position::from_char_idx(1).to_utf16(precomposed), Some(1));
        assert_eq!(Position::from_char_idx(1).to_byte(precomposed), Some(2));

        assert_eq!(char_len(decomposed), 2);
        assert_eq!(Position::from_char_idx(2).to_utf16(decomposed), Some(2));
        assert_eq!(Position::from_char_idx(2).to_byte(decomposed), Some(3));
    }
}
