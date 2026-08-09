-- Hash share-link tokens instead of storing plaintext.
-- The token column is renamed to token_hash and stores a SHA-256 hex digest.
-- Plaintext tokens are returned once at creation time and never persisted.
ALTER TABLE share_links RENAME COLUMN token TO token_hash;
