//! Goal: prove Librarian's own domain -- documents, immutable versions,
//! search, and domain idempotency -- entirely without a live infernal-law
//! kernel. Nothing here signs a request or knows infernal-law exists.

use infernal_librarian_simple::database::Database;
use infernal_librarian_simple::domain::{DocumentError, DocumentRepository, PutDocument};
use infernal_librarian_simple::postgres_document_repository::PostgresDocumentRepository;
use uuid::Uuid;

fn repository() -> PostgresDocumentRepository {
    let database = Database::connect_from_env().expect("database should connect and migrate");
    PostgresDocumentRepository::new(database)
}

fn document(content: &str) -> PutDocument {
    PutDocument {
        document_id: None,
        content: content.to_owned(),
        content_type: "text/plain".to_owned(),
        title: Some("A title".to_owned()),
        source_uri: Some("https://example.test/source".to_owned()),
    }
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn put_creates_a_new_document_with_version_one() {
    let repository = repository();

    let outcome = repository
        .put(Uuid::new_v4(), document("hello world"))
        .unwrap();

    assert_eq!(outcome.version, 1);
    assert!(!outcome.was_already_processed);

    let stored = repository.get(outcome.document_id, None).unwrap();
    assert_eq!(stored.content, "hello world");
    assert_eq!(stored.content_type, "text/plain");
    assert_eq!(stored.title.as_deref(), Some("A title"));
    assert_eq!(
        stored.source_uri.as_deref(),
        Some("https://example.test/source")
    );
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn put_with_an_existing_document_id_creates_a_new_immutable_version() {
    let repository = repository();
    let first = repository
        .put(Uuid::new_v4(), document("version one"))
        .unwrap();

    let second = repository
        .put(
            Uuid::new_v4(),
            PutDocument {
                document_id: Some(first.document_id),
                ..document("version two")
            },
        )
        .unwrap();

    assert_eq!(second.document_id, first.document_id);
    assert_eq!(second.version, 2);

    // Both versions remain readable -- updating never overwrites history.
    let v1 = repository.get(first.document_id, Some(1)).unwrap();
    let v2 = repository.get(first.document_id, Some(2)).unwrap();
    assert_eq!(v1.content, "version one");
    assert_eq!(v2.content, "version two");

    // The latest version is returned by default.
    let latest = repository.get(first.document_id, None).unwrap();
    assert_eq!(latest.version, 2);
    assert_eq!(latest.content, "version two");
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn put_rejects_a_document_id_that_does_not_exist() {
    let repository = repository();

    let result = repository.put(
        Uuid::new_v4(),
        PutDocument {
            document_id: Some(Uuid::new_v4()),
            ..document("orphaned version")
        },
    );

    assert!(matches!(result, Err(DocumentError::UnknownDocument)));
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn get_rejects_an_unknown_document() {
    let repository = repository();

    assert!(matches!(
        repository.get(Uuid::new_v4(), None),
        Err(DocumentError::NotFound)
    ));
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn repeated_put_with_the_same_request_id_does_not_create_a_duplicate_document() {
    let repository = repository();
    let request_id = Uuid::new_v4();

    let first = repository
        .put(request_id, document("idempotent content"))
        .unwrap();
    assert!(!first.was_already_processed);

    // A second call with the *same* request_id -- exactly what happens
    // when Librarian reclaims a route after crashing between its own
    // domain commit and completing the kernel claim (README, "domain
    // commit succeeded, but kernel completion was not recorded") -- must
    // recognize the prior success rather than create a second document.
    let retried = repository
        .put(request_id, document("different content, same retry"))
        .unwrap();

    assert!(retried.was_already_processed);
    assert_eq!(retried.document_id, first.document_id);
    assert_eq!(retried.version, first.version);

    // Only the original content was ever persisted.
    let stored = repository.get(first.document_id, None).unwrap();
    assert_eq!(stored.content, "idempotent content");
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn search_finds_matching_documents_by_content_and_title() {
    let repository = repository();
    let unique = Uuid::new_v4().simple().to_string();
    let matching = repository
        .put(
            Uuid::new_v4(),
            PutDocument {
                document_id: None,
                content: format!("The lighthouse keeper {unique} watched the storm."),
                content_type: "text/plain".to_owned(),
                title: Some(format!("Lighthouse {unique}")),
                source_uri: None,
            },
        )
        .unwrap();
    repository
        .put(
            Uuid::new_v4(),
            PutDocument {
                document_id: None,
                content: format!("An unrelated recipe for bread {unique}."),
                content_type: "text/plain".to_owned(),
                title: None,
                source_uri: None,
            },
        )
        .unwrap();

    let hits = repository
        .search(&format!("lighthouse {unique}"), 10)
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document_id, matching.document_id);
    assert_eq!(hits[0].version, matching.version);
}

#[test]
#[ignore = "requires LIBRARIAN_DATABASE_URL and PostgreSQL"]
fn put_rejects_empty_content() {
    let repository = repository();

    assert!(matches!(
        repository.put(Uuid::new_v4(), document("   ")),
        Err(DocumentError::EmptyContent)
    ));
}
