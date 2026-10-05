//! The configuration file (TOML) and its defaults.
//!
//! `hayaid config --network <name>` prints [`default_toml`]: every key with its default and
//! a comment. Unknown keys are errors, so a misspelt key never falls back to a default.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    pub shadow: Option<ShadowSection>,
    pub regtest: Option<RegtestSection>,
    #[serde(default)]
    pub log: LogSection,
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
    pub network: NetworkKind,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    /// P2P listen address. Absent: the node only dials out.
    pub listen_addr: Option<SocketAddr>,
    /// Peers to dial and to redial while they are not connected.
    #[serde(default)]
    pub peers: Vec<SocketAddr>,
    /// Offer the compact-relay extension (`zcmpctver`). Off: a plain legacy node.
    #[serde(default = "default_true")]
    pub compact_relay: bool,
    /// Connections above this count are closed, newest inbound first.
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,
    /// Candidates of peers' lanes on the tip whose layer the node prebuilds while idle, at
    /// most. A block equal to one commits as a pointer swap. 0: off.
    #[serde(default)]
    pub prebuilt_candidates: usize,
    /// Full mode: outbound peers that the peer manager keeps. Absent: the default of
    /// `hayai_net::PeerConfig` (8).
    pub outbound_peers: Option<usize>,
    /// Inbound peers accepted, at most. Absent: 64.
    pub max_inbound: Option<usize>,
    /// Connections with one IP address, at most. Absent: 1, and no bound on Regtest.
    pub max_per_ip: Option<usize>,
    /// DNS seeders as `host:port`. Absent: the seeders of the network.
    pub seeders: Option<Vec<String>>,
    /// Duration of a ban in seconds. Absent: 86,400.
    pub ban_secs: Option<u64>,
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
}

impl Default for SyncSection {
    fn default() -> Self {
        Self {
            memory_budget_bytes: default_memory_budget(),
            request_timeout_ms: default_request_timeout(),
            header_timeout_ms: default_header_timeout(),
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
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
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
            data_dir: default_data_dir(),
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
    /// Directory of the cookie file `.cookie`. Absent: `[state] data_dir`.
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
    pub listen_addr: Option<SocketAddr>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceSection {
    /// Directory of the JSONL trace tables. Absent: tracing is off.
    pub dir: Option<PathBuf>,
    /// Value of the `node` field of every row.
    #[serde(default = "default_node")]
    pub node: String,
}

impl Default for TraceSection {
    fn default() -> Self {
        Self {
            dir: None,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    /// `error`, `warn`, `info`, `debug` or `trace`.
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LogSection {
    fn default() -> Self {
        Self {
            level: default_log_level(),
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
fn default_max_peers() -> usize {
    16
}
fn default_data_dir() -> PathBuf {
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
fn default_log_level() -> String {
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
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parses and checks the combinations the node cannot run.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.check()?;
        Ok(config)
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
                return invalid("the [regtest] section applies to network = \"regtest\" only");
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
            return invalid("the template coinbase needs miner_address or miner_script");
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
        match self.log.level.as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
            _ => invalid("log level must be error, warn, info, debug or trace"),
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
             # Directory of the cookie file `.cookie`. Absent: data_dir.\n\
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
         network = \"{network}\"\n\
         # full: own state from genesis, from the peers. shadow: follow a local zakurad.\n\
         mode = \"{mode}\"\n\
         # P2P listen address. Remove the key to only dial out.\n\
         listen_addr = \"{listen}\"\n\
         # Peers to dial, and to redial while disconnected.\n\
         peers = {peers}\n\
         # Offer the compact-relay extension. false: a plain legacy node.\n\
         compact_relay = {compact}\n\
         # Connections above this count are closed, newest inbound first.\n\
         max_peers = 16\n\
         # Candidates of peers' lanes whose layer is prebuilt while idle, at most. 0: off.\n\
         prebuilt_candidates = 0\n\
         # Full mode, the peer manager. Remove a key to take the default of the network.\n\
         # outbound_peers = 8\n\
         # max_inbound = 64\n\
         # max_per_ip = 1\n\
         # seeders = [\"host:port\"]\n\
         # ban_secs = 86400\n\
         \n\
         [sync]\n\
         # Full mode. Bound of the downloaded blocks in memory plus 2 MB for each request.\n\
         memory_budget_bytes = 1073741824\n\
         # A peer with a request that sends no block for this time stalls.\n\
         request_timeout_ms = 8000\n\
         # The peer of the header sync is disconnected after this time without an answer.\n\
         header_timeout_ms = 120000\n\
         \n\
         [state]\n\
         # Coins database, block files and nothing else. hayaid needs an empty directory.\n\
         data_dir = \"hayaid-data\"\n\
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
         listen_addr = \"{metrics}\"\n\
         \n\
         [trace]\n\
         # JSONL trace directory. Absent: tracing is off.\n\
         # dir = \"traces\"\n\
         # The `node` field of every trace row.\n\
         node = \"hayaid\"\n\
         \n\
         [mining]\n\
         {mining}\n\
         # Keep the layer of the template's body prebuilt: an own block commits as a swap.\n\
         prebuild_own = true\n\
         # Template published to the peers in advance (batch lanes and candidates):\n\
         # all, public (all but the transactions of sendprivatetransaction) or none.\n\
         lane_publication = \"all\"\n\
         {shadow}\
         {regtest}\
         \n\
         [log]\n\
         # error, warn, info, debug or trace.\n\
         level = \"info\"\n",
        network = network.name(),
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
        let Some(_) = regtest.metrics.listen_addr else {
            panic!("metrics on by default");
        };
        let None = regtest.trace.dir else {
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
        let base = "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n";
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
            "[network]\nnetwork = \"testnet\"\n[mining]\nminer_script = \"51\"\n[regtest]\n",
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
        let base = "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n\
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
            Config::parse("[network]\nnetwork = \"mainnet\"\n[mining]\nminer_script = \"51\"\n")
                .expect("full mode on Mainnet");
        assert_eq!(full.network.mode, Mode::Full);
        rejects(
            "[network]\nnetwork = \"mainnet\"\n[mining]\nminer_script = \"51\"\nregtest_produce = true\n",
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

    #[test]
    fn minimal_config_takes_the_defaults() {
        let c = Config::parse(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[trace]\ndir = \"t\"\n",
        )
        .expect("parses");
        assert_eq!(c.network.mode, Mode::Full);
        assert_eq!(c.network.max_peers, 16);
        assert_eq!(c.network.outbound_peers, None);
        assert_eq!(c.sync.memory_budget_bytes, 1 << 30);
        assert_eq!(c.sync.request_timeout_ms, 8_000);
        assert_eq!(c.sync.header_timeout_ms, 120_000);
        let testnet =
            Config::parse("[network]\nnetwork = \"testnet\"\n[mining]\nminer_script = \"51\"\n")
                .expect("full mode on Testnet");
        assert_eq!(testnet.network.mode, Mode::Full);
        assert!(c.network.peers.is_empty());
        let None = c.network.listen_addr else {
            panic!("no listener by default");
        };
        assert_eq!(c.state.data_dir, PathBuf::from("hayaid-data"));
        assert_eq!(c.trace.node, "hayaid");
        assert_eq!(c.state.backend, Backend::Memory);
        assert_eq!(c.trace.dir, Some(PathBuf::from("t")));
        assert_eq!(c.log.level, "info");
        assert_eq!(c.mining.lane_publication, LanePublication::All);
        for (value, publication) in [
            ("all", LanePublication::All),
            ("public", LanePublication::Public),
            ("none", LanePublication::None),
        ] {
            let c = Config::parse(&format!(
                "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\nlane_publication = \"{value}\"\n"
            ))
            .expect("parses");
            assert_eq!(c.mining.lane_publication, publication);
        }
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\nlane_publication = \"some\"\n",
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
                "[network]\nnetwork = \"regtest\"\n[rpc]\n{keys}\n[mining]\nminer_script = \"51\"\n"
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

    #[test]
    fn impossible_combinations_are_errors() {
        rejects(
            "[network]\nnetwork = \"testnet\"\nmode = \"shadow\"\n",
            "[shadow] section",
        );
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[sync]\nmemory_budget_bytes = 1000\n",
            "two blocks",
        );
        rejects(
            "[network]\nnetwork = \"testnet\"\nmode = \"shadow\"\n[shadow]\nrpc_addr = \"127.0.0.1:1\"\n[rpc]\nlisten_addr = \"127.0.0.1:2\"\n[mining]\nminer_script = \"51\"\n",
            "serves no RPC",
        );
        rejects(
            "[network]\nnetwork = \"regtest\"\n",
            "miner_address or miner_script",
        );
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\nminer_address = \"x\"\n",
            "not both",
        );
        rejects(
            "[network]\nnetwork = \"testnet\"\nmode = \"shadow\"\n[shadow]\nrpc_addr = \"127.0.0.1:1\"\n[mining]\nregtest_produce = true\n",
            "Regtest full node",
        );
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[log]\nlevel = \"loud\"\n",
            "log level",
        );
    }

    #[test]
    fn unknown_keys_and_values_are_errors() {
        rejects(
            "[network]\nnetwork = \"regtest\"\nlisten = \"x\"\n",
            "unknown field",
        );
        rejects("[network]\nnetwork = \"signet\"\n", "unknown variant");
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nbackend = \"lmdb\"\n",
            "unknown variant",
        );
        let rocks = Config::parse(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nbackend = \"rocksdb\"\n",
        )
        .expect("rocksdb backend");
        assert_eq!(rocks.state.backend, Backend::Rocksdb);
        rejects(
            "[network]\nnetwork = \"regtest\"\n[mining]\nminer_script = \"51\"\n[state]\nflush_interval_blocks = 0\n",
            "at least 1",
        );
    }
}
