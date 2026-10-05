//! JSON-RPC client of the followed node (shadow mode).
//!
//! Every method is in the unauthenticated set that Zakura serves on its restricted surface
//! (`zakura-rpc/src/methods.rs`, `RPC_METHOD_ACCESS`): `getbestblockhash`, `getblockcount`,
//! `getblockhash`, `getblock`, `getrawtransaction`, `z_gettreestate`.
//! One request per connection (`Connection: close`) keeps the client to a few lines; the
//! server is on loopback.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use bytes::Bytes;
use hayai_crypto::zcash_primitives::transaction::TxId;
use hayai_wire::header::BlockHash;
use serde_json::{json, Value};

use crate::params::parse_hash;

/// zcashd `RPC_INVALID_ADDRESS_OR_KEY`: unknown transaction or block.
const RPC_NOT_FOUND: i64 = -5;

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("upstream connection: {0}")]
    Io(#[from] std::io::Error),
    #[error("upstream HTTP status {0}")]
    Http(u16),
    #[error("upstream {method}: error {code}: {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    #[error("upstream {method}: {reason}")]
    Malformed { method: String, reason: String },
}

/// What `getblock <hash> 1` says about a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub hash: BlockHash,
    pub height: u32,
    pub time: u32,
    /// `nBits` of the block header.
    pub bits: u32,
    pub prev: Option<BlockHash>,
    /// Chain value pools after the block, in zatoshis (`valuePools`, the ids `transparent`,
    /// `sprout`, `sapling`, `orchard`, `ironwood` and `lockbox` of zcashd and of Zakura's
    /// `GetBlockchainInfoBalance`, `zakura-rpc/src/methods/types/get_blockchain_info.rs`).
    pub transparent_pool: u64,
    pub sprout_pool: u64,
    pub sapling_pool: u64,
    pub orchard_pool: u64,
    /// `None` when the node reports no Ironwood value pool.
    pub ironwood_pool: Option<u64>,
    /// The deferred pool (`lockbox`). `None` when the node reports no such pool.
    pub deferred_pool: Option<u64>,
}

/// Serialized note commitment trees after a block (`z_gettreestate`, zcashd's legacy
/// `CommitmentTree` encoding). `None` is a tree without a final state in the answer: an
/// empty tree before the activation of its pool. Zakura gives the `ironwood` field the
/// form of the `sapling` and `orchard` fields (`zakura-rpc/src/methods/trees.rs`,
/// `GetTreestateResponse`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeState {
    pub sapling: Option<Vec<u8>>,
    pub orchard: Option<Vec<u8>>,
    pub ironwood: Option<Vec<u8>>,
}

pub struct Upstream {
    addr: SocketAddr,
    timeout: Duration,
}

fn malformed(method: &str, reason: impl Into<String>) -> UpstreamError {
    UpstreamError::Malformed {
        method: method.to_string(),
        reason: reason.into(),
    }
}

impl Upstream {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            timeout: Duration::from_secs(30),
        }
    }

    /// One JSON-RPC 2.0 call. A JSON-RPC error is [`UpstreamError::Rpc`].
    pub fn call(&self, method: &str, params: Value) -> Result<Value, UpstreamError> {
        let body =
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let mut stream = TcpStream::connect_timeout(&self.addr, self.timeout)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        let request = format!(
            "POST / HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            body.len()
        );
        stream.write_all(request.as_bytes())?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response)?;
        let Some(split) = response.windows(4).position(|w| w == b"\r\n\r\n") else {
            return Err(malformed(method, "no end of HTTP headers"));
        };
        let head = String::from_utf8_lossy(&response[..split]);
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| malformed(method, "no HTTP status"))?;
        let body = &response[split + 4..];
        // A JSON-RPC error may come with any status; the body decides when it parses.
        let value: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) if status != 200 => return Err(UpstreamError::Http(status)),
            Err(e) => return Err(malformed(method, format!("body: {e}"))),
        };
        match value.get("error") {
            None | Some(Value::Null) => {}
            Some(error) => {
                return Err(UpstreamError::Rpc {
                    method: method.to_string(),
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
            }
        }
        if status != 200 {
            return Err(UpstreamError::Http(status));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| malformed(method, "no result"))
    }

    fn hash_result(&self, method: &str, params: Value) -> Result<BlockHash, UpstreamError> {
        let v = self.call(method, params)?;
        let Some(s) = v.as_str() else {
            return Err(malformed(method, "hash is not a string"));
        };
        parse_hash(s).map_err(|e| malformed(method, e))
    }

    pub fn best_block_hash(&self) -> Result<BlockHash, UpstreamError> {
        self.hash_result("getbestblockhash", json!([]))
    }

    pub fn block_count(&self) -> Result<u32, UpstreamError> {
        let v = self.call("getblockcount", json!([]))?;
        v.as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .ok_or_else(|| malformed("getblockcount", "height is not a u32"))
    }

    pub fn block_hash(&self, height: u32) -> Result<BlockHash, UpstreamError> {
        self.hash_result("getblockhash", json!([height]))
    }

    /// Wire bytes of a block (`getblock <hash> 0`).
    pub fn block_bytes(&self, hash: &BlockHash) -> Result<Bytes, UpstreamError> {
        let v = self.call("getblock", json!([hash.to_string(), 0]))?;
        let Some(s) = v.as_str() else {
            return Err(malformed("getblock", "verbosity 0 is not a hex string"));
        };
        hex::decode(s)
            .map(Bytes::from)
            .map_err(|e| malformed("getblock", e.to_string()))
    }

    /// `getblock <hash> 1`: height, time, `nBits`, parent and the chain value pools.
    pub fn block_info(&self, hash: &BlockHash) -> Result<BlockInfo, UpstreamError> {
        let m = "getblock";
        let v = self.call(m, json!([hash.to_string(), 1]))?;
        let height = v["height"]
            .as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .ok_or_else(|| malformed(m, "no height"))?;
        let time = v["time"]
            .as_u64()
            .and_then(|t| u32::try_from(t).ok())
            .ok_or_else(|| malformed(m, "no time"))?;
        // zcashd and Zakura print `nBits` as 8 hex digits, most significant byte first.
        let bits = match v["bits"].as_str() {
            Some(hex) if hex.len() == 8 => u32::from_str_radix(hex, 16)
                .map_err(|e| malformed(m, format!("bits {hex}: {e}")))?,
            _ => return Err(malformed(m, "no bits of 8 hex digits")),
        };
        let prev = match v.get("previousblockhash").and_then(Value::as_str) {
            Some(s) => Some(parse_hash(s).map_err(|e| malformed(m, e))?),
            None => None,
        };
        let optional_pool = |id: &str| -> Result<Option<u64>, UpstreamError> {
            let Some(pools) = v["valuePools"].as_array() else {
                return Err(malformed(m, "no valuePools"));
            };
            let Some(entry) = pools.iter().find(|p| p["id"] == id) else {
                return Ok(None);
            };
            entry["chainValueZat"]
                .as_u64()
                .map(Some)
                .ok_or_else(|| malformed(m, format!("{id} chainValueZat is not a u64")))
        };
        let pool = |id: &str| -> Result<u64, UpstreamError> {
            optional_pool(id)?.ok_or_else(|| malformed(m, format!("no {id} value pool")))
        };
        Ok(BlockInfo {
            hash: *hash,
            height,
            time,
            bits,
            prev,
            transparent_pool: pool("transparent")?,
            sprout_pool: pool("sprout")?,
            sapling_pool: pool("sapling")?,
            orchard_pool: pool("orchard")?,
            ironwood_pool: optional_pool("ironwood")?,
            deferred_pool: optional_pool("lockbox")?,
        })
    }

    /// Wire bytes and block height of a mined transaction (`getrawtransaction <txid> 1`).
    /// `None` for a transaction the node does not know. The height is `None` for a
    /// transaction outside the best chain.
    pub fn transaction(&self, txid: &TxId) -> Result<Option<(Bytes, Option<u32>)>, UpstreamError> {
        let m = "getrawtransaction";
        let v = match self.call(m, json!([txid.to_string(), 1])) {
            Ok(v) => v,
            Err(UpstreamError::Rpc { code, .. }) if code == RPC_NOT_FOUND => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(hex_tx) = v["hex"].as_str() else {
            return Err(malformed(m, "no hex"));
        };
        let bytes = hex::decode(hex_tx).map_err(|e| malformed(m, e.to_string()))?;
        let height = v
            .get("height")
            .and_then(Value::as_i64)
            .and_then(|h| u32::try_from(h).ok());
        Ok(Some((Bytes::from(bytes), height)))
    }

    /// `z_gettreestate <hash>`: by hash, so that an upstream reorg between two calls
    /// cannot answer for another block at the same height.
    pub fn tree_state(&self, hash: &BlockHash) -> Result<TreeState, UpstreamError> {
        let m = "z_gettreestate";
        let v = self.call(m, json!([hash.to_string()]))?;
        let tree = |pool: &str| -> Result<Option<Vec<u8>>, UpstreamError> {
            match v[pool]["commitments"]["finalState"].as_str() {
                None | Some("") => Ok(None),
                Some(s) => hex::decode(s)
                    .map(Some)
                    .map_err(|e| malformed(m, format!("{pool}: {e}"))),
            }
        };
        Ok(TreeState {
            sapling: tree("sapling")?,
            orchard: tree("orchard")?,
            ironwood: tree("ironwood")?,
        })
    }
}
