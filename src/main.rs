mod threadpool;
mod server;
mod http;
mod config;
mod middleware;

use server::Server;
use std::process;
use std::sync::{Arc, Mutex};
use log::{info, error};
use env_logger::Env;
use config::Config;
use middleware::{LoggingMiddleware, SecurityHeadersMiddleware, ErrorHandlingMiddleware};
use std::path::Path;

fn main() {
    // Load configuration
    let config = match Config::from_file(Path::new("config.json")) {
        Ok(config) => config,
        Err(_) => {
            info!("No config file found, using default configuration");
            Config::default()
        }
    };

    // Initialize logger
    env_logger::Builder::from_env(Env::default().default_filter_or(&config.log_level))
        .format_timestamp_millis()
        .init();

    info!("Starting HTTP server...");

    let server = match Server::new(&config.address(), config.workers) {
        Ok(server) => server,
        Err(e) => {
            error!("Failed to start server: {:?}", e);
            process::exit(1);
        }
    };

    // Add middleware
    let server = server
        .with_middleware(Box::new(LoggingMiddleware))
        .with_middleware(Box::new(SecurityHeadersMiddleware))
        .with_middleware(Box::new(ErrorHandlingMiddleware));

    let server = Arc::new(Mutex::new(server));
    let server_clone = Arc::clone(&server);

    info!("Server available at http://{}", config.address());
    info!("Press Ctrl+C to stop the server");

    // Handle graceful shutdown
    ctrlc::set_handler(move || {
        info!("Shutting down server...");
        if let Ok(guard) = server_clone.lock() {
            if let Err(e) = guard.shutdown() {
                error!("Error during shutdown: {:?}", e);
            }
        }
        process::exit(0);
    }).expect("Error setting Ctrl-C handler");

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
