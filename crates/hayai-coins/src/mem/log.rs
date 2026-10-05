//! The generation log of [`super::MemBacking`]: one record per write, appended.
//!
//! Record layout (integers little endian):
//!
//! ```text
//! header   magic u32 | payload length u64 | payload CRC32C u32 | header CRC32C u32
//! payload  sequence u64 | flags u8 | [best height u32 | best hash 32]
//!          | adds u32 | adds × coin record | spends u32 | spends × key 36
//!          | 4 × (count u32 | count × nullifier 32)            (pools in Pool::ALL order)
//! ```
//!
//! The header CRC covers the first 16 header bytes, so a damaged length is detected before
//! it is used. A record is valid when both CRCs match and the payload parses.

use std::fs::File;
use std::io::{self, Read};

use super::table::{decode_packed, encode_packed, Key, Packed};
use crate::BestBlock;

/// The bytes `HLCG` in file order.
const MAGIC: u32 = 0x4743_4c48;
pub(super) const HEADER_BYTES: usize = 20;
const FLAG_BEST_BLOCK: u8 = 1;

/// The content of one record: what one write does to the set.
#[derive(Debug, Default)]
pub(super) struct Batch {
    pub(super) best_block: Option<BestBlock>,
    pub(super) adds: Vec<Packed>,
    pub(super) spends: Vec<Key>,
    pub(super) nullifiers: [Vec<[u8; 32]>; 4],
}

impl Batch {
    /// The framed record of this batch under sequence number `seq`.
    pub(super) fn encode(&self, seq: u64) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_BYTES];
        out.extend_from_slice(&seq.to_le_bytes());
        match &self.best_block {
            Some(best) => {
                out.push(FLAG_BEST_BLOCK);
                out.extend_from_slice(&best.height.to_le_bytes());
                out.extend_from_slice(&best.hash);
            }
            None => out.push(0),
        }
        out.extend_from_slice(&count(self.adds.len()).to_le_bytes());
        for (entry, raw) in &self.adds {
            encode_packed(entry, raw.as_deref(), &mut out);
        }
        out.extend_from_slice(&count(self.spends.len()).to_le_bytes());
        for key in &self.spends {
            out.extend_from_slice(key);
        }
        for pool in &self.nullifiers {
            out.extend_from_slice(&count(pool.len()).to_le_bytes());
            for nullifier in pool {
                out.extend_from_slice(nullifier);
            }
        }
        let payload_len = (out.len() - HEADER_BYTES) as u64;
        let payload_crc = crc32c::crc32c(&out[HEADER_BYTES..]);
        out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        out[4..12].copy_from_slice(&payload_len.to_le_bytes());
        out[12..16].copy_from_slice(&payload_crc.to_le_bytes());
        let header_crc = crc32c::crc32c(&out[0..16]);
        out[16..20].copy_from_slice(&header_crc.to_le_bytes());
        out
    }

    /// Parses a payload whose CRC matched.
    fn decode(mut payload: &[u8]) -> Result<(u64, Batch), &'static str> {
        let buf = &mut payload;
        let seq = u64::from_le_bytes(take(buf)?);
        let [flags] = take::<1>(buf)?;
        let best_block = match flags {
            0 => None,
            FLAG_BEST_BLOCK => Some(BestBlock {
                height: u32::from_le_bytes(take(buf)?),
                hash: take(buf)?,
            }),
            _ => return Err("record has unknown flags"),
        };
        let mut batch = Batch {
            best_block,
            ..Batch::default()
        };
        // Counts are not trusted for preallocation: a count can only claim as many items
        // as the payload has bytes for.
        let adds = u32::from_le_bytes(take(buf)?);
        for _ in 0..adds {
            batch.adds.push(decode_packed(buf)?);
        }
        let spends = u32::from_le_bytes(take(buf)?);
        for _ in 0..spends {
            batch.spends.push(take(buf)?);
        }
        for pool in &mut batch.nullifiers {
            let n = u32::from_le_bytes(take(buf)?);
            for _ in 0..n {
                pool.push(take(buf)?);
            }
        }
        if !buf.is_empty() {
            return Err("record has trailing bytes");
        }
        Ok((seq, batch))
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).expect("fewer than 2^32 items in one write")
}

fn take<const N: usize>(buf: &mut &[u8]) -> Result<[u8; N], &'static str> {
    let Some((head, rest)) = buf.split_first_chunk::<N>() else {
        return Err("record payload is truncated");
    };
    *buf = rest;
    Ok(*head)
}

/// What reading a record at an offset gave.
pub(super) enum Next {
    /// A valid record and its framed length.
    Record { seq: u64, batch: Batch, len: u64 },
    /// The end of the log: the offset is the file length.
    End,
    /// A torn last record: the bytes from the offset to the end of the file are an
    /// incomplete write.
    TornTail,
    /// A damaged record that is not the last one, or a valid frame whose payload does not
    /// parse.
    Corrupt(&'static str),
}

/// Reads the record that starts at `offset` of a log of `file_len` bytes.
///
/// Torn-tail rules, for a crash that interrupts an append: fewer than [`HEADER_BYTES`]
/// bytes left; a header that fails its CRC followed only by zero bytes (an extension that
/// the file system filled with zeros); a valid header whose payload passes the end of the
/// file; a valid header whose payload fails its CRC and ends exactly at the end of the
/// file. Every other failure is corruption.
pub(super) fn read_record(file: &mut File, offset: u64, file_len: u64) -> io::Result<Next> {
    let left = file_len - offset;
    if left == 0 {
        return Ok(Next::End);
    }
    if left < HEADER_BYTES as u64 {
        return Ok(Next::TornTail);
    }
    let mut header = [0u8; HEADER_BYTES];
    file.read_exact(&mut header)?;
    let magic = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let header_crc = u32::from_le_bytes(header[16..20].try_into().expect("4 bytes"));
    if magic != MAGIC || header_crc != crc32c::crc32c(&header[0..16]) {
        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;
        if header.iter().chain(&rest).all(|&b| b == 0) {
            return Ok(Next::TornTail);
        }
        return Ok(Next::Corrupt("record header fails its checksum"));
    }
    let payload_len = u64::from_le_bytes(header[4..12].try_into().expect("8 bytes"));
    let payload_crc = u32::from_le_bytes(header[12..16].try_into().expect("4 bytes"));
    let record_len = HEADER_BYTES as u64 + payload_len;
    if record_len > left {
        return Ok(Next::TornTail);
    }
    let mut payload = vec![0u8; payload_len as usize];
    file.read_exact(&mut payload)?;
    if crc32c::crc32c(&payload) != payload_crc {
        if record_len == left {
            return Ok(Next::TornTail);
        }
        return Ok(Next::Corrupt("record payload fails its checksum"));
    }
    match Batch::decode(&payload) {
        Ok((seq, batch)) => Ok(Next::Record {
            seq,
            batch,
            len: record_len,
        }),
        Err(reason) => Ok(Next::Corrupt(reason)),
    }
}
