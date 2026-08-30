//! Goal: give every failure mode a specific, typed variant -- matching
//! infernal-law's own error style -- rather than collapsing configuration,
//! transport, protocol, and domain failures into one opaque string.

use std::fmt::{self, Display, Formatter};

use infernal_client::ClientError;

use crate::database::DatabaseError;
use crate::domain::DocumentError;

#[derive(Debug)]
pub enum LibrarianError {
    MissingEnv(&'static str),
    InvalidServiceId,
    Client(ClientError),
    UnexpectedStatus(u16),
    MalformedResponse(String),
    /// `KERNEL_CA_CERT_PATH` was set but the file could not be read.
    CaCertificateUnreadable(std::io::Error),
    /// `ENROLLMENT_CHALLENGE` was set but the projected ServiceAccount
    /// token file it implies (`WORKLOAD_TOKEN_PATH`) could not be read.
    EnrollmentTokenUnreadable(std::io::Error),
    /// `ENROLLMENT_CHALLENGE` was not a valid base64url-encoded 32-byte
    /// value.
    InvalidEnrollmentChallenge,
    /// A routed request's `scope` did not decode into what the named
    /// action expects -- see `dispatch.rs` for the per-action shape.
    MalformedScope(&'static str),
    /// A routed request's `action` is not one this service performs.
    /// Librarian claims and completes it anyway (leaving it unclaimed
    /// forever would strand the route) but performs no domain mutation.
    UnknownAction(String),
    Database(DatabaseError),
    Document(DocumentError),
}

impl Display for LibrarianError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingEnv(name) => {
                write!(formatter, "missing required environment variable {name}")
            }
            Self::InvalidServiceId => formatter.write_str("service ID must be a UUID"),
            Self::Client(error) => write!(formatter, "kernel client error: {error}"),
            Self::UnexpectedStatus(status) => {
                write!(formatter, "kernel returned unexpected status {status}")
            }
            Self::MalformedResponse(message) => {
                write!(formatter, "malformed kernel response: {message}")
            }
            Self::CaCertificateUnreadable(error) => {
                write!(formatter, "could not read KERNEL_CA_CERT_PATH: {error}")
            }
            Self::EnrollmentTokenUnreadable(error) => {
                write!(formatter, "could not read WORKLOAD_TOKEN_PATH: {error}")
            }
            Self::InvalidEnrollmentChallenge => {
                formatter.write_str("ENROLLMENT_CHALLENGE is not a valid base64url 32-byte value")
            }
            Self::MalformedScope(reason) => write!(formatter, "malformed request scope: {reason}"),
            Self::UnknownAction(action) => write!(formatter, "unknown action: {action}"),
            Self::Database(error) => write!(formatter, "librarian database error: {error}"),
            Self::Document(error) => write!(formatter, "document error: {error}"),
        }
    }
}

impl std::error::Error for LibrarianError {}

impl From<ClientError> for LibrarianError {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}

impl From<DatabaseError> for LibrarianError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl From<DocumentError> for LibrarianError {
    fn from(error: DocumentError) -> Self {
        Self::Document(error)
    }
}
