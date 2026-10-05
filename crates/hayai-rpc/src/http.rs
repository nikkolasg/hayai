//! A small HTTP/1.1 server for JSON-RPC: `POST` with `Content-Length`, keep-alive, one
//! thread per connection (a long poll blocks only its own connection).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use crate::rpc::Rpc;

/// Largest request body that the server accepts (a 2 MB block in hex plus JSON framing).
pub const MAX_BODY: usize = 5 * 1024 * 1024;
/// Idle time after which the server closes a keep-alive connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct HttpServer {
    addr: SocketAddr,
    stopped: AtomicBool,
}

impl HttpServer {
    /// Binds `addr` and serves `rpc` on a background thread.
    pub fn serve(addr: impl ToSocketAddrs, rpc: Arc<Rpc>) -> io::Result<Arc<Self>> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let server = Arc::new(Self {
            addr,
            stopped: AtomicBool::new(false),
        });
        let weak: Weak<Self> = Arc::downgrade(&server);
        thread::Builder::new()
            .name(format!("rpc-accept-{addr}"))
            .spawn(move || {
                for stream in listener.incoming() {
                    let Some(server) = weak.upgrade() else {
                        break;
                    };
                    if server.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let rpc = rpc.clone();
                    let spawned = thread::Builder::new()
                        .name("rpc-conn".into())
                        .spawn(move || {
                            if let Err(e) = serve_connection(stream, &rpc) {
                                tracing::debug!(error = %e, "rpc connection ended");
                            }
                        });
                    if let Err(e) = spawned {
                        tracing::warn!(error = %e, "cannot spawn rpc connection thread");
                    }
                }
            })?;
        Ok(server)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops the accept loop. Open connections finish their current request.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) target: String,
    pub(crate) body: Vec<u8>,
    pub(crate) keep_alive: bool,
}

#[derive(Debug)]
pub(crate) enum HttpError {
    BadRequest(&'static str),
    TooLarge,
    Io(io::Error),
}

impl From<io::Error> for HttpError {
    fn from(e: io::Error) -> Self {
        HttpError::Io(e)
    }
}

fn serve_connection(stream: TcpStream, rpc: &Rpc) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let request = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(HttpError::BadRequest(why)) => {
                write_response(&mut writer, 400, "Bad Request", why.as_bytes(), false)?;
                return Ok(());
            }
            Err(HttpError::TooLarge) => {
                write_response(&mut writer, 413, "Payload Too Large", b"", false)?;
                return Ok(());
            }
            Err(HttpError::Io(e)) => return Err(e),
        };
        if request.method != "POST" {
            write_response(
                &mut writer,
                405,
                "Method Not Allowed",
                b"JSON-RPC requests are POST",
                request.keep_alive,
            )?;
            if !request.keep_alive {
                return Ok(());
            }
            continue;
        }
        let body = rpc.handle(&request.body);
        write_response(&mut writer, 200, "OK", &body, request.keep_alive)?;
        if !request.keep_alive {
            return Ok(());
        }
    }
}

/// Reads one request. Returns `None` on a clean close before any byte of a new request.
pub(crate) fn read_request(
    reader: &mut BufReader<TcpStream>,
) -> Result<Option<Request>, HttpError> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
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
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Err(HttpError::BadRequest("connection closed inside headers"));
        }
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
        body,
        keep_alive,
    }))
}

fn write_response(
    w: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &[u8],
    keep_alive: bool,
) -> io::Result<()> {
    write_typed_response(w, status, reason, "application/json", body, keep_alive)
}

pub(crate) fn write_typed_response(
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
