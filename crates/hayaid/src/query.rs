//! The query methods of the JSON-RPC server: the committed chain, the state after the tip
//! and the mempool, as `hayai_rpc::NodeQuery`.

use std::sync::Arc;

use bytes::Bytes;
use hayai_blockstore::BlockStore;
use hayai_net::Relay;
use hayai_prepared::PreparedStore;
use hayai_rpc::{NodeQuery, TipState};
use hayai_state::ChainView;
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::{RawTx, TxLookup};
use parking_lot::RwLock;

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
}

impl NodeQuery for Query {
    fn block_hash(&self, height: u32) -> Option<BlockHash> {
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

    fn send_transaction(&self, bytes: Bytes) -> Result<[u8; 32], String> {
        let height = self.view.read().tip_height() + 1;
        let branch = self.params.branch_at(height).map_err(|e| e.to_string())?;
        let tx = Arc::new(RawTx::parse(bytes, branch).map_err(|e| e.to_string())?);
        self.mempool.admit(tx.clone()).map_err(|e| e.to_string())?;
        self.relay.announce_tx(&tx);
        Ok(*tx.txid.as_ref())
    }
}
