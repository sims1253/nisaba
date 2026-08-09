//! Verify CRDT infrastructure exists for canonical document authority.
//!
//! This regression test asserts the intended invariant (CRDT schemas exist)
//! rather than the buggy state (body column as sole authority).

#[cfg(test)]
mod tests {
    /// The CRDT WAL, frontier, lease, and checkpoint tables must exist in
    /// the migration set, providing the infrastructure for canonical document
    /// authority. The documents table body column remains as a read model but
    /// must not be the sole authority after the canonical-authority cutover.
    #[test]
    fn crdt_infrastructure_migration_exists() {
        let migration = include_str!("../../../migrations/0005_crdt_infrastructure.sql");

        // The CRDT WAL table must exist for durable update logging.
        assert!(
            migration.contains("CREATE TABLE crdt_wal"),
            "Migration 0005 must create crdt_wal table"
        );
        // The frontier table must exist for version vector tracking.
        assert!(
            migration.contains("CREATE TABLE crdt_frontier"),
            "Migration 0005 must create crdt_frontier table"
        );
        // The lease table must exist for document actor fencing.
        assert!(
            migration.contains("CREATE TABLE document_lease"),
            "Migration 0005 must create document_lease table"
        );
        // The checkpoint table must exist for immutable snapshots.
        assert!(
            migration.contains("CREATE TABLE checkpoint"),
            "Migration 0005 must create checkpoint table"
        );
    }
}
