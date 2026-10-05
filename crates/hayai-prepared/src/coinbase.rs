//! Shielded outputs of a coinbase transaction (ZIP 213).
//!
//! Protocol specification, section 7.1.2:
//!
//! - Before Heartwood, a coinbase transaction has no shielded output.
//! - From Heartwood, every Sapling, Orchard and Ironwood output of a coinbase transaction
//!   decrypts with an outgoing viewing key of 32 zero bytes (section 4.20.3). The upstream
//!   recovery also checks that the decrypted note gives the note commitment of the output.
//! - The lead byte of a Sapling note plaintext is 0x01 in Heartwood and 0x02 from Canopy
//!   (ZIP 212). The grace period of ZIP 212 does not apply to a coinbase. The lead byte of
//!   an Orchard note plaintext is 0x02, and of an Ironwood note plaintext 0x03.
//!
//! Zakura applies the same rule in `zakura-chain/src/primitives/zcash_note_encryption.rs`
//! (`decrypts_successfully`).

use hayai_coins::Pool;
use hayai_crypto::{
    orchard, sapling_crypto, zcash_note_encryption, zcash_primitives, zcash_protocol,
};
use orchard::note_encryption::{IronwoodDomain, OrchardDomain};
use sapling_crypto::bundle::OutputDescription;
use sapling_crypto::note_encryption::{SaplingDomain, Zip212Enforcement};
use zcash_note_encryption::try_output_recovery_with_ovk;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

/// A coinbase transaction whose shielded outputs break ZIP 213.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoinbaseError {
    #[error("coinbase has a shielded output before Heartwood")]
    ShieldedOutputBeforeHeartwood,
    #[error("coinbase {pool} output {index} does not decrypt with the zero outgoing viewing key")]
    OutputNotDecryptable { pool: Pool, index: usize },
}

/// The outgoing viewing key of ZIP 213.
const ZERO_OVK: [u8; 32] = [0u8; 32];

/// Whether a Sapling output decrypts with the zero outgoing viewing key to a note plaintext
/// with the lead byte of `zip212`.
fn sapling_output_decrypts<P>(output: &OutputDescription<P>, zip212: Zip212Enforcement) -> bool {
    let recovered = try_output_recovery_with_ovk(
        &SaplingDomain::new(zip212),
        &sapling_crypto::keys::OutgoingViewingKey(ZERO_OVK),
        output,
        output.cv(),
        output.out_ciphertext(),
    );
    matches!(recovered, Some(_note))
}

/// Whether an action of an Orchard bundle decrypts with the zero outgoing viewing key.
fn orchard_action_decrypts<A>(action: &orchard::Action<A>) -> bool {
    let recovered = try_output_recovery_with_ovk(
        &OrchardDomain::for_action(action),
        &orchard::keys::OutgoingViewingKey::from(ZERO_OVK),
        action,
        action.cv_net(),
        &action.encrypted_note().out_ciphertext,
    );
    matches!(recovered, Some(_note))
}

/// Whether an action of an Ironwood bundle decrypts with the zero outgoing viewing key.
fn ironwood_action_decrypts<A>(action: &orchard::Action<A>) -> bool {
    let recovered = try_output_recovery_with_ovk(
        &IronwoodDomain::for_action(action),
        &orchard::keys::OutgoingViewingKey::from(ZERO_OVK),
        action,
        action.cv_net(),
        &action.encrypted_note().out_ciphertext,
    );
    matches!(recovered, Some(_note))
}

/// Checks the shielded outputs of the coinbase transaction `tx` of a block under `branch`.
pub(crate) fn check_shielded_outputs(
    tx: &Transaction,
    branch: BranchId,
) -> Result<(), CoinbaseError> {
    let sapling = tx
        .sapling_bundle()
        .map_or(&[][..], |b| b.shielded_outputs());
    let orchard = tx.orchard_bundle().map(|b| b.actions());
    let ironwood = tx.ironwood_bundle().map(|b| b.actions());
    let zip212 = match branch {
        BranchId::Sprout | BranchId::Overwinter | BranchId::Sapling | BranchId::Blossom => {
            let (true, None, None) = (sapling.is_empty(), orchard, ironwood) else {
                return Err(CoinbaseError::ShieldedOutputBeforeHeartwood);
            };
            return Ok(());
        }
        BranchId::Heartwood => Zip212Enforcement::Off,
        _ => Zip212Enforcement::On,
    };
    let undecryptable = |pool, index| Err(CoinbaseError::OutputNotDecryptable { pool, index });
    for (index, output) in sapling.iter().enumerate() {
        if !sapling_output_decrypts(output, zip212) {
            return undecryptable(Pool::Sapling, index);
        }
    }
    for (index, action) in orchard.into_iter().flatten().enumerate() {
        if !orchard_action_decrypts(action) {
            return undecryptable(Pool::Orchard, index);
        }
    }
    for (index, action) in ironwood.into_iter().flatten().enumerate() {
        if !ironwood_action_decrypts(action) {
            return undecryptable(Pool::Ironwood, index);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use bytes::Bytes;
    use hayai_coins::OutPoint;
    use hayai_crypto::rng::seeded;
    use hayai_crypto::zcash_transparent;
    use hayai_wire::RawTx;
    use orchard::builder::{Builder as OrchardBuilder, BundleType as OrchardBundleType};
    use orchard::bundle::{Authorized as OrchardAuthorized, BundleVersion, Flags};
    use orchard::keys::{FullViewingKey, Scope, SpendingKey};
    use sapling_crypto::builder::{Builder as SaplingBuilder, BundleType as SaplingBundleType};
    use sapling_crypto::bundle::Authorized as SaplingAuthorized;
    use sapling_crypto::circuit::{OutputParameters, SpendParameters};
    use sapling_crypto::zip32::ExtendedSpendingKey;
    use sapling_crypto::PaymentAddress;
    use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
    use zcash_protocol::consensus::BlockHeight;
    use zcash_protocol::value::{ZatBalance, Zatoshis};
    use zcash_transparent::address::Script;
    use zcash_transparent::bundle::{Authorized as TAuthorized, Bundle as TBundle, TxIn, TxOut};

    use super::*;
    use crate::{draft, PrepareError, RuleEpoch};

    const OTHER_OVK: [u8; 32] = [7u8; 32];

    pub(crate) fn sapling_recipient() -> PaymentAddress {
        ExtendedSpendingKey::master(&[1u8; 32]).default_address().1
    }

    type SaplingBundle = sapling_crypto::Bundle<SaplingAuthorized, ZatBalance>;
    type OrchardBundle = orchard::Bundle<OrchardAuthorized, ZatBalance>;

    /// A coinbase Sapling bundle with one output that the sender encrypts to `ovk`. The
    /// proof and the signature are zero bytes: ZIP 213 reads neither.
    fn sapling_bundle(ovk: Option<[u8; 32]>, zip212: Zip212Enforcement) -> SaplingBundle {
        let mut builder = SaplingBuilder::new(
            zip212,
            SaplingBundleType::Coinbase,
            sapling_crypto::Anchor::empty_tree(),
        );
        builder
            .add_output(
                ovk.map(sapling_crypto::keys::OutgoingViewingKey),
                sapling_recipient(),
                sapling_crypto::value::NoteValue::from_raw(50_000),
                [0u8; 512],
            )
            .expect("the output is valid");
        let (bundle, _) = builder
            .build::<SpendParameters, OutputParameters, _, ZatBalance>(&[], seeded(1))
            .expect("the bundle builds")
            .expect("the bundle has an output");
        bundle.map_authorization(
            (),
            |_, _| [0u8; 192],
            |_, _| [0u8; 192],
            |_, _| [0u8; 64].into(),
            |_, _| SaplingAuthorized {
                binding_sig: [0u8; 64].into(),
            },
        )
    }

    /// A coinbase bundle of `version` with one output that the sender encrypts to `ovk`.
    /// The proof and the signatures are zero bytes.
    fn orchard_bundle(ovk: Option<[u8; 32]>, version: BundleVersion) -> OrchardBundle {
        let recipient = FullViewingKey::from(
            &SpendingKey::from_bytes([3u8; 32]).expect("the bytes are a spending key"),
        )
        .address_at(0u32, Scope::External);
        let mut builder = OrchardBuilder::new(
            OrchardBundleType::Coinbase,
            version,
            Flags::SPENDS_DISABLED,
            orchard::Anchor::empty_tree(),
        )
        .expect("the flags are those of a coinbase");
        builder
            .add_output(
                ovk.map(orchard::keys::OutgoingViewingKey::from),
                recipient,
                orchard::value::NoteValue::from_raw(50_000),
                [0u8; 512],
            )
            .expect("outputs are enabled");
        let (bundle, _) = builder
            .build::<ZatBalance>(seeded(2))
            .expect("the bundle builds")
            .expect("the bundle has an output");
        let actions = bundle.actions().len();
        bundle.map_authorization(
            &mut (),
            |_, _, _| [0u8; 64].into(),
            |_, _| {
                OrchardAuthorized::from_parts(
                    orchard::Proof::new(vec![0u8; orchard::Proof::expected_proof_size(actions)]),
                    [0u8; 64].into(),
                )
            },
        )
    }

    /// A v5 coinbase transaction of `branch` with one transparent output and the bundles.
    fn coinbase(
        branch: BranchId,
        sapling: Option<SaplingBundle>,
        orchard: Option<OrchardBundle>,
    ) -> RawTx {
        let script =
            |bytes: &[u8]| Script(hayai_crypto::zcash_script04::script::Code(bytes.to_vec()));
        let transparent = TBundle {
            vin: vec![TxIn::from_parts(
                OutPoint::NULL,
                script(&[1, 2, 3, 4]),
                u32::MAX,
            )],
            vout: vec![TxOut::new(Zatoshis::const_from_u64(1), script(&[]))],
            authorization: TAuthorized,
        };
        let tx = TransactionData::<Authorized>::from_parts(
            TxVersion::V5,
            branch,
            0,
            BlockHeight::from_u32(0),
            Some(transparent),
            None,
            sapling,
            orchard,
        )
        .freeze()
        .expect("a v5 transaction freezes");
        let mut bytes = Vec::new();
        tx.write(&mut bytes).expect("vec write");
        RawTx::parse(Bytes::from(bytes), branch).expect("round trip")
    }

    fn undecryptable(pool: Pool) -> Result<(), CoinbaseError> {
        Err(CoinbaseError::OutputNotDecryptable { pool, index: 0 })
    }

    #[test]
    fn a_sapling_coinbase_output_decrypts_only_with_the_zero_key() {
        let on = Zip212Enforcement::On;
        let check = |ovk| {
            let raw = coinbase(BranchId::Nu5, Some(sapling_bundle(ovk, on)), None);
            check_shielded_outputs(&raw.tx, BranchId::Nu5)
        };
        assert_eq!(check(Some(ZERO_OVK)), Ok(()));
        assert_eq!(check(Some(OTHER_OVK)), undecryptable(Pool::Sapling));
        // Without an outgoing viewing key, the sender encrypts to a random key.
        assert_eq!(check(None), undecryptable(Pool::Sapling));
    }

    /// The lead byte of the note plaintext is 0x01 in Heartwood and 0x02 from Canopy.
    #[test]
    fn the_sapling_lead_byte_follows_the_upgrade() {
        let output = |zip212| sapling_bundle(Some(ZERO_OVK), zip212).shielded_outputs()[0].clone();
        let (v1, v2) = (
            output(Zip212Enforcement::Off),
            output(Zip212Enforcement::On),
        );
        assert!(sapling_output_decrypts(&v1, Zip212Enforcement::Off));
        assert!(!sapling_output_decrypts(&v2, Zip212Enforcement::Off));
        assert!(!sapling_output_decrypts(&v1, Zip212Enforcement::On));
        assert!(sapling_output_decrypts(&v2, Zip212Enforcement::On));

        // The branch selects the lead byte. The transaction is the same for each branch:
        // the check reads the outputs only.
        let raw = |zip212| {
            coinbase(
                BranchId::Nu5,
                Some(sapling_bundle(Some(ZERO_OVK), zip212)),
                None,
            )
        };
        let (v1, v2) = (raw(Zip212Enforcement::Off), raw(Zip212Enforcement::On));
        assert_eq!(check_shielded_outputs(&v1.tx, BranchId::Heartwood), Ok(()));
        assert_eq!(
            check_shielded_outputs(&v2.tx, BranchId::Heartwood),
            undecryptable(Pool::Sapling)
        );
        for branch in [BranchId::Canopy, BranchId::Nu5, BranchId::Nu6_3] {
            assert_eq!(
                check_shielded_outputs(&v1.tx, branch),
                undecryptable(Pool::Sapling)
            );
            assert_eq!(check_shielded_outputs(&v2.tx, branch), Ok(()));
        }
    }

    #[test]
    fn a_coinbase_has_no_shielded_output_before_heartwood() {
        let shielded = coinbase(
            BranchId::Nu5,
            Some(sapling_bundle(Some(ZERO_OVK), Zip212Enforcement::Off)),
            None,
        );
        let transparent = coinbase(BranchId::Nu5, None, None);
        for branch in [
            BranchId::Sprout,
            BranchId::Overwinter,
            BranchId::Sapling,
            BranchId::Blossom,
        ] {
            assert_eq!(
                check_shielded_outputs(&shielded.tx, branch),
                Err(CoinbaseError::ShieldedOutputBeforeHeartwood)
            );
            assert_eq!(check_shielded_outputs(&transparent.tx, branch), Ok(()));
        }
    }

    #[test]
    fn an_orchard_coinbase_output_decrypts_only_with_the_zero_key() {
        let check = |ovk| {
            let bundle = orchard_bundle(ovk, BundleVersion::orchard_v2());
            let raw = coinbase(BranchId::Nu6_2, None, Some(bundle));
            check_shielded_outputs(&raw.tx, BranchId::Nu6_2)
        };
        assert_eq!(check(Some(ZERO_OVK)), Ok(()));
        assert_eq!(check(Some(OTHER_OVK)), undecryptable(Pool::Orchard));
        assert_eq!(check(None), undecryptable(Pool::Orchard));
    }

    /// An Orchard action has a note plaintext with the lead byte 0x02 and an Ironwood
    /// action one with the lead byte 0x03. Each check accepts only its own lead byte.
    #[test]
    fn an_ironwood_coinbase_output_decrypts_only_with_the_zero_key() {
        let action = |ovk, version| orchard_bundle(ovk, version).actions().first().clone();
        let ironwood = action(Some(ZERO_OVK), BundleVersion::ironwood_v3());
        let orchard = action(Some(ZERO_OVK), BundleVersion::orchard_v2());
        assert!(ironwood_action_decrypts(&ironwood));
        assert!(!ironwood_action_decrypts(&orchard));
        assert!(!orchard_action_decrypts(&ironwood));
        assert!(orchard_action_decrypts(&orchard));
        let other = action(Some(OTHER_OVK), BundleVersion::ironwood_v3());
        assert!(!ironwood_action_decrypts(&other));
    }

    /// `draft` runs the check on a coinbase transaction, and on no other transaction.
    #[test]
    fn draft_checks_the_shielded_outputs_of_a_coinbase() {
        let epoch = RuleEpoch::consensus(BranchId::Nu6_2);
        let sapling = |ovk| sapling_bundle(Some(ovk), Zip212Enforcement::On);
        let orchard = |ovk| orchard_bundle(Some(ovk), BundleVersion::orchard_v2());
        let run = |sapling, orchard| {
            draft(
                coinbase(BranchId::Nu6_2, sapling, orchard),
                epoch,
                Vec::new(),
            )
            .map(|_| ())
        };
        assert_eq!(
            run(Some(sapling(ZERO_OVK)), Some(orchard(ZERO_OVK))),
            Ok(())
        );
        assert_eq!(
            run(Some(sapling(OTHER_OVK)), Some(orchard(ZERO_OVK))),
            Err(PrepareError::CoinbaseShieldedOutput(
                CoinbaseError::OutputNotDecryptable {
                    pool: Pool::Sapling,
                    index: 0
                }
            ))
        );
        assert_eq!(
            run(Some(sapling(ZERO_OVK)), Some(orchard(OTHER_OVK))),
            Err(PrepareError::CoinbaseShieldedOutput(
                CoinbaseError::OutputNotDecryptable {
                    pool: Pool::Orchard,
                    index: 0
                }
            ))
        );
    }
}
