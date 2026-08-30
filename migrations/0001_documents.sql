-- Librarian owns this schema entirely. infernal-law's own migrations never
-- reference these tables, and this repository's migrations never reference
-- infernal-law's -- Librarian is deletable and rebuildable without
-- affecting kernel correctness (see README's Architecture section).

CREATE TABLE IF NOT EXISTS documents (
    document_id uuid PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp()
);

-- Documents are immutable per version: updating a document creates a new
-- row here rather than overwriting an existing one. "Current version" is
-- always MAX(version) for a document_id -- no separate head pointer, kept
-- deliberately simple (see README's Domain model section).
CREATE TABLE IF NOT EXISTS document_versions (
    document_id uuid NOT NULL REFERENCES documents (document_id),
    version bigint NOT NULL CHECK (version >= 1),
    content text NOT NULL,
    content_type text NOT NULL CHECK (char_length(content_type) BETWEEN 1 AND 200),
    title text CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 500),
    source_uri text CHECK (source_uri IS NULL OR char_length(source_uri) BETWEEN 1 AND 2000),
    content_digest bytea NOT NULL CHECK (octet_length(content_digest) = 32),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (document_id, version)
);

CREATE INDEX IF NOT EXISTS document_versions_latest_idx
    ON document_versions (document_id, version DESC);

-- Librarian's own domain idempotency boundary (see README's "Domain
-- idempotency" section): infernal-law's own Request/route/claim
-- machinery guarantees a governed Request is accepted exactly once, but
-- says nothing about how many times Librarian itself might process that
-- same accepted Request (a reclaimed route after Librarian's own crash,
-- for example, delivers the same Request again). Keying this table by
-- the kernel's own request_id -- stable, and already the caller's
-- retry/idempotency handle for the governed call itself -- means
-- reprocessing the same request_id is recognized and short-circuited
-- before any second domain mutation happens.
CREATE TABLE IF NOT EXISTS put_operations (
    request_id uuid PRIMARY KEY,
    document_id uuid NOT NULL REFERENCES documents (document_id),
    version bigint NOT NULL,
    processed_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    FOREIGN KEY (document_id, version) REFERENCES document_versions (document_id, version)
);
