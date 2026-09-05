//! Goal: give Kubernetes something to probe, and make it probe the thing
//! that actually breaks.
//!
//! This service's work loop only makes outbound calls -- to the kernel and
//! to its own database -- so a failure has no inbound surface to show up
//! on. Readiness used to check only the database, which meant the most
//! common real failure was invisible: an instance lease that has expired
//! cannot be renewed (the kernel rejects the renewal along with every other
//! signed call), so the loop 401s forever while the database stays
//! perfectly reachable and Kubernetes reports a healthy container.
//!
//! Readiness now requires both: a reachable database *and* a recent
//! successful pass against the kernel.
//!
//! Liveness deliberately checks only the kernel heartbeat, never the
//! database. Restarting repairs an expired lease, because a new process
//! enrolls again. Restarting does not repair an unreachable database -- it
//! would only turn an outage into a crash loop, and lose the readiness
//! signal that was correctly reporting the outage.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread;

use crate::database::Database;

/// Shared record of when the work loop last completed a pass against the
/// kernel. Zero means "not yet".
#[derive(Clone, Debug, Default)]
pub struct Heartbeat(Arc<AtomicI64>);

impl Heartbeat {
    pub fn new() -> Self {
        Self(Arc::new(AtomicI64::new(0)))
    }

    pub fn record_success(&self, at: i64) {
        self.0.store(at, Ordering::Relaxed);
    }

    pub fn last_success(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn age(&self, now: i64) -> Option<i64> {
        match self.last_success() {
            0 => None,
            last => Some(now.saturating_sub(last)),
        }
    }
}

pub const DEFAULT_READY_STALE_SECONDS: i64 = 30;
/// Much slacker than readiness: a restart throws away a working enrollment,
/// so it must not be the response to a brief kernel or network hiccup.
pub const DEFAULT_LIVE_STALE_SECONDS: i64 = 150;

pub struct Thresholds {
    pub ready_stale_seconds: i64,
    pub live_stale_seconds: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            ready_stale_seconds: DEFAULT_READY_STALE_SECONDS,
            live_stale_seconds: DEFAULT_LIVE_STALE_SECONDS,
        }
    }
}

/// Probe policy with no I/O, so it is testable on its own. `database_ok` is
/// passed in rather than checked here for the same reason.
pub fn evaluate(
    path: &str,
    heartbeat: &Heartbeat,
    database_ok: bool,
    thresholds: &Thresholds,
    now: i64,
) -> (&'static str, String) {
    match path {
        "/health/live" => match heartbeat.age(now) {
            // Still starting or enrolling: Kubernetes' own initialDelay
            // governs startup, not a restart-worthy failure here.
            None => ("200 OK", "starting\n".to_owned()),
            Some(age) if age <= thresholds.live_stale_seconds => ("200 OK", "ok\n".to_owned()),
            Some(age) => (
                "503 Service Unavailable",
                format!("no successful kernel pass for {age}s\n"),
            ),
        },
        "/health/ready" => {
            if !database_ok {
                return (
                    "503 Service Unavailable",
                    "database unavailable\n".to_owned(),
                );
            }
            match heartbeat.age(now) {
                None => (
                    "503 Service Unavailable",
                    "no successful kernel pass yet\n".to_owned(),
                ),
                Some(age) if age <= thresholds.ready_stale_seconds => ("200 OK", "ok\n".to_owned()),
                Some(age) => (
                    "503 Service Unavailable",
                    format!("last successful kernel pass {age}s ago\n"),
                ),
            }
        }
        _ => ("404 Not Found", "not found\n".to_owned()),
    }
}

pub fn serve(
    address: &str,
    database: Database,
    heartbeat: Heartbeat,
    thresholds: Thresholds,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(address)?;
    println!("librarian health endpoint listening on {address}");
    let thresholds = Arc::new(thresholds);
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let database = database.clone();
                let heartbeat = heartbeat.clone();
                let thresholds = Arc::clone(&thresholds);
                thread::spawn(move || {
                    let _ = handle_connection(stream, &database, &heartbeat, &thresholds);
                });
            }
            Err(error) => eprintln!("health connection failed: {error}"),
        }
    }
    Ok(())
}

fn handle_connection(
    mut stream: TcpStream,
    database: &Database,
    heartbeat: &Heartbeat,
    thresholds: &Thresholds,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 1024];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");
    // Only consulted for readiness; liveness must not depend on it.
    let database_ok = path != "/health/ready" || database.check_connection().is_ok();
    let (status, body) = evaluate(path, heartbeat, database_ok, thresholds, now_seconds());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this change exists for: the database is fine, the loop is
    /// running, and every kernel call is being rejected.
    #[test]
    fn a_healthy_database_does_not_make_a_stuck_loop_ready() {
        let heartbeat = Heartbeat::new();
        heartbeat.record_success(1_000);
        let (status, _) = evaluate(
            "/health/ready",
            &heartbeat,
            true,
            &Thresholds::default(),
            1_100,
        );
        assert_eq!(status, "503 Service Unavailable");
    }

    #[test]
    fn a_recent_kernel_pass_with_a_reachable_database_is_ready() {
        let heartbeat = Heartbeat::new();
        heartbeat.record_success(1_000);
        let (status, _) = evaluate(
            "/health/ready",
            &heartbeat,
            true,
            &Thresholds::default(),
            1_010,
        );
        assert_eq!(status, "200 OK");
    }

    #[test]
    fn an_unreachable_database_is_not_ready() {
        let heartbeat = Heartbeat::new();
        heartbeat.record_success(1_000);
        let (status, _) = evaluate(
            "/health/ready",
            &heartbeat,
            false,
            &Thresholds::default(),
            1_010,
        );
        assert_eq!(status, "503 Service Unavailable");
    }

    /// Restarting cannot fix a database outage, so liveness must ignore it.
    #[test]
    fn an_unreachable_database_never_triggers_a_restart() {
        let heartbeat = Heartbeat::new();
        heartbeat.record_success(1_000);
        let (status, _) = evaluate(
            "/health/live",
            &heartbeat,
            false,
            &Thresholds::default(),
            1_010,
        );
        assert_eq!(status, "200 OK");
    }

    /// An expired lease is only recoverable by restarting and re-enrolling.
    #[test]
    fn a_long_stuck_loop_does_trigger_a_restart() {
        let heartbeat = Heartbeat::new();
        heartbeat.record_success(1_000);
        let (status, _) = evaluate(
            "/health/live",
            &heartbeat,
            true,
            &Thresholds::default(),
            1_200,
        );
        assert_eq!(status, "503 Service Unavailable");
    }

    #[test]
    fn liveness_is_slacker_than_readiness() {
        let defaults = Thresholds::default();
        assert!(defaults.live_stale_seconds > defaults.ready_stale_seconds);
    }
}
