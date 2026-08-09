-- Document revision history: stores a snapshot of the body every time a
-- document is patched, enabling Overleaf-style version diffs.
CREATE TABLE document_revisions (
    id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    project_id uuid NOT NULL,
    body text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 0),
    author text,
    created_at timestamptz NOT NULL
);
CREATE INDEX document_revisions_doc_idx ON document_revisions(document_id, created_at DESC);

-- Shareable links: an opaque token grants project-scoped access at a fixed role,
-- so a project owner can share a read-only or reviewer link without per-user
-- invitation. `expires_at` is nullable (NULL = no expiry).
CREATE TABLE share_links (
    token text PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('author', 'reviewer', 'read-only')),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL,
    expires_at timestamptz,
    label text
);
CREATE INDEX share_links_project_idx ON share_links(project_id);
