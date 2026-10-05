//! Zebra baseline for `benches/wire.rs`: block parsing with upstream `zebra-chain` 13.0.1
//! (`zb_chain`), the real code.
//!
//! Since zebra-chain 13.0.0 (Zebra PR #10461), `transaction::Transaction` wraps
//! `zcash_primitives::transaction::Transaction` (`zebra-chain-13.0.1/src/transaction.rs:53`).
//! A parse is a `zcash_primitives` read per transaction, which computes the txid digests.
//! `Transaction::hash` returns that txid (`transaction.rs:258-261`), and
//! `Transaction::auth_digest` computes the ZIP 244 auth commitment (`transaction.rs:266-276`).
//! Zakura keeps its own transaction structs and computes the txid from them, so the two
//! parse paths are different code. Zebra does not keep the wire bytes either: serving or
//! relaying the block serializes it again. [`parse_round_trip`] does the same work as
//! `crate::zakura_wire::parse_round_trip`, so the two rows compare.

use zb_chain::block::merkle::AUTH_DIGEST_PLACEHOLDER;
use zb_chain::block::Block;
use zb_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zb_chain::transaction::{AuthDigest, Hash};

/// Deserialize, get the txid and the auth digest of every transaction, and serialize again.
/// Returns the serialized block so the compiler cannot remove the work.
pub fn parse_round_trip(bytes: &[u8]) -> (Vec<Hash>, Vec<AuthDigest>, Vec<u8>) {
    let block = Block::zcash_deserialize(bytes).expect("zebra parses the fixture");
    let txids: Vec<Hash> = block.transactions.iter().map(|tx| tx.hash()).collect();
    let digests: Vec<AuthDigest> = block
        .transactions
        .iter()
        .map(|tx| tx.auth_digest().unwrap_or(AUTH_DIGEST_PLACEHOLDER))
        .collect();
    let out = block.zcash_serialize_to_vec().expect("vec write");
    (txids, digests, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    /// Zebra's txids and auth digests equal hayai's, and the serialization is the input.
    #[test]
    fn zebra_ids_equal_hayai_ids() {
        let fixture = fixtures::mixed_block(3, 1, 2, 2);
        let block = fixture.parse();
        let (txids, digests, reserialized) = parse_round_trip(&fixture.bytes);
        assert_eq!(reserialized, fixture.bytes);
        let hayai_txids = block.txids();
        assert_eq!(txids.len(), hayai_txids.len());
        for (h, z) in hayai_txids.iter().zip(&txids) {
            assert_eq!(h.as_ref(), &z.0);
        }
        let hayai_digests = block.auth_digests();
        assert_eq!(digests.len(), hayai_digests.len());
        for (h, z) in hayai_digests.iter().zip(&digests) {
            assert_eq!(h, &z.0);
        }
    }
}
