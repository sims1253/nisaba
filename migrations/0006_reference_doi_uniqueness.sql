-- Enforce project-scoped DOI uniqueness so duplicate references are rejected
-- with a clean 409 (Postgres 23505 -> RepoError::Conflict) instead of silently
-- accepted. The expression index only covers non-empty DOIs.
CREATE UNIQUE INDEX reference_entries_project_doi_idx
    ON reference_entries (project_id, (metadata->>'doi'))
    WHERE metadata->>'doi' IS NOT NULL AND metadata->>'doi' <> '';
