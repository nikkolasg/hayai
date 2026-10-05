//! The command line of `hayaid`. It takes the forms of `zakurad` for `start`, `generate`
//! and `tip-height`, and its own forms `hayaid start -c <config.toml>` and
//! `hayaid config --network <name>`. `docs/zakura-compat.md` has the table.
//!
//! `start` runs the node until SIGINT, SIGTERM or, on Regtest, the `stop` method of the RPC
//! server.

use std::fs::File;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use crossbeam_channel::{bounded, select};
use hayaid::config::StateSection;
use hayaid::node::stored_tip;
use hayaid::{default_toml, Config, NetworkKind, Node};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

const USAGE: &str = "usage: hayaid [OPTIONS] [COMMAND]

commands:
  start [FILTER]                        run the node (the default command)
  generate [-o <file>]                  print a default configuration (Mainnet)
  config --network <network>            print the default configuration of a network
  tip-height -n <network> [-c <dir>]    print the height of the best tip on disk
  help                                  print this text

options:
  -c, --config <file>   the configuration file; the default is hayaid.toml in
                        $XDG_CONFIG_HOME or $HOME/.config
  -v, --verbose         the log level debug
      --filters <level> the log level: error, warn, info, debug or trace
  -h, --help            print this text
  -V, --version         print the version

start options:
  -c, --config <file>   as the option before the command

generate options:
  -o, --output-file <file>  write the configuration to this file

tip-height options:
  -c, --cache-dir <dir>  the data directory; the default is cache_dir of [state]
  -n, --network <name>   mainnet, testnet or regtest";

/// The commands of `zakurad` that hayaid does not have.
const ZAKURAD_ONLY: [&str; 4] = [
    "audit-historical-treestates",
    "verify-historical-treestates",
    "prune-state",
    "rollback-state",
];

#[derive(Debug, PartialEq)]
enum Command {
    Start {
        config: Option<PathBuf>,
        /// The log level of the command line: it replaces `[tracing] filter`.
        filter: Option<String>,
    },
    /// `generate`, or `config --network`.
    Generate {
        network: NetworkKind,
        output: Option<PathBuf>,
    },
    TipHeight {
        config: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
        network: NetworkKind,
    },
    Help,
    Version,
}

fn network(name: &str) -> Result<NetworkKind, String> {
    match name.to_lowercase().as_str() {
        "regtest" => Ok(NetworkKind::Regtest),
        "testnet" => Ok(NetworkKind::Testnet),
        "mainnet" => Ok(NetworkKind::Mainnet),
        _ => Err(format!("unknown network {name}")),
    }
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    fn value<'a>(args: &mut impl Iterator<Item = &'a str>, option: &str) -> Result<String, String> {
        args.next()
            .map(str::to_string)
            .ok_or(format!("{option} needs a value"))
    }
    let mut args = args.iter().map(String::as_str);
    let mut config = None;
    let mut filter = None;
    let command = loop {
        match args.next() {
            None => break "start",
            Some("-c" | "--config") => config = Some(PathBuf::from(value(&mut args, "--config")?)),
            Some("-v" | "--verbose") => filter = filter.or(Some("debug".to_string())),
            Some("--filters") => filter = Some(value(&mut args, "--filters")?),
            Some("-h" | "--help" | "help") => return Ok(Command::Help),
            Some("-V" | "--version") => return Ok(Command::Version),
            Some(option) if option.starts_with('-') => {
                return Err(format!("unknown option {option}\n{USAGE}"))
            }
            Some(command) => break command,
        }
    };
    let unknown = |option: &str| Err(format!("{command}: unknown argument {option}\n{USAGE}"));
    match command {
        "start" => {
            while let Some(arg) = args.next() {
                match arg {
                    "-c" | "--config" => config = Some(PathBuf::from(value(&mut args, arg)?)),
                    "-h" | "--help" => return Ok(Command::Help),
                    "--zcashd-compat" | "--unsafe-low-specs" => {
                        return Err(format!(
                            "start {arg}: hayaid does not have the zcashd-compat mode of zakurad"
                        ))
                    }
                    option if option.starts_with('-') => return unknown(option),
                    level => filter = Some(level.to_string()),
                }
            }
            Ok(Command::Start { config, filter })
        }
        "generate" => {
            let mut output = None;
            while let Some(arg) = args.next() {
                match arg {
                    "-o" | "--output-file" => output = Some(PathBuf::from(value(&mut args, arg)?)),
                    "-h" | "--help" => return Ok(Command::Help),
                    other => return unknown(other),
                }
            }
            Ok(Command::Generate {
                network: NetworkKind::Mainnet,
                output,
            })
        }
        "config" => match (args.next(), args.next(), args.next()) {
            (Some("--network"), Some(name), None) => Ok(Command::Generate {
                network: network(name)?,
                output: None,
            }),
            _ => Err(USAGE.to_string()),
        },
        "tip-height" => {
            let (mut cache_dir, mut name) = (None, None);
            while let Some(arg) = args.next() {
                match arg {
                    "-c" | "--cache-dir" => cache_dir = Some(PathBuf::from(value(&mut args, arg)?)),
                    "-n" | "--network" => name = Some(value(&mut args, arg)?),
                    "-h" | "--help" => return Ok(Command::Help),
                    other => return unknown(other),
                }
            }
            let Some(name) = name else {
                return Err("tip-height needs --network <name>".to_string());
            };
            Ok(Command::TipHeight {
                config,
                cache_dir,
                network: network(&name)?,
            })
        }
        other if ZAKURAD_ONLY.contains(&other) => Err(format!(
            "{other}: hayaid does not have this command of zakurad"
        )),
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

/// The directory of the default configuration file, by the rule of `zakurad`
/// (`dirs::preference_dir`): `$XDG_CONFIG_HOME` or `$HOME/.config` on Linux,
/// `$HOME/Library/Preferences` on macOS.
fn preference_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    match cfg!(target_os = "macos") {
        true => absolute("HOME").map(|home| home.join("Library/Preferences")),
        false => {
            absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|h| h.join(".config")))
        }
    }
}

/// Name of the default configuration file in [`preference_dir`].
const CONFIG_FILE: &str = "hayaid.toml";

/// The configuration file: the file of the command line, or the default file when it
/// exists.
fn config_path(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    let default = preference_dir().map(|dir| dir.join(CONFIG_FILE));
    match default {
        Some(path) if path.exists() => Ok(path),
        Some(path) => Err(format!(
            "no configuration file: give -c <file> or write {}",
            path.display()
        )),
        None => Err("no configuration file: give -c <file>".to_string()),
    }
}

fn level(name: &str) -> Result<LevelFilter, String> {
    match name {
        "error" => Ok(LevelFilter::ERROR),
        "warn" => Ok(LevelFilter::WARN),
        "info" => Ok(LevelFilter::INFO),
        "debug" => Ok(LevelFilter::DEBUG),
        "trace" => Ok(LevelFilter::TRACE),
        other => Err(format!(
            "log level {other}: the levels are error, warn, info, debug and trace"
        )),
    }
}

fn start(config: Option<PathBuf>, filter: Option<String>) -> Result<(), String> {
    let path = config_path(config)?;
    let config = Config::load(&path).map_err(|e| e.to_string())?;
    let tracing = &config.tracing;
    let level = level(filter.as_deref().unwrap_or(&tracing.filter))?;
    // The log goes to stderr, or to `log_file`. ANSI colour codes only on a terminal:
    // docker logs, journald and a file show the escape sequences as text.
    let (writer, terminal) = match &tracing.log_file {
        Some(file) => {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let file = File::options()
                .append(true)
                .create(true)
                .open(file)
                .map_err(|e| format!("log_file {}: {e}", file.display()))?;
            (BoxMakeWriter::new(Mutex::new(file)), false)
        }
        None => (
            BoxMakeWriter::new(std::io::stderr),
            std::io::stderr().is_terminal(),
        ),
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(writer)
        .with_ansi(tracing.force_use_color || (tracing.use_color && terminal))
        .init();
    for line in &config.zakura_unused {
        tracing::warn!("{line}");
    }
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

fn generate(network: NetworkKind, output: Option<PathBuf>) -> Result<(), String> {
    let text = default_toml(network);
    match output {
        Some(path) => std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display())),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

/// Prints the height of the best tip that the data directory holds: the tip that a
/// restart of the node resumes at (`hayaid::node::stored_tip`). The command starts no
/// node and writes nothing. The directory is `cache_dir`, or `[state] cache_dir` of the
/// configuration file, or the default of that key when there is no file.
///
/// An error has the text of `zakurad`. `zakurad` logs it and exits with the status 0.
/// hayaid exits with the status 1, as for each other error.
fn tip_height(
    config: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    network: NetworkKind,
) -> Result<(), String> {
    let dir = match (cache_dir, config_path(config)) {
        (Some(dir), _) => dir,
        (None, Ok(path)) => {
            Config::load(&path)
                .map_err(|e| e.to_string())?
                .state
                .cache_dir
        }
        (None, Err(_)) => StateSection::default().cache_dir,
    };
    let failed = |cause: String| format!("Failed to read chain tip height from state: {cause}");
    let (stored, height) = stored_tip(&dir).map_err(|e| failed(e.to_string()))?;
    if stored != network {
        return Err(failed(format!(
            "{} belongs to a {} node, not to a {} node",
            dir.display(),
            stored.name(),
            network.name()
        )));
    }
    println!("{height}");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match parse_args(&args) {
        Ok(Command::Start { config, filter }) => start(config, filter),
        Ok(Command::Generate { network, output }) => generate(network, output),
        Ok(Command::TipHeight {
            config,
            cache_dir,
            network,
        }) => tip_height(config, cache_dir, network),
        Ok(Command::Help) => {
            println!("{USAGE}");
            Ok(())
        }
        Ok(Command::Version) => {
            println!("hayaid {}", env!("CARGO_PKG_VERSION"));
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

    fn parse(s: &[&str]) -> Result<Command, String> {
        parse_args(&s.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    fn start_of(config: Option<&str>, filter: Option<&str>) -> Result<Command, String> {
        Ok(Command::Start {
            config: config.map(PathBuf::from),
            filter: filter.map(str::to_string),
        })
    }

    #[test]
    fn the_forms_of_zakurad_and_of_hayaid_parse() {
        // start: the default command, the option before or after the command.
        assert_eq!(parse(&[]), start_of(None, None));
        assert_eq!(parse(&["start"]), start_of(None, None));
        assert_eq!(parse(&["-c", "x.toml"]), start_of(Some("x.toml"), None));
        assert_eq!(
            parse(&["-c", "x.toml", "start"]),
            start_of(Some("x.toml"), None)
        );
        assert_eq!(
            parse(&["--config", "x.toml", "start"]),
            start_of(Some("x.toml"), None)
        );
        assert_eq!(
            parse(&["start", "-c", "x.toml"]),
            start_of(Some("x.toml"), None)
        );
        assert_eq!(
            parse(&["start", "--config", "x.toml"]),
            start_of(Some("x.toml"), None)
        );
        // The log level: -v, --filters, and the argument of start. The last one wins.
        assert_eq!(parse(&["-v", "start"]), start_of(None, Some("debug")));
        assert_eq!(
            parse(&["--filters", "warn", "-v"]),
            start_of(None, Some("warn"))
        );
        assert_eq!(
            parse(&["-v", "start", "trace"]),
            start_of(None, Some("trace"))
        );

        let generate = |network, output: Option<&str>| {
            Ok(Command::Generate {
                network,
                output: output.map(PathBuf::from),
            })
        };
        assert_eq!(parse(&["generate"]), generate(NetworkKind::Mainnet, None));
        for flag in ["-o", "--output-file"] {
            assert_eq!(
                parse(&["generate", flag, "out.toml"]),
                generate(NetworkKind::Mainnet, Some("out.toml"))
            );
        }
        for (name, network) in [
            ("regtest", NetworkKind::Regtest),
            ("testnet", NetworkKind::Testnet),
            ("mainnet", NetworkKind::Mainnet),
        ] {
            assert_eq!(
                parse(&["config", "--network", name]),
                generate(network, None)
            );
        }

        assert_eq!(
            parse(&["-c", "x.toml", "tip-height", "--network", "Testnet"]),
            Ok(Command::TipHeight {
                config: Some(PathBuf::from("x.toml")),
                cache_dir: None,
                network: NetworkKind::Testnet,
            })
        );
        assert_eq!(
            parse(&["tip-height", "-n", "mainnet", "-c", "dir"]),
            Ok(Command::TipHeight {
                config: None,
                cache_dir: Some(PathBuf::from("dir")),
                network: NetworkKind::Mainnet,
            })
        );
        assert_eq!(
            parse(&["tip-height", "--cache-dir", "dir", "-n", "regtest"]),
            Ok(Command::TipHeight {
                config: None,
                cache_dir: Some(PathBuf::from("dir")),
                network: NetworkKind::Regtest,
            })
        );

        for help in [
            &["--help"][..],
            &["-h"],
            &["help"],
            &["start", "--help"],
            &["generate", "-h"],
            &["tip-height", "--help"],
        ] {
            assert_eq!(parse(help), Ok(Command::Help));
        }
        for version in ["--version", "-V"] {
            assert_eq!(parse(&[version]), Ok(Command::Version));
        }
    }

    #[test]
    fn a_wrong_command_line_is_an_error_that_names_its_cause() {
        for command in ZAKURAD_ONLY {
            assert_eq!(
                parse(&[command, "--cache-dir", "x"]),
                Err(format!(
                    "{command}: hayaid does not have this command of zakurad"
                ))
            );
        }
        for flag in ["--zcashd-compat", "--unsafe-low-specs"] {
            assert_eq!(
                parse(&["start", flag]),
                Err(format!(
                    "start {flag}: hayaid does not have the zcashd-compat mode of zakurad"
                ))
            );
        }
        assert_eq!(
            parse(&["tip-height"]),
            Err("tip-height needs --network <name>".to_string())
        );
        assert_eq!(
            parse(&["config", "--network", "signet"]),
            Err("unknown network signet".to_string())
        );
        assert_eq!(parse(&["-c"]), Err("--config needs a value".to_string()));
        for (line, cause) in [
            (&["run"][..], "unknown command run"),
            (&["--fast"], "unknown option --fast"),
            (&["start", "--fast"], "start: unknown argument --fast"),
            (&["generate", "x"], "generate: unknown argument x"),
        ] {
            let Err(e) = parse(line) else {
                panic!("{line:?} parsed");
            };
            assert!(e.starts_with(cause), "{e}");
        }
        let Err(e) = level("zakura_network=debug") else {
            panic!("a filter directive is not a level");
        };
        assert!(e.contains("log level zakura_network=debug"), "{e}");
    }
}
