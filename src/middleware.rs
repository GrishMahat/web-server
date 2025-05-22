use crate::http::{Request, Response};
use log::{info, error};
use std::time::Instant;
use chrono::Utc;

pub trait Middleware: Send + Sync {
    fn process(&self, request: &mut Request) -> Option<Response>;
    fn after(&self, request: &Request, response: &mut Response);
}

pub struct LoggingMiddleware;

impl Middleware for LoggingMiddleware {
    fn process(&self, request: &mut Request) -> Option<Response> {
        request.headers.insert("x-start-time".to_string(), Instant::now().elapsed().as_millis().to_string());
        None
    }

    fn after(&self, request: &Request, response: &mut Response) {
        let start_time = request.headers.get("x-start-time")
            .and_then(|t| t.parse::<u128>().ok())
            .unwrap_or(0);
        let duration = Instant::now().elapsed().as_millis() - start_time;
        
        info!(
            "{} {:?} {} {} {}ms",
            Utc::now().format("%Y-%m-%d %H:%M:%S"),
            request.method,
            request.path,
            response.status_code,
            duration
        );
    }
}

pub struct SecurityHeadersMiddleware;

impl Middleware for SecurityHeadersMiddleware {
    fn process(&self, _request: &mut Request) -> Option<Response> {
        None
    }

    fn after(&self, _request: &Request, response: &mut Response) {
        response.headers.insert(
            "X-Content-Type-Options".to_string(),
            "nosniff".to_string(),
        );
        response.headers.insert(
            "X-Frame-Options".to_string(),
            "DENY".to_string(),
        );
        response.headers.insert(
            "X-XSS-Protection".to_string(),
            "1; mode=block".to_string(),
        );
    }
}

pub struct ErrorHandlingMiddleware;

impl Middleware for ErrorHandlingMiddleware {
    fn process(&self, _request: &mut Request) -> Option<Response> {
        None
    }

    fn after(&self, _request: &Request, response: &mut Response) {
        if response.status_code >= 400 {
            error!(
                "Error response: {} - {}",
                response.status_code,
                response.status_text
            );
        }
    }
} 