//! Deterministic redline marker injection.
//!
//! The redline projection keeps **every** character of the text and injects change markers
//! around inserted/deleted runs so a Typst "review show-rules" package can render a
//! typeset redline (struck-through deletions, underlined insertions, change bars).
//!
//! ## The structural trap
//!
//! The central hazard is:
//!
//! > A tracked deletion spanning unbalanced syntax (e.g. half of `#figure(`) cannot be
//! > redlined by naive text injection. Parse before injecting; when a change is not
//! > inline-safe, fall back to a block-level "replaced" region.
//!
//! Naively wrapping such a span in `#review.del[…]` would put an unbalanced `(` inside a
//! content block and break compilation. The compile service is the fidelity authority and
//! owns the real Typst parser; this component is a pure, dependency-free function. So rather
//! than invoking Typst, it runs a **conservative bracket-balance heuristic**: if there is
//! any doubt that wrapping is safe, it falls back to a block-level "replaced" region.
//!
//! ## Why the block fallback is safe by construction
//!
//! The block fallback emits the span *verbatim between two standalone, self-contained
//! marker calls*:
//!
//! ```text
//! <replaced_open><span text unchanged><replaced_close>
//! ```
//!
//! Provided `replaced_open` / `replaced_close` are each complete Typst fragments (a whole
//! function call such as `#review.rep-open[]`), inserting them between existing text
//! **cannot** change whether the surrounding source compiles: the original span compiled
//! in its context, and the inserted fragments are syntactically complete on their own.
//! This is the load-bearing invariant of [`RedlineStyle`] (see [`RedlineStyle::validate`]).
//!
//! This means the redline projection satisfies the contract: *it never turns compilable
//! source into non-compilable source*. (A source that was already broken,
//! "CRDT convergence ≠ syntactic validity" — stays broken for the same reason it was
//! already broken; the injection introduces no new errors.)

use std::fmt;

use crate::Position;
use crate::mark::{MarkKind, MarkSet};
use crate::position::{TextRange, char_len};
use crate::projection::{FLAG_DELETE, FLAG_INSERT, char_flags};

/// The result of classifying whether a span can be safely wrapped in inline markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineSafety {
    /// The span's brackets are balanced and its math/raw delimiters are paired, so
    /// wrapping it in `<open>…<close>` (where the markers open and close one Typst content
    /// block) cannot introduce a syntax error.
    InlineSafe,
    /// The span contains unbalanced structural syntax and must use the block-level
    /// "replaced" fallback. Carries the reason for testability and diagnostics.
    BlockReplaced(ReplacedReason),
}

/// Why a span was judged not inline-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacedReason {
    /// An unbalanced `[`, `{` or `(` bracket (the span opens one it does not close, or
    /// closes one it did not open). This is the `#figure(` half-span case.
    UnbalancedBrackets,
    /// An odd number of `$` delimiters: the span enters or leaves math mode.
    UnbalancedMath,
    /// An odd number of backticks: the span enters or leaves a Typst raw span.
    UnbalancedRaw,
}

impl fmt::Display for ReplacedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReplacedReason::UnbalancedBrackets => "unbalanced brackets",
            ReplacedReason::UnbalancedMath => "unbalanced math delimiters",
            ReplacedReason::UnbalancedRaw => "unbalanced raw delimiters",
        })
    }
}

/// Classify a span of text as inline-safe or block-replaced.
///
/// A span is inline-safe iff, scanning it left to right:
///
/// - the nesting depth of `[`/`]`, `{`/`}` and `(`/`)` never goes negative and each ends
///   at zero; and
/// - the count of `$` delimiters is even; and
/// - the count of `` ` `` (backtick) delimiters is even.
///
/// This is conservative: some spans it flags as unsafe could in principle be wrapped
/// safely, but a span it flags as safe is genuinely safe to wrap in a single content
/// block.
#[must_use]
pub fn classify_inline_safety(span: &str) -> InlineSafety {
    let (mut sq, mut cu, mut pa) = (0i64, 0i64, 0i64);
    let (mut dollars, mut backticks) = (0u64, 0u64);
    for ch in span.chars() {
        match ch {
            '[' => sq += 1,
            ']' => {
                sq -= 1;
                if sq < 0 {
                    return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets);
                }
            }
            '{' => cu += 1,
            '}' => {
                cu -= 1;
                if cu < 0 {
                    return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets);
                }
            }
            '(' => pa += 1,
            ')' => {
                pa -= 1;
                if pa < 0 {
                    return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets);
                }
            }
            '$' => dollars += 1,
            '`' => backticks += 1,
            _ => {}
        }
    }
    if sq != 0 || cu != 0 || pa != 0 {
        return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets);
    }
    if dollars % 2 != 0 {
        return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedMath);
    }
    if backticks % 2 != 0 {
        return InlineSafety::BlockReplaced(ReplacedReason::UnbalancedRaw);
    }
    InlineSafety::InlineSafe
}

/// The marker strings the redline emitter injects.
///
/// All three pairs are **configurable**. The defaults target a Typst review package that
/// exposes `#review.add[…]` / `#review.del[…]` for inline changes and
/// `#review.rep-open[]` / `#review.rep-close[]` for the block-level "replaced" fallback.
///
/// # Safety contract
///
/// Every marker string **must be a complete, self-contained Typst fragment** (typically a
/// whole function call). This is what makes the block fallback safe by construction: a
/// complete fragment inserted between existing text cannot change whether that text
/// compiles. [`validate`](RedlineStyle::validate) checks the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedlineStyle {
    /// Inline-insertion open marker, e.g. `#review.add[`.
    pub insert_open: String,
    /// Inline-insertion close marker, e.g. `]`.
    pub insert_close: String,
    /// Inline-deletion open marker, e.g. `#review.del[`.
    pub delete_open: String,
    /// Inline-deletion close marker, e.g. `]`.
    pub delete_close: String,
    /// Block-level "replaced" open marker (a self-contained fragment).
    pub replaced_open: String,
    /// Block-level "replaced" close marker (a self-contained fragment).
    pub replaced_close: String,
}

impl RedlineStyle {
    /// Default inline-insertion open marker: `#review.add[`.
    pub const DEFAULT_INSERT_OPEN: &'static str = "#review.add[";
    /// Default inline-insertion close marker: `]`.
    pub const DEFAULT_INSERT_CLOSE: &'static str = "]";
    /// Default inline-deletion open marker: `#review.del[`.
    pub const DEFAULT_DELETE_OPEN: &'static str = "#review.del[";
    /// Default inline-deletion close marker: `]`.
    pub const DEFAULT_DELETE_CLOSE: &'static str = "]";
    /// Default block-level "replaced" open marker: `#review.rep-open[]`.
    pub const DEFAULT_REPLACED_OPEN: &'static str = "#review.rep-open[]";
    /// Default block-level "replaced" close marker: `#review.rep-close[]`.
    pub const DEFAULT_REPLACED_CLOSE: &'static str = "#review.rep-close[]";

    /// The inline marker pair for a change kind: `(open, close)`.
    #[must_use]
    pub fn inline_markers(&self, kind: MarkKind) -> (&str, &str) {
        match kind {
            MarkKind::Insert => (&self.insert_open, &self.insert_close),
            MarkKind::Delete => (&self.delete_open, &self.delete_close),
            // Comments/secrets never take the inline path.
            _ => (&self.replaced_open, &self.replaced_close),
        }
    }

    /// Construct the default review-package style.
    #[must_use]
    pub fn new_default() -> Self {
        RedlineStyle {
            insert_open: Self::DEFAULT_INSERT_OPEN.to_string(),
            insert_close: Self::DEFAULT_INSERT_CLOSE.to_string(),
            delete_open: Self::DEFAULT_DELETE_OPEN.to_string(),
            delete_close: Self::DEFAULT_DELETE_CLOSE.to_string(),
            replaced_open: Self::DEFAULT_REPLACED_OPEN.to_string(),
            replaced_close: Self::DEFAULT_REPLACED_CLOSE.to_string(),
        }
    }

    /// Validate the style's safety contract.
    ///
    /// Returns a list of human-readable problems (empty means valid). The checks are:
    ///
    /// - each inline open/close pair wraps exactly one Typst content block `[ … ]` and
    ///   carries no stray `{`, `(`, `$` or `` ` `` delimiters (those must be balanced
    ///   within each marker); and
    /// - each block `replaced_*` marker is a complete, self-contained Typst fragment
    ///   (all delimiters balanced), because the replaced content sits between them
    ///   verbatim.
    #[must_use]
    pub fn validate(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        // Insert pair.
        if !opens_one_content_block(&self.insert_open, &self.insert_close) {
            problems.push("insert_open/close do not wrap a single content block");
        }
        if !non_square_delimiters_clean(&self.insert_open) {
            problems.push("insert_open carries unbalanced braces/parens/math/raw");
        }
        if !non_square_delimiters_clean(&self.insert_close) {
            problems.push("insert_close carries unbalanced braces/parens/math/raw");
        }
        // Delete pair.
        if !opens_one_content_block(&self.delete_open, &self.delete_close) {
            problems.push("delete_open/close do not wrap a single content block");
        }
        if !non_square_delimiters_clean(&self.delete_open) {
            problems.push("delete_open carries unbalanced braces/parens/math/raw");
        }
        if !non_square_delimiters_clean(&self.delete_close) {
            problems.push("delete_close carries unbalanced braces/parens/math/raw");
        }
        // Block-level replaced markers must be complete fragments.
        if !matches!(
            classify_inline_safety(&self.replaced_open),
            InlineSafety::InlineSafe
        ) {
            problems.push("replaced_open is not a self-contained Typst fragment");
        }
        if !matches!(
            classify_inline_safety(&self.replaced_close),
            InlineSafety::InlineSafe
        ) {
            problems.push("replaced_close is not a self-contained Typst fragment");
        }
        problems
    }
}

impl Default for RedlineStyle {
    fn default() -> Self {
        Self::new_default()
    }
}

/// True iff `open` followed by `close` opens exactly one Typst content block: the square
/// depth, scanning `open` from zero, never goes negative and ends at +1, then scanning
/// `close` from +1 never goes negative and ends at zero. Other bracket types and math/raw
/// delimiters are handled by [`non_square_delimiters_clean`].
fn opens_one_content_block(open: &str, close: &str) -> bool {
    let mut d = 0i64;
    for ch in open.chars() {
        match ch {
            '[' => d += 1,
            ']' => {
                d -= 1;
                if d < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    if d != 1 {
        return false;
    }
    for ch in close.chars() {
        match ch {
            '[' => d += 1,
            ']' => {
                d -= 1;
                if d < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    d == 0
}

/// True iff `s` carries no stray delimiters other than square brackets: curly braces and
/// parentheses are balanced (never negative, net zero), and `$` / backtick counts are even.
/// Square brackets are ignored because inline open/close markers legitimately carry a
/// single unmatched `[` or `]` that their partner closes.
fn non_square_delimiters_clean(s: &str) -> bool {
    let (mut cu, mut pa) = (0i64, 0i64);
    let (mut dollars, mut backticks) = (0u64, 0u64);
    for ch in s.chars() {
        match ch {
            '{' => cu += 1,
            '}' => {
                cu -= 1;
                if cu < 0 {
                    return false;
                }
            }
            '(' => pa += 1,
            ')' => {
                pa -= 1;
                if pa < 0 {
                    return false;
                }
            }
            '$' => dollars += 1,
            '`' => backticks += 1,
            _ => {}
        }
    }
    cu == 0 && pa == 0 && dollars % 2 == 0 && backticks % 2 == 0
}

/// Emit the redline projection: the full text with change markers injected.
#[must_use]
pub fn redline(text: &str, marks: &MarkSet, style: &RedlineStyle) -> String {
    redline_with_report(text, marks, style).output
}

/// The redline output plus a report of decisions made (for tests and diagnostics).
#[derive(Debug, Clone)]
pub struct RedlineReport {
    /// The projected redline string.
    pub output: String,
    /// One entry per change run, in order, recording whether it was emitted inline or via
    /// the block-level "replaced" fallback (and why).
    pub runs: Vec<RedlineRunRecord>,
}

/// Record of a single change run's redline treatment.
#[derive(Debug, Clone)]
pub struct RedlineRunRecord {
    /// Character range of the run in the source text.
    pub range: TextRange,
    /// Whether the run is an insertion or a deletion.
    pub kind: MarkKind,
    /// How it was emitted.
    pub safety: InlineSafety,
}

/// A run of text that shares a single redline treatment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Plain,
    Inserted,
    Deleted,
}

/// Produce the redline projection together with a report of every run's safety decision.
#[must_use]
pub fn redline_with_report(text: &str, marks: &MarkSet, style: &RedlineStyle) -> RedlineReport {
    let flags = char_flags(text, marks);
    let n = char_len(text);
    let mut out = String::with_capacity(text.len() + marks.len() * 8);
    let mut runs = Vec::new();

    let mut offsets = Vec::with_capacity(n + 1);
    let mut byte = 0usize;
    for ch in text.chars() {
        offsets.push(byte);
        byte += ch.len_utf8();
    }
    offsets.push(byte);

    let mut idx = 0usize;
    while idx < n {
        let kind = flag_to_run_kind(flags[idx]);
        // Extend the run while the flags agree on the change status.
        let start = idx;
        let mut end = idx;
        while end < n && flag_to_run_kind(flags[end]) == kind {
            end += 1;
        }
        let span = char_slice(text, start, end, &offsets);
        match kind {
            RunKind::Plain => out.push_str(span),
            RunKind::Inserted => emit_change(
                &mut out,
                &mut runs,
                span,
                start,
                end,
                MarkKind::Insert,
                style,
            ),
            RunKind::Deleted => emit_change(
                &mut out,
                &mut runs,
                span,
                start,
                end,
                MarkKind::Delete,
                style,
            ),
        }
        idx = end;
    }

    RedlineReport { output: out, runs }
}

/// Classify a per-character flag byte into the redline run kind it belongs to.
fn flag_to_run_kind(f: u8) -> RunKind {
    if f & FLAG_DELETE != 0 {
        RunKind::Deleted
    } else if f & FLAG_INSERT != 0 {
        RunKind::Inserted
    } else {
        RunKind::Plain
    }
}

/// Wrap `text` in the style's inline open/close markers for `kind`.
///
/// This is the single definition of marker wrapping; the redline emitter
/// ([`emit_change`]) and the milestone-comparison redline both go through it so the
/// marker pairs can never drift apart.
pub fn wrap_change(out: &mut String, text: &str, kind: MarkKind, style: &RedlineStyle) {
    let (open, close) = style.inline_markers(kind);
    out.push_str(open);
    out.push_str(text);
    out.push_str(close);
}

fn emit_change(
    out: &mut String,
    runs: &mut Vec<RedlineRunRecord>,
    span: &str,
    start: usize,
    end: usize,
    kind: MarkKind,
    style: &RedlineStyle,
) {
    let safety = classify_inline_safety(span);
    match safety {
        InlineSafety::InlineSafe => wrap_change(out, span, kind, style),
        InlineSafety::BlockReplaced(_) => {
            out.push_str(&style.replaced_open);
            out.push_str(span);
            out.push_str(&style.replaced_close);
        }
    }
    runs.push(RedlineRunRecord {
        range: TextRange::new(
            Position::from_char_idx(u32::try_from(start).unwrap_or(u32::MAX)),
            Position::from_char_idx(u32::try_from(end).unwrap_or(u32::MAX)),
        ),
        kind,
        safety,
    });
}

/// Slice `text` by character indices `[start, end)`, using the ascending per-character
/// byte-offset table produced in a single pass. Returns an empty slice if the range is
/// empty or out of bounds.
fn char_slice<'a>(text: &'a str, start: usize, end: usize, offsets: &[usize]) -> &'a str {
    if start >= end {
        return "";
    }
    let byte_start = offsets[start];
    match offsets.get(end) {
        Some(byte_end) if byte_start <= *byte_end => &text[byte_start..*byte_end],
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mark::{AuthorId, Mark, MarkId, MarkSet, Timestamp};

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
    fn empty_text_redline_is_empty() {
        let marks = MarkSet::new();
        assert_eq!(redline("", &marks, &RedlineStyle::default()), "");
    }

    #[test]
    fn no_marks_redline_is_plain_text() {
        let marks = MarkSet::new();
        assert_eq!(
            redline("hello world", &marks, &RedlineStyle::default()),
            "hello world"
        );
    }

    #[test]
    fn simple_insertion_wraps_inline() {
        let text = "ABCDE";
        let marks = set(&[mark(1, MarkKind::Insert, 1, 3)]); // insert "BC"
        assert_eq!(
            redline(text, &marks, &RedlineStyle::default()),
            "A#review.add[BC]DE"
        );
    }

    #[test]
    fn simple_deletion_wraps_inline() {
        let text = "ABCDE";
        let marks = set(&[mark(2, MarkKind::Delete, 1, 3)]); // delete "BC"
        assert_eq!(
            redline(text, &marks, &RedlineStyle::default()),
            "A#review.del[BC]DE"
        );
    }

    #[test]
    fn unbalanced_figure_paren_falls_back_to_block() {
        // The PLAN's canonical trap: a deletion spanning half of `#figure(`.
        let text = "= Heading\n\n#figure(image(\"x\"))\n\nMore.";
        let target = "#figure(image(\"x\")";
        let start = text.find(target).expect("substring present");
        let cstart = text[..start].chars().count();
        let clen = target.chars().count();
        let cend = cstart + clen;
        let marks = set(&[mark(
            1,
            MarkKind::Delete,
            u32::try_from(cstart).unwrap(),
            u32::try_from(cend).unwrap(),
        )]);
        let report = redline_with_report(text, &marks, &RedlineStyle::default());
        assert_eq!(report.runs.len(), 1);
        assert!(matches!(
            report.runs[0].safety,
            InlineSafety::BlockReplaced(_)
        ));
        // The emitted text must NOT wrap that span in an inline delete marker.
        assert!(!report.output.contains(&format!("#review.del[{target}")));
        // It must contain the replaced markers around the verbatim span.
        assert!(report.output.contains("#review.rep-open[]"));
        assert!(report.output.contains("#review.rep-close[]"));
        assert!(report.output.contains(target));
    }

    #[test]
    fn balanced_span_is_inlined() {
        // Deleting a balanced `#figure(image("x"))` is inline-safe.
        let target = "#figure(image(\"x\"))";
        let text = format!("X{target}Y");
        let start = text.find(target).unwrap();
        let cstart = text[..start].chars().count();
        let clen = target.chars().count();
        let cend = cstart + clen;
        let marks = set(&[mark(
            1,
            MarkKind::Delete,
            u32::try_from(cstart).unwrap(),
            u32::try_from(cend).unwrap(),
        )]);
        let report = redline_with_report(&text, &marks, &RedlineStyle::default());
        assert!(matches!(report.runs[0].safety, InlineSafety::InlineSafe));
        assert_eq!(report.output, format!("X#review.del[{target}]Y"));
    }

    #[test]
    fn math_split_falls_back_to_block() {
        let span = "a $b c"; // one dollar
        let text = format!("PRE{span}POST");
        let cstart = "PRE".chars().count();
        let cend = cstart + span.chars().count();
        let marks = set(&[mark(
            1,
            MarkKind::Insert,
            u32::try_from(cstart).unwrap(),
            u32::try_from(cend).unwrap(),
        )]);
        let report = redline_with_report(&text, &marks, &RedlineStyle::default());
        assert!(matches!(
            report.runs[0].safety,
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedMath)
        ));
    }

    #[test]
    fn raw_split_falls_back_to_block() {
        let span = "a `code"; // one backtick
        let text = format!("PRE{span}POST");
        let cstart = "PRE".chars().count();
        let cend = cstart + span.chars().count();
        let marks = set(&[mark(
            1,
            MarkKind::Delete,
            u32::try_from(cstart).unwrap(),
            u32::try_from(cend).unwrap(),
        )]);
        let report = redline_with_report(&text, &marks, &RedlineStyle::default());
        assert!(matches!(
            report.runs[0].safety,
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedRaw)
        ));
    }

    #[test]
    fn classify_inline_safety_table() {
        assert_eq!(
            classify_inline_safety("plain text"),
            InlineSafety::InlineSafe
        );
        assert_eq!(classify_inline_safety("a [b] c"), InlineSafety::InlineSafe);
        assert_eq!(
            classify_inline_safety("#figure(image(\"x\"))"),
            InlineSafety::InlineSafe
        );
        assert_eq!(classify_inline_safety("$a + b$"), InlineSafety::InlineSafe);
        assert_eq!(classify_inline_safety("`code`"), InlineSafety::InlineSafe);
        assert!(matches!(
            classify_inline_safety("#figure("),
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets)
        ));
        assert!(matches!(
            classify_inline_safety("closing ) alone"),
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedBrackets)
        ));
        assert!(matches!(
            classify_inline_safety("$half math"),
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedMath)
        ));
        assert!(matches!(
            classify_inline_safety("`half raw"),
            InlineSafety::BlockReplaced(ReplacedReason::UnbalancedRaw)
        ));
        // Multibyte content is fine.
        assert_eq!(
            classify_inline_safety("Überstraße €"),
            InlineSafety::InlineSafe
        );
    }

    #[test]
    fn default_style_validates() {
        let problems = RedlineStyle::default().validate();
        assert!(
            problems.is_empty(),
            "default style should be valid: {problems:?}"
        );
    }

    #[test]
    fn bad_style_is_rejected_by_validate() {
        let style = RedlineStyle {
            insert_open: "#review.add(".to_string(), // paren, never closed by the close marker
            ..RedlineStyle::default()
        };
        let problems = style.validate();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("insert_open/close do not wrap"))
        );
    }

    #[test]
    fn redline_preserves_all_source_text() {
        // Every source character still appears in the output (markers add but never
        // remove).
        let text = "ABCDE";
        let marks = set(&[
            mark(1, MarkKind::Insert, 0, 2),
            mark(2, MarkKind::Delete, 3, 5),
        ]);
        let out = redline(text, &marks, &RedlineStyle::default());
        for ch in text.chars() {
            assert!(out.contains(ch), "output missing source char {ch:?}: {out}");
        }
    }
}
