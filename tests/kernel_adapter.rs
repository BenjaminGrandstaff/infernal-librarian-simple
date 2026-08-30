//! Goal: prove the mapping between a routed governed Request and a
//! Librarian domain command -- `work_once`'s orchestration of
//! `KernelPort` and `DocumentRepository` -- entirely against fakes. No
//! live kernel, no live database.
//!
//! The one failure-semantics property this file exists specifically to
//! prove: a Librarian database failure must never let the kernel claim
//! be completed. See infernal-librarian-simple's README, "domain commit
//! succeeded, but kernel completion was not recorded" -- the sequencing
//! this test locks in is what makes that safe.

use std::sync::Mutex;

use infernal_librarian_simple::claims::{ClaimOutcome, CompleteOutcome, WorkClaim};
use infernal_librarian_simple::domain::{
    DocumentError, DocumentRepository, DocumentVersion, PutDocument, PutOutcome, SearchHit,
};
use infernal_librarian_simple::error::LibrarianError;
use infernal_librarian_simple::kernel_client::KernelPort;
use infernal_librarian_simple::routed_request::RoutedRequestOutcome;
use infernal_librarian_simple::routes::EligibleRoute;
use infernal_librarian_simple::{WorkOutcome, work_once};
use uuid::Uuid;

#[derive(Default)]
struct FakePort {
    routes: Vec<EligibleRoute>,
    claim_outcome: Option<ClaimOutcome>,
    request_outcome: Option<RoutedRequestOutcome>,
    complete_outcome: Option<CompleteOutcome>,
    complete_calls: Mutex<u32>,
}

impl KernelPort for FakePort {
    fn eligible_routes(&self) -> Result<Vec<EligibleRoute>, LibrarianError> {
        Ok(self.routes.clone())
    }

    fn propose_claim(
        &self,
        _route_id: &str,
        _lease_seconds: i64,
    ) -> Result<ClaimOutcome, LibrarianError> {
        Ok(self
            .claim_outcome
            .clone()
            .unwrap_or(ClaimOutcome::AlreadyClaimed))
    }

    fn routed_request(&self, _route_id: &str) -> Result<RoutedRequestOutcome, LibrarianError> {
        Ok(self
            .request_outcome
            .clone()
            .unwrap_or(RoutedRequestOutcome::NotFound))
    }

    fn complete_claim(
        &self,
        _claim_id: &str,
        _fencing_token: i64,
    ) -> Result<CompleteOutcome, LibrarianError> {
        *self.complete_calls.lock().unwrap() += 1;
        Ok(self
            .complete_outcome
            .clone()
            .unwrap_or(CompleteOutcome::NotFound))
    }
}

#[derive(Default)]
struct FakeRepository {
    put_result: Option<Result<PutOutcome, DocumentError>>,
    get_result: Option<DocumentVersion>,
}

impl DocumentRepository for FakeRepository {
    fn put(&self, _request_id: Uuid, _document: PutDocument) -> Result<PutOutcome, DocumentError> {
        match &self.put_result {
            Some(Ok(outcome)) => Ok(PutOutcome {
                document_id: outcome.document_id,
                version: outcome.version,
                was_already_processed: outcome.was_already_processed,
            }),
            Some(Err(error)) => Err(*error),
            None => Ok(PutOutcome {
                document_id: Uuid::new_v4(),
                version: 1,
                was_already_processed: false,
            }),
        }
    }

    fn get(
        &self,
        _document_id: Uuid,
        _version: Option<i64>,
    ) -> Result<DocumentVersion, DocumentError> {
        self.get_result.clone().ok_or(DocumentError::NotFound)
    }

    fn search(&self, _query: &str, _limit: i64) -> Result<Vec<SearchHit>, DocumentError> {
        Ok(Vec::new())
    }
}

fn route() -> EligibleRoute {
    EligibleRoute {
        route_id: "route-1".to_owned(),
        request_id: Uuid::new_v4().to_string(),
        subscription_id: "subscription-1".to_owned(),
        destination_service_id: "destination-1".to_owned(),
        created_at: 1,
    }
}

fn claim() -> WorkClaim {
    WorkClaim {
        claim_id: "claim-1".to_owned(),
        route_id: "route-1".to_owned(),
        worker_service_id: "destination-1".to_owned(),
        worker_instance_id: "instance-1".to_owned(),
        fencing_token: 1,
        status: "active".to_owned(),
        claimed_at: 1,
        lease_expires_at: 301,
    }
}

fn routed_request(route: &EligibleRoute, action: &str, scope: &str) -> RoutedRequestOutcome {
    RoutedRequestOutcome::Found(infernal_librarian_simple::routed_request::RoutedRequest {
        request_id: route.request_id.clone(),
        source_service_id: "source-1".to_owned(),
        action: action.to_owned(),
        scope: scope.to_owned(),
        artifact_schema_version_id: "a1".to_owned(),
        permission_policy_schema_version_id: "p1".to_owned(),
        accepted_at: 1,
    })
}

#[test]
fn does_nothing_when_no_route_is_eligible() {
    let port = FakePort::default();
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(matches!(outcome, WorkOutcome::NothingEligible));
}

#[test]
fn reports_a_lost_claim_race_without_erroring() {
    let port = FakePort {
        routes: vec![route()],
        claim_outcome: Some(ClaimOutcome::AlreadyClaimed),
        ..FakePort::default()
    };
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(matches!(outcome, WorkOutcome::ClaimLost { route_id } if route_id == "route-1"));
}

#[test]
fn completes_a_full_put_dispatch() {
    let route = route();
    let port = FakePort {
        routes: vec![route.clone()],
        claim_outcome: Some(ClaimOutcome::Claimed(claim())),
        request_outcome: Some(routed_request(
            &route,
            "librarian.document.put",
            "hello world",
        )),
        complete_outcome: Some(CompleteOutcome::Completed(WorkClaim {
            status: "completed".to_owned(),
            ..claim()
        })),
        ..FakePort::default()
    };
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(matches!(outcome, WorkOutcome::Completed { .. }));
    assert_eq!(*port.complete_calls.lock().unwrap(), 1);
}

#[test]
fn completes_but_performs_no_mutation_for_an_unknown_action() {
    let route = route();
    let port = FakePort {
        routes: vec![route.clone()],
        claim_outcome: Some(ClaimOutcome::Claimed(claim())),
        request_outcome: Some(routed_request(&route, "librarian.document.delete", "x")),
        complete_outcome: Some(CompleteOutcome::Completed(claim())),
        ..FakePort::default()
    };
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(
        matches!(outcome, WorkOutcome::UnknownAction { action, .. } if action == "librarian.document.delete")
    );
    assert_eq!(*port.complete_calls.lock().unwrap(), 1);
}

#[test]
fn a_domain_repository_failure_never_completes_the_kernel_claim() {
    // This is the property this test file exists to lock in: if
    // Librarian's own database write fails, work_once must fail the pass
    // outright rather than call complete_claim -- reporting success for a
    // domain mutation that never happened would be exactly the corrupted
    // state the README's "domain commit succeeded, but kernel completion
    // was not recorded" section is about avoiding in the other direction.
    let route = route();
    let port = FakePort {
        routes: vec![route.clone()],
        claim_outcome: Some(ClaimOutcome::Claimed(claim())),
        request_outcome: Some(routed_request(
            &route,
            "librarian.document.put",
            "hello world",
        )),
        complete_outcome: Some(CompleteOutcome::Completed(claim())),
        ..FakePort::default()
    };
    let repository = FakeRepository {
        put_result: Some(Err(DocumentError::Repository)),
        ..FakeRepository::default()
    };

    let result = work_once(&port, &repository, 300);

    assert!(matches!(
        result,
        Err(LibrarianError::Document(DocumentError::Repository))
    ));
    assert_eq!(
        *port.complete_calls.lock().unwrap(),
        0,
        "a failed domain write must never be followed by a kernel completion call"
    );
}

#[test]
fn reports_fencing_loss_before_completion_without_erroring() {
    let route = route();
    let port = FakePort {
        routes: vec![route.clone()],
        claim_outcome: Some(ClaimOutcome::Claimed(claim())),
        request_outcome: Some(routed_request(
            &route,
            "librarian.document.put",
            "hello world",
        )),
        complete_outcome: Some(CompleteOutcome::Fenced),
        ..FakePort::default()
    };
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(matches!(
        outcome,
        WorkOutcome::LostBeforeCompletion { route_id, claim_id }
            if route_id == "route-1" && claim_id == "claim-1"
    ));
}

#[test]
fn reports_an_unavailable_request_without_touching_the_repository() {
    let port = FakePort {
        routes: vec![route()],
        claim_outcome: Some(ClaimOutcome::Claimed(claim())),
        request_outcome: Some(RoutedRequestOutcome::NotFound),
        ..FakePort::default()
    };
    let repository = FakeRepository::default();

    let outcome = work_once(&port, &repository, 300).unwrap();

    assert!(
        matches!(outcome, WorkOutcome::RequestUnavailable { route_id } if route_id == "route-1")
    );
}
