//! The founders' reward (protocol specification §7.9, before Canopy).
//!
//! A coinbase at a height from 1 to the last height before the first halving pays 20 % of
//! the block subsidy to the founders' address of the height. The rule ends at Canopy.
//!
//! Zakura and Zebra check the output in every block that the full verifier receives
//! (`zakura-consensus/src/block/check.rs:204-224`). Their mandatory checkpoint is the last
//! block before Canopy on Mainnet and Testnet, so there the rule runs only on a network
//! with a configured checkpoint list.

use crate::{
    subsidy_schedule, ConsensusError, CoreSpec, P2shScript, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    PRE_BLOSSOM_TARGET_SPACING,
};

/// The founders' reward of one height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoundersReward {
    /// `FoundersReward(height)` in zatoshis.
    pub value: u64,
    /// The P2SH script of `FounderRedeemScriptHash(height)`.
    pub script: P2shScript,
}

/// `FoundersFraction` is 1 / 5.
///
/// Spec §7.8: `FoundersReward(height) = BlockSubsidy(height) * FoundersFraction` while
/// `Halving(height) < 1`.
const FOUNDERS_FRACTION_DIVISOR: u64 = 5;

/// The founders' reward that the coinbase at `height` must pay. `None` when the rule does
/// not apply: the genesis block, a height at or after Canopy, a height at or after the
/// first halving, and every height of a chain without founders' scripts
/// ([`CoreSpec::founders_scripts`]).
///
/// Spec §7.9: [Pre-Canopy] a coinbase at a height from 1 to
/// `FoundersRewardLastBlockHeight` pays `FoundersReward(height)` to the P2SH script of
/// `FounderRedeemScriptHash(height)`. ZIP 207: the rule ends at Canopy, also on Testnet.
pub fn founders_reward(
    spec: &CoreSpec,
    height: u32,
) -> Result<Option<FoundersReward>, ConsensusError> {
    let scripts = &spec.founders_scripts;
    if scripts.len() == 0 {
        return Ok(None);
    }
    if height == 0
        || spec.upgrade_at(height)? >= Upgrade::Canopy
        || subsidy_schedule::halving(spec, height)? >= 1
    {
        return Ok(None);
    }
    // Spec §7.9: FounderAddressChangeInterval = ceiling((SlowStartShift +
    // PreBlossomHalvingInterval) / NumFounderAddresses).
    let Ok(count) = u32::try_from(scripts.len()) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(span) = (spec.slow_start_interval / 2).checked_add(spec.pre_blossom_halving_interval)
    else {
        return Err(ConsensusError::Overflow);
    };
    let Some(rounding) = count.checked_sub(1) else {
        return Err(ConsensusError::Overflow);
    };
    let Some(span_up) = span.checked_add(rounding) else {
        return Err(ConsensusError::Overflow);
    };
    let change_interval = span_up / count;
    // Spec §7.9: FounderAddressAdjustedHeight. A block from Blossom counts as the part of
    // a pre-Blossom block that its target spacing is.
    let adjusted_height = match spec.activation_height(Upgrade::Blossom) {
        Some(blossom) if height >= blossom => {
            let ratio = PRE_BLOSSOM_TARGET_SPACING / POST_BLOSSOM_TARGET_SPACING;
            let Some(adjusted) = blossom.checked_add((height - blossom) / ratio) else {
                return Err(ConsensusError::Overflow);
            };
            adjusted
        }
        _ => height,
    };
    let Some(index) = adjusted_height.checked_div(change_interval) else {
        return Err(ConsensusError::DivisionByZero);
    };
    let Ok(index) = usize::try_from(index) else {
        return Err(ConsensusError::Overflow);
    };
    // Spec §7.9: FounderAddressIndex, from 0 here. The index is below the count at every
    // height before the first halving.
    let Some(script) = scripts.get(index) else {
        return Err(ConsensusError::UncheckedSpec);
    };
    Ok(Some(FoundersReward {
        value: subsidy_schedule::scheduled_subsidy(spec, height)? / FOUNDERS_FRACTION_DIVISOR,
        script: *script,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain_spec::tests::{heights, regtest};
    use crate::funding::tests::{SCRIPT_A, SCRIPT_B};

    /// A chain with the schedule of Mainnet and 2 founders' scripts: the change interval is
    /// `ceiling((10,000 + 840,000) / 2)` = 425,000 adjusted blocks.
    fn mainnet_like() -> CoreSpec {
        let mut spec = regtest();
        spec.activation_heights = heights(&[
            (Upgrade::Overwinter, 347_500),
            (Upgrade::Sapling, 419_200),
            (Upgrade::Blossom, 653_600),
            (Upgrade::Heartwood, 903_000),
            (Upgrade::Canopy, 1_046_400),
        ]);
        spec.slow_start_interval = 20_000;
        spec.pre_blossom_halving_interval = 840_000;
        spec.founders_scripts = vec![SCRIPT_A, SCRIPT_B];
        spec.first_halving = Some(1_046_400);
        spec
    }

    #[test]
    fn the_reward_is_a_fifth_of_the_subsidy_until_canopy() {
        let spec = mainnet_like();
        let value = |height| {
            founders_reward(&spec, height)
                .unwrap()
                .map(|reward| reward.value)
        };
        assert_eq!(value(0), None);
        assert_eq!(value(1), Some(12_500));
        assert_eq!(value(20_000), Some(250_000_000));
        assert_eq!(value(653_599), Some(250_000_000));
        assert_eq!(value(653_600), Some(125_000_000));
        assert_eq!(value(1_046_399), Some(125_000_000));
        assert_eq!(value(1_046_400), None);
        assert_eq!(founders_reward(&regtest(), 1), Ok(None));
    }

    #[test]
    fn the_script_changes_at_the_adjusted_interval() {
        let spec = mainnet_like();
        let script = |height| founders_reward(&spec, height).unwrap().unwrap().script;
        assert_eq!(script(1), SCRIPT_A);
        assert_eq!(script(424_999), SCRIPT_A);
        assert_eq!(script(425_000), SCRIPT_B);
        // From Blossom two blocks are one adjusted block: the second script stays.
        assert_eq!(script(1_046_399), SCRIPT_B);
        // Canopy before the first halving ends the rule.
        let mut early_canopy = spec.clone();
        early_canopy.activation_heights[Upgrade::Canopy.index()] = Some(700_000);
        assert_eq!(founders_reward(&early_canopy, 700_000), Ok(None));
        assert_eq!(
            founders_reward(&early_canopy, 699_999).map(|r| r.map(|r| r.value)),
            Ok(Some(125_000_000))
        );
    }
}
