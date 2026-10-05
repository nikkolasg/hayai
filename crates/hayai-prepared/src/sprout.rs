//! Sprout JoinSplits: the structural rules, the Groth16 verifying key, the proofs and the
//! JoinSplit signature.
//!
//! The Groth16 verifying key of the JoinSplit circuit is in the binary.
//! `sprout_vk/sprout-groth16.vk` is the first 1,828 bytes of the official parameter file
//! `sprout-groth16.params`: the encoding of bellman `groth16::Parameters` starts with the
//! verifying key. `scripts/extract-sprout-vk.sh` writes the file from a parameter file
//! whose size and BLAKE2b-512 hash it checks. Zebra and Zakura embed the same bytes
//! (`zakura-consensus/src/primitives/groth16/sprout-groth16.vk`).
//!
//! What hayai verifies, as Zakura (`zakura-consensus/src/transaction.rs`,
//! `verify_sprout_shielded_data`):
//!
//! - Each Groth16 JoinSplit proof (v4 transactions, from Sapling), with upstream
//!   `zcash_proofs::sprout::verify_proof`.
//! - The JoinSplit signature: Ed25519 on the shielded sighash of the transaction, with
//!   the ZIP 215 rules at every height (`ed25519-zebra`).
//!
//! hayai has no verifier for BCTV14 proofs (v2 and v3 transactions, before Sapling), as
//! Zebra and Zakura. A JoinSplit with a BCTV14 proof is an error here. Such a block is
//! valid only on the checkpoint path, which does not come here.

use hayai_coins::Pool;
use hayai_consensus::RuleSet;
use hayai_crypto::bellman::groth16::{prepare_verifying_key, PreparedVerifyingKey, VerifyingKey};
use hayai_crypto::bls12_381::Bls12;
use hayai_crypto::ed25519_zebra::{Signature, VerificationKey};
use hayai_crypto::zcash_primitives::transaction::components::sprout::{Bundle, JsDescription};
use hayai_crypto::zcash_proofs::sprout::verify_proof;
use hayai_crypto::zcash_protocol::value::ZatBalance;

use crate::shielded::Item;
use crate::PrepareError;

/// The verifying key of the JoinSplit circuit: the start of `sprout-groth16.params`.
const JOINSPLIT_VK: &[u8] = include_bytes!("sprout_vk/sprout-groth16.vk");

/// BLAKE2b-512 hash of the official `sprout-groth16.params` (725,523,612 bytes), the source
/// of the embedded key. It equals `SPROUT_HASH` of `zcash_proofs`.
pub const SPROUT_GROTH16_PARAMS_BLAKE2B: &str = "e9b238411bd6c0ec4791e9d04245ec350c9c5744f5610dfcce4365d5ca49dfefd5054e371842b3f88fa1b9d7e8e075249b3ebabd167fa8b0f3161292d36c180a";

/// BLAKE2b-512 hash of the embedded key file, as `scripts/extract-sprout-vk.sh` wrote it
/// from the parameter file with the hash [`SPROUT_GROTH16_PARAMS_BLAKE2B`].
pub const SPROUT_GROTH16_VK_BLAKE2B: &str = "847777e71ceea9d06ef47ac0c980c2b2a9e65e7617033908f621e067e8c0460242addb6c4bf1e2850c24fe3f9f6e63b9cade4fbf531f5321a1e93c447786c05d";

/// The Groth16 verifying key of the Sprout JoinSplit circuit.
pub struct SproutKey {
    joinsplit: PreparedVerifyingKey<Bls12>,
}

impl SproutKey {
    /// The key that the binary embeds. The point encodings are checked.
    pub fn embedded() -> Self {
        let mut bytes = JOINSPLIT_VK;
        let key = VerifyingKey::<Bls12>::read(&mut bytes)
            .expect("the embedded Sprout verifying key is valid");
        assert!(
            bytes.is_empty(),
            "the embedded Sprout verifying key has bytes after its IC points"
        );
        Self {
            joinsplit: prepare_verifying_key(&key),
        }
    }
}

/// `vpub_old` or `vpub_new` in zatoshis. The parser reads each as an amount in
/// `0..=MAX_MONEY`.
fn zatoshis(value: ZatBalance) -> u64 {
    u64::try_from(value).expect("the parser reads vpub as a non-negative amount")
}

/// The structural rules of the JoinSplits of a transaction under `rules`:
///
/// - `vpub_old` or `vpub_new` is zero (Zakura `joinsplit_has_vpub_zero`,
///   `zakura-consensus/src/transaction/check.rs:279`);
/// - from Canopy `vpub_old` is zero (ZIP 211, Zakura `disabled_add_to_sprout_pool`,
///   `check.rs:304`);
/// - no nullifier is revealed twice in the transaction.
///
/// Returns the nullifiers and the note commitments, in transaction order.
#[allow(clippy::type_complexity)]
pub(crate) fn check_joinsplits(
    bundle: &Bundle,
    rules: &RuleSet,
) -> Result<(Vec<[u8; 32]>, Vec<[u8; 32]>), PrepareError> {
    let mut nullifiers: Vec<[u8; 32]> = Vec::with_capacity(2 * bundle.joinsplits.len());
    let mut commitments = Vec::with_capacity(2 * bundle.joinsplits.len());
    for (i, joinsplit) in bundle.joinsplits.iter().enumerate() {
        let vpub_old = zatoshis(joinsplit.vpub_old());
        let vpub_new = zatoshis(joinsplit.vpub_new());
        if vpub_old != 0 && vpub_new != 0 {
            return Err(PrepareError::JoinSplitBothVpub(i));
        }
        if vpub_old != 0 && !rules.sprout_deposit {
            return Err(PrepareError::SproutPoolDeposit(i));
        }
        for nullifier in joinsplit.nullifiers() {
            if nullifiers.contains(nullifier) {
                return Err(PrepareError::DuplicateNullifier(Pool::Sprout));
            }
            nullifiers.push(*nullifier);
        }
        commitments.extend_from_slice(joinsplit.commitments());
    }
    Ok((nullifiers, commitments))
}

/// `hSig` of a JoinSplit (protocol specification §5.4.1.4, hSigCRH): BLAKE2b-256 with the
/// personalization `ZcashComputehSig` of the random seed, the two nullifiers and the
/// JoinSplit public key. `zcash_proofs` has no public function for it; Zakura has the same
/// function (`zakura-consensus/src/primitives/groth16.rs`, `h_sig`).
fn h_sig(random_seed: &[u8; 32], nullifiers: &[[u8; 32]; 2], public_key: &[u8; 32]) -> [u8; 32] {
    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashComputehSig")
        .to_state()
        .update(random_seed)
        .update(&nullifiers[0])
        .update(&nullifiers[1])
        .update(public_key)
        .finalize();
    hash.as_bytes().try_into().expect("a hash of 32 bytes")
}

/// Whether the Groth16 proof of `joinsplit` is valid under `key`, in a transaction with
/// the JoinSplit public key `public_key`. A BCTV14 proof is never valid here.
fn joinsplit_proof_is_valid(
    key: &SproutKey,
    joinsplit: &JsDescription,
    public_key: &[u8; 32],
) -> bool {
    let Some(proof) = joinsplit.groth_proof_bytes() else {
        return false;
    };
    let nullifiers = joinsplit.nullifiers();
    let commitments = joinsplit.commitments();
    let macs = joinsplit.macs();
    verify_proof(
        proof,
        joinsplit.anchor(),
        &h_sig(joinsplit.random_seed(), nullifiers, public_key),
        &macs[0],
        &macs[1],
        &nullifiers[0],
        &nullifiers[1],
        &commitments[0],
        &commitments[1],
        zatoshis(joinsplit.vpub_old()),
        zatoshis(joinsplit.vpub_new()),
        &key.joinsplit,
    )
}

/// Whether the JoinSplit signature and every JoinSplit proof of the transaction of `item`
/// are valid. The signature is on the shielded sighash of the transaction. The public key
/// must be the encoding of a point of the curve, and the signature must be valid by the
/// ZIP 215 rules.
pub(crate) fn verify_sprout(key: &SproutKey, item: &Item) -> bool {
    let Some(bundle) = item.tx.sprout_bundle() else {
        unreachable!("queued under the Sprout group");
    };
    let Ok(public_key) = VerificationKey::try_from(bundle.joinsplit_pubkey) else {
        return false;
    };
    let signature = Signature::from_bytes(&bundle.joinsplit_sig);
    let Ok(()) = public_key.verify(&signature, &item.sighash) else {
        return false;
    };
    bundle
        .joinsplits
        .iter()
        .all(|joinsplit| joinsplit_proof_is_valid(key, joinsplit, &bundle.joinsplit_pubkey))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use hayai_crypto::zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
    use hayai_crypto::zcash_protocol::consensus::{BlockHeight, BranchId};
    use hayai_crypto::zcash_transparent::bundle::{
        Authorized as TAuthorized, Bundle as TBundle, TxIn, TxOut,
    };

    /// Size of a Groth16 proof and of a BCTV14 proof.
    const GROTH_PROOF_BYTES: usize = 192;
    const BCTV14_PROOF_BYTES: usize = 296;

    /// The wire bytes of a transaction of `version` and `branch` with the transparent
    /// inputs `vin`, the transparent outputs `vout` and one JoinSplit for each entry
    /// `(tag, vpub_old, vpub_new)` of `joinsplits` ([`joinsplit_bytes`]). A version before
    /// v4 has BCTV14 proofs. The proofs and the JoinSplit signature are not valid.
    pub(crate) fn joinsplit_tx(
        version: TxVersion,
        branch: BranchId,
        vin: Vec<TxIn<TAuthorized>>,
        vout: Vec<TxOut>,
        joinsplits: &[(u8, u64, u64)],
    ) -> Vec<u8> {
        let groth = version.has_sapling();
        let proof_bytes = match groth {
            true => GROTH_PROOF_BYTES,
            false => BCTV14_PROOF_BYTES,
        };
        let transparent = if vin.is_empty() && vout.is_empty() {
            None
        } else {
            Some(TBundle {
                vin,
                vout,
                authorization: TAuthorized,
            })
        };
        let sprout = match joinsplits {
            [] => None,
            _ => Some(Bundle {
                joinsplits: joinsplits
                    .iter()
                    .map(|(tag, vpub_old, vpub_new)| {
                        let bytes = joinsplit_bytes(*tag, *vpub_old, *vpub_new, proof_bytes);
                        JsDescription::read(&bytes[..], groth).expect("the JoinSplit parses")
                    })
                    .collect(),
                joinsplit_pubkey: [3; 32],
                joinsplit_sig: [4; 64],
            }),
        };
        let data = TransactionData::<Authorized>::from_parts(
            version,
            branch,
            0,
            BlockHeight::from_u32(0),
            transparent,
            sprout,
            None,
            None,
        );
        let mut bytes = Vec::new();
        data.freeze()
            .expect("the transaction freezes")
            .write(&mut bytes)
            .expect("vec write");
        bytes
    }

    /// The encoding of a JoinSplit description with the given public values, the anchor
    /// `[tag; 32]`, nullifiers and commitments that depend on `tag`, and a proof of
    /// `proof_bytes` zero bytes. The proof is not valid.
    fn joinsplit_bytes(tag: u8, vpub_old: u64, vpub_new: u64, proof_bytes: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&vpub_old.to_le_bytes());
        bytes.extend_from_slice(&vpub_new.to_le_bytes());
        bytes.extend_from_slice(&[tag; 32]);
        for part in 1..=4u8 {
            // Two nullifiers, then two commitments.
            let mut value = [tag; 32];
            value[0] = part;
            bytes.extend_from_slice(&value);
        }
        // Ephemeral key, random seed, two MACs.
        bytes.extend_from_slice(&[0u8; 4 * 32]);
        bytes.extend_from_slice(&vec![0u8; proof_bytes]);
        bytes.extend_from_slice(&[0u8; 2 * 601]);
        bytes
    }

    fn hex32_reversed(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().rev().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex digits");
        }
        out
    }

    /// The vectors of zcashd `src/gtest/test_joinsplit.cpp` (byte-reversed there), as
    /// `zebra-consensus` 11.0.0 and Zakura publish them
    /// (`src/primitives/groth16/vectors.rs`).
    #[test]
    fn h_sig_gives_the_published_values() {
        let vectors: [[&str; 5]; 4] = [
            [
                "6161616161616161616161616161616161616161616161616161616161616161",
                "6262626262626262626262626262626262626262626262626262626262626262",
                "6363636363636363636363636363636363636363636363636363636363636363",
                "6464646464646464646464646464646464646464646464646464646464646464",
                "a8cba69f1fa329c055756b4af900f8a00b61e44f4cb8a1824ceb58b90a5b8113",
            ],
            [
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000",
                "697322276b5dd93b12fb1fcbd2144b2960f24c73aac6c6a0811447be1e7f1e19",
            ],
            [
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "4961048919f0ca79d49c9378c36a91a8767060001f4212fe6f7d426f3ccf9f32",
            ],
            [
                "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
                "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
                "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
                "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100",
                "b61110ec162693bc3d9ca7fb0eec3afd2e278e2f41394b3ff11d7cb761ad4b27",
            ],
        ];
        for [seed, nf1, nf2, key, expected] in vectors {
            let result = h_sig(
                &hex32_reversed(seed),
                &[hex32_reversed(nf1), hex32_reversed(nf2)],
                &hex32_reversed(key),
            );
            assert_eq!(result, hex32_reversed(expected));
        }
    }

    /// The embedded file is the file that `scripts/extract-sprout-vk.sh` wrote from the
    /// official parameters, and it is one whole verifying key with the 9 public inputs of
    /// the JoinSplit statement. `hayai-bench/tests/sprout.rs` verifies published JoinSplit
    /// proofs of the chain with the key.
    #[test]
    fn the_embedded_key_is_the_extracted_key() {
        assert_eq!(
            blake2b_simd::blake2b(JOINSPLIT_VK).to_hex().as_str(),
            SPROUT_GROTH16_VK_BLAKE2B
        );
        // 3 G1 points and 3 G2 points, the count of the IC points, and the IC points.
        let fixed = 3 * 96 + 3 * 192;
        assert_eq!(JOINSPLIT_VK[fixed..fixed + 4], 10u32.to_be_bytes());
        assert_eq!(JOINSPLIT_VK.len(), fixed + 4 + 96 * 10);
        SproutKey::embedded();
    }

    /// A proof of zero bytes, and a BCTV14 proof, are not valid.
    #[test]
    fn a_wrong_proof_is_not_valid() {
        let key = SproutKey::embedded();
        for (proof_bytes, groth) in [(GROTH_PROOF_BYTES, true), (BCTV14_PROOF_BYTES, false)] {
            let bytes = joinsplit_bytes(7, 0, 5, proof_bytes);
            let joinsplit = JsDescription::read(&bytes[..], groth).expect("the JoinSplit parses");
            assert!(!joinsplit_proof_is_valid(&key, &joinsplit, &[9; 32]));
        }
    }
}
