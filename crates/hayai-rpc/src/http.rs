//! A small HTTP/1.1 server for JSON-RPC: `POST` with `Content-Length`, keep-alive, one
//! thread per connection (a long poll blocks only its own connection).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use crate::cookie::{self, Cookie};
use crate::rpc::Rpc;

/// Largest request body that the server accepts (a 2 MB block in hex plus JSON framing).
pub const MAX_BODY: usize = 5 * 1024 * 1024;
/// Largest request line plus headers that the server accepts.
pub const MAX_HEAD: usize = 16 * 1024;
/// Idle time after which the server closes a keep-alive connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct HttpServer {
    addr: SocketAddr,
    stopped: AtomicBool,
    /// The cookie file of this run. The shutdown removes it.
    cookie: Mutex<Option<Cookie>>,
}

impl HttpServer {
    /// Binds `addr` and serves `rpc` on a background thread. With a cookie, each request
    /// must have the credentials of the cookie file (`crate::cookie`). Without a cookie,
    /// the server has no authentication.
    pub fn serve(
        addr: impl ToSocketAddrs,
        rpc: Arc<Rpc>,
        cookie: Option<Cookie>,
    ) -> io::Result<Arc<Self>> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let secret = cookie.as_ref().map(Cookie::secret);
        let server = Arc::new(Self {
            addr,
            stopped: AtomicBool::new(false),
            cookie: Mutex::new(cookie),
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
                    let secret = secret.clone();
                    let spawned = thread::Builder::new()
                        .name("rpc-conn".into())
                        .spawn(move || {
                            if let Err(e) = serve_connection(stream, &rpc, secret.as_deref()) {
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

    /// Stops the accept loop and removes the cookie file. Open connections finish their
    /// current request.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        // No code can panic while it holds this lock.
        self.cookie.lock().expect("cookie lock").take();
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) target: String,
    /// The value of the `Authorization` header.
    pub(crate) authorization: Option<String>,
    pub(crate) body: Vec<u8>,
    pub(crate) keep_alive: bool,
}

#[derive(Debug)]
pub(crate) enum HttpError {
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

fn serve_connection(stream: TcpStream, rpc: &Rpc, secret: Option<&str>) -> io::Result<()> {
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
            Err(HttpError::HeadTooLarge) => {
                write_response(
                    &mut writer,
                    431,
                    "Request Header Fields Too Large",
                    b"",
                    false,
                )?;
                return Ok(());
            }
            Err(HttpError::TooLarge) => {
                write_response(&mut writer, 413, "Payload Too Large", b"", false)?;
                return Ok(());
            }
            Err(HttpError::Io(e)) => return Err(e),
        };
        if let Some(secret) = secret {
            if !cookie::accepts(secret, request.authorization.as_deref()) {
                // The status and the header of zcashd. The server closes the connection.
                writer.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"jsonrpc\"\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                )?;
                return writer.flush();
            }
        }
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
