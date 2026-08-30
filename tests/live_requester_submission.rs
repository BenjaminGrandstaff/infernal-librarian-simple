//! Goal: submit real signed governed Requests as a distinct Requester
//! identity against a live deployed infernal-law kernel, and let a real,
//! independently running `infernal-librarian-simple` Deployment (already
//! enrolled and polling on its own) claim and process them -- proving the
//! vertical slice through the kernel's own routing rather than by calling
//! `work_once` in-process the way `tests/live_vertical_slice.rs` does.
//!
//! Requires a real deployed kernel plus a real running Librarian
//! Deployment, provisioned exactly as this repository's README documents:
//! - `KERNEL_AUTHORITY`, `KERNEL_CA_CERT_PATH` if the kernel's TLS sidecar
//!   uses a private CA
//! - `REQUESTER_SERVICE_ID`, `REQUESTER_ENROLLMENT_CHALLENGE`,
//!   `REQUESTER_SERVICE_ENDPOINT`, `REQUESTER_POD_UID`,
//!   `REQUESTER_WORKLOAD_TOKEN_PATH` for a Requester identity, separate
//!   from Librarian's own, with grants for
//!   librarian.document.{put,get,search}
//! - `LIBRARIAN_DATABASE_URL` reachable from this test process, purely to
//!   observe the result Librarian already committed on its own -- this
//!   test never writes to it directly, matching the same boundary
//!   `tests/live_vertical_slice.rs`'s restart check relies on.

use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use infernal_client::{
    Client, ClientCredential, EnrollmentSubmission, RequestParts, SignedRequest,
};
use infernal_librarian_simple::database::Database;
use infernal_librarian_simple::domain::DocumentRepository;
use infernal_librarian_simple::postgres_document_repository::PostgresDocumentRepository;
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

fn submit(
    http: &Client,
    authority: &str,
    credential: &ClientCredential,
    action: &str,
    scope: &str,
) {
    let body = serde_json::json!({
        "action": action,
        "scope": scope,
        "artifact_schema_version_id": "00000000-0000-0000-0000-000000000001",
        "permission_policy_schema_version_id": "00000000-0000-0000-0000-000000000002",
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let request_id = Uuid::new_v4();
    let now = unix_time();
    let parts = RequestParts::new(
        "POST",
        authority,
        "/v1/requests",
        "application/json",
        &body_bytes,
        request_id,
    )
    .unwrap();
    let nonce = infernal_client::generate_nonce().unwrap();
    let signed = SignedRequest::sign(parts, credential, now, now + 30, &nonce).unwrap();
    let response = http.send(&signed).unwrap();
    assert_eq!(
        response.status,
        201,
        "{action} submission must be accepted: {:?}",
        String::from_utf8_lossy(&response.body)
    );
    println!("submitted {action} scope={scope:?} request_id={request_id}");
}

#[test]
#[ignore = "requires a real deployed infernal-law kernel and a real running infernal-librarian-simple Deployment -- see this file's own module documentation"]
fn a_real_requester_drives_put_get_and_search_through_a_live_librarian_deployment() {
    let authority = env::var("KERNEL_AUTHORITY").unwrap();
    let service_id: Uuid = env::var("REQUESTER_SERVICE_ID").unwrap().parse().unwrap();
    let credential = ClientCredential::generate(service_id);
    let http = match env::var("KERNEL_CA_CERT_PATH") {
        Ok(path) => Client::with_extra_root_certificate(&std::fs::read(&path).unwrap()).unwrap(),
        Err(_) => Client::new().unwrap(),
    };

    // Enroll this Requester instance before signing anything else -- a
    // freshly generated credential has no registered public key with the
    // kernel yet, so every ADR-0003 signed call would otherwise fail
    // authentication.
    let challenge_b64 = env::var("REQUESTER_ENROLLMENT_CHALLENGE").unwrap();
    let endpoint = env::var("REQUESTER_SERVICE_ENDPOINT").unwrap();
    let pod_uid = env::var("REQUESTER_POD_UID").unwrap();
    let token_path = env::var("REQUESTER_WORKLOAD_TOKEN_PATH").unwrap();
    let workload_token = std::fs::read_to_string(&token_path)
        .unwrap()
        .trim()
        .to_owned();
    let challenge: [u8; 32] = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&challenge_b64)
        .unwrap()
        .try_into()
        .unwrap();
    let submission =
        EnrollmentSubmission::sign(&credential, challenge, &endpoint, &pod_uid, workload_token)
            .unwrap();
    let enrolled = http
        .submit_enrollment(&format!("https://{authority}"), &submission)
        .unwrap();
    println!("requester enrolled: {enrolled:?}");

    let unique = Uuid::new_v4().simple().to_string();
    let content = format!("live requester submission content {unique}");

    submit(
        &http,
        &authority,
        &credential,
        "librarian.document.put",
        &content,
    );

    let database = Database::connect_from_env().unwrap();
    let repository = PostgresDocumentRepository::new(database);
    let document_id = (0..30)
        .find_map(|_| {
            std::thread::sleep(Duration::from_secs(1));
            let hits = repository.search(&unique, 10).ok()?;
            hits.into_iter().next().map(|hit| hit.document_id)
        })
        .expect("Librarian's real Deployment should have claimed, stored, and indexed the put within 30s");

    let stored = repository.get(document_id, None).unwrap();
    assert_eq!(stored.content, content);

    submit(
        &http,
        &authority,
        &credential,
        "librarian.document.get",
        &document_id.to_string(),
    );
    submit(
        &http,
        &authority,
        &credential,
        "librarian.document.search",
        &unique,
    );
}
