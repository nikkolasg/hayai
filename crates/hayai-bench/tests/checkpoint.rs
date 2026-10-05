//! The checkpoint path (`hayai_validate::apply_checkpointed`) against full validation: the
//! same chain of generated blocks through both paths gives the same state. The path
//! refuses a block that is not on the checkpointed chain, and it keeps the checks that
//! guard the state. Full validation refuses a block at or below the mandatory checkpoint.

use std::sync::Arc;

use bytes::Bytes;
use hayai_bench::chain_fixture::{coin, harness, harness_with_history, Harness};
use hayai_bench::fixtures::{nu6_3_block, orchard_block, transparent_block, Fixture};
use hayai_coins::{CoinsView, MemBacking, MemConfig, OutPoint, Pool};
use hayai_consensus::{rules_at, Checkpoints, Network};
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_prepared::PrepareError;
use hayai_state::{checkpoint_layer, Base, Chain, CheckConfig, ContextError, Layer};
use hayai_validate::{
    apply_checkpointed, block_commitments, validate_block, BlockError, ValidateConfig,
};
use hayai_wire::header::BlockHash;
use hayai_wire::{auth_data_root, merkle_root, RawBlock, RawTx};
use sha2::{Digest, Sha256};

/// The blocks of the generated chain, in chain order: transparent transactions, Orchard
/// bundles, and a block with only a coinbase. No two of them spend the same funding coin.
fn chain_fixtures() -> Vec<Fixture> {
    vec![
        transparent_block(5, 2),
        orchard_block(2, 2),
        transparent_block(0, 1),
    ]
}

/// A harness with a known history tree whose base holds the funding coins of every
/// fixture. The block of the harness is the block of the first fixture.
fn base_for(fixtures: &[Fixture]) -> Harness {
    let h = harness_with_history(&fixtures[0]);
    {
        let mut base = h.chain.base().write();
        for fixture in &fixtures[1..] {
            for (outpoint, funding) in &fixture.funding {
                base.coins
                    .add(outpoint.clone(), coin(funding))
                    .expect("funding outpoints are distinct");
                base.value_pools.transparent += funding.value;
            }
        }
    }
    h
}

/// Builds one block for each fixture on the chain of `full`, validates it in full and
/// pushes its layer. The header of each block commits to the history tree of its parent.
fn build_chain(full: &mut Harness, fixtures: &[Fixture]) -> Vec<(RawBlock, Arc<Layer>)> {
    fixtures
        .iter()
        .map(|fixture| {
            let view = full.chain.view();
            let tip = view.tip();
            let mut raw = fixture.at(tip.height + 1, tip.hash);
            let history = view.history().expect("the base has a history tree");
            raw.header.block_commitments =
                block_commitments(&history.root(), &auth_data_root(&raw.auth_digests()));
            let (layer, _) = validate_block(raw.clone(), &full.store, &view, &full.cfg)
                .unwrap_or_else(|e| panic!("{}: {e}", fixture.name));
            (
                raw,
                full.chain.push(layer).expect("the layer is the next one"),
            )
        })
        .collect()
}

/// The checkpoint list of a generated chain: its last block.
fn checkpoint_at(block: &RawBlock, height: u32) -> Checkpoints {
    Checkpoints::new(vec![(height, block.hash())]).expect("one checkpoint")
}

fn same_layer(a: &Layer, b: &Layer) {
    assert_eq!(a.height, b.height);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.parent, b.parent);
    assert_eq!((a.time, a.bits), (b.time, b.bits));
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
    assert_eq!(a.sprout_frontier, b.sprout_frontier);
    assert!(a.history_root().is_some());
    assert_eq!(a.history_root(), b.history_root());
}

/// SHA-256 of the coin, or of its absence, at each outpoint of `outpoints`.
fn coin_set_digest(chain: &Chain, outpoints: &[OutPoint]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for (outpoint, coin) in outpoints.iter().zip(chain.view().get_coins(outpoints)) {
        hasher.update(outpoint.hash());
        hasher.update(outpoint.n().to_le_bytes());
        match coin {
            Some(coin) => {
                hasher.update([1]);
                hasher.update(coin.encode());
            }
            None => hasher.update([0]),
        }
    }
    hasher.finalize().into()
}

/// The state of a chain that the two paths must agree on: the tip, the tree roots, the
/// history root, the value pools, the coin set over `outpoints` and the presence of each
/// nullifier of `nullifiers`.
fn state_of(
    chain: &Chain,
    outpoints: &[OutPoint],
    nullifiers: &[(Pool, [u8; 32])],
) -> impl PartialEq + std::fmt::Debug {
    let view = chain.view();
    let revealed: Vec<bool> = nullifiers
        .iter()
        .map(|(pool, nf)| view.contains_nullifier_many(*pool, &[*nf])[0])
        .collect();
    (
        view.tip(),
        view.frontiers().anchors,
        view.history().map(|h| h.root()),
        view.value_pools(),
        coin_set_digest(chain, outpoints),
        revealed,
    )
}

#[test]
fn a_generated_chain_gives_the_same_state_through_both_paths() {
    let fixtures = chain_fixtures();
    let mut full = base_for(&fixtures);
    let mut fast = base_for(&fixtures);
    let blocks = build_chain(&mut full, &fixtures);
    let (last, _) = blocks.last().expect("three blocks");
    let checkpoints = checkpoint_at(last, full.chain.tip().height);

    // Every outpoint and nullifier that the chain reads or writes.
    let mut outpoints: Vec<OutPoint> = fixtures
        .iter()
        .flat_map(|f| f.funding.iter().map(|(outpoint, _)| outpoint.clone()))
        .collect();
    let mut nullifiers = Vec::new();
    for (raw, full_layer) in &blocks {
        let (layer, timings) =
            apply_checkpointed(raw, raw.hash(), &fast.chain.view(), &fast.cfg, &checkpoints)
                .expect("a block of the checkpointed chain");
        same_layer(&layer, full_layer);
        assert_eq!(timings.scripts, std::time::Duration::ZERO);
        assert_eq!(timings.shielded, std::time::Duration::ZERO);
        outpoints.extend(layer.created.keys().cloned());
        for pool in Pool::ALL {
            nullifiers.extend(layer.nullifiers[pool.index()].iter().map(|nf| (pool, *nf)));
        }
        fast.chain.push(layer).expect("the layer is the next one");
    }
    assert!(nullifiers.iter().any(|(pool, _)| *pool == Pool::Orchard));
    outpoints.sort_by_key(|o| (*o.hash(), o.n()));
    assert_eq!(
        state_of(&full.chain, &outpoints, &nullifiers),
        state_of(&fast.chain, &outpoints, &nullifiers)
    );
    // The chain spent every funding coin and left the outputs of the three blocks.
    let unspent = fast.chain.view().get_coins(&outpoints);
    assert!(unspent.iter().flatten().count() >= 3);
    assert!(unspent.contains(&None));

    // The same state in the base, after the layers merge into it.
    for chain in [&mut full.chain, &mut fast.chain] {
        assert_eq!(chain.finalize_excess(0).expect("the layers merge"), 3);
    }
    assert_eq!(
        state_of(&full.chain, &outpoints, &nullifiers),
        state_of(&fast.chain, &outpoints, &nullifiers)
    );
}

/// A block of NU6.3 with Ironwood and Orchard bundles: the Ironwood tree, nullifiers and
/// pool follow the same state update.
#[test]
fn a_nu6_3_block_gives_the_same_layer_through_both_paths() {
    let fixture = nu6_3_block(2, 2, 1, 1, false);
    let h = harness_with_history(&fixture);
    let view = h.chain.view();
    let (full, _) = validate_block(h.block.clone(), &h.store, &view, &h.cfg).expect("valid");
    let checkpoints = checkpoint_at(&h.block, fixture.height);
    let (fast, _) = apply_checkpointed(&h.block, h.block.hash(), &view, &h.cfg, &checkpoints)
        .expect("a block of the checkpointed chain");
    same_layer(&fast, &full);
    assert!(!fast.nullifiers[Pool::Ironwood.index()].is_empty());
    assert_ne!(fast.anchors.ironwood, view.frontiers().anchors.ironwood);
}

#[test]
fn the_checkpoint_path_accepts_only_the_checkpointed_chain() {
    let fixture = transparent_block(5, 2);
    let h = harness_with_history(&fixture);
    let view = h.chain.view();
    let hash = h.block.hash();
    let other = BlockHash([7; 32]);
    let apply = |raw: &RawBlock, expected: BlockHash, checkpoints: &Checkpoints| {
        apply_checkpointed(raw, expected, &view, &h.cfg, checkpoints).map(|(layer, _)| layer)
    };
    let list = |entries: Vec<(u32, BlockHash)>| Checkpoints::new(entries).expect("a list");
    let at = checkpoint_at(&h.block, fixture.height);

    // A checkpoint at the height, or above it.
    apply(&h.block, hash, &at).expect("the block is the checkpoint");
    let above = list(vec![(fixture.height + 400, other)]);
    apply(&h.block, hash, &above).expect("the block is below the checkpoint");

    // No checkpoint at or above the height.
    for (checkpoints, last) in [
        (list(Vec::new()), None),
        (
            list(vec![(fixture.height - 1, other)]),
            Some(fixture.height - 1),
        ),
    ] {
        let Err(BlockError::AboveLastCheckpoint {
            height,
            last: found,
        }) = apply(&h.block, hash, &checkpoints)
        else {
            panic!("a block above the last checkpoint");
        };
        assert_eq!((height, found), (fixture.height, last));
    }
    // The real lists: a generated block is not the checkpoint of its height, and Regtest
    // has only the genesis block.
    assert!(matches!(
        apply(&h.block, hash, Network::Regtest.checkpoints()),
        Err(BlockError::AboveLastCheckpoint { last: Some(0), .. })
    ));

    // Another hash in the header chain, then another checkpoint hash.
    for (expected, checkpoints) in [(other, &at), (hash, &list(vec![(fixture.height, other)]))] {
        let Err(BlockError::NotOnCheckpointedChain {
            height,
            expected: wanted,
            found,
        }) = apply(&h.block, expected, checkpoints)
        else {
            panic!("a block that is not on the checkpointed chain");
        };
        assert_eq!((height, wanted, found), (fixture.height, other, hash));
    }

    // Another parent.
    let mut orphan = h.block.clone();
    orphan.header.prev_hash = other;
    assert!(matches!(
        apply(&orphan, orphan.hash(), &above),
        Err(BlockError::Context(ContextError::WrongParent { .. }))
    ));
}

#[test]
fn the_checkpoint_path_keeps_the_checks_that_guard_the_state() {
    let fixture = transparent_block(5, 2);
    let h = harness_with_history(&fixture);
    let view = h.chain.view();
    let above = Checkpoints::new(vec![(fixture.height + 400, BlockHash([7; 32]))]).unwrap();
    let apply = |raw: &RawBlock, view| {
        apply_checkpointed(raw, raw.hash(), view, &h.cfg, &above).map(|(layer, _)| layer)
    };

    // The body does not match the header.
    let mut changed = h.block.clone();
    changed.header.merkle_root[0] ^= 0x01;
    assert!(matches!(
        apply(&changed, &view),
        Err(BlockError::MerkleRoot)
    ));

    // The header does not commit to the history tree of the parent.
    let mut uncommitted = h.block.clone();
    uncommitted.header.block_commitments = [0x11; 32];
    assert!(matches!(
        apply(&uncommitted, &view),
        Err(BlockError::Context(ContextError::BlockCommitments))
    ));

    // An input without a coin: the base of another fixture does not hold the funding.
    let empty = harness(&transparent_block(0, 1));
    let empty_view = empty.chain.view();
    let moved = fixture.at(fixture.height, empty_view.tip().hash);
    assert!(matches!(
        apply(&moved, &empty_view),
        Err(BlockError::Context(ContextError::MissingInput {
            tx: 1,
            input: 0
        }))
    ));

    // An outpoint spent twice in the block: transaction 2 becomes a copy of transaction 1.
    // The path stops at the txid that is in the block twice. The state update alone
    // (`checkpoint_layer`) stops at the double spend.
    let mut txs = h.block.txs.clone();
    txs[2] = txs[1].clone();
    let mut double = h.block.clone();
    double.header.merkle_root = merkle_root(&txs.iter().map(|t| t.txid).collect::<Vec<_>>());
    double.txs = txs;
    let auth = auth_data_root(&double.auth_digests());
    double.header.block_commitments =
        block_commitments(&view.history().expect("a history tree").root(), &auth);
    assert!(matches!(
        apply(&double, &view),
        Err(BlockError::Context(ContextError::DuplicateTxid(txid))) if txid == double.txs[1].txid
    ));
    let check_cfg = CheckConfig {
        network: h.cfg.network,
        rules: &h.cfg.rules,
    };
    assert!(matches!(
        checkpoint_layer(&view, &double, &auth, &check_cfg).map(|_| ()),
        Err(ContextError::DoubleSpend { tx: 2, input: 0 })
    ));
}

/// CVE-2012-2459 on the checkpoint path: the body `[coinbase, a, b, b]` has the merkle
/// root and the hash of the checkpointed block `[coinbase, a, b]`. The error is
/// `DuplicateTxid`, which names a fault of the body, and the true body of the same header
/// passes after it.
#[test]
fn a_body_with_a_txid_twice_is_a_duplicate_txid_on_the_checkpoint_path() {
    let fixture = transparent_block(2, 1);
    let h = harness_with_history(&fixture);
    let view = h.chain.view();
    let checkpoints = checkpoint_at(&h.block, fixture.height);
    assert_eq!(h.block.txs.len(), 3);
    let mut mutated = h.block.clone();
    mutated.txs.push(mutated.txs[2].clone());
    assert_eq!(merkle_root(&mutated.txids()), h.block.header.merkle_root);
    assert_eq!(mutated.hash(), h.block.hash());
    assert_eq!(checkpoints.hash_at(fixture.height), Some(mutated.hash()));
    let Err(BlockError::Context(ContextError::DuplicateTxid(txid))) =
        apply_checkpointed(&mutated, mutated.hash(), &view, &h.cfg, &checkpoints)
    else {
        panic!("a body with a txid twice");
    };
    assert_eq!(txid, mutated.txs[2].txid);
    apply_checkpointed(&h.block, h.block.hash(), &view, &h.cfg, &checkpoints)
        .expect("the true body of the checkpointed block");
}

/// A published Mainnet block by the height digits of its file name.
fn mainnet_vector(digits: &str, height: u32) -> RawBlock {
    let name = format!(
        "{}/tests/vectors/block-main-{digits}.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let bytes = hex::decode(hex.trim()).expect("hex");
    let rules = rules_at(Network::Mainnet, height).expect("a rule set");
    RawBlock::parse(Bytes::from(bytes), rules.branch_id).expect("a block")
}

/// `tx` with the bytes `from` replaced by `to` at their one position.
fn patched(tx: &RawTx, from: &[u8; 32], to: &[u8; 32], branch: BranchId) -> RawTx {
    let mut bytes = tx.bytes.to_vec();
    let positions: Vec<usize> = bytes
        .windows(32)
        .enumerate()
        .filter_map(|(at, window)| (window == from).then_some(at))
        .collect();
    let [at] = positions[..] else {
        panic!(
            "the nullifier is in the transaction {} times",
            positions.len()
        );
    };
    bytes[at..at + 32].copy_from_slice(to);
    RawTx::parse(Bytes::from(bytes), branch).expect("the changed transaction parses")
}

/// The checkpoint path refuses a nullifier that the block reveals twice, in the Sapling
/// pool and in the Orchard pool. The transactions are the shielded transactions of the
/// Mainnet block 1,687,107 (published vector) with one nullifier changed.
#[test]
fn the_checkpoint_path_refuses_a_nullifier_twice_in_the_block() {
    const HEIGHT: u32 = 1_687_107;
    let block = mainnet_vector("1-687-107", HEIGHT);
    let rules = rules_at(Network::Mainnet, HEIGHT).expect("a rule set");
    let branch = rules.branch_id;
    let (sapling_and_orchard, sapling) = (&block.txs[4], &block.txs[5]);
    let sapling_nullifier = |tx: &RawTx| {
        tx.tx
            .sapling_bundle()
            .expect("a Sapling bundle")
            .shielded_spends()[0]
            .nullifier()
            .0
    };
    let orchard_nullifiers: Vec<[u8; 32]> = sapling_and_orchard
        .tx
        .orchard_bundle()
        .expect("an Orchard bundle")
        .actions()
        .iter()
        .map(|action| action.nullifier().to_bytes())
        .collect();

    let dir = hayai_bench::scratch_dir();
    let (backing, _) = MemBacking::open(dir.path(), &MemConfig::default()).expect("a backing");
    let base = Base::new(
        Arc::new(backing),
        HEIGHT - 1,
        block.header.prev_hash,
        block.header.time - 75,
    );
    let chain = Chain::new(base);
    let view = chain.view();
    let cfg = CheckConfig {
        network: Network::Mainnet,
        rules,
    };
    let check = |txs: Vec<RawTx>| {
        let raw = RawBlock {
            txs,
            ..block.clone()
        };
        let auth = auth_data_root(&raw.auth_digests());
        checkpoint_layer(&view, &raw, &auth, &cfg).map(|_| ())
    };
    let coinbase = block.txs[0].clone();

    // The two transactions as published reveal distinct nullifiers: the loop over the
    // transactions passes, and the block fails a later rule of this base without pools.
    let unchanged = check(vec![
        coinbase.clone(),
        sapling_and_orchard.clone(),
        sapling.clone(),
    ]);
    assert!(
        !matches!(unchanged, Err(ContextError::DuplicateNullifier { .. })),
        "{unchanged:?}"
    );

    // Sapling: transaction 2 reveals the nullifier of transaction 1.
    let second = patched(
        sapling,
        &sapling_nullifier(sapling),
        &sapling_nullifier(sapling_and_orchard),
        branch,
    );
    assert_eq!(
        check(vec![coinbase.clone(), sapling_and_orchard.clone(), second]),
        Err(ContextError::DuplicateNullifier {
            pool: Pool::Sapling,
            tx: 2
        })
    );

    // Orchard: the second action of transaction 1 reveals the nullifier of its first.
    let twice = patched(
        sapling_and_orchard,
        &orchard_nullifiers[1],
        &orchard_nullifiers[0],
        branch,
    );
    assert_eq!(
        check(vec![coinbase, twice]),
        Err(ContextError::DuplicateNullifier {
            pool: Pool::Orchard,
            tx: 1
        })
    );
}

/// The checkpoint hash replaces the scripts: a block with a wrong signature fails full
/// validation and passes the checkpoint path.
#[test]
fn the_checkpoint_path_runs_no_script() {
    let fixture = transparent_block(5, 2);
    let h = harness(&fixture);
    let view = h.chain.view();
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
    let mut block = RawBlock::parse(Bytes::from(bytes), fixture.branch_id).unwrap();
    block.header.merkle_root = merkle_root(&block.txids());
    assert!(matches!(
        validate_block(block.clone(), &h.store, &view, &h.cfg),
        Err(BlockError::Prepare {
            tx: 1,
            error: PrepareError::Script(0, _),
        })
    ));
    let checkpoints = checkpoint_at(&block, fixture.height);
    let (layer, _) = apply_checkpointed(&block, block.hash(), &view, &h.cfg, &checkpoints)
        .expect("the hash is the checkpoint");
    assert_eq!(layer.hash, block.hash());
}

/// A block of Zebra's Mainnet vectors.
fn mainnet_block(height: u32) -> RawBlock {
    let name = format!(
        "{}/tests/vectors/block-main-0-000-{height:03}.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let bytes = hex::decode(hex.trim()).expect("hex");
    let rules = rules_at(Network::Mainnet, height).expect("a rule set");
    RawBlock::parse(Bytes::from(bytes), rules.branch_id).expect("a block")
}

/// A chain whose base is the Mainnet genesis block. The directory holds the files of the
/// backing while the chain lives.
fn mainnet_at_genesis() -> (Chain, tempfile::TempDir) {
    let genesis = mainnet_block(0);
    assert_eq!(genesis.hash(), Network::Mainnet.params().genesis_hash);
    let dir = hayai_bench::scratch_dir();
    let (backing, _) = MemBacking::open(dir.path(), &MemConfig::default()).expect("a backing");
    let base = Base::new(Arc::new(backing), 0, genesis.hash(), genesis.header.time);
    (Chain::new(base), dir)
}

fn mainnet_config(h: &Harness, height: u32) -> ValidateConfig {
    ValidateConfig {
        rules: *rules_at(Network::Mainnet, height).expect("a rule set"),
        ..h.cfg.clone()
    }
}

/// Blocks 1 to 10 of Mainnet (published vectors) with the checkpoint list of Mainnet: the
/// blocks are below the checkpoint at height 400.
#[test]
fn the_first_mainnet_blocks_pass_the_checkpoint_path() {
    let h = harness(&transparent_block(0, 1));
    let (mut chain, _dir) = mainnet_at_genesis();
    let checkpoints = Network::Mainnet.checkpoints();
    let mut paid = 0;
    for height in 1..=10 {
        let block = mainnet_block(height);
        let cfg = mainnet_config(&h, height);
        let view = chain.view();
        if height == 2 {
            let child = mainnet_block(3).hash();
            assert!(matches!(
                apply_checkpointed(&block, child, &view, &cfg, checkpoints),
                Err(BlockError::NotOnCheckpointedChain { height: 2, .. })
            ));
        }
        let (layer, _) = apply_checkpointed(&block, block.hash(), &view, &cfg, checkpoints)
            .unwrap_or_else(|e| panic!("block {height}: {e}"));
        assert_eq!((layer.height, layer.hash), (height, block.hash()));
        paid += layer.created.values().map(|coin| coin.value).sum::<u64>();
        assert_eq!(layer.value_pools.transparent, paid);
        chain.push(layer).expect("the layer is the next one");
    }
    assert!(paid > 0);
}

/// Zakura's rule: full validation does not start at or below the mandatory checkpoint.
#[test]
fn full_validation_refuses_a_block_at_or_below_the_mandatory_checkpoint() {
    let h = harness(&transparent_block(0, 1));
    let (chain, _dir) = mainnet_at_genesis();
    let Err(BlockError::BelowMandatoryCheckpoint { height, mandatory }) = validate_block(
        mainnet_block(1),
        &h.store,
        &chain.view(),
        &mainnet_config(&h, 1),
    ) else {
        panic!("block 1 has no full validation");
    };
    assert_eq!((height, mandatory), (1, 1_046_399));

    // The last height of the rule, and the first height above it.
    let mandatory = Network::Mainnet.mandatory_checkpoint_height();
    for (base_height, refused) in [(mandatory - 1, true), (mandatory, false)] {
        let dir = hayai_bench::scratch_dir();
        let (backing, _) = MemBacking::open(dir.path(), &MemConfig::default()).expect("a backing");
        let parent = h.block.header.prev_hash;
        let chain = Chain::new(Base::new(Arc::new(backing), base_height, parent, 0));
        // The block is of another height: above the rule it fails a later rule.
        let Err(error) = validate_block(h.block.clone(), &h.store, &chain.view(), &h.cfg) else {
            panic!("the block is of another height");
        };
        assert_eq!(
            matches!(error, BlockError::BelowMandatoryCheckpoint { .. }),
            refused,
            "base height {base_height}: {error}"
        );
    }
}
