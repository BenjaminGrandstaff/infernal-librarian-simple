//! Goal: give Kubernetes something to probe. Librarian's real work loop
//! only ever makes outbound calls (to the kernel and to its own
//! database), so without this there would be no inbound listener at all
//! -- and no way for Kubernetes to notice a database that has gone
//! unreachable, since readiness is the only thing that depends on it.
//!
//! Deliberately minimal: no signing, no routing beyond two fixed paths,
//! no dependency on infernal-client-rs (this is not a governed call).
//! `/health/ready` is the only path that touches Librarian's own database,
//! matching the failure semantics this service documents in its README:
//! a database outage should be visible to Kubernetes, not silently
//! ignored.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::database::Database;

pub fn serve(address: &str, database: Database) -> std::io::Result<()> {
    let listener = TcpListener::bind(address)?;
    println!("librarian health endpoint listening on {address}");
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                let database = database.clone();
                thread::spawn(move || {
                    let _ = handle_connection(stream, &database);
                });
            }
            Err(error) => eprintln!("health connection failed: {error}"),
        }
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, database: &Database) -> std::io::Result<()> {
    let mut buffer = [0_u8; 1024];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let (status, body) = match path {
        "/health/live" => ("200 OK", "ok\n"),
        "/health/ready" => {
            if database.check_connection().is_ok() {
                ("200 OK", "ok\n")
            } else {
                ("503 Service Unavailable", "database unavailable\n")
            }
        }
        _ => ("404 Not Found", "not found\n"),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}
