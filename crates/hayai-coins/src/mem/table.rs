//! The coin shards of [`super::MemBacking`] and the record form of one coin.
//!
//! A coin is one 69-byte [`Entry`]: the outpoint key, the value, the height, a tag and a
//! 20-byte script hash. P2PKH and P2SH scripts are rebuilt from the tag and the hash (the
//! script compression of Bitcoin Core's `ScriptCompression`); every other script is kept
//! whole in a side map of the shard. A shard keeps its entries in a dense vector and a
//! hash index of their positions, so that the table costs 69 bytes per coin plus 5 bytes
//! per index bucket instead of 70 bytes per bucket (`CHANGES.md` has the measurements).

use std::collections::HashMap;

use bytes::Bytes;
use hashbrown::HashTable;

use crate::{Coin, OUTPOINT_KEY_BYTES};

pub(super) type Key = [u8; OUTPOINT_KEY_BYTES];

const KIND_P2PKH: u8 = 0;
const KIND_P2SH: u8 = 1;
const KIND_RAW: u8 = 2;
const KIND_MASK: u8 = 0b0000_0011;
const COINBASE_BIT: u8 = 0b1000_0000;

const P2PKH_PREFIX: [u8; 3] = [0x76, 0xa9, 0x14];
const P2PKH_SUFFIX: [u8; 2] = [0x88, 0xac];
const P2SH_PREFIX: [u8; 2] = [0xa9, 0x14];
const P2SH_SUFFIX: u8 = 0x87;

/// Encoded size of an entry before its script payload: key, value, height, tag.
const ENTRY_HEAD_BYTES: usize = OUTPOINT_KEY_BYTES + 8 + 4 + 1;
/// The shortest record: a raw entry with an empty script (head and a 4-byte length).
pub(super) const MIN_RECORD_BYTES: usize = ENTRY_HEAD_BYTES + 4;

/// One coin in compact form. Every field is a byte array, so the struct has alignment 1
/// and no padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub(super) struct Entry {
    key: Key,
    value: [u8; 8],
    height: [u8; 4],
    /// Script kind in the low two bits, coinbase flag in the high bit.
    tag: u8,
    /// The hash of a P2PKH or P2SH script; zero for a raw script.
    hash: [u8; 20],
}

/// A coin as the shards take it: the entry, and the script when it is not compressible.
pub(super) type Packed = (Entry, Option<Box<[u8]>>);

impl Entry {
    /// Compresses `coin` under `key`.
    pub(super) fn pack(key: Key, coin: &Coin) -> Packed {
        let script = &coin.script_pubkey[..];
        let mut hash = [0u8; 20];
        let kind = if let ([0x76, 0xa9, 0x14], body, [0x88, 0xac]) = split3(script, 3, 2) {
            hash.copy_from_slice(body);
            KIND_P2PKH
        } else if let ([0xa9, 0x14], body, [0x87]) = split3(script, 2, 1) {
            hash.copy_from_slice(body);
            KIND_P2SH
        } else {
            KIND_RAW
        };
        let tag = kind | if coin.is_coinbase { COINBASE_BIT } else { 0 };
        let entry = Entry {
            key,
            value: coin.value.to_le_bytes(),
            height: coin.height.to_le_bytes(),
            tag,
            hash,
        };
        let raw = match kind {
            KIND_RAW => Some(Box::from(script)),
            _ => None,
        };
        (entry, raw)
    }

    pub(super) fn key(&self) -> &Key {
        &self.key
    }

    fn kind(&self) -> u8 {
        self.tag & KIND_MASK
    }

    /// Rebuilds the coin; `raw` is the side-map script of a raw entry.
    fn coin(&self, raw: Option<&[u8]>) -> Coin {
        let script = match (self.kind(), raw) {
            (KIND_P2PKH, _) => {
                let mut s = Vec::with_capacity(25);
                s.extend_from_slice(&P2PKH_PREFIX);
                s.extend_from_slice(&self.hash);
                s.extend_from_slice(&P2PKH_SUFFIX);
                Bytes::from(s)
            }
            (KIND_P2SH, _) => {
                let mut s = Vec::with_capacity(23);
                s.extend_from_slice(&P2SH_PREFIX);
                s.extend_from_slice(&self.hash);
                s.push(P2SH_SUFFIX);
                Bytes::from(s)
            }
            (_, Some(raw)) => Bytes::copy_from_slice(raw),
            (_, None) => unreachable!("a raw entry always has its script in the side map"),
        };
        Coin {
            value: u64::from_le_bytes(self.value),
            script_pubkey: script,
            height: u32::from_le_bytes(self.height),
            is_coinbase: self.tag & COINBASE_BIT != 0,
        }
    }
}

/// Rebuilds the coin of a compact entry.
pub(super) fn unpack((entry, raw): &Packed) -> Coin {
    entry.coin(raw.as_deref())
}

/// `(script[..head], script[head..len - tail], script[len - tail..])` when `script` is
/// `head + 20 + tail` bytes long, else three empty slices (which match no pattern above).
fn split3(script: &[u8], head: usize, tail: usize) -> (&[u8], &[u8], &[u8]) {
    if script.len() != head + 20 + tail {
        return (&[], &[], &[]);
    }
    let (h, rest) = script.split_at(head);
    let (body, t) = rest.split_at(20);
    (h, body, t)
}

/// Appends the record form of a packed coin: key, value LE, height LE, tag, then the 20-byte
/// hash, or for a raw script its length (u32 LE) and bytes.
pub(super) fn encode_packed(entry: &Entry, raw: Option<&[u8]>, out: &mut Vec<u8>) {
    out.extend_from_slice(&entry.key);
    out.extend_from_slice(&entry.value);
    out.extend_from_slice(&entry.height);
    out.push(entry.tag);
    match raw {
        None => out.extend_from_slice(&entry.hash),
        Some(raw) => {
            let len = u32::try_from(raw.len()).expect("a script is shorter than 4 GiB");
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(raw);
        }
    }
}

/// Reads one [`encode_packed`] record from the front of `buf` and advances it.
pub(super) fn decode_packed(buf: &mut &[u8]) -> Result<Packed, &'static str> {
    let Some((head, rest)) = buf.split_at_checked(ENTRY_HEAD_BYTES) else {
        return Err("coin record is truncated");
    };
    let mut entry = Entry {
        key: head[..36].try_into().expect("36 bytes"),
        value: head[36..44].try_into().expect("8 bytes"),
        height: head[44..48].try_into().expect("4 bytes"),
        tag: head[48],
        hash: [0; 20],
    };
    if entry.tag & !(KIND_MASK | COINBASE_BIT) != 0 {
        return Err("coin record has an unknown tag");
    }
    match entry.kind() {
        KIND_P2PKH | KIND_P2SH => {
            let Some((hash, rest)) = rest.split_first_chunk::<20>() else {
                return Err("coin record is truncated");
            };
            entry.hash = *hash;
            *buf = rest;
            Ok((entry, None))
        }
        KIND_RAW => {
            let Some((len, rest)) = rest.split_first_chunk::<4>() else {
                return Err("coin record is truncated");
            };
            let len = u32::from_le_bytes(*len) as usize;
            let Some((script, rest)) = rest.split_at_checked(len) else {
                return Err("coin record is truncated");
            };
            *buf = rest;
            Ok((entry, Some(Box::from(script))))
        }
        _ => Err("coin record has an unknown script kind"),
    }
}

/// One shard of the coin set: entries in a dense vector, a hash index of their positions,
/// and the raw scripts by key.
pub(super) struct CoinShard {
    entries: Vec<Entry>,
    index: HashTable<u32>,
    raw: HashMap<Key, Box<[u8]>, ahash::RandomState>,
}

impl CoinShard {
    pub(super) fn with_capacity(capacity: usize, hasher: &ahash::RandomState) -> Self {
        CoinShard {
            entries: Vec::with_capacity(capacity),
            index: HashTable::with_capacity(capacity),
            raw: HashMap::with_hasher(hasher.clone()),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    fn position(&self, hasher: &ahash::RandomState, key: &Key) -> Option<usize> {
        let entries = &self.entries;
        self.index
            .find(hasher.hash_one(key), |&i| entries[i as usize].key == *key)
            .map(|&i| i as usize)
    }

    /// A copy of the coin under `key` in compact form; [`unpack`] rebuilds the coin.
    pub(super) fn get(&self, hasher: &ahash::RandomState, key: &Key) -> Option<Packed> {
        let entry = self.entries[self.position(hasher, key)?];
        let raw = match entry.kind() {
            KIND_RAW => self.raw.get(key).cloned(),
            _ => None,
        };
        Some((entry, raw))
    }

    /// Inserts a coin, or replaces the coin under the same key.
    pub(super) fn insert(&mut self, hasher: &ahash::RandomState, (entry, raw): Packed) {
        let key = entry.key;
        match self.position(hasher, &key) {
            Some(position) => self.entries[position] = entry,
            None => {
                let position = u32::try_from(self.entries.len()).expect("a shard holds < 2^32");
                self.entries.push(entry);
                let entries = &self.entries;
                self.index
                    .insert_unique(hasher.hash_one(key), position, |&i| {
                        hasher.hash_one(entries[i as usize].key)
                    });
            }
        }
        match raw {
            Some(raw) => {
                self.raw.insert(key, raw);
            }
            None => {
                self.raw.remove(&key);
            }
        }
    }

    /// Inserts a coin whose key the shard must not hold yet (a snapshot section).
    pub(super) fn insert_new(
        &mut self,
        hasher: &ahash::RandomState,
        packed: Packed,
    ) -> Result<(), &'static str> {
        let None = self.position(hasher, packed.0.key()) else {
            return Err("a coin key appears twice");
        };
        self.insert(hasher, packed);
        Ok(())
    }

    /// Removes a coin; a key the shard does not hold is a no-op, as a RocksDB delete is.
    pub(super) fn remove(&mut self, hasher: &ahash::RandomState, key: &Key) {
        let entries = &self.entries;
        let Ok(found) = self
            .index
            .find_entry(hasher.hash_one(key), |&i| entries[i as usize].key == *key)
        else {
            return;
        };
        let (position, _) = found.remove();
        let position = position as usize;
        let last = self.entries.len() - 1;
        if self.entries[position].kind() == KIND_RAW {
            self.raw.remove(key);
        }
        self.entries.swap_remove(position);
        if position == last {
            return;
        }
        // The last entry moved into the hole: point its index slot at the new position.
        let moved = self.entries[position].key;
        let Ok(mut slot) = self
            .index
            .find_entry(hasher.hash_one(moved), |&i| i as usize == last)
        else {
            unreachable!("every entry has an index slot");
        };
        *slot.get_mut() = position as u32;
    }

    /// Appends every coin of the shard in record form.
    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        for entry in &self.entries {
            let raw = match entry.kind() {
                KIND_RAW => Some(&self.raw[&entry.key][..]),
                _ => None,
            };
            encode_packed(entry, raw, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coin(script: &[u8], is_coinbase: bool) -> Coin {
        Coin {
            value: 0x0102_0304_0506_0708,
            script_pubkey: Bytes::copy_from_slice(script),
            height: u32::MAX,
            is_coinbase,
        }
    }

    #[test]
    fn every_script_kind_round_trips() {
        let mut p2pkh = P2PKH_PREFIX.to_vec();
        p2pkh.extend_from_slice(&[7; 20]);
        p2pkh.extend_from_slice(&P2PKH_SUFFIX);
        let mut p2sh = P2SH_PREFIX.to_vec();
        p2sh.extend_from_slice(&[9; 20]);
        p2sh.push(P2SH_SUFFIX);
        // A P2PKH lookalike of the wrong length, an empty script and a P2PK script are raw.
        let near = [&p2pkh[..], &[0]].concat();
        let p2pk = [&[33u8][..], &[2; 33], &[0xac]].concat();
        for (script, kind) in [
            (&p2pkh[..], KIND_P2PKH),
            (&p2sh[..], KIND_P2SH),
            (&near[..], KIND_RAW),
            (&[][..], KIND_RAW),
            (&p2pk[..], KIND_RAW),
        ] {
            for is_coinbase in [false, true] {
                let coin = coin(script, is_coinbase);
                let (entry, raw) = Entry::pack([3; 36], &coin);
                assert_eq!(entry.kind(), kind);
                assert_eq!(entry.coin(raw.as_deref()), coin);
                let mut out = Vec::new();
                encode_packed(&entry, raw.as_deref(), &mut out);
                let mut buf = &out[..];
                assert_eq!(decode_packed(&mut buf), Ok((entry, raw)));
                assert!(buf.is_empty());
                assert_eq!(
                    decode_packed(&mut &out[..out.len() - 1]),
                    Err("coin record is truncated")
                );
            }
        }
    }

    #[test]
    fn remove_keeps_the_index_consistent() {
        let hasher = ahash::RandomState::new();
        let mut shard = CoinShard::with_capacity(0, &hasher);
        let key = |i: u8| [i; 36];
        for i in 0..50 {
            shard.insert(&hasher, Entry::pack(key(i), &coin(&[i, 1, 2], i % 2 == 0)));
        }
        for i in (0..50).step_by(3) {
            shard.remove(&hasher, &key(i));
        }
        shard.remove(&hasher, &key(200));
        for i in 0..50 {
            let expected = match i % 3 {
                0 => None,
                _ => Some(coin(&[i, 1, 2], i % 2 == 0)),
            };
            assert_eq!(shard.get(&hasher, &key(i)).as_ref().map(unpack), expected);
        }
        assert_eq!(shard.len(), 33);
        assert_eq!(shard.raw.len(), 33);
    }
}
