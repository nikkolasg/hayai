//! The rule set of each upgrade with the upstream types, and the selection of the rule set
//! of a height.
//!
//! The rules are the table of `hayai_consensus_core::rule_sets`. This module maps each entry to
//! the branch id and the script flags of the crypto backend. An upgrade whose branch id
//! the backend does not know has no entry: NU7 on the upstream backend
//! (`hayai_crypto::nu7_branch`), so [`rules_at`] fails at every height at which NU7 is
//! active.

use std::sync::LazyLock;

use hayai_consensus_core::rule_sets as core;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_crypto::zcash_script::interpreter::Flags;

use crate::{
    branch_id, BlockLimits, CoinbaseRules, ConsensusError, DifficultyParams, HistoryVersion,
    Network, ShieldedPools, TxVersions, Upgrade,
};

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

impl RuleSet {
    /// The rule set of the core with the types of the backend. `None` when the backend has
    /// no branch id for the upgrade.
    fn from_core(rules: &core::RuleSet) -> Option<Self> {
        let branch = branch_id(rules.upgrade)?;
        let Some(script_flags) = Flags::from_bits(rules.script_flags) else {
            unreachable!("the script flags of the core are flags of the interpreter");
        };
        Some(RuleSet {
            upgrade: rules.upgrade,
            branch_id: branch,
            tx_versions: rules.tx_versions,
            script_flags,
            pools: rules.pools,
            sprout_deposit: rules.sprout_deposit,
            limits: rules.limits,
            history: rules.history,
            coinbase: rules.coinbase,
            difficulty: rules.difficulty,
        })
    }

    /// The rule set of `upgrade` at its activation. `None` when this build has no rule set
    /// for it.
    pub fn of(upgrade: Upgrade) -> Option<&'static RuleSet> {
        RULE_SETS[upgrade.index()].as_ref()
    }

    /// The rule set of the upgrade of `branch`.
    pub fn of_branch(branch: BranchId) -> Result<&'static RuleSet, ConsensusError> {
        let upgrade = Upgrade::of_branch(u32::from(branch))?;
        Self::of(upgrade).ok_or(ConsensusError::NoRuleSet(upgrade))
    }
}

/// The slot of the NU6.1 rule set without the Orchard pool, after the slots of the
/// upgrades.
const ORCHARD_DISABLED: usize = core::RULE_SETS.len();

/// The rule sets with the types of the backend: one slot for each upgrade at
/// [`Upgrade::index`], then [`core::NU6_1_ORCHARD_DISABLED`]. A slot is `None` when the
/// backend has no branch id for the upgrade.
static RULE_SETS: LazyLock<[Option<RuleSet>; ORCHARD_DISABLED + 1]> = LazyLock::new(|| {
    let mut sets = [None; ORCHARD_DISABLED + 1];
    for (slot, rules) in core::RULE_SETS.iter().enumerate() {
        sets[slot] = RuleSet::from_core(rules);
    }
    sets[ORCHARD_DISABLED] = RuleSet::from_core(&core::NU6_1_ORCHARD_DISABLED);
    sets
});

/// The rule set of the block at `height` on `network`.
///
/// It fails with [`ConsensusError::UnsupportedUpgrade`] when the upgrade that is active at
/// `height` has no rule set. The rule set of an earlier upgrade is never returned for such
/// a height.
///
/// A rule that depends on the height inside one upgrade is a rule set of its own: from the
/// Orchard soft fork until the NU6.2 activation the result is the NU6.1 rule set with the
/// Orchard pool off (`hayai_consensus_core::rule_sets::rules_at`). A caller that checks a block
/// must take the rule set from this function, not from the branch id of the block.
///
/// ZIP 200: a block of a known height is validated under the rules of the consensus
/// branch of that height. The block at `ACTIVATION_HEIGHT - 1` has the rules before the
/// upgrade.
pub fn rules_at(network: Network, height: u32) -> Result<&'static RuleSet, ConsensusError> {
    let rules = core::rules_at(network.core(), height)?;
    RULE_SETS[slot(&rules)]
        .as_ref()
        .ok_or(ConsensusError::UnsupportedUpgrade {
            upgrade: rules.upgrade,
            height,
        })
}

/// The rule set of the core at `height` on `network`, when this build has a rule set for
/// its upgrade ([`rules_at`]; [`ConsensusError::UnsupportedUpgrade`] otherwise). The
/// wrappers of the core pass it to the rules of the core, so that each path selects the
/// rule set one time.
pub(crate) fn core_rules_at(
    network: Network,
    height: u32,
) -> Result<core::RuleSet, ConsensusError> {
    let rules = core::rules_at(network.core(), height)?;
    match &RULE_SETS[slot(&rules)] {
        Some(_) => Ok(rules),
        None => Err(ConsensusError::UnsupportedUpgrade {
            upgrade: rules.upgrade,
            height,
        }),
    }
}

/// The slot of the rule set of the core `rules` in [`RULE_SETS`].
fn slot(rules: &core::RuleSet) -> usize {
    match (rules.upgrade, rules.pools.orchard) {
        (Upgrade::Nu6_1, false) => ORCHARD_DISABLED,
        (upgrade, _) => upgrade.index(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each upgrade has one rule set with its branch id. NU7 has one when the crypto
    /// backend has the NU7 branch id, and none on the other backend. Each rule set has the
    /// values of the core.
    #[test]
    fn every_upgrade_with_a_branch_id_has_one_rule_set_with_its_branch() {
        for upgrade in Upgrade::ALL {
            let core_rules = core::RuleSet::of(upgrade);
            match (branch_id(upgrade), RuleSet::of(upgrade)) {
                (None, None) => assert_eq!(upgrade, Upgrade::Nu7),
                (Some(branch), Some(rules)) => {
                    assert_eq!(rules.upgrade, upgrade);
                    assert_eq!(rules.branch_id, branch);
                    assert_eq!(u32::from(branch), core_rules.branch_id);
                    assert_eq!(RuleSet::of_branch(branch), Ok(rules));
                    let limits = match upgrade {
                        Upgrade::Nu7 => BlockLimits::NU7,
                        _ => BlockLimits::PRE_NU7,
                    };
                    assert_eq!(rules.limits, limits);
                    assert_eq!(
                        rules.script_flags,
                        Flags::P2SH.union(Flags::CHECKLOCKTIMEVERIFY)
                    );
                    assert_eq!(rules.script_flags.bits(), core_rules.script_flags);
                    assert_eq!(rules.tx_versions, core_rules.tx_versions);
                    assert_eq!(rules.pools, core_rules.pools);
                    assert_eq!(rules.sprout_deposit, core_rules.sprout_deposit);
                    assert_eq!(rules.history, core_rules.history);
                    assert_eq!(rules.coinbase, core_rules.coinbase);
                    assert_eq!(rules.difficulty, core_rules.difficulty);
                }
                (branch, rules) => panic!("{upgrade:?}: {branch:?} {rules:?}"),
            }
        }
        assert_eq!(
            RuleSet::of(Upgrade::Nu7).map(|rules| rules.upgrade),
            (hayai_crypto::BACKEND == "zakura").then_some(Upgrade::Nu7)
        );
        assert_eq!(
            core::SCRIPT_VERIFY_P2SH | core::SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY,
            Flags::P2SH.union(Flags::CHECKLOCKTIMEVERIFY).bits()
        );
    }

    #[test]
    fn the_rule_set_changes_at_every_activation_height() {
        let orchard_disabled = RULE_SETS[ORCHARD_DISABLED].expect("NU6.1 has a branch id");
        for network in [Network::Mainnet, Network::Testnet] {
            assert_eq!(
                rules_at(network, 0),
                RuleSet::of(Upgrade::Sprout)
                    .ok_or(())
                    .map_err(|_| unreachable!())
            );
            for upgrade in &Upgrade::ALL[1..] {
                let Some(rules) = RuleSet::of(*upgrade) else {
                    assert_eq!(*upgrade, Upgrade::Nu7);
                    continue;
                };
                let Some(height) = network.activation_height(*upgrade) else {
                    assert_eq!((network, *upgrade), (Network::Mainnet, Upgrade::Nu7));
                    continue;
                };
                assert_eq!(rules_at(network, height), Ok(rules));
                let before = rules_at(network, height - 1).expect("an earlier rule set");
                assert!(before.upgrade < rules.upgrade);
                let earlier = RuleSet::of(Upgrade::ALL[upgrade.index() - 1]);
                // The block before NU6.2 is the last block of the Orchard soft fork.
                match upgrade {
                    Upgrade::Nu6_2 => {
                        assert_eq!(before, &orchard_disabled);
                        assert_eq!(earlier, RuleSet::of(Upgrade::Nu6_1));
                    }
                    _ => assert_eq!(Some(before), earlier),
                }
            }
        }
        // Regtest: Sprout at the genesis block, NU5 from height 1.
        assert_eq!(
            rules_at(Network::Regtest, 0),
            Ok(RuleSet::of(Upgrade::Sprout).unwrap())
        );
        assert_eq!(
            rules_at(Network::Regtest, 1),
            Ok(RuleSet::of(Upgrade::Nu5).unwrap())
        );
        assert_eq!(
            rules_at(Network::Regtest, u32::MAX),
            Ok(RuleSet::of(Upgrade::Nu5).unwrap())
        );
    }

    /// The NU7 boundary on Testnet, 4,465,026 (Zakura
    /// `zakura-chain/src/parameters/constants.rs:80`). With the NU7 branch id, the rule
    /// set changes there. Without it, `rules_at` refuses each height from there. Mainnet
    /// and Regtest have no NU7 height.
    #[test]
    fn the_nu7_boundary_on_testnet() {
        let nu7 = 4_465_026;
        assert_eq!(Network::Testnet.activation_height(Upgrade::Nu7), Some(nu7));
        assert_eq!(
            rules_at(Network::Testnet, nu7 - 1),
            Ok(RuleSet::of(Upgrade::Nu6_3).unwrap())
        );
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
        assert_eq!(
            rules_at(Network::Mainnet, u32::MAX),
            Ok(RuleSet::of(Upgrade::Nu6_3).unwrap())
        );
        assert_eq!(
            rules_at(Network::Regtest, u32::MAX),
            Ok(RuleSet::of(Upgrade::Nu5).unwrap())
        );
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
        assert_eq!(
            rules_at(network, 8),
            Ok(RuleSet::of(Upgrade::Nu6_3).unwrap())
        );
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

    /// T10, the Orchard soft fork: `rules_at` gives the NU6.1 rule set with the Orchard pool
    /// off from the start height until the block before NU6.2, and the plain rule sets on
    /// both sides of that range.
    #[test]
    fn the_orchard_pool_is_off_in_the_soft_fork_range() {
        let nu6_1 = RuleSet::of(Upgrade::Nu6_1).unwrap();
        let orchard_disabled = RULE_SETS[ORCHARD_DISABLED].expect("NU6.1 has a branch id");
        for (network, start) in [(Network::Mainnet, 3_363_426), (Network::Testnet, 4_048_500)] {
            let Some(nu6_2) = network.activation_height(Upgrade::Nu6_2) else {
                panic!("NU6.2 has a height on {network:?}");
            };
            assert_eq!(rules_at(network, start - 1), Ok(nu6_1));
            for height in [start, start + 1, nu6_2 - 1] {
                let rules = rules_at(network, height).expect("a rule set");
                assert_eq!(rules, &orchard_disabled, "{network:?} {height}");
                assert!(!rules.pools.orchard);
                assert!(!rules.coinbase.orchard_bundle);
                assert!(rules.pools.sapling && rules.pools.sprout && !rules.pools.ironwood);
                // Every other rule is the NU6.1 rule.
                assert_eq!(
                    &RuleSet {
                        pools: nu6_1.pools,
                        coinbase: nu6_1.coinbase,
                        ..*rules
                    },
                    nu6_1
                );
            }
            let after = rules_at(network, nu6_2).expect("a rule set");
            assert_eq!(after, RuleSet::of(Upgrade::Nu6_2).unwrap());
            assert!(after.pools.orchard);
        }
        // The lookup by upgrade and by branch gives the rule set of the activation.
        assert_eq!(RuleSet::of_branch(BranchId::Nu6_1), Ok(nu6_1));
        // Regtest has no soft fork.
        assert_eq!(
            rules_at(Network::Regtest, 3_363_426),
            Ok(RuleSet::of(Upgrade::Nu5).unwrap())
        );
    }
}
