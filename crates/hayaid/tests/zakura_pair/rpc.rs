//! A JSON-RPC client over one HTTP request for each call.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

/// The cookie file of the RPC server at an address. A server without an entry has no
/// authentication.
static COOKIES: Mutex<BTreeMap<SocketAddr, PathBuf>> = Mutex::new(BTreeMap::new());

/// Each later call to `addr` sends the credentials of the cookie file `file`.
pub fn set_cookie(addr: SocketAddr, file: PathBuf) {
    COOKIES.lock().expect("cookie table").insert(addr, file);
}

/// The `Authorization` header line for `addr`. A node writes a new cookie file at each
/// start, so each request reads the file.
fn authorization(addr: SocketAddr) -> Result<String, String> {
    match COOKIES.lock().expect("cookie table").get(&addr) {
        Some(file) => hayai_rpc::cookie::authorization(file)
            .map(|value| format!("Authorization: {value}\r\n"))
            .map_err(|e| format!("cookie file {}: {e}", file.display())),
        None => Ok(String::new()),
    }
}

/// One call. `Err` holds the `error` object of the answer or the transport error.
pub fn call(addr: SocketAddr, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        authorization(addr)?,
        body.len()
    );
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("{method}: connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(300)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("{method}: write: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("{method}: read: {e}"))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| format!("{method}: no HTTP header end"))?;
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let mut payload = raw[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        payload = dechunk(&payload).ok_or_else(|| format!("{method}: bad chunked body"))?;
    }
    let answer: Value = serde_json::from_slice(&payload).map_err(|e| {
        format!(
            "{method}: answer is not JSON ({e}): {}",
            String::from_utf8_lossy(&payload)
        )
    })?;
    match &answer["error"] {
        Value::Null => Ok(answer["result"].clone()),
        error => Err(error.to_string()),
    }
}

/// The HTTP status of one call without credentials. `None`: the server closed the
/// connection without an answer.
pub fn status_without_cookie(
    addr: SocketAddr,
    method: &str,
    params: Value,
) -> Result<Option<u16>, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .map_err(|e| format!("{method}: connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("{method}: write: {e}"))?;
    let mut raw = Vec::new();
    // A reset of the connection is an end without an answer, too.
    let _ = stream.read_to_end(&mut raw);
    let head = String::from_utf8_lossy(&raw);
    match head.split_whitespace().nth(1) {
        Some(status) => status.parse().map(Some).map_err(|e| format!("{head}: {e}")),
        None => Ok(None),
    }
}

fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = data.windows(2).position(|w| w == b"\r\n")?;
        let len = usize::from_str_radix(std::str::from_utf8(&data[..end]).ok()?.trim(), 16).ok()?;
        data = &data[end + 2..];
        if len == 0 {
            return Some(out);
        }
        out.extend_from_slice(data.get(..len)?);
        data = data.get(len + 2..)?;
    }
}

/// A plain HTTP GET, for `/metrics`.
pub fn get(addr: SocketAddr, path: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    Ok(raw)
}
