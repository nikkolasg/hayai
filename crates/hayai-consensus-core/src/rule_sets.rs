//! One rule set per network upgrade and the selection of the rule set of a height.
//!
//! A new upgrade is one more entry of the rule set table [`RULE_SETS`]. The adapter decides
//! which entries its crypto backend supports: NU7 needs the NU7 branch id of the backend.

use crate::{
    BlockLimits, ConsensusError, CoreSpec, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    POST_NU7_TARGET_SPACING, PRE_BLOSSOM_TARGET_SPACING, UPGRADES,
};

/// The transaction versions that a block of one upgrade can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxVersions(u8);

impl TxVersions {
    /// The set of `versions`. Each version is in 1..=6; a version outside that range is
    /// not in the set (the test `tx_versions_hold_the_versions_one_to_six`). The function
    /// does not panic: the table test `the_rules_of_each_upgrade` asserts the exact version
    /// set of every upgrade, and it is the guard against a version outside the range.
    pub const fn of(versions: &[u32]) -> Self {
        let mut mask = 0u8;
        let mut i = 0;
        while i < versions.len() {
            if versions[i] >= 1 && versions[i] <= 6 {
                mask |= 1 << versions[i];
            }
            i += 1;
        }
        Self(mask)
    }

    /// Whether a transaction with version number `version` is allowed.
    pub const fn allows(self, version: u32) -> bool {
        version <= 6 && self.0 & (1 << version) != 0
    }
}

/// The shielded pools that transactions of one upgrade can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShieldedPools {
    pub sprout: bool,
    pub sapling: bool,
    pub orchard: bool,
    pub ironwood: bool,
}

/// The version of the ZIP 221 history tree of one upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryVersion {
    /// Before Heartwood: no history tree.
    None,
    /// Heartwood and Canopy: Sapling data in a leaf.
    V1,
    /// NU5 to NU6.2: Sapling and Orchard data in a leaf.
    V2,
    /// From NU6.3: Sapling, Orchard and Ironwood data in a leaf.
    V3,
}

/// The coinbase rules that change between upgrades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoinbaseRules {
    /// ZIP 203, from NU5: the expiry height of the coinbase equals the block height.
    pub expiry_is_height: bool,
    /// ZIP 213, from Heartwood: the coinbase can have shielded outputs.
    pub shielded_outputs: bool,
    /// ZIP 236, from NU6: the coinbase pays the subsidy and the fees exactly. Before NU6
    /// it pays at most that amount.
    pub exact_value: bool,
    /// Until NU6.2: the coinbase can have an Orchard bundle. From NU6.3 it cannot.
    pub orchard_bundle: bool,
    /// From NU7: the coinbase gets the miner share of the fees
    /// ([`crate::nsm::miner_fee_share`]), and the rest stays out of the chain value pools.
    pub nsm_fee_share: bool,
}

/// The parameters of the difficulty adjustment (protocol specification §7.7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DifficultyParams {
    /// Target block spacing in seconds.
    pub target_spacing: u32,
    /// `PoWAveragingWindow`: blocks whose targets form the mean target.
    pub averaging_window: u32,
    /// `PoWMaxAdjustUp` in percent.
    pub max_adjust_up_percent: u32,
    /// `PoWMaxAdjustDown` in percent.
    pub max_adjust_down_percent: u32,
    /// `PoWDampingFactor`.
    pub damping_factor: u32,
    /// Testnet minimum-difficulty rule (ZIP 205, ZIP 208, ZIP 218): a block whose time is
    /// more than this number of target spacings after its parent must have the
    /// proof-of-work limit as `nBits`. The gap is 450 s from Blossom: 6 spacings of 75 s,
    /// and 18 spacings of 25 s from NU7.
    pub min_difficulty_gap_spacings: u32,
}

impl DifficultyParams {
    /// Spec §7.7.3 with the constants of §5.3: `PoWAveragingWindow` 17, `PoWMaxAdjustUp`
    /// 16 %, `PoWMaxAdjustDown` 32 %, `PoWDampingFactor` 4. ZIP 205: the minimum-difficulty
    /// gap is 6 spacings.
    pub const PRE_BLOSSOM: Self = Self {
        target_spacing: PRE_BLOSSOM_TARGET_SPACING,
        averaging_window: 17,
        max_adjust_up_percent: 16,
        max_adjust_down_percent: 32,
        damping_factor: 4,
        min_difficulty_gap_spacings: 6,
    };
    /// ZIP 208: the target spacing is 75 s from Blossom, and the averaging window and the
    /// minimum-difficulty gap of 6 spacings do not change.
    pub const POST_BLOSSOM: Self = Self {
        target_spacing: POST_BLOSSOM_TARGET_SPACING,
        ..Self::PRE_BLOSSOM
    };
    /// ZIP 218: `PostNU7PoWTargetSpacing` 25 s, `PostNU7PoWAveragingWindow` 102 blocks,
    /// the Testnet minimum-difficulty gap of 18 spacings (Zakura
    /// `zakura-chain/src/parameters/network_upgrade.rs:257,285,336`).
    pub const POST_NU7: Self = Self {
        target_spacing: POST_NU7_TARGET_SPACING,
        averaging_window: 102,
        min_difficulty_gap_spacings: 18,
        ..Self::PRE_BLOSSOM
    };
}

/// The flags zcashd applies to every transaction of a block (`ConnectBlock`:
/// `SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY`), which Zakura's verifier also
/// uses (`zakura-script/src/lib.rs:173-174`). The bits are those of the script interpreter
/// (`zcash_script::interpreter::Flags`); the adapter checks them.
pub const SCRIPT_VERIFY_P2SH: u32 = 1 << 0;
pub const SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY: u32 = 1 << 9;
pub const SCRIPT_FLAGS: u32 = SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY;

/// The rules of one network upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleSet {
    pub upgrade: Upgrade,
    /// The consensus branch id of transactions and of the history tree (ZIP 200).
    pub branch_id: u32,
    pub tx_versions: TxVersions,
    /// The script verification flags of block validation ([`SCRIPT_FLAGS`]).
    pub script_flags: u32,
    pub pools: ShieldedPools,
    /// ZIP 211: until Heartwood a JoinSplit can move value into the Sprout pool. From
    /// Canopy `vpub_old` of every JoinSplit is zero.
    pub sprout_deposit: bool,
    pub limits: BlockLimits,
    pub history: HistoryVersion,
    pub coinbase: CoinbaseRules,
    pub difficulty: DifficultyParams,
}

const SPROUT: RuleSet = RuleSet {
    upgrade: Upgrade::Sprout,
    branch_id: Upgrade::Sprout.branch_id(),
    tx_versions: TxVersions::of(&[1, 2]),
    script_flags: SCRIPT_FLAGS,
    pools: ShieldedPools {
        sprout: true,
        sapling: false,
        orchard: false,
        ironwood: false,
    },
    sprout_deposit: true,
    limits: BlockLimits::PRE_NU7,
    history: HistoryVersion::None,
    coinbase: CoinbaseRules {
        expiry_is_height: false,
        shielded_outputs: false,
        exact_value: false,
        orchard_bundle: false,
        nsm_fee_share: false,
    },
    difficulty: DifficultyParams::PRE_BLOSSOM,
};

const OVERWINTER: RuleSet = RuleSet {
    upgrade: Upgrade::Overwinter,
    branch_id: Upgrade::Overwinter.branch_id(),
    tx_versions: TxVersions::of(&[3]),
    ..SPROUT
};

const SAPLING: RuleSet = RuleSet {
    upgrade: Upgrade::Sapling,
    branch_id: Upgrade::Sapling.branch_id(),
    tx_versions: TxVersions::of(&[4]),
    pools: ShieldedPools {
        sapling: true,
        ..OVERWINTER.pools
    },
    ..OVERWINTER
};

const BLOSSOM: RuleSet = RuleSet {
    upgrade: Upgrade::Blossom,
    branch_id: Upgrade::Blossom.branch_id(),
    difficulty: DifficultyParams::POST_BLOSSOM,
    ..SAPLING
};

const HEARTWOOD: RuleSet = RuleSet {
    upgrade: Upgrade::Heartwood,
    branch_id: Upgrade::Heartwood.branch_id(),
    history: HistoryVersion::V1,
    coinbase: CoinbaseRules {
        shielded_outputs: true,
        ..BLOSSOM.coinbase
    },
    ..BLOSSOM
};

const CANOPY: RuleSet = RuleSet {
    upgrade: Upgrade::Canopy,
    branch_id: Upgrade::Canopy.branch_id(),
    sprout_deposit: false,
    ..HEARTWOOD
};

const NU5: RuleSet = RuleSet {
    upgrade: Upgrade::Nu5,
    branch_id: Upgrade::Nu5.branch_id(),
    // ZIP 252: an NU5 node accepts the v4 and the v5 transaction formats.
    tx_versions: TxVersions::of(&[4, 5]),
    pools: ShieldedPools {
        orchard: true,
        ..CANOPY.pools
    },
    history: HistoryVersion::V2,
    coinbase: CoinbaseRules {
        expiry_is_height: true,
        orchard_bundle: true,
        ..CANOPY.coinbase
    },
    ..CANOPY
};

const NU6: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6,
    branch_id: Upgrade::Nu6.branch_id(),
    coinbase: CoinbaseRules {
        exact_value: true,
        ..NU5.coinbase
    },
    ..NU5
};

const NU6_1: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6_1,
    branch_id: Upgrade::Nu6_1.branch_id(),
    ..NU6
};

/// NU6.1 from the Orchard soft fork until the NU6.2 activation
/// ([`CoreSpec::orchard_disabled`]): no transaction has an Orchard bundle. The branch id
/// and every other rule are those of NU6.1.
pub const NU6_1_ORCHARD_DISABLED: RuleSet = RuleSet {
    pools: ShieldedPools {
        orchard: false,
        ..NU6_1.pools
    },
    coinbase: CoinbaseRules {
        orchard_bundle: false,
        ..NU6_1.coinbase
    },
    ..NU6_1
};

const NU6_2: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6_2,
    branch_id: Upgrade::Nu6_2.branch_id(),
    ..NU6_1
};

const NU6_3: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6_3,
    branch_id: Upgrade::Nu6_3.branch_id(),
    tx_versions: TxVersions::of(&[4, 5, 6]),
    pools: ShieldedPools {
        ironwood: true,
        ..NU6_2.pools
    },
    history: HistoryVersion::V3,
    coinbase: CoinbaseRules {
        orchard_bundle: false,
        ..NU6_2.coinbase
    },
    ..NU6_2
};

/// The NU7 rule set.
///
/// - ZIP 2003: the transaction version is 5 or 6 (Zakura
///   `zakura-consensus/src/transaction.rs:1023-1040`). No transaction has a JoinSplit, so
///   the Sprout pool is off (ZIP 218, `SproutBlockJoinSplitLimit` of 0).
/// - ZIP 218: the block limits and the difficulty parameters.
/// - The NU7 deployment draft: the fee share of the miner.
/// - The history tree, the script flags and every other rule are those of NU6.3 (Zakura
///   `zakura-chain/src/history_tree.rs:142,241`, `zakura-consensus/src/primitives/
///   halo2.rs:405`).
const NU7: RuleSet = RuleSet {
    upgrade: Upgrade::Nu7,
    branch_id: Upgrade::Nu7.branch_id(),
    tx_versions: TxVersions::of(&[5, 6]),
    pools: ShieldedPools {
        sprout: false,
        ..NU6_3.pools
    },
    limits: BlockLimits::NU7,
    coinbase: CoinbaseRules {
        nsm_fee_share: true,
        ..NU6_3.coinbase
    },
    difficulty: DifficultyParams::POST_NU7,
    ..NU6_3
};

/// The rule sets in activation order, one for each upgrade, at [`Upgrade::index`].
/// [`NU6_1_ORCHARD_DISABLED`] is not in the table: only [`rules_at`] selects it, because
/// it depends on the height and not only on the upgrade.
pub const RULE_SETS: [RuleSet; UPGRADES] = [
    SPROUT, OVERWINTER, SAPLING, BLOSSOM, HEARTWOOD, CANOPY, NU5, NU6, NU6_1, NU6_2, NU6_3, NU7,
];

impl RuleSet {
    /// The rule set of `upgrade` at its activation.
    pub fn of(upgrade: Upgrade) -> &'static RuleSet {
        &RULE_SETS[upgrade.index()]
    }
}

/// The rule set of the block at `height` on the chain of `spec`.
///
/// A rule that depends on the height inside one upgrade is a rule set of its own: from the
/// Orchard soft fork until the NU6.2 activation the result is the NU6.1 rule set with the
/// Orchard pool off. A caller that checks a block must take the rule set from this
/// function, not from the branch id of the block. The soft fork outside NU6.1 is
/// [`ConsensusError::UncheckedSpec`]: [`CoreSpec::checked`] refuses such a spec.
///
/// ZIP 200: a block of a known height is validated under the rules of the consensus
/// branch of that height. The block at `ACTIVATION_HEIGHT - 1` has the rules before the
/// upgrade.
pub fn rules_at(spec: &CoreSpec, height: u32) -> Result<RuleSet, ConsensusError> {
    let upgrade = spec.upgrade_at(height)?;
    if spec.orchard_disabled(height) {
        if upgrade != Upgrade::Nu6_1 {
            return Err(ConsensusError::UncheckedSpec);
        }
        return Ok(NU6_1_ORCHARD_DISABLED);
    }
    Ok(RULE_SETS[upgrade.index()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;

    #[test]
    fn each_rule_set_is_at_the_index_of_its_upgrade() {
        for i in 0..UPGRADES {
            let rules = &RULE_SETS[i];
            assert_eq!(rules.upgrade, Upgrade::ALL[i]);
            assert_eq!(rules.branch_id, Upgrade::ALL[i].branch_id());
            assert_eq!(RuleSet::of(Upgrade::ALL[i]), rules);
            assert_eq!(rules.script_flags, SCRIPT_FLAGS);
            let limits = match rules.upgrade {
                Upgrade::Nu7 => BlockLimits::NU7,
                _ => BlockLimits::PRE_NU7,
            };
            assert_eq!(rules.limits, limits);
        }
    }

    #[test]
    fn tx_versions_hold_the_versions_one_to_six() {
        let versions = |set: TxVersions| -> Vec<u32> {
            let mut allowed = Vec::new();
            for v in 0..=8 {
                if set.allows(v) {
                    allowed.push(v);
                }
            }
            allowed
        };
        assert_eq!(versions(TxVersions::of(&[1, 2])), [1, 2]);
        assert_eq!(versions(TxVersions::of(&[4, 5, 6])), [4, 5, 6]);
        // A version outside 1..=6 is not a transaction version: the set does not hold it.
        assert_eq!(versions(TxVersions::of(&[0, 7, 5])), [5]);
    }

    /// The rules that NU7 changes, against the NU6.3 rule set. The values are those of
    /// Zakura (`zakura-chain/src/parameters/network_upgrade.rs:257-336`).
    #[test]
    fn the_nu7_rule_set_changes_these_rules() {
        let rules = NU7;
        for v in 0..=7 {
            assert_eq!(rules.tx_versions.allows(v), v == 5 || v == 6, "{v}");
        }
        assert!(!rules.pools.sprout);
        assert!(rules.pools.sapling && rules.pools.orchard && rules.pools.ironwood);
        assert_eq!(
            rules.limits,
            BlockLimits {
                sigops: 20_000,
                orchard_actions: 330,
                ironwood_actions: 330,
                sapling_ios: 300,
                shielded_cost: 330,
            }
        );
        assert!(rules.coinbase.nsm_fee_share);
        assert_eq!(
            rules.difficulty,
            DifficultyParams {
                target_spacing: 25,
                averaging_window: 102,
                max_adjust_up_percent: 16,
                max_adjust_down_percent: 32,
                damping_factor: 4,
                min_difficulty_gap_spacings: 18,
            }
        );
        // Every other rule is the NU6.3 rule.
        assert_eq!(
            RuleSet {
                upgrade: NU6_3.upgrade,
                branch_id: NU6_3.branch_id,
                tx_versions: NU6_3.tx_versions,
                pools: NU6_3.pools,
                limits: NU6_3.limits,
                coinbase: NU6_3.coinbase,
                difficulty: NU6_3.difficulty,
                ..rules
            },
            NU6_3
        );
        assert_eq!(
            CoinbaseRules {
                nsm_fee_share: false,
                ..rules.coinbase
            },
            NU6_3.coinbase
        );
    }

    #[test]
    fn the_rules_of_each_upgrade() {
        let versions = |upgrade: Upgrade| -> Vec<u32> {
            let rules = RuleSet::of(upgrade);
            let mut allowed = Vec::new();
            for v in 0..=7 {
                if rules.tx_versions.allows(v) {
                    allowed.push(v);
                }
            }
            allowed
        };
        assert_eq!(versions(Upgrade::Sprout), [1, 2]);
        assert_eq!(versions(Upgrade::Overwinter), [3]);
        for upgrade in [
            Upgrade::Sapling,
            Upgrade::Blossom,
            Upgrade::Heartwood,
            Upgrade::Canopy,
        ] {
            assert_eq!(versions(upgrade), [4]);
        }
        for upgrade in [Upgrade::Nu5, Upgrade::Nu6, Upgrade::Nu6_1, Upgrade::Nu6_2] {
            assert_eq!(versions(upgrade), [4, 5]);
        }
        assert_eq!(versions(Upgrade::Nu6_3), [4, 5, 6]);
        assert_eq!(versions(Upgrade::Nu7), [5, 6]);

        for rules in RULE_SETS {
            let u = rules.upgrade;
            assert_eq!(rules.pools.sprout, u < Upgrade::Nu7);
            assert_eq!(rules.coinbase.nsm_fee_share, u >= Upgrade::Nu7);
            assert_eq!(rules.sprout_deposit, u < Upgrade::Canopy);
            assert_eq!(rules.pools.sapling, u >= Upgrade::Sapling);
            assert_eq!(rules.pools.orchard, u >= Upgrade::Nu5);
            assert_eq!(rules.pools.ironwood, u >= Upgrade::Nu6_3);
            let history = match u {
                _ if u < Upgrade::Heartwood => HistoryVersion::None,
                _ if u < Upgrade::Nu5 => HistoryVersion::V1,
                _ if u < Upgrade::Nu6_3 => HistoryVersion::V2,
                _ => HistoryVersion::V3,
            };
            assert_eq!(rules.history, history);
            assert_eq!(rules.coinbase.expiry_is_height, u >= Upgrade::Nu5);
            assert_eq!(rules.coinbase.shielded_outputs, u >= Upgrade::Heartwood);
            assert_eq!(rules.coinbase.exact_value, u >= Upgrade::Nu6);
            assert_eq!(
                rules.coinbase.orchard_bundle,
                u >= Upgrade::Nu5 && u < Upgrade::Nu6_3
            );
            let (spacing, window, gap) = match u {
                _ if u < Upgrade::Blossom => (150, 17, 6),
                _ if u < Upgrade::Nu7 => (75, 17, 6),
                _ => (25, 102, 18),
            };
            assert_eq!(rules.difficulty.target_spacing, spacing);
            assert_eq!(rules.difficulty.averaging_window, window);
            assert_eq!(rules.difficulty.min_difficulty_gap_spacings, gap);
        }
        // The minimum-difficulty gap is 450 s in both eras from Blossom.
        assert_eq!(6 * 75, 18 * 25);
    }

    /// The largest averaging window of the rule sets is the NU7 window of the context
    /// constant.
    #[test]
    fn the_largest_window_is_the_nu7_window() {
        let mut largest = 0;
        for rules in RULE_SETS {
            largest = largest.max(rules.difficulty.averaging_window);
        }
        assert_eq!(
            usize::try_from(largest),
            Ok(crate::LARGEST_AVERAGING_WINDOW)
        );
    }

    /// The Orchard soft fork: `rules_at` gives the NU6.1 rule set with the Orchard pool off
    /// from the start height until the block before NU6.2, and the plain rule sets on both
    /// sides of that range. Outside NU6.1 the soft fork is an error.
    #[test]
    fn the_orchard_pool_is_off_in_the_soft_fork_range() {
        let mut spec = regtest();
        spec.activation_heights[Upgrade::Nu6.index()] = Some(10);
        spec.activation_heights[Upgrade::Nu6_1.index()] = Some(20);
        spec.activation_heights[Upgrade::Nu6_2.index()] = Some(40);
        spec.orchard_disabled_start_height = Some(30);
        assert_eq!(rules_at(&spec, 0), Ok(SPROUT));
        assert_eq!(rules_at(&spec, 1), Ok(NU5));
        assert_eq!(rules_at(&spec, 10), Ok(NU6));
        assert_eq!(rules_at(&spec, 29), Ok(NU6_1));
        for height in [30, 31, 39] {
            let rules = rules_at(&spec, height).unwrap();
            assert_eq!(rules, NU6_1_ORCHARD_DISABLED, "{height}");
            assert!(!rules.pools.orchard && !rules.coinbase.orchard_bundle);
            assert_eq!(
                RuleSet {
                    pools: NU6_1.pools,
                    coinbase: NU6_1.coinbase,
                    ..rules
                },
                NU6_1
            );
        }
        assert_eq!(rules_at(&spec, 40), Ok(NU6_2));
        assert_eq!(rules_at(&spec, u32::MAX), Ok(NU6_2));
        spec.orchard_disabled_start_height = Some(15);
        assert_eq!(rules_at(&spec, 15), Err(ConsensusError::UncheckedSpec));
        assert_eq!(rules_at(&regtest(), u32::MAX), Ok(NU5));
    }
}
