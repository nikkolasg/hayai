//! The command line of the `hayaid` binary: each form of `zakurad` that hayaid takes, each
//! form of hayaid, and the message for each command of `zakurad` that hayaid does not have.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hayaid-cli");
    std::fs::create_dir_all(&base).expect("scratch base");
    tempfile::tempdir_in(base).expect("scratch dir")
}

fn hayaid() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hayaid"));
    // No file of the user of the test: the default configuration file is absent.
    command.env("XDG_CONFIG_HOME", "/nonexistent");
    command
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = hayaid().args(args).output().expect("hayaid runs");
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("UTF-8");
    (status.code(), text(stdout), text(stderr))
}

/// A Regtest configuration without a listener, with `extra` appended. Returns its path.
fn write_config(dir: &Path, extra: &str) -> PathBuf {
    let path = dir.join("hayaid.toml");
    let text = format!(
        "[network]\nnetwork = \"Regtest\"\n[state]\ncache_dir = \"{}\"\n\
         [mining]\nminer_script = \"51\"\n{extra}",
        dir.join("data").display()
    );
    std::fs::write(&path, text).expect("config");
    path
}

/// Starts `command`, reads the log from stderr up to the line of the start, then stops the
/// process. Returns the log lines. The read ends when the process ends.
fn log_until_started(command: &mut Command) -> Vec<String> {
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hayaid starts");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut seen = Vec::new();
    for line in BufReader::new(stderr).lines() {
        let line = line.expect("log line");
        let started = line.contains("hayaid started");
        seen.push(line);
        if started {
            child.kill().expect("stop the child that this test started");
            child.wait().expect("reap");
            return seen;
        }
    }
    child.wait().expect("reap");
    panic!("hayaid exited before it logged its start: {seen:?}");
}

#[test]
fn help_and_version() {
    for args in [&["--help"][..], &["-h"], &["help"], &["start", "--help"]] {
        let (code, stdout, stderr) = run(args);
        assert_eq!((code, stderr.as_str()), (Some(0), ""), "{args:?}");
        assert!(
            stdout.starts_with("usage: hayaid [OPTIONS] [COMMAND]"),
            "{stdout}"
        );
        for command in ["start", "generate", "config", "tip-height"] {
            assert!(stdout.contains(&format!("\n  {command} ")), "{command}");
        }
    }
    for args in [["--version"], ["-V"]] {
        assert_eq!(
            run(&args),
            (
                Some(0),
                format!("hayaid {}\n", env!("CARGO_PKG_VERSION")),
                String::new()
            )
        );
    }
}

/// `generate` prints the default Mainnet configuration, as `config --network mainnet`, to
/// stdout or to the file of `-o`. Each printed configuration is a valid one.
#[test]
fn generate_and_config_print_a_default_configuration() {
    let (code, mainnet, stderr) = run(&["generate"]);
    assert_eq!((code, stderr.as_str()), (Some(0), ""));
    assert_eq!(run(&["config", "--network", "mainnet"]).1, mainnet);
    let config = hayaid::Config::parse(&mainnet).expect("the default configuration");
    assert_eq!(config.network.network, hayaid::NetworkKind::Mainnet);
    assert_eq!(config.zakura_unused, Vec::<String>::new());

    let dir = scratch();
    for flag in ["-o", "--output-file"] {
        let file = dir.path().join(format!("out{flag}.toml"));
        let (code, stdout, stderr) = run(&["generate", flag, file.to_str().expect("UTF-8")]);
        assert_eq!((code, stdout.as_str(), stderr.as_str()), (Some(0), "", ""));
        assert_eq!(std::fs::read_to_string(&file).expect("file"), mainnet);
    }
    for (name, network) in [
        ("regtest", hayaid::NetworkKind::Regtest),
        ("testnet", hayaid::NetworkKind::Testnet),
    ] {
        let (code, text, _) = run(&["config", "--network", name]);
        assert_eq!(code, Some(0));
        let config = hayaid::Config::parse(&text).expect("the default configuration");
        assert_eq!(config.network.network, network);
    }
}

/// The node starts with the configuration option before the command (the form of
/// `zakurad`), without a command, and after the command (the form of hayaid).
#[test]
fn start_takes_the_forms_of_zakurad_and_of_hayaid() {
    for form in 0..4 {
        let dir = scratch();
        let config = write_config(dir.path(), "");
        let config = config.to_str().expect("UTF-8");
        let mut command = hayaid();
        match form {
            0 => command.args(["-c", config, "start"]),
            1 => command.args(["--config", config]),
            2 => command.args(["start", "-c", config]),
            _ => command.args(["start", "--config", config]),
        };
        let log = log_until_started(&mut command);
        assert!(log.iter().any(|l| l.contains("INFO")), "{form}: {log:?}");
        assert!(log.iter().all(|l| !l.contains("DEBUG")), "{form}: {log:?}");
    }
}

/// Without `-c` the node reads `hayaid.toml` in the directory of `XDG_CONFIG_HOME`, and
/// without that variable in `.config` of the home directory, which is the rule of
/// `zakurad` on Linux. Without a file the start is an error that names the path.
#[test]
#[cfg(target_os = "linux")]
fn start_reads_the_default_configuration_file() {
    let dir = scratch();
    write_config(dir.path(), "");
    let log = log_until_started(hayaid().env("XDG_CONFIG_HOME", dir.path()).arg("start"));
    assert!(log.iter().any(|l| l.contains("hayaid started")));

    let home = scratch();
    let config_dir = home.path().join(".config");
    std::fs::create_dir(&config_dir).expect(".config");
    std::fs::rename(
        dir.path().join("hayaid.toml"),
        config_dir.join("hayaid.toml"),
    )
    .expect("move the file");
    std::fs::remove_dir_all(dir.path().join("data")).expect("empty data directory");
    let log = log_until_started(
        hayaid()
            .env_remove("XDG_CONFIG_HOME")
            .env("HOME", home.path()),
    );
    assert!(log.iter().any(|l| l.contains("hayaid started")));

    let (code, stdout, stderr) = run(&["start"]);
    assert_eq!(
        (code, stdout.as_str(), stderr.as_str()),
        (
            Some(1),
            "",
            "hayaid: no configuration file: give -c <file> or write /nonexistent/hayaid.toml\n"
        )
    );
}

/// `-v`, `--filters` and the argument of `start` set the log level in place of
/// `[tracing] filter`: the file has the level `error`, and the log has the INFO line of
/// the start.
#[test]
fn the_log_level_of_the_command_line_replaces_the_one_of_the_file() {
    for args in [
        &["-v", "start"][..],
        &["--filters", "info", "start"],
        &["start", "info"],
    ] {
        let dir = scratch();
        let config = write_config(dir.path(), "[tracing]\nfilter = \"error\"\n");
        let log = log_until_started(hayaid().arg("-c").arg(&config).args(args));
        assert!(log.iter().any(|l| l.contains("INFO")), "{args:?}: {log:?}");
    }
    let dir = scratch();
    let config = write_config(dir.path(), "");
    let (code, _, stderr) = run(&["-c", config.to_str().expect("UTF-8"), "start", "a=debug"]);
    assert_eq!(
        (code, stderr.as_str()),
        (
            Some(1),
            "hayaid: log level a=debug: the levels are error, warn, info, debug and trace\n"
        )
    );
}

/// A key of `zakurad` that hayaid does not use is a warning in the log, one line for each
/// key. With `log_file` the log is in that file and not on stderr.
#[test]
fn start_warns_about_each_zakura_key_that_it_does_not_use() {
    let dir = scratch();
    let config = write_config(
        dir.path(),
        "[health]\nlisten_addr = \"127.0.0.1:8080\"\nmin_connected_peers = 1\n",
    );
    let log = log_until_started(hayaid().arg("-c").arg(&config));
    for key in ["health.listen_addr", "health.min_connected_peers"] {
        let line = format!("{key}: Zakura setting that hayaid does not use");
        assert!(
            log.iter().any(|l| l.contains("WARN") && l.ends_with(&line)),
            "{key}: {log:?}"
        );
    }

    let dir = scratch();
    let log_file = dir.path().join("logs/hayaid.log");
    let config = write_config(
        dir.path(),
        &format!(
            "[health]\nlisten_addr = \"127.0.0.1:8080\"\n[tracing]\nlog_file = \"{}\"\n\
             [rpc]\nlisten_addr = \"127.0.0.1:0\"\n",
            log_file.display()
        ),
    );
    let mut child = hayaid()
        .arg("-c")
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hayaid starts");
    // The node writes the cookie file after the line of the warning. The wait for the
    // file has a bound.
    let cookie = dir.path().join("data/.cookie");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !cookie.exists() {
        assert!(std::time::Instant::now() < deadline, "no cookie file");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().expect("stop the child that this test started");
    let output = child.wait_with_output().expect("reap");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let log = std::fs::read_to_string(&log_file).expect("log file");
    assert!(
        log.contains("health.listen_addr: Zakura setting that hayaid does not use"),
        "{log}"
    );
    assert!(!log.contains('\u{1b}'), "ANSI escape in {log}");
}

/// `tip-height` prints the height of the state of a data directory: the directory of
/// `--cache-dir`, or the one of the configuration file.
#[test]
fn tip_height_prints_the_height_of_the_state_on_disk() {
    let dir = scratch();
    let config = write_config(dir.path(), "");
    let config = config.to_str().expect("UTF-8");
    // The first start writes the start record of the state log: the genesis block.
    log_until_started(hayaid().args(["-c", config]));
    let data = dir.path().join("data");
    let data = data.to_str().expect("UTF-8");
    let ok = (Some(0), "0\n".to_string(), String::new());
    assert_eq!(
        run(&["tip-height", "--network", "regtest", "--cache-dir", data]),
        ok
    );
    assert_eq!(run(&["tip-height", "-n", "Regtest", "-c", data]), ok);
    assert_eq!(run(&["-c", config, "tip-height", "-n", "regtest"]), ok);

    let (code, stdout, stderr) = run(&["tip-height", "-n", "mainnet", "-c", data]);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert_eq!(
        stderr,
        format!("hayaid: {data} belongs to a regtest node, not to a mainnet node\n")
    );
    let empty = dir.path().join("empty");
    let (code, _, stderr) = run(&[
        "tip-height",
        "-n",
        "regtest",
        "-c",
        empty.to_str().expect("UTF-8"),
    ]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("state.log"), "{stderr}");
}

/// Each other command of `zakurad` is one line that names the command, with the exit
/// status 1. An unknown command is an error with the usage text.
#[test]
fn the_other_commands_of_zakurad_are_named_in_an_error() {
    for command in [
        "audit-historical-treestates",
        "verify-historical-treestates",
        "prune-state",
        "rollback-state",
    ] {
        assert_eq!(
            run(&[command, "--cache-dir", "x"]),
            (
                Some(1),
                String::new(),
                format!("hayaid: {command}: hayaid does not have this command of zakurad\n")
            )
        );
    }
    assert_eq!(
        run(&["start", "--zcashd-compat"]),
        (
            Some(1),
            String::new(),
            "hayaid: start --zcashd-compat: hayaid does not have the zcashd-compat mode of zakurad\n"
                .to_string()
        )
    );
    let (code, stdout, stderr) = run(&["run"]);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(
        stderr.starts_with("hayaid: unknown command run\nusage:"),
        "{stderr}"
    );
}
