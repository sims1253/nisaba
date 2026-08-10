-- Projects with references could not be deleted: `reference_entries.project_id`
-- was ON DELETE RESTRICT while every other project child cascades. `DELETE FROM
-- projects` then hit FK 23503, which the app mapped to a misleading 404, rolled
-- the transaction back, and the project survived. Match the rest of the schema:
-- deleting a project deletes its reference metadata rows (the app handler
-- removes the fulltext blobs from object storage first).
ALTER TABLE reference_entries
    DROP CONSTRAINT reference_entries_project_id_fkey,
    ADD CONSTRAINT reference_entries_project_id_fkey
        FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE;
