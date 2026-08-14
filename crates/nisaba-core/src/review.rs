//! Review-domain aggregates: immutable milestones, comparisons, bulk resolution, and threads.

use std::collections::BTreeMap;
use std::fmt;

use crate::mark::{MarkId, MarkKind, MarkSet};
use crate::position::TextRange;
use crate::project::{DocumentPath, Project};
use crate::projection::View;
use crate::redline::{RedlineStyle, wrap_change};
use crate::resolution::{Resolution, ResolutionError, resolve};

/// Stable location of a document in a project snapshot.
pub type SnapshotPath = DocumentPath;

/// One immutable projected document captured by a milestone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedSnapshot {
    /// Location in the project.
    pub path: SnapshotPath,
    /// Projection used when the snapshot was made.
    pub view: View,
    /// Projected Typst source.
    pub text: String,
}

/// A named, immutable view of a project at one point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    name: String,
    view: View,
    snapshots: BTreeMap<SnapshotPath, ProjectedSnapshot>,
}

impl Milestone {
    /// Make a milestone from a project. The resulting value has no link to the project and
    /// cannot change when the live project changes.
    #[must_use]
    pub fn capture(project: &Project, name: impl Into<String>, view: View) -> Self {
        let snapshots = project
            .documents()
            .map(|document| {
                let path = document.path().clone();
                let snapshot = ProjectedSnapshot {
                    path: path.clone(),
                    view,
                    text: document.document().project(view),
                };
                (path, snapshot)
            })
            .collect();
        Self {
            name: name.into(),
            view,
            snapshots,
        }
    }

    /// The human-readable milestone name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The view captured by this milestone.
    #[must_use]
    pub fn view(&self) -> View {
        self.view
    }

    /// Snapshots in deterministic path order.
    pub fn snapshots(&self) -> impl Iterator<Item = &ProjectedSnapshot> {
        self.snapshots.values()
    }

    /// Find one captured document.
    #[must_use]
    pub fn snapshot(&self, path: &SnapshotPath) -> Option<&ProjectedSnapshot> {
        self.snapshots.get(path)
    }

    /// Compare this milestone with `other` (this is the old side, `other` the new side).
    #[must_use]
    pub fn compare(&self, other: &Milestone) -> MilestoneComparison {
        compare_milestones(self, other)
    }
}

/// A text change between two milestone snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotChange {
    /// Text present only in the old snapshot.
    Delete(String),
    /// Text present only in the new snapshot.
    Insert(String),
    /// Text shared by both snapshots.
    Equal(String),
}

/// Comparison of two immutable milestones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MilestoneComparison {
    /// Old milestone name.
    pub from: String,
    /// New milestone name.
    pub to: String,
    /// Per-document changes, including documents added or removed.
    pub documents: BTreeMap<SnapshotPath, Vec<SnapshotChange>>,
}

impl MilestoneComparison {
    /// Render all document changes with the supplied redline style. This is a source-level
    /// comparison and does not invoke Typst.
    #[must_use]
    pub fn redline(&self, style: &RedlineStyle) -> BTreeMap<SnapshotPath, String> {
        self.documents
            .iter()
            .map(|(path, changes)| {
                let mut output = String::new();
                for change in changes {
                    match change {
                        SnapshotChange::Delete(text) => {
                            wrap_change(&mut output, text, MarkKind::Delete, style);
                        }
                        SnapshotChange::Insert(text) => {
                            wrap_change(&mut output, text, MarkKind::Insert, style);
                        }
                        SnapshotChange::Equal(text) => output.push_str(text),
                    }
                }
                (path.clone(), output)
            })
            .collect()
    }
}

/// Compare two milestones with a deterministic tiered diff (lines, then words, then
/// characters). Memory is linear in the snapshot sizes; see [`diff_chars`].
#[must_use]
pub fn compare_milestones(from: &Milestone, to: &Milestone) -> MilestoneComparison {
    let paths: std::collections::BTreeSet<_> = from
        .snapshots
        .keys()
        .chain(to.snapshots.keys())
        .cloned()
        .collect();
    let documents = paths
        .into_iter()
        .map(|path| {
            let old = from.snapshots.get(&path).map_or("", |s| s.text.as_str());
            let new = to.snapshots.get(&path).map_or("", |s| s.text.as_str());
            (path, diff_chars(old, new))
        })
        .collect();
    MilestoneComparison {
        from: from.name.clone(),
        to: to.name.clone(),
        documents,
    }
}

/// Diff two texts into [`SnapshotChange`] runs with **linear** memory.
///
/// The previous implementation built a full quadratic LCS matrix of the character
/// sequences (two 50k-character snapshots would have needed ~20 GB) and allocated one
/// `String` per character. This one instead:
///
/// 1. diffs *lines* with a Hirschberg divide-and-conquer LCS (two rolling rows of `u32`,
///    so `O(min(lines))` memory per invocation);
/// 2. refines each changed region at *word* granularity, then at *character* granularity,
///    with a size cap above which a region is emitted as one delete plus one insert
///    rather than refined;
/// 3. builds each run from a contiguous `&str` slice of the input, so a run costs one
///    `String` allocation regardless of its length.
///
/// Output semantics are unchanged: concatenating the `Equal` + `Delete` texts reproduces
/// `old`, and concatenating the `Equal` + `Insert` texts reproduces `new` (both are
/// asserted in tests). Exact chunking of equal/changed runs may differ from any
/// particular LCS tie-break, but is a deterministic function of the inputs.
fn diff_chars(old: &str, new: &str) -> Vec<SnapshotChange> {
    let mut changes = Vec::new();
    if old == new {
        if !old.is_empty() {
            changes.push(SnapshotChange::Equal(old.to_owned()));
        }
        return changes;
    }
    diff_region(old, new, Tier::Line, &mut changes);
    changes
}

/// Tokenize `text` into lines, each including its trailing `\n` (the last line may lack
/// one). The tokens exactly partition the input.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            lines.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Tokenize `text` into alternating word/whitespace atoms. The tokens exactly partition
/// the input.
fn split_words(text: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut start = 0;
    let mut in_whitespace = text.chars().next().is_some_and(char::is_whitespace);
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() != in_whitespace {
            words.push(&text[start..i]);
            start = i;
            in_whitespace = !in_whitespace;
        }
    }
    if start < text.len() {
        words.push(&text[start..]);
    }
    words
}

/// Above this combined character count a changed region is not refined at the next finer
/// tier (and above `DIFF_WORK_LIMIT` the LCS itself is skipped): the region is emitted as
/// one delete plus one insert. This keeps both time and memory bounded on degenerate
/// inputs (for example a single 50k-character line) at the cost of coarser chunking.
const REFINE_LIMIT_CHARS: usize = 8192;

/// Upper bound on DP cell updates for one LCS invocation (`a.len() * b.len()`). Above it
/// the sequences are treated as a wholesale replacement. Hirschberg sub-problems partition
/// their parent's grid, so children of an invocation that passed this check also pass it.
const DIFF_WORK_LIMIT: usize = 64 * 1024 * 1024;

/// A run produced by the generic diff: its length in elements (`Equal`/`Delete` consume
/// elements of `a`, `Equal`/`Insert` consume elements of `b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    /// A run of elements common to both sides.
    Equal(usize),
    /// A run of elements present only in `a`.
    Delete(usize),
    /// A run of elements present only in `b`.
    Insert(usize),
}

/// Hirschberg linear-space LCS diff over element slices, emitted as compact runs.
///
/// Common prefixes and suffixes are trimmed before any DP work, which makes
/// modestly-differing inputs run in near-linear time; total DP work is bounded by
/// [`DIFF_WORK_LIMIT`] cell updates, above which the inputs are emitted as a wholesale
/// replacement (correct but coarse). Because Hirschberg sub-problems partition their
/// parent's grid, every recursive call of an invocation that passed the limit also passes.
fn diff_seq<T: PartialEq>(a: &[T], b: &[T]) -> Vec<DiffOp> {
    let mut out = Vec::new();
    diff_rec(a, b, &mut out);
    out
}

fn diff_rec<T: PartialEq>(a: &[T], b: &[T], out: &mut Vec<DiffOp>) {
    if a.is_empty() {
        push_op(out, DiffOp::Insert(b.len()));
        return;
    }
    if b.is_empty() {
        push_op(out, DiffOp::Delete(a.len()));
        return;
    }
    // Written as a division so the product cannot overflow; `b` is non-empty here.
    if a.len() > DIFF_WORK_LIMIT / b.len() {
        push_op(out, DiffOp::Delete(a.len()));
        push_op(out, DiffOp::Insert(b.len()));
        return;
    }
    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    push_op(out, DiffOp::Equal(prefix));
    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];
    if a_mid.is_empty() {
        push_op(out, DiffOp::Insert(b_mid.len()));
    } else if b_mid.is_empty() {
        push_op(out, DiffOp::Delete(a_mid.len()));
    } else if a_mid.len() == 1 {
        // Base case: match the single element against its first occurrence in `b_mid`.
        if let Some(k) = b_mid.iter().position(|x| *x == a_mid[0]) {
            push_op(out, DiffOp::Insert(k));
            push_op(out, DiffOp::Equal(1));
            push_op(out, DiffOp::Insert(b_mid.len() - k - 1));
        } else {
            push_op(out, DiffOp::Delete(1));
            push_op(out, DiffOp::Insert(b_mid.len()));
        }
    } else {
        // Hirschberg split: the b-split maximizing the LCS through a's midpoint.
        let mid = a_mid.len() / 2;
        let left = lcs_row_prefix(&a_mid[..mid], b_mid);
        let right = lcs_row_suffix(&a_mid[mid..], b_mid);
        let mut split = 0usize;
        let mut best = left[0] + right[0];
        for (j, (l, r)) in left.iter().zip(&right).enumerate() {
            if l + r > best {
                best = l + r;
                split = j;
            }
        }
        diff_rec(&a_mid[..mid], &b_mid[..split], out);
        diff_rec(&a_mid[mid..], &b_mid[split..], out);
    }
    push_op(out, DiffOp::Equal(suffix));
}

/// `row[j] = LCS(a, b[..j])` for every `j`, via two rolling rows of `u32`.
fn lcs_row_prefix<T: PartialEq>(a: &[T], b: &[T]) -> Vec<u32> {
    let mut prev = vec![0u32; b.len() + 1];
    let mut cur = vec![0u32; b.len() + 1];
    for x in a {
        for j in 0..b.len() {
            cur[j + 1] = if *x == b[j] {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

/// `row[j] = LCS(a, b[j..])` for every `j`, via two rolling rows of `u32`.
fn lcs_row_suffix<T: PartialEq>(a: &[T], b: &[T]) -> Vec<u32> {
    let mut prev = vec![0u32; b.len() + 1];
    let mut cur = vec![0u32; b.len() + 1];
    for x in a.iter().rev() {
        for j in (0..b.len()).rev() {
            cur[j] = if *x == b[j] {
                prev[j + 1] + 1
            } else {
                cur[j + 1].max(prev[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

fn push_op(out: &mut Vec<DiffOp>, op: DiffOp) {
    let len = match op {
        DiffOp::Equal(n) | DiffOp::Delete(n) | DiffOp::Insert(n) => n,
    };
    if len == 0 {
        return;
    }
    let same = matches!(
        (out.last(), op),
        (Some(DiffOp::Equal(_)), DiffOp::Equal(_))
            | (Some(DiffOp::Delete(_)), DiffOp::Delete(_))
            | (Some(DiffOp::Insert(_)), DiffOp::Insert(_))
    );
    if same && let Some(last) = out.last_mut() {
        match (last, op) {
            (DiffOp::Equal(a), DiffOp::Equal(b))
            | (DiffOp::Delete(a), DiffOp::Delete(b))
            | (DiffOp::Insert(a), DiffOp::Insert(b)) => *a += b,
            // Guarded by `same` above.
            _ => unreachable!("run kinds differ"),
        }
    } else {
        out.push(op);
    }
}

/// The granularity at which a region is currently being diffed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Whole lines (each including its trailing newline).
    Line,
    /// Whitespace-delimited words.
    Word,
    /// Single characters (the terminal tier).
    Char,
}

impl Tier {
    fn next(self) -> Self {
        match self {
            Tier::Line => Tier::Word,
            Tier::Word | Tier::Char => Tier::Char,
        }
    }
}

/// Diff one region at `tier`, recursing into finer tiers inside each changed group.
fn diff_region(old: &str, new: &str, tier: Tier, out: &mut Vec<SnapshotChange>) {
    if tier == Tier::Char {
        refine_chars(old, new, out);
        return;
    }
    let old_tokens = if tier == Tier::Line {
        split_lines(old)
    } else {
        split_words(old)
    };
    let new_tokens = if tier == Tier::Line {
        split_lines(new)
    } else {
        split_words(new)
    };
    let ops = diff_seq(&old_tokens, &new_tokens);
    // Cursors (token index + byte offset) so every emitted run is one contiguous slice.
    let mut ai = 0usize;
    let mut bi = 0usize;
    let mut old_byte = 0usize;
    let mut new_byte = 0usize;
    let mut i = 0;
    while i < ops.len() {
        match ops[i] {
            DiffOp::Equal(n) => {
                let start = old_byte;
                old_byte += byte_len(&old_tokens[ai..ai + n]);
                new_byte += byte_len(&new_tokens[bi..bi + n]);
                ai += n;
                bi += n;
                push_change(out, SnapshotChange::Equal(old[start..old_byte].to_owned()));
                i += 1;
            }
            DiffOp::Delete(_) | DiffOp::Insert(_) => {
                // One replacement group: every consecutive delete/insert run.
                let mut del = 0usize;
                let mut ins = 0usize;
                while i < ops.len() && !matches!(ops[i], DiffOp::Equal(_)) {
                    match ops[i] {
                        DiffOp::Delete(n) => del += n,
                        DiffOp::Insert(n) => ins += n,
                        DiffOp::Equal(_) => unreachable!("guarded by the while condition"),
                    }
                    i += 1;
                }
                let old_start = old_byte;
                old_byte += byte_len(&old_tokens[ai..ai + del]);
                ai += del;
                let new_start = new_byte;
                new_byte += byte_len(&new_tokens[bi..bi + ins]);
                bi += ins;
                let old_region = &old[old_start..old_byte];
                let new_region = &new[new_start..new_byte];
                if old_region.chars().count() + new_region.chars().count() > REFINE_LIMIT_CHARS {
                    // Too large to refine at the next tier: emit the region as one
                    // replacement. Keeps time and memory bounded on degenerate inputs.
                    push_change(out, SnapshotChange::Delete(old_region.to_owned()));
                    push_change(out, SnapshotChange::Insert(new_region.to_owned()));
                } else {
                    diff_region(old_region, new_region, tier.next(), out);
                }
            }
        }
    }
}

/// Character-level refinement of one changed region: an LCS over `char`s whose runs are
/// mapped back to byte slices of the region.
fn refine_chars(old: &str, new: &str, out: &mut Vec<SnapshotChange>) {
    if old.is_empty() {
        push_change(out, SnapshotChange::Insert(new.to_owned()));
        return;
    }
    if new.is_empty() {
        push_change(out, SnapshotChange::Delete(old.to_owned()));
        return;
    }
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let ops = diff_seq(&a, &b);
    let mut ai = 0usize;
    let mut bi = 0usize;
    let mut old_byte = 0usize;
    let mut new_byte = 0usize;
    for op in ops {
        match op {
            DiffOp::Equal(n) => {
                let old_len = utf8_len(&a[ai..ai + n]);
                let new_len = utf8_len(&b[bi..bi + n]);
                push_change(
                    out,
                    SnapshotChange::Equal(old[old_byte..old_byte + old_len].to_owned()),
                );
                old_byte += old_len;
                new_byte += new_len;
                ai += n;
                bi += n;
            }
            DiffOp::Delete(n) => {
                let old_len = utf8_len(&a[ai..ai + n]);
                push_change(
                    out,
                    SnapshotChange::Delete(old[old_byte..old_byte + old_len].to_owned()),
                );
                old_byte += old_len;
                ai += n;
            }
            DiffOp::Insert(n) => {
                let new_len = utf8_len(&b[bi..bi + n]);
                push_change(
                    out,
                    SnapshotChange::Insert(new[new_byte..new_byte + new_len].to_owned()),
                );
                new_byte += new_len;
                bi += n;
            }
        }
    }
}

fn utf8_len(chars: &[char]) -> usize {
    chars.iter().map(|c| c.len_utf8()).sum()
}

fn byte_len(tokens: &[&str]) -> usize {
    tokens.iter().map(|t| t.len()).sum()
}

/// Append `change`, merging it into the previous run when the kinds agree.
fn push_change(out: &mut Vec<SnapshotChange>, change: SnapshotChange) {
    let same = matches!(
        (out.last(), &change),
        (Some(SnapshotChange::Equal(_)), SnapshotChange::Equal(_))
            | (Some(SnapshotChange::Delete(_)), SnapshotChange::Delete(_))
            | (Some(SnapshotChange::Insert(_)), SnapshotChange::Insert(_))
    );
    if same && let Some(last) = out.last_mut() {
        match (last, &change) {
            (SnapshotChange::Equal(s), SnapshotChange::Equal(t))
            | (SnapshotChange::Delete(s), SnapshotChange::Delete(t))
            | (SnapshotChange::Insert(s), SnapshotChange::Insert(t)) => s.push_str(t),
            // Guarded by `same` above.
            _ => unreachable!("run kinds differ"),
        }
    } else {
        out.push(change);
    }
}

/// Summary of one mark's resolution inside a bulk batch. Records what the step did
/// without retaining a cumulative copy of the text and marks (the full final state lives
/// once on [`BulkResolutionOutcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkResolutionOperation {
    /// The resolved mark.
    pub id: MarkId,
    /// Whether characters were removed from the text by this step.
    pub text_changed: bool,
    /// The character range this step removed from the text, if any.
    pub removed_range: Option<TextRange>,
    /// Number of surviving marks whose range this step adjusted.
    pub remapped: usize,
    /// Ids of marks whose range became empty as a result of this step.
    pub emptied: Vec<MarkId>,
}

/// Result of resolving a deterministic batch of change marks.
#[derive(Debug, Clone)]
pub struct BulkResolutionOutcome {
    /// Summary for every selected mark, in deterministic occurrence order.
    pub operations: Vec<BulkResolutionOperation>,
    /// The final text and mark set.
    pub text: String,
    /// The final marks.
    pub marks: MarkSet,
}

/// Resolve selected changes in occurrence order (timestamp, author, id), not caller order.
/// This makes overlapping batches converge and makes the overlap rule explicit: an earlier
/// destructive operation clips the ranges seen by later operations.
pub fn resolve_bulk(
    text: &str,
    marks: &MarkSet,
    ids: impl IntoIterator<Item = MarkId>,
    resolution: Resolution,
) -> Result<BulkResolutionOutcome, ResolutionError> {
    let mut selected: Vec<_> = ids
        .into_iter()
        .filter_map(|id| marks.get(id).map(|m| (id, m)))
        .collect();
    selected.sort_by(|(_, a), (_, b)| a.occurrence_order(b));
    let mut current_text = text.to_owned();
    let mut current_marks = marks.clone();
    let mut operations = Vec::with_capacity(selected.len());
    for (id, _) in selected {
        if current_marks.get(id).is_none() {
            continue;
        }
        let outcome = resolve(&current_text, &current_marks, id, resolution)?;
        operations.push(BulkResolutionOperation {
            id,
            text_changed: outcome.text_changed,
            removed_range: outcome.removed_range,
            remapped: outcome.remapped,
            emptied: outcome.emptied,
        });
        // Move the step's state forward instead of cloning it: the summaries above keep
        // `operations` O(steps) rather than O(steps × document size).
        current_text = outcome.text;
        current_marks = outcome.marks;
    }
    Ok(BulkResolutionOutcome {
        operations,
        text: current_text,
        marks: current_marks,
    })
}

/// A stable id for a discussion thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommentThreadId(u64);
impl CommentThreadId {
    /// Construct an id.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    /// Raw id.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Thread lifecycle state. Orphaned is distinct from resolved so no discussion disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentState {
    /// The thread is active.
    Open,
    /// The thread was explicitly resolved.
    Resolved,
    /// Its anchor no longer points at surviving content.
    Orphaned,
}

/// One message in a comment thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentMessage {
    /// Message author.
    pub author: String,
    /// Message body.
    pub body: String,
    /// Logical message timestamp.
    pub timestamp: u64,
}

/// A discussion anchored by a comment mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentThread {
    /// Stable thread id.
    pub id: CommentThreadId,
    /// Comment mark carrying the anchor range.
    pub anchor: MarkId,
    /// Messages in deterministic insertion order.
    pub messages: Vec<CommentMessage>,
    /// Current lifecycle state.
    pub state: CommentState,
}

impl CommentThread {
    /// Create an open thread.
    #[must_use]
    pub fn new(id: CommentThreadId, anchor: MarkId) -> Self {
        Self {
            id,
            anchor,
            messages: Vec::new(),
            state: CommentState::Open,
        }
    }
    /// Add a message while retaining insertion order.
    pub fn add_message(&mut self, message: CommentMessage) {
        self.messages.push(message);
    }

    /// Explicitly resolve this discussion.
    pub fn resolve(&mut self) {
        self.state = CommentState::Resolved;
    }

    /// Reopen a discussion after a user explicitly wants to continue it.
    pub fn reopen(&mut self) {
        self.state = CommentState::Open;
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> CommentState {
        self.state
    }
}

/// Refresh thread states against the current document. Empty/missing comment anchors are
/// orphaned; resolving remains an explicit user action and is never inferred.
pub fn refresh_comment_threads(
    threads: &mut BTreeMap<CommentThreadId, CommentThread>,
    marks: &MarkSet,
) {
    // Computed once for all threads, not once per thread.
    let deletes = marks.of_kind(MarkKind::Delete);
    for thread in threads.values_mut() {
        if let Some(anchor) = marks.get(thread.anchor) {
            if anchor.kind != MarkKind::Comment
                || anchor.range.is_empty()
                || crate::validation::is_range_fully_deleted(anchor.range, &deletes)
            {
                thread.state = CommentState::Orphaned;
            } else if thread.state == CommentState::Orphaned {
                thread.state = CommentState::Open;
            }
        } else {
            thread.state = CommentState::Orphaned;
        }
    }
}

impl fmt::Display for CommentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Orphaned => "orphaned",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;
    use crate::position::Position;
    use crate::project::{DocumentPath, Project};
    use crate::{AuthorId, Mark, Timestamp};

    fn mark(id: u64, kind: MarkKind, s: u32, e: u32) -> Mark {
        Mark::new(
            MarkId::new(id),
            TextRange::new(Position::from_char_idx(s), Position::from_char_idx(e)),
            kind,
            AuthorId::new("a"),
            Timestamp::new(id),
            None,
        )
    }

    #[test]
    fn bulk_order_is_not_caller_order() {
        let text = "abcd";
        let marks: MarkSet = [
            mark(2, MarkKind::Delete, 2, 3),
            mark(1, MarkKind::Delete, 0, 1),
        ]
        .into_iter()
        .collect();
        let out = resolve_bulk(
            text,
            &marks,
            [MarkId::new(2), MarkId::new(1)],
            Resolution::Accept,
        )
        .unwrap();
        assert_eq!(out.text, "bd");
        assert_eq!(
            out.operations
                .iter()
                .map(|x| x.id.as_u64())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn overlapping_accept_batch_is_independent_of_caller_order() {
        let text = "abcdef";
        let marks: MarkSet = [
            mark(1, MarkKind::Delete, 1, 4),
            mark(2, MarkKind::Delete, 3, 5),
        ]
        .into_iter()
        .collect();
        let occurrence = resolve_bulk(
            text,
            &marks,
            [MarkId::new(1), MarkId::new(2)],
            Resolution::Accept,
        )
        .unwrap();
        let reversed = resolve_bulk(
            text,
            &marks,
            [MarkId::new(2), MarkId::new(1)],
            Resolution::Accept,
        )
        .unwrap();
        assert_eq!(occurrence.text, reversed.text);
        assert_eq!(occurrence.text, "af");
    }

    #[test]
    fn milestone_redline_marks_changes_between_captures() {
        let mut project = Project::new("p1");
        let path = DocumentPath::new("chapters/a.typ").unwrap();
        project.insert_document(path.clone(), Document::from_text("The quick brown fox."));
        let before = Milestone::capture(&project, "before", View::Proposed);
        project
            .document_mut(&path)
            .expect("document exists")
            .set_text("The quick brown cat.");
        let after = Milestone::capture(&project, "after", View::Proposed);

        let comparison = before.compare(&after);
        assert_eq!(comparison.from, "before");
        assert_eq!(comparison.to, "after");
        let style = RedlineStyle {
            insert_open: "{{+".into(),
            insert_close: "+}}".into(),
            delete_open: "{{-".into(),
            delete_close: "-}}".into(),
            ..RedlineStyle::new_default()
        };
        let redline = comparison.redline(&style);
        let rendered = redline.get(&path).expect("path captured");
        // The changed word is wrapped in delete/insert markers around the shared text.
        assert_eq!(rendered, "The quick brown {{-fox-}}{{+cat+}}.");
    }

    #[test]
    fn milestone_comparison_covers_added_and_removed_documents() {
        let mut project = Project::new("p1");
        let a = DocumentPath::new("a.typ").unwrap();
        project.insert_document(a.clone(), Document::from_text("alpha"));
        let before = Milestone::capture(&project, "before", View::Proposed);

        let b = DocumentPath::new("b.typ").unwrap();
        project.insert_document(b.clone(), Document::from_text("beta"));
        project.remove_document(&a);
        let after = Milestone::capture(&project, "after", View::Proposed);

        let comparison = before.compare(&after);
        // `a` exists only in the old snapshot: one delete run with its whole text.
        assert_eq!(
            comparison.documents.get(&a),
            Some(&vec![SnapshotChange::Delete("alpha".into())])
        );
        // `b` exists only in the new snapshot: one insert run.
        assert_eq!(
            comparison.documents.get(&b),
            Some(&vec![SnapshotChange::Insert("beta".into())])
        );
    }

    #[test]
    fn diff_reconstructs_both_sides() {
        // Whatever the chunking, concatenating Equal+Delete must reproduce the old text
        // and Equal+Insert the new text — the load-bearing semantic of SnapshotChange.
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("", "inserted"),
            ("deleted", ""),
            ("same", "same"),
            ("The quick brown fox.", "The quick brown cat."),
            ("one two three", "one three"),
            ("one three", "one two three"),
            ("a\nb\nc\nd", "a\nB\nc"),
            ("über Straße", "uber Strasse"),
            ("x", "y"),
        ];
        for (old, new) in cases {
            let changes = diff_chars(old, new);
            let rebuilt_old: String = changes
                .iter()
                .map(|c| match c {
                    SnapshotChange::Equal(t) | SnapshotChange::Delete(t) => t.as_str(),
                    SnapshotChange::Insert(_) => "",
                })
                .collect();
            let rebuilt_new: String = changes
                .iter()
                .map(|c| match c {
                    SnapshotChange::Equal(t) | SnapshotChange::Insert(t) => t.as_str(),
                    SnapshotChange::Delete(_) => "",
                })
                .collect();
            assert_eq!(
                &rebuilt_old, old,
                "old reconstruction for {old:?} vs {new:?}"
            );
            assert_eq!(
                &rebuilt_new, new,
                "new reconstruction for {old:?} vs {new:?}"
            );
            // Runs are compact: no two adjacent runs share a kind.
            let kind = |c: &SnapshotChange| match c {
                SnapshotChange::Equal(_) => 0,
                SnapshotChange::Delete(_) => 1,
                SnapshotChange::Insert(_) => 2,
            };
            for pair in changes.windows(2) {
                assert_ne!(kind(&pair[0]), kind(&pair[1]), "unmerged runs: {changes:?}");
            }
        }
    }

    #[test]
    fn diff_of_large_snapshots_completes_quickly() {
        // Two ~50k-character snapshots with modest scattered edits. Regression test for
        // the quadratic-matrix diff: this must run in linear-ish time and memory.
        let paragraph = |i: usize| {
            format!(
                "Paragraph {i}: the deterministic model keeps every character ever \
                 typed, including characters marked deleted by a pending suggestion.\n"
            )
        };
        let old: String = (0..400).map(paragraph).collect();
        assert!(old.len() > 49_000, "fixture must be ~50k chars");
        let mut new = old.clone();
        for i in [3usize, 57, 121, 199, 250, 311, 390] {
            let replacement = format!(
                "Paragraph {i}: every character ever typed is kept by the deterministic model.\n"
            );
            let target = paragraph(i);
            let start = new.find(&target).expect("paragraph present");
            new.replace_range(start..start + target.len(), &replacement);
        }
        assert_eq!(old.len().max(new.len()), old.len());
        let changes = diff_chars(&old, &new);
        // The invariant holds at scale too.
        let rebuilt_old: String = changes
            .iter()
            .map(|c| match c {
                SnapshotChange::Equal(t) | SnapshotChange::Delete(t) => t.as_str(),
                SnapshotChange::Insert(_) => "",
            })
            .collect();
        let rebuilt_new: String = changes
            .iter()
            .map(|c| match c {
                SnapshotChange::Equal(t) | SnapshotChange::Insert(t) => t.as_str(),
                SnapshotChange::Delete(_) => "",
            })
            .collect();
        assert_eq!(rebuilt_old, old);
        assert_eq!(rebuilt_new, new);
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, SnapshotChange::Delete(_)))
        );
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, SnapshotChange::Insert(_)))
        );
    }

    #[test]
    fn comment_thread_becomes_orphaned_under_pending_delete() {
        let comment = mark(7, MarkKind::Comment, 1, 3);
        let delete = mark(8, MarkKind::Delete, 1, 3);
        let marks: MarkSet = [comment, delete].into_iter().collect();
        let mut threads = BTreeMap::new();
        let mut thread = CommentThread::new(CommentThreadId::new(1), MarkId::new(7));
        thread.add_message(CommentMessage {
            author: "reviewer".into(),
            body: "Please check".into(),
            timestamp: 1,
        });
        threads.insert(thread.id, thread);
        refresh_comment_threads(&mut threads, &marks);
        assert_eq!(
            threads[&CommentThreadId::new(1)].state(),
            CommentState::Orphaned
        );
        threads
            .get_mut(&CommentThreadId::new(1))
            .expect("thread exists")
            .resolve();
        assert_eq!(
            threads[&CommentThreadId::new(1)].state(),
            CommentState::Resolved
        );
    }
}
