//! A JSON-RPC client over one HTTP request for each call.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::{json, Value};

/// One call. `Err` holds the `error` object of the answer or the transport error.
pub fn call(addr: SocketAddr, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
