//! One rule set per network upgrade and the selection of the rule set of a height.
//!
//! A new upgrade is one more entry of the rule set table. An upgrade without an entry makes
//! [`rules_at`] fail at every height at which the upgrade is active. NU7 has an entry when
//! the crypto backend has the NU7 branch id (`hayai_crypto::nu7_branch`).

use std::sync::LazyLock;

use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_crypto::zcash_script::interpreter::Flags;

use crate::{
    BlockLimits, ConsensusError, Network, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    POST_NU7_TARGET_SPACING, PRE_BLOSSOM_TARGET_SPACING,
};

/// The transaction versions that a block of one upgrade can hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxVersions(u8);

impl TxVersions {
    /// The set of `versions`. Each version is in 1..=6.
    pub const fn of(versions: &[u32]) -> Self {
        let mut mask = 0u8;
        let mut i = 0;
        while i < versions.len() {
            assert!(versions[i] >= 1 && versions[i] <= 6);
            mask |= 1 << versions[i];
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
    /// Testnet minimum-difficulty rule (ZIP 205, ZIP 208): a block whose time is more than
    /// this number of target spacings after its parent can use the proof-of-work limit.
    /// The gap is 450 s from Blossom: 6 spacings of 75 s, and 18 spacings of 25 s from NU7.
    pub min_difficulty_gap_spacings: u32,
}

impl DifficultyParams {
    pub const PRE_BLOSSOM: Self = Self {
        target_spacing: PRE_BLOSSOM_TARGET_SPACING,
        averaging_window: 17,
        max_adjust_up_percent: 16,
        max_adjust_down_percent: 32,
        damping_factor: 4,
        min_difficulty_gap_spacings: 6,
    };
    pub const POST_BLOSSOM: Self = Self {
        target_spacing: POST_BLOSSOM_TARGET_SPACING,
        ..Self::PRE_BLOSSOM
    };
    /// ZIP 218: `PostNU7PoWTargetSpacing`, `PostNU7PoWAveragingWindow` (Zakura
    /// `zakura-chain/src/parameters/network_upgrade.rs:257,285,336`).
    pub const POST_NU7: Self = Self {
        target_spacing: POST_NU7_TARGET_SPACING,
        averaging_window: 102,
        min_difficulty_gap_spacings: 18,
        ..Self::PRE_BLOSSOM
    };
}

/// The rules of one network upgrade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleSet {
    pub upgrade: Upgrade,
    /// The consensus branch id of transactions and of the history tree.
    pub branch_id: BranchId,
    pub tx_versions: TxVersions,
    /// The script verification flags of block validation.
    pub script_flags: Flags,
    pub pools: ShieldedPools,
    /// ZIP 211: until Heartwood a JoinSplit can move value into the Sprout pool. From
    /// Canopy `vpub_old` of every JoinSplit is zero.
    pub sprout_deposit: bool,
    pub limits: BlockLimits,
    pub history: HistoryVersion,
    pub coinbase: CoinbaseRules,
    pub difficulty: DifficultyParams,
}

/// The flags zcashd applies to every transaction of a block (`ConnectBlock`:
/// `SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY`), which Zakura's verifier also
/// uses (`zakura-script/src/lib.rs:173-174`).
const SCRIPT_FLAGS: Flags = Flags::P2SH.union(Flags::CHECKLOCKTIMEVERIFY);

const SPROUT: RuleSet = RuleSet {
    upgrade: Upgrade::Sprout,
    branch_id: BranchId::Sprout,
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
    branch_id: BranchId::Overwinter,
    tx_versions: TxVersions::of(&[3]),
    ..SPROUT
};

const SAPLING: RuleSet = RuleSet {
    upgrade: Upgrade::Sapling,
    branch_id: BranchId::Sapling,
    tx_versions: TxVersions::of(&[4]),
    pools: ShieldedPools {
        sapling: true,
        ..OVERWINTER.pools
    },
    ..OVERWINTER
};

const BLOSSOM: RuleSet = RuleSet {
    upgrade: Upgrade::Blossom,
    branch_id: BranchId::Blossom,
    difficulty: DifficultyParams::POST_BLOSSOM,
    ..SAPLING
};

const HEARTWOOD: RuleSet = RuleSet {
    upgrade: Upgrade::Heartwood,
    branch_id: BranchId::Heartwood,
    history: HistoryVersion::V1,
    coinbase: CoinbaseRules {
        shielded_outputs: true,
        ..BLOSSOM.coinbase
    },
    ..BLOSSOM
};

const CANOPY: RuleSet = RuleSet {
    upgrade: Upgrade::Canopy,
    branch_id: BranchId::Canopy,
    sprout_deposit: false,
    ..HEARTWOOD
};

const NU5: RuleSet = RuleSet {
    upgrade: Upgrade::Nu5,
    branch_id: BranchId::Nu5,
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
    branch_id: BranchId::Nu6,
    coinbase: CoinbaseRules {
        exact_value: true,
        ..NU5.coinbase
    },
    ..NU5
};

const NU6_1: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6_1,
    branch_id: BranchId::Nu6_1,
    ..NU6
};

/// NU6.1 from the Orchard soft fork until the NU6.2 activation
/// ([`Network::orchard_disabled`]): no transaction has an Orchard bundle. The branch id
/// and every other rule are those of NU6.1.
const NU6_1_ORCHARD_DISABLED: RuleSet = RuleSet {
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
    branch_id: BranchId::Nu6_2,
    ..NU6_1
};

const NU6_3: RuleSet = RuleSet {
    upgrade: Upgrade::Nu6_3,
    branch_id: BranchId::Nu6_3,
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

/// The NU7 rule set with the branch id `branch_id`.
///
/// - ZIP 2003: the transaction version is 5 or 6 (Zakura
///   `zakura-consensus/src/transaction.rs:1023-1040`). No transaction has a JoinSplit, so
///   the Sprout pool is off (ZIP 218, `SproutBlockJoinSplitLimit` of 0).
/// - ZIP 218: the block limits and the difficulty parameters.
/// - The NU7 deployment draft: the fee share of the miner.
/// - The history tree, the script flags and every other rule are those of NU6.3 (Zakura
///   `zakura-chain/src/history_tree.rs:142,241`, `zakura-consensus/src/primitives/
///   halo2.rs:405`).
const fn nu7(branch_id: BranchId) -> RuleSet {
    RuleSet {
        upgrade: Upgrade::Nu7,
        branch_id,
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
    }
}

/// The rule sets in activation order, one for each upgrade. An upgrade without an entry is
/// not supported. [`NU6_1_ORCHARD_DISABLED`] is not in the table: only [`rules_at`] selects
/// it, because it depends on the height and not only on the upgrade. The NU7 rule set is
/// the last entry when the crypto backend has the NU7 branch id.
static RULE_SETS: LazyLock<Vec<RuleSet>> = LazyLock::new(|| {
    let mut sets = vec![
        SPROUT, OVERWINTER, SAPLING, BLOSSOM, HEARTWOOD, CANOPY, NU5, NU6, NU6_1, NU6_2, NU6_3,
    ];
    sets.extend(hayai_crypto::nu7_branch().map(nu7));
    sets
});

impl RuleSet {
    /// The rule set of `upgrade` at its activation. `None` when this build has no rule set
    /// for it.
    pub fn of(upgrade: Upgrade) -> Option<&'static RuleSet> {
        RULE_SETS.iter().find(|rules| rules.upgrade == upgrade)
    }

    /// The rule set of the upgrade of `branch`.
    pub fn of_branch(branch: BranchId) -> Result<&'static RuleSet, ConsensusError> {
        let upgrade = Upgrade::of_branch(branch)?;
        Self::of(upgrade).ok_or(ConsensusError::NoRuleSet(upgrade))
    }
}

/// The rule set of the block at `height` on `network`.
///
/// It fails with [`ConsensusError::UnsupportedUpgrade`] when the upgrade that is active at
/// `height` has no rule set. The rule set of an earlier upgrade is never returned for such
/// a height.
///
/// A rule that depends on the height inside one upgrade is a rule set of its own: from the
/// Orchard soft fork until the NU6.2 activation the result is the NU6.1 rule set with the
/// Orchard pool off. A caller that checks a block must take the rule set from this
/// function, not from the branch id of the block.
pub fn rules_at(network: Network, height: u32) -> Result<&'static RuleSet, ConsensusError> {
    let upgrade = network.upgrade_at(height);
    if network.orchard_disabled(height) {
        // The soft fork starts and ends in NU6.1 on every network that has it (the test
        // `the_orchard_pool_is_off_from_the_soft_fork_until_nu6_2`).
        assert_eq!(
            upgrade,
            Upgrade::Nu6_1,
            "the Orchard soft fork at height {height} is outside NU6.1"
        );
        return Ok(&NU6_1_ORCHARD_DISABLED);
    }
    RuleSet::of(upgrade).ok_or(ConsensusError::UnsupportedUpgrade { upgrade, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each upgrade has one rule set with its branch id. NU7 has one when the crypto
    /// backend has the NU7 branch id, and none on the other backend.
    #[test]
    fn every_upgrade_with_a_branch_id_has_one_rule_set_with_its_branch() {
        for upgrade in Upgrade::ALL {
            match (upgrade.branch_id(), RuleSet::of(upgrade)) {
                (None, None) => assert_eq!(upgrade, Upgrade::Nu7),
                (Some(branch), Some(rules)) => {
                    assert_eq!(rules.upgrade, upgrade);
                    assert_eq!(rules.branch_id, branch);
                    assert_eq!(RuleSet::of_branch(branch), Ok(rules));
                    let limits = match upgrade {
                        Upgrade::Nu7 => BlockLimits::NU7,
                        _ => BlockLimits::PRE_NU7,
                    };
                    assert_eq!(rules.limits, limits);
                    assert_eq!(rules.script_flags, SCRIPT_FLAGS);
                }
                (branch, rules) => panic!("{upgrade:?}: {branch:?} {rules:?}"),
            }
        }
        assert_eq!(
            RuleSet::of(Upgrade::Nu7).map(|rules| rules.upgrade),
            (hayai_crypto::BACKEND == "zakura").then_some(Upgrade::Nu7)
        );
    }

    #[test]
    fn the_rule_set_changes_at_every_activation_height() {
        for network in [Network::Mainnet, Network::Testnet] {
            assert_eq!(rules_at(network, 0), Ok(&SPROUT));
            for rules in &RULE_SETS[1..] {
                let Some(height) = network.activation_height(rules.upgrade) else {
                    assert_eq!((network, rules.upgrade), (Network::Mainnet, Upgrade::Nu7));
                    continue;
                };
                assert_eq!(rules_at(network, height), Ok(rules));
                let before = rules_at(network, height - 1).expect("an earlier rule set");
                assert!(before.upgrade < rules.upgrade);
                let earlier = RULE_SETS.iter().rfind(|r| r.upgrade < rules.upgrade);
                // The block before NU6.2 is the last block of the Orchard soft fork.
                match rules.upgrade {
                    Upgrade::Nu6_2 => {
                        assert_eq!(before, &NU6_1_ORCHARD_DISABLED);
                        assert_eq!(earlier, Some(&NU6_1));
                    }
                    _ => assert_eq!(Some(before), earlier),
                }
            }
        }
        // Regtest: Sprout at the genesis block, NU5 from height 1.
        assert_eq!(rules_at(Network::Regtest, 0), Ok(&SPROUT));
        assert_eq!(rules_at(Network::Regtest, 1), Ok(&NU5));
        assert_eq!(rules_at(Network::Regtest, u32::MAX), Ok(&NU5));
    }

    /// The NU7 boundary on Testnet, 4,465,026 (Zakura
    /// `zakura-chain/src/parameters/constants.rs:80`). With the NU7 branch id, the rule
    /// set changes there. Without it, `rules_at` refuses each height from there. Mainnet
    /// and Regtest have no NU7 height.
    #[test]
    fn the_nu7_boundary_on_testnet() {
        let nu7 = 4_465_026;
        assert_eq!(Network::Testnet.activation_height(Upgrade::Nu7), Some(nu7));
        assert_eq!(rules_at(Network::Testnet, nu7 - 1), Ok(&NU6_3));
        for height in [nu7, nu7 + 1, u32::MAX] {
            match RuleSet::of(Upgrade::Nu7) {
                Some(rules) => assert_eq!(rules_at(Network::Testnet, height), Ok(rules)),
                None => assert_eq!(
                    rules_at(Network::Testnet, height),
                    Err(ConsensusError::UnsupportedUpgrade {
                        upgrade: Upgrade::Nu7,
                        height,
                    })
                ),
            }
        }
        for network in [Network::Mainnet, Network::Regtest] {
            assert_eq!(network.activation_height(Upgrade::Nu7), None);
        }
        assert_eq!(rules_at(Network::Mainnet, u32::MAX), Ok(&NU6_3));
        assert_eq!(rules_at(Network::Regtest, u32::MAX), Ok(&NU5));
        let message = ConsensusError::UnsupportedUpgrade {
            upgrade: Upgrade::Nu7,
            height: nu7,
        }
        .to_string();
        assert!(
            message.contains("Nu7") && message.contains("4465026"),
            "{message}"
        );
    }

    /// The NU7 boundary on a configured Regtest: the same change, or the same refusal.
    #[test]
    fn the_nu7_boundary_on_a_configured_regtest() {
        let config =
            crate::RegtestConfig::new(&[(Upgrade::Nu6_3, 5), (Upgrade::Nu7, 9)], Vec::new(), 0);
        let network = config.expect("a valid configuration").network();
        assert_eq!(network.activation_height(Upgrade::Nu7), Some(9));
        assert_eq!(network.upgrade_at(8), Upgrade::Nu6_3);
        assert_eq!(network.upgrade_at(9), Upgrade::Nu7);
        assert_eq!(rules_at(network, 8), Ok(&NU6_3));
        for height in [9, 10] {
            match RuleSet::of(Upgrade::Nu7) {
                Some(rules) => assert_eq!(rules_at(network, height), Ok(rules)),
                None => assert_eq!(
                    rules_at(network, height),
                    Err(ConsensusError::UnsupportedUpgrade {
                        upgrade: Upgrade::Nu7,
                        height,
                    })
                ),
            }
        }
    }

    /// The rules that NU7 changes, against the NU6.3 rule set. The values are those of
    /// Zakura (`zakura-chain/src/parameters/network_upgrade.rs:257-336`).
    #[test]
    fn the_nu7_rule_set_changes_these_rules() {
        let rules = nu7(BranchId::Nu6_3);
        assert_eq!(
            (0..=7)
                .filter(|v| rules.tx_versions.allows(*v))
                .collect::<Vec<u32>>(),
            [5, 6]
        );
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

    /// T10, the Orchard soft fork: `rules_at` gives the NU6.1 rule set with the Orchard pool
    /// off from the start height until the block before NU6.2, and the plain rule sets on
    /// both sides of that range.
    #[test]
    fn the_orchard_pool_is_off_in_the_soft_fork_range() {
        for (network, start) in [(Network::Mainnet, 3_363_426), (Network::Testnet, 4_048_500)] {
            let Some(nu6_2) = network.activation_height(Upgrade::Nu6_2) else {
                panic!("NU6.2 has a height on {network:?}");
            };
            assert_eq!(rules_at(network, start - 1), Ok(&NU6_1));
            for height in [start, start + 1, nu6_2 - 1] {
                let rules = rules_at(network, height).expect("a rule set");
                assert_eq!(rules, &NU6_1_ORCHARD_DISABLED, "{network:?} {height}");
                assert!(!rules.pools.orchard);
                assert!(!rules.coinbase.orchard_bundle);
                assert!(rules.pools.sapling && rules.pools.sprout && !rules.pools.ironwood);
                // Every other rule is the NU6.1 rule.
                assert_eq!(
                    RuleSet {
                        pools: NU6_1.pools,
                        coinbase: NU6_1.coinbase,
                        ..*rules
                    },
                    NU6_1
                );
            }
            let after = rules_at(network, nu6_2).expect("a rule set");
            assert_eq!(after, &NU6_2);
            assert!(after.pools.orchard);
        }
        // The lookup by upgrade and by branch gives the rule set of the activation.
        assert_eq!(RuleSet::of(Upgrade::Nu6_1), Some(&NU6_1));
        assert_eq!(RuleSet::of_branch(BranchId::Nu6_1), Ok(&NU6_1));
        // Regtest has no soft fork.
        assert_eq!(rules_at(Network::Regtest, 3_363_426), Ok(&NU5));
    }

    #[test]
    fn the_rules_of_each_upgrade() {
        let versions = |upgrade: Upgrade| -> Vec<u32> {
            let rules = RuleSet::of(upgrade).expect("a rule set");
            (0..=7).filter(|v| rules.tx_versions.allows(*v)).collect()
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

        for rules in RULE_SETS.iter() {
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
        // The largest averaging window plus the median time span.
        assert_eq!(crate::DIFFICULTY_CONTEXT_BLOCKS, 113);
    }
}
