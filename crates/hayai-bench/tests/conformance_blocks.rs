//! Block conformance: every published block vector through hayai's validation path.
//!
//! The vectors are Zebra's block test vectors (`tests/vectors/`, inventory in
//! `docs/conformance.md`). Each vector runs these stages in order and stops at the first
//! one that fails:
//!
//! 1. `parse`: `RawBlock::parse` under the branch of the height, and agreement of the layout
//!    scanner with the sequential parser.
//! 2. `header`: the genesis hash, the hash link to the next vector, and the proof of work
//!    of `hayai_consensus::header::check_proof_of_work` (the solution length, the target at
//!    or below the limit of the network, the hash at or below the target, Equihash).
//! 3. `merkle_root`: the header root against the transaction ids.
//! 4. `transactions`: `draft`, the scripts and the shielded batch of every transaction
//!    whose spent coins the harness holds. A transaction of a version before Sapling in a
//!    block at or below the mandatory checkpoint does not run: hayai has no verification
//!    for it, and the node applies such a block on the checkpoint path only.
//! 5. `context`: `hayai_validate::validate_block` on a chain whose tip is the parent
//!    (`conformance/context.rs`). The contextual header rules run on the context that the
//!    chain holds. No range of the set has the 28 blocks that the difficulty rule reads
//!    above height 17, so a rule whose context is too short does not run
//!    (`HeaderPolicy::TrustShortContext`). The stage runs only when the harness holds every part of
//!    the state that the block reads. The genesis block has no parent state and stops
//!    after stage 4. A block at or below the mandatory checkpoint of its network has no
//!    full validation: the stage runs `hayai_validate::apply_checkpointed` with the
//!    checkpoint list of the network.
//! 6. `final_roots`: the tree roots of the layer against the published roots.
//!
//! The vectors of a network run in height order. A valid block becomes the tip of the
//! chain, so a contiguous range runs on the state that hayai built.
//!
//! The test compares each outcome with `tests/vectors/expected-blocks.json`
//! (`conformance/expected.rs`) and writes the full results to
//! `target/conformance/blocks.results.json`.

#[path = "conformance/context.rs"]
mod context;
#[path = "conformance/expected.rs"]
mod expected;
#[path = "conformance/vectors.rs"]
mod vectors;

use std::collections::BTreeMap;
use std::sync::Arc;

use hayai_coins::{Coin, CoinsView, OutPoint};
use hayai_consensus::header::check_proof_of_work;
use hayai_consensus::{RuleSet, Upgrade};
use hayai_prepared::{
    check_scripts, draft, PrepareError, PreparedStore, RuleEpoch, ScopedBatch, VerifyingKeys,
};
use hayai_state::{
    block_outputs, ChainView, ContextError, HistoryError, HistoryLeaf, HistoryState, Layer, Map,
};
use hayai_template::Zip317Params;
use hayai_validate::{
    apply_checkpointed, validate_block, BlockError, HeaderPolicy, ValidateConfig,
};
use hayai_wire::{merkle_root, RawBlock, RawTx};
use serde::Serialize;

use context::{Analysis, Knowledge, Seeded};
use expected::{Outcome, Verdict};
use vectors::{has_history_leaf, orchard_in_history_leaf, BlockVector, Net, VectorSet};

/// Byte budget of the prepared store of a validation. The store stays empty.
const STORE_LIMIT: usize = 1 << 20;

/// How much parent state a vector can get from the set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
enum Class {
    /// The genesis block: no parent state exists.
    Genesis,
    /// The parent or the child of the block is a vector.
    Range,
    /// No neighbour of the block is a vector.
    Isolated,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Genesis => "genesis",
            Class::Range => "range",
            Class::Isolated => "isolated",
        }
    }
}

/// The machine-readable result of one vector.
#[derive(Serialize)]
struct BlockResult {
    vector: String,
    network: &'static str,
    height: u32,
    upgrade: String,
    class: Class,
    /// False for a vector that Zebra publishes as an invalid block.
    published_valid: bool,
    transactions: usize,
    /// Transactions that the `transactions` stage prepared.
    prepared: usize,
    /// Transactions that the `transactions` stage did not prepare: the harness does not
    /// hold their spent coins.
    without_coins: usize,
    /// Transactions that the `transactions` stage did not prepare: a version before
    /// Sapling in a block that has only the checkpoint path.
    checkpoint_only: usize,
    #[serde(flatten)]
    outcome: Outcome,
    /// Context the block needs and the vector set cannot supply.
    missing: Vec<String>,
    /// Context the harness takes on trust.
    assumed: Vec<String>,
}

/// The state that the vectors of one network share.
#[derive(Default)]
struct NetworkState {
    /// The chain whose tip is the last valid vector.
    chain: Option<Seeded>,
    /// The unspent outputs of the vectors that ran.
    coins: Map<OutPoint, Coin>,
}

fn stop(stage: &str, verdict: Verdict, error: impl ToString) -> Outcome {
    Outcome {
        stage: stage.to_string(),
        verdict,
        error: Some(error.to_string()),
    }
}

/// The verdict of a transaction that hayai did not prepare.
fn prepare_verdict(error: &PrepareError) -> Verdict {
    match error {
        PrepareError::Unsupported(_) => Verdict::Unsupported,
        _ => Verdict::Rejected,
    }
}

/// The verdict of a block that `validate_block` did not accept.
fn block_verdict(error: &BlockError) -> Verdict {
    match error {
        BlockError::Prepare { error, .. } => prepare_verdict(error),
        BlockError::Context(ContextError::History(HistoryError::Unsupported(_))) => {
            Verdict::Unsupported
        }
        _ => Verdict::Rejected,
    }
}

/// Stage 1: both parsers read the block and agree on every transaction.
fn parse(vector: &BlockVector, rules: &RuleSet) -> Result<RawBlock, String> {
    let branch = rules.branch_id;
    let raw = RawBlock::parse(vector.bytes.clone(), branch).map_err(|e| e.to_string())?;
    let sequential = RawBlock::parse_sequential(vector.bytes.clone(), branch)
        .map_err(|e| format!("sequential parser: {e}"))?;
    let same = raw.header == sequential.header
        && raw.txs.len() == sequential.txs.len()
        && raw
            .txs
            .iter()
            .zip(&sequential.txs)
            .all(|(a, b)| a.wtxid() == b.wtxid() && a.bytes == b.bytes);
    if !same {
        return Err("the layout scanner and the sequential parser disagree".to_string());
    }
    Ok(raw)
}

/// Stage 2: the context-free header rules.
fn check_header(vector: &BlockVector, set: &VectorSet, raw: &RawBlock) -> Result<(), String> {
    let network = vector.net.network();
    let params = network.params();
    let hash = raw.hash();
    if vector.height == 0 && hash != params.genesis_hash {
        return Err(format!("genesis hash is {hash}"));
    }
    if let Some(next) = set
        .header(vector.net, vector.height + 1)
        .filter(|_| !vector.invalid)
    {
        if next.prev_hash != hash {
            return Err(format!("the next vector does not build on the hash {hash}"));
        }
    }
    check_proof_of_work(network, &raw.header).map_err(|e| e.to_string())
}

/// The coins that `tx` spends, from the outputs of its block or from `view`. `None` when the
/// harness does not hold one of them.
fn spent_coins(
    tx: &RawTx,
    created: &Map<OutPoint, Coin>,
    view: Option<&ChainView>,
) -> Option<Vec<Coin>> {
    let Some(bundle) = tx.tx.transparent_bundle().filter(|b| !b.is_coinbase()) else {
        return Some(Vec::new());
    };
    bundle
        .vin
        .iter()
        .map(|txin| {
            created
                .get(txin.prevout())
                .cloned()
                .or_else(|| view.and_then(|v| v.get_coin(txin.prevout())))
        })
        .collect()
}

/// The counts and the error of stage 4.
struct Transactions {
    prepared: usize,
    without_coins: usize,
    checkpoint_only: usize,
    error: Option<(Verdict, String)>,
}

/// Stage 4: the context-free rules of every transaction whose spent coins the harness holds.
///
/// The stage runs every transaction, so that an unsupported transaction does not hide an
/// invalid one. The reported error is the first rejection, or the first unsupported rule
/// when hayai rejects nothing. The order is the stage order of `validate_block` (draft,
/// scripts, shielded batch), then the block position.
///
/// `checkpoint_path` tells that the block is at or below the mandatory checkpoint. hayai
/// then applies it on the checkpoint path, and has no verification for a transaction of a
/// version before Sapling (v1 to v3): the stage counts such a transaction and does not
/// prepare it. Above the mandatory checkpoint every transaction runs.
fn check_transactions(
    raw: &RawBlock,
    height: u32,
    checkpoint_path: bool,
    epoch: RuleEpoch,
    view: Option<&ChainView>,
    keys: &VerifyingKeys,
) -> Transactions {
    let created = block_outputs(raw, height);
    let mut drafts = Vec::new();
    let mut positions = Vec::new();
    let mut without_coins = 0;
    let mut checkpoint_only = 0;
    let mut errors = Vec::new();
    let mut record = |at: usize, e: &PrepareError| {
        errors.push((prepare_verdict(e), format!("transaction {at}: {e}")));
    };
    for (i, tx) in raw.txs.iter().enumerate() {
        if checkpoint_path && !tx.tx.version().has_sapling() {
            checkpoint_only += 1;
            continue;
        }
        let Some(coins) = spent_coins(tx, &created, view) else {
            without_coins += 1;
            continue;
        };
        match draft(tx.clone(), epoch, coins) {
            Ok(d) => {
                drafts.push(d);
                positions.push(i);
            }
            Err(e) => record(i, &e),
        }
    }
    if let Err((k, e)) = check_scripts(&drafts) {
        record(positions[k], &e);
    }
    let mut batch = ScopedBatch::new(keys);
    for (d, i) in drafts.iter().zip(&positions) {
        if let Err(e) = d.add_shielded(&mut batch) {
            record(*i, &e);
        }
    }
    let failed = batch.finalize().failed;
    if !failed.is_empty() {
        let at: Vec<usize> = raw
            .txs
            .iter()
            .enumerate()
            .filter(|(_, t)| failed.contains(&t.wtxid()))
            .map(|(i, _)| i)
            .collect();
        errors.push((
            Verdict::Rejected,
            format!("the shielded bundles of transactions {at:?} are invalid"),
        ));
    }
    let rejection = errors.iter().position(|(v, _)| *v == Verdict::Rejected);
    let error = match (rejection, errors.is_empty()) {
        (Some(at), _) => Some(errors.swap_remove(at)),
        (None, false) => Some(errors.swap_remove(0)),
        (None, true) => None,
    };
    Transactions {
        prepared: drafts.len(),
        without_coins,
        checkpoint_only,
        error,
    }
}

/// Stage 6: the roots of `layer` against the published roots of the vector.
fn check_final_roots(
    vector: &BlockVector,
    set: &VectorSet,
    seeded: &Seeded,
    layer: &Layer,
) -> Result<(), String> {
    let published = set.roots(vector.net, vector.height);
    let pools = [
        (
            "Sprout",
            seeded.sprout,
            published.sprout,
            layer.sprout_frontier.root(),
        ),
        (
            "Sapling",
            seeded.sapling,
            published.sapling,
            layer.anchors.sapling,
        ),
        (
            "Orchard",
            seeded.orchard,
            published.orchard,
            layer.anchors.orchard,
        ),
    ];
    for (pool, knowledge, published, actual) in pools {
        let Some(published) = published.filter(|_| knowledge > Knowledge::Unknown) else {
            continue;
        };
        if published != actual {
            return Err(format!(
                "final {pool} root {} differs from the published root {}",
                hex::encode(actual),
                hex::encode(published)
            ));
        }
    }
    Ok(())
}

/// The history tree after the first block of an upgrade, when the tree of the parent is
/// unknown: an upgrade starts a new tree, so the tree has the leaf of this block only.
fn history_after_activation(
    vector: &BlockVector,
    raw: &RawBlock,
    seeded: &Seeded,
    layer: &Layer,
) -> Option<Arc<HistoryState>> {
    let rules = vector.net.rules(vector.height);
    let parent_rules = vector.net.rules(vector.height - 1);
    let roots_known = seeded.sapling > Knowledge::Unknown
        && (!orchard_in_history_leaf(rules) || seeded.orchard > Knowledge::Unknown);
    if rules.upgrade == parent_rules.upgrade || !has_history_leaf(rules) || !roots_known {
        return None;
    }
    let leaf = HistoryLeaf::from_block(raw, vector.height, &layer.anchors);
    let tree = HistoryState::empty(parent_rules.branch_id)
        .append(rules.branch_id, &leaf)
        .expect("a first leaf starts a tree");
    Some(Arc::new(tree))
}

/// Runs every stage of `vector` and fills the counts and the context lists of `result`.
fn stages(
    vector: &BlockVector,
    set: &VectorSet,
    state: &mut NetworkState,
    keys: &Arc<VerifyingKeys>,
    result: &mut BlockResult,
) -> Outcome {
    let net = vector.net;
    let height = vector.height;
    let rules = net.rules(height);
    let epoch = RuleEpoch::of(rules);

    let raw = match parse(vector, rules) {
        Ok(raw) => raw,
        Err(e) => return stop("parse", Verdict::Rejected, e),
    };
    result.transactions = raw.txs.len();
    if let Err(e) = check_header(vector, set, &raw) {
        return stop("header", Verdict::Rejected, e);
    }
    if merkle_root(&raw.txids()) != raw.header.merkle_root {
        return stop(
            "merkle_root",
            Verdict::Rejected,
            "the header root differs from the root of the transaction ids",
        );
    }

    // The chain whose tip is the parent. A published-invalid vector never takes the running
    // chain: the vectors after it continue on the state of the valid blocks.
    let seeded = match height {
        0 => None,
        _ => Some(
            state
                .chain
                .take_if(|s| !vector.invalid && s.chain.tip().hash == raw.header.prev_hash)
                .unwrap_or_else(|| Seeded::fresh(set, net, height, &raw, &state.coins)),
        ),
    };
    let view = seeded.as_ref().map(|s| s.chain.view());

    let checkpoint_path = height <= net.network().mandatory_checkpoint_height();
    let transactions =
        check_transactions(&raw, height, checkpoint_path, epoch, view.as_ref(), keys);
    result.prepared = transactions.prepared;
    result.without_coins = transactions.without_coins;
    result.checkpoint_only = transactions.checkpoint_only;
    if let Some((verdict, error)) = transactions.error {
        return stop("transactions", verdict, error);
    }
    let (Some(mut seeded), Some(view)) = (seeded, view) else {
        return Outcome {
            stage: "transactions".to_string(),
            verdict: Verdict::Valid,
            error: None,
        };
    };

    let Analysis { missing, assumed } = seeded.analysis(&raw, rules, &view);
    result.assumed = assumed;
    if !missing.is_empty() {
        let error = format!("needs context: {}", missing.join(", "));
        result.missing = missing;
        return stop("context", Verdict::ContextFree, error);
    }
    let cfg = ValidateConfig {
        network: net.network(),
        rules: *rules,
        keys: keys.clone(),
        header: HeaderPolicy::TrustShortContext,
    };
    let store = PreparedStore::new(epoch, STORE_LIMIT, Zip317Params::ZIP317);
    // At or below the mandatory checkpoint a block has only the checkpoint path, with the
    // checkpoint list of the network.
    let network = net.network();
    let validated = if checkpoint_path {
        apply_checkpointed(&raw, raw.hash(), &view, &cfg, network.checkpoints())
    } else {
        validate_block(raw.clone(), &store, &view, &cfg)
    };
    let mut layer = match validated {
        Ok((layer, _)) => layer,
        Err(e) => return stop("context", block_verdict(&e), e),
    };
    if let Err(e) = check_final_roots(vector, set, &seeded, &layer) {
        return stop("final_roots", Verdict::Rejected, e);
    }
    if !vector.invalid {
        let history = layer.history.take();
        layer.history = history.or_else(|| history_after_activation(vector, &raw, &seeded, &layer));
        seeded.chain.push(layer).expect("the layer extends the tip");
        state.chain = Some(seeded);
    }
    Outcome {
        stage: "final_roots".to_string(),
        verdict: Verdict::Valid,
        error: None,
    }
}

/// Runs `vector` and records its outputs in the coin set of the network.
fn run(
    vector: &BlockVector,
    set: &VectorSet,
    state: &mut NetworkState,
    keys: &Arc<VerifyingKeys>,
) -> BlockResult {
    let neighbour = |height: Option<u32>| height.and_then(|h| set.header(vector.net, h));
    let class = match vector.height {
        0 => Class::Genesis,
        h => match (neighbour(h.checked_sub(1)), neighbour(h.checked_add(1))) {
            (None, None) => Class::Isolated,
            _ => Class::Range,
        },
    };
    let mut result = BlockResult {
        vector: vector.name.clone(),
        network: vector.net.name(),
        height: vector.height,
        upgrade: format!("{:?}", vector.net.rules(vector.height).upgrade),
        class,
        published_valid: !vector.invalid,
        transactions: 0,
        prepared: 0,
        without_coins: 0,
        checkpoint_only: 0,
        outcome: stop("parse", Verdict::Rejected, "the vector did not run"),
        missing: Vec::new(),
        assumed: Vec::new(),
    };
    result.outcome = stages(vector, set, state, keys, &mut result);

    // The outputs of a published-valid block are coins of the real chain, whatever hayai
    // decided: later vectors can spend them.
    if !vector.invalid {
        if let Ok(raw) = RawBlock::parse(
            vector.bytes.clone(),
            vector.net.rules(vector.height).branch_id,
        ) {
            for txin in raw
                .txs
                .iter()
                .filter_map(|t| t.tx.transparent_bundle())
                .flat_map(|b| &b.vin)
            {
                state.coins.remove(txin.prevout());
            }
            // The outputs of the genesis coinbase are not spendable (zcashd never connects
            // the genesis block).
            if vector.height > 0 {
                state.coins.extend(block_outputs(&raw, vector.height));
            }
        }
    }
    result
}

/// The summary table: one row for each class, one column for each verdict.
fn summary(results: &[BlockResult]) -> String {
    let verdicts = [
        (Verdict::Valid, "valid"),
        (Verdict::ContextFree, "context_free"),
        (Verdict::Unsupported, "unsupported"),
        (Verdict::Rejected, "rejected"),
    ];
    let mut counts: BTreeMap<(Class, Verdict), usize> = BTreeMap::new();
    for r in results {
        *counts.entry((r.class, r.outcome.verdict)).or_default() += 1;
    }
    let mut table = format!("{:<10}", "class");
    for (_, name) in verdicts {
        table.push_str(&format!("{name:>14}"));
    }
    table.push_str(&format!("{:>8}\n", "total"));
    let mut row = |name: &str, filter: &dyn Fn(Class) -> bool| {
        table.push_str(&format!("{name:<10}"));
        let mut total = 0;
        for (v, _) in verdicts {
            let n: usize = counts
                .iter()
                .filter(|((c, verdict), _)| filter(*c) && *verdict == v)
                .map(|(_, n)| n)
                .sum();
            total += n;
            table.push_str(&format!("{n:>14}"));
        }
        table.push_str(&format!("{total:>8}\n"));
    };
    for class in [Class::Genesis, Class::Range, Class::Isolated] {
        row(class.name(), &|c| c == class);
    }
    row("all", &|_| true);

    let count = |f: &dyn Fn(&BlockResult) -> bool| results.iter().filter(|r| f(r)).count();
    let verdict = |r: &BlockResult, v: Verdict| r.outcome.verdict == v;
    table.push_str(&format!(
        "vectors total: {}\n\
         fully validated: {}\n\
         validated with assumed context: {}\n\
         context-free only: {}\n\
         expected unsupported: {}\n\
         rejected: {} ({} published as invalid)\n",
        results.len(),
        count(&|r| verdict(r, Verdict::Valid) && r.assumed.is_empty()),
        count(&|r| verdict(r, Verdict::Valid) && !r.assumed.is_empty()),
        count(&|r| verdict(r, Verdict::ContextFree)),
        count(&|r| verdict(r, Verdict::Unsupported)),
        count(&|r| verdict(r, Verdict::Rejected)),
        count(&|r| verdict(r, Verdict::Rejected) && !r.published_valid),
    ));
    table
}

/// The verifying keys of the harness. The Orchard blocks of the set are NU5 blocks. The
/// key is built before any batch. The Sapling keys are the keys that hayai embeds.
fn keys() -> Arc<VerifyingKeys> {
    let Some(nu5) = RuleSet::of(Upgrade::Nu5) else {
        unreachable!("NU5 has a rule set");
    };
    let keys = VerifyingKeys::prebuild(RuleEpoch::of(nu5), None);
    keys.ready();
    keys
}

#[test]
fn block_vectors_match_the_expected_outcomes() {
    let set = VectorSet::load();
    let keys = keys();

    let mut results = Vec::with_capacity(set.blocks.len());
    for net in Net::ALL {
        let mut state = NetworkState::default();
        for vector in set.blocks.iter().filter(|b| b.net == net) {
            results.push(run(vector, &set, &mut state, &keys));
        }
    }

    let dir = expected::results_dir();
    let mut text = serde_json::to_string_pretty(&results).expect("serialize the results");
    text.push('\n');
    std::fs::write(dir.join("blocks.results.json"), text).expect("write the results");
    let summary = summary(&results);
    std::fs::write(dir.join("blocks.summary.txt"), &summary).expect("write the summary");
    println!("{summary}\nresults: {}", dir.display());

    let actual: Vec<(String, Outcome)> = results
        .iter()
        .map(|r| (r.vector.clone(), r.outcome.clone()))
        .collect();
    let differences = expected::differences("expected-blocks.json", &actual);
    assert!(
        differences.is_empty(),
        "outcomes differ from tests/vectors/expected-blocks.json:\n{}",
        differences.join("\n")
    );
}

/// The harness rejects a changed vector at the stage that reads the changed bytes. The
/// vector is a Testnet NU5 block with a coinbase and one shielded transaction, so every
/// transaction runs without chain context.
#[test]
fn changed_vectors_stop_at_the_stage_of_the_change() {
    let set = VectorSet::load();
    let keys = keys();
    let Some(vector) = set.blocks.iter().find(|b| b.name == "test-1-842-467") else {
        panic!("the vector set has no test-1-842-467");
    };
    let outcome_with_flip = |at: usize| {
        let mut bytes = vector.bytes.to_vec();
        bytes[at] ^= 0x01;
        let changed = BlockVector {
            name: format!("{}-changed-{at}", vector.name),
            net: vector.net,
            height: vector.height,
            invalid: true,
            bytes: bytes.into(),
        };
        run(&changed, &set, &mut NetworkState::default(), &keys).outcome
    };
    let rejected_at = |outcome: &Outcome, stage: &str| {
        assert_eq!(
            (outcome.stage.as_str(), outcome.verdict),
            (stage, Verdict::Rejected),
            "{outcome:?}"
        );
    };

    let unchanged = run(vector, &set, &mut NetworkState::default(), &keys);
    assert_eq!(unchanged.outcome.verdict, Verdict::ContextFree);
    assert_eq!((unchanged.prepared, unchanged.without_coins), (2, 0));

    // The first byte of the nonce: the Equihash solution no longer fits the header.
    let nonce = hayai_wire::header::BlockHeader::EQUIHASH_INPUT_LEN;
    rejected_at(&outcome_with_flip(nonce), "header");

    // The lock time of the coinbase (after the header, the transaction count and three
    // 4-byte fields of a v5 transaction): the transaction id changes.
    let raw = RawBlock::parse(
        vector.bytes.clone(),
        vector.net.rules(vector.height).branch_id,
    )
    .expect("the vector parses");
    let lock_time = raw.header.serialized_len() + 1 + 12;
    rejected_at(&outcome_with_flip(lock_time), "merkle_root");

    // The last byte of the block is in the Orchard binding signature of the last
    // transaction. The transaction id of a v5 transaction does not cover it.
    let outcome = outcome_with_flip(vector.bytes.len() - 1);
    rejected_at(&outcome, "transactions");
    assert_eq!(
        outcome.error.as_deref(),
        Some("the shielded bundles of transactions [1] are invalid")
    );
}
