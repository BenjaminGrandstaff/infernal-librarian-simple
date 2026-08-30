//! Goal: implement `DocumentRepository` against Librarian's own PostgreSQL
//! database with fixed, parameterized SQL only -- the same discipline
//! infernal-law's own repository adapters use, applied here to a database
//! the kernel never touches and never needs to know exists.
//!
//! UUIDs travel as `text` parameters cast to `uuid` in SQL (`$1::text::uuid`),
//! matching infernal-law's own repository adapters, rather than pulling in
//! a separate `uuid`-crate-to-`postgres` integration feature for the same
//! effect.

use uuid::Uuid;

use crate::database::Database;
use crate::domain::{
    DocumentError, DocumentRepository, DocumentVersion, PutDocument, PutOutcome, SearchHit,
};

const FIND_PUT_OPERATION_SQL: &str =
    "SELECT document_id::text, version FROM put_operations WHERE request_id = $1::text::uuid";
const INSERT_DOCUMENT_SQL: &str = "INSERT INTO documents (document_id) VALUES ($1::text::uuid)";
const LOCK_DOCUMENT_SQL: &str =
    "SELECT 1 FROM documents WHERE document_id = $1::text::uuid FOR UPDATE";
const NEXT_VERSION_SQL: &str = "SELECT COALESCE(MAX(version), 0) + 1 FROM document_versions \
    WHERE document_id = $1::text::uuid";
const INSERT_VERSION_SQL: &str = "INSERT INTO document_versions \
    (document_id, version, content, content_type, title, source_uri, content_digest) \
    VALUES ($1::text::uuid, $2, $3, $4, $5, $6, $7)";
const INSERT_PUT_OPERATION_SQL: &str = "INSERT INTO put_operations \
    (request_id, document_id, version) VALUES ($1::text::uuid, $2::text::uuid, $3) \
    ON CONFLICT (request_id) DO NOTHING";
const FIND_VERSION_SQL: &str = "SELECT document_id::text, version, content, content_type, title, \
        source_uri, content_digest, extract(epoch FROM created_at)::bigint AS created_at_unix \
    FROM document_versions WHERE document_id = $1::text::uuid AND version = $2";
const FIND_LATEST_VERSION_SQL: &str = "SELECT document_id::text, version, content, content_type, title, \
        source_uri, content_digest, extract(epoch FROM created_at)::bigint AS created_at_unix \
    FROM document_versions WHERE document_id = $1::text::uuid ORDER BY version DESC LIMIT 1";
const SEARCH_SQL: &str = "SELECT document_id::text, version, title, snippet FROM ( \
        SELECT DISTINCT ON (dv.document_id) dv.document_id, dv.version, dv.title, \
            ts_headline('english', dv.content, plainto_tsquery('english', $1)) AS snippet, \
            ts_rank(dv.search_vector, plainto_tsquery('english', $1)) AS rank \
        FROM document_versions dv \
        WHERE dv.search_vector @@ plainto_tsquery('english', $1) \
        ORDER BY dv.document_id, dv.version DESC \
    ) AS latest_matching_version \
    ORDER BY rank DESC \
    LIMIT $2";

#[derive(Clone)]
pub struct PostgresDocumentRepository {
    database: Database,
}

impl PostgresDocumentRepository {
    pub const fn new(database: Database) -> Self {
        Self { database }
    }

    /// Exposes the underlying connection pool so a caller (`main.rs`'s
    /// health endpoint) can share it rather than opening a second one.
    pub fn database(&self) -> &Database {
        &self.database
    }
}

fn parse_document_id(
    row: &r2d2_postgres::postgres::Row,
    column: &str,
) -> Result<Uuid, DocumentError> {
    row.get::<_, String>(column)
        .parse()
        .map_err(|_| DocumentError::Repository)
}

impl DocumentRepository for PostgresDocumentRepository {
    fn put(&self, request_id: Uuid, document: PutDocument) -> Result<PutOutcome, DocumentError> {
        if document.content.trim().is_empty() {
            return Err(DocumentError::EmptyContent);
        }
        if document.content_type.trim().is_empty() {
            return Err(DocumentError::EmptyContentType);
        }
        let digest = document.content_digest();
        let request_id_text = request_id.to_string();

        let mut connection = self
            .database
            .connection()
            .map_err(|_| DocumentError::Repository)?;
        if let Some(row) = connection
            .query_opt(FIND_PUT_OPERATION_SQL, &[&request_id_text])
            .map_err(|_| DocumentError::Repository)?
        {
            return Ok(PutOutcome {
                document_id: parse_document_id(&row, "document_id")?,
                version: row.get("version"),
                was_already_processed: true,
            });
        }

        let mut transaction = connection
            .transaction()
            .map_err(|_| DocumentError::Repository)?;
        let (document_id, version) = match document.document_id {
            None => {
                let document_id = Uuid::new_v4();
                transaction
                    .execute(INSERT_DOCUMENT_SQL, &[&document_id.to_string()])
                    .map_err(|_| DocumentError::Repository)?;
                (document_id, 1_i64)
            }
            Some(document_id) => {
                let document_id_text = document_id.to_string();
                let exists = transaction
                    .query_opt(LOCK_DOCUMENT_SQL, &[&document_id_text])
                    .map_err(|_| DocumentError::Repository)?;
                if exists.is_none() {
                    return Err(DocumentError::UnknownDocument);
                }
                let next_version: i64 = transaction
                    .query_one(NEXT_VERSION_SQL, &[&document_id_text])
                    .map_err(|_| DocumentError::Repository)?
                    .get(0);
                (document_id, next_version)
            }
        };
        let document_id_text = document_id.to_string();

        transaction
            .execute(
                INSERT_VERSION_SQL,
                &[
                    &document_id_text,
                    &version,
                    &document.content,
                    &document.content_type,
                    &document.title,
                    &document.source_uri,
                    &digest.as_slice(),
                ],
            )
            .map_err(|_| DocumentError::Repository)?;

        let inserted = transaction
            .execute(
                INSERT_PUT_OPERATION_SQL,
                &[&request_id_text, &document_id_text, &version],
            )
            .map_err(|_| DocumentError::Repository)?;
        if inserted == 0 {
            // Lost a race with a concurrent processor of the same
            // request_id (the kernel's own claim uniqueness makes this
            // rare -- see README's "Domain idempotency" -- but the
            // repository stays correct even if it happens): discard this
            // attempt's write and report the winner's already-committed
            // result instead.
            transaction
                .rollback()
                .map_err(|_| DocumentError::Repository)?;
            let row = connection
                .query_one(FIND_PUT_OPERATION_SQL, &[&request_id_text])
                .map_err(|_| DocumentError::Repository)?;
            return Ok(PutOutcome {
                document_id: parse_document_id(&row, "document_id")?,
                version: row.get("version"),
                was_already_processed: true,
            });
        }

        transaction
            .commit()
            .map_err(|_| DocumentError::Repository)?;
        Ok(PutOutcome {
            document_id,
            version,
            was_already_processed: false,
        })
    }

    fn get(
        &self,
        document_id: Uuid,
        version: Option<i64>,
    ) -> Result<DocumentVersion, DocumentError> {
        let mut connection = self
            .database
            .connection()
            .map_err(|_| DocumentError::Repository)?;
        let document_id_text = document_id.to_string();
        let row = match version {
            Some(version) => connection.query_opt(FIND_VERSION_SQL, &[&document_id_text, &version]),
            None => connection.query_opt(FIND_LATEST_VERSION_SQL, &[&document_id_text]),
        }
        .map_err(|_| DocumentError::Repository)?
        .ok_or(DocumentError::NotFound)?;

        let digest: Vec<u8> = row.get("content_digest");
        let content_digest: [u8; 32] = digest.try_into().map_err(|_| DocumentError::Repository)?;
        Ok(DocumentVersion {
            document_id: parse_document_id(&row, "document_id")?,
            version: row.get("version"),
            content: row.get("content"),
            content_type: row.get("content_type"),
            title: row.get("title"),
            source_uri: row.get("source_uri"),
            content_digest,
            created_at_unix: row.get("created_at_unix"),
        })
    }

    fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>, DocumentError> {
        let mut connection = self
            .database
            .connection()
            .map_err(|_| DocumentError::Repository)?;
        connection
            .query(SEARCH_SQL, &[&query, &limit])
            .map_err(|_| DocumentError::Repository)?
            .iter()
            .map(|row| {
                Ok(SearchHit {
                    document_id: parse_document_id(row, "document_id")?,
                    version: row.get("version"),
                    title: row.get("title"),
                    snippet: row.get("snippet"),
                })
            })
            .collect()
    }
}
