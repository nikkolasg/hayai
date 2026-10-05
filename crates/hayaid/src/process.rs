//! Process statistics from `/proc/self`: resident memory and CPU time.
//!
//! The exporter exposes them under the standard Prometheus names
//! (`process_resident_memory_bytes`, `process_cpu_seconds_total`), so the dashboard does
//! not depend on the host exporter for the node's own use.

/// Clock ticks per second of `utime` and `stime` in `/proc/<pid>/stat` (`USER_HZ`). It is
/// 100 on every Linux architecture.
const CLOCK_TICKS_PER_SECOND: f64 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessStats {
    pub resident_bytes: u64,
    pub cpu_seconds: f64,
}

/// Reads the statistics of this process.
pub fn read() -> Result<ProcessStats, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("/proc/self/status: {e}"))?;
    let stat =
        std::fs::read_to_string("/proc/self/stat").map_err(|e| format!("/proc/self/stat: {e}"))?;
    parse(&status, &stat)
}

fn parse(status: &str, stat: &str) -> Result<ProcessStats, String> {
    let resident_kib: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.trim().strip_suffix("kB"))
        .and_then(|kib| kib.trim().parse().ok())
        .ok_or("no VmRSS line in /proc/self/status")?;
    // The command name is in parentheses and can contain spaces and parentheses: the fields
    // after the last `)` start at field 3 (`state`). `utime` and `stime` are fields 14, 15.
    let after_name = stat
        .rsplit_once(')')
        .ok_or("no command name in /proc/self/stat")?
        .1;
    let mut fields = after_name.split_whitespace().skip(11);
    let mut ticks = || -> Result<f64, String> {
        fields
            .next()
            .and_then(|f| f.parse::<u64>().ok())
            .map(|t| t as f64)
            .ok_or_else(|| "no utime or stime in /proc/self/stat".to_string())
    };
    let cpu_ticks = ticks()? + ticks()?;
    Ok(ProcessStats {
        resident_bytes: resident_kib * 1024,
        cpu_seconds: cpu_ticks / CLOCK_TICKS_PER_SECOND,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_proc_formats() {
        let status = "Name:\thayaid\nVmPeak:\t  900 kB\nVmRSS:\t  2048 kB\nThreads:\t4\n";
        // A command name with a space and a parenthesis; utime 150, stime 25.
        let stat = "42 (hay (ai) d) S 1 42 42 0 -1 4194560 100 0 0 0 150 25 0 0 20 0 4 0";
        assert_eq!(
            parse(status, stat),
            Ok(ProcessStats {
                resident_bytes: 2048 * 1024,
                cpu_seconds: 1.75,
            })
        );
        let Err(_) = parse("Name: x\n", stat) else {
            panic!("a missing VmRSS is an error");
        };
        let Err(_) = parse(status, "42 (x) S 1") else {
            panic!("a short stat line is an error");
        };
    }

    #[test]
    fn reads_this_process() {
        let stats = read().expect("/proc is readable on Linux");
        assert!(stats.resident_bytes > 0);
    }
}
