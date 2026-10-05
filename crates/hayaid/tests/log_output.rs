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
            "[network]\nnetwork = \"regtest\"\n[state]\ndata_dir = \"{}\"\n[mining]\nminer_script = \"51\"\n",
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
