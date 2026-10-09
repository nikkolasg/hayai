//! The values of one chain that the rules read (protocol specification §5.3 and §7.8, ZIP
//! 200 and the deployment ZIPs), and the upgrade that is active at a height.
//!
//! A [`CoreSpec`] is plain data: activation heights, the slow start and the halving
//! interval, the proof-of-work limit and the heights of the rules that only some networks
//! have, the funding stream sets with their recipients as scripts, the lockbox
//! disbursements, the founders' scripts and the NSM seed. The adapter builds it from a
//! network and checks it with [`CoreSpec::checked`] one time.

use crate::funding::{self, StreamSet};
use crate::lockbox::{self, Disbursement};
use crate::subsidy_schedule;
use crate::{ConsensusError, P2shScript, POST_BLOSSOM_TARGET_SPACING, PRE_BLOSSOM_TARGET_SPACING};

/// The number of network upgrades: the length of [`CoreSpec::activation_heights`].
pub const UPGRADES: usize = 12;

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
    pub const ALL: [Upgrade; UPGRADES] = [
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

    /// The position of the upgrade in [`Upgrade::ALL`]: the index of its activation height.
    pub const fn index(self) -> usize {
        match self {
            Upgrade::Sprout => 0,
            Upgrade::Overwinter => 1,
            Upgrade::Sapling => 2,
            Upgrade::Blossom => 3,
            Upgrade::Heartwood => 4,
            Upgrade::Canopy => 5,
            Upgrade::Nu5 => 6,
            Upgrade::Nu6 => 7,
            Upgrade::Nu6_1 => 8,
            Upgrade::Nu6_2 => 9,
            Upgrade::Nu6_3 => 10,
            Upgrade::Nu7 => 11,
        }
    }

    /// The consensus branch id of the upgrade.
    ///
    /// ZIP 200: `CONSENSUS_BRANCH_ID` of each upgrade, 0 for Sprout. The values are those
    /// of the deployment ZIPs 201, 205, 206, 250, 251, 252, 253, 255, 257, 258 and 259.
    pub const fn branch_id(self) -> u32 {
        match self {
            Upgrade::Sprout => 0,
            Upgrade::Overwinter => 0x5ba8_1b19,
            Upgrade::Sapling => 0x76b8_09bb,
            Upgrade::Blossom => 0x2bb4_0e60,
            Upgrade::Heartwood => 0xf5b9_230b,
            Upgrade::Canopy => 0xe9ff_75a6,
            Upgrade::Nu5 => 0xc2d6_d0b4,
            Upgrade::Nu6 => 0xc8e7_1055,
            Upgrade::Nu6_1 => 0x4dec_4df0,
            Upgrade::Nu6_2 => 0x5437_f330,
            Upgrade::Nu6_3 => 0x37a5_165b,
            Upgrade::Nu7 => 0x7719_0ad9,
        }
    }

    /// The upgrade of a consensus branch id.
    pub fn of_branch(branch: u32) -> Result<Upgrade, ConsensusError> {
        for i in 0..UPGRADES {
            if Upgrade::ALL[i].branch_id() == branch {
                return Ok(Upgrade::ALL[i]);
            }
        }
        Err(ConsensusError::UnknownBranch(branch))
    }
}

/// The values of one chain that the rules read.
///
/// The adapter copies the values of its network into this struct. The rules of an upgrade
/// are code ([`crate::rule_sets`]): a chain chooses only when each upgrade activates, and the
/// values of its spec.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CoreSpec {
    /// The activation height of each upgrade, at [`Upgrade::index`]. `None`: the upgrade
    /// does not activate. Sprout activates at height 0.
    pub activation_heights: [Option<u32>; UPGRADES],
    /// `SlowStartInterval`: the subsidy ramps up over this number of blocks.
    pub slow_start_interval: u32,
    /// Blocks between two halvings at the pre-Blossom target spacing.
    pub pre_blossom_halving_interval: u32,
    /// The proof-of-work limit: the easiest target, as a 256-bit little-endian integer.
    pub pow_limit: [u8; 32],
    /// The compact form of [`CoreSpec::pow_limit`] (zcashd `powLimit.GetCompact()`).
    pub pow_limit_bits: u32,
    /// The network waives the proof of work (Zakura's `disable_pow`, Regtest only): a
    /// header has no expected `nBits`.
    pub disable_pow: bool,
    /// First height at which a block whose time is more than the minimum-difficulty gap
    /// after its parent must have the proof-of-work limit as `nBits`
    /// ([`crate::rule_sets::DifficultyParams::min_difficulty_gap_spacings`]; zcashd
    /// `nPowAllowMinDifficultyBlocksAfterHeight` plus 1). `None`: the network has no such
    /// rule. ZIP 205: Testnet height 299,188.
    pub min_difficulty_start_height: Option<u32>,
    /// First height at which the time of a block is at most its median-time-past plus
    /// 90 min (spec §7.6; Zakura `is_max_block_time_enforced`). Spec §7.6: height 2 on
    /// Mainnet, height 653,606 on Testnet.
    pub max_time_start_height: u32,
    /// First height of the soft fork that removes the Orchard pool for a time: from this
    /// height until the NU6.2 activation, a transaction has no Orchard bundle. `None`: the
    /// network has no such soft fork.
    pub orchard_disabled_start_height: Option<u32>,
    /// A transaction that spends a coinbase output has no transparent output (zcashd
    /// `fCoinbaseMustBeShielded`). Regtest does not have the rule. No rule of this crate
    /// reads the flag yet: hayai-state and hayai-mempool read it through the spec, and
    /// stage 2 of the core (item M2, the contextual rules) moves the rule here.
    pub coinbase_must_be_shielded: bool,
    /// The funding stream sets, in height order ([`crate::funding`]).
    pub funding_streams: Vec<StreamSet>,
    /// The outputs that the coinbase of the NU6.1 activation block must have
    /// ([`crate::lockbox::disbursements`]).
    pub lockbox_disbursements: Vec<Disbursement>,
    /// The scripts of `FounderAddressList` (spec §7.9). Empty: the network has no
    /// founders' reward ([`crate::founders`]).
    pub founders_scripts: Vec<P2shScript>,
    /// `INITIAL_NSM_VALUE_BALANCE` (ZIP 237, [`crate::nsm::expected_seed`]). `None`: the
    /// balance before NU7 is the balance that the chain gives.
    pub nsm_seed: Option<u64>,
    /// The NSM reissuance height of a test. `None` on each built-in network.
    pub test_reissuance_height: Option<u32>,
    /// `HeightForHalving(1)` (Zakura `height_for_first_halving`). `None`: no height has the
    /// halving index 1. [`CoreSpec::checked`] derives it.
    pub first_halving: Option<u32>,
}

/// Why a [`CoreSpec`] is not valid: a value that a rule would later meet as an error.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SpecError {
    #[error("the activation height of Sprout is not 0")]
    Sprout,
    #[error(
        "the activation height {height} of {upgrade:?} is below the height of an earlier upgrade"
    )]
    Order { upgrade: Upgrade, height: u32 },
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
    #[error("the funding stream range from {start} to {end} ends below its start")]
    StreamRange { start: u32, end: u32 },
    #[error("the funding stream receiver {0:?} is two times in one range")]
    StreamReceiver(funding::Receiver),
    #[error("the funding stream numerators of one range have the sum {0}, above 100")]
    StreamNumerators(u128),
    #[error("the deferred pool is a funding stream receiver with a script")]
    DeferredScript,
    #[error(
        "the funding stream receiver {receiver:?} has {found} scripts and its range from \
         {start} to {end} has {required} address periods"
    )]
    StreamScripts {
        receiver: funding::Receiver,
        start: u32,
        end: u32,
        required: usize,
        found: usize,
    },
    #[error("a lockbox disbursement, or the sum of all, is above MAX_MONEY zatoshis")]
    DisbursementAmount,
    #[error("the heights without the Orchard pool from height {0} are not all in NU6.1")]
    OrchardSoftFork(u32),
    /// A rule failed on the spec during the checks.
    #[error(transparent)]
    Rule(#[from] ConsensusError),
}

impl CoreSpec {
    /// The height at which `upgrade` activates. `None` when the chain has no height for it.
    pub fn activation_height(&self, upgrade: Upgrade) -> Option<u32> {
        self.activation_heights[upgrade.index()]
    }

    /// The upgrade whose rules apply at `height`: the last upgrade in activation order with
    /// an activation height at or below `height`.
    ///
    /// ZIP 200: a block of a known height is validated under the rules of the consensus
    /// branch of that height. [`ConsensusError::UncheckedSpec`] when no upgrade is active:
    /// Sprout activates at height 0 on a checked spec.
    pub fn upgrade_at(&self, height: u32) -> Result<Upgrade, ConsensusError> {
        let mut found: Option<Upgrade> = None;
        let mut i = UPGRADES;
        while i > 0 {
            i -= 1;
            let activation = self.activation_heights[i];
            if let Some(activation) = activation {
                if activation <= height {
                    found = Some(Upgrade::ALL[i]);
                    break;
                }
            }
        }
        match found {
            Some(upgrade) => Ok(upgrade),
            None => Err(ConsensusError::UncheckedSpec),
        }
    }

    /// Whether the Orchard pool is off at `height`: the height is at or after the start of
    /// the soft fork and before the NU6.2 activation, which starts the pool again (Zakura
    /// `is_orchard_temporarily_disabled`, `zakura-chain/src/parameters/network.rs:373-378`).
    /// On a chain without an NU6.2 height the pool stays off from the start height.
    pub fn orchard_disabled(&self, height: u32) -> bool {
        let started = match self.orchard_disabled_start_height {
            Some(start) => height >= start,
            None => false,
        };
        let ended = match self.activation_height(Upgrade::Nu6_2) {
            Some(nu6_2) => height >= nu6_2,
            None => false,
        };
        started && !ended
    }

    /// The upgrade whose rules apply at the first activation height above `height`. `None`
    /// when no upgrade with a height activates after `height`.
    pub fn next_upgrade(&self, height: u32) -> Result<Option<Upgrade>, ConsensusError> {
        let mut next: Option<u32> = None;
        for i in 0..UPGRADES {
            let activation = self.activation_heights[i];
            let Some(activation) = activation else {
                continue;
            };
            if activation <= height {
                continue;
            }
            next = match next {
                Some(earliest) if earliest <= activation => Some(earliest),
                _ => Some(activation),
            };
        }
        match next {
            Some(activation) => Ok(Some(self.upgrade_at(activation)?)),
            None => Ok(None),
        }
    }

    /// Blocks between two halvings at the post-Blossom target spacing.
    pub fn post_blossom_halving_interval(&self) -> Result<u32, ConsensusError> {
        let ratio = PRE_BLOSSOM_TARGET_SPACING / POST_BLOSSOM_TARGET_SPACING;
        let Some(interval) = self.pre_blossom_halving_interval.checked_mul(ratio) else {
            return Err(ConsensusError::Overflow);
        };
        Ok(interval)
    }

    /// The spec with its first halving derived, after these checks:
    /// - Sprout activates at height 0, and no activation height is below the height of an
    ///   earlier upgrade.
    /// - The post-Blossom halving interval has one block or more for each of its 48
    ///   address periods, and it fits in 32 bits.
    /// - The slow start is not 1 block, and it ends before the first halving.
    /// - A chain with funding streams has a first halving.
    /// - Each funding stream set has a range that does not end below its start, one entry
    ///   for each receiver, numerators of 100 or less in total, no script for the deferred
    ///   pool, and an end that NU7 keeps below the largest height. Each stream with two
    ///   scripts or more has one script for each address period of its range.
    /// - The lockbox disbursements sum to a valid amount of money.
    /// - The heights without the Orchard pool are in NU6.1.
    ///
    /// The adapter decodes the addresses and checks the checkpoints before this function.
    pub fn checked(mut self) -> Result<Self, SpecError> {
        if self.activation_heights[Upgrade::Sprout.index()] != Some(0) {
            return Err(SpecError::Sprout);
        }
        let mut floor = 0;
        let mut out_of_order: Option<(usize, u32)> = None;
        for i in 0..UPGRADES {
            let activation = self.activation_heights[i];
            let Some(height) = activation else {
                continue;
            };
            if height < floor {
                out_of_order = Some((i, height));
                break;
            }
            floor = height;
        }
        if let Some((i, height)) = out_of_order {
            return Err(SpecError::Order {
                upgrade: Upgrade::ALL[i],
                height,
            });
        }
        // The address period divides by the post-Blossom interval over 48, and the halving
        // index by the pre-Blossom interval.
        let interval = self.pre_blossom_halving_interval;
        match self.post_blossom_halving_interval() {
            Ok(post) if post >= funding::PERIODS_PER_HALVING_INTERVAL => {}
            _ => return Err(SpecError::HalvingInterval(interval)),
        }
        self.first_halving = subsidy_schedule::halving_height(&self, 1, u32::MAX)?;
        // The closed form of `subsidy_schedule::scheduled_issuance` needs a slow start shift of 1
        // block or more, and the halving index 0 during the slow start.
        let slow_start = self.slow_start_interval;
        let first_inside_slow_start = match self.first_halving {
            Some(first) => first < slow_start,
            None => false,
        };
        if slow_start == 1 || first_inside_slow_start {
            return Err(SpecError::SlowStart(slow_start));
        }
        if self.funding_streams.len() > 0 {
            let Some(_) = self.first_halving else {
                return Err(SpecError::NoFirstHalving);
            };
        }
        let nu7 = self.activation_height(Upgrade::Nu7);
        funding::check_sets(nu7, &self.funding_streams)?;
        lockbox::check_disbursements(&self.lockbox_disbursements)?;
        if let Some(start) = self.orchard_disabled_start_height {
            // `rules_at` gives the NU6.1 rules without the Orchard pool from the start height
            // to the block before NU6.2, and requires NU6.1 at each of these heights.
            let last = match self.activation_height(Upgrade::Nu6_2) {
                Some(nu6_2) => nu6_2.checked_sub(1),
                None => Some(u32::MAX),
            };
            if let Some(last) = last {
                if last >= start {
                    let outside = self.upgrade_at(start)? != Upgrade::Nu6_1
                        || self.upgrade_at(last)? != Upgrade::Nu6_1;
                    if outside {
                        return Err(SpecError::OrchardSoftFork(start));
                    }
                }
            }
        }
        funding::check_script_counts(&self, true)?;
        Ok(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The activation heights of the `(upgrade, height)` pairs of `list`, with Sprout at
    /// height 0.
    pub(crate) fn heights(list: &[(Upgrade, u32)]) -> [Option<u32>; UPGRADES] {
        let mut heights = [None; UPGRADES];
        heights[Upgrade::Sprout.index()] = Some(0);
        for (upgrade, height) in list {
            heights[upgrade.index()] = Some(*height);
        }
        heights
    }

    /// A chain with the values of Regtest: no slow start, a halving interval of 144 blocks,
    /// Overwinter to NU5 at height 1, no funding stream, no disbursement, no founders'
    /// reward. The tests set the other values.
    pub(crate) fn regtest() -> CoreSpec {
        CoreSpec {
            activation_heights: heights(&[
                (Upgrade::Overwinter, 1),
                (Upgrade::Sapling, 1),
                (Upgrade::Blossom, 1),
                (Upgrade::Heartwood, 1),
                (Upgrade::Canopy, 1),
                (Upgrade::Nu5, 1),
            ]),
            slow_start_interval: 0,
            pre_blossom_halving_interval: 144,
            pow_limit: [0x0f; 32],
            pow_limit_bits: 0x200f_0f0f,
            disable_pow: true,
            min_difficulty_start_height: None,
            max_time_start_height: 2,
            orchard_disabled_start_height: None,
            coinbase_must_be_shielded: false,
            funding_streams: Vec::new(),
            lockbox_disbursements: Vec::new(),
            founders_scripts: Vec::new(),
            nsm_seed: None,
            test_reissuance_height: None,
            first_halving: Some(287),
        }
    }

    #[test]
    fn the_upgrade_indexes_are_the_positions_in_all() {
        for i in 0..UPGRADES {
            assert_eq!(Upgrade::ALL[i].index(), i);
        }
    }

    /// The `CONSENSUS_BRANCH_ID` of each deployment ZIP: 201 (Overwinter), 205, 206, 250,
    /// 251, 252, 253, 255, 257, 258 and 259.
    #[test]
    fn the_branch_ids_of_the_zips() {
        let zips: [(Upgrade, u32); 12] = [
            (Upgrade::Sprout, 0),
            (Upgrade::Overwinter, 0x5ba8_1b19),
            (Upgrade::Sapling, 0x76b8_09bb),
            (Upgrade::Blossom, 0x2bb4_0e60),
            (Upgrade::Heartwood, 0xf5b9_230b),
            (Upgrade::Canopy, 0xe9ff_75a6),
            (Upgrade::Nu5, 0xc2d6_d0b4),
            (Upgrade::Nu6, 0xc8e7_1055),
            (Upgrade::Nu6_1, 0x4dec_4df0),
            (Upgrade::Nu6_2, 0x5437_f330),
            (Upgrade::Nu6_3, 0x37a5_165b),
            (Upgrade::Nu7, 0x7719_0ad9),
        ];
        for (upgrade, branch) in zips {
            assert_eq!(upgrade.branch_id(), branch, "{upgrade:?}");
            assert_eq!(Upgrade::of_branch(branch), Ok(upgrade));
        }
        assert_eq!(
            Upgrade::of_branch(0xdead_beef),
            Err(ConsensusError::UnknownBranch(0xdead_beef))
        );
    }

    #[test]
    fn the_upgrade_of_a_height_and_the_next_one() {
        let spec = regtest();
        assert_eq!(spec.upgrade_at(0), Ok(Upgrade::Sprout));
        assert_eq!(spec.upgrade_at(1), Ok(Upgrade::Nu5));
        assert_eq!(spec.upgrade_at(u32::MAX), Ok(Upgrade::Nu5));
        assert_eq!(spec.next_upgrade(0), Ok(Some(Upgrade::Nu5)));
        assert_eq!(spec.next_upgrade(1), Ok(None));
        let mut later = regtest();
        later.activation_heights[Upgrade::Nu6.index()] = Some(20);
        later.activation_heights[Upgrade::Nu6_2.index()] = Some(40);
        assert_eq!(later.upgrade_at(19), Ok(Upgrade::Nu5));
        assert_eq!(later.upgrade_at(20), Ok(Upgrade::Nu6));
        assert_eq!(later.upgrade_at(40), Ok(Upgrade::Nu6_2));
        assert_eq!(later.next_upgrade(1), Ok(Some(Upgrade::Nu6)));
        assert_eq!(later.next_upgrade(20), Ok(Some(Upgrade::Nu6_2)));
        assert_eq!(later.next_upgrade(40), Ok(None));
        assert_eq!(later.post_blossom_halving_interval(), Ok(288));
    }

    /// A spec without Sprout at height 0 has no active upgrade: the error, not a default.
    #[test]
    fn no_active_upgrade_is_an_error() {
        let mut spec = regtest();
        spec.activation_heights = [None; UPGRADES];
        spec.activation_heights[Upgrade::Sprout.index()] = Some(5);
        assert_eq!(spec.upgrade_at(4), Err(ConsensusError::UncheckedSpec));
        assert_eq!(spec.upgrade_at(5), Ok(Upgrade::Sprout));
        assert_eq!(spec.next_upgrade(0), Ok(Some(Upgrade::Sprout)));
        spec.activation_heights[Upgrade::Sprout.index()] = None;
        spec.activation_heights[Upgrade::Blossom.index()] = Some(3);
        assert_eq!(spec.next_upgrade(0), Ok(Some(Upgrade::Blossom)));
        assert_eq!(spec.next_upgrade(3), Ok(None));
        assert_eq!(spec.clone().checked(), Err(SpecError::Sprout));
        let mut huge = regtest();
        huge.pre_blossom_halving_interval = u32::MAX;
        assert_eq!(
            huge.post_blossom_halving_interval(),
            Err(ConsensusError::Overflow)
        );
    }

    #[test]
    fn the_orchard_soft_fork_range() {
        let mut spec = regtest();
        spec.activation_heights[Upgrade::Nu6.index()] = Some(10);
        spec.activation_heights[Upgrade::Nu6_1.index()] = Some(20);
        spec.activation_heights[Upgrade::Nu6_2.index()] = Some(40);
        spec.orchard_disabled_start_height = Some(30);
        for (height, disabled) in [(29, false), (30, true), (39, true), (40, false)] {
            assert_eq!(spec.orchard_disabled(height), disabled, "{height}");
        }
        let Ok(_) = spec.clone().checked() else {
            panic!("the soft fork inside NU6.1 is valid");
        };
        spec.orchard_disabled_start_height = Some(19);
        assert_eq!(spec.clone().checked(), Err(SpecError::OrchardSoftFork(19)));
        // Without an NU6.2 height the pool stays off, and the tail of the chain is NU6.1.
        spec.orchard_disabled_start_height = Some(30);
        spec.activation_heights[Upgrade::Nu6_2.index()] = None;
        assert!(spec.orchard_disabled(u32::MAX));
        let Ok(_) = spec.clone().checked() else {
            panic!("a soft fork until the end of an NU6.1 chain is valid");
        };
        spec.activation_heights[Upgrade::Nu6_3.index()] = Some(50);
        assert_eq!(spec.checked(), Err(SpecError::OrchardSoftFork(30)));
    }

    #[test]
    fn the_checks_of_a_spec() {
        let refused = |change: fn(&mut CoreSpec)| {
            let mut spec = regtest();
            change(&mut spec);
            spec.checked().expect_err("refused")
        };
        let Ok(checked) = regtest().checked() else {
            panic!("the Regtest values are valid");
        };
        assert_eq!(checked.first_halving, Some(287));
        assert_eq!(
            refused(|spec| spec.activation_heights[Upgrade::Nu6.index()] = Some(0)),
            SpecError::Order {
                upgrade: Upgrade::Nu6,
                height: 0
            }
        );
        // 23 blocks before Blossom are 46 blocks after it: 0 blocks for each of 48 periods.
        assert_eq!(
            refused(|spec| spec.pre_blossom_halving_interval = 23),
            SpecError::HalvingInterval(23)
        );
        assert_eq!(
            refused(|spec| spec.pre_blossom_halving_interval = u32::MAX / 2 + 1),
            SpecError::HalvingInterval(u32::MAX / 2 + 1)
        );
        assert_eq!(
            refused(|spec| spec.slow_start_interval = 1),
            SpecError::SlowStart(1)
        );
        // A slow start of 2,000 blocks shifts the first halving to 2,287, so the slow start
        // ends before it. Without Blossom, a halving interval of 400 blocks puts the first
        // halving at 1,400: inside the slow start.
        let mut long = regtest();
        long.slow_start_interval = 2_000;
        let Ok(long) = long.checked() else {
            panic!("a slow start before the first halving is valid");
        };
        assert_eq!(long.first_halving, Some(2_287));
        assert_eq!(
            refused(|spec| {
                spec.activation_heights =
                    heights(&[(Upgrade::Overwinter, 1), (Upgrade::Sapling, 1)]);
                spec.slow_start_interval = 2_000;
                spec.pre_blossom_halving_interval = 400;
            }),
            SpecError::SlowStart(2_000)
        );
    }
}
