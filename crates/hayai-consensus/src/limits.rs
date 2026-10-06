//! Per-block totals.

/// The limits on the totals of one block. Shielded limits exist from NU7 (ZIP 218).
/// Earlier upgrades use [`BlockLimits::PRE_NU7`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockLimits {
    pub sigops: u32,
    /// `OrchardProtocolBlockActionLimit` for the Orchard pool.
    pub orchard_actions: u32,
    /// Ironwood actions of a block. ZIP 218 gives the Ironwood pool the limit of the
    /// Orchard pool.
    pub ironwood_actions: u32,
    /// `SaplingBlockIOLimit`: Sapling spends plus outputs.
    pub sapling_ios: u32,
    /// `GlobalShieldedBudget`: Orchard actions plus Ironwood actions plus Sapling spends
    /// and outputs. ZIP 218 adds two units for each JoinSplit. A rule set with this limit
    /// has no Sprout pool, so a block has no JoinSplit.
    pub shielded_cost: u32,
}

impl BlockLimits {
    pub const PRE_NU7: Self = Self {
        sigops: 20_000,
        orchard_actions: u32::MAX,
        ironwood_actions: u32::MAX,
        sapling_ios: u32::MAX,
        shielded_cost: u32::MAX,
    };
    /// Zakura `zakura-chain/src/parameters/network_upgrade.rs:301-327`.
    ///
    /// ZIP 218: `OrchardProtocolBlockActionLimit` 330 for Orchard and for Ironwood,
    /// `SaplingBlockIOLimit` 300, `GlobalShieldedBudget` 330. `SproutBlockJoinSplitLimit`
    /// 0 is the Sprout pool that the NU7 rule set turns off.
    pub const NU7: Self = Self {
        sigops: 20_000,
        orchard_actions: 330,
        ironwood_actions: 330,
        sapling_ios: 300,
        shielded_cost: 330,
    };
}
