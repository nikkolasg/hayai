//! The writer thread of the wallet index: a bounded queue of committed blocks, the build of
//! their entries in parallel, one write batch for the blocks that wait.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;

use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use hayai_coins::Coin;
use hayai_trees::{IronwoodFrontier, OrchardFrontier, SaplingFrontier};
use hayai_wire::RawBlock;
use rayon::prelude::*;

use crate::{Delta, Error, WalletIndex};

/// Blocks in the queue of the writer, at most. A full queue makes the node wait. The bound
/// also limits one write batch. The memory of the queue is at most this number of blocks
/// with their spent coins.
pub const QUEUE_BLOCKS: usize = 64;

/// The note commitment frontiers before a block.
#[derive(Clone)]
pub struct TreesBefore {
    pub sapling: Arc<SaplingFrontier>,
    pub orchard: Arc<OrchardFrontier>,
    pub ironwood: Arc<IronwoodFrontier>,
}

/// A committed block for the index.
pub struct BlockJob {
    pub height: u32,
    pub hash: [u8; 32],
    pub parent: [u8; 32],
    pub raw: Arc<RawBlock>,
    /// The coin of each transparent input: `spent_coins[i][j]` is input `j` of transaction
    /// `i`. The coinbase has an empty list.
    pub spent_coins: Vec<Vec<Coin>>,
    pub trees_before: TreesBefore,
}

enum Msg {
    Block(BlockJob),
    Undo {
        height: u32,
        hash: [u8; 32],
    },
    Persist {
        prune_through: u32,
        /// The answer: the height of the synced tip.
        done: Sender<Result<u32, String>>,
    },
}

/// A sync that the writer runs in the background ([`IndexWriter::persist`]).
struct Pending {
    done: Receiver<Result<u32, String>>,
    /// The height below the lowest block that the node undid after the request.
    low: u32,
}

/// Counters of the writer, for the metrics of the node.
#[derive(Default)]
pub struct WriterStats {
    pub blocks: AtomicU64,
    pub batches: AtomicU64,
    /// Bytes of the write batches.
    pub bytes: AtomicU64,
    /// Time of the build of the entries, in microseconds.
    pub build_us: AtomicU64,
    /// Time of the writes to RocksDB, in microseconds.
    pub write_us: AtomicU64,
    /// Time that the node waited on a full queue, in microseconds.
    pub queue_wait_us: AtomicU64,
    /// Time that the node waited for a persist, in microseconds.
    pub persist_wait_us: AtomicU64,
    /// Time of the syncs in the writer thread (the removal of the undo records and the sync
    /// of the write-ahead log), in microseconds.
    pub sync_us: AtomicU64,
    /// Persists that waited for a new sync: the sync in the background did not hold the
    /// base.
    pub persist_stalls: AtomicU64,
}

/// The writer of the wallet index. Each method returns the error of the thread after a
/// failure: the node must stop then.
pub struct IndexWriter {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    failure: Arc<OnceLock<String>>,
    /// Set by [`IndexWriter::abandon`]: the thread writes nothing more.
    abort: Arc<AtomicBool>,
    /// The sync that the last persist started.
    pending: Option<Pending>,
    pub stats: Arc<WriterStats>,
}

impl IndexWriter {
    /// Starts the thread on `index`. The index must have a tip.
    pub fn spawn(index: Arc<WalletIndex>) -> Result<Self, Error> {
        let Some(tip) = index.tip()? else {
            return Err(Error::Chain("the wallet index has no tip".into()));
        };
        let (tx, rx) = bounded(QUEUE_BLOCKS);
        let failure = Arc::new(OnceLock::new());
        let stats = Arc::new(WriterStats::default());
        let abort = Arc::new(AtomicBool::new(false));
        let thread = {
            let (failure, stats, abort) = (failure.clone(), stats.clone(), abort.clone());
            std::thread::Builder::new()
                .name("wallet-index".into())
                .spawn(move || {
                    if let Err(e) = run(&index, tip, &rx, &stats, &abort) {
                        tracing::error!(error = %e, "wallet index writer stopped");
                        let _ = failure.set(e.to_string());
                        // A bounded channel keeps its messages after the drop of its
                        // receiver, and a queued persist would then wait without an end.
                        // The thread drops each message until the writer closes.
                        for msg in rx.iter() {
                            drop(msg);
                        }
                    }
                })
                .map_err(|e| Error::Chain(format!("wallet index thread: {e}")))?
        };
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            failure,
            abort,
            pending: None,
            stats,
        })
    }

    fn failed(&self) -> Error {
        Error::Chain(format!(
            "wallet index writer: {}",
            self.failure
                .get()
                .map_or("the thread stopped", String::as_str)
        ))
    }

    fn send(&self, msg: Msg) -> Result<(), Error> {
        let (Some(tx), None) = (&self.tx, self.failure.get()) else {
            return Err(self.failed());
        };
        tx.send(msg).map_err(|_| self.failed())
    }

    /// Queues the committed block `job`. Waits while the queue is full.
    pub fn apply(&self, job: BlockJob) -> Result<(), Error> {
        let started = Instant::now();
        self.send(Msg::Block(job))?;
        self.stats
            .queue_wait_us
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Queues the undo of the index tip, which must be the block `hash` of `height`.
    pub fn undo(&mut self, height: u32, hash: [u8; 32]) -> Result<(), Error> {
        if let Some(pending) = &mut self.pending {
            pending.low = pending.low.min(height.saturating_sub(1));
        }
        self.send(Msg::Undo { height, hash })
    }

    /// Returns when the durable index holds the block at `base` of the chain that the node
    /// sent, then starts the next sync in the background. Each sync first removes the
    /// undo records at or below `prune_through`.
    ///
    /// A sync makes durable the index of the time of its request: the thread handles the
    /// messages in order, and the write-ahead log holds each later write after the synced
    /// ones. After the request the node only adds blocks, or undoes blocks above `low`. So
    /// each durable state from the request on holds the blocks of the chain of the node
    /// up to the lower of the synced tip and `low`. When that height is at or above `base`,
    /// the call returns after the wait for the sync of the call before, which usually ended
    /// long before. Else it waits for a new sync.
    pub fn persist(&mut self, base: u32, prune_through: u32) -> Result<(), Error> {
        let started = Instant::now();
        let held = match self.pending.take() {
            Some(pending) => Some(self.wait(&pending)?.min(pending.low)),
            None => None,
        };
        match held {
            Some(height) if height >= base => {}
            _ => {
                self.stats.persist_stalls.fetch_add(1, Ordering::Relaxed);
                let tip = self.wait(&self.request(prune_through)?)?;
                if tip < base {
                    return Err(Error::Chain(format!(
                        "the index tip {tip} is below the base {base}"
                    )));
                }
            }
        }
        self.pending = Some(self.request(prune_through)?);
        self.stats
            .persist_wait_us
            .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn request(&self, prune_through: u32) -> Result<Pending, Error> {
        let (done, answer) = bounded(1);
        self.send(Msg::Persist {
            prune_through,
            done,
        })?;
        Ok(Pending {
            done: answer,
            low: u32::MAX,
        })
    }

    /// The height of the tip that the sync of `pending` made durable.
    fn wait(&self, pending: &Pending) -> Result<u32, Error> {
        match pending.done.recv() {
            Ok(outcome) => outcome.map_err(Error::Chain),
            Err(_) => Err(self.failed()),
        }
    }

    /// Writes each queued block and stops the thread.
    pub fn close(mut self) -> Result<(), Error> {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let Ok(()) = thread.join() else {
                return Err(Error::Chain("the wallet index thread panicked".into()));
            };
        }
        match self.failure.get() {
            Some(_) => Err(self.failed()),
            None => Ok(()),
        }
    }
}

impl IndexWriter {
    /// Fault injection for the tests of a crash: the thread stops without a write of the
    /// queued blocks.
    pub fn abandon(mut self) {
        self.abort.store(true, Ordering::Release);
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for IndexWriter {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    index: &WalletIndex,
    mut tip: (u32, [u8; 32]),
    rx: &Receiver<Msg>,
    stats: &WriterStats,
    abort: &AtomicBool,
) -> Result<(), Error> {
    // Reused for each batch: at most QUEUE_BLOCKS jobs and deltas.
    let mut jobs: Vec<BlockJob> = Vec::with_capacity(QUEUE_BLOCKS);
    let mut deltas: Vec<Delta> = Vec::with_capacity(QUEUE_BLOCKS);
    let mut size_hint = 4096;
    let mut next = None;
    loop {
        let msg = match next.take() {
            Some(msg) => msg,
            None => match rx.recv() {
                Ok(msg) => msg,
                Err(_) => return Ok(()),
            },
        };
        if abort.load(Ordering::Acquire) {
            return Ok(());
        }
        match msg {
            Msg::Block(job) => {
                jobs.push(job);
                while jobs.len() < QUEUE_BLOCKS {
                    match rx.try_recv() {
                        Ok(Msg::Block(job)) => jobs.push(job),
                        Ok(other) => {
                            next = Some(other);
                            break;
                        }
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                size_hint =
                    write_blocks(index, &mut jobs, &mut deltas, &mut tip, size_hint, stats)?;
            }
            Msg::Undo { height, hash } => tip = index.undo_tip(height, &hash)?,
            Msg::Persist {
                prune_through,
                done,
            } => {
                let started = Instant::now();
                let result = index.persist(prune_through);
                stats
                    .sync_us
                    .fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                let _ = done.send(result.as_ref().map(|()| tip.0).map_err(ToString::to_string));
                result?;
            }
        }
    }
}

/// Builds the entries of `jobs` in parallel and writes them in one batch. Returns the size
/// of the batch, the size hint of the next one.
fn write_blocks(
    index: &WalletIndex,
    jobs: &mut Vec<BlockJob>,
    deltas: &mut Vec<Delta>,
    tip: &mut (u32, [u8; 32]),
    size_hint: usize,
    stats: &WriterStats,
) -> Result<usize, Error> {
    for job in jobs.iter() {
        if (job.height, job.parent) != (tip.0 + 1, tip.1) {
            return Err(Error::Chain(format!(
                "block {} does not extend the index tip {}",
                job.height, tip.0
            )));
        }
        *tip = (job.height, job.hash);
    }
    if deltas.len() < jobs.len() {
        deltas.resize_with(jobs.len(), Delta::default);
    }
    let started = Instant::now();
    let deltas = &mut deltas[..jobs.len()];
    deltas
        .par_iter_mut()
        .zip(jobs.par_iter())
        .try_for_each(|(delta, job)| delta.build(job))?;
    let built = Instant::now();
    let bytes = index.write(deltas, size_hint)?;
    stats.build_us.fetch_add(
        built.duration_since(started).as_micros() as u64,
        Ordering::Relaxed,
    );
    stats
        .write_us
        .fetch_add(built.elapsed().as_micros() as u64, Ordering::Relaxed);
    stats.blocks.fetch_add(jobs.len() as u64, Ordering::Relaxed);
    stats.batches.fetch_add(1, Ordering::Relaxed);
    stats.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    jobs.clear();
    Ok(bytes)
}
