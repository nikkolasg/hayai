//! Compact block construction and reconstruction (`docs/protocol-compact-relay.md`,
//! sections CompactBlock and Block announcement).
//!
//! Transaction order: prefilled transactions and full ids sit at their absolute indexes; the
//! remaining positions, in increasing order, are filled by the batch references expanded in
//! order and then by the short ids in order. A batch reference therefore covers a run of
//! consecutive positions that are neither prefilled nor full ids, and all batch-covered
//! positions precede all short-id positions.
//!
//! Reception has two gates. [`resolve`] turns a compact block into a [`Partial`]: every
//! position is held (bytes present), known (WtxId present, bytes absent) or unknown (a short
//! id the store does not resolve). Once no position is unknown, [`Partial::verify_ids`]
//! checks the merkle root of the txids, and the ZIP 244 auth data root when the caller knows
//! the parent's history root, against the header. A node forwards at that point: short ids
//! are re-keyed, batch references and full ids are content-addressed and travel unchanged.
//! [`Partial::assemble`] builds the block once every position is held and checks the merkle
//! root again before the block is used.
//!
//! Candidate form (candidates feature). A block in canonical order whose set is close to a
//! published candidate travels as a [`CandidateBlock`]: the coinbase, the candidate
//! reference, and the difference. [`CompactBuilder::build_candidate`] chooses the candidate
//! with the smallest difference. [`resolve_candidate`] rebuilds the set; once every
//! transaction is held, [`CandidatePartial::into_partial`] puts it in canonical order and
//! hands over a [`Partial`], which then takes the same gates as any other compact block.

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use hayai_crypto::{zcash_encoding, zcash_primitives, zcash_protocol};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{ParseError, RawBlock, RawTx, TxLookup, WtxId};
use zcash_encoding::CompactSize;
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::BranchId;

use crate::batch::{Batch, BatchId, LaneStore};
use crate::candidate::{
    expand, without_positions, CandidateError, CandidateStore, ResolvedCandidate,
};
use crate::message::{
    BlockTxn, BlockTxnRequest, CandidateBlock, CompactBlock, FullId, LaneId, PrefilledTx,
    CANONICAL_ORDER,
};
use crate::short_id::{short_id, ShortId, ShortIdIndex, ShortIdKey};

/// Upper bound on the transactions of one block: a 2 MB block cannot hold more than this
/// many of the smallest transactions, so a compact block announcing more is malformed.
pub const MAX_BLOCK_TXS: usize = 100_000;

/// How a sender references one transaction of a block for one peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdForm {
    /// Wire bytes at the index. Index 0 always; otherwise transactions the peer cannot have.
    Prefilled,
    /// Six-byte short id, or a batch reference when a batch matches.
    Short,
    /// Full WtxId at the index (protocol version 2): the peer may not hold the bytes yet.
    Full,
}

/// One position of a block as the builder sees it: the id always, the bytes when held.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: WtxId,
    pub bytes: Option<Bytes>,
}

/// Builds the compact form of one block for several peers: ids and short ids are computed
/// once under one nonce, and each [`CompactBuilder::build`] only selects the form per
/// position.
pub struct CompactBuilder {
    header: Bytes,
    nonce: u64,
    entries: Vec<Entry>,
    short_ids: Vec<ShortId>,
    /// The transactions after the coinbase are in canonical order, so the block can travel
    /// in the candidate form.
    canonical: bool,
}

impl CompactBuilder {
    /// `header` is the serialized header of the block. A builder made here never uses the
    /// candidate form: it cannot tell whether the order is canonical.
    pub fn new(header: Bytes, entries: Vec<Entry>, nonce: u64) -> Self {
        let key = ShortIdKey::from_header(&header, nonce);
        let short_ids = entries.iter().map(|e| short_id(&key, &e.id)).collect();
        CompactBuilder {
            header,
            nonce,
            entries,
            short_ids,
            canonical: false,
        }
    }

    pub fn from_block(block: &RawBlock, nonce: u64) -> Self {
        let header = block.bytes.slice(..block.header.serialized_len());
        let entries = block
            .txs
            .iter()
            .map(|tx| Entry {
                id: tx.wtxid(),
                bytes: Some(tx.bytes.clone()),
            })
            .collect();
        let body: Vec<&RawTx> = block.txs.iter().skip(1).collect();
        let mut builder = Self::new(header, entries, nonce);
        builder.canonical = hayai_wire::is_canonical(&body);
        builder
    }

    /// The hash of the block's parent, from the header bytes.
    pub fn parent(&self) -> BlockHash {
        BlockHash(
            self.header[4..36]
                .try_into()
                .expect("a serialized header holds the parent hash at bytes 4..36"),
        )
    }

    /// The candidate form of the block for one peer, against the candidate of `candidates`
    /// on the block's parent with the smallest difference, or `None` when the short form
    /// costs less: the block is not in canonical order, the coinbase bytes are not held, no
    /// candidate is on the parent, the difference has as many entries as the block has
    /// transactions after the coinbase, or `form` wants an addition prefilled. An addition
    /// whose bytes this node does not hold, or that `form` wants as a full id, is a full id;
    /// every other addition is a short id.
    pub fn build_candidate(
        &self,
        candidates: &[&ResolvedCandidate],
        mut form: impl FnMut(usize, &WtxId) -> IdForm,
    ) -> Option<CandidateBlock> {
        if !self.canonical {
            return None;
        }
        let coinbase = self.entries.first()?.bytes.clone()?;
        let body = &self.entries[1..];
        if body.is_empty() {
            return None;
        }
        let in_block: HashSet<WtxId> = body.iter().map(|e| e.id).collect();
        let parent = self.parent();
        let (cost, candidate) = candidates
            .iter()
            .filter(|c| c.parent == parent)
            .map(|c| {
                let kept = c.ids.iter().filter(|id| in_block.contains(id)).count();
                ((c.ids.len() - kept) + (body.len() - kept), *c)
            })
            .min_by_key(|(cost, _)| *cost)?;
        if cost >= body.len() {
            return None;
        }
        let in_candidate: HashSet<&WtxId> = candidate.ids.iter().collect();
        let removed = candidate
            .ids
            .iter()
            .enumerate()
            .filter(|(_, id)| !in_block.contains(id))
            .map(|(i, _)| u32::try_from(i).expect("a candidate holds under 2^32 ids"))
            .collect();
        let mut short_ids = Vec::new();
        let mut full_ids = Vec::new();
        for (index, entry) in self.entries.iter().enumerate().skip(1) {
            if in_candidate.contains(&entry.id) {
                continue;
            }
            match (form(index, &entry.id), &entry.bytes) {
                (IdForm::Prefilled, Some(_)) => return None,
                (_, None) | (IdForm::Full, Some(_)) => full_ids.push(entry.id),
                (IdForm::Short, Some(_)) => short_ids.push(self.short_ids[index]),
            }
        }
        Some(CandidateBlock {
            header: self.header.clone(),
            nonce: self.nonce,
            lane_id: candidate.lane,
            seq: candidate.seq,
            flags: CANONICAL_ORDER,
            coinbase,
            removed,
            short_ids,
            full_ids,
        })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Builds the compact block for one peer.
    ///
    /// `form` decides each position after index 0; index 0 (the coinbase) is prefilled. A
    /// position whose bytes this node does not hold is always a full id: this node cannot
    /// serve a request for it, and the full id lets the next hop verify the block without
    /// it. Starting from the first open position, the longest batch in `batches` whose ids
    /// equal the next open transactions is referenced, repeatedly, until no batch matches;
    /// every later open transaction gets a short id.
    pub fn build(
        &self,
        batches: &[&Batch],
        mut form: impl FnMut(usize, &WtxId) -> IdForm,
    ) -> CompactBlock {
        let mut prefilled = Vec::new();
        let mut full_ids = Vec::new();
        let mut open = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            let wanted = if index == 0 {
                IdForm::Prefilled
            } else {
                form(index, &entry.id)
            };
            match (wanted, &entry.bytes) {
                (IdForm::Prefilled, Some(bytes)) => prefilled.push(PrefilledTx {
                    index: index as u32,
                    bytes: bytes.clone(),
                }),
                (_, None) | (IdForm::Full, Some(_)) => full_ids.push(FullId {
                    index: index as u32,
                    id: entry.id,
                }),
                (IdForm::Short, Some(_)) => open.push(index),
            }
        }

        let mut batch_refs = Vec::new();
        let mut covered = 0;
        while covered < open.len() {
            let rest = &open[covered..];
            let best = batches
                .iter()
                .filter(|batch| {
                    batch.ids.len() <= rest.len()
                        && batch
                            .ids
                            .iter()
                            .zip(rest)
                            .all(|(id, &index)| *id == self.entries[index].id)
                })
                .max_by_key(|batch| batch.ids.len());
            let Some(batch) = best else {
                break;
            };
            batch_refs.push(batch.id);
            covered += batch.ids.len();
        }
        let short_ids = open[covered..]
            .iter()
            .map(|&index| self.short_ids[index])
            .collect();

        CompactBlock {
            header: self.header.clone(),
            nonce: self.nonce,
            batch_refs,
            short_ids,
            prefilled,
            full_ids,
        }
    }
}

impl CompactBlock {
    /// The compact form of `block` for one peer; see [`CompactBuilder::build`].
    pub fn from_block(
        block: &RawBlock,
        batches: &[&Batch],
        form: impl FnMut(usize, &WtxId) -> IdForm,
        nonce: u64,
    ) -> CompactBlock {
        CompactBuilder::from_block(block, nonce).build(batches, form)
    }

    pub fn parse_header(&self) -> Result<BlockHeader, ParseError> {
        BlockHeader::parse(&self.header)
    }
}

#[derive(Clone, Debug)]
enum Slot {
    /// Bytes and id present.
    Held(Arc<RawTx>),
    /// Id present (full id or batch entry), bytes absent.
    Known(WtxId),
    /// A short id the store does not resolve.
    Unknown,
}

/// A block under reconstruction: what every position resolved to so far.
#[derive(Clone, Debug)]
pub struct Partial {
    header: BlockHeader,
    header_bytes: Bytes,
    slots: Vec<Slot>,
}

/// What [`Partial::verify_ids`] checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdCheck {
    /// The auth data root was folded into `hashBlockCommitments` and compared; `false`
    /// when the caller did not know the parent's history root.
    pub auth_root_checked: bool,
}

#[derive(thiserror::Error, Debug)]
pub enum ReconstructError {
    #[error("batch {0:?} is not in the lane store")]
    UnknownBatch(BatchId),
    #[error("header: {0}")]
    Header(ParseError),
    #[error("{0} transactions cannot fit in a block")]
    TooManyTransactions(usize),
    #[error("{field} index {index} is not after the previous one")]
    IndexOrder { field: &'static str, index: u32 },
    #[error("{field} index {index} is outside the {count} transactions of the block")]
    IndexOutOfRange {
        field: &'static str,
        index: u32,
        count: usize,
    },
    #[error("index {0} is both prefilled and a full id")]
    IndexTwice(u32),
    #[error("transaction at index {index}: {source}")]
    Transaction { index: u32, source: ParseError },
    #[error("{0} positions have no transaction id yet")]
    IdsIncomplete(usize),
    #[error("{0} positions have no transaction bytes yet")]
    BytesIncomplete(usize),
    /// The ids resolved to a different body than the header commits to (a short-id
    /// collision or a stale entry). Not a fault of the sender (BIP 152): the caller
    /// requests the full block instead of penalizing it.
    #[error("merkle root {actual} does not match header root {expected}")]
    MerkleMismatch { expected: String, actual: String },
    /// The txids match but the authorizing data does not: a transaction with the same txid
    /// and different witnesses. Handled as a merkle mismatch.
    #[error("hashBlockCommitments does not match the chain history and auth data roots")]
    CommitmentsMismatch,
    #[error("BlockTxn answers block {got:?}, request was for block {expected:?}")]
    WrongBlock { expected: BlockHash, got: BlockHash },
    #[error("BlockTxn carries {received} transactions, {requested} were requested")]
    TxnCountMismatch { requested: usize, received: usize },
    #[error("BlockTxn fills index {0}, which is already held")]
    AlreadyHeld(u32),
    #[error("candidate block flags {0:#04x}: this version requires canonical order only")]
    CandidateFlags(u8),
    #[error("candidate {seq} of lane {lane:02x?} is not in the candidate store")]
    UnknownCandidate { lane: LaneId, seq: u64 },
    #[error("candidate extends {candidate:?}, the block extends {block:?}")]
    CandidateParent {
        candidate: BlockHash,
        block: BlockHash,
    },
    #[error("candidate: {0}")]
    Candidate(#[from] CandidateError),
    #[error("{0} short ids of the additions do not resolve")]
    UnresolvedShortIds(usize),
    #[error("transaction {0:?} appears twice in the candidate block")]
    DuplicateId(WtxId),
    #[error("{0}")]
    Order(#[from] hayai_wire::OrderCycle),
}

/// Resolves a compact block against the local stores.
///
/// Prefilled transactions are parsed with `branch_id` (ZIP 244 digests depend on it for v4
/// transactions). Batch entries and full ids give known positions; the store supplies the
/// bytes it holds. Short ids resolve against a per-block index of the store; one that does
/// not resolve leaves its position unknown.
pub fn resolve(
    cb: &CompactBlock,
    store: &dyn TxLookup,
    lanes: &LaneStore,
    branch_id: BranchId,
) -> Result<Partial, ReconstructError> {
    let header = cb.parse_header().map_err(ReconstructError::Header)?;

    let mut batches = Vec::with_capacity(cb.batch_refs.len());
    for id in &cb.batch_refs {
        let Some(batch) = lanes.get(id) else {
            return Err(ReconstructError::UnknownBatch(*id));
        };
        batches.push(batch);
    }
    let batched: usize = batches.iter().map(|b| b.ids.len()).sum();
    let count = cb.prefilled.len() + cb.full_ids.len() + batched + cb.short_ids.len();
    if count > MAX_BLOCK_TXS {
        return Err(ReconstructError::TooManyTransactions(count));
    }

    let mut slots: Vec<Option<Slot>> = vec![None; count];
    let mut prev = None;
    for p in &cb.prefilled {
        let slot = place(&mut slots, "prefilled", &mut prev, p.index)?;
        let tx = RawTx::parse(p.bytes.clone(), branch_id).map_err(|source| {
            ReconstructError::Transaction {
                index: p.index,
                source,
            }
        })?;
        *slot = Some(Slot::Held(Arc::new(tx)));
    }
    let mut prev = None;
    for f in &cb.full_ids {
        let slot = place(&mut slots, "full id", &mut prev, f.index)?;
        *slot = Some(from_store(store, &f.id));
    }

    let index = if cb.short_ids.is_empty() {
        None
    } else {
        Some(ShortIdIndex::build(
            &ShortIdKey::from_header(&cb.header, cb.nonce),
            store,
        ))
    };
    let from_batches = batches
        .iter()
        .flat_map(|b| b.ids.iter())
        .map(|id| from_store(store, id));
    let from_short_ids = cb.short_ids.iter().map(|sid| {
        index
            .as_ref()
            .and_then(|index| index.resolve(sid))
            .and_then(|id| store.get(&id))
            .map_or(Slot::Unknown, Slot::Held)
    });
    let mut resolved = from_batches.chain(from_short_ids);
    let slots = slots
        .into_iter()
        .map(|slot| match slot {
            Some(slot) => slot,
            None => resolved
                .next()
                .expect("open positions equal batched plus short ids"),
        })
        .collect();
    Ok(Partial {
        header,
        header_bytes: cb.header.clone(),
        slots,
    })
}

/// Claims the slot at `index` for an indexed section, checking order, range and overlap.
fn place<'a>(
    slots: &'a mut [Option<Slot>],
    field: &'static str,
    prev: &mut Option<u32>,
    index: u32,
) -> Result<&'a mut Option<Slot>, ReconstructError> {
    if matches!(*prev, Some(p) if index <= p) {
        return Err(ReconstructError::IndexOrder { field, index });
    }
    *prev = Some(index);
    let count = slots.len();
    let Some(slot) = slots.get_mut(index as usize) else {
        return Err(ReconstructError::IndexOutOfRange {
            field,
            index,
            count,
        });
    };
    let None = slot else {
        return Err(ReconstructError::IndexTwice(index));
    };
    Ok(slot)
}

fn from_store(store: &dyn TxLookup, id: &WtxId) -> Slot {
    match store.get(id) {
        Some(tx) => Slot::Held(tx),
        None => Slot::Known(*id),
    }
}

impl Partial {
    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn block_hash(&self) -> BlockHash {
        self.header.hash()
    }

    /// Positions with no transaction id, strictly increasing: what a `BlockTxnRequest` to
    /// the sender asks for.
    pub fn unknown(&self) -> Vec<u32> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, Slot::Unknown))
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Ids whose bytes this node lacks, in block order: what a `TxRequest` asks for.
    pub fn missing_ids(&self) -> Vec<WtxId> {
        self.slots
            .iter()
            .filter_map(|s| match s {
                Slot::Known(id) => Some(*id),
                Slot::Held(_) | Slot::Unknown => None,
            })
            .collect()
    }

    /// Whether `id` names a position whose bytes this node lacks.
    pub fn wants(&self, id: &WtxId) -> bool {
        self.slots
            .iter()
            .any(|s| matches!(s, Slot::Known(k) if k == id))
    }

    /// Every position is held.
    pub fn is_complete(&self) -> bool {
        self.slots.iter().all(|s| matches!(s, Slot::Held(_)))
    }

    /// Checks the id list against the header once every position has an id.
    ///
    /// The merkle root of the txids is always compared with the header. With the parent's
    /// ZIP 221 history root, `hashBlockCommitments` is recomputed from it and the auth data
    /// root of the ids and compared as well; without it, only the txids are checked and
    /// the result says so.
    pub fn verify_ids(&self, history_root: Option<&[u8; 32]>) -> Result<IdCheck, ReconstructError> {
        let mut txids = Vec::with_capacity(self.slots.len());
        let mut digests = Vec::with_capacity(self.slots.len());
        let mut unknown = 0;
        for slot in &self.slots {
            let id = match slot {
                Slot::Held(tx) => tx.wtxid(),
                Slot::Known(id) => *id,
                Slot::Unknown => {
                    unknown += 1;
                    continue;
                }
            };
            txids.push(id.txid);
            digests.push(id.auth_digest);
        }
        if unknown > 0 {
            return Err(ReconstructError::IdsIncomplete(unknown));
        }
        check_merkle(&self.header, &txids)?;
        let Some(history) = history_root else {
            return Ok(IdCheck {
                auth_root_checked: false,
            });
        };
        let auth = hayai_wire::auth_data_root(&digests);
        if hayai_wire::block_commitments(history, &auth) != self.header.block_commitments {
            return Err(ReconstructError::CommitmentsMismatch);
        }
        Ok(IdCheck {
            auth_root_checked: true,
        })
    }

    /// The builder for forwarding this block, with its ids complete and bytes where held.
    /// The candidate form is open to it when every position is held and the order is
    /// canonical.
    pub fn builder(&self, nonce: u64) -> Result<CompactBuilder, ReconstructError> {
        let mut entries = Vec::with_capacity(self.slots.len());
        let mut unknown = 0;
        for slot in &self.slots {
            match slot {
                Slot::Held(tx) => entries.push(Entry {
                    id: tx.wtxid(),
                    bytes: Some(tx.bytes.clone()),
                }),
                Slot::Known(id) => entries.push(Entry {
                    id: *id,
                    bytes: None,
                }),
                Slot::Unknown => unknown += 1,
            }
        }
        if unknown > 0 {
            return Err(ReconstructError::IdsIncomplete(unknown));
        }
        let held: Option<Vec<&RawTx>> = self
            .slots
            .iter()
            .skip(1)
            .map(|s| match s {
                Slot::Held(tx) => Some(tx.as_ref()),
                Slot::Known(_) | Slot::Unknown => None,
            })
            .collect();
        let mut builder = CompactBuilder::new(self.header_bytes.clone(), entries, nonce);
        builder.canonical = matches!(held, Some(body) if hayai_wire::is_canonical(&body));
        Ok(builder)
    }

    /// Fills the positions a `BlockTxnRequest` for `indexes` asked for.
    pub fn apply_block_txn(
        &mut self,
        txn: &BlockTxn,
        indexes: &[u32],
        branch_id: BranchId,
    ) -> Result<(), ReconstructError> {
        let hash = self.block_hash();
        if txn.block_hash != hash {
            return Err(ReconstructError::WrongBlock {
                expected: hash,
                got: txn.block_hash,
            });
        }
        if txn.txs.len() != indexes.len() {
            return Err(ReconstructError::TxnCountMismatch {
                requested: indexes.len(),
                received: txn.txs.len(),
            });
        }
        for (&index, bytes) in indexes.iter().zip(&txn.txs) {
            let count = self.slots.len();
            let Some(slot) = self.slots.get_mut(index as usize) else {
                return Err(ReconstructError::IndexOutOfRange {
                    field: "BlockTxn",
                    index,
                    count,
                });
            };
            if let Slot::Held(_) = slot {
                return Err(ReconstructError::AlreadyHeld(index));
            }
            let tx = RawTx::parse(bytes.clone(), branch_id)
                .map_err(|source| ReconstructError::Transaction { index, source })?;
            *slot = Slot::Held(Arc::new(tx));
        }
        Ok(())
    }

    /// Fills every known position whose id is `tx`'s. Returns whether any position wanted it.
    pub fn supply(&mut self, tx: &Arc<RawTx>) -> bool {
        let id = tx.wtxid();
        let mut wanted = false;
        for slot in &mut self.slots {
            if matches!(slot, Slot::Known(k) if *k == id) {
                *slot = Slot::Held(tx.clone());
                wanted = true;
            }
        }
        wanted
    }

    /// Concatenates header, count and transaction bytes into one buffer, checks the merkle
    /// root, and builds the block with every transaction's bytes as a sub-slice of that
    /// buffer.
    pub fn assemble(self) -> Result<RawBlock, ReconstructError> {
        let mut txs = Vec::with_capacity(self.slots.len());
        let mut missing = 0;
        for slot in self.slots {
            match slot {
                Slot::Held(tx) => txs.push(tx),
                Slot::Known(_) | Slot::Unknown => missing += 1,
            }
        }
        if missing > 0 {
            return Err(ReconstructError::BytesIncomplete(missing));
        }
        let txids: Vec<TxId> = txs.iter().map(|t| t.txid).collect();
        check_merkle(&self.header, &txids)?;

        let body: usize = txs.iter().map(|t| t.bytes.len()).sum();
        let mut bytes = Vec::with_capacity(self.header_bytes.len() + 9 + body);
        bytes.extend_from_slice(&self.header_bytes);
        CompactSize::write(&mut bytes, txs.len()).expect("write to Vec");
        let mut ranges = Vec::with_capacity(txs.len());
        for tx in &txs {
            let start = bytes.len();
            bytes.extend_from_slice(&tx.bytes);
            ranges.push(start..bytes.len());
        }
        let bytes = Bytes::from(bytes);
        let txs = txs
            .into_iter()
            .zip(ranges)
            .map(|(tx, range)| RawTx {
                bytes: bytes.slice(range),
                tx: tx.tx.clone(),
                txid: tx.txid,
                auth_digest: tx.auth_digest,
            })
            .collect();
        Ok(RawBlock {
            bytes,
            header: self.header,
            txs,
        })
    }
}

fn check_merkle(header: &BlockHeader, txids: &[TxId]) -> Result<(), ReconstructError> {
    let actual = hayai_wire::merkle_root(txids);
    if actual != header.merkle_root {
        return Err(ReconstructError::MerkleMismatch {
            expected: hex_string(&header.merkle_root),
            actual: hex_string(&actual),
        });
    }
    Ok(())
}

/// Rebuilds the block body from local stores in one step, for a node that holds every
/// transaction. Anything the stores lack is an error: [`resolve`] is the incremental path.
pub fn reconstruct(
    cb: &CompactBlock,
    store: &dyn TxLookup,
    lanes: &LaneStore,
    branch_id: BranchId,
) -> Result<RawBlock, ReconstructError> {
    let partial = resolve(cb, store, lanes, branch_id)?;
    let unknown = partial.unknown().len();
    if unknown > 0 {
        return Err(ReconstructError::IdsIncomplete(unknown));
    }
    partial.assemble()
}

/// A candidate block under reconstruction: the coinbase and the set of its other
/// transactions, each held or known by id.
#[derive(Clone, Debug)]
pub struct CandidatePartial {
    header: BlockHeader,
    header_bytes: Bytes,
    coinbase: Arc<RawTx>,
    txs: Vec<Slot>,
}

/// Resolves a candidate block against the local stores: the candidate's id list without
/// the removed positions, then the additions. A short id that does not resolve is an
/// error ([`ReconstructError::UnresolvedShortIds`]): the positions of a candidate block are
/// known only once its set is complete, so no `BlockTxnRequest` can name them, and the
/// caller requests the full block. A candidate the store does not hold, a candidate on
/// another parent, a batch the lane store does not hold or a removed position out of range
/// are errors of the same kind. A frame with flags other than [`CANONICAL_ORDER`] is
/// malformed.
pub fn resolve_candidate(
    cb: &CandidateBlock,
    store: &dyn TxLookup,
    candidates: &CandidateStore,
    lanes: &LaneStore,
    branch_id: BranchId,
) -> Result<CandidatePartial, ReconstructError> {
    let header = cb.parse_header().map_err(ReconstructError::Header)?;
    if cb.flags != CANONICAL_ORDER {
        return Err(ReconstructError::CandidateFlags(cb.flags));
    }
    let Some(announce) = candidates.get(&cb.lane_id, cb.seq) else {
        return Err(ReconstructError::UnknownCandidate {
            lane: cb.lane_id,
            seq: cb.seq,
        });
    };
    if announce.parent != header.prev_hash {
        return Err(ReconstructError::CandidateParent {
            candidate: announce.parent,
            block: header.prev_hash,
        });
    }
    let candidate = expand(announce, lanes)?;
    let mut ids = without_positions(candidate.ids, &cb.removed)?;
    let count = 1 + ids.len() + cb.short_ids.len() + cb.full_ids.len();
    if count > MAX_BLOCK_TXS {
        return Err(ReconstructError::TooManyTransactions(count));
    }
    ids.extend_from_slice(&cb.full_ids);
    if !cb.short_ids.is_empty() {
        let index = ShortIdIndex::build(&ShortIdKey::from_header(&cb.header, cb.nonce), store);
        let resolved: Vec<WtxId> = cb
            .short_ids
            .iter()
            .filter_map(|sid| index.resolve(sid))
            .collect();
        let unresolved = cb.short_ids.len() - resolved.len();
        if unresolved > 0 {
            return Err(ReconstructError::UnresolvedShortIds(unresolved));
        }
        ids.extend(resolved);
    }
    let mut seen = HashSet::with_capacity(ids.len());
    for id in &ids {
        if !seen.insert(*id) {
            return Err(ReconstructError::DuplicateId(*id));
        }
    }
    let coinbase = RawTx::parse(cb.coinbase.clone(), branch_id)
        .map_err(|source| ReconstructError::Transaction { index: 0, source })?;
    Ok(CandidatePartial {
        header,
        header_bytes: cb.header.clone(),
        coinbase: Arc::new(coinbase),
        txs: ids.iter().map(|id| from_store(store, id)).collect(),
    })
}

impl CandidatePartial {
    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn block_hash(&self) -> BlockHash {
        self.header.hash()
    }

    /// Ids whose bytes this node lacks: what a `TxRequest` asks for.
    pub fn missing_ids(&self) -> Vec<WtxId> {
        self.txs
            .iter()
            .filter_map(|s| match s {
                Slot::Known(id) => Some(*id),
                Slot::Held(_) | Slot::Unknown => None,
            })
            .collect()
    }

    /// Whether `id` is a transaction of the block whose bytes this node lacks.
    pub fn wants(&self, id: &WtxId) -> bool {
        self.txs
            .iter()
            .any(|s| matches!(s, Slot::Known(k) if k == id))
    }

    /// Fills the transaction whose id is `tx`'s. Returns whether the block wanted it.
    pub fn supply(&mut self, tx: &Arc<RawTx>) -> bool {
        let id = tx.wtxid();
        let mut wanted = false;
        for slot in &mut self.txs {
            if matches!(slot, Slot::Known(k) if *k == id) {
                *slot = Slot::Held(tx.clone());
                wanted = true;
            }
        }
        wanted
    }

    pub fn is_complete(&self) -> bool {
        self.txs.iter().all(|s| matches!(s, Slot::Held(_)))
    }

    /// The block in canonical order, once every transaction is held: the coinbase, then
    /// the set sorted by depth and txid. The id list and the body still have to pass
    /// [`Partial::verify_ids`] and [`Partial::assemble`].
    pub fn into_partial(self) -> Result<Partial, ReconstructError> {
        let missing = self.missing_ids().len();
        if missing > 0 {
            return Err(ReconstructError::BytesIncomplete(missing));
        }
        let held: Vec<Arc<RawTx>> = self
            .txs
            .into_iter()
            .map(|slot| match slot {
                Slot::Held(tx) => tx,
                Slot::Known(_) | Slot::Unknown => unreachable!("checked above"),
            })
            .collect();
        let refs: Vec<&RawTx> = held.iter().map(|t| t.as_ref()).collect();
        let order = hayai_wire::canonical_order_of(&refs)?;
        let mut slots = Vec::with_capacity(held.len() + 1);
        slots.push(Slot::Held(self.coinbase));
        slots.extend(order.into_iter().map(|i| Slot::Held(held[i].clone())));
        Ok(Partial {
            header: self.header,
            header_bytes: self.header_bytes,
            slots,
        })
    }
}

impl BlockTxnRequest {
    pub fn for_missing(block_hash: BlockHash, indexes: Vec<u32>) -> Self {
        BlockTxnRequest {
            block_hash,
            indexes,
        }
    }
}

fn hex_string(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateError, CandidateStore, ResolvedCandidate};
    use crate::message::BatchAnnounce;
    use crate::test_util::{make_block, make_tx, MemStore, BRANCH};
    use std::time::Instant;

    fn fixture(n: u32) -> (RawBlock, Vec<Arc<RawTx>>, MemStore) {
        let txs: Vec<Arc<RawTx>> = (0..n).map(|i| make_tx(1000 + i)).collect();
        let block = make_block(&txs);
        let mut store = MemStore::default();
        for tx in &txs[1..] {
            store.insert(tx.clone());
        }
        (block, txs, store)
    }

    fn batch_of(block: &RawBlock, range: std::ops::Range<usize>) -> Batch {
        let ids: Vec<WtxId> = block.txs[range].iter().map(RawTx::wtxid).collect();
        Batch {
            id: BatchId::compute(&ids),
            lane: [7; 32],
            seq: range_seq(&ids),
            ids,
        }
    }

    fn range_seq(ids: &[WtxId]) -> u64 {
        u64::from(ids[0].txid.as_ref()[0])
    }

    fn lanes_with(store: &MemStore, batches: &[&Batch]) -> LaneStore {
        let mut lanes = LaneStore::new();
        for (seq, b) in batches.iter().enumerate() {
            let announce = BatchAnnounce {
                lane_id: [seq as u8; 32],
                seq: seq as u64,
                batch_id: b.id,
                ids: b.ids.clone(),
            };
            lanes.insert(&announce, store, Instant::now()).unwrap();
        }
        lanes
    }

    fn short(_: usize, _: &WtxId) -> IdForm {
        IdForm::Short
    }

    fn assert_same_block(rebuilt: &RawBlock, original: &RawBlock) {
        assert_eq!(rebuilt.bytes, original.bytes);
        assert_eq!(rebuilt.header, original.header);
        assert_eq!(rebuilt.txs.len(), original.txs.len());
        for (a, b) in rebuilt.txs.iter().zip(&original.txs) {
            assert_eq!(a.bytes, b.bytes);
            assert_eq!(a.txid, b.txid);
            assert_eq!(a.auth_digest, b.auth_digest);
            // Sub-slice of the block buffer, not a copy.
            let base = rebuilt.bytes.as_ptr() as usize;
            let p = a.bytes.as_ptr() as usize;
            assert!(p >= base && p + a.bytes.len() <= base + rebuilt.bytes.len());
        }
    }

    #[test]
    fn short_ids_round_trip() {
        let (block, _, store) = fixture(12);
        let cb = CompactBlock::from_block(&block, &[], short, 42);
        assert_eq!(cb.prefilled.len(), 1);
        assert_eq!(cb.prefilled[0].index, 0);
        assert_eq!(cb.prefilled[0].bytes, block.txs[0].bytes);
        assert!(cb.batch_refs.is_empty());
        assert!(cb.full_ids.is_empty());
        assert_eq!(cb.short_ids.len(), 11);
        let key = ShortIdKey::from_header(&cb.header, 42);
        assert_eq!(cb.short_ids[3], short_id(&key, &block.txs[4].wtxid()));

        let lanes = LaneStore::new();
        let rebuilt = reconstruct(&cb, &store, &lanes, BRANCH).unwrap();
        assert_same_block(&rebuilt, &block);
    }

    #[test]
    fn batch_refs_cover_prefix_then_short_ids() {
        let (block, _, store) = fixture(12);
        let a = batch_of(&block, 1..4);
        let b = batch_of(&block, 4..6);
        let unordered = Batch {
            ids: block.txs[6..9].iter().rev().map(RawTx::wtxid).collect(),
            ..batch_of(&block, 6..9)
        };
        let gap = Batch {
            ids: vec![block.txs[6].wtxid(), block.txs[8].wtxid()],
            ..batch_of(&block, 6..8)
        };
        let cb = CompactBlock::from_block(&block, &[&b, &unordered, &a, &gap], short, 1);
        assert_eq!(cb.batch_refs, vec![a.id, b.id]);
        assert_eq!(cb.short_ids.len(), 6);

        let lanes = lanes_with(&store, &[&a, &b]);
        let rebuilt = reconstruct(&cb, &store, &lanes, BRANCH).unwrap();
        assert_same_block(&rebuilt, &block);
    }

    #[test]
    fn longest_matching_batch_wins() {
        let (block, _, store) = fixture(8);
        let short_batch = batch_of(&block, 1..3);
        let long = batch_of(&block, 1..6);
        let cb = CompactBlock::from_block(&block, &[&short_batch, &long], short, 1);
        assert_eq!(cb.batch_refs, vec![long.id]);
        assert_eq!(cb.short_ids.len(), 2);
        let lanes = lanes_with(&store, &[&short_batch, &long]);
        assert_same_block(&reconstruct(&cb, &store, &lanes, BRANCH).unwrap(), &block);
    }

    #[test]
    fn prefilled_positions_break_batches() {
        let (block, txs, mut store) = fixture(10);
        let lacking = txs[3].wtxid();
        let whole = batch_of(&block, 1..6);
        let after = batch_of(&block, 4..6);
        let form = |_: usize, id: &WtxId| {
            if *id == lacking {
                IdForm::Prefilled
            } else {
                IdForm::Short
            }
        };
        let cb = CompactBlock::from_block(&block, &[&whole, &after], form, 1);
        let indexes: Vec<u32> = cb.prefilled.iter().map(|p| p.index).collect();
        assert_eq!(indexes, vec![0, 3]);
        // Positions 1, 2 have no matching batch, so no batch is referenced at all.
        assert!(cb.batch_refs.is_empty());
        assert_eq!(cb.short_ids.len(), 8);

        store.remove(&lacking);
        let lanes = lanes_with(&store, &[&whole, &after]);
        assert_same_block(&reconstruct(&cb, &store, &lanes, BRANCH).unwrap(), &block);
    }

    /// Full ids sit at their own indexes, so a fresh transaction in the middle of the block
    /// leaves the batch and short-id positions around it untouched.
    #[test]
    fn full_ids_keep_their_index_between_batches_and_short_ids() {
        let (block, txs, mut store) = fixture(10);
        let fresh = txs[4].wtxid();
        let a = batch_of(&block, 1..4);
        let b = batch_of(&block, 5..7);
        let form = |_: usize, id: &WtxId| {
            if *id == fresh {
                IdForm::Full
            } else {
                IdForm::Short
            }
        };
        let cb = CompactBlock::from_block(&block, &[&a, &b], form, 1);
        assert_eq!(cb.batch_refs, vec![a.id, b.id]);
        assert_eq!(
            cb.full_ids,
            vec![FullId {
                index: 4,
                id: fresh
            }]
        );
        assert_eq!(cb.short_ids.len(), 3);

        // With the bytes in the store the block completes at once.
        let lanes = lanes_with(&store, &[&a, &b]);
        assert_same_block(&reconstruct(&cb, &store, &lanes, BRANCH).unwrap(), &block);

        // Without them the ids are still complete: the roots verify and a forwarded block
        // carries the fresh transaction as a full id; the bytes arrive by `supply`.
        store.remove(&fresh);
        let mut partial = resolve(&cb, &store, &lanes, BRANCH).unwrap();
        assert!(partial.unknown().is_empty());
        assert_eq!(partial.missing_ids(), vec![fresh]);
        assert!(partial.wants(&fresh));
        assert!(!partial.is_complete());
        assert_eq!(
            partial.verify_ids(None).unwrap(),
            IdCheck {
                auth_root_checked: false
            }
        );
        let forwarded = partial.builder(9).unwrap().build(&[], |_, _| IdForm::Short);
        assert_eq!(
            forwarded.full_ids,
            vec![FullId {
                index: 4,
                id: fresh
            }]
        );
        assert_eq!(forwarded.short_ids.len(), 8);
        assert_eq!(forwarded.prefilled.len(), 1);
        assert!(matches!(
            partial.clone().assemble(),
            Err(ReconstructError::BytesIncomplete(1))
        ));
        assert!(!partial.supply(&txs[2]));
        assert!(partial.supply(&txs[4]));
        assert!(partial.is_complete());
        assert_same_block(&partial.assemble().unwrap(), &block);
    }

    #[test]
    fn unresolved_short_ids_are_requested_by_index_then_applied() {
        let (block, txs, mut store) = fixture(10);
        let a = batch_of(&block, 1..4);
        let cb = CompactBlock::from_block(&block, &[&a], short, 1);
        let lanes = lanes_with(&store, &[&a]);
        // One from the batch (known id, no bytes), two from the short ids (unknown).
        store.remove(&txs[2].wtxid());
        store.remove(&txs[5].wtxid());
        store.remove(&txs[9].wtxid());

        let mut partial = resolve(&cb, &store, &lanes, BRANCH).unwrap();
        assert_eq!(partial.unknown(), vec![5, 9]);
        assert_eq!(partial.missing_ids(), vec![txs[2].wtxid()]);
        assert!(matches!(
            partial.verify_ids(None),
            Err(ReconstructError::IdsIncomplete(2))
        ));
        assert!(matches!(
            partial.builder(1),
            Err(ReconstructError::IdsIncomplete(2))
        ));
        assert!(matches!(
            reconstruct(&cb, &store, &lanes, BRANCH),
            Err(ReconstructError::IdsIncomplete(2))
        ));
        let request = BlockTxnRequest::for_missing(partial.block_hash(), partial.unknown());
        assert_eq!(request.block_hash, block.hash());
        assert_eq!(request.indexes, vec![5, 9]);

        let wrong_block = BlockTxn {
            block_hash: BlockHash([9; 32]),
            txs: vec![],
        };
        assert!(matches!(
            partial.apply_block_txn(&wrong_block, &[5, 9], BRANCH),
            Err(ReconstructError::WrongBlock { .. })
        ));
        let short_answer = BlockTxn {
            block_hash: block.hash(),
            txs: vec![txs[5].bytes.clone()],
        };
        assert!(matches!(
            partial.apply_block_txn(&short_answer, &[5, 9], BRANCH),
            Err(ReconstructError::TxnCountMismatch {
                requested: 2,
                received: 1
            })
        ));
        let held = BlockTxn {
            block_hash: block.hash(),
            txs: vec![txs[1].bytes.clone()],
        };
        assert!(matches!(
            partial.apply_block_txn(&held, &[1], BRANCH),
            Err(ReconstructError::AlreadyHeld(1))
        ));
        let swapped = BlockTxn {
            block_hash: block.hash(),
            txs: vec![txs[9].bytes.clone(), txs[5].bytes.clone()],
        };
        let mut wrong = partial.clone();
        wrong.apply_block_txn(&swapped, &[5, 9], BRANCH).unwrap();
        assert!(matches!(
            wrong.verify_ids(None),
            Err(ReconstructError::MerkleMismatch { .. })
        ));

        let answer = BlockTxn {
            block_hash: block.hash(),
            txs: vec![txs[5].bytes.clone(), txs[9].bytes.clone()],
        };
        partial.apply_block_txn(&answer, &[5, 9], BRANCH).unwrap();
        partial.verify_ids(None).unwrap();
        assert_eq!(partial.missing_ids(), vec![txs[2].wtxid()]);
        assert!(partial.supply(&txs[2]));
        assert_same_block(&partial.assemble().unwrap(), &block);
    }

    #[test]
    fn auth_data_root_is_checked_through_block_commitments() {
        let txs: Vec<Arc<RawTx>> = (0..5).map(|i| make_tx(2000 + i)).collect();
        let history = [0x5a; 32];
        let auth =
            hayai_wire::auth_data_root(&txs.iter().map(|t| t.auth_digest).collect::<Vec<_>>());
        let block =
            make_block_with_commitments(&txs, hayai_wire::block_commitments(&history, &auth));
        let mut store = MemStore::default();
        for tx in &txs[1..] {
            store.insert(tx.clone());
        }
        let cb = CompactBlock::from_block(&block, &[], short, 1);
        let partial = resolve(&cb, &store, &LaneStore::new(), BRANCH).unwrap();
        assert_eq!(
            partial.verify_ids(Some(&history)).unwrap(),
            IdCheck {
                auth_root_checked: true
            }
        );
        assert!(matches!(
            partial.verify_ids(Some(&[0x5b; 32])),
            Err(ReconstructError::CommitmentsMismatch)
        ));
        // Without the history root only the merkle root is checked.
        assert_eq!(
            partial.verify_ids(None).unwrap(),
            IdCheck {
                auth_root_checked: false
            }
        );
    }

    fn make_block_with_commitments(txs: &[Arc<RawTx>], commitments: [u8; 32]) -> RawBlock {
        let plain = make_block(txs);
        let mut header = plain.header.clone();
        header.block_commitments = commitments;
        let mut bytes = header.serialize();
        bytes.extend_from_slice(&plain.bytes[plain.header.serialized_len()..]);
        RawBlock::parse(Bytes::from(bytes), BRANCH).expect("block parses")
    }

    #[test]
    fn unknown_batch_fails_whole_reconstruction() {
        let (block, _, store) = fixture(6);
        let a = batch_of(&block, 1..3);
        let cb = CompactBlock::from_block(&block, &[&a], short, 1);
        assert_eq!(cb.batch_refs, vec![a.id]);
        let lanes = LaneStore::new();
        assert!(matches!(
            reconstruct(&cb, &store, &lanes, BRANCH),
            Err(ReconstructError::UnknownBatch(id)) if id == a.id
        ));
    }

    #[test]
    fn header_merkle_root_is_verified() {
        let (block, _, store) = fixture(5);
        // Everything prefilled, so the header change cannot disturb short ids.
        let mut cb = CompactBlock::from_block(&block, &[], |_, _| IdForm::Prefilled, 1);
        assert_eq!(cb.prefilled.len(), 5);
        assert_same_block(
            &reconstruct(&cb, &store, &LaneStore::new(), BRANCH).unwrap(),
            &block,
        );
        let mut header = cb.header.to_vec();
        header[36] ^= 1;
        cb.header = header.into();
        assert!(matches!(
            reconstruct(&cb, &store, &LaneStore::new(), BRANCH),
            Err(ReconstructError::MerkleMismatch { .. })
        ));
        let partial = resolve(&cb, &store, &LaneStore::new(), BRANCH).unwrap();
        assert!(matches!(
            partial.verify_ids(None),
            Err(ReconstructError::MerkleMismatch { .. })
        ));
    }

    /// A full id that names a different transaction than the block holds fails the id
    /// check, so the block is never forwarded on it.
    #[test]
    fn wrong_full_id_fails_the_id_check() {
        let (block, txs, store) = fixture(5);
        let mut cb = CompactBlock::from_block(&block, &[], short, 1);
        cb.short_ids.remove(2);
        cb.full_ids.push(FullId {
            index: 3,
            id: make_tx(77).wtxid(),
        });
        let partial = resolve(&cb, &store, &LaneStore::new(), BRANCH).unwrap();
        assert!(partial.unknown().is_empty());
        assert_eq!(partial.missing_ids(), vec![make_tx(77).wtxid()]);
        assert!(matches!(
            partial.verify_ids(None),
            Err(ReconstructError::MerkleMismatch { .. })
        ));
        cb.full_ids[0].id = txs[3].wtxid();
        assert_same_block(
            &reconstruct(&cb, &store, &LaneStore::new(), BRANCH).unwrap(),
            &block,
        );
    }

    #[test]
    fn malformed_compact_blocks_are_rejected() {
        let (block, txs, store) = fixture(5);
        let lanes = LaneStore::new();
        let good = CompactBlock::from_block(&block, &[], short, 1);

        let mut out_of_range = good.clone();
        out_of_range.prefilled[0].index = 5;
        assert!(matches!(
            reconstruct(&out_of_range, &store, &lanes, BRANCH),
            Err(ReconstructError::IndexOutOfRange {
                field: "prefilled",
                index: 5,
                count: 5
            })
        ));

        let mut duplicate = good.clone();
        duplicate.prefilled.push(good.prefilled[0].clone());
        assert!(matches!(
            reconstruct(&duplicate, &store, &lanes, BRANCH),
            Err(ReconstructError::IndexOrder {
                field: "prefilled",
                index: 0
            })
        ));

        let mut garbage = good.clone();
        garbage.prefilled[0].bytes = Bytes::from_static(&[1, 2, 3]);
        assert!(matches!(
            reconstruct(&garbage, &store, &lanes, BRANCH),
            Err(ReconstructError::Transaction { index: 0, .. })
        ));

        let mut huge = good.clone();
        huge.short_ids = vec![ShortId([0; 6]); MAX_BLOCK_TXS];
        assert!(matches!(
            reconstruct(&huge, &store, &lanes, BRANCH),
            Err(ReconstructError::TooManyTransactions(_))
        ));

        let mut bad_header = good.clone();
        let mut header = good.header.to_vec();
        header[140] = 0;
        bad_header.header = header.into();
        assert!(matches!(
            reconstruct(&bad_header, &store, &lanes, BRANCH),
            Err(ReconstructError::Header(_))
        ));

        let full = |i: u32| FullId {
            index: i,
            id: txs[i as usize].wtxid(),
        };
        let mut twice = good.clone();
        twice.short_ids.pop();
        twice.full_ids = vec![full(0)];
        assert!(matches!(
            reconstruct(&twice, &store, &lanes, BRANCH),
            Err(ReconstructError::IndexTwice(0))
        ));

        let mut unordered = good.clone();
        unordered.short_ids.truncate(2);
        unordered.full_ids = vec![full(4), full(3)];
        assert!(matches!(
            reconstruct(&unordered, &store, &lanes, BRANCH),
            Err(ReconstructError::IndexOrder {
                field: "full id",
                index: 3
            })
        ));

        let mut beyond = good.clone();
        beyond.short_ids.pop();
        beyond.full_ids = vec![FullId {
            index: 5,
            id: txs[4].wtxid(),
        }];
        assert!(matches!(
            reconstruct(&beyond, &store, &lanes, BRANCH),
            Err(ReconstructError::IndexOutOfRange {
                field: "full id",
                index: 5,
                count: 5
            })
        ));
    }

    #[test]
    fn compact_block_survives_the_codec() {
        let (block, txs, store) = fixture(6);
        let a = batch_of(&block, 1..3);
        let fresh = txs[5].wtxid();
        let form = |_: usize, id: &WtxId| {
            if *id == fresh {
                IdForm::Full
            } else {
                IdForm::Short
            }
        };
        let cb = CompactBlock::from_block(&block, &[&a], form, 3);
        assert_eq!(cb.full_ids.len(), 1);
        let frame =
            crate::message::encode(&crate::message::Message::CompactBlock(Box::new(cb.clone())));
        let crate::message::Message::CompactBlock(decoded) =
            crate::message::decode(&frame).unwrap()
        else {
            panic!("compact block expected");
        };
        assert_eq!(*decoded, cb);
        let lanes = lanes_with(&store, &[&a]);
        assert_same_block(
            &reconstruct(&decoded, &store, &lanes, BRANCH).unwrap(),
            &block,
        );
    }

    /// A block in canonical order over the transactions `body`, with `coinbase` first.
    fn canonical_block(coinbase: &Arc<RawTx>, body: &[Arc<RawTx>]) -> RawBlock {
        let mut sorted = body.to_vec();
        sorted.sort_by(|a, b| a.txid.as_ref().cmp(b.txid.as_ref()));
        let mut txs = vec![coinbase.clone()];
        txs.extend(sorted);
        make_block(&txs)
    }

    /// The publisher's candidate on the block's parent, recorded by a receiver.
    fn published(
        ids: &[WtxId],
        store: &MemStore,
    ) -> (LaneStore, CandidateStore, ResolvedCandidate) {
        let mut publisher = crate::LanePublisher::new([5; 32]);
        let publication = publisher.publish(BlockHash([0x33; 32]), 7, ids);
        let mut lanes = LaneStore::new();
        lanes
            .insert(publication.batch.as_ref().unwrap(), store, Instant::now())
            .unwrap();
        let mut candidates = CandidateStore::new();
        candidates
            .insert(publication.candidate, Instant::now())
            .unwrap();
        (lanes, candidates, publication.resolved)
    }

    fn rebuild(
        cb: &CandidateBlock,
        store: &MemStore,
        candidates: &CandidateStore,
        lanes: &LaneStore,
    ) -> RawBlock {
        let partial = resolve_candidate(cb, store, candidates, lanes, BRANCH)
            .unwrap()
            .into_partial()
            .unwrap();
        partial.verify_ids(None).unwrap();
        partial.assemble().unwrap()
    }

    /// A block equal to a candidate travels as the header, the coinbase and 61 bytes.
    #[test]
    fn a_block_equal_to_a_candidate_is_a_reference() {
        let txs: Vec<Arc<RawTx>> = (0..40).map(|i| make_tx(3000 + i)).collect();
        let mut store = MemStore::default();
        for tx in &txs[1..] {
            store.insert(tx.clone());
        }
        // The publisher's order is its own; the block is canonical.
        let ids: Vec<WtxId> = txs[1..].iter().map(|t| t.wtxid()).collect();
        let (lanes, candidates, resolved) = published(&ids, &store);
        let block = canonical_block(&txs[0], &txs[1..]);
        let builder = CompactBuilder::from_block(&block, 77);
        let cb = builder.build_candidate(&[&resolved], short).unwrap();
        assert!(cb.removed.is_empty() && cb.short_ids.is_empty() && cb.full_ids.is_empty());
        let frame = crate::message::encode(&crate::message::Message::CandidateBlock(Box::new(
            cb.clone(),
        )));
        let reference = frame.len() - cb.header.len() - cb.coinbase.len();
        assert_eq!(reference, 4 + 57);
        let short_form = crate::message::encode(&crate::message::Message::CompactBlock(Box::new(
            builder.build(&[], short),
        )));
        assert!(frame.len() + 39 * 6 - 60 < short_form.len());
        assert_same_block(&rebuild(&cb, &store, &candidates, &lanes), &block);
    }

    /// Two additions (one held, one not) and one removal travel as the difference.
    #[test]
    fn a_block_close_to_a_candidate_is_a_difference() {
        let txs: Vec<Arc<RawTx>> = (0..20).map(|i| make_tx(4000 + i)).collect();
        let mut store = MemStore::default();
        for tx in &txs[1..] {
            store.insert(tx.clone());
        }
        let candidate_ids: Vec<WtxId> = txs[1..18].iter().map(|t| t.wtxid()).collect();
        let (lanes, candidates, resolved) = published(&candidate_ids, &store);
        // The block drops txs[5] and adds txs[18] and txs[19].
        let body: Vec<Arc<RawTx>> = txs[1..]
            .iter()
            .filter(|t| t.wtxid() != txs[5].wtxid())
            .cloned()
            .collect();
        let block = canonical_block(&txs[0], &body);
        let fresh = txs[19].wtxid();
        let form = |_: usize, id: &WtxId| match *id == fresh {
            true => IdForm::Full,
            false => IdForm::Short,
        };
        let cb = CompactBuilder::from_block(&block, 5)
            .build_candidate(&[&resolved], form)
            .unwrap();
        let removed_at = resolved
            .ids
            .iter()
            .position(|id| *id == txs[5].wtxid())
            .unwrap();
        assert_eq!(cb.removed, vec![removed_at as u32]);
        assert_eq!(cb.short_ids.len(), 1);
        assert_eq!(cb.full_ids, vec![fresh]);
        assert_same_block(&rebuild(&cb, &store, &candidates, &lanes), &block);

        // Without the fresh transaction's bytes, its id is known and its bytes arrive later.
        store.remove(&fresh);
        let mut partial = resolve_candidate(&cb, &store, &candidates, &lanes, BRANCH).unwrap();
        assert_eq!(partial.missing_ids(), vec![fresh]);
        assert!(partial.wants(&fresh));
        assert!(matches!(
            partial.clone().into_partial(),
            Err(ReconstructError::BytesIncomplete(1))
        ));
        assert!(partial.supply(&txs[19]));
        assert!(partial.is_complete());
        let partial = partial.into_partial().unwrap();
        assert_same_block(&partial.assemble().unwrap(), &block);
    }

    #[test]
    fn candidate_form_needs_canonical_order_and_a_known_candidate() {
        let txs: Vec<Arc<RawTx>> = (0..8).map(|i| make_tx(5000 + i)).collect();
        let mut store = MemStore::default();
        for tx in &txs[1..] {
            store.insert(tx.clone());
        }
        let ids: Vec<WtxId> = txs[1..].iter().map(|t| t.wtxid()).collect();
        let (lanes, candidates, resolved) = published(&ids, &store);
        let mut body = txs[1..].to_vec();
        body.sort_by(|a, b| b.txid.as_ref().cmp(a.txid.as_ref()));
        let mut reversed = vec![txs[0].clone()];
        reversed.extend(body);
        let None = CompactBuilder::from_block(&make_block(&reversed), 1)
            .build_candidate(&[&resolved], short)
        else {
            panic!("a block outside canonical order has no candidate form");
        };
        let block = canonical_block(&txs[0], &txs[1..]);
        let cb = CompactBuilder::from_block(&block, 1)
            .build_candidate(&[&resolved], short)
            .unwrap();
        let mut unknown = cb.clone();
        unknown.seq = 8;
        assert!(matches!(
            resolve_candidate(&unknown, &store, &candidates, &lanes, BRANCH),
            Err(ReconstructError::UnknownCandidate { seq: 8, .. })
        ));
        let mut flags = cb.clone();
        flags.flags = 3;
        assert!(matches!(
            resolve_candidate(&flags, &store, &candidates, &lanes, BRANCH),
            Err(ReconstructError::CandidateFlags(3))
        ));
        assert!(matches!(
            resolve_candidate(&cb, &store, &candidates, &LaneStore::new(), BRANCH),
            Err(ReconstructError::Candidate(CandidateError::UnknownBatch(_)))
        ));
        // A block whose additions are most of it keeps the short form.
        let other: Vec<Arc<RawTx>> = (0..8).map(|i| make_tx(6000 + i)).collect();
        let None = CompactBuilder::from_block(&canonical_block(&txs[0], &other), 1)
            .build_candidate(&[&resolved], short)
        else {
            panic!("no candidate is close to an unrelated block");
        };
    }

    /// One builder serves several peers: forms differ per peer, short ids are shared.
    #[test]
    fn builder_selects_per_peer_forms_over_shared_short_ids() {
        let (block, txs, _) = fixture(6);
        let builder = CompactBuilder::from_block(&block, 11);
        assert_eq!(builder.entries().len(), 6);
        let for_v1 = builder.build(&[], |i, _| {
            if i == 2 {
                IdForm::Prefilled
            } else {
                IdForm::Short
            }
        });
        let for_v2 = builder.build(
            &[],
            |i, _| {
                if i == 2 {
                    IdForm::Full
                } else {
                    IdForm::Short
                }
            },
        );
        assert_eq!(for_v1.prefilled.len(), 2);
        assert!(for_v1.full_ids.is_empty());
        assert_eq!(for_v2.prefilled.len(), 1);
        assert_eq!(
            for_v2.full_ids,
            vec![FullId {
                index: 2,
                id: txs[2].wtxid()
            }]
        );
        assert_eq!(for_v1.short_ids, for_v2.short_ids);
        assert_eq!(for_v1.nonce, for_v2.nonce);
    }
}
