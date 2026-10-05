//! `hayaid start -c <config.toml>` runs the node until SIGINT, SIGTERM or, on Regtest, the
//! `stop` method of the RPC server.
//! `hayaid config --network regtest|testnet|mainnet` prints a commented default configuration.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use crossbeam_channel::{bounded, select};
use hayaid::{default_toml, Config, NetworkKind, Node};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::level_filters::LevelFilter;

const USAGE: &str = "usage:
  hayaid start -c <config.toml>
  hayaid config --network regtest|testnet|mainnet";

enum Command {
    Start(PathBuf),
    Config(NetworkKind),
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    match args {
        [cmd, flag, path] if cmd == "start" && (flag == "-c" || flag == "--config") => {
            Ok(Command::Start(PathBuf::from(path)))
        }
        [cmd, flag, network] if cmd == "config" && flag == "--network" => match network.as_str() {
            "regtest" => Ok(Command::Config(NetworkKind::Regtest)),
            "testnet" => Ok(Command::Config(NetworkKind::Testnet)),
            "mainnet" => Ok(Command::Config(NetworkKind::Mainnet)),
            other => Err(format!("unknown network {other}")),
        },
        _ => Err(USAGE.to_string()),
    }
}

fn level(name: &str) -> LevelFilter {
    match name {
        "error" => LevelFilter::ERROR,
        "warn" => LevelFilter::WARN,
        "debug" => LevelFilter::DEBUG,
        "trace" => LevelFilter::TRACE,
        _ => LevelFilter::INFO,
    }
}

fn start(path: PathBuf) -> Result<(), String> {
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    // Logs go to stderr. ANSI colour codes only when stderr is a terminal: docker logs and
    // journald show the escape sequences as text.
    tracing_subscriber::fmt()
        .with_max_level(level(&config.log.level))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
    let (signal_tx, signal_rx) = bounded(1);
    let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        if let Some(signal) = signals.forever().next() {
            let _ = signal_tx.send(signal);
        }
    });
    let node = Node::start(&config).map_err(|e| e.to_string())?;
    tracing::info!(
        network = config.network.network.name(),
        p2p = ?node.p2p_addr,
        rpc = ?node.rpc_addr,
        metrics = ?node.metrics_addr,
        cookie = ?node.rpc_cookie,
        "hayaid started"
    );
    let stopped_by = select! {
        recv(signal_rx) -> signal => format!("signal {}", signal.unwrap_or(0)),
        recv(node.stop_requested()) -> _ => "stop method of the RPC server".to_string(),
        recv(node.done()) -> result => match result {
            Ok(Err(e)) => format!("fatal error: {e}"),
            _ => "driver exit".to_string(),
        },
    };
    tracing::info!(reason = %stopped_by, "shutting down");
    node.shutdown().map_err(|e| e.to_string())?;
    if stopped_by.starts_with("fatal") {
        return Err(stopped_by);
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse_args(&args) {
        Ok(Command::Start(path)) => start(path),
        Ok(Command::Config(network)) => {
            print!("{}", default_toml(network));
            Ok(())
        }
        Err(e) => Err(e),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hayaid: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn subcommands_parse() {
        let Ok(Command::Start(p)) = parse_args(&args(&["start", "-c", "x.toml"])) else {
            panic!("start");
        };
        assert_eq!(p, PathBuf::from("x.toml"));
        let Ok(Command::Config(NetworkKind::Testnet)) =
            parse_args(&args(&["config", "--network", "testnet"]))
        else {
            panic!("config");
        };
        let Ok(Command::Config(NetworkKind::Mainnet)) =
            parse_args(&args(&["config", "--network", "mainnet"]))
        else {
            panic!("config mainnet");
        };
        let Err(_) = parse_args(&args(&["config", "--network", "signet"])) else {
            panic!("unknown network");
        };
        let Err(_) = parse_args(&args(&["run"])) else {
            panic!("unknown subcommand");
        };
    }
}
