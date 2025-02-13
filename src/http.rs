use std::collections::HashMap;
use std::io::{self, Read, ErrorKind};
use std::thread;
use std::time::Duration;

const MAX_HEADER_SIZE: usize = 8192; // 8KB
const MAX_READ_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub enum Method {
    GET,
    POST,
    PUT,
    DELETE,
    HEAD,
    OPTIONS,
    PATCH,
}

impl From<&str> for Method {
    fn from(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            "HEAD" => Method::HEAD,
            "OPTIONS" => Method::OPTIONS,
            "PATCH" => Method::PATCH,
            _ => Method::GET // Default  GET
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    InvalidRequest,
    ContentTooLarge,
    IoError(io::Error),
}

impl From<io::Error> for ParseError {
    fn from(error: io::Error) -> Self {
        ParseError::IoError(error)
    }
}

#[derive(Debug)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status_code: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn parse(mut stream: impl Read) -> Result<Request, ParseError> {
        let mut headers_buffer = vec![0; MAX_HEADER_SIZE];
        let mut headers_pos = 0;
        let mut found_header_end = false;
        let mut retries = 0;

        // Read headers with retry
        'read_headers: while headers_pos < headers_buffer.len() {
            match stream.read(&mut headers_buffer[headers_pos..headers_pos + 1]) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    headers_pos += n;
                    if headers_pos >= 4 && 
                       &headers_buffer[headers_pos - 4..headers_pos] == b"\r\n\r\n" {
                        found_header_end = true;
                        break;
                    }
                    retries = 0; // Reset retry counter on successful read
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                    if retries < MAX_READ_RETRIES {
                        retries += 1;
                        thread::sleep(RETRY_DELAY);
                        continue 'read_headers;
                    }
                    return Err(ParseError::IoError(e));
                }
                Err(e) => return Err(ParseError::IoError(e)),
            }
        }

        if !found_header_end {
            return Err(ParseError::InvalidRequest);
        }

        let headers_str = String::from_utf8_lossy(&headers_buffer[..headers_pos]);
        let mut lines = headers_str.lines();

        // Parse request line
        let request_line = lines.next().ok_or(ParseError::InvalidRequest)?;
        let mut parts = request_line.split_whitespace();
        let method = Method::from(parts.next().ok_or(ParseError::InvalidRequest)?);
        let path = parts.next().ok_or(ParseError::InvalidRequest)?.to_string();

        // Parse headers
        let mut headers = HashMap::new();
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(": ") {
                headers.insert(key.to_string(), value.to_string());
            }
        }

        let body = if let Some(length) = headers.get("Content-Length") {
            let length: usize = length.parse().map_err(|_| ParseError::InvalidRequest)?;
            if length > 1024 * 1024 * 10 { // 10MB limit
                return Err(ParseError::ContentTooLarge);
            }
            let mut body = vec![0; length];
            let mut pos = 0;
            let mut retries = 0;

            while pos < length {
                match stream.read(&mut body[pos..]) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        pos += n;
                        retries = 0; // Reset retry counter on successful read
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                        if retries < MAX_READ_RETRIES {
                            retries += 1;
                            thread::sleep(RETRY_DELAY);
                            continue;
                        }
                        return Err(ParseError::IoError(e));
                    }
                    Err(e) => return Err(ParseError::IoError(e)),
                }
            }
            body
        } else if headers.get("Transfer-Encoding").map_or(false, |v| v.to_lowercase() == "chunked") {
            let mut body = Vec::new();
            let mut retries = 0;

            loop {
                let mut size_line = String::new();
                let mut size_bytes = [0; 2];
                
                // Read chunk size with retry
                'read_size: loop {
                    match stream.read(&mut size_bytes[..1]) {
                        Ok(0) => break,
                        Ok(1) => {
                            size_line.push(size_bytes[0] as char);
                            if size_line.ends_with("\r\n") {
                                break;
                            }
                            retries = 0;
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                            if retries < MAX_READ_RETRIES {
                                retries += 1;
                                thread::sleep(RETRY_DELAY);
                                continue 'read_size;
                            }
                            return Err(ParseError::IoError(e));
                        }
                        Err(e) => return Err(ParseError::IoError(e)),
                        _ => continue,
                    }
                }

                let size = usize::from_str_radix(size_line.trim_end(), 16)
                    .map_err(|_| ParseError::InvalidRequest)?;
                if size == 0 {
                    break;
                }

                let mut chunk = vec![0; size];
                let mut pos = 0;
                retries = 0;

                // Read chunk data with retry
                while pos < size {
                    match stream.read(&mut chunk[pos..]) {
                        Ok(0) => break,
                        Ok(n) => {
                            pos += n;
                            retries = 0;
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                            if retries < MAX_READ_RETRIES {
                                retries += 1;
                                thread::sleep(RETRY_DELAY);
                                continue;
                            }
                            return Err(ParseError::IoError(e));
                        }
                        Err(e) => return Err(ParseError::IoError(e)),
                    }
                }

                body.extend(chunk);

                // Read trailing CRLF with retry
                retries = 0;
                'read_crlf: loop {
                    match stream.read(&mut size_bytes) {
                        Ok(2) => break,
                        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                            if retries < MAX_READ_RETRIES {
                                retries += 1;
                                thread::sleep(RETRY_DELAY);
                                continue 'read_crlf;
                            }
                            return Err(ParseError::IoError(e));
                        }
                        Err(e) => return Err(ParseError::IoError(e)),
                        _ => continue,
                    }
                }
            }
            body
        } else {
            Vec::new()
        };

        Ok(Request {
            method,
            path,
            headers,
            body,
        })
    }
}

impl Response {
    pub fn new(status_code: u16, status_text: &str, content_type: &str, body: Vec<u8>) -> Response {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), content_type.to_string());
        headers.insert("Content-Length".to_string(), body.len().to_string());
        headers.insert("Connection".to_string(), "close".to_string());
        headers.insert("Server".to_string(), "Rust-HTTP-Server/1.0".to_string());
        
        Response {
            status_code,
            status_text: status_text.to_string(),
            headers,
            body,
        }
    }
    
    pub fn ok(content_type: &str, body: Vec<u8>) -> Response {
        Response::new(200, "OK", content_type, body)
    }
    
    pub fn not_found() -> Response {
        Response::new(404, "Not Found", "text/html", 
            b"<!DOCTYPE html>\
            <html>\
            <head><title>404 Not Found</title></head>\
            <body>\
                <h1>404 Not Found</h1>\
                <p>The requested resource could not be found on this server.</p>\
            </body>\
            </html>".to_vec())
    }
    
    #[allow(dead_code)]
    pub fn internal_server_error() -> Response {
        Response::new(500, "Internal Server Error", "text/html",
            b"<!DOCTYPE html>\
            <html>\
            <head><title>500 Internal Server Error</title></head>\
            <body>\
                <h1>500 Internal Server Error</h1>\
                <p>The server encountered an internal error.</p>\
            </body>\
            </html>".to_vec())
    }
    
    pub fn method_not_allowed(allowed_methods: &[&str]) -> Response {
        let mut response = Response::new(405, "Method Not Allowed", "text/html",
            b"<!DOCTYPE html>\
            <html>\
            <head><title>405 Method Not Allowed</title></head>\
            <body>\
                <h1>405 Method Not Allowed</h1>\
                <p>The requested method is not allowed for this resource.</p>\
            </body>\
            </html>".to_vec());
        response.headers.insert("Allow".to_string(), allowed_methods.join(", "));
        response
    }

    pub fn bad_request(message: &str) -> Response {
        Response::new(400, "Bad Request", "text/html",
            format!("<!DOCTYPE html>\
            <html>\
            <head><title>400 Bad Request</title></head>\
            <body>\
                <h1>400 Bad Request</h1>\
                <p>{}</p>\
            </body>\
            </html>", message).into_bytes())
    }
    
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut response = Vec::new();
        
        response.extend_from_slice(
            format!("HTTP/1.1 {} {}\r\n", self.status_code, self.status_text).as_bytes()
        );
        
        // Headers
        for (key, value) in &self.headers {
            response.extend_from_slice(
                format!("{}: {}\r\n", key, value).as_bytes()
            );
        }
        
        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(&self.body);
        
        response
    }
} 