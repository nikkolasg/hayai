//! The wallet index of hayaid: the transactions by id, the transparent addresses and the
//! completed note commitment subtrees of the committed chain. It serves
//! `getrawtransaction`, `getaddressbalance`, `getaddresstxids`, `getaddressutxos` and
//! `z_getsubtreesbyindex`.
//!
//! Storage: one RocksDB database with these column families. A location is
//! `height BE u32 || tx index BE u16` (6 bytes), so the keys that start with a height sort
//! in chain order.
//!
//! | Column family | Key | Value |
//! |---|---|---|
//! | `tx_loc` | txid (32) | location (6) |
//! | `tx_id` | location (6) | txid (32) |
//! | `addr_tx` | address (21) ‖ location (6) | empty |
//! | `addr_utxo` | address (21) ‖ height BE (4) ‖ txid (32) ‖ output index BE (4) | value LE u64 (8) |
//! | `addr_balance` | address (21) | balance LE i64 ‖ received LE i64 (16), merge operator add |
//! | `subtree` | pool (1) ‖ subtree index BE u16 (2) | end height BE u32 (4) ‖ root (32) |
//! | `undo` | height BE (4) | the undo record of the block |
//! | `meta` | `tip` | height LE u32 ‖ block hash (32) |
//!
//! An address is `kind || hash`: kind 0 is P2PKH, kind 1 is P2SH. Only these two script forms
//! have an address, as in Zakura.
//!
//! Write path. [`IndexWriter`] owns a thread and a bounded queue. The node sends each
//! committed block with the coins that its inputs spent ([`BlockJob`]), which the validation
//! read: the index reads no coin. The thread builds the entries of the queued blocks in
//! parallel and writes them in one write batch, in block order. Balances are merge operands,
//! so no write reads a value first. Each batch also writes the undo record of each block and
//! the tip.
//!
//! Durability. The writes go to the write-ahead log without a sync. A sync of the log runs
//! in the writer thread. The node calls [`IndexWriter::persist`] before each flush of its
//! coins: the call returns when a sync holds the base of the flush, and starts the next
//! sync in the background. So the durable index holds the base block of the coins store.
//! After a crash the node undoes the index blocks above the base
//! ([`WalletIndex::rewind_to`], from the undo records) and indexes the blocks that its
//! replay pushes. Each sync first removes the undo records at or below the base of the
//! flush before.

#![forbid(unsafe_code)]

mod writer;

pub use writer::{BlockJob, IndexWriter, TreesBefore, WriterStats, QUEUE_BLOCKS};

use std::path::Path;

use hayai_crypto::incrementalmerkletree::Level;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::sapling_crypto::Node;
use hayai_trees::{OrchardFrontier, SaplingFrontier};
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamilyDescriptor, DBCompressionType, Direction, IteratorMode,
    MergeOperands, Options, ReadOptions, SliceTransform, WriteBatch, DB,
};

const CF_TX_LOC: &str = "tx_loc";
const CF_TX_ID: &str = "tx_id";
const CF_ADDR_TX: &str = "addr_tx";
const CF_ADDR_UTXO: &str = "addr_utxo";
const CF_ADDR_BALANCE: &str = "addr_balance";
const CF_SUBTREE: &str = "subtree";
const CF_UNDO: &str = "undo";
const CF_META: &str = "meta";
const CFS: [&str; 8] = [
    CF_TX_LOC,
    CF_TX_ID,
    CF_ADDR_TX,
    CF_ADDR_UTXO,
    CF_ADDR_BALANCE,
    CF_SUBTREE,
    CF_UNDO,
    CF_META,
];
const KEY_TIP: &[u8] = b"tip";

/// The newest indexed block: its height and its hash.
pub type Tip = (u32, [u8; 32]);

/// The level of the note commitment subtrees (`z_getsubtreesbyindex`): 2^16 leaves.
pub const SUBTREE_LEVEL: u8 = 16;
const SUBTREE_LEAVES: u64 = 1 << SUBTREE_LEVEL;
/// The block cache that the column families share.
const BLOCK_CACHE_BYTES: usize = 64 << 20;

/// Bytes of an address key.
pub const ADDRESS_BYTES: usize = 21;
const LOC_BYTES: usize = 6;
const UTXO_KEY_BYTES: usize = ADDRESS_BYTES + 4 + 32 + 4;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("wallet index database: {0}")]
    Db(#[from] rocksdb::Error),
    #[error("wallet index is damaged: {0}")]
    Corrupt(String),
    #[error("{0}")]
    Chain(String),
    #[error("note commitment tree: {0}")]
    Tree(#[from] hayai_trees::TreeError),
}

/// A transparent address: `kind || hash`, kind 0 is P2PKH and kind 1 is P2SH.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AddressKey(pub [u8; ADDRESS_BYTES]);

impl AddressKey {
    pub fn p2pkh(hash: [u8; 20]) -> Self {
        Self::of(0, hash)
    }

    pub fn p2sh(hash: [u8; 20]) -> Self {
        Self::of(1, hash)
    }

    fn of(kind: u8, hash: [u8; 20]) -> Self {
        let mut key = [0; ADDRESS_BYTES];
        key[0] = kind;
        key[1..].copy_from_slice(&hash);
        Self(key)
    }

    /// The address of a P2PKH or a P2SH script. Another script has no address.
    pub fn of_script(script: &[u8]) -> Option<Self> {
        match script {
            [0x76, 0xa9, 0x14, hash @ .., 0x88, 0xac] if hash.len() == 20 => {
                Some(Self::p2pkh(hash.try_into().expect("20 bytes")))
            }
            [0xa9, 0x14, hash @ .., 0x87] if hash.len() == 20 => {
                Some(Self::p2sh(hash.try_into().expect("20 bytes")))
            }
            _ => None,
        }
    }

    pub fn is_p2sh(&self) -> bool {
        self.0[0] == 1
    }

    pub fn hash(&self) -> [u8; 20] {
        self.0[1..].try_into().expect("20 bytes")
    }

    /// The script of the address.
    pub fn script(&self) -> Vec<u8> {
        let hash = &self.0[1..];
        match self.is_p2sh() {
            true => [&[0xa9, 0x14][..], hash, &[0x87]].concat(),
            false => [&[0x76, 0xa9, 0x14][..], hash, &[0x88, 0xac]].concat(),
        }
    }
}

/// The place of a transaction in the committed chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxLoc {
    pub height: u32,
    pub index: u16,
}

impl TxLoc {
    fn encode(self) -> [u8; LOC_BYTES] {
        let mut out = [0; LOC_BYTES];
        out[..4].copy_from_slice(&self.height.to_be_bytes());
        out[4..].copy_from_slice(&self.index.to_be_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let Ok(bytes) = <[u8; LOC_BYTES]>::try_from(bytes) else {
            return Err(Error::Corrupt(format!(
                "a location of {} bytes",
                bytes.len()
            )));
        };
        Ok(Self {
            height: u32::from_be_bytes(bytes[..4].try_into().expect("4")),
            index: u16::from_be_bytes(bytes[4..].try_into().expect("2")),
        })
    }
}

/// The note commitment trees with subtrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtreePool {
    Sapling = 0,
    Orchard = 1,
    Ironwood = 2,
}

/// One completed subtree: its root (in the byte order of the tree) and the height of the
/// block that added its last leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subtree {
    pub index: u16,
    pub root: [u8; 32],
    pub end_height: u32,
}

/// An unspent output of an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utxo {
    pub address: AddressKey,
    pub txid: [u8; 32],
    pub index: u32,
    pub value: u64,
    pub height: u32,
}

/// The balance of a set of addresses, in zatoshis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Balance {
    pub balance: i64,
    /// The sum of all outputs that the addresses received.
    pub received: i64,
}

/// An output to an address that a block created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Created {
    address: AddressKey,
    tx: u16,
    index: u32,
    value: u64,
}

/// A coin of an address that a block spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spent {
    address: AddressKey,
    /// The height of the block that created the coin.
    height: u32,
    txid: [u8; 32],
    index: u32,
    value: u64,
    /// The transaction of the block that spent the coin.
    spender: u16,
}

/// What one block changes in the index. The buffers are reused for the next block.
#[derive(Default)]
pub(crate) struct Delta {
    height: u32,
    hash: [u8; 32],
    parent: [u8; 32],
    txids: Vec<[u8; 32]>,
    created: Vec<Created>,
    spent: Vec<Spent>,
    subtrees: Vec<(SubtreePool, Subtree)>,
    undo: Vec<u8>,
}

const CREATED_BYTES: usize = ADDRESS_BYTES + 2 + 4 + 8;
const SPENT_BYTES: usize = ADDRESS_BYTES + 4 + 32 + 4 + 8 + 2;
const SUBTREE_BYTES: usize = 1 + 2 + 32;

impl Delta {
    /// Fills the delta of `job`. `spent_coins[i][j]` is the coin of input `j` of
    /// transaction `i`.
    pub(crate) fn build(&mut self, job: &BlockJob) -> Result<(), Error> {
        let raw = &job.raw;
        if job.spent_coins.len() != raw.txs.len() {
            return Err(Error::Chain(format!(
                "block {}: {} spent coin lists for {} transactions",
                job.height,
                job.spent_coins.len(),
                raw.txs.len()
            )));
        }
        let Ok(_) = u16::try_from(raw.txs.len()) else {
            return Err(Error::Chain(format!(
                "block {} has {} transactions, above the u16 location",
                job.height,
                raw.txs.len()
            )));
        };
        self.height = job.height;
        self.hash = job.hash;
        self.parent = job.parent;
        self.txids.clear();
        self.created.clear();
        self.spent.clear();
        self.subtrees.clear();
        self.txids.reserve(raw.txs.len());
        for (i, (tx, coins)) in raw.txs.iter().zip(&job.spent_coins).enumerate() {
            let i = i as u16;
            self.txids.push(*tx.txid.as_ref());
            let Some(bundle) = tx.tx.transparent_bundle() else {
                continue;
            };
            for (n, out) in bundle.vout.iter().enumerate() {
                if let Some(address) = AddressKey::of_script(&out.script_pubkey().0 .0) {
                    self.created.push(Created {
                        address,
                        tx: i,
                        index: n as u32,
                        value: out.value().into_u64(),
                    });
                }
            }
            if bundle.is_coinbase() {
                continue;
            }
            if coins.len() != bundle.vin.len() {
                return Err(Error::Chain(format!(
                    "block {} transaction {i}: {} coins for {} inputs",
                    job.height,
                    coins.len(),
                    bundle.vin.len()
                )));
            }
            for (input, coin) in bundle.vin.iter().zip(coins) {
                if let Some(address) = AddressKey::of_script(&coin.script_pubkey) {
                    let prevout = input.prevout();
                    self.spent.push(Spent {
                        address,
                        height: coin.height,
                        txid: *prevout.hash(),
                        index: prevout.n(),
                        value: coin.value,
                        spender: i,
                    });
                }
            }
        }
        self.completed_subtrees(job)?;
        self.encode_undo();
        Ok(())
    }

    /// The subtrees that the note commitments of the block complete. The roots come from the
    /// frontier before the block and the leaves of the block up to the last leaf of each
    /// completed subtree. Only a block that crosses a multiple of 2^16 leaves does this work.
    fn completed_subtrees(&mut self, job: &BlockJob) -> Result<(), Error> {
        let txs = &job.raw.txs;
        let sapling = || {
            txs.iter()
                .filter_map(|t| t.tx.sapling_bundle())
                .flat_map(|b| b.shielded_outputs().iter().map(|o| Node::from_cmu(o.cmu())))
                .collect::<Vec<_>>()
        };
        let orchard = |ironwood: bool| {
            txs.iter()
                .filter_map(|t| match ironwood {
                    true => t.tx.ironwood_bundle(),
                    false => t.tx.orchard_bundle(),
                })
                .flat_map(|b| {
                    b.actions()
                        .iter()
                        .map(|a| MerkleHashOrchard::from_cmx(a.cmx()))
                })
                .collect::<Vec<_>>()
        };
        let count = |pool: SubtreePool| -> u64 {
            txs.iter()
                .map(|t| match pool {
                    SubtreePool::Sapling => {
                        t.tx.sapling_bundle()
                            .map_or(0, |b| b.shielded_outputs().len())
                    }
                    SubtreePool::Orchard => t.tx.orchard_bundle().map_or(0, |b| b.actions().len()),
                    SubtreePool::Ironwood => {
                        t.tx.ironwood_bundle().map_or(0, |b| b.actions().len())
                    }
                } as u64)
                .sum()
        };
        let trees = &job.trees_before;
        let size = trees.sapling.frontier().tree_size();
        let added = count(SubtreePool::Sapling);
        if crosses(size, added) {
            let leaves = sapling();
            for (index, prefix) in boundaries(size, added) {
                let mut frontier = SaplingFrontier::clone(&trees.sapling);
                frontier.append_many(&leaves[..prefix])?;
                let root = level_root(frontier.frontier().value())?.to_bytes();
                self.subtree(SubtreePool::Sapling, index, root, job.height)?;
            }
        }
        for (pool, before) in [
            (SubtreePool::Orchard, &trees.orchard),
            (SubtreePool::Ironwood, &trees.ironwood),
        ] {
            let size = before.frontier().tree_size();
            let added = count(pool);
            if !crosses(size, added) {
                continue;
            }
            let leaves = orchard(pool == SubtreePool::Ironwood);
            for (index, prefix) in boundaries(size, added) {
                let mut frontier = OrchardFrontier::clone(before);
                frontier.append_many(&leaves[..prefix])?;
                let root = level_root(frontier.frontier().value())?.to_bytes();
                self.subtree(pool, index, root, job.height)?;
            }
        }
        Ok(())
    }

    fn subtree(
        &mut self,
        pool: SubtreePool,
        index: u64,
        root: [u8; 32],
        end_height: u32,
    ) -> Result<(), Error> {
        let Ok(index) = u16::try_from(index) else {
            return Err(Error::Chain(format!("subtree index {index} above u16")));
        };
        self.subtrees.push((
            pool,
            Subtree {
                index,
                root,
                end_height,
            },
        ));
        Ok(())
    }

    /// The undo record: `hash || parent || created || spent || subtrees`, each list with a
    /// count.
    fn encode_undo(&mut self) {
        let out = &mut self.undo;
        out.clear();
        out.reserve(
            64 + 12
                + self.created.len() * CREATED_BYTES
                + self.spent.len() * SPENT_BYTES
                + self.subtrees.len() * SUBTREE_BYTES,
        );
        out.extend_from_slice(&self.hash);
        out.extend_from_slice(&self.parent);
        out.extend_from_slice(&(self.created.len() as u32).to_le_bytes());
        for c in &self.created {
            out.extend_from_slice(&c.address.0);
            out.extend_from_slice(&c.tx.to_le_bytes());
            out.extend_from_slice(&c.index.to_le_bytes());
            out.extend_from_slice(&c.value.to_le_bytes());
        }
        out.extend_from_slice(&(self.spent.len() as u32).to_le_bytes());
        for s in &self.spent {
            out.extend_from_slice(&s.address.0);
            out.extend_from_slice(&s.height.to_le_bytes());
            out.extend_from_slice(&s.txid);
            out.extend_from_slice(&s.index.to_le_bytes());
            out.extend_from_slice(&s.value.to_le_bytes());
            out.extend_from_slice(&s.spender.to_le_bytes());
        }
        out.extend_from_slice(&(self.subtrees.len() as u32).to_le_bytes());
        for (pool, t) in &self.subtrees {
            out.push(*pool as u8);
            out.extend_from_slice(&t.index.to_le_bytes());
            out.extend_from_slice(&t.root);
        }
    }

    /// Reads an undo record of `height` back into the delta. The txids come from the caller.
    fn decode_undo(&mut self, height: u32, bytes: &[u8]) -> Result<(), Error> {
        let corrupt = || Error::Corrupt(format!("the undo record of height {height}"));
        let mut r = Reader { bytes, at: 0 };
        self.height = height;
        self.hash = r.array().ok_or_else(corrupt)?;
        self.parent = r.array().ok_or_else(corrupt)?;
        self.created.clear();
        self.spent.clear();
        self.subtrees.clear();
        for _ in 0..r.u32().ok_or_else(corrupt)? {
            self.created.push(Created {
                address: AddressKey(r.array().ok_or_else(corrupt)?),
                tx: u16::from_le_bytes(r.array().ok_or_else(corrupt)?),
                index: r.u32().ok_or_else(corrupt)?,
                value: u64::from_le_bytes(r.array().ok_or_else(corrupt)?),
            });
        }
        for _ in 0..r.u32().ok_or_else(corrupt)? {
            self.spent.push(Spent {
                address: AddressKey(r.array().ok_or_else(corrupt)?),
                height: r.u32().ok_or_else(corrupt)?,
                txid: r.array().ok_or_else(corrupt)?,
                index: r.u32().ok_or_else(corrupt)?,
                value: u64::from_le_bytes(r.array().ok_or_else(corrupt)?),
                spender: u16::from_le_bytes(r.array().ok_or_else(corrupt)?),
            });
        }
        for _ in 0..r.u32().ok_or_else(corrupt)? {
            let pool = match r.array::<1>().ok_or_else(corrupt)? {
                [0] => SubtreePool::Sapling,
                [1] => SubtreePool::Orchard,
                [2] => SubtreePool::Ironwood,
                _ => return Err(corrupt()),
            };
            let index = u16::from_le_bytes(r.array().ok_or_else(corrupt)?);
            let root = r.array().ok_or_else(corrupt)?;
            self.subtrees.push((
                pool,
                Subtree {
                    index,
                    root,
                    end_height: height,
                },
            ));
        }
        match r.at == bytes.len() {
            true => Ok(()),
            false => Err(corrupt()),
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let out = self.bytes.get(self.at..self.at + N)?.try_into().ok()?;
        self.at += N;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }
}

/// Whether a tree of `size` leaves completes a subtree with `added` more leaves.
fn crosses(size: u64, added: u64) -> bool {
    added > 0 && (size + added) / SUBTREE_LEAVES > size / SUBTREE_LEAVES
}

/// The subtrees that `added` leaves after `size` complete: the subtree index and the number
/// of leaves of the block up to its last leaf.
fn boundaries(size: u64, added: u64) -> impl Iterator<Item = (u64, usize)> {
    (size / SUBTREE_LEAVES + 1..=(size + added) / SUBTREE_LEAVES)
        .map(move |end| (end - 1, (end * SUBTREE_LEAVES - size) as usize))
}

/// The root of the subtree of level 16 that ends at the last leaf of `frontier`.
fn level_root<H: hayai_crypto::incrementalmerkletree::Hashable + Clone>(
    frontier: Option<&hayai_crypto::incrementalmerkletree::frontier::NonEmptyFrontier<H>>,
) -> Result<H, Error> {
    let Some(frontier) = frontier else {
        return Err(Error::Chain("a completed subtree in an empty tree".into()));
    };
    Ok(frontier.root(Some(Level::from(SUBTREE_LEVEL))))
}

fn utxo_key(
    address: &AddressKey,
    height: u32,
    txid: &[u8; 32],
    index: u32,
) -> [u8; UTXO_KEY_BYTES] {
    let mut key = [0; UTXO_KEY_BYTES];
    key[..ADDRESS_BYTES].copy_from_slice(&address.0);
    key[ADDRESS_BYTES..ADDRESS_BYTES + 4].copy_from_slice(&height.to_be_bytes());
    key[ADDRESS_BYTES + 4..ADDRESS_BYTES + 36].copy_from_slice(txid);
    key[ADDRESS_BYTES + 36..].copy_from_slice(&index.to_be_bytes());
    key
}

fn addr_tx_key(address: &AddressKey, loc: TxLoc) -> [u8; ADDRESS_BYTES + LOC_BYTES] {
    let mut key = [0; ADDRESS_BYTES + LOC_BYTES];
    key[..ADDRESS_BYTES].copy_from_slice(&address.0);
    key[ADDRESS_BYTES..].copy_from_slice(&loc.encode());
    key
}

fn subtree_key(pool: SubtreePool, index: u16) -> [u8; 3] {
    let [a, b] = index.to_be_bytes();
    [pool as u8, a, b]
}

fn balance_value(balance: i64, received: i64) -> [u8; 16] {
    let mut out = [0; 16];
    out[..8].copy_from_slice(&balance.to_le_bytes());
    out[8..].copy_from_slice(&received.to_le_bytes());
    out
}

fn decode_balance(bytes: &[u8]) -> Option<Balance> {
    let bytes: [u8; 16] = bytes.try_into().ok()?;
    Some(Balance {
        balance: i64::from_le_bytes(bytes[..8].try_into().expect("8")),
        received: i64::from_le_bytes(bytes[8..].try_into().expect("8")),
    })
}

/// The merge operator of `addr_balance`: the sum of the operands. An operand of another
/// length is a damaged database, and the merge fails.
fn add_balances(_key: &[u8], existing: Option<&[u8]>, operands: &MergeOperands) -> Option<Vec<u8>> {
    let mut sum = match existing {
        Some(bytes) => decode_balance(bytes)?,
        None => Balance::default(),
    };
    for operand in operands.iter() {
        let delta = decode_balance(operand)?;
        sum.balance = sum.balance.checked_add(delta.balance)?;
        sum.received = sum.received.checked_add(delta.received)?;
    }
    Some(balance_value(sum.balance, sum.received).to_vec())
}

fn decode_tip(bytes: &[u8]) -> Result<(u32, [u8; 32]), Error> {
    let Ok(bytes) = <[u8; 36]>::try_from(bytes) else {
        return Err(Error::Corrupt(format!("a tip of {} bytes", bytes.len())));
    };
    Ok((
        u32::from_le_bytes(bytes[..4].try_into().expect("4")),
        bytes[4..].try_into().expect("32"),
    ))
}

fn tip_value(height: u32, hash: &[u8; 32]) -> [u8; 36] {
    let mut out = [0; 36];
    out[..4].copy_from_slice(&height.to_le_bytes());
    out[4..].copy_from_slice(hash);
    out
}

/// The wallet index database.
pub struct WalletIndex {
    db: DB,
}

impl WalletIndex {
    /// Opens or creates the index in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cache = Cache::new_lru_cache(BLOCK_CACHE_BYTES);
        let cf = |name: &str| {
            let mut table = BlockBasedOptions::default();
            table.set_block_cache(&cache);
            let mut o = Options::default();
            match name {
                // Random 32-byte keys and values: point lookups with a filter, no compression.
                CF_TX_LOC => {
                    table.set_bloom_filter(10.0, false);
                    o.set_compression_type(DBCompressionType::None);
                }
                CF_TX_ID => o.set_compression_type(DBCompressionType::None),
                // Range scans of one address: the address is the prefix.
                CF_ADDR_TX | CF_ADDR_UTXO => {
                    o.set_prefix_extractor(SliceTransform::create_fixed_prefix(ADDRESS_BYTES));
                    o.set_memtable_prefix_bloom_ratio(0.1);
                    table.set_bloom_filter(10.0, false);
                    table.set_whole_key_filtering(false);
                    o.set_compression_type(DBCompressionType::Lz4);
                }
                CF_ADDR_BALANCE => {
                    o.set_merge_operator_associative("hayai_balance_add", add_balances);
                    table.set_bloom_filter(10.0, false);
                    o.set_compression_type(DBCompressionType::Lz4);
                }
                _ => o.set_compression_type(DBCompressionType::Lz4),
            }
            o.set_block_based_table_factory(&table);
            ColumnFamilyDescriptor::new(name, o)
        };
        let db = DB::open_cf_descriptors(&opts, dir, CFS.map(cf))?;
        Ok(Self { db })
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(name)
            .expect("column family created at open")
    }

    /// The newest indexed block: height and hash. `None` for a new index.
    pub fn tip(&self) -> Result<Option<Tip>, Error> {
        match self.db.get_pinned_cf(self.cf(CF_META), KEY_TIP)? {
            Some(bytes) => Ok(Some(decode_tip(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Sets the tip of a new index to the genesis block, which the index does not hold.
    pub fn start_at_genesis(&self, hash: &[u8; 32]) -> Result<(), Error> {
        let Ok(None) = self.tip() else {
            return Err(Error::Chain("the index is not new".into()));
        };
        self.db
            .put_cf(self.cf(CF_META), KEY_TIP, tip_value(0, hash))?;
        self.db.flush_wal(true)?;
        Ok(())
    }

    /// Adds the entries of `delta` to `batch`.
    fn put_delta(&self, batch: &mut WriteBatch, delta: &Delta) {
        let h = delta.height;
        let (tx_loc, tx_id) = (self.cf(CF_TX_LOC), self.cf(CF_TX_ID));
        let (addr_tx, addr_utxo) = (self.cf(CF_ADDR_TX), self.cf(CF_ADDR_UTXO));
        let balance = self.cf(CF_ADDR_BALANCE);
        for (i, txid) in delta.txids.iter().enumerate() {
            let loc = TxLoc {
                height: h,
                index: i as u16,
            }
            .encode();
            batch.put_cf(tx_loc, txid, loc);
            batch.put_cf(tx_id, loc, txid);
        }
        // The outputs first: a spend of an output of the same block then removes it.
        for c in &delta.created {
            let txid = &delta.txids[usize::from(c.tx)];
            let value = c.value as i64;
            batch.put_cf(
                addr_utxo,
                utxo_key(&c.address, h, txid, c.index),
                c.value.to_le_bytes(),
            );
            let loc = TxLoc {
                height: h,
                index: c.tx,
            };
            batch.put_cf(addr_tx, addr_tx_key(&c.address, loc), []);
            batch.merge_cf(balance, c.address.0, balance_value(value, value));
        }
        for s in &delta.spent {
            batch.delete_cf(addr_utxo, utxo_key(&s.address, s.height, &s.txid, s.index));
            let loc = TxLoc {
                height: h,
                index: s.spender,
            };
            batch.put_cf(addr_tx, addr_tx_key(&s.address, loc), []);
            batch.merge_cf(balance, s.address.0, balance_value(-(s.value as i64), 0));
        }
        for (pool, t) in &delta.subtrees {
            let mut value = [0; 36];
            value[..4].copy_from_slice(&t.end_height.to_be_bytes());
            value[4..].copy_from_slice(&t.root);
            batch.put_cf(self.cf(CF_SUBTREE), subtree_key(*pool, t.index), value);
        }
        batch.put_cf(self.cf(CF_UNDO), h.to_be_bytes(), &delta.undo);
        batch.put_cf(self.cf(CF_META), KEY_TIP, tip_value(h, &delta.hash));
    }

    /// Writes `deltas` (consecutive blocks) in one write batch. The write is not synced.
    pub(crate) fn write(&self, deltas: &[Delta], size_hint: usize) -> Result<usize, Error> {
        let mut batch = WriteBatch::with_capacity_bytes(size_hint);
        for delta in deltas {
            self.put_delta(&mut batch, delta);
        }
        let bytes = batch.size_in_bytes();
        self.db.write(batch)?;
        Ok(bytes)
    }

    /// The txids of the block at `height`, in block order.
    fn txids_at(&self, height: u32) -> Result<Vec<[u8; 32]>, Error> {
        let start = TxLoc { height, index: 0 }.encode();
        let mut txids = Vec::new();
        for entry in self.db.iterator_cf(
            self.cf(CF_TX_ID),
            IteratorMode::From(&start, Direction::Forward),
        ) {
            let (key, value) = entry?;
            let loc = TxLoc::decode(&key)?;
            if loc.height != height {
                break;
            }
            if usize::from(loc.index) != txids.len() {
                return Err(Error::Corrupt(format!(
                    "a gap in the txids of height {height}"
                )));
            }
            let Ok(txid) = <[u8; 32]>::try_from(&value[..]) else {
                return Err(Error::Corrupt(format!("a txid of {} bytes", value.len())));
            };
            txids.push(txid);
        }
        Ok(txids)
    }

    /// Undoes the tip block, which must be `height` with `hash`, and returns the new tip.
    pub(crate) fn undo_tip(&self, height: u32, hash: &[u8; 32]) -> Result<(u32, [u8; 32]), Error> {
        let Some(tip) = self.tip()? else {
            return Err(Error::Chain("an undo on a new index".into()));
        };
        if tip != (height, *hash) {
            return Err(Error::Chain(format!(
                "an undo of block {height} on the index tip {}",
                tip.0
            )));
        }
        let Some(record) = self
            .db
            .get_pinned_cf(self.cf(CF_UNDO), height.to_be_bytes())?
        else {
            return Err(Error::Corrupt(format!(
                "no undo record for height {height}"
            )));
        };
        let mut delta = Delta::default();
        delta.decode_undo(height, &record)?;
        if delta.hash != *hash {
            return Err(Error::Corrupt(format!(
                "the undo record of height {height} names another block"
            )));
        }
        let txids = self.txids_at(height)?;
        let (addr_tx, addr_utxo) = (self.cf(CF_ADDR_TX), self.cf(CF_ADDR_UTXO));
        let balance = self.cf(CF_ADDR_BALANCE);
        let mut batch = WriteBatch::default();
        // The spent coins first: an output that the block created and spent then ends
        // removed.
        for s in &delta.spent {
            batch.put_cf(
                addr_utxo,
                utxo_key(&s.address, s.height, &s.txid, s.index),
                s.value.to_le_bytes(),
            );
            let loc = TxLoc {
                height,
                index: s.spender,
            };
            batch.delete_cf(addr_tx, addr_tx_key(&s.address, loc));
            batch.merge_cf(balance, s.address.0, balance_value(s.value as i64, 0));
        }
        for c in &delta.created {
            let Some(txid) = txids.get(usize::from(c.tx)) else {
                return Err(Error::Corrupt(format!(
                    "the undo record of height {height} names transaction {}",
                    c.tx
                )));
            };
            batch.delete_cf(addr_utxo, utxo_key(&c.address, height, txid, c.index));
            batch.delete_cf(
                addr_tx,
                addr_tx_key(
                    &c.address,
                    TxLoc {
                        height,
                        index: c.tx,
                    },
                ),
            );
            let value = c.value as i64;
            batch.merge_cf(balance, c.address.0, balance_value(-value, -value));
        }
        for (i, txid) in txids.iter().enumerate() {
            batch.delete_cf(self.cf(CF_TX_LOC), txid);
            batch.delete_cf(
                self.cf(CF_TX_ID),
                TxLoc {
                    height,
                    index: i as u16,
                }
                .encode(),
            );
        }
        for (pool, t) in &delta.subtrees {
            batch.delete_cf(self.cf(CF_SUBTREE), subtree_key(*pool, t.index));
        }
        batch.delete_cf(self.cf(CF_UNDO), height.to_be_bytes());
        let parent = (height - 1, delta.parent);
        batch.put_cf(self.cf(CF_META), KEY_TIP, tip_value(parent.0, &parent.1));
        self.db.write(batch)?;
        Ok(parent)
    }

    /// Undoes the indexed blocks above `height` and checks that the tip is then the block
    /// `hash` of `height`. The node calls it at its start with the base of the coins store.
    pub fn rewind_to(&self, height: u32, hash: &[u8; 32]) -> Result<usize, Error> {
        let mut undone = 0;
        loop {
            let Some(tip) = self.tip()? else {
                return Err(Error::Chain(
                    "the wallet index is empty: it holds no block of the chain".into(),
                ));
            };
            if tip.0 <= height {
                if tip != (height, *hash) {
                    return Err(Error::Chain(format!(
                        "the wallet index ends at height {}, and the chain state starts at \
                         height {height}: the index does not belong to this chain state \
                         (a node that ran without the index); start the node with an empty \
                         cache_dir",
                        tip.0
                    )));
                }
                self.db.flush_wal(true)?;
                return Ok(undone);
            }
            self.undo_tip(tip.0, &tip.1)?;
            undone += 1;
        }
    }

    /// Syncs the write-ahead log, after the removal of the undo records at or below
    /// `prune_through`: no restart undoes such a block.
    pub(crate) fn persist(&self, prune_through: u32) -> Result<(), Error> {
        if let Some(end) = prune_through.checked_add(1) {
            self.db
                .delete_range_cf(self.cf(CF_UNDO), 0u32.to_be_bytes(), end.to_be_bytes())?;
        }
        self.db.flush_wal(true)?;
        Ok(())
    }

    // ----- queries -----

    /// The location of the transaction `txid` in the committed chain.
    pub fn tx_location(&self, txid: &[u8; 32]) -> Result<Option<TxLoc>, Error> {
        match self.db.get_pinned_cf(self.cf(CF_TX_LOC), txid)? {
            Some(bytes) => Ok(Some(TxLoc::decode(&bytes)?)),
            None => Ok(None),
        }
    }

    /// The balance of `addresses` together. An address without an output has 0.
    pub fn balance(&self, addresses: &[AddressKey]) -> Result<Balance, Error> {
        let cf = self.cf(CF_ADDR_BALANCE);
        let mut total = Balance::default();
        for value in self
            .db
            .batched_multi_get_cf(cf, addresses.iter().map(|a| &a.0), false)
        {
            let Some(bytes) = value? else {
                continue;
            };
            let Some(balance) = decode_balance(&bytes) else {
                return Err(Error::Corrupt("a balance value".into()));
            };
            total.balance += balance.balance;
            total.received += balance.received;
        }
        Ok(total)
    }

    /// The txids of the transactions that pay to or spend from `addresses` in the heights
    /// `start..=end`, in chain order, each one time.
    pub fn address_txids(
        &self,
        addresses: &[AddressKey],
        start: u32,
        end: u32,
    ) -> Result<Vec<[u8; 32]>, Error> {
        let snapshot = self.db.snapshot();
        let mut locs = Vec::new();
        for address in addresses {
            let from = addr_tx_key(
                address,
                TxLoc {
                    height: start,
                    index: 0,
                },
            );
            let mut opts = ReadOptions::default();
            opts.set_prefix_same_as_start(true);
            opts.set_snapshot(&snapshot);
            for entry in self.db.iterator_cf_opt(
                self.cf(CF_ADDR_TX),
                opts,
                IteratorMode::From(&from, Direction::Forward),
            ) {
                let (key, _) = entry?;
                if key[..ADDRESS_BYTES] != address.0 {
                    break;
                }
                let loc = TxLoc::decode(&key[ADDRESS_BYTES..])?;
                if loc.height > end {
                    break;
                }
                locs.push(loc);
            }
        }
        locs.sort_unstable();
        locs.dedup();
        let keys: Vec<[u8; LOC_BYTES]> = locs.iter().map(|l| l.encode()).collect();
        let mut out = Vec::with_capacity(keys.len());
        let mut opts = ReadOptions::default();
        opts.set_snapshot(&snapshot);
        for value in self
            .db
            .batched_multi_get_cf_opt(self.cf(CF_TX_ID), &keys, true, &opts)
        {
            let Some(bytes) = value? else {
                return Err(Error::Corrupt(
                    "an address entry without its transaction".into(),
                ));
            };
            let Ok(txid) = <[u8; 32]>::try_from(&bytes[..]) else {
                return Err(Error::Corrupt(format!("a txid of {} bytes", bytes.len())));
            };
            out.push(txid);
        }
        Ok(out)
    }

    /// The unspent outputs of `addresses` in chain order (height, transaction index,
    /// output index), with the tip of the index that they belong to.
    pub fn address_utxos(
        &self,
        addresses: &[AddressKey],
    ) -> Result<(Vec<Utxo>, Option<Tip>), Error> {
        let snapshot = self.db.snapshot();
        let tip = match snapshot.get_cf(self.cf(CF_META), KEY_TIP)? {
            Some(bytes) => Some(decode_tip(&bytes)?),
            None => None,
        };
        let mut utxos = Vec::new();
        for address in addresses {
            let mut opts = ReadOptions::default();
            opts.set_prefix_same_as_start(true);
            opts.set_snapshot(&snapshot);
            for entry in self.db.iterator_cf_opt(
                self.cf(CF_ADDR_UTXO),
                opts,
                IteratorMode::From(&address.0, Direction::Forward),
            ) {
                let (key, value) = entry?;
                if key[..ADDRESS_BYTES] != address.0 {
                    break;
                }
                let (Ok(key), Ok(value)) = (
                    <[u8; UTXO_KEY_BYTES]>::try_from(&key[..]),
                    <[u8; 8]>::try_from(&value[..]),
                ) else {
                    return Err(Error::Corrupt("an unspent output entry".into()));
                };
                utxos.push(Utxo {
                    address: *address,
                    height: u32::from_be_bytes(key[21..25].try_into().expect("4")),
                    txid: key[25..57].try_into().expect("32"),
                    index: u32::from_be_bytes(key[57..].try_into().expect("4")),
                    value: u64::from_le_bytes(value),
                });
            }
        }
        // The key orders the outputs of one height by txid. The location of each txid
        // gives the chain order.
        let mut txids: Vec<[u8; 32]> = utxos.iter().map(|u| u.txid).collect();
        txids.sort_unstable();
        txids.dedup();
        let mut order = std::collections::HashMap::with_capacity(txids.len());
        let mut opts = ReadOptions::default();
        opts.set_snapshot(&snapshot);
        for (txid, value) in txids.iter().zip(self.db.batched_multi_get_cf_opt(
            self.cf(CF_TX_LOC),
            &txids,
            true,
            &opts,
        )) {
            let Some(bytes) = value? else {
                return Err(Error::Corrupt(
                    "an unspent output without its transaction".into(),
                ));
            };
            order.insert(*txid, TxLoc::decode(&bytes)?);
        }
        utxos.sort_unstable_by_key(|u| (order[&u.txid], u.index));
        Ok((utxos, tip))
    }

    /// The completed subtrees of `pool` from `start`, at most `limit`, in index order. The
    /// list ends at the first index that is not complete.
    pub fn subtrees(
        &self,
        pool: SubtreePool,
        start: u16,
        limit: Option<u16>,
    ) -> Result<Vec<Subtree>, Error> {
        let from = subtree_key(pool, start);
        let mut out = Vec::new();
        for entry in self.db.iterator_cf(
            self.cf(CF_SUBTREE),
            IteratorMode::From(&from, Direction::Forward),
        ) {
            if let Some(limit) = limit {
                if out.len() >= usize::from(limit) {
                    break;
                }
            }
            let (key, value) = entry?;
            let (Ok(key), Ok(value)) = (
                <[u8; 3]>::try_from(&key[..]),
                <[u8; 36]>::try_from(&value[..]),
            ) else {
                return Err(Error::Corrupt("a subtree entry".into()));
            };
            let index = u16::from_be_bytes([key[1], key[2]]);
            if key[0] != pool as u8 || usize::from(index) != usize::from(start) + out.len() {
                break;
            }
            out.push(Subtree {
                index,
                end_height: u32::from_be_bytes(value[..4].try_into().expect("4")),
                root: value[4..].try_into().expect("32"),
            });
        }
        Ok(out)
    }

    /// The bytes of the files of the index, from the RocksDB property of each column family.
    pub fn disk_bytes(&self) -> Result<u64, Error> {
        let mut total = 0;
        for name in CFS {
            total += self
                .db
                .property_int_value_cf(self.cf(name), "rocksdb.total-sst-files-size")?
                .unwrap_or(0);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests;
