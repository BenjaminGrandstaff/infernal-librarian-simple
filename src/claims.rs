//! Goal: mirror the kernel's work-claim wire format
//! (`src/http/work_claim_dto.rs` in infernal-law) -- proven first in
//! `infernal-worker-simple`, reused here unchanged.

use serde::{Deserialize, Serialize};

use crate::error::LibrarianError;

#[derive(Serialize)]
pub struct ClaimRequest {
    pub lease_seconds: i64,
}

#[derive(Serialize)]
pub struct FencedActionRequest {
    pub fencing_token: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct WorkClaim {
    pub claim_id: String,
    pub route_id: String,
    pub worker_service_id: String,
    pub worker_instance_id: String,
    pub fencing_token: i64,
    pub status: String,
    pub claimed_at: i64,
    pub lease_expires_at: i64,
}

/// Every outcome the kernel's atomic claim arbitration (ILK-011) can
/// produce for a claim attempt. `AlreadyClaimed` and `RouteNotFound` are
/// not failures of this service -- they are the kernel correctly
/// rejecting an attempt that lost a race or targeted a route this service
/// does not own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimOutcome {
    Claimed(WorkClaim),
    AlreadyClaimed,
    RouteNotFound,
}

pub fn parse_claim_response(status: u16, body: &[u8]) -> Result<ClaimOutcome, LibrarianError> {
    match status {
        201 => {
            let claim: WorkClaim = serde_json::from_slice(body)
                .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
            Ok(ClaimOutcome::Claimed(claim))
        }
        409 => Ok(ClaimOutcome::AlreadyClaimed),
        404 => Ok(ClaimOutcome::RouteNotFound),
        other => Err(LibrarianError::UnexpectedStatus(other)),
    }
}

/// Every outcome `POST /v1/claims/{id}/complete` can produce. `Fenced`
/// means this service's fencing token is no longer current, and
/// `NotFound` means the claim ID itself is unknown; neither is a failure
/// to recover from -- a stale holder losing its claim is exactly what
/// fencing exists to make safe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompleteOutcome {
    Completed(WorkClaim),
    Fenced,
    NotFound,
}

pub fn parse_complete_response(
    status: u16,
    body: &[u8],
) -> Result<CompleteOutcome, LibrarianError> {
    match status {
        200 => {
            let claim: WorkClaim = serde_json::from_slice(body)
                .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
            Ok(CompleteOutcome::Completed(claim))
        }
        409 => Ok(CompleteOutcome::Fenced),
        404 => Ok(CompleteOutcome::NotFound),
        other => Err(LibrarianError::UnexpectedStatus(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> WorkClaim {
        WorkClaim {
            claim_id: "c1".to_owned(),
            route_id: "r1".to_owned(),
            worker_service_id: "w1".to_owned(),
            worker_instance_id: "i1".to_owned(),
            fencing_token: 1,
            status: "active".to_owned(),
            claimed_at: 10,
            lease_expires_at: 310,
        }
    }

    fn claim_body() -> &'static [u8] {
        br#"{"claim_id":"c1","route_id":"r1","worker_service_id":"w1","worker_instance_id":"i1","fencing_token":1,"status":"active","claimed_at":10,"lease_expires_at":310}"#
    }

    #[test]
    fn parses_a_successful_claim() {
        assert_eq!(
            parse_claim_response(201, claim_body()).unwrap(),
            ClaimOutcome::Claimed(claim())
        );
    }

    #[test]
    fn classifies_a_lost_race_as_already_claimed_not_an_error() {
        assert_eq!(
            parse_claim_response(409, br#"{"code":"route_already_claimed"}"#).unwrap(),
            ClaimOutcome::AlreadyClaimed
        );
    }

    #[test]
    fn classifies_an_unowned_or_unknown_route_as_route_not_found() {
        assert_eq!(
            parse_claim_response(404, br#"{"code":"claim_not_found"}"#).unwrap(),
            ClaimOutcome::RouteNotFound
        );
    }

    #[test]
    fn claim_surfaces_any_other_status_as_an_error() {
        assert!(matches!(
            parse_claim_response(503, b"{}"),
            Err(LibrarianError::UnexpectedStatus(503))
        ));
    }

    #[test]
    fn parses_a_successful_completion() {
        let body = br#"{"claim_id":"c1","route_id":"r1","worker_service_id":"w1","worker_instance_id":"i1","fencing_token":1,"status":"completed","claimed_at":10,"lease_expires_at":310}"#;

        let outcome = parse_complete_response(200, body).unwrap();

        assert_eq!(
            outcome,
            CompleteOutcome::Completed(WorkClaim {
                status: "completed".to_owned(),
                ..claim()
            })
        );
    }

    #[test]
    fn classifies_a_stale_fencing_token_as_fenced_not_an_error() {
        assert_eq!(
            parse_complete_response(409, br#"{"code":"claim_fenced"}"#).unwrap(),
            CompleteOutcome::Fenced
        );
    }

    #[test]
    fn classifies_an_unknown_claim_as_not_found() {
        assert_eq!(
            parse_complete_response(404, br#"{"code":"claim_not_found"}"#).unwrap(),
            CompleteOutcome::NotFound
        );
    }

    #[test]
    fn complete_surfaces_any_other_status_as_an_error() {
        assert!(matches!(
            parse_complete_response(503, b"{}"),
            Err(LibrarianError::UnexpectedStatus(503))
        ));
    }
}
