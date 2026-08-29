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
fn project_listing_orders_by_recent_touch() {
    let source = include_str!("../src/persistence.rs");
    let list = source
        .lines()
        .find(|line| line.contains("FROM projects ORDER BY"))
        .expect("project list query");
    assert!(list.contains("ORDER BY updated_at DESC, id"));
}

#[test]
fn every_document_write_path_touches_the_project_timestamp() {
    // create, update, and delete document must each bump the owning
    // project's updated_at so the recently-touched-first listing (and the
    // projects screen's "edited …" label) reflects writing, not renames.
    let source = include_str!("../src/persistence.rs");
    assert_eq!(source.matches("touch_project(&mut tx").count(), 3);
}

#[test]
fn initial_schema_uses_unique_project_relative_paths() {
    let schema = include_str!("../../../migrations/0001_initial.sql");
    assert!(schema.contains("UNIQUE (project_id, path)"));
    assert!(schema.contains("path text NOT NULL"));
}
