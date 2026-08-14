-- Drop the dead CRDT infrastructure schema and a redundant index.
--
-- Migration 0005 created crdt_wal, crdt_frontier, document_lease, and
-- checkpoint for a planned "single CRDT authority with materialized read
-- models" cutover that never shipped: no code (app, sync, compile, web)
-- ever referenced these tables, and document persistence stayed on
-- documents.body via the app's PATCH/autosave path.
--
-- 0005 itself must stay in the tree: sqlx refuses to start ("migration 5
-- was previously applied but is missing in the resolved migrations") against
-- any database that already applied it, and development/QA databases exist.
-- Fresh installs create the dead objects at 0005 and drop them again here;
-- existing databases skip straight to the drop. Both converge on the same
-- schema.
--
-- Also drop documents_project_idx (from 0001): documents UNIQUE (project_id,
-- path) already serves queries by project_id through its leading column, so
-- the single-column index is pure write overhead.
--
-- No backfill needed anywhere in this migration: both changes only remove
-- unused objects.
DROP TABLE IF EXISTS checkpoint;
DROP TABLE IF EXISTS document_lease;
DROP TABLE IF EXISTS crdt_frontier;
DROP TABLE IF EXISTS crdt_wal;
DROP INDEX IF EXISTS documents_project_idx;
