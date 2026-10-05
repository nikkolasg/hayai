//! Wire messages and their codec (`docs/protocol-compact-relay.md`, section Messages).
//!
//! A frame is a little-endian `u32` payload length followed by the payload. The payload
//! starts with a one-byte message type followed by the fields in the order of the protocol
//! table. Integers are little-endian, counts are Bitcoin `CompactSize`, and index lists
//! (prefilled transactions, full ids, `BlockTxnRequest`) are differentially encoded as in
//! BIP 152: the first index is absolute, each following one is the gap to its predecessor
//! minus one.
//!
//! The message type names the layout of the payload. No section is optional: the decoder
//! never infers a section from the bytes that remain, so a payload that ends early never
//! decodes. A compact block has two types. Type 6 (`CompactBlock`) is the version 1 layout
//! and has no full-id section. Type 12 (`CompactBlockV2`, protocol version 2) is the same
//! layout followed by the full-id section. The encoder uses type 12 if and only if the block
//! has full ids, and a type 12 payload with a zero full-id count is malformed, so each
//! message has one encoding. A sender never has full ids for a version 1 connection, so a
//! version 1 peer receives type 6 only.
//!
//! The candidates feature (feature bit 2) adds `CandidateAnnounce` and `CandidateBlock`.
//! They are new message types: a node sends them only to peers that negotiated the bit, so
//! the decoder of a peer without it never sees them.

use std::io::{Cursor, Read};

use bytes::Bytes;
use hayai_crypto::{zcash_encoding, zcash_primitives};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::WtxId;
use zcash_encoding::CompactSize;
use zcash_primitives::transaction::TxId;

use crate::batch::BatchId;
use crate::short_id::ShortId;

/// Largest payload accepted by [`decode`]. A Zcash block is at most 2 MB; the bound leaves
/// room for a `Tx` or `BlockTxn` message carrying several blocks' worth of transactions.
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

/// Identifier of a lane owner.
pub type LaneId = [u8; 32];

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TxAnnounce {
    pub ids: Vec<WtxId>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TxRequest {
    pub ids: Vec<WtxId>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tx {
    pub txs: Vec<Bytes>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchAnnounce {
    pub lane_id: LaneId,
    pub seq: u64,
    pub batch_id: BatchId,
    pub ids: Vec<WtxId>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchRequest {
    pub ids: Vec<BatchId>,
}

/// A transaction carried inside a `CompactBlock` with its absolute position in the block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PrefilledTx {
    pub index: u32,
    pub bytes: Bytes,
}

/// A transaction referenced by its full WtxId at an absolute position (protocol version 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FullId {
    pub index: u32,
    pub id: WtxId,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompactBlock {
    /// The serialized header of the network, including nonce and Equihash solution. The
    /// header delimits itself (its solution has a CompactSize length prefix); the decoder
    /// accepts the solution lengths of the known Equihash parameter sets only, so a Mainnet
    /// header is 1487 bytes and a Regtest header 177 bytes.
    pub header: Bytes,
    /// Short-id key nonce.
    pub nonce: u64,
    /// Batch references, expanded in order after the prefilled transactions are placed.
    pub batch_refs: Vec<BatchId>,
    /// Short ids of the transactions that follow the batch-covered ones.
    pub short_ids: Vec<ShortId>,
    /// Prefilled transactions in increasing index order.
    pub prefilled: Vec<PrefilledTx>,
    /// Full WtxIds in increasing index order; empty on a version 1 connection.
    pub full_ids: Vec<FullId>,
}

/// A candidate block of a lane: the transactions of `batches`, concatenated in order,
/// without the positions in `removed` (candidates feature). `seq` is the template revision
/// of the publisher and `parent` the block the candidate extends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CandidateAnnounce {
    pub lane_id: LaneId,
    pub seq: u64,
    pub parent: BlockHash,
    pub batches: Vec<BatchId>,
    /// Positions in the concatenated ids of `batches`, strictly increasing.
    pub removed: Vec<u32>,
}

/// Bit 0 of [`CandidateBlock::flags`]: the transactions after the coinbase are in canonical
/// order (`hayai_wire::order`). This version requires it; the other bits are zero.
pub const CANONICAL_ORDER: u8 = 1;

/// A block given as a candidate plus a difference (candidates feature): the coinbase, then
/// the canonical order of the candidate's set without `removed` and with the additions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CandidateBlock {
    /// The serialized header, as in [`CompactBlock::header`].
    pub header: Bytes,
    /// Short-id key nonce of `short_ids`.
    pub nonce: u64,
    pub lane_id: LaneId,
    pub seq: u64,
    pub flags: u8,
    /// Wire bytes of the coinbase, position 0.
    pub coinbase: Bytes,
    /// Positions in the candidate's id list, strictly increasing.
    pub removed: Vec<u32>,
    /// Additions the receiver holds.
    pub short_ids: Vec<ShortId>,
    /// Additions the receiver may not hold.
    pub full_ids: Vec<WtxId>,
}

impl CandidateBlock {
    pub fn parse_header(&self) -> Result<BlockHeader, hayai_wire::ParseError> {
        BlockHeader::parse(&self.header)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockTxnRequest {
    pub block_hash: BlockHash,
    /// Absolute transaction indexes, strictly increasing.
    pub indexes: Vec<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockTxn {
    pub block_hash: BlockHash,
    /// Wire bytes of the requested transactions, in request order.
    pub txs: Vec<Bytes>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Block {
    pub bytes: Bytes,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Message {
    TxAnnounce(TxAnnounce),
    TxRequest(TxRequest),
    Tx(Tx),
    BatchAnnounce(BatchAnnounce),
    BatchRequest(BatchRequest),
    CompactBlock(Box<CompactBlock>),
    BlockTxnRequest(BlockTxnRequest),
    BlockTxn(BlockTxn),
    Block(Block),
    CandidateAnnounce(CandidateAnnounce),
    CandidateBlock(Box<CandidateBlock>),
}

#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    #[error("frame shorter than its length prefix")]
    Truncated,
    #[error("frame has {0} bytes after the payload")]
    Trailing(usize),
    #[error("payload of {0} bytes exceeds the {MAX_PAYLOAD} byte limit")]
    Oversize(usize),
    #[error("frame length prefix {declared} does not match {actual} available bytes")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("empty payload")]
    Empty,
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("field {0}: input ended early")]
    Short(&'static str),
    #[error("count {count} of {field} needs more than the {remaining} remaining bytes")]
    CountTooLarge {
        field: &'static str,
        count: u64,
        remaining: usize,
    },
    #[error("{0}: invalid CompactSize")]
    CompactSize(&'static str),
    #[error("index list of {0} overflows u32")]
    IndexOverflow(&'static str),
    #[error("{0}: zero count in a message type that requires entries")]
    EmptySection(&'static str),
    #[error("header: {0}")]
    Header(String),
}

const TAG_TX_ANNOUNCE: u8 = 1;
const TAG_TX_REQUEST: u8 = 2;
const TAG_TX: u8 = 3;
const TAG_BATCH_ANNOUNCE: u8 = 4;
const TAG_BATCH_REQUEST: u8 = 5;
const TAG_COMPACT_BLOCK: u8 = 6;
const TAG_BLOCK_TXN_REQUEST: u8 = 7;
const TAG_BLOCK_TXN: u8 = 8;
const TAG_BLOCK: u8 = 9;
const TAG_CANDIDATE_ANNOUNCE: u8 = 10;
const TAG_CANDIDATE_BLOCK: u8 = 11;
const TAG_COMPACT_BLOCK_V2: u8 = 12;

/// Encodes a message as one frame: `u32` little-endian payload length, then the payload.
pub fn encode(message: &Message) -> Vec<u8> {
    let mut out = vec![0u8; 4];
    let mut w = Writer(&mut out);
    match message {
        Message::TxAnnounce(m) => {
            w.u8(TAG_TX_ANNOUNCE);
            w.wtxids(&m.ids);
        }
        Message::TxRequest(m) => {
            w.u8(TAG_TX_REQUEST);
            w.wtxids(&m.ids);
        }
        Message::Tx(m) => {
            w.u8(TAG_TX);
            w.blobs(&m.txs);
        }
        Message::BatchAnnounce(m) => {
            w.u8(TAG_BATCH_ANNOUNCE);
            w.bytes(&m.lane_id);
            w.u64(m.seq);
            w.bytes(&m.batch_id.0);
            w.wtxids(&m.ids);
        }
        Message::BatchRequest(m) => {
            w.u8(TAG_BATCH_REQUEST);
            w.compact_size(m.ids.len());
            for id in &m.ids {
                w.bytes(&id.0);
            }
        }
        Message::CompactBlock(m) => {
            w.u8(match m.full_ids.is_empty() {
                true => TAG_COMPACT_BLOCK,
                false => TAG_COMPACT_BLOCK_V2,
            });
            w.bytes(&m.header);
            w.u64(m.nonce);
            w.compact_size(m.batch_refs.len());
            for id in &m.batch_refs {
                w.bytes(&id.0);
            }
            w.compact_size(m.short_ids.len());
            for id in &m.short_ids {
                w.bytes(&id.0);
            }
            w.compact_size(m.prefilled.len());
            let mut prev = None;
            for p in &m.prefilled {
                w.compact_size(differential(prev, p.index));
                prev = Some(p.index);
                w.blob(&p.bytes);
            }
            if !m.full_ids.is_empty() {
                w.compact_size(m.full_ids.len());
                let mut prev = None;
                for f in &m.full_ids {
                    w.compact_size(differential(prev, f.index));
                    prev = Some(f.index);
                    w.bytes(&f.id.to_bytes());
                }
            }
        }
        Message::BlockTxnRequest(m) => {
            w.u8(TAG_BLOCK_TXN_REQUEST);
            w.bytes(&m.block_hash.0);
            w.indexes(&m.indexes);
        }
        Message::BlockTxn(m) => {
            w.u8(TAG_BLOCK_TXN);
            w.bytes(&m.block_hash.0);
            w.blobs(&m.txs);
        }
        Message::Block(m) => {
            w.u8(TAG_BLOCK);
            w.blob(&m.bytes);
        }
        Message::CandidateAnnounce(m) => {
            w.u8(TAG_CANDIDATE_ANNOUNCE);
            w.bytes(&m.lane_id);
            w.u64(m.seq);
            w.bytes(&m.parent.0);
            w.compact_size(m.batches.len());
            for id in &m.batches {
                w.bytes(&id.0);
            }
            w.indexes(&m.removed);
        }
        Message::CandidateBlock(m) => {
            w.u8(TAG_CANDIDATE_BLOCK);
            w.bytes(&m.header);
            w.u64(m.nonce);
            w.bytes(&m.lane_id);
            w.u64(m.seq);
            w.u8(m.flags);
            w.blob(&m.coinbase);
            w.indexes(&m.removed);
            w.compact_size(m.short_ids.len());
            for id in &m.short_ids {
                w.bytes(&id.0);
            }
            w.wtxids(&m.full_ids);
        }
    }
    let len = u32::try_from(out.len() - 4).expect("payload length fits u32");
    out[..4].copy_from_slice(&len.to_le_bytes());
    out
}

/// Differential index as in BIP 152: absolute for the first, gap minus one afterwards.
/// Callers keep index lists strictly increasing, which the subtraction relies on.
fn differential(prev: Option<u32>, index: u32) -> usize {
    let value = match prev {
        None => index,
        Some(p) => index
            .checked_sub(p)
            .and_then(|d| d.checked_sub(1))
            .expect("index lists are strictly increasing"),
    };
    value as usize
}

/// Decodes one complete frame (length prefix included). The slice must contain exactly one
/// frame; trailing bytes are an error so that framing bugs surface instead of being skipped.
pub fn decode(frame: &[u8]) -> Result<Message, DecodeError> {
    let Some(prefix) = frame.get(..4) else {
        return Err(DecodeError::Truncated);
    };
    let declared = u32::from_le_bytes(prefix.try_into().expect("4 bytes")) as usize;
    if declared > MAX_PAYLOAD {
        return Err(DecodeError::Oversize(declared));
    }
    let actual = frame.len() - 4;
    if declared > actual {
        return Err(DecodeError::LengthMismatch { declared, actual });
    }
    if declared < actual {
        return Err(DecodeError::Trailing(actual - declared));
    }
    decode_payload(&frame[4..])
}

/// Decodes a payload whose length prefix has already been consumed and checked.
pub fn decode_payload(payload: &[u8]) -> Result<Message, DecodeError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(DecodeError::Oversize(payload.len()));
    }
    let mut r = Reader(Cursor::new(payload));
    let tag = r.u8("type").map_err(|_| DecodeError::Empty)?;
    let message = match tag {
        TAG_TX_ANNOUNCE => Message::TxAnnounce(TxAnnounce { ids: r.wtxids()? }),
        TAG_TX_REQUEST => Message::TxRequest(TxRequest { ids: r.wtxids()? }),
        TAG_TX => Message::Tx(Tx { txs: r.blobs()? }),
        TAG_BATCH_ANNOUNCE => Message::BatchAnnounce(BatchAnnounce {
            lane_id: r.array("lane_id")?,
            seq: r.u64("seq")?,
            batch_id: BatchId(r.array("batch_id")?),
            ids: r.wtxids()?,
        }),
        TAG_BATCH_REQUEST => {
            let count = r.count("batch_ids", 32)?;
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(BatchId(r.array("batch_id")?));
            }
            Message::BatchRequest(BatchRequest { ids })
        }
        TAG_COMPACT_BLOCK | TAG_COMPACT_BLOCK_V2 => {
            let header = r.header()?;
            let nonce = r.u64("nonce")?;
            let batch_count = r.count("batch_refs", 32)?;
            let mut batch_refs = Vec::with_capacity(batch_count);
            for _ in 0..batch_count {
                batch_refs.push(BatchId(r.array("batch_ref")?));
            }
            let short_count = r.count("short_ids", 6)?;
            let mut short_ids = Vec::with_capacity(short_count);
            for _ in 0..short_count {
                short_ids.push(ShortId(r.array("short_id")?));
            }
            let prefilled_count = r.count("prefilled", 5)?;
            let mut prefilled = Vec::with_capacity(prefilled_count);
            let mut prev = None;
            for _ in 0..prefilled_count {
                let index = r.differential_index("prefilled", prev)?;
                prev = Some(index);
                let bytes = r.blob("prefilled")?;
                prefilled.push(PrefilledTx { index, bytes });
            }
            // The type says if the full-id section follows. Type 6 carries the blocks
            // without full ids, so a zero count in type 12 is a second encoding.
            let mut full_ids = Vec::new();
            if tag == TAG_COMPACT_BLOCK_V2 {
                let full_count = r.count("full_ids", 65)?;
                if full_count == 0 {
                    return Err(DecodeError::EmptySection("full_ids"));
                }
                full_ids.reserve(full_count);
                let mut prev = None;
                for _ in 0..full_count {
                    let index = r.differential_index("full_ids", prev)?;
                    prev = Some(index);
                    let id = r.wtxid()?;
                    full_ids.push(FullId { index, id });
                }
            }
            Message::CompactBlock(Box::new(CompactBlock {
                header,
                nonce,
                batch_refs,
                short_ids,
                prefilled,
                full_ids,
            }))
        }
        TAG_BLOCK_TXN_REQUEST => {
            let block_hash = BlockHash(r.array("block_hash")?);
            let indexes = r.indexes("indexes")?;
            Message::BlockTxnRequest(BlockTxnRequest {
                block_hash,
                indexes,
            })
        }
        TAG_BLOCK_TXN => Message::BlockTxn(BlockTxn {
            block_hash: BlockHash(r.array("block_hash")?),
            txs: r.blobs()?,
        }),
        TAG_BLOCK => Message::Block(Block {
            bytes: r.blob("block")?,
        }),
        TAG_CANDIDATE_ANNOUNCE => {
            let lane_id = r.array("lane_id")?;
            let seq = r.u64("seq")?;
            let parent = BlockHash(r.array("parent")?);
            let count = r.count("batches", 32)?;
            let mut batches = Vec::with_capacity(count);
            for _ in 0..count {
                batches.push(BatchId(r.array("batch")?));
            }
            Message::CandidateAnnounce(CandidateAnnounce {
                lane_id,
                seq,
                parent,
                batches,
                removed: r.indexes("removed")?,
            })
        }
        TAG_CANDIDATE_BLOCK => {
            let header = r.header()?;
            let nonce = r.u64("nonce")?;
            let lane_id = r.array("lane_id")?;
            let seq = r.u64("seq")?;
            let flags = r.u8("flags")?;
            let coinbase = r.blob("coinbase")?;
            let removed = r.indexes("removed")?;
            let short_count = r.count("short_ids", 6)?;
            let mut short_ids = Vec::with_capacity(short_count);
            for _ in 0..short_count {
                short_ids.push(ShortId(r.array("short_id")?));
            }
            Message::CandidateBlock(Box::new(CandidateBlock {
                header,
                nonce,
                lane_id,
                seq,
                flags,
                coinbase,
                removed,
                short_ids,
                full_ids: r.wtxids()?,
            }))
        }
        other => return Err(DecodeError::UnknownType(other)),
    };
    let consumed = r.0.position() as usize;
    if consumed < payload.len() {
        return Err(DecodeError::Trailing(payload.len() - consumed));
    }
    Ok(message)
}

struct Writer<'a>(&'a mut Vec<u8>);

impl Writer<'_> {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn compact_size(&mut self, n: usize) {
        CompactSize::write(&mut *self.0, n).expect("writing to a Vec cannot fail");
    }
    fn wtxids(&mut self, ids: &[WtxId]) {
        self.compact_size(ids.len());
        for id in ids {
            self.bytes(&id.to_bytes());
        }
    }
    /// `len u32`, bytes.
    fn blob(&mut self, b: &[u8]) {
        self.u32(u32::try_from(b.len()).expect("blob length fits u32"));
        self.bytes(b);
    }
    fn blobs(&mut self, blobs: &[Bytes]) {
        self.compact_size(blobs.len());
        for b in blobs {
            self.blob(b);
        }
    }
    /// `count`, then the differential encoding of a strictly increasing index list.
    fn indexes(&mut self, indexes: &[u32]) {
        self.compact_size(indexes.len());
        let mut prev = None;
        for &index in indexes {
            self.compact_size(differential(prev, index));
            prev = Some(index);
        }
    }
}

struct Reader<'a>(Cursor<&'a [u8]>);

impl Reader<'_> {
    fn remaining(&self) -> usize {
        let total = self.0.get_ref().len();
        total.saturating_sub(self.0.position() as usize)
    }
    fn array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        self.0
            .read_exact(&mut out)
            .map_err(|_| DecodeError::Short(field))?;
        Ok(out)
    }
    fn u8(&mut self, field: &'static str) -> Result<u8, DecodeError> {
        Ok(self.array::<1>(field)?[0])
    }
    /// A serialized block header, delimited by its own solution length prefix.
    fn header(&mut self) -> Result<Bytes, DecodeError> {
        let start = self.0.position() as usize;
        let rest = &self.0.get_ref()[start..];
        let header = BlockHeader::parse(rest).map_err(|e| DecodeError::Header(e.to_string()))?;
        let len = header.serialized_len();
        self.0.set_position((start + len) as u64);
        Ok(Bytes::copy_from_slice(&rest[..len]))
    }
    fn u32(&mut self, field: &'static str) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array(field)?))
    }
    fn u64(&mut self, field: &'static str) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array(field)?))
    }
    fn compact_size(&mut self, field: &'static str) -> Result<u64, DecodeError> {
        CompactSize::read(&mut self.0).map_err(|_| DecodeError::CompactSize(field))
    }
    /// A count of items that each occupy at least `min_item` bytes; rejected before any
    /// allocation when the remaining input cannot hold that many.
    fn count(&mut self, field: &'static str, min_item: usize) -> Result<usize, DecodeError> {
        let count = self.compact_size(field)?;
        let remaining = self.remaining();
        let fits = count
            .checked_mul(min_item as u64)
            .map(|needed| needed <= remaining as u64);
        match fits {
            Some(true) => Ok(count as usize),
            Some(false) | None => Err(DecodeError::CountTooLarge {
                field,
                count,
                remaining,
            }),
        }
    }
    fn differential_index(
        &mut self,
        field: &'static str,
        prev: Option<u32>,
    ) -> Result<u32, DecodeError> {
        let delta = self.compact_size(field)?;
        let absolute = match prev {
            None => delta,
            Some(p) => u64::from(p) + 1 + delta,
        };
        u32::try_from(absolute).map_err(|_| DecodeError::IndexOverflow(field))
    }
    /// A count and a differentially encoded index list, as [`Writer::indexes`] writes it.
    fn indexes(&mut self, field: &'static str) -> Result<Vec<u32>, DecodeError> {
        let count = self.count(field, 1)?;
        let mut indexes = Vec::with_capacity(count);
        let mut prev = None;
        for _ in 0..count {
            let index = self.differential_index(field, prev)?;
            prev = Some(index);
            indexes.push(index);
        }
        Ok(indexes)
    }
    fn wtxid(&mut self) -> Result<WtxId, DecodeError> {
        let txid = TxId::from_bytes(self.array("txid")?);
        let auth_digest = self.array("auth_digest")?;
        Ok(WtxId { txid, auth_digest })
    }
    fn wtxids(&mut self) -> Result<Vec<WtxId>, DecodeError> {
        let count = self.count("wtxids", 64)?;
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(self.wtxid()?);
        }
        Ok(ids)
    }
    fn blob(&mut self, field: &'static str) -> Result<Bytes, DecodeError> {
        let len = self.u32(field)? as usize;
        if len > self.remaining() {
            return Err(DecodeError::Short(field));
        }
        let start = self.0.position() as usize;
        let bytes = Bytes::copy_from_slice(&self.0.get_ref()[start..start + len]);
        self.0.set_position((start + len) as u64);
        Ok(bytes)
    }
    fn blobs(&mut self) -> Result<Vec<Bytes>, DecodeError> {
        let count = self.count("txs", 4)?;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.blob("tx")?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_wire::header::PowParams;
    use proptest::prelude::*;
    use rand::{Rng, SeedableRng};

    fn wtxid_strategy() -> impl Strategy<Value = WtxId> + Clone {
        (any::<[u8; 32]>(), any::<[u8; 32]>()).prop_map(|(t, a)| WtxId {
            txid: TxId::from_bytes(t),
            auth_digest: a,
        })
    }

    fn blob_strategy() -> impl Strategy<Value = Bytes> + Clone {
        proptest::collection::vec(any::<u8>(), 0..300).prop_map(Bytes::from)
    }

    fn increasing_indexes() -> impl Strategy<Value = Vec<u32>> {
        proptest::collection::vec(0u32..1000, 0..20).prop_map(|gaps| {
            let mut acc = 0u32;
            gaps.iter()
                .map(|g| {
                    acc = acc.saturating_add(*g);
                    let v = acc;
                    acc = acc.saturating_add(1);
                    v
                })
                .collect()
        })
    }

    fn compact_block_strategy() -> impl Strategy<Value = CompactBlock> {
        (
            prop_oneof![Just(PowParams::MAINNET), Just(PowParams::REGTEST),].prop_map(header_bytes),
            any::<u64>(),
            proptest::collection::vec(any::<[u8; 32]>().prop_map(BatchId), 0..6),
            proptest::collection::vec(any::<[u8; 6]>().prop_map(ShortId), 0..10),
            increasing_indexes(),
            proptest::collection::vec(blob_strategy(), 20),
            increasing_indexes(),
            proptest::collection::vec(wtxid_strategy(), 20),
        )
            .prop_map(
                |(header, nonce, batch_refs, short_ids, indexes, blobs, full_indexes, ids)| {
                    let prefilled = indexes
                        .into_iter()
                        .zip(blobs)
                        .map(|(index, bytes)| PrefilledTx { index, bytes })
                        .collect();
                    let full_ids = full_indexes
                        .into_iter()
                        .zip(ids)
                        .map(|(index, id)| FullId { index, id })
                        .collect();
                    CompactBlock {
                        header,
                        nonce,
                        batch_refs,
                        short_ids,
                        prefilled,
                        full_ids,
                    }
                },
            )
    }

    fn message_strategy() -> impl Strategy<Value = Message> {
        let ids = proptest::collection::vec(wtxid_strategy(), 0..8);
        let batch_ids = proptest::collection::vec(any::<[u8; 32]>().prop_map(BatchId), 0..6);
        let blobs = proptest::collection::vec(blob_strategy(), 0..5);
        prop_oneof![
            ids.clone()
                .prop_map(|ids| Message::TxAnnounce(TxAnnounce { ids })),
            ids.clone()
                .prop_map(|ids| Message::TxRequest(TxRequest { ids })),
            blobs.clone().prop_map(|txs| Message::Tx(Tx { txs })),
            (any::<[u8; 32]>(), any::<u64>(), any::<[u8; 32]>(), ids).prop_map(
                |(lane_id, seq, batch_id, ids)| Message::BatchAnnounce(BatchAnnounce {
                    lane_id,
                    seq,
                    batch_id: BatchId(batch_id),
                    ids,
                })
            ),
            batch_ids
                .clone()
                .prop_map(|ids| Message::BatchRequest(BatchRequest { ids })),
            compact_block_strategy().prop_map(|cb| Message::CompactBlock(Box::new(cb))),
            // Type 6: without this arm 1 compact block in 20 has no full ids.
            compact_block_strategy().prop_map(|mut cb| {
                cb.full_ids.clear();
                Message::CompactBlock(Box::new(cb))
            }),
            (any::<[u8; 32]>(), increasing_indexes()).prop_map(|(h, indexes)| {
                Message::BlockTxnRequest(BlockTxnRequest {
                    block_hash: BlockHash(h),
                    indexes,
                })
            }),
            (any::<[u8; 32]>(), blobs).prop_map(|(h, txs)| Message::BlockTxn(BlockTxn {
                block_hash: BlockHash(h),
                txs,
            })),
            blob_strategy().prop_map(|bytes| Message::Block(Block { bytes })),
            (
                any::<[u8; 32]>(),
                any::<u64>(),
                any::<[u8; 32]>(),
                proptest::collection::vec(any::<[u8; 32]>().prop_map(BatchId), 0..6),
                increasing_indexes(),
            )
                .prop_map(|(lane_id, seq, parent, batches, removed)| {
                    Message::CandidateAnnounce(CandidateAnnounce {
                        lane_id,
                        seq,
                        parent: BlockHash(parent),
                        batches,
                        removed,
                    })
                }),
            (
                prop_oneof![Just(PowParams::MAINNET), Just(PowParams::REGTEST),]
                    .prop_map(header_bytes),
                (any::<u64>(), any::<[u8; 32]>(), any::<u64>(), any::<u8>()),
                blob_strategy(),
                increasing_indexes(),
                proptest::collection::vec(any::<[u8; 6]>().prop_map(ShortId), 0..10),
                proptest::collection::vec(wtxid_strategy(), 0..5),
            )
                .prop_map(
                    |(
                        header,
                        (nonce, lane_id, seq, flags),
                        coinbase,
                        removed,
                        short_ids,
                        full_ids,
                    )| {
                        Message::CandidateBlock(Box::new(CandidateBlock {
                            header,
                            nonce,
                            lane_id,
                            seq,
                            flags,
                            coinbase,
                            removed,
                            short_ids,
                            full_ids,
                        }))
                    }
                ),
        ]
    }

    proptest! {
        #[test]
        fn round_trip(message in message_strategy()) {
            let frame = encode(&message);
            let declared = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
            prop_assert_eq!(declared, frame.len() - 4);
            prop_assert_eq!(decode(&frame), Ok(message));
        }

        /// No proper prefix of a frame decodes, as a frame or as a payload.
        #[test]
        fn truncation_never_decodes(message in message_strategy()) {
            let frame = encode(&message);
            for len in 0..frame.len() {
                let truncated = &frame[..len];
                let Err(_) = decode(truncated) else {
                    return Err(TestCaseError::fail(format!("frame cut to {len} bytes decodes")));
                };
                if let Some(payload) = truncated.get(4..) {
                    let Err(_) = decode_payload(payload) else {
                        return Err(TestCaseError::fail(format!(
                            "payload cut to {} bytes decodes",
                            len - 4
                        )));
                    };
                }
            }
        }

        #[test]
        fn trailing_bytes_rejected(message in message_strategy(), extra in 1usize..8) {
            let mut frame = encode(&message);
            let payload_len = frame.len() - 4;
            frame.extend(std::iter::repeat_n(0u8, extra));
            let mut payload = frame[4..].to_vec();
            payload.truncate(payload_len + extra);
            prop_assert_eq!(decode(&frame), Err(DecodeError::Trailing(extra)));
            prop_assert_eq!(decode_payload(&payload), Err(DecodeError::Trailing(extra)));
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        for _ in 0..20_000 {
            let len = rng.gen_range(0..600);
            let mut buf: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
            let _ = decode(&buf);
            let _ = decode_payload(&buf);
            // The same bytes with a valid tag and a consistent length prefix exercise the
            // field parsers rather than failing on the prefix.
            if buf.len() >= 5 {
                buf[4] = rng.gen_range(0..14);
                let len = (buf.len() - 4) as u32;
                buf[..4].copy_from_slice(&len.to_le_bytes());
                let _ = decode(&buf);
            }
        }
    }

    #[test]
    fn oversize_prefix_rejected() {
        let mut frame = ((MAX_PAYLOAD + 1) as u32).to_le_bytes().to_vec();
        frame.push(TAG_TX_ANNOUNCE);
        assert_eq!(decode(&frame), Err(DecodeError::Oversize(MAX_PAYLOAD + 1)));
    }

    #[test]
    fn huge_count_rejected_before_allocation() {
        // TxAnnounce with a CompactSize count of 0x01ff_ffff ids and no bytes behind it.
        let payload = [TAG_TX_ANNOUNCE, 0xfe, 0xff, 0xff, 0xff, 0x01];
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert_eq!(
            decode(&frame),
            Err(DecodeError::CountTooLarge {
                field: "wtxids",
                count: 0x01ff_ffff,
                remaining: 0
            })
        );
    }

    #[test]
    fn non_canonical_compact_size_rejected() {
        // Count 1 encoded with the 3-byte form.
        let payload = [TAG_BATCH_REQUEST, 0xfd, 0x01, 0x00];
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert_eq!(decode(&frame), Err(DecodeError::CompactSize("batch_ids")));
    }

    #[test]
    fn unknown_type_rejected() {
        let payload = [42u8];
        let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(&payload);
        assert_eq!(decode(&frame), Err(DecodeError::UnknownType(42)));
        assert_eq!(decode(&[0, 0, 0, 0]), Err(DecodeError::Empty));
    }

    /// A serialized header with every fixed field 0x11 and a solution of `params`.
    fn header_bytes(params: PowParams) -> Bytes {
        let header = BlockHeader {
            version: 0x1111_1111,
            prev_hash: BlockHash([0x11; 32]),
            merkle_root: [0x11; 32],
            block_commitments: [0x11; 32],
            time: 0x1111_1111,
            bits: 0x1111_1111,
            nonce: [0x11; 32],
            solution: vec![0x11; params.solution_len()],
        };
        Bytes::from(header.serialize())
    }

    fn compact_block_frame(prefilled: Vec<PrefilledTx>, full_ids: Vec<FullId>) -> Vec<u8> {
        compact_block_frame_with(header_bytes(PowParams::MAINNET), prefilled, full_ids)
    }

    fn compact_block_frame_with(
        header: Bytes,
        prefilled: Vec<PrefilledTx>,
        full_ids: Vec<FullId>,
    ) -> Vec<u8> {
        encode(&Message::CompactBlock(Box::new(CompactBlock {
            header,
            nonce: 5,
            batch_refs: vec![BatchId([2; 32])],
            short_ids: vec![ShortId([3; 6]), ShortId([4; 6])],
            prefilled,
            full_ids,
        })))
    }

    /// Type 6 ends after the prefilled transactions. Type 12 has the same fields, then
    /// `full_count` and the `(differential index, WtxId)` entries.
    #[test]
    fn compact_block_type_names_the_layout() {
        let prefilled = vec![PrefilledTx {
            index: 0,
            bytes: Bytes::from_static(&[9, 9]),
        }];
        let v1 = compact_block_frame(prefilled.clone(), vec![]);
        // Tag, then the 1487 header bytes as they are, then the nonce.
        assert_eq!(v1[4], TAG_COMPACT_BLOCK);
        assert_eq!(&v1[5..5 + 1487], &header_bytes(PowParams::MAINNET)[..]);
        assert_eq!(&v1[5 + 1487..5 + 1487 + 8], &5u64.to_le_bytes());
        let tail = &v1[4 + 1 + 1487 + 8..];
        // batch_count 1, batch id, short_count 2, two short ids, prefilled_count 1, index 0,
        // len 2, bytes; nothing after.
        let mut expected = vec![1u8];
        expected.extend_from_slice(&[2; 32]);
        expected.push(2);
        expected.extend_from_slice(&[3; 6]);
        expected.extend_from_slice(&[4; 6]);
        expected.extend_from_slice(&[1, 0, 2, 0, 0, 0, 9, 9]);
        assert_eq!(tail, &expected[..]);
        let Ok(Message::CompactBlock(decoded)) = decode(&v1) else {
            panic!("type 6 frame decodes");
        };
        assert!(decoded.full_ids.is_empty());

        let id_a = WtxId {
            txid: TxId::from_bytes([0xaa; 32]),
            auth_digest: [0xab; 32],
        };
        let id_b = WtxId {
            txid: TxId::from_bytes([0xba; 32]),
            auth_digest: [0xbb; 32],
        };
        let full_ids = vec![FullId { index: 2, id: id_a }, FullId { index: 7, id: id_b }];
        let v2 = compact_block_frame(prefilled, full_ids.clone());
        assert_eq!(v2[4], TAG_COMPACT_BLOCK_V2);
        assert_eq!(
            &v2[5..v1.len()],
            &v1[5..],
            "the same fields before the section"
        );
        // full_count 2, index 2, WtxId, gap 7 - 2 - 1 = 4, WtxId.
        let mut section = vec![2u8, 2];
        section.extend_from_slice(&id_a.to_bytes());
        section.push(4);
        section.extend_from_slice(&id_b.to_bytes());
        assert_eq!(&v2[v1.len()..], &section[..]);
        let Ok(Message::CompactBlock(decoded)) = decode(&v2) else {
            panic!("type 12 frame decodes");
        };
        assert_eq!(decoded.full_ids, full_ids);

        let with_len = |payload: &[u8]| {
            let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
            frame.extend_from_slice(payload);
            frame
        };
        // Type 12 cut before its full-id section is not a type 6 payload: it does not decode.
        assert_eq!(
            decode(&with_len(&v2[4..v1.len()])),
            Err(DecodeError::CompactSize("full_ids"))
        );
        // Type 6 has no full-id section: the bytes of one are trailing bytes.
        let mut relabelled = v2[4..].to_vec();
        relabelled[0] = TAG_COMPACT_BLOCK;
        assert_eq!(
            decode(&with_len(&relabelled)),
            Err(DecodeError::Trailing(section.len()))
        );
        // Type 6 is the one encoding of a block without full ids: type 12 with a zero count
        // is malformed.
        let mut zero_count = v1[4..].to_vec();
        zero_count[0] = TAG_COMPACT_BLOCK_V2;
        zero_count.push(0);
        assert_eq!(
            decode(&with_len(&zero_count)),
            Err(DecodeError::EmptySection("full_ids"))
        );
    }

    /// One message of each type code. Each proper prefix of the frame fails to decode.
    #[test]
    fn every_type_rejects_every_truncation() {
        let id = WtxId {
            txid: TxId::from_bytes([0xaa; 32]),
            auth_digest: [0xab; 32],
        };
        let header = header_bytes(PowParams::REGTEST);
        let blob = Bytes::from_static(&[9, 9, 9]);
        let compact = |full_ids| {
            Message::CompactBlock(Box::new(CompactBlock {
                header: header.clone(),
                nonce: 5,
                batch_refs: vec![BatchId([2; 32])],
                short_ids: vec![ShortId([3; 6])],
                prefilled: vec![PrefilledTx {
                    index: 0,
                    bytes: blob.clone(),
                }],
                full_ids,
            }))
        };
        let messages = [
            Message::TxAnnounce(TxAnnounce { ids: vec![id] }),
            Message::TxRequest(TxRequest { ids: vec![id] }),
            Message::Tx(Tx {
                txs: vec![blob.clone()],
            }),
            Message::BatchAnnounce(BatchAnnounce {
                lane_id: [1; 32],
                seq: 2,
                batch_id: BatchId([3; 32]),
                ids: vec![id],
            }),
            Message::BatchRequest(BatchRequest {
                ids: vec![BatchId([3; 32])],
            }),
            compact(vec![]),
            Message::BlockTxnRequest(BlockTxnRequest {
                block_hash: BlockHash([4; 32]),
                indexes: vec![1, 3],
            }),
            Message::BlockTxn(BlockTxn {
                block_hash: BlockHash([4; 32]),
                txs: vec![blob.clone()],
            }),
            Message::Block(Block {
                bytes: blob.clone(),
            }),
            Message::CandidateAnnounce(CandidateAnnounce {
                lane_id: [1; 32],
                seq: 2,
                parent: BlockHash([4; 32]),
                batches: vec![BatchId([3; 32])],
                removed: vec![0, 5],
            }),
            Message::CandidateBlock(Box::new(CandidateBlock {
                header: header.clone(),
                nonce: 3,
                lane_id: [7; 32],
                seq: 9,
                flags: CANONICAL_ORDER,
                coinbase: blob.clone(),
                removed: vec![1],
                short_ids: vec![ShortId([3; 6])],
                full_ids: vec![id],
            })),
            compact(vec![FullId { index: 4, id }]),
        ];
        for (message, tag) in messages.iter().zip(1u8..) {
            let frame = encode(message);
            assert_eq!(frame[4], tag);
            assert_eq!(decode(&frame).as_ref(), Ok(message));
            for len in 0..frame.len() {
                let Err(_) = decode(&frame[..len]) else {
                    panic!("type {tag}: frame cut to {len} bytes decodes");
                };
                if let Some(payload) = frame[..len].get(4..) {
                    let Err(_) = decode_payload(payload) else {
                        panic!("type {tag}: payload cut to {} bytes decodes", len - 4);
                    };
                }
            }
        }
        assert_eq!(messages.len(), usize::from(TAG_COMPACT_BLOCK_V2));
    }

    /// A Regtest header (36-byte solution) takes 177 bytes in the frame; the fields after
    /// it are the fields of a Mainnet frame. A header whose solution length matches no known
    /// parameter set is rejected.
    #[test]
    fn compact_block_header_length_follows_the_solution_prefix() {
        let regtest = header_bytes(PowParams::REGTEST);
        assert_eq!(regtest.len(), 177);
        let short = compact_block_frame_with(regtest.clone(), vec![], vec![]);
        let long = compact_block_frame(vec![], vec![]);
        assert_eq!(long.len() - short.len(), 1487 - 177);
        assert_eq!(&short[5..5 + 177], &regtest[..]);
        assert_eq!(&short[5 + 177..], &long[5 + 1487..]);
        let Ok(Message::CompactBlock(decoded)) = decode(&short) else {
            panic!("Regtest frame decodes");
        };
        assert_eq!(decoded.header, regtest);

        // Solution length 37 at offset 140 of the header.
        let mut bad = short.clone();
        bad[5 + 140] = 37;
        let Err(DecodeError::Header(_)) = decode(&bad) else {
            panic!("unknown solution length accepted");
        };
        // A frame that ends inside the solution.
        let mut cut = short[..5 + 100].to_vec();
        let len = (cut.len() - 4) as u32;
        cut[..4].copy_from_slice(&len.to_le_bytes());
        let Err(DecodeError::Header(_)) = decode(&cut) else {
            panic!("truncated header accepted");
        };
    }

    /// The candidate reference costs 57 bytes of payload beyond the header and the coinbase
    /// bytes: tag, nonce, lane, seq, flags, coinbase length and three empty counts.
    #[test]
    fn candidate_block_layout() {
        let header = header_bytes(PowParams::MAINNET);
        let message = Message::CandidateBlock(Box::new(CandidateBlock {
            header: header.clone(),
            nonce: 3,
            lane_id: [7; 32],
            seq: 9,
            flags: CANONICAL_ORDER,
            coinbase: Bytes::from_static(&[0xc0; 5]),
            removed: vec![],
            short_ids: vec![],
            full_ids: vec![],
        }));
        let frame = encode(&message);
        assert_eq!(frame.len() - 4 - header.len() - 5, 57);
        let mut expected = vec![TAG_CANDIDATE_BLOCK];
        expected.extend_from_slice(&header);
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(&[7; 32]);
        expected.extend_from_slice(&9u64.to_le_bytes());
        expected.push(1);
        expected.extend_from_slice(&5u32.to_le_bytes());
        expected.extend_from_slice(&[0xc0; 5]);
        expected.extend_from_slice(&[0, 0, 0]);
        assert_eq!(&frame[4..], &expected[..]);
        assert_eq!(decode(&frame), Ok(message));

        let announce = Message::CandidateAnnounce(CandidateAnnounce {
            lane_id: [1; 32],
            seq: 2,
            parent: BlockHash([3; 32]),
            batches: vec![BatchId([4; 32])],
            removed: vec![0, 5],
        });
        let frame = encode(&announce);
        // tag, lane, seq, parent, one batch, two removed positions (0, then gap 4).
        assert_eq!(frame.len() - 4, 1 + 32 + 8 + 32 + 1 + 32 + 1 + 2);
        assert_eq!(&frame[frame.len() - 3..], &[2, 0, 4]);
        assert_eq!(decode(&frame), Ok(announce));
    }

    #[test]
    fn differential_indexes_match_bip152() {
        let message = Message::BlockTxnRequest(BlockTxnRequest {
            block_hash: BlockHash([0; 32]),
            indexes: vec![0, 1, 5, 6, 300],
        });
        let frame = encode(&message);
        // tag, 32-byte hash, count 5, then 0, 0, 3, 0, 293 (0xfd 0x25 0x01).
        let tail = &frame[4 + 1 + 32..];
        assert_eq!(tail, &[5, 0, 0, 3, 0, 0xfd, 0x25, 0x01]);
        assert_eq!(decode(&frame), Ok(message));
    }
}
