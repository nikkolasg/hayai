//! Sprout end to end.
//!
//! - The embedded Groth16 verifying key and the JoinSplit signature rule on the published
//!   block vectors that hold a v4 transaction with JoinSplits and no transparent input.
//! - The Sprout tree against the published final roots, and against the trees of
//!   `zebra-chain` and `zakura-chain`.
//! - The Sprout state rules on generated blocks, through the contextual check of full
//!   validation and through the checkpoint path. The JoinSplits of these blocks have no
//!   valid proof: the two paths under test read no proof.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness_with_history, Harness};
use hayai_bench::fixtures::{transparent_block, Fixture, FIXTURE_BRANCH};
use hayai_coins::Pool;
use hayai_consensus::{rules_at, Checkpoints, Network};
use hayai_crypto::zcash_primitives::transaction::components::sprout::{Bundle, JsDescription};
use hayai_crypto::zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use hayai_crypto::zcash_protocol::consensus::BlockHeight;
use hayai_crypto::zcash_protocol::value::Zatoshis;
use hayai_crypto::zcash_transparent::address::Script;
use hayai_crypto::zcash_transparent::bundle::{
    Authorized as TAuthorized, Bundle as TBundle, TxOut,
};
use hayai_prepared::{draft, RuleEpoch, ScopedBatch, VerifyingKeys};
use hayai_state::{
    contextual_check, prebuild_body, CheckConfig, ContextError, Layer, PreparedBlock,
};
use hayai_trees::SproutFrontier;
use hayai_validate::{apply_checkpointed, block_commitments, BlockError};
use hayai_wire::{auth_data_root, merkle_root, RawBlock, RawTx};

// ----- published vectors -----

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

/// The block vector `name` (`main-0-419-201`), parsed under the branch of `height`.
fn vector(network: Network, name: &str, height: u32) -> RawBlock {
    let text = std::fs::read_to_string(vectors_dir().join(format!("block-{name}.hex")))
        .expect("the vector exists");
    let bytes = hex::decode(text.trim()).expect("hex");
    let rules = rules_at(network, height).expect("a rule set");
    RawBlock::parse(Bytes::from(bytes), rules.branch_id).expect("the vector parses")
}

/// Size of a JoinSplit description with a Groth16 proof, and the offsets of its fields.
const JOINSPLIT_BYTES: usize = 1698;
const ANCHOR_AT: usize = 16;
const EPHEMERAL_KEY_AT: usize = 176;
const PROOF_AT: usize = 304;

/// The v4 transactions of the vector set that have JoinSplits and no transparent input:
/// the harness needs no coin to verify them. Each JoinSplit has a Groth16 proof.
const JOINSPLIT_VECTORS: [(Network, &str, u32, &[usize]); 3] = [
    (Network::Mainnet, "main-0-419-201", 419_201, &[8]),
    (Network::Mainnet, "main-0-903-000", 903_000, &[13, 14]),
    (Network::Testnet, "test-0-925-483", 925_483, &[4]),
];

/// Whether the JoinSplits (and the Sapling bundle, when the transaction has one) of `tx`
/// pass the shielded verification under `epoch`.
fn verifies(tx: &RawTx, epoch: RuleEpoch, keys: &VerifyingKeys) -> bool {
    let d = draft(tx.clone(), epoch, Vec::new()).expect("the structural rules pass");
    assert!(d.tx().joinsplits > 0 && !d.tx().shielded_ok);
    let mut batch = ScopedBatch::new(keys);
    d.add_shielded(&mut batch).expect("the bundles are queued");
    let outcome = batch.finalize();
    match (&outcome.ok[..], &outcome.failed[..]) {
        ([id], []) if *id == tx.wtxid() => true,
        ([], [id]) if *id == tx.wtxid() => false,
        _ => panic!("the transaction is in one list: {outcome:?}"),
    }
}

/// The key that hayai embeds verifies every published Groth16 JoinSplit of the vector
/// set, with the JoinSplit signature of its transaction. The proofs come from the Sprout
/// parameters of the network, so a key of other parameters does not verify them.
#[test]
fn the_embedded_key_verifies_the_published_joinsplits() {
    let keys = VerifyingKeys::new();
    let mut joinsplits = 0;
    for (network, name, height, positions) in JOINSPLIT_VECTORS {
        let block = vector(network, name, height);
        let epoch = RuleEpoch::of(rules_at(network, height).expect("a rule set"));
        for position in positions {
            let tx = &block.txs[*position];
            let None = tx.tx.transparent_bundle().filter(|b| !b.vin.is_empty()) else {
                panic!("{name} transaction {position} has transparent inputs");
            };
            let bundle = tx.tx.sprout_bundle().expect("JoinSplits");
            joinsplits += bundle.joinsplits.len();
            assert!(verifies(tx, epoch, &keys), "{name} transaction {position}");
        }
    }
    assert_eq!(joinsplits, 5);
}

/// A change to a published transaction fails its verification: a proof byte, the
/// signature, the public key, and a byte that the proof statement does not cover (the
/// signature covers it).
#[test]
fn a_changed_joinsplit_transaction_fails() {
    let (network, name, height, positions) = JOINSPLIT_VECTORS[0];
    let block = vector(network, name, height);
    let rules = rules_at(network, height).expect("a rule set");
    let epoch = RuleEpoch::of(rules);
    let tx = &block.txs[positions[0]];
    let bundle = tx.tx.sprout_bundle().expect("JoinSplits");
    let None = tx.tx.sapling_bundle() else {
        panic!("the layout below is the layout of a transaction without a Sapling bundle");
    };
    // The transaction ends with its JoinSplits, the public key and the signature.
    let key_at = tx.bytes.len() - 64 - 32;
    let first = key_at - bundle.joinsplits.len() * JOINSPLIT_BYTES;
    assert_eq!(
        tx.bytes[first + ANCHOR_AT..first + ANCHOR_AT + 32],
        bundle.joinsplits[0].anchor()[..]
    );
    assert_eq!(tx.bytes[key_at..key_at + 32], bundle.joinsplit_pubkey[..]);

    let keys = VerifyingKeys::new();
    assert!(verifies(tx, epoch, &keys));
    for (what, at) in [
        ("a proof byte", first + PROOF_AT + 7),
        ("the ephemeral key", first + EPHEMERAL_KEY_AT),
        ("the last ciphertext byte", first + JOINSPLIT_BYTES - 1),
        ("the public key", key_at + 3),
        ("the signature", key_at + 32 + 40),
    ] {
        let mut bytes = tx.bytes.to_vec();
        bytes[at] ^= 1;
        let changed = RawTx::parse(Bytes::from(bytes), rules.branch_id).expect("parses");
        assert_ne!(changed.wtxid(), tx.wtxid());
        assert!(!verifies(&changed, epoch, &keys), "{what}");
    }
}

/// The published final Sprout roots (`final-roots.json`, in the byte order that
/// `zcash-cli` prints), by network name and height.
fn published_sprout_roots() -> BTreeMap<(String, u32), [u8; 32]> {
    let text =
        std::fs::read_to_string(vectors_dir().join("final-roots.json")).expect("final roots");
    let by_network: BTreeMap<String, Vec<serde_json::Value>> =
        serde_json::from_str(&text).expect("json");
    let mut roots = BTreeMap::new();
    for (network, entries) in by_network {
        for entry in entries {
            let Some(root) = entry["sprout"].as_str() else {
                continue;
            };
            let mut root: [u8; 32] = hex::decode(root).expect("hex").try_into().expect("32");
            root.reverse();
            let height = u32::try_from(entry["height"].as_u64().expect("height")).expect("u32");
            roots.insert((network.clone(), height), root);
        }
    }
    roots
}

/// The first block with a JoinSplit on each network (Mainnet 396, Testnet 2,259): its
/// commitments on the empty tree give the published final Sprout root of the block. The
/// published root of the genesis block is the root of the empty tree.
#[test]
fn the_sprout_tree_gives_the_published_final_roots() {
    let roots = published_sprout_roots();
    assert_eq!(roots.len(), 7);
    for (network, net, name, height) in [
        (Network::Mainnet, "main", "main-0-000-396", 396),
        (Network::Testnet, "test", "test-0-002-259", 2_259),
    ] {
        let mut tree = SproutFrontier::empty();
        assert_eq!(tree.root(), roots[&(net.to_string(), 0)]);
        let block = vector(network, name, height);
        let commitments: Vec<[u8; 32]> = block
            .txs
            .iter()
            .filter_map(|t| t.tx.sprout_bundle())
            .flat_map(|b| &b.joinsplits)
            .flat_map(|j| *j.commitments())
            .collect();
        assert!(!commitments.is_empty());
        let root = tree.append_many(&commitments).expect("room");
        assert_eq!(root, roots[&(net.to_string(), height)], "{name}");
    }
}

/// The Sprout frontier of hayai, the tree of `zebra-chain` and the tree of `zakura-chain`
/// have the same root after each of 300 random commitments.
#[cfg(feature = "baselines")]
#[test]
fn the_sprout_tree_equals_the_zebra_and_the_zakura_tree() {
    use hayai_crypto::rng::{seeded, RngCore};
    let mut rng = seeded(11);
    let mut tree = SproutFrontier::empty();
    let mut zebra = zb_chain::sprout::tree::NoteCommitmentTree::default();
    let mut zakura = zk_chain::sprout::tree::NoteCommitmentTree::default();
    assert_eq!(tree.root(), <[u8; 32]>::from(zebra.root()));
    assert_eq!(tree.root(), <[u8; 32]>::from(zakura.root()));
    for _ in 0..300 {
        let mut commitment = [0u8; 32];
        rng.fill_bytes(&mut commitment);
        let root = tree.append_many(&[commitment]).expect("room");
        zebra
            .append(zb_chain::sprout::NoteCommitment::from(commitment))
            .expect("room");
        zakura
            .append(zk_chain::sprout::NoteCommitment::from(commitment))
            .expect("room");
        assert_eq!(root, <[u8; 32]>::from(zebra.root()));
        assert_eq!(root, <[u8; 32]>::from(zakura.root()));
    }
}

// ----- state rules on generated blocks -----

/// One JoinSplit of a generated transaction: its anchor, a tag that gives its nullifiers
/// and its commitments, and the value that it takes out of the Sprout pool.
#[derive(Clone, Copy)]
struct Js {
    anchor: [u8; 32],
    tag: u8,
    vpub_new: u64,
}

fn js(anchor: [u8; 32], tag: u8) -> Js {
    Js {
        anchor,
        tag,
        vpub_new: 0,
    }
}

/// Nullifier (`part` 1, 2) or commitment (`part` 3, 4) of the JoinSplit with `tag`.
fn part(tag: u8, part: u8) -> [u8; 32] {
    let mut value = [tag; 32];
    value[0] = part;
    value
}

fn commitments(tag: u8) -> [[u8; 32]; 2] {
    [part(tag, 3), part(tag, 4)]
}

fn nullifiers(tag: u8) -> [[u8; 32]; 2] {
    [part(tag, 1), part(tag, 2)]
}

/// `tree` after the commitments of the JoinSplits with `tags`.
fn tree_after(tree: &SproutFrontier, tags: &[u8]) -> SproutFrontier {
    let mut tree = tree.clone();
    for tag in tags {
        tree.append_many(&commitments(*tag)).expect("room");
    }
    tree
}

fn joinsplit(js: &Js) -> JsDescription {
    let mut bytes = Vec::with_capacity(JOINSPLIT_BYTES);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&js.vpub_new.to_le_bytes());
    bytes.extend_from_slice(&js.anchor);
    for value in nullifiers(js.tag).iter().chain(&commitments(js.tag)) {
        bytes.extend_from_slice(value);
    }
    bytes.resize(JOINSPLIT_BYTES, 0);
    JsDescription::read(&bytes[..], true).expect("the JoinSplit parses")
}

/// A v4 transaction with `joinsplits` and no transparent input. It pays every `vpub_new`
/// to one transparent output, so its fee is zero.
fn joinsplit_tx(joinsplits: &[Js]) -> RawTx {
    let value: u64 = joinsplits.iter().map(|j| j.vpub_new).sum();
    let transparent = (value > 0).then(|| TBundle {
        vin: Vec::new(),
        vout: vec![TxOut::new(
            Zatoshis::const_from_u64(value),
            Script::default(),
        )],
        authorization: TAuthorized,
    });
    let sprout = Bundle {
        joinsplits: joinsplits.iter().map(joinsplit).collect(),
        joinsplit_pubkey: [3; 32],
        joinsplit_sig: [4; 64],
    };
    let tx = TransactionData::<Authorized>::from_parts(
        TxVersion::V4,
        FIXTURE_BRANCH,
        0,
        BlockHeight::from_u32(0),
        transparent,
        Some(sprout),
        None,
        None,
    )
    .freeze()
    .expect("a v4 transaction");
    let mut bytes = Vec::new();
    tx.write(&mut bytes).expect("vec write");
    RawTx::parse(Bytes::from(bytes), FIXTURE_BRANCH).expect("round trip")
}

/// A block with only a coinbase, the base of every generated block of these tests.
fn coinbase_only() -> Fixture {
    transparent_block(0, 1)
}

/// The next block of the chain of `h`: the coinbase of `fixture`, then `txs`. The header
/// has the merkle root of the body and commits to the history tree of the parent.
fn next_block(h: &Harness, fixture: &Fixture, txs: Vec<RawTx>) -> RawBlock {
    let view = h.chain.view();
    let tip = view.tip();
    let mut raw = fixture.at(tip.height + 1, tip.hash);
    raw.txs.extend(txs);
    raw.header.merkle_root = merkle_root(&raw.txids());
    let history = view.history().expect("the chain has a history tree");
    raw.header.block_commitments =
        block_commitments(&history.root(), &auth_data_root(&raw.auth_digests()));
    raw
}

/// The contextual check of full validation on `raw`. No transaction of `raw` has a
/// transparent input.
fn full(h: &Harness, raw: &RawBlock) -> Result<Layer, ContextError> {
    let txs = raw
        .txs
        .iter()
        .map(|t| {
            let d = draft(t.clone(), h.cfg.epoch(), Vec::new()).expect("the draft rules pass");
            d.shared().clone()
        })
        .collect();
    let block = PreparedBlock::new(raw.clone(), txs);
    let cfg = CheckConfig {
        network: h.cfg.network,
        rules: &h.cfg.rules,
    };
    contextual_check(&h.chain.view(), &block, &cfg).map(|checked| checked.layer)
}

/// The checkpoint path on `raw`, with `raw` as the checkpoint of its height.
fn checkpointed(h: &Harness, raw: &RawBlock) -> Result<Layer, ContextError> {
    let view = h.chain.view();
    let checkpoints =
        Checkpoints::new(vec![(view.tip_height() + 1, raw.hash())]).expect("one checkpoint");
    match apply_checkpointed(raw, raw.hash(), &view, &h.cfg, &checkpoints) {
        Ok((layer, _)) => Ok(layer),
        Err(BlockError::Context(e)) => Err(e),
        Err(e) => panic!("the checkpoint path stops before the state update: {e}"),
    }
}

/// The layer of `raw` from both paths, which must agree on the Sprout state.
fn both(h: &Harness, raw: &RawBlock) -> Layer {
    let layer = full(h, raw).expect("full validation accepts the block");
    let fast = checkpointed(h, raw).expect("the checkpoint path accepts the block");
    assert_eq!(layer.sprout_frontier, fast.sprout_frontier);
    for pool in Pool::ALL {
        assert_eq!(
            layer.nullifiers[pool.index()],
            fast.nullifiers[pool.index()]
        );
    }
    assert_eq!(layer.value_pools, fast.value_pools);
    assert_eq!(layer.created, fast.created);
    assert_eq!(layer.history_root(), fast.history_root());
    layer
}

fn bad_sprout_anchor(tx: usize) -> ContextError {
    ContextError::BadAnchor {
        pool: Pool::Sprout,
        tx,
    }
}

/// A block with JoinSplits appends their commitments to the Sprout tree in block order
/// and reveals their nullifiers, on both paths. The final treestate of a block is a valid
/// anchor of a later block, from a layer and from the base.
#[test]
fn a_joinsplit_block_updates_the_sprout_state_on_both_paths() {
    let fixture = coinbase_only();
    let mut h = harness_with_history(&fixture);
    let empty = SproutFrontier::empty();
    // The second JoinSplit of the first transaction continues the output treestate of
    // the first one.
    let interstitial = tree_after(&empty, &[1]);
    let first = next_block(
        &h,
        &fixture,
        vec![
            joinsplit_tx(&[js(empty.root(), 1), js(interstitial.root(), 2)]),
            joinsplit_tx(&[js(empty.root(), 3)]),
        ],
    );
    let layer = both(&h, &first);
    let after_first = tree_after(&empty, &[1, 2, 3]);
    assert_eq!(*layer.sprout_frontier, after_first);
    let revealed: Vec<[u8; 32]> = [1, 2, 3].into_iter().flat_map(nullifiers).collect();
    assert_eq!(layer.nullifiers[Pool::Sprout.index()].len(), 6);
    for nullifier in &revealed {
        assert!(layer.nullifiers[Pool::Sprout.index()].contains(nullifier));
    }
    h.chain.push(layer).expect("the next layer");
    assert_eq!(
        h.chain
            .view()
            .contains_nullifier_many(Pool::Sprout, &revealed),
        [true; 6]
    );

    // The next block: the final treestate of the first block and the empty tree.
    let second = next_block(
        &h,
        &fixture,
        vec![joinsplit_tx(&[
            js(after_first.root(), 4),
            js(empty.root(), 5),
        ])],
    );
    let layer = both(&h, &second);
    // The block tree continues the tip tree, whatever the anchors are.
    let after_second = tree_after(&after_first, &[4, 5]);
    assert_eq!(*layer.sprout_frontier, after_second);
    h.chain.push(layer).expect("the next layer");

    // A block without a JoinSplit keeps the tree.
    let third = next_block(&h, &fixture, Vec::new());
    let layer = both(&h, &third);
    assert_eq!(*layer.sprout_frontier, after_second);
    h.chain.push(layer).expect("the next layer");

    // The treestates stay valid anchors in the base.
    h.chain.finalize_excess(0).expect("finalize");
    let fourth = next_block(
        &h,
        &fixture,
        vec![joinsplit_tx(&[
            js(after_first.root(), 6),
            js(after_second.root(), 7),
        ])],
    );
    let layer = both(&h, &fourth);
    assert_eq!(*layer.sprout_frontier, tree_after(&after_second, &[6, 7]));
}

/// A body with JoinSplits that is prebuilt before its block exists commits to the layer
/// of the contextual check: the Sprout tree, the nullifiers and the pool are part of the
/// prebuilt state. A wrong Sprout anchor fails the prebuild.
#[test]
fn a_prebuilt_body_holds_the_sprout_state() {
    let fixture = coinbase_only();
    let h = harness_with_history(&fixture);
    let empty = SproutFrontier::empty();
    h.chain.base().write().value_pools.sprout = 9;
    let withdrawal = Js {
        anchor: empty.root(),
        tag: 2,
        vpub_new: 9,
    };
    let raw = next_block(
        &h,
        &fixture,
        vec![
            joinsplit_tx(&[js(empty.root(), 1)]),
            joinsplit_tx(&[withdrawal]),
        ],
    );
    let layer = both(&h, &raw);
    let view = h.chain.view();
    let prepared: Vec<_> = raw
        .txs
        .iter()
        .map(|t| {
            let d = draft(t.clone(), h.cfg.epoch(), Vec::new()).expect("the draft rules pass");
            d.shared().clone()
        })
        .collect();
    let cfg = CheckConfig {
        network: h.cfg.network,
        rules: &h.cfg.rules,
    };
    let body = prebuild_body(&view, &prepared[1..], &cfg).expect("the body is valid");
    let committed = body
        .commit(&view, &raw, &prepared[0], &cfg)
        .expect("the block has the prebuilt body")
        .layer;
    assert_eq!(*committed.sprout_frontier, tree_after(&empty, &[1, 2]));
    assert_eq!(committed.sprout_frontier, layer.sprout_frontier);
    assert_eq!(committed.nullifiers, layer.nullifiers);
    assert_eq!(committed.value_pools, layer.value_pools);
    assert_eq!(committed.value_pools.sprout, 0);

    let wrong = draft(joinsplit_tx(&[js([9; 32], 3)]), h.cfg.epoch(), Vec::new())
        .expect("the draft rules pass")
        .shared()
        .clone();
    let Err(error) = prebuild_body(&view, &[wrong], &cfg).map(|_| ()) else {
        panic!("a wrong Sprout anchor fails the prebuild");
    };
    assert_eq!(error, bad_sprout_anchor(1));
}

/// The anchor of a JoinSplit is the final Sprout treestate of an earlier block, or the
/// output treestate of an earlier JoinSplit of the same transaction. Every other root is
/// an error of full validation. The checkpoint path does not apply the anchor rule.
#[test]
fn a_sprout_anchor_is_an_earlier_final_treestate_or_an_interstitial_treestate() {
    let fixture = coinbase_only();
    let mut h = harness_with_history(&fixture);
    let empty = SproutFrontier::empty();
    let after_one = tree_after(&empty, &[1]);
    type Case = (&'static str, Vec<Vec<Js>>, usize);
    let cases: [Case; 4] = [
        ("a root that is no treestate", vec![vec![js([9; 32], 1)]], 1),
        (
            "the output treestate of a JoinSplit of another transaction of the block",
            vec![vec![js(empty.root(), 1)], vec![js(after_one.root(), 2)]],
            2,
        ),
        (
            "the output treestate of a later JoinSplit of the transaction",
            vec![vec![
                js(tree_after(&empty, &[2]).root(), 1),
                js(empty.root(), 2),
            ]],
            1,
        ),
        (
            "the output treestate of the JoinSplit itself",
            vec![vec![js(after_one.root(), 1)]],
            1,
        ),
    ];
    for (what, txs, position) in cases {
        let txs = txs
            .iter()
            .map(|joinsplits| joinsplit_tx(joinsplits))
            .collect();
        let raw = next_block(&h, &fixture, txs);
        assert_eq!(
            full(&h, &raw).map(|_| ()),
            Err(bad_sprout_anchor(position)),
            "{what}"
        );
        checkpointed(&h, &raw).expect("the checkpoint path does not apply the anchor rule");
    }

    // The treestate inside a transaction of an earlier block is not a final treestate.
    let first = next_block(
        &h,
        &fixture,
        vec![joinsplit_tx(&[
            js(empty.root(), 1),
            js(after_one.root(), 2),
        ])],
    );
    let layer = both(&h, &first);
    h.chain.push(layer).expect("the next layer");
    let raw = next_block(&h, &fixture, vec![joinsplit_tx(&[js(after_one.root(), 3)])]);
    assert_eq!(full(&h, &raw).map(|_| ()), Err(bad_sprout_anchor(1)));
    let after_block = tree_after(&empty, &[1, 2]);
    let raw = next_block(
        &h,
        &fixture,
        vec![joinsplit_tx(&[js(after_block.root(), 3)])],
    );
    both(&h, &raw);
}

/// A Sprout nullifier is revealed once: in a block (both paths) and in the chain (full
/// validation).
#[test]
fn a_sprout_nullifier_is_revealed_once() {
    let fixture = coinbase_only();
    let mut h = harness_with_history(&fixture);
    let empty = SproutFrontier::empty().root();
    let duplicate = |tx: usize| ContextError::DuplicateNullifier {
        pool: Pool::Sprout,
        tx,
    };
    let raw = next_block(
        &h,
        &fixture,
        vec![
            joinsplit_tx(&[js(empty, 1)]),
            joinsplit_tx(&[js(empty, 2)]),
            joinsplit_tx(&[js(empty, 4), js(empty, 1)]),
        ],
    );
    assert_eq!(full(&h, &raw).map(|_| ()), Err(duplicate(3)));
    assert_eq!(checkpointed(&h, &raw).map(|_| ()), Err(duplicate(3)));

    let first = next_block(&h, &fixture, vec![joinsplit_tx(&[js(empty, 1)])]);
    let layer = both(&h, &first);
    h.chain.push(layer).expect("the next layer");
    let again = next_block(
        &h,
        &fixture,
        vec![joinsplit_tx(&[js(empty, 2)]), joinsplit_tx(&[js(empty, 1)])],
    );
    assert_eq!(full(&h, &again).map(|_| ()), Err(duplicate(2)));
    // The same nullifiers in the base.
    h.chain.finalize_excess(0).expect("finalize");
    assert_eq!(full(&h, &again).map(|_| ()), Err(duplicate(2)));
}

/// `vpub_new` leaves the Sprout pool. The pool at exactly zero is valid, and one zatoshi
/// more is an error on both paths.
#[test]
fn the_sprout_pool_is_never_negative() {
    let fixture = coinbase_only();
    let h = harness_with_history(&fixture);
    let empty = SproutFrontier::empty().root();
    h.chain.base().write().value_pools.sprout = 5;
    let before = h.chain.view().value_pools();
    let withdraw = |value: u64| {
        let tx = joinsplit_tx(&[Js {
            anchor: empty,
            tag: 1,
            vpub_new: value,
        }]);
        next_block(&h, &fixture, vec![tx])
    };
    let layer = both(&h, &withdraw(5));
    assert_eq!(layer.value_pools.sprout, 0);
    // The value is now in a transparent output. The coinbase adds its outputs.
    let coinbase = both(&h, &next_block(&h, &fixture, Vec::new()));
    assert_eq!(
        layer.value_pools.transparent,
        coinbase.value_pools.transparent + 5
    );
    assert_eq!(coinbase.value_pools.sprout, before.sprout);

    let negative = ContextError::NegativeValuePool(Pool::Sprout);
    let raw = withdraw(6);
    assert_eq!(full(&h, &raw).map(|_| ()), Err(negative.clone()));
    assert_eq!(checkpointed(&h, &raw).map(|_| ()), Err(negative));
}

/// A base that does not know the Sprout state accepts a block without a JoinSplit and
/// refuses a block with one, on both paths. It never takes the empty tree as the state.
#[test]
fn a_base_without_the_sprout_state_refuses_a_joinsplit() {
    let fixture = coinbase_only();
    let h = harness_with_history(&fixture);
    h.chain.base().write().set_sprout_unknown();
    both(&h, &next_block(&h, &fixture, Vec::new()));
    let empty = SproutFrontier::empty().root();
    let raw = next_block(&h, &fixture, vec![joinsplit_tx(&[js(empty, 1)])]);
    let unknown = ContextError::SproutStateUnknown { tx: 1 };
    assert_eq!(full(&h, &raw).map(|_| ()), Err(unknown.clone()));
    assert_eq!(checkpointed(&h, &raw).map(|_| ()), Err(unknown));
}
