//! Template candidates and the event stream that a candidate source delivers.

use std::cmp::Reverse;

use bytes::Bytes;
use hayai_crypto::zcash_transparent;
use hayai_wire::{RawTx, WtxId};
use zcash_transparent::bundle::OutPoint;

use crate::zip317::{logical_actions, WeightRatio, Zip317Params};

/// A mempool transaction as the template sees it: wire bytes plus the context-free results
/// that the selection needs. Sizes and action counts are fixed for the life of the candidate.
/// Only the fee can change, through [`SetEvent::Repriced`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub wtxid: WtxId,
    /// Exact wire bytes. The template copies them into a block as they are.
    pub bytes: Bytes,
    /// Fee in zatoshis.
    pub fee: u64,
    /// ZIP 317 conventional fee in zatoshis.
    pub conventional_fee: u64,
    /// ZIP 317 `weight_ratio`: `min(max(1, fee) / conventional_fee, cap)`.
    pub weight_ratio: WeightRatio,
    /// ZIP 317 unpaid actions. The value is 0 exactly when `fee >= conventional_fee`.
    pub unpaid_actions: u32,
    /// Block-level sigop count (legacy plus P2SH).
    pub sigops: u32,
    /// Orchard actions, counted against the NU7 per-block Orchard action limit.
    pub orchard_actions: u32,
    /// Ironwood actions, counted against the NU7 per-block Ironwood action limit.
    pub ironwood_actions: u32,
    /// Sapling spends plus outputs, counted against the NU7 per-block Sapling I/O limit.
    pub sapling_ios: u32,
    /// Parents that are themselves unmined (in the candidate set). A candidate is selectable
    /// only after the selection contains all of them.
    pub depends_on: Vec<WtxId>,
    /// Transparent outpoints spent. The caller uses them to detect conflicts with a new tip.
    pub spends: Vec<OutPoint>,
}

impl Candidate {
    /// Builds a candidate from a parsed transaction and computes the ZIP 317 fields under
    /// `params`. `fee` and `sigops` need the spent coins, so the caller supplies them.
    pub fn from_raw(
        raw: &RawTx,
        fee: u64,
        sigops: u32,
        depends_on: Vec<WtxId>,
        params: &Zip317Params,
    ) -> Self {
        let conventional_fee = params.conventional_fee(logical_actions(&raw.tx));
        let orchard_actions = raw
            .tx
            .orchard_bundle()
            .map_or(0, |bundle| bundle.actions().len());
        let ironwood_actions = raw
            .tx
            .ironwood_bundle()
            .map_or(0, |bundle| bundle.actions().len());
        let sapling_ios = raw.tx.sapling_bundle().map_or(0, |bundle| {
            bundle.shielded_spends().len() + bundle.shielded_outputs().len()
        });
        let spends = raw.tx.transparent_bundle().map_or_else(Vec::new, |bundle| {
            bundle
                .vin
                .iter()
                .map(|txin| txin.prevout().clone())
                .collect()
        });
        Self {
            wtxid: raw.wtxid(),
            bytes: raw.bytes.clone(),
            fee,
            conventional_fee,
            weight_ratio: params.weight_ratio(fee, conventional_fee),
            unpaid_actions: params.unpaid_actions(fee, conventional_fee),
            sigops,
            orchard_actions: u32::try_from(orchard_actions).expect("fits in u32"),
            ironwood_actions: u32::try_from(ironwood_actions).expect("fits in u32"),
            sapling_ios: u32::try_from(sapling_ios).expect("fits in u32"),
            depends_on,
            spends,
        }
    }

    pub fn size_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Position of this candidate in the deterministic selection order.
    pub fn order_key(&self) -> OrderKey {
        OrderKey {
            ratio: Reverse(self.weight_ratio),
            size: u32::try_from(self.bytes.len()).expect("transaction size fits in u32"),
            wtxid: self.wtxid,
        }
    }
}

/// Total order of candidates: highest weight ratio first, then smallest size, then WtxId.
/// Every candidate that pays the conventional fee (weight ratio of 1 or more) comes before
/// every candidate that does not.
///
/// ZIP 317: the two passes of the block production algorithm. The pick in a pass follows
/// this order: it differs from the RECOMMENDED random pick by weight ratio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderKey {
    pub ratio: Reverse<WeightRatio>,
    pub size: u32,
    pub wtxid: WtxId,
}

/// A change to the candidate set.
#[derive(Clone, Debug)]
pub enum SetEvent {
    /// A new candidate. Its unmined parents must already be known to the template.
    Added(Candidate),
    /// A candidate left the source. The removal of an id that the template no longer holds is
    /// a no-op, because a tip event can already have dropped it.
    Removed(WtxId),
    /// The fee of a known candidate changed (for example, after a replacement policy
    /// evaluated it again). The source computes the new weight ratio and the new unpaid
    /// action count with its own parameters.
    Repriced {
        wtxid: WtxId,
        fee: u64,
        weight_ratio: WeightRatio,
        unpaid_actions: u32,
    },
}

/// Provider of the candidate set: an initial snapshot plus a stream of changes. The prepared
/// transaction store implements it.
pub trait CandidateSource: Send + Sync {
    /// Every candidate currently held, in any order. Parents and children can be interleaved.
    fn candidates(&self) -> Vec<Candidate>;
    /// Changes after the snapshot that [`CandidateSource::candidates`] returned.
    fn events(&self) -> crossbeam_channel::Receiver<SetEvent>;
}
