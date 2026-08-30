-- Simple domain-owned search (README's Search section): PostgreSQL
-- full-text search over each document version's own content and title.
-- No vector embeddings, no external search engine, no graph database --
-- deliberately deferred until the service/kernel boundary this project
-- exists to prove is itself proven.

ALTER TABLE document_versions
    ADD COLUMN IF NOT EXISTS search_vector tsvector
        GENERATED ALWAYS AS (
            setweight(to_tsvector('english', coalesce(title, '')), 'A')
            || setweight(to_tsvector('english', content), 'B')
        ) STORED;

CREATE INDEX IF NOT EXISTS document_versions_search_idx
    ON document_versions USING gin (search_vector);
