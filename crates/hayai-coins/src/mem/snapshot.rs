//! The snapshot file of [`super::MemBacking`]: the whole set at one log sequence number.
//!
//! Layout (integers little endian):
//!
//! ```text
//! header    magic "HAYAICS1" | version u32 | sequence u64 | flags u8 | best height u32
//!           | best hash 32 | shard count u32 | header CRC32C u32
//! sections  SHARDS coin sections, then SHARDS nullifier sections per pool (Pool::ALL order)
//! section   item count u64 | body length u64 | body CRC32C u32 | body
//! ```
//!
//! A coin section body is the coins of one shard in record form; a nullifier section body
//! is the sorted run of one shard. Each section has its own checksum, so the sections are
//! encoded and decoded in parallel, [`BATCH`] at a time, which also bounds the memory that
//! a write or a load holds beside the set.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use parking_lot::RwLock;
use rayon::prelude::*;

use super::nfset::NullifierShard;
use super::table::{decode_packed, CoinShard, MIN_RECORD_BYTES};
use super::{PersistError, SHARDS};
use crate::BestBlock;

const MAGIC: [u8; 8] = *b"HAYAICS1";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 8 + 4 + 8 + 1 + 4 + 32 + 4 + 4;
const SECTION_HEADER_BYTES: usize = 8 + 8 + 4;
const FLAG_BEST_BLOCK: u8 = 1;
/// Sections encoded or decoded per parallel round.
const BATCH: usize = 32;

/// The set read from a snapshot.
pub(super) struct Loaded {
    pub(super) seq: u64,
    pub(super) best_block: Option<BestBlock>,
    pub(super) coins: Vec<CoinShard>,
    pub(super) nullifiers: [Vec<NullifierShard>; 4],
}

struct Section {
    count: u64,
    body: Vec<u8>,
}

fn write_sections(file: &mut File, sections: Vec<Section>) -> io::Result<u64> {
    let mut written = 0;
    for section in sections {
        let mut head = [0u8; SECTION_HEADER_BYTES];
        head[0..8].copy_from_slice(&section.count.to_le_bytes());
        head[8..16].copy_from_slice(&(section.body.len() as u64).to_le_bytes());
        head[16..20].copy_from_slice(&crc32c::crc32c(&section.body).to_le_bytes());
        file.write_all(&head)?;
        file.write_all(&section.body)?;
        written += (SECTION_HEADER_BYTES + section.body.len()) as u64;
    }
    Ok(written)
}

/// Writes the snapshot to `path` (a new file) and syncs it; returns its size. The caller
/// holds the writer lock, so the shards do not change during the write.
pub(super) fn write(
    path: &Path,
    seq: u64,
    best_block: Option<BestBlock>,
    coins: &[RwLock<CoinShard>],
    nullifiers: &[Box<[RwLock<NullifierShard>]>; 4],
) -> io::Result<u64> {
    let mut file = File::create(path)?;
    let mut header = Vec::with_capacity(HEADER_BYTES);
    header.extend_from_slice(&MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());
    header.extend_from_slice(&seq.to_le_bytes());
    let best = best_block.unwrap_or(BestBlock {
        height: 0,
        hash: [0; 32],
    });
    header.push(match best_block {
        Some(_) => FLAG_BEST_BLOCK,
        None => 0,
    });
    header.extend_from_slice(&best.height.to_le_bytes());
    header.extend_from_slice(&best.hash);
    header.extend_from_slice(&(SHARDS as u32).to_le_bytes());
    header.extend_from_slice(&crc32c::crc32c(&header).to_le_bytes());
    file.write_all(&header)?;
    let mut written = HEADER_BYTES as u64;

    for group in coins.chunks(BATCH) {
        let sections = group
            .par_iter()
            .map(|shard| {
                let shard = shard.read();
                let mut body = Vec::new();
                shard.encode(&mut body);
                Section {
                    count: shard.len() as u64,
                    body,
                }
            })
            .collect();
        written += write_sections(&mut file, sections)?;
    }
    for pool in nullifiers {
        for group in pool.chunks(BATCH) {
            let sections = group
                .par_iter()
                .map(|shard| {
                    let run = shard.read().merged();
                    Section {
                        count: run.len() as u64,
                        body: run.into_iter().flatten().collect(),
                    }
                })
                .collect();
            written += write_sections(&mut file, sections)?;
        }
    }
    file.sync_all()?;
    Ok(written)
}

fn corrupt(path: &Path, reason: impl Into<String>) -> PersistError {
    PersistError::CorruptSnapshot {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> PersistError + '_ {
    move |source| PersistError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Reads [`BATCH`] or fewer section frames; the bodies are not checked yet.
fn read_sections(
    file: &mut File,
    path: &Path,
    n: usize,
    left: &mut u64,
) -> Result<Vec<Section>, PersistError> {
    let mut sections = Vec::with_capacity(n);
    for _ in 0..n {
        let mut head = [0u8; SECTION_HEADER_BYTES];
        if *left < SECTION_HEADER_BYTES as u64 {
            return Err(corrupt(path, "file ends inside a section header"));
        }
        file.read_exact(&mut head).map_err(io_error(path))?;
        let count = u64::from_le_bytes(head[0..8].try_into().expect("8 bytes"));
        let len = u64::from_le_bytes(head[8..16].try_into().expect("8 bytes"));
        let crc = u32::from_le_bytes(head[16..20].try_into().expect("4 bytes"));
        *left -= SECTION_HEADER_BYTES as u64;
        // The length is checked against the file before it sizes an allocation.
        if len > *left {
            return Err(corrupt(path, "a section runs past the end of the file"));
        }
        let mut body = vec![0u8; len as usize];
        file.read_exact(&mut body).map_err(io_error(path))?;
        *left -= len;
        if crc32c::crc32c(&body) != crc {
            return Err(corrupt(path, "a section fails its checksum"));
        }
        sections.push(Section { count, body });
    }
    Ok(sections)
}

fn decode_coins(
    shard: usize,
    section: Section,
    hasher: &ahash::RandomState,
) -> Result<CoinShard, &'static str> {
    // The shortest coin record bounds a damaged count before it sizes an allocation.
    if section.count > (section.body.len() / MIN_RECORD_BYTES) as u64 {
        return Err("a coin section claims more coins than its bytes hold");
    }
    let mut coins = CoinShard::with_capacity(section.count as usize, hasher);
    let mut buf = &section.body[..];
    for _ in 0..section.count {
        let packed = decode_packed(&mut buf)?;
        if usize::from(packed.0.key()[0]) != shard {
            return Err("a coin is in the wrong shard");
        }
        coins.insert_new(hasher, packed)?;
    }
    if !buf.is_empty() {
        return Err("a coin section has trailing bytes");
    }
    Ok(coins)
}

fn decode_nullifiers(
    shard: usize,
    section: Section,
    hasher: &ahash::RandomState,
) -> Result<NullifierShard, &'static str> {
    if section.count.checked_mul(32) != Some(section.body.len() as u64) {
        return Err("a nullifier section count does not match its length");
    }
    let run: Vec<[u8; 32]> = section.body.as_chunks::<32>().0.to_vec();
    if run.iter().any(|nf| usize::from(nf[0]) != shard) {
        return Err("a nullifier is in the wrong shard");
    }
    NullifierShard::from_run(run, hasher)
}

/// Reads the header of the snapshot `file`: the sequence number and the best block.
fn read_header(
    file: &mut File,
    path: &Path,
    left: &mut u64,
) -> Result<(u64, Option<BestBlock>), PersistError> {
    let mut header = [0u8; HEADER_BYTES];
    if *left < HEADER_BYTES as u64 {
        return Err(corrupt(path, "file is shorter than the header"));
    }
    file.read_exact(&mut header).map_err(io_error(path))?;
    *left -= HEADER_BYTES as u64;
    let crc = u32::from_le_bytes(header[HEADER_BYTES - 4..].try_into().expect("4 bytes"));
    if header[..8] != MAGIC || crc != crc32c::crc32c(&header[..HEADER_BYTES - 4]) {
        return Err(corrupt(
            path,
            "header has a wrong magic or fails its checksum",
        ));
    }
    let field = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("4 bytes"));
    if field(8) != VERSION {
        return Err(corrupt(path, format!("unknown version {}", field(8))));
    }
    let seq = u64::from_le_bytes(header[12..20].try_into().expect("8 bytes"));
    let best_block = match header[20] {
        0 => None,
        FLAG_BEST_BLOCK => Some(BestBlock {
            height: field(21),
            hash: header[25..57].try_into().expect("32 bytes"),
        }),
        other => return Err(corrupt(path, format!("unknown flags {other}"))),
    };
    if field(57) as usize != SHARDS {
        return Err(corrupt(
            path,
            format!("{} shards, expected {SHARDS}", field(57)),
        ));
    }
    Ok((seq, best_block))
}

/// The sequence number and the best block of the snapshot at `path`, from its header only.
pub(super) fn header(path: &Path) -> Result<(u64, Option<BestBlock>), PersistError> {
    let mut file = File::open(path).map_err(io_error(path))?;
    let mut left = file.metadata().map_err(io_error(path))?.len();
    read_header(&mut file, path, &mut left)
}

/// Reads the snapshot at `path`. Every damage is an error: a snapshot is written whole
/// and renamed into place, so it is never torn.
pub(super) fn load(path: &Path, hasher: &ahash::RandomState) -> Result<Loaded, PersistError> {
    let mut file = File::open(path).map_err(io_error(path))?;
    let mut left = file.metadata().map_err(io_error(path))?.len();
    let (seq, best_block) = read_header(&mut file, path, &mut left)?;

    let mut coins = Vec::with_capacity(SHARDS);
    for first in (0..SHARDS).step_by(BATCH) {
        let sections = read_sections(&mut file, path, BATCH, &mut left)?;
        let decoded: Vec<CoinShard> = sections
            .into_par_iter()
            .enumerate()
            .map(|(i, section)| decode_coins(first + i, section, hasher))
            .collect::<Result<_, _>>()
            .map_err(|reason| corrupt(path, reason))?;
        coins.extend(decoded);
    }
    let mut nullifiers: [Vec<NullifierShard>; 4] = Default::default();
    for pool in &mut nullifiers {
        for first in (0..SHARDS).step_by(BATCH) {
            let sections = read_sections(&mut file, path, BATCH, &mut left)?;
            let decoded: Vec<NullifierShard> = sections
                .into_par_iter()
                .enumerate()
                .map(|(i, section)| decode_nullifiers(first + i, section, hasher))
                .collect::<Result<_, _>>()
                .map_err(|reason| corrupt(path, reason))?;
            pool.extend(decoded);
        }
    }
    if left != 0 {
        return Err(corrupt(path, "file has trailing bytes"));
    }
    Ok(Loaded {
        seq,
        best_block,
        coins,
        nullifiers,
    })
}
