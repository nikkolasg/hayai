//! A block as a value that the mutations change: the header fields, and each transaction
//! as its transparent parts plus the bytes after them.
//!
//! The model has its own encoder. It can write values that no parser accepts (a value
//! above the money limit, a count in a long encoding), which the builders of the Zcash
//! crates refuse to make.

use serde::{Deserialize, Serialize};

/// Bytes of a block header before the solution: version, parent, merkle root,
/// commitments, time, bits, nonce.
pub const HEADER_PREFIX: usize = 140;
/// Offset of the merkle root in a serialized header.
pub const MERKLE_OFFSET: usize = 36;
/// Offset of the block commitments field in a serialized header.
pub const COMMITMENTS_OFFSET: usize = 68;

pub const V4_GROUP: u32 = 0x892F_2085;
pub const V5_GROUP: u32 = 0x26A7_270A;
pub const V6_GROUP: u32 = 0xD884_B698;
const OVERWINTERED: u32 = 0x8000_0000;

/// Bytes of one Orchard or Ironwood action on the wire.
pub const ACTION_BYTES: usize = 820;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub prev: [u8; 32],
    pub merkle: [u8; 32],
    pub commitments: [u8; 32],
    pub time: u32,
    pub bits: u32,
    pub nonce: [u8; 32],
    pub solution: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxIn {
    pub prev_hash: [u8; 32],
    pub prev_index: u32,
    #[serde(with = "hex::serde")]
    pub script: Vec<u8>,
    pub sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOut {
    /// The 8 value bytes of the wire, as an unsigned number.
    pub value: u64,
    #[serde(with = "hex::serde")]
    pub script: Vec<u8>,
}

/// The order of the fields of a transaction on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    /// Version 4: header, group, inputs, outputs, lock time, expiry, then the tail.
    V4,
    /// Versions 5 and 6: header, group, branch, lock time, expiry, inputs, outputs, then
    /// the tail.
    V5,
}

/// A transaction as its transparent parts and its tail. The tail is every byte after the
/// transparent parts: the shielded sections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxParts {
    pub layout: Layout,
    /// The first 4 bytes: the version with the overwintered bit.
    pub header: u32,
    pub group: u32,
    /// The consensus branch id. The V4 layout does not write it.
    pub branch: u32,
    pub lock_time: u32,
    pub expiry: u32,
    pub vin: Vec<TxIn>,
    pub vout: Vec<TxOut>,
    #[serde(with = "hex::serde")]
    pub tail: Vec<u8>,
    /// Bytes of the encoding of the input count, the output count and the script lengths:
    /// 3, 5 or 9. `None` is the shortest encoding.
    #[serde(default)]
    pub count_width: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tx {
    Parts(TxParts),
    /// Bytes that the model does not read.
    Raw(#[serde(with = "hex::serde")] Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: Header,
    pub txs: Vec<Tx>,
    /// The transaction count that the block states, when it is not the number of
    /// transactions.
    pub stated_count: Option<u64>,
    /// Bytes of the encoding of the transaction count: 1, 3, 5 or 9. `None` is the
    /// shortest encoding.
    pub count_width: Option<u8>,
}

pub fn write_compact(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => write_compact_wide(out, n, 3),
        0x1_0000..=0xffff_ffff => write_compact_wide(out, n, 5),
        _ => write_compact_wide(out, n, 9),
    }
}

/// Writes `n` in the encoding of `width` bytes (3, 5 or 9), also when a shorter encoding
/// exists. A width of 1 writes the low byte.
pub fn write_compact_wide(out: &mut Vec<u8>, n: u64, width: u8) {
    match width {
        3 => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        5 => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        9 => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
        _ => out.push(n as u8),
    }
}

/// A reader of the wire format of a seed. A seed is a valid block, so a failure is an
/// error of the fuzzer, and the reader panics.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> &'a [u8] {
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        slice
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().expect("4 bytes"))
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().expect("8 bytes"))
    }
    fn hash(&mut self) -> [u8; 32] {
        self.take(32).try_into().expect("32 bytes")
    }
    fn compact(&mut self) -> u64 {
        match self.take(1)[0] {
            0xfd => u64::from(u16::from_le_bytes(
                self.take(2).try_into().expect("2 bytes"),
            )),
            0xfe => u64::from(self.u32()),
            0xff => self.u64(),
            n => u64::from(n),
        }
    }
    /// A count from bytes that a mutation can change. `None`: the bytes end before the
    /// count ends, or the count is not a `usize`.
    fn try_compact(&mut self) -> Option<usize> {
        let width = match *self.bytes.get(self.pos)? {
            0xfd => 3,
            0xfe => 5,
            0xff => 9,
            _ => 1,
        };
        if self.pos + width > self.bytes.len() {
            return None;
        }
        usize::try_from(self.compact()).ok()
    }
    fn var_bytes(&mut self) -> Vec<u8> {
        let len = self.compact() as usize;
        self.take(len).to_vec()
    }
}

impl Header {
    /// Reads a header and returns it with its length in bytes.
    pub fn parse(bytes: &[u8]) -> (Self, usize) {
        let mut r = Reader { bytes, pos: 0 };
        let header = Header {
            version: r.u32(),
            prev: r.hash(),
            merkle: r.hash(),
            commitments: r.hash(),
            time: r.u32(),
            bits: r.u32(),
            nonce: r.hash(),
            solution: r.var_bytes(),
        };
        (header, r.pos)
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.prev);
        out.extend_from_slice(&self.merkle);
        out.extend_from_slice(&self.commitments);
        out.extend_from_slice(&self.time.to_le_bytes());
        out.extend_from_slice(&self.bits.to_le_bytes());
        out.extend_from_slice(&self.nonce);
        write_compact(out, self.solution.len() as u64);
        out.extend_from_slice(&self.solution);
    }
}

impl TxParts {
    /// Reads a version 5 or version 6 transaction of a seed.
    pub fn parse_v5(bytes: &[u8]) -> Self {
        let mut r = Reader { bytes, pos: 0 };
        let header = r.u32();
        assert!(
            matches!(header & !OVERWINTERED, 5 | 6),
            "the seed transactions are version 5 or 6"
        );
        let group = r.u32();
        let branch = r.u32();
        let lock_time = r.u32();
        let expiry = r.u32();
        let vin = (0..r.compact())
            .map(|_| TxIn {
                prev_hash: r.hash(),
                prev_index: r.u32(),
                script: r.var_bytes(),
                sequence: r.u32(),
            })
            .collect();
        let vout = (0..r.compact())
            .map(|_| TxOut {
                value: r.u64(),
                script: r.var_bytes(),
            })
            .collect();
        TxParts {
            layout: Layout::V5,
            header,
            group,
            branch,
            lock_time,
            expiry,
            vin,
            vout,
            tail: bytes[r.pos..].to_vec(),
            count_width: None,
        }
    }

    /// A transparent transaction of version 4, 5 or 6 without inputs and outputs.
    pub fn transparent(version: u32, branch: u32) -> Self {
        let (layout, group, tail) = match version {
            4 => (Layout::V4, V4_GROUP, vec![0u8; 11]),
            5 => (Layout::V5, V5_GROUP, vec![0u8; 3]),
            6 => (Layout::V5, V6_GROUP, vec![0u8; 4]),
            _ => panic!("version {version} has no transparent template"),
        };
        TxParts {
            layout,
            header: OVERWINTERED | version,
            group,
            branch,
            lock_time: 0,
            expiry: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            tail,
            count_width: None,
        }
    }

    fn write_transparent(&self, out: &mut Vec<u8>) {
        let count = |out: &mut Vec<u8>, n: usize| match self.count_width {
            Some(width) => write_compact_wide(out, n as u64, width),
            None => write_compact(out, n as u64),
        };
        count(out, self.vin.len());
        for input in &self.vin {
            out.extend_from_slice(&input.prev_hash);
            out.extend_from_slice(&input.prev_index.to_le_bytes());
            count(out, input.script.len());
            out.extend_from_slice(&input.script);
            out.extend_from_slice(&input.sequence.to_le_bytes());
        }
        count(out, self.vout.len());
        for output in &self.vout {
            out.extend_from_slice(&output.value.to_le_bytes());
            count(out, output.script.len());
            out.extend_from_slice(&output.script);
        }
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.header.to_le_bytes());
        out.extend_from_slice(&self.group.to_le_bytes());
        match self.layout {
            Layout::V4 => {
                self.write_transparent(out);
                out.extend_from_slice(&self.lock_time.to_le_bytes());
                out.extend_from_slice(&self.expiry.to_le_bytes());
            }
            Layout::V5 => {
                out.extend_from_slice(&self.branch.to_le_bytes());
                out.extend_from_slice(&self.lock_time.to_le_bytes());
                out.extend_from_slice(&self.expiry.to_le_bytes());
                self.write_transparent(out);
            }
        }
        out.extend_from_slice(&self.tail);
    }

    /// The Orchard section and the Ironwood section of the tail that have actions.
    pub fn sections(&self) -> Vec<Section> {
        if self.layout != Layout::V5 {
            return Vec::new();
        }
        let version = self.header & !OVERWINTERED;
        let mut r = Reader {
            bytes: &self.tail,
            pos: 0,
        };
        let mut sections = Vec::new();
        // The model reads a tail without Sapling parts only: the seeds have none.
        if self.tail.len() < 3 || self.tail[0] != 0 || self.tail[1] != 0 {
            return sections;
        }
        r.pos = 2;
        let pools = if version == 6 { 2 } else { 1 };
        for pool in 0..pools {
            if r.pos >= self.tail.len() {
                break;
            }
            let start = r.pos;
            let Some(actions) = r.try_compact() else {
                break;
            };
            if actions == 0 {
                continue;
            }
            let actions_at = r.pos;
            // The counts come from bytes that a mutation can change: every sum is checked.
            let Some(flags_at) = actions
                .checked_mul(ACTION_BYTES)
                .and_then(|len| actions_at.checked_add(len))
                .filter(|flags_at| *flags_at + 41 < self.tail.len())
            else {
                break;
            };
            r.pos = flags_at + 41;
            let proof_len_at = r.pos;
            let Some(proof_len) = r.try_compact() else {
                break;
            };
            let proof_at = r.pos;
            let Some((sigs_at, end)) = proof_at.checked_add(proof_len).and_then(|sigs_at| {
                let sigs = actions.checked_mul(64)?.checked_add(64)?;
                Some((sigs_at, sigs_at.checked_add(sigs)?))
            }) else {
                break;
            };
            if end > self.tail.len() {
                break;
            }
            r.pos = end;
            sections.push(Section {
                ironwood: pool == 1,
                start,
                actions,
                actions_at,
                flags_at,
                proof_len_at,
                proof_at,
                proof_len,
                sigs_at,
                end,
            });
        }
        sections
    }
}

/// Offsets of one Orchard or Ironwood section in the tail of a transaction.
#[derive(Clone, Copy, Debug)]
pub struct Section {
    pub ironwood: bool,
    pub start: usize,
    pub actions: usize,
    pub actions_at: usize,
    /// The flags byte. The value balance (8 bytes) and the anchor (32 bytes) follow it.
    pub flags_at: usize,
    pub proof_len_at: usize,
    pub proof_at: usize,
    pub proof_len: usize,
    /// The spend authorization signatures. The binding signature follows them.
    pub sigs_at: usize,
    pub end: usize,
}

impl Tx {
    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Tx::Parts(parts) => parts.write(out),
            Tx::Raw(bytes) => out.extend_from_slice(bytes),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    pub fn parts_mut(&mut self) -> Option<&mut TxParts> {
        match self {
            Tx::Parts(parts) => Some(parts),
            Tx::Raw(_) => None,
        }
    }

    /// Makes the transaction raw bytes and returns them.
    pub fn raw_mut(&mut self) -> &mut Vec<u8> {
        if let Tx::Parts(parts) = self {
            let mut bytes = Vec::new();
            parts.write(&mut bytes);
            *self = Tx::Raw(bytes);
        }
        let Tx::Raw(bytes) = self else {
            unreachable!("the transaction is raw now");
        };
        bytes
    }
}

impl Block {
    /// Reads a seed block. `tx_lengths` gives the length of each transaction.
    pub fn parse(bytes: &[u8], tx_lengths: &[usize]) -> Self {
        let (header, mut pos) = Header::parse(bytes);
        let mut r = Reader { bytes, pos };
        let count = r.compact() as usize;
        assert_eq!(count, tx_lengths.len(), "one length for each transaction");
        pos = r.pos;
        let mut txs = Vec::with_capacity(count);
        for len in tx_lengths {
            txs.push(Tx::Parts(TxParts::parse_v5(&bytes[pos..pos + len])));
            pos += len;
        }
        assert_eq!(pos, bytes.len(), "the lengths cover the block");
        let block = Block {
            header,
            txs,
            stated_count: None,
            count_width: None,
        };
        assert_eq!(block.bytes(), bytes, "the model writes the seed again");
        block
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.header.write(&mut out);
        let count = self.stated_count.unwrap_or(self.txs.len() as u64);
        match self.count_width {
            Some(width) => write_compact_wide(&mut out, count, width),
            None => write_compact(&mut out, count),
        }
        for tx in &self.txs {
            tx.write(&mut out);
        }
        out
    }
}
