//! The Regtest pair of one hayaid and one zakurad on loopback (`docs/regtest-pair.md`).
//!
//! Usage:
//!
//! ```text
//! cargo test --release -p hayaid --test zakura_pair -- --zakurad PATH [--scenario LIST]
//!     [--hayaid PATH] [--work DIR] [--port-base N] [--minutes N] [--hayaid-log LEVEL]
//! ```
//!
//! `LIST` is a comma-separated list of `a`, `b`, `c`, `d`, `e`, `g`, `nu61`, `rpc`, `nu7`, or
//! `all`.
//! `all` does not have `nu7`: that scenario needs the hayaid and the test binary of the
//! Zakura backend.
//! Without `--zakurad` the binary runs nothing: `cargo test --workspace` has no Zakura
//! node.

mod nodes;
mod rpc;
mod rpc_compare;
mod scenarios;
mod txs;

use std::path::PathBuf;

use nodes::{Pair, Ports, Setup};

/// One line of the report.
pub struct Row {
    pub scenario: String,
    pub check: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Default)]
pub struct Report {
    pub scenario: String,
    pub rows: Vec<Row>,
}

impl Report {
    pub fn check(&mut self, check: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!(
            "[{}] {} {check}: {detail}",
            self.scenario,
            if ok { "ok  " } else { "FAIL" }
        );
        self.rows.push(Row {
            scenario: self.scenario.clone(),
            check: check.to_string(),
            ok,
            detail,
        });
    }

    /// Records the result of a step. Returns whether it passed.
    pub fn result(&mut self, check: &str, result: Result<String, String>) -> bool {
        match result {
            Ok(detail) => {
                self.check(check, true, detail);
                true
            }
            Err(detail) => {
                self.check(check, false, detail);
                false
            }
        }
    }
}

pub struct Args {
    pub zakurad: PathBuf,
    pub hayaid: PathBuf,
    pub work: PathBuf,
    pub port_base: u16,
    pub minutes: u64,
    pub repo: PathBuf,
    pub hayai_log: String,
}

impl Args {
    /// A pair without funding streams, with a lockbox disbursement of `disbursement`
    /// zatoshis in the NU6.1 activation block.
    pub fn pair(&self, name: &str, disbursement: Option<u64>) -> Result<Pair, String> {
        self.pair_with(name, disbursement, false)
    }

    pub fn pair_with(
        &self,
        name: &str,
        disbursement: Option<u64>,
        funding_streams: bool,
    ) -> Result<Pair, String> {
        Pair::new(Setup {
            hayaid_bin: self.hayaid.clone(),
            zakurad_bin: self.zakurad.clone(),
            dir: self.work.join(name),
            ports: Ports::from_base(self.port_base),
            disbursement,
            funding_streams,
            hayai_log: self.hayai_log.clone(),
        })
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut zakurad = None;
    let mut hayaid = PathBuf::from(env!("CARGO_BIN_EXE_hayaid"));
    let mut work = None;
    let mut scenarios = "all".to_string();
    let mut port_base = 28_100u16;
    let mut minutes = 30u64;
    let mut hayai_log = "info".to_string();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| panic!("{name} needs a value"))
        };
        match arg.as_str() {
            "--zakurad" => zakurad = Some(PathBuf::from(value("--zakurad"))),
            "--hayaid" => hayaid = PathBuf::from(value("--hayaid")),
            "--work" => work = Some(PathBuf::from(value("--work"))),
            "--scenario" => scenarios = value("--scenario"),
            "--port-base" => port_base = value("--port-base").parse().expect("a port"),
            "--minutes" => minutes = value("--minutes").parse().expect("a number"),
            "--hayaid-log" => hayai_log = value("--hayaid-log"),
            // The arguments of the default test harness, for example `--nocapture`.
            _ => {}
        }
    }
    let Some(zakurad) = zakurad else {
        println!(
            "zakura_pair: no --zakurad PATH argument, so no scenario runs (docs/regtest-pair.md)"
        );
        return;
    };
    let work = work.unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        repo.join(format!("target/regtest-pair/run-{secs}"))
    });
    std::fs::create_dir_all(&work).expect("work directory");
    let work = work.canonicalize().expect("work directory");
    let args = Args {
        zakurad,
        hayaid,
        work,
        port_base,
        minutes,
        repo,
        hayai_log,
    };
    println!(
        "zakura_pair: hayaid {}, zakurad {}, work {}",
        args.hayaid.display(),
        args.zakurad.display(),
        args.work.display()
    );
    let all = ["a", "b", "c", "d", "e", "nu61", "rpc", "g"];
    let selected: Vec<&str> = match scenarios.as_str() {
        "all" => all.to_vec(),
        list => list.split(',').collect(),
    };
    let mut report = Report::default();
    for name in selected {
        report.scenario = name.to_string();
        let outcome = match name {
            "a" => scenarios::a(&args, &mut report),
            "b" => scenarios::b(&args, &mut report),
            "c" => scenarios::c(&args, &mut report),
            "d" => scenarios::d(&args, &mut report),
            "e" => scenarios::e(&args, &mut report),
            "g" => scenarios::g(&args, &mut report),
            "nu61" => scenarios::nu61(&args, &mut report),
            "rpc" => rpc_compare::run(&args, &mut report),
            "nu7" => scenarios::nu7(&args, &mut report),
            other => Err(format!("unknown scenario {other}")),
        };
        // A scenario that stops on an error is a failed row, and the next one still runs.
        if let Err(e) = outcome {
            report.check("scenario completed", false, e);
        }
    }
    let mut table = String::from("| Scenario | Check | Result | Detail |\n|---|---|---|---|\n");
    for row in &report.rows {
        table.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            row.scenario,
            row.check,
            if row.ok { "ok" } else { "FAIL" },
            row.detail.replace('|', "/").replace('\n', " ")
        ));
    }
    let path = args
        .work
        .join(format!("report-{}.md", scenarios.replace(',', "-")));
    std::fs::write(&path, &table).expect("report");
    let failed = report.rows.iter().filter(|r| !r.ok).count();
    println!(
        "zakura_pair: {} checks, {failed} failed, report {}",
        report.rows.len(),
        path.display()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}
