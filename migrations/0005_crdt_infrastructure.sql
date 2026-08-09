-- CRDT infrastructure schemas for canonical document authority.
--
-- These tables support the transition from dual-authority (Postgres body + Loro CRDT)
-- to a single CRDT authority with materialized read models.
--
-- The existing documents.body/data columns remain for now as a read model but will
-- become derived from the CRDT frontier after the authority cutover.

-- CRDT Write-Ahead Log: durable record of every accepted CRDT update batch.
-- Each row is a single Loro update blob (opaque bytes) appended atomically before ACK.
CREATE TABLE crdt_wal (
    id BIGSERIAL PRIMARY KEY,
    document_id UUID NOT NULL,
    -- Monotonically increasing server-assigned sequence number per document.
    server_seq BIGINT NOT NULL,
    -- The opaque Loro update bytes.
    update_bytes BYTEA NOT NULL,
    -- Size of the update for quota enforcement.
    update_size INT NOT NULL CHECK (update_size >= 0),
    -- Actor that submitted the update (from OIDC subject).
    actor TEXT NOT NULL,
    -- Client-assigned sequence (for idempotency / dedup).
    client_seq BIGINT,
    -- Timestamp of acceptance.
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, server_seq)
);
CREATE INDEX crdt_wal_doc_seq_idx ON crdt_wal (document_id, server_seq);

-- CRDT Frontier: the current version vector / frontier per document.
-- Updated atomically when a WAL entry is appended.
CREATE TABLE crdt_frontier (
    document_id UUID PRIMARY KEY,
    -- The Loro version vector encoded as bytes.
    version_vector BYTEA NOT NULL,
    -- The server_seq of the last accepted update.
    last_server_seq BIGINT NOT NULL DEFAULT 0,
    -- Content hash of the projected text (for migration verification).
    content_hash TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Document Lease: fencing token for exclusive document actor ownership.
-- Exactly one actor can acknowledge updates for a document at a time.
CREATE TABLE document_lease (
    document_id UUID PRIMARY KEY,
    -- Monotonically increasing fencing token (incremented on each lease acquisition).
    fencing_token BIGINT NOT NULL,
    -- The actor instance that holds the lease (process ID / pod name).
    lease_holder TEXT NOT NULL,
    -- When the lease was acquired.
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the lease expires (the holder must renew before this).
    expires_at TIMESTAMPTZ NOT NULL
);

-- Checkpoint: immutable snapshot of a document's complete state at a point in time.
-- Referenced by compile, history comparison, restore, export, and audit.
CREATE TABLE checkpoint (
    id UUID PRIMARY KEY,
    document_id UUID NOT NULL,
    -- The CRDT frontier at this checkpoint (version vector bytes).
    frontier BYTEA NOT NULL,
    -- Object-store reference to the snapshot blob.
    snapshot_ref TEXT NOT NULL,
    -- SHA-256 checksum of the snapshot blob.
    snapshot_checksum TEXT,
    -- Creator of the checkpoint.
    created_by TEXT NOT NULL,
    -- Human-readable reason/label for the checkpoint.
    label TEXT,
    -- Parent checkpoint(s) for history chain (nullable for initial).
    parent_id UUID REFERENCES checkpoint(id),
    -- Profile/template version at checkpoint time.
    profile_version TEXT,
    -- Compiler toolchain image digest at checkpoint time.
    toolchain_digest TEXT,
    -- Compilation date policy.
    compiled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX checkpoint_doc_idx ON checkpoint (document_id, created_at DESC);
