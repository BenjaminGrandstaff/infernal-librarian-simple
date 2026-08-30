//! Goal: mirror the kernel's `GET /v1/routes/eligible` wire format
//! (`src/http/route_dto.rs` in infernal-law) field-for-field -- proven
//! first in `infernal-worker-simple`, reused here unchanged.

use serde::Deserialize;

use crate::error::LibrarianError;

pub const ELIGIBLE_ROUTES_PATH: &str = "/v1/routes/eligible";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct EligibleRoute {
    pub route_id: String,
    pub request_id: String,
    pub subscription_id: String,
    pub destination_service_id: String,
    pub created_at: i64,
}

#[derive(Deserialize)]
struct EligibleRouteListResponse {
    routes: Vec<EligibleRoute>,
}

/// Parses `GET /v1/routes/eligible`'s response body. The kernel already
/// returns routes ordered by `(created_at, route_id)`, so the first
/// element is the oldest eligible route.
pub fn parse_eligible_routes(
    status: u16,
    body: &[u8],
) -> Result<Vec<EligibleRoute>, LibrarianError> {
    if status != 200 {
        return Err(LibrarianError::UnexpectedStatus(status));
    }
    let parsed: EligibleRouteListResponse = serde_json::from_slice(body)
        .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
    Ok(parsed.routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_kernels_actual_eligible_route_wire_shape() {
        let body = br#"{"routes":[{"route_id":"r1","request_id":"q1","subscription_id":"s1","destination_service_id":"d1","created_at":100}]}"#;

        let routes = parse_eligible_routes(200, body).unwrap();

        assert_eq!(
            routes,
            vec![EligibleRoute {
                route_id: "r1".to_owned(),
                request_id: "q1".to_owned(),
                subscription_id: "s1".to_owned(),
                destination_service_id: "d1".to_owned(),
                created_at: 100,
            }]
        );
    }

    #[test]
    fn parses_an_empty_eligible_set() {
        let routes = parse_eligible_routes(200, br#"{"routes":[]}"#).unwrap();

        assert!(routes.is_empty());
    }

    #[test]
    fn rejects_a_non_200_status() {
        assert!(matches!(
            parse_eligible_routes(503, b"{}"),
            Err(LibrarianError::UnexpectedStatus(503))
        ));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse_eligible_routes(200, b"not json"),
            Err(LibrarianError::MalformedResponse(_))
        ));
    }
}
