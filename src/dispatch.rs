//! Goal: interpret a routed Request's namespaced `action` and perform the
//! corresponding Librarian domain operation -- the boundary between "the
//! kernel handed us governed work" and "Librarian's own domain knows what
//! to do with it." Nothing below this module knows infernal-law exists;
//! nothing above it knows what a Document is.
//!
//! ## The kernel's Request has no payload field
//!
//! infernal-law's MVP `Request` carries only a namespaced `action`, a
//! `scope` string (bounded to 200 characters), and schema version
//! references -- ILK-006 artifact/content mediation (a real payload
//! channel) is explicitly Future Kernel, not built yet. Rather than
//! route around that with a Librarian-invented side channel (a direct
//! upload endpoint, a second signed protocol, lateral communication to
//! the caller), this reference service treats it exactly like the
//! architecture's own "Results" limitation: accept the constraint, keep
//! functionality proportionally small, and document the gap plainly (see
//! this repository's README, "Kernel payload limitations") rather than
//! build an accidental abstraction that would need to be unwound once
//! ILK-006 exists.
//!
//! Concretely, `scope` carries:
//! - `librarian.document.put` -- the document's raw text content
//!   directly (so puts are capped at 200 bytes for now; `content_type` is
//!   always `text/plain`, and `title`/`source_uri` are not populated
//!   through this path -- the domain layer supports both, exercised by
//!   domain tests, independent of this limitation);
//! - `librarian.document.get` -- a document ID, optionally followed by
//!   `@<version>` for a specific version (latest otherwise);
//! - `librarian.document.search` -- the raw query text.

use uuid::Uuid;

use crate::domain::{DocumentRepository, PutDocument};
use crate::error::LibrarianError;

pub const PUT_ACTION: &str = "librarian.document.put";
pub const GET_ACTION: &str = "librarian.document.get";
pub const SEARCH_ACTION: &str = "librarian.document.search";

/// Every action this service subscribes to and performs. Kept in one
/// place so `lib.rs`'s startup subscription-provisioning step and this
/// module's dispatch can never silently drift apart.
pub const ACTIONS: [&str; 3] = [PUT_ACTION, GET_ACTION, SEARCH_ACTION];

const SEARCH_RESULT_LIMIT: i64 = 10;

/// What one dispatch performed, for the caller (the main loop) to log.
/// Librarian does not return this to the original caller through the
/// kernel -- see the module documentation above and the README's
/// "Results" section for why not, and how an operator can still observe
/// it (directly against Librarian's own database).
#[derive(Debug)]
pub enum DispatchOutcome {
    Put {
        document_id: Uuid,
        version: i64,
        was_already_processed: bool,
    },
    Get {
        document_id: Uuid,
        version: i64,
        content_type: String,
    },
    Search {
        query: String,
        hit_count: usize,
    },
}

pub fn dispatch(
    action: &str,
    request_id: Uuid,
    scope: &str,
    repository: &dyn DocumentRepository,
) -> Result<DispatchOutcome, LibrarianError> {
    match action {
        PUT_ACTION => {
            let outcome = repository.put(
                request_id,
                PutDocument {
                    document_id: None,
                    content: scope.to_owned(),
                    content_type: "text/plain".to_owned(),
                    title: None,
                    source_uri: None,
                },
            )?;
            Ok(DispatchOutcome::Put {
                document_id: outcome.document_id,
                version: outcome.version,
                was_already_processed: outcome.was_already_processed,
            })
        }
        GET_ACTION => {
            let (document_id, version) = parse_get_scope(scope)?;
            let document = repository.get(document_id, version)?;
            Ok(DispatchOutcome::Get {
                document_id: document.document_id,
                version: document.version,
                content_type: document.content_type,
            })
        }
        SEARCH_ACTION => {
            let hits = repository.search(scope, SEARCH_RESULT_LIMIT)?;
            Ok(DispatchOutcome::Search {
                query: scope.to_owned(),
                hit_count: hits.len(),
            })
        }
        other => Err(LibrarianError::UnknownAction(other.to_owned())),
    }
}

fn parse_get_scope(scope: &str) -> Result<(Uuid, Option<i64>), LibrarianError> {
    match scope.split_once('@') {
        Some((document_id, version)) => {
            let document_id = document_id
                .parse()
                .map_err(|_| LibrarianError::MalformedScope("document ID must be a UUID"))?;
            let version = version
                .parse()
                .map_err(|_| LibrarianError::MalformedScope("version must be an integer"))?;
            Ok((document_id, Some(version)))
        }
        None => {
            let document_id = scope
                .parse()
                .map_err(|_| LibrarianError::MalformedScope("document ID must be a UUID"))?;
            Ok((document_id, None))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::domain::{DocumentError, DocumentVersion, SearchHit};

    use super::*;

    #[derive(Default)]
    struct FakeRepository {
        put_calls: Mutex<Vec<(Uuid, PutDocument)>>,
        get_result: Option<DocumentVersion>,
        search_result: Vec<SearchHit>,
    }

    impl DocumentRepository for FakeRepository {
        fn put(
            &self,
            request_id: Uuid,
            document: PutDocument,
        ) -> Result<crate::domain::PutOutcome, DocumentError> {
            let document_id = document.document_id.unwrap_or_else(Uuid::new_v4);
            self.put_calls.lock().unwrap().push((request_id, document));
            Ok(crate::domain::PutOutcome {
                document_id,
                version: 1,
                was_already_processed: false,
            })
        }

        fn get(
            &self,
            _document_id: Uuid,
            _version: Option<i64>,
        ) -> Result<DocumentVersion, DocumentError> {
            self.get_result.clone().ok_or(DocumentError::NotFound)
        }

        fn search(&self, _query: &str, _limit: i64) -> Result<Vec<SearchHit>, DocumentError> {
            Ok(self.search_result.clone())
        }
    }

    fn document_version() -> DocumentVersion {
        DocumentVersion {
            document_id: Uuid::new_v4(),
            version: 3,
            content: "hello".to_owned(),
            content_type: "text/plain".to_owned(),
            title: None,
            source_uri: None,
            content_digest: [0; 32],
            created_at_unix: 10,
        }
    }

    #[test]
    fn put_forwards_scope_as_content_with_a_new_document_id() {
        let repository = FakeRepository::default();
        let request_id = Uuid::new_v4();

        let outcome = dispatch(PUT_ACTION, request_id, "hello world", &repository).unwrap();

        assert!(matches!(
            outcome,
            DispatchOutcome::Put {
                was_already_processed: false,
                ..
            }
        ));
        let calls = repository.put_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, request_id);
        assert_eq!(calls[0].1.content, "hello world");
        assert_eq!(calls[0].1.content_type, "text/plain");
        assert!(calls[0].1.document_id.is_none());
    }

    #[test]
    fn get_parses_a_bare_document_id_as_the_latest_version() {
        let version = document_version();
        let repository = FakeRepository {
            get_result: Some(version.clone()),
            ..FakeRepository::default()
        };

        let outcome = dispatch(
            GET_ACTION,
            Uuid::new_v4(),
            &version.document_id.to_string(),
            &repository,
        )
        .unwrap();

        assert!(matches!(outcome, DispatchOutcome::Get { version: 3, .. }));
    }

    #[test]
    fn get_parses_an_explicit_version_suffix() {
        let version = document_version();
        let repository = FakeRepository {
            get_result: Some(version.clone()),
            ..FakeRepository::default()
        };
        let scope = format!("{}@2", version.document_id);

        let outcome = dispatch(GET_ACTION, Uuid::new_v4(), &scope, &repository).unwrap();

        assert!(matches!(outcome, DispatchOutcome::Get { .. }));
    }

    #[test]
    fn get_rejects_a_non_uuid_scope() {
        let repository = FakeRepository::default();

        assert!(matches!(
            dispatch(GET_ACTION, Uuid::new_v4(), "not-a-uuid", &repository),
            Err(LibrarianError::MalformedScope(_))
        ));
    }

    #[test]
    fn search_forwards_scope_as_the_query() {
        let repository = FakeRepository {
            search_result: vec![SearchHit {
                document_id: Uuid::new_v4(),
                version: 1,
                title: None,
                snippet: "...match...".to_owned(),
            }],
            ..FakeRepository::default()
        };

        let outcome = dispatch(SEARCH_ACTION, Uuid::new_v4(), "hello", &repository).unwrap();

        match outcome {
            DispatchOutcome::Search { query, hit_count } => {
                assert_eq!(query, "hello");
                assert_eq!(hit_count, 1);
            }
            other => panic!("expected Search, got {other:?}"),
        }
    }

    #[test]
    fn unknown_actions_are_rejected_without_touching_the_repository() {
        let repository = FakeRepository::default();

        let result = dispatch(
            "librarian.document.delete",
            Uuid::new_v4(),
            "x",
            &repository,
        );

        match result {
            Err(LibrarianError::UnknownAction(action)) => {
                assert_eq!(action, "librarian.document.delete");
            }
            other => panic!("expected UnknownAction, got {other:?}"),
        }
        assert!(repository.put_calls.lock().unwrap().is_empty());
    }
}
