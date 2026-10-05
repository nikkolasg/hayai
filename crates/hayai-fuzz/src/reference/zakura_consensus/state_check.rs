//! The transparent coinbase spend rule of zakura-state 10.0.0
//! (`src/service/check/utxo.rs:185-227`).
//!
//! Change: the function returns the two matching variants of `TransactionError`. The
//! source returns `ValidateContextError`, a type of zakura-state. The variant
//! `UnshieldedTransparentCoinbaseSpend` of `TransactionError` has a field
//! `min_spend_height` that the source variant does not have: it gets the height of the
//! output. Only the error text shows the field.

use zakura_chain::transparent::{
    self,
    CoinbaseSpendRestriction::{CheckCoinbaseMaturity, DisallowCoinbaseSpend},
    MIN_TRANSPARENT_COINBASE_MATURITY,
};

use super::error::TransactionError::{
    self, ImmatureTransparentCoinbaseSpend, UnshieldedTransparentCoinbaseSpend,
};

/// Check that `utxo` is spendable, based on the coinbase `spend_restriction`.
///
/// # Consensus
///
/// > A transaction with one or more transparent inputs from coinbase transactions
/// > MUST have no transparent outputs (i.e. tx_out_count MUST be 0).
/// > Inputs from coinbase transactions include Founders’ Reward outputs and
/// > funding stream outputs.
///
/// > A transaction MUST NOT spend a transparent output of a coinbase transaction
/// > from a block less than 100 blocks prior to the spend.
/// > Note that transparent outputs of coinbase transactions include
/// > Founders’ Reward outputs and transparent funding stream outputs.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
pub fn transparent_coinbase_spend(
    outpoint: transparent::OutPoint,
    spend_restriction: transparent::CoinbaseSpendRestriction,
    utxo: &transparent::Utxo,
) -> Result<(), TransactionError> {
    if !utxo.from_coinbase {
        return Ok(());
    }

    match spend_restriction {
        CheckCoinbaseMaturity { spend_height } => {
            let min_spend_height = utxo.height + MIN_TRANSPARENT_COINBASE_MATURITY.into();
            let min_spend_height =
                min_spend_height.expect("valid UTXOs have coinbase heights far below Height::MAX");
            if spend_height >= min_spend_height {
                Ok(())
            } else {
                Err(ImmatureTransparentCoinbaseSpend {
                    outpoint,
                    spend_height,
                    min_spend_height,
                    created_height: utxo.height,
                })
            }
        }
        DisallowCoinbaseSpend => Err(UnshieldedTransparentCoinbaseSpend {
            outpoint,
            min_spend_height: utxo.height,
        }),
    }
}
