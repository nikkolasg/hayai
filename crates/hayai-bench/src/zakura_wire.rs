//! Zakura baselines for `benches/wire.rs`: block parsing and the two block-header roots,
//! computed with `zakura-chain` (`zk_chain`).
//!
//! Zakura never retains wire bytes: a received block is deserialized into owned structures,
//! each transaction hash is a fresh serialization through a SHA-256d writer, and serving or
//! relaying the block serializes it again. [`parse_round_trip`] is that path.

use zk_chain::block::merkle::{AuthDataRoot, Root};
use zk_chain::block::Block;
use zk_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zk_chain::transaction::{AuthDigest, Hash};

/// Deserialize, hash every transaction (txid and auth digest, as a node needs for the
/// merkle and auth-data roots), and serialize again. Returns the serialized block so the
/// compiler cannot elide the work.
pub fn parse_round_trip(bytes: &[u8]) -> (Vec<Hash>, Vec<AuthDigest>, Vec<u8>) {
    let block = Block::zcash_deserialize(bytes).expect("zakura parses the fixture");
    let txids: Vec<Hash> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let digests: Vec<AuthDigest> = block
        .transactions
        .iter()
        .map(|tx| {
            tx.auth_digest()
                .unwrap_or(zk_chain::block::merkle::AUTH_DIGEST_PLACEHOLDER)
        })
        .collect();
    let out = block.zcash_serialize_to_vec().expect("vec write");
    (txids, digests, out)
}

pub fn merkle_root(txids: &[Hash]) -> [u8; 32] {
    txids.iter().copied().collect::<Root>().0
}

pub fn auth_data_root(digests: &[AuthDigest]) -> [u8; 32] {
    digests.iter().copied().collect::<AuthDataRoot>().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_fixtures as fixtures;

    /// hayai-wire must not depend on zakura crates, so the equality of its roots with
    /// Zakura's implementation is checked here.
    #[test]
    fn hayai_roots_equal_zakura_roots() {
        let fixture = fixtures::mixed_block(3, 1, 2, 2);
        let block = fixture.parse();
        let (zk_txids, zk_digests, reserialized) = parse_round_trip(&fixture.bytes);
        assert_eq!(reserialized, fixture.bytes);

        let hayai_txids = block.txids();
        for (h, z) in hayai_txids.iter().zip(&zk_txids) {
            assert_eq!(h.as_ref(), &z.0);
        }
        let hayai_digests = block.auth_digests();
        for (h, z) in hayai_digests.iter().zip(&zk_digests) {
            assert_eq!(h, &z.0);
        }
        assert_eq!(
            hayai_wire::merkle_root(&hayai_txids),
            merkle_root(&zk_txids)
        );
        assert_eq!(
            hayai_wire::auth_data_root(&hayai_digests),
            auth_data_root(&zk_digests)
        );
    }

    #[test]
    fn roots_agree_on_random_leaf_counts() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        for n in [1usize, 2, 3, 5, 64, 65, 100, 257] {
            let leaves: Vec<[u8; 32]> = (0..n).map(|_| rng.gen()).collect();
            let hayai_ids: Vec<_> = leaves
                .iter()
                .map(|l| hayai_crypto::zcash_primitives::transaction::TxId::from_bytes(*l))
                .collect();
            let zk_ids: Vec<Hash> = leaves.iter().map(|l| Hash(*l)).collect();
            assert_eq!(
                hayai_wire::merkle_root(&hayai_ids),
                merkle_root(&zk_ids),
                "{n}"
            );
            let zk_digests: Vec<AuthDigest> = leaves.iter().map(|l| AuthDigest(*l)).collect();
            assert_eq!(
                hayai_wire::auth_data_root(&leaves),
                auth_data_root(&zk_digests),
                "{n}"
            );
        }
    }
}
