use std::env;
use std::thread;

use infernal_librarian_simple::{Config, health, run};

const HEALTH_ADDRESS_ENV: &str = "HEALTH_ADDRESS";
const DEFAULT_HEALTH_ADDRESS: &str = "0.0.0.0:8090";

fn main() {
    let config = Config::from_env().unwrap_or_else(|error| {
        eprintln!("configuration failed: {error}");
        std::process::exit(1);
    });

    let health_database = config.repository.database().clone();
    let health_address =
        env::var(HEALTH_ADDRESS_ENV).unwrap_or_else(|_| DEFAULT_HEALTH_ADDRESS.to_owned());
    let heartbeat = health::Heartbeat::new();
    let health_heartbeat = heartbeat.clone();
    thread::spawn(move || {
        if let Err(error) = health::serve(
            &health_address,
            health_database,
            health_heartbeat,
            health::Thresholds::default(),
        ) {
            eprintln!("health endpoint failed: {error}");
        }
    });

    run(config, heartbeat);
}
