//! The networks, their parameters and the activation heights of the network upgrades.
//!
//! Regtest follows Zakura's Regtest (`zakura-chain/src/parameters/network/testnet.rs`,
//! `Parameters::new_regtest`): the zcashd Regtest genesis block, Overwinter to Canopy at
//! height 1 (Zakura's default), NU5 at height 1 (Zakura's `[network.
//! testnet_parameters.activation_heights] NU5 = 1`, which a Zakura node in a pair must set),
//! no slow start, a pre-Blossom halving interval of 144 blocks, the proof-of-work limit
//! `0x0f0f…0f` and Equihash (48, 5). Mainnet and Testnet use the activation heights of the
//! crypto backend's `zcash_protocol` and Equihash (200, 9).
//!
//! A Regtest network can have its own activation heights for the upgrades after NU5, its
//! own checkpoint list and its own mandatory checkpoint height ([`RegtestConfig`],
//! [`Network::ConfiguredRegtest`]). Every other value is the value of [`Network::Regtest`].

use hayai_crypto::zcash_protocol::consensus::{
    BranchId, NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
};
use hayai_wire::header::{BlockHash, PowParams};
use serde::{Deserialize, Serialize};

use crate::{Checkpoints, ConsensusError, DuplicateCheckpoint};

/// The networks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
    /// Regtest with the values of a [`RegtestConfig`]. The name in a configuration file is
    /// `regtest`: the node makes this value from its configuration
    /// ([`RegtestConfig::network`]).
    #[serde(skip)]
    ConfiguredRegtest(&'static RegtestConfig),
}

/// The upgrades after NU5, whose Regtest activation height a [`RegtestConfig`] can set.
const CONFIGURABLE: [Upgrade; 5] = [
    Upgrade::Nu6,
    Upgrade::Nu6_1,
    Upgrade::Nu6_2,
    Upgrade::Nu6_3,
    Upgrade::Nu7,
];

/// The values of a Regtest network that a node operator or a test can set.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RegtestConfig {
    /// The activation height of each upgrade of [`CONFIGURABLE`], in that order.
    activation_heights: [Option<u32>; 5],
    checkpoints: Checkpoints,
    mandatory_checkpoint_height: u32,
    /// [`RegtestConfig::with_test_reissuance_height`].
    test_reissuance_height: Option<u32>,
}

/// Why a [`RegtestConfig`] is not valid.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum RegtestConfigError {
    #[error("the Regtest activation height of {0:?} is not configurable")]
    FixedUpgrade(Upgrade),
    #[error("the activation height {height} of {upgrade:?} is not above 1 and at or above the height of each earlier upgrade")]
    Order { upgrade: Upgrade, height: u32 },
    #[error(transparent)]
    Checkpoints(#[from] DuplicateCheckpoint),
    #[error("the checkpoint at height 0 is not the genesis block")]
    Genesis,
    #[error("the last checkpoint is below the mandatory checkpoint height {0}")]
    Coverage(u32),
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
    ) -> Result<Self, RegtestConfigError> {
        let mut activation_heights = [None; 5];
        for (upgrade, height) in activations {
            let Some(slot) = CONFIGURABLE.iter().position(|u| u == upgrade) else {
                return Err(RegtestConfigError::FixedUpgrade(*upgrade));
            };
            activation_heights[slot] = Some(*height);
        }
        let mut floor = 2;
        for (upgrade, height) in CONFIGURABLE.into_iter().zip(activation_heights) {
            let Some(height) = height else { continue };
            if height < floor {
                return Err(RegtestConfigError::Order { upgrade, height });
            }
            floor = height;
        }
        let genesis = REGTEST.genesis_hash;
        match checkpoints.iter().find(|(height, _)| *height == 0) {
            Some((_, hash)) if *hash != genesis => return Err(RegtestConfigError::Genesis),
            Some(_) => {}
            None => checkpoints.push((0, genesis)),
        }
        let checkpoints = Checkpoints::new(checkpoints)?;
        if !matches!(checkpoints.last_height(), Some(last) if last >= mandatory_checkpoint_height) {
            return Err(RegtestConfigError::Coverage(mandatory_checkpoint_height));
        }
        Ok(Self {
            activation_heights,
            checkpoints,
            mandatory_checkpoint_height,
            test_reissuance_height: None,
        })
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
        self.test_reissuance_height = Some(height);
        self
    }

    pub(crate) fn test_reissuance_height(&self) -> Option<u32> {
        self.test_reissuance_height
    }

    /// The network of this configuration. The configuration stays in memory until the
    /// process ends: a node calls this function one time at its start.
    pub fn network(self) -> Network {
        Network::ConfiguredRegtest(Box::leak(Box::new(self)))
    }

    pub(crate) fn checkpoints(&self) -> &Checkpoints {
        &self.checkpoints
    }

    pub(crate) fn mandatory_checkpoint_height(&self) -> u32 {
        self.mandatory_checkpoint_height
    }
}

/// The network upgrades in activation order. `Sprout` is the rule set of the genesis block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Upgrade {
    Sprout,
    Overwinter,
    Sapling,
    Blossom,
    Heartwood,
    Canopy,
    Nu5,
    Nu6,
    Nu6_1,
    Nu6_2,
    Nu6_3,
    Nu7,
}

impl Upgrade {
    /// Every upgrade, in activation order.
    pub const ALL: [Upgrade; 12] = [
        Upgrade::Sprout,
        Upgrade::Overwinter,
        Upgrade::Sapling,
        Upgrade::Blossom,
        Upgrade::Heartwood,
        Upgrade::Canopy,
        Upgrade::Nu5,
        Upgrade::Nu6,
        Upgrade::Nu6_1,
        Upgrade::Nu6_2,
        Upgrade::Nu6_3,
        Upgrade::Nu7,
    ];

    /// The consensus branch id of the upgrade. `None` when the crypto backend does not know
    /// the upgrade: NU7 on the upstream backend.
    pub fn branch_id(self) -> Option<BranchId> {
        Some(match self {
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

    /// The upgrade of a consensus branch id.
    pub fn of_branch(branch: BranchId) -> Result<Upgrade, ConsensusError> {
        Upgrade::ALL
            .into_iter()
            .find(|upgrade| upgrade.branch_id() == Some(branch))
            .ok_or(ConsensusError::UnknownBranch(u32::from(branch)))
    }
}

/// The values that depend only on the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkParams {
    pub network: Network,
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
    /// First height at which a block more than 6 target spacings after its parent can use
    /// the proof-of-work limit (ZIP 205, ZIP 208; zcashd
    /// `nPowAllowMinDifficultyBlocksAfterHeight` plus 1). `None`: the network has no such
    /// rule.
    pub min_difficulty_start_height: Option<u32>,
    /// First height at which the time of a block is at most its median-time-past plus
    /// 90 min (protocol specification §7.6; Zakura `is_max_block_time_enforced`).
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

impl NetworkParams {
    /// Blocks between two halvings at the post-Blossom target spacing.
    pub const fn post_blossom_halving_interval(&self) -> u32 {
        self.pre_blossom_halving_interval
            * (crate::PRE_BLOSSOM_TARGET_SPACING / crate::POST_BLOSSOM_TARGET_SPACING)
    }
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

const MAINNET: NetworkParams = NetworkParams {
    network: Network::Mainnet,
    // `zcash-cli getblockhash 0`.
    genesis_hash: hash_from_display(
        "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08",
    ),
    genesis_time: 1_477_641_360,
    pow: PowParams::MAINNET,
    // zcashd `chainparams.cpp`: `0x0007ffff…ff`.
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

const TESTNET: NetworkParams = NetworkParams {
    network: Network::Testnet,
    // `zcash-cli -testnet getblockhash 0`.
    genesis_hash: hash_from_display(
        "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38",
    ),
    genesis_time: 1_477_648_033,
    pow: PowParams::TESTNET,
    // zcashd `chainparams.cpp`: `0x07ffff…ff`.
    pow_limit: ones(251),
    pow_limit_bits: 0x2007_ffff,
    disable_pow: false,
    // Zakura `TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT`.
    min_difficulty_start_height: Some(299_188),
    // Zakura `TESTNET_MAX_TIME_START_HEIGHT`.
    max_time_start_height: 653_606,
    // Zakura `TESTNET_TEMPORARY_ORCHARD_DISABLING_SOFT_FORK_HEIGHT`.
    orchard_disabled_start_height: Some(4_048_500),
    coinbase_must_be_shielded: true,
    slow_start_interval: 20_000,
    pre_blossom_halving_interval: 840_000,
};

const REGTEST: NetworkParams = NetworkParams {
    network: Network::Regtest,
    // `zcash-cli -regtest getblockhash 0`.
    genesis_hash: hash_from_display(
        "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327",
    ),
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

impl Network {
    pub const ALL: [Network; 3] = [Network::Mainnet, Network::Testnet, Network::Regtest];

    pub const fn name(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
            Network::Regtest | Network::ConfiguredRegtest(_) => "regtest",
        }
    }

    /// Whether the network is Regtest, with or without a [`RegtestConfig`].
    pub const fn is_regtest(self) -> bool {
        matches!(self, Network::Regtest | Network::ConfiguredRegtest(_))
    }

    /// The values that depend only on the network. A configured Regtest has the values of
    /// [`Network::Regtest`], and `network` names that network.
    pub const fn params(self) -> &'static NetworkParams {
        match self {
            Network::Mainnet => &MAINNET,
            Network::Testnet => &TESTNET,
            Network::Regtest | Network::ConfiguredRegtest(_) => &REGTEST,
        }
    }

    /// The height at which `upgrade` activates. `None` when the network has no height for
    /// it.
    pub fn activation_height(self, upgrade: Upgrade) -> Option<u32> {
        match self {
            Network::Mainnet => protocol_height(&MAIN_NETWORK, upgrade),
            Network::Testnet => protocol_height(&TEST_NETWORK, upgrade),
            Network::Regtest | Network::ConfiguredRegtest(_) => match upgrade {
                Upgrade::Sprout => Some(0),
                Upgrade::Overwinter
                | Upgrade::Sapling
                | Upgrade::Blossom
                | Upgrade::Heartwood
                | Upgrade::Canopy
                | Upgrade::Nu5 => Some(1),
                Upgrade::Nu6 | Upgrade::Nu6_1 | Upgrade::Nu6_2 | Upgrade::Nu6_3 | Upgrade::Nu7 => {
                    let Network::ConfiguredRegtest(config) = self else {
                        return None;
                    };
                    let slot = CONFIGURABLE.iter().position(|u| *u == upgrade)?;
                    config.activation_heights[slot]
                }
            },
        }
    }

    /// The upgrade whose rules apply at `height`: the last upgrade in activation order with
    /// an activation height at or below `height`.
    pub fn upgrade_at(self, height: u32) -> Upgrade {
        let active = Upgrade::ALL
            .into_iter()
            .rev()
            .find(|upgrade| matches!(self.activation_height(*upgrade), Some(h) if h <= height));
        let Some(upgrade) = active else {
            unreachable!("Sprout is active from height 0");
        };
        upgrade
    }

    /// Whether the Orchard pool is off at `height`: the height is at or after the start of
    /// the soft fork and before the NU6.2 activation, which starts the pool again (Zakura
    /// `is_orchard_temporarily_disabled`, `zakura-chain/src/parameters/network.rs:373-378`).
    /// On a network without an NU6.2 height the pool stays off from the start height.
    pub fn orchard_disabled(self, height: u32) -> bool {
        let started =
            matches!(self.params().orchard_disabled_start_height, Some(start) if height >= start);
        let ended =
            matches!(self.activation_height(Upgrade::Nu6_2), Some(nu6_2) if height >= nu6_2);
        started && !ended
    }

    /// The upgrade whose rules apply at the first activation height above `height`. `None`
    /// when no upgrade with a height activates after `height`.
    pub fn next_upgrade(self, height: u32) -> Option<Upgrade> {
        let next = Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| self.activation_height(upgrade))
            .filter(|h| *h > height)
            .min()?;
        Some(self.upgrade_at(next))
    }
}

/// The NU7 activation height of Testnet (Zakura `testnet::NU7`,
/// `zakura-chain/src/parameters/constants.rs:80`). Mainnet has no NU7 height: Zakura has
/// none (`constants.rs:83-112`, and `zakura-protocol` `consensus.rs:503`).
///
/// The value is here and not in the crypto backend: the upstream `zcash_protocol` has no
/// NU7 height, and `rules_at` must refuse a height with NU7 rules on a backend without
/// the NU7 branch id.
///
/// A Mainnet height needs a rule that this crate does not have: the ZIP 2008 recipient of
/// the last funding stream, a P2PKH address (Zakura `subsidy/constants/mainnet.rs:196-221`).
/// The test `funding::tests::zip_2008_has_no_code_while_mainnet_has_no_nu7_height` fails
/// when Mainnet gets a height.
const TESTNET_NU7_HEIGHT: u32 = 4_465_026;

/// The activation height of `upgrade` in the backend's `zcash_protocol` parameters. The
/// NU7 height is [`TESTNET_NU7_HEIGHT`] on every backend.
fn protocol_height<P: Parameters>(params: &P, upgrade: Upgrade) -> Option<u32> {
    let protocol = match upgrade {
        Upgrade::Sprout => return Some(0),
        Upgrade::Nu7 => {
            return match params.network_type() {
                NetworkType::Test => Some(TESTNET_NU7_HEIGHT),
                NetworkType::Main | NetworkType::Regtest => None,
            }
        }
        Upgrade::Overwinter => NetworkUpgrade::Overwinter,
        Upgrade::Sapling => NetworkUpgrade::Sapling,
        Upgrade::Blossom => NetworkUpgrade::Blossom,
        Upgrade::Heartwood => NetworkUpgrade::Heartwood,
        Upgrade::Canopy => NetworkUpgrade::Canopy,
        Upgrade::Nu5 => NetworkUpgrade::Nu5,
        Upgrade::Nu6 => NetworkUpgrade::Nu6,
        Upgrade::Nu6_1 => NetworkUpgrade::Nu6_1,
        Upgrade::Nu6_2 => NetworkUpgrade::Nu6_2,
        Upgrade::Nu6_3 => NetworkUpgrade::Nu6_3,
    };
    params.activation_height(protocol).map(u32::from)
}

#[cfg(test)]
mod tests {
    use hayai_crypto::zcash_protocol::consensus::{BlockHeight, NetworkType};
    use hayai_wire::header::expand_target;

    use super::*;

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
                        network.upgrade_at(h).branch_id(),
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

        let refused = |activations: &[(Upgrade, u32)], checkpoints, mandatory| {
            RegtestConfig::new(activations, checkpoints, mandatory).expect_err("refused")
        };
        assert_eq!(
            refused(&[(Upgrade::Nu5, 5)], Vec::new(), 0),
            RegtestConfigError::FixedUpgrade(Upgrade::Nu5)
        );
        assert_eq!(
            refused(&[(Upgrade::Nu6, 1)], Vec::new(), 0),
            RegtestConfigError::Order {
                upgrade: Upgrade::Nu6,
                height: 1
            }
        );
        assert_eq!(
            refused(&[(Upgrade::Nu6, 9), (Upgrade::Nu6_1, 8)], Vec::new(), 0),
            RegtestConfigError::Order {
                upgrade: Upgrade::Nu6_1,
                height: 8
            }
        );
        assert_eq!(
            refused(&[], vec![(0, hash)], 0),
            RegtestConfigError::Genesis
        );
        assert_eq!(
            refused(&[], vec![(4, hash)], 5),
            RegtestConfigError::Coverage(5)
        );
        assert_eq!(
            refused(&[], vec![(4, hash), (4, hash)], 0),
            RegtestConfigError::Checkpoints(DuplicateCheckpoint(4))
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
        assert_eq!(params.post_blossom_halving_interval(), 288);
        assert_eq!(params.slow_start_interval, 0);
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
        assert_eq!(mainnet.post_blossom_halving_interval(), 1_680_000);
        let testnet = Network::Testnet.params();
        assert_eq!(
            testnet.genesis_hash.to_string(),
            "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38"
        );
        assert_eq!(testnet.pow, PowParams::TESTNET);
        assert_eq!(testnet.pow_limit_bits, 0x2007_ffff);
        for network in Network::ALL {
            assert_eq!(network.params().network, network);
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
    fn every_branch_id_maps_to_its_upgrade() {
        for upgrade in Upgrade::ALL {
            match upgrade.branch_id() {
                Some(branch) => assert_eq!(Upgrade::of_branch(branch), Ok(upgrade)),
                None => assert_eq!(upgrade, Upgrade::Nu7),
            }
        }
        assert_eq!(Upgrade::Nu7.branch_id(), hayai_crypto::nu7_branch());
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
}
