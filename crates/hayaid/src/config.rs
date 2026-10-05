//! The configuration file (TOML) and its defaults.
//!
//! `hayaid config --network <name>` prints [`default_toml`]: every key with its default and
//! a comment. Unknown keys are errors, so a misspelt key never falls back to a default.
//!
//! A setting that Zakura also has uses the section, the key and the value format of
//! `zakurad`. A key of `zakurad` that hayaid does not use is not an unknown key: the parser
//! reports each one ([`ZAKURA_UNUSED`], [`ZAKURA_REFUSED`]). `docs/zakura-compat.md` has
//! the table of all keys.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use hayai_consensus::coinbase::CoinbaseTerms;
use hayai_consensus::{
    ConsensusError, RegtestConfig, RegtestDisbursement, RegtestFundingStreams, Upgrade,
};

use crate::params::{parse_hash, NetworkKind};

/// What the node does with blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Own state from genesis; produces, validates, relays and serves templates.
    Full,
    /// Follows a local Zakura node through its RPC and P2P port; validates every block and
    /// records traces and metrics; never announces blocks or serves templates. Testnet and
    /// Mainnet are the targets; Regtest serves the tests.
    Shadow,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub network: NetworkSection,
    #[serde(default)]
    pub state: StateSection,
    #[serde(default)]
    pub rpc: RpcSection,
    #[serde(default)]
    pub metrics: MetricsSection,
    #[serde(default)]
    pub trace: TraceSection,
    #[serde(default)]
    pub mining: MiningSection,
    #[serde(default)]
    pub sync: SyncSection,
    #[serde(default)]
    pub mempool: MempoolSection,
    pub shadow: Option<ShadowSection>,
    pub regtest: Option<RegtestSection>,
    #[serde(default)]
    pub tracing: TracingSection,
    /// One line for each key of `zakurad` in the file that hayaid does not use. The node
    /// logs them as warnings at its start.
    #[serde(skip)]
    pub zakura_unused: Vec<String>,
}

/// Regtest only: the values of `hayai_consensus::RegtestConfig`. Every node of one Regtest
/// network must have the same activation heights. A node must keep its values for the life
/// of its data directory.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegtestSection {
    #[serde(default)]
    pub activation_heights: ActivationHeights,
    /// Checkpoints as `[height, "hash"]`, the hash as `getblockhash` prints it. The genesis
    /// block is always a checkpoint.
    #[serde(default)]
    pub checkpoints: Vec<(u32, String)>,
    /// A block at or below this height has the checkpoint path only. The last checkpoint
    /// must be at or above it.
    #[serde(default)]
    pub mandatory_checkpoint_height: u32,
    /// The outputs that the coinbase of the NU6.1 activation block must have, as
    /// `lockbox_disbursements` of the Regtest parameters of Zakura: `address` (P2SH) and
    /// `amount` (zatoshis). A network with an `nu6_1` height needs one entry or more.
    #[serde(default)]
    pub lockbox_disbursements: Vec<RegtestDisbursement>,
    /// The funding streams, as `funding_streams` of the Regtest parameters of Zakura:
    /// `height_range` (`start`, `end`) and `recipients` (`receiver`, `numerator`,
    /// `addresses`).
    #[serde(default)]
    pub funding_streams: Vec<RegtestFundingStreams>,
    /// An NSM reissuance height for the tests of a short chain
    /// (`RegtestConfig::with_test_reissuance_height`). A configuration file cannot set it.
    #[serde(skip)]
    pub test_reissuance_height: Option<u32>,
}

/// The Regtest activation heights of the upgrades after NU5. Absent: the upgrade does not
/// activate.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationHeights {
    pub nu6: Option<u32>,
    pub nu6_1: Option<u32>,
    pub nu6_2: Option<u32>,
    pub nu6_3: Option<u32>,
    pub nu7: Option<u32>,
}

impl RegtestSection {
    /// The network of the section. Its values stay in memory until the process ends.
    fn network(&self) -> Result<NetworkKind, ConfigError> {
        let invalid = |e: &dyn std::fmt::Display| ConfigError::Invalid(format!("[regtest]: {e}"));
        let heights = self.activation_heights;
        let activations: Vec<(Upgrade, u32)> = [
            (Upgrade::Nu6, heights.nu6),
            (Upgrade::Nu6_1, heights.nu6_1),
            (Upgrade::Nu6_2, heights.nu6_2),
            (Upgrade::Nu6_3, heights.nu6_3),
            (Upgrade::Nu7, heights.nu7),
        ]
        .into_iter()
        .filter_map(|(upgrade, height)| Some((upgrade, height?)))
        .collect();
        let checkpoints = self
            .checkpoints
            .iter()
            .map(|(height, hash)| Ok((*height, parse_hash(hash).map_err(ConfigError::Invalid)?)))
            .collect::<Result<Vec<_>, ConfigError>>()?;
        let config =
            RegtestConfig::new(&activations, checkpoints, self.mandatory_checkpoint_height)
                .and_then(|c| c.with_lockbox_disbursements(self.lockbox_disbursements.clone()))
                .and_then(|c| c.with_funding_streams(&self.funding_streams))
                .map_err(|e| invalid(&e))?;
        let network = match self.test_reissuance_height {
            Some(height) => config.with_test_reissuance_height(height),
            None => config,
        }
        .network();
        // No node accepts the NU6.1 activation block of a network without a lockbox
        // disbursement (Zakura `subsidy_is_valid`), so the chain of such a network ends
        // below that height.
        if let Some(height) = heights.nu6_1 {
            if let Err(e @ ConsensusError::NoLockboxDisbursement { .. }) =
                CoinbaseTerms::at(network, height)
            {
                return Err(invalid(&format!("{e}: set lockbox_disbursements")));
            }
        }
        Ok(network)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// `Mainnet`, `Testnet` or `Regtest`, as in a configuration of `zakurad`.
    #[serde(deserialize_with = "network_name")]
    pub network: NetworkKind,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    /// P2P listen address. Absent: the node only dials out. An address without a port
    /// takes the default port of the network.
    pub listen_addr: Option<SocketAddr>,
    /// Directory of the address book: `true` is `[state] cache_dir`, `false` keeps the
    /// address book in memory only, a path is that directory.
    #[serde(default)]
    pub cache_dir: PeerCacheDir,
    /// The keys of `[network.zakura]` that hayaid uses.
    #[serde(default)]
    pub zakura: ZakuraSection,
    /// Peers to dial and to redial while they are not connected.
    #[serde(default)]
    pub peers: Vec<SocketAddr>,
    /// Offer the compact-relay extension (`zcmpctver`). Off: a plain legacy node.
    #[serde(default = "default_true")]
    pub compact_relay: bool,
    /// Connections above this count are closed, newest inbound first. Absent: 16, or the
    /// sum of the two limits of `peerset_initial_target_size` when that key is present.
    pub max_peers: Option<usize>,
    /// Candidates of peers' lanes on the tip whose layer the node prebuilds while idle, at
    /// most. A block equal to one commits as a pointer swap. 0: off.
    #[serde(default)]
    pub prebuilt_candidates: usize,
    /// Full mode, the size of the peer set with the rule of Zakura: the node keeps
    /// 3/2 of this count as outbound peers and accepts 3 times this count as inbound
    /// peers. Absent: the defaults of `outbound_peers` and `max_inbound`.
    pub peerset_initial_target_size: Option<usize>,
    /// Full mode: outbound peers that the peer manager keeps. Absent: the value of
    /// `peerset_initial_target_size`, or the default of `hayai_net::PeerConfig` (8).
    pub outbound_peers: Option<usize>,
    /// Inbound peers accepted, at most. Absent: the value of
    /// `peerset_initial_target_size`, or 64.
    pub max_inbound: Option<usize>,
    /// Connections with one IP address, at most. Absent: 1, and no bound on Regtest.
    pub max_connections_per_ip: Option<usize>,
    /// Mainnet: DNS seeders and peer addresses as `host:port`. Absent: the seeders of
    /// the network.
    pub initial_mainnet_peers: Option<Vec<String>>,
    /// Testnet and Regtest: DNS seeders and peer addresses as `host:port`. Absent: the
    /// seeders of the network (Regtest has none).
    pub initial_testnet_peers: Option<Vec<String>>,
    /// Duration of a ban in seconds. Absent: 86,400.
    pub ban_secs: Option<u64>,
}

/// The value of `[network] cache_dir`.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(untagged)]
pub enum PeerCacheDir {
    Enabled(bool),
    Dir(PathBuf),
}

impl Default for PeerCacheDir {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZakuraSection {
    /// Directory of the JSONL trace tables. Absent: tracing is off.
    pub trace_dir: Option<PathBuf>,
}

/// The peer limits of `[network]`. `None`: the default of `hayai_net::PeerConfig`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PeerLimits {
    pub outbound: Option<usize>,
    pub inbound: Option<usize>,
    /// Connections above this count are closed.
    pub total: usize,
}

impl NetworkSection {
    /// `peerset_initial_target_size` with the multipliers of Zakura
    /// (`OUTBOUND_PEER_LIMIT_MULTIPLIER / OUTBOUND_PEER_LIMIT_DIVISOR` = 3/2,
    /// `INBOUND_PEER_LIMIT_MULTIPLIER` = 3). A key of hayaid replaces the value of its
    /// limit.
    pub fn peer_limits(&self) -> PeerLimits {
        let size = self.peerset_initial_target_size;
        let outbound = self.outbound_peers.or(size.map(|s| s * 3 / 2));
        let inbound = self.max_inbound.or(size.map(|s| s * 3));
        let total = self.max_peers.unwrap_or(match size {
            Some(_) => outbound.unwrap_or(0) + inbound.unwrap_or(0),
            None => DEFAULT_MAX_PEERS,
        });
        PeerLimits {
            outbound,
            inbound,
            total,
        }
    }

    /// The seeders of the configured network, when the file has their key.
    pub fn initial_peers(&self) -> Option<&Vec<String>> {
        match self.network {
            NetworkKind::Mainnet => self.initial_mainnet_peers.as_ref(),
            _ => self.initial_testnet_peers.as_ref(),
        }
    }

    /// The directory of the address book. `None`: the book is not saved.
    pub fn peer_cache_dir<'a>(&'a self, state_dir: &'a Path) -> Option<&'a Path> {
        match &self.cache_dir {
            PeerCacheDir::Enabled(true) => Some(state_dir),
            PeerCacheDir::Enabled(false) => None,
            PeerCacheDir::Dir(dir) => Some(dir),
        }
    }
}

fn network_name<'de, D: Deserializer<'de>>(d: D) -> Result<NetworkKind, D::Error> {
    let name = String::deserialize(d)?;
    match name.as_str() {
        "Mainnet" => Ok(NetworkKind::Mainnet),
        "Testnet" => Ok(NetworkKind::Testnet),
        "Regtest" => Ok(NetworkKind::Regtest),
        _ => Err(serde::de::Error::custom(format!(
            "unknown network {name}: the networks are \"Mainnet\", \"Testnet\" and \"Regtest\""
        ))),
    }
}

/// The block synchronization of a full node.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncSection {
    /// Bound of the downloaded bodies that wait for the validator plus 2 MB for each
    /// request without an answer.
    #[serde(default = "default_memory_budget")]
    pub memory_budget_bytes: u64,
    /// Time without a block from a peer with a request, after which the peer stalls.
    #[serde(default = "default_request_timeout")]
    pub request_timeout_ms: u64,
    /// Time without a `headers` answer, after which the node disconnects the peer of the
    /// header sync.
    #[serde(default = "default_header_timeout")]
    pub header_timeout_ms: u64,
    /// First delay of the idle poll of the header sync. The delay doubles after each poll
    /// and starts again at this value when the node learns of a new block.
    #[serde(default = "default_header_poll")]
    pub header_poll_ms: u64,
    /// Largest delay of the idle poll of the header sync.
    #[serde(default = "default_header_poll_max")]
    pub header_poll_max_ms: u64,
}

impl Default for SyncSection {
    fn default() -> Self {
        Self {
            memory_budget_bytes: default_memory_budget(),
            request_timeout_ms: default_request_timeout(),
            header_timeout_ms: default_header_timeout(),
            header_poll_ms: default_header_poll(),
            header_poll_max_ms: default_header_poll_max(),
        }
    }
}

/// The store of the finalized coins and nullifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// `hayai_coins::MemBacking`: the whole set in memory, a log and snapshots on disk.
    Memory,
    /// `hayai_coins::RocksBacking`.
    Rocksdb,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSection {
    /// The data directory of the node. hayaid puts its files in this directory, without
    /// the `state/` level of Zakura, and refuses a directory of another node.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    #[serde(default = "default_backend")]
    pub backend: Backend,
    /// Blocks between two flushes of the finalized coins to disk.
    #[serde(default = "default_flush_interval")]
    pub flush_interval_blocks: u32,
    /// Memory backend: blocks between two snapshots of the coin set. A clean shutdown
    /// also writes a snapshot.
    #[serde(default = "default_snapshot_interval")]
    pub snapshot_interval_blocks: u32,
}

impl Default for StateSection {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            backend: default_backend(),
            flush_interval_blocks: default_flush_interval(),
            snapshot_interval_blocks: default_snapshot_interval(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcSection {
    /// JSON-RPC listen address. Absent: no RPC server.
    pub listen_addr: Option<SocketAddr>,
    /// Each request must have the credentials of the cookie file, as in Zakura
    /// (`docs/hayaid.md`, JSON-RPC server).
    #[serde(default = "default_true")]
    pub enable_cookie_auth: bool,
    /// Directory of the cookie file `.cookie`. Absent: `[state] cache_dir`.
    pub cookie_dir: Option<PathBuf>,
}

impl Default for RpcSection {
    fn default() -> Self {
        Self {
            listen_addr: None,
            enable_cookie_auth: true,
            cookie_dir: None,
        }
    }
}

impl RpcSection {
    /// The listen address when each host that reaches it can call each method: the
    /// cookie authentication is off and the address is not a loopback address.
    pub fn open_addr(&self) -> Option<SocketAddr> {
        self.listen_addr
            .filter(|addr| !self.enable_cookie_auth && !addr.ip().is_loopback())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    /// Prometheus `/metrics` listen address. Absent: no metrics endpoint.
    pub endpoint_addr: Option<SocketAddr>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSection {
    /// Value of the `node` field of every row of the trace tables
    /// (`[network.zakura] trace_dir`).
    #[serde(default = "default_node")]
    pub node: String,
}

impl Default for TraceSection {
    fn default() -> Self {
        Self {
            node: default_node(),
        }
    }
}

/// What a mining node shows to its hayai peers of its template before it finds a block.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanePublication {
    /// Each template change goes out as a batch and a candidate. The node refuses a
    /// private transaction.
    All,
    /// As `All`, without the private transactions (`sendprivatetransaction`).
    Public,
    /// No batch and no candidate of the template. The node takes private transactions.
    None,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiningSection {
    /// Transparent address of the coinbase miner output (`t1…`, `t3…`, `tm…`, `t2…`).
    pub miner_address: Option<String>,
    /// Text that follows `hayai: ` in the coinbase input of each template, at most
    /// [`MAX_EXTRA_COINBASE_DATA`] bytes. Zakura writes its key of this name in the same
    /// way after its own marker.
    pub extra_coinbase_data: Option<String>,
    /// Hex scriptPubKey of the coinbase miner output; an alternative to `miner_address`.
    pub miner_script: Option<String>,
    /// Regtest only: serve the `generate` RPC.
    #[serde(default)]
    pub regtest_produce: bool,
    /// Full mode: keep the layer of the current template's body prebuilt, so that an own
    /// block commits as a pointer swap.
    #[serde(default = "default_true")]
    pub prebuild_own: bool,
    /// Full mode with the compact relay: the part of the template that the node publishes
    /// to its peers in advance (batch lanes and candidates). A found block goes out in
    /// each case. The lanes of other miners do not depend on this key.
    #[serde(default = "default_lane_publication")]
    pub lane_publication: LanePublication,
}

impl Default for MiningSection {
    fn default() -> Self {
        Self {
            miner_address: None,
            extra_coinbase_data: None,
            miner_script: None,
            regtest_produce: false,
            prebuild_own: true,
            lane_publication: default_lane_publication(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSection {
    /// JSON-RPC address of the followed Zakura node (cookie authentication off).
    pub rpc_addr: SocketAddr,
    /// First height hayai validates is `start_height + 1`. Absent: the upstream tip at start.
    pub start_height: Option<u32>,
    /// Time between two `getbestblockhash` polls.
    #[serde(default = "default_poll_ms")]
    pub poll_interval_ms: u64,
}

/// The largest `extra_coinbase_data` in bytes. A coinbase input script has 100 bytes at
/// most: 5 for the height, 2 for the push opcode of the data, 7 for `hayai: `.
pub const MAX_EXTRA_COINBASE_DATA: usize = 86;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MempoolSection {
    /// ZIP 401 `mempooltxcostlimit`: the total cost of the transactions in the mempool,
    /// at most.
    #[serde(default = "default_tx_cost_limit")]
    pub tx_cost_limit: u64,
}

impl Default for MempoolSection {
    fn default() -> Self {
        Self {
            tx_cost_limit: default_tx_cost_limit(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingSection {
    /// The log level: `error`, `warn`, `info`, `debug` or `trace`. hayaid takes one level
    /// and no filter directive of a module.
    #[serde(default = "default_filter")]
    pub filter: String,
    /// ANSI colours in the log when the log goes to a terminal.
    #[serde(default = "default_true")]
    pub use_color: bool,
    /// ANSI colours in the log in each case.
    #[serde(default)]
    pub force_use_color: bool,
    /// The log goes to this file, in append mode. Absent: the log goes to stderr.
    pub log_file: Option<PathBuf>,
}

impl Default for TracingSection {
    fn default() -> Self {
        Self {
            filter: default_filter(),
            use_color: true,
            force_use_color: false,
            log_file: None,
        }
    }
}

fn default_mode() -> Mode {
    Mode::Full
}
fn default_true() -> bool {
    true
}
fn default_lane_publication() -> LanePublication {
    LanePublication::All
}
const DEFAULT_MAX_PEERS: usize = 16;
fn default_tx_cost_limit() -> u64 {
    hayai_prepared::MEMPOOL_TX_COST_LIMIT as u64
}
fn default_cache_dir() -> PathBuf {
    PathBuf::from("hayaid-data")
}
fn default_flush_interval() -> u32 {
    100
}
fn default_backend() -> Backend {
    Backend::Memory
}
fn default_snapshot_interval() -> u32 {
    10_000
}
fn default_memory_budget() -> u64 {
    1 << 30
}
fn default_header_poll() -> u64 {
    crate::sync::HEADER_POLL_MS
}
fn default_header_poll_max() -> u64 {
    crate::sync::HEADER_POLL_MAX_MS
}
fn default_request_timeout() -> u64 {
    8_000
}
fn default_header_timeout() -> u64 {
    120_000
}
fn default_node() -> String {
    "hayaid".to_string()
}
fn default_poll_ms() -> u64 {
    200
}
fn default_filter() -> String {
    "info".to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Parse(#[from] toml::de::Error),
    #[error("{0}")]
    Invalid(String),
    /// An error of a file that also has keys of `zakurad` that hayaid does not use: the
    /// error, then one line for each such key.
    #[error("{source}\n{}", unused.join("\n"))]
    WithUnused {
        source: Box<ConfigError>,
        unused: Vec<String>,
    },
}

/// The keys of a configuration of `zakurad` that hayaid does not use. A file with one of
/// them is valid: the node reports each one with a warning. A name without a key after the
/// last section is a table that Zakura reads as one value.
pub const ZAKURA_UNUSED: &[&str] = &[
    "consensus.vct_fast_sync",
    "consensus.debug_skip_parameter_preload",
    "health.listen_addr",
    "health.min_connected_peers",
    "health.ready_max_blocks_behind",
    "health.enforce_on_test_networks",
    "health.ready_max_tip_age",
    "mempool.max_transaction_bytes",
    "mempool.eviction_memory_time",
    "mempool.debug_enable_at_height",
    "mempool.max_datacarrier_bytes",
    "mining.miner_memo",
    "mining.internal_miner",
    "mining.optimistic_block_inventory",
    "network.external_addr",
    "network.identity_dir",
    "network.zakura_node_secret_key",
    "network.p2p_stack",
    "network.legacy_p2p",
    "network.v2_p2p",
    "network.crawl_new_peer_interval",
    "network.expose_peer_addresses",
    "network.zakura.bootstrap_peers",
    "network.zakura.listen_addr",
    "network.zakura.nat_traversal",
    "network.zakura.max_connections",
    "network.zakura.max_connections_per_ip",
    "network.zakura.max_pending_handshakes",
    "network.zakura.stream_open_rate_per_second",
    "network.zakura.message_rate_per_second",
    "network.zakura.header_sync",
    "network.zakura.block_sync",
    "network.zakura.dev_network",
    "rpc.admin_listen_addr",
    "rpc.indexer_listen_addr",
    "rpc.indexer_tls",
    "rpc.parallel_cpu_threads",
    "rpc.debug_force_finished_sync",
    "rpc.max_response_body_size",
    "rpc.tls",
    "state.should_backup_non_finalized_state",
    "state.repair_zakura_header_store_on_startup",
    "state.historical_frontier_artifact",
    "state.delete_old_database",
    "state.storage_mode",
    "state.debug_stop_at_height",
    "state.debug_validity_check_interval",
    "state.debug_skip_non_finalized_state_backup_task",
    "sync.download_concurrency_limit",
    "sync.checkpoint_verify_concurrency_limit",
    "sync.full_verify_concurrency_limit",
    "sync.zakura_block_apply_concurrency_limit",
    "sync.parallel_cpu_threads",
    "sync.debug_skip_regtest_genesis_self_seed",
    "sync.debug_blocksync_throughput_target_height",
    "tracing.buffer_limit",
    "tracing.endpoint_addr",
    "tracing.flamegraph",
    "tracing.progress_bar",
    "tracing.use_journald",
    "tracing.opentelemetry_endpoint",
    "tracing.opentelemetry_service_name",
    "tracing.opentelemetry_sample_percent",
    "zcashd_compat.enabled",
    "zcashd_compat.manage_zcashd",
    "zcashd_compat.zcashd_source",
    "zcashd_compat.zcashd_path",
    "zcashd_compat.zcashd_datadir",
    "zcashd_compat.zcashd_extra_args",
    "zcashd_compat.block_gossip_peer_ips",
    "zcashd_compat.startup_delay",
    "zcashd_compat.restart_backoff",
    "zcashd_compat.restart_backoff_max",
    "zcashd_compat.restart_reset_after",
    "zcashd_compat.shutdown_grace_period",
];

/// The keys of `zakurad` that change the consensus rules, the network or a data location
/// and that hayaid cannot follow: the key, the one value that is the fixed behaviour of
/// hayaid (as TOML; empty when there is none), and the reason. Each other value is an
/// error.
pub const ZAKURA_REFUSED: &[(&str, &str, &str)] = &[
    (
        "consensus.checkpoint_sync",
        "true",
        "hayaid always uses its checkpoints",
    ),
    (
        "state.ephemeral",
        "false",
        "hayaid keeps its state in [state] cache_dir",
    ),
    (
        "rpc.cookie_file_name",
        "\".cookie\"",
        "the cookie file of hayaid is .cookie",
    ),
    (
        "network.testnet_parameters",
        "",
        "hayaid has the [regtest] section for a Regtest network and no configured Testnet",
    ),
];

/// The sections of `zakurad` of which hayaid uses no key.
const ZAKURA_SECTIONS: [&str; 3] = ["consensus", "health", "zcashd_compat"];

const UNUSED: &str = "Zakura setting that hayaid does not use";

/// Removes `key` (sections and key with dots between them) from `root`.
fn take(root: &mut toml::Table, key: &str) -> Option<toml::Value> {
    let (sections, leaf) = match key.rsplit_once('.') {
        Some((sections, leaf)) => (sections.split('.').collect(), leaf),
        None => (Vec::new(), key),
    };
    let mut table = root;
    for section in sections {
        table = table.get_mut(section)?.as_table_mut()?;
    }
    table.remove(leaf)
}

/// Removes the keys of `zakurad` that hayaid does not use from `root`. Returns one line for
/// each key of [`ZAKURA_UNUSED`], in the order of that list. A key of [`ZAKURA_REFUSED`]
/// with another value than the one of hayaid is an error: the error has a line for each
/// such key.
fn strip_zakura_keys(root: &mut toml::Table) -> Result<Vec<String>, ConfigError> {
    let mut refused = Vec::new();
    for (key, accepted, reason) in ZAKURA_REFUSED {
        if let Some(value) = take(root, key) {
            let shown = match &value {
                toml::Value::Table(_) => String::new(),
                value => format!(" = {value}"),
            };
            if shown.trim_start_matches(" = ") != *accepted || accepted.is_empty() {
                refused.push(format!(
                    "{key}{shown}: {UNUSED} and cannot ignore ({reason})"
                ));
            }
        }
    }
    let table_form = root
        .get("network")
        .and_then(|network| network.get("network"))
        .is_some_and(toml::Value::is_table);
    if table_form {
        refused.push(format!(
            "network.network: {UNUSED} in its table form and cannot ignore ({})",
            ZAKURA_REFUSED[3].2
        ));
    }
    let unused: Vec<String> = ZAKURA_UNUSED
        .iter()
        .filter(|key| take(root, key).is_some())
        .map(|key| format!("{key}: {UNUSED}"))
        .collect();
    for section in ZAKURA_SECTIONS {
        match root.get(section).and_then(toml::Value::as_table) {
            Some(rest) if rest.is_empty() => {
                root.remove(section);
            }
            Some(rest) => refused.extend(
                rest.keys()
                    .map(|key| format!("{section}.{key}: unknown key")),
            ),
            None => {}
        }
    }
    match refused.is_empty() {
        true => Ok(unused),
        false => Err(with_unused(
            ConfigError::Invalid(refused.join("\n")),
            unused,
        )),
    }
}

fn with_unused(source: ConfigError, unused: Vec<String>) -> ConfigError {
    match unused.is_empty() {
        true => source,
        false => ConfigError::WithUnused {
            source: Box::new(source),
            unused,
        },
    }
}

/// Gives `[network] listen_addr` the default port of the network when it has none, as
/// Zakura does.
fn add_listen_port(root: &mut toml::Table) {
    let Some(network) = root.get_mut("network").and_then(toml::Value::as_table_mut) else {
        return;
    };
    let port = match network.get("network").and_then(toml::Value::as_str) {
        Some("Mainnet") => 8233,
        Some("Testnet") => 18233,
        Some("Regtest") => 18344,
        _ => return,
    };
    let ip = network
        .get("listen_addr")
        .and_then(toml::Value::as_str)
        .and_then(|addr| addr.trim_matches(['[', ']']).parse::<IpAddr>().ok());
    if let Some(ip) = ip {
        let addr = SocketAddr::new(ip, port).to_string();
        network.insert("listen_addr".to_string(), toml::Value::String(addr));
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parses and checks the combinations the node cannot run. A key of `zakurad` that
    /// hayaid does not use is in [`Config::zakura_unused`], or in the error.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut root: toml::Table = toml::from_str(text)?;
        let unused = strip_zakura_keys(&mut root)?;
        add_listen_port(&mut root);
        let checked = toml::Value::Table(root)
            .try_into::<Config>()
            .map_err(ConfigError::from)
            .and_then(|config| config.check().map(|()| config));
        match checked {
            Ok(mut config) => {
                config.zakura_unused = unused;
                Ok(config)
            }
            Err(e) => Err(with_unused(e, unused)),
        }
    }

    /// The network of the consensus rules: the network of `[network]`, with the values of
    /// `[regtest]` when the section is present. The values of the section stay in memory
    /// until the process ends, so a node calls this function one time.
    pub fn consensus_network(&self) -> Result<NetworkKind, ConfigError> {
        match &self.regtest {
            None => Ok(self.network.network),
            Some(regtest) => regtest.network(),
        }
    }

    fn check(&self) -> Result<(), ConfigError> {
        let invalid = |m: &str| Err(ConfigError::Invalid(m.to_string()));
        if let Some(regtest) = &self.regtest {
            if self.network.network != NetworkKind::Regtest {
                return invalid("the [regtest] section applies to network = \"Regtest\" only");
            }
            regtest.network()?;
        }
        match (self.network.mode, self.network.network, &self.shadow) {
            (Mode::Shadow, _, Some(_)) => {}
            (Mode::Shadow, _, None) => {
                return invalid("mode = \"shadow\" needs a [shadow] section with rpc_addr");
            }
            (Mode::Full, _, Some(_)) => {
                return invalid("the [shadow] section applies to mode = \"shadow\" only");
            }
            (Mode::Full, _, None) => {}
        }
        if self.mining.regtest_produce
            && (self.network.network, self.network.mode) != (NetworkKind::Regtest, Mode::Full)
        {
            return invalid("regtest_produce is a setting of a Regtest full node");
        }
        if let (Mode::Shadow, Some(_)) = (self.network.mode, self.rpc.listen_addr) {
            return invalid("shadow mode serves no RPC; remove [rpc] listen_addr");
        }
        if let (Some(_), Some(_)) = (&self.mining.miner_address, &self.mining.miner_script) {
            return invalid("set miner_address or miner_script, not both");
        }
        if let (None, None) = (&self.mining.miner_address, &self.mining.miner_script) {
            return invalid(
                "[mining] has no miner_address: hayaid needs miner_address or miner_script \
                 for the coinbase of its templates",
            );
        }
        if self.state.flush_interval_blocks == 0 {
            return invalid("flush_interval_blocks must be at least 1");
        }
        if self.state.snapshot_interval_blocks == 0 {
            return invalid("snapshot_interval_blocks must be at least 1");
        }
        if self.sync.memory_budget_bytes < 2 * hayai_wire::MAX_BLOCK_BYTES as u64 {
            return invalid("memory_budget_bytes must be at least 4000000 (two blocks)");
        }
        if self.sync.request_timeout_ms == 0 || self.sync.header_timeout_ms == 0 {
            return invalid("request_timeout_ms and header_timeout_ms must be at least 1");
        }
        if self.sync.header_poll_ms == 0 || self.sync.header_poll_max_ms < self.sync.header_poll_ms
        {
            return invalid("header_poll_ms must be at least 1 and at most header_poll_max_ms");
        }
        if let Some(data) = &self.mining.extra_coinbase_data {
            if data.len() > MAX_EXTRA_COINBASE_DATA {
                return Err(ConfigError::Invalid(format!(
                    "extra_coinbase_data is {} bytes, but the maximum is {MAX_EXTRA_COINBASE_DATA}",
                    data.len()
                )));
            }
        }
        match self.tracing.filter.as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
            _ => invalid("[tracing] filter must be error, warn, info, debug or trace"),
        }
    }
}

/// The commented default configuration of `network`.
pub fn default_toml(network: NetworkKind) -> String {
    let shadow_rpc = |port: u16| {
        format!(
            "\n[shadow]\n\
             # JSON-RPC of the followed zakurad (enable_cookie_auth = false).\n\
             rpc_addr = \"127.0.0.1:{port}\"\n\
             # Validate from start_height + 1. Absent: the upstream tip at start.\n\
             # start_height = 3900000\n\
             # Time between two getbestblockhash polls.\n\
             poll_interval_ms = 200\n"
        )
    };
    let (mode, listen, peers, rpc, metrics, mining, shadow) = match network {
        NetworkKind::Regtest | NetworkKind::ConfiguredRegtest(_) => (
            "full",
            "127.0.0.1:18344",
            "[]",
            "listen_addr = \"127.0.0.1:18345\"\n\
             # Each request needs the credentials of the cookie file (HTTP Basic). false: no\n\
             # authentication, each client that reaches the port can call each method.\n\
             enable_cookie_auth = true\n\
             # Directory of the cookie file `.cookie`. Absent: cache_dir of [state].\n\
             # cookie_dir = \"hayaid-data\"",
            "127.0.0.1:19101",
            "# Regtest address encoding is the Testnet one.\n\
             miner_address = \"tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV\"\n\
             # Serve the `generate n` RPC.\n\
             regtest_produce = true",
            String::new(),
        ),
        NetworkKind::Testnet => (
            "shadow",
            "127.0.0.1:18333",
            "[\"127.0.0.1:18233\"]",
            "# Shadow mode serves no templates: no RPC server.\n# listen_addr = \"127.0.0.1:18345\"",
            "127.0.0.1:19101",
            "# The shadow template is never served; the script only completes the coinbase.\n\
             miner_address = \"tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV\"",
            shadow_rpc(18232),
        ),
        NetworkKind::Mainnet => (
            "shadow",
            "127.0.0.1:8333",
            "[\"127.0.0.1:8233\"]",
            "# Shadow mode serves no templates: no RPC server.\n# listen_addr = \"127.0.0.1:8345\"",
            "127.0.0.1:19101",
            "# The shadow template is never served; the script only completes the coinbase.\n\
             miner_address = \"t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs\"",
            shadow_rpc(8232),
        ),
    };
    format!(
        "# hayaid configuration ({network}). Every key shows its default.\n\
         \n\
         [network]\n\
         # Mainnet, Testnet or Regtest.\n\
         network = \"{network}\"\n\
         # full: own state from genesis, from the peers. shadow: follow a local zakurad.\n\
         mode = \"{mode}\"\n\
         # P2P listen address. Remove the key to only dial out.\n\
         listen_addr = \"{listen}\"\n\
         # Address book: true (in cache_dir of [state]), false (in memory only) or a directory.\n\
         cache_dir = true\n\
         # Peers to dial, and to redial while disconnected.\n\
         peers = {peers}\n\
         # Offer the compact-relay extension. false: a plain legacy node.\n\
         compact_relay = {compact}\n\
         # Candidates of peers' lanes whose layer is prebuilt while idle, at most. 0: off.\n\
         prebuilt_candidates = 0\n\
         # Full mode, the peer manager. Remove a key to take the default of the network.\n\
         # The peer set as in Zakura: 3/2 of the size outbound, 3 times the size inbound.\n\
         # peerset_initial_target_size = 100\n\
         # outbound_peers = 8\n\
         # max_inbound = 64\n\
         # Connections above this count are closed, newest inbound first.\n\
         # max_peers = 16\n\
         # max_connections_per_ip = 1\n\
         # DNS seeders and peers (host:port) of the network of this node.\n\
         # {initial_peers} = [\"host:port\"]\n\
         # ban_secs = 86400\n\
         \n\
         [network.zakura]\n\
         # JSONL trace directory. Absent: tracing is off.\n\
         # trace_dir = \"traces\"\n\
         \n\
         [sync]\n\
         # Full mode. Bound of the downloaded blocks in memory plus 2 MB for each request.\n\
         memory_budget_bytes = 1073741824\n\
         # A peer with a request that sends no block for this time stalls.\n\
         request_timeout_ms = 8000\n\
         # The peer of the header sync is disconnected after this time without an answer.\n\
         header_timeout_ms = 120000\n\
         # Without a header sync in progress: delay of the `getheaders` poll of one peer.\n\
         # The delay doubles after each poll up to the largest value. A new block sets it back.\n\
         header_poll_ms = 30000\n\
         header_poll_max_ms = 480000\n\
         \n\
         [state]\n\
         # Coins database, block files and nothing else. hayaid needs an empty directory.\n\
         cache_dir = \"hayaid-data\"\n\
         # Coins store: memory (MemBacking: log and snapshots) or rocksdb (RocksBacking).\n\
         backend = \"memory\"\n\
         # Blocks between two flushes of the finalized coins.\n\
         flush_interval_blocks = 100\n\
         # Memory backend: blocks between two snapshots. A clean shutdown writes one too.\n\
         snapshot_interval_blocks = 10000\n\
         \n\
         [rpc]\n\
         {rpc}\n\
         \n\
         [metrics]\n\
         # Prometheus endpoint (GET /metrics). Remove the key to disable it.\n\
         endpoint_addr = \"{metrics}\"\n\
         \n\
         [trace]\n\
         # The `node` field of every trace row.\n\
         node = \"hayaid\"\n\
         \n\
         [mempool]\n\
         # Total cost of the transactions in the mempool, at most (ZIP 401).\n\
         tx_cost_limit = 80000000\n\
         \n\
         [mining]\n\
         {mining}\n\
         # Text after `hayai: ` in the coinbase input, at most 86 bytes.\n\
         # extra_coinbase_data = \"\"\n\
         # Keep the layer of the template's body prebuilt: an own block commits as a swap.\n\
         prebuild_own = true\n\
         # Template published to the peers in advance (batch lanes and candidates):\n\
         # all, public (all but the transactions of sendprivatetransaction) or none.\n\
         lane_publication = \"all\"\n\
         {shadow}\
         {regtest}\
         \n\
         [tracing]\n\
         # The log level: error, warn, info, debug or trace.\n\
         filter = \"info\"\n\
         # ANSI colours when the log goes to a terminal.\n\
         use_color = true\n\
         # ANSI colours in each case.\n\
         force_use_color = false\n\
         # The log goes to this file. Absent: stderr.\n\
         # log_file = \"hayaid.log\"\n",
        network = match network {
            NetworkKind::Mainnet => "Mainnet",
            NetworkKind::Testnet => "Testnet",
            NetworkKind::Regtest | NetworkKind::ConfiguredRegtest(_) => "Regtest",
        },
        initial_peers = match network {
            NetworkKind::Mainnet => "initial_mainnet_peers",
            _ => "initial_testnet_peers",
        },
        compact = network.is_regtest(),
        regtest = match network.is_regtest() {
            true => {
                "\n# Regtest only. Each node of the network needs the same activation heights.\n\
                 # [regtest]\n\
                 # Activation heights of the upgrades after NU5. Absent: no activation.\n\
                 # A build without the NU7 rule set stops at the nu7 height.\n\
                 # activation_heights = { nu6 = 200, nu6_1 = 300, nu6_2 = 400, nu6_3 = 500, \
                 nu7 = 600 }\n\
                 # Checkpoints: [height, \"hash\"]. The genesis block is always one.\n\
                 # checkpoints = []\n\
                 # A block at or below this height has the checkpoint path only.\n\
                 # mandatory_checkpoint_height = 0\n\
                 # Outputs of the coinbase of the nu6_1 block, paid by the deferred pool.\n\
                 # A network with an nu6_1 height needs one entry or more.\n\
                 # lockbox_disbursements = [{ address = \"t2...\", amount = 0 }]\n\
                 # Funding streams: a share of the block subsidy in hundredths for each\n\
                 # receiver (\"ECC\", \"ZcashFoundation\", \"MajorGrants\", \"Deferred\"),\n\
                 # with one P2SH address for each 6 blocks of the range (18 from nu7).\n\
                 # The receiver \"Deferred\" is the deferred pool and has no address.\n\
                 # [[regtest.funding_streams]]\n\
                 # height_range = { start = 200, end = 206 }\n\
                 # recipients = [{ receiver = \"Deferred\", numerator = 12 }, \
                 { receiver = \"MajorGrants\", numerator = 8, addresses = [\"t2...\"] }]\n"
            }
            false => "",
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_default_configs_parse_and_pass_the_checks() {
        let regtest = Config::parse(&default_toml(NetworkKind::Regtest)).expect("regtest");
        assert_eq!(regtest.network.network, NetworkKind::Regtest);
        assert_eq!(regtest.network.mode, Mode::Full);
        assert!(regtest.network.compact_relay);
        assert!(regtest.mining.regtest_produce);
        assert_eq!(regtest.mining.lane_publication, LanePublication::All);
        assert_eq!(regtest.state.flush_interval_blocks, 100);
        assert_eq!(regtest.state.backend, Backend::Memory);
        assert_eq!(regtest.state.snapshot_interval_blocks, 10_000);
        assert_eq!(
            regtest.rpc.listen_addr,
            Some("127.0.0.1:18345".parse().unwrap())
        );
        assert!(regtest.rpc.enable_cookie_auth);
        assert_eq!(regtest.rpc.cookie_dir, None);
        let Some(_) = regtest.metrics.endpoint_addr else {
            panic!("metrics on by default");
        };
        let None = regtest.network.zakura.trace_dir else {
            panic!("tracing off by default");
        };
        let testnet = Config::parse(&default_toml(NetworkKind::Testnet)).expect("testnet");
        assert_eq!(testnet.network.mode, Mode::Shadow);
        assert!(!testnet.network.compact_relay);
        let None = testnet.rpc.listen_addr else {
            panic!("shadow serves no RPC");
        };
        let Some(shadow) = testnet.shadow else {
            panic!("shadow section");
        };
        assert_eq!(shadow.rpc_addr, "127.0.0.1:18232".parse().unwrap());
        assert_eq!(shadow.poll_interval_ms, 200);
        let None = shadow.start_height else {
            panic!("start at the upstream tip by default");
        };
    }

    #[test]
    fn the_regtest_section_gives_the_consensus_network() {
        let base = "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n";
        let plain = Config::parse(base).expect("no section");
        assert_eq!(
            plain.consensus_network().expect("a network"),
            NetworkKind::Regtest
        );
        let hash = "00".repeat(31) + "07";
        let text = format!(
            "{base}[regtest]\nactivation_heights = {{ nu6 = 20, nu6_2 = 40, nu7 = 60 }}\n\
             checkpoints = [[30, \"{hash}\"]]\nmandatory_checkpoint_height = 25\n"
        );
        let network = Config::parse(&text)
            .expect("a section")
            .consensus_network()
            .expect("a network");
        assert!(network.is_regtest());
        assert_eq!(network.activation_height(Upgrade::Nu6), Some(20));
        assert_eq!(network.activation_height(Upgrade::Nu6_1), None);
        assert_eq!(network.activation_height(Upgrade::Nu6_2), Some(40));
        assert_eq!(network.activation_height(Upgrade::Nu6_3), None);
        assert_eq!(network.activation_height(Upgrade::Nu7), Some(60));
        assert_eq!(network.mandatory_checkpoint_height(), 25);
        assert_eq!(network.checkpoints().last_height(), Some(30));

        rejects(
            &format!("{base}[regtest]\nactivation_heights = {{ nu6 = 1 }}\n"),
            "[regtest]",
        );
        rejects(
            &format!("{base}[regtest]\nmandatory_checkpoint_height = 5\n"),
            "mandatory checkpoint",
        );
        // The reissuance height of the tests is not a key of the file.
        rejects(
            &format!("{base}[regtest]\ntest_reissuance_height = 5\n"),
            "test_reissuance_height",
        );
        rejects(
            &format!("{base}[regtest]\ncheckpoints = [[3, \"zz\"]]\n"),
            "block hash",
        );
        rejects(
            "[network]\nnetwork = \"Testnet\"\n[mining]\nminer_script = \"51\"\n[regtest]\n",
            "regtest",
        );
    }

    /// The keys `lockbox_disbursements` and `funding_streams` have the names and the
    /// meaning of the Regtest parameters of Zakura.
    #[test]
    fn the_regtest_section_takes_disbursements_and_funding_streams() {
        use hayai_consensus::coinbase::OutputKind;
        use hayai_consensus::funding::Receiver;

        const ADDRESS: &str = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";
        let base = "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n\
                    [regtest]\nactivation_heights = { nu6 = 20, nu6_1 = 30 }\n";
        let disbursement =
            format!("lockbox_disbursements = [{{ address = \"{ADDRESS}\", amount = 7 }}]\n");
        let streams = |addresses: &str| {
            format!(
                "[[regtest.funding_streams]]\nheight_range = {{ start = 11, end = 17 }}\n\
                 [[regtest.funding_streams.recipients]]\nreceiver = \"Deferred\"\nnumerator = 12\n\
                 [[regtest.funding_streams.recipients]]\nreceiver = \"MajorGrants\"\n\
                 numerator = 8\naddresses = [{addresses}]\n"
            )
        };
        let text = format!("{base}{disbursement}{}", streams(&format!("\"{ADDRESS}\"")));
        let network = Config::parse(&text)
            .expect("a section")
            .consensus_network()
            .expect("a network");
        let kinds = |height| -> Vec<OutputKind> {
            let terms = CoinbaseTerms::at(network, height).expect("terms");
            terms.required.iter().map(|output| output.kind).collect()
        };
        let stream = OutputKind::FundingStream(Receiver::MajorGrants);
        assert_eq!(kinds(10), vec![]);
        assert_eq!(kinds(11), vec![stream]);
        assert_eq!(kinds(16), vec![stream]);
        assert_eq!(kinds(17), vec![]);
        assert_eq!(kinds(30), vec![OutputKind::LockboxDisbursement]);
        let terms = CoinbaseTerms::at(network, 30).expect("terms");
        assert_eq!((terms.disbursed, terms.required[0].value), (7, 7));

        // An NU6.1 height without a disbursement: Zakura refuses each block at that
        // height, and the node does not start.
        rejects(base, "lockbox_disbursements");
        rejects(&format!("{base}lockbox_disbursements = []\n"), "NU6.1");
        rejects(
            &format!("{base}lockbox_disbursements = [{{ address = \"x\", amount = 0 }}]\n"),
            "P2SH",
        );
        rejects(
            &format!("{base}{disbursement}{}", streams("")),
            "address periods",
        );
        rejects(
            &format!("{base}{disbursement}unknown_key = 1\n"),
            "unknown_key",
        );
    }

    #[test]
    fn mainnet_default_config_is_a_shadow_node_and_full_mode_is_accepted() {
        let mainnet = Config::parse(&default_toml(NetworkKind::Mainnet)).expect("mainnet");
        assert_eq!(mainnet.network.network, NetworkKind::Mainnet);
        assert_eq!(mainnet.network.mode, Mode::Shadow);
        let Some(shadow) = mainnet.shadow else {
            panic!("shadow section");
        };
        assert_eq!(shadow.rpc_addr, "127.0.0.1:8232".parse().unwrap());
        let full =
            Config::parse("[network]\nnetwork = \"Mainnet\"\n[mining]\nminer_script = \"51\"\n")
                .expect("full mode on Mainnet");
        assert_eq!(full.network.mode, Mode::Full);
        rejects(
            "[network]\nnetwork = \"Mainnet\"\n[mining]\nminer_script = \"51\"\nregtest_produce = true\n",
            "Regtest full node",
        );
    }

    #[test]
    fn the_shipped_docker_configs_parse() {
        for (file, network, mode) in [
            ("hayaid.regtest.toml", NetworkKind::Regtest, Mode::Full),
            ("hayaid.testnet.toml", NetworkKind::Testnet, Mode::Shadow),
            ("hayaid.mainnet.toml", NetworkKind::Mainnet, Mode::Shadow),
        ] {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docker/config")
                .join(file);
            let config = Config::load(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(
                (config.network.network, config.network.mode),
                (network, mode)
            );
        }
    }

    /// The files of the race (`docker/race/config`). For each network, the file of hayaid
    /// and the one of zakurad give hayaid the same network, peer limits and RPC address.
    /// The only key of the file of zakurad that hayaid does not use is the P2P stack.
    #[test]
    fn the_configs_of_the_sync_race_parse() {
        let race = |file: &str| {
            Config::parse(&fixture(&format!("../../docker/race/config/{file}")))
                .unwrap_or_else(|e| panic!("{file}: {e}"))
        };
        for (name, network, rpc) in [
            ("mainnet", NetworkKind::Mainnet, "127.0.0.1:8232"),
            ("testnet", NetworkKind::Testnet, "127.0.0.1:18232"),
        ] {
            let hayaid = race(&format!("hayaid.{name}.toml"));
            let zakurad = race(&format!("zakurad.{name}.toml"));
            assert_eq!(
                zakurad.zakura_unused,
                ["network.p2p_stack: Zakura setting that hayaid does not use"]
            );
            for config in [&hayaid, &zakurad] {
                assert_eq!(config.network.network, network);
                assert_eq!(config.network.mode, Mode::Full);
                assert_eq!(
                    config.network.peer_limits(),
                    PeerLimits {
                        outbound: Some(37),
                        inbound: Some(75),
                        total: 112
                    }
                );
                assert_eq!(config.network.max_connections_per_ip, Some(1));
                assert!(config.network.peers.is_empty());
                assert_eq!(config.network.initial_peers(), None, "the default seeders");
                assert_eq!(config.rpc.listen_addr, Some(rpc.parse().unwrap()));
                assert!(config.rpc.enable_cookie_auth);
                let Some(_) = &config.network.zakura.trace_dir else {
                    panic!("{name}: both nodes write trace tables");
                };
            }
            assert!(!hayaid.network.compact_relay);
        }
        let dry_run = race("hayaid.regtest.toml");
        assert_eq!(dry_run.network.network, NetworkKind::Regtest);
        assert_eq!(dry_run.network.initial_peers(), Some(&Vec::new()));
        assert_eq!(dry_run.network.peers, ["127.0.0.1:38233".parse().unwrap()]);
    }

    #[test]
    fn minimal_config_takes_the_defaults() {
        let c = Config::parse(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[network.zakura]\ntrace_dir = \"t\"\n",
        )
        .expect("parses");
        assert_eq!(c.network.mode, Mode::Full);
        assert_eq!(c.network.max_peers, None);
        assert_eq!(c.network.outbound_peers, None);
        assert_eq!(c.sync.memory_budget_bytes, 1 << 30);
        assert_eq!(c.sync.request_timeout_ms, 8_000);
        assert_eq!(c.sync.header_timeout_ms, 120_000);
        assert_eq!(c.sync.header_poll_ms, 30_000);
        assert_eq!(c.sync.header_poll_max_ms, 480_000);
        let testnet =
            Config::parse("[network]\nnetwork = \"Testnet\"\n[mining]\nminer_script = \"51\"\n")
                .expect("full mode on Testnet");
        assert_eq!(testnet.network.mode, Mode::Full);
        assert!(c.network.peers.is_empty());
        let None = c.network.listen_addr else {
            panic!("no listener by default");
        };
        assert_eq!(c.state.cache_dir, PathBuf::from("hayaid-data"));
        assert_eq!(c.trace.node, "hayaid");
        assert_eq!(c.state.backend, Backend::Memory);
        assert_eq!(c.network.zakura.trace_dir, Some(PathBuf::from("t")));
        assert_eq!(c.tracing.filter, "info");
        assert_eq!(c.mining.lane_publication, LanePublication::All);
        for (value, publication) in [
            ("all", LanePublication::All),
            ("public", LanePublication::Public),
            ("none", LanePublication::None),
        ] {
            let c = Config::parse(&format!(
                "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\nlane_publication = \"{value}\"\n"
            ))
            .expect("parses");
            assert_eq!(c.mining.lane_publication, publication);
        }
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\nlane_publication = \"some\"\n",
            "unknown variant",
        );
    }

    fn rejects(text: &str, needle: &str) {
        match Config::parse(text) {
            Ok(_) => panic!("accepted: {text}"),
            Err(e) => assert!(
                e.to_string().contains(needle),
                "error {e} does not mention {needle}"
            ),
        }
    }

    #[test]
    fn the_cookie_authentication_is_on_unless_the_config_turns_it_off() {
        let rpc = |keys: &str| {
            Config::parse(&format!(
                "[network]\nnetwork = \"Regtest\"\n[rpc]\n{keys}\n[mining]\nminer_script = \"51\"\n"
            ))
            .expect("config")
            .rpc
        };
        let default = rpc("listen_addr = \"192.0.2.1:18345\"");
        assert!(default.enable_cookie_auth);
        assert_eq!(default.open_addr(), None);
        assert!(RpcSection::default().enable_cookie_auth);

        let moved = rpc("listen_addr = \"127.0.0.1:18345\"\ncookie_dir = \"/run/hayai\"");
        assert_eq!(moved.cookie_dir, Some(PathBuf::from("/run/hayai")));

        // Without the cookie, a loopback address is not open to the network. Each other
        // address is.
        let off = "enable_cookie_auth = false";
        let local = rpc(&format!("listen_addr = \"127.0.0.1:18345\"\n{off}"));
        assert!(!local.enable_cookie_auth);
        assert_eq!(local.open_addr(), None);
        assert_eq!(
            rpc(&format!("listen_addr = \"[::1]:18345\"\n{off}")).open_addr(),
            None
        );
        assert_eq!(rpc(off).open_addr(), None);
        for addr in ["192.0.2.1:18345", "0.0.0.0:18345", "[::]:18345"] {
            assert_eq!(
                rpc(&format!("listen_addr = \"{addr}\"\n{off}")).open_addr(),
                Some(addr.parse().unwrap())
            );
        }
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    const NO_MINER: &str = "[mining] has no miner_address: hayaid needs miner_address or \
                            miner_script for the coinbase of its templates";
    const MINER: &str = "[mining]\nminer_address = \"tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV\"\n";

    fn unused(keys: &[&str]) -> Vec<String> {
        keys.iter()
            .map(|key| format!("{key}: Zakura setting that hayaid does not use"))
            .collect()
    }

    /// The keys of the output of `zakurad generate` that hayaid does not use.
    const GENERATE_UNUSED: [&str; 45] = [
        "health.min_connected_peers",
        "health.ready_max_blocks_behind",
        "health.enforce_on_test_networks",
        "health.ready_max_tip_age",
        "mempool.max_transaction_bytes",
        "mempool.eviction_memory_time",
        "mempool.max_datacarrier_bytes",
        "mining.internal_miner",
        "mining.optimistic_block_inventory",
        "network.identity_dir",
        "network.p2p_stack",
        "network.crawl_new_peer_interval",
        "network.expose_peer_addresses",
        "network.zakura.bootstrap_peers",
        "network.zakura.listen_addr",
        "network.zakura.nat_traversal",
        "network.zakura.max_connections",
        "network.zakura.max_connections_per_ip",
        "network.zakura.max_pending_handshakes",
        "network.zakura.stream_open_rate_per_second",
        "network.zakura.message_rate_per_second",
        "rpc.parallel_cpu_threads",
        "rpc.debug_force_finished_sync",
        "rpc.max_response_body_size",
        "state.should_backup_non_finalized_state",
        "state.delete_old_database",
        "state.storage_mode",
        "state.debug_skip_non_finalized_state_backup_task",
        "sync.download_concurrency_limit",
        "sync.checkpoint_verify_concurrency_limit",
        "sync.full_verify_concurrency_limit",
        "sync.zakura_block_apply_concurrency_limit",
        "sync.parallel_cpu_threads",
        "tracing.buffer_limit",
        "tracing.use_journald",
        "zcashd_compat.enabled",
        "zcashd_compat.manage_zcashd",
        "zcashd_compat.zcashd_source",
        "zcashd_compat.zcashd_extra_args",
        "zcashd_compat.block_gossip_peer_ips",
        "zcashd_compat.startup_delay",
        "zcashd_compat.restart_backoff",
        "zcashd_compat.restart_backoff_max",
        "zcashd_compat.restart_reset_after",
        "zcashd_compat.shutdown_grace_period",
    ];

    /// The Docker default configuration of Zakura has no key that hayaid does not use. It
    /// has no miner address, which hayaid needs. With one, it is a Mainnet full node on the
    /// directories and the address of the file.
    #[test]
    fn the_docker_default_configuration_of_zakura_gives_an_exact_report() {
        let text = fixture("tests/fixtures/default-zakura-config.toml");
        let Err(e) = Config::parse(&text) else {
            panic!("no miner address");
        };
        assert_eq!(e.to_string(), NO_MINER);

        let config = Config::parse(&text.replace(
            "# miner_address = \"your_mining_address\"",
            "miner_address = \"t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs\"",
        ))
        .expect("with a miner address");
        assert_eq!(config.zakura_unused, Vec::<String>::new());
        assert_eq!(config.network.network, NetworkKind::Mainnet);
        assert_eq!(config.network.mode, Mode::Full);
        assert_eq!(
            config.network.listen_addr,
            Some("[::]:8233".parse().unwrap())
        );
        let dir = PathBuf::from("/home/zebra/.cache/zakura");
        assert_eq!(config.state.cache_dir, dir);
        assert_eq!(config.rpc.cookie_dir, Some(dir.clone()));
        assert_eq!(config.network.cache_dir, PeerCacheDir::Dir(dir));
        assert!(config.tracing.use_color);
    }

    /// The output of `zakurad generate`: one line for each key that hayaid does not use, in
    /// one report, with the error of the absent miner address. With a miner address the
    /// file is a valid configuration with the same lines as warnings, and hayaid takes the
    /// values of the keys that it has.
    #[test]
    fn the_output_of_zakurad_generate_gives_an_exact_report() {
        let text = fixture("tests/fixtures/zakurad-generate.toml");
        let Err(e) = Config::parse(&text) else {
            panic!("no miner address");
        };
        let lines = unused(&GENERATE_UNUSED);
        assert_eq!(e.to_string(), format!("{NO_MINER}\n{}", lines.join("\n")));

        let config = Config::parse(&text.replace(
            "[mining]",
            "[mining]\nminer_address = \"t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs\"",
        ))
        .expect("with a miner address");
        assert_eq!(config.zakura_unused, lines);
        assert_eq!(config.network.network, NetworkKind::Mainnet);
        assert_eq!(config.network.cache_dir, PeerCacheDir::Enabled(true));
        assert_eq!(
            config.network.peer_limits(),
            PeerLimits {
                outbound: Some(150),
                inbound: Some(300),
                total: 450
            }
        );
        assert_eq!(config.network.max_connections_per_ip, Some(1));
        assert_eq!(
            config.network.initial_peers().map(Vec::len),
            Some(4),
            "initial_mainnet_peers"
        );
        assert_eq!(config.mempool.tx_cost_limit, 80_000_000);
        assert_eq!(
            config.state.cache_dir,
            PathBuf::from("/home/user/.cache/zakura")
        );
        assert!(config.rpc.enable_cookie_auth);
        assert_eq!(config.metrics.endpoint_addr, None);
        assert_eq!(config.tracing.filter, "info");
    }

    /// `docker/config/zakurad.testnet.toml`, the file of the compose stack: the four keys
    /// of `[health]` are the keys that hayaid does not use.
    #[test]
    fn the_zakurad_testnet_file_of_the_repository_gives_an_exact_report() {
        let text = fixture("../../docker/config/zakurad.testnet.toml");
        let health = unused(&[
            "health.listen_addr",
            "health.ready_max_blocks_behind",
            "health.enforce_on_test_networks",
            "health.ready_max_tip_age",
        ]);
        let Err(e) = Config::parse(&text) else {
            panic!("no miner address");
        };
        assert_eq!(e.to_string(), format!("{NO_MINER}\n{}", health.join("\n")));

        let config = Config::parse(&format!("{text}\n{MINER}")).expect("with a miner address");
        assert_eq!(config.zakura_unused, health);
        assert_eq!(config.network.network, NetworkKind::Testnet);
        assert_eq!(
            config.network.listen_addr,
            Some("[::]:18233".parse().unwrap())
        );
        assert_eq!(
            config.network.zakura.trace_dir,
            Some(PathBuf::from("/home/zebra/.cache/zakura/traces"))
        );
        assert_eq!(
            config.rpc.listen_addr,
            Some("127.0.0.1:18232".parse().unwrap())
        );
        assert!(!config.rpc.enable_cookie_auth);
        assert_eq!(
            config.metrics.endpoint_addr,
            Some("0.0.0.0:9999".parse().unwrap())
        );
        assert!(!config.tracing.use_color);
    }

    /// A key of Zakura that changes the consensus rules, the network or a data location
    /// is an error unless its value is the behaviour of hayaid. One error has each such
    /// key and each key that hayaid does not use.
    #[test]
    fn a_zakura_key_that_hayaid_cannot_follow_is_an_error() {
        let base = format!("[network]\nnetwork = \"Regtest\"\n{MINER}");
        let accepted = Config::parse(&format!(
            "{base}[consensus]\ncheckpoint_sync = true\n[state]\nephemeral = false\n\
             [rpc]\ncookie_file_name = \".cookie\"\n"
        ))
        .expect("the values of hayaid");
        assert_eq!(accepted.zakura_unused, Vec::<String>::new());

        let Err(e) = Config::parse(&format!(
            "{base}[consensus]\ncheckpoint_sync = false\n[state]\nephemeral = true\n\
             [rpc]\ncookie_file_name = \"auth\"\n[health]\nlisten_addr = \"127.0.0.1:8080\"\n\
             [network.testnet_parameters]\ndisable_pow = true\n"
        )) else {
            panic!("accepted");
        };
        assert_eq!(
            e.to_string(),
            "consensus.checkpoint_sync = false: Zakura setting that hayaid does not use and \
             cannot ignore (hayaid always uses its checkpoints)\n\
             state.ephemeral = true: Zakura setting that hayaid does not use and cannot ignore \
             (hayaid keeps its state in [state] cache_dir)\n\
             rpc.cookie_file_name = \"auth\": Zakura setting that hayaid does not use and \
             cannot ignore (the cookie file of hayaid is .cookie)\n\
             network.testnet_parameters: Zakura setting that hayaid does not use and cannot \
             ignore (hayaid has the [regtest] section for a Regtest network and no configured \
             Testnet)\n\
             health.listen_addr: Zakura setting that hayaid does not use"
        );

        let Err(e) = Config::parse(&format!(
            "[network.network]\nRegtest = {{}}\n{MINER}[health]\nlisten = 1\n"
        )) else {
            panic!("accepted");
        };
        assert_eq!(
            e.to_string(),
            "network.network: Zakura setting that hayaid does not use in its table form and \
             cannot ignore (hayaid has the [regtest] section for a Regtest network and no \
             configured Testnet)\n\
             health.listen: unknown key"
        );
    }

    /// Each key of the two lists is a key of Zakura and not a key of hayaid: a file with
    /// each key parses, and reports each key of the first list.
    #[test]
    fn each_listed_zakura_key_is_taken_out_of_the_file() {
        let mut root = toml::Table::new();
        let mut set = |key: &str, value: toml::Value| {
            let mut table = &mut root;
            let (sections, leaf) = key.rsplit_once('.').expect("a section");
            for section in sections.split('.') {
                table = table
                    .entry(section)
                    .or_insert(toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .expect("a table");
            }
            table.insert(leaf.to_string(), value);
        };
        for key in ZAKURA_UNUSED.iter().chain(&["consensus.checkpoint_sync"]) {
            set(key, toml::Value::Boolean(true));
        }
        set("network.network", toml::Value::String("Regtest".into()));
        set("mining.miner_script", toml::Value::String("51".into()));
        let text = toml::to_string(&root).expect("TOML");
        let config = Config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(config.zakura_unused, unused(ZAKURA_UNUSED));
    }

    #[test]
    fn the_network_keys_of_zakura_have_the_meaning_of_zakura() {
        let network = |keys: &str| {
            Config::parse(&format!("[network]\n{keys}\n{MINER}"))
                .unwrap_or_else(|e| panic!("{keys}: {e}"))
                .network
        };
        // An address without a port takes the default port of the network.
        for (name, addr, expected) in [
            ("Mainnet", "[::]", "[::]:8233"),
            ("Testnet", "0.0.0.0", "0.0.0.0:18233"),
            ("Regtest", "127.0.0.1", "127.0.0.1:18344"),
            ("Testnet", "127.0.0.1:7", "127.0.0.1:7"),
        ] {
            let section = network(&format!("network = \"{name}\"\nlisten_addr = \"{addr}\""));
            assert_eq!(section.listen_addr, Some(expected.parse().unwrap()));
        }
        // The peer set of Zakura, and the keys of hayaid that replace one limit.
        let default = network("network = \"Testnet\"");
        assert_eq!(
            default.peer_limits(),
            PeerLimits {
                outbound: None,
                inbound: None,
                total: 16
            }
        );
        let sized = network("network = \"Testnet\"\npeerset_initial_target_size = 25");
        assert_eq!(
            sized.peer_limits(),
            PeerLimits {
                outbound: Some(37),
                inbound: Some(75),
                total: 112
            }
        );
        let mixed = network(
            "network = \"Testnet\"\npeerset_initial_target_size = 25\noutbound_peers = 4\n\
             max_peers = 30",
        );
        assert_eq!(
            mixed.peer_limits(),
            PeerLimits {
                outbound: Some(4),
                inbound: Some(75),
                total: 30
            }
        );
        // The seeders: the key of the network of the node.
        let peers = "initial_mainnet_peers = [\"m.example:8233\"]\n\
                     initial_testnet_peers = [\"t.example:18233\"]";
        for (name, expected) in [
            ("Mainnet", "m.example:8233"),
            ("Testnet", "t.example:18233"),
            ("Regtest", "t.example:18233"),
        ] {
            let section = network(&format!("network = \"{name}\"\n{peers}"));
            assert_eq!(section.initial_peers(), Some(&vec![expected.to_string()]));
        }
        assert_eq!(default.initial_peers(), None);
        // The directory of the address book.
        let state = Path::new("state");
        assert_eq!(default.peer_cache_dir(state), Some(state));
        let off = network("network = \"Testnet\"\ncache_dir = false");
        assert_eq!(off.peer_cache_dir(state), None);
        let moved = network("network = \"Testnet\"\ncache_dir = \"/var/cache/peers\"");
        assert_eq!(
            moved.peer_cache_dir(state),
            Some(Path::new("/var/cache/peers"))
        );
    }

    #[test]
    fn the_extra_coinbase_data_has_a_length_limit() {
        let mining = |data: &str| {
            Config::parse(&format!(
                "[network]\nnetwork = \"Regtest\"\n{MINER}extra_coinbase_data = \"{data}\"\n"
            ))
        };
        let config = mining(&"x".repeat(MAX_EXTRA_COINBASE_DATA)).expect("the longest text");
        assert_eq!(
            config.mining.extra_coinbase_data.map(|data| data.len()),
            Some(86)
        );
        let Err(e) = mining(&"x".repeat(MAX_EXTRA_COINBASE_DATA + 1)) else {
            panic!("too long");
        };
        assert_eq!(
            e.to_string(),
            "extra_coinbase_data is 87 bytes, but the maximum is 86"
        );
    }

    #[test]
    fn impossible_combinations_are_errors() {
        rejects(
            "[network]\nnetwork = \"Testnet\"\nmode = \"shadow\"\n",
            "[shadow] section",
        );
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[sync]\nmemory_budget_bytes = 1000\n",
            "two blocks",
        );
        rejects(
            "[network]\nnetwork = \"Testnet\"\nmode = \"shadow\"\n[shadow]\nrpc_addr = \"127.0.0.1:1\"\n[rpc]\nlisten_addr = \"127.0.0.1:2\"\n[mining]\nminer_script = \"51\"\n",
            "serves no RPC",
        );
        rejects(
            "[network]\nnetwork = \"Regtest\"\n",
            "miner_address or miner_script",
        );
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\nminer_address = \"x\"\n",
            "not both",
        );
        rejects(
            "[network]\nnetwork = \"Testnet\"\nmode = \"shadow\"\n[shadow]\nrpc_addr = \"127.0.0.1:1\"\n[mining]\nregtest_produce = true\n",
            "Regtest full node",
        );
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[tracing]\nfilter = \"loud\"\n",
            "[tracing] filter",
        );
    }

    #[test]
    fn unknown_keys_and_values_are_errors() {
        rejects(
            "[network]\nnetwork = \"Regtest\"\nlisten = \"x\"\n",
            "unknown field",
        );
        rejects(
            "[network]\nnetwork = \"signet\"\n",
            "unknown network signet",
        );
        // The lower-case names of older files are not names of a network.
        rejects(
            "[network]\nnetwork = \"regtest\"\n",
            "unknown network regtest",
        );
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nbackend = \"lmdb\"\n",
            "unknown variant",
        );
        let rocks = Config::parse(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nbackend = \"rocksdb\"\n",
        )
        .expect("rocksdb backend");
        assert_eq!(rocks.state.backend, Backend::Rocksdb);
        rejects(
            "[network]\nnetwork = \"Regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nflush_interval_blocks = 0\n",
            "at least 1",
        );
    }
}
