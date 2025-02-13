# web-server

A simple multi-threaded web server written in Rust that handles HTTP requests.

## Features

- Multi-threaded request handling using a thread pool
- Support for GET and POST requests
- Request timeout handling
- Graceful shutdown with timeout
- Basic error handling and recovery
- Server statistics endpoint
- Clean shutdown on Ctrl+C

## Usage

1. Clone the repository
2. Run `cargo run` to start the server
3. Server will be available at http://127.0.0.1:7878
4. Press Ctrl+C to initiate graceful shutdown

## API Endpoints

- `GET /` - Returns a simple status page
- `GET /stats` - Returns server statistics in JSON format


## Known Issues

- Ctrl+C handling for graceful shutdown is currently not working as expected and will be fixed later. The server may need to be terminated manually if Ctrl+C does not trigger a clean shutdown.


## Configuration

The following constants can be modified in the code:

- `MAX_REQUEST_TIMEOUT`: Maximum time to wait for a request (30s)
- `SHUTDOWN_TIMEOUT`: Maximum time to wait during shutdown (30s) 
- `MAX_CONSECUTIVE_ERRORS`: Number of errors before temporary shutdown (10)
