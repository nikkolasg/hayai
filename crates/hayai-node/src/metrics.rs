//! The node's metrics. Names with the meaning of a Zakura metric are Zakura's names as its
//! Prometheus exporter writes them (dots become underscores); the others start with
//! `hayai_`.

use std::sync::Arc;
use std::time::Duration;

use hayai_metrics::DURATION_BUCKETS;
use hayai_metrics::{Counter, FloatCounter, Gauge, Histogram, Registry};
use hayai_net::RelayCounters;
use hayai_trace::{Table, Tracer};
use hayai_validate::Timings;

use crate::config::{Backend, Config, Mode};
use crate::process::ProcessStats;

/// Stage names of `hayai_validate_stage_duration_seconds`, in `Timings` order.
pub const STAGES: [&str; 10] = [
    "parse",
    "roots",
    "lookup",
    "prepare_unknown",
    "scripts",
    "shielded",
    "context",
    "trees",
    "history",
    "total",
];

pub fn stage_durations(t: &Timings) -> [Duration; 10] {
    [
        t.parse,
        t.roots,
        t.lookup,
        t.prepare_unknown,
        t.scripts,
        t.shielded,
        t.context,
        t.trees,
        t.history,
        t.total,
    ]
}

/// The values of `source` in `hayai_last_block_source`: the `origin` of the `commit_start`
/// row. `download`: the block download asked a peer for the block. `compact`: the compact
/// relay completed it. `legacy`: a legacy peer sent it without a request of the download.
/// `local`: a block of this node (`submitblock`, `generate`). `stored`: the block store
/// (a block that is on the best chain again). `upstream_rpc`: shadow mode.
pub const SOURCES: [&str; 6] = [
    "download",
    "compact",
    "legacy",
    "local",
    "stored",
    "upstream_rpc",
];

/// The seconds of a duration in whole microseconds: a gauge of a block has the value of
/// the `_us` field of its trace row.
pub fn seconds(micros: u64) -> f64 {
    micros as f64 / 1e6
}

/// The measurements of one committed block. Each duration is in microseconds and comes
/// from one clock reading, which the trace row of the block also has.
///
/// Points on the clock, in order:
/// - received: the node has each byte of the block. For `download` and `legacy` it is the
///   arrival of the `block` message, before the parse. For `compact` and `local` it is
///   the arrival of the complete block at the driver queue (after the reconstruction).
///   For `stored` and `upstream_rpc` it is the read of the block by the driver.
/// - start: the driver takes the block (`commit_start` row).
/// - validated: the block passed each consensus check (`block_validated` row).
/// - committed: the block is the tip of the chain, it is in the block store, and the
///   prepared store has the new tip (`commit_finish` row).
pub struct LastBlock<'a> {
    pub height: u32,
    /// The last 12 hexadecimal digits of the block hash as the RPC server prints it.
    pub hash_suffix: u64,
    /// The `origin` of the `commit_start` row: one of [`SOURCES`].
    pub source: &'a str,
    pub size_bytes: usize,
    pub transactions: usize,
    /// Received to validated (`since_received_us` of `block_validated`).
    pub received_to_validated_us: u64,
    /// Start to validated (`validation_us` of `block_validated`).
    pub validation_us: u64,
    /// Validated to committed (`commit_us` of `commit_finish`).
    pub commit_us: u64,
    /// Received to committed (`received_to_commit_us` of `commit_finish`). The stop is
    /// the reading of the clock after `TipWatch::set`.
    pub received_to_committed_us: u64,
    /// The stages `context`, `trees` and `history` plus the push of the layer on the
    /// chain (`contextual_commit_us` of `block_validated`). Zakura measures the same work
    /// in `state_contextual_total_duration_seconds`.
    pub contextual_commit_us: u64,
}

/// The last 12 hexadecimal digits of a block hash in the form of the RPC server, as an
/// integer. A 64-bit float holds it exactly. The first digits of a hash are zeros (proof
/// of work), so the last digits tell two blocks of one height apart.
pub fn hash_suffix(hash_hex: &str) -> u64 {
    let start = hash_hex.len().saturating_sub(12);
    u64::from_str_radix(&hash_hex[start..], 16).unwrap_or(0)
}

/// Whole microseconds of a duration, as the `_us` fields of the trace rows.
pub fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

pub struct NodeMetrics {
    pub verified_height: Arc<Gauge>,
    pub committed_height: Arc<Gauge>,
    /// Height of the newest block whose coins the node flushed to the coins store on disk.
    pub finalized_height: Arc<Gauge>,
    /// Height of the base: the newest block whose layer is merged into the finalized state
    /// in memory.
    pub base_height: Arc<Gauge>,
    /// The counters of the writer of the wallet index (`[state] wallet_index`), as totals
    /// since the start: blocks, bytes of the write batches, and seconds of the build, the
    /// writes, the waits of the driver on a full queue and on a persist.
    pub wallet_index: [Arc<Gauge>; 6],
    /// Blocks that the node committed since its start.
    pub verified_blocks: Arc<Counter>,
    pub verify_success: Arc<Histogram>,
    pub verify_failure: Arc<Histogram>,
    pub mempool_transactions: Arc<Gauge>,
    pub mempool_bytes: Arc<Gauge>,
    pub template_rebuilt: Arc<Counter>,
    pub stages: Vec<Arc<Histogram>>,
    pub commit_duration: Arc<Histogram>,
    pub store_hits: Arc<Counter>,
    pub store_misses: Arc<Counter>,
    pub blocks_rejected: Arc<Counter>,
    pub peers: Arc<Gauge>,
    /// The peers of `peers` that completed the handshake, under the name of Zakura.
    pub net_peers: Arc<Gauge>,
    pub net_in_bytes: Arc<Counter>,
    pub net_out_bytes: Arc<Counter>,
    /// Blocks that the block download stored (Zakura name).
    pub sync_downloaded_blocks: Arc<Counter>,
    pub relay_forwarded_on_ids: Arc<Counter>,
    pub relay_forwarded_without_auth_root: Arc<Counter>,
    pub relay_forwarded_after_body: Arc<Counter>,
    pub relay_root_mismatches: Arc<Counter>,
    pub relay_candidate_blocks_sent: Arc<Counter>,
    pub relay_candidate_blocks_resolved: Arc<Counter>,
    pub relay_candidate_fallbacks: Arc<Counter>,
    /// Blocks committed from a prebuilt body, by origin of the body (`own`, `candidate`).
    pub prebuilt_commits_own: Arc<Counter>,
    pub prebuilt_commits_candidate: Arc<Counter>,
    pub prebuild_duration: Arc<Histogram>,
    pub trace_dropped: Vec<(Table, Arc<Counter>)>,
    pub commitments_unchecked: Arc<Counter>,
    pub trusted_coins: Arc<Counter>,
    pub trusted_nullifiers: Arc<Counter>,
    pub trusted_anchors: Arc<Counter>,
    pub trusted_bits: Arc<Counter>,
    pub upstream_agreements: Arc<Counter>,
    pub upstream_disagreements: Arc<Counter>,
    pub process_resident_bytes: Arc<Gauge>,
    pub process_cpu_seconds: Arc<FloatCounter>,
    /// Time from the tip event to the published empty and full template.
    pub template_empty_latency: Arc<Histogram>,
    pub template_full_latency: Arc<Histogram>,
    pub coins_cache_entries: Arc<Gauge>,
    pub coins_cache_bytes: Arc<Gauge>,
    pub coins_store_coins: Arc<Gauge>,
    /// Height of the best header chain.
    pub sync_header_height: Arc<Gauge>,
    /// Peers of the block download.
    pub sync_peers: Arc<Gauge>,
    pub sync_requests_in_flight: Arc<Gauge>,
    /// Block requests without an answer plus downloaded blocks that wait for the
    /// validator, as Zakura counts its download and verification tasks.
    pub sync_downloads_in_flight: Arc<Gauge>,
    pub sync_held_bytes: Arc<Gauge>,
    /// 1 while a header chain is out of the fork choice because no peer sends its blocks.
    pub sync_bodies_withheld: Arc<Gauge>,
    /// Times that the node took a header chain out of the fork choice for that reason.
    pub sync_withheld_chains: Arc<Counter>,
    /// Blocks that a reorg disconnected.
    pub blocks_disconnected: Arc<Counter>,
    /// Transactions that the mempool refused, by reason class.
    pub mempool_rejected_policy: Arc<Counter>,
    pub mempool_rejected_invalid: Arc<Counter>,
    /// The gauges of the last committed block ([`LastBlock`]).
    pub last_block_height: Arc<Gauge>,
    pub last_block_hash_suffix: Arc<Gauge>,
    pub last_block_source: Vec<(&'static str, Arc<Gauge>)>,
    pub last_block_size_bytes: Arc<Gauge>,
    pub last_block_transactions: Arc<Gauge>,
    pub last_block_prepared_known: Arc<Gauge>,
    pub last_block_prepared_unknown: Arc<Gauge>,
    pub last_block_received_to_validated: Arc<Gauge>,
    pub last_block_validation: Arc<Gauge>,
    pub last_block_commit: Arc<Gauge>,
    pub last_block_received_to_committed: Arc<Gauge>,
    /// One gauge for each name of [`STAGES`].
    pub last_block_stages: Vec<Arc<Gauge>>,
    /// For the empty and for the full template: the height of the tip block of the last
    /// template that the node timed from the reception of that block, and the time.
    pub last_template_empty: (Arc<Gauge>, Arc<Gauge>),
    pub last_template_full: (Arc<Gauge>, Arc<Gauge>),
    pub last_block_contextual_commit: Arc<Gauge>,
    pub contextual_commit: Arc<Histogram>,
    pub received_to_validated: Arc<Histogram>,
    pub receive_to_commit: Arc<Histogram>,
    pub received_to_template_empty: Arc<Histogram>,
    pub received_to_template_full: Arc<Histogram>,
}

/// The empty (coinbase only) or the full template.
#[derive(Clone, Copy)]
pub enum TemplateKind {
    Empty,
    Full,
}

/// Registers `hayai_build_info`: a gauge of value 1 whose labels name the build and the
/// configuration, so rules and dashboards do not infer them from the target name.
pub fn register_build_info(r: &Registry, config: &Config) {
    let mode = match config.network.mode {
        Mode::Full => "full",
        Mode::Shadow => "shadow",
    };
    let coins_backend = match config.state.backend {
        Backend::Memory => "memory",
        Backend::Rocksdb => "rocksdb",
    };
    r.gauge(
        "hayai_build_info",
        "Build and configuration of the node; the value is always 1.",
        &[
            ("version", env!("CARGO_PKG_VERSION")),
            ("chain", config.network.network.name()),
            ("mode", mode),
            ("crypto_backend", hayai_crypto::BACKEND),
            ("coins_backend", coins_backend),
        ],
    )
    .set(1.0);
}

impl NodeMetrics {
    pub fn new(r: &Registry) -> Self {
        let verify = |result| {
            r.histogram(
                "sync_block_verify_duration_seconds",
                "Reception of a block to its commit (success) or to its rejection (failure).",
                &[("result", result)],
                &DURATION_BUCKETS,
            )
        };
        let template_latency = |template| {
            r.histogram(
                "hayai_template_latency_seconds",
                "Time from a tip change to the published empty or full template.",
                &[("template", template)],
                &DURATION_BUCKETS,
            )
        };
        let last_template = |template| {
            (
                r.gauge(
                    "hayai_last_template_tip_height",
                    "Height of the tip block of hayai_last_template_received_to_ready_seconds with the same label.",
                    &[("template", template)],
                ),
                r.gauge(
                    "hayai_last_template_received_to_ready_seconds",
                    "Reception of the tip block to the first empty or full template on it.",
                    &[("template", template)],
                ),
            )
        };
        let received_to_template = |template| {
            r.histogram(
                "hayai_block_received_to_template_seconds",
                "Reception of a block to the first empty or full template on it.",
                &[("template", template)],
                &DURATION_BUCKETS,
            )
        };
        Self {
            verified_height: r.gauge(
                "zcash_chain_verified_block_height",
                "Height of the newest validated block on the best chain.",
                &[],
            ),
            committed_height: r.gauge(
                "state_memory_best_committed_block_height",
                "Height of the best chain tip in the in-memory state.",
                &[],
            ),
            finalized_height: r.gauge(
                "state_finalized_block_height",
                "Height of the newest block whose coins are in the coins store on disk.",
                &[],
            ),
            base_height: r.gauge(
                "hayai_base_height",
                "Height of the newest block whose layer is merged into the finalized state in memory.",
                &[],
            ),
            wallet_index: [
                ("hayai_wallet_index_blocks", "Blocks that the wallet index wrote since the start."),
                ("hayai_wallet_index_batch_bytes", "Bytes of the write batches of the wallet index since the start."),
                ("hayai_wallet_index_build_seconds", "Time of the build of the wallet index entries since the start."),
                ("hayai_wallet_index_write_seconds", "Time of the wallet index writes to RocksDB since the start."),
                ("hayai_wallet_index_queue_wait_seconds", "Time that the driver waited on the full queue of the wallet index since the start."),
                ("hayai_wallet_index_persist_wait_seconds", "Time that the driver waited for the sync of the wallet index since the start."),
            ]
            .map(|(name, help)| r.gauge(name, help, &[])),
            verified_blocks: r.counter(
                "zcash_chain_verified_block_total",
                "Blocks that the node committed since its start.",
                &[],
            ),
            verify_success: verify("success"),
            verify_failure: verify("failure"),
            mempool_transactions: r.gauge(
                "zcash_mempool_size_transactions",
                "Transactions in the prepared store.",
                &[],
            ),
            mempool_bytes: r.gauge(
                "zcash_mempool_size_bytes",
                "Wire bytes of the transactions in the prepared store.",
                &[],
            ),
            template_rebuilt: r.counter(
                "hayai_template_updates_total",
                "Full or changed templates the live template produced.",
                &[],
            ),
            stages: STAGES
                .iter()
                .map(|stage| {
                    r.histogram(
                        "hayai_validate_stage_duration_seconds",
                        "Wall time of each stage of hayai-validate (Timings).",
                        &[("stage", stage)],
                        &DURATION_BUCKETS,
                    )
                })
                .collect(),
            commit_duration: r.histogram(
                "hayai_commit_duration_seconds",
                "commit_start to commit_finish of a committed block.",
                &[],
                &DURATION_BUCKETS,
            ),
            store_hits: r.counter(
                "hayai_prepared_store_hits_total",
                "Block transactions served by the prepared store.",
                &[],
            ),
            store_misses: r.counter(
                "hayai_prepared_store_misses_total",
                "Block transactions prepared during block validation.",
                &[],
            ),
            blocks_rejected: r.counter(
                "hayai_blocks_rejected_total",
                "Blocks that failed validation.",
                &[],
            ),
            peers: r.gauge("hayai_peers", "Connected peers.", &[]),
            net_peers: r.gauge(
                "zcash_net_peers",
                "Connected peers after the handshake.",
                &[],
            ),
            net_in_bytes: r.counter(
                "zcash_net_in_bytes_total",
                "Bytes of the P2P messages that the node received.",
                &[],
            ),
            net_out_bytes: r.counter(
                "zcash_net_out_bytes_total",
                "Bytes of the P2P messages that the node sent.",
                &[],
            ),
            sync_downloaded_blocks: r.counter(
                "sync_downloaded_block_count",
                "Blocks that the block download received and stored.",
                &[],
            ),
            relay_forwarded_on_ids: r.counter(
                "hayai_relay_forwarded_on_ids_total",
                "Compact blocks forwarded on a verified id list before the body.",
                &[],
            ),
            relay_forwarded_without_auth_root: r.counter(
                "hayai_relay_forwarded_without_auth_root_total",
                "Forwards on ids whose auth data root was not checked.",
                &[],
            ),
            relay_forwarded_after_body: r.counter(
                "hayai_relay_forwarded_after_body_total",
                "Blocks whose first compact forwarding carried the complete body.",
                &[],
            ),
            relay_root_mismatches: r.counter(
                "hayai_relay_root_mismatches_total",
                "Compact id lists that failed the merkle or commitments check.",
                &[],
            ),
            relay_candidate_blocks_sent: r.counter(
                "hayai_relay_candidate_blocks_sent_total",
                "Blocks sent as a candidate reference plus a difference.",
                &[],
            ),
            relay_candidate_blocks_resolved: r.counter(
                "hayai_relay_candidate_blocks_resolved_total",
                "Received candidate blocks that resolved against a stored candidate.",
                &[],
            ),
            relay_candidate_fallbacks: r.counter(
                "hayai_relay_candidate_fallbacks_total",
                "Received candidate blocks that fell back to the full block.",
                &[],
            ),
            prebuilt_commits_own: r.counter(
                "hayai_prebuilt_commits_total",
                "Blocks committed from a prebuilt body.",
                &[("origin", "own")],
            ),
            prebuilt_commits_candidate: r.counter(
                "hayai_prebuilt_commits_total",
                "Blocks committed from a prebuilt body.",
                &[("origin", "candidate")],
            ),
            prebuild_duration: r.histogram(
                "hayai_prebuild_duration_seconds",
                "Wall time of one body prebuild (own template or a peer's candidate).",
                &[],
                &DURATION_BUCKETS,
            ),
            trace_dropped: Table::ALL
                .iter()
                .map(|table| {
                    (
                        *table,
                        r.counter(
                            "hayai_trace_dropped_rows_total",
                            "Trace rows dropped because the writer queue was full.",
                            &[("table", table.file_name())],
                        ),
                    )
                })
                .collect(),
            commitments_unchecked: r.counter(
                "hayai_block_commitments_unchecked_total",
                "Blocks validated without the hashBlockCommitments check (no ZIP 221 root).",
                &[],
            ),
            trusted_coins: r.counter(
                "hayai_shadow_trusted_coins_total",
                "Coins created before the shadow start height, read from upstream.",
                &[],
            ),
            trusted_nullifiers: r.counter(
                "hayai_shadow_trusted_nullifiers_total",
                "Nullifier lookups that only the history before the shadow start could fail.",
                &[],
            ),
            trusted_anchors: r.counter(
                "hayai_shadow_trusted_anchors_total",
                "Anchors from before the shadow start, accepted from upstream.",
                &[],
            ),
            trusted_bits: r.counter(
                "hayai_shadow_trusted_bits_total",
                "Headers with a rule that the context was too short to check.",
                &[],
            ),
            upstream_agreements: r.counter(
                "hayai_shadow_agreements_total",
                "Upstream blocks that hayai also accepted.",
                &[],
            ),
            upstream_disagreements: r.counter(
                "hayai_shadow_disagreements_total",
                "Upstream blocks that hayai rejected.",
                &[],
            ),
            process_resident_bytes: r.gauge(
                "process_resident_memory_bytes",
                "Resident memory size of the process in bytes.",
                &[],
            ),
            process_cpu_seconds: r.float_counter(
                "process_cpu_seconds_total",
                "User and system CPU time of the process in seconds.",
                &[],
            ),
            template_empty_latency: template_latency("empty"),
            template_full_latency: template_latency("full"),
            coins_cache_entries: r.gauge(
                "hayai_coins_cache_entries",
                "Entries of the coins cache of the finalized state.",
                &[],
            ),
            coins_cache_bytes: r.gauge(
                "hayai_coins_cache_bytes",
                "Memory of the coins cache of the finalized state in bytes.",
                &[],
            ),
            coins_store_coins: r.gauge(
                "hayai_coins_store_coins",
                "Coins in the memory backing (memory backend only).",
                &[],
            ),
            sync_header_height: r.gauge(
                "hayai_sync_header_height",
                "Height of the best header chain.",
                &[],
            ),
            sync_peers: r.gauge(
                "hayai_sync_peers",
                "Peers that the block download can ask.",
                &[],
            ),
            sync_requests_in_flight: r.gauge(
                "hayai_sync_requests_in_flight",
                "Block requests without an answer.",
                &[],
            ),
            sync_downloads_in_flight: r.gauge(
                "sync_downloads_in_flight",
                "Block requests without an answer plus downloaded blocks that wait for the validator.",
                &[],
            ),
            sync_held_bytes: r.gauge(
                "hayai_sync_held_bytes",
                "Bytes of the downloaded blocks that wait for the validator.",
                &[],
            ),
            sync_bodies_withheld: r.gauge(
                "hayai_sync_bodies_withheld",
                "1 while a header chain is out of the fork choice because no peer sends its blocks.",
                &[],
            ),
            sync_withheld_chains: r.counter(
                "hayai_sync_withheld_chains_total",
                "Header chains that the node took out of the fork choice because no peer sent their blocks.",
                &[],
            ),
            blocks_disconnected: r.counter(
                "hayai_blocks_disconnected_total",
                "Committed blocks that a reorg disconnected.",
                &[],
            ),
            mempool_rejected_policy: r.counter(
                "hayai_mempool_rejected_total",
                "Transactions that the mempool refused.",
                &[("reason", "policy")],
            ),
            mempool_rejected_invalid: r.counter(
                "hayai_mempool_rejected_total",
                "Transactions that the mempool refused.",
                &[("reason", "invalid")],
            ),
            last_block_height: r.gauge(
                "hayai_last_block_height",
                "Height of the last committed block. The other hayai_last_block gauges are of this block.",
                &[],
            ),
            last_block_hash_suffix: r.gauge(
                "hayai_last_block_hash_suffix",
                "Last 12 hexadecimal digits of the hash of the last committed block, as an integer.",
                &[],
            ),
            last_block_source: SOURCES
                .iter()
                .map(|source| {
                    (
                        *source,
                        r.gauge(
                            "hayai_last_block_source",
                            "1 for the source of the last committed block, 0 for each other source.",
                            &[("source", source)],
                        ),
                    )
                })
                .collect(),
            last_block_size_bytes: r.gauge(
                "hayai_last_block_size_bytes",
                "Wire bytes of the last committed block.",
                &[],
            ),
            last_block_transactions: r.gauge(
                "hayai_last_block_transactions",
                "Transactions of the last committed block, with the coinbase.",
                &[],
            ),
            last_block_prepared_known: r.gauge(
                "hayai_last_block_prepared_transactions",
                "Transactions of the last committed block: found in the prepared store (known) or prepared at block time (unknown).",
                &[("state", "known")],
            ),
            last_block_prepared_unknown: r.gauge(
                "hayai_last_block_prepared_transactions",
                "Transactions of the last committed block: found in the prepared store (known) or prepared at block time (unknown).",
                &[("state", "unknown")],
            ),
            last_block_received_to_validated: r.gauge(
                "hayai_last_block_received_to_validated_seconds",
                "Reception of the last committed block to its valid verdict.",
                &[],
            ),
            last_block_validation: r.gauge(
                "hayai_last_block_validation_seconds",
                "commit_start to the valid verdict of the last committed block.",
                &[],
            ),
            last_block_commit: r.gauge(
                "hayai_last_block_commit_seconds",
                "Valid verdict to commit_finish of the last committed block.",
                &[],
            ),
            last_block_received_to_committed: r.gauge(
                "hayai_last_block_received_to_committed_seconds",
                "Reception of the last committed block to commit_finish.",
                &[],
            ),
            last_block_contextual_commit: r.gauge(
                "hayai_last_block_contextual_commit_seconds",
                "Stages context, trees and history plus the chain push of the last committed block.",
                &[],
            ),
            last_block_stages: STAGES
                .iter()
                .map(|stage| {
                    r.gauge(
                        "hayai_last_block_validate_stage_seconds",
                        "Wall time of each stage of hayai-validate for the last committed block.",
                        &[("stage", stage)],
                    )
                })
                .collect(),
            last_template_empty: last_template("empty"),
            last_template_full: last_template("full"),
            received_to_validated: r.histogram(
                "hayai_block_received_to_validated_seconds",
                "Reception of a block to its valid verdict.",
                &[],
                &DURATION_BUCKETS,
            ),
            receive_to_commit: r.histogram(
                "hayai_block_receive_to_commit_seconds",
                "Reception of a block to the moment at which it is the tip (commit_finish).",
                &[],
                &DURATION_BUCKETS,
            ),
            contextual_commit: r.histogram(
                "hayai_contextual_commit_duration_seconds",
                "Stages context, trees and history plus the push of the block on the chain.",
                &[],
                &DURATION_BUCKETS,
            ),
            received_to_template_empty: received_to_template("empty"),
            received_to_template_full: received_to_template("full"),
        }
    }

    /// Writes the gauges and the histograms of a committed block.
    pub fn record_last_block(&self, block: &LastBlock<'_>, timings: &Timings) {
        self.last_block_height.set(f64::from(block.height));
        self.last_block_hash_suffix.set(block.hash_suffix as f64);
        for (source, gauge) in &self.last_block_source {
            gauge.set(match *source == block.source {
                true => 1.0,
                false => 0.0,
            });
        }
        self.last_block_size_bytes.set(block.size_bytes as f64);
        self.last_block_transactions.set(block.transactions as f64);
        self.last_block_prepared_known.set(timings.known as f64);
        self.last_block_prepared_unknown.set(timings.unknown as f64);
        let validated = seconds(block.received_to_validated_us);
        let committed = seconds(block.received_to_committed_us);
        self.last_block_received_to_validated.set(validated);
        self.last_block_validation.set(seconds(block.validation_us));
        self.last_block_commit.set(seconds(block.commit_us));
        self.last_block_received_to_committed.set(committed);
        for (gauge, d) in self.last_block_stages.iter().zip(stage_durations(timings)) {
            gauge.set(seconds(micros(d)));
        }
        self.received_to_validated.observe(validated);
        self.receive_to_commit.observe(committed);
        self.verify_success.observe(committed);
        let contextual = seconds(block.contextual_commit_us);
        self.last_block_contextual_commit.set(contextual);
        self.contextual_commit.observe(contextual);
    }

    /// Writes the gauge and the histogram of the first template of `kind` on the block at
    /// `tip_height`, `micros` after the reception of that block.
    pub fn record_last_template(&self, kind: TemplateKind, tip_height: u32, micros: u64) {
        let ((height, gauge), histogram) = match kind {
            TemplateKind::Empty => (&self.last_template_empty, &self.received_to_template_empty),
            TemplateKind::Full => (&self.last_template_full, &self.received_to_template_full),
        };
        // The time first: a reader that sees the new height sees the time of that height.
        gauge.set(seconds(micros));
        height.set(f64::from(tip_height));
        histogram.observe(seconds(micros));
    }

    pub fn record_process(&self, stats: ProcessStats) {
        self.process_resident_bytes.set(stats.resident_bytes as f64);
        self.process_cpu_seconds.set(stats.cpu_seconds);
    }

    pub fn record_stages(&self, t: &Timings) {
        for (histogram, d) in self.stages.iter().zip(stage_durations(t)) {
            histogram.observe_duration(d);
        }
    }

    pub fn record_relay(&self, c: RelayCounters) {
        self.relay_forwarded_on_ids.set(c.forwarded_on_ids);
        self.relay_forwarded_without_auth_root
            .set(c.forwarded_without_auth_root);
        self.relay_forwarded_after_body.set(c.forwarded_after_body);
        self.relay_root_mismatches.set(c.root_mismatches);
        self.relay_candidate_blocks_sent
            .set(c.candidate_blocks_sent);
        self.relay_candidate_blocks_resolved
            .set(c.candidate_blocks_resolved);
        self.relay_candidate_fallbacks.set(c.candidate_fallbacks);
    }

    pub fn record_wallet_index(&self, stats: &hayai_index::WriterStats) {
        use std::sync::atomic::Ordering::Relaxed;
        let seconds = |us: &std::sync::atomic::AtomicU64| us.load(Relaxed) as f64 / 1e6;
        let values = [
            stats.blocks.load(Relaxed) as f64,
            stats.bytes.load(Relaxed) as f64,
            seconds(&stats.build_us),
            seconds(&stats.write_us),
            seconds(&stats.queue_wait_us),
            seconds(&stats.persist_wait_us),
        ];
        for (gauge, value) in self.wallet_index.iter().zip(values) {
            gauge.set(value);
        }
    }

    pub fn record_trace_drops(&self, tracer: &Tracer) {
        for (table, counter) in &self.trace_dropped {
            counter.set(tracer.dropped(*table));
        }
    }
}
