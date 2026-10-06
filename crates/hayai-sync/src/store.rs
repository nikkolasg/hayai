//! The header log: every accepted header of the header chain, on disk.
//!
//! The log is one append-only file. A record has the frame of the coin log
//! (`hayai-coins/src/mem/log.rs`). Integers are little endian.
//!
//! ```text
//! frame    magic u32 | payload length u64 | payload CRC32C u32 | frame CRC32C u32
//! payload  kind u8 | body
//!          kind 0: the serialized block header
//!          kind 1: the hash of a block whose body is not valid (32 bytes)
//!          kind 2: the hash of a header that the chain removed and accepted again
//!                  (32 bytes); the log has the header in an earlier record of kind 0
//! ```
//!
//! The frame CRC covers the first 16 frame bytes, so the reader detects a damaged length
//! before it uses the length. The order of the records is the order in which the chain
//! accepted them. The chain applies the records in that order at start and gets the same
//! entries.
//!
//! The log does not call `fsync` for each record. [`HeaderLog::sync`] does it. The node
//! downloads again the headers that a crash of the operating system removes.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

use hayai_wire::header::{BlockHash, BlockHeader, PowParams};

/// The bytes `HHLG` in file order.
const MAGIC: u32 = 0x474c_4848;
const FRAME_BYTES: usize = 20;
const KIND_HEADER: u8 = 0;
const KIND_INVALID: u8 = 1;
const KIND_AGAIN: u8 = 2;
/// Longest payload: the kind byte and a header with a (200, 9) solution.
const MAX_PAYLOAD: u64 = 1 + PowParams::MAINNET.header_len() as u64;

/// One record of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// An accepted header.
    Header(BlockHeader),
    /// The body of this block is not valid.
    Invalid(BlockHash),
    /// The chain accepted again this header, which it removed after an earlier record.
    Again(BlockHash),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("header log: {0}")]
    Io(#[from] io::Error),
    /// Damage that is not a torn tail. The log does not repair it.
    #[error("the header log is damaged at offset {offset}: {reason}")]
    Corrupt { offset: u64, reason: &'static str },
}

/// What [`HeaderLog::open`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadReport {
    /// Valid records.
    pub records: u64,
    /// Bytes of an incomplete last record. The log cut them.
    pub torn_bytes: u64,
}

/// The open header log.
#[derive(Debug)]
pub struct HeaderLog {
    file: File,
    len: u64,
}

/// What the reader found at an offset.
enum Next {
    Record(Record, u64),
    End,
    TornTail,
    Corrupt(&'static str),
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; FRAME_BYTES];
    out.extend_from_slice(payload);
    out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    out[4..12].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    out[12..16].copy_from_slice(&crc32c::crc32c(payload).to_le_bytes());
    let frame_crc = crc32c::crc32c(&out[0..16]);
    out[16..20].copy_from_slice(&frame_crc.to_le_bytes());
    out
}

/// The payload length and the payload CRC of a frame. `None` when the frame fails its own
/// check.
fn parse_frame(frame: &[u8; FRAME_BYTES]) -> Option<(u64, u32)> {
    let u32_at = |at: usize| u32::from_le_bytes(frame[at..at + 4].try_into().expect("4 bytes"));
    if u32_at(0) != MAGIC || u32_at(16) != crc32c::crc32c(&frame[0..16]) {
        return None;
    }
    let len = u64::from_le_bytes(frame[4..12].try_into().expect("8 bytes"));
    Some((len, u32_at(12)))
}

fn decode(payload: &[u8]) -> Result<Record, &'static str> {
    let Some((kind, body)) = payload.split_first() else {
        return Err("record has no kind");
    };
    match *kind {
        KIND_HEADER => {
            let Ok(header) = BlockHeader::parse(body) else {
                return Err("header record does not parse");
            };
            if header.serialized_len() != body.len() {
                return Err("header record has trailing bytes");
            }
            Ok(Record::Header(header))
        }
        KIND_INVALID => {
            let Ok(hash) = <[u8; 32]>::try_from(body) else {
                return Err("invalid-block record is not 32 bytes");
            };
            Ok(Record::Invalid(BlockHash(hash)))
        }
        KIND_AGAIN => {
            let Ok(hash) = <[u8; 32]>::try_from(body) else {
                return Err("accepted-again record is not 32 bytes");
            };
            Ok(Record::Again(BlockHash(hash)))
        }
        _ => Err("record has an unknown kind"),
    }
}

/// Whether the rest of `reader` is only zero bytes.
fn rest_is_zero(reader: &mut impl Read) -> io::Result<bool> {
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(true);
        }
        if chunk[..n].iter().any(|b| *b != 0) {
            return Ok(false);
        }
    }
}

/// Reads the record at the position of `reader`, with `left` bytes to the end of the file.
///
/// Torn-tail rules, for a crash that interrupts an append: fewer than [`FRAME_BYTES`] bytes
/// left; a frame that fails its CRC and only zero bytes after it; a valid frame whose
/// payload passes the end of the file; a valid frame whose payload fails its CRC and ends
/// at the end of the file. Every other failure is damage.
fn read_record(reader: &mut impl Read, left: u64) -> io::Result<Next> {
    if left == 0 {
        return Ok(Next::End);
    }
    if left < FRAME_BYTES as u64 {
        return Ok(Next::TornTail);
    }
    let mut frame = [0u8; FRAME_BYTES];
    reader.read_exact(&mut frame)?;
    let Some((payload_len, payload_crc)) = parse_frame(&frame) else {
        if frame.iter().all(|b| *b == 0) && rest_is_zero(reader)? {
            return Ok(Next::TornTail);
        }
        return Ok(Next::Corrupt("record frame fails its checksum"));
    };
    if payload_len > MAX_PAYLOAD {
        return Ok(Next::Corrupt("record is longer than a header record"));
    }
    let record_len = FRAME_BYTES as u64 + payload_len;
    if record_len > left {
        return Ok(Next::TornTail);
    }
    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload)?;
    if crc32c::crc32c(&payload) != payload_crc {
        if record_len == left {
            return Ok(Next::TornTail);
        }
        return Ok(Next::Corrupt("record payload fails its checksum"));
    }
    match decode(&payload) {
        Ok(record) => Ok(Next::Record(record, record_len)),
        Err(reason) => Ok(Next::Corrupt(reason)),
    }
}

impl HeaderLog {
    /// Opens the log at `path` and creates it when it does not exist. `apply` gets each
    /// record and its offset, in file order. The function cuts a torn tail and reports it.
    /// Other damage is [`StoreError::Corrupt`].
    pub fn open<E: From<StoreError>>(
        path: &Path,
        mut apply: impl FnMut(u64, Record) -> Result<(), E>,
    ) -> Result<(Self, LoadReport), E> {
        let io = |e: io::Error| E::from(StoreError::Io(e));
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)
            .map_err(io)?;
        let file_len = file.metadata().map_err(io)?.len();
        let mut reader = BufReader::with_capacity(1 << 20, &file);
        let mut offset = 0u64;
        let mut report = LoadReport {
            records: 0,
            torn_bytes: 0,
        };
        loop {
            match read_record(&mut reader, file_len - offset).map_err(io)? {
                Next::Record(record, len) => {
                    apply(offset, record)?;
                    offset += len;
                    report.records += 1;
                }
                Next::End => break,
                Next::TornTail => {
                    report.torn_bytes = file_len - offset;
                    file.set_len(offset).map_err(io)?;
                    break;
                }
                Next::Corrupt(reason) => {
                    return Err(StoreError::Corrupt { offset, reason }.into());
                }
            }
        }
        Ok((Self { file, len: offset }, report))
    }

    /// Appends one record and returns its offset. A failed write leaves the log as it was.
    fn append(&mut self, payload: &[u8]) -> Result<u64, StoreError> {
        let record = frame(payload);
        if let Err(e) = self.file.write_all(&record) {
            // Remove the bytes of a partial write, so that the next record starts at
            // `self.len`.
            self.file.set_len(self.len)?;
            return Err(e.into());
        }
        let offset = self.len;
        self.len += record.len() as u64;
        Ok(offset)
    }

    /// Appends `header` and returns the offset of its record.
    pub fn append_header(&mut self, header: &BlockHeader) -> Result<u64, StoreError> {
        let mut payload = vec![KIND_HEADER];
        payload.extend_from_slice(&header.serialize());
        self.append(&payload)
    }

    /// Records that the body of block `hash` is not valid.
    pub fn append_invalid(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        let mut payload = vec![KIND_INVALID];
        payload.extend_from_slice(&hash.0);
        self.append(&payload)?;
        Ok(())
    }

    /// Records that the chain accepted again the removed header `hash`. The log has the
    /// header one time, in its first record.
    pub fn append_again(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        let mut payload = vec![KIND_AGAIN];
        payload.extend_from_slice(&hash.0);
        self.append(&payload)?;
        Ok(())
    }

    /// Reads the header whose record starts at `offset`.
    pub fn read_header(&self, offset: u64) -> Result<BlockHeader, StoreError> {
        let corrupt = |reason| StoreError::Corrupt { offset, reason };
        let mut frame = [0u8; FRAME_BYTES];
        self.file.read_exact_at(&mut frame, offset)?;
        let Some((payload_len, payload_crc)) = parse_frame(&frame) else {
            return Err(corrupt("record frame fails its checksum"));
        };
        if payload_len > MAX_PAYLOAD {
            return Err(corrupt("record is longer than a header record"));
        }
        let mut payload = vec![0u8; payload_len as usize];
        self.file
            .read_exact_at(&mut payload, offset + FRAME_BYTES as u64)?;
        if crc32c::crc32c(&payload) != payload_crc {
            return Err(corrupt("record payload fails its checksum"));
        }
        match decode(&payload).map_err(corrupt)? {
            Record::Header(header) => Ok(header),
            Record::Invalid(_) | Record::Again(_) => Err(corrupt("record is not a header")),
        }
    }

    /// Length of the log in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the log has no record.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Makes the records durable (`fsync`).
    pub fn sync(&self) -> Result<(), StoreError> {
        Ok(self.file.sync_data()?)
    }
}
