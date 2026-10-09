//! NU6.3 (Ironwood) end to end, on generated blocks with real proofs under the NU6.3
//! circuit: v6 transactions with Ironwood bundles, v5 and v6 transactions with an Orchard
//! bundle of NU6.3, a shielded (Ironwood) coinbase. A valid block validates cold and warm
//! to identical layers, and each rule rejects its violation.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{chain_with_layers, harness, harness_with_history, keys, Harness};
use hayai_bench::zakura_chain_clone::BlockShape;
use hayai_coins::Pool;
use hayai_consensus::coinbase::{CoinbaseError, CoinbaseOutput, ShieldedBalances};
use hayai_consensus::{BlockLimits, RuleSet};
use hayai_crypto::incrementalmerkletree::frontier::Frontier;
use hayai_crypto::orchard::tree::MerkleHashOrchard;
use hayai_crypto::orchard::Anchor;
use hayai_crypto::zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use hayai_fixtures::{nu6_3_block, Fixture, NU6_3_FIXTURE_BRANCH, NU6_3_FIXTURE_HEIGHT};
use hayai_prepared::{draft, PrepareError, PreparedTx};
use hayai_state::history::{header_commitment, HeaderCommitment};
use hayai_state::{contextual_check, CheckConfig, ContextError, HistoryLeaf, Layer, PreparedBlock};
use hayai_trees::IronwoodFrontier;
use hayai_validate::{
    block_commitments, build_layer, commit_prebuilt, prebuild, validate_block, validate_bytes,
    verify, BlockError, CommitError,
};
use hayai_wire::{auth_data_root, merkle_root, RawBlock, RawTx};

const BRANCH: hayai_crypto::zcash_protocol::consensus::BranchId = NU6_3_FIXTURE_BRANCH;
/// Bytes of one action on the wire.
const ACTION_BYTES: usize = 5 * 32 + 580 + 80;
const SPENDS: u8 = 0b001;
const OUTPUTS: u8 = 0b010;

/// Two Ironwood transactions (positions 1 and 2), one v5 Orchard transaction (position 3),
/// one v6 transaction with both bundles (position 4), and a coinbase with an Ironwood
/// output.
fn shielded_coinbase_block() -> Fixture {
    nu6_3_block(2, 2, 1, 1, true)
}

/// The same body with a transparent coinbase.
fn transparent_coinbase_block() -> Fixture {
    nu6_3_block(2, 2, 1, 1, false)
}

fn same_layer(a: &Layer, b: &Layer) {
    assert_eq!(a.height, b.height);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.parent, b.parent);
    assert_eq!(a.wtxids, b.wtxids);
    assert_eq!(a.created, b.created);
    assert_eq!(a.spent, b.spent);
    for pool in Pool::ALL {
        assert_eq!(a.nullifiers[pool.index()], b.nullifiers[pool.index()]);
    }
    assert_eq!(a.anchors, b.anchors);
    assert_eq!(a.value_pools, b.value_pools);
    assert_eq!(a.orchard_frontier, b.orchard_frontier);
    assert_eq!(a.sapling_frontier, b.sapling_frontier);
    assert_eq!(a.ironwood_frontier, b.ironwood_frontier);
    assert_eq!(a.history, b.history);
}

/// The position of the flag byte of each Orchard-protocol bundle of a transaction, in wire
/// order (Orchard, then Ironwood): the flag byte, the value balance (8 bytes) and the
/// anchor follow the actions, and every fixture bundle has the anchor of the empty tree.
fn flag_positions(tx: &[u8]) -> Vec<usize> {
    let anchor = Anchor::empty_tree().to_bytes();
    tx.windows(32)
        .enumerate()
        .filter(|(_, window)| *window == anchor)
        .map(|(at, _)| at - 9)
        .collect()
}

/// `block` with transaction `i` replaced by the transaction of `bytes`, and the merkle
/// root of the new transaction list.
fn with_tx(block: &RawBlock, i: usize, bytes: Vec<u8>) -> RawBlock {
    let mut block = block.clone();
    block.txs[i] = RawTx::parse(Bytes::from(bytes), BRANCH).expect("the transaction parses");
    block.header.merkle_root = merkle_root(&block.txids());
    block
}

/// `block` with `edit` applied to the wire bytes of transaction `i`.
fn with_edit(block: &RawBlock, i: usize, edit: impl FnOnce(&mut Vec<u8>)) -> RawBlock {
    let mut bytes = block.txs[i].bytes.to_vec();
    edit(&mut bytes);
    with_tx(block, i, bytes)
}

fn validate(h: &Harness, block: RawBlock) -> Result<Layer, BlockError> {
    validate_block(block, &h.store, &h.chain.view(), &h.cfg).map(|(layer, _)| layer)
}

/// The prepared form of the harness block, without the verification of scripts and
/// proofs: the contextual rules read neither.
fn prepared_block(h: &Harness) -> PreparedBlock {
    let view = h.chain.view();
    let created = hayai_state::block_outputs(&h.block, NU6_3_FIXTURE_HEIGHT);
    let inputs = hayai_state::resolve_inputs(&view, &h.block, &created).expect("funded");
    let txs = h
        .block
        .txs
        .iter()
        .zip(inputs)
        .map(|(raw, spent)| {
            draft(raw.clone(), h.cfg.epoch(), spent)
                .expect("fixture transactions are valid")
                .shared()
                .clone()
        })
        .collect();
    PreparedBlock::new(h.block.clone(), txs)
}

fn check(h: &Harness, block: &PreparedBlock) -> Result<Layer, ContextError> {
    check_with(h, block, &h.cfg.rules)
}

fn check_with(h: &Harness, block: &PreparedBlock, rules: &RuleSet) -> Result<Layer, ContextError> {
    contextual_check(
        &h.chain.view(),
        block,
        &CheckConfig {
            network: h.cfg.network,
            rules,
        },
    )
    .map(|checked| checked.layer)
}

fn edit(block: &mut PreparedBlock, i: usize, f: impl FnOnce(&mut PreparedTx)) {
    let mut p: PreparedTx = (*block.txs[i]).clone();
    f(&mut p);
    block.txs[i] = Arc::new(p);
}

fn ironwood_nullifiers(tx: &RawTx) -> Vec<[u8; 32]> {
    let bundle = tx.tx.ironwood_bundle().expect("an Ironwood bundle");
    bundle
        .actions()
        .iter()
        .map(|a| a.nullifier().to_bytes())
        .collect()
}

/// The root of the tree of `leaves`, by the upstream frontier and the upstream hash.
fn upstream_root(leaves: impl Iterator<Item = MerkleHashOrchard>) -> [u8; 32] {
    let mut frontier = Frontier::<MerkleHashOrchard, 32>::empty();
    for leaf in leaves {
        assert!(frontier.append(leaf), "the tree has room");
    }
    frontier.root().to_bytes()
}

#[test]
fn a_nu6_3_block_validates_cold_and_warm_to_identical_layers() {
    for fixture in [shielded_coinbase_block(), transparent_coinbase_block()] {
        let shielded_coinbase = fixture.name.ends_with("-shielded");
        let cold = harness_with_history(&fixture);
        let block = &cold.block;
        // The shape of the block: versions and bundles per position.
        assert_eq!(block.txs.len(), 5);
        let shape: Vec<(TxVersion, bool, bool)> = block
            .txs
            .iter()
            .map(|t| {
                (
                    t.tx.version(),
                    matches!(t.tx.orchard_bundle(), Some(_bundle)),
                    matches!(t.tx.ironwood_bundle(), Some(_bundle)),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (TxVersion::V6, false, shielded_coinbase),
                (TxVersion::V6, false, true),
                (TxVersion::V6, false, true),
                (TxVersion::V5, true, false),
                (TxVersion::V6, true, true),
            ]
        );
        for t in &block.txs {
            assert_eq!(t.tx.consensus_branch_id(), BRANCH);
            if let Some(b) = t.tx.orchard_bundle() {
                assert!(!b.flags().cross_address_enabled());
                assert_eq!(i64::from(*b.value_balance()), 0);
            }
        }

        // Cold, from bytes. The fixture header has a commitment only in the parsed block
        // of the harness, so the cold run takes the parsed block.
        let (cold_layer, cold_t) =
            validate_block(block.clone(), &cold.store, &cold.chain.view(), &cold.cfg)
                .unwrap_or_else(|e| panic!("{} cold: {e}", fixture.name));
        assert_eq!(cold_t.known, 0);
        assert_eq!(cold_t.unknown, 5);
        let plain = harness(&fixture);
        let (from_bytes, t) = validate_bytes(
            fixture.bytes.clone(),
            &plain.store,
            &plain.chain.view(),
            &plain.cfg,
        )
        .unwrap_or_else(|e| panic!("{} from bytes: {e}", fixture.name));
        assert!(t.parse > std::time::Duration::ZERO);
        assert_eq!(from_bytes.anchors, cold_layer.anchors);

        // Warm: the prepared store holds every transaction except the coinbase.
        let warm = harness_with_history(&fixture);
        warm.fill_store();
        let (warm_layer, warm_t) = validate_block(
            warm.block.clone(),
            &warm.store,
            &warm.chain.view(),
            &warm.cfg,
        )
        .unwrap_or_else(|e| panic!("{} warm: {e}", fixture.name));
        assert_eq!(warm_t.known, 4);
        assert_eq!(warm_t.unknown, 1, "only the coinbase is prepared");
        same_layer(&cold_layer, &warm_layer);

        // The speculative path gives the same layer.
        let (built, verification, _) =
            build_layer(block.clone(), &cold.store, &cold.chain.view(), &cold.cfg)
                .unwrap_or_else(|e| panic!("{} build: {e}", fixture.name));
        assert_eq!(verification.transactions(), 5);
        verify(verification).unwrap_or_else(|e| panic!("{} verify: {e}", fixture.name));
        same_layer(&cold_layer, &built);

        // The layer. Nullifiers: two per Ironwood bundle of the body. A coinbase action
        // has a nullifier too.
        let layer = cold_layer;
        let coinbase_actions = usize::from(shielded_coinbase);
        assert_eq!(layer.height, NU6_3_FIXTURE_HEIGHT);
        assert_eq!(
            layer.nullifiers[Pool::Ironwood.index()].len(),
            3 * 2 + coinbase_actions
        );
        assert_eq!(layer.nullifiers[Pool::Orchard.index()].len(), 2 * 2);
        for nf in ironwood_nullifiers(&block.txs[1]) {
            assert!(layer.nullifiers[Pool::Ironwood.index()].contains(&nf));
            assert!(!layer.nullifiers[Pool::Orchard.index()].contains(&nf));
        }
        // Trees: the roots equal the roots of the upstream frontier over the commitments
        // in block order. The Orchard bundles add their padding commitments.
        let cmxs = |pool: Pool| {
            block.txs.iter().flat_map(move |t| {
                let bundle = match pool {
                    Pool::Orchard => t.tx.orchard_bundle(),
                    _ => t.tx.ironwood_bundle(),
                };
                bundle
                    .into_iter()
                    .flat_map(|b| b.actions().iter())
                    .map(|a| MerkleHashOrchard::from_cmx(a.cmx()))
            })
        };
        assert_eq!(layer.anchors.ironwood, upstream_root(cmxs(Pool::Ironwood)));
        assert_eq!(layer.anchors.orchard, upstream_root(cmxs(Pool::Orchard)));
        assert_ne!(layer.anchors.ironwood, layer.anchors.orchard);
        assert_eq!(
            layer.ironwood_frontier.root().to_bytes(),
            layer.anchors.ironwood
        );
        assert_ne!(
            layer.anchors.ironwood,
            IronwoodFrontier::empty().root().to_bytes()
        );
        // Pools: the Ironwood pool takes the outputs, the Orchard pool takes nothing.
        let ironwood_in: i64 = block
            .txs
            .iter()
            .filter_map(|t| t.tx.ironwood_bundle())
            .map(|b| -i64::from(*b.value_balance()))
            .sum();
        assert!(ironwood_in > 0);
        assert_eq!(layer.value_pools.ironwood, ironwood_in as u64);
        assert_eq!(layer.value_pools.orchard, 0);
        // History: a leaf of tree version 3 with the Ironwood root and count.
        let parent_history = cold.chain.view().history().expect("seeded");
        let leaf = HistoryLeaf::from_block(block, NU6_3_FIXTURE_HEIGHT, &layer.anchors);
        assert_eq!(leaf.ironwood_root, layer.anchors.ironwood);
        assert_eq!(leaf.ironwood_tx, 3 + coinbase_actions as u64);
        assert_eq!(leaf.orchard_tx, 2);
        let expected = parent_history.append(BRANCH, &leaf).expect("appends");
        let after = layer.history.clone().expect("the history is known");
        assert_eq!(*after, expected);
        assert_eq!(after.upgrade(), BRANCH);

        // The layer commits. Its Ironwood root is an anchor of the next block.
        let mut chain = warm.chain;
        let root = layer.anchors.ironwood;
        chain.push(layer).expect("the layer extends the tip");
        let view = chain.view();
        assert!(view.has_anchor(Pool::Ironwood, &root));
        assert!(!view.has_anchor(Pool::Orchard, &root));
        let nfs = ironwood_nullifiers(&block.txs[2]);
        assert_eq!(
            view.contains_nullifier_many(Pool::Ironwood, &nfs),
            [true, true]
        );
        assert_eq!(
            view.contains_nullifier_many(Pool::Orchard, &nfs),
            [false, false]
        );
    }
}

/// The header of a NU6.3 block commits to the history tree of version 3 of its parent.
#[test]
fn the_header_commits_to_the_history_tree_of_version_3() {
    let fixture = transparent_coinbase_block();
    let h = harness_with_history(&fixture);
    let view = h.chain.view();
    let parent = view.history().expect("seeded");
    assert_eq!(parent.upgrade(), BRANCH);
    let auth = auth_data_root(&h.block.auth_digests());
    assert_eq!(
        header_commitment(BRANCH, Some(&parent), &[0; 32], &auth),
        HeaderCommitment::Expected(h.block.header.block_commitments)
    );
    validate(&h, h.block.clone()).expect("the commitment matches");

    // Another root is refused.
    let mut block = h.block.clone();
    block.header.block_commitments = [0x11; 32];
    let Err(BlockError::Context(ContextError::BlockCommitments)) = validate(&h, block) else {
        panic!("a wrong commitment");
    };
    // The root of a tree whose newest leaf has another Ironwood root is refused: the
    // parent tree with its last leaf replaced. One leaf back, then the two candidates.
    let leaf = |ironwood_root: [u8; 32]| HistoryLeaf {
        hash: [3; 32],
        time: 7,
        bits: 0x1c01_0000,
        height: NU6_3_FIXTURE_HEIGHT - 1,
        sapling_root: [1; 32],
        orchard_root: [2; 32],
        ironwood_root,
        sapling_tx: 1,
        orchard_tx: 1,
        ironwood_tx: 1,
    };
    let start = hayai_state::HistoryState::empty(BRANCH);
    let good = start.append(BRANCH, &leaf([5; 32])).expect("appends");
    let bad = start.append(BRANCH, &leaf([6; 32])).expect("appends");
    assert_ne!(good.root(), bad.root());
    let on_good = harness(&fixture);
    on_good.chain.base().write().history = Some(Arc::new(good.clone()));
    let mut block = on_good.block.clone();
    block.header.block_commitments = block_commitments(&bad.root(), &auth);
    let Err(BlockError::Context(ContextError::BlockCommitments)) = validate(&on_good, block) else {
        panic!("a commitment to another Ironwood root");
    };
    let mut block = on_good.block.clone();
    block.header.block_commitments = block_commitments(&good.root(), &auth);
    let layer = validate(&on_good, block).expect("the commitment to the parent tree");
    assert_eq!(layer.history.expect("known").length(), 3);
    assert_ne!(layer.anchors.ironwood, [5; 32]);

    // The NU6.3 activation block starts a new tree: on a parent of NU6.2 the tree after
    // the block has one leaf, and the header commits to the whole NU6.2 tree.
    let nu6_2 = hayai_state::HistoryState::empty(hayai_fixtures::FIXTURE_BRANCH)
        .append(hayai_fixtures::FIXTURE_BRANCH, &leaf([0; 32]))
        .expect("appends");
    let activation = harness(&fixture);
    activation.chain.base().write().history = Some(Arc::new(nu6_2.clone()));
    let mut block = activation.block.clone();
    block.header.block_commitments = block_commitments(&nu6_2.root(), &auth);
    let layer = validate(&activation, block).expect("the activation block");
    let after = layer.history.expect("known");
    assert_eq!((after.length(), after.upgrade()), (1, BRANCH));
}

#[test]
fn invalid_proofs_and_keys_fail_the_shielded_stage() {
    let fixture = shielded_coinbase_block();
    let h = harness(&fixture);
    // One byte of a proof, in each kind of bundle. The proof is before the signatures at
    // the end of a bundle: 64 bytes per action and 64 bytes of binding signature.
    let in_last_proof = |bytes: &mut Vec<u8>| {
        let at = bytes.len() - 200;
        bytes[at] ^= 0x01;
    };
    // The proof of the first bundle starts after its anchor and the length of the proof.
    let in_first_proof = |bytes: &mut Vec<u8>| {
        let at = flag_positions(bytes)[0] + 9 + 32 + 3 + 100;
        bytes[at] ^= 0x01;
    };
    type Edit<'a> = &'a dyn Fn(&mut Vec<u8>);
    let cases: [(&str, usize, Edit<'_>); 5] = [
        ("Ironwood coinbase", 0, &in_last_proof),
        ("Ironwood v6", 1, &in_last_proof),
        ("Orchard v5 at NU6.3", 3, &in_last_proof),
        ("Orchard of a v6 transaction", 4, &in_first_proof),
        ("Ironwood of a v6 transaction with both", 4, &in_last_proof),
    ];
    for (label, i, edit) in cases {
        let block = with_edit(&h.block, i, edit);
        let bad = block.txs[i].wtxid();
        let Err(BlockError::Shielded(failed)) = validate(&h, block) else {
            panic!("{label}: a bad proof");
        };
        assert_eq!(failed, vec![bad], "{label}");
    }
    // A spend authorization signature of an Ironwood action.
    let block = with_edit(&h.block, 2, |bytes| {
        let at = bytes.len() - 64 - 10;
        bytes[at] ^= 0x01;
    });
    let bad = block.txs[2].wtxid();
    let Err(BlockError::Shielded(failed)) = validate(&h, block) else {
        panic!("a bad signature");
    };
    assert_eq!(failed, vec![bad]);
    // `enableCrossAddress` of an Ironwood bundle after the proof: the proof is for the
    // instance with the flag set, and the sighash covers the flag byte. The coinbase has no
    // script, so the shielded stage gives the verdict. In a transaction with a transparent
    // input the script stage comes first: the same sighash signs the input.
    let block = with_edit(&h.block, 0, |bytes| {
        let at = flag_positions(bytes)[0];
        assert_eq!(bytes[at], OUTPUTS | 0b100);
        bytes[at] = OUTPUTS;
    });
    let bad = block.txs[0].wtxid();
    let Err(BlockError::Shielded(failed)) = validate(&h, block) else {
        panic!("another cross-address instance");
    };
    assert_eq!(failed, vec![bad]);
    let block = with_edit(&h.block, 1, |bytes| {
        let at = flag_positions(bytes)[0];
        assert_eq!(bytes[at], 0b111);
        bytes[at] = SPENDS | OUTPUTS;
    });
    let Err(BlockError::Prepare {
        tx: 1,
        error: PrepareError::Script(0, _),
    }) = validate(&h, block)
    else {
        panic!("the flag byte is in the sighash of the transparent input");
    };

    // The bundles of NU6.3 need the key of the NU6.3 circuit: with the key of NU6.2 only,
    // the block is not verified.
    let mut old_keys = harness(&fixture);
    old_keys.cfg.keys = keys();
    let Err(BlockError::Prepare {
        tx: 0,
        error: PrepareError::Unsupported("orchard verifying key not built"),
    }) = validate(&old_keys, old_keys.block.clone())
    else {
        panic!("the key of another circuit version");
    };
}

#[test]
fn context_free_rules_reject_at_the_block_level() {
    let fixture = transparent_coinbase_block();
    let h = harness(&fixture);
    // Flags: the Ironwood bundle of transaction 1 is its only sink of funds. Without
    // `enableOutputs` the transaction has no sink.
    let block = with_edit(&h.block, 1, |bytes| {
        let at = flag_positions(bytes)[0];
        bytes[at] = SPENDS;
    });
    let Err(BlockError::Prepare {
        tx: 1,
        error: PrepareError::NoSink,
    }) = validate(&h, block)
    else {
        panic!("an Ironwood bundle with outputs disabled as the only sink");
    };
    // Flags: bit 2 in the Orchard slot does not parse.
    let mut bytes = h.block.txs[3].bytes.to_vec();
    let at = flag_positions(&bytes)[0];
    bytes[at] |= 0b100;
    let Err(_) = RawTx::parse(Bytes::from(bytes), BRANCH) else {
        panic!("an Orchard bundle with enableCrossAddress parses");
    };
    // A deposit to the Orchard pool.
    let block = with_edit(&h.block, 3, |bytes| {
        let at = flag_positions(bytes)[0] + 1;
        bytes[at..at + 8].copy_from_slice(&(-1i64).to_le_bytes());
    });
    let Err(BlockError::Prepare {
        tx: 3,
        error: PrepareError::OrchardPoolDeposit(-1),
    }) = validate(&h, block)
    else {
        panic!("a deposit to the Orchard pool");
    };
    // A repeated nullifier in an Ironwood bundle.
    let block = with_edit(&h.block, 1, |bytes| {
        let first = flag_positions(bytes)[0] - 2 * ACTION_BYTES;
        let nullifier = bytes[first + 32..first + 64].to_vec();
        let second = first + ACTION_BYTES;
        bytes[second + 32..second + 64].copy_from_slice(&nullifier);
    });
    let Err(BlockError::Prepare {
        tx: 1,
        error: PrepareError::DuplicateNullifier(Pool::Ironwood),
    }) = validate(&h, block)
    else {
        panic!("a repeated Ironwood nullifier in a transaction");
    };

    // A coinbase with an Orchard bundle: the coinbase of the block with the Orchard
    // bundle of transaction 3, `enableSpends = 0`.
    let coinbase: &TransactionData<Authorized> = &h.block.txs[0].tx;
    let orchard = h.block.txs[3].tx.orchard_bundle().expect("Orchard").clone();
    for version in [TxVersion::V5, TxVersion::V6] {
        let transparent = coinbase.transparent_bundle().cloned();
        let data = match version {
            TxVersion::V6 => TransactionData::<Authorized>::from_parts_v6(
                BRANCH,
                0,
                coinbase.expiry_height(),
                transparent,
                None,
                Some(orchard.clone()),
                None,
            ),
            _ => TransactionData::<Authorized>::from_parts(
                TxVersion::V5,
                BRANCH,
                0,
                coinbase.expiry_height(),
                transparent,
                None,
                None,
                Some(orchard.clone()),
            ),
        };
        let mut bytes = Vec::new();
        data.freeze()
            .expect("freezes")
            .write(&mut bytes)
            .expect("vec write");
        let at = flag_positions(&bytes)[0];
        bytes[at] = OUTPUTS;
        let block = with_tx(&h.block, 0, bytes);
        let Err(BlockError::Prepare {
            tx: 0,
            error: PrepareError::CoinbaseOrchardBundle,
        }) = validate(&h, block)
        else {
            panic!("{version:?}: a coinbase with an Orchard bundle");
        };
    }
    // An Ironwood coinbase with `enableSpends = 1`.
    let shielded = harness(&shielded_coinbase_block());
    let block = with_edit(&shielded.block, 0, |bytes| {
        let at = flag_positions(bytes)[0];
        bytes[at] |= SPENDS;
    });
    let Err(BlockError::Prepare {
        tx: 0,
        error: PrepareError::CoinbaseShieldedSpend,
    }) = validate(&shielded, block)
    else {
        panic!("an Ironwood coinbase with spends enabled");
    };
}

#[test]
fn ironwood_nullifiers_are_unique_in_the_block_the_layers_and_the_base() {
    let fixture = transparent_coinbase_block();
    let h = harness(&fixture);
    let valid = prepared_block(&h);
    check(&h, &valid).expect("the block is valid");

    // In the block: transaction 2 reveals a nullifier of transaction 1.
    let nf = ironwood_nullifiers(&h.block.txs[1])[0];
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| p.nullifiers.push((Pool::Ironwood, nf)));
    let Err(ContextError::DuplicateNullifier {
        pool: Pool::Ironwood,
        tx: 2,
    }) = check(&h, &block)
    else {
        panic!("a nullifier repeats in the block");
    };
    // The same bytes as an Orchard nullifier are no repetition.
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| p.nullifiers.push((Pool::Orchard, nf)));
    check(&h, &block).expect("the pools have distinct nullifier sets");

    // In a layer of the window: the parent block revealed the nullifier.
    let mut windowed = chain_with_layers(&fixture, 1, BlockShape::TYPICAL);
    let parent = windowed.chain.pop().expect("one layer");
    let mut parent = Arc::try_unwrap(parent).expect("the chain held the only reference");
    parent.nullifiers[Pool::Ironwood.index()].insert(nf);
    windowed.chain.push(parent).expect("the layer goes back");
    let Err(BlockError::Context(ContextError::DuplicateNullifier {
        pool: Pool::Ironwood,
        tx: 1,
    })) = validate(&windowed, windowed.block.clone())
    else {
        panic!("a nullifier of a layer of the window");
    };
    // In the base: the layer is final.
    windowed.chain.finalize_excess(0).expect("finalizes");
    assert_eq!(windowed.chain.layers().count(), 0);
    let Err(BlockError::Context(ContextError::DuplicateNullifier {
        pool: Pool::Ironwood,
        tx: 1,
    })) = validate(&windowed, windowed.block.clone())
    else {
        panic!("a nullifier of the base");
    };
    // The Orchard set of the parent does not count for the Ironwood pool.
    let mut other = chain_with_layers(&fixture, 1, BlockShape::TYPICAL);
    let parent = other.chain.pop().expect("one layer");
    let mut parent = Arc::try_unwrap(parent).expect("the chain held the only reference");
    parent.nullifiers[Pool::Orchard.index()].insert(nf);
    other.chain.push(parent).expect("the layer goes back");
    validate(&other, other.block.clone()).expect("an Orchard nullifier with the same bytes");
}

#[test]
fn ironwood_anchors_are_roots_of_earlier_blocks() {
    let fixture = transparent_coinbase_block();
    let mut h = harness(&fixture);
    let layer = check(&h, &prepared_block(&h)).expect("the block is valid");
    let own_root = layer.anchors.ironwood;
    let empty = IronwoodFrontier::empty().root().to_bytes();
    assert_ne!(own_root, empty);
    // Every Ironwood bundle of the fixture has the anchor of the empty tree.
    let block = prepared_block(&h);
    assert!(block.txs[1].anchors.contains(&(Pool::Ironwood, empty)));

    // An unknown root.
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| {
        p.anchors = vec![(Pool::Ironwood, [9; 32])]
    });
    let Err(ContextError::BadAnchor {
        pool: Pool::Ironwood,
        tx: 2,
    }) = check(&h, &block)
    else {
        panic!("an unknown Ironwood anchor");
    };
    // The root that this block produces is not valid for its own transactions.
    let mut block = prepared_block(&h);
    edit(&mut block, 2, |p| {
        p.anchors = vec![(Pool::Ironwood, own_root)]
    });
    let Err(ContextError::BadAnchor {
        pool: Pool::Ironwood,
        tx: 2,
    }) = check(&h, &block)
    else {
        panic!("the root of the block itself");
    };
    // An Orchard root is not an Ironwood anchor.
    let orchard_root = layer.anchors.orchard;
    // After the block, its Ironwood root is an anchor, in the window and in the base.
    h.chain.push(layer).expect("the layer extends the tip");
    for window in [1, 0] {
        h.chain.finalize_excess(window).expect("finalizes");
        let view = h.chain.view();
        assert!(view.has_anchor(Pool::Ironwood, &own_root));
        assert!(view.has_anchor(Pool::Ironwood, &empty));
        assert!(!view.has_anchor(Pool::Ironwood, &orchard_root));
        assert!(!view.has_anchor(Pool::Orchard, &own_root));
        assert!(!view.has_anchor(Pool::Ironwood, &[9; 32]));
    }
}

#[test]
fn the_ironwood_pool_does_not_go_negative() {
    // One Ironwood transaction. Its value balance becomes +5: it takes 5 zatoshis out of
    // the pool. The fee of the prepared transaction stays, so the coinbase value rule
    // holds and the pool rule is the one that decides.
    let fixture = nu6_3_block(1, 2, 0, 0, false);
    let h = harness(&fixture);
    let mut bytes = h.block.txs[1].bytes.to_vec();
    let at = flag_positions(&bytes)[0] + 1;
    bytes[at..at + 8].copy_from_slice(&5i64.to_le_bytes());
    let raw = RawTx::parse(Bytes::from(bytes), BRANCH).expect("parses");
    let withdrawal = |h: &Harness| {
        let mut block = prepared_block(h);
        block.raw.txs[1] = raw.clone();
        edit(&mut block, 1, |p| p.raw = Arc::new(raw.clone()));
        block
    };
    let Err(ContextError::NegativeValuePool(Pool::Ironwood)) = check(&h, &withdrawal(&h)) else {
        panic!("more value leaves the Ironwood pool than it holds");
    };
    // With 5 zatoshis in the pool the block is valid and the pool is empty after it.
    h.chain.base().write().value_pools.ironwood = 5;
    let layer = check(&h, &withdrawal(&h)).expect("the pool holds the value");
    assert_eq!(layer.value_pools.ironwood, 0);
    h.chain.base().write().value_pools.ironwood = 4;
    let Err(ContextError::NegativeValuePool(Pool::Ironwood)) = check(&h, &withdrawal(&h)) else {
        panic!("one zatoshi short");
    };
    // The Orchard pool of the base does not pay for the Ironwood pool.
    h.chain.base().write().value_pools.orchard = 100;
    let Err(ContextError::NegativeValuePool(Pool::Ironwood)) = check(&h, &withdrawal(&h)) else {
        panic!("the pools are separate");
    };
}

#[test]
fn ironwood_actions_count_for_the_block_limit() {
    let fixture = transparent_coinbase_block();
    let h = harness(&fixture);
    let block = prepared_block(&h);
    let actions: Vec<u32> = block.txs.iter().map(|t| t.ironwood_actions).collect();
    assert_eq!(actions, [0, 2, 2, 0, 2]);
    let limited = |ironwood_actions| RuleSet {
        limits: BlockLimits {
            ironwood_actions,
            ..BlockLimits::PRE_NU7
        },
        ..h.cfg.rules
    };
    check_with(&h, &block, &limited(6)).expect("six Ironwood actions");
    let Err(ContextError::TooManyIronwoodActions(6)) = check_with(&h, &block, &limited(5)) else {
        panic!("over the Ironwood limit");
    };
    // The Orchard limit counts the Orchard actions only.
    let orchard_limited = RuleSet {
        limits: BlockLimits {
            orchard_actions: 4,
            ..BlockLimits::PRE_NU7
        },
        ..h.cfg.rules
    };
    check_with(&h, &block, &orchard_limited).expect("four Orchard actions");
    assert_eq!(BlockLimits::NU7.ironwood_actions, 330);
    // ZIP 218, the shielded cost: the actions of both pools and the Sapling spends and
    // outputs together, with each pool within its own limit.
    let cost: u32 = block
        .txs
        .iter()
        .map(|tx| tx.orchard_actions + tx.ironwood_actions + tx.sapling_ios)
        .sum();
    let orchard: u32 = block.txs.iter().map(|tx| tx.orchard_actions).sum();
    assert!(orchard > 0 && cost >= orchard + 6, "{cost} {orchard}");
    let with_budget = |shielded_cost| RuleSet {
        limits: BlockLimits {
            shielded_cost,
            ..BlockLimits::NU7
        },
        ..h.cfg.rules
    };
    check_with(&h, &block, &with_budget(cost)).expect("at the budget");
    let Err(ContextError::ShieldedCostAboveBudget(found)) =
        check_with(&h, &block, &with_budget(cost - 1))
    else {
        panic!("over the shielded budget");
    };
    assert_eq!(found, cost);
}

/// The coinbase value includes the value that enters the Ironwood pool.
#[test]
fn the_coinbase_value_counts_the_ironwood_output() {
    let h = harness(&shielded_coinbase_block());
    let block = prepared_block(&h);
    let coinbase = &h.block.txs[0].tx;
    let shielded = -i64::from(
        *coinbase
            .ironwood_bundle()
            .expect("an Ironwood output")
            .value_balance(),
    );
    assert!(shielded > 0);
    // The value rule of the fixture height is the equality of ZIP 236. The block is valid,
    // so the value that the coinbase pays includes the Ironwood output.
    let terms = hayai_fixtures::coinbase_terms(NU6_3_FIXTURE_HEIGHT);
    assert!(terms.exact_value);
    check(&h, &block).expect("the coinbase pays its terms exactly");
    // The transparent outputs alone pay less than the terms by the Ironwood output.
    let fees: u64 = block.txs.iter().map(|t| t.fee).sum();
    let outputs: Vec<CoinbaseOutput> = coinbase
        .transparent_bundle()
        .expect("outputs")
        .vout
        .iter()
        .map(|o| CoinbaseOutput {
            value: o.value().into_u64(),
            script: o.script_pubkey().0 .0.to_vec(),
        })
        .collect();
    let Err(CoinbaseError::ValueNotExact { paid, required }) =
        terms.check(&outputs, ShieldedBalances::default(), fees)
    else {
        panic!("the transparent outputs are not the whole coinbase value");
    };
    assert_eq!(paid + i128::from(shielded), required);
    let with_ironwood = ShieldedBalances {
        ironwood: -shielded,
        ..ShieldedBalances::default()
    };
    assert_eq!(terms.check(&outputs, with_ironwood, fees), Ok(()));
}

/// ZIP 317 with Ironwood: the template candidate of a transaction counts its Ironwood
/// actions as logical actions and for the block limit of the Ironwood pool.
#[test]
fn a_template_candidate_counts_its_ironwood_actions() {
    use hayai_template::zip317::logical_actions;
    use hayai_template::{Candidate, Zip317Params};

    let fixture = transparent_coinbase_block();
    let h = harness(&fixture);
    let prepared = h.prepare_all();
    let params = Zip317Params::ZAKURA;
    let mut with_ironwood = 0;
    for tx in &prepared {
        let raw = &tx.raw;
        let orchard = raw.tx.orchard_bundle().map_or(0, |b| b.actions().len()) as u32;
        let ironwood = raw.tx.ironwood_bundle().map_or(0, |b| b.actions().len()) as u32;
        let candidate = Candidate::from_raw(raw, tx.fee, tx.sigops, Vec::new(), &params);
        assert_eq!(candidate.ironwood_actions, ironwood);
        assert_eq!(candidate.ironwood_actions, tx.ironwood_actions);
        assert_eq!(candidate.orchard_actions, orchard);
        // Each fixture transaction has one transparent input and at most one transparent
        // output: one transparent logical action.
        assert_eq!(logical_actions(&raw.tx), 1 + orchard + ironwood);
        assert_eq!(
            candidate.conventional_fee,
            params.conventional_fee(1 + orchard + ironwood)
        );
        if ironwood > 0 {
            with_ironwood += 1;
            assert!(candidate.conventional_fee > params.conventional_fee(1 + orchard));
        }
    }
    assert_eq!(with_ironwood, 3);
}

/// A prebuilt body with Ironwood bundles commits to the layer of the full validation. A
/// block with an Ironwood coinbase is not the prebuilt block.
#[test]
fn a_prebuilt_body_with_ironwood_bundles_commits() {
    let fixture = transparent_coinbase_block();
    let h = harness_with_history(&fixture);
    h.fill_store();
    let view = h.chain.view();
    let body: Vec<_> = h.block.txs[1..].iter().map(|t| t.wtxid()).collect();
    let prebuilt = prebuild(&body, &h.store, &view, &h.cfg).expect("the body prebuilds");
    let (swapped, _) = commit_prebuilt(&h.block, prebuilt, &view, &h.cfg).expect("commits");
    let (full, _) = validate_block(h.block.clone(), &h.store, &view, &h.cfg).expect("validates");
    same_layer(&swapped, &full);
    assert_ne!(
        swapped.anchors.ironwood,
        IronwoodFrontier::empty().root().to_bytes()
    );

    // The same body under a coinbase with an Ironwood output: the coinbase adds a note
    // commitment before the commitments of the body, so the prebuilt trees do not apply.
    let shielded = harness_with_history(&shielded_coinbase_block());
    shielded.fill_store();
    let view = shielded.chain.view();
    let body: Vec<_> = shielded.block.txs[1..].iter().map(|t| t.wtxid()).collect();
    let prebuilt = prebuild(&body, &shielded.store, &view, &shielded.cfg).expect("prebuilds");
    let Err(CommitError::Mismatch("a shielded coinbase")) =
        commit_prebuilt(&shielded.block, prebuilt, &view, &shielded.cfg).map(|_| ())
    else {
        panic!("an Ironwood coinbase is a shielded coinbase");
    };
}
