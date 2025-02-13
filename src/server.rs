use std::net::{TcpListener, TcpStream};
use std::io::{self, Write, ErrorKind};
use std::time::Duration;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::fmt;
use log::{info, warn, error, debug, trace};
use chrono::Utc;
use serde_json::json;
use crate::threadpool::{ThreadPool, ThreadPoolError};
use crate::http::{Request, Response, ParseError, Method};

const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_ERRORS: usize = 10;
const ERROR_RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
const TEMP_ERROR_RETRY_DELAY: Duration = Duration::from_millis(50);
const MAX_TEMP_ERROR_RETRIES: u32 = 3;

type RouteHandler = Arc<dyn Fn(&Request, &ServerState) -> Response + Send + Sync>;

pub struct ServerState {
    start_time: chrono::DateTime<Utc>,
    request_count: AtomicUsize,
    error_count: AtomicUsize,
    routes: Arc<RwLock<HashMap<(Method, String), RouteHandler>>>,
    consecutive_errors: AtomicUsize,
    last_error_time: RwLock<chrono::DateTime<Utc>>,
}

pub struct Server {
    listener: TcpListener,
    thread_pool: ThreadPool,
    state: Arc<ServerState>,
    is_shutting_down: Arc<AtomicUsize>, 
    // 0 = running 1 = shutting down 2 = shutdown complete
}

#[derive(Debug)]
pub enum ServerError {
    IoError(io::Error),
    ThreadPoolError(ThreadPoolError),
    ShuttingDown,
    TooManyErrors,
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::IoError(e) => write!(f, "IO Error: {}", e),
            ServerError::ThreadPoolError(e) => write!(f, "Thread Pool Error: {}", e),
            ServerError::ShuttingDown => write!(f, "Server is shutting down"),
            ServerError::TooManyErrors => write!(f, "Too many consecutive errors"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<io::Error> for ServerError {
    fn from(error: io::Error) -> Self {
        ServerError::IoError(error)
    }
}

impl From<ThreadPoolError> for ServerError {
    fn from(error: ThreadPoolError) -> Self {
        ServerError::ThreadPoolError(error)
    }
}

impl Server {
    pub fn new(addr: &str, pool_size: usize) -> Result<Server, ServerError> {
        info!("Initializing server on {} with {} worker threads", addr, pool_size);
        let listener = TcpListener::bind(addr)?;
        let thread_pool = ThreadPool::new(pool_size)?;
        
        let state = Arc::new(ServerState {
            start_time: Utc::now(),
            request_count: AtomicUsize::new(0),
            error_count: AtomicUsize::new(0),
            routes: Arc::new(RwLock::new(HashMap::new())),
            consecutive_errors: AtomicUsize::new(0),
            last_error_time: RwLock::new(Utc::now()),
        });

        // Register routes
        Server::register_default_routes(&state);
        
        Ok(Server {
            listener,
            thread_pool,
            state,
            is_shutting_down: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn register_default_routes(state: &ServerState) {
        let mut routes = state.routes.write().unwrap();
        
        // Home page
        routes.insert(
            (Method::GET, "/".to_string()),
            Arc::new(|_req, state| {
                Response::ok("text/html", Server::render_home_page(state))
            })
        );

        // Health check
        routes.insert(
            (Method::GET, "/health".to_string()),
            Arc::new(|_req, _state| {
                Response::ok("text/plain", b"Server is healthy!".to_vec())
            })
        );

        // Server stats
        routes.insert(
            (Method::GET, "/stats".to_string()),
            Arc::new(|_req, state| {
                let mut response = Response::ok("application/json", 
                    Server::get_server_stats(state).into_bytes());
                response.headers.insert("Cache-Control".to_string(), "no-cache".to_string());
                response
            })
        );

        // Echo server
        routes.insert(
            (Method::POST, "/echo".to_string()),
            Arc::new(|req, _state| {
                Response::ok("text/plain", req.body.clone())
            })
        );
    }

    #[allow(dead_code)]
    pub fn add_route<F>(&self, method: Method, path: &str, handler: F) 
    where
        F: Fn(&Request, &ServerState) -> Response + Send + Sync + 'static,
    {
        let mut routes = self.state.routes.write().unwrap();
        routes.insert((method, path.to_string()), Arc::new(handler));
    }

    pub fn run(&self) -> Result<(), ServerError> {
        info!("Server listening on {}", self.listener.local_addr()?);
        info!("Active worker threads: {}", self.thread_pool.active_count());

        while self.is_shutting_down.load(Ordering::Relaxed) == 0 {
            if self.state.consecutive_errors.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_ERRORS {
                let last_error = *self.state.last_error_time.read().unwrap();
                let elapsed = Utc::now().signed_duration_since(last_error);
                
                if elapsed < chrono::Duration::from_std(ERROR_RECOVERY_INTERVAL).unwrap() {
                    error!("Too many consecutive errors, pausing for recovery");
                    std::thread::sleep(ERROR_RECOVERY_INTERVAL);
                    self.state.consecutive_errors.store(0, Ordering::Relaxed);
                    return Err(ServerError::TooManyErrors);
                }
            }

            if self.is_shutting_down.load(Ordering::Relaxed) > 0 {
                return Err(ServerError::ShuttingDown);
            }

            match self.listener.accept() {
                Ok((stream, addr)) => {
                    self.state.consecutive_errors.store(0, Ordering::Relaxed);
                    self.state.request_count.fetch_add(1, Ordering::Relaxed);
                    
                    let start_time = Utc::now();
                    debug!("New connection from {}", addr);

                    // Configure stream
                    if let Err(e) = stream.set_read_timeout(Some(MAX_REQUEST_TIMEOUT)) {
                        error!("Failed to set read timeout: {}", e);
                        continue;
                    }
                    if let Err(e) = stream.set_write_timeout(Some(MAX_REQUEST_TIMEOUT)) {
                        error!("Failed to set write timeout: {}", e);
                        continue;
                    }

                    let state = Arc::clone(&self.state);
                    let is_shutting_down = Arc::clone(&self.is_shutting_down);

                    self.thread_pool.execute(move || {
                        if is_shutting_down.load(Ordering::Relaxed) > 0 {
                            return;
                        }

                        if let Err(e) = handle_connection(stream, &state) {
                            error!("Error handling connection from {}: {}", addr, e);
                            state.error_count.fetch_add(1, Ordering::Relaxed);
                            state.consecutive_errors.fetch_add(1, Ordering::Relaxed);
                            *state.last_error_time.write().unwrap() = Utc::now();
                        }
                        
                        let duration = Utc::now().signed_duration_since(start_time);
                        debug!("Request from {} completed in {}ms", addr, duration.num_milliseconds());
                    }).map_err(|e| {
                        error!("Failed to execute job: {}", e);
                        ServerError::ThreadPoolError(e)
                    })?;
                }
                Err(e) => {
                    error!("Error accepting connection: {}", e);
                    self.state.error_count.fetch_add(1, Ordering::Relaxed);
                    self.state.consecutive_errors.fetch_add(1, Ordering::Relaxed);
                    *self.state.last_error_time.write().unwrap() = Utc::now();
                }
            }
        }
        Ok(())
    }

    pub fn is_shutdown_complete(&self) -> bool {
        // Check if shutdown is complete (state 2) and all worker threads are done
        self.is_shutting_down.load(Ordering::Relaxed) == 2 && 
        self.thread_pool.active_count() == 0
    }

    pub fn shutdown(&mut self) {
        info!("Initiating graceful shutdown...");
        self.is_shutting_down.store(1, Ordering::Relaxed);
        
        let start = Utc::now();
        while self.thread_pool.active_count() > 0 {
            if Utc::now().signed_duration_since(start).num_seconds() > SHUTDOWN_TIMEOUT.as_secs() as i64 {
                warn!("Shutdown timeout reached, forcing shutdown");
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        if let Err(e) = self.thread_pool.graceful_shutdown(SHUTDOWN_TIMEOUT) {
            error!("Error during thread pool shutdown: {}", e);
        }

        self.is_shutting_down.store(2, Ordering::Relaxed);
        info!("Server shutdown initiated");
    }

    fn render_home_page(state: &ServerState) -> Vec<u8> {
        format!(
            "<!DOCTYPE html>\
            <html>\
            <head>\
                <title>Welcome</title>\
                <style>\
                    body {{ font-family: Arial, sans-serif; margin: 40px; line-height: 1.6; }}\
                    h1 {{ color: #333; }}\
                    .status {{ color: #4CAF50; }}\
                    .nav {{ background: #f8f9fa; padding: 15px; border-radius: 5px; }}\
                    .stats {{ background: #e9ecef; padding: 15px; border-radius: 5px; margin-top: 20px; }}\
                    a {{ color: #007bff; text-decoration: none; }}\
                    a:hover {{ text-decoration: underline; }}\
                </style>\
            </head>\
            <body>\
                <h1>Welcome to Rust HTTP Server</h1>\
                <p class='status'>Server Status: Running</p>\
                <div class='nav'>\
                    <h3>Available Routes:</h3>\
                    <ul>\
                        <li><a href='/'>/</a> - This page</li>\
                        <li><a href='/health'>/health</a> - Basic health check</li>\
                        <li><a href='/stats'>/stats</a> - Detailed server statistics</li>\
                        <li><a href='/echo'>/echo</a> - Echo server (POST)</li>\
                    </ul>\
                </div>\
                <div class='stats'>\
                    <h3>Quick Stats:</h3>\
                    <ul>\
                        <li>Total Requests: {}</li>\
                        <li>Error Rate: {:.2}%</li>\
                        <li>Uptime: {} seconds</li>\
                    </ul>\
                </div>\
            </body>\
            </html>",
            state.request_count.load(Ordering::Relaxed),
            100.0 * state.error_count.load(Ordering::Relaxed) as f64 
                / state.request_count.load(Ordering::Relaxed) as f64,
            Utc::now().signed_duration_since(state.start_time).num_seconds()
        ).into_bytes()
    }

    fn get_server_stats(state: &ServerState) -> String {
        let uptime = Utc::now().signed_duration_since(state.start_time);
        let total_requests = state.request_count.load(Ordering::Relaxed);
        let error_count = state.error_count.load(Ordering::Relaxed);
        let routes: Vec<String> = state.routes.read().unwrap()
            .keys()
            .map(|(method, path)| format!("{:?} {}", method, path))
            .collect();

        json!({
            "status": "healthy",
            "uptime_seconds": uptime.num_seconds(),
            "start_time": state.start_time.to_rfc3339(),
            "total_requests": total_requests,
            "error_count": error_count,
            "success_rate": format!("{:.2}%", 
                if total_requests > 0 {
                    100.0 * (1.0 - error_count as f64 / total_requests as f64)
                } else {
                    100.0
                }
            ),
            "consecutive_errors": state.consecutive_errors.load(Ordering::Relaxed),
            "available_routes": routes,
        }).to_string()
    }
}

fn handle_connection(mut stream: TcpStream, state: &ServerState) -> io::Result<()> {
    let peer_addr = stream.peer_addr()?;
    trace!("Starting request handling for {}", peer_addr);
    
    // Parse the request
    let request = match Request::parse(&mut stream) {
        Ok(request) => {
            info!("Received {:?} request for {} from {} with {} headers", 
                request.method, request.path, peer_addr, request.headers.len());
            
            if request.method == Method::POST && !request.headers.contains_key("Content-Type") {
                warn!("Missing Content-Type header for POST request from {}", peer_addr);
                let response = Response::bad_request("Missing Content-Type header");
                write_response_with_retry(&mut stream, &response.to_bytes())?;
                return Ok(());
            }
            request
        },
        Err(ParseError::ContentTooLarge) => {
            warn!("Request too large from {}", peer_addr);
            let response = Response::bad_request("Request body too large");
            write_response_with_retry(&mut stream, &response.to_bytes())?;
            return Ok(());
        },
        Err(ParseError::InvalidRequest) => {
            warn!("Invalid request from {}", peer_addr);
            let response = Response::bad_request("Invalid request format");
            write_response_with_retry(&mut stream, &response.to_bytes())?;
            return Ok(());
        },
        Err(ParseError::IoError(e)) => {
            if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut {
                debug!("Temporary IO error reading request from {}: {}", peer_addr, e);
            } else {
                error!("IO error reading request from {}: {}", peer_addr, e);
            }
            return Err(e);
        }
    };
    
    let response = {
        let routes = state.routes.read().unwrap();
        let key = (request.method.clone(), request.path.clone());
        
        if routes.contains_key(&key) {
            routes[&key](&request, state)
        } else if routes.keys().any(|(_, p)| p == &request.path) {
            warn!("405 Method Not Allowed: {:?} {}", request.method, request.path);
            Response::method_not_allowed(&["GET", "POST"])
        } else {
            warn!("404 Not Found: {:?} {}", request.method, request.path);
            Response::not_found()
        }
    };
    
    // Send the response 
    write_response_with_retry(&mut stream, &response.to_bytes())?;
    
    trace!("Completed request handling for {}", peer_addr);
    Ok(())
}

fn write_response_with_retry(stream: &mut TcpStream, response: &[u8]) -> io::Result<()> {
    let mut retries = 0;
    let mut written = 0;
    
    while written < response.len() {
        match stream.write(&response[written..]) {
            Ok(n) => {
                written += n;
                retries = 0; // Reset retry counter on successful write
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if retries < MAX_TEMP_ERROR_RETRIES {
                    retries += 1;
                    std::thread::sleep(TEMP_ERROR_RETRY_DELAY);
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }
    
    let mut retries = 0;
    loop {
        match stream.flush() {
            Ok(_) => break,
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if retries < MAX_TEMP_ERROR_RETRIES {
                    retries += 1;
                    std::thread::sleep(TEMP_ERROR_RETRY_DELAY);
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
    }
    
    Ok(())
}
