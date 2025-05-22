# Rust Web Server

A modern, multi-threaded web server written in Rust with middleware support and robust error handling.

## Features

- Multi-threaded request handling using a thread pool
- Middleware support for request/response processing
- Built-in middleware:
  - Request logging with timing information
  - Security headers (XSS protection, content type options)
  - Error handling and logging
- Support for GET and POST requests
- Request timeout handling
- Graceful shutdown with Ctrl+C support
- Server statistics and health monitoring
- JSON configuration support
- Clean error handling and recovery

## Usage

1. Clone the repository
2. Configure the server (optional):
   - Create a `config.json` file with your settings:
   ```json
   {
       "host": "127.0.0.1",
       "port": 7878,
       "workers": 4,
       "static_dir": "public",
       "log_level": "info"
   }
   ```
3. Run `cargo run` to start the server
4. Server will be available at http://127.0.0.1:7878
5. Press Ctrl+C for graceful shutdown

## API Endpoints

- `GET /` - Returns a beautiful status page with server metrics
- `GET /health` - Health check endpoint
- `GET /stats` - Returns server statistics in JSON format
- `POST /echo` - Echo service that returns the request body

## Configuration

The server can be configured through `config.json`:

- `host`: Server host address (default: "127.0.0.1")
- `port`: Server port (default: 7878)
- `workers`: Number of worker threads (default: 4)
- `static_dir`: Directory for static files (optional)
- `log_level`: Logging level (default: "info")

## Security Features

- X-Content-Type-Options: nosniff
- X-Frame-Options: DENY
- X-XSS-Protection: 1; mode=block
- Request timeout protection
- Error rate limiting
- Graceful error recovery

## Dependencies

- log & env_logger - Logging
- chrono - Time handling
- http - HTTP types
- serde & serde_json - JSON handling
- ctrlc - Graceful shutdown

## Development

To build the project:
```bash
cargo build
```

To run tests:
```bash
cargo test
```

## License

MIT License

## Known Issues

- Ctrl+C handling for graceful shutdown is currently not working as expected and will be fixed later. The server may need to be terminated manually if Ctrl+C does not trigger a clean shutdown.

## Configuration

The following constants can be modified in the code:

- `MAX_REQUEST_TIMEOUT`: Maximum time to wait for a request (30s)
- `SHUTDOWN_TIMEOUT`: Maximum time to wait during shutdown (30s) 
- `MAX_CONSECUTIVE_ERRORS`: Number of errors before temporary shutdown (10)
