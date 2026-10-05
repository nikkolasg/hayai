//! Test doubles: an in-memory `TxLookup`, synthetic v5 transactions and blocks.

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use hayai_crypto::{zcash_encoding, zcash_primitives, zcash_protocol};
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::{RawBlock, RawTx, TxLookup, WtxId};
use zcash_primitives::transaction::{Authorized, TransactionData, TxId, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};

pub const BRANCH: BranchId = BranchId::Nu5;

pub fn wtxid(n: u8) -> WtxId {
    WtxId {
        txid: TxId::from_bytes([n; 32]),
        auth_digest: [n.wrapping_add(1); 32],
    }
}

/// A v5 transaction with no bundles whose expiry height is `n`, so distinct `n` give
/// distinct txids. Serialization and txid come from `zcash_primitives`.
pub fn make_tx(n: u32) -> Arc<RawTx> {
    let tx = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        BRANCH,
        0,
        BlockHeight::from_u32(n),
        None,
        None,
        None,
        None,
    )
    .freeze()
    .expect("v5 transaction with no bundles");
    let mut bytes = Vec::new();
    tx.write(&mut bytes).expect("write to Vec");
    let txid = tx.txid();
    let auth_digest: [u8; 32] = tx
        .auth_commitment()
        .as_bytes()
        .try_into()
        .expect("32 bytes");
    Arc::new(RawTx {
        bytes: Bytes::from(bytes),
        tx: Arc::new(tx),
        txid,
        auth_digest,
    })
}

#[derive(Default)]
pub struct MemStore {
    txs: HashMap<WtxId, Arc<RawTx>>,
    /// Ids without bytes, for tests that only need identities.
    ids: Vec<WtxId>,
}

impl MemStore {
    pub fn insert(&mut self, tx: Arc<RawTx>) {
        self.txs.insert(tx.wtxid(), tx);
    }

    pub fn insert_id(&mut self, id: WtxId) {
        self.ids.push(id);
    }

    pub fn remove(&mut self, id: &WtxId) {
        self.txs.remove(id);
    }
}

impl TxLookup for MemStore {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>> {
        self.txs.get(id).cloned()
    }

    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId)) {
        self.txs.keys().for_each(&mut *f);
        self.ids.iter().for_each(f);
    }

    fn len(&self) -> usize {
        self.txs.len() + self.ids.len()
    }
}

/// A block over `txs` whose header commits to their merkle root; the first transaction
/// plays the coinbase. Every transaction's bytes are sub-slices of the block bytes.
pub fn make_block(txs: &[Arc<RawTx>]) -> RawBlock {
    let txids: Vec<TxId> = txs.iter().map(|t| t.txid).collect();
    let header = BlockHeader {
        version: 4,
        prev_hash: BlockHash([0x33; 32]),
        merkle_root: hayai_wire::merkle_root(&txids),
        block_commitments: [0x44; 32],
        time: 1_700_000_000,
        bits: 0x1f07_ffff,
        nonce: [0x55; 32],
        solution: vec![0x66; PowParams::MAINNET.solution_len()],
    };
    let mut bytes = header.serialize();
    zcash_encoding::CompactSize::write(&mut bytes, txs.len()).expect("write to Vec");
    let mut ranges = Vec::with_capacity(txs.len());
    for tx in txs {
        let start = bytes.len();
        bytes.extend_from_slice(&tx.bytes);
        ranges.push(start..bytes.len());
    }
    let bytes = Bytes::from(bytes);
    let txs = txs
        .iter()
        .zip(ranges)
        .map(|(tx, range)| RawTx {
            bytes: bytes.slice(range),
            tx: tx.tx.clone(),
            txid: tx.txid,
            auth_digest: tx.auth_digest,
        })
        .collect();
    RawBlock { bytes, header, txs }
}
