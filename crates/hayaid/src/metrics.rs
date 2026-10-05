//! The node's metrics. Names with the meaning of a Zakura metric are Zakura's names as its
//! Prometheus exporter writes them (dots become underscores); the others start with
//! `hayai_`.

use std::sync::Arc;
use std::time::Duration;

use hayai_net::RelayCounters;
use hayai_rpc::metrics::DURATION_BUCKETS;
use hayai_rpc::{Counter, FloatCounter, Gauge, Histogram, Registry};
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

pub struct NodeMetrics {
    pub verified_height: Arc<Gauge>,
    pub committed_height: Arc<Gauge>,
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
                "Time from the start of a block's validation to its verdict.",
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
                "mining_template_rebuilt",
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
        }
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

    pub fn record_trace_drops(&self, tracer: &Tracer) {
        for (table, counter) in &self.trace_dropped {
            counter.set(tracer.dropped(*table));
        }
    }
}
