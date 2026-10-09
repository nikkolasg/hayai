//! The HTTP/1.1 front end of the JSON-RPC server, on `hayai_http`: one thread per
//! connection (a long poll blocks only its own connection), the cookie check, `POST` only.

use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use hayai_http::cookie::{self, Cookie};
use hayai_http::{read_request, write_response, HttpError, IDLE_TIMEOUT};

use crate::rpc::Rpc;

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
