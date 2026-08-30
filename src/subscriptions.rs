//! Goal: mirror the kernel's ILK-010 subscription wire format
//! (`src/http/subscription_dto.rs` in infernal-law). Unlike
//! `infernal-worker-simple` and `infernal-taskmaster-simple`, Librarian
//! creates its own subscriptions at startup rather than relying on one
//! being provisioned out of band -- see `lib.rs`'s
//! `ensure_subscriptions`.

use serde::{Deserialize, Serialize};

use crate::error::LibrarianError;

pub const SUBSCRIPTIONS_PATH: &str = "/v1/subscriptions";
pub const ACTIVE_SUBSCRIPTIONS_PATH: &str = "/v1/subscriptions?active=true";

#[derive(Serialize)]
pub struct CreateSubscriptionRequest {
    pub event_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub service_id: String,
    pub event_type: String,
    pub created_at: i64,
    pub disabled_at: Option<i64>,
    pub active: bool,
}

#[derive(Deserialize)]
struct SubscriptionListResponse {
    subscriptions: Vec<Subscription>,
}

pub fn parse_subscription_list(
    status: u16,
    body: &[u8],
) -> Result<Vec<Subscription>, LibrarianError> {
    if status != 200 {
        return Err(LibrarianError::UnexpectedStatus(status));
    }
    let parsed: SubscriptionListResponse = serde_json::from_slice(body)
        .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))?;
    Ok(parsed.subscriptions)
}

/// `201` is a freshly created subscription; `200` never happens for
/// create, but a caller re-running this at startup after a restart
/// should treat "already have an active one" (checked separately via
/// `parse_subscription_list`) as success too, not just a fresh `201`.
pub fn parse_create_subscription_response(
    status: u16,
    body: &[u8],
) -> Result<Subscription, LibrarianError> {
    if status != 201 {
        return Err(LibrarianError::UnexpectedStatus(status));
    }
    serde_json::from_slice(body)
        .map_err(|error| LibrarianError::MalformedResponse(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_kernels_actual_subscription_list_shape() {
        let body = br#"{"subscriptions":[{"id":"s1","service_id":"d1","event_type":"librarian.document.put","created_at":10,"disabled_at":null,"active":true}]}"#;

        let subscriptions = parse_subscription_list(200, body).unwrap();

        assert_eq!(
            subscriptions,
            vec![Subscription {
                id: "s1".to_owned(),
                service_id: "d1".to_owned(),
                event_type: "librarian.document.put".to_owned(),
                created_at: 10,
                disabled_at: None,
                active: true,
            }]
        );
    }

    #[test]
    fn parses_a_freshly_created_subscription() {
        let body = br#"{"id":"s1","service_id":"d1","event_type":"librarian.document.put","created_at":10,"disabled_at":null,"active":true}"#;

        let subscription = parse_create_subscription_response(201, body).unwrap();

        assert_eq!(subscription.event_type, "librarian.document.put");
        assert!(subscription.active);
    }

    #[test]
    fn create_rejects_any_status_other_than_201() {
        assert!(matches!(
            parse_create_subscription_response(503, b"{}"),
            Err(LibrarianError::UnexpectedStatus(503))
        ));
    }
}
