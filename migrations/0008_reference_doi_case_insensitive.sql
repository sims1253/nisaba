-- DOIs are case-insensitive per the DOI handbook, but the 0006 unique index is
-- byte-exact, so "10.1000/QA" and "10.1000/qa" could both be added to one
-- project. Replace it with a lower() expression index. The stored value keeps
-- its original spelling; only uniqueness is compared case-insensitively.
DROP INDEX reference_entries_project_doi_idx;
CREATE UNIQUE INDEX reference_entries_project_doi_ci_idx
    ON reference_entries (project_id, LOWER(metadata->>'doi'))
    WHERE metadata->>'doi' IS NOT NULL AND metadata->>'doi' <> '';
