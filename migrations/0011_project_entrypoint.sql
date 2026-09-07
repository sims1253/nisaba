-- Document identity keeps the compilation entrypoint stable across renames.
ALTER TABLE projects ADD COLUMN entry_document_id uuid
    REFERENCES documents(id) ON DELETE SET NULL;
