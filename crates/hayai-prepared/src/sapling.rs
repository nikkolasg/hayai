//! Sapling verifying keys and the batch verification of Sapling bundles.
//!
//! The two Groth16 verifying keys (Spend and Output) are in the binary. hayai reads no
//! parameter file. `sapling_vk/sapling-spend.vk` and `sapling_vk/sapling-output.vk` are the
//! first 1,636 and 1,444 bytes of the official parameter files `sapling-spend.params` and
//! `sapling-output.params`: the encoding of bellman `groth16::Parameters` starts with the
//! verifying key. `scripts/extract-sapling-vk.sh` writes the two files from parameter files
//! whose size and BLAKE2b-512 hash it checks. A test compares the files with the parameters
//! of the `wagyu-zcash-parameters` crate, which are the official files.
//!
//! Rules that the upstream code applies:
//!
//! - `cv` of a Spend or an Output is not of small order: the transaction parser
//!   (`ValueCommitment::from_bytes_not_small_order`).
//! - `rk` and `epk` are not of small order, and the proofs, the spend authorization
//!   signatures and the binding signature are valid: `sapling_crypto::BatchValidator`.
//! - The batch validator applies the canonical point encodings of ZIP 216 at every height.
//!   ZIP 216 activates with Canopy. Zebra and Zakura use the same validator at every
//!   height, because no earlier block has a non-canonical encoding.

use hayai_crypto::rng::os_rng;
use hayai_crypto::sapling_crypto;
use sapling_crypto::circuit::{
    OutputParameters, OutputVerifyingKey, SpendParameters, SpendVerifyingKey,
};

use crate::shielded::Item;

/// The verifying key of the Sapling Spend circuit: the start of `sapling-spend.params`.
const SPEND_VK: &[u8] = include_bytes!("sapling_vk/sapling-spend.vk");
/// The verifying key of the Sapling Output circuit: the start of `sapling-output.params`.
const OUTPUT_VK: &[u8] = include_bytes!("sapling_vk/sapling-output.vk");

/// BLAKE2b-512 hash of the official `sapling-spend.params` (47,958,396 bytes), the source of
/// the embedded Spend key. It equals `SAPLING_SPEND_HASH` of `zcash_proofs`.
pub const SAPLING_SPEND_PARAMS_BLAKE2B: &str = "8270785a1a0d0bc77196f000ee6d221c9c9894f55307bd9357c3f0105d31ca63991ab91324160d8f53e2bbd3c2633a6eb8bdf5205d822e7f3f73edac51b2b70c";
/// BLAKE2b-512 hash of the official `sapling-output.params` (3,592,860 bytes), the source of
/// the embedded Output key. It equals `SAPLING_OUTPUT_HASH` of `zcash_proofs`.
pub const SAPLING_OUTPUT_PARAMS_BLAKE2B: &str = "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028";

/// Sapling Groth16 verifying keys.
pub struct SaplingKeys {
    spend: SpendVerifyingKey,
    output: OutputVerifyingKey,
}

/// The encoding of `groth16::Parameters` that holds the verifying key `vk` and no prover
/// data: the key, then the lengths (zero) of the five prover vectors.
///
/// `sapling-crypto` 0.7.0 has no public constructor of `SpendVerifyingKey` or
/// `OutputVerifyingKey` from a `groth16::VerifyingKey` (`circuit.rs`: the field is
/// `pub(crate)`; the same holds for `zakura-sapling-crypto` 2.2.0). The public path to a key
/// is `SpendParameters::read(..).verifying_key()`.
fn parameters_without_prover_data(vk: &[u8]) -> Vec<u8> {
    const PROVER_VECTORS: usize = 5;
    let mut bytes = Vec::with_capacity(vk.len() + 4 * PROVER_VECTORS);
    bytes.extend_from_slice(vk);
    bytes.extend_from_slice(&[0u8; 4 * PROVER_VECTORS]);
    bytes
}

impl SaplingKeys {
    /// The keys that the binary embeds. The point encodings are checked.
    pub fn embedded() -> Self {
        let spend = parameters_without_prover_data(SPEND_VK);
        let output = parameters_without_prover_data(OUTPUT_VK);
        let (mut spend, mut output) = (&spend[..], &output[..]);
        let spend_params = SpendParameters::read(&mut spend, true)
            .expect("the embedded Sapling Spend verifying key is valid");
        let output_params = OutputParameters::read(&mut output, true)
            .expect("the embedded Sapling Output verifying key is valid");
        assert!(
            spend.is_empty() && output.is_empty(),
            "an embedded Sapling verifying key has bytes after its IC points"
        );
        Self {
            spend: spend_params.verifying_key(),
            output: output_params.verifying_key(),
        }
    }
}

/// Whether every Sapling bundle of `items` is valid under `keys`.
pub(crate) fn verify_sapling(keys: &SaplingKeys, items: &[&Item]) -> bool {
    let mut validator = sapling_crypto::BatchValidator::new();
    for item in items {
        let Some(bundle) = item.tx.sapling_bundle() else {
            unreachable!("queued under the Sapling group");
        };
        if !validator.check_bundle(bundle.clone(), item.sighash) {
            return false;
        }
    }
    validator.validate(&keys.spend, &keys.output, os_rng())
}

#[cfg(test)]
mod tests {
    use hayai_crypto::rng::seeded;
    use sapling_crypto::builder::{Builder, BundleType};
    use sapling_crypto::keys::OutgoingViewingKey;
    use sapling_crypto::note_encryption::Zip212Enforcement;
    use sapling_crypto::value::NoteValue;
    use sapling_crypto::{Anchor, Bundle};

    use super::*;
    use crate::coinbase::tests::sapling_recipient;

    fn blake2b_512(bytes: &[u8]) -> String {
        blake2b_simd::blake2b(bytes).to_hex().to_string()
    }

    /// The embedded keys are the start of the official parameter files. The parameters of
    /// `wagyu-zcash-parameters` are the official files: they have the BLAKE2b-512 hashes
    /// that `zcash_proofs` requires.
    #[test]
    fn the_embedded_keys_are_the_start_of_the_official_parameters() {
        let (spend, output) = wagyu_zcash_parameters::load_sapling_parameters();
        assert_eq!(blake2b_512(&spend), SAPLING_SPEND_PARAMS_BLAKE2B);
        assert_eq!(blake2b_512(&output), SAPLING_OUTPUT_PARAMS_BLAKE2B);
        assert_eq!(&spend[..SPEND_VK.len()], SPEND_VK);
        assert_eq!(&output[..OUTPUT_VK.len()], OUTPUT_VK);
        // The key ends where the prover data of the parameters starts: 3 G1 points and 3 G2
        // points, the count of the IC points, and the IC points (7 and 5 public inputs).
        let fixed = 3 * 96 + 3 * 192;
        for (vk, ic) in [(SPEND_VK, 8u32), (OUTPUT_VK, 6u32)] {
            assert_eq!(vk[fixed..fixed + 4], ic.to_be_bytes());
            assert_eq!(vk.len(), fixed + 4 + 96 * ic as usize);
        }
    }

    /// The embedded keys verify a bundle that the official parameters proved, and reject
    /// the same bundle under another sighash.
    #[test]
    fn the_embedded_keys_verify_a_proof_of_the_official_parameters() {
        let (spend, output) = wagyu_zcash_parameters::load_sapling_parameters();
        let spend = SpendParameters::read(&spend[..], false).expect("the Spend parameters parse");
        let output =
            OutputParameters::read(&output[..], false).expect("the Output parameters parse");
        let mut rng = seeded(4);
        let mut builder = Builder::new(
            Zip212Enforcement::On,
            BundleType::DEFAULT,
            Anchor::empty_tree(),
        );
        builder
            .add_output(
                Some(OutgoingViewingKey([7; 32])),
                sapling_recipient(),
                NoteValue::from_raw(50_000),
                [0u8; 512],
            )
            .expect("the output is valid");
        let (bundle, _) = builder
            .build::<SpendParameters, OutputParameters, _, i64>(&[], &mut rng)
            .expect("the bundle builds")
            .expect("the bundle has an output");
        let sighash = [9u8; 32];
        let bundle: Bundle<_, i64> = bundle
            .create_proofs(&spend, &output, &mut rng, ())
            .apply_signatures(&mut rng, sighash, &[])
            .expect("the bundle has no spend to sign");

        let keys = SaplingKeys::embedded();
        let valid = |sighash: [u8; 32]| {
            let mut validator = sapling_crypto::BatchValidator::new();
            assert!(validator.check_bundle(bundle.clone(), sighash));
            validator.validate(&keys.spend, &keys.output, os_rng())
        };
        assert!(valid(sighash));
        assert!(!valid([8u8; 32]));
    }
}
