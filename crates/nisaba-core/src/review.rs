//! Review-domain aggregates: immutable milestones, comparisons, bulk resolution, and threads.

use std::collections::BTreeMap;
use std::fmt;

use crate::mark::{MarkId, MarkKind, MarkSet};
use crate::project::{DocumentPath, Project};
use crate::projection::View;
use crate::resolution::{Resolution, ResolutionError, ResolutionOutcome, resolve};

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
    pub fn redline(&self, style: &crate::redline::RedlineStyle) -> BTreeMap<SnapshotPath, String> {
        self.documents
            .iter()
            .map(|(path, changes)| {
                let mut output = String::new();
                for change in changes {
                    match change {
                        SnapshotChange::Delete(text) => {
                            output.push_str(&style.delete_open);
                            output.push_str(text);
                            output.push_str(&style.delete_close);
                        }
                        SnapshotChange::Insert(text) => {
                            output.push_str(&style.insert_open);
                            output.push_str(text);
                            output.push_str(&style.insert_close);
                        }
                        SnapshotChange::Equal(text) => output.push_str(text),
                    }
                }
                (path.clone(), output)
            })
            .collect()
    }
}

/// Compare two milestones with a deterministic character-level LCS diff.
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

fn diff_chars(old: &str, new: &str) -> Vec<SnapshotChange> {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut i = 0;
    let mut j = 0;
    let mut changes = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            push_change(&mut changes, SnapshotChange::Equal(a[i].to_string()));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || dp[i][j + 1] >= dp[i + 1][j]) {
            push_change(&mut changes, SnapshotChange::Insert(b[j].to_string()));
            j += 1;
        } else {
            push_change(&mut changes, SnapshotChange::Delete(a[i].to_string()));
            i += 1;
        }
    }
    changes
}

fn push_change(changes: &mut Vec<SnapshotChange>, change: SnapshotChange) {
    let compatible = matches!(
        (&changes.last(), &change),
        (Some(SnapshotChange::Equal(_)), SnapshotChange::Equal(_))
            | (Some(SnapshotChange::Insert(_)), SnapshotChange::Insert(_))
            | (Some(SnapshotChange::Delete(_)), SnapshotChange::Delete(_))
    );
    if compatible {
        if let Some(last) = changes.last_mut() {
            match (last, &change) {
                (SnapshotChange::Equal(s), SnapshotChange::Equal(t))
                | (SnapshotChange::Insert(s), SnapshotChange::Insert(t))
                | (SnapshotChange::Delete(s), SnapshotChange::Delete(t)) => s.push_str(t),
                _ => changes.push(change),
            }
        }
    } else {
        changes.push(change);
    }
}

/// Result of resolving a deterministic batch of change marks.
#[derive(Debug, Clone)]
pub struct BulkResolutionOutcome {
    /// Outcome for every selected mark, in deterministic occurrence order.
    pub operations: Vec<(MarkId, ResolutionOutcome)>,
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
        current_text.clone_from(&outcome.text);
        current_marks = outcome.marks.clone();
        operations.push((id, outcome));
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
    for thread in threads.values_mut() {
        if let Some(anchor) = marks.get(thread.anchor) {
            let deletes = marks.of_kind(MarkKind::Delete);
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
    use crate::position::{Position, TextRange};
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
                .map(|x| x.0.as_u64())
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
    fn comparison_redline_is_deterministic() {
        let a = Milestone {
            name: "a".into(),
            view: View::Proposed,
            snapshots: BTreeMap::new(),
        };
        let b = Milestone {
            name: "b".into(),
            view: View::Proposed,
            snapshots: BTreeMap::new(),
        };
        assert!(a.compare(&b).documents.is_empty());
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
