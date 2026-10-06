//! ZIP 317 fee arithmetic: logical actions, conventional fee and block-production weight.
//!
//! The node uses the values of Zakura ([`Zip317Params::ZAKURA`]) in its mempool policy, its
//! store and its template, so that it relays and mines the transactions that a Zakura node
//! relays and mines. Zakura has a marginal fee of 400 zatoshis and a weight ratio cap of 13
//! (`zakura-chain/src/transaction/unmined/zip317.rs:25` `MARGINAL_FEE`, `:28`
//! `GRACE_ACTIONS`, `:37` `BLOCK_PRODUCTION_WEIGHT_RATIO_CAP`). ZIP 317 as published has
//! 5,000 zatoshis and a cap of 4 ([`Zip317Params::ZIP317`]).

use std::io::Write;

use hayai_crypto::zcash_primitives;
use zcash_primitives::transaction::Transaction;

/// ZIP 317 `block_unpaid_action_limit`: the unpaid actions that one block template can
/// hold, and the unpaid actions that the mempool admits in one transaction. The value is
/// 0, the value of Zakura and Zebra (Zakura `zakura-chain/src/transaction/unmined/
/// zip317.rs:48`, with the mempool check at lines 166-175): each transaction pays the
/// marginal fee for each of its logical actions, and the node relays what its peers relay.
/// ZIP 317 gives 50 as the default, which is the default of zcashd `-txunpaidactionlimit`.
pub const BLOCK_UNPAID_ACTION_LIMIT: u32 = 0;

/// Fee parameters of ZIP 317.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zip317Params {
    /// Fee per logical action, in zatoshis.
    pub marginal_fee: u64,
    /// Minimum number of actions that the fee charges for.
    pub grace_actions: u32,
    /// Upper bound of `fee / conventional_fee`. The selection uses it as the weight.
    pub weight_ratio_cap: u32,
}

impl Zip317Params {
    /// The values published in ZIP 317.
    pub const ZIP317: Self = Self {
        marginal_fee: 5000,
        grace_actions: 2,
        weight_ratio_cap: 4,
    };

    /// The values of Zakura, which the node uses (see the module documentation). ZIP 317:
    /// the marginal fee 400 and the cap 13 differ from 5,000 and 4.
    pub const ZAKURA: Self = Self {
        marginal_fee: 400,
        grace_actions: 2,
        weight_ratio_cap: 13,
    };

    /// ZIP 317: `marginal_fee * max(grace_actions, logical_actions)`.
    pub fn conventional_fee(&self, logical_actions: u32) -> u64 {
        self.marginal_fee * u64::from(logical_actions.max(self.grace_actions))
    }

    /// ZIP 317 `weight_ratio`: `min(max(1, fee) / conventional_fee, weight_ratio_cap)` as a
    /// fixed-point ratio. A transaction without a fee has the weight of a fee of 1 zatoshi.
    ///
    /// Panics if `conventional_fee` is zero, which cannot happen for a positive marginal fee.
    pub fn weight_ratio(&self, fee: u64, conventional_fee: u64) -> WeightRatio {
        assert!(
            conventional_fee > 0,
            "ZIP 317 conventional fee is never zero"
        );
        WeightRatio::from_ratio(fee.max(1), conventional_fee, self.weight_ratio_cap)
    }

    /// `max(0, conventional_actions - floor(fee / marginal_fee))`: the ZIP 317 unpaid action
    /// count. It is 0 exactly when `fee >= conventional_fee`. The block production algorithm
    /// limits its total in a block to [`BLOCK_UNPAID_ACTION_LIMIT`].
    pub fn unpaid_actions(&self, fee: u64, conventional_fee: u64) -> u32 {
        let conventional_actions = conventional_fee / self.marginal_fee;
        let paid_actions = fee / self.marginal_fee;
        u32::try_from(conventional_actions.saturating_sub(paid_actions))
            .expect("action counts fit in u32")
    }
}

/// `fee / conventional_fee`, capped, as an unsigned fixed-point number with 32 fractional
/// bits. Integer arithmetic keeps the candidate order identical on every machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WeightRatio(u64);

impl WeightRatio {
    pub const FRACTION_BITS: u32 = 32;

    pub fn from_ratio(numerator: u64, denominator: u64, cap: u32) -> Self {
        let scaled = (u128::from(numerator) << Self::FRACTION_BITS) / u128::from(denominator);
        let capped = scaled.min(u128::from(cap) << Self::FRACTION_BITS);
        Self(u64::try_from(capped).expect("capped ratio fits in 64 bits"))
    }

    pub fn to_f64(self) -> f64 {
        self.0 as f64 / (1u64 << Self::FRACTION_BITS) as f64
    }
}

/// Standard sizes that ZIP 317 uses to convert transparent input and output bytes into
/// actions.
const P2PKH_STANDARD_INPUT_SIZE: usize = 150;
const P2PKH_STANDARD_OUTPUT_SIZE: usize = 34;

/// ZIP 317 logical actions of a transaction:
/// `max(ceil(tx_in_total_size / 150), ceil(tx_out_total_size / 34)) + 2 * nJoinSplit
///  + max(nSpendsSapling, nOutputsSapling) + nActionsOrchard + nActionsIronwood`.
/// The Ironwood term is the one of Zakura's `conventional_actions`
/// (`zakura-chain/src/transaction/unmined/zip317.rs:131-163`).
pub fn logical_actions(tx: &Transaction) -> u32 {
    let (vin_bytes, vout_bytes) = match tx.transparent_bundle() {
        Some(bundle) => {
            let mut vin = CountingWriter::default();
            for txin in &bundle.vin {
                txin.write(&mut vin).expect("counting writer never fails");
            }
            let mut vout = CountingWriter::default();
            for txout in &bundle.vout {
                txout.write(&mut vout).expect("counting writer never fails");
            }
            (vin.0, vout.0)
        }
        None => (0, 0),
    };
    let transparent = vin_bytes
        .div_ceil(P2PKH_STANDARD_INPUT_SIZE)
        .max(vout_bytes.div_ceil(P2PKH_STANDARD_OUTPUT_SIZE));
    let sprout = tx
        .sprout_bundle()
        .map_or(0, |bundle| 2 * bundle.joinsplits.len());
    let sapling = tx.sapling_bundle().map_or(0, |bundle| {
        bundle
            .shielded_spends()
            .len()
            .max(bundle.shielded_outputs().len())
    });
    let orchard = tx
        .orchard_bundle()
        .map_or(0, |bundle| bundle.actions().len());
    let ironwood = tx
        .ironwood_bundle()
        .map_or(0, |bundle| bundle.actions().len());
    u32::try_from(transparent + sprout + sapling + orchard + ironwood)
        .expect("action count fits in u32")
}

#[derive(Default)]
struct CountingWriter(usize);

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_fee_applies_grace_actions() {
        let p = Zip317Params::ZAKURA;
        assert_eq!(p.conventional_fee(0), 800);
        assert_eq!(p.conventional_fee(2), 800);
        assert_eq!(p.conventional_fee(3), 1_200);
        assert_eq!(Zip317Params::ZIP317.conventional_fee(3), 15_000);
    }

    #[test]
    fn weight_ratio_is_capped_and_ordered() {
        let p = Zip317Params::ZAKURA;
        let one = p.weight_ratio(800, 800);
        let half = p.weight_ratio(400, 800);
        // The cap: 13 times the conventional fee, and each fee above it.
        let below_cap = p.weight_ratio(13 * 800 - 1, 800);
        let capped = p.weight_ratio(13 * 800, 800);
        assert_eq!(one.to_f64(), 1.0);
        assert_eq!(half.to_f64(), 0.5);
        assert_eq!(capped.to_f64(), 13.0);
        assert_eq!(p.weight_ratio(1_000_000, 800), capped);
        assert!(half < one && one < below_cap && below_cap < capped);
        // No fee: the weight of 1 zatoshi, so the larger conventional fee has the lower weight.
        assert_eq!(p.weight_ratio(0, 800), p.weight_ratio(1, 800));
        assert!(p.weight_ratio(0, 1_200) < p.weight_ratio(0, 800));
        assert!(p.weight_ratio(0, 1_200) > WeightRatio::from_ratio(0, 1, 13));
        assert_eq!(
            Zip317Params::ZIP317
                .weight_ratio(1_000_000, 10_000)
                .to_f64(),
            4.0
        );
    }

    #[test]
    fn unpaid_actions_follow_the_marginal_fee() {
        let p = Zip317Params::ZAKURA;
        assert_eq!(p.unpaid_actions(800, 800), 0);
        assert_eq!(p.unpaid_actions(799, 800), 1);
        assert_eq!(p.unpaid_actions(399, 800), 2);
        assert_eq!(p.unpaid_actions(400, 1_200), 2);
        assert_eq!(p.unpaid_actions(50_000, 1_200), 0);
        assert_eq!(Zip317Params::ZIP317.unpaid_actions(4_999, 10_000), 2);
    }

    #[test]
    fn logical_actions_count_transparent_bytes() {
        let tx = crate::test_support::transparent_tx(1, 1, 1);
        // One approximately standard input (small script) and one 34-byte output: both round to 1.
        assert_eq!(logical_actions(&tx), 1);
        let tx = crate::test_support::transparent_tx(2, 1, 6);
        // Six 34-byte outputs are exactly 6 actions. Two inputs under 150 bytes each are 2.
        assert_eq!(logical_actions(&tx), 6);
    }
}
