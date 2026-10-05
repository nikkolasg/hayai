//! Prometheus metrics: a small registry of atomic counters, gauges and histograms, and an
//! HTTP endpoint that serves them in the text exposition format (version 0.0.4).
//!
//! A metric that has the meaning of a Zakura metric has Zakura's name with the dots
//! replaced by underscores, as Zakura's Prometheus exporter writes it. Every other metric
//! starts with `hayai_`. The registry keeps one series per name and label set. Updates are
//! single atomic operations. Rendering takes the registration lock.

use std::fmt::Write as _;
use std::io::{self, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use crate::http::{read_request, write_typed_response, HttpError, IDLE_TIMEOUT};

/// Content type of the text exposition format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Default histogram buckets for durations in seconds, from 100 µs to 60 s.
pub const DURATION_BUCKETS: [f64; 14] = [
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 1.0, 10.0, 60.0,
];

/// A monotonic count.
#[derive(Default, Debug)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self) {
        self.add(1);
    }

    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Sets the count from a cumulative source (for example a component's own counter
    /// snapshot). The source must be monotonic.
    pub fn set(&self, total: u64) {
        self.0.store(total, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A monotonic count in fractional units, such as CPU seconds.
#[derive(Default, Debug)]
pub struct FloatCounter(AtomicU64);

impl FloatCounter {
    /// Sets the total from a cumulative source. The source must be monotonic.
    pub fn set(&self, total: f64) {
        self.0.store(total.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// A value that goes up and down.
#[derive(Default, Debug)]
pub struct Gauge(AtomicU64);

impl Gauge {
    pub fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }

    /// Adds `delta` to the value in one atomic step.
    pub fn add(&self, delta: f64) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                Some((f64::from_bits(bits) + delta).to_bits())
            });
    }

    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Observations in cumulative buckets, with their sum and count.
#[derive(Debug)]
pub struct Histogram {
    bounds: Vec<f64>,
    /// One count per bound, not cumulative; the exposition accumulates.
    buckets: Vec<AtomicU64>,
    /// Observations above the last bound.
    overflow: AtomicU64,
    sum: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new(bounds: &[f64]) -> Self {
        assert!(
            bounds.windows(2).all(|w| w[0] < w[1]),
            "histogram bounds must increase"
        );
        Self {
            bounds: bounds.to_vec(),
            buckets: bounds.iter().map(|_| AtomicU64::new(0)).collect(),
            overflow: AtomicU64::new(0),
            sum: AtomicU64::new(0f64.to_bits()),
            count: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, value: f64) {
        match self.bounds.iter().position(|b| value <= *b) {
            Some(i) => self.buckets[i].fetch_add(1, Ordering::Relaxed),
            None => self.overflow.fetch_add(1, Ordering::Relaxed),
        };
        let mut current = self.sum.load(Ordering::Relaxed);
        loop {
            let next = (f64::from_bits(current) + value).to_bits();
            match self.sum.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_duration(&self, d: Duration) {
        self.observe(d.as_secs_f64());
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn sum(&self) -> f64 {
        f64::from_bits(self.sum.load(Ordering::Relaxed))
    }
}

#[derive(Clone)]
enum Value {
    Counter(Arc<Counter>),
    FloatCounter(Arc<FloatCounter>),
    Gauge(Arc<Gauge>),
    Histogram(Arc<Histogram>),
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Value::Counter(_) | Value::FloatCounter(_) => "counter",
            Value::Gauge(_) => "gauge",
            Value::Histogram(_) => "histogram",
        }
    }
}

struct Series {
    name: String,
    help: String,
    labels: Vec<(String, String)>,
    value: Value,
}

/// The set of metrics a process exposes.
#[derive(Default)]
pub struct Registry {
    series: Mutex<Vec<Series>>,
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

impl Registry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Returns the series of `name` and `labels`, or registers it with `make`. Panics when
    /// the name is not a Prometheus metric name or when the name is already registered with
    /// another type: both are programming errors.
    fn register(
        &self,
        name: &str,
        help: &str,
        labels: &[(&str, &str)],
        make: impl FnOnce() -> Value,
    ) -> Value {
        assert!(valid_name(name), "{name} is not a Prometheus metric name");
        let mut series = self.series.lock().unwrap_or_else(|e| e.into_inner());
        let value = make();
        if let Some(existing) = series.iter().find(|s| s.name == name) {
            assert_eq!(
                existing.value.type_name(),
                value.type_name(),
                "{name} is registered with two types"
            );
        }
        if let Some(existing) = series.iter().find(|s| {
            s.name == name
                && s.labels.len() == labels.len()
                && s.labels
                    .iter()
                    .zip(labels)
                    .all(|((k, v), (k2, v2))| k == k2 && v == v2)
        }) {
            return existing.value.clone();
        }
        series.push(Series {
            name: name.to_string(),
            help: help.to_string(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            value: value.clone(),
        });
        value
    }

    pub fn counter(&self, name: &str, help: &str, labels: &[(&str, &str)]) -> Arc<Counter> {
        match self.register(name, help, labels, || {
            Value::Counter(Arc::new(Counter::default()))
        }) {
            Value::Counter(c) => c,
            _ => unreachable!("register checks the type"),
        }
    }

    /// A counter with fractional values (it has the Prometheus type `counter`).
    pub fn float_counter(
        &self,
        name: &str,
        help: &str,
        labels: &[(&str, &str)],
    ) -> Arc<FloatCounter> {
        match self.register(name, help, labels, || {
            Value::FloatCounter(Arc::new(FloatCounter::default()))
        }) {
            Value::FloatCounter(c) => c,
            _ => unreachable!("register checks the type"),
        }
    }

    pub fn gauge(&self, name: &str, help: &str, labels: &[(&str, &str)]) -> Arc<Gauge> {
        match self.register(name, help, labels, || {
            Value::Gauge(Arc::new(Gauge::default()))
        }) {
            Value::Gauge(g) => g,
            _ => unreachable!("register checks the type"),
        }
    }

    pub fn histogram(
        &self,
        name: &str,
        help: &str,
        labels: &[(&str, &str)],
        bounds: &[f64],
    ) -> Arc<Histogram> {
        match self.register(name, help, labels, || {
            Value::Histogram(Arc::new(Histogram::new(bounds)))
        }) {
            Value::Histogram(h) => h,
            _ => unreachable!("register checks the type"),
        }
    }

    /// The text exposition of every series, grouped by name in registration order.
    pub fn render(&self) -> String {
        let series = self.series.lock().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<&str> = Vec::new();
        for s in series.iter() {
            if !names.contains(&s.name.as_str()) {
                names.push(&s.name);
            }
        }
        let mut out = String::new();
        for name in names {
            let mut group = series.iter().filter(|s| s.name == name).peekable();
            let Some(first) = group.peek() else {
                unreachable!("the name comes from a series");
            };
            let _ = writeln!(out, "# HELP {name} {}", escape_help(&first.help));
            let _ = writeln!(out, "# TYPE {name} {}", first.value.type_name());
            for s in group {
                match &s.value {
                    Value::Counter(c) => {
                        let _ = writeln!(out, "{name}{} {}", labels(&s.labels, None), c.get());
                    }
                    Value::FloatCounter(c) => {
                        let _ =
                            writeln!(out, "{name}{} {}", labels(&s.labels, None), number(c.get()));
                    }
                    Value::Gauge(g) => {
                        let _ =
                            writeln!(out, "{name}{} {}", labels(&s.labels, None), number(g.get()));
                    }
                    Value::Histogram(h) => {
                        let mut cumulative = 0u64;
                        for (bound, bucket) in h.bounds.iter().zip(&h.buckets) {
                            cumulative += bucket.load(Ordering::Relaxed);
                            let le = number(*bound);
                            let _ = writeln!(
                                out,
                                "{name}_bucket{} {cumulative}",
                                labels(&s.labels, Some(&le))
                            );
                        }
                        cumulative += h.overflow.load(Ordering::Relaxed);
                        let _ = writeln!(
                            out,
                            "{name}_bucket{} {cumulative}",
                            labels(&s.labels, Some("+Inf"))
                        );
                        let _ = writeln!(
                            out,
                            "{name}_sum{} {}",
                            labels(&s.labels, None),
                            number(h.sum())
                        );
                        let _ =
                            writeln!(out, "{name}_count{} {}", labels(&s.labels, None), h.count());
                    }
                }
            }
        }
        out
    }
}

fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf" } else { "-Inf" }.into()
    } else {
        format!("{v}")
    }
}

fn escape_help(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn labels(pairs: &[(String, String)], le: Option<&str>) -> String {
    if let (true, None) = (pairs.is_empty(), le) {
        return String::new();
    }
    let mut parts: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape_label(v)))
        .collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    format!("{{{}}}", parts.join(","))
}

/// HTTP endpoint for scrapers: `GET /metrics` answers the exposition, every other path 404.
pub struct MetricsServer {
    addr: SocketAddr,
    stopped: AtomicBool,
}

impl MetricsServer {
    /// Binds `addr` and serves `registry` on a background thread, one thread per connection.
    pub fn serve(addr: impl ToSocketAddrs, registry: Arc<Registry>) -> io::Result<Arc<Self>> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let server = Arc::new(Self {
            addr,
            stopped: AtomicBool::new(false),
        });
        let weak: Weak<Self> = Arc::downgrade(&server);
        thread::Builder::new()
            .name(format!("metrics-accept-{addr}"))
            .spawn(move || {
                for stream in listener.incoming() {
                    let Some(server) = weak.upgrade() else {
                        break;
                    };
                    if server.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let registry = registry.clone();
                    let spawned =
                        thread::Builder::new()
                            .name("metrics-conn".into())
                            .spawn(move || {
                                if let Err(e) = serve_scrapes(stream, &registry) {
                                    tracing::debug!(error = %e, "metrics connection ended");
                                }
                            });
                    if let Err(e) = spawned {
                        tracing::warn!(error = %e, "cannot spawn metrics connection thread");
                    }
                }
            })?;
        Ok(server)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops the accept loop.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

fn serve_scrapes(stream: TcpStream, registry: &Registry) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let request = match read_request(&mut reader) {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(()),
            Err(HttpError::BadRequest(why)) => {
                return write_typed_response(
                    &mut writer,
                    400,
                    "Bad Request",
                    "text/plain",
                    why.as_bytes(),
                    false,
                );
            }
            Err(HttpError::HeadTooLarge) => {
                return write_typed_response(
                    &mut writer,
                    431,
                    "Request Header Fields Too Large",
                    "text/plain",
                    b"",
                    false,
                );
            }
            Err(HttpError::TooLarge) => {
                return write_typed_response(
                    &mut writer,
                    413,
                    "Payload Too Large",
                    "text/plain",
                    b"",
                    false,
                );
            }
            Err(HttpError::Io(e)) => return Err(e),
        };
        let path = request.target.split('?').next().unwrap_or("");
        match (request.method.as_str(), path) {
            ("GET", "/metrics") => write_typed_response(
                &mut writer,
                200,
                "OK",
                CONTENT_TYPE,
                registry.render().as_bytes(),
                request.keep_alive,
            )?,
            ("GET", _) => write_typed_response(
                &mut writer,
                404,
                "Not Found",
                "text/plain",
                b"metrics are at /metrics",
                request.keep_alive,
            )?,
            _ => write_typed_response(
                &mut writer,
                405,
                "Method Not Allowed",
                "text/plain",
                b"use GET",
                request.keep_alive,
            )?,
        }
        if !request.keep_alive {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposition_groups_series_and_accumulates_buckets() {
        let r = Registry::new();
        let height = r.gauge(
            "zcash_chain_verified_block_height",
            "Height of the best verified block.",
            &[],
        );
        height.set(10.0);
        let ok = r.histogram(
            "sync_block_verify_duration_seconds",
            "Block verification time.",
            &[("result", "success")],
            &[0.1, 1.0],
        );
        let bad = r.histogram(
            "sync_block_verify_duration_seconds",
            "ignored second help",
            &[("result", "failure")],
            &[0.1, 1.0],
        );
        ok.observe(0.05);
        ok.observe(0.5);
        ok.observe(5.0);
        bad.observe(0.01);
        let blocks = r.counter("hayai_blocks_total", "Blocks.", &[]);
        blocks.add(3);
        // Registering the same series again returns the same metric.
        r.counter("hayai_blocks_total", "Blocks.", &[]).inc();
        let text = r.render();
        let expected = "\
# HELP zcash_chain_verified_block_height Height of the best verified block.
# TYPE zcash_chain_verified_block_height gauge
zcash_chain_verified_block_height 10
# HELP sync_block_verify_duration_seconds Block verification time.
# TYPE sync_block_verify_duration_seconds histogram
sync_block_verify_duration_seconds_bucket{result=\"success\",le=\"0.1\"} 1
sync_block_verify_duration_seconds_bucket{result=\"success\",le=\"1\"} 2
sync_block_verify_duration_seconds_bucket{result=\"success\",le=\"+Inf\"} 3
sync_block_verify_duration_seconds_sum{result=\"success\"} 5.55
sync_block_verify_duration_seconds_count{result=\"success\"} 3
sync_block_verify_duration_seconds_bucket{result=\"failure\",le=\"0.1\"} 1
sync_block_verify_duration_seconds_bucket{result=\"failure\",le=\"1\"} 1
sync_block_verify_duration_seconds_bucket{result=\"failure\",le=\"+Inf\"} 1
sync_block_verify_duration_seconds_sum{result=\"failure\"} 0.01
sync_block_verify_duration_seconds_count{result=\"failure\"} 1
# HELP hayai_blocks_total Blocks.
# TYPE hayai_blocks_total counter
hayai_blocks_total 4
";
        assert_eq!(text, expected);
    }

    #[test]
    fn a_float_counter_is_exposed_as_a_counter() {
        let r = Registry::new();
        r.float_counter("process_cpu_seconds_total", "CPU time.", &[])
            .set(12.25);
        assert_eq!(
            r.render(),
            "# HELP process_cpu_seconds_total CPU time.\n\
             # TYPE process_cpu_seconds_total counter\n\
             process_cpu_seconds_total 12.25\n"
        );
    }

    #[test]
    #[should_panic(expected = "registered with two types")]
    fn one_name_has_one_type() {
        let r = Registry::new();
        r.counter("x", "", &[]);
        r.gauge("x", "", &[]);
    }

    #[test]
    #[should_panic(expected = "not a Prometheus metric name")]
    fn dotted_names_are_rejected() {
        Registry::new().counter("zcash.chain", "", &[]);
    }

    #[test]
    fn label_values_are_escaped() {
        let r = Registry::new();
        r.gauge("g", "h", &[("k", "a\"b\\c")]).set(1.5);
        assert!(r.render().contains("g{k=\"a\\\"b\\\\c\"} 1.5\n"));
    }
}
