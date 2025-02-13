mod threadpool;
mod server;
mod http;

use server::Server;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use log::{info, error, warn};
use env_logger::Env;
use ctrlc;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

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
    let server_clone = Arc::clone(&server);
    let force_shutdown = Arc::new(Mutex::new(false));
    let force_shutdown_clone = Arc::clone(&force_shutdown);
    
    ctrlc::set_handler(move || {
        let mut force = force_shutdown_clone.lock().unwrap();
        if *force {
            info!("Forcing immediate shutdown...");
            process::exit(1);
        }
        
        *force = true;
        info!("\nReceived shutdown signal, initiating graceful shutdown...");
        
        let shutdown_start = Instant::now();
        // todo ctrlc is not working properly  i will fix it later 
        match server_clone.lock() {
            Ok(mut guard) => {
                guard.shutdown();
                
                while shutdown_start.elapsed() < SHUTDOWN_TIMEOUT {
                    if guard.is_shutdown_complete() {
                        info!("Server shutdown completed successfully");
                        process::exit(0);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                
                warn!("Shutdown timeout reached, forcing exit");
                process::exit(1);
            }
            Err(e) => {
                error!("Failed to acquire server lock for shutdown: {}", e);
                process::exit(1);
            }
        }
    }).expect("Error setting Ctrl-C handler");

    info!("Server available at http://127.0.0.1:7878");
    info!("Press Ctrl+C to stop the server");

    // Run the server
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
