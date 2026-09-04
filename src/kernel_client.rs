//! Goal: implement the outbound signed calls Librarian makes into the
//! kernel's subscription, route, and claim contracts (ADR-0011,
//! ILK-003/ILK-010/ILK-011), signing with this process's own long-lived
//! instance credential -- the same pattern proven first in
//! `infernal-worker-simple`'s `kernel_client.rs`, extended here with
//! subscription management (worker-simple relies on one being
//! provisioned out of band; Librarian creates its own at startup).
//!
//! Every call here uses Librarian's own verified identity as both
//! `worker_service` and `worker_instance` -- the kernel takes both from
//! the caller's signed request, never a body field, so there is no way
//! for one process to claim work on another's behalf. A route is only
//! ever eligible for, claimable by, and completable by the destination
//! service that is also the caller.

use std::time::{SystemTime, UNIX_EPOCH};

use infernal_client::{
    CHALLENGE_LENGTH, Client, ClientCredential, EnrolledInstance, EnrollmentSubmission,
    RequestParts, SignedRequest,
};
use uuid::Uuid;

use crate::claims::{
    ClaimOutcome, ClaimRequest, CompleteOutcome, FencedActionRequest, parse_claim_response,
    parse_complete_response,
};
use crate::error::LibrarianError;
use crate::instance_lease::{
    RENEW_INSTANCE_PATH, RenewedLease, parse_renewal_response, renewal_request_body,
};
use crate::routed_request::{RoutedRequestOutcome, parse_routed_request_response};
use crate::routes::{ELIGIBLE_ROUTES_PATH, EligibleRoute, parse_eligible_routes};
use crate::subscriptions::{
    ACTIVE_SUBSCRIPTIONS_PATH, CreateSubscriptionRequest, SUBSCRIPTIONS_PATH, Subscription,
    parse_create_subscription_response, parse_subscription_list,
};

const SIGNATURE_VALIDITY_SECONDS: i64 = 30;

/// The kernel operations Librarian needs -- an interface boundary so the
/// dispatch loop (`lib.rs`) can be proven against a fake, the same way
/// `infernal-worker-simple`'s `KernelPort` separates its loop from a
/// specific transport.
pub trait KernelPort {
    fn eligible_routes(&self) -> Result<Vec<EligibleRoute>, LibrarianError>;

    fn propose_claim(
        &self,
        route_id: &str,
        lease_seconds: i64,
    ) -> Result<ClaimOutcome, LibrarianError>;

    fn routed_request(&self, route_id: &str) -> Result<RoutedRequestOutcome, LibrarianError>;

    fn complete_claim(
        &self,
        claim_id: &str,
        fencing_token: i64,
    ) -> Result<CompleteOutcome, LibrarianError>;
}

pub struct KernelClient {
    client: Client,
    credential: ClientCredential,
    authority: String,
}

impl KernelClient {
    /// `authority` is the kernel's host (and, if needed, port), for
    /// example `infernal-law` -- the same shape as an HTTP `Host` header,
    /// never including a scheme or path.
    pub fn new(
        credential: ClientCredential,
        authority: impl Into<String>,
    ) -> Result<Self, LibrarianError> {
        Ok(Self {
            client: Client::new()?,
            credential,
            authority: authority.into(),
        })
    }

    /// Like [`KernelClient::new`], but additionally trusts
    /// `extra_root_certificate_pem` -- for a kernel reachable only behind
    /// a private or self-signed certificate authority.
    pub fn with_extra_root_certificate(
        credential: ClientCredential,
        authority: impl Into<String>,
        extra_root_certificate_pem: &[u8],
    ) -> Result<Self, LibrarianError> {
        Ok(Self {
            client: Client::with_extra_root_certificate(extra_root_certificate_pem)?,
            credential,
            authority: authority.into(),
        })
    }

    /// Performs ADR-0008 initial enrollment. `challenge` comes from a
    /// kernel operator's own out-of-band challenge issuance -- there is
    /// no self-service call for requesting one (see
    /// `infernal_client::EnrollmentSubmission`'s own documentation for
    /// why). Must be called with the very credential this `KernelClient`
    /// will go on to sign ordinary requests with.
    /// Asks the kernel to issue this workload its own enrollment challenge.
    /// Used when `ENROLLMENT_CHALLENGE` is unset, which is the normal case:
    /// a challenge is single-use, so an injected one survives only the
    /// first Pod of a Deployment revision.
    pub fn request_challenge(
        &self,
        pod_uid: &str,
        workload_token: &str,
    ) -> Result<[u8; CHALLENGE_LENGTH], LibrarianError> {
        let issued = self.client.request_enrollment_challenge(
            &format!("https://{}", self.authority),
            pod_uid,
            workload_token,
        )?;
        Ok(issued.challenge_bytes()?)
    }

    pub fn enroll(
        &self,
        challenge: [u8; CHALLENGE_LENGTH],
        endpoint: &str,
        pod_uid: &str,
        workload_token: String,
    ) -> Result<EnrolledInstance, LibrarianError> {
        let submission = EnrollmentSubmission::sign(
            &self.credential,
            challenge,
            endpoint,
            pod_uid,
            workload_token,
        )?;
        Ok(self
            .client
            .submit_enrollment(&format!("https://{}", self.authority), &submission)?)
    }

    /// Extends this instance's own registration lease before the kernel's
    /// default 60-second grant expires -- the kernel has no way to renew a
    /// lease that has *already* expired (every signed call, including
    /// this one, is rejected once that happens), so `lib.rs`'s `run` calls
    /// this proactively, well before the deadline. `expected_revision`
    /// must be this instance's current `lease_revision`, from the last
    /// enrollment or renewal -- a stale value is rejected rather than
    /// silently accepted, the same optimistic-concurrency guard the
    /// kernel's own work-claim leases use.
    pub fn renew_lease(&self, expected_revision: i64) -> Result<RenewedLease, LibrarianError> {
        let body = renewal_request_body(expected_revision);
        let signed = build_post(
            &self.credential,
            &self.authority,
            RENEW_INSTANCE_PATH,
            &body,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_renewal_response(response.status, &response.body)
    }

    /// Lists Librarian's own currently-active subscriptions.
    pub fn active_subscriptions(&self) -> Result<Vec<Subscription>, LibrarianError> {
        let signed = build_get(
            &self.credential,
            &self.authority,
            ACTIVE_SUBSCRIPTIONS_PATH,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_subscription_list(response.status, &response.body)
    }

    /// Creates an inclusive subscription for `event_type` under
    /// Librarian's own identity.
    pub fn create_subscription(&self, event_type: &str) -> Result<Subscription, LibrarianError> {
        let body = serde_json::to_vec(&CreateSubscriptionRequest {
            event_type: event_type.to_owned(),
        })
        .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
        let signed = build_post(
            &self.credential,
            &self.authority,
            SUBSCRIPTIONS_PATH,
            &body,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_create_subscription_response(response.status, &response.body)
    }

    /// Idempotently ensures Librarian has an active inclusive subscription
    /// for `event_type`: a restarted process must not fail, or create a
    /// second subscription, just because one already exists from before.
    pub fn ensure_subscription(&self, event_type: &str) -> Result<(), LibrarianError> {
        let already_active = self
            .active_subscriptions()?
            .iter()
            .any(|subscription| subscription.event_type == event_type);
        if already_active {
            return Ok(());
        }
        self.create_subscription(event_type)?;
        Ok(())
    }
}

impl KernelPort for KernelClient {
    fn eligible_routes(&self) -> Result<Vec<EligibleRoute>, LibrarianError> {
        let signed = build_get(
            &self.credential,
            &self.authority,
            ELIGIBLE_ROUTES_PATH,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_eligible_routes(response.status, &response.body)
    }

    fn propose_claim(
        &self,
        route_id: &str,
        lease_seconds: i64,
    ) -> Result<ClaimOutcome, LibrarianError> {
        let path = format!("/v1/routes/{route_id}/claims");
        let body = serde_json::to_vec(&ClaimRequest { lease_seconds })
            .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
        let signed = build_post(
            &self.credential,
            &self.authority,
            &path,
            &body,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_claim_response(response.status, &response.body)
    }

    fn routed_request(&self, route_id: &str) -> Result<RoutedRequestOutcome, LibrarianError> {
        let path = format!("/v1/routes/{route_id}/request");
        let signed = build_get(
            &self.credential,
            &self.authority,
            &path,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_routed_request_response(response.status, &response.body)
    }

    fn complete_claim(
        &self,
        claim_id: &str,
        fencing_token: i64,
    ) -> Result<CompleteOutcome, LibrarianError> {
        let path = format!("/v1/claims/{claim_id}/complete");
        let body = serde_json::to_vec(&FencedActionRequest { fencing_token })
            .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
        let signed = build_post(
            &self.credential,
            &self.authority,
            &path,
            &body,
            Uuid::new_v4(),
            unix_time(),
        )?;
        let response = self.client.send(&signed)?;
        parse_complete_response(response.status, &response.body)
    }
}

fn build_get(
    credential: &ClientCredential,
    authority: &str,
    path: &str,
    request_id: Uuid,
    now: i64,
) -> Result<SignedRequest, LibrarianError> {
    let parts = RequestParts::new("GET", authority, path, "application/json", &[], request_id)?;
    sign(credential, parts, now)
}

fn build_post(
    credential: &ClientCredential,
    authority: &str,
    path: &str,
    body: &[u8],
    request_id: Uuid,
    now: i64,
) -> Result<SignedRequest, LibrarianError> {
    let parts = RequestParts::new(
        "POST",
        authority,
        path,
        "application/json",
        body,
        request_id,
    )?;
    sign(credential, parts, now)
}

fn sign(
    credential: &ClientCredential,
    parts: RequestParts,
    now: i64,
) -> Result<SignedRequest, LibrarianError> {
    let nonce = infernal_client::generate_nonce()?;
    Ok(SignedRequest::sign(
        parts,
        credential,
        now,
        now + SIGNATURE_VALIDITY_SECONDS,
        &nonce,
    )?)
}

fn unix_time() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use infernal_client::{IncomingRequest, verify_incoming};

    use super::*;

    fn incoming_from(signed: &SignedRequest) -> IncomingRequest {
        IncomingRequest::from_wire(
            signed.parts().clone(),
            &signed.service_id().to_string(),
            &signed.instance_id().to_string(),
            signed.content_digest(),
            signed.signature_input(),
            signed.signature(),
        )
        .unwrap()
    }

    #[test]
    fn the_eligible_routes_request_verifies_under_its_own_public_key() {
        let credential = ClientCredential::generate(Uuid::new_v4());

        let signed = build_get(
            &credential,
            "kernel.example.test",
            ELIGIBLE_ROUTES_PATH,
            Uuid::new_v4(),
            1_000,
        )
        .unwrap();

        assert_eq!(signed.parts().method(), "GET");
        assert_eq!(signed.parts().path_and_query(), ELIGIBLE_ROUTES_PATH);
        let verified =
            verify_incoming(&incoming_from(&signed), credential.public_key(), 1_005).unwrap();
        assert_eq!(verified.service_id(), credential.public_key().service_id());
    }

    #[test]
    fn the_renewal_request_targets_the_instances_path_and_carries_the_expected_revision() {
        let credential = ClientCredential::generate(Uuid::new_v4());
        let body = renewal_request_body(3);

        let signed = build_post(
            &credential,
            "kernel.example.test",
            RENEW_INSTANCE_PATH,
            &body,
            Uuid::new_v4(),
            1_000,
        )
        .unwrap();

        assert_eq!(signed.parts().method(), "POST");
        assert_eq!(signed.parts().path_and_query(), RENEW_INSTANCE_PATH);
        assert_eq!(signed.parts().body(), br#"{"expected_revision":3}"#);
        verify_incoming(&incoming_from(&signed), credential.public_key(), 1_005).unwrap();
    }

    #[test]
    fn the_claim_request_targets_the_right_route_and_carries_the_lease() {
        let credential = ClientCredential::generate(Uuid::new_v4());
        let body = serde_json::to_vec(&ClaimRequest { lease_seconds: 300 }).unwrap();

        let signed = build_post(
            &credential,
            "kernel.example.test",
            "/v1/routes/route-42/claims",
            &body,
            Uuid::new_v4(),
            1_000,
        )
        .unwrap();

        assert_eq!(signed.parts().method(), "POST");
        assert_eq!(
            signed.parts().path_and_query(),
            "/v1/routes/route-42/claims"
        );
        assert_eq!(signed.parts().body(), br#"{"lease_seconds":300}"#);
        verify_incoming(&incoming_from(&signed), credential.public_key(), 1_005).unwrap();
    }

    #[test]
    fn the_subscription_create_request_carries_the_event_type() {
        let credential = ClientCredential::generate(Uuid::new_v4());
        let body = serde_json::to_vec(&CreateSubscriptionRequest {
            event_type: "librarian.document.put".to_owned(),
        })
        .unwrap();

        let signed = build_post(
            &credential,
            "kernel.example.test",
            SUBSCRIPTIONS_PATH,
            &body,
            Uuid::new_v4(),
            1_000,
        )
        .unwrap();

        assert_eq!(
            signed.parts().body(),
            br#"{"event_type":"librarian.document.put"}"#
        );
        verify_incoming(&incoming_from(&signed), credential.public_key(), 1_005).unwrap();
    }

    #[test]
    fn the_complete_request_targets_the_right_claim_and_carries_the_fencing_token() {
        let credential = ClientCredential::generate(Uuid::new_v4());
        let body = serde_json::to_vec(&FencedActionRequest { fencing_token: 7 }).unwrap();

        let signed = build_post(
            &credential,
            "kernel.example.test",
            "/v1/claims/claim-9/complete",
            &body,
            Uuid::new_v4(),
            1_000,
        )
        .unwrap();

        assert_eq!(
            signed.parts().path_and_query(),
            "/v1/claims/claim-9/complete"
        );
        assert_eq!(signed.parts().body(), br#"{"fencing_token":7}"#);
        verify_incoming(&incoming_from(&signed), credential.public_key(), 1_005).unwrap();
    }
}
