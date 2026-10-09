//! Fixtures, store stand-in and bodies of the relay benchmarks (`benches/relay.rs`) and of
//! the `relay_reconstruct` sysbench scenario: rebuilding a block from a store holding every
//! transaction (compact relay) against what a full-body relay pays on receipt, zakura-chain's
//! `Block::zcash_deserialize` plus txids and auth digests.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use hayai_crypto::{zcash_encoding, zcash_primitives, zcash_protocol};
use hayai_relay::{
    reconstruct, Batch, BatchAnnounce, BatchId, CandidateStore, CompactBlock, IdForm,
    LanePublisher, LaneStore, ResolvedCandidate,
};
use hayai_wire::{RawBlock, RawTx, TxLookup, WtxId};
use rand::{Rng, SeedableRng};
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::BranchId;
use zk_chain::serialization::ZcashDeserialize;

use super::{Built, Impl};
use hayai_fixtures as fixtures;

pub struct Fixture {
    pub name: String,
    pub branch: BranchId,
    pub block: RawBlock,
}

impl From<fixtures::Fixture> for Fixture {
    fn from(f: fixtures::Fixture) -> Self {
        Fixture {
            block: f.parse(),
            name: f.name,
            branch: f.branch_id,
        }
    }
}

/// Synthetic blocks from `hayai_fixtures` (real signatures and Orchard proofs, cached under
/// `bench-fixtures/`).
pub fn synthetic_fixtures() -> Vec<Fixture> {
    vec![
        fixtures::transparent_block(2000, 2).into(),
        fixtures::orchard_block(200, 2).into(),
        fixtures::mixed_block(1000, 2, 100, 2).into(),
    ]
}

/// The synthetic fixtures and two real mainnet NU5 blocks.
pub fn all_fixtures() -> Vec<Fixture> {
    let mut out = synthetic_fixtures();
    for (name, hex) in [
        (
            "main-1687107",
            include_str!("../../../hayai-wire/tests/vectors/block-main-1-687-107.hex"),
        ),
        (
            "main-1687108",
            include_str!("../../../hayai-wire/tests/vectors/block-main-1-687-108.hex"),
        ),
    ] {
        let bytes = hex::decode(hex.trim()).expect("hex vector");
        let block = RawBlock::parse(Bytes::from(bytes), BranchId::Nu5).expect("mainnet block");
        out.push(Fixture {
            name: name.to_string(),
            branch: BranchId::Nu5,
            block,
        });
    }
    out
}

/// In-memory prepared-store stand-in: the block's transactions plus filler ids so that
/// the short-id index covers a realistic mempool.
pub struct MemStore {
    txs: HashMap<WtxId, Arc<RawTx>>,
    filler: Vec<WtxId>,
}

impl MemStore {
    pub fn holding(block: &RawBlock, total_ids: usize) -> Self {
        let txs: HashMap<WtxId, Arc<RawTx>> = block
            .txs
            .iter()
            .skip(1)
            .map(|tx| (tx.wtxid(), Arc::new(tx.clone())))
            .collect();
        let mut rng = rand::rngs::StdRng::seed_from_u64(99);
        let filler = (txs.len()..total_ids)
            .map(|_| WtxId {
                txid: TxId::from_bytes(rng.gen()),
                auth_digest: rng.gen(),
            })
            .collect();
        MemStore { txs, filler }
    }
}

impl TxLookup for MemStore {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>> {
        self.txs.get(id).cloned()
    }
    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        self.txs.keys().for_each(&mut *f);
        self.filler.iter().for_each(f);
    }
    fn len(&self) -> usize {
        self.txs.len() + self.filler.len()
    }
}

/// One batch covering every non-coinbase transaction, registered in a lane store.
pub fn whole_block_batch(block: &RawBlock, store: &dyn TxLookup) -> (Batch, LaneStore) {
    let ids: Vec<WtxId> = block.txs[1..].iter().map(RawTx::wtxid).collect();
    let batch = Batch {
        id: BatchId::compute(&ids),
        lane: [1; 32],
        seq: 1,
        ids: ids.clone(),
    };
    let mut lanes = LaneStore::new();
    lanes
        .insert(
            &BatchAnnounce {
                lane_id: batch.lane,
                seq: batch.seq,
                batch_id: batch.id,
                ids,
            },
            store,
            std::time::Instant::now(),
        )
        .expect("batch accepted");
    (batch, lanes)
}

/// `block` with its transactions after the coinbase in canonical order (parents first,
/// then txid) and the header's merkle root rewritten to match: the order of every
/// template, and the order the candidate form needs.
pub fn canonical(block: &RawBlock, branch: BranchId) -> RawBlock {
    let body: Vec<&RawTx> = block.txs[1..].iter().collect();
    let order = hayai_wire::canonical_order_of(&body).expect("a real block has no cycle");
    let mut txs: Vec<&RawTx> = vec![&block.txs[0]];
    txs.extend(order.iter().map(|&i| body[i]));
    let mut header = block.header.clone();
    let txids: Vec<TxId> = txs.iter().map(|t| t.txid).collect();
    header.merkle_root = hayai_wire::merkle_root(&txids);
    let mut bytes = header.serialize();
    zcash_encoding::CompactSize::write(&mut bytes, txs.len()).expect("write to Vec");
    for tx in &txs {
        bytes.extend_from_slice(&tx.bytes);
    }
    RawBlock::parse(Bytes::from(bytes), branch).expect("reordered block parses")
}

/// The lane of a publisher whose candidate is every transaction of `block` after the
/// coinbase, as a receiver records it: the batch in its lane store and the candidate in
/// its candidate store.
pub fn published_candidate(
    block: &RawBlock,
    store: &dyn TxLookup,
) -> (LaneStore, CandidateStore, ResolvedCandidate) {
    let ids: Vec<WtxId> = block.txs[1..].iter().map(RawTx::wtxid).collect();
    let publication = LanePublisher::new([2; 32]).publish(block.header.prev_hash, 1, &ids);
    let mut lanes = LaneStore::new();
    let now = std::time::Instant::now();
    if let Some(batch) = &publication.batch {
        lanes.insert(batch, store, now).expect("batch accepted");
    }
    let mut candidates = CandidateStore::new();
    candidates
        .insert(publication.candidate, now)
        .expect("candidate accepted");
    (lanes, candidates, publication.resolved)
}

/// The full-block baseline: zakura-chain deserialization plus the ids a node computes on
/// receipt.
pub fn zakura_parse_with_ids(
    bytes: &[u8],
) -> Vec<(
    zk_chain::transaction::Hash,
    Option<zk_chain::transaction::AuthDigest>,
)> {
    let block = zk_chain::block::Block::zcash_deserialize(bytes).expect("parses");
    block
        .transactions
        .iter()
        .map(|tx| (tx.hash(), tx.auth_digest()))
        .collect()
}

/// `relay_reconstruct` on the named synthetic fixture.
pub fn build_reconstruct(fixture: &str, imp: Impl) -> Result<Built, String> {
    let Some(f) = synthetic_fixtures().into_iter().find(|f| f.name == fixture) else {
        return Err(format!("no relay fixture named {fixture}"));
    };
    Ok(match imp {
        Impl::Hayai => {
            let store = MemStore::holding(&f.block, 10_000);
            let (_, lanes) = whole_block_batch(&f.block, &store);
            let short = CompactBlock::from_block(&f.block, &[], |_, _| IdForm::Short, 1);
            let txs = f.block.txs.len();
            Built::new(move |m| {
                let block =
                    m.timed(|| reconstruct(&short, &store, &lanes, f.branch).expect("complete"));
                assert_eq!(block.txs.len(), txs);
            })
        }
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => {
            let bytes = f.block.bytes.clone();
            let txs = f.block.txs.len();
            Built::new(move |m| {
                let ids = m.timed(|| zakura_parse_with_ids(&bytes));
                assert_eq!(ids.len(), txs);
            })
        }
    })
}
