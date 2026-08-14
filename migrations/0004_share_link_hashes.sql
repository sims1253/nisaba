-- Hash share-link tokens instead of storing plaintext.
-- The token column is renamed to token_hash and stores a SHA-256 hex digest.
-- Plaintext tokens are returned once at creation time and never persisted.
--
-- No backfill was performed for rows created before this rename (their
-- token_hash column holds the original plaintext token): the project was
-- pre-release with zero deployments, so no such rows existed outside tests.
ALTER TABLE share_links RENAME COLUMN token TO token_hash;
