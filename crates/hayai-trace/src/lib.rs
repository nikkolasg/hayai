//! Non-blocking JSONL trace writer.
//!
//! The row format is the format of Zakura's `zakura-jsonl-trace`, so one script joins the
//! traces of both nodes. Every row is one JSON object on one line:
//!
//! ```text
//! {"ts": <µs since the tracer opened>, "unix_us": <µs since the Unix epoch>,
//!  "node": <label>, "process_trace_id": "<pid>-<start ns>", "event": <name>, ...fields}
//! ```
//!
//! `ts`, `node`, `process_trace_id` and `event` have Zakura's meaning. `unix_us` is the
//! wall clock of the emitting thread. It joins the rows of two processes by time.
//!
//! The emitting thread never blocks and never touches a file. It serializes the row and
//! puts it on a bounded channel ([`Options::capacity`], 16,384 slots as in Zakura). When the
//! channel is full, the tracer drops the row and counts it ([`Tracer::dropped`]). One writer
//! thread appends the rows to one file per [`Table`] and flushes and fsyncs every file at
//! [`Options::flush_interval`] (1 s). [`Tracer::close`] writes every queued row, flushes and
//! fsyncs the files and stops the writer.

#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};
use serde::Serialize;

/// Event names. The names that Zakura also emits have Zakura's spelling.
pub mod event {
    pub const BLOCK_RECEIVED: &str = "block_received";
    pub const BLOCK_HEADER_CHECKED: &str = "block_header_checked";
    pub const BLOCK_RECONSTRUCTED: &str = "block_reconstructed";
    pub const BLOCK_FORWARDED: &str = "block_forwarded";
    pub const COMMIT_START: &str = "commit_start";
    pub const COMMIT_FINISH: &str = "commit_finish";
    pub const BLOCK_VALIDATED: &str = "block_validated";
    pub const UPSTREAM_VERDICT: &str = "upstream_verdict";
    pub const TEMPLATE_EMPTY: &str = "template_empty";
    pub const TEMPLATE_FULL: &str = "template_full";
    pub const SYNC_PROGRESS: &str = "sync_progress";
    pub const BLOCK_DISCONNECTED: &str = "block_disconnected";
}

/// One output file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Table {
    /// Block arrival and relay: `block_sync.jsonl`.
    BlockSync,
    /// Validation and commit: `commit_state.jsonl`.
    CommitState,
    /// Template updates: `template.jsonl`.
    Template,
}

impl Table {
    pub const ALL: [Table; 3] = [Table::BlockSync, Table::CommitState, Table::Template];

    pub const fn file_name(self) -> &'static str {
        match self {
            Table::BlockSync => "block_sync.jsonl",
            Table::CommitState => "commit_state.jsonl",
            Table::Template => "template.jsonl",
        }
    }

    const fn index(self) -> usize {
        match self {
            Table::BlockSync => 0,
            Table::CommitState => 1,
            Table::Template => 2,
        }
    }
}

/// Writer settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Rows that wait for the writer. A row that finds the channel full is dropped.
    pub capacity: usize,
    /// Time between two flushes and fsyncs of every file.
    pub flush_interval: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            capacity: 16_384,
            flush_interval: Duration::from_secs(1),
        }
    }
}

enum Msg {
    Row(Table, Vec<u8>),
    Stop,
}

struct Inner {
    tx: Sender<Msg>,
    node: String,
    process_trace_id: String,
    started: Instant,
    dropped: [AtomicU64; 3],
    closed: AtomicBool,
    writer: Mutex<Option<JoinHandle<io::Result<()>>>>,
}

/// A cheap handle to the trace writer. A disabled tracer accepts every call and writes
/// nothing.
#[derive(Clone)]
pub struct Tracer {
    inner: Option<Arc<Inner>>,
}

#[derive(Serialize)]
struct Row<'a, F> {
    ts: u64,
    unix_us: u64,
    node: &'a str,
    process_trace_id: &'a str,
    event: &'a str,
    #[serde(flatten)]
    fields: F,
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// Microseconds since the Unix epoch.
pub fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, micros)
}

impl Tracer {
    /// A tracer that writes nothing.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// Creates `dir` when it does not exist, opens the three table files in append mode and
    /// starts the writer thread. Every row carries `node` as its node label.
    pub fn open(dir: &Path, node: &str) -> io::Result<Self> {
        Self::open_with(dir, node, Options::default())
    }

    pub fn open_with(dir: &Path, node: &str, options: Options) -> io::Result<Self> {
        Self::start(dir, node, options, None)
    }

    fn start(
        dir: &Path,
        node: &str,
        options: Options,
        gate: Option<Receiver<()>>,
    ) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut files = Vec::with_capacity(Table::ALL.len());
        for table in Table::ALL {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(table.file_name()))?;
            files.push(BufWriter::with_capacity(256 * 1024, file));
        }
        let (tx, rx) = bounded(options.capacity);
        let interval = options.flush_interval;
        let writer = thread::Builder::new()
            .name("trace-writer".into())
            .spawn(move || {
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                run_writer(files, rx, interval)
            })?;
        let started_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Ok(Self {
            inner: Some(Arc::new(Inner {
                tx,
                node: node.to_string(),
                process_trace_id: format!("{}-{started_ns}", std::process::id()),
                started: Instant::now(),
                dropped: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
                closed: AtomicBool::new(false),
                writer: Mutex::new(Some(writer)),
            })),
        })
    }

    /// Queues one row. `fields` runs only when the row can be queued; it must produce a map
    /// (a struct or a JSON object), whose entries follow the envelope fields.
    pub fn emit<F: Serialize>(&self, table: Table, event: &str, fields: impl FnOnce() -> F) {
        let Some(inner) = &self.inner else {
            return;
        };
        let dropped = &inner.dropped[table.index()];
        if inner.closed.load(Ordering::Acquire) || inner.tx.is_full() {
            dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let row = Row {
            ts: micros(inner.started.elapsed()),
            unix_us: unix_micros(),
            node: &inner.node,
            process_trace_id: &inner.process_trace_id,
            event,
            fields: fields(),
        };
        let Ok(line) = serde_json::to_vec(&row) else {
            dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match inner.tx.try_send(Msg::Row(table, line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Rows of `table` that the tracer dropped: channel full, writer stopped, or tracer
    /// closed.
    pub fn dropped(&self, table: Table) -> u64 {
        match &self.inner {
            Some(inner) => inner.dropped[table.index()].load(Ordering::Relaxed),
            None => 0,
        }
    }

    /// Writes every queued row, flushes and fsyncs the files and stops the writer. Later
    /// rows are dropped. Returns the first write error of the writer thread.
    pub fn close(&self) -> io::Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        inner.closed.store(true, Ordering::Release);
        // A blocking send: the writer drains the channel, so a full channel only delays
        // the stop. A stopped writer makes the send fail, and the join reports why.
        let _ = inner.tx.send(Msg::Stop);
        let handle = inner
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        match handle {
            Some(handle) => match handle.join() {
                Ok(result) => result,
                Err(_) => Err(io::Error::other("trace writer panicked")),
            },
            None => Ok(()),
        }
    }
}

fn flush_all(files: &mut [BufWriter<File>]) -> io::Result<()> {
    for file in files {
        file.flush()?;
        file.get_ref().sync_data()?;
    }
    Ok(())
}

fn run_writer(
    mut files: Vec<BufWriter<File>>,
    rx: Receiver<Msg>,
    interval: Duration,
) -> io::Result<()> {
    let mut next_flush = Instant::now() + interval;
    loop {
        match rx.recv_deadline(next_flush) {
            Ok(Msg::Row(table, line)) => {
                let file = &mut files[table.index()];
                file.write_all(&line)?;
                file.write_all(b"\n")?;
            }
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                return flush_all(&mut files);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() >= next_flush {
            flush_all(&mut files)?;
            next_flush = Instant::now() + interval;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn scratch() -> tempfile::TempDir {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        fs::create_dir_all(&base).expect("scratch base");
        tempfile::tempdir_in(base).expect("scratch dir")
    }

    fn rows(dir: &Path, table: Table) -> Vec<Value> {
        fs::read_to_string(dir.join(table.file_name()))
            .expect("table file")
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect()
    }

    #[test]
    fn rows_carry_the_zakura_envelope_and_the_wall_clock() {
        let dir = scratch();
        let tracer = Tracer::open(dir.path(), "node-a").expect("open");
        let before = unix_micros();
        tracer.emit(
            Table::CommitState,
            event::COMMIT_START,
            || json!({"height": 7, "hash": "ab"}),
        );
        tracer.close().expect("close");
        let rows = rows(dir.path(), Table::CommitState);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["event"], "commit_start");
        assert_eq!(row["node"], "node-a");
        assert_eq!(row["height"], 7);
        assert_eq!(row["hash"], "ab");
        let pid = std::process::id().to_string();
        assert!(row["process_trace_id"]
            .as_str()
            .expect("string")
            .starts_with(&format!("{pid}-")));
        let Some(ts) = row["ts"].as_u64() else {
            panic!("ts is an integer");
        };
        assert!(ts < 60_000_000);
        let Some(unix_us) = row["unix_us"].as_u64() else {
            panic!("unix_us is an integer");
        };
        assert!(unix_us >= before);
        assert!(rows_empty(dir.path(), Table::BlockSync));
    }

    fn rows_empty(dir: &Path, table: Table) -> bool {
        rows(dir, table).is_empty()
    }

    #[test]
    fn a_full_channel_drops_and_counts_rows() {
        let dir = scratch();
        let (release, gate) = bounded(1);
        let options = Options {
            capacity: 4,
            flush_interval: Duration::from_secs(60),
        };
        let tracer = Tracer::start(dir.path(), "n", options, Some(gate)).expect("open");
        for i in 0..10 {
            tracer.emit(Table::Template, event::TEMPLATE_FULL, || json!({"i": i}));
        }
        assert_eq!(tracer.dropped(Table::Template), 6);
        assert_eq!(tracer.dropped(Table::CommitState), 0);
        release.send(()).expect("writer waits on the gate");
        tracer.close().expect("close");
        let written: Vec<u64> = rows(dir.path(), Table::Template)
            .iter()
            .map(|r| r["i"].as_u64().expect("i"))
            .collect();
        assert_eq!(written, vec![0, 1, 2, 3]);
        // A closed tracer drops every later row.
        tracer.emit(Table::Template, event::TEMPLATE_FULL, || json!({}));
        assert_eq!(tracer.dropped(Table::Template), 7);
    }

    #[test]
    fn the_writer_flushes_on_its_interval_without_a_close() {
        let dir = scratch();
        let options = Options {
            capacity: 16,
            flush_interval: Duration::from_millis(20),
        };
        let tracer = Tracer::open_with(dir.path(), "n", options).expect("open");
        tracer.emit(
            Table::BlockSync,
            event::BLOCK_RECEIVED,
            || json!({"hash": "cd"}),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if rows(dir.path(), Table::BlockSync).len() == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "row not flushed within 5 s");
            thread::sleep(Duration::from_millis(10));
        }
        tracer.close().expect("close");
    }

    #[test]
    fn reopening_appends() {
        let dir = scratch();
        for n in 0..2 {
            let tracer = Tracer::open(dir.path(), "n").expect("open");
            tracer.emit(Table::CommitState, event::COMMIT_FINISH, || json!({"n": n}));
            tracer.close().expect("close");
        }
        assert_eq!(rows(dir.path(), Table::CommitState).len(), 2);
    }

    #[test]
    fn a_disabled_tracer_writes_nothing() {
        let tracer = Tracer::disabled();
        let mut called = false;
        tracer.emit(Table::CommitState, event::COMMIT_START, || {
            called = true;
            json!({})
        });
        assert!(!called);
        assert_eq!(tracer.dropped(Table::CommitState), 0);
        tracer.close().expect("close is a no-op");
    }
}
