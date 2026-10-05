//! The query methods of the JSON-RPC server: the committed chain, the state after the tip
//! and the mempool, as `hayai_rpc::NodeQuery`.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_channel::Sender;
use hayai_blockstore::BlockStore;
use hayai_net::{Direction, Relay};
use hayai_prepared::{PreparedStore, MIN_RELAY_FEE_RATE};
use hayai_rpc::{BlockInfo, BlockState, ChainTip, NodeQuery, NodeState, PeerRow, Pools, TipState};
use hayai_state::{Base, ChainView, Layer, ValuePools};
use hayai_sync::headers::{HeaderChain, Status};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{RawTx, TxLookup};
use parking_lot::{Mutex, RwLock};

use crate::mempool::Mempool;
use crate::params::NetParams;

pub struct Query {
    pub params: NetParams,
    pub blocks: Arc<BlockStore>,
    /// The chain state on the committed tip.
    pub view: Arc<RwLock<ChainView>>,
    pub store: Arc<PreparedStore>,
    pub mempool: Arc<Mempool>,
    pub relay: Arc<Relay>,
    /// The base of the chain: the state after the oldest block that the node holds.
    pub base: Arc<RwLock<Base>>,
    pub headers: Arc<Mutex<HeaderChain>>,
    /// The node takes private transactions (`[mining] lane_publication` is not `all`).
    pub private: bool,
    /// The `stop` method sends on this channel.
    pub stop: Sender<()>,
}

fn pools(pools: &ValuePools) -> Pools {
    Pools {
        transparent: pools.transparent,
        sprout: pools.sprout,
        sapling: pools.sapling,
        orchard: pools.orchard,
        lockbox: pools.deferred,
        ironwood: pools.ironwood,
    }
}

fn layer_state(layer: &Layer, parent_pools: Option<Pools>) -> BlockState {
    BlockState {
        sapling_root: layer.anchors.sapling,
        orchard_root: layer.anchors.orchard,
        sapling_size: layer.sapling_frontier.frontier().tree_size(),
        orchard_size: layer.orchard_frontier.frontier().tree_size(),
        ironwood_size: layer.ironwood_frontier.frontier().tree_size(),
        pools: pools(&layer.value_pools),
        parent_pools,
    }
}

impl Query {
    /// The state after the block `hash` of `height`: the state of a layer, or the state of
    /// the base block.
    fn block_state(&self, height: u32, hash: &BlockHash) -> Option<BlockState> {
        let view = self.view.read().clone();
        let layers = view.layers();
        let base = self.base.read();
        let base_pools =
            |parent: &BlockHash| (base.hash == *parent).then(|| pools(&base.value_pools));
        if let Some(at) = layers.iter().position(|layer| layer.hash == *hash) {
            let parent_pools = match at.checked_sub(1) {
                Some(before) => Some(pools(&layers[before].value_pools)),
                None => base_pools(&layers[at].parent),
            };
            return Some(layer_state(&layers[at], parent_pools));
        }
        if base.hash != *hash {
            return None;
        }
        Some(BlockState {
            sapling_root: base.anchors.sapling,
            orchard_root: base.anchors.orchard,
            sapling_size: base.sapling_frontier.frontier().tree_size(),
            orchard_size: base.orchard_frontier.frontier().tree_size(),
            ironwood_size: base.ironwood_frontier.frontier().tree_size(),
            pools: pools(&base.value_pools),
            // The pools are empty before the genesis block.
            parent_pools: (height == 0).then(Pools::default),
        })
    }
}

impl NodeQuery for Query {
    fn block_hash(&self, height: u32) -> Option<BlockHash> {
        // The node does not store the genesis block. Its hash is a value of the network.
        if height == 0 {
            return Some(self.params.genesis().0);
        }
        // Above the tip the index by height can name a block of a chain that a reorg left.
        if height > self.view.read().tip_height() {
            return None;
        }
        let bytes = match self.blocks.get_bytes(height) {
            Ok(found) => found?,
            Err(e) => {
                tracing::warn!(height, error = %e, "block store read failed");
                return None;
            }
        };
        match BlockHeader::parse(&bytes) {
            Ok(header) => Some(header.hash()),
            Err(e) => {
                tracing::warn!(height, error = %e, "stored block has no valid header");
                None
            }
        }
    }

    fn block_bytes(&self, hash: &BlockHash) -> Option<Bytes> {
        match self.blocks.get_by_hash(hash) {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(%hash, error = %e, "block store read failed");
                None
            }
        }
    }

    fn tip_state(&self) -> TipState {
        let view = self.view.read().clone();
        let tip = view.tip();
        let anchors = view.frontiers().anchors;
        let pools = view.value_pools();
        TipState {
            height: tip.height,
            hash: tip.hash,
            time: view.recent_times().first().copied().unwrap_or_default(),
            sapling_root: anchors.sapling,
            orchard_root: anchors.orchard,
            ironwood_root: anchors.ironwood,
            value_pools: [
                ("transparent", pools.transparent),
                ("sprout", pools.sprout),
                ("sapling", pools.sapling),
                ("orchard", pools.orchard),
                ("ironwood", pools.ironwood),
                ("lockbox", pools.deferred),
            ],
        }
    }

    fn mempool_txids(&self) -> Vec<[u8; 32]> {
        let mut ids = Vec::with_capacity(self.store.len());
        self.store
            .for_each_id(&mut |id| ids.push(*id.txid.as_ref()));
        ids
    }

    fn send_transaction(&self, bytes: Bytes, private: bool) -> Result<[u8; 32], String> {
        if private && !self.private {
            return Err(
                "the node takes no private transaction: [mining] lane_publication is \"all\""
                    .into(),
            );
        }
        let height = self.view.read().tip_height() + 1;
        let branch = self.params.branch_at(height).map_err(|e| e.to_string())?;
        let tx = Arc::new(RawTx::parse(bytes, branch).map_err(|e| e.to_string())?);
        match private {
            true => self.mempool.admit_private(tx.clone()),
            false => self
                .mempool
                .admit(tx.clone())
                .map(|()| self.relay.announce_tx(&tx)),
        }
        .map_err(|e| e.to_string())?;
        Ok(*tx.txid.as_ref())
    }

    fn block_info(&self, hash: &BlockHash) -> Option<BlockInfo> {
        let height = match self.blocks.height_of(hash) {
            Ok(found) => found?,
            Err(e) => {
                tracing::warn!(%hash, error = %e, "block store read failed");
                return None;
            }
        };
        let tip = self.view.read().tip_height();
        let committed = self.block_hash(height) == Some(*hash);
        Some(BlockInfo {
            height,
            confirmations: match committed {
                true => i64::from(tip - height) + 1,
                false => -1,
            },
            next: self.block_hash(height + 1).filter(|_| committed),
            state: self.block_state(height, hash),
        })
    }

    fn header_context(&self, height: u32) -> Option<(u32, u32)> {
        if height > self.view.read().tip_height() {
            return None;
        }
        let headers = self.headers.lock();
        let block = headers.best_chain_from(height).next()?;
        let entry = headers.entry(&block.hash)?;
        Some((entry.time, entry.bits))
    }

    fn chain_tips(&self) -> Vec<ChainTip> {
        let active = self.view.read().tip();
        let mut tips = vec![ChainTip {
            height: active.height,
            hash: active.hash,
            branch_len: 0,
            status: "active",
        }];
        let headers = self.headers.lock();
        for hash in headers
            .tips()
            .into_iter()
            .filter(|hash| *hash != active.hash)
        {
            let (Some(entry), Some(branch_len)) = (
                headers.entry(&hash),
                headers.blocks_not_on(&hash, &active.hash),
            ) else {
                continue;
            };
            tips.push(ChainTip {
                height: entry.height,
                hash,
                branch_len,
                status: match entry.status {
                    Status::Invalid => "invalid",
                    Status::BodyValid => "valid-fork",
                    Status::HeaderValid | Status::BodyKnown => "headers-only",
                },
            });
        }
        tips
    }

    fn node_state(&self) -> NodeState {
        let config = self.relay.config();
        let part = |text: &str| text.parse().expect("a version number of Cargo");
        NodeState {
            version: (
                part(env!("CARGO_PKG_VERSION_MAJOR")),
                part(env!("CARGO_PKG_VERSION_MINOR")),
                part(env!("CARGO_PKG_VERSION_PATCH")),
            ),
            user_agent: config.user_agent.clone(),
            protocol_version: config.protocol_version,
            services: config.services(),
            connections: self.peers().len(),
            relay_fee_rate: MIN_RELAY_FEE_RATE,
        }
    }

    fn peers(&self) -> Vec<PeerRow> {
        self.relay
            .peers()
            .into_iter()
            .filter(|peer| peer.established)
            .map(|peer| PeerRow {
                addr: peer.addr,
                user_agent: peer.user_agent,
                version: peer.version,
                inbound: peer.direction == Direction::Inbound,
                ping_time: peer.ping_time.map(|d| d.as_secs_f64()),
                ping_wait: peer.ping_wait.map(|d| d.as_secs_f64()),
            })
            .collect()
    }

    fn mempool_size(&self) -> (usize, usize) {
        (self.store.len(), self.store.cost_bytes())
    }

    fn add_node(&self, addr: SocketAddr) -> bool {
        self.relay.peer_manager().add_peers(&[addr]) == 1
    }

    fn ping(&self) {
        self.relay.ping_peers();
    }

    fn stop(&self) {
        // A full channel holds a request already.
        let _ = self.stop.try_send(());
    }
}
