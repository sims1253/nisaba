//! Persistence and generic document-path regression tests.

#[test]
fn document_updates_use_atomic_revision_predicate() {
    let source = include_str!("../src/persistence.rs");
    let update = source
        .lines()
        .find(|line| line.contains("UPDATE documents SET"))
        .expect("document update query");
    assert!(update.contains("WHERE id=$1 AND revision=$9"));
}

#[test]
fn initial_schema_uses_unique_project_relative_paths() {
    let schema = include_str!("../../../migrations/0001_initial.sql");
    assert!(schema.contains("UNIQUE (project_id, path)"));
    assert!(schema.contains("path text NOT NULL"));
}
