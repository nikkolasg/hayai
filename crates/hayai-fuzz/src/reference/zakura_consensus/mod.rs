//! The check functions of zakura-consensus 10.0.0, copied from the Zakura source tree at
//! commit 1377915 (`crates/zakura-consensus/src`). License: MIT OR Apache-2.0, as Zakura.
//!
//! zakura-consensus does not link into this process (see `Cargo.toml`), and its block
//! checks are private. The copies compile against the published zakura-chain 9.0.0,
//! zakura-header-chain 4.0.0 and zakura-script 4.0.0, which are the crates that
//! zakura-consensus 10.0.0 uses.
//!
//! | File | Source | Changes |
//! |---|---|---|
//! | `transaction_check.rs` | `transaction/check.rs` | `crate::error` is `super::error`; `zakura_state::check::transparent_coinbase_spend` is `super::state_check::transparent_coinbase_spend` |
//! | `block_check.rs` | `block/check.rs` | `crate::{error, funding_stream_address}` is `super::{error, subsidy::funding_stream_address}` |
//! | `subsidy.rs` | `block/subsidy.rs` | no `mod tests` |
//! | `error.rs` | `error.rs` | see the head of the file |
//! | `state_check.rs` | zakura-state `service/check/utxo.rs` | one function; see the head of the file |
//!
//! [`MAX_BLOCK_SIGOPS`] is the constant of `block.rs`.
//!
//! Update procedure: copy the files again, apply the changes of the table, and
//! change the commit in this comment.

// The copies keep the style of their source.
#![allow(clippy::all, missing_docs, dead_code, unused_imports)]

#[rustfmt::skip]
pub mod block_check;
#[rustfmt::skip]
pub mod error;
#[rustfmt::skip]
pub mod state_check;
#[rustfmt::skip]
pub mod subsidy;
#[rustfmt::skip]
pub mod transaction_check;

/// The maximum number of transparent signature operations in a block (`block.rs:303`).
pub const MAX_BLOCK_SIGOPS: u32 = 20_000;

/// `transaction_check::shielded_proof_size_is_canonical`, which is `pub(super)` in its
/// source file.
pub fn shielded_proof_size_is_canonical(
    tx: &zakura_chain::transaction::Transaction,
) -> Result<(), error::TransactionError> {
    transaction_check::shielded_proof_size_is_canonical(tx)
}
