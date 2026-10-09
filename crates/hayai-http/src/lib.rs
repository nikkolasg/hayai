//! A small HTTP/1.1 server side for the services of a node: `POST` with `Content-Length`,
//! keep-alive, one request at a time on a connection. The JSON-RPC server of hayai-rpc
//! and the `/metrics` endpoint of hayai-metrics read their requests and write their
//! responses with it; each has its own accept loop and its own thread per connection.
//!
//! - [`read_request`] reads one request with the bounds [`MAX_HEAD`] and [`MAX_BODY`];
//!   [`write_response`] and [`write_typed_response`] write one response.
//! - [`cookie`]: the cookie authentication of a server, as Zakura.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub mod cookie;

pub use cookie::Cookie;

/// Largest request body that the server accepts (a 2 MB block in hex plus JSON framing).
pub const MAX_BODY: usize = 5 * 1024 * 1024;
/// Largest request line plus headers that the server accepts.
pub const MAX_HEAD: usize = 16 * 1024;
/// Idle time after which the server closes a keep-alive connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Request {
    pub method: String,
    pub target: String,
    /// The value of the `Authorization` header.
    pub authorization: Option<String>,
    pub body: Vec<u8>,
    pub keep_alive: bool,
}

#[derive(Debug)]
pub enum HttpError {
    BadRequest(&'static str),
    HeadTooLarge,
    TooLarge,
    Io(io::Error),
}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        HttpError::Io(e)
    }
}

/// Reads one request. Returns `None` on a clean close before any byte of a new request.
pub fn read_request(reader: &mut BufReader<TcpStream>) -> Result<Option<Request>, HttpError> {
    // The request line and the headers have one bound for their sum.
    let mut head = reader.by_ref().take(MAX_HEAD as u64);
    let mut read_line = |closed: &'static str| -> Result<Option<String>, HttpError> {
        let mut line = String::new();
        head.read_line(&mut line)?;
        match (line.ends_with('\n'), head.limit(), line.is_empty()) {
            (true, _, _) => Ok(Some(line)),
            (false, 0, _) => Err(HttpError::HeadTooLarge),
            (false, _, true) => Ok(None),
            (false, _, false) => Err(HttpError::BadRequest(closed)),
        }
    };
    let Some(line) = read_line("connection closed inside the request line")? else {
        return Ok(None);
    };
    let mut parts = line.split_whitespace();
    let method = parts
        .next()
        .ok_or(HttpError::BadRequest("missing method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or(HttpError::BadRequest("missing request target"))?
        .to_string();
    let version = parts
        .next()
        .ok_or(HttpError::BadRequest("missing HTTP version"))?;
    let mut keep_alive = version == "HTTP/1.1";
    let mut content_length = 0usize;
    let mut authorization = None;
    loop {
        let Some(header) = read_line("connection closed inside headers")? else {
            return Err(HttpError::BadRequest("connection closed inside headers"));
        };
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(HttpError::BadRequest("malformed header"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value
                .parse()
                .map_err(|_| HttpError::BadRequest("bad Content-Length"))?;
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("connection") {
            if value.eq_ignore_ascii_case("close") {
                keep_alive = false;
            } else if value.eq_ignore_ascii_case("keep-alive") {
                keep_alive = true;
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(HttpError::BadRequest(
                "chunked requests are not supported; send Content-Length",
            ));
        }
    }
    if content_length > MAX_BODY {
        return Err(HttpError::TooLarge);
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(Request {
        method,
        target,
        authorization,
        body,
        keep_alive,
    }))
}

pub fn write_response(
    w: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
    keep_alive: bool,
) -> io::Result<()> {
    write_typed_response(w, status, reason, "application/json", body, keep_alive)
}

pub fn write_typed_response(
    w: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    keep_alive: bool,
) -> io::Result<()> {
    let connection = if keep_alive { "keep-alive" } else { "close" };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n",
        body.len()
    );
    w.write_all(head.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}
