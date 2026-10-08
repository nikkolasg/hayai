//! The mempool: the store of prepared transactions, the relay policy and the admission order.
//!
//! Contract: `docs/architecture.md`, section hayai-mempool, and `docs/mempool-policy.md`.
//!
//! - [`PreparedStore`] keeps prepared transactions by [`hayai_wire::WtxId`] with conflict
//!   detection on spent outpoints and nullifiers and the ZIP 401 eviction. It serves
//!   [`hayai_prepared::PreparedLookup`] for block validation, [`hayai_wire::TxLookup`] for
//!   compact-block reconstruction and [`hayai_template::CandidateSource`] for the live
//!   template.
//! - [`MempoolPolicy`] holds the rules that decide whether a node stores and relays a valid
//!   transaction: ZIP 317 fees, ZIP 203 expiry and the standardness of zcashd.
//! - [`Mempool`] applies the admission order on the committed tip: the eviction memory,
//!   `prepare`, the policy, the tip state, the proofs, then the insert. It also admits the
//!   transactions again after a reorg.
//!
//! The crate has no relay code and no metrics. A node gives each admitted transaction to its
//! relay, records its own metrics, and decides which stored transactions a peer can see.

#![forbid(unsafe_code)]

mod admission;
mod policy;
mod store;
#[cfg(test)]
mod test_support;

pub use admission::{Mempool, Reject};
pub use policy::{
    dust_threshold, is_relayable, min_relay_fee, MempoolPolicy, PolicyContext, PolicyReject,
    MAX_DATACARRIER_BYTES, MAX_P2SH_SIGOPS, MAX_STANDARD_MULTISIG_PUBKEYS,
    MAX_STANDARD_SCRIPTSIG_SIZE, MAX_STANDARD_TX_SIGOPS, MIN_RELAY_FEE_CAP, MIN_RELAY_FEE_RATE,
    ONE_THIRD_DUST_THRESHOLD_RATE, TX_EXPIRING_SOON_THRESHOLD,
};
pub use store::{
    EntryFees, InsertError, PreparedStore, EVICTION_MEMORY, EVICTION_MEMORY_ENTRIES,
    LOW_FEE_PENALTY, MEMPOOL_COST_THRESHOLD, MEMPOOL_TX_COST_LIMIT,
};
