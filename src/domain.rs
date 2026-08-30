//! Goal: own everything about what a Librarian document *is* and how it is
//! stored, versioned, and searched -- independent of how a request to act
//! on one arrived. Nothing in this module knows infernal-law exists.

use std::fmt::{self, Display, Formatter};

use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A single immutable version of a document's content. Never overwritten;
/// updating a document appends a new version instead (see this crate's
/// README, "Domain model").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentVersion {
    pub document_id: Uuid,
    pub version: i64,
    pub content: String,
    pub content_type: String,
    pub title: Option<String>,
    pub source_uri: Option<String>,
    pub content_digest: [u8; 32],
    pub created_at_unix: i64,
}

/// One search hit: enough for a caller to understand the match without
/// exposing arbitrary Librarian database access (README, "Search").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchHit {
    pub document_id: Uuid,
    pub version: i64,
    pub title: Option<String>,
    pub snippet: String,
}

/// A `librarian.document.put` input, already validated shape-wise (fields
/// present and non-empty where required) but not yet persisted.
pub struct PutDocument {
    /// `None` creates a brand new document; `Some` appends a new version
    /// to an existing one. The current kernel-adapter integration
    /// (`kernel_adapter.rs`) only ever creates new documents today -- see
    /// its own module documentation for why -- but the domain layer
    /// supports both, and this is exercised directly by domain tests.
    pub document_id: Option<Uuid>,
    pub content: String,
    pub content_type: String,
    pub title: Option<String>,
    pub source_uri: Option<String>,
}

impl PutDocument {
    pub fn content_digest(&self) -> [u8; 32] {
        Sha256::digest(self.content.as_bytes()).into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentError {
    NotFound,
    EmptyContent,
    EmptyContentType,
    /// A `document_id` was supplied that does not exist -- `put` cannot
    /// append a version to a document that was never created.
    UnknownDocument,
    Repository,
}

impl Display for DocumentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("document was not found"),
            Self::EmptyContent => formatter.write_str("document content must not be empty"),
            Self::EmptyContentType => formatter.write_str("content type must not be empty"),
            Self::UnknownDocument => {
                formatter.write_str("cannot version a document that does not exist")
            }
            Self::Repository => formatter.write_str("document repository failed"),
        }
    }
}

impl std::error::Error for DocumentError {}

/// One idempotent domain operation, keyed by the kernel's own stable
/// request ID -- this is Librarian's own domain idempotency boundary
/// (README, "Domain idempotency"). infernal-law's Request/route/claim
/// machinery guarantees a governed Request is *accepted* exactly once;
/// it says nothing about how many times Librarian itself might process
/// that accepted Request (for example, a route reclaimed after Librarian
/// crashes between committing its own domain write and completing the
/// kernel claim -- see README, "domain commit succeeded, but kernel
/// completion was not recorded"). Recognizing a repeated `request_id`
/// before performing a second domain mutation is what makes that safe to
/// retry.
pub struct PutOutcome {
    pub document_id: Uuid,
    pub version: i64,
    /// Whether this call actually performed the domain mutation, or
    /// recognized `request_id` as already-processed and returned the
    /// existing result untouched.
    pub was_already_processed: bool,
}

pub trait DocumentRepository: Send + Sync {
    /// Idempotently performs a `librarian.document.put` for `request_id`:
    /// if this exact `request_id` was already processed, returns the
    /// existing result without writing anything new. Otherwise persists
    /// `document` as a new document (if `document.document_id` is `None`)
    /// or a new version of an existing one, and records `request_id`
    /// against the result in the same transaction so the two can never
    /// diverge.
    fn put(&self, request_id: Uuid, document: PutDocument) -> Result<PutOutcome, DocumentError>;

    /// Resolves a document at `version`, or its latest version if `None`.
    fn get(
        &self,
        document_id: Uuid,
        version: Option<i64>,
    ) -> Result<DocumentVersion, DocumentError>;

    /// Full-text searches the latest version of every document for
    /// `query`, most relevant first, bounded to `limit` results.
    fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>, DocumentError>;
}
