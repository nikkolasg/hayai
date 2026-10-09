//! System-resource measurements for the `sysbench` binary: CPU time, page faults and context
//! switches from `getrusage`, heap traffic from a counting global allocator, hardware
//! counters from `perf_event_open`, disk traffic from `/proc/self/io` and from the size of a
//! scratch directory, and the child's high-water RSS from `wait4`.
//!
//! Sources of each number:
//! - `cpu_user_ms`, `cpu_sys_ms`, `minor_faults`, `major_faults`, `ctx_voluntary`,
//!   `ctx_involuntary`: deltas of `getrusage(RUSAGE_SELF)` around each timed region, all
//!   threads of the process included.
//! - `max_rss_kb`: `ru_maxrss` of the child's `wait4` rusage, a high-water mark over the whole
//!   child (fixtures and warm-up included), which is why every (scenario, impl) pair runs in
//!   a fresh child process.
//! - `alloc_bytes`, `alloc_count`, `peak_heap_bytes`: [`CountingAlloc`], a wrapper around the
//!   base allocator (glibc malloc or mimalloc, a build feature) that the binary installs as
//!   `#[global_allocator]`, counting on per-thread stripes so that the counters themselves
//!   do not serialise the threads; `peak_heap_bytes` is the largest live heap size seen
//!   during a timed region, baseline included, exact to within 64 KiB per thread.
//! - `cycles` .. `branch_misses`: one `perf_event_open` counter per event, counting mode,
//!   user space only (`perf_event_paranoid` 2 allows no more), `inherit = 1` so threads
//!   spawned after the counters are opened are counted; counts are scaled by
//!   `time_enabled / time_running` when the kernel multiplexes them.
//! - `io_write_bytes`, `io_read_bytes`: deltas of `/proc/self/io` `write_bytes`/`read_bytes`
//!   (bytes the process caused to be sent to or fetched from the storage layer).
//! - `scratch_bytes`: size of the scenario's scratch directory after the timed iterations
//!   minus before.
//! - `blocked_ms`: `wall − user − sys` of the timed regions; positive when the process waited
//!   (I/O, page faults, scheduling), negative when more than one thread was running, so it is
//!   only a blocked time for single-threaded scenarios.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- counting allocator

/// Counter stripes. Each thread picks one stripe at its first allocation and only ever
/// updates that stripe, so the read-modify-writes stay on a cache line no other core writes
/// (barring the rare collision of two threads on one stripe). A single set of process-wide
/// counters would be hit by every core at once: measured on a 32-thread parse of a 6,500
/// transaction block, four shared atomics per allocation turned 2 ms of wall time into
/// 7.5 ms and 33 ms of CPU time into 177 ms.
const STRIPES: usize = 256;

/// Net live-byte change a stripe accumulates before it is folded into [`LIVE_BYTES`] and
/// compared against [`PEAK_BYTES`]; the peak is therefore exact to within this many bytes per
/// thread.
const LIVE_FLUSH_BYTES: i64 = 64 << 10;

#[repr(align(128))]
struct Stripe {
    alloc_bytes: AtomicU64,
    alloc_count: AtomicU64,
    pending_live: AtomicI64,
}

#[allow(clippy::declare_interior_mutable_const)]
const EMPTY_STRIPE: Stripe = Stripe {
    alloc_bytes: AtomicU64::new(0),
    alloc_count: AtomicU64::new(0),
    pending_live: AtomicI64::new(0),
};

static STRIPE_TABLE: [Stripe; STRIPES] = [EMPTY_STRIPE; STRIPES];
static NEXT_STRIPE: AtomicU32 = AtomicU32::new(0);
/// Live bytes as last flushed by the stripes (exact to within `LIVE_FLUSH_BYTES` per thread).
static LIVE_BYTES: AtomicI64 = AtomicI64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// Index of this thread's stripe; `u32::MAX` until the first allocation. Const-initialised
    /// and without a destructor, so touching it never allocates.
    static MY_STRIPE: Cell<u32> = const { Cell::new(u32::MAX) };
}

fn my_stripe() -> &'static Stripe {
    let index = MY_STRIPE
        .try_with(|slot| {
            if slot.get() == u32::MAX {
                slot.set(NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) % STRIPES as u32);
            }
            slot.get()
        })
        .unwrap_or(0);
    &STRIPE_TABLE[index as usize]
}

fn live_change(stripe: &Stripe, delta: i64) {
    let pending = stripe.pending_live.fetch_add(delta, Ordering::Relaxed) + delta;
    if pending.abs() < LIVE_FLUSH_BYTES {
        return;
    }
    let flushed = stripe.pending_live.swap(0, Ordering::Relaxed);
    let live = LIVE_BYTES.fetch_add(flushed, Ordering::Relaxed) + flushed;
    PEAK_BYTES.fetch_max(live.max(0) as u64, Ordering::Relaxed);
}

/// A base allocator with allocation counters. Installed by the `sysbench` binary only:
/// `#[global_allocator] static GLOBAL: CountingAlloc = CountingAlloc(System);` (or with
/// `mimalloc::MiMalloc` as the base). The counters are process-wide statics so that
/// [`heap_snapshot`] works whether or not it is installed (zeros when it is not).
pub struct CountingAlloc<A: GlobalAlloc = System>(pub A);

impl<A: GlobalAlloc> CountingAlloc<A> {
    fn on_alloc(size: usize) {
        let stripe = my_stripe();
        stripe.alloc_bytes.fetch_add(size as u64, Ordering::Relaxed);
        stripe.alloc_count.fetch_add(1, Ordering::Relaxed);
        live_change(stripe, size as i64);
    }

    fn on_free(size: usize) {
        live_change(my_stripe(), -(size as i64));
    }
}

// SAFETY: every method forwards to the base allocator with the same arguments and only
// updates counters around the call.
unsafe impl<A: GlobalAlloc> GlobalAlloc for CountingAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = self.0.alloc(layout);
        if !p.is_null() {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = self.0.alloc_zeroed(layout);
        if !p.is_null() {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.dealloc(ptr, layout);
        Self::on_free(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = self.0.realloc(ptr, layout, new_size);
        if !p.is_null() {
            Self::on_free(layout.size());
            Self::on_alloc(new_size);
        }
        p
    }
}

/// Heap counters at one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapSnapshot {
    /// Bytes allocated since process start (frees not subtracted).
    pub alloc_bytes: u64,
    /// Allocations since process start (a realloc counts as one).
    pub alloc_count: u64,
    /// Bytes currently allocated.
    pub live_bytes: u64,
}

pub fn heap_snapshot() -> HeapSnapshot {
    let mut alloc_bytes = 0;
    let mut alloc_count = 0;
    let mut pending = 0i64;
    for stripe in &STRIPE_TABLE {
        alloc_bytes += stripe.alloc_bytes.load(Ordering::Relaxed);
        alloc_count += stripe.alloc_count.load(Ordering::Relaxed);
        pending += stripe.pending_live.load(Ordering::Relaxed);
    }
    HeapSnapshot {
        alloc_bytes,
        alloc_count,
        live_bytes: (LIVE_BYTES.load(Ordering::Relaxed) + pending).max(0) as u64,
    }
}

/// Folds every stripe's pending change into the live count and returns the live size, exact
/// at this instant.
fn fold_live() -> u64 {
    let mut pending = 0i64;
    for stripe in &STRIPE_TABLE {
        pending += stripe.pending_live.swap(0, Ordering::Relaxed);
    }
    (LIVE_BYTES.fetch_add(pending, Ordering::Relaxed) + pending).max(0) as u64
}

/// Starts a new peak measurement at the current live size and returns the previous peak.
pub fn heap_reset_peak() -> u64 {
    let live = fold_live();
    PEAK_BYTES.swap(live, Ordering::Relaxed)
}

/// The largest live size since the last reset, the current live size included.
pub fn heap_peak() -> u64 {
    let live = fold_live();
    PEAK_BYTES.fetch_max(live, Ordering::Relaxed).max(live)
}

// ---------------------------------------------------------------- rusage

/// The fields of `struct rusage` this harness uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rusage {
    pub user: Duration,
    pub sys: Duration,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub ctx_voluntary: u64,
    pub ctx_involuntary: u64,
    /// High-water resident set size in KiB (Linux `ru_maxrss` unit).
    pub max_rss_kb: u64,
}

impl Rusage {
    pub fn from_libc(ru: &libc::rusage) -> Rusage {
        Rusage {
            user: timeval(ru.ru_utime),
            sys: timeval(ru.ru_stime),
            minor_faults: ru.ru_minflt as u64,
            major_faults: ru.ru_majflt as u64,
            ctx_voluntary: ru.ru_nvcsw as u64,
            ctx_involuntary: ru.ru_nivcsw as u64,
            max_rss_kb: ru.ru_maxrss as u64,
        }
    }

    /// `getrusage(RUSAGE_SELF)`: every thread of the calling process.
    pub fn of_self() -> io::Result<Rusage> {
        // SAFETY: a zeroed `rusage` is a valid out-parameter for getrusage.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `ru` is a valid, writable rusage.
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Rusage::from_libc(&ru))
    }

    /// `self − earlier` field by field; `max_rss_kb` is kept from `self` (it is a high-water
    /// mark, not a counter).
    pub fn delta(&self, earlier: &Rusage) -> Rusage {
        Rusage {
            user: self.user.saturating_sub(earlier.user),
            sys: self.sys.saturating_sub(earlier.sys),
            minor_faults: self.minor_faults - earlier.minor_faults,
            major_faults: self.major_faults - earlier.major_faults,
            ctx_voluntary: self.ctx_voluntary - earlier.ctx_voluntary,
            ctx_involuntary: self.ctx_involuntary - earlier.ctx_involuntary,
            max_rss_kb: self.max_rss_kb,
        }
    }

    fn add(&mut self, d: &Rusage) {
        self.user += d.user;
        self.sys += d.sys;
        self.minor_faults += d.minor_faults;
        self.major_faults += d.major_faults;
        self.ctx_voluntary += d.ctx_voluntary;
        self.ctx_involuntary += d.ctx_involuntary;
        self.max_rss_kb = self.max_rss_kb.max(d.max_rss_kb);
    }
}

fn timeval(tv: libc::timeval) -> Duration {
    Duration::new(tv.tv_sec as u64, (tv.tv_usec as u32) * 1_000)
}

/// Exit status and rusage of a child process, collected with `wait4`. The caller must have
/// drained the child's pipes first.
pub fn wait4_rusage(pid: u32) -> io::Result<(i32, Rusage)> {
    let mut status: libc::c_int = 0;
    // SAFETY: a zeroed `rusage` is a valid out-parameter for wait4.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `status` and `ru` are valid, writable out-parameters.
    let rc = unsafe { libc::wait4(pid as libc::pid_t, &mut status, 0, &mut ru) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -libc::WTERMSIG(status)
    };
    Ok((code, Rusage::from_libc(&ru)))
}

// ---------------------------------------------------------------- /proc/self/io

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcIo {
    pub read_bytes: u64,
    pub write_bytes: u64,
}

impl ProcIo {
    pub fn of_self() -> io::Result<ProcIo> {
        let text = fs::read_to_string("/proc/self/io")?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> io::Result<ProcIo> {
        let mut io_ = ProcIo::default();
        let mut seen = (false, false);
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let field = match key {
                "read_bytes" => &mut io_.read_bytes,
                "write_bytes" => &mut io_.write_bytes,
                _ => continue,
            };
            *field = value
                .trim()
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{key}: {e}")))?;
            match key {
                "read_bytes" => seen.0 = true,
                _ => seen.1 = true,
            }
        }
        let (true, true) = seen else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "/proc/self/io without read_bytes and write_bytes",
            ));
        };
        Ok(io_)
    }

    fn delta(&self, earlier: &ProcIo) -> ProcIo {
        ProcIo {
            read_bytes: self.read_bytes - earlier.read_bytes,
            write_bytes: self.write_bytes - earlier.write_bytes,
        }
    }
}

/// Total size of the regular files under `path`, recursively; 0 when the directory does not
/// exist (yet).
pub fn dir_size(path: &Path) -> io::Result<u64> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut total = 0;
    for entry in entries {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

// ---------------------------------------------------------------- hardware counters

/// The hardware events `sysbench` reads, in the order of the JSON fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwEvent {
    Cycles,
    Instructions,
    CacheRefs,
    CacheMisses,
    LlcLoads,
    LlcMisses,
    BranchMisses,
}

impl HwEvent {
    pub const ALL: [HwEvent; 7] = [
        HwEvent::Cycles,
        HwEvent::Instructions,
        HwEvent::CacheRefs,
        HwEvent::CacheMisses,
        HwEvent::LlcLoads,
        HwEvent::LlcMisses,
        HwEvent::BranchMisses,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HwEvent::Cycles => "cycles",
            HwEvent::Instructions => "instructions",
            HwEvent::CacheRefs => "cache_refs",
            HwEvent::CacheMisses => "cache_misses",
            HwEvent::LlcLoads => "llc_loads",
            HwEvent::LlcMisses => "llc_misses",
            HwEvent::BranchMisses => "branch_misses",
        }
    }
}

/// Counts per event; `None` where the PMU does not provide the event (the kernel answers
/// `ENOENT` at open, as AMD cores do for the generic LLC load events) or the open failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwCounts {
    pub cycles: Option<u64>,
    pub instructions: Option<u64>,
    pub cache_refs: Option<u64>,
    pub cache_misses: Option<u64>,
    pub llc_loads: Option<u64>,
    pub llc_misses: Option<u64>,
    pub branch_misses: Option<u64>,
}

impl HwCounts {
    fn slot(&mut self, event: HwEvent) -> &mut Option<u64> {
        match event {
            HwEvent::Cycles => &mut self.cycles,
            HwEvent::Instructions => &mut self.instructions,
            HwEvent::CacheRefs => &mut self.cache_refs,
            HwEvent::CacheMisses => &mut self.cache_misses,
            HwEvent::LlcLoads => &mut self.llc_loads,
            HwEvent::LlcMisses => &mut self.llc_misses,
            HwEvent::BranchMisses => &mut self.branch_misses,
        }
    }

    fn add(&mut self, event: HwEvent, count: u64) {
        let slot = self.slot(event);
        *slot = Some(slot.unwrap_or(0) + count);
    }

    /// Every event is available.
    pub fn complete(&self) -> bool {
        matches!(
            (
                self.cycles,
                self.instructions,
                self.cache_refs,
                self.cache_misses,
                self.llc_loads,
                self.llc_misses,
                self.branch_misses,
            ),
            (
                Some(_),
                Some(_),
                Some(_),
                Some(_),
                Some(_),
                Some(_),
                Some(_)
            )
        )
    }
}

/// One `perf_event_open` counter per available event, process-wide (`inherit`), user space
/// only; the events the kernel refused, with the error, alongside. On a system without
/// `perf_event_open` every event is unavailable.
pub struct HwCounters {
    counters: Vec<(HwEvent, counter::Counter)>,
    unavailable: Vec<(HwEvent, io::Error)>,
}

impl HwCounters {
    /// Opens the counters disabled. Must run before the process spawns the threads it wants
    /// counted: `inherit` only covers threads created after the counter exists.
    pub fn open() -> HwCounters {
        let mut counters = Vec::new();
        let mut unavailable = Vec::new();
        for event in HwEvent::ALL {
            match counter::open(event) {
                Ok(c) => counters.push((event, c)),
                Err(e) => unavailable.push((event, e)),
            }
        }
        HwCounters {
            counters,
            unavailable,
        }
    }

    /// `event: error` for every event that could not be opened, or `None` when all could.
    pub fn unavailable(&self) -> Option<String> {
        if self.unavailable.is_empty() {
            return None;
        }
        Some(
            self.unavailable
                .iter()
                .map(|(e, err)| format!("{}: {err}", e.name()))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    fn start(&mut self) -> io::Result<()> {
        for (_, c) in &mut self.counters {
            counter::start(c)?;
        }
        Ok(())
    }

    /// Disables the counters and adds their multiplexing-scaled counts to `total`.
    fn stop_into(&mut self, total: &mut HwCounts) -> io::Result<()> {
        for (event, c) in &mut self.counters {
            total.add(*event, counter::stop(c)?);
        }
        Ok(())
    }
}

/// The counters of `perf_event_open`.
#[cfg(target_os = "linux")]
mod counter {
    use std::io;

    use perf_event::events::{Cache, CacheId, CacheOp, CacheResult, Hardware};
    use perf_event::Builder;
    pub use perf_event::Counter;

    use super::HwEvent;

    pub fn open(event: HwEvent) -> io::Result<Counter> {
        let llc = |operation, result| Cache {
            which: CacheId::LL,
            operation,
            result,
        };
        let mut builder = match event {
            HwEvent::Cycles => Builder::new(Hardware::CPU_CYCLES),
            HwEvent::Instructions => Builder::new(Hardware::INSTRUCTIONS),
            HwEvent::CacheRefs => Builder::new(Hardware::CACHE_REFERENCES),
            HwEvent::CacheMisses => Builder::new(Hardware::CACHE_MISSES),
            HwEvent::LlcLoads => Builder::new(llc(CacheOp::READ, CacheResult::ACCESS)),
            HwEvent::LlcMisses => Builder::new(llc(CacheOp::READ, CacheResult::MISS)),
            HwEvent::BranchMisses => Builder::new(Hardware::BRANCH_MISSES),
        };
        builder
            .observe_self()
            .any_cpu()
            .inherit(true)
            .exclude_kernel(true)
            .exclude_hv(true)
            .enabled(false)
            .build()
    }

    pub fn start(c: &mut Counter) -> io::Result<()> {
        c.reset()?;
        c.enable()
    }

    /// Disables `c` and returns its count, scaled for multiplexing.
    pub fn stop(c: &mut Counter) -> io::Result<u64> {
        c.disable()?;
        let data = c.read_full()?;
        let count = data.count();
        Ok(match (data.time_enabled(), data.time_running()) {
            (Some(enabled), Some(running)) if !running.is_zero() && running < enabled => {
                (count as f64 * enabled.as_secs_f64() / running.as_secs_f64()) as u64
            }
            _ => count,
        })
    }
}

/// No hardware counters: the system has no `perf_event_open`.
#[cfg(not(target_os = "linux"))]
mod counter {
    use std::io;

    use super::HwEvent;

    /// No value: [`open`] never returns a counter.
    pub enum Counter {}

    pub fn open(_: HwEvent) -> io::Result<Counter> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "perf_event_open is a Linux system call",
        ))
    }

    pub fn start(c: &mut Counter) -> io::Result<()> {
        match *c {}
    }

    pub fn stop(c: &mut Counter) -> io::Result<u64> {
        match *c {}
    }
}

// ---------------------------------------------------------------- meter

/// Accumulates the measurements of the timed regions of one (scenario, impl) child.
pub struct Meter {
    hw: HwCounters,
    recording: bool,
    walls: Vec<Duration>,
    rusage: Rusage,
    io_: ProcIo,
    heap: HeapSnapshot,
    peak_heap: u64,
    hw_total: HwCounts,
}

/// What one child measured over its timed iterations: the per-iteration median wall time and
/// the sums of everything else.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Measured {
    pub iterations: u64,
    pub wall_ms_median: f64,
    pub wall_ms_total: f64,
    pub rusage_user_ms: f64,
    pub rusage_sys_ms: f64,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub ctx_voluntary: u64,
    pub ctx_involuntary: u64,
    pub alloc_bytes: u64,
    pub alloc_count: u64,
    pub peak_heap_bytes: u64,
    pub hw: HwCounts,
    /// The events that could not be opened, with the kernel's error.
    pub hw_unavailable: Option<String>,
    pub io_write_bytes: u64,
    pub io_read_bytes: u64,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

impl Meter {
    /// Opens the hardware counters (the unavailable ones are recorded as such).
    pub fn new() -> Meter {
        Meter {
            hw: HwCounters::open(),
            recording: true,
            walls: Vec::new(),
            rusage: Rusage::default(),
            io_: ProcIo::default(),
            heap: HeapSnapshot {
                alloc_bytes: 0,
                alloc_count: 0,
                live_bytes: 0,
            },
            peak_heap: 0,
            hw_total: HwCounts::default(),
        }
    }

    /// Whether [`Meter::timed`] records; off during warm-up.
    pub fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }

    /// Runs `f` and, when recording, accumulates its wall time, rusage, I/O, heap and
    /// hardware-counter deltas.
    pub fn timed<R>(&mut self, f: impl FnOnce() -> R) -> R {
        if !self.recording {
            return f();
        }
        let io_before = ProcIo::of_self().expect("/proc/self/io");
        let ru_before = Rusage::of_self().expect("getrusage");
        let heap_before = heap_snapshot();
        heap_reset_peak();
        self.hw.start().expect("perf counters enable");
        let start = Instant::now();
        let out = f();
        let wall = start.elapsed();
        self.hw
            .stop_into(&mut self.hw_total)
            .expect("perf counters read");
        let heap_after = heap_snapshot();
        let ru_after = Rusage::of_self().expect("getrusage");
        let io_after = ProcIo::of_self().expect("/proc/self/io");

        self.walls.push(wall);
        self.rusage.add(&ru_after.delta(&ru_before));
        let io_delta = io_after.delta(&io_before);
        self.io_.read_bytes += io_delta.read_bytes;
        self.io_.write_bytes += io_delta.write_bytes;
        self.heap.alloc_bytes += heap_after.alloc_bytes - heap_before.alloc_bytes;
        self.heap.alloc_count += heap_after.alloc_count - heap_before.alloc_count;
        self.peak_heap = self.peak_heap.max(heap_peak());
        out
    }

    pub fn finish(mut self) -> Measured {
        self.walls.sort();
        let n = self.walls.len();
        let median = match n {
            0 => 0.0,
            _ if n % 2 == 1 => self.walls[n / 2].as_secs_f64(),
            _ => (self.walls[n / 2 - 1].as_secs_f64() + self.walls[n / 2].as_secs_f64()) / 2.0,
        };
        let total: Duration = self.walls.iter().sum();
        Measured {
            iterations: n as u64,
            wall_ms_median: median * 1e3,
            wall_ms_total: total.as_secs_f64() * 1e3,
            rusage_user_ms: self.rusage.user.as_secs_f64() * 1e3,
            rusage_sys_ms: self.rusage.sys.as_secs_f64() * 1e3,
            minor_faults: self.rusage.minor_faults,
            major_faults: self.rusage.major_faults,
            ctx_voluntary: self.rusage.ctx_voluntary,
            ctx_involuntary: self.rusage.ctx_involuntary,
            alloc_bytes: self.heap.alloc_bytes,
            alloc_count: self.heap.alloc_count,
            peak_heap_bytes: self.peak_heap,
            hw: self.hw_total,
            hw_unavailable: self.hw.unavailable(),
            io_write_bytes: self.io_.write_bytes,
            io_read_bytes: self.io_.read_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rusage_delta_subtracts_counters_and_keeps_high_water_mark() {
        let earlier = Rusage {
            user: Duration::from_millis(100),
            sys: Duration::from_millis(10),
            minor_faults: 5,
            major_faults: 1,
            ctx_voluntary: 7,
            ctx_involuntary: 2,
            max_rss_kb: 1_000,
        };
        let later = Rusage {
            user: Duration::from_millis(350),
            sys: Duration::from_millis(40),
            minor_faults: 25,
            major_faults: 1,
            ctx_voluntary: 9,
            ctx_involuntary: 6,
            max_rss_kb: 2_500,
        };
        assert_eq!(
            later.delta(&earlier),
            Rusage {
                user: Duration::from_millis(250),
                sys: Duration::from_millis(30),
                minor_faults: 20,
                major_faults: 0,
                ctx_voluntary: 2,
                ctx_involuntary: 4,
                max_rss_kb: 2_500,
            }
        );
    }

    #[test]
    fn rusage_of_self_grows_with_work() {
        let before = Rusage::of_self().expect("getrusage");
        let mut acc = 0u64;
        for i in 0..5_000_000u64 {
            acc = acc.wrapping_mul(6364136223846793005).wrapping_add(i);
        }
        assert_ne!(acc, 1);
        let after = Rusage::of_self().expect("getrusage");
        let d = after.delta(&before);
        assert!(d.user + d.sys > Duration::ZERO, "{d:?}");
        assert!(after.max_rss_kb > 0);
    }

    #[test]
    fn counting_allocator_tracks_bytes_count_and_peak() {
        let a = CountingAlloc(System);
        let layout = Layout::from_size_align(1 << 20, 8).expect("layout");
        let before = heap_snapshot();
        heap_reset_peak();
        // SAFETY: valid layout, every pointer freed with the layout it was allocated with.
        unsafe {
            let p = a.alloc(layout);
            assert!(!p.is_null());
            let q = a.alloc_zeroed(layout);
            assert!(!q.is_null());
            let during = heap_snapshot();
            assert!(during.alloc_bytes - before.alloc_bytes >= 2 << 20);
            assert!(during.alloc_count - before.alloc_count >= 2);
            assert!(during.live_bytes >= before.live_bytes + (2 << 20));
            // Two 1 MiB allocations exceed the per-thread flush threshold, so the peak saw
            // at least the first of them.
            assert!(heap_peak() >= before.live_bytes + (1 << 20));
            let r = a.realloc(p, layout, 2 << 20);
            assert!(!r.is_null());
            let grown = heap_snapshot();
            assert!(grown.alloc_bytes - during.alloc_bytes >= 2 << 20);
            assert_eq!(grown.alloc_count - during.alloc_count, 1);
            a.dealloc(r, Layout::from_size_align(2 << 20, 8).expect("layout"));
            a.dealloc(q, layout);
        }
        let after = heap_snapshot();
        // Other tests may be allocating on other threads; totals never decrease.
        assert!(after.alloc_bytes >= before.alloc_bytes + (4 << 20));
        assert!(after.alloc_count >= before.alloc_count + 3);
    }

    #[test]
    fn proc_io_parses_the_two_byte_counters() {
        let text = "rchar: 1\nwchar: 2\nsyscr: 3\nsyscw: 4\nread_bytes: 4096\nwrite_bytes: 8192\ncancelled_write_bytes: 0\n";
        assert_eq!(
            ProcIo::parse(text).expect("parses"),
            ProcIo {
                read_bytes: 4096,
                write_bytes: 8192
            }
        );
        let Err(_) = ProcIo::parse("rchar: 1\n") else {
            panic!("missing counters are an error");
        };
        let Ok(_) = ProcIo::of_self() else {
            panic!("/proc/self/io readable on Linux");
        };
    }

    #[test]
    fn dir_size_sums_regular_files_recursively() {
        let dir = crate::scratch_dir();
        assert_eq!(dir_size(dir.path()).expect("size"), 0);
        fs::write(dir.path().join("a"), [0u8; 100]).expect("write");
        fs::create_dir(dir.path().join("sub")).expect("mkdir");
        fs::write(dir.path().join("sub/b"), [0u8; 50]).expect("write");
        assert_eq!(dir_size(dir.path()).expect("size"), 150);
        assert_eq!(dir_size(&dir.path().join("absent")).expect("size"), 0);
    }

    #[test]
    fn meter_records_only_while_recording_and_reports_median() {
        let mut meter = Meter::new();
        meter.set_recording(false);
        meter.timed(|| std::thread::sleep(Duration::from_millis(5)));
        meter.set_recording(true);
        for ms in [1u64, 9, 3] {
            let v: Vec<u8> = meter.timed(|| {
                std::thread::sleep(Duration::from_millis(ms));
                vec![0u8; 1 << 16]
            });
            assert_eq!(v.len(), 1 << 16);
        }
        let opened: Vec<HwEvent> = meter.hw.counters.iter().map(|(e, _)| *e).collect();
        let m = meter.finish();
        assert_eq!(m.iterations, 3);
        assert!(m.wall_ms_median >= 3.0 && m.wall_ms_median < 9.0, "{m:?}");
        assert!(m.wall_ms_total >= 13.0);
        // An opened counter has a count, a refused one has none and is named in the error.
        let mut counts = m.hw;
        for event in HwEvent::ALL {
            match (opened.contains(&event), *counts.slot(event)) {
                (true, Some(_)) => {}
                (false, None) => {
                    let Some(unavailable) = m.hw_unavailable.as_deref() else {
                        panic!("{event:?} refused but no error recorded");
                    };
                    assert!(unavailable.contains(event.name()), "{unavailable}");
                }
                other => panic!("{event:?}: {other:?}"),
            }
        }
        assert_eq!(counts.complete(), opened.len() == HwEvent::ALL.len());
    }
}
