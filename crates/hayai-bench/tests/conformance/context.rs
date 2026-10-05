//! The chain context of a block vector.
//!
//! A block needs the state after its parent. The harness has only the vector set, so it
//! builds what the set gives and records the rest:
//!
//! - A block whose parent is the tip of the running chain uses that chain: the state that
//!   hayai built from the earlier vectors of the run.
//! - Every other block gets a new base at its parent ([`Seeded::fresh`]). The base holds the
//!   unspent outputs of the earlier vectors, the empty trees when the parent is before the
//!   activation of a pool, the published tree state or final root when the set has one, and
//!   the empty history tree when the parent is before Heartwood. The Sprout state is the
//!   empty state when the parent is before the first block with a JoinSplit. At a later
//!   parent the base does not know the Sprout state (`Base::set_sprout_unknown`).
//! - [`Seeded::analysis`] lists the context a block needs and the base cannot hold
//!   (`missing`), and the context the harness takes on trust (`assumed`).

use std::sync::Arc;

use hayai_coins::{Coin, CoinsView, MemBacking, MemConfig, OutPoint, Pool};
use hayai_consensus::{RuleSet, Upgrade};
use hayai_crypto::sapling_crypto::Node;
use hayai_crypto::zcash_primitives::merkle_tree::read_frontier_v0;
use hayai_state::{Base, Chain, ChainView, HistoryState, Map};
use hayai_trees::SaplingFrontier;
use hayai_wire::RawBlock;

use crate::vectors::{has_history_leaf, orchard_in_history_leaf, Net, VectorSet};

/// What the harness knows about one note commitment tree at the tip of a chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Knowledge {
    /// Nothing.
    Unknown,
    /// The root only: a block without outputs for the pool keeps it.
    Root,
    /// The frontier: hayai can append to the tree. Earlier anchors, the nullifier set and
    /// the pool value are unknown.
    Frontier,
    /// Everything: the chain started before the activation of the pool.
    Complete,
}

/// A chain whose tip is the parent of the next vector, and what the harness knows about it.
pub struct Seeded {
    pub chain: Chain,
    /// `Complete` or `Unknown`: no rule reads the Sprout root alone.
    pub sprout: Knowledge,
    pub sapling: Knowledge,
    pub orchard: Knowledge,
    /// The chain holds every unspent output: it started at the genesis block.
    pub coins_complete: bool,
    _dir: tempfile::TempDir,
}

/// The context a block needs that the chain does not hold, and the context taken on trust.
#[derive(Default)]
pub struct Analysis {
    pub missing: Vec<String>,
    pub assumed: Vec<String>,
}

/// The shielded content of a block that decides which context it needs.
struct Shielded {
    spends: usize,
    outputs: usize,
    /// Net value that leaves the pool, in zatoshis.
    balance: i128,
    /// Anchors that the view does not hold.
    unknown_anchors: usize,
}

fn sprout_content(raw: &RawBlock, view: &ChainView) -> Shielded {
    let mut content = Shielded {
        spends: 0,
        outputs: 0,
        balance: 0,
        unknown_anchors: 0,
    };
    for bundle in raw.txs.iter().filter_map(|t| t.tx.sprout_bundle()) {
        for joinsplit in &bundle.joinsplits {
            // Each JoinSplit reveals two nullifiers and adds two commitments.
            content.spends += 2;
            content.outputs += 2;
            content.balance += i128::from(i64::from(joinsplit.net_value()));
            // A treestate inside the transaction is not in the view. The count is used
            // only when the harness does not know the Sprout state.
            if !view.has_anchor(Pool::Sprout, joinsplit.anchor()) {
                content.unknown_anchors += 1;
            }
        }
    }
    content
}

fn sapling_content(raw: &RawBlock, view: &ChainView) -> Shielded {
    let mut content = Shielded {
        spends: 0,
        outputs: 0,
        balance: 0,
        unknown_anchors: 0,
    };
    for bundle in raw.txs.iter().filter_map(|t| t.tx.sapling_bundle()) {
        content.spends += bundle.shielded_spends().len();
        content.outputs += bundle.shielded_outputs().len();
        content.balance += i128::from(i64::from(*bundle.value_balance()));
        content.unknown_anchors += bundle
            .shielded_spends()
            .iter()
            .filter(|s| !view.has_anchor(Pool::Sapling, &s.anchor().to_bytes()))
            .count();
    }
    content
}

fn orchard_content(raw: &RawBlock, view: &ChainView) -> Shielded {
    let mut content = Shielded {
        spends: 0,
        outputs: 0,
        balance: 0,
        unknown_anchors: 0,
    };
    for bundle in raw.txs.iter().filter_map(|t| t.tx.orchard_bundle()) {
        // Each action reveals one nullifier and adds one commitment.
        content.spends += bundle.actions().len();
        content.outputs += bundle.actions().len();
        content.balance += i128::from(i64::from(*bundle.value_balance()));
        if !view.has_anchor(Pool::Orchard, &bundle.anchor().to_bytes()) {
            content.unknown_anchors += 1;
        }
    }
    content
}

/// Adds to `analysis` what the block needs from a pool of which the harness knows
/// `knowledge`. `root_checked` is true when a rule reads the root of the pool after the
/// block: the header commits to it, or the history leaf of the block holds it.
fn pool_analysis(
    pool: &str,
    content: &Shielded,
    knowledge: Knowledge,
    root_checked: bool,
    analysis: &mut Analysis,
) {
    if content.outputs > 0 && knowledge < Knowledge::Frontier {
        analysis.missing.push(format!("{pool} tree frontier"));
    } else if root_checked && knowledge < Knowledge::Root {
        analysis.missing.push(format!("{pool} tree root"));
    }
    if knowledge == Knowledge::Complete {
        return;
    }
    if content.unknown_anchors > 0 {
        analysis.missing.push(format!("{pool} anchors"));
    }
    if content.balance > 0 {
        analysis.missing.push(format!("{pool} pool value"));
    }
    if content.spends > 0 {
        analysis.assumed.push(format!(
            "the {pool} nullifiers are not in the nullifier set"
        ));
    }
}

impl Seeded {
    /// A chain with a new base at the parent of `raw`, the block of `net` at `height`.
    /// `coins` are the unspent outputs of the earlier vectors.
    pub fn fresh(
        set: &VectorSet,
        net: Net,
        height: u32,
        raw: &RawBlock,
        coins: &Map<OutPoint, Coin>,
    ) -> Self {
        let parent = height - 1;
        let dir = hayai_bench::scratch_dir();
        let config = MemConfig {
            fsync_every_generations: 0,
        };
        let (backing, _) = MemBacking::open(dir.path(), &config).expect("open the coin store");
        // The parent time is in the set only when the parent is. No rule of `validate_block`
        // reads it.
        let time = set
            .header(net, parent)
            .map_or(raw.header.time.saturating_sub(1), |h| h.time);
        let mut base = Base::new(Arc::new(backing), parent, raw.header.prev_hash, time);
        for (outpoint, coin) in coins {
            base.coins
                .add(outpoint.clone(), coin.clone())
                .expect("the tracked outpoints are distinct");
        }

        let has_sapling_outputs = raw
            .txs
            .iter()
            .filter_map(|t| t.tx.sapling_bundle())
            .any(|b| !b.shielded_outputs().is_empty());
        let has_orchard_actions = raw.txs.iter().any(|t| t.tx.orchard_bundle().is_some());
        // The root after the parent: the published root of the parent, or the published root
        // of the block when the block does not change the tree.
        let published = set.roots(net, parent);
        let own = set.roots(net, height);
        let sapling_root = published
            .sapling
            .or(own.sapling.filter(|_| !has_sapling_outputs));
        let orchard_root = published
            .orchard
            .or(own.orchard.filter(|_| !has_orchard_actions));

        let sprout = if parent < net.first_joinsplit_height() {
            Knowledge::Complete
        } else {
            base.set_sprout_unknown();
            Knowledge::Unknown
        };
        let sapling = if parent < net.activation(Upgrade::Sapling) {
            Knowledge::Complete
        } else if let Some(tree) = set.sapling_tree_state(net, parent) {
            let frontier = read_frontier_v0::<Node, _>(tree).expect("a zcashd tree state");
            let frontier = SaplingFrontier::from_frontier(frontier);
            let root = frontier.root().to_bytes();
            base.sapling_frontier = Arc::new(frontier);
            base.anchors.sapling = root;
            base.insert_anchor(Pool::Sapling, root);
            Knowledge::Frontier
        } else if let Some(root) = sapling_root {
            base.anchors.sapling = root;
            base.insert_anchor(Pool::Sapling, root);
            Knowledge::Root
        } else {
            Knowledge::Unknown
        };
        let orchard = if parent < net.activation(Upgrade::Nu5) {
            Knowledge::Complete
        } else if let Some(root) = orchard_root {
            base.anchors.orchard = root;
            base.insert_anchor(Pool::Orchard, root);
            Knowledge::Root
        } else {
            Knowledge::Unknown
        };

        let parent_rules = net.rules(parent);
        if !has_history_leaf(parent_rules) {
            base.history = Some(Arc::new(HistoryState::empty(parent_rules.branch_id)));
        }
        Self {
            chain: Chain::new(base),
            sprout,
            sapling,
            orchard,
            coins_complete: parent == 0,
            _dir: dir,
        }
    }

    /// The context that `raw`, a block under `rules`, needs on top of this chain.
    pub fn analysis(&self, raw: &RawBlock, rules: &RuleSet, view: &ChainView) -> Analysis {
        let mut analysis = Analysis::default();
        if !self.coins_complete {
            let created = hayai_state::block_outputs(raw, view.tip_height() + 1);
            let outpoints: Vec<OutPoint> = raw
                .txs
                .iter()
                .filter_map(|t| t.tx.transparent_bundle())
                .filter(|b| !b.is_coinbase())
                .flat_map(|b| b.vin.iter().map(|i| i.prevout().clone()))
                .filter(|o| !created.contains_key(o))
                .collect();
            let found = view.get_coins(&outpoints).iter().flatten().count();
            let absent = outpoints.len() - found;
            if absent > 0 {
                analysis
                    .missing
                    .push(format!("spent coins of {absent} inputs"));
            }
        }
        // From Heartwood the history leaf of the block holds the roots, and the header of the
        // next block commits to the leaf. The leaf exists only when the parent tree is known.
        let leaf = match (has_history_leaf(rules), view.history()) {
            (false, _) => false,
            (true, Some(_)) => true,
            (true, None) => {
                analysis.assumed.push(
                    "the header commitment is not checked: the history tree of the parent is \
                     unknown"
                        .to_string(),
                );
                false
            }
        };
        // No header and no history leaf commits to the Sprout root.
        pool_analysis(
            "Sprout",
            &sprout_content(raw, view),
            self.sprout,
            false,
            &mut analysis,
        );
        let sapling_in_header = matches!(rules.upgrade, Upgrade::Sapling | Upgrade::Blossom);
        let orchard_in_leaf = leaf && orchard_in_history_leaf(rules);
        pool_analysis(
            "Sapling",
            &sapling_content(raw, view),
            self.sapling,
            sapling_in_header || leaf,
            &mut analysis,
        );
        pool_analysis(
            "Orchard",
            &orchard_content(raw, view),
            self.orchard,
            orchard_in_leaf,
            &mut analysis,
        );
        analysis
    }
}
