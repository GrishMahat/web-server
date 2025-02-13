mod threadpool;
mod server;
mod http;

use server::Server;
use std::process;
use std::sync::{Arc, Mutex};
use log::{info, error};
use env_logger::Env;

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    info!("Starting HTTP server...");

    let server = match Server::new("127.0.0.1:7878", 4) {
        Ok(server) => server,
        Err(e) => {
            error!("Failed to start server: {:?}", e);
            process::exit(1);
        }
    };

    let server = Arc::new(Mutex::new(server));

    info!("Server available at http://127.0.0.1:7878");
    info!("Press Ctrl+C to stop the server");

    let guard = match server.lock() {
        Ok(guard) => guard,
        Err(e) => {
            error!("Failed to lock server: {}", e);
            process::exit(1);
        }
    };

    if let Err(e) = guard.run() {
        error!("Server error: {:?}", e);
        process::exit(1);
    }
}
