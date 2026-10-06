//! Retained-bytes block and transaction model.
//!
//! The crate parses a block or transaction exactly once. It keeps the wire bytes for the life
//! of the object as a [`Bytes`] slice. Hashing, storage, relay and serving all read the
//! original bytes. The crate never serializes an object again.
//!
//! Contract (see `docs/architecture.md`, section hayai-wire):
//!
//! - [`RawTx::parse`] parses one transaction from a byte slice with
//!   `zcash_primitives::transaction::Transaction::read`. It keeps the slice that the parser
//!   consumed. It computes the txid and the ZIP 244 authorizing-data digest.
//! - [`RawBlock::parse`] parses a block in this order: the header, the transaction count,
//!   then every transaction boundary with the layout scanner ([`tx_wire_len`]), then the
//!   transactions in parallel with `Transaction::read` on their slices. Therefore every
//!   [`RawTx::bytes`] is a sub-slice of [`RawBlock::bytes`]. [`RawBlock::parse_sequential`]
//!   is the one-pass reference implementation. The tests compare the parallel path against it.
//! - The crate hashes the authorizing-data digest from the byte ranges that the scanner
//!   recorded. It does not traverse the parsed transaction a second time. The crate cannot
//!   remove the txid computation from the upstream parser: `Transaction::read` computes it
//!   unconditionally (`HashReader` for v1–v4, `from_data_v5`/`from_data_v6` for v5 and v6;
//!   zcash_primitives-0.30.1 `src/transaction/mod.rs:736-746`, `795-797`, `712-728`). The
//!   parser exposes no constructor that takes a precomputed txid.
//! - [`merkle_root`] and [`auth_data_root`] hash in parallel above a small size threshold.
//!   [`block_commitments`] folds the auth data root into the header's `hashBlockCommitments`.
//! - [`merkle_branch_first`] and [`auth_branch_first`] are the branches of position 0, so a
//!   body keeps its roots' work when only the coinbase changes.
//! - [`order`] is the canonical block order: parents first, then txid.
//! - [`header`] holds the header hashing, the PoW target checks and the Equihash verification
//!   with the network's parameters ([`header::PowParams`]).
//!
//! Everything here is context-free.

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::io::{self, Cursor};
use std::sync::Arc;

use bytes::Bytes;
use hayai_crypto::{zcash_encoding, zcash_primitives, zcash_protocol};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use zcash_encoding::CompactSize;
use zcash_primitives::transaction::{Transaction, TxId, TxVersion};
use zcash_protocol::consensus::BranchId;

pub mod header;
pub mod order;
mod scan;

pub use order::{canonical_order, canonical_order_of, is_canonical, spent_txids, OrderCycle};
pub use scan::tx_wire_len;

/// Maximum serialized block size (consensus rule `MAX_BLOCK_SIZE`).
pub const MAX_BLOCK_BYTES: usize = 2_000_000;

/// Smallest possible serialized transaction. The parser uses it only to bound the capacity
/// that it reserves for the transaction vector before parsing.
const MIN_TX_BYTES: usize = 10;

/// Authorizing-data digest of transactions that predate ZIP 244 (ZIP 239).
pub const PRE_V5_AUTH_DIGEST: [u8; 32] = [0xff; 32];

/// ZIP 239 transaction identifier: txid plus authorizing-data digest.
///
/// For v4 and earlier transactions, the authorizing-data digest is all ones (ZIP 239).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct WtxId {
    pub txid: TxId,
    pub auth_digest: [u8; 32],
}

impl WtxId {
    /// The 64-byte encoding `txid || auth_digest`. It is the short-id preimage.
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(self.txid.as_ref());
        out[32..].copy_from_slice(&self.auth_digest);
        out
    }
}

/// A parsed transaction together with the exact bytes that the parser read it from.
#[derive(Clone, Debug)]
pub struct RawTx {
    /// The wire bytes of this transaction only.
    pub bytes: Bytes,
    /// The parsed transaction.
    pub tx: Arc<Transaction>,
    pub txid: TxId,
    pub auth_digest: [u8; 32],
}

impl RawTx {
    /// Spec §7.1.1, ZIP 239: the wtxid is the txid and the auth digest.
    pub fn wtxid(&self) -> WtxId {
        WtxId {
            txid: self.txid,
            auth_digest: self.auth_digest,
        }
    }

    /// `valueBalanceSapling` of a v4 transaction that has no Sapling spends and no outputs.
    /// `None` for every other transaction. Protocol specification §7.1.2 requires the value
    /// to be zero in that case. Upstream `Transaction::read_v4` discards the value. The parser
    /// therefore reads it back from the wire bytes for the consensus check.
    pub fn v4_value_balance_without_components(&self) -> Option<i64> {
        let (TxVersion::V4, None) = (self.tx.version(), self.tx.sapling_bundle()) else {
            return None;
        };
        Some(scan::v4_value_balance(&self.bytes).expect("bytes parsed as a v4 transaction"))
    }

    /// Parses one transaction that occupies the whole of `bytes`.
    ///
    /// `branch_id` is the consensus branch id that the parser uses to interpret the transaction
    /// version. It comes from the block height (or the current tip height for mempool
    /// transactions).
    pub fn parse(bytes: Bytes, branch_id: BranchId) -> Result<Self, ParseError> {
        let mut cursor = Cursor::new(&bytes[..]);
        let tx = Transaction::read(&mut cursor, branch_id)?;
        if cursor.position() as usize != bytes.len() {
            return Err(ParseError::Trailing);
        }
        let layout = scan::scan(&bytes)?;
        Self::from_parsed_layout(bytes, tx, &layout)
    }

    /// Parses the transaction that occupies exactly `bytes`. The scanner already scanned its
    /// layout.
    fn parse_with_layout(
        bytes: Bytes,
        layout: &scan::TxLayout,
        branch_id: BranchId,
    ) -> Result<Self, ParseError> {
        let mut cursor = Cursor::new(&bytes[..]);
        let tx = Transaction::read(&mut cursor, branch_id)?;
        if cursor.position() as usize != bytes.len() {
            return Err(layout_mismatch());
        }
        Self::from_parsed_layout(bytes, tx, layout)
    }

    fn from_parsed_layout(
        bytes: Bytes,
        tx: Transaction,
        layout: &scan::TxLayout,
    ) -> Result<Self, ParseError> {
        if layout.len != bytes.len() {
            return Err(layout_mismatch());
        }
        let auth_digest = scan::auth_digest(&bytes, layout)?;
        Ok(Self {
            bytes,
            txid: tx.txid(),
            tx: Arc::new(tx),
            auth_digest,
        })
    }

    /// Reference construction: txid and authorizing digest from the parsed form only.
    fn from_parsed(bytes: Bytes, tx: Transaction) -> Self {
        let txid = tx.txid();
        let auth_digest = match tx.version() {
            TxVersion::Sprout(_) | TxVersion::V3 | TxVersion::V4 => PRE_V5_AUTH_DIGEST,
            TxVersion::V5 | TxVersion::V6 => tx
                .auth_commitment()
                .as_bytes()
                .try_into()
                .expect("BLAKE2b-256 digest is 32 bytes"),
        };
        Self {
            bytes,
            tx: Arc::new(tx),
            txid,
            auth_digest,
        }
    }
}

/// A parsed block together with its wire bytes. The bytes of every transaction are
/// sub-slices.
#[derive(Clone, Debug)]
pub struct RawBlock {
    pub bytes: Bytes,
    pub header: header::BlockHeader,
    pub txs: Vec<RawTx>,
}

impl RawBlock {
    /// Parses a whole block. It rejects blocks over [`MAX_BLOCK_BYTES`], blocks without
    /// transactions, and trailing bytes after the last transaction.
    ///
    /// The layout scanner finds the transaction boundaries first (length arithmetic only).
    /// The parser then parses the transactions in parallel on their slices. It parses blocks
    /// with at most [`PARALLEL_PARSE_THRESHOLD`] transactions on the calling thread.
    pub fn parse(bytes: Bytes, branch_id: BranchId) -> Result<Self, ParseError> {
        let (header, count, body_start) = Self::parse_prefix(&bytes)?;
        let mut layouts = Vec::with_capacity(count.min((bytes.len() - body_start) / MIN_TX_BYTES));
        let mut pos = body_start;
        for _ in 0..count {
            let layout = scan::scan(&bytes[pos..])?;
            let end = pos + layout.len;
            // This thread makes the slices, and the workers do not. Every slice increments the
            // one shared refcount of `bytes`. On the workers, that refcount would be a contended
            // atomic.
            layouts.push((bytes.slice(pos..end), layout));
            pos = end;
        }
        if pos != bytes.len() {
            return Err(ParseError::Trailing);
        }
        let parse_one = |(tx_bytes, layout): (Bytes, scan::TxLayout)| {
            RawTx::parse_with_layout(tx_bytes, &layout, branch_id)
        };
        let txs = if count > PARALLEL_PARSE_THRESHOLD {
            // One exactly sized vector of results. The code does not use rayon's `Result`
            // collection, because that collection concatenates growing per-task vectors.
            let mut parsed = Vec::with_capacity(layouts.len());
            layouts
                .into_par_iter()
                .map(parse_one)
                .collect_into_vec(&mut parsed);
            let mut txs = Vec::with_capacity(parsed.len());
            for tx in parsed {
                txs.push(tx?);
            }
            txs
        } else {
            layouts
                .into_iter()
                .map(parse_one)
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self { bytes, header, txs })
    }

    /// The wire bytes of transaction `index` of the block `bytes`, without a parse: the
    /// layout scanner walks the transactions before it.
    pub fn tx_bytes(bytes: &Bytes, index: usize) -> Result<Bytes, ParseError> {
        let (_, count, mut pos) = Self::parse_prefix(bytes)?;
        if index >= count {
            return Err(ParseError::NoTransaction { index, count });
        }
        for _ in 0..index {
            pos += scan::tx_wire_len(&bytes[pos..])?;
        }
        let len = scan::tx_wire_len(&bytes[pos..])?;
        Ok(bytes.slice(pos..pos + len))
    }

    /// Reference implementation of [`RawBlock::parse`]: one sequential pass of
    /// `Transaction::read` over the block. The txid and the authorizing digest come from the
    /// parsed form (`Transaction::auth_commitment`). It gives the same result and the same
    /// errors, and it uses no scanner. It is the oracle for the parallel path and the
    /// benchmark baseline.
    pub fn parse_sequential(bytes: Bytes, branch_id: BranchId) -> Result<Self, ParseError> {
        let (header, count, body_start) = Self::parse_prefix(&bytes)?;
        let mut cursor = Cursor::new(&bytes[..]);
        cursor.set_position(body_start as u64);
        let mut parsed = Vec::with_capacity(count.min((bytes.len() - body_start) / MIN_TX_BYTES));
        for _ in 0..count {
            let start = cursor.position() as usize;
            let tx = Transaction::read(&mut cursor, branch_id)?;
            let end = cursor.position() as usize;
            parsed.push((bytes.slice(start..end), tx));
        }
        if cursor.position() as usize != bytes.len() {
            return Err(ParseError::Trailing);
        }
        let txs = if parsed.len() > PARALLEL_THRESHOLD {
            parsed
                .into_par_iter()
                .map(|(b, tx)| RawTx::from_parsed(b, tx))
                .collect()
        } else {
            parsed
                .into_iter()
                .map(|(b, tx)| RawTx::from_parsed(b, tx))
                .collect()
        };
        Ok(Self { bytes, header, txs })
    }

    /// Checks the size limit, the header and the transaction count. Returns the header, the
    /// count and the offset of the first transaction.
    fn parse_prefix(bytes: &Bytes) -> Result<(header::BlockHeader, usize, usize), ParseError> {
        // Spec §7.6: a block has at most 2,000,000 bytes.
        if bytes.len() > MAX_BLOCK_BYTES {
            return Err(ParseError::TooLarge(MAX_BLOCK_BYTES));
        }
        let header = header::BlockHeader::parse(bytes)?;
        let mut cursor = Cursor::new(&bytes[..]);
        cursor.set_position(header.serialized_len() as u64);
        let count: usize = CompactSize::read_t(&mut cursor).map_err(ParseError::TxCount)?;
        // Spec §7.6: a block has at least one transaction.
        if count == 0 {
            return Err(ParseError::Empty);
        }
        Ok((header, count, cursor.position() as usize))
    }

    pub fn hash(&self) -> header::BlockHash {
        self.header.hash()
    }

    pub fn txids(&self) -> Vec<TxId> {
        self.txs.iter().map(|t| t.txid).collect()
    }

    pub fn auth_digests(&self) -> Vec<[u8; 32]> {
        self.txs.iter().map(|t| t.auth_digest).collect()
    }
}

/// The hasher hashes levels with more nodes than this in parallel.
const PARALLEL_THRESHOLD: usize = 64;

/// [`RawBlock::parse`] parses blocks with more transactions than this in parallel.
pub const PARALLEL_PARSE_THRESHOLD: usize = 16;

fn layout_mismatch() -> ParseError {
    ParseError::Transaction(io::Error::new(
        io::ErrorKind::InvalidData,
        "transaction layout length differs from the parsed length",
    ))
}

fn sha256d_pair(pair: &[[u8; 32]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(pair[0]);
    h.update(pair[1]);
    Sha256::digest(h.finalize()).into()
}

fn auth_pair(pair: &[[u8; 32]]) -> [u8; 32] {
    blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashAuthDatHash")
        .to_state()
        .update(&pair[0])
        .update(&pair[1])
        .finalize()
        .as_bytes()
        .try_into()
        .expect("32-byte digest")
}

/// Reduces `level` pairwise with `f` until one node remains. It duplicates the last node of
/// an odd level (Bitcoin merkle rule; a no-op for the power-of-two auth-data tree).
fn reduce(mut level: Vec<[u8; 32]>, f: fn(&[[u8; 32]]) -> [u8; 32]) -> [u8; 32] {
    assert!(!level.is_empty(), "merkle tree of zero leaves");
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(level[level.len() - 1]);
        }
        level = if level.len() > PARALLEL_THRESHOLD {
            level.par_chunks(2).map(f).collect()
        } else {
            level.chunks(2).map(f).collect()
        };
    }
    level[0]
}

/// Merkle root of transaction ids (double SHA-256, Bitcoin-style duplication of the last node
/// on odd levels). It panics on an empty slice, because a block always has a coinbase.
pub fn merkle_root(txids: &[TxId]) -> [u8; 32] {
    reduce(txids.iter().map(|t| *t.as_ref()).collect(), sha256d_pair)
}

/// The first txid that `txids` holds twice, in block order of the second occurrence.
///
/// [`merkle_root`] duplicates the last node of an odd level, so the list `[a, b, c]` and
/// the list `[a, b, c, c]` have one root (CVE-2012-2459). A block whose list holds a txid
/// twice is not valid, and it can be a changed copy of a valid block with the same header.
/// A caller that compares the merkle root must also call this function, as Zakura does
/// (`zakura-consensus/src/block/check.rs:523-559`).
pub fn duplicate_txid(txids: &[TxId]) -> Option<TxId> {
    let mut seen = HashSet::with_capacity(txids.len());
    txids.iter().find(|txid| !seen.insert(**txid)).copied()
}

/// ZIP 244 authorizing-data commitment over the transactions of the block. It is a full
/// binary tree over the digests, padded with `[0u8; 32]` leaves to the next power of two.
/// The inner nodes use BLAKE2b-256 personalized `ZcashAuthDatHash`. It panics on an empty
/// slice.
pub fn auth_data_root(auth_digests: &[[u8; 32]]) -> [u8; 32] {
    assert!(
        !auth_digests.is_empty(),
        "auth_data_root of zero transactions"
    );
    let padded = auth_digests.len().next_power_of_two();
    let mut level = Vec::with_capacity(padded);
    level.extend_from_slice(auth_digests);
    level.resize(padded, [0u8; 32]);
    reduce(level, auth_pair)
}

/// The siblings of the path from leaf 0 to the root of a tree whose leaves are a first leaf
/// and then `rest`, bottom up. They do not depend on the first leaf, so a block body keeps
/// its branch while its coinbase changes ([`root_from_branch`]).
fn first_leaf_branch(mut level: Vec<[u8; 32]>, f: fn(&[[u8; 32]]) -> [u8; 32]) -> Vec<[u8; 32]> {
    let mut siblings = Vec::new();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(level[level.len() - 1]);
        }
        siblings.push(level[1]);
        level = if level.len() > PARALLEL_THRESHOLD {
            level.par_chunks(2).map(f).collect()
        } else {
            level.chunks(2).map(f).collect()
        };
    }
    siblings
}

/// The [`merkle_root`] branch of position 0 for a block whose transactions after the
/// coinbase have the txids `rest`.
pub fn merkle_branch_first(rest: &[TxId]) -> Vec<[u8; 32]> {
    let mut level = Vec::with_capacity(rest.len() + 1);
    level.push([0u8; 32]);
    level.extend(rest.iter().map(|t| *t.as_ref()));
    first_leaf_branch(level, sha256d_pair)
}

/// The [`auth_data_root`] branch of position 0 for a block whose transactions after the
/// coinbase have the auth digests `rest`.
pub fn auth_branch_first(rest: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let padded = (rest.len() + 1).next_power_of_two();
    let mut level = Vec::with_capacity(padded);
    level.push([0u8; 32]);
    level.extend_from_slice(rest);
    level.resize(padded, [0u8; 32]);
    first_leaf_branch(level, auth_pair)
}

/// The merkle root of a block from its coinbase txid and [`merkle_branch_first`].
pub fn merkle_root_from_branch(first: &TxId, branch: &[[u8; 32]]) -> [u8; 32] {
    root_from_branch(*first.as_ref(), branch, sha256d_pair)
}

/// The auth data root of a block from its coinbase auth digest and [`auth_branch_first`].
pub fn auth_root_from_branch(first: &[u8; 32], branch: &[[u8; 32]]) -> [u8; 32] {
    root_from_branch(*first, branch, auth_pair)
}

fn root_from_branch(
    first: [u8; 32],
    branch: &[[u8; 32]],
    f: fn(&[[u8; 32]]) -> [u8; 32],
) -> [u8; 32] {
    branch
        .iter()
        .fold(first, |node, sibling| f(&[node, *sibling]))
}

/// ZIP 244 `hashBlockCommitments`: BLAKE2b-256 personalized `ZcashBlockCommit` over the
/// ZIP 221 chain history root, the auth data root and a 32-byte zero terminator.
pub fn block_commitments(chain_history_root: &[u8; 32], auth_data_root: &[u8; 32]) -> [u8; 32] {
    blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashBlockCommit")
        .to_state()
        .update(chain_history_root)
        .update(auth_data_root)
        .update(&[0u8; 32])
        .finalize()
        .as_bytes()
        .try_into()
        .expect("32-byte digest")
}

/// Lookup of transactions by [`WtxId`]. The prepared-transaction store implements it. The
/// relay layer uses it for compact-block reconstruction.
pub trait TxLookup: Send + Sync {
    fn get(&self, id: &WtxId) -> Option<Arc<RawTx>>;
    /// Visits every stored id. The relay layer uses it to build the per-block short-id index.
    fn for_each_id(&self, f: &mut dyn FnMut(&WtxId));
    /// Visits every id that the relay announces in the answer to a `mempool` message when
    /// the next block has height `next_height`. ZIP 204: a store leaves out a transaction
    /// that expires within 3 blocks. The default visits every id.
    fn for_each_relay_id(&self, _next_height: u32, f: &mut dyn FnMut(&WtxId)) {
        self.for_each_id(f)
    }
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ParseError {
    #[error("transaction: {0}")]
    Transaction(#[from] std::io::Error),
    #[error("transaction count: {0}")]
    TxCount(std::io::Error),
    #[error("block has no transactions")]
    Empty,
    #[error("block header: {0}")]
    Header(String),
    #[error("block exceeds {0} bytes")]
    TooLarge(usize),
    #[error("trailing bytes after block")]
    Trailing,
    #[error("block has {count} transactions and no transaction {index}")]
    NoTransaction { index: usize, count: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn sequential_merkle(leaves: &[[u8; 32]]) -> [u8; 32] {
        let mut level = leaves.to_vec();
        while level.len() > 1 {
            let mut next = Vec::new();
            for i in (0..level.len()).step_by(2) {
                let right = if i + 1 < level.len() {
                    level[i + 1]
                } else {
                    level[i]
                };
                next.push(sha256d_pair(&[level[i], right]));
            }
            level = next;
        }
        level[0]
    }

    fn sequential_auth(leaves: &[[u8; 32]]) -> [u8; 32] {
        let mut level = leaves.to_vec();
        level.resize(leaves.len().next_power_of_two(), [0u8; 32]);
        while level.len() > 1 {
            level = level.chunks(2).map(auth_pair).collect();
        }
        level[0]
    }

    proptest! {
        #[test]
        fn merkle_root_matches_sequential(
            leaves in proptest::collection::vec(any::<[u8; 32]>(), 1..300)
        ) {
            let txids: Vec<TxId> = leaves.iter().map(|l| TxId::from_bytes(*l)).collect();
            prop_assert_eq!(merkle_root(&txids), sequential_merkle(&leaves));
        }

        #[test]
        fn auth_data_root_matches_sequential(
            leaves in proptest::collection::vec(any::<[u8; 32]>(), 1..300)
        ) {
            prop_assert_eq!(auth_data_root(&leaves), sequential_auth(&leaves));
        }

        #[test]
        fn first_leaf_branches_give_the_roots(
            leaves in proptest::collection::vec(any::<[u8; 32]>(), 1..300)
        ) {
            let txids: Vec<TxId> = leaves.iter().map(|l| TxId::from_bytes(*l)).collect();
            let merkle = merkle_branch_first(&txids[1..]);
            prop_assert_eq!(merkle_root_from_branch(&txids[0], &merkle), merkle_root(&txids));
            let auth = auth_branch_first(&leaves[1..]);
            prop_assert_eq!(auth_root_from_branch(&leaves[0], &auth), auth_data_root(&leaves));
        }

        #[test]
        fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..2000)) {
            let _ = RawTx::parse(Bytes::from(bytes.clone()), BranchId::Nu5);
            let _ = RawBlock::parse(Bytes::from(bytes), BranchId::Nu5);
        }
    }

    #[test]
    fn single_leaf_roots_are_the_leaf() {
        let leaf = [7u8; 32];
        assert_eq!(merkle_root(&[TxId::from_bytes(leaf)]), leaf);
        assert_eq!(auth_data_root(&[leaf]), leaf);
    }

    #[test]
    fn block_commitments_is_personalized_blake2b_over_both_roots() {
        let history = [0x11u8; 32];
        let auth = [0x22u8; 32];
        let mut preimage = history.to_vec();
        preimage.extend_from_slice(&auth);
        preimage.extend_from_slice(&[0u8; 32]);
        let expected = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(b"ZcashBlockCommit")
            .hash(&preimage);
        assert_eq!(block_commitments(&history, &auth), expected.as_bytes());
        assert_ne!(
            block_commitments(&history, &auth),
            block_commitments(&auth, &history)
        );
    }

    #[test]
    fn auth_data_root_padding_is_zero_leaves() {
        let three = [[0x42u8; 32], [0xaa; 32], [0x77; 32]];
        let four = [[0x42u8; 32], [0xaa; 32], [0x77; 32], [0; 32]];
        assert_eq!(auth_data_root(&three), auth_data_root(&four));
    }

    #[test]
    fn wtxid_bytes_are_txid_then_digest() {
        let id = WtxId {
            txid: TxId::from_bytes([1; 32]),
            auth_digest: [2; 32],
        };
        let b = id.to_bytes();
        assert_eq!(&b[..32], &[1; 32]);
        assert_eq!(&b[32..], &[2; 32]);
    }

    #[test]
    fn oversized_block_is_rejected_before_parsing() {
        let bytes = Bytes::from(vec![0u8; MAX_BLOCK_BYTES + 1]);
        assert!(matches!(
            RawBlock::parse(bytes, BranchId::Nu5),
            Err(ParseError::TooLarge(MAX_BLOCK_BYTES))
        ));
    }
}
