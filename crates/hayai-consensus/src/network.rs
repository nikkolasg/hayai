//! The networks, their parameters and the activation heights of the network upgrades.
//!
//! A [`ChainSpec`] holds each value of a network that the rules read. The rules of each
//! upgrade are code (`hayai_consensus_core::rule_sets`): a chain chooses only when each
//! upgrade activates, and the values of its spec. Mainnet, Testnet and Regtest have built-in
//! specs ([`Network::spec`]). Another chain is a [`Network::Custom`]: a crate clones a
//! built-in spec, changes its fields, and calls [`ChainSpec::network`].
//!
//! The core reads a [`CoreSpec`]: the same values without the names, the network type and
//! the checkpoints, and with each address decoded to its script. [`ChainSpec::network`]
//! builds it one time, and the three built-in specs hold it as constant data
//! ([`Network::core`]).
//!
//! Regtest follows Zakura's Regtest (`zakura-chain/src/parameters/network/testnet.rs`,
//! `Parameters::new_regtest`): the zcashd Regtest genesis block, Overwinter to Canopy at
//! height 1 (Zakura's default), NU5 at height 1 (Zakura's `[network.
//! testnet_parameters.activation_heights] NU5 = 1`, which a Zakura node in a pair must set),
//! no slow start, a pre-Blossom halving interval of 144 blocks, the proof-of-work limit
//! `0x0f0f…0f` and Equihash (48, 5). Mainnet and Testnet have the activation heights of the
//! deployment ZIPs, which are the heights of the crypto backend's `zcash_protocol`, and
//! Equihash (200, 9).
//!
//! A Regtest network can have its own activation heights for the upgrades after NU5, its
//! own checkpoint list, its own mandatory checkpoint height, its own funding streams and
//! its own lockbox disbursements ([`RegtestConfig`]). Every other value is the value of
//! [`Network::Regtest`].

use std::sync::OnceLock;

use hayai_consensus_core::funding::StreamSet as CoreStreamSet;
use hayai_consensus_core::lockbox::Disbursement as CoreDisbursement;
use hayai_consensus_core::{CoreSpec, P2shScript, SpecError, UPGRADES};
use hayai_crypto::zcash_protocol::consensus::{BranchId, NetworkType};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::ptr;

use hayai_wire::header::{BlockHash, PowParams};
use serde::{Deserialize, Serialize};

use crate::address::p2sh_script;
use crate::funding::{self, Receiver, StreamSet};
use crate::lockbox::{self, Disbursement};
use crate::{
    checkpoints, founders, nsm, Checkpoints, ConsensusError, DuplicateCheckpoint, Upgrade,
};

/// The networks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
    /// A chain with the values of a [`ChainSpec`]: a fork, a test network, or Regtest with
    /// the values of a [`RegtestConfig`]. [`ChainSpec::network`] makes this value after it
    /// checks the values. A configuration file has no name for it: the node makes the
    /// value from its configuration.
    #[serde(skip)]
    Custom(CheckedSpec),
}

/// A [`ChainSpec`] that [`ChainSpec::network`] checked, with its [`CoreSpec`]. The spec
/// stays in memory until the process ends. Two values are equal when they are the same
/// spec in memory.
#[derive(Clone, Copy)]
pub struct CheckedSpec(&'static CustomSpec);

/// A checked spec with the core that [`ChainSpec::network`] derived from it.
struct CustomSpec {
    spec: ChainSpec,
    core: CoreSpec,
}

impl PartialEq for CheckedSpec {
    fn eq(&self, other: &Self) -> bool {
        ptr::eq(self.0, other.0)
    }
}

impl Eq for CheckedSpec {}

impl Hash for CheckedSpec {
    fn hash<H: Hasher>(&self, state: &mut H) {
        ptr::hash(self.0, state);
    }
}

impl fmt::Debug for CheckedSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CheckedSpec")
            .field(&self.0.spec.name)
            .finish()
    }
}

/// The cores of the built-in networks, built on the first use ([`Network::core`]).
static BUILT_IN_CORES: [OnceLock<CoreSpec>; 3] =
    [OnceLock::new(), OnceLock::new(), OnceLock::new()];

/// The values of one chain. The rules read each value of a network from its spec.
///
/// The rules of an upgrade are code, so a chain has the upgrades of [`Upgrade`]. A chain
/// starts from a clone of the spec of a built-in network ([`Network::spec`]), changes the
/// public fields, and gets its [`Network`] from [`ChainSpec::network`]. The private field
/// holds the [`CoreSpec`] that [`ChainSpec::network`] derives from the public fields.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChainSpec {
    /// The name of the network ([`Network::name`]).
    pub name: &'static str,
    /// The network type of `zcash_protocol`: it selects the address encodings of the chain
    /// (Base58Check prefixes, Bech32 human-readable parts). `Regtest` also selects the
    /// rules and the methods that only Regtest has ([`Network::is_regtest`]).
    pub network_type: NetworkType,
    pub params: NetworkParams,
    /// The activation height of each upgrade, at the index `Upgrade::index`. `None`: the
    /// upgrade does not activate. Sprout activates at height 0.
    pub activation_heights: [Option<u32>; UPGRADES],
    /// The checkpoint list ([`Network::checkpoints`]).
    pub checkpoints: Checkpoints,
    /// The height at or below which a block has no full validation
    /// ([`Network::mandatory_checkpoint_height`]).
    pub mandatory_checkpoint_height: u32,
    /// The funding stream sets, in height order ([`crate::funding`]).
    pub funding_streams: &'static [StreamSet],
    /// The outputs that the coinbase of the NU6.1 activation block must have
    /// ([`crate::lockbox::disbursements`]).
    pub lockbox_disbursements: &'static [Disbursement],
    /// `FounderAddressList` (Spec §7.9). Empty: the network has no founders' reward
    /// ([`crate::founders`]).
    pub founders_addresses: &'static [&'static str],
    /// `INITIAL_NSM_VALUE_BALANCE` (ZIP 237, [`crate::nsm::expected_seed`]). `None`: the
    /// balance before NU7 is the balance that the chain gives.
    pub nsm_seed: Option<u64>,
    /// The NSM reissuance height of a test
    /// ([`RegtestConfig::with_test_reissuance_height`]). `None` on each built-in network.
    pub test_reissuance_height: Option<u32>,
    /// The tables of the core of a built-in network, as the compiler evaluates them.
    core_tables: CoreTables,
}

/// The tables of the core of a built-in network, with the scripts of the addresses: the
/// form that the compiler evaluates. [`Network::core`] builds the [`CoreSpec`] from them
/// on the first use. A spec of [`ChainSpec::network`] derives its core from its public
/// fields and does not read them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct CoreTables {
    funding: &'static [funding::ScriptSet],
    lockbox: &'static [CoreDisbursement],
    founders: &'static [P2shScript],
    /// `HeightForHalving(1)` of the network, the value that `CoreSpec::checked` derives
    /// (the test `the_first_halving_of_each_built_in_spec_is_the_derived_one`).
    first_halving: Option<u32>,
}

/// The core spec of `params`, `heights` and the decoded tables, with `first_halving`.
#[allow(clippy::too_many_arguments)]
fn core_spec(
    params: &NetworkParams,
    heights: [Option<u32>; UPGRADES],
    funding_streams: Vec<CoreStreamSet>,
    lockbox_disbursements: Vec<CoreDisbursement>,
    founders_scripts: Vec<P2shScript>,
    nsm_seed: Option<u64>,
    test_reissuance_height: Option<u32>,
    first_halving: Option<u32>,
) -> CoreSpec {
    CoreSpec {
        activation_heights: heights,
        slow_start_interval: params.slow_start_interval,
        pre_blossom_halving_interval: params.pre_blossom_halving_interval,
        pow_limit: params.pow_limit,
        pow_limit_bits: params.pow_limit_bits,
        disable_pow: params.disable_pow,
        min_difficulty_start_height: params.min_difficulty_start_height,
        max_time_start_height: params.max_time_start_height,
        orchard_disabled_start_height: params.orchard_disabled_start_height,
        coinbase_must_be_shielded: params.coinbase_must_be_shielded,
        funding_streams,
        lockbox_disbursements,
        founders_scripts,
        nsm_seed,
        test_reissuance_height,
        first_halving,
    }
}

impl ChainSpec {
    /// The network of this spec, after these checks:
    /// - Each funding stream address, lockbox disbursement address and founders' address
    ///   is a P2SH address of the network type.
    /// - A network with the Mainnet network type has no NU7 height: this crate has no code
    ///   for the ZIP 2008 recipient of Mainnet.
    /// - The checkpoint at height 0 is the genesis block, and the last checkpoint is at or
    ///   above the mandatory checkpoint height (Zakura `check_checkpoint_coverage`).
    /// - The checks of `CoreSpec::checked` on the values that the rules read: Sprout at
    ///   height 0 and the order of the activation heights, the halving interval, the slow
    ///   start, the first halving, the funding stream sets and their address counts, the
    ///   lockbox amounts, and the Orchard soft fork inside NU6.1.
    ///
    /// The spec stays in memory until the process ends. A node calls this function one
    /// time at its start.
    pub fn network(mut self) -> Result<Network, ChainSpecError> {
        let network_type = self.network_type;
        let nu7 = self.activation_heights[Upgrade::Nu7.index()];
        let funding_streams = funding::core_sets(network_type, self.funding_streams)?;
        let lockbox_disbursements =
            lockbox::core_disbursements(network_type, self.lockbox_disbursements)?;
        let founders_scripts = founders::core_scripts(network_type, self.founders_addresses)?;
        if let (NetworkType::Main, Some(_)) = (network_type, nu7) {
            return Err(ChainSpecError::MainnetNu7);
        }
        self.check_checkpoints()?;
        let core = core_spec(
            &self.params,
            self.activation_heights,
            funding_streams,
            lockbox_disbursements,
            founders_scripts,
            self.nsm_seed,
            self.test_reissuance_height,
            None,
        );
        let core = core.checked()?;
        self.checkpoints = self.checkpoints.leak();
        Ok(Network::Custom(CheckedSpec(Box::leak(Box::new(
            CustomSpec { spec: self, core },
        )))))
    }

    /// The core of a built-in network, from its tables.
    fn build_core(&self) -> CoreSpec {
        let tables = &self.core_tables;
        core_spec(
            &self.params,
            self.activation_heights,
            funding::core_sets_of(tables.funding),
            tables.lockbox.to_vec(),
            tables.founders.to_vec(),
            self.nsm_seed,
            self.test_reissuance_height,
            tables.first_halving,
        )
    }

    /// The checkpoint at height 0 is the genesis block, and the last checkpoint is at or
    /// above the mandatory checkpoint height (Zakura `check_checkpoint_coverage`).
    fn check_checkpoints(&self) -> Result<(), ChainSpecError> {
        if self.checkpoints.hash_at(0) != Some(self.params.genesis_hash) {
            return Err(ChainSpecError::Genesis);
        }
        let mandatory = self.mandatory_checkpoint_height;
        if !matches!(self.checkpoints.last_height(), Some(last) if last >= mandatory) {
            return Err(ChainSpecError::Coverage(mandatory));
        }
        Ok(())
    }
}

/// The upgrades after NU5, whose Regtest activation height a [`RegtestConfig`] can set.
const CONFIGURABLE: [Upgrade; 5] = [
    Upgrade::Nu6,
    Upgrade::Nu6_1,
    Upgrade::Nu6_2,
    Upgrade::Nu6_3,
    Upgrade::Nu7,
];

/// One output that the coinbase of the NU6.1 activation block of a Regtest network must
/// have (Zakura `ConfiguredLockboxDisbursement`). The deferred pool pays it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegtestDisbursement {
    /// A Base58Check P2SH address of any network.
    pub address: String,
    /// Zatoshis.
    pub amount: u64,
}

/// The funding streams of one range of heights of a Regtest network (Zakura
/// `ConfiguredFundingStreams`). The field names and the receiver names are those of a
/// Zakura configuration file.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegtestFundingStreams {
    /// The first height of the streams, and the first height after them.
    pub height_range: Range<u32>,
    pub recipients: Vec<RegtestRecipient>,
}

/// One funding stream of a [`RegtestFundingStreams`] (Zakura
/// `ConfiguredFundingStreamRecipient`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegtestRecipient {
    #[serde(with = "crate::funding::ReceiverDef")]
    pub receiver: Receiver,
    /// The share of the block subsidy, in hundredths.
    pub numerator: u64,
    /// One Base58Check P2SH address of any network for each address period of the range,
    /// from the period of the start height. A period has 6 blocks before NU7 and 18
    /// blocks from NU7. Empty for [`Receiver::Deferred`].
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// The values of a Regtest network that a node operator or a test can set: a builder of
/// the [`ChainSpec`] of a Regtest network.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RegtestConfig {
    /// The values of [`Network::Regtest`], with the values of the configuration.
    spec: ChainSpec,
}

/// Why a [`ChainSpec`] or a [`RegtestConfig`] is not valid.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ChainSpecError {
    #[error("the activation height of Sprout is not 0")]
    Sprout,
    #[error("the Regtest activation height of {0:?} is not configurable")]
    FixedUpgrade(Upgrade),
    #[error(
        "the activation height {height} of {upgrade:?} is below the height of an earlier \
         upgrade, or it is a Regtest height that is not above 1"
    )]
    Order { upgrade: Upgrade, height: u32 },
    #[error(transparent)]
    Checkpoints(#[from] DuplicateCheckpoint),
    #[error("the checkpoint at height 0 is not the genesis block")]
    Genesis,
    #[error("the last checkpoint is below the mandatory checkpoint height {0}")]
    Coverage(u32),
    #[error("the heights without the Orchard pool from height {0} are not all in NU6.1")]
    OrchardSoftFork(u32),
    #[error(
        "the pre-Blossom halving interval {0} gives an address period of 0 blocks or does \
         not fit in 32 bits after Blossom"
    )]
    HalvingInterval(u32),
    #[error("the slow start interval {0} is 1 block or does not end before the first halving")]
    SlowStart(u32),
    #[error("the network has funding streams and no height has the halving index 1")]
    NoFirstHalving,
    #[error("NU7 moves the funding stream end {0} above the largest height")]
    StreamEnd(u32),
    #[error("a network with the Mainnet network type has an NU7 height, and the ZIP 2008 rule has no code")]
    MainnetNu7,
    #[error("{address} is not a P2SH address: {reason}")]
    Address { address: String, reason: String },
    #[error("a lockbox disbursement, or the sum of all, is above MAX_MONEY zatoshis")]
    DisbursementAmount,
    #[error("the funding stream range from {start} to {end} ends below its start")]
    StreamRange { start: u32, end: u32 },
    #[error("the funding stream receiver {0:?} is two times in one range")]
    StreamReceiver(Receiver),
    #[error("the funding stream numerators of one range have the sum {0}, above 100")]
    StreamNumerators(u128),
    #[error("the deferred pool is a funding stream receiver without an address")]
    DeferredAddress,
    #[error(
        "the funding stream receiver {receiver:?} has {found} addresses and its range from \
         {start} to {end} has {required} address periods"
    )]
    StreamAddresses {
        receiver: Receiver,
        start: u32,
        end: u32,
        required: usize,
        found: usize,
    },
    /// A rule of the core failed on the values of the spec during the checks.
    #[error(transparent)]
    Rule(ConsensusError),
}

impl From<SpecError> for ChainSpecError {
    fn from(error: SpecError) -> Self {
        match error {
            SpecError::Sprout => ChainSpecError::Sprout,
            SpecError::Order { upgrade, height } => ChainSpecError::Order { upgrade, height },
            SpecError::HalvingInterval(interval) => ChainSpecError::HalvingInterval(interval),
            SpecError::SlowStart(interval) => ChainSpecError::SlowStart(interval),
            SpecError::NoFirstHalving => ChainSpecError::NoFirstHalving,
            SpecError::StreamEnd(end) => ChainSpecError::StreamEnd(end),
            SpecError::StreamRange { start, end } => ChainSpecError::StreamRange { start, end },
            SpecError::StreamReceiver(receiver) => ChainSpecError::StreamReceiver(receiver),
            SpecError::StreamNumerators(sum) => ChainSpecError::StreamNumerators(sum),
            SpecError::DeferredScript => ChainSpecError::DeferredAddress,
            SpecError::StreamScripts {
                receiver,
                start,
                end,
                required,
                found,
            } => ChainSpecError::StreamAddresses {
                receiver,
                start,
                end,
                required,
                found,
            },
            SpecError::DisbursementAmount => ChainSpecError::DisbursementAmount,
            SpecError::OrchardSoftFork(start) => ChainSpecError::OrchardSoftFork(start),
            SpecError::Rule(error) => ChainSpecError::Rule(error),
        }
    }
}

/// Checks that `address` is a P2SH address of `network_type` (of any network for Regtest).
pub(crate) fn check_address(
    network_type: NetworkType,
    address: &str,
) -> Result<(), ChainSpecError> {
    match p2sh_script(network_type, address) {
        Ok(_) => Ok(()),
        Err(reason) => Err(ChainSpecError::Address {
            address: address.to_string(),
            reason,
        }),
    }
}

/// The value of a rule of the core on a checked spec. The core returns an error for a value
/// that `CoreSpec::checked` refuses; every spec of a [`Network`] passed the checks, so the
/// error does not occur.
pub(crate) fn checked<T>(result: Result<T, ConsensusError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => unreachable!("a checked spec gives no error to a rule of the core: {error}"),
    }
}

impl RegtestConfig {
    /// A Regtest network with the activation heights `activations` for upgrades after
    /// NU5, the checkpoints `checkpoints` (the genesis block is always a checkpoint) and
    /// the mandatory checkpoint height `mandatory_checkpoint_height` (0: the genesis
    /// block, as on [`Network::Regtest`]).
    ///
    /// The activation heights must be above 1 and must not decrease in upgrade order. The
    /// last checkpoint must be at or above the mandatory checkpoint height (Zakura
    /// `check_checkpoint_coverage`).
    pub fn new(
        activations: &[(Upgrade, u32)],
        mut checkpoints: Vec<(u32, BlockHash)>,
        mandatory_checkpoint_height: u32,
    ) -> Result<Self, ChainSpecError> {
        let mut activation_heights = [None; 5];
        for (upgrade, height) in activations {
            let Some(slot) = CONFIGURABLE.iter().position(|u| u == upgrade) else {
                return Err(ChainSpecError::FixedUpgrade(*upgrade));
            };
            activation_heights[slot] = Some(*height);
        }
        let mut floor = 2;
        for (upgrade, height) in CONFIGURABLE.into_iter().zip(activation_heights) {
            let Some(height) = height else { continue };
            if height < floor {
                return Err(ChainSpecError::Order { upgrade, height });
            }
            floor = height;
        }
        let mut spec = REGTEST.clone();
        for (upgrade, height) in CONFIGURABLE.into_iter().zip(activation_heights) {
            spec.activation_heights[upgrade.index()] = height;
        }
        if !checkpoints.iter().any(|(height, _)| *height == 0) {
            checkpoints.push((0, spec.params.genesis_hash));
        }
        spec.checkpoints = Checkpoints::new(checkpoints)?;
        spec.mandatory_checkpoint_height = mandatory_checkpoint_height;
        spec.check_checkpoints()?;
        Ok(Self { spec })
    }

    /// Sets the outputs that the coinbase of the NU6.1 activation block must have, with
    /// the meaning of `lockbox_disbursements` of Zakura's Regtest parameters
    /// (`zakura-chain/src/parameters/network/testnet.rs`, `check_lockbox_disbursements`):
    /// each address is a P2SH address of any network, and the sum of the amounts is a
    /// valid amount of money.
    ///
    /// A network with an NU6.1 height and no disbursement has no valid block at that
    /// height while the block subsidy is not 0, as in Zakura
    /// ([`ConsensusError::NoLockboxDisbursement`]).
    ///
    /// The disbursements stay in memory until the process ends, as the configuration does
    /// ([`RegtestConfig::network`]).
    pub fn with_lockbox_disbursements(
        mut self,
        disbursements: Vec<RegtestDisbursement>,
    ) -> Result<Self, ChainSpecError> {
        let outputs = disbursements
            .into_iter()
            .map(|disbursement| Disbursement {
                count: 1,
                value: disbursement.amount,
                address: Box::leak(disbursement.address.into_boxed_str()),
            })
            .collect::<Vec<_>>();
        lockbox::core_disbursements(self.spec.network_type, &outputs)?;
        self.spec.lockbox_disbursements = Box::leak(outputs.into_boxed_slice());
        Ok(self)
    }

    /// Sets the funding streams, with the meaning of `funding_streams` of Zakura's Regtest
    /// parameters with a height range and recipients in each entry: the streams of the
    /// first range that holds a height apply at that height, from height 1.
    ///
    /// The configuration is refused where Zakura stops at its start or at a block: a
    /// range that ends below its start, a receiver two times in one range, numerators
    /// above 100 in total, an address that is not a P2SH address, and fewer addresses
    /// than the range has address periods. An address of [`Receiver::Deferred`] is
    /// refused too.
    ///
    /// The tables of the streams stay in memory until the process ends, as the
    /// configuration does ([`RegtestConfig::network`]).
    pub fn with_funding_streams(
        mut self,
        streams: &[RegtestFundingStreams],
    ) -> Result<Self, ChainSpecError> {
        self.spec.funding_streams = funding::regtest_sets(streams);
        // The address periods depend on the activation heights: the check needs the
        // network of this configuration. A Regtest configuration has one address for each
        // period, as in Zakura.
        let network = self.spec.clone().network()?;
        hayai_consensus_core::funding::check_script_counts(network.core(), false)?;
        Ok(self)
    }

    /// Sets an NSM reissuance height for the tests of a short chain. The rules of a
    /// network give Regtest no reissuance height (`crate::nsm::reissuance_height`), so no
    /// short chain reaches the reissuance without this value. Zakura has the same value
    /// for its tests only (`ParametersBuilder::with_test_nsm_reissuance_height`,
    /// `zakura-chain/src/parameters/network/testnet.rs:1095-1101`, read in
    /// `network/subsidy.rs:708-713`): a node configuration cannot set it. The reissuance
    /// starts at this height or at the NU7 height, whichever is higher, and never on a
    /// network without an NU7 height.
    pub fn with_test_reissuance_height(mut self, height: u32) -> Self {
        self.spec.test_reissuance_height = Some(height);
        self
    }

    /// The network of this configuration. The configuration stays in memory until the
    /// process ends: a node calls this function one time at its start.
    ///
    /// The checks of [`ChainSpec::network`] hold: the constructors check the values that a
    /// configuration sets, and the other values are the values of [`Network::Regtest`].
    pub fn network(self) -> Network {
        let Ok(network) = self.spec.network() else {
            unreachable!("the constructors of a RegtestConfig make a spec that passes the checks");
        };
        network
    }
}

/// The consensus branch id of `upgrade` in the crypto backend. `None` when the backend does
/// not know the upgrade: NU7 on the upstream backend (`hayai_crypto::nu7_branch`).
///
/// ZIP 200: `CONSENSUS_BRANCH_ID` of each upgrade, 0 for Sprout. The value of each branch
/// id is the value of `Upgrade::branch_id` of the core (the test
/// `the_branch_ids_of_the_backend_are_the_branch_ids_of_the_core`).
pub fn branch_id(upgrade: Upgrade) -> Option<BranchId> {
    Some(match upgrade {
        Upgrade::Sprout => BranchId::Sprout,
        Upgrade::Overwinter => BranchId::Overwinter,
        Upgrade::Sapling => BranchId::Sapling,
        Upgrade::Blossom => BranchId::Blossom,
        Upgrade::Heartwood => BranchId::Heartwood,
        Upgrade::Canopy => BranchId::Canopy,
        Upgrade::Nu5 => BranchId::Nu5,
        Upgrade::Nu6 => BranchId::Nu6,
        Upgrade::Nu6_1 => BranchId::Nu6_1,
        Upgrade::Nu6_2 => BranchId::Nu6_2,
        Upgrade::Nu6_3 => BranchId::Nu6_3,
        Upgrade::Nu7 => return hayai_crypto::nu7_branch(),
    })
}

/// The values that depend only on the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NetworkParams {
    /// Hash of the genesis block.
    pub genesis_hash: BlockHash,
    /// `nTime` of the genesis block.
    pub genesis_time: u32,
    /// The Equihash parameters: the solution length of every header of the network.
    pub pow: PowParams,
    /// The proof-of-work limit: the easiest target, as a 256-bit little-endian integer.
    pub pow_limit: [u8; 32],
    /// The compact form of [`NetworkParams::pow_limit`] (zcashd `powLimit.GetCompact()`).
    pub pow_limit_bits: u32,
    /// The network waives the proof of work (Zakura's `disable_pow`, Regtest only): a
    /// header has no hash filter, no Equihash verification and no expected `nBits`.
    pub disable_pow: bool,
    /// First height at which a block whose time is more than the minimum-difficulty gap
    /// after its parent must have the proof-of-work limit as `nBits`
    /// ([`crate::DifficultyParams::min_difficulty_gap_spacings`]; zcashd
    /// `nPowAllowMinDifficultyBlocksAfterHeight` plus 1). `None`: the network has no such
    /// rule. ZIP 205: Testnet height 299,188.
    pub min_difficulty_start_height: Option<u32>,
    /// First height at which the time of a block is at most its median-time-past plus
    /// 90 min (protocol specification §7.6; Zakura `is_max_block_time_enforced`).
    /// Spec §7.6: height 2 on Mainnet, height 653,606 on Testnet.
    pub max_time_start_height: u32,
    /// First height of the soft fork that removes the Orchard pool for a time: from this
    /// height until the NU6.2 activation, a transaction has no Orchard bundle (Zakura
    /// `zakura-chain/src/parameters/network.rs:26,31`, the rule in
    /// `zakura-consensus/src/transaction.rs:484-493`). `None`: the network has no such
    /// soft fork (Zakura's Regtest, `testnet.rs:1430`).
    pub orchard_disabled_start_height: Option<u32>,
    /// A transaction that spends a coinbase output has no transparent output (zcashd
    /// `fCoinbaseMustBeShielded`). Regtest does not have the rule (Zakura
    /// `with_unshielded_coinbase_spends(true)`, `zakura-chain/src/parameters/network/
    /// testnet.rs:1426`, read in `zakura-chain/src/transaction.rs:557`). Coinbase maturity
    /// applies on every network.
    pub coinbase_must_be_shielded: bool,
    /// `SlowStartInterval`: the subsidy ramps up over this number of blocks.
    pub slow_start_interval: u32,
    /// Blocks between two halvings at the pre-Blossom target spacing.
    pub pre_blossom_halving_interval: u32,
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("a block hash constant is lower-case hex"),
    }
}

/// A block hash from its display form (byte-reversed hex).
const fn hash_from_display(hex: &str) -> BlockHash {
    let hex = hex.as_bytes();
    assert!(hex.len() == 64, "a block hash is 32 bytes");
    let mut bytes = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        bytes[31 - i] = hex_digit(hex[2 * i]) << 4 | hex_digit(hex[2 * i + 1]);
        i += 1;
    }
    BlockHash(bytes)
}

/// `2^bits - 1` as a 256-bit little-endian integer.
const fn ones(bits: usize) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let mut i = 0;
    while i < bits / 8 {
        bytes[i] = 0xff;
        i += 1;
    }
    let rest = bits - 8 * i;
    if rest > 0 {
        bytes[i] = (1 << rest) - 1;
    }
    bytes
}

/// The activation heights of the `(upgrade, height)` pairs of `list`, with Sprout at
/// height 0.
const fn heights(list: &[(Upgrade, u32)]) -> [Option<u32>; UPGRADES] {
    let mut heights = [None; UPGRADES];
    heights[Upgrade::Sprout.index()] = Some(0);
    let mut i = 0;
    while i < list.len() {
        heights[list[i].0.index()] = Some(list[i].1);
        i += 1;
    }
    heights
}

/// The last block before the Canopy activation: the mandatory checkpoint height of Zakura
/// (`mandatory_checkpoint_height`, `zakura-chain/src/parameters/network.rs:271`).
const fn before_canopy(heights: [Option<u32>; UPGRADES]) -> u32 {
    let Some(canopy) = heights[Upgrade::Canopy.index()] else {
        panic!("a built-in network has a Canopy activation height");
    };
    canopy - 1
}

/// The Mainnet activation heights. A Mainnet NU7 height needs a rule that this crate does
/// not have: the ZIP 2008 recipient of the last funding stream, a P2PKH address (Zakura
/// `subsidy/constants/mainnet.rs:196-221`). The test
/// `funding::tests::zip_2008_has_no_code_while_mainnet_has_no_nu7_height` fails when
/// Mainnet gets a height. Zakura has no Mainnet NU7 height (`constants.rs:83-112`, and
/// `zakura-protocol` `consensus.rs:503`).
///
/// ZIP 205, 206, 250, 251, 252, 253, 255, 257, 258: the `ACTIVATION_HEIGHT` of each
/// upgrade on Mainnet and Testnet (the test `the_deployment_constants_of_the_zips`). These
/// are the heights of `zcash_protocol` on each backend (the test
/// `activation_heights_match_zcash_protocol`).
const MAINNET_HEIGHTS: [Option<u32>; UPGRADES] = heights(&[
    (Upgrade::Overwinter, 347_500),
    (Upgrade::Sapling, 419_200),
    (Upgrade::Blossom, 653_600),
    (Upgrade::Heartwood, 903_000),
    (Upgrade::Canopy, 1_046_400),
    (Upgrade::Nu5, 1_687_104),
    (Upgrade::Nu6, 2_726_400),
    (Upgrade::Nu6_1, 3_146_400),
    (Upgrade::Nu6_2, 3_364_600),
    (Upgrade::Nu6_3, 3_428_143),
]);

/// The Testnet activation heights ([`MAINNET_HEIGHTS`] gives the sources).
///
/// The NU7 height is Zakura `testnet::NU7` (`zakura-chain/src/parameters/constants.rs:80`).
/// The value is here and not in the crypto backend: the upstream `zcash_protocol` has no
/// NU7 height, and `rules_at` must refuse a height with NU7 rules on a backend without the
/// NU7 branch id. ZIP 259: `ACTIVATION_HEIGHT (NU7)` on Testnet is 4,465,026, a multiple
/// of 3.
const TESTNET_HEIGHTS: [Option<u32>; UPGRADES] = heights(&[
    (Upgrade::Overwinter, 207_500),
    (Upgrade::Sapling, 280_000),
    (Upgrade::Blossom, 584_000),
    (Upgrade::Heartwood, 903_800),
    (Upgrade::Canopy, 1_028_500),
    (Upgrade::Nu5, 1_842_420),
    (Upgrade::Nu6, 2_976_000),
    (Upgrade::Nu6_1, 3_536_500),
    (Upgrade::Nu6_2, 4_052_000),
    (Upgrade::Nu6_3, 4_134_000),
    (Upgrade::Nu7, 4_465_026),
]);

/// The Regtest activation heights: Overwinter to NU5 at height 1.
const REGTEST_HEIGHTS: [Option<u32>; UPGRADES] = heights(&[
    (Upgrade::Overwinter, 1),
    (Upgrade::Sapling, 1),
    (Upgrade::Blossom, 1),
    (Upgrade::Heartwood, 1),
    (Upgrade::Canopy, 1),
    (Upgrade::Nu5, 1),
]);

const MAINNET_PARAMS: NetworkParams = NetworkParams {
    // `zcash-cli getblockhash 0`.
    genesis_hash: hash_from_display(
        "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08",
    ),
    genesis_time: 1_477_641_360,
    // Spec §7.7.1: Equihash n = 200, k = 9 on Mainnet and Testnet.
    pow: PowParams::MAINNET,
    // zcashd `chainparams.cpp`: `0x0007ffff…ff`. Spec §5.3: `PoWLimit` of Mainnet.
    pow_limit: ones(243),
    pow_limit_bits: 0x1f07_ffff,
    disable_pow: false,
    min_difficulty_start_height: None,
    max_time_start_height: 2,
    // Zakura `MAINNET_TEMPORARY_ORCHARD_DISABLING_SOFT_FORK_HEIGHT`.
    orchard_disabled_start_height: Some(3_363_426),
    coinbase_must_be_shielded: true,
    slow_start_interval: 20_000,
    pre_blossom_halving_interval: 840_000,
};

static MAINNET: ChainSpec = ChainSpec {
    name: "mainnet",
    network_type: NetworkType::Main,
    params: MAINNET_PARAMS,
    activation_heights: MAINNET_HEIGHTS,
    checkpoints: Checkpoints::embedded(&checkpoints::MAINNET),
    mandatory_checkpoint_height: before_canopy(MAINNET_HEIGHTS),
    funding_streams: &funding::MAINNET,
    lockbox_disbursements: &lockbox::MAINNET,
    founders_addresses: &founders::MAINNET_ADDRESSES,
    nsm_seed: Some(nsm::MAINNET_SEED),
    test_reissuance_height: None,
    core_tables: CoreTables {
        funding: &funding::MAINNET_CORE,
        lockbox: &lockbox::MAINNET_CORE,
        founders: &founders::MAINNET_SCRIPTS,
        first_halving: Some(1_046_400),
    },
};

const TESTNET_PARAMS: NetworkParams = NetworkParams {
    // `zcash-cli -testnet getblockhash 0`.
    genesis_hash: hash_from_display(
        "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38",
    ),
    genesis_time: 1_477_648_033,
    pow: PowParams::TESTNET,
    // zcashd `chainparams.cpp`: `0x07ffff…ff`. Spec §5.3: `PoWLimit` of Testnet.
    pow_limit: ones(251),
    pow_limit_bits: 0x2007_ffff,
    disable_pow: false,
    // Zakura `TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT`. ZIP 205: minimum-difficulty
    // blocks from Testnet height 299,188.
    min_difficulty_start_height: Some(299_188),
    // Zakura `TESTNET_MAX_TIME_START_HEIGHT`. Spec §7.6: the 90 min rule from Testnet
    // height 653,606.
    max_time_start_height: 653_606,
    // Zakura `TESTNET_TEMPORARY_ORCHARD_DISABLING_SOFT_FORK_HEIGHT`.
    orchard_disabled_start_height: Some(4_048_500),
    coinbase_must_be_shielded: true,
    slow_start_interval: 20_000,
    pre_blossom_halving_interval: 840_000,
};

static TESTNET: ChainSpec = ChainSpec {
    name: "testnet",
    network_type: NetworkType::Test,
    params: TESTNET_PARAMS,
    activation_heights: TESTNET_HEIGHTS,
    checkpoints: Checkpoints::embedded(&checkpoints::TESTNET),
    mandatory_checkpoint_height: before_canopy(TESTNET_HEIGHTS),
    funding_streams: &funding::TESTNET,
    lockbox_disbursements: &lockbox::TESTNET,
    founders_addresses: &founders::TESTNET_ADDRESSES,
    nsm_seed: Some(nsm::TESTNET_SEED),
    test_reissuance_height: None,
    core_tables: CoreTables {
        funding: &funding::TESTNET_CORE,
        lockbox: &lockbox::TESTNET_CORE,
        founders: &founders::TESTNET_SCRIPTS,
        first_halving: Some(1_116_000),
    },
};

/// `zcash-cli -regtest getblockhash 0`.
const REGTEST_GENESIS: BlockHash =
    hash_from_display("029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327");

const REGTEST_PARAMS: NetworkParams = NetworkParams {
    genesis_hash: REGTEST_GENESIS,
    genesis_time: 1_296_688_602,
    pow: PowParams::REGTEST,
    // zcashd `chainparams.cpp`: `0x0f0f…0f`.
    pow_limit: [0x0f; 32],
    pow_limit_bits: 0x200f_0f0f,
    disable_pow: true,
    min_difficulty_start_height: None,
    // Zakura's default Regtest (`max_block_time_start_height`).
    max_time_start_height: 2,
    orchard_disabled_start_height: None,
    coinbase_must_be_shielded: false,
    slow_start_interval: 0,
    // zcashd `PRE_BLOSSOM_REGTEST_HALVING_INTERVAL` as Zakura uses it.
    pre_blossom_halving_interval: 144,
};

static REGTEST: ChainSpec = ChainSpec {
    name: "regtest",
    network_type: NetworkType::Regtest,
    params: REGTEST_PARAMS,
    activation_heights: REGTEST_HEIGHTS,
    // Regtest has one checkpoint: the genesis block.
    checkpoints: Checkpoints::embedded(&[(0, REGTEST_GENESIS)]),
    mandatory_checkpoint_height: before_canopy(REGTEST_HEIGHTS),
    funding_streams: &[],
    lockbox_disbursements: &[],
    founders_addresses: &[],
    nsm_seed: None,
    test_reissuance_height: None,
    core_tables: CoreTables {
        funding: &[],
        lockbox: &[],
        founders: &[],
        first_halving: Some(287),
    },
};

impl Network {
    pub const ALL: [Network; 3] = [Network::Mainnet, Network::Testnet, Network::Regtest];

    /// The values of the network.
    pub const fn spec(self) -> &'static ChainSpec {
        match self {
            Network::Mainnet => &MAINNET,
            Network::Testnet => &TESTNET,
            Network::Regtest => &REGTEST,
            Network::Custom(CheckedSpec(custom)) => &custom.spec,
        }
    }

    /// The values that the rules of the core read ([`ChainSpec::core`]).
    ///
    /// A built-in network builds its core on the first call from its tables; every later
    /// call is one load. A custom network has its core from [`ChainSpec::network`].
    pub fn core(self) -> &'static CoreSpec {
        match self {
            Network::Mainnet => BUILT_IN_CORES[0].get_or_init(|| MAINNET.build_core()),
            Network::Testnet => BUILT_IN_CORES[1].get_or_init(|| TESTNET.build_core()),
            Network::Regtest => BUILT_IN_CORES[2].get_or_init(|| REGTEST.build_core()),
            Network::Custom(CheckedSpec(custom)) => &custom.core,
        }
    }

    pub const fn name(self) -> &'static str {
        self.spec().name
    }

    /// The network type of `zcash_protocol` ([`ChainSpec::network_type`]).
    pub const fn network_type(self) -> NetworkType {
        self.spec().network_type
    }

    /// Whether the network is Regtest, with or without a [`RegtestConfig`]: its network
    /// type is `Regtest`.
    pub const fn is_regtest(self) -> bool {
        matches!(self.network_type(), NetworkType::Regtest)
    }

    /// The values that depend only on the network ([`ChainSpec::params`]).
    pub const fn params(self) -> &'static NetworkParams {
        &self.spec().params
    }

    /// The height at which `upgrade` activates. `None` when the network has no height for
    /// it.
    pub fn activation_height(self, upgrade: Upgrade) -> Option<u32> {
        self.spec().activation_heights[upgrade.index()]
    }

    /// The upgrade whose rules apply at `height`: the last upgrade in activation order with
    /// an activation height at or below `height`.
    pub fn upgrade_at(self, height: u32) -> Upgrade {
        checked(self.core().upgrade_at(height))
    }

    /// Whether the Orchard pool is off at `height`: the height is at or after the start of
    /// the soft fork and before the NU6.2 activation, which starts the pool again (Zakura
    /// `is_orchard_temporarily_disabled`, `zakura-chain/src/parameters/network.rs:373-378`).
    /// On a network without an NU6.2 height the pool stays off from the start height.
    pub fn orchard_disabled(self, height: u32) -> bool {
        self.core().orchard_disabled(height)
    }

    /// The upgrade whose rules apply at the first activation height above `height`. `None`
    /// when no upgrade with a height activates after `height`.
    pub fn next_upgrade(self, height: u32) -> Option<Upgrade> {
        checked(self.core().next_upgrade(height))
    }
}

#[cfg(test)]
mod tests {
    use hayai_crypto::zcash_protocol::consensus::{
        BlockHeight, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
    };
    use hayai_wire::header::expand_target;

    use super::*;
    use crate::MAX_MONEY;

    /// The heights of `zcash_protocol` 0.10.5 and `zakura-protocol` 2.2.0, which agree up
    /// to NU6.3.
    const MAINNET_HEIGHTS: [(Upgrade, u32); 10] = [
        (Upgrade::Overwinter, 347_500),
        (Upgrade::Sapling, 419_200),
        (Upgrade::Blossom, 653_600),
        (Upgrade::Heartwood, 903_000),
        (Upgrade::Canopy, 1_046_400),
        (Upgrade::Nu5, 1_687_104),
        (Upgrade::Nu6, 2_726_400),
        (Upgrade::Nu6_1, 3_146_400),
        (Upgrade::Nu6_2, 3_364_600),
        (Upgrade::Nu6_3, 3_428_143),
    ];

    /// The `NetworkUpgrade` of each upgrade that `zcash_protocol` knows on both backends.
    fn protocol_upgrades() -> [(Upgrade, NetworkUpgrade); 10] {
        [
            (Upgrade::Overwinter, NetworkUpgrade::Overwinter),
            (Upgrade::Sapling, NetworkUpgrade::Sapling),
            (Upgrade::Blossom, NetworkUpgrade::Blossom),
            (Upgrade::Heartwood, NetworkUpgrade::Heartwood),
            (Upgrade::Canopy, NetworkUpgrade::Canopy),
            (Upgrade::Nu5, NetworkUpgrade::Nu5),
            (Upgrade::Nu6, NetworkUpgrade::Nu6),
            (Upgrade::Nu6_1, NetworkUpgrade::Nu6_1),
            (Upgrade::Nu6_2, NetworkUpgrade::Nu6_2),
            (Upgrade::Nu6_3, NetworkUpgrade::Nu6_3),
        ]
    }

    #[test]
    fn activation_heights_match_zcash_protocol() {
        for (upgrade, protocol) in protocol_upgrades() {
            assert_eq!(
                Network::Mainnet.activation_height(upgrade),
                MAIN_NETWORK.activation_height(protocol).map(u32::from),
                "{upgrade:?}"
            );
            assert_eq!(
                Network::Testnet.activation_height(upgrade),
                TEST_NETWORK.activation_height(protocol).map(u32::from),
                "{upgrade:?}"
            );
            let Some(_) = Network::Testnet.activation_height(upgrade) else {
                panic!("{upgrade:?} is active on Testnet");
            };
        }
        for (upgrade, height) in MAINNET_HEIGHTS {
            assert_eq!(Network::Mainnet.activation_height(upgrade), Some(height));
        }
        assert_eq!(
            Network::Testnet.activation_height(Upgrade::Nu6_3),
            Some(4_134_000)
        );
        for network in Network::ALL {
            assert_eq!(network.activation_height(Upgrade::Sprout), Some(0));
        }
        // The NU7 heights are the same on every backend. A backend that knows an NU7
        // height has the same value.
        assert_eq!(Network::Mainnet.activation_height(Upgrade::Nu7), None);
        assert_eq!(
            Network::Testnet.activation_height(Upgrade::Nu7),
            Some(4_465_026)
        );
        assert_eq!(Network::Regtest.activation_height(Upgrade::Nu7), None);
        assert_eq!(hayai_crypto::nu7_activation(NetworkType::Main), None);
        if let Some(height) = hayai_crypto::nu7_activation(NetworkType::Test) {
            assert_eq!(height, 4_465_026);
        }
    }

    /// The `CONSENSUS_BRANCH_ID` and the `ACTIVATION_HEIGHT` of each deployment ZIP: 201
    /// (Overwinter), 205, 206, 250, 251, 252, 253, 255, 257, 258 and 259.
    /// The Testnet NU5 height is the second activation of ZIP 252. ZIP 259 requires an NU7
    /// height that is a multiple of 3.
    #[test]
    fn the_deployment_constants_of_the_zips() {
        let zips: [(Upgrade, u32, u32, u32); 10] = [
            (Upgrade::Overwinter, 0x5ba8_1b19, 207_500, 347_500),
            (Upgrade::Sapling, 0x76b8_09bb, 280_000, 419_200),
            (Upgrade::Blossom, 0x2bb4_0e60, 584_000, 653_600),
            (Upgrade::Heartwood, 0xf5b9_230b, 903_800, 903_000),
            (Upgrade::Canopy, 0xe9ff_75a6, 1_028_500, 1_046_400),
            (Upgrade::Nu5, 0xc2d6_d0b4, 1_842_420, 1_687_104),
            (Upgrade::Nu6, 0xc8e7_1055, 2_976_000, 2_726_400),
            (Upgrade::Nu6_1, 0x4dec_4df0, 3_536_500, 3_146_400),
            (Upgrade::Nu6_2, 0x5437_f330, 4_052_000, 3_364_600),
            (Upgrade::Nu6_3, 0x37a5_165b, 4_134_000, 3_428_143),
        ];
        for (upgrade, branch, testnet, mainnet) in zips {
            assert_eq!(
                branch_id(upgrade).map(u32::from),
                Some(branch),
                "{upgrade:?}"
            );
            assert_eq!(
                Network::Testnet.activation_height(upgrade),
                Some(testnet),
                "{upgrade:?}"
            );
            assert_eq!(
                Network::Mainnet.activation_height(upgrade),
                Some(mainnet),
                "{upgrade:?}"
            );
        }
        assert_eq!(branch_id(Upgrade::Sprout).map(u32::from), Some(0));
        if let Some(nu7) = branch_id(Upgrade::Nu7) {
            assert_eq!(u32::from(nu7), 0x7719_0ad9);
        }
        let Some(nu7) = Network::Testnet.activation_height(Upgrade::Nu7) else {
            panic!("ZIP 259 gives Testnet an NU7 height");
        };
        assert_eq!((nu7, nu7 % 3), (4_465_026, 0));
        assert_eq!(Network::Mainnet.activation_height(Upgrade::Nu7), None);
    }

    /// The branch id of the backend for each upgrade is the branch id constant of the core.
    #[test]
    fn the_branch_ids_of_the_backend_are_the_branch_ids_of_the_core() {
        for upgrade in Upgrade::ALL {
            match branch_id(upgrade) {
                Some(branch) => {
                    assert_eq!(u32::from(branch), upgrade.branch_id(), "{upgrade:?}");
                    assert_eq!(Upgrade::of_branch(u32::from(branch)), Ok(upgrade));
                }
                None => assert_eq!(upgrade, Upgrade::Nu7),
            }
        }
        assert_eq!(branch_id(Upgrade::Nu7), hayai_crypto::nu7_branch());
    }

    #[test]
    fn the_upgrade_changes_at_every_activation_height() {
        for (network, params) in [
            (Network::Mainnet, &MAIN_NETWORK as &dyn BranchAt),
            (Network::Testnet, &TEST_NETWORK as &dyn BranchAt),
        ] {
            assert_eq!(network.upgrade_at(0), Upgrade::Sprout);
            for (upgrade, _) in protocol_upgrades() {
                let Some(height) = network.activation_height(upgrade) else {
                    panic!("{upgrade:?} has a height on {network:?}");
                };
                assert_eq!(network.upgrade_at(height), upgrade);
                assert!(network.upgrade_at(height - 1) < upgrade);
                // The branch id is the one `zcash_protocol` selects for the height.
                for h in [height - 1, height] {
                    assert_eq!(
                        branch_id(network.upgrade_at(h)),
                        Some(params.branch_at(h)),
                        "{network:?} {h}"
                    );
                }
            }
        }
    }

    /// `BranchId::for_height` of a `zcash_protocol` parameter set.
    trait BranchAt {
        fn branch_at(&self, height: u32) -> BranchId;
    }

    impl<P: Parameters> BranchAt for P {
        fn branch_at(&self, height: u32) -> BranchId {
            BranchId::for_height(self, BlockHeight::from_u32(height))
        }
    }

    #[test]
    fn a_configured_regtest_has_its_heights_and_its_checkpoints() {
        let hash = BlockHash([7; 32]);
        let config = RegtestConfig::new(
            &[(Upgrade::Nu6_2, 40), (Upgrade::Nu6, 20)],
            vec![(30, hash)],
            25,
        )
        .expect("a valid configuration");
        let network = config.network();
        assert!(network.is_regtest());
        assert_eq!(network.params(), Network::Regtest.params());
        assert_eq!(network.upgrade_at(1), Upgrade::Nu5);
        assert_eq!(network.upgrade_at(19), Upgrade::Nu5);
        assert_eq!(network.upgrade_at(20), Upgrade::Nu6);
        assert_eq!(network.upgrade_at(39), Upgrade::Nu6);
        assert_eq!(network.upgrade_at(40), Upgrade::Nu6_2);
        assert_eq!(network.activation_height(Upgrade::Nu6_1), None);
        assert_eq!(network.next_upgrade(1), Some(Upgrade::Nu6));
        assert_eq!(network.next_upgrade(20), Some(Upgrade::Nu6_2));
        assert_eq!(network.next_upgrade(40), None);
        assert_eq!(network.mandatory_checkpoint_height(), 25);
        let checkpoints: Vec<_> = network.checkpoints().iter().collect();
        assert_eq!(
            checkpoints,
            [(0, Network::Regtest.params().genesis_hash), (30, hash)]
        );
        // The core of the configured network has the heights and the values of Regtest.
        assert_eq!(
            network.core().activation_heights,
            network.spec().activation_heights
        );
        assert_eq!(network.core().first_halving, Some(287));
        assert_eq!(
            CoreSpec {
                activation_heights: Network::Regtest.core().activation_heights,
                ..network.core().clone()
            },
            *Network::Regtest.core()
        );

        let refused = |activations: &[(Upgrade, u32)], checkpoints, mandatory| {
            RegtestConfig::new(activations, checkpoints, mandatory).expect_err("refused")
        };
        assert_eq!(
            refused(&[(Upgrade::Nu5, 5)], Vec::new(), 0),
            ChainSpecError::FixedUpgrade(Upgrade::Nu5)
        );
        assert_eq!(
            refused(&[(Upgrade::Nu6, 1)], Vec::new(), 0),
            ChainSpecError::Order {
                upgrade: Upgrade::Nu6,
                height: 1
            }
        );
        assert_eq!(
            refused(&[(Upgrade::Nu6, 9), (Upgrade::Nu6_1, 8)], Vec::new(), 0),
            ChainSpecError::Order {
                upgrade: Upgrade::Nu6_1,
                height: 8
            }
        );
        assert_eq!(refused(&[], vec![(0, hash)], 0), ChainSpecError::Genesis);
        assert_eq!(
            refused(&[], vec![(4, hash)], 5),
            ChainSpecError::Coverage(5)
        );
        assert_eq!(
            refused(&[], vec![(4, hash), (4, hash)], 0),
            ChainSpecError::Checkpoints(DuplicateCheckpoint(4))
        );
    }

    #[test]
    fn regtest_values_are_the_values_of_the_pair_configuration() {
        let regtest = Network::Regtest;
        assert_eq!(regtest.upgrade_at(0), Upgrade::Sprout);
        assert_eq!(regtest.upgrade_at(1), Upgrade::Nu5);
        assert_eq!(regtest.upgrade_at(10_000), Upgrade::Nu5);
        assert_eq!(regtest.upgrade_at(u32::MAX), Upgrade::Nu5);
        assert_eq!(regtest.next_upgrade(0), Some(Upgrade::Nu5));
        assert_eq!(regtest.next_upgrade(1), None);
        assert_eq!(regtest.next_upgrade(5), None);
        let params = regtest.params();
        assert_eq!(
            params.genesis_hash.to_string(),
            "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327"
        );
        assert_eq!(params.genesis_time, 1_296_688_602);
        assert_eq!(params.pow, PowParams::REGTEST);
        assert_eq!(params.pow_limit_bits, 0x200f_0f0f);
        assert_eq!(params.pre_blossom_halving_interval, 144);
        assert_eq!(regtest.core().post_blossom_halving_interval(), Ok(288));
        assert_eq!(params.slow_start_interval, 0);
    }

    /// The first halving of each built-in spec is the height that `CoreSpec::checked`
    /// derives, and each built-in core passes the checks with its constant values.
    #[test]
    fn the_core_of_each_built_in_spec_is_derived_and_checked() {
        for network in Network::ALL {
            let core = network.core();
            assert_eq!(
                core.first_halving,
                hayai_consensus_core::subsidy_schedule::halving_height(core, 1, u32::MAX).unwrap(),
                "{network:?}"
            );
            assert_eq!(core.clone().checked(), Ok(core.clone()), "{network:?}");
            let params = network.params();
            assert_eq!(core.pow_limit, params.pow_limit);
            assert_eq!(core.pow_limit_bits, params.pow_limit_bits);
            assert_eq!(core.disable_pow, params.disable_pow);
            assert_eq!(core.slow_start_interval, params.slow_start_interval);
            assert_eq!(
                core.pre_blossom_halving_interval,
                params.pre_blossom_halving_interval
            );
            assert_eq!(core.max_time_start_height, params.max_time_start_height);
            assert_eq!(
                core.min_difficulty_start_height,
                params.min_difficulty_start_height
            );
            assert_eq!(
                core.orchard_disabled_start_height,
                params.orchard_disabled_start_height
            );
            assert_eq!(
                core.coinbase_must_be_shielded,
                params.coinbase_must_be_shielded
            );
            assert_eq!(core.activation_heights, network.spec().activation_heights);
            assert_eq!(core.nsm_seed, network.spec().nsm_seed);
            assert_eq!(core.test_reissuance_height, None);
        }
    }

    #[test]
    fn mainnet_and_testnet_values() {
        let mainnet = Network::Mainnet.params();
        assert_eq!(
            mainnet.genesis_hash.to_string(),
            "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"
        );
        assert_eq!(mainnet.genesis_time, 1_477_641_360);
        assert_eq!(mainnet.pow, PowParams::MAINNET);
        assert_eq!((mainnet.pow.n, mainnet.pow.k), (200, 9));
        assert_eq!(mainnet.pow_limit_bits, 0x1f07_ffff);
        assert_eq!(
            Network::Mainnet.core().post_blossom_halving_interval(),
            Ok(1_680_000)
        );
        let testnet = Network::Testnet.params();
        assert_eq!(
            testnet.genesis_hash.to_string(),
            "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38"
        );
        assert_eq!(testnet.pow, PowParams::TESTNET);
        assert_eq!(testnet.pow_limit_bits, 0x2007_ffff);
        for (network, network_type) in [
            (Network::Mainnet, NetworkType::Main),
            (Network::Testnet, NetworkType::Test),
            (Network::Regtest, NetworkType::Regtest),
        ] {
            assert_eq!(network.network_type(), network_type);
        }
    }

    /// The compact form keeps the three most significant bytes of the limit, so the target
    /// that the bits encode is the limit with every lower byte cleared.
    #[test]
    fn the_compact_limit_is_the_limit_rounded_to_three_bytes() {
        for network in Network::ALL {
            let params = network.params();
            let Some(compact) = expand_target(params.pow_limit_bits) else {
                panic!("{network:?}: the compact limit encodes a target");
            };
            let Some(top) = params.pow_limit.iter().rposition(|b| *b != 0) else {
                panic!("{network:?}: the limit is not zero");
            };
            let mut rounded = [0u8; 32];
            rounded[top - 2..=top].copy_from_slice(&params.pow_limit[top - 2..=top]);
            assert_eq!(compact, rounded, "{network:?}");
        }
        assert_eq!(Network::Mainnet.params().pow_limit[30], 0x07);
        assert_eq!(Network::Mainnet.params().pow_limit[31], 0x00);
        assert_eq!(Network::Testnet.params().pow_limit[31], 0x07);
    }

    /// The Orchard soft fork: the pool is off from the start height until the block before
    /// the NU6.2 activation, and the whole range is in NU6.1.
    #[test]
    fn the_orchard_pool_is_off_from_the_soft_fork_until_nu6_2() {
        for (network, start, nu6_2) in [
            (Network::Mainnet, 3_363_426, 3_364_600),
            (Network::Testnet, 4_048_500, 4_052_000),
        ] {
            assert_eq!(network.params().orchard_disabled_start_height, Some(start));
            assert_eq!(network.activation_height(Upgrade::Nu6_2), Some(nu6_2));
            for (height, disabled) in [
                (0, false),
                (start - 1, false),
                (start, true),
                (start + 1, true),
                (nu6_2 - 1, true),
                (nu6_2, false),
                (u32::MAX, false),
            ] {
                assert_eq!(
                    network.orchard_disabled(height),
                    disabled,
                    "{network:?} {height}"
                );
            }
            assert_eq!(network.upgrade_at(start), Upgrade::Nu6_1);
            assert_eq!(network.upgrade_at(nu6_2 - 1), Upgrade::Nu6_1);
        }
        for height in [0, 1, 3_363_426, 4_048_500, u32::MAX] {
            assert!(!Network::Regtest.orchard_disabled(height));
        }
    }

    #[test]
    fn next_upgrade_is_the_upgrade_of_the_following_activation() {
        let mainnet = Network::Mainnet;
        assert_eq!(mainnet.next_upgrade(2_726_399), Some(Upgrade::Nu6));
        assert_eq!(mainnet.next_upgrade(2_726_400), Some(Upgrade::Nu6_1));
        // After the last upgrade with a height there is no next one.
        let last = Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| mainnet.activation_height(upgrade))
            .max();
        assert_eq!(last, Some(3_428_143));
        assert_eq!(mainnet.next_upgrade(3_428_143), None);
    }

    #[test]
    fn the_network_name_is_its_configuration_value() {
        for network in Network::ALL {
            let json = serde_json::to_string(&network).expect("serializes");
            assert_eq!(json, format!("\"{}\"", network.name()));
            let back: Network = serde_json::from_str(&json).expect("parses");
            assert_eq!(back, network);
        }
    }

    /// The checks of `ParametersBuilder` and of `new_regtest` of Zakura on the lockbox
    /// disbursements and the funding streams, and the address count that Zakura checks at
    /// a block.
    #[test]
    fn a_configured_regtest_checks_its_disbursements_and_its_funding_streams() {
        const REGTEST: &str = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";
        const MAINNET: &str = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd";
        const P2PKH: &str = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV";
        let config = || RegtestConfig::new(&[], Vec::new(), 0).expect("valid");
        let disbursements = |entries: &[(&str, u64)]| {
            let entries = entries
                .iter()
                .map(|(address, amount)| RegtestDisbursement {
                    address: address.to_string(),
                    amount: *amount,
                })
                .collect();
            config().with_lockbox_disbursements(entries)
        };
        let Ok(_) = disbursements(&[(REGTEST, MAX_MONEY - 1), (MAINNET, 1)]) else {
            panic!("an address of each network and the largest amount are valid");
        };
        assert_eq!(
            disbursements(&[(REGTEST, MAX_MONEY), (MAINNET, 1)]),
            Err(ChainSpecError::DisbursementAmount)
        );
        for address in [P2PKH, "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUni", ""] {
            let Err(ChainSpecError::Address { address: found, .. }) =
                disbursements(&[(address, 1)])
            else {
                panic!("{address} is refused");
            };
            assert_eq!(found, address);
        }

        let recipient = |receiver, numerator, addresses: &[&str]| RegtestRecipient {
            receiver,
            numerator,
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
        };
        let streams = |start, end, recipients: Vec<RegtestRecipient>| {
            config().with_funding_streams(&[RegtestFundingStreams {
                height_range: start..end,
                recipients,
            }])
        };
        let (deferred, grants) = (Receiver::Deferred, Receiver::MajorGrants);
        // The heights 10, 11 to 16 and 17 to 21 are 3 address periods. An empty range
        // needs no address.
        for (start, end, addresses) in [
            (10, 22, vec![REGTEST, MAINNET, REGTEST]),
            (11, 17, vec![REGTEST]),
            (10, 10, vec![]),
        ] {
            let recipients = vec![
                recipient(deferred, 92, &[]),
                recipient(grants, 8, &addresses),
            ];
            let Ok(_) = streams(start, end, recipients) else {
                panic!("the range from {start} to {end} is valid");
            };
        }
        assert_eq!(
            streams(10, 22, vec![recipient(grants, 8, &[REGTEST, MAINNET])]),
            Err(ChainSpecError::StreamAddresses {
                receiver: grants,
                start: 10,
                end: 22,
                required: 3,
                found: 2,
            })
        );
        assert_eq!(
            streams(11, 18, vec![recipient(grants, 8, &[REGTEST])]),
            Err(ChainSpecError::StreamAddresses {
                receiver: grants,
                start: 11,
                end: 18,
                required: 2,
                found: 1,
            })
        );
        assert_eq!(
            streams(10, 9, Vec::new()),
            Err(ChainSpecError::StreamRange { start: 10, end: 9 })
        );
        assert_eq!(
            streams(
                11,
                17,
                vec![
                    recipient(grants, 1, &[REGTEST]),
                    recipient(grants, 1, &[REGTEST])
                ]
            ),
            Err(ChainSpecError::StreamReceiver(grants))
        );
        assert_eq!(
            streams(
                11,
                17,
                vec![
                    recipient(deferred, 93, &[]),
                    recipient(grants, 8, &[REGTEST])
                ]
            ),
            Err(ChainSpecError::StreamNumerators(101))
        );
        assert_eq!(
            streams(11, 17, vec![recipient(deferred, 1, &[REGTEST])]),
            Err(ChainSpecError::DeferredAddress)
        );
        let Err(ChainSpecError::Address { .. }) =
            streams(11, 17, vec![recipient(grants, 1, &[P2PKH])])
        else {
            panic!("a P2PKH address is refused");
        };
    }
}
