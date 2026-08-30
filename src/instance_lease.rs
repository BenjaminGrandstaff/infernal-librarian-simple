//! Goal: mirror the kernel's instance lease renewal wire format
//! (`src/http/instance_renewal_dto.rs` in infernal-law). An enrolled
//! instance's lease is short (60 seconds by kernel default) and the
//! kernel exposes no other way to extend it, so a process that intends to
//! keep polling past that must renew proactively, well before expiry --
//! see `lib.rs`'s `run` for how this is scheduled.

use serde::{Deserialize, Serialize};

use crate::error::LibrarianError;

pub const RENEW_INSTANCE_PATH: &str = "/v1/instances/renew";

/// How many seconds of margin to renew before the lease the kernel granted
/// actually expires. The kernel's own default lease is 60 seconds; renewing
/// this early leaves several poll cycles' worth of retry room if a renewal
/// call transiently fails, without renewing so aggressively that it adds
/// meaningful request volume.
pub const RENEWAL_MARGIN_SECONDS: i64 = 20;

#[derive(Serialize)]
struct RenewInstanceRequest {
    expected_revision: i64,
}

pub fn renewal_request_body(expected_revision: i64) -> Vec<u8> {
    serde_json::to_vec(&RenewInstanceRequest { expected_revision })
        .expect("a renewal request body always serializes")
}

/// Same wire shape as a fresh enrollment's success response -- see
/// `infernal_client::EnrolledInstance`'s own documentation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RenewedLease {
    pub lease_expires_at: i64,
    pub lease_revision: i64,
}

pub fn parse_renewal_response(status: u16, body: &[u8]) -> Result<RenewedLease, LibrarianError> {
    if status != 200 {
        return Err(LibrarianError::UnexpectedStatus(status));
    }
    serde_json::from_slice(body)
        .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_request_carries_the_expected_revision() {
        assert_eq!(renewal_request_body(3), br#"{"expected_revision":3}"#);
    }

    #[test]
    fn parses_the_kernels_actual_renewal_success_shape() {
        let body = br#"{"service_id":"s1","instance_id":"i1","key_id":"k1","algorithm":"ed25519","public_key":"pk","endpoint":"https://x","registered_at":10,"lease_expires_at":70,"lease_revision":2}"#;

        let renewed = parse_renewal_response(200, body).unwrap();

        assert_eq!(
            renewed,
            RenewedLease {
                lease_expires_at: 70,
                lease_revision: 2,
            }
        );
    }

    #[test]
    fn a_conflicting_revision_is_reported_as_an_unexpected_status() {
        assert!(matches!(
            parse_renewal_response(409, b"{}"),
            Err(LibrarianError::UnexpectedStatus(409))
        ));
    }
}
