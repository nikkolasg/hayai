//! Deterministic synthetic blocks for tests, benchmarks and the differential fuzzer.
//!
//! Every fixture is a mainnet-shaped block, built from upstream types with real ECDSA
//! signatures and real Orchard proofs. The fixtures of the NU6.2 epoch are at
//! [`FIXTURE_HEIGHT`] in the [`FIXTURE_BRANCH`] epoch:
//!
//! - transaction 0 is a coinbase that pays the Mainnet terms of the height
//!   (`hayai_consensus::coinbase::CoinbaseTerms`): the height pushed in `scriptSig`, the
//!   expiry height equal to the block height, one P2PKH output with the miner's part of the
//!   subsidy and the fees of the block, and one output for each funding stream;
//! - transparent transactions are v5, spend `inputs_per_tx` P2PKH coins of a synthetic
//!   funding set, pay two P2PKH outputs and the ZIP 317 conventional fee;
//! - Orchard transactions are v5, spend one P2PKH funding coin and create
//!   `actions_per_bundle` Orchard outputs (dummy spends, anchor = empty tree, proof from
//!   `orchard::circuit::ProvingKey`), paying the ZIP 317 conventional fee.
//!
//! The fixtures of the NU6.3 epoch ([`nu6_3_block`]) are at [`NU6_3_FIXTURE_HEIGHT`] in the
//! [`NU6_3_FIXTURE_BRANCH`] epoch, with proofs under the NU6.3 circuit:
//!
//! - the coinbase is a v6 transaction. With a shielded coinbase it has one Ironwood output
//!   (`enableSpends = 0`, encrypted to the zero outgoing viewing key of ZIP 213);
//! - Ironwood transactions are v6, spend one P2PKH funding coin and create Ironwood
//!   outputs (dummy spends, anchor = empty tree);
//! - Orchard transactions are v5, spend one P2PKH funding coin, pay one P2PKH output and
//!   have an Orchard bundle of two padding actions: from NU6.3 the Orchard pool takes no
//!   value and no cross-address transfer (`enableCrossAddress = 0`), so the bundle has a
//!   value balance of zero;
//! - transactions with both bundles are v6: the Orchard padding bundle and Ironwood
//!   outputs.
//!
//! The header carries the merkle root of the body; the block commitments field is zero and
//! the proof of work and Equihash solution are not valid (valid merkle only). The funding set
//! is returned with each fixture so that a UTXO set can be seeded before validation.
//!
//! Generation is reproducible from [`SEED`]: keys, outpoints and the RNG of every proof are
//! derived from it with SHA-256, so parallel generation is order-independent. Expensive
//! fixtures are cached as raw bytes under `<repo>/bench-fixtures/<name>.bin`; the funding set
//! is cheap and is re-derived on every call. Delete the cache or bump
//! [`GENERATOR_VERSION`] after changing the generator.
//!
//! No Sapling: the Sapling Groth16 parameters are not installed; see
//! [`sapling_params_todo`].
//!
//! [`regtest`] builds single Regtest transactions that spend one transparent coin, for the
//! admission tests of the mempool and the tests of the node.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use bytes::Bytes;
use hayai_crypto::rng::{SeedableRng, StdRng};
use hayai_crypto::{
    orchard, sapling_crypto, zcash_encoding, zcash_primitives, zcash_protocol, zcash_transparent,
};
use orchard::builder::{Builder as OrchardBuilder, BundleType};
use orchard::bundle::{BundleVersion, Flags};
use orchard::circuit::{OrchardCircuitVersion, ProvingKey};
use orchard::keys::{FullViewingKey, OutgoingViewingKey, Scope, SpendingKey};
use orchard::value::NoteValue;
use orchard::Anchor;
use rayon::prelude::*;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use zcash_encoding::CompactSize;
use zcash_primitives::transaction::sighash::{signature_hash, SignableInput};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{
    Authorization, Authorized, Transaction, TransactionData, TxVersion,
};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::{ZatBalance, Zatoshis};
use zcash_transparent::address::{Script, TransparentAddress};
use zcash_transparent::builder::{Coinbase, TransparentBuilder, TransparentSigningSet};
use zcash_transparent::bundle::{OutPoint, TxOut};

use hayai_consensus::coinbase::CoinbaseTerms;
use hayai_consensus::Network;
use hayai_wire::header::{BlockHash, BlockHeader, PowParams};
use hayai_wire::{merkle_root, RawBlock};

pub mod regtest;

/// The network of every fixture block: the heights and the branches of the fixtures are
/// Mainnet heights and branches, and the coinbase pays the Mainnet terms of its height.
pub const FIXTURE_NETWORK: Network = Network::Mainnet;

/// Height of every fixture block; on mainnet this height is in the NU6.2 epoch.
pub const FIXTURE_HEIGHT: u32 = 3_400_000;
/// Consensus branch of every fixture block. NU6.2 is the last epoch whose Orchard pool
/// accepts v5 bundles with bare outputs (NU6.3 disables cross-address transfers for the
/// Orchard pool and moves to v6 transactions).
pub const FIXTURE_BRANCH: BranchId = BranchId::Nu6_2;
/// Height of every fixture block of the NU6.3 epoch; on mainnet NU6.3 activates at
/// 3,428,143.
pub const NU6_3_FIXTURE_HEIGHT: u32 = 3_450_000;
/// Consensus branch of the fixture blocks of the NU6.3 epoch.
pub const NU6_3_FIXTURE_BRANCH: BranchId = BranchId::Nu6_3;
/// Root of all key, outpoint and RNG derivations.
pub const SEED: u64 = 0x6861_7961_6900_0001;
/// Bumped whenever generated bytes change; part of the cache file header.
pub const GENERATOR_VERSION: u32 = 2;

const CACHE_MAGIC: &[u8; 8] = b"HAYAIFIX";
/// Value of every P2PKH funding coin spent by a transparent transaction.
const TRANSPARENT_INPUT_VALUE: u64 = 1_000_000;
/// Value of every Orchard and Ironwood output note.
const ORCHARD_NOTE_VALUE: u64 = 100_000;
/// Value of the Ironwood output of a shielded coinbase.
const COINBASE_NOTE_VALUE: u64 = 1_000_000;
const ZIP317_MARGINAL_FEE: u64 = 5_000;
const ZIP317_GRACE_ACTIONS: u64 = 2;
const HEADER_TIME: u32 = 1_700_000_000;
const HEADER_BITS: u32 = 0x1f07_ffff;

/// A spendable transparent output of the synthetic funding set, in the shape of a coins
/// entry (`docs/architecture.md`, hayai-coins).
#[derive(Clone, Debug)]
pub struct FundingCoin {
    pub value: u64,
    pub script_pubkey: Script,
    pub height: u32,
    pub is_coinbase: bool,
}

/// A generated block plus the funding outputs its transactions spend.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub name: String,
    pub bytes: Bytes,
    pub height: u32,
    pub branch_id: BranchId,
    /// Outputs spent by transactions 1.. of the block, in input order.
    pub funding: Vec<(OutPoint, FundingCoin)>,
}

impl Fixture {
    pub fn parse(&self) -> RawBlock {
        RawBlock::parse(self.bytes.clone(), self.branch_id).expect("fixture parses")
    }

    /// The block of this fixture with a changed coinbase, for the tests of the coinbase
    /// rules: `changes` apply in order to the transparent outputs of the coinbase. The
    /// coinbase keeps its input and its expiry height, and the header has the merkle root
    /// of the new body. The coinbase must have no shielded bundle: the signatures of a
    /// bundle do not cover the new outputs.
    pub fn with_coinbase(&self, changes: &[CoinbaseChange]) -> RawBlock {
        let block = self.parse();
        let coinbase = &block.txs[0].tx;
        assert!(
            matches!(
                (coinbase.orchard_bundle(), coinbase.ironwood_bundle()),
                (None, None)
            ),
            "{}: the coinbase has a shielded bundle",
            self.name
        );
        let bundle = coinbase
            .transparent_bundle()
            .expect("a coinbase has a transparent bundle");
        let mut vout = bundle.vout.clone();
        for change in changes {
            match *change {
                CoinbaseChange::AddValue { output, delta } => {
                    let value = i128::from(vout[output].value().into_u64()) + i128::from(delta);
                    let value = u64::try_from(value).expect("the new value is not negative");
                    vout[output] = TxOut::new(
                        Zatoshis::const_from_u64(value),
                        vout[output].script_pubkey().clone(),
                    );
                }
                CoinbaseChange::Remove { output } => {
                    vout.remove(output);
                }
                CoinbaseChange::ChangeScript { output } => {
                    let mut script = vout[output].script_pubkey().clone();
                    script.0 .0[2] ^= 0x80;
                    vout[output] = TxOut::new(vout[output].value(), script);
                }
            }
        }
        let epoch = Epoch {
            height: self.height,
            branch: self.branch_id,
        };
        let transparent = zcash_transparent::bundle::Bundle {
            vin: bundle.vin.clone(),
            vout,
            authorization: zcash_transparent::bundle::Authorized,
        };
        let changed = transaction_data::<Authorized>(
            coinbase.version(),
            epoch,
            self.height,
            transparent,
            None,
            None,
        )
        .freeze()
        .expect("the bundles match the transaction version");
        let mut txids = block.txids();
        txids[0] = changed.txid();
        let header = BlockHeader {
            merkle_root: merkle_root(&txids),
            ..block.header.clone()
        };
        let mut out = header.serialize();
        CompactSize::write(&mut out, block.txs.len()).expect("vec write");
        changed.write(&mut out).expect("vec write");
        for tx in &block.txs[1..] {
            out.extend_from_slice(&tx.bytes);
        }
        RawBlock::parse(Bytes::from(out), self.branch_id).expect("the changed block parses")
    }

    /// The block of this fixture as the block at `height` with the parent `prev_hash`, for
    /// a chain of fixture blocks: the coinbase has the new height and pays the terms of
    /// that height, and the header has the new parent and the merkle root of the new
    /// body. The other transactions stay: they have no expiry height. `height` must be in
    /// the epoch of the fixture, and the coinbase must have no shielded bundle.
    pub fn at(&self, height: u32, prev_hash: BlockHash) -> RawBlock {
        let block = self.parse();
        let paid = block.txs[0]
            .tx
            .transparent_bundle()
            .expect("a coinbase has a transparent bundle")
            .vout[0]
            .value()
            .into_u64();
        let fees = paid - coinbase_terms(self.height).miner_subsidy;
        let epoch = Epoch {
            height,
            branch: self.branch_id,
        };
        let moved = coinbase(fees, epoch, false);
        let mut txids = block.txids();
        txids[0] = moved.txid();
        let header = BlockHeader {
            prev_hash,
            merkle_root: merkle_root(&txids),
            ..block.header.clone()
        };
        let mut out = header.serialize();
        CompactSize::write(&mut out, block.txs.len()).expect("vec write");
        moved.write(&mut out).expect("vec write");
        for tx in &block.txs[1..] {
            out.extend_from_slice(&tx.bytes);
        }
        RawBlock::parse(Bytes::from(out), self.branch_id).expect("the moved block parses")
    }
}

/// One change to the transparent outputs of the coinbase of a fixture block
/// ([`Fixture::with_coinbase`]). Output 0 is the miner output. The outputs after it are the
/// required outputs of the coinbase terms.
#[derive(Clone, Copy, Debug)]
pub enum CoinbaseChange {
    /// Adds `delta` zatoshis to the value of the output.
    AddValue { output: usize, delta: i64 },
    /// Removes the output.
    Remove { output: usize },
    /// Changes one bit of the script hash of the output.
    ChangeScript { output: usize },
}

/// Block of `n_txs` transparent transactions with `inputs_per_tx` inputs each.
pub fn transparent_block(n_txs: usize, inputs_per_tx: usize) -> Fixture {
    assert!(
        inputs_per_tx >= 1,
        "a transparent transaction needs an input"
    );
    let specs: Vec<TxSpec> = (0..n_txs)
        .map(|i| TxSpec {
            index: i as u64,
            kind: Kind::Transparent {
                inputs: inputs_per_tx,
            },
        })
        .collect();
    generate(&format!("transparent-{n_txs}x{inputs_per_tx}"), &specs)
}

/// Block of `n_bundles` Orchard transactions with `actions_per_bundle` actions each
/// (at least 2: the upstream builder pads to the two-action minimum).
pub fn orchard_block(n_bundles: usize, actions_per_bundle: usize) -> Fixture {
    assert!(
        actions_per_bundle >= 2,
        "orchard bundles are padded to at least 2 actions"
    );
    let specs: Vec<TxSpec> = (0..n_bundles)
        .map(|i| TxSpec {
            index: i as u64,
            kind: Kind::Orchard {
                actions: actions_per_bundle,
            },
        })
        .collect();
    generate(&format!("orchard-{n_bundles}x{actions_per_bundle}"), &specs)
}

/// Block interleaving transparent and Orchard transactions (transparent first).
pub fn mixed_block(
    n_transparent: usize,
    inputs_per_tx: usize,
    n_bundles: usize,
    actions_per_bundle: usize,
) -> Fixture {
    assert!(inputs_per_tx >= 1 && actions_per_bundle >= 2);
    let mut specs: Vec<TxSpec> = (0..n_transparent)
        .map(|i| TxSpec {
            index: i as u64,
            kind: Kind::Transparent {
                inputs: inputs_per_tx,
            },
        })
        .collect();
    specs.extend((0..n_bundles).map(|i| TxSpec {
        index: i as u64,
        kind: Kind::Orchard {
            actions: actions_per_bundle,
        },
    }));
    generate(
        &format!("mixed-{n_transparent}x{inputs_per_tx}-{n_bundles}x{actions_per_bundle}"),
        &specs,
    )
}

/// Block of the NU6.3 epoch: `n_ironwood` v6 transactions with an Ironwood bundle of
/// `actions_per_bundle` actions (at least 2), `n_orchard` v5 transactions with an Orchard
/// bundle of NU6.3 (two padding actions), and `n_both` v6 transactions with both bundles.
/// With `shielded_coinbase`, the coinbase has an Ironwood output.
pub fn nu6_3_block(
    n_ironwood: usize,
    actions_per_bundle: usize,
    n_orchard: usize,
    n_both: usize,
    shielded_coinbase: bool,
) -> Fixture {
    assert!(
        actions_per_bundle >= 2,
        "ironwood bundles are padded to at least 2 actions"
    );
    let kinds = [
        (
            n_ironwood,
            Kind::Ironwood {
                actions: actions_per_bundle,
            },
        ),
        (n_orchard, Kind::OrchardNu6_3),
        (
            n_both,
            Kind::OrchardAndIronwood {
                actions: actions_per_bundle,
            },
        ),
    ];
    let specs: Vec<TxSpec> = kinds
        .into_iter()
        .flat_map(|(n, kind)| {
            (0..n).map(move |i| TxSpec {
                index: i as u64,
                kind,
            })
        })
        .collect();
    let coinbase = if shielded_coinbase { "-shielded" } else { "" };
    generate_in(
        &format!(
            "nu63-{n_ironwood}x{actions_per_bundle}-{n_orchard}-{n_both}x{actions_per_bundle}{coinbase}"
        ),
        &specs,
        Epoch::NU6_3,
        shielded_coinbase,
    )
}

/// The fixtures every bench target measures, from a 1,000-transaction transparent block to
/// a full 2 MB block of each shape. First generation of the Orchard-bearing fixtures takes
/// tens of seconds (one proof per bundle); later calls read the cache.
pub fn standard_set() -> Vec<Fixture> {
    vec![
        transparent_block(1000, 2),
        transparent_block(6500, 1),
        orchard_block(165, 2),
        mixed_block(2000, 1, 100, 2),
    ]
}

/// Directory of cached fixtures: `<repo>/bench-fixtures`.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench-fixtures")
}

/// ZIP 317 conventional fee for a transaction with the given transparent inputs and
/// outputs and Orchard and Ironwood actions (no Sapling).
fn zip317_fee(tin: usize, tout: usize, orchard_actions: usize) -> u64 {
    let logical = (tin.max(tout) + orchard_actions) as u64;
    ZIP317_MARGINAL_FEE * logical.max(ZIP317_GRACE_ACTIONS)
}

/// Actions of the Orchard bundle of a NU6.3 fixture transaction: the padding minimum.
const ORCHARD_NU6_3_ACTIONS: usize = 2;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Transparent {
        inputs: usize,
    },
    Orchard {
        actions: usize,
    },
    /// NU6.3, v6: an Ironwood bundle with `actions` outputs.
    Ironwood {
        actions: usize,
    },
    /// NU6.3, v5: an Orchard bundle of padding actions and one transparent output.
    OrchardNu6_3,
    /// NU6.3, v6: an Orchard bundle of padding actions and an Ironwood bundle with
    /// `actions` outputs.
    OrchardAndIronwood {
        actions: usize,
    },
}

/// The height and the consensus branch of a fixture block.
#[derive(Clone, Copy, Debug)]
struct Epoch {
    height: u32,
    branch: BranchId,
}

impl Epoch {
    const NU6_2: Self = Self {
        height: FIXTURE_HEIGHT,
        branch: FIXTURE_BRANCH,
    };
    const NU6_3: Self = Self {
        height: NU6_3_FIXTURE_HEIGHT,
        branch: NU6_3_FIXTURE_BRANCH,
    };
}

#[derive(Clone, Copy, Debug)]
struct TxSpec {
    index: u64,
    kind: Kind,
}

/// SHA-256 of `SEED || tag || i || j`: the single derivation used for every secret and nonce.
fn derive(tag: &str, i: u64, j: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(SEED.to_le_bytes());
    h.update(tag.as_bytes());
    h.update(i.to_le_bytes());
    h.update(j.to_le_bytes());
    h.finalize().into()
}

fn derived_rng(tag: &str, i: u64) -> StdRng {
    StdRng::from_seed(derive(tag, i, 0))
}

struct FundingInput {
    outpoint: OutPoint,
    coin: FundingCoin,
    sk: SecretKey,
    pk: PublicKey,
}

impl TxSpec {
    fn tag(&self) -> &'static str {
        match self.kind {
            Kind::Transparent { .. } => "transparent",
            Kind::Orchard { .. } => "orchard",
            Kind::Ironwood { .. } => "ironwood",
            Kind::OrchardNu6_3 => "orchard-nu63",
            Kind::OrchardAndIronwood { .. } => "orchard-ironwood",
        }
    }

    fn input_count(&self) -> usize {
        match self.kind {
            Kind::Transparent { inputs } => inputs,
            Kind::Orchard { .. }
            | Kind::Ironwood { .. }
            | Kind::OrchardNu6_3
            | Kind::OrchardAndIronwood { .. } => 1,
        }
    }

    fn fee(&self) -> u64 {
        match self.kind {
            Kind::Transparent { inputs } => zip317_fee(inputs, 2, 0),
            Kind::Orchard { actions } | Kind::Ironwood { actions } => zip317_fee(1, 0, actions),
            Kind::OrchardNu6_3 => zip317_fee(1, 1, ORCHARD_NU6_3_ACTIONS),
            Kind::OrchardAndIronwood { actions } => {
                zip317_fee(1, 0, ORCHARD_NU6_3_ACTIONS + actions)
            }
        }
    }

    fn input_value(&self) -> u64 {
        match self.kind {
            Kind::Transparent { .. } | Kind::OrchardNu6_3 => TRANSPARENT_INPUT_VALUE,
            Kind::Orchard { actions }
            | Kind::Ironwood { actions }
            | Kind::OrchardAndIronwood { actions } => {
                actions as u64 * ORCHARD_NOTE_VALUE + self.fee()
            }
        }
    }

    fn funding_inputs(&self, secp: &Secp256k1<secp256k1::All>, epoch: Epoch) -> Vec<FundingInput> {
        (0..self.input_count())
            .map(|j| {
                let j = j as u64;
                let sk = SecretKey::from_slice(&derive(self.tag(), self.index, j)).expect(
                    "a SHA-256 output is a valid secp256k1 scalar with overwhelming probability",
                );
                let pk = PublicKey::from_secret_key(secp, &sk);
                let outpoint = OutPoint::new(
                    derive(&format!("{}-txid", self.tag()), self.index, j),
                    j as u32,
                );
                let coin = FundingCoin {
                    value: self.input_value(),
                    script_pubkey: TransparentAddress::from_pubkey(&pk).script().into(),
                    height: epoch.height - 1000,
                    is_coinbase: false,
                };
                FundingInput {
                    outpoint,
                    coin,
                    sk,
                    pk,
                }
            })
            .collect()
    }

    fn build(&self, secp: &Secp256k1<secp256k1::All>, epoch: Epoch) -> Transaction {
        let inputs = self.funding_inputs(secp, epoch);
        let mut signing = TransparentSigningSet::new();
        let mut tb = TransparentBuilder::empty();
        for input in &inputs {
            signing.add_key(input.sk);
            tb.add_p2pkh_input(
                input.pk,
                input.outpoint.clone(),
                TxOut::new(
                    Zatoshis::const_from_u64(input.coin.value),
                    input.coin.script_pubkey.clone(),
                ),
            )
            .expect("valid p2pkh input");
        }
        match self.kind {
            Kind::Transparent { .. } => {
                let total: u64 = inputs.iter().map(|i| i.coin.value).sum();
                let out = (total - self.fee()) / 2;
                for k in 0..2u64 {
                    let dest = derive("transparent-dest", self.index, k);
                    tb.add_output(
                        &TransparentAddress::PublicKeyHash(dest[..20].try_into().expect("20")),
                        Zatoshis::const_from_u64(out),
                    )
                    .expect("valid output");
                }
                sign(
                    tb.build().expect("has inputs"),
                    None,
                    None,
                    &signing,
                    derived_rng(self.tag(), self.index),
                    epoch,
                    TxVersion::V5,
                )
            }
            Kind::Orchard { actions } => {
                let version = BundleVersion::orchard_v2();
                let mut ob = OrchardBuilder::new(
                    BundleType::DEFAULT,
                    version,
                    version.default_flags(),
                    Anchor::empty_tree(),
                )
                .expect("default flags are representable");
                for _ in 0..actions {
                    ob.add_output(
                        None,
                        orchard_recipient(),
                        NoteValue::from_raw(ORCHARD_NOTE_VALUE),
                        [0u8; 512],
                    )
                    .expect("outputs enabled");
                }
                let mut rng = derived_rng(self.tag(), self.index);
                let (bundle, _meta) = ob
                    .build::<ZatBalance>(&mut rng)
                    .expect("bundle builds")
                    .expect("bundle has outputs");
                sign(
                    tb.build().expect("has inputs"),
                    Some(bundle),
                    None,
                    &signing,
                    rng,
                    epoch,
                    TxVersion::V5,
                )
            }
            Kind::Ironwood { actions } => {
                let mut rng = derived_rng(self.tag(), self.index);
                let ironwood = ironwood_outputs(actions, &mut rng);
                sign(
                    tb.build().expect("has inputs"),
                    None,
                    Some(ironwood),
                    &signing,
                    rng,
                    epoch,
                    TxVersion::V6,
                )
            }
            Kind::OrchardNu6_3 => {
                let dest = derive("orchard-nu63-dest", self.index, 0);
                tb.add_output(
                    &TransparentAddress::PublicKeyHash(dest[..20].try_into().expect("20")),
                    Zatoshis::const_from_u64(self.input_value() - self.fee()),
                )
                .expect("valid output");
                let mut rng = derived_rng(self.tag(), self.index);
                let orchard = orchard_nu6_3_padding(&mut rng);
                sign(
                    tb.build().expect("has inputs"),
                    Some(orchard),
                    None,
                    &signing,
                    rng,
                    epoch,
                    TxVersion::V5,
                )
            }
            Kind::OrchardAndIronwood { actions } => {
                let mut rng = derived_rng(self.tag(), self.index);
                let orchard = orchard_nu6_3_padding(&mut rng);
                let ironwood = ironwood_outputs(actions, &mut rng);
                sign(
                    tb.build().expect("has inputs"),
                    Some(orchard),
                    Some(ironwood),
                    &signing,
                    rng,
                    epoch,
                    TxVersion::V6,
                )
            }
        }
    }
}

/// Authorization marker for a transaction whose transparent inputs and Orchard bundle are
/// not yet signed; the Sapling slot is irrelevant because no fixture has a Sapling bundle.
struct Unsigned;

impl Authorization for Unsigned {
    type TransparentAuth = zcash_transparent::builder::Unauthorized;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth =
        orchard::builder::InProgress<orchard::builder::Unproven, orchard::builder::Unauthorized>;
}

type UnprovenOrchard = orchard::Bundle<
    orchard::builder::InProgress<orchard::builder::Unproven, orchard::builder::Unauthorized>,
    ZatBalance,
>;

/// The proving key of `version`, built at the first use.
fn orchard_proving_key(version: OrchardCircuitVersion) -> &'static ProvingKey {
    static NU6_2: OnceLock<ProvingKey> = OnceLock::new();
    static NU6_3: OnceLock<ProvingKey> = OnceLock::new();
    let key = match version {
        OrchardCircuitVersion::FixedPostNu6_2 => &NU6_2,
        OrchardCircuitVersion::PostNu6_3 => &NU6_3,
        OrchardCircuitVersion::InsecurePreNu6_2 => {
            panic!("no fixture has a bundle of the circuit before NU6.2")
        }
    };
    key.get_or_init(|| ProvingKey::build(version))
}

fn orchard_recipient() -> orchard::Address {
    let sk = SpendingKey::from_bytes(derive("orchard-recipient", 0, 0))
        .expect("a SHA-256 output is a valid spending key with overwhelming probability");
    FullViewingKey::from(&sk).address_at(0u32, Scope::External)
}

/// An unproven Ironwood bundle with `actions` outputs, dummy spends and the anchor of the
/// empty tree. The Ironwood pool permits cross-address transfers.
fn ironwood_outputs(actions: usize, rng: &mut StdRng) -> UnprovenOrchard {
    let version = BundleVersion::ironwood_v3();
    let mut builder = OrchardBuilder::new(
        BundleType::DEFAULT,
        version,
        version.default_flags(),
        Anchor::empty_tree(),
    )
    .expect("default flags are representable");
    for _ in 0..actions {
        builder
            .add_output(
                None,
                orchard_recipient(),
                NoteValue::from_raw(ORCHARD_NOTE_VALUE),
                [0u8; 512],
            )
            .expect("outputs enabled");
    }
    let (bundle, _meta) = builder
        .build::<ZatBalance>(rng)
        .expect("bundle builds")
        .expect("bundle has outputs");
    bundle
}

/// An unproven Orchard bundle of NU6.3 with [`ORCHARD_NU6_3_ACTIONS`] padding actions:
/// `enableCrossAddress = 0`, value balance 0, the anchor of the empty tree.
fn orchard_nu6_3_padding(rng: &mut StdRng) -> UnprovenOrchard {
    let version = BundleVersion::orchard_v3();
    let builder = OrchardBuilder::new(
        BundleType::Transactional {
            bundle_required: true,
            pad_to_minimum: Some(ORCHARD_NU6_3_ACTIONS as u8),
        },
        version,
        version.default_flags(),
        Anchor::empty_tree(),
    )
    .expect("default flags are representable");
    let (bundle, _meta) = builder
        .build::<ZatBalance>(rng)
        .expect("bundle builds")
        .expect("a required bundle has padding actions");
    bundle
}

/// A v5 or v6 transaction of `epoch` from its parts. A v5 transaction has no Ironwood
/// bundle.
fn transaction_data<A: Authorization>(
    version: TxVersion,
    epoch: Epoch,
    expiry_height: u32,
    transparent: zcash_transparent::bundle::Bundle<A::TransparentAuth>,
    orchard: Option<orchard::Bundle<A::OrchardAuth, ZatBalance>>,
    ironwood: Option<orchard::Bundle<A::OrchardAuth, ZatBalance>>,
) -> TransactionData<A> {
    let expiry_height = BlockHeight::from_u32(expiry_height);
    match (version, ironwood) {
        (TxVersion::V6, ironwood) => TransactionData::from_parts_v6(
            epoch.branch,
            0,
            expiry_height,
            Some(transparent),
            None,
            orchard,
            ironwood,
        ),
        (TxVersion::V5, None) => TransactionData::from_parts(
            TxVersion::V5,
            epoch.branch,
            0,
            expiry_height,
            Some(transparent),
            None,
            None,
            orchard,
        ),
        (version, _) => panic!("no fixture transaction has the form of {version:?}"),
    }
}

/// Proves `bundle` under the circuit of its bundle version and signs it (dummy spends
/// only) under `sighash`.
fn prove(
    bundle: UnprovenOrchard,
    sighash: [u8; 32],
    rng: &mut StdRng,
) -> orchard::Bundle<orchard::bundle::Authorized, ZatBalance> {
    let key = orchard_proving_key(bundle.bundle_version().circuit_version());
    bundle
        .create_proof(key, &mut *rng)
        .expect("proof")
        .apply_signatures(&mut *rng, sighash, &[])
        .expect("only dummy spends to sign")
}

/// Computes the ZIP 244 (v5) or v6 sighashes of the unsigned transaction, signs the
/// transparent inputs with `signing`, proves and signs the Orchard and Ironwood bundles, and
/// freezes the result.
fn sign(
    transparent: zcash_transparent::bundle::Bundle<zcash_transparent::builder::Unauthorized>,
    orchard: Option<UnprovenOrchard>,
    ironwood: Option<UnprovenOrchard>,
    signing: &TransparentSigningSet,
    mut rng: StdRng,
    epoch: Epoch,
    version: TxVersion,
) -> Transaction {
    let unsigned = transaction_data::<Unsigned>(
        version,
        epoch,
        0,
        transparent.clone(),
        orchard.clone(),
        ironwood.clone(),
    );
    let txid_parts = unsigned.digest(TxIdDigester);
    let transparent = transparent
        .apply_signatures(
            |input| {
                *signature_hash(&unsigned, &SignableInput::Transparent(input), &txid_parts).as_ref()
            },
            signing,
        )
        .expect("all signing keys present");
    let shielded_sighash =
        *signature_hash(&unsigned, &SignableInput::Shielded, &txid_parts).as_ref();
    let orchard = orchard.map(|bundle| prove(bundle, shielded_sighash, &mut rng));
    let ironwood = ironwood.map(|bundle| prove(bundle, shielded_sighash, &mut rng));
    transaction_data::<Authorized>(version, epoch, 0, transparent, orchard, ironwood)
        .freeze()
        .expect("the bundles match the transaction version")
}

/// Authorization marker for a coinbase whose Ironwood bundle is not yet proven.
struct UnsignedCoinbase;

impl Authorization for UnsignedCoinbase {
    type TransparentAuth = Coinbase;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth =
        orchard::builder::InProgress<orchard::builder::Unproven, orchard::builder::Unauthorized>;
}

/// The coinbase terms of a fixture block at `height`: the terms of [`FIXTURE_NETWORK`].
pub fn coinbase_terms(height: u32) -> CoinbaseTerms {
    hayai_consensus::coinbase::terms_at(FIXTURE_NETWORK, height)
        .expect("the fixture heights have a rule set")
}

/// The coinbase of a block of `epoch` that pays the terms of its height and `fees`: a v5
/// transaction in the NU6.2 epoch, a v6 transaction in the NU6.3 epoch. The first output
/// has the miner's part of the subsidy and the fees. The outputs after it are the required
/// outputs of the terms (the funding streams). With `shielded`, the coinbase pays
/// [`COINBASE_NOTE_VALUE`] of the miner's part to one Ironwood output, which decrypts with
/// the zero outgoing viewing key (ZIP 213). The value that the coinbase pays is the exact
/// value of ZIP 236.
fn coinbase(fees: u64, epoch: Epoch, shielded: bool) -> Transaction {
    let version = match epoch.branch {
        BranchId::Nu6_3 => TxVersion::V6,
        _ => TxVersion::V5,
    };
    let shielded_value = if shielded { COINBASE_NOTE_VALUE } else { 0 };
    let terms = coinbase_terms(epoch.height);
    let dest = derive("coinbase-dest", 0, 0);
    let mut tb = TransparentBuilder::empty();
    tb.add_output(
        &TransparentAddress::PublicKeyHash(dest[..20].try_into().expect("20")),
        Zatoshis::const_from_u64(terms.miner_subsidy + fees - shielded_value),
    )
    .expect("valid output");
    for required in &terms.required {
        // A required output pays a P2SH script: `OP_HASH160 <20 bytes> OP_EQUAL`.
        let [0xa9, 0x14, hash @ .., 0x87] = required.script.as_slice() else {
            panic!(
                "a required output pays a P2SH script: {:02x?}",
                required.script
            );
        };
        tb.add_output(
            &TransparentAddress::ScriptHash(hash.try_into().expect("20")),
            Zatoshis::const_from_u64(required.value),
        )
        .expect("valid output");
    }
    let transparent = tb
        .build_coinbase(BlockHeight::from_u32(epoch.height), None)
        .expect("coinbase");
    let ironwood = shielded.then(|| {
        let mut rng = derived_rng("coinbase-ironwood", 0);
        let mut builder = OrchardBuilder::new(
            BundleType::Coinbase,
            BundleVersion::ironwood_v3(),
            Flags::SPENDS_DISABLED,
            Anchor::empty_tree(),
        )
        .expect("the flags of a coinbase are representable");
        builder
            .add_output(
                Some(OutgoingViewingKey::from([0u8; 32])),
                orchard_recipient(),
                NoteValue::from_raw(COINBASE_NOTE_VALUE),
                [0u8; 512],
            )
            .expect("outputs enabled");
        let (bundle, _meta) = builder
            .build::<ZatBalance>(&mut rng)
            .expect("bundle builds")
            .expect("bundle has an output");
        let unsigned = transaction_data::<UnsignedCoinbase>(
            version,
            epoch,
            epoch.height,
            transparent.clone(),
            None,
            Some(bundle.clone()),
        );
        let txid_parts = unsigned.digest(TxIdDigester);
        let sighash = *signature_hash(&unsigned, &SignableInput::Shielded, &txid_parts).as_ref();
        prove(bundle, sighash, &mut rng)
    });
    transaction_data::<Authorized>(
        version,
        epoch,
        epoch.height,
        transparent.map_authorization(Coinbase),
        None,
        ironwood,
    )
    .freeze()
    .expect("the bundles match the transaction version")
}

fn assemble(specs: &[TxSpec], epoch: Epoch, shielded_coinbase: bool) -> Vec<u8> {
    let secp = Secp256k1::new();
    let mut txs: Vec<Transaction> = specs.par_iter().map(|s| s.build(&secp, epoch)).collect();
    let fees: u64 = specs.iter().map(TxSpec::fee).sum();
    txs.insert(0, coinbase(fees, epoch, shielded_coinbase));

    let txids: Vec<_> = txs.iter().map(Transaction::txid).collect();
    let header = BlockHeader {
        version: 4,
        prev_hash: BlockHash(derive("prev-hash", 0, 0)),
        merkle_root: merkle_root(&txids),
        block_commitments: [0u8; 32],
        time: HEADER_TIME,
        bits: HEADER_BITS,
        nonce: [0u8; 32],
        solution: vec![0u8; PowParams::MAINNET.solution_len()],
    };
    let mut out = header.serialize();
    CompactSize::write(&mut out, txs.len()).expect("vec write");
    for tx in &txs {
        tx.write(&mut out).expect("vec write");
    }
    assert!(
        out.len() <= hayai_wire::MAX_BLOCK_BYTES,
        "fixture of {} bytes exceeds the block size limit; reduce its parameters",
        out.len()
    );
    out
}

fn cache_path(name: &str) -> PathBuf {
    fixtures_dir().join(format!("{name}.bin"))
}

fn load_cached(name: &str) -> Option<Vec<u8>> {
    let data = fs::read(cache_path(name)).ok()?;
    let (header, body) = data.split_at_checked(CACHE_MAGIC.len() + 4)?;
    if &header[..8] != CACHE_MAGIC
        || u32::from_le_bytes(header[8..].try_into().expect("4")) != GENERATOR_VERSION
    {
        return None;
    }
    Some(body.to_vec())
}

/// Writes the cache file of a fixture. The temporary name is unique per call (process id
/// and counter), so parallel writers never share a temporary file; the rename replaces the
/// cache file in one step. The file that remains is the one of the last writer, and every
/// caller reads it back instead of using its own bytes (proofs are not deterministic).
fn store_cached(name: &str, bytes: &[u8]) {
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = cache_path(name);
    fs::create_dir_all(path.parent().expect("has parent")).expect("create bench-fixtures");
    let tmp = path.with_extension(format!(
        "bin.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut f = fs::File::create(&tmp).expect("create fixture file");
    f.write_all(CACHE_MAGIC).expect("write");
    f.write_all(&GENERATOR_VERSION.to_le_bytes())
        .expect("write");
    f.write_all(bytes).expect("write");
    drop(f);
    fs::rename(tmp, path).expect("rename fixture file");
}

/// A fixture of the NU6.2 epoch with a transparent coinbase.
fn generate(name: &str, specs: &[TxSpec]) -> Fixture {
    generate_in(name, specs, Epoch::NU6_2, false)
}

fn generate_in(name: &str, specs: &[TxSpec], epoch: Epoch, shielded_coinbase: bool) -> Fixture {
    let secp = Secp256k1::new();
    let funding = specs
        .iter()
        .flat_map(|s| {
            s.funding_inputs(&secp, epoch)
                .into_iter()
                .map(|i| (i.outpoint, i.coin))
        })
        .collect();
    let bytes = match load_cached(name) {
        Some(b) => b,
        None => {
            let started = std::time::Instant::now();
            let b = assemble(specs, epoch, shielded_coinbase);
            store_cached(name, &b);
            let Some(b) = load_cached(name) else {
                panic!("fixture {name}: the cache file is missing or invalid after the write");
            };
            eprintln!(
                "generated fixture {name}: {} bytes, {} transactions, {:.1?}",
                b.len(),
                specs.len() + 1,
                started.elapsed()
            );
            b
        }
    };
    Fixture {
        name: name.to_string(),
        bytes: Bytes::from(bytes),
        height: epoch.height,
        branch_id: epoch.branch,
        funding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use zcash_protocol::consensus::{NetworkUpgrade, Parameters, MAIN_NETWORK};

    #[test]
    fn fixture_epoch_matches_mainnet() {
        assert_eq!(
            BranchId::for_height(&MAIN_NETWORK, BlockHeight::from_u32(FIXTURE_HEIGHT)),
            FIXTURE_BRANCH
        );
        let Some(nu6_3) = MAIN_NETWORK.activation_height(NetworkUpgrade::Nu6_3) else {
            return;
        };
        assert!(nu6_3 > BlockHeight::from_u32(FIXTURE_HEIGHT));
    }

    #[test]
    fn zip317_fee_matches_the_zip() {
        assert_eq!(zip317_fee(1, 2, 0), 10_000);
        assert_eq!(zip317_fee(3, 2, 0), 15_000);
        assert_eq!(zip317_fee(1, 0, 2), 15_000);
        assert_eq!(zip317_fee(1, 0, 4), 25_000);
    }

    fn check_structure(f: &Fixture, expect_orchard: bool, expect_transparent: bool) {
        let block = f.parse();
        assert_eq!(merkle_root(&block.txids()), block.header.merkle_root);
        let cb = &block.txs[0].tx;
        let cb_bundle = cb.transparent_bundle().unwrap();
        assert!(cb_bundle.is_coinbase());
        assert_eq!(cb.expiry_height(), BlockHeight::from_u32(FIXTURE_HEIGHT));
        let fees: u64 = block.txs[1..]
            .iter()
            .map(|t| {
                let b = t.tx.transparent_bundle().unwrap();
                let inputs: u64 = b
                    .vin
                    .iter()
                    .map(|i| {
                        f.funding
                            .iter()
                            .find(|(o, _)| o == i.prevout())
                            .expect("every input is funded")
                            .1
                            .value
                    })
                    .sum();
                let outputs: u64 = b.vout.iter().map(|o| o.value().into_u64()).sum();
                let orchard: i64 =
                    t.tx.orchard_bundle()
                        .map_or(0, |ob| ob.value_balance().into());
                (inputs as i64 - outputs as i64 + orchard) as u64
            })
            .sum();
        // The Mainnet terms of the fixture height: the miner's part with the fees, then the
        // funding stream output.
        let terms = coinbase_terms(FIXTURE_HEIGHT);
        assert_eq!(
            cb_bundle.vout[0].value().into_u64(),
            terms.miner_subsidy + fees,
            "coinbase pays the miner's part of the subsidy plus fees"
        );
        assert_eq!(
            (terms.miner_subsidy, terms.required.len()),
            (125_000_000, 1)
        );
        let outputs: Vec<hayai_consensus::coinbase::CoinbaseOutput> = cb_bundle
            .vout
            .iter()
            .map(|o| hayai_consensus::coinbase::CoinbaseOutput {
                value: o.value().into_u64(),
                script: o.script_pubkey().0 .0.to_vec(),
            })
            .collect();
        assert_eq!(terms.check(&outputs, Default::default(), fees), Ok(()));
        let unique: HashSet<_> = f.funding.iter().map(|(o, _)| o.clone()).collect();
        assert_eq!(
            unique.len(),
            f.funding.len(),
            "funding outpoints are distinct"
        );
        let total_inputs: usize = block.txs[1..]
            .iter()
            .map(|t| t.tx.transparent_bundle().unwrap().vin.len())
            .sum();
        assert_eq!(total_inputs, f.funding.len());
        let has_orchard = block.txs[1..].iter().any(|t| {
            let Some(_) = t.tx.orchard_bundle() else {
                return false;
            };
            true
        });
        assert_eq!(has_orchard, expect_orchard);
        let has_pure_transparent = block.txs[1..].iter().any(|t| {
            let None = t.tx.orchard_bundle() else {
                return false;
            };
            true
        });
        assert_eq!(has_pure_transparent, expect_transparent);
    }

    #[test]
    fn transparent_fixture_is_well_formed_and_reproducible() {
        let f = transparent_block(5, 2);
        check_structure(&f, false, true);
        let block = f.parse();
        assert_eq!(block.txs.len(), 6);
        assert_eq!(block.txs[1].tx.transparent_bundle().unwrap().vin.len(), 2);
        // The signature is a real one: a 65-byte-ish DER signature plus a 33-byte pubkey.
        let script_sig = &block.txs[1].tx.transparent_bundle().unwrap().vin[0].script_sig();
        assert!(script_sig.0 .0.len() > 100);

        let again = assemble(
            &(0..5)
                .map(|i| TxSpec {
                    index: i,
                    kind: Kind::Transparent { inputs: 2 },
                })
                .collect::<Vec<_>>(),
            Epoch::NU6_2,
            false,
        );
        assert_eq!(again, f.bytes, "generation is deterministic");
    }

    /// Transparent authorization that carries the spent coins, which the ZIP 244 sighash of
    /// a transaction with transparent inputs commits to.
    #[derive(Debug)]
    struct Funded {
        amounts: Vec<Zatoshis>,
        scripts: Vec<Script>,
    }

    impl zcash_transparent::bundle::Authorization for Funded {
        type ScriptSig = Script;
    }

    impl zcash_transparent::sighash::TransparentAuthorizingContext for Funded {
        fn input_amounts(&self) -> Vec<Zatoshis> {
            self.amounts.clone()
        }
        fn input_scriptpubkeys(&self) -> Vec<Script> {
            self.scripts.clone()
        }
    }

    impl zcash_transparent::bundle::MapAuth<zcash_transparent::bundle::Authorized, Funded> for Funded {
        fn map_script_sig(&self, s: Script) -> Script {
            s
        }
        fn map_authorization(&self, _: zcash_transparent::bundle::Authorized) -> Funded {
            Funded {
                amounts: self.amounts.clone(),
                scripts: self.scripts.clone(),
            }
        }
    }

    struct FundedAuth;

    impl Authorization for FundedAuth {
        type TransparentAuth = Funded;
        type SaplingAuth = sapling_crypto::bundle::Authorized;
        type OrchardAuth = orchard::bundle::Authorized;
    }

    /// Re-attaches the funding coins to a parsed transaction and computes a sighash.
    fn sighash(
        tx: &Transaction,
        funding: &[(OutPoint, FundingCoin)],
        input: Option<usize>,
    ) -> [u8; 32] {
        let bundle = tx.transparent_bundle().unwrap();
        let coins: Vec<&FundingCoin> = bundle
            .vin
            .iter()
            .map(|i| &funding.iter().find(|(o, _)| o == i.prevout()).unwrap().1)
            .collect();
        let funded = Funded {
            amounts: coins
                .iter()
                .map(|c| Zatoshis::const_from_u64(c.value))
                .collect(),
            scripts: coins.iter().map(|c| c.script_pubkey.clone()).collect(),
        };
        let data = TransactionData::<FundedAuth>::from_parts(
            tx.version(),
            tx.consensus_branch_id(),
            tx.lock_time(),
            tx.expiry_height(),
            Some(bundle.clone().map_authorization(funded)),
            None,
            None,
            tx.orchard_bundle().cloned(),
        );
        let parts = data.digest(TxIdDigester);
        let signable = match input {
            None => SignableInput::Shielded,
            Some(i) => SignableInput::Transparent(
                zcash_transparent::sighash::SignableInput::from_parts(
                    data.transparent_bundle().unwrap(),
                    zcash_transparent::sighash::SighashType::ALL,
                    i,
                    &coins[i].script_pubkey,
                    &coins[i].script_pubkey,
                    Zatoshis::const_from_u64(coins[i].value),
                )
                .unwrap(),
            ),
        };
        *signature_hash(&data, &signable, &parts).as_ref()
    }

    #[test]
    fn transparent_signatures_verify() {
        let f = transparent_block(3, 2);
        let block = f.parse();
        let secp = Secp256k1::verification_only();
        for tx in &block.txs[1..] {
            let bundle = tx.tx.transparent_bundle().unwrap();
            for (i, txin) in bundle.vin.iter().enumerate() {
                // scriptSig = PUSH(sig || hash_type) PUSH(pubkey)
                let bytes = &txin.script_sig().0 .0;
                let sig_len = bytes[0] as usize;
                let sig = &bytes[1..sig_len];
                assert_eq!(bytes[sig_len], 0x01, "SIGHASH_ALL");
                let pk_len = bytes[sig_len + 1] as usize;
                let pk = &bytes[sig_len + 2..sig_len + 2 + pk_len];
                assert_eq!(pk_len, 33);
                let sig = secp256k1::ecdsa::Signature::from_der(sig).unwrap();
                let pk = PublicKey::from_slice(pk).unwrap();
                let msg = secp256k1::Message::from_digest(sighash(&tx.tx, &f.funding, Some(i)));
                secp.verify_ecdsa(&msg, &sig, &pk).unwrap();
                // The pubkey matches the funding coin's P2PKH script.
                let coin = &f
                    .funding
                    .iter()
                    .find(|(o, _)| o == txin.prevout())
                    .unwrap()
                    .1;
                assert_eq!(
                    coin.script_pubkey,
                    TransparentAddress::from_pubkey(&pk).script().into()
                );
            }
        }
    }

    #[test]
    fn orchard_fixture_has_real_bundles() {
        let f = orchard_block(2, 2);
        check_structure(&f, true, false);
        let block = f.parse();
        let bundle = block.txs[1].tx.orchard_bundle().unwrap();
        assert_eq!(bundle.actions().len(), 2);
        assert_eq!(
            i64::from(*bundle.value_balance()),
            -(2 * ORCHARD_NOTE_VALUE as i64)
        );
        // Verify the proofs and signatures with the upstream batch validator.
        let vk =
            orchard::circuit::VerifyingKey::build(BundleVersion::orchard_v2().circuit_version());
        let mut validator = orchard::bundle::BatchValidator::new(&vk);
        for tx in &block.txs[1..] {
            let b = tx.tx.orchard_bundle().unwrap();
            validator
                .add_bundle(b, sighash(&tx.tx, &f.funding, None))
                .unwrap();
        }
        assert!(validator.validate(hayai_crypto::rng::os_rng()));
    }

    #[test]
    fn mixed_fixture_has_both() {
        let f = mixed_block(3, 1, 2, 2);
        check_structure(&f, true, true);
        assert_eq!(f.parse().txs.len(), 6);
    }

    #[test]
    fn parallel_writers_share_the_cache_file() {
        let name = "test-cache-parallel";
        let _ = fs::remove_file(cache_path(name));
        let results: Vec<Vec<u8>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8u8)
                .map(|i| {
                    s.spawn(move || {
                        let bytes = vec![i; 1 << 16];
                        store_cached(name, &bytes);
                        load_cached(name).expect("cache file readable after the write")
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let on_disk = load_cached(name).expect("cache file");
        assert!(results
            .iter()
            .all(|r| r.len() == 1 << 16 && r.iter().all(|b| *b == r[0])));
        assert!(on_disk.iter().all(|b| *b == on_disk[0]));
        fs::remove_file(cache_path(name)).unwrap();
    }

    #[test]
    fn parallel_generation_on_an_empty_cache() {
        let name = "transparent-3x1";
        let _ = fs::remove_file(cache_path(name));
        let blocks: Vec<Fixture> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| transparent_block(3, 1)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(blocks.iter().all(|b| b.parse().txs.len() == 4));
        assert!(load_cached(name).is_some());
        fs::remove_file(cache_path(name)).unwrap();
    }

    #[test]
    fn cache_round_trip_and_version_check() {
        let name = "test-cache-roundtrip";
        let bytes = vec![1u8, 2, 3, 4, 5];
        store_cached(name, &bytes);
        assert_eq!(load_cached(name), Some(bytes));
        // Corrupt the version field.
        let path = cache_path(name);
        let mut data = fs::read(&path).unwrap();
        data[8] ^= 1;
        fs::write(&path, data).unwrap();
        assert_eq!(load_cached(name), None);
        fs::remove_file(path).unwrap();
        assert_eq!(load_cached(name), None);
    }

    /// Generates (or loads) the full benchmark set. Ignored by default because first
    /// generation proves 265 Orchard bundles; run with
    /// `cargo test -p hayai-fixtures --release -- --ignored standard_set`.
    #[test]
    #[ignore = "generates the full fixture set; run in release mode"]
    fn standard_set_parses_and_fits_the_block_limit() {
        let names: Vec<String> = standard_set()
            .iter()
            .map(|f| {
                assert!(f.bytes.len() <= hayai_wire::MAX_BLOCK_BYTES);
                let orchard = !f.name.starts_with("transparent");
                let transparent = !f.name.starts_with("orchard");
                check_structure(f, orchard, transparent);
                f.name.clone()
            })
            .collect();
        assert_eq!(
            names,
            [
                "transparent-1000x2",
                "transparent-6500x1",
                "orchard-165x2",
                "mixed-2000x1-100x2"
            ]
        );
    }
}
