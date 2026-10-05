//! Short transaction ids (BIP 152 construction over 64-byte WtxIds).
//!
//! Key: `k0, k1 = SHA-256(header || nonce_le)[0..16]` as two little-endian `u64`.
//! Id: the low 6 bytes (little-endian) of `SipHash-2-4(k0, k1, wtxid)`.

use std::hash::Hasher;

use hayai_wire::{TxLookup, WtxId};
use sha2::{Digest, Sha256};
use siphasher::sip::SipHasher24;

/// A 6-byte short transaction id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ShortId(pub [u8; 6]);

/// Per-block SipHash key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ShortIdKey {
    pub k0: u64,
    pub k1: u64,
}

impl ShortIdKey {
    /// The key of the block whose serialized header is `header`.
    pub fn from_header(header: &[u8], nonce: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(header);
        hasher.update(nonce.to_le_bytes());
        let digest = hasher.finalize();
        let k0 = u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"));
        let k1 = u64::from_le_bytes(digest[8..16].try_into().expect("8 bytes"));
        ShortIdKey { k0, k1 }
    }
}

pub fn short_id(key: &ShortIdKey, id: &WtxId) -> ShortId {
    let mut hasher = SipHasher24::new_with_keys(key.k0, key.k1);
    hasher.write(&id.to_bytes());
    let full = hasher.finish().to_le_bytes();
    ShortId(full[..6].try_into().expect("6 bytes"))
}

/// Short ids of every transaction in a store under one block's key.
///
/// A short id shared by two stored transactions is ambiguous: it resolves to nothing, and
/// the reconstruction requests the transaction at that index instead of guessing.
pub struct ShortIdIndex {
    map: ahash::AHashMap<ShortId, Option<WtxId>>,
}

impl ShortIdIndex {
    pub fn build(key: &ShortIdKey, store: &dyn TxLookup) -> Self {
        let mut index = ShortIdIndex {
            map: ahash::AHashMap::with_capacity(store.len()),
        };
        store.for_each_id(&mut |id| index.insert(short_id(key, id), *id));
        index
    }

    fn insert(&mut self, short: ShortId, id: WtxId) {
        self.map
            .entry(short)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(id));
    }

    /// Resolves a short id to the unique stored transaction it identifies, if any.
    pub fn resolve(&self, id: &ShortId) -> Option<WtxId> {
        self.map.get(id).copied().flatten()
    }

    /// Number of distinct short ids, ambiguous ones included.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::MemStore;
    use hayai_crypto::zcash_primitives::transaction::TxId;

    fn id(n: u8) -> WtxId {
        WtxId {
            txid: TxId::from_bytes([n; 32]),
            auth_digest: [n.wrapping_add(1); 32],
        }
    }

    #[test]
    fn key_is_sha256_of_header_and_nonce() {
        let header = [0x11u8; 1487];
        let key = ShortIdKey::from_header(&header, 0x0102_0304_0506_0708);
        let mut preimage = header.to_vec();
        preimage.extend_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes());
        let digest = Sha256::digest(&preimage);
        assert_eq!(key.k0, u64::from_le_bytes(digest[..8].try_into().unwrap()));
        assert_eq!(
            key.k1,
            u64::from_le_bytes(digest[8..16].try_into().unwrap())
        );
        assert_ne!(key, ShortIdKey::from_header(&header, 1));
    }

    #[test]
    fn short_id_is_low_six_bytes_of_siphash24() {
        let key = ShortIdKey { k0: 7, k1: 9 };
        let wtxid = id(3);
        let mut hasher = SipHasher24::new_with_keys(7, 9);
        hasher.write(&wtxid.to_bytes());
        let expected = hasher.finish().to_le_bytes();
        assert_eq!(short_id(&key, &wtxid).0, expected[..6]);
        assert_ne!(short_id(&key, &wtxid), short_id(&key, &id(4)));
    }

    #[test]
    fn index_marks_collisions_ambiguous() {
        let key = ShortIdKey { k0: 1, k1: 2 };
        let mut store = MemStore::default();
        store.insert_id(id(1));
        store.insert_id(id(2));
        let index = ShortIdIndex::build(&key, &store);
        assert_eq!(index.len(), 2);
        assert_eq!(index.resolve(&short_id(&key, &id(1))), Some(id(1)));
        assert_eq!(index.resolve(&short_id(&key, &id(9))), None);

        // A 48-bit collision cannot be found by search; insert a second id under the first
        // id's short id directly to exercise the ambiguity path.
        let mut colliding = ShortIdIndex {
            map: Default::default(),
        };
        colliding.insert(short_id(&key, &id(1)), id(1));
        colliding.insert(short_id(&key, &id(1)), id(2));
        colliding.insert(short_id(&key, &id(3)), id(3));
        assert_eq!(colliding.resolve(&short_id(&key, &id(1))), None);
        assert_eq!(colliding.resolve(&short_id(&key, &id(3))), Some(id(3)));
        assert_eq!(colliding.len(), 2);
    }
}
