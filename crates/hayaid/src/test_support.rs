//! A valid Regtest transaction for the node tests.

use std::sync::OnceLock;

use bytes::Bytes;
use hayai_coins::OutPoint;
use hayai_crypto::rng::os_rng;
use hayai_crypto::{
    orchard, sapling_crypto, zcash_primitives, zcash_protocol, zcash_script04, zcash_transparent,
};
use orchard::builder::{Builder, BundleType, InProgress, Unauthorized, Unproven};
use orchard::bundle::BundleVersion;
use orchard::circuit::ProvingKey;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::value::NoteValue;
use orchard::Anchor;
use zcash_primitives::transaction::sighash::{signature_hash, SignableInput};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{Authorization, Authorized, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::{ZatBalance, Zatoshis};
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Authorized as TAuthorized, Bundle, TxIn, TxOut};
use zcash_transparent::sighash::TransparentAuthorizingContext;

/// The scriptPubKey of the spent coin: `OP_TRUE`.
const OP_TRUE: [u8; 1] = [0x51];

/// The transparent part of [`Unsigned`]. It holds the spent coin, to which the ZIP 244
/// sighash commits.
#[derive(Debug)]
struct SpentCoin(TxOut);

impl zcash_transparent::bundle::Authorization for SpentCoin {
    type ScriptSig = Script;
}

impl TransparentAuthorizingContext for SpentCoin {
    fn input_amounts(&self) -> Vec<Zatoshis> {
        vec![self.0.value()]
    }

    fn input_scriptpubkeys(&self) -> Vec<Script> {
        vec![self.0.script_pubkey().clone()]
    }
}

/// Authorization marker of the transaction before the proof and the signatures of the
/// Orchard bundle. The transaction has no Sapling bundle.
struct Unsigned;

impl Authorization for Unsigned {
    type TransparentAuth = SpentCoin;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = InProgress<Unproven, Unauthorized>;
}

fn script(bytes: &[u8]) -> Script {
    Script(zcash_script04::script::Code(bytes.to_vec()))
}

/// The transaction of the epoch `branch` with one transparent input and the bundle
/// `shielded`: a v5 transaction with an Orchard bundle, or with `ironwood` a v6
/// transaction with an Ironwood bundle.
fn transaction_data<A: Authorization>(
    branch: BranchId,
    outpoint: OutPoint,
    expiry_height: u32,
    authorization: A::TransparentAuth,
    shielded: orchard::Bundle<A::OrchardAuth, ZatBalance>,
    ironwood: bool,
) -> TransactionData<A>
where
    A::TransparentAuth: zcash_transparent::bundle::Authorization<ScriptSig = Script>,
{
    let expiry_height = BlockHeight::from_u32(expiry_height);
    let transparent = Some(Bundle {
        vin: vec![TxIn::from_parts(outpoint, script(&[]), u32::MAX)],
        vout: Vec::new(),
        authorization,
    });
    match ironwood {
        true => TransactionData::from_parts_v6(
            branch,
            0,
            expiry_height,
            transparent,
            None,
            None,
            Some(shielded),
        ),
        false => TransactionData::from_parts(
            TxVersion::V5,
            branch,
            0,
            expiry_height,
            transparent,
            None,
            None,
            Some(shielded),
        ),
    }
}

/// A Regtest transaction of the epoch `branch` that spends one transparent coin with the
/// script `OP_TRUE` (`[0x51]`) into one shielded output: a v5 transaction with an Orchard
/// output from NU5 to NU6.2, and a v6 transaction with an Ironwood output in NU6.3 and
/// NU7. The fee is `value - shielded`.
pub fn shielding_tx(
    outpoint: OutPoint,
    value: u64,
    fee: u64,
    expiry_height: u32,
    branch: BranchId,
) -> Bytes {
    static PROVING_KEYS: [OnceLock<ProvingKey>; 3] =
        [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    // The bundle version of the epoch, with its circuit version: `InsecurePreNu6_2` until
    // NU6.1, `FixedPostNu6_2` in NU6.2, and the Ironwood pool with `PostNu6_3` in NU6.3
    // and NU7.
    let (version, proving_key) = match branch {
        BranchId::Nu5 | BranchId::Nu6 | BranchId::Nu6_1 => {
            (BundleVersion::orchard_insecure_v1(), &PROVING_KEYS[0])
        }
        BranchId::Nu6_2 => (BundleVersion::orchard_v2(), &PROVING_KEYS[1]),
        BranchId::Nu6_3 => (BundleVersion::ironwood_v3(), &PROVING_KEYS[2]),
        other if Some(other) == hayai_crypto::nu7_branch() => {
            (BundleVersion::ironwood_v3(), &PROVING_KEYS[2])
        }
        other => panic!("no shielding transaction for {other:?}"),
    };
    let ironwood = version == BundleVersion::ironwood_v3();
    let recipient = {
        let sk = SpendingKey::from_bytes([7; 32]).expect("a valid spending key");
        FullViewingKey::from(&sk).address_at(0u32, Scope::External)
    };
    let mut builder = Builder::new(
        BundleType::DEFAULT,
        version,
        version.default_flags(),
        Anchor::empty_tree(),
    )
    .expect("default flags are representable");
    builder
        .add_output(None, recipient, NoteValue::from_raw(value - fee), [0; 512])
        .expect("outputs enabled");
    let mut rng = os_rng();
    let (bundle, _meta) = builder
        .build::<ZatBalance>(&mut rng)
        .expect("bundle builds")
        .expect("bundle has an output");

    let coin = TxOut::new(
        Zatoshis::from_u64(value).expect("a valid amount"),
        script(&OP_TRUE),
    );
    let unsigned = transaction_data::<Unsigned>(
        branch,
        outpoint.clone(),
        expiry_height,
        SpentCoin(coin),
        bundle.clone(),
        ironwood,
    );
    let txid_parts = unsigned.digest(TxIdDigester);
    let sighash = *signature_hash(&unsigned, &SignableInput::Shielded, &txid_parts).as_ref();

    let key = proving_key.get_or_init(|| ProvingKey::build(version.circuit_version()));
    let bundle = bundle
        .create_proof(key, &mut rng)
        .expect("proof")
        .apply_signatures(rng, sighash, &[])
        .expect("only dummy spends to sign");
    let tx = transaction_data::<Authorized>(
        branch,
        outpoint,
        expiry_height,
        TAuthorized,
        bundle,
        ironwood,
    )
    .freeze()
    .expect("the transaction freezes");
    let mut bytes = Vec::new();
    tx.write(&mut bytes).expect("vec write");
    Bytes::from(bytes)
}

#[cfg(test)]
mod tests {
    use hayai_coins::{Coin, CoinsView};
    use hayai_consensus::{rules_at, Network};
    use hayai_prepared::{
        prepare, MempoolPolicy, PolicyContext, PolicyReject, PreparedTx, RuleEpoch, ScopedBatch,
        VerifyingKeys,
    };
    use hayai_wire::RawTx;

    use super::*;

    const VALUE: u64 = 625_000_000;
    const NEXT_HEIGHT: u32 = 150;

    /// A view with one coin: a coinbase coin of height 1 with the script `OP_TRUE`.
    struct OneCoin(OutPoint);

    impl CoinsView for OneCoin {
        fn get_coins(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
            outpoints
                .iter()
                .map(|outpoint| {
                    (*outpoint == self.0).then(|| Coin {
                        value: VALUE,
                        script_pubkey: Bytes::from_static(&OP_TRUE),
                        height: 1,
                        is_coinbase: true,
                    })
                })
                .collect()
        }
    }

    #[test]
    fn shielding_tx_is_valid_and_follows_the_policy() {
        let rules = rules_at(Network::Regtest, NEXT_HEIGHT).expect("Regtest rules");
        let epoch = RuleEpoch::of(rules);
        let keys = VerifyingKeys::prebuild(epoch, None);
        keys.ready();
        let outpoint = OutPoint::new([3; 32], 0);
        let view = OneCoin(outpoint.clone());

        let prepared = |fee: u64, expiry_height: u32| -> PreparedTx {
            let bytes = shielding_tx(outpoint.clone(), VALUE, fee, expiry_height, BranchId::Nu5);
            let raw = RawTx::parse(bytes, BranchId::Nu5).expect("the transaction parses");
            let mut batch = ScopedBatch::new(&keys);
            let prepared = prepare(raw, epoch, &view, &mut batch).expect("prepare passes");
            assert!(batch.finalize().failed.is_empty());
            assert_eq!(prepared.fee, fee);
            assert_eq!(prepared.expiry_height, expiry_height);
            prepared
        };
        let policy = MempoolPolicy::of(Network::Regtest);
        let admit = |tx: &PreparedTx| {
            policy.admit(
                tx,
                &PolicyContext {
                    next_height: NEXT_HEIGHT,
                    median_time_past: 0,
                    rules,
                },
            )
        };

        // One transparent input and two Orchard actions: 3 logical actions, 15,000
        // zatoshis. The unpaid action limit is 0.
        assert_eq!(admit(&prepared(15_000, 0)), Ok(()));
        assert_eq!(
            admit(&prepared(14_999, 0)),
            Err(PolicyReject::UnpaidActions {
                unpaid: 1,
                limit: 0
            })
        );
        assert_eq!(
            admit(&prepared(15_000, NEXT_HEIGHT + 1)),
            Err(PolicyReject::ExpiringSoon {
                expiry: NEXT_HEIGHT + 1,
                next_height: NEXT_HEIGHT
            })
        );
    }
}
