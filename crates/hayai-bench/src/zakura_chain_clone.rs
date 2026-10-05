//! Model of Zakura's non-finalized `Chain` commit cost: the whole-chain deep clone.
//!
//! Zakura keeps the non-finalized chain as one `Chain` struct holding plain `HashMap`s of
//! every index for the window (`zakura-state/src/service/non_finalized_state/chain.rs:100`
//! `height_by_hash`, `:103` `tx_loc_by_hash`, `:121` `spent_utxos`, `:230-239` the per-pool
//! nullifier maps). A commit takes the chain out of its `Arc` and clones it when a reader
//! still holds a snapshot (`non_finalized_state.rs:837` `Arc::unwrap_or_clone`, `:364`
//! `Arc::make_mut`), so every block push copies every map. This model holds the three
//! largest maps at the sizes the audit measured for a 1,000-block window (4 x 50k
//! nullifiers, 1M spent outpoints, 1M transaction locations) and clones them per push.
//! Zakura's map value types are replaced by fixed-size stand-ins of the same size class.

use std::collections::{HashMap, VecDeque};

use hayai_crypto::zcash_transparent::bundle::OutPoint;

/// Entries one block contributes to the indexes.
#[derive(Clone, Debug)]
pub struct BlockEntries {
    pub height: u32,
    /// Per pool, in `Pool::ALL` order.
    pub nullifiers: [Vec<[u8; 32]>; 4],
    pub spent: Vec<OutPoint>,
    pub txids: Vec<[u8; 32]>,
}

/// Rows a block adds to each index.
#[derive(Clone, Copy, Debug)]
pub struct BlockShape {
    pub nullifiers_per_pool: usize,
    pub spent: usize,
    pub txs: usize,
}

impl BlockShape {
    /// The audit's 1,000-block window divided per block.
    pub const AUDIT: Self = Self {
        nullifiers_per_pool: 50,
        spent: 1_000,
        txs: 1_000,
    };

    /// A typical Zcash block: a few hundred transparent outputs and inputs plus some
    /// nullifiers (the design review's "200 inputs, 50 nullifiers").
    pub const TYPICAL: Self = Self {
        nullifiers_per_pool: 12,
        spent: 300,
        txs: 300,
    };
}

fn key(tag: u8, height: u32, i: usize) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[0] = tag;
    k[1..5].copy_from_slice(&height.to_le_bytes());
    k[5..13].copy_from_slice(&(i as u64).to_le_bytes());
    k
}

impl BlockEntries {
    pub fn synthetic(height: u32, shape: BlockShape) -> Self {
        let nullifiers = [0u8, 1, 2, 3].map(|pool| {
            (0..shape.nullifiers_per_pool)
                .map(|i| key(0x10 + pool, height, i))
                .collect()
        });
        Self {
            height,
            nullifiers,
            spent: (0..shape.spent)
                .map(|i| OutPoint::new(key(0x20, height, i), 0))
                .collect(),
            txids: (0..shape.txs).map(|i| key(0x30, height, i)).collect(),
        }
    }
}

/// Zakura's `Chain` indexes, cloned on every push.
#[derive(Clone)]
pub struct ZakuraChainClone {
    nullifiers: [HashMap<[u8; 32], u32>; 4],
    spent_utxos: HashMap<OutPoint, u32>,
    tx_loc_by_hash: HashMap<[u8; 32], (u32, u32)>,
    blocks: VecDeque<BlockEntries>,
}

impl ZakuraChainClone {
    /// A chain holding `blocks` synthetic blocks of `shape`.
    pub fn with_blocks(blocks: usize, shape: BlockShape) -> Self {
        let mut chain = Self {
            nullifiers: Default::default(),
            spent_utxos: HashMap::new(),
            tx_loc_by_hash: HashMap::new(),
            blocks: VecDeque::new(),
        };
        for h in 0..blocks {
            chain.insert(BlockEntries::synthetic(h as u32, shape));
        }
        chain
    }

    pub fn next_height(&self) -> u32 {
        self.blocks.back().map_or(0, |b| b.height + 1)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    fn insert(&mut self, block: BlockEntries) {
        for (pool, nfs) in block.nullifiers.iter().enumerate() {
            self.nullifiers[pool].extend(nfs.iter().map(|nf| (*nf, block.height)));
        }
        self.spent_utxos
            .extend(block.spent.iter().map(|o| (o.clone(), block.height)));
        self.tx_loc_by_hash.extend(
            block
                .txids
                .iter()
                .enumerate()
                .map(|(i, t)| (*t, (block.height, i as u32))),
        );
        self.blocks.push_back(block);
    }

    fn remove_oldest(&mut self) {
        let Some(block) = self.blocks.pop_front() else {
            return;
        };
        for (pool, nfs) in block.nullifiers.iter().enumerate() {
            for nf in nfs {
                self.nullifiers[pool].remove(nf);
            }
        }
        for o in &block.spent {
            self.spent_utxos.remove(o);
        }
        for t in &block.txids {
            self.tx_loc_by_hash.remove(t);
        }
    }

    /// Commits a block the way Zakura does while a snapshot is alive: clone the whole chain,
    /// insert into the clone, finalize the oldest block when the window is exceeded, and
    /// replace the chain.
    pub fn push_block(&mut self, block: BlockEntries, window: usize) {
        let mut next = self.clone();
        next.insert(block);
        if next.blocks.len() > window {
            next.remove_oldest();
        }
        *self = next;
    }
}
