//! Chain mechanics: view correctness through push, pop and finalization.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{BestBlock, Coin, CoinsBacking, CoinsView, FlushGeneration, OutPoint, Pool};
use hayai_trees::{OrchardFrontier, SaplingFrontier};
use hayai_wire::header::BlockHash;
use parking_lot::Mutex;

use crate::{Anchors, Base, Chain, ChainError, Layer, SpecId, ValuePools, COINBASE_MATURITY};

/// A backing store in two hash maps, enough to observe what a flush wrote.
#[derive(Default)]
struct MemBacking {
    coins: Mutex<HashMap<OutPoint, Coin>>,
    nullifiers: Mutex<[HashSet<[u8; 32]>; 4]>,
    best_block: Mutex<Option<BestBlock>>,
}

impl CoinsBacking for MemBacking {
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, hayai_coins::Error> {
        let coins = self.coins.lock();
        Ok(outpoints.iter().map(|o| coins.get(o).cloned()).collect())
    }

    fn write_batch(
        &self,
        adds: &[(&OutPoint, &Coin)],
        spends: &[&OutPoint],
    ) -> Result<(), hayai_coins::Error> {
        let mut coins = self.coins.lock();
        for (o, c) in adds {
            coins.insert((*o).clone(), (*c).clone());
        }
        for o in spends {
            coins.remove(*o);
        }
        Ok(())
    }

    fn contains_many(
        &self,
        pool: Pool,
        nullifiers: &[[u8; 32]],
    ) -> Result<Vec<bool>, hayai_coins::Error> {
        let sets = self.nullifiers.lock();
        Ok(nullifiers
            .iter()
            .map(|n| sets[pool.index()].contains(n))
            .collect())
    }

    fn insert_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<(), hayai_coins::Error> {
        self.nullifiers.lock()[pool.index()].extend(nullifiers.iter().copied());
        Ok(())
    }

    fn write_generation(&self, generation: &FlushGeneration) -> Result<(), hayai_coins::Error> {
        let adds: Vec<(&OutPoint, &Coin)> = generation.adds.iter().map(|(o, c)| (o, c)).collect();
        let spends: Vec<&OutPoint> = generation.spends.iter().collect();
        self.write_batch(&adds, &spends)?;
        for pool in Pool::ALL {
            self.insert_many(pool, &generation.nullifiers[pool.index()])?;
        }
        *self.best_block.lock() = Some(generation.best_block);
        Ok(())
    }
}

fn hash(n: u32) -> BlockHash {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&n.to_le_bytes());
    BlockHash(h)
}

fn outpoint(block: u32, n: u32) -> OutPoint {
    let mut txid = [0u8; 32];
    txid[..4].copy_from_slice(&block.to_le_bytes());
    txid[4] = 0xcc;
    OutPoint::new(txid, n)
}

fn coin(height: u32, value: u64) -> Coin {
    Coin {
        value,
        script_pubkey: Bytes::from_static(&[0x51]),
        height,
        is_coinbase: false,
    }
}

fn nullifier(block: u32, n: u32) -> [u8; 32] {
    let mut nf = [0u8; 32];
    nf[..4].copy_from_slice(&block.to_le_bytes());
    nf[4..8].copy_from_slice(&n.to_le_bytes());
    nf
}

/// Block `height` creates two coins, spends coin 0 of block `height - 1` (when `height > 1`)
/// and reveals one Orchard nullifier.
fn layer(height: u32, parent: &Chain) -> Layer {
    layer_on(height, parent.tip().hash, &parent.view())
}

/// As [`layer`], on top of the speculative tip.
fn speculative_layer(height: u32, parent: &Chain) -> Layer {
    layer_on(
        height,
        parent.speculative_tip().hash,
        &parent.view_speculative(),
    )
}

/// The `bits` of the test block at `height`.
fn bits_of(height: u32) -> u32 {
    0x1f00_0000 + height
}

fn layer_on(height: u32, parent: BlockHash, view: &crate::ChainView) -> Layer {
    let mut created = HashMap::default();
    created.insert(outpoint(height, 0), coin(height, 100));
    created.insert(outpoint(height, 1), coin(height, 200));
    let mut spent = HashSet::default();
    if height > 1 {
        spent.insert(outpoint(height - 1, 0));
    }
    let mut nullifiers: [HashSet<[u8; 32], ahash::RandomState>; 4] = Default::default();
    nullifiers[Pool::Orchard.index()].insert(nullifier(height, 0));
    let parent_frontiers = view.frontiers();
    Layer {
        height,
        hash: hash(height),
        parent,
        time: 1_000 + height * 75,
        bits: bits_of(height),
        wtxids: Vec::new(),
        created,
        spent,
        spent_coins: Vec::new(),
        nullifiers,
        orchard_frontier: parent_frontiers.orchard,
        sapling_frontier: parent_frontiers.sapling,
        ironwood_frontier: parent_frontiers.ironwood,
        sprout_frontier: parent_frontiers.sprout,
        anchors: Anchors {
            sapling: [(height % 256) as u8; 32],
            orchard: [0xa0u8.wrapping_add((height % 256) as u8); 32],
            // No Ironwood commitment: the root of the parent stays.
            ironwood: parent_frontiers.anchors.ironwood,
        },
        value_pools: ValuePools::default(),
        history: None,
    }
}

fn chain() -> (Chain, Arc<MemBacking>) {
    let backing = Arc::new(MemBacking::default());
    let base = Base::new(backing.clone(), 0, hash(0), 1_000);
    (Chain::new(base), backing)
}

#[test]
fn view_shadows_spent_coins_and_sees_layers_newest_first() {
    let (mut chain, _) = chain();
    for h in 1..=3 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    let view = chain.view();
    assert_eq!(view.tip_height(), 3);
    let coins = view.get_coins(&[
        outpoint(1, 0),
        outpoint(1, 1),
        outpoint(2, 0),
        outpoint(3, 0),
        outpoint(9, 0),
    ]);
    assert_eq!(coins[0], None, "spent in block 2");
    assert_eq!(coins[1], Some(coin(1, 200)));
    assert_eq!(coins[2], None, "spent in block 3");
    assert_eq!(coins[3], Some(coin(3, 100)));
    assert_eq!(coins[4], None);
    assert_eq!(
        view.contains_nullifier_many(Pool::Orchard, &[nullifier(2, 0), nullifier(7, 0)]),
        vec![true, false]
    );
    assert!(view.has_anchor(Pool::Orchard, &[0xa2; 32]));
    assert!(view.has_anchor(Pool::Orchard, &OrchardFrontier::empty().root().to_bytes()));
    assert!(view.has_anchor(Pool::Sapling, &SaplingFrontier::empty().root().to_bytes()));
    assert!(!view.has_anchor(Pool::Orchard, &[0xa9; 32]));
}

#[test]
fn push_rejects_a_layer_off_the_tip() {
    let (mut chain, _) = chain();
    let mut l = layer(1, &chain);
    l.parent = hash(77);
    let Err(ChainError::NotOnTip { .. }) = chain.push(l) else {
        panic!("wrong parent must be rejected");
    };
    let mut l = layer(1, &chain);
    l.height = 2;
    let Err(ChainError::NotOnTip { .. }) = chain.push(l) else {
        panic!("wrong height must be rejected");
    };
}

#[test]
fn pop_restores_the_previous_view() {
    let (mut chain, _) = chain();
    for h in 1..=2 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    let before = chain.view();
    assert_eq!(before.get_coin(&outpoint(1, 0)), None);
    let popped = chain.pop().unwrap();
    assert_eq!(popped.height, 2);
    let after = chain.view();
    assert_eq!(after.tip_height(), 1);
    assert_eq!(after.get_coin(&outpoint(1, 0)), Some(coin(1, 100)));
    assert_eq!(after.get_coin(&outpoint(2, 0)), None);
    assert!(!after.has_anchor(Pool::Orchard, &[0xa2; 32]));
    assert_eq!(
        after.contains_nullifier_many(Pool::Orchard, &[nullifier(2, 0)]),
        vec![false]
    );
    // The older snapshot is unaffected by the pop.
    assert_eq!(before.tip_height(), 2);
    assert_eq!(before.get_coin(&outpoint(1, 0)), None);
}

#[test]
fn finalize_window_round_trip_against_the_cache() {
    let (mut chain, backing) = chain();
    let window = 100;
    let total = 150u32;
    for h in 1..=total {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
        let merged = chain.finalize_excess(window).unwrap();
        assert_eq!(merged, usize::from(h > window as u32));
    }
    assert_eq!(chain.layers().count(), window);
    let view = chain.view();
    assert_eq!(view.tip_height(), total);
    // The base now holds blocks 1..=50; their net effect is visible through the view.
    assert_eq!(view.get_coin(&outpoint(1, 1)), Some(coin(1, 200)));
    assert_eq!(view.get_coin(&outpoint(1, 0)), None, "spent by block 2");
    assert_eq!(
        view.get_coin(&outpoint(50, 0)),
        None,
        "spent by block 51, the oldest layer"
    );
    assert_eq!(view.get_coin(&outpoint(50, 1)), Some(coin(50, 200)));
    assert_eq!(
        view.contains_nullifier_many(Pool::Orchard, &[nullifier(1, 0), nullifier(150, 0)]),
        vec![true, true]
    );
    assert!(view.has_anchor(Pool::Orchard, &[0xa0 + 1; 32]));
    assert!(view.has_anchor(Pool::Sapling, &[1; 32]));
    {
        let base = chain.base().read();
        assert_eq!(base.height, 50);
        assert_eq!(base.hash, hash(50));
        assert_eq!(base.anchors.sapling, [50; 32]);
        // 50 blocks x 2 coins, minus 49 spent inside the base window; the spent ones were
        // fresh so they never need a disk write.
        assert_eq!(base.coins.len(), 51);
        assert_eq!(base.coins.dirty_len(), 51);
    }
    // Nothing reached disk before the flush.
    assert!(backing.coins.lock().is_empty());
    let (stats, nullifiers) = chain.flush().unwrap();
    assert_eq!(stats.adds, 51);
    assert_eq!(stats.spends, 0);
    assert_eq!(nullifiers, 50);
    assert_eq!(backing.coins.lock().len(), 51);
    assert!(backing.nullifiers.lock()[Pool::Orchard.index()].contains(&nullifier(25, 0)));
    // After the flush and a clean-entry drop, reads go to the backing and still agree.
    chain.base().write().coins.drop_clean();
    let view = chain.view();
    assert_eq!(view.get_coin(&outpoint(1, 1)), Some(coin(1, 200)));
    assert_eq!(view.get_coin(&outpoint(1, 0)), None);
}

#[test]
fn median_time_past_uses_layers_then_base() {
    let (mut chain, _) = chain();
    assert_eq!(chain.view().median_time_past(), 1_000);
    for h in 1..=5 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    // Times: base 1000, layers 1075..=1375; the median of six values is the fourth.
    assert_eq!(chain.view().median_time_past(), 1_000 + 3 * 75);
    for h in 6..=30 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
        chain.finalize_excess(10).unwrap();
    }
    // Only the newest eleven count: blocks 20..=30, median at block 25.
    assert_eq!(chain.view().median_time_past(), 1_000 + 25 * 75);
    chain.finalize_excess(0).unwrap();
    assert_eq!(chain.view().median_time_past(), 1_000 + 25 * 75);
}

#[test]
fn coinbase_maturity_constant_matches_zcash() {
    assert_eq!(COINBASE_MATURITY, 100);
}

#[test]
fn the_layer_window_is_the_finality_depth() {
    assert_eq!(crate::LAYER_WINDOW, 1_000);
    assert_eq!(
        crate::LAYER_WINDOW,
        hayai_consensus::FINALITY_DEPTH as usize
    );
}

#[test]
fn flush_phases_keep_entries_readable_and_record_the_best_block() {
    let (mut chain, backing) = chain();
    for h in 1..=5 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    chain.finalize_excess(2).unwrap();
    let view = chain.view();

    // Phase one: the generation holds the base's net effect (blocks 1..=3) and its tip.
    let generation = chain.begin_flush().unwrap();
    assert_eq!(
        generation.best_block,
        BestBlock {
            height: 3,
            hash: hash(3).0
        }
    );
    assert_eq!(generation.stats().adds, 4, "3 x 2 coins minus 2 spent");
    assert_eq!(generation.nullifier_count(), 3);
    let Err(ChainError::Coins(hayai_coins::Error::FlushInFlight)) = chain.begin_flush() else {
        panic!("a second generation cannot start");
    };
    // Between the phases nothing is on disk and every read still answers from memory.
    assert!(backing.coins.lock().is_empty());
    assert_eq!(view.get_coin(&outpoint(1, 1)), Some(coin(1, 200)));
    assert_eq!(view.get_coin(&outpoint(3, 1)), Some(coin(3, 200)));
    assert_eq!(
        view.contains_nullifier_many(Pool::Orchard, &[nullifier(2, 0)]),
        vec![true]
    );
    // The writer keeps going during the write: block 4 is finalized (spends coin 3/0,
    // which is in the generation) and that change is not lost at the end of the flush.
    chain.finalize_excess(1).unwrap();
    assert_eq!(view.get_coin(&outpoint(3, 0)), None);

    chain.backing().write_generation(&generation).unwrap();
    assert_eq!(backing.coins.lock().len(), 4);
    assert_eq!(*backing.best_block.lock(), Some(generation.best_block));
    chain.end_flush().unwrap();
    let Err(ChainError::Coins(hayai_coins::Error::NoFlushInFlight)) = chain.end_flush() else {
        panic!("end without begin");
    };
    {
        let base = chain.base().read();
        assert_eq!(
            base.coins.dirty_len(),
            3,
            "block 4: two adds and one tombstone"
        );
        assert_eq!(base.nullifiers.pool(Pool::Orchard).pending_len(), 1);
    }
    // The next flush brings the disk to block 4.
    let (stats, nullifiers) = chain.flush().unwrap();
    assert_eq!(stats.adds, 2);
    assert_eq!(stats.spends, 1);
    assert_eq!(nullifiers, 1);
    assert_eq!(
        *backing.best_block.lock(),
        Some(BestBlock {
            height: 4,
            hash: hash(4).0
        })
    );
    chain.base().write().coins.drop_clean();
    let view = chain.view();
    assert_eq!(view.get_coin(&outpoint(3, 0)), None);
    assert_eq!(view.get_coin(&outpoint(3, 1)), Some(coin(3, 200)));
    assert_eq!(
        view.get_coin(&outpoint(4, 0)),
        None,
        "spent by block 5, a layer"
    );
    assert_eq!(view.get_coin(&outpoint(4, 1)), Some(coin(4, 200)));
}

#[test]
fn a_crash_between_the_phases_leaves_the_disk_at_one_best_block() {
    // Before the write: nothing of the generation reached the disk.
    let (mut chain, backing) = chain();
    for h in 1..=3 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    chain.finalize_excess(0).unwrap();
    let generation = chain.begin_flush().unwrap();
    drop(chain);
    assert!(backing.coins.lock().is_empty());
    assert_eq!(*backing.best_block.lock(), None);
    // After the write, before phase three: the disk is the state after block 3, exactly
    // what the record says. Recovery replays from block 4.
    backing.write_generation(&generation).unwrap();
    assert_eq!(backing.coins.lock().len(), 4);
    assert_eq!(
        *backing.best_block.lock(),
        Some(BestBlock {
            height: 3,
            hash: hash(3).0
        })
    );
    assert!(backing.nullifiers.lock()[Pool::Orchard.index()].contains(&nullifier(3, 0)));
}

#[test]
fn a_stale_view_walks_its_own_layers() {
    let (mut chain, _) = chain();
    for h in 1..=3 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    let stale = chain.view();
    let l = layer(4, &chain);
    chain.push(l).unwrap();
    // Block 4 spends coin 3/0; the stale view does not know block 4.
    assert_eq!(stale.get_coin(&outpoint(3, 0)), Some(coin(3, 100)));
    assert_eq!(stale.get_coin(&outpoint(4, 0)), None);
    assert_eq!(
        stale.contains_nullifier_many(Pool::Orchard, &[nullifier(3, 0), nullifier(4, 0)]),
        vec![true, false]
    );
    assert_eq!(chain.view().get_coin(&outpoint(3, 0)), None);
    chain.pop().unwrap();
    chain.pop().unwrap();
    // After two pops the stale view is ahead of the chain.
    assert_eq!(stale.get_coin(&outpoint(3, 1)), Some(coin(3, 200)));
    assert_eq!(chain.view().get_coin(&outpoint(3, 1)), None);
}

#[test]
fn speculative_layers_commit_in_order() {
    let (mut chain, _) = chain();
    let l = layer(1, &chain);
    chain.push(l).unwrap();
    let l = speculative_layer(2, &chain);
    let two = chain.push_speculative(l).unwrap();
    let l = speculative_layer(3, &chain);
    let three = chain.push_speculative(l).unwrap();
    assert_eq!(chain.tip().height, 1);
    assert_eq!(chain.speculative_tip().height, 3);
    // The committed view does not see the speculative blocks; the speculative view does.
    assert_eq!(chain.view().get_coin(&outpoint(1, 0)), Some(coin(1, 100)));
    let spec = chain.view_speculative();
    assert_eq!(spec.tip_height(), 3);
    assert_eq!(spec.get_coin(&outpoint(1, 0)), None, "spent by block 2");
    assert_eq!(spec.get_coin(&outpoint(2, 0)), None, "spent by block 3");
    assert_eq!(spec.get_coin(&outpoint(3, 1)), Some(coin(3, 200)));
    assert_eq!(
        spec.contains_nullifier_many(Pool::Orchard, &[nullifier(3, 0), nullifier(4, 0)]),
        vec![true, false]
    );
    // A direct commit is refused while speculative layers are on the tip.
    let Err(ChainError::SpeculativePending) = chain.push(layer(2, &chain)) else {
        panic!("push must wait for the speculative layers");
    };
    // Block 3 verifies first: nothing is committed until block 2 verifies too.
    assert!(chain.confirm(three).unwrap().is_empty());
    assert_eq!(chain.tip().height, 1);
    let committed = chain.confirm(two).unwrap();
    let heights: Vec<u32> = committed.iter().map(|l| l.height).collect();
    assert_eq!(heights, vec![2, 3]);
    assert_eq!(chain.tip().height, 3);
    assert_eq!(chain.speculative().count(), 0);
    let Err(ChainError::UnknownSpeculative(_)) = chain.confirm(two) else {
        panic!("a committed layer is no longer speculative");
    };
    // The index now holds the committed blocks.
    let view = chain.view();
    assert_eq!(
        view.get_coins(&[outpoint(2, 0), outpoint(3, 1)]),
        vec![None, Some(coin(3, 200))]
    );
    assert_eq!(
        view.get_coins(&[outpoint(2, 0), outpoint(3, 1)]),
        view.get_coins_by_walk(&[outpoint(2, 0), outpoint(3, 1)])
    );
}

#[test]
fn reject_drops_the_layer_and_its_descendants() {
    let (mut chain, _) = chain();
    let l = layer(1, &chain);
    chain.push(l).unwrap();
    let ids: Vec<SpecId> = (2..=4)
        .map(|h| {
            let l = speculative_layer(h, &chain);
            chain.push_speculative(l).unwrap()
        })
        .collect();
    let before = chain.view_speculative();
    // Block 4 verified, but block 3 fails: blocks 3 and 4 go, block 2 stays speculative.
    chain.confirm(ids[2]).unwrap();
    let dropped = chain.reject(ids[1]).unwrap();
    let heights: Vec<u32> = dropped.iter().map(|l| l.height).collect();
    assert_eq!(heights, vec![3, 4]);
    assert_eq!(chain.speculative_tip().height, 2);
    let Err(ChainError::UnknownSpeculative(_)) = chain.reject(ids[2]) else {
        panic!("a dropped layer is gone");
    };
    let view = chain.view_speculative();
    assert_eq!(
        view.get_coin(&outpoint(2, 0)),
        Some(coin(2, 100)),
        "block 3 is gone"
    );
    assert_eq!(
        view.contains_nullifier_many(Pool::Orchard, &[nullifier(3, 0)]),
        vec![false]
    );
    // A view taken before the reject keeps its own layers.
    assert_eq!(before.get_coin(&outpoint(2, 0)), None);
    // Another block 3 can follow block 2.
    let l = speculative_layer(3, &chain);
    let three = chain.push_speculative(l).unwrap();
    chain.confirm(ids[0]).unwrap();
    assert_eq!(chain.tip().height, 2);
    chain.confirm(three).unwrap();
    assert_eq!(chain.tip().height, 3);
    // A pop drops the speculative layers above the popped tip.
    let l = speculative_layer(4, &chain);
    chain.push_speculative(l).unwrap();
    assert_eq!(chain.pop().unwrap().height, 3);
    assert_eq!(chain.speculative().count(), 0);
    assert_eq!(chain.speculative_tip().height, 2);
}

#[test]
fn finalization_and_flush_never_touch_speculative_layers() {
    let (mut chain, backing) = chain();
    for h in 1..=3 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    let l = speculative_layer(4, &chain);
    let four = chain.push_speculative(l).unwrap();
    assert_eq!(chain.finalize_excess(0).unwrap(), 3);
    chain.flush().unwrap();
    assert_eq!(chain.base().read().height, 3);
    assert_eq!(
        *backing.best_block.lock(),
        Some(BestBlock {
            height: 3,
            hash: hash(3).0
        })
    );
    assert!(!backing.coins.lock().contains_key(&outpoint(4, 1)));
    // With an empty committed window, the speculative view walks block 4, then the base.
    let view = chain.view_speculative();
    assert_eq!(view.get_coin(&outpoint(4, 1)), Some(coin(4, 200)));
    assert_eq!(view.get_coin(&outpoint(3, 0)), None, "spent by block 4");
    assert_eq!(view.get_coin(&outpoint(3, 1)), Some(coin(3, 200)));
    chain.confirm(four).unwrap();
    assert_eq!(chain.view().get_coin(&outpoint(3, 0)), None);
}

mod index_against_walk {
    //! Random histories of push, pop and finalize over a small key universe: the window
    //! index answers exactly what a newest-first walk of the layers answers, for the
    //! current view and for views taken earlier in the history.

    use super::*;
    use proptest::prelude::*;

    const KEYS: u32 = 10;

    #[derive(Clone, Debug)]
    enum Op {
        /// Create the keys at `create` that are not unspent, spend `spends` picks among the
        /// unspent keys, reveal `nullifiers` picks, and spend `in_block` of the created keys
        /// in the same layer.
        Push {
            create: Vec<bool>,
            spends: Vec<u8>,
            in_block: Vec<u8>,
            nullifiers: Vec<u8>,
        },
        Pop,
        Finalize(usize),
        /// As `Push`, as a speculative layer on top of the speculative tip.
        PushSpeculative {
            create: Vec<bool>,
            spends: Vec<u8>,
            in_block: Vec<u8>,
            nullifiers: Vec<u8>,
        },
        /// Confirms the speculative layer at this pick.
        Confirm(u8),
        /// Rejects the speculative layer at this pick.
        Reject(u8),
    }

    fn body() -> impl Strategy<Value = (Vec<bool>, Vec<u8>, Vec<u8>, Vec<u8>)> {
        (
            proptest::collection::vec(any::<bool>(), KEYS as usize),
            proptest::collection::vec(0u8..255, 0..4),
            proptest::collection::vec(0u8..255, 0..2),
            proptest::collection::vec(0u8..255, 0..3),
        )
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            5 => body().prop_map(|(create, spends, in_block, nullifiers)| Op::Push {
                create,
                spends,
                in_block,
                nullifiers,
            }),
            2 => Just(Op::Pop),
            2 => (0usize..4).prop_map(Op::Finalize),
            4 => body().prop_map(|(create, spends, in_block, nullifiers)| Op::PushSpeculative {
                create,
                spends,
                in_block,
                nullifiers,
            }),
            3 => any::<u8>().prop_map(Op::Confirm),
            1 => any::<u8>().prop_map(Op::Reject),
        ]
    }

    /// A layer on top of `view` at `height`, its contents drawn from the picks.
    fn random_layer(
        view: &crate::ChainView,
        parent: BlockHash,
        block_hash: BlockHash,
        (create, spends, in_block, nullifiers): (&[bool], &[u8], &[u8], &[u8]),
    ) -> Layer {
        let outpoints = all_outpoints();
        let nfs = all_nullifiers();
        let height = view.tip_height() + 1;
        let state = view.get_coins_by_walk(&outpoints);
        let mut created = HashMap::default();
        for (k, flag) in create.iter().enumerate() {
            if matches!((flag, &state[k]), (true, None)) {
                created.insert(
                    outpoints[k].clone(),
                    coin(height, u64::from(height) * 100 + k as u64),
                );
            }
        }
        let unspent: Vec<&OutPoint> = outpoints
            .iter()
            .zip(&state)
            .filter_map(|(o, c)| c.as_ref().map(|_| o))
            .collect();
        let mut spent = HashSet::default();
        for pick in spends {
            if let Some(o) = unspent.get(usize::from(*pick) % unspent.len().max(1)) {
                spent.insert((*o).clone());
            }
        }
        let created_keys: Vec<OutPoint> = created.keys().cloned().collect();
        for pick in in_block {
            if let Some(o) = created_keys.get(usize::from(*pick) % created_keys.len().max(1)) {
                spent.insert(o.clone());
            }
        }
        let mut sets: [HashSet<[u8; 32], ahash::RandomState>; 4] = Default::default();
        for pick in nullifiers {
            let pool = match pick % 3 {
                0 => Pool::Orchard,
                1 => Pool::Sapling,
                _ => Pool::Ironwood,
            };
            sets[pool.index()].insert(nfs[usize::from(*pick) % nfs.len()]);
        }
        let frontiers = view.frontiers();
        Layer {
            height,
            hash: block_hash,
            parent,
            time: 1_000 + height * 75,
            bits: 0x1f07_ffff,
            wtxids: Vec::new(),
            created,
            spent,
            spent_coins: Vec::new(),
            nullifiers: sets,
            orchard_frontier: frontiers.orchard,
            sapling_frontier: frontiers.sapling,
            ironwood_frontier: frontiers.ironwood,
            sprout_frontier: frontiers.sprout,
            anchors: Anchors {
                sapling: [0; 32],
                orchard: [0; 32],
                ironwood: [0; 32],
            },
            value_pools: ValuePools::default(),
            history: None,
        }
    }

    fn all_outpoints() -> Vec<OutPoint> {
        (0..KEYS).map(|k| outpoint(0xffff, k)).collect()
    }

    fn all_nullifiers() -> Vec<[u8; 32]> {
        (0..KEYS).map(|k| nullifier(0xffff, k)).collect()
    }

    fn assert_agrees(view: &crate::ChainView) {
        let outpoints = all_outpoints();
        assert_eq!(
            view.get_coins(&outpoints),
            view.get_coins_by_walk(&outpoints)
        );
        let nfs = all_nullifiers();
        for pool in [Pool::Orchard, Pool::Sapling, Pool::Ironwood] {
            assert_eq!(
                view.contains_nullifier_many(pool, &nfs),
                view.contains_nullifier_many_by_walk(pool, &nfs)
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]
        #[test]
        fn index_equals_walk(ops in proptest::collection::vec(op(), 1..40)) {
            let (mut chain, _) = chain();
            let mut views: Vec<crate::ChainView> = Vec::new();
            // Every pushed block gets a distinct hash, as distinct blocks do: a pop followed
            // by a push at the same height is a different block.
            let mut pushed = 0u32;
            for op in ops {
                match op {
                    Op::Push { create, spends, in_block, nullifiers } => {
                        pushed += 1;
                        let l = random_layer(
                            &chain.view(),
                            chain.tip().hash,
                            hash(0x1000 + pushed),
                            (&create, &spends, &in_block, &nullifiers),
                        );
                        match (chain.push(l), chain.speculative().count()) {
                            (Ok(_), 0) | (Err(ChainError::SpeculativePending), 1..) => {}
                            (result, pending) => panic!("push with {pending} speculative: {:?}", result.map(|l| l.height)),
                        }
                    }
                    Op::PushSpeculative { create, spends, in_block, nullifiers } => {
                        pushed += 1;
                        let l = random_layer(
                            &chain.view_speculative(),
                            chain.speculative_tip().hash,
                            hash(0x1000 + pushed),
                            (&create, &spends, &in_block, &nullifiers),
                        );
                        chain.push_speculative(l).unwrap();
                    }
                    Op::Confirm(pick) => {
                        let ids: Vec<SpecId> = chain.speculative().map(|(id, _)| id).collect();
                        if let Some(id) = ids.get(usize::from(pick) % ids.len().max(1)) {
                            let before = chain.tip().height;
                            let committed = chain.confirm(*id).unwrap();
                            prop_assert_eq!(chain.tip().height, before + committed.len() as u32);
                        }
                    }
                    Op::Reject(pick) => {
                        let ids: Vec<SpecId> = chain.speculative().map(|(id, _)| id).collect();
                        if let Some(id) = ids.get(usize::from(pick) % ids.len().max(1)) {
                            let k = usize::from(pick) % ids.len();
                            let dropped = chain.reject(*id).unwrap();
                            prop_assert_eq!(dropped.len(), ids.len() - k);
                        }
                    }
                    Op::Pop => {
                        chain.pop();
                    }
                    Op::Finalize(window) => {
                        chain.finalize_excess(window).unwrap();
                    }
                }
                for view in [chain.view(), chain.view_speculative()] {
                    assert_agrees(&view);
                    views.push(view);
                }
                for old in &views {
                    assert_agrees(old);
                }
            }
            // Popping the whole window empties the index.
            while let Some(popped) = chain.pop() {
                drop(popped);
            }
            prop_assert_eq!(chain.index.read().coins_len(), 0);
            assert_agrees(&chain.view());
        }
    }
}

#[test]
fn restored_base_equals_the_finalized_base() {
    let (mut chain, backing) = chain();
    for h in 1..=5 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    chain.finalize_excess(0).unwrap();
    let (state, anchors) = {
        let mut base = chain.base().write();
        (base.state(), base.take_new_anchors())
    };
    // The three construction anchors, then one Sapling and one Orchard anchor per absorbed
    // layer. The Ironwood root of the test layers stays the empty root.
    assert_eq!(anchors.len(), 3 + 2 * 5);
    let again = chain.base().write().take_new_anchors();
    assert!(again.is_empty(), "the anchors are taken once");

    let restored = Base::restore(backing, state.clone(), anchors, Vec::new());
    assert_eq!(restored.height, 5);
    assert_eq!(restored.hash, hash(5));
    assert_eq!(restored.state().times, state.times);
    // The tip anchors are the roots of the frontiers (the test layers use synthetic ones).
    assert_eq!(
        restored.anchors.sapling,
        state.sapling_frontier.root().to_bytes()
    );
    for h in 1..=5u32 {
        assert!(restored.has_anchor(Pool::Orchard, &[0xa0u8.wrapping_add(h as u8); 32]));
        assert!(restored.has_anchor(Pool::Sapling, &[h as u8; 32]));
    }
    assert!(!restored.has_anchor(Pool::Orchard, &[0xa9; 32]));
    let mut restored = restored;
    assert!(restored.take_new_anchors().is_empty());
}

#[test]
fn difficulty_context_uses_layers_then_base() {
    use hayai_consensus::DIFFICULTY_CONTEXT_BLOCKS;
    let entry = |h: u32| (1_000 + h * 75, bits_of(h));
    let (mut chain, backing) = chain();
    // The bits of the base block are unknown, so the context is empty.
    assert_eq!(chain.view().difficulty_context(), Vec::new());
    // Its time is known.
    assert_eq!(chain.view().recent_times(), vec![1_000]);
    for h in 1..=5 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
    }
    let expected: Vec<(u32, u32)> = (1..=5).rev().map(entry).collect();
    assert_eq!(chain.view().difficulty_context(), expected);
    let times: Vec<u32> = (0..=5).rev().map(|h| 1_000 + h * 75).collect();
    assert_eq!(chain.view().recent_times(), times);
    for h in 6..=140 {
        let l = layer(h, &chain);
        chain.push(l).unwrap();
        chain.finalize_excess(10).unwrap();
    }
    // Ten layers, then the newest blocks of the base.
    assert_eq!(DIFFICULTY_CONTEXT_BLOCKS, 113);
    let expected: Vec<(u32, u32)> = (28..=140).rev().map(entry).collect();
    assert_eq!(chain.view().difficulty_context(), expected);
    chain.finalize_excess(0).unwrap();
    assert_eq!(chain.view().difficulty_context(), expected);
    // The base keeps no more than the context, and a restored base has the same context.
    let (state, anchors) = {
        let mut base = chain.base().write();
        (base.state(), base.take_new_anchors())
    };
    assert_eq!(state.times.len(), DIFFICULTY_CONTEXT_BLOCKS);
    assert_eq!(state.bits.len(), DIFFICULTY_CONTEXT_BLOCKS);
    let restored = Chain::new(Base::restore(backing, state, anchors, Vec::new()));
    assert_eq!(restored.view().difficulty_context(), expected);
    let times: Vec<u32> = expected.iter().map(|(time, _)| *time).collect();
    assert_eq!(restored.view().recent_times(), times);
    assert_eq!(restored.view().median_time_past(), 1_000 + 135 * 75);
}

/// A base that starts above the genesis block takes the times and the `nBits` of its
/// start state: the view then has the whole context of the next block.
#[test]
fn a_seeded_base_has_the_header_context_of_its_start_state() {
    use hayai_consensus::DIFFICULTY_CONTEXT_BLOCKS;
    let (mut chain, _backing) = chain();
    let times: Vec<u32> = (0..140).map(|i| 500 + i * 75).collect();
    let bits: Vec<u32> = (0..140).map(|i| 0x1c00_0000 + i).collect();
    chain.base().write().set_header_context(&times, &bits);
    // The newest 113 entries, newest first.
    let expected: Vec<(u32, u32)> = (27..140)
        .rev()
        .map(|i| (500 + i * 75, 0x1c00_0000 + i))
        .collect();
    assert_eq!(expected.len(), DIFFICULTY_CONTEXT_BLOCKS);
    assert_eq!(chain.view().difficulty_context(), expected);
    let newest_times: Vec<u32> = expected.iter().map(|(time, _)| *time).collect();
    assert_eq!(chain.view().recent_times(), newest_times);
    // A layer goes in front of the seeded context.
    let l = layer(1, &chain);
    let (time, layer_bits) = (l.time, l.bits);
    chain.push(l).unwrap();
    let context = chain.view().difficulty_context();
    assert_eq!(context[0], (time, layer_bits));
    assert_eq!(context[1..], expected[..DIFFICULTY_CONTEXT_BLOCKS - 1]);
    // Fewer `nBits` than times: the context ends with the last block that has both.
    chain
        .base()
        .write()
        .set_header_context(&times[..11], &bits[..3]);
    assert_eq!(chain.view().recent_times().len(), 12);
    assert_eq!(chain.view().difficulty_context().len(), 4);
}

#[test]
fn the_ironwood_state_follows_the_layers_into_the_base() {
    let (mut chain, backing) = chain();
    let empty = hayai_trees::IronwoodFrontier::empty();
    let empty_root = empty.root().to_bytes();
    // A new base holds the empty Ironwood tree, and its root is a valid anchor.
    assert_eq!(*chain.view().frontiers().ironwood, empty);
    assert_eq!(chain.view().frontiers().anchors.ironwood, empty_root);
    assert!(chain.view().has_anchor(Pool::Ironwood, &empty_root));
    assert_eq!(
        chain.base().write().take_new_anchors(),
        vec![
            (Pool::Sapling, SaplingFrontier::empty().root().to_bytes()),
            (Pool::Orchard, OrchardFrontier::empty().root().to_bytes()),
            (Pool::Ironwood, empty_root),
        ]
    );

    let mut frontier = empty.clone();
    let leaf = hayai_crypto::orchard::tree::MerkleHashOrchard::from_bytes(&[9u8; 32])
        .expect("a small value is a field element");
    let root = frontier.append_many(&[leaf]).expect("room").to_bytes();
    assert_ne!(root, empty_root);
    assert!(!chain.view().has_anchor(Pool::Ironwood, &root));
    let frontier = Arc::new(frontier);
    let mut l = layer(1, &chain);
    l.ironwood_frontier = frontier.clone();
    l.anchors.ironwood = root;
    l.nullifiers[Pool::Ironwood.index()].insert(nullifier(1, 7));
    l.value_pools.ironwood = 5;
    l.value_pools.deferred = 6;
    chain.push(l).unwrap();
    let view = chain.view();
    assert!(view.has_anchor(Pool::Ironwood, &root));
    // The anchor sets are per pool: the Ironwood root is not an Orchard anchor.
    assert!(!view.has_anchor(Pool::Orchard, &root));
    assert_eq!(view.frontiers().ironwood, frontier);
    assert_eq!(view.frontiers().anchors.ironwood, root);
    // The nullifier sets are per pool too, through the index and through the walk.
    let nf = [nullifier(1, 7)];
    assert_eq!(view.contains_nullifier_many(Pool::Ironwood, &nf), [true]);
    assert_eq!(view.contains_nullifier_many(Pool::Orchard, &nf), [false]);
    assert_eq!(
        view.contains_nullifier_many_by_walk(Pool::Ironwood, &nf),
        [true]
    );
    // A layer without an Ironwood commitment keeps the frontier and the root.
    let l = layer(2, &chain);
    assert_eq!(l.ironwood_frontier, frontier);
    assert_eq!(l.anchors.ironwood, root);
    // A pop removes the anchor and the nullifier.
    chain.pop().unwrap();
    assert!(!chain.view().has_anchor(Pool::Ironwood, &root));
    assert_eq!(
        chain.view().contains_nullifier_many(Pool::Ironwood, &nf),
        [false]
    );
    let mut l = layer(1, &chain);
    l.ironwood_frontier = frontier.clone();
    l.anchors.ironwood = root;
    l.nullifiers[Pool::Ironwood.index()].insert(nullifier(1, 7));
    l.value_pools.ironwood = 5;
    l.value_pools.deferred = 6;
    chain.push(l).unwrap();

    chain.finalize_excess(0).unwrap();
    let view = chain.view();
    assert!(view.has_anchor(Pool::Ironwood, &root));
    assert!(view.has_anchor(Pool::Ironwood, &empty_root));
    assert_eq!(view.contains_nullifier_many(Pool::Ironwood, &nf), [true]);
    assert_eq!(view.contains_nullifier_many(Pool::Orchard, &nf), [false]);
    let (state, anchors) = {
        let mut base = chain.base().write();
        (base.state(), base.take_new_anchors())
    };
    assert!(anchors.contains(&(Pool::Ironwood, root)));
    assert_eq!(state.ironwood_frontier, frontier);
    assert_eq!(state.value_pools.ironwood, 5);
    assert_eq!(state.value_pools.deferred, 6);
    // A restored base has the recorded anchors and the empty root, which is in every
    // base whether the record lists it or not.
    let restored = Base::restore(backing, state, anchors, Vec::new());
    assert!(restored.has_anchor(Pool::Ironwood, &root));
    assert!(restored.has_anchor(Pool::Ironwood, &empty_root));
    assert_eq!(restored.anchors.ironwood, root);
    assert_eq!(restored.ironwood_frontier, frontier);
    assert_eq!(restored.value_pools.ironwood, 5);
}

/// The Sprout frontier of a layer is a valid Sprout anchor with its tree, through the
/// window and in the base. The base records each new treestate once, and a restored base
/// has them. A base that does not know the Sprout state has no anchor.
#[test]
fn the_sprout_state_follows_the_layers_into_the_base() {
    use hayai_trees::SproutFrontier;

    let (mut chain, backing) = chain();
    let empty = SproutFrontier::empty();
    assert!(chain.view().sprout_known());
    assert_eq!(*chain.view().frontiers().sprout, empty);
    assert_eq!(
        chain.view().sprout_tree(&empty.root()).as_deref(),
        Some(&empty)
    );
    assert!(chain.view().has_anchor(Pool::Sprout, &empty.root()));

    let mut first = empty.clone();
    first.append_many(&[[1; 32], [2; 32]]).expect("room");
    let mut second = first.clone();
    second.append_many(&[[3; 32], [4; 32]]).expect("room");
    let (first, second) = (Arc::new(first), Arc::new(second));
    assert_eq!(chain.view().sprout_tree(&first.root()), None);

    let sprout_layer = |height: u32, tree: &Arc<SproutFrontier>, chain: &Chain| {
        let mut l = layer(height, chain);
        l.sprout_frontier = tree.clone();
        l.nullifiers[Pool::Sprout.index()].insert(nullifier(height, 7));
        l.value_pools.sprout = u64::from(height);
        l
    };
    chain.push(sprout_layer(1, &first, &chain)).unwrap();
    // A layer without a JoinSplit keeps the frontier of its parent.
    let l = layer(2, &chain);
    assert_eq!(l.sprout_frontier, first);
    chain.push(l).unwrap();
    chain.push(sprout_layer(3, &second, &chain)).unwrap();
    let view = chain.view();
    assert_eq!(view.frontiers().sprout, second);
    for tree in [&first, &second] {
        assert_eq!(view.sprout_tree(&tree.root()).as_ref(), Some(tree));
    }
    // The nullifier set is the Sprout set, and no other pool has the anchor.
    let nf = [nullifier(1, 7)];
    assert_eq!(view.contains_nullifier_many(Pool::Sprout, &nf), [true]);
    assert_eq!(view.contains_nullifier_many(Pool::Sapling, &nf), [false]);
    assert!(!view.has_anchor(Pool::Sapling, &first.root()));
    // A pop removes the treestate of the popped block only.
    chain.pop().unwrap();
    assert_eq!(chain.view().sprout_tree(&second.root()), None);
    assert_eq!(chain.view().sprout_tree(&first.root()), Some(first.clone()));
    chain.push(sprout_layer(3, &second, &chain)).unwrap();

    chain.finalize_excess(0).unwrap();
    let view = chain.view();
    for tree in [&Arc::new(empty.clone()), &first, &second] {
        assert_eq!(view.sprout_tree(&tree.root()).as_ref(), Some(tree));
    }
    assert_eq!(view.contains_nullifier_many(Pool::Sprout, &nf), [true]);
    let (state, anchors, trees) = {
        let mut base = chain.base().write();
        (
            base.state(),
            base.take_new_anchors(),
            base.take_new_sprout_trees(),
        )
    };
    // Three layers, two new treestates: the layer at height 2 kept the first one.
    assert_eq!(trees, vec![first.clone(), second.clone()]);
    assert!(chain.base().write().take_new_sprout_trees().is_empty());
    assert_eq!(state.sprout_frontier, Some(second.clone()));
    assert_eq!(state.value_pools.sprout, 3);

    chain.flush().unwrap();
    let restored = Base::restore(backing.clone(), state.clone(), anchors.clone(), trees);
    assert_eq!(restored.sprout_frontier, second);
    assert_eq!(restored.state().sprout_frontier, Some(second.clone()));
    let mut restored = Chain::new(restored);
    assert!(restored.base().write().take_new_sprout_trees().is_empty());
    for tree in [&Arc::new(empty.clone()), &first, &second] {
        assert_eq!(
            restored.view().sprout_tree(&tree.root()).as_ref(),
            Some(tree)
        );
    }
    // A treestate that the base holds is not recorded again.
    let l = sprout_layer(4, &first, &restored);
    restored.push(l).unwrap();
    restored.finalize_excess(0).unwrap();
    assert!(restored.base().write().take_new_sprout_trees().is_empty());

    // A state without the Sprout frontier: the base does not know the Sprout state.
    let unknown = crate::BaseState {
        sprout_frontier: None,
        ..state
    };
    let base = Base::restore(backing, unknown, anchors, vec![first.clone()]);
    assert_eq!(base.state().sprout_frontier, None);
    let view = Chain::new(base).view();
    assert!(!view.sprout_known());
    assert_eq!(view.sprout_tree(&first.root()), None);
    assert_eq!(view.sprout_tree(&empty.root()), None);
    assert!(!view.has_anchor(Pool::Sprout, &empty.root()));
}
