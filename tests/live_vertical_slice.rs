//! Goal: prove the full vertical slice against a real deployed
//! infernal-law kernel and a real Librarian PostgreSQL -- not fakes, not
//! an in-process shortcut. This is the same discipline infernal-law's own
//! live-Postgres tests use, extended across the kernel/domain boundary
//! this whole repository exists to prove.
//!
//! Requires, provisioned out of band exactly as this repository's README
//! documents (identities row, enrollment binding, communication
//! admission, ILK-002 grants for librarian.document.put/get/search and
//! subscription.create, and a real ADR-0008 enrollment challenge):
//!
//! - `KERNEL_AUTHORITY`, `LIBRARIAN_SERVICE_ID`, `LIBRARIAN_DATABASE_URL`
//! - `KERNEL_CA_CERT_PATH` if the kernel's TLS sidecar uses a private CA
//! - `ENROLLMENT_CHALLENGE`, `SERVICE_ENDPOINT`, `POD_UID`,
//!   `WORKLOAD_TOKEN_PATH` for Librarian's own enrollment
//! - `REQUESTER_SERVICE_ID`, `REQUESTER_ENROLLMENT_CHALLENGE`,
//!   `REQUESTER_SERVICE_ENDPOINT`, `REQUESTER_POD_UID`,
//!   `REQUESTER_WORKLOAD_TOKEN_PATH` for a *second*, separately enrolled
//!   identity to submit the Request as (Librarian must never submit its
//!   own governed work)
//! - `KERNEL_DATABASE_URL` (infernal-law's own database, read-only here)
//!   to confirm no Librarian-specific table exists there

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use infernal_client::{Client, ClientCredential, RequestParts, SignedRequest};
use infernal_librarian_simple::database::Database;
use infernal_librarian_simple::domain::DocumentRepository;
use infernal_librarian_simple::kernel_client::KernelClient;
use infernal_librarian_simple::postgres_document_repository::PostgresDocumentRepository;
use infernal_librarian_simple::{WorkOutcome, work_once};
use r2d2_postgres::postgres::{Client as PgClient, NoTls};
use uuid::Uuid;

fn unix_time() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn kernel_client_for(service_id_env: &str) -> KernelClient {
    let authority = env::var("KERNEL_AUTHORITY").unwrap();
    let service_id: Uuid = env::var(service_id_env).unwrap().parse().unwrap();
    let credential = ClientCredential::generate(service_id);
    match env::var("KERNEL_CA_CERT_PATH") {
        Ok(path) => {
            let pem = std::fs::read(&path).unwrap();
            KernelClient::with_extra_root_certificate(credential, authority, &pem).unwrap()
        }
        Err(_) => KernelClient::new(credential, authority).unwrap(),
    }
}

fn enroll(client: &KernelClient, prefix: &str) {
    let challenge_b64 = env::var(format!("{prefix}_ENROLLMENT_CHALLENGE")).unwrap();
    let endpoint = env::var(format!("{prefix}_SERVICE_ENDPOINT")).unwrap();
    let pod_uid = env::var(format!("{prefix}_POD_UID")).unwrap();
    let token_path = env::var(format!("{prefix}_WORKLOAD_TOKEN_PATH")).unwrap();
    let workload_token = std::fs::read_to_string(&token_path)
        .unwrap()
        .trim()
        .to_owned();
    use base64::Engine;
    let challenge: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&challenge_b64)
        .unwrap()
        .try_into()
        .unwrap();
    client
        .enroll(challenge, &endpoint, &pod_uid, workload_token)
        .unwrap();
}

#[test]
#[ignore = "requires a real deployed infernal-law kernel and Librarian PostgreSQL -- see this file's own module documentation for required env vars"]
fn a_real_signed_put_is_claimed_stored_and_survives_a_restart_with_no_kernel_data_leakage() {
    // Librarian enrolls under its own identity, exactly as it would at
    // real startup (src/lib.rs's Config::from_env), and creates its own
    // subscriptions.
    let librarian_client = kernel_client_for("LIBRARIAN_SERVICE_ID");
    enroll(&librarian_client, "LIBRARIAN");
    for action in infernal_librarian_simple::dispatch::ACTIONS {
        librarian_client.ensure_subscription(action).unwrap();
    }

    // A *separate* identity submits the Request -- Librarian never
    // submits its own governed work; that would blur the same
    // authenticated-service boundary this whole architecture depends on.
    let requester_client = kernel_client_for("REQUESTER_SERVICE_ID");
    enroll(&requester_client, "REQUESTER");
    let requester_credential =
        ClientCredential::generate(env::var("REQUESTER_SERVICE_ID").unwrap().parse().unwrap());
    let requester_authority = env::var("KERNEL_AUTHORITY").unwrap();
    let requester_http = match env::var("KERNEL_CA_CERT_PATH") {
        Ok(path) => Client::with_extra_root_certificate(&std::fs::read(&path).unwrap()).unwrap(),
        Err(_) => Client::new().unwrap(),
    };

    let unique = Uuid::new_v4().simple().to_string();
    let content = format!("live vertical slice content {unique}");
    let body = serde_json::json!({
        "action": "librarian.document.put",
        "scope": content,
        "artifact_schema_version_id": "00000000-0000-0000-0000-000000000001",
        "permission_policy_schema_version_id": "00000000-0000-0000-0000-000000000002",
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let request_id = Uuid::new_v4();
    let now = unix_time();
    let parts = RequestParts::new(
        "POST",
        &requester_authority,
        "/v1/requests",
        "application/json",
        &body_bytes,
        request_id,
    )
    .unwrap();
    let nonce = infernal_client::generate_nonce().unwrap();
    let signed = SignedRequest::sign(parts, &requester_credential, now, now + 30, &nonce).unwrap();
    let response = requester_http.send(&signed).unwrap();
    assert_eq!(response.status, 201, "request submission must be accepted");

    // Librarian sees the eligible route, claims it, reads it, stores it,
    // and completes the claim -- the real work_once loop, against the
    // real kernel and Librarian's real database.
    let database = Database::connect_from_env().unwrap();
    let repository = PostgresDocumentRepository::new(database);
    let outcome = work_once(&librarian_client, &repository, 300).unwrap();
    let WorkOutcome::Completed { outcome, .. } = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    let infernal_librarian_simple::dispatch::DispatchOutcome::Put { document_id, .. } = outcome
    else {
        panic!("expected a Put outcome, got {outcome:?}");
    };

    // Restart: a fresh Database/repository, exactly as a real process
    // restart would reconnect -- no in-memory state carries over.
    let restarted_database = Database::connect_from_env().unwrap();
    let restarted_repository = PostgresDocumentRepository::new(restarted_database);
    let stored = restarted_repository.get(document_id, None).unwrap();
    assert_eq!(stored.content, content);

    // Search finds it too, through Librarian's own index.
    let hits = restarted_repository.search(&unique, 10).unwrap();
    assert!(hits.iter().any(|hit| hit.document_id == document_id));

    // No Librarian-specific data has appeared in infernal-law's own
    // database -- checked directly against the kernel's own connection,
    // never through Librarian's code path.
    if let Ok(kernel_database_url) = env::var("KERNEL_DATABASE_URL") {
        let mut kernel_db = PgClient::connect(&kernel_database_url, NoTls).unwrap();
        let librarian_tables = kernel_db
            .query(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' \
                   AND (table_name LIKE '%document%' OR table_name = 'put_operations')",
                &[],
            )
            .unwrap();
        assert!(
            librarian_tables.is_empty(),
            "infernal-law's database must never contain Librarian-specific tables"
        );
    }
}
