//! Reference domain service proving a useful domain can run on top of the
//! infernal-law kernel while owning its own data, storage, search, and
//! business semantics entirely -- see this repository's README for the
//! full architecture this crate exists to prove.
//!
//! Librarian claims its own eligible work directly, the same way
//! `infernal-worker-simple` does and for the same reason: the kernel ties
//! a claim to whichever caller signs the claim request, with no
//! delegation, so whatever claims a route must also be what completes it.
//! Taskmaster may recommend what should run next; it never claims on
//! Librarian's behalf.

pub mod claims;
pub mod database;
pub mod dispatch;
pub mod domain;
pub mod error;
pub mod health;
pub mod instance_lease;
pub mod kernel_client;
pub mod postgres_document_repository;
pub mod routed_request;
pub mod routes;
pub mod subscriptions;

use std::env;
use std::time::Duration;

use infernal_client::ClientCredential;
use uuid::Uuid;

use crate::claims::ClaimOutcome;
use crate::database::Database;
use crate::domain::DocumentRepository;
use crate::error::LibrarianError;
use crate::instance_lease::RENEWAL_MARGIN_SECONDS;
use crate::kernel_client::{KernelClient, KernelPort};
use crate::postgres_document_repository::PostgresDocumentRepository;
use crate::routed_request::RoutedRequestOutcome;

const KERNEL_AUTHORITY_ENV: &str = "KERNEL_AUTHORITY";
const LIBRARIAN_SERVICE_ID_ENV: &str = "LIBRARIAN_SERVICE_ID";
const CLAIM_LEASE_SECONDS_ENV: &str = "CLAIM_LEASE_SECONDS";
const POLL_INTERVAL_SECONDS_ENV: &str = "POLL_INTERVAL_SECONDS";
/// Path to a PEM-encoded certificate authority this process should trust
/// in addition to the default public root store, for a kernel reachable
/// only behind a private or self-signed CA.
const KERNEL_CA_CERT_PATH_ENV: &str = "KERNEL_CA_CERT_PATH";
/// A base64url-encoded, 32-byte ADR-0008 enrollment challenge, from a
/// kernel operator's own out-of-band challenge issuance. Optional: unset
/// entirely if this process's identity was already enrolled some other
/// way. When set, `SERVICE_ENDPOINT` and `POD_UID` become required.
const ENROLLMENT_CHALLENGE_ENV: &str = "ENROLLMENT_CHALLENGE";
/// This process's own HTTPS endpoint, submitted as part of the enrollment
/// proof. Librarian has no inbound listener reachable by peers (only the
/// local-only health endpoint -- see `health.rs`), so nothing currently
/// connects to this address; it is recorded by the kernel as instance
/// metadata, not verified for reachability at enrollment time.
const SERVICE_ENDPOINT_ENV: &str = "SERVICE_ENDPOINT";
const POD_UID_ENV: &str = "POD_UID";
const WORKLOAD_TOKEN_PATH_ENV: &str = "WORKLOAD_TOKEN_PATH";
const DEFAULT_WORKLOAD_TOKEN_PATH: &str = "/var/run/secrets/infernal-law-enrollment/token";
const DEFAULT_LEASE_SECONDS: i64 = 300;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 5;

pub struct Config {
    pub client: KernelClient,
    pub repository: PostgresDocumentRepository,
    pub lease_seconds: i64,
    pub poll_interval: Duration,
    /// This process's own instance lease, tracked only when this process
    /// performed its own enrollment at startup (see `run`'s renewal
    /// logic). `None` when `ENROLLMENT_CHALLENGE` was unset because this
    /// identity was already enrolled some other way -- there is no way to
    /// discover another process's enrollment's current lease state after
    /// the fact, so such a process cannot renew and simply keeps today's
    /// behavior of failing once its lease expires.
    pub instance_lease: Option<InstanceLease>,
}

/// This process's own registration lease with the kernel, tracked entirely
/// client-side from the last enrollment or renewal response so `run` knows
/// when to renew next and what revision to renew with.
#[derive(Clone, Copy, Debug)]
pub struct InstanceLease {
    pub revision: i64,
    pub expires_at: i64,
}

impl Config {
    /// `LIBRARIAN_SERVICE_ID` names a `service_id` that must already be
    /// provisioned and enrolled with the kernel (an `identities` row, plus
    /// the real ADR-0008 Kubernetes-TokenReview enrollment for this
    /// process's freshly generated instance key) before any call this
    /// process signs will be accepted -- deployment configuration, not
    /// something this scaffold performs itself, matching every other
    /// reference service.
    pub fn from_env() -> Result<Self, LibrarianError> {
        let authority = env::var(KERNEL_AUTHORITY_ENV)
            .map_err(|_| LibrarianError::MissingEnv(KERNEL_AUTHORITY_ENV))?;
        let service_id: Uuid = env::var(LIBRARIAN_SERVICE_ID_ENV)
            .map_err(|_| LibrarianError::MissingEnv(LIBRARIAN_SERVICE_ID_ENV))?
            .parse()
            .map_err(|_| LibrarianError::InvalidServiceId)?;
        let lease_seconds = env::var(CLAIM_LEASE_SECONDS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_LEASE_SECONDS);
        let poll_interval_seconds = env::var(POLL_INTERVAL_SECONDS_ENV)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
        let credential = ClientCredential::generate(service_id);
        let client = match env::var(KERNEL_CA_CERT_PATH_ENV) {
            Ok(path) => {
                let pem = std::fs::read(&path).map_err(LibrarianError::CaCertificateUnreadable)?;
                KernelClient::with_extra_root_certificate(credential, authority, &pem)?
            }
            Err(_) => KernelClient::new(credential, authority)?,
        };
        let mut instance_lease = None;
        if let Ok(challenge) = env::var(ENROLLMENT_CHALLENGE_ENV) {
            let endpoint = env::var(SERVICE_ENDPOINT_ENV)
                .map_err(|_| LibrarianError::MissingEnv(SERVICE_ENDPOINT_ENV))?;
            let pod_uid =
                env::var(POD_UID_ENV).map_err(|_| LibrarianError::MissingEnv(POD_UID_ENV))?;
            let token_path = env::var(WORKLOAD_TOKEN_PATH_ENV)
                .unwrap_or_else(|_| DEFAULT_WORKLOAD_TOKEN_PATH.to_owned());
            let workload_token = std::fs::read_to_string(&token_path)
                .map_err(LibrarianError::EnrollmentTokenUnreadable)?
                .trim()
                .to_owned();
            let challenge = decode_challenge(&challenge)?;
            let enrolled = client.enroll(challenge, &endpoint, &pod_uid, workload_token)?;
            println!("enrolled with the kernel: {enrolled:?}");
            instance_lease = Some(InstanceLease {
                revision: enrolled.lease_revision,
                expires_at: enrolled.lease_expires_at,
            });
        }
        // Idempotent: a restarted process must not fail, or create a
        // second subscription, just because one already exists.
        for action in dispatch::ACTIONS {
            client.ensure_subscription(action)?;
            println!("subscription active for {action}");
        }
        let database = Database::connect_from_env()?;
        let repository = PostgresDocumentRepository::new(database);
        Ok(Self {
            client,
            repository,
            lease_seconds,
            poll_interval: Duration::from_secs(poll_interval_seconds),
            instance_lease,
        })
    }
}

fn decode_challenge(
    value: &str,
) -> Result<[u8; infernal_client::CHALLENGE_LENGTH], LibrarianError> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| LibrarianError::InvalidEnrollmentChallenge)?
        .try_into()
        .map_err(|_| LibrarianError::InvalidEnrollmentChallenge)
}

/// What one receive/dispatch/complete pass did, for the caller (the main
/// loop) to log. None of the non-`Completed` variants are this service
/// failing: `ClaimLost`/`RequestUnavailable`/`LostBeforeCompletion` are
/// the kernel correctly enforcing an invariant this service must simply
/// accept and move on from, and `UnknownAction` is a routed action this
/// deployment does not perform -- still claimed and completed rather than
/// left dangling, since leaving it claimed-but-incomplete forever would
/// strand the route with no other consumer able to see it.
#[derive(Debug)]
pub enum WorkOutcome {
    NothingEligible,
    ClaimLost {
        route_id: String,
    },
    RequestUnavailable {
        route_id: String,
    },
    LostBeforeCompletion {
        route_id: String,
        claim_id: String,
    },
    Completed {
        route_id: String,
        outcome: dispatch::DispatchOutcome,
    },
    /// The routed request's action is not one Librarian performs. The
    /// claim is still completed -- see this type's own documentation --
    /// but no domain mutation happens.
    UnknownAction {
        route_id: String,
        action: String,
    },
}

pub fn work_once(
    port: &impl KernelPort,
    repository: &impl DocumentRepository,
    lease_seconds: i64,
) -> Result<WorkOutcome, LibrarianError> {
    let routes = port.eligible_routes()?;
    let Some(route) = routes.into_iter().next() else {
        return Ok(WorkOutcome::NothingEligible);
    };

    let claim = match port.propose_claim(&route.route_id, lease_seconds)? {
        ClaimOutcome::Claimed(claim) => claim,
        ClaimOutcome::AlreadyClaimed | ClaimOutcome::RouteNotFound => {
            return Ok(WorkOutcome::ClaimLost {
                route_id: route.route_id,
            });
        }
    };

    let request = match port.routed_request(&route.route_id)? {
        RoutedRequestOutcome::Found(request) => request,
        RoutedRequestOutcome::NotFound => {
            return Ok(WorkOutcome::RequestUnavailable {
                route_id: route.route_id,
            });
        }
    };

    let request_id: Uuid = request
        .request_id
        .parse()
        .map_err(|_| LibrarianError::MalformedResponse("request_id was not a UUID".to_owned()))?;

    // Domain work happens *before* completing the kernel claim, and the
    // claim is only completed if it succeeds (or is recognized as
    // already done) -- see README, "domain commit succeeded, but kernel
    // completion was not recorded": a Librarian database failure here
    // must fail this pass outright, not report a completion that never
    // happened.
    let dispatch_result =
        dispatch::dispatch(&request.action, request_id, &request.scope, repository);
    let outcome = match dispatch_result {
        Ok(outcome) => outcome,
        Err(LibrarianError::UnknownAction(action)) => {
            // Still completed, not left dangling forever: an action
            // Librarian does not perform is not a fencing loss or a
            // transient failure, so there is nothing to retry by leaving
            // the claim open.
            port.complete_claim(&claim.claim_id, claim.fencing_token)?;
            return Ok(WorkOutcome::UnknownAction {
                route_id: route.route_id,
                action,
            });
        }
        Err(error) => return Err(error),
    };

    match port.complete_claim(&claim.claim_id, claim.fencing_token)? {
        crate::claims::CompleteOutcome::Completed(_) => Ok(WorkOutcome::Completed {
            route_id: route.route_id,
            outcome,
        }),
        crate::claims::CompleteOutcome::Fenced | crate::claims::CompleteOutcome::NotFound => {
            Ok(WorkOutcome::LostBeforeCompletion {
                route_id: route.route_id,
                claim_id: claim.claim_id,
            })
        }
    }
}

/// Runs the receive/dispatch/complete loop forever: poll, work, sleep,
/// repeat. A failed pass is logged and retried on the next tick rather
/// than crashing the process -- a transient kernel or network hiccup
/// should not take Librarian down entirely, and the kernel's own claim
/// arbitration is what actually has to be correct, not this loop's
/// uptime. A Librarian *database* failure during a pass, by contrast, is
/// also just logged and retried here -- it already failed before
/// reaching the kernel completion call (see `work_once`), so no
/// incorrect success was ever reported.
pub fn run(config: Config) -> ! {
    let Config {
        client,
        repository,
        lease_seconds,
        poll_interval,
        mut instance_lease,
    } = config;
    loop {
        renew_lease_if_due(&client, &mut instance_lease);
        match work_once(&client, &repository, lease_seconds) {
            Ok(WorkOutcome::NothingEligible) => {}
            Ok(outcome) => println!("{outcome:?}"),
            Err(error) => eprintln!("work pass failed: {error}"),
        }
        std::thread::sleep(poll_interval);
    }
}

/// Renews this process's own instance lease well before the kernel's
/// grant expires -- see `InstanceLease`'s own documentation for why this
/// is only possible when this process performed its own enrollment at
/// startup. A failed renewal is logged and retried on the next tick, the
/// same tolerance `run`'s own work-pass loop already has for a transient
/// kernel or network hiccup; if every attempt fails before the lease
/// actually expires, every subsequent signed call -- including the next
/// renewal attempt -- starts failing until this process restarts and
/// re-enrolls, exactly as it always has.
fn renew_lease_if_due(client: &KernelClient, instance_lease: &mut Option<InstanceLease>) {
    let Some(lease) = instance_lease else {
        return;
    };
    if unix_time() < lease.expires_at - RENEWAL_MARGIN_SECONDS {
        return;
    }
    match client.renew_lease(lease.revision) {
        Ok(renewed) => {
            lease.revision = renewed.lease_revision;
            lease.expires_at = renewed.lease_expires_at;
            println!("renewed instance lease: {renewed:?}");
        }
        Err(error) => eprintln!("instance lease renewal failed: {error}"),
    }
}

fn unix_time() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}
