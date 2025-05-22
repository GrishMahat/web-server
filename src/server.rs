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
use crate::middleware::Middleware;

const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
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
    pool: ThreadPool,
    middleware: Arc<Vec<Box<dyn Middleware>>>,
    state: Arc<ServerState>,
    is_shutting_down: Arc<AtomicUsize>,
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
    pub fn new(addr: &str, workers: usize) -> Result<Self, ServerError> {
        info!("Initializing server on {} with {} worker threads", addr, workers);
        let listener = TcpListener::bind(addr)?;
        let pool = ThreadPool::new(workers)?;
        
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
            pool,
            middleware: Arc::new(Vec::new()),
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

    pub fn with_middleware(mut self, middleware: Box<dyn Middleware>) -> Self {
        let mut m = Vec::new();
        std::mem::swap(&mut m, Arc::get_mut(&mut self.middleware).unwrap());
        m.push(middleware);
        self.middleware = Arc::new(m);
        self
    }

    pub fn run(&self) -> Result<(), ServerError> {
        info!("Server listening on {}", self.listener.local_addr()?);
        info!("Active worker threads: {}", self.pool.active_count());

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
                    let middleware = Arc::clone(&self.middleware);

                    self.pool.execute(move || {
                        if is_shutting_down.load(Ordering::Relaxed) > 0 {
                            return;
                        }

                        if let Err(e) = handle_connection(stream, &state, &middleware) {
                            error!("Error handling connection from {}: {}", addr, e);
                            state.error_count.fetch_add(1, Ordering::Relaxed);
                            state.consecutive_errors.fetch_add(1, Ordering::Relaxed);
                            *state.last_error_time.write().unwrap() = Utc::now();
                        }
                        
                        let duration = Utc::now().signed_duration_since(start_time);
                        debug!("Request from {} completed in {}ms", addr, duration.num_milliseconds());
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

    pub fn shutdown(&self) -> Result<(), ServerError> {
        info!("Shutting down server...");
        self.is_shutting_down.store(1, Ordering::Relaxed);
        Ok(())
    }

    fn render_home_page(state: &ServerState) -> Vec<u8> {
        let html = format!(r#"<!DOCTYPE html>
    <html lang="en">
    <head>
        <meta charset="utf-8">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <title>Rust HTTP Server - Welcome</title>
        <style>
            body {{
                font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
                margin: 0;
                padding: 0;
                background: linear-gradient(135deg, #ece9e6, #ffffff);
                color: #333;
            }}
            .container {{
                max-width: 1200px;
                margin: 50px auto;
                background: #fff;
                padding: 40px;
                border-radius: 12px;
                box-shadow: 0 4px 12px rgba(0,0,0,0.1);
            }}
            header {{
                text-align: center;
                margin-bottom: 30px;
            }}
            .logo {{
                width: 80px;
                height: 80px;
                background: #2980b9;
                border-radius: 50%;
                margin: 0 auto 20px;
                display: flex;
                align-items: center;
                justify-content: center;
                font-size: 2em;
                color: #fff;
            }}
            header h1 {{
                font-size: 3em;
                margin: 0;
                color: #2c3e50;
            }}
            .status {{
                display: inline-block;
                background: #27ae60;
                color: #fff;
                padding: 8px 16px;
                border-radius: 20px;
                font-weight: bold;
                margin-top: 10px;
                animation: pulse 2s infinite;
            }}
            @keyframes pulse {{
                0% {{ transform: scale(1); }}
                50% {{ transform: scale(1.05); }}
                100% {{ transform: scale(1); }}
            }}
            nav {{
                background: #f8f9fa;
                padding: 20px;
                border-radius: 8px;
                margin: 30px 0;
                border: 1px solid #dee2e6;
            }}
            nav ul {{
                list-style: none;
                padding: 0;
                display: flex;
                flex-wrap: wrap;
                justify-content: center;
            }}
            nav li {{
                margin: 10px 15px;
            }}
            nav a {{
                color: #3498db;
                text-decoration: none;
                font-weight: 500;
                transition: color 0.2s;
            }}
            nav a:hover {{
                color: #2980b9;
            }}
            .stats {{
                background: #e9ecef;
                padding: 30px;
                border-radius: 8px;
                border: 1px solid #dee2e6;
                margin-bottom: 30px;
            }}
            .stats h2 {{
                text-align: center;
                color: #34495e;
                margin-bottom: 20px;
            }}
            .metrics {{
                display: grid;
                grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
                gap: 20px;
            }}
            .metric-card {{
                background: #fff;
                padding: 20px;
                border-radius: 8px;
                text-align: center;
                box-shadow: 0 2px 6px rgba(0,0,0,0.1);
            }}
            .metric-value {{
                font-size: 2em;
                font-weight: bold;
                color: #2980b9;
            }}
            .metric-label {{
                font-size: 0.9em;
                color: #7f8c8d;
                margin-top: 5px;
            }}
            footer {{
                text-align: center;
                font-size: 0.9em;
                color: #7f8c8d;
                margin-top: 40px;
            }}
        </style>
    </head>
    <body>
        <div class="container">
            <header>
                <div class="logo">🦀</div>
                <h1>Rust HTTP Server</h1>
                <p class="status">Server Status: Running</p>
            </header>
            <nav>
                <h3>Available Routes</h3>
                <ul>
                    <li><a href="/">Home</a></li>
                    <li><a href="/health">Health Check</a></li>
                    <li><a href="/stats">Server Statistics (JSON)</a></li>
                    <li><a href="/echo">Echo Service (POST)</a></li>
                </ul>
            </nav>
            <section class="stats">
                <h2>Server Metrics</h2>
                <div class="metrics">
                    <div class="metric-card">
                        <div class="metric-value">{}</div>
                        <div class="metric-label">Total Requests</div>
                    </div>
                    <div class="metric-card">
                        <div class="metric-value">{:.1}%</div>
                        <div class="metric-label">Success Rate</div>
                    </div>
                    <div class="metric-card">
                        <div class="metric-value">{}</div>
                        <div class="metric-label">Uptime (seconds)</div>
                    </div>
                </div>
            </section>
            <footer>
                <p>Powered by Rust 🦀 | Server Time: {}</p>
            </footer>
        </div>
    </body>
    </html>"#,
            state.request_count.load(Ordering::Relaxed),
            100.0 - (100.0 * state.error_count.load(Ordering::Relaxed) as f64 
                    / state.request_count.load(Ordering::Relaxed).max(1) as f64),
            Utc::now().signed_duration_since(state.start_time).num_seconds(),
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        );
        html.into_bytes()
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

fn handle_connection(mut stream: TcpStream, state: &ServerState, middleware: &[Box<dyn Middleware>]) -> io::Result<()> {
    let peer_addr = stream.peer_addr()?;
    trace!("Starting request handling for {}", peer_addr);
    
    // Parse the request
    let mut request = match Request::parse(&mut stream) {
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
    
    let mut response = {
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
    
    // Process middleware
    for m in middleware {
        if let Some(m_response) = m.process(&mut request) {
            response = m_response;
        }
    }

    // Process after middleware
    for m in middleware {
        m.after(&request, &mut response);
    }

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
