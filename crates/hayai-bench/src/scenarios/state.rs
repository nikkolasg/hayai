//! The synthetic chain of the state benchmarks (`benches/state.rs`) and of the
//! `state_push_1000_window` sysbench scenario: committing one block of the audit's per-block
//! shape to a chain whose window holds `window` blocks, as a hayai layer push or as Zakura's
//! deep clone of the chain's index maps.

use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{Coin, CoinsBacking, FlushGeneration, OutPoint, Pool};
use hayai_state::{Base, Chain, ChainView, Layer};
use hayai_wire::header::BlockHash;

use super::{Built, Impl};
use crate::zakura_chain_clone::{BlockEntries, BlockShape, ZakuraChainClone};

/// A backing that is never flushed: finalization stays in the coins cache.
pub struct NoBacking;

impl CoinsBacking for NoBacking {
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, hayai_coins::Error> {
        Ok(vec![None; outpoints.len()])
    }
    fn write_batch(
        &self,
        _: &[(&OutPoint, &Coin)],
        _: &[&OutPoint],
    ) -> Result<(), hayai_coins::Error> {
        Ok(())
    }
    fn contains_many(
        &self,
        _: Pool,
        nullifiers: &[[u8; 32]],
    ) -> Result<Vec<bool>, hayai_coins::Error> {
        Ok(vec![false; nullifiers.len()])
    }
    fn insert_many(&self, _: Pool, _: &[[u8; 32]]) -> Result<(), hayai_coins::Error> {
        Ok(())
    }
    fn write_generation(&self, _: &FlushGeneration) -> Result<(), hayai_coins::Error> {
        Ok(())
    }
}

/// Hash of synthetic block `height`.
pub fn block_hash(height: u32) -> BlockHash {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash(h)
}

/// The layer of synthetic block `height`: it creates the outputs block `height + 1` spends
/// and spends those of block `height - 1`, so the window's coin set stays constant.
pub fn layer(entries: &BlockEntries, parent: &Chain) -> Layer {
    let height = entries.height;
    let created: hayai_state::Map<OutPoint, Coin> = entries
        .spent
        .iter()
        .map(|o| {
            // Output `n` of "transaction" n of this block, spent by the next block's entries.
            let mut txid = *o.hash();
            txid[1..5].copy_from_slice(&(height + 1).to_le_bytes());
            (
                OutPoint::new(txid, 0),
                Coin {
                    value: 1,
                    script_pubkey: Bytes::from_static(&[0x51]),
                    height,
                    is_coinbase: false,
                },
            )
        })
        .collect();
    // The first layer over the base has no predecessor whose outputs it could spend.
    let spent: hayai_state::Set<OutPoint> = match parent.layers().next() {
        None => hayai_state::Set::default(),
        Some(_) => entries.spent.iter().cloned().collect(),
    };
    let mut nullifiers: [hayai_state::Set<[u8; 32]>; 4] = Default::default();
    for pool in Pool::ALL {
        nullifiers[pool.index()].extend(entries.nullifiers[pool.index()].iter().copied());
    }
    // A synthetic block moves no value: the pools and the trees of the parent stay.
    let view = parent.view();
    let frontiers = view.frontiers();
    Layer {
        height,
        hash: block_hash(height),
        parent: parent.tip().hash,
        time: 1_700_000_000 + height * 75,
        bits: 0x1f07_ffff,
        wtxids: Vec::new(),
        created,
        spent,
        spent_coins: Vec::new(),
        nullifiers,
        orchard_frontier: frontiers.orchard,
        sapling_frontier: frontiers.sapling,
        ironwood_frontier: frontiers.ironwood,
        sprout_frontier: frontiers.sprout,
        anchors: frontiers.anchors,
        value_pools: view.value_pools(),
        history: None,
    }
}

/// An empty in-memory chain at height 0.
pub fn new_chain() -> Chain {
    let base = Base::new(Arc::new(NoBacking), 0, block_hash(0), 1_700_000_000);
    Chain::new(base)
}

/// The hayai body: build the next block's layer, push it, finalize the layer that leaves the
/// window into the coins cache and take a view.
pub fn push(chain: &mut Chain, shape: BlockShape, window: usize) -> ChainView {
    let entries = BlockEntries::synthetic(chain.tip().height + 1, shape);
    let l = layer(&entries, chain);
    chain.push(l).expect("on tip");
    chain.finalize_excess(window).expect("in-memory finalize");
    chain.view()
}

/// `state_push_1000_window`.
pub fn build_push(window: usize, imp: Impl) -> Built {
    let shape = BlockShape::AUDIT;
    match imp {
        Impl::Hayai => {
            let mut chain = new_chain();
            for _ in 0..window {
                push(&mut chain, shape, window);
            }
            Built::new(move |m| {
                let view = m.timed(|| push(&mut chain, shape, window));
                assert_eq!(view.tip().height, chain.tip().height);
            })
        }
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => {
            let mut zakura = ZakuraChainClone::with_blocks(window, shape);
            Built::new(move |m| {
                let len = m.timed(|| {
                    let entries = BlockEntries::synthetic(zakura.next_height(), shape);
                    zakura.push_block(entries, window);
                    zakura.len()
                });
                assert_eq!(len, window);
            })
        }
    }
}
