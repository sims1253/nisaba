-- Project-scoped lookup indexes.

CREATE INDEX reference_entries_project_idx ON reference_entries(project_id);

CREATE INDEX project_memberships_subject_idx ON project_memberships(subject);
