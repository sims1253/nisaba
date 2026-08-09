-- Initial schema: generic project → path-addressed document model.
--
-- Projects contain documents addressed by a logical path (e.g. "main.typ",
-- "chapters/intro.typ"). No product-specific hierarchy is imposed.

CREATE TABLE projects (
    id uuid PRIMARY KEY,
    name text NOT NULL UNIQUE,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE project_memberships (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    subject text NOT NULL,
    role text NOT NULL CHECK (role IN ('owner', 'author', 'reviewer', 'read-only')),
    created_at timestamptz NOT NULL,
    PRIMARY KEY (project_id, subject)
);

CREATE TABLE documents (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    data jsonb NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 0),
    updated_at timestamptz NOT NULL,
    UNIQUE (project_id, path)
);
CREATE INDEX documents_project_idx ON documents(project_id);

CREATE TABLE reference_entries (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    metadata jsonb NOT NULL,
    provenance jsonb,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE fulltexts (
    reference_id uuid PRIMARY KEY REFERENCES reference_entries(id) ON DELETE CASCADE,
    blob_ref text NOT NULL UNIQUE,
    filename text NOT NULL,
    content_type text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    checksum_sha256 text,
    uploaded_at timestamptz NOT NULL
);

CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    actor text NOT NULL,
    action text NOT NULL,
    resource_type text NOT NULL,
    resource_id uuid NOT NULL,
    at timestamptz NOT NULL,
    details jsonb NOT NULL
);
CREATE INDEX audit_events_project_at_idx ON audit_events(project_id, at, id);
