//! The speculative tip end to end: `build_layer` publishes a layer before `verify`, the
//! template moves to it, the next block (built from that template) validates on the
//! speculative view, and the verdicts commit or reject the layers.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{harness_with_history, Harness};
use hayai_coins::{CoinsView, OutPoint, Pool};
use hayai_fixtures::{transparent_block, FIXTURE_BRANCH};
use hayai_prepared::PrepareError;
use hayai_state::{Chain, ChainView, Layer};
use hayai_template::messages::{Hash32, HexBytes, Submit};
use hayai_template::{
    Candidate, CoinbaseSpec, LiveTemplate, TemplateConfig, TemplateUpdate, Tip, Zip317Params,
};
use hayai_validate::{block_commitments, build_layer, validate_block, verify, BlockError};
use hayai_wire::{auth_data_root, RawBlock, WtxId};

fn template(h: &Harness) -> LiveTemplate {
    let spec = CoinbaseSpec {
        script_pubkey: vec![0x51],
        miner_data: b"speculative".to_vec(),
        network: h.cfg.network,
    };
    let mut live = LiveTemplate::new(TemplateConfig::new(spec));
    let candidates: Vec<Candidate> = h
        .prepare_all()
        .iter()
        .map(|p| Candidate::from_raw(&p.raw, p.fee, p.sigops, Vec::new(), &Zip317Params::ZAKURA))
        .collect();
    live.load(candidates).unwrap();
    live
}

/// The template tip on top of `view`'s tip, with the history root of that tip.
fn tip_of(view: &ChainView, time: u32) -> Tip {
    let tip = view.tip();
    Tip {
        parent_hash: tip.hash,
        height: tip.height + 1,
        time,
        median_time_past: time - 1,
        bits: 0x1c01_0000,
        history_root: view.history().expect("the history tree is known").root(),
        issued_supply: Some(view.value_pools().total()),
    }
}

/// The block a pool mines from the live template (nonce and solution are not checked by
/// validation).
fn mine(live: &LiveTemplate) -> RawBlock {
    let current = live.current().unwrap();
    let rebuilt = live
        .rebuild_submission(&Submit {
            template_id: current.id,
            time: current.tip.time,
            nonce: Hash32([0; 32]),
            solution: HexBytes(Bytes::from(vec![0u8; 1344])),
            coinbase: None,
        })
        .unwrap();
    RawBlock::parse(rebuilt.bytes, FIXTURE_BRANCH).unwrap()
}

/// Every outpoint and Orchard nullifier that `layers` mention.
fn keys(layers: &[Arc<Layer>]) -> (Vec<OutPoint>, Vec<[u8; 32]>) {
    let mut outpoints = Vec::new();
    let mut nullifiers = Vec::new();
    for layer in layers {
        outpoints.extend(layer.created.keys().cloned());
        outpoints.extend(layer.spent.iter().cloned());
        nullifiers.extend(layer.nullifiers[Pool::Orchard.index()].iter().copied());
    }
    (outpoints, nullifiers)
}

/// The view answers as a fresh walk of its layers does, for every key of `layers`.
fn index_equals_walk(chain: &Chain, layers: &[Arc<Layer>]) {
    let (outpoints, nullifiers) = keys(layers);
    for view in [chain.view(), chain.view_speculative()] {
        assert_eq!(
            view.get_coins(&outpoints),
            view.get_coins_by_walk(&outpoints)
        );
        assert_eq!(
            view.contains_nullifier_many(Pool::Orchard, &nullifiers),
            view.contains_nullifier_many_by_walk(Pool::Orchard, &nullifiers)
        );
    }
}

/// A block whose scripts fail after its layer went out as a speculative tip: the layer and
/// the speculative block built on it are rejected, the template returns to the parent with
/// the block's transactions, and the views agree with a fresh walk.
#[test]
fn a_failed_verification_rejects_the_layer_and_its_descendants() {
    let fixture = transparent_block(5, 2);
    let mut h = harness_with_history(&fixture);
    let mut live = template(&h);
    let parent_tip = tip_of(&h.chain.view(), h.block.header.time + 1);
    live.on_tip(parent_tip, &[], &[], |_| {}).unwrap();
    let before: Vec<WtxId> = live
        .current()
        .unwrap()
        .txs
        .iter()
        .map(|c| c.wtxid)
        .collect();
    assert_eq!(before.len(), 5);

    // A signature byte of the first transaction: v5 txids do not cover signatures, so the
    // merkle root stays; the miner commits to the new auth data root.
    let mut bytes = fixture.bytes.to_vec();
    let script_sig = &h.block.txs[1].tx.transparent_bundle().unwrap().vin[0]
        .script_sig()
        .0
         .0;
    let at = bytes
        .windows(script_sig.len())
        .position(|w| w == &script_sig[..])
        .unwrap()
        + 5;
    bytes[at] ^= 0x01;
    let mut bad = RawBlock::parse(Bytes::from(bytes), fixture.branch_id).unwrap();
    assert_eq!(
        bad.header.merkle_root,
        hayai_wire::merkle_root(&bad.txids())
    );
    bad.header.block_commitments = block_commitments(
        &h.chain.view().history().unwrap().root(),
        &auth_data_root(&bad.auth_digests()),
    );

    let chain = &mut h.chain;
    let (layer_a, verification_a, _) =
        build_layer(bad, &h.store, &chain.view(), &h.cfg).expect("context rules pass");
    // The template's first transaction has the block's txid but another wtxid: it leaves
    // as a conflict (it spends what the block spends), the others as mined.
    let conflicting: Vec<WtxId> = live
        .selection()
        .filter(|c| c.spends.iter().any(|o| layer_a.spent.contains(o)))
        .map(|c| c.wtxid)
        .collect();
    assert_eq!(conflicting, before);
    assert!(!conflicting.contains(&layer_a.wtxids[1]));
    let hash_a = layer_a.hash;
    let id_a = chain.push_speculative(layer_a).unwrap();
    live.on_speculative_tip(
        tip_of(&chain.view_speculative(), parent_tip.time + 1),
        &[],
        &conflicting,
        |_| {},
    )
    .unwrap();
    assert!(live.current().unwrap().txs.is_empty());

    // The pool mines block B on the speculative tip; B validates on the speculative view,
    // including its commitment to A's history tree.
    let block_b = mine(&live);
    let (layer_b, verification_b, _) =
        build_layer(block_b, &h.store, &chain.view_speculative(), &h.cfg).unwrap();
    let id_b = chain.push_speculative(layer_b).unwrap();
    live.on_speculative_tip(
        tip_of(&chain.view_speculative(), parent_tip.time + 2),
        &[],
        &[],
        |_| {},
    )
    .unwrap();
    let speculative: Vec<Arc<Layer>> = chain.speculative().map(|(_, l)| l.clone()).collect();
    index_equals_walk(chain, &speculative);

    // B verifies first and waits for A; A fails.
    verify(verification_b).unwrap();
    assert!(chain.confirm(id_b).unwrap().is_empty());
    let Err(BlockError::Prepare {
        tx: 1,
        error: PrepareError::Script(0, _),
    }) = verify(verification_a)
    else {
        panic!("the bad signature fails verification");
    };
    let dropped = chain.reject(id_a).unwrap();
    assert_eq!(dropped.len(), 2);
    assert_eq!(chain.speculative().count(), 0);
    assert_eq!(chain.view_speculative().tip(), chain.view().tip());
    index_equals_walk(chain, &dropped);

    let mut updates = Vec::new();
    live.on_revert(parent_tip, |u| updates.push(u)).unwrap();
    let [TemplateUpdate::Reverted { rejected, template }] = updates.as_slice() else {
        panic!("a revert emits Reverted, got {updates:?}");
    };
    assert_eq!(*rejected, hash_a);
    assert_eq!(template.tip, parent_tip);
    let after: Vec<WtxId> = template.txs.iter().map(|c| c.wtxid).collect();
    assert_eq!(after, before);
}

/// Two speculative blocks: the second validates on the first's layer, verifies first, and
/// both commit in order once the first verifies. The committed layers equal the layers of
/// a one-call validation.
#[test]
fn a_two_block_speculative_chain_commits_in_order() {
    let fixture = transparent_block(5, 2);
    let mut h = harness_with_history(&fixture);
    let (expected, _) = validate_block(h.block.clone(), &h.store, &h.chain.view(), &h.cfg).unwrap();
    let mut live = template(&h);
    let parent_tip = tip_of(&h.chain.view(), h.block.header.time + 1);
    live.on_tip(parent_tip, &[], &[], |_| {}).unwrap();

    let chain = &mut h.chain;
    let (layer_a, verification_a, timings) =
        build_layer(h.block.clone(), &h.store, &chain.view(), &h.cfg).unwrap();
    assert_eq!(timings.scripts, std::time::Duration::ZERO);
    assert_eq!(layer_a.history_root(), expected.history_root());
    let hash_a = layer_a.hash;
    let mined: Vec<WtxId> = layer_a.wtxids[1..].to_vec();
    let id_a = chain.push_speculative(layer_a).unwrap();
    live.on_speculative_tip(
        tip_of(&chain.view_speculative(), parent_tip.time + 1),
        &mined,
        &[],
        |_| {},
    )
    .unwrap();

    // Verification of A runs on another thread while B is built on the speculative tip.
    std::thread::scope(|s| {
        let verifying = s.spawn(|| verify(verification_a));
        let block_b = mine(&live);
        let (layer_b, verification_b, _) =
            build_layer(block_b, &h.store, &chain.view_speculative(), &h.cfg).unwrap();
        let hash_b = layer_b.hash;
        let id_b = chain.push_speculative(layer_b).unwrap();
        verify(verification_b).unwrap();
        assert!(chain.confirm(id_b).unwrap().is_empty(), "B waits for A");
        verifying.join().unwrap().unwrap();
        let committed = chain.confirm(id_a).unwrap();
        let hashes: Vec<_> = committed.iter().map(|l| l.hash).collect();
        assert_eq!(hashes, vec![hash_a, hash_b]);
        live.on_confirm(hash_a).unwrap();
        let committed_layers: Vec<Arc<Layer>> = chain.layers().cloned().collect();
        index_equals_walk(chain, &committed_layers);
        assert_eq!(chain.tip().hash, hash_b);
        let a = &committed_layers[0];
        assert_eq!(a.created, expected.created);
        assert_eq!(a.spent, expected.spent);
        // The history tree of B extends A's by one leaf.
        let b = &committed_layers[1];
        assert_eq!(
            b.history.as_ref().unwrap().last_height(),
            Some(u64::from(fixture.height) + 1)
        );
        // A coin A created is visible through the committed view.
        let (created, _) = a.created.iter().next().unwrap();
        let Some(_) = chain.view().get_coin(created) else {
            panic!("A's coins are committed");
        };
    });
}
