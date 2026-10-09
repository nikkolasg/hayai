//! Shadow mode: a node that follows an upstream node (Zakura or zcashd) through its
//! JSON-RPC and validates every block of its best chain against its own rules.
//!
//! - [`upstream`]: the JSON-RPC client of the followed node.
//! - [`seed`]: the start state of a shadow node at a height of the upstream chain: the
//!   note commitment trees, the value pools and the header context, read from upstream.
//! - [`spawn_follower`] and [`Follower`]: the poll of the upstream tip; each new part of
//!   the upstream best chain goes to an [`UpstreamSink`] (the driver of the node).
//! - [`backing`]: the coins store of a shadow node, which reads the coins below its start
//!   height from upstream.
//!
//! The crate names no type of the node: the network is `hayai_consensus::Network`, the
//! blocks go through [`UpstreamSink`], and the counters of the trusted coins are
//! `hayai_metrics::Counter`s that the node registers.

#![forbid(unsafe_code)]

pub mod backing;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;
pub mod upstream;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hayai_consensus::{rules_at, Network, Upgrade, DIFFICULTY_CONTEXT_BLOCKS};
use hayai_crypto::zcash_primitives::merkle_tree::read_frontier_v0;
use hayai_state::ValuePools;
use hayai_trees::{IronwoodFrontier, OrchardFrontier, SaplingFrontier};
use hayai_wire::header::BlockHash;
use hayai_wire::RawBlock;

use crate::upstream::{BlockInfo, TreeState, Upstream, UpstreamError};
use hayai_sync::index::{HeaderIndex, SeedBlock};

/// Blocks the follower fetches in one report at most. A node further behind stops.
pub const MAX_CATCH_UP: usize = 1000;

/// The upstream state at the start height.
pub struct Seed {
    pub height: u32,
    pub hash: BlockHash,
    /// The start block and the blocks before it, oldest first, each with its time and its
    /// `nBits`: [`DIFFICULTY_CONTEXT_BLOCKS`] blocks, or every block from the genesis block
    /// when the chain is shorter. It is the whole context of the header rules of the first
    /// block after the start.
    pub ancestors: Vec<SeedBlock>,
    pub sapling: SaplingFrontier,
    pub orchard: OrchardFrontier,
    pub ironwood: IronwoodFrontier,
    /// The chain value pools after the start block.
    pub value_pools: ValuePools,
}

/// The note commitment trees of upstream after a block.
pub struct UpstreamTrees {
    pub sapling: SaplingFrontier,
    pub orchard: OrchardFrontier,
    pub ironwood: IronwoodFrontier,
}

/// Parses `z_gettreestate` final states (zcashd's legacy `CommitmentTree` encoding).
/// `ironwood_active` tells that the Ironwood pool is active at the height of the block: the
/// answer must then have the Ironwood tree. Before that height the Ironwood tree is empty.
pub fn frontiers(trees: &TreeState, ironwood_active: bool) -> Result<UpstreamTrees, String> {
    let sapling = match &trees.sapling {
        None => SaplingFrontier::empty(),
        Some(bytes) => SaplingFrontier::from_frontier(
            read_frontier_v0(&bytes[..]).map_err(|e| format!("Sapling tree state: {e}"))?,
        ),
    };
    let orchard = match &trees.orchard {
        None => OrchardFrontier::empty(),
        Some(bytes) => OrchardFrontier::from_frontier(
            read_frontier_v0(&bytes[..]).map_err(|e| format!("Orchard tree state: {e}"))?,
        ),
    };
    let ironwood = match (&trees.ironwood, ironwood_active) {
        (None, false) => IronwoodFrontier::empty(),
        (None, true) => {
            return Err(
                "upstream gives no Ironwood tree state for a block at or after NU6.3".to_string(),
            )
        }
        (Some(bytes), _) => IronwoodFrontier::from_frontier(
            read_frontier_v0(&bytes[..]).map_err(|e| format!("Ironwood tree state: {e}"))?,
        ),
    };
    Ok(UpstreamTrees {
        sapling,
        orchard,
        ironwood,
    })
}

/// Reads the start state from upstream. `start_height` defaults to the upstream tip.
pub fn seed(
    upstream: &Upstream,
    network: Network,
    start_height: Option<u32>,
) -> Result<Seed, String> {
    let e = |e: UpstreamError| e.to_string();
    let height = match start_height {
        Some(h) => h,
        None => upstream.block_count().map_err(e)?,
    };
    let hash = upstream.block_hash(height).map_err(e)?;
    let info = upstream.block_info(&hash).map_err(e)?;
    if info.height != height {
        return Err(format!(
            "upstream block {hash} is at height {}, not {height}",
            info.height
        ));
    }
    let seed_block = |hash: BlockHash, info: &BlockInfo| SeedBlock {
        hash,
        time: info.time,
        bits: Some(info.bits),
    };
    let mut ancestors = vec![seed_block(hash, &info)];
    let mut prev = info.prev;
    while ancestors.len() < DIFFICULTY_CONTEXT_BLOCKS {
        let Some(parent) = prev else {
            break;
        };
        let parent_info = upstream.block_info(&parent).map_err(e)?;
        ancestors.push(seed_block(parent, &parent_info));
        prev = parent_info.prev;
    }
    ancestors.reverse();
    // The difficulty rule of the first block after the start reads these blocks. A seed
    // with fewer blocks leaves a rule that cannot run, so it is not a seed.
    let needed = DIFFICULTY_CONTEXT_BLOCKS.min(height as usize + 1);
    if ancestors.len() != needed {
        return Err(format!(
            "upstream gives {} of the {needed} blocks that end at the start block {hash} at \
             height {height}: the header rules of the next block need all of them",
            ancestors.len()
        ));
    }
    let ironwood_active = rules_at(network, height)
        .map_err(|e| e.to_string())?
        .pools
        .ironwood;
    let trees = frontiers(&upstream.tree_state(&hash).map_err(e)?, ironwood_active)?;
    let value_pools = seed_pools(network, &info, ironwood_active)?;
    Ok(Seed {
        height,
        hash,
        ancestors,
        sapling: trees.sapling,
        orchard: trees.orchard,
        ironwood: trees.ironwood,
        value_pools,
    })
}

/// The chain value pools of the start block `info`. A pool that upstream does not report
/// is zero only when no block up to the start height can change it: the Ironwood pool
/// before NU6.3 and the deferred pool before NU6. In every other case the seed fails. The
/// deferred pool is not zero from NU6 (each block from NU6 adds to it, and the NU6.1
/// disbursement leaves the part of its own block), so a pool of zero at such a height is
/// an error too.
fn seed_pools(
    network: Network,
    info: &BlockInfo,
    ironwood_active: bool,
) -> Result<ValuePools, String> {
    let (hash, height) = (info.hash, info.height);
    let ironwood = match (info.ironwood_pool, ironwood_active) {
        (Some(pool), _) => pool,
        (None, false) => 0,
        (None, true) => {
            return Err(format!(
                "upstream gives no Ironwood value pool for block {hash} at or after NU6.3"
            ))
        }
    };
    let deferred_active =
        matches!(network.activation_height(Upgrade::Nu6), Some(nu6) if height >= nu6);
    let deferred = match (info.deferred_pool, deferred_active) {
        (Some(0) | None, true) => {
            return Err(format!(
                "upstream gives no deferred value pool (`lockbox`) above zero for block {hash} \
                 at height {height}, at or after NU6: hayai cannot check the lockbox terms \
                 without it"
            ))
        }
        (Some(pool), _) => pool,
        (None, false) => 0,
    };
    Ok(ValuePools {
        transparent: info.transparent_pool,
        sprout: info.sprout_pool,
        sapling: info.sapling_pool,
        orchard: info.orchard_pool,
        ironwood,
        deferred,
    })
}

/// A block of the upstream best chain with upstream's note commitment tree roots after it.
pub struct UpstreamBlock {
    pub raw: Arc<RawBlock>,
    pub height: u32,
    pub sapling_root: [u8; 32],
    pub orchard_root: [u8; 32],
    pub ironwood_root: [u8; 32],
}

/// Where the follower reports the upstream chain: the driver of the node.
pub trait UpstreamSink: Send + 'static {
    /// A new part of the upstream best chain: `fork` is the fork point (a block the
    /// receiver holds or a block of an earlier report) and `blocks` the blocks after it,
    /// oldest first. Returns `false` when the receiver is gone: the follower stops.
    fn on_upstream(&self, fork: BlockHash, blocks: Vec<UpstreamBlock>) -> bool;
}

/// Polls the upstream tip and reports every new part of the upstream best chain to the
/// driver as the [`UpstreamSink`]: the fork point (a block the driver holds or a block of
/// an earlier report) and the blocks after it, oldest first.
pub fn spawn_follower<S: UpstreamSink>(
    upstream: Arc<Upstream>,
    network: Network,
    index: Arc<HeaderIndex>,
    sink: S,
    stop: Arc<AtomicBool>,
    poll: Duration,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("shadow-follower".into())
        .spawn(move || {
            let mut follower = Follower::new(upstream, network, index, sink);
            while !stop.load(Ordering::Acquire) {
                match follower.step() {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => tracing::warn!(error = %e, "shadow follower step failed"),
                }
                thread::sleep(poll);
            }
        })
}

pub struct Follower<S> {
    upstream: Arc<Upstream>,
    network: Network,
    index: Arc<HeaderIndex>,
    sink: S,
    /// Heights of reported blocks, which the driver may not have committed yet.
    reported: HashMap<BlockHash, u32>,
    order: VecDeque<BlockHash>,
    last_best: Option<BlockHash>,
}

impl<S: UpstreamSink> Follower<S> {
    pub fn new(
        upstream: Arc<Upstream>,
        network: Network,
        index: Arc<HeaderIndex>,
        sink: S,
    ) -> Self {
        Self {
            upstream,
            network,
            index,
            sink,
            reported: HashMap::new(),
            order: VecDeque::new(),
            last_best: None,
        }
    }

    fn known_height(&self, hash: &BlockHash) -> Option<u32> {
        self.index
            .height_of(hash)
            .or_else(|| self.reported.get(hash).copied())
    }

    /// One poll. Returns `false` when the sink is gone.
    pub fn step(&mut self) -> Result<bool, String> {
        let best = self.upstream.best_block_hash().map_err(|e| e.to_string())?;
        if self.last_best == Some(best) {
            return Ok(true);
        }
        let mut path: Vec<RawBlock> = Vec::new();
        let mut cursor = best;
        let fork_height = loop {
            if let Some(h) = self.known_height(&cursor) {
                break h;
            }
            if path.len() == MAX_CATCH_UP {
                return Err(format!(
                    "upstream is more than {MAX_CATCH_UP} blocks ahead of hayai's chain"
                ));
            }
            let bytes = self
                .upstream
                .block_bytes(&cursor)
                .map_err(|e| e.to_string())?;
            let height_hint = self.index.tip().0 + 1;
            let branch = rules_at(self.network, height_hint)
                .map(|rules| rules.branch_id)
                .map_err(|e| e.to_string())?;
            let raw = RawBlock::parse(bytes, branch)
                .map_err(|e| format!("upstream block {cursor}: {e}"))?;
            cursor = raw.header.prev_hash;
            path.push(raw);
        };
        let fork = cursor;
        path.reverse();
        let mut blocks = Vec::with_capacity(path.len());
        for (i, raw) in path.into_iter().enumerate() {
            let hash = raw.hash();
            let height = fork_height + 1 + i as u32;
            let ironwood_active = rules_at(self.network, height)
                .map_err(|e| e.to_string())?
                .pools
                .ironwood;
            let trees = self.upstream.tree_state(&hash).map_err(|e| e.to_string())?;
            let trees = frontiers(&trees, ironwood_active)?;
            blocks.push(UpstreamBlock {
                raw: Arc::new(raw),
                height,
                sapling_root: trees.sapling.root().to_bytes(),
                orchard_root: trees.orchard.root().to_bytes(),
                ironwood_root: trees.ironwood.root().to_bytes(),
            });
        }
        for b in &blocks {
            let hash = b.raw.hash();
            self.reported.insert(hash, b.height);
            self.order.push_back(hash);
            if self.order.len() > 2 * MAX_CATCH_UP {
                if let Some(old) = self.order.pop_front() {
                    self.reported.remove(&old);
                }
            }
        }
        if !self.sink.on_upstream(fork, blocks) {
            return Ok(false);
        }
        self.last_best = Some(best);
        Ok(true)
    }
}
