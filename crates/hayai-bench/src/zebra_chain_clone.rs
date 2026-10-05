//! Model of upstream Zebra's non-finalized `Chain` commit cost: deep clones of the whole
//! chain.
//!
//! Source: `zebra-state` 14.0.0 (the release on `zebra-chain` 13.0.1; 15.0.0 has the same
//! `Chain` and the same clone sites).
//!
//! Every index of `ChainInner` (`src/service/non_finalized_state/chain.rs:86`) is a plain
//! `std` collection that the derived `Clone` copies. Zebra clones the chain twice per block
//! when the window is full:
//!
//! 1. `commit_block` takes the parent chain's `Arc` (`non_finalized_state.rs:871-874`), and
//!    `validate_and_update_parallel` calls `Arc::try_unwrap(..).unwrap_or_else(|c| (*c).clone())`
//!    (`:669-670`). The `chain_set` still holds the `Arc`, so this is always a deep clone
//!    (`chain.rs:65-68` says so).
//! 2. The write task sends the state to the watch channel, which keeps a snapshot
//!    (`write.rs:114`). When the chain is longer than `MAX_BLOCK_REORG_HEIGHT` (1,000,
//!    `write.rs:462-465`), `finalize` calls `Arc::make_mut` on the best chain
//!    (`non_finalized_state.rs:307`), which clones it again before it removes the root block.
//!
//! The model holds the maps that grow with the window, with Zebra's value types from
//! `zebra-chain` 13.0.1 where the clone cost depends on them (a `Script` is a heap `Vec`):
//!
//! - `blocks: BTreeMap<Height, ContextuallyVerifiedBlock>` (`chain.rs:90`), by value: each
//!   block's `new_outputs` and `spent_outputs` maps (`request.rs:332`, `:341`) are copied.
//! - `height_by_hash` (`:93`), `tx_loc_by_hash` (`:96`), `created_utxos` (`:106`),
//!   `spent_utxos` (`:111`, value `()` without the `indexer` feature), the four nullifier
//!   maps (`:216-225`, value `()`), `partial_transparent_transfers` (`:231`, per address a
//!   balance, a multiset of transaction ids, a `BTreeMap` of created outputs and a
//!   `BTreeSet` of spent locations: `chain/index.rs:21-55`) and `block_info_by_height`
//!   (`:251`).
//!
//! The tree, anchor and history maps add at most a few entries per block and are not
//! modelled. The block shape is [`BlockShape`] (nullifiers, spent outpoints, transactions),
//! the same as the Zakura model. Assumption: each transaction creates one P2PKH output to an
//! address that no other block touches, and that address entry also records one spent
//! location. This sets the address index to one entry per transaction. Real blocks reuse
//! addresses, which gives fewer entries with more items each; the clone cost of the index
//! depends on that mix and is an estimate here.
//!
//! Zakura shares the blocks, the created UTXOs and the address index through `Arc`s
//! (`zakura-state/.../chain.rs:97`, `:116`, `:248`), so its model clones only the other maps.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use zb_chain::block::Height;
use zb_chain::parameters::NetworkKind;
use zb_chain::transaction::Hash as TxHash;
use zb_chain::transparent::{Address, OrderedUtxo, OutPoint, Output, Script, Utxo};

use crate::zakura_chain_clone::BlockShape;

/// `OutputLocation`: transaction height, index in the block, output index.
type OutputLocation = (u32, u16, u32);

/// `TransparentTransfers` (`chain/index.rs:21-55`): the balance, the `MultiSet` of
/// transaction ids (a `HashMap<T, usize>`), the created outputs and the spent locations.
#[derive(Clone)]
struct TransparentTransfers {
    _balance: i64,
    _tx_ids: HashMap<TxHash, usize>,
    _created_utxos: BTreeMap<OutputLocation, Output>,
    _spent_utxos: BTreeSet<OutputLocation>,
}

/// `ContextuallyVerifiedBlock` (`request.rs:313`). The block itself and the transaction
/// hashes are behind `Arc`s; the two output maps are copied with the block.
#[derive(Clone)]
struct ContextuallyVerifiedBlock {
    _block: Arc<()>,
    hash: [u8; 32],
    height: u32,
    new_outputs: HashMap<OutPoint, OrderedUtxo>,
    spent_outputs: HashMap<OutPoint, OrderedUtxo>,
    transaction_hashes: Arc<[TxHash]>,
    _chain_value_pool_change: [i64; 6],
}

/// The entries one block adds, as Zebra's types.
pub struct ZebraBlockEntries {
    block: ContextuallyVerifiedBlock,
    nullifiers: [Vec<[u8; 32]>; 4],
    addresses: Vec<(Address, OutputLocation, Output, TxHash)>,
}

fn key(tag: u8, height: u32, i: usize) -> [u8; 32] {
    let mut k = [0u8; 32];
    k[0] = tag;
    k[1..5].copy_from_slice(&height.to_le_bytes());
    k[5..13].copy_from_slice(&(i as u64).to_le_bytes());
    k
}

/// The address of the P2PKH output that pays to the first 20 bytes of `hash`.
fn address(hash: &[u8; 32]) -> Address {
    let mut pkh = [0u8; 20];
    pkh.copy_from_slice(&hash[..20]);
    Address::from_pub_key_hash(NetworkKind::Mainnet, pkh)
}

fn p2pkh(hash: &[u8; 32]) -> (Address, Output) {
    let pkh = &hash[..20];
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(pkh);
    script.extend_from_slice(&[0x88, 0xac]);
    let output = Output {
        value: 1_000u64.try_into().expect("non-negative"),
        lock_script: Script::new(&script),
    };
    (address(hash), output)
}

fn ordered(output: Output, height: u32, tx_index: usize) -> OrderedUtxo {
    OrderedUtxo {
        utxo: Utxo {
            output,
            height: Height(height),
            from_coinbase: false,
        },
        tx_index_in_block: tx_index,
    }
}

impl ZebraBlockEntries {
    pub fn synthetic(height: u32, shape: BlockShape) -> Self {
        let txids: Vec<TxHash> = (0..shape.txs)
            .map(|i| TxHash(key(0x30, height, i)))
            .collect();
        let mut new_outputs = HashMap::with_capacity(shape.txs);
        let mut addresses = Vec::with_capacity(shape.txs);
        for (i, txid) in txids.iter().enumerate() {
            let (address, output) = p2pkh(&txid.0);
            new_outputs.insert(
                OutPoint {
                    hash: *txid,
                    index: 0,
                },
                ordered(output.clone(), height, i),
            );
            let location = (height, i as u16, 0);
            addresses.push((address, location, output, *txid));
        }
        let spent_outputs = (0..shape.spent)
            .map(|i| {
                let hash = key(0x20, height, i);
                let (_, output) = p2pkh(&hash);
                (
                    OutPoint {
                        hash: TxHash(hash),
                        index: 0,
                    },
                    ordered(output, height.saturating_sub(1), i),
                )
            })
            .collect();
        let nullifiers = [0u8, 1, 2, 3].map(|pool| {
            (0..shape.nullifiers_per_pool)
                .map(|i| key(0x10 + pool, height, i))
                .collect()
        });
        Self {
            block: ContextuallyVerifiedBlock {
                _block: Arc::new(()),
                hash: key(0x40, height, 0),
                height,
                new_outputs,
                spent_outputs,
                transaction_hashes: txids.into(),
                _chain_value_pool_change: [0; 6],
            },
            nullifiers,
            addresses,
        }
    }
}

/// The deep-cloned part of Zebra's `ChainInner`.
#[derive(Clone)]
pub struct ZebraChainClone {
    blocks: BTreeMap<u32, ContextuallyVerifiedBlock>,
    height_by_hash: HashMap<[u8; 32], u32>,
    tx_loc_by_hash: HashMap<TxHash, (u32, u16)>,
    created_utxos: HashMap<OutPoint, OrderedUtxo>,
    spent_utxos: HashMap<OutPoint, ()>,
    nullifiers: [HashMap<[u8; 32], ()>; 4],
    partial_transparent_transfers: HashMap<Address, TransparentTransfers>,
    block_info_by_height: BTreeMap<u32, ([i64; 6], u32)>,
    /// Nullifiers per pool of every block, to find the root block's nullifier keys.
    nullifiers_per_pool: usize,
}

impl ZebraChainClone {
    /// A chain holding `blocks` synthetic blocks of `shape`.
    pub fn with_blocks(blocks: usize, shape: BlockShape) -> Self {
        let mut chain = Self {
            blocks: BTreeMap::new(),
            height_by_hash: HashMap::new(),
            tx_loc_by_hash: HashMap::new(),
            created_utxos: HashMap::new(),
            spent_utxos: HashMap::new(),
            nullifiers: Default::default(),
            partial_transparent_transfers: HashMap::new(),
            block_info_by_height: BTreeMap::new(),
            nullifiers_per_pool: shape.nullifiers_per_pool,
        };
        for h in 0..blocks {
            chain.insert(ZebraBlockEntries::synthetic(h as u32, shape));
        }
        chain
    }

    pub fn next_height(&self) -> u32 {
        self.blocks.last_key_value().map_or(0, |(h, _)| h + 1)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    fn insert(&mut self, entries: ZebraBlockEntries) {
        let block = entries.block;
        let height = block.height;
        self.height_by_hash.insert(block.hash, height);
        for (i, txid) in block.transaction_hashes.iter().enumerate() {
            self.tx_loc_by_hash.insert(*txid, (height, i as u16));
        }
        self.created_utxos
            .extend(block.new_outputs.iter().map(|(o, u)| (*o, u.clone())));
        self.spent_utxos
            .extend(block.spent_outputs.keys().map(|o| (*o, ())));
        for (pool, nfs) in entries.nullifiers.iter().enumerate() {
            self.nullifiers[pool].extend(nfs.iter().map(|nf| (*nf, ())));
        }
        for (address, location, output, txid) in entries.addresses {
            let transfers = TransparentTransfers {
                _balance: 0,
                _tx_ids: HashMap::from([(txid, 1)]),
                _created_utxos: BTreeMap::from([(location, output)]),
                _spent_utxos: BTreeSet::from([(location.0, location.1, 1)]),
            };
            self.partial_transparent_transfers
                .insert(address, transfers);
        }
        self.block_info_by_height.insert(height, ([0; 6], 0));
        self.blocks.insert(height, block);
    }

    /// `Chain::pop_root`: removes the root block's entries from every index.
    fn remove_root(&mut self) {
        let Some((height, block)) = self.blocks.pop_first() else {
            return;
        };
        self.height_by_hash.remove(&block.hash);
        for txid in block.transaction_hashes.iter() {
            self.tx_loc_by_hash.remove(txid);
        }
        for o in block.new_outputs.keys() {
            self.created_utxos.remove(o);
            self.partial_transparent_transfers
                .remove(&address(&o.hash.0));
        }
        for o in block.spent_outputs.keys() {
            self.spent_utxos.remove(o);
        }
        for (pool, map) in (0u8..).zip(self.nullifiers.iter_mut()) {
            for i in 0..self.nullifiers_per_pool {
                map.remove(&key(0x10 + pool, height, i));
            }
        }
        self.block_info_by_height.remove(&height);
    }

    /// Commits a block the way Zebra does: clone the parent chain, push the block and, when
    /// the chain is longer than `window`, clone it again and remove the root block.
    pub fn push_block(&mut self, entries: ZebraBlockEntries, window: usize) {
        let mut next = self.clone();
        next.insert(entries);
        if next.blocks.len() > window {
            let mut finalized = next.clone();
            drop(next);
            finalized.remove_root();
            *self = finalized;
        } else {
            *self = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_keeps_the_window_and_removes_the_root_entries() {
        let shape = BlockShape {
            nullifiers_per_pool: 3,
            spent: 5,
            txs: 4,
        };
        let mut chain = ZebraChainClone::with_blocks(3, shape);
        assert_eq!(chain.len(), 3);
        for _ in 0..4 {
            let entries = ZebraBlockEntries::synthetic(chain.next_height(), shape);
            chain.push_block(entries, 3);
        }
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.next_height(), 7);
        assert_eq!(chain.height_by_hash.len(), 3);
        assert_eq!(chain.tx_loc_by_hash.len(), 3 * shape.txs);
        assert_eq!(chain.created_utxos.len(), 3 * shape.txs);
        assert_eq!(chain.spent_utxos.len(), 3 * shape.spent);
        for map in &chain.nullifiers {
            assert_eq!(map.len(), 3 * shape.nullifiers_per_pool);
        }
        assert_eq!(chain.partial_transparent_transfers.len(), 3 * shape.txs);
        assert_eq!(chain.block_info_by_height.len(), 3);
    }
}
