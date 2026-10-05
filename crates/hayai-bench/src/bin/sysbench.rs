//! System-resource benchmarks: every scenario of `hayai_bench::scenarios` for both
//! implementations, with wall time, CPU time, memory, page faults, context switches, heap
//! traffic, hardware counters and disk traffic per iteration, written to
//! `bench-results/system.json`.
//!
//! Usage:
//!   sysbench --all [--iterations N] [--threads T]
//!   sysbench --scenario NAME [--param P] [--impl hayai|zakura|zebra] [--iterations N] [--threads T]
//!
//! `--threads T` sizes the global rayon pool of every child (default: rayon's, one thread
//! per logical CPU); it is the knob for measuring a pool sized to physical cores.
//!
//! Every (scenario, param, impl) triple runs in a fresh child process (this binary re-executed
//! with `--child`), so `max_rss_kb` is that triple's own high-water mark and allocator state
//! does not leak between triples. The child opens the hardware counters before any thread
//! exists, builds the scenario, runs one warm-up step and `iterations` recorded steps, and
//! prints one JSON line; the parent adds the `wait4` rusage and writes the aggregate.
//! The sources and caveats of each field are documented in `hayai_bench::sysmetrics`.

#[cfg(not(feature = "mimalloc"))]
use std::alloc::System;
use std::fs;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};

use hayai_bench::scenarios::{self, Impl, Spec};
use hayai_bench::sysmetrics::{self, CountingAlloc, Measured, Meter};
use serde::{Deserialize, Serialize};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: CountingAlloc<hayai_bench::mimalloc::MiMalloc> =
    CountingAlloc(hayai_bench::mimalloc::MiMalloc);
#[cfg(not(feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc(System);

const DEFAULT_ITERATIONS: u64 = 20;

/// One row of `bench-results/system.json`. Counters are per iteration (totals over the
/// recorded iterations divided by their number); `wall_ms_median` is the median iteration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Row {
    name: String,
    #[serde(rename = "impl")]
    imp: String,
    param: String,
    iterations: u64,
    wall_ms_median: f64,
    cpu_user_ms: f64,
    cpu_sys_ms: f64,
    max_rss_kb: u64,
    minor_faults: f64,
    major_faults: f64,
    ctx_voluntary: f64,
    ctx_involuntary: f64,
    alloc_bytes: f64,
    alloc_count: f64,
    peak_heap_bytes: u64,
    cycles: Option<f64>,
    instructions: Option<f64>,
    cache_refs: Option<f64>,
    cache_misses: Option<f64>,
    llc_loads: Option<f64>,
    llc_misses: Option<f64>,
    branch_misses: Option<f64>,
    io_write_bytes: f64,
    io_read_bytes: f64,
    scratch_bytes: f64,
    blocked_ms: f64,
    /// Every hardware counter could be opened; otherwise the missing ones are `null` and
    /// `counters_unavailable` names them with the kernel's error.
    counters_available: bool,
    counters_unavailable: Option<String>,
}

#[derive(Serialize)]
struct Machine {
    cpu: String,
    /// Base allocator under the counting wrapper: `system` (glibc malloc) or `mimalloc`.
    allocator: &'static str,
    /// Threads of the children's rayon pool: `--threads`, or rayon's default (one per
    /// logical CPU) when absent.
    rayon_threads: usize,
    threads: usize,
    kernel: String,
    perf_event_paranoid: String,
}

#[derive(Serialize)]
struct Report {
    machine: Machine,
    scenarios: Vec<Row>,
}

/// What the child prints: the measurements plus the growth of its scratch directory.
#[derive(Serialize, Deserialize)]
struct ChildOutput {
    measured: Measured,
    scratch_bytes: u64,
}

struct Args {
    child: bool,
    all: bool,
    scenario: Option<String>,
    param: Option<String>,
    imp: Option<Impl>,
    iterations: u64,
    threads: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args {
        child: false,
        all: false,
        scenario: None,
        param: None,
        imp: None,
        iterations: DEFAULT_ITERATIONS,
        threads: None,
    };
    let mut it = argv;
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().ok_or_else(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--child" => args.child = true,
            "--all" => args.all = true,
            "--scenario" => args.scenario = Some(value("--scenario")?),
            "--param" => args.param = Some(value("--param")?),
            "--impl" => args.imp = Some(Impl::parse(&value("--impl")?)?),
            "--iterations" => {
                args.iterations = value("--iterations")?
                    .parse()
                    .map_err(|e| format!("--iterations: {e}"))?
            }
            "--threads" => {
                args.threads = Some(
                    value("--threads")?
                        .parse()
                        .map_err(|e| format!("--threads: {e}"))?,
                )
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if args.iterations == 0 {
        return Err("--iterations must be at least 1".into());
    }
    if let Some(0) = args.threads {
        return Err("--threads must be at least 1".into());
    }
    match (args.all, &args.scenario) {
        (false, None) => Err("one of --all or --scenario NAME is required".into()),
        (true, Some(_)) => Err("--all and --scenario are exclusive".into()),
        _ => Ok(args),
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sysbench: {e}");
            std::process::exit(2);
        }
    };
    if args.child {
        let (Some(name), Some(param), Some(imp)) = (&args.scenario, &args.param, args.imp) else {
            eprintln!("sysbench: --child needs --scenario, --param and --impl");
            std::process::exit(2);
        };
        if let Err(e) = run_child(name, param, imp, args.iterations, args.threads) {
            eprintln!("sysbench child {name}/{param}/{imp}: {e}");
            std::process::exit(1);
        }
        return;
    }
    let specs = select_specs(&args);
    if specs.is_empty() {
        eprintln!("sysbench: nothing matches the selection");
        std::process::exit(2);
    }
    let mut rows = Vec::new();
    for (spec, imp) in &specs {
        eprintln!("== {} {} {}", spec.name, spec.param, imp);
        match run_parent(spec, *imp, args.iterations, args.threads) {
            Ok(row) => rows.push(row),
            Err(e) => {
                eprintln!("sysbench: {} {} {}: {e}", spec.name, spec.param, imp);
                std::process::exit(1);
            }
        }
    }
    let report = Report {
        machine: machine(args.threads),
        scenarios: rows,
    };
    let out = hayai_bench::results_dir().join("system.json");
    fs::write(
        &out,
        serde_json::to_vec_pretty(&report).expect("serializable"),
    )
    .unwrap_or_else(|e| panic!("writing {}: {e}", out.display()));
    print_table(&report.scenarios);
    eprintln!("{} rows -> {}", report.scenarios.len(), out.display());
}

fn select_specs(args: &Args) -> Vec<(Spec, Impl)> {
    scenarios::catalogue()
        .into_iter()
        .filter(|s| match &args.scenario {
            Some(name) => s.name == name,
            None => true,
        })
        .filter(|s| match &args.param {
            Some(p) => &s.param == p,
            None => true,
        })
        .flat_map(|s| {
            let impls: Vec<Impl> = s
                .impls
                .iter()
                .copied()
                .filter(|i| match args.imp {
                    Some(wanted) => *i == wanted,
                    None => true,
                })
                .collect();
            impls.into_iter().map(move |i| (s.clone(), i))
        })
        .collect()
}

// ---------------------------------------------------------------- child

fn run_child(
    name: &str,
    param: &str,
    imp: Impl,
    iterations: u64,
    threads: Option<usize>,
) -> Result<(), String> {
    // Before any thread exists: inherited counters cover only threads created afterwards.
    let mut meter = Meter::new();
    if let Some(threads) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .map_err(|e| format!("rayon pool of {threads} threads: {e}"))?;
    }
    let mut built = scenarios::build(name, param, imp)?;
    let scratch_before = match &built.scratch {
        Some(dir) => sysmetrics::dir_size(dir).map_err(|e| format!("scratch size: {e}"))?,
        None => 0,
    };
    meter.set_recording(false);
    built.step(&mut meter);
    meter.set_recording(true);
    for _ in 0..iterations {
        built.step(&mut meter);
    }
    let scratch_after = match &built.scratch {
        Some(dir) => sysmetrics::dir_size(dir).map_err(|e| format!("scratch size: {e}"))?,
        None => 0,
    };
    let out = ChildOutput {
        measured: meter.finish(),
        scratch_bytes: scratch_after.saturating_sub(scratch_before),
    };
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &out).map_err(|e| e.to_string())?;
    stdout.write_all(b"\n").map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------- parent

fn run_parent(
    spec: &Spec,
    imp: Impl,
    iterations: u64,
    threads: Option<usize>,
) -> Result<Row, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut command = Command::new(exe);
    command.args([
        "--child",
        "--scenario",
        spec.name,
        "--param",
        &spec.param,
        "--impl",
        imp.as_str(),
        "--iterations",
        &iterations.to_string(),
    ]);
    if let Some(threads) = threads {
        command.args(["--threads", &threads.to_string()]);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn child: {e}"))?;
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("piped stdout")
        .read_to_string(&mut stdout)
        .map_err(|e| format!("child stdout: {e}"))?;
    let (code, rusage) = sysmetrics::wait4_rusage(child.id()).map_err(|e| format!("wait4: {e}"))?;
    if code != 0 {
        return Err(format!("child exited with status {code}"));
    }
    let output: ChildOutput =
        serde_json::from_str(stdout.trim()).map_err(|e| format!("child output {stdout:?}: {e}"))?;
    Ok(row(spec, imp, output, rusage.max_rss_kb))
}

fn row(spec: &Spec, imp: Impl, out: ChildOutput, max_rss_kb: u64) -> Row {
    let m = out.measured;
    let n = m.iterations as f64;
    let per = |v: u64| v as f64 / n;
    let per_opt = |v: Option<u64>| v.map(per);
    let hw = m.hw;
    Row {
        name: spec.name.to_string(),
        imp: imp.as_str().to_string(),
        param: spec.param.clone(),
        iterations: m.iterations,
        wall_ms_median: m.wall_ms_median,
        cpu_user_ms: m.rusage_user_ms / n,
        cpu_sys_ms: m.rusage_sys_ms / n,
        max_rss_kb,
        minor_faults: per(m.minor_faults),
        major_faults: per(m.major_faults),
        ctx_voluntary: per(m.ctx_voluntary),
        ctx_involuntary: per(m.ctx_involuntary),
        alloc_bytes: per(m.alloc_bytes),
        alloc_count: per(m.alloc_count),
        peak_heap_bytes: m.peak_heap_bytes,
        cycles: per_opt(hw.cycles),
        instructions: per_opt(hw.instructions),
        cache_refs: per_opt(hw.cache_refs),
        cache_misses: per_opt(hw.cache_misses),
        llc_loads: per_opt(hw.llc_loads),
        llc_misses: per_opt(hw.llc_misses),
        branch_misses: per_opt(hw.branch_misses),
        io_write_bytes: per(m.io_write_bytes),
        io_read_bytes: per(m.io_read_bytes),
        scratch_bytes: per(out.scratch_bytes),
        blocked_ms: (m.wall_ms_total - m.rusage_user_ms - m.rusage_sys_ms) / n,
        counters_available: hw.complete(),
        counters_unavailable: m.hw_unavailable,
    }
}

fn machine(threads: Option<usize>) -> Machine {
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        })
        .unwrap_or_else(|| "unknown".to_string());
    let read = |p: &str| {
        fs::read_to_string(p)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    };
    Machine {
        cpu,
        allocator: hayai_bench::ALLOCATOR,
        rayon_threads: threads.unwrap_or_else(rayon::current_num_threads),
        threads: std::thread::available_parallelism().map_or(0, |n| n.get()),
        kernel: read("/proc/sys/kernel/osrelease"),
        perf_event_paranoid: read("/proc/sys/kernel/perf_event_paranoid"),
    }
}

// ---------------------------------------------------------------- table

fn human_bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn print_table(rows: &[Row]) {
    println!();
    println!(
        "{:<22} {:<20} {:<6} {:>10} {:>9} {:>9} {:>10} {:>10} {:>10} {:>8} {:>6}",
        "scenario",
        "param",
        "impl",
        "wall ms",
        "cpu ms",
        "rss MiB",
        "alloc",
        "allocs",
        "cache-miss",
        "llc-miss",
        "ipc"
    );
    for r in rows {
        let percent = |num: Option<f64>, den: Option<f64>| match (num, den) {
            (Some(n), Some(d)) => format!("{:.1}%", 100.0 * n / d.max(1.0)),
            _ => "n/a".to_string(),
        };
        let miss = percent(r.cache_misses, r.cache_refs);
        let llc = percent(r.llc_misses, r.llc_loads);
        let ipc = match (r.instructions, r.cycles) {
            (Some(i), Some(c)) => format!("{:.2}", i / c.max(1.0)),
            _ => "n/a".to_string(),
        };
        println!(
            "{:<22} {:<20} {:<6} {:>10.2} {:>9.1} {:>9.0} {:>10} {:>10.0} {:>10} {:>8} {:>6}",
            r.name,
            r.param,
            r.imp,
            r.wall_ms_median,
            r.cpu_user_ms + r.cpu_sys_ms,
            r.max_rss_kb as f64 / 1024.0,
            human_bytes(r.alloc_bytes),
            r.alloc_count,
            miss,
            llc,
            ipc,
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_bench::sysmetrics::HwCounts;

    fn sample_output() -> ChildOutput {
        ChildOutput {
            measured: Measured {
                iterations: 4,
                wall_ms_median: 10.0,
                wall_ms_total: 44.0,
                rusage_user_ms: 80.0,
                rusage_sys_ms: 4.0,
                minor_faults: 40,
                major_faults: 0,
                ctx_voluntary: 8,
                ctx_involuntary: 2,
                alloc_bytes: 4096,
                alloc_count: 16,
                peak_heap_bytes: 1 << 20,
                hw: HwCounts {
                    cycles: Some(4_000),
                    instructions: Some(8_000),
                    ..Default::default()
                },
                hw_unavailable: Some("llc_loads: ENOENT".into()),
                io_write_bytes: 8192,
                io_read_bytes: 0,
            },
            scratch_bytes: 2048,
        }
    }

    #[test]
    fn row_json_has_the_documented_schema() {
        let spec = Spec {
            name: "parse_block",
            param: "transparent-6500x1".into(),
            impls: vec![Impl::Hayai],
        };
        let r = row(&spec, Impl::Hayai, sample_output(), 123_456);
        let value = serde_json::to_value(&r).expect("serializable");
        let obj = value.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let mut expected = vec![
            "name",
            "impl",
            "param",
            "iterations",
            "wall_ms_median",
            "cpu_user_ms",
            "cpu_sys_ms",
            "max_rss_kb",
            "minor_faults",
            "major_faults",
            "ctx_voluntary",
            "ctx_involuntary",
            "alloc_bytes",
            "alloc_count",
            "peak_heap_bytes",
            "cycles",
            "instructions",
            "cache_refs",
            "cache_misses",
            "llc_loads",
            "llc_misses",
            "branch_misses",
            "io_write_bytes",
            "io_read_bytes",
            "scratch_bytes",
            "blocked_ms",
            "counters_available",
            "counters_unavailable",
        ];
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert_eq!(obj["impl"], "hayai");
        assert_eq!(obj["counters_available"], false);
        // Per-iteration averages and the wait4 high-water mark.
        assert_eq!(obj["cpu_user_ms"], 20.0);
        assert_eq!(obj["minor_faults"], 10.0);
        assert_eq!(obj["alloc_bytes"], 1024.0);
        assert_eq!(obj["scratch_bytes"], 512.0);
        assert_eq!(obj["max_rss_kb"], 123_456);
        assert_eq!(obj["blocked_ms"], -10.0);
        assert_eq!(obj["cycles"], 1000.0);
        assert_eq!(obj["llc_loads"], serde_json::Value::Null);
        let back: Row = serde_json::from_value(value).expect("round trip");
        assert_eq!(back, r);
    }

    #[test]
    fn counters_available_means_every_event_was_opened() {
        let mut out = sample_output();
        out.measured.hw = HwCounts {
            cycles: Some(1),
            instructions: Some(1),
            cache_refs: Some(1),
            cache_misses: Some(1),
            llc_loads: Some(1),
            llc_misses: Some(1),
            branch_misses: Some(1),
        };
        out.measured.hw_unavailable = None;
        let spec = Spec {
            name: "x",
            param: String::new(),
            impls: vec![Impl::Zakura],
        };
        let value = serde_json::to_value(row(&spec, Impl::Zakura, out, 1)).expect("serializable");
        assert_eq!(value["counters_available"], true);
        assert_eq!(value["counters_unavailable"], serde_json::Value::Null);
    }

    #[test]
    fn threads_option_is_parsed_and_must_be_positive() {
        fn argv(s: &str) -> impl Iterator<Item = String> + '_ {
            s.split(' ').map(String::from)
        }
        let args = parse_args_from(argv("--all --threads 16")).expect("parses");
        assert_eq!(args.threads, Some(16));
        assert!(args.all);
        let args = parse_args_from(argv("--scenario parse_block")).expect("parses");
        assert_eq!(args.threads, None);
        let Err(e) = parse_args_from(argv("--all --threads 0")) else {
            panic!("zero threads is rejected");
        };
        assert!(e.contains("--threads"), "{e}");
    }

    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(human_bytes(512.0), "512 B");
        assert_eq!(human_bytes(1536.0), "1.5 KiB");
        assert_eq!(human_bytes(3.0 * 1024.0 * 1024.0), "3.0 MiB");
    }
}
