//! Layered chain state and contextual validation.
//!
//! Contract: `docs/architecture.md`, section hayai-state, and `docs/consensus-rules.md`.
//!
//! - A [`Layer`] is the state delta of one block. A [`Chain`] is a [`Base`] (the finalized
//!   state: coins cache, nullifier sets, tip trees) plus a window of layers. Commit is an Arc
//!   push, reorg is a pop, and [`Chain::finalize_excess`] merges the oldest layer into the
//!   base once the window is exceeded.
//! - A [`ChainView`] is a cheap clone of the chain handed to validators and the template.
//!   A lookup is one probe in the window index (`window.rs`: the newest contribution of
//!   each outpoint and the oldest layer of each nullifier across the window), then the base,
//!   in one batched round. The index describes the committed tip. A view first walks its
//!   layers above that tip (its speculative layers, or the layers a pop removed after the
//!   view was taken). A view that does not hold the index's tip walks all its layers newest
//!   first; that walk is also the reference the index is tested against.
//! - [`contextual_check`] applies every contextual rule to a prepared block against a view
//!   and builds the block's layer, with the ZIP 221 history tree after the block
//!   ([`history`]).
//! - [`checkpoint_layer`] builds the layer of a block at or below the last checkpoint: the
//!   state update of [`contextual_check`] without the rules that the checkpoint hash
//!   replaces.
//! - [`prebuild_body`] does the contextual work of a block body (every transaction after
//!   the coinbase) before the block exists. [`PrebuiltBody::commit`] then applies the
//!   header and coinbase rules to a block with that body and builds its layer from the
//!   prebuilt state, with no further pass over the body.
//! - Speculative layers: [`Chain::push_speculative`] puts a layer whose block is not yet
//!   fully verified (scripts and proofs) on top of the committed tip.
//!   [`Chain::view_speculative`] includes them, [`Chain::view`] does not.
//!   [`Chain::confirm`] commits a layer once its own verification and the verification of
//!   every speculative ancestor succeeded. [`Chain::reject`] drops a layer and every
//!   speculative layer above it. The window index never holds a speculative layer:
//!   a view walks the speculative layers, then probes the index.
//!
//! The Sprout state is the tip frontier, the final treestate of every block by root (a
//! JoinSplit continues the tree of its anchor) and the Sprout nullifier set.
//!
//! Finalized anchors are kept in memory (one set per pool: Sapling, Orchard, Ironwood;
//! seeded with the empty tree root, which zcashd's `GetSaplingAnchorAt` /
//! `GetOrchardAnchorAt` treat as always present). A
//! node restarting from disk reloads them through [`Base::insert_anchor`]; persisting them
//! is a hayai-coins concern left for the blockstore phase.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use hayai_coins::{
    BestBlock, Coin, CoinsBacking, CoinsCache, CoinsView, FlushGeneration, FlushStats,
    NullifierStore, OutPoint, Pool,
};
use hayai_consensus::DIFFICULTY_CONTEXT_BLOCKS;
use hayai_trees::{IronwoodFrontier, OrchardFrontier, SaplingFrontier, SproutFrontier};
use hayai_wire::header::BlockHash;
use hayai_wire::WtxId;
use parking_lot::RwLock;

mod check;
pub mod history;
mod window;

pub use check::{
    block_outputs, check_parent, check_sprout_anchors, checkpoint_layer, contextual_check,
    contextual_check_with_outputs, prebuild_body, resolve_inputs, CheckConfig, Checked,
    ContextError, ContextTimings, PrebuiltBody, PreparedBlock, SwapError,
};
pub use history::{HistoryError, HistoryLeaf, HistoryState};
use window::WindowIndex;

/// Hash map type of the layer indexes (ahash with random keys).
pub type Map<K, V> = HashMap<K, V, ahash::RandomState>;
/// Hash set type of the layer indexes.
pub type Set<K> = HashSet<K, ahash::RandomState>;

pub use hayai_consensus::{COINBASE_MATURITY, MEDIAN_TIME_SPAN};

/// Committed layers a chain keeps above the finalized base: the reorganization depth. A
/// node passes it to [`Chain::finalize_excess`].
pub const LAYER_WINDOW: usize = hayai_consensus::FINALITY_DEPTH as usize;

/// Roots of the note commitment trees after a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchors {
    pub sapling: [u8; 32],
    pub orchard: [u8; 32],
    /// Root of the Ironwood tree. Before NU6.3 it is the root of the empty tree.
    pub ironwood: [u8; 32],
}

impl Anchors {
    pub fn get(&self, pool: Pool) -> Option<[u8; 32]> {
        match pool {
            Pool::Sapling => Some(self.sapling),
            Pool::Orchard => Some(self.orchard),
            Pool::Ironwood => Some(self.ironwood),
            Pool::Sprout => None,
        }
    }
}

/// The note commitment frontiers after a block, with their roots.
#[derive(Clone, Debug)]
pub struct Frontiers {
    pub orchard: Arc<OrchardFrontier>,
    pub sapling: Arc<SaplingFrontier>,
    pub ironwood: Arc<IronwoodFrontier>,
    /// The Sprout frontier. Its root is not in `anchors`: no header commits to it.
    pub sprout: Arc<SproutFrontier>,
    pub anchors: Anchors,
}

/// Chain value pool balances in zatoshis after a block. The contextual check maintains
/// every balance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValuePools {
    pub transparent: u64,
    pub sprout: u64,
    pub sapling: u64,
    pub orchard: u64,
    pub ironwood: u64,
    /// The deferred pool (lockbox, ZIP 1015 / ZIP 2001).
    pub deferred: u64,
}

impl ValuePools {
    /// The total of the pools (`IssuedSupply`, protocol specification §4.17). The NSM
    /// value balance of NU7 is the scheduled issuance minus this total
    /// (`hayai_consensus::nsm`).
    pub fn total(&self) -> u64 {
        [
            self.transparent,
            self.sprout,
            self.sapling,
            self.orchard,
            self.ironwood,
            self.deferred,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// The state delta of one block over its parent.
#[derive(Debug)]
pub struct Layer {
    pub height: u32,
    pub hash: BlockHash,
    pub parent: BlockHash,
    pub time: u32,
    /// `nBits` of the block header.
    pub bits: u32,
    pub wtxids: Vec<WtxId>,
    /// Every output the block created, including those spent later in the same block.
    pub created: Map<OutPoint, Coin>,
    /// Every outpoint the block spent, including ones created in the same block.
    pub spent: Set<OutPoint>,
    /// The coin of each transparent input, by transaction (`spent_coins[i][j]` is input `j`
    /// of transaction `i`; [`resolve_inputs`]). The validation read these coins, so a
    /// consumer of the block (the wallet index of hayaid) needs no second read. A holder of
    /// many layers takes the list out before it keeps the layer: the chain does not read it.
    pub spent_coins: Vec<Vec<Coin>>,
    /// Nullifiers revealed, indexed by [`Pool::index`].
    pub nullifiers: [Set<[u8; 32]>; 4],
    pub orchard_frontier: Arc<OrchardFrontier>,
    pub sapling_frontier: Arc<SaplingFrontier>,
    /// The Ironwood frontier after the block. Before NU6.3 it is the empty tree.
    pub ironwood_frontier: Arc<IronwoodFrontier>,
    /// The Sprout frontier after the block: a valid anchor of a later JoinSplit. On a base
    /// that does not know the Sprout state ([`Base::set_sprout_unknown`]) it is the empty
    /// tree, and no block with a JoinSplit is accepted.
    pub sprout_frontier: Arc<SproutFrontier>,
    pub anchors: Anchors,
    pub value_pools: ValuePools,
    /// The ZIP 221 history tree after the block. `None` when the node does not know the
    /// history state of the parent and the block is at or after Heartwood: the header
    /// commitment of such a block is not checked (`docs/consensus-rules.md`).
    pub history: Option<Arc<HistoryState>>,
}

impl Layer {
    /// `hashChainHistoryRoot` after the block, when the history state is known: the value
    /// a template on top of this block commits to.
    pub fn history_root(&self) -> Option<[u8; 32]> {
        self.history.as_ref().map(|h| h.root())
    }
}

/// The finalized state: everything older than the layer window.
pub struct Base {
    pub coins: CoinsCache,
    pub nullifiers: NullifierStore,
    pub orchard_frontier: Arc<OrchardFrontier>,
    pub sapling_frontier: Arc<SaplingFrontier>,
    /// The Ironwood frontier. Before NU6.3 it is the empty tree.
    pub ironwood_frontier: Arc<IronwoodFrontier>,
    /// The Sprout frontier after the base block.
    pub sprout_frontier: Arc<SproutFrontier>,
    /// The final Sprout treestate of every finalized block, by root, with the empty tree.
    /// A JoinSplit continues the tree of its anchor, so the base keeps the frontier and
    /// not only the root (Zakura `sprout_trees_by_anchor`). `None`: the node does not know
    /// the Sprout state ([`Base::set_sprout_unknown`]).
    sprout_trees: Option<Map<[u8; 32], Arc<SproutFrontier>>>,
    sapling_anchors: Set<[u8; 32]>,
    orchard_anchors: Set<[u8; 32]>,
    ironwood_anchors: Set<[u8; 32]>,
    /// Roots of the tip trees.
    pub anchors: Anchors,
    pub height: u32,
    pub hash: BlockHash,
    /// Timestamps of the newest finalized blocks, oldest first, at most
    /// [`DIFFICULTY_CONTEXT_BLOCKS`].
    times: VecDeque<u32>,
    /// `nBits` of the newest finalized blocks, oldest first, at most
    /// [`DIFFICULTY_CONTEXT_BLOCKS`]. The last entry belongs to the base block. The list is
    /// shorter than `times` when the node does not know the bits of older blocks.
    bits: VecDeque<u32>,
    pub value_pools: ValuePools,
    /// The ZIP 221 history tree after the base block. [`Base::new`] leaves it `None`
    /// (unknown). A node seeds it with [`HistoryState::from_peaks`], or with
    /// [`HistoryState::empty`] when the base is before Heartwood.
    pub history: Option<Arc<HistoryState>>,
    /// Anchors inserted since the last [`Base::take_new_anchors`] call, in insertion order.
    new_anchors: Vec<(Pool, [u8; 32])>,
    /// Sprout treestates inserted since the last [`Base::take_new_sprout_trees`] call, in
    /// insertion order.
    new_sprout_trees: Vec<Arc<SproutFrontier>>,
}

/// The part of a [`Base`] that is not the coins, the nullifiers or the anchor sets: what a
/// node persists to resume from the base (`hayaid`'s state log).
#[derive(Clone, Debug)]
pub struct BaseState {
    pub height: u32,
    pub hash: BlockHash,
    /// Timestamps of the newest finalized blocks, oldest first.
    pub times: Vec<u32>,
    /// `nBits` of the newest finalized blocks, oldest first. The last entry belongs to the
    /// base block. Empty when the node does not know them.
    pub bits: Vec<u32>,
    pub value_pools: ValuePools,
    pub history: Option<Arc<HistoryState>>,
    pub sapling_frontier: Arc<SaplingFrontier>,
    pub orchard_frontier: Arc<OrchardFrontier>,
    pub ironwood_frontier: Arc<IronwoodFrontier>,
    /// `None`: the node does not know the Sprout state of the base.
    pub sprout_frontier: Option<Arc<SproutFrontier>>,
}

impl Base {
    /// A base at `height`/`hash` with empty trees, empty pending sets and the empty-tree
    /// anchors, over `backing`.
    pub fn new(backing: Arc<dyn CoinsBacking>, height: u32, hash: BlockHash, time: u32) -> Self {
        let orchard_frontier = OrchardFrontier::empty();
        let sapling_frontier = SaplingFrontier::empty();
        let ironwood_frontier = IronwoodFrontier::empty();
        let sprout_frontier = Arc::new(SproutFrontier::empty());
        let anchors = Anchors {
            sapling: sapling_frontier.root().to_bytes(),
            orchard: orchard_frontier.root().to_bytes(),
            ironwood: ironwood_frontier.root().to_bytes(),
        };
        let new_anchors = vec![
            (Pool::Sapling, anchors.sapling),
            (Pool::Orchard, anchors.orchard),
            (Pool::Ironwood, anchors.ironwood),
        ];
        Self {
            coins: CoinsCache::new(backing.clone()),
            nullifiers: NullifierStore::new(backing),
            orchard_frontier: Arc::new(orchard_frontier),
            sapling_frontier: Arc::new(sapling_frontier),
            ironwood_frontier: Arc::new(ironwood_frontier),
            sprout_trees: Some(Map::from_iter([(
                sprout_frontier.root(),
                sprout_frontier.clone(),
            )])),
            sprout_frontier,
            sapling_anchors: Set::from_iter([anchors.sapling]),
            orchard_anchors: Set::from_iter([anchors.orchard]),
            ironwood_anchors: Set::from_iter([anchors.ironwood]),
            anchors,
            height,
            hash,
            times: VecDeque::from([time]),
            bits: VecDeque::new(),
            value_pools: ValuePools::default(),
            history: None,
            new_anchors,
            new_sprout_trees: Vec::new(),
        }
    }

    /// Records that the node does not know the Sprout state of this base: a start above
    /// the genesis block without the Sprout treestates. A block with a JoinSplit on such a
    /// base is [`ContextError::SproutStateUnknown`]. The node never takes the empty tree
    /// in place of a tree that it does not know.
    pub fn set_sprout_unknown(&mut self) {
        self.sprout_trees = None;
        self.new_sprout_trees.clear();
    }

    /// Sets the times and the `nBits` of the newest blocks that end at the base block,
    /// oldest first: the context of the header rules of the next blocks. A node that
    /// starts above the genesis block calls it with the blocks of its start state. The
    /// last entry of each list belongs to the base block, and the base keeps the newest
    /// [`DIFFICULTY_CONTEXT_BLOCKS`] entries of each list.
    pub fn set_header_context(&mut self, times: &[u32], bits: &[u32]) {
        assert!(!times.is_empty(), "the base block has a time");
        let newest = |list: &[u32]| -> VecDeque<u32> {
            let start = list.len().saturating_sub(DIFFICULTY_CONTEXT_BLOCKS);
            list[start..].iter().copied().collect()
        };
        self.times = newest(times);
        self.bits = newest(bits);
    }

    /// The state of this base without its coins, nullifiers and anchor sets.
    pub fn state(&self) -> BaseState {
        BaseState {
            height: self.height,
            hash: self.hash,
            times: self.times.iter().copied().collect(),
            bits: self.bits.iter().copied().collect(),
            value_pools: self.value_pools,
            history: self.history.clone(),
            sapling_frontier: self.sapling_frontier.clone(),
            orchard_frontier: self.orchard_frontier.clone(),
            ironwood_frontier: self.ironwood_frontier.clone(),
            sprout_frontier: self
                .sprout_trees
                .as_ref()
                .map(|_| self.sprout_frontier.clone()),
        }
    }

    /// Takes the Sprout treestates inserted since the last call. With [`Base::restore`]
    /// they rebuild the Sprout treestates of the base. The empty tree is never in the list.
    pub fn take_new_sprout_trees(&mut self) -> Vec<Arc<SproutFrontier>> {
        std::mem::take(&mut self.new_sprout_trees)
    }

    /// Takes the anchors inserted since the last call (the first call returns the anchors
    /// of the construction too). With [`Base::restore`] they rebuild the anchor sets.
    pub fn take_new_anchors(&mut self) -> Vec<(Pool, [u8; 32])> {
        std::mem::take(&mut self.new_anchors)
    }

    /// A base at `state` over `backing`, with the anchor sets rebuilt from `anchors` and
    /// the Sprout treestates from `sprout_trees`. The tip anchors are the roots of the
    /// frontiers. It reports no new anchors and no new Sprout treestates. A state without
    /// the Sprout frontier gives a base that does not know the Sprout state.
    pub fn restore(
        backing: Arc<dyn CoinsBacking>,
        state: BaseState,
        anchors: impl IntoIterator<Item = (Pool, [u8; 32])>,
        sprout_trees: impl IntoIterator<Item = Arc<SproutFrontier>>,
    ) -> Self {
        let mut base = Self::new(backing, state.height, state.hash, 0);
        base.times = state.times.into();
        base.bits = state.bits.into();
        base.value_pools = state.value_pools;
        base.history = state.history;
        base.anchors = Anchors {
            sapling: state.sapling_frontier.root().to_bytes(),
            orchard: state.orchard_frontier.root().to_bytes(),
            ironwood: state.ironwood_frontier.root().to_bytes(),
        };
        base.sapling_frontier = state.sapling_frontier;
        base.orchard_frontier = state.orchard_frontier;
        base.ironwood_frontier = state.ironwood_frontier;
        for (pool, root) in anchors {
            base.insert_anchor(pool, root);
        }
        base.new_anchors.clear();
        match state.sprout_frontier {
            Some(frontier) => {
                for tree in sprout_trees {
                    base.insert_sprout_tree(tree);
                }
                base.insert_sprout_tree(frontier.clone());
                base.sprout_frontier = frontier;
                base.new_sprout_trees.clear();
            }
            None => base.set_sprout_unknown(),
        }
        base
    }

    /// Records a final Sprout treestate. A base that does not know the Sprout state
    /// records nothing.
    fn insert_sprout_tree(&mut self, tree: Arc<SproutFrontier>) {
        let Some(trees) = &mut self.sprout_trees else {
            return;
        };
        if let std::collections::hash_map::Entry::Vacant(slot) = trees.entry(tree.root()) {
            slot.insert(tree.clone());
            self.new_sprout_trees.push(tree);
        }
    }

    /// Records a finalized anchor (used when restoring a base from disk).
    pub fn insert_anchor(&mut self, pool: Pool, root: [u8; 32]) {
        let inserted = match pool {
            Pool::Sapling => self.sapling_anchors.insert(root),
            Pool::Orchard => self.orchard_anchors.insert(root),
            Pool::Ironwood => self.ironwood_anchors.insert(root),
            Pool::Sprout => panic!("a Sprout anchor is a treestate: Base::restore takes it"),
        };
        if inserted {
            self.new_anchors.push((pool, root));
        }
    }

    fn has_anchor(&self, pool: Pool, root: &[u8; 32]) -> bool {
        match pool {
            Pool::Sapling => self.sapling_anchors.contains(root),
            Pool::Orchard => self.orchard_anchors.contains(root),
            Pool::Ironwood => self.ironwood_anchors.contains(root),
            Pool::Sprout => matches!(&self.sprout_trees, Some(trees) if trees.contains_key(root)),
        }
    }

    /// Applies a layer: coins, nullifiers, anchors, trees, tip and value pools.
    fn absorb(&mut self, layer: &Layer) -> Result<(), hayai_coins::Error> {
        for (outpoint, coin) in &layer.created {
            self.coins.add(outpoint.clone(), coin.clone())?;
        }
        for outpoint in &layer.spent {
            self.coins.spend(outpoint)?;
        }
        for pool in Pool::ALL {
            let set = &layer.nullifiers[pool.index()];
            if set.is_empty() {
                continue;
            }
            let nullifiers: Vec<[u8; 32]> = set.iter().copied().collect();
            self.nullifiers.pool_mut(pool).insert_many(&nullifiers);
        }
        self.insert_anchor(Pool::Sapling, layer.anchors.sapling);
        self.insert_anchor(Pool::Orchard, layer.anchors.orchard);
        self.insert_anchor(Pool::Ironwood, layer.anchors.ironwood);
        self.anchors = layer.anchors;
        self.orchard_frontier = layer.orchard_frontier.clone();
        self.sapling_frontier = layer.sapling_frontier.clone();
        self.ironwood_frontier = layer.ironwood_frontier.clone();
        self.sprout_frontier = layer.sprout_frontier.clone();
        self.insert_sprout_tree(layer.sprout_frontier.clone());
        self.height = layer.height;
        self.hash = layer.hash;
        self.times.push_back(layer.time);
        self.bits.push_back(layer.bits);
        for list in [&mut self.times, &mut self.bits] {
            while list.len() > DIFFICULTY_CONTEXT_BLOCKS {
                list.pop_front();
            }
        }
        self.value_pools = layer.value_pools;
        self.history = layer.history.clone();
        Ok(())
    }
}

/// The tip of a chain or view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tip {
    pub height: u32,
    pub hash: BlockHash,
}

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("layer at height {height} with parent {parent} does not extend tip {tip:?}")]
    NotOnTip {
        height: u32,
        parent: BlockHash,
        tip: Tip,
    },
    #[error("coins: {0}")]
    Coins(#[from] hayai_coins::Error),
    #[error("speculative layer {0:?} is not on the chain")]
    UnknownSpeculative(SpecId),
    #[error("a layer cannot be committed directly while speculative layers are on the tip")]
    SpeculativePending,
}

/// The handle of a speculative layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpecId(u64);

struct Speculative {
    id: SpecId,
    layer: Arc<Layer>,
    /// The block's own verification (scripts and proofs) succeeded.
    verified: bool,
}

/// The base plus the window of non-finalized layers, owned by the single state writer.
pub struct Chain {
    base: Arc<RwLock<Base>>,
    /// Committed layers, oldest first.
    layers: VecDeque<Arc<Layer>>,
    /// Speculative layers above the committed tip, oldest first. Each one extends the one
    /// before it.
    speculative: VecDeque<Speculative>,
    next_spec: u64,
    index: Arc<RwLock<WindowIndex>>,
    backing: Arc<dyn CoinsBacking>,
}

impl Chain {
    pub fn new(base: Base) -> Self {
        let tip = Tip {
            height: base.height,
            hash: base.hash,
        };
        let backing = base.coins.backing().clone();
        Self {
            base: Arc::new(RwLock::new(base)),
            layers: VecDeque::new(),
            speculative: VecDeque::new(),
            next_spec: 0,
            index: Arc::new(RwLock::new(WindowIndex::new(tip))),
            backing,
        }
    }

    /// A snapshot of the committed chain for readers: an Arc of the layer list plus the
    /// shared base and index.
    pub fn view(&self) -> ChainView {
        ChainView {
            base: self.base.clone(),
            layers: self.layers.iter().cloned().collect(),
            index: self.index.clone(),
        }
    }

    /// A snapshot that also includes the speculative layers: the view a validator of the
    /// next block or the template uses before the newest blocks are verified.
    pub fn view_speculative(&self) -> ChainView {
        ChainView {
            base: self.base.clone(),
            layers: self
                .layers
                .iter()
                .cloned()
                .chain(self.speculative.iter().map(|s| s.layer.clone()))
                .collect(),
            index: self.index.clone(),
        }
    }

    /// The newest speculative layer's block, or the committed tip.
    pub fn speculative_tip(&self) -> Tip {
        match self.speculative.back() {
            Some(s) => Tip {
                height: s.layer.height,
                hash: s.layer.hash,
            },
            None => self.tip(),
        }
    }

    /// The speculative layers, oldest first.
    pub fn speculative(&self) -> impl Iterator<Item = (SpecId, &Arc<Layer>)> {
        self.speculative.iter().map(|s| (s.id, &s.layer))
    }

    /// Puts `layer` on top of the speculative tip. The layer's block passed the layer build
    /// (`hayai_validate::build_layer`) and waits for its verification.
    pub fn push_speculative(&mut self, layer: Layer) -> Result<SpecId, ChainError> {
        let tip = self.speculative_tip();
        if layer.parent != tip.hash || layer.height != tip.height + 1 {
            return Err(ChainError::NotOnTip {
                height: layer.height,
                parent: layer.parent,
                tip,
            });
        }
        let id = SpecId(self.next_spec);
        self.next_spec += 1;
        self.speculative.push_back(Speculative {
            id,
            layer: Arc::new(layer),
            verified: false,
        });
        Ok(id)
    }

    /// Records that the verification of `id` succeeded, then commits every speculative
    /// layer from the bottom up to the first one that is not verified yet. Returns the
    /// layers this call committed, oldest first (empty while an ancestor still waits).
    pub fn confirm(&mut self, id: SpecId) -> Result<Vec<Arc<Layer>>, ChainError> {
        let Some(entry) = self.speculative.iter_mut().find(|s| s.id == id) else {
            return Err(ChainError::UnknownSpeculative(id));
        };
        entry.verified = true;
        let mut committed = Vec::new();
        while let Some(Speculative { verified: true, .. }) = self.speculative.front() {
            let Some(entry) = self.speculative.pop_front() else {
                unreachable!("the front exists");
            };
            self.index.write().push(&entry.layer);
            self.layers.push_back(entry.layer.clone());
            committed.push(entry.layer);
        }
        Ok(committed)
    }

    /// Drops `id` and every speculative layer above it: their blocks extend a block that
    /// failed verification. Returns the dropped layers, oldest first.
    pub fn reject(&mut self, id: SpecId) -> Result<Vec<Arc<Layer>>, ChainError> {
        let Some(position) = self.speculative.iter().position(|s| s.id == id) else {
            return Err(ChainError::UnknownSpeculative(id));
        };
        Ok(self
            .speculative
            .drain(position..)
            .map(|s| s.layer)
            .collect())
    }

    /// The committed tip.
    pub fn tip(&self) -> Tip {
        match self.layers.back() {
            Some(layer) => Tip {
                height: layer.height,
                hash: layer.hash,
            },
            None => {
                let base = self.base.read();
                Tip {
                    height: base.height,
                    hash: base.hash,
                }
            }
        }
    }

    /// Commits a layer on top of the tip. Fails while speculative layers are on the tip:
    /// they are committed with [`Chain::confirm`].
    pub fn push(&mut self, layer: Layer) -> Result<Arc<Layer>, ChainError> {
        let None = self.speculative.front() else {
            return Err(ChainError::SpeculativePending);
        };
        let tip = self.tip();
        if layer.parent != tip.hash || layer.height != tip.height + 1 {
            return Err(ChainError::NotOnTip {
                height: layer.height,
                parent: layer.parent,
                tip,
            });
        }
        let layer = Arc::new(layer);
        self.index.write().push(&layer);
        self.layers.push_back(layer.clone());
        Ok(layer)
    }

    /// Disconnects the committed tip layer. Finalized blocks cannot be popped. The
    /// speculative layers extend the popped block, so the pop drops them too.
    pub fn pop(&mut self) -> Option<Arc<Layer>> {
        let layer = self.layers.pop_back()?;
        self.speculative.clear();
        let tip = self.tip();
        self.layers.make_contiguous();
        let (remaining, _) = self.layers.as_slices();
        self.index.write().pop(&layer, remaining, tip);
        Some(layer)
    }

    /// Merges the oldest layers into the base until at most `window` layers remain. Returns
    /// how many were merged. Flushing to disk is left to [`Chain::flush`]. The base absorbs
    /// a layer before the index drops its entries, so a reader that misses the index finds
    /// the layer's effect in the base.
    pub fn finalize_excess(&mut self, window: usize) -> Result<usize, ChainError> {
        let mut merged = 0;
        while self.layers.len() > window {
            let Some(layer) = self.layers.pop_front() else {
                unreachable!("len > window >= 0");
            };
            self.base.write().absorb(&layer)?;
            self.index.write().finalize(&layer);
            merged += 1;
        }
        Ok(merged)
    }

    /// Phase one of a flush, under the base lock for the time of a map scan: takes the
    /// base's dirty coins and pending nullifiers into a generation stamped with the base's
    /// tip as best block. Readers keep seeing the entries from the caches while the
    /// generation is written. Fails when a generation is already in flight.
    pub fn begin_flush(&mut self) -> Result<FlushGeneration, ChainError> {
        let mut base = self.base.write();
        let best_block = BestBlock {
            height: base.height,
            hash: base.hash.0,
        };
        let coins = base.coins.begin_flush()?;
        let nullifiers = match base.nullifiers.begin_flush() {
            Ok(nullifiers) => nullifiers,
            Err(error) => {
                // The coins generation is already taken; the two must stay in step.
                base.coins.end_flush()?;
                return Err(error.into());
            }
        };
        Ok(FlushGeneration {
            adds: coins.adds,
            spends: coins.spends,
            nullifiers,
            best_block,
        })
    }

    /// Phase three of a flush, under the base lock, once the generation is on disk: marks
    /// its entries clean. Fails when no generation is in flight.
    pub fn end_flush(&mut self) -> Result<(), ChainError> {
        let mut base = self.base.write();
        base.coins.end_flush()?;
        base.nullifiers.end_flush()?;
        Ok(())
    }

    /// The backing store the generations are written to
    /// ([`CoinsBacking::write_generation`], phase two, outside every lock).
    pub fn backing(&self) -> &Arc<dyn CoinsBacking> {
        &self.backing
    }

    /// The three phases in a row: begin, one atomic write of coins, nullifiers and best
    /// block, end. Returns what the write did to the coins and how many nullifiers it
    /// inserted.
    pub fn flush(&mut self) -> Result<(FlushStats, usize), ChainError> {
        let generation = self.begin_flush()?;
        self.backing.write_generation(&generation)?;
        self.end_flush()?;
        Ok((generation.stats(), generation.nullifier_count()))
    }

    pub fn layers(&self) -> impl Iterator<Item = &Arc<Layer>> {
        self.layers.iter()
    }

    pub fn base(&self) -> &Arc<RwLock<Base>> {
        &self.base
    }
}

/// A read-only snapshot of a chain: the layers newest to oldest, then the base.
#[derive(Clone)]
pub struct ChainView {
    base: Arc<RwLock<Base>>,
    layers: Arc<[Arc<Layer>]>,
    index: Arc<RwLock<WindowIndex>>,
}

impl ChainView {
    pub fn tip(&self) -> Tip {
        match self.layers.last() {
            Some(layer) => Tip {
                height: layer.height,
                hash: layer.hash,
            },
            None => {
                let base = self.base.read();
                Tip {
                    height: base.height,
                    hash: base.hash,
                }
            }
        }
    }

    pub fn tip_height(&self) -> u32 {
        self.tip().height
    }

    pub fn layers(&self) -> &[Arc<Layer>] {
        &self.layers
    }

    /// Value pools after the tip.
    pub fn value_pools(&self) -> ValuePools {
        match self.layers.last() {
            Some(layer) => layer.value_pools,
            None => self.base.read().value_pools,
        }
    }

    /// The ZIP 221 history tree after the tip, when the node knows it.
    pub fn history(&self) -> Option<Arc<HistoryState>> {
        match self.layers.last() {
            Some(layer) => layer.history.clone(),
            None => self.base.read().history.clone(),
        }
    }

    /// The layers of this view above the tip that `index` describes, newest last, when that
    /// tip is in the view: a layer of the view, or the parent of its oldest layer (a window
    /// with no committed layer). `None` when the view does not hold the index's tip (a
    /// view taken before a pop and a push): the view then walks all its layers. `view_tip`
    /// is this view's tip, read before the index lock.
    fn above_index(&self, index: Tip, view_tip: Tip) -> Option<&[Arc<Layer>]> {
        for (i, layer) in self.layers.iter().enumerate().rev() {
            if layer.hash == index.hash && layer.height == index.height {
                return Some(&self.layers[i + 1..]);
            }
        }
        match self.layers.first() {
            Some(first) if first.parent == index.hash && first.height == index.height + 1 => {
                Some(&self.layers[..])
            }
            Some(_) => None,
            None if view_tip == index => Some(&self.layers[..]),
            None => None,
        }
    }

    /// Frontiers and their roots after the tip.
    pub fn frontiers(&self) -> Frontiers {
        match self.layers.last() {
            Some(layer) => Frontiers {
                orchard: layer.orchard_frontier.clone(),
                sapling: layer.sapling_frontier.clone(),
                ironwood: layer.ironwood_frontier.clone(),
                sprout: layer.sprout_frontier.clone(),
                anchors: layer.anchors,
            },
            None => {
                let base = self.base.read();
                Frontiers {
                    orchard: base.orchard_frontier.clone(),
                    sapling: base.sapling_frontier.clone(),
                    ironwood: base.ironwood_frontier.clone(),
                    sprout: base.sprout_frontier.clone(),
                    anchors: base.anchors,
                }
            }
        }
    }

    /// Positional membership of `nullifiers` in `pool`, over the window and the base: the
    /// layers above the index's tip first (speculative layers), then one index probe.
    pub fn contains_nullifier_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Vec<bool> {
        let mut result = vec![false; nullifiers.len()];
        let mut misses: Vec<usize> = Vec::new();
        let tip = self.tip();
        {
            let index = self.index.read();
            let Some(above) = self.above_index(index.tip(), tip) else {
                drop(index);
                return self.contains_nullifier_many_by_walk(pool, nullifiers);
            };
            for (i, nf) in nullifiers.iter().enumerate() {
                let in_above = above
                    .iter()
                    .any(|layer| layer.nullifiers[pool.index()].contains(nf));
                if in_above || index.contains_nullifier(pool, nf) {
                    result[i] = true;
                } else {
                    misses.push(i);
                }
            }
        }
        if misses.is_empty() {
            return result;
        }
        let keys: Vec<[u8; 32]> = misses.iter().map(|&i| nullifiers[i]).collect();
        self.nullifiers_from_base(pool, &keys, misses, result)
    }

    /// Reference implementation of [`ChainView::contains_nullifier_many`]: a newest-first
    /// walk of the layers, then the base. The index is tested against it and the
    /// `state/lookup_through_window` benchmark measures it.
    pub fn contains_nullifier_many_by_walk(
        &self,
        pool: Pool,
        nullifiers: &[[u8; 32]],
    ) -> Vec<bool> {
        let mut result = vec![false; nullifiers.len()];
        let mut misses: Vec<usize> = Vec::new();
        'each: for (i, nf) in nullifiers.iter().enumerate() {
            for layer in self.layers.iter().rev() {
                if layer.nullifiers[pool.index()].contains(nf) {
                    result[i] = true;
                    continue 'each;
                }
            }
            misses.push(i);
        }
        if misses.is_empty() {
            return result;
        }
        let keys: Vec<[u8; 32]> = misses.iter().map(|&i| nullifiers[i]).collect();
        self.nullifiers_from_base(pool, &keys, misses, result)
    }

    /// Fills `result` at `misses` with the base's answer for `keys`.
    fn nullifiers_from_base(
        &self,
        pool: Pool,
        keys: &[[u8; 32]],
        misses: Vec<usize>,
        mut result: Vec<bool>,
    ) -> Vec<bool> {
        let found = match self.base.read().nullifiers.pool(pool).contains_many(keys) {
            Ok(found) => found,
            // As for coins: a failing backing store would otherwise read as "absent" and let
            // a double spend through.
            Err(error) => panic!("nullifier backing store failed: {error}"),
        };
        for (i, present) in misses.into_iter().zip(found) {
            result[i] = present;
        }
        result
    }

    /// Whether the node knows the Sprout state of this view
    /// ([`Base::set_sprout_unknown`]).
    pub fn sprout_known(&self) -> bool {
        let Some(_) = &self.base.read().sprout_trees else {
            return false;
        };
        true
    }

    /// The Sprout tree whose root is `root`, when `root` is the final Sprout treestate of
    /// some block at or before the tip, or the root of the empty tree.
    pub fn sprout_tree(&self, root: &[u8; 32]) -> Option<Arc<SproutFrontier>> {
        if let Some(layer) = self
            .layers
            .iter()
            .rev()
            .find(|layer| layer.sprout_frontier.root() == *root)
        {
            return Some(layer.sprout_frontier.clone());
        }
        self.base.read().sprout_trees.as_ref()?.get(root).cloned()
    }

    /// Whether `root` is the final treestate of some block at or before the tip. A Sprout
    /// anchor can also be a treestate inside its transaction, which this function does
    /// not know.
    pub fn has_anchor(&self, pool: Pool, root: &[u8; 32]) -> bool {
        if let Pool::Sprout = pool {
            let Some(_) = self.sprout_tree(root) else {
                return false;
            };
            return true;
        }
        if self
            .layers
            .iter()
            .any(|layer| layer.anchors.get(pool) == Some(*root))
        {
            return true;
        }
        self.base.read().has_anchor(pool, root)
    }

    /// Median of the last [`MEDIAN_TIME_SPAN`] block times ending at the tip.
    pub fn median_time_past(&self) -> u32 {
        let mut times: Vec<u32> = self
            .layers
            .iter()
            .rev()
            .take(MEDIAN_TIME_SPAN)
            .map(|l| l.time)
            .collect();
        if times.len() < MEDIAN_TIME_SPAN {
            let base = self.base.read();
            let missing = MEDIAN_TIME_SPAN - times.len();
            times.extend(base.times.iter().rev().take(missing));
        }
        times.sort_unstable();
        times[times.len() / 2]
    }

    /// `(time, bits)` of the newest blocks ending at the tip, newest first: the context of
    /// the difficulty rule of the next block. It holds at most
    /// [`DIFFICULTY_CONTEXT_BLOCKS`] entries. It is shorter when the chain is shorter, or
    /// when the base does not know the bits of its older blocks.
    pub fn difficulty_context(&self) -> Vec<(u32, u32)> {
        let mut context: Vec<(u32, u32)> = self
            .layers
            .iter()
            .rev()
            .take(DIFFICULTY_CONTEXT_BLOCKS)
            .map(|l| (l.time, l.bits))
            .collect();
        if context.len() < DIFFICULTY_CONTEXT_BLOCKS {
            let base = self.base.read();
            let missing = DIFFICULTY_CONTEXT_BLOCKS - context.len();
            let times = base.times.iter().rev();
            let bits = base.bits.iter().rev();
            context.extend(times.zip(bits).take(missing).map(|(t, b)| (*t, *b)));
        }
        context
    }

    /// The times of the newest blocks ending at the tip, newest first: the times that the
    /// header rules of the next block read. The list holds at most
    /// [`DIFFICULTY_CONTEXT_BLOCKS`] entries. Unlike [`ChainView::difficulty_context`], it
    /// also holds the times of the base blocks whose bits the base does not know.
    pub fn recent_times(&self) -> Vec<u32> {
        let mut times: Vec<u32> = self
            .layers
            .iter()
            .rev()
            .take(DIFFICULTY_CONTEXT_BLOCKS)
            .map(|l| l.time)
            .collect();
        if times.len() < DIFFICULTY_CONTEXT_BLOCKS {
            let base = self.base.read();
            let missing = DIFFICULTY_CONTEXT_BLOCKS - times.len();
            times.extend(base.times.iter().rev().take(missing));
        }
        times
    }
}

impl ChainView {
    /// Reference implementation of [`CoinsView::get_coins`] for a view: a newest-first walk
    /// of the layers, then the base. The index is tested against it and the
    /// `state/lookup_through_window` benchmark measures it.
    pub fn get_coins_by_walk(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
        let mut result: Vec<Option<Coin>> = vec![None; outpoints.len()];
        let mut misses: Vec<usize> = Vec::new();
        'each: for (i, outpoint) in outpoints.iter().enumerate() {
            for layer in self.layers.iter().rev() {
                if layer.spent.contains(outpoint) {
                    continue 'each;
                }
                if let Some(coin) = layer.created.get(outpoint) {
                    result[i] = Some(coin.clone());
                    continue 'each;
                }
            }
            misses.push(i);
        }
        self.coins_from_base(outpoints, misses, result)
    }

    /// Fills `result` at `misses` with the base's coins.
    fn coins_from_base(
        &self,
        outpoints: &[OutPoint],
        misses: Vec<usize>,
        mut result: Vec<Option<Coin>>,
    ) -> Vec<Option<Coin>> {
        if misses.is_empty() {
            return result;
        }
        let keys: Vec<OutPoint> = misses.iter().map(|&i| outpoints[i].clone()).collect();
        let found = self.base.read().coins.get_coins(&keys);
        for (i, coin) in misses.into_iter().zip(found) {
            result[i] = coin;
        }
        result
    }
}

impl CoinsView for ChainView {
    /// The layers above the index's tip first (speculative layers), newest first, then one
    /// probe in the window index per outpoint, then one base round for the misses.
    fn get_coins(&self, outpoints: &[OutPoint]) -> Vec<Option<Coin>> {
        let mut result: Vec<Option<Coin>> = vec![None; outpoints.len()];
        let mut misses: Vec<usize> = Vec::new();
        let tip = self.tip();
        {
            let index = self.index.read();
            let Some(above) = self.above_index(index.tip(), tip) else {
                drop(index);
                return self.get_coins_by_walk(outpoints);
            };
            'each: for (i, outpoint) in outpoints.iter().enumerate() {
                for layer in above.iter().rev() {
                    if layer.spent.contains(outpoint) {
                        continue 'each;
                    }
                    if let Some(coin) = layer.created.get(outpoint) {
                        result[i] = Some(coin.clone());
                        continue 'each;
                    }
                }
                match index.coin(outpoint) {
                    Some(coin) => result[i] = coin,
                    None => misses.push(i),
                }
            }
        }
        self.coins_from_base(outpoints, misses, result)
    }
}

#[cfg(test)]
mod tests;
