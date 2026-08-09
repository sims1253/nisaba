//! A general project containing documents addressed by stable paths.
//!
//! This is intentionally a flat content model. Folders are represented by path
//! segments rather than domain-specific hierarchy types, so the same interface
//! works for a single note, a book, a knowledge base, or a multi-file Typst project.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::Document;

/// Opaque, comparable project identifier.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectId(Arc<str>);

impl ProjectId {
    /// Construct a project identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(Arc::from(id.into().into_boxed_str()))
    }

    /// Return the identifier as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Stable, project-relative path of a document.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentPath(Arc<str>);

impl DocumentPath {
    /// Construct a normalized relative path.
    ///
    /// Backslashes become slashes, repeated separators are collapsed, and
    /// leading separators are removed. Empty, `.` and `..` segments are rejected.
    pub fn new(path: impl AsRef<str>) -> Result<Self, InvalidDocumentPath> {
        let normalized = path.as_ref().replace('\\', "/");
        let segments: Vec<_> = normalized
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        if segments.is_empty()
            || segments
                .iter()
                .any(|segment| matches!(*segment, "." | ".."))
        {
            return Err(InvalidDocumentPath);
        }
        Ok(Self(Arc::from(segments.join("/").into_boxed_str())))
    }

    /// Return the normalized project-relative path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DocumentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

impl std::fmt::Display for DocumentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A path was empty, absolute after normalization, or contained traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDocumentPath;

impl std::fmt::Display for InvalidDocumentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("document path must be relative and contain no traversal segments")
    }
}

impl std::error::Error for InvalidDocumentPath {}

/// A document stored at a project-relative path.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectDocument {
    path: DocumentPath,
    document: Document,
}

impl ProjectDocument {
    /// Construct a project document.
    #[must_use]
    pub fn new(path: DocumentPath, document: Document) -> Self {
        Self { path, document }
    }

    /// Stable path of this document.
    #[must_use]
    pub fn path(&self) -> &DocumentPath {
        &self.path
    }

    /// Document content and review state.
    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Mutable document content and review state.
    #[must_use]
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }
}

/// A project is an ordered set of path-addressed documents.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    id: ProjectId,
    documents: BTreeMap<DocumentPath, ProjectDocument>,
}

impl Project {
    /// Construct an empty project.
    #[must_use]
    pub fn new(id: impl Into<ProjectId>) -> Self {
        Self {
            id: id.into(),
            documents: BTreeMap::new(),
        }
    }

    /// Project identifier.
    #[must_use]
    pub fn id(&self) -> &ProjectId {
        &self.id
    }

    /// Insert or replace a document at `path`.
    pub fn insert_document(
        &mut self,
        path: DocumentPath,
        document: Document,
    ) -> &mut ProjectDocument {
        let key = path.clone();
        self.documents
            .insert(key.clone(), ProjectDocument::new(path, document));
        self.documents.get_mut(&key).expect("inserted document")
    }

    /// Find a document by path.
    #[must_use]
    pub fn document(&self, path: &DocumentPath) -> Option<&Document> {
        self.documents.get(path).map(ProjectDocument::document)
    }

    /// Find a document mutably by path.
    #[must_use]
    pub fn document_mut(&mut self, path: &DocumentPath) -> Option<&mut Document> {
        self.documents
            .get_mut(path)
            .map(ProjectDocument::document_mut)
    }

    /// Remove a document by path.
    pub fn remove_document(&mut self, path: &DocumentPath) -> Option<ProjectDocument> {
        self.documents.remove(path)
    }

    /// Iterate documents in deterministic path order.
    pub fn documents(&self) -> impl Iterator<Item = &ProjectDocument> {
        self.documents.values()
    }

    /// Number of documents.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// Whether the project contains no documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

impl From<&str> for ProjectId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ProjectId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_normalized_and_traversal_is_rejected() {
        assert_eq!(
            DocumentPath::new("/notes//ideas.typ").unwrap().as_str(),
            "notes/ideas.typ"
        );
        assert_eq!(
            DocumentPath::new(r"notes\ideas.typ").unwrap().as_str(),
            "notes/ideas.typ"
        );
        assert!(DocumentPath::new("../secret.typ").is_err());
        assert!(DocumentPath::new("").is_err());
    }

    #[test]
    fn documents_are_ordered_by_path_and_keep_identity_on_edit() {
        let mut project = Project::new("p1");
        let a = DocumentPath::new("chapters/a.typ").unwrap();
        let b = DocumentPath::new("chapters/b.typ").unwrap();
        project.insert_document(b.clone(), Document::from_text("B"));
        project.insert_document(a.clone(), Document::from_text("A"));

        assert_eq!(
            project
                .documents()
                .map(|document| document.path().as_str())
                .collect::<Vec<_>>(),
            ["chapters/a.typ", "chapters/b.typ"]
        );
        project.document_mut(&a).unwrap().set_text("updated");
        assert_eq!(project.document(&a).unwrap().text(), "updated");
    }
}
