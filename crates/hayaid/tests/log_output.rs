//! The log of `hayaid start` has no ANSI escape sequences when stderr is not a terminal.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn the_log_has_no_ansi_codes_when_stderr_is_a_pipe() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hayaid-tests");
    std::fs::create_dir_all(&base).expect("scratch base");
    let dir = tempfile::tempdir_in(base).expect("scratch dir");
    let config = dir.path().join("hayaid.toml");
    std::fs::write(
        &config,
        format!(
            "[network]\nnetwork = \"Regtest\"\n[state]\ncache_dir = \"{}\"\n[mining]\nminer_script = \"51\"\n",
            dir.path().join("data").display()
        ),
    )
    .expect("config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hayaid"))
        .args(["start", "-c"])
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hayaid starts");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut lines = BufReader::new(stderr).lines();
    let mut seen = Vec::new();
    loop {
        let Some(line) = lines.next() else {
            panic!("hayaid exited before it logged its start: {seen:?}");
        };
        let line = line.expect("log line");
        let started = line.contains("hayaid started");
        seen.push(line);
        if started {
            break;
        }
    }
    child.kill().expect("stop the child that this test started");
    child.wait().expect("reap");
    assert!(seen.iter().any(|l| l.contains("INFO")), "{seen:?}");
    assert!(
        seen.iter().all(|l| !l.contains('\u{1b}')),
        "ANSI escape in {seen:?}"
    );
}

/// The `stop` method of the RPC server ends a Regtest `hayaid` through the shutdown of
/// the node, with the exit status 0. The caller reads the credentials from the cookie file
/// in the data directory. The shutdown removes that file.
#[test]
fn the_stop_method_ends_the_process_cleanly() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hayaid-tests");
    std::fs::create_dir_all(&base).expect("scratch base");
    let dir = tempfile::tempdir_in(base).expect("scratch dir");
    let config = dir.path().join("hayaid.toml");
    std::fs::write(
        &config,
        format!(
            "[network]\nnetwork = \"Regtest\"\ninitial_testnet_peers = []\n[state]\ncache_dir = \"{}\"\n[rpc]\nlisten_addr = \"127.0.0.1:0\"\n[mining]\nminer_script = \"51\"\n",
            dir.path().join("data").display()
        ),
    )
    .expect("config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hayaid"))
        .args(["start", "-c"])
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hayaid starts");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut lines = BufReader::new(stderr).lines();
    // The start line has the RPC address: `rpc=Some(127.0.0.1:PORT)`.
    let addr = loop {
        let Some(Ok(line)) = lines.next() else {
            child.kill().expect("stop the child that this test started");
            panic!("hayaid exited before it logged its start");
        };
        if let Some((_, rest)) = line.split_once("rpc=Some(") {
            let addr = rest.split(')').next().expect("an address");
            break addr.parse::<std::net::SocketAddr>().expect("an address");
        }
    };
    // The node wrote the cookie file to the data directory, for its owner only.
    let cookie = dir.path().join("data").join(".cookie");
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cookie)
            .expect("cookie file")
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
    }
    let authorization = hayai_rpc::cookie::authorization(&cookie).expect("cookie file");
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"stop","params":[]}"#;
    let post = |authorization: &str| {
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("timeout");
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: x\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("write");
        let mut answer = String::new();
        stream.read_to_string(&mut answer).expect("answer");
        answer
    };
    // Without the credentials the method does not run: the next request has an answer.
    let answer = post("");
    assert!(answer.starts_with("HTTP/1.1 401 "), "{answer}");
    let answer = post(&format!("Authorization: {authorization}\r\n"));
    assert!(answer.starts_with("HTTP/1.1 200 "), "{answer}");
    assert!(answer.contains("hayaid server stopping"), "{answer}");
    // The log goes on to the end of the process. The process has 60 s to end.
    let rest = std::thread::spawn(move || lines.map_while(Result::ok).collect::<Vec<_>>());
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().expect("stop the child that this test started");
            panic!("hayaid runs 60 s after `stop`");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
    assert!(
        !cookie.exists(),
        "the clean shutdown removes the cookie file"
    );
    let log = rest.join().expect("the log");
    assert!(
        log.iter()
            .any(|l| l.contains("shutting down") && l.contains("stop method")),
        "{log:?}"
    );
}
