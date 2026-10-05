//! JSON-RPC dispatch: `getblocktemplate`, `submitblock`, `getblockcount`,
//! `getbestblockhash`, and on test networks `generate`, in zcashd's shapes, over JSON-RPC 1.0
//! or 2.0 as the request chose. With a [`NodeQuery`] the server also has the query methods
//! `getblockhash`, `getblock` (verbosity 0 and 1), `getblockchaininfo`, `z_gettreestate` (the
//! tip), `getrawmempool`, `sendrawtransaction`, `sendprivatetransaction` and the methods
//! of [`crate::info`].

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use hayai_consensus::{rules_at, Network};
use hayai_crypto::zcash_script::script::Code;
use hayai_crypto::zcash_script::{opcode::PossiblyBad, Opcode};
use hayai_template::messages::{Hash32, HexBytes, Submit};
use hayai_template::submission::rebuild_block;
use hayai_template::RebuiltBlock;
use hayai_wire::header::BlockHash;
use hayai_wire::RawBlock;
use serde_json::{json, Value};

use crate::feed::{TemplateFeed, Wake};
use crate::metrics::{Registry, DURATION_BUCKETS};
use crate::template::block_template;

/// JSON-RPC and zcashd error codes that this module uses.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// zcashd `RPC_INVALID_PARAMETER`.
    pub const INVALID_PARAMETER: i64 = -8;
    /// zcashd `RPC_DESERIALIZATION_ERROR` (`Block decode failed`).
    pub const DESERIALIZATION: i64 = -22;
    /// zcashd `RPC_CLIENT_IN_INITIAL_DOWNLOAD`: no template yet.
    pub const IN_WARMUP: i64 = -10;
    /// zcashd `RPC_VERIFY_REJECTED`: the mempool refused the transaction.
    pub const VERIFY_REJECTED: i64 = -26;
    /// zcashd `RPC_MISC_ERROR`.
    pub const MISC: i64 = -1;
    /// zcashd `RPC_INVALID_ADDRESS_OR_KEY`: no block has the hash.
    pub const INVALID_ADDRESS_OR_KEY: i64 = -5;
    /// zcashd `RPC_CLIENT_NODE_ALREADY_ADDED`.
    pub const NODE_ALREADY_ADDED: i64 = -23;
}

/// A block that `submitblock` handed over.
#[derive(Debug)]
pub enum SubmittedBlock {
    /// The block is a template that this node served and that the pool completed: the
    /// own-block path (the transaction set is already prepared).
    FromTemplate(RebuiltBlock),
    /// Any other block. The node validates it like a block from a peer.
    Full(RawBlock),
}

impl SubmittedBlock {
    pub fn bytes(&self) -> &Bytes {
        match self {
            SubmittedBlock::FromTemplate(b) => &b.bytes,
            SubmittedBlock::Full(b) => &b.bytes,
        }
    }
}

/// zcashd `submitblock` outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    Accepted,
    Duplicate,
    Inconclusive,
    Rejected(String),
}

/// Validator plus relay for submitted blocks.
pub trait BlockSubmitSink: Send + Sync {
    fn submit(&self, block: SubmittedBlock) -> SubmitOutcome;
}

/// The best chain tip, for `getblockcount` and `getbestblockhash`.
pub trait TipSource: Send + Sync {
    fn tip(&self) -> (u32, BlockHash);
}

/// Block production on demand for test networks (zcashd's `generate n`).
pub trait BlockGenerator: Send + Sync {
    /// Produces and commits `n` blocks on the tip. Returns their hashes in chain order, or
    /// the reason the production stopped.
    fn generate(&self, n: u32) -> Result<Vec<BlockHash>, String>;
}

/// The state after the committed tip, for `getblockchaininfo` and `z_gettreestate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipState {
    pub height: u32,
    pub hash: BlockHash,
    /// The time of the header of the tip.
    pub time: u32,
    /// The roots of the note commitment trees, in the byte order of the tree.
    pub sapling_root: [u8; 32],
    pub orchard_root: [u8; 32],
    pub ironwood_root: [u8; 32],
    /// The chain value pools in zatoshis, with the ids of zcashd and Zakura.
    pub value_pools: [(&'static str, u64); 6],
}

/// The chain value pools in zatoshis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pools {
    pub transparent: u64,
    pub sprout: u64,
    pub sapling: u64,
    pub orchard: u64,
    pub lockbox: u64,
    pub ironwood: u64,
}

/// The state after one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockState {
    /// The roots of the note commitment trees, in the byte order of the tree.
    pub sapling_root: [u8; 32],
    pub orchard_root: [u8; 32],
    /// The number of note commitments in each tree.
    pub sapling_size: u64,
    pub orchard_size: u64,
    pub ironwood_size: u64,
    pub pools: Pools,
    /// The pools after the parent block. `None`: the node does not hold that state.
    pub parent_pools: Option<Pools>,
}

/// The place of a stored block in the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub height: u32,
    /// The number of blocks from this block to the tip, with both. -1: the block is not on
    /// the committed chain.
    pub confirmations: i64,
    /// The next block of the committed chain.
    pub next: Option<BlockHash>,
    /// `None`: the node does not hold the state after this block. A node holds the state
    /// of the blocks that a reorg can disconnect, and not of an older block.
    pub state: Option<BlockState>,
}

/// One tip of the header tree, for `getchaintips`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainTip {
    pub height: u32,
    pub hash: BlockHash,
    /// Blocks of the branch that are not on the committed chain.
    pub branch_len: u32,
    /// `active`, `valid-fork`, `headers-only` or `invalid`.
    pub status: &'static str,
}

/// One connected peer, for `getpeerinfo`.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerRow {
    pub addr: SocketAddr,
    pub user_agent: Option<String>,
    pub version: Option<u32>,
    pub inbound: bool,
    /// Seconds from the last answered `ping` to its `pong`.
    pub ping_time: Option<f64>,
    /// Seconds since the `ping` that has no `pong` yet.
    pub ping_wait: Option<f64>,
}

/// What the node states of itself, for `getinfo` and `getnetworkinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeState {
    /// The version of the build as `major.minor.patch`.
    pub version: (u64, u64, u64),
    pub user_agent: String,
    pub protocol_version: u32,
    /// The service bits of the `version` message.
    pub services: u64,
    /// The number of connected peers.
    pub connections: usize,
    /// The lowest fee rate of the mempool policy, in zatoshis for each 1,000 bytes.
    pub relay_fee_rate: u64,
}

/// The chain and the mempool of the node, for the query methods.
pub trait NodeQuery: Send + Sync {
    /// The hash of the block at `height` on the committed chain.
    fn block_hash(&self, height: u32) -> Option<BlockHash>;
    /// The wire bytes of a stored block.
    fn block_bytes(&self, hash: &BlockHash) -> Option<Bytes>;
    fn tip_state(&self) -> TipState;
    /// The transaction ids of the mempool, in the byte order of the wire.
    fn mempool_txids(&self) -> Vec<[u8; 32]>;
    /// Admits a transaction into the mempool and announces it to the peers. Returns the
    /// transaction id in the byte order of the wire, or the reason of the refusal.
    ///
    /// `private`: the node does not announce the transaction and does not give it to a
    /// peer before a block contains it. The transaction is in the block template.
    fn send_transaction(&self, bytes: Bytes, private: bool) -> Result<[u8; 32], String>;
    /// The place of the stored block `hash` in the chain. `None`: no stored block.
    fn block_info(&self, hash: &BlockHash) -> Option<BlockInfo>;
    /// The time and the `nBits` of the block at `height` on the committed chain.
    fn header_context(&self, height: u32) -> Option<(u32, u32)>;
    /// The committed tip first, then each other tip of the header tree.
    fn chain_tips(&self) -> Vec<ChainTip>;
    fn node_state(&self) -> NodeState;
    fn peers(&self) -> Vec<PeerRow>;
    /// The number of transactions of the mempool and the total of their wire bytes.
    fn mempool_size(&self) -> (usize, usize);
    /// Adds a peer address to the address book. `false`: the book has the address.
    fn add_node(&self, addr: SocketAddr) -> bool;
    /// Sends a `ping` to each peer.
    fn ping(&self);
    /// Asks the node to stop as for SIGINT.
    fn stop(&self);
}

#[derive(Clone)]
pub struct RpcConfig {
    /// The network: with a height it gives the rule set, and thus the consensus branch id,
    /// of a template or of a submitted block.
    pub network: Network,
    /// Longest time that a long poll blocks before it answers with the unchanged template.
    pub long_poll_max: Duration,
    /// Minimum time that a long poll waits before it answers a transaction-set-only change
    /// (zcashd checks the mempool every 10 s; Zakura every 5 s).
    pub long_poll_set_delay: Duration,
    /// The registry of the request metrics, which have the names and the labels of
    /// Zakura: `rpc_requests_total`, `rpc_request_duration_seconds`, `rpc_errors_total`
    /// and `rpc_active_requests`. `None`: the server counts no request.
    pub metrics: Option<Arc<Registry>>,
}

impl RpcConfig {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            long_poll_max: Duration::from_secs(60),
            long_poll_set_delay: Duration::from_secs(5),
            metrics: None,
        }
    }
}

/// Counts one request in `registry`. `method` is the name in the request, or `unknown`
/// for a method that the server does not have: the number of series has a bound.
fn count_request(registry: &Registry, method: &str, elapsed: Duration, error: Option<i64>) {
    let method = match error {
        Some(codes::METHOD_NOT_FOUND) => "unknown",
        _ => method,
    };
    let status = match error {
        Some(_) => "error",
        None => "success",
    };
    registry
        .counter(
            "rpc_requests_total",
            "RPC requests by method and status.",
            &[("method", method), ("status", status)],
        )
        .inc();
    registry
        .histogram(
            "rpc_request_duration_seconds",
            "Time from the dispatch of an RPC request to its result.",
            &[("method", method)],
            &DURATION_BUCKETS,
        )
        .observe_duration(elapsed);
    if let Some(code) = error {
        registry
            .counter(
                "rpc_errors_total",
                "RPC errors by method and JSON-RPC error code.",
                &[("method", method), ("error_code", &code.to_string())],
            )
            .inc();
    }
}

pub struct Rpc {
    pub(crate) config: RpcConfig,
    pub(crate) feed: Arc<TemplateFeed>,
    submit: Arc<dyn BlockSubmitSink>,
    pub(crate) tip: Arc<dyn TipSource>,
    generator: Option<Arc<dyn BlockGenerator>>,
    query: Option<Arc<dyn NodeQuery>>,
}

pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
}

pub(crate) fn err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
    }
}

impl Rpc {
    pub fn new(
        config: RpcConfig,
        feed: Arc<TemplateFeed>,
        submit: Arc<dyn BlockSubmitSink>,
        tip: Arc<dyn TipSource>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            feed,
            submit,
            tip,
            generator: None,
            query: None,
        })
    }

    /// As [`Rpc::new`], and `generate` produces blocks through `generator`. Without a
    /// generator `generate` is an unknown method.
    pub fn with_generator(
        config: RpcConfig,
        feed: Arc<TemplateFeed>,
        submit: Arc<dyn BlockSubmitSink>,
        tip: Arc<dyn TipSource>,
        generator: Arc<dyn BlockGenerator>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            feed,
            submit,
            tip,
            generator: Some(generator),
            query: None,
        })
    }

    /// A server with each optional part: `generate` needs `generator`, and the query
    /// methods need `query`. A method without its part is an unknown method.
    pub fn with_parts(
        config: RpcConfig,
        feed: Arc<TemplateFeed>,
        submit: Arc<dyn BlockSubmitSink>,
        tip: Arc<dyn TipSource>,
        generator: Option<Arc<dyn BlockGenerator>>,
        query: Option<Arc<dyn NodeQuery>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            feed,
            submit,
            tip,
            generator,
            query,
        })
    }

    pub fn feed(&self) -> &Arc<TemplateFeed> {
        &self.feed
    }

    /// Handles one request body and returns the response body. Malformed JSON and invalid
    /// requests produce JSON-RPC error responses. They never produce a transport failure.
    pub fn handle(&self, body: &[u8]) -> Vec<u8> {
        let response = match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(request)) => self.handle_object(&request),
            Ok(_) => error_response(
                &Value::Null,
                false,
                codes::INVALID_REQUEST,
                "request is not an object",
            ),
            Err(e) => error_response(
                &Value::Null,
                false,
                codes::PARSE_ERROR,
                format!("parse error: {e}"),
            ),
        };
        serde_json::to_vec(&response).expect("response is serializable")
    }

    fn handle_object(&self, request: &serde_json::Map<String, Value>) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let v2 = matches!(request.get("jsonrpc"), Some(Value::String(v)) if v == "2.0");
        let Some(Value::String(method)) = request.get("method") else {
            return error_response(&id, v2, codes::INVALID_REQUEST, "missing method");
        };
        let params = match request.get("params") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                return error_response(&id, v2, codes::INVALID_REQUEST, "params must be an array")
            }
        };
        let started = Instant::now();
        let active = self.config.metrics.as_ref().map(|registry| {
            registry.gauge("rpc_active_requests", "RPC requests in progress.", &[])
        });
        if let Some(active) = &active {
            active.add(1.0);
        }
        let result = self.dispatch(method, &params);
        if let (Some(registry), Some(active)) = (&self.config.metrics, &active) {
            let error = result.as_ref().err().map(|e| e.code);
            count_request(registry, method, started.elapsed(), error);
            active.add(-1.0);
        }
        match result {
            Ok(result) => {
                if v2 {
                    json!({ "jsonrpc": "2.0", "result": result, "id": id })
                } else {
                    json!({ "result": result, "error": Value::Null, "id": id })
                }
            }
            Err(e) => error_response(&id, v2, e.code, e.message),
        }
    }

    fn dispatch(&self, method: &str, params: &[Value]) -> Result<Value, RpcError> {
        match method {
            "getblocktemplate" => self.get_block_template(params),
            "submitblock" => self.submit_block(params),
            "getblockcount" => Ok(json!(self.tip.tip().0)),
            "getbestblockhash" => Ok(json!(self.tip.tip().1.to_string())),
            "generate" => self.generate(params),
            "getblockhash"
            | "getblock"
            | "getblockchaininfo"
            | "z_gettreestate"
            | "getrawmempool"
            | "sendrawtransaction"
            | "sendprivatetransaction" => self.query(method, params),
            other => match (crate::info::METHODS.contains(&other), &self.query) {
                (true, Some(query)) => self.info(query.as_ref(), other, params),
                _ => Err(err(
                    codes::METHOD_NOT_FOUND,
                    format!("Method not found: {other}"),
                )),
            },
        }
    }

    fn get_block_template(&self, params: &[Value]) -> Result<Value, RpcError> {
        let options = match params.first() {
            None | Some(Value::Null) => serde_json::Map::new(),
            Some(Value::Object(o)) => o.clone(),
            Some(_) => {
                return Err(err(
                    codes::INVALID_PARAMETER,
                    "getblocktemplate takes an optional object",
                ))
            }
        };
        if let Some(mode) = options.get("mode") {
            if mode != "template" {
                return Err(err(
                    codes::INVALID_PARAMETER,
                    format!("mode {mode} is not supported; only \"template\""),
                ));
            }
        }
        let wants_long_poll = match options.get("capabilities") {
            Some(Value::Array(caps)) => caps.iter().any(|c| c == "longpoll"),
            _ => false,
        };
        let long_poll_id = match options.get("longpollid") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.parse::<u64>().map_err(|_| {
                err(
                    codes::INVALID_PARAMETER,
                    "longpollid is not one this node issued",
                )
            })?),
            Some(_) => return Err(err(codes::INVALID_PARAMETER, "longpollid must be a string")),
        };
        let (template, submitold) = match (long_poll_id, wants_long_poll) {
            (Some(since), true) => {
                let Some((template, wake)) = self.feed.wait_for_newer(
                    since,
                    self.config.long_poll_set_delay,
                    self.config.long_poll_max,
                ) else {
                    return Err(err(codes::IN_WARMUP, "no block template yet"));
                };
                let submitold = match wake {
                    Wake::Tip => Some(false),
                    Wake::Set | Wake::Timeout => Some(true),
                };
                (template, submitold)
            }
            _ => {
                let Some(template) = self.feed.current() else {
                    return Err(err(codes::IN_WARMUP, "no block template yet"));
                };
                (template, None)
            }
        };
        let result = block_template(&template, self.config.network, unix_time(), submitold)
            .map_err(|e| err(codes::INTERNAL, e.to_string()))?;
        serde_json::to_value(result).map_err(|e| err(codes::INTERNAL, e.to_string()))
    }

    fn generate(&self, params: &[Value]) -> Result<Value, RpcError> {
        let Some(generator) = &self.generator else {
            return Err(err(codes::METHOD_NOT_FOUND, "Method not found: generate"));
        };
        let n = match params.first().and_then(Value::as_u64) {
            Some(n) => u32::try_from(n)
                .map_err(|_| err(codes::INVALID_PARAMETER, "block count is too large"))?,
            None => {
                return Err(err(
                    codes::INVALID_PARAMETER,
                    "generate takes the number of blocks",
                ))
            }
        };
        let hashes = generator
            .generate(n)
            .map_err(|reason| err(codes::INTERNAL, reason))?;
        Ok(json!(hashes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()))
    }

    fn query(&self, method: &str, params: &[Value]) -> Result<Value, RpcError> {
        let Some(query) = &self.query else {
            return Err(err(
                codes::METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            ));
        };
        let block_hash = |param: Option<&Value>| block_param(query.as_ref(), param);
        match method {
            "getblockhash" => Ok(json!(block_hash(params.first())?.to_string())),
            "getblock" => {
                // The default verbosity is 1, as in zcashd and Zakura.
                let verbosity = match params.get(1) {
                    None | Some(Value::Null) => Some(1),
                    Some(Value::Number(n)) => n.as_u64(),
                    Some(_) => None,
                };
                let hash = block_hash(params.first())?;
                match verbosity {
                    Some(0) => {
                        let bytes = query
                            .block_bytes(&hash)
                            .ok_or_else(|| err(codes::INVALID_PARAMETER, "Block not found"))?;
                        Ok(json!(hex::encode(bytes)))
                    }
                    Some(1) => self.block_object(query.as_ref(), &hash),
                    _ => Err(err(
                        codes::INVALID_PARAMETER,
                        "getblock has the verbosity 0 and 1",
                    )),
                }
            }
            "getblockchaininfo" => {
                let state = query.tip_state();
                let pools: Vec<Value> = state
                    .value_pools
                    .iter()
                    .map(|(id, zat)| json!({ "id": id, "chainValueZat": zat }))
                    .collect();
                Ok(json!({
                    "chain": self.config.network.name(),
                    "blocks": state.height,
                    "bestblockhash": state.hash.to_string(),
                    "valuePools": pools,
                }))
            }
            "z_gettreestate" => {
                let state = query.tip_state();
                if block_hash(params.first())? != state.hash {
                    return Err(err(
                        codes::INVALID_PARAMETER,
                        "z_gettreestate has the tree state of the tip only",
                    ));
                }
                // zcashd and Zakura print the Sapling root as a block hash (bytes
                // reversed) and the Orchard and Ironwood roots in the order of the tree.
                let tree = |root: String| json!({ "commitments": { "finalRoot": root } });
                Ok(json!({
                    "hash": state.hash.to_string(),
                    "height": state.height,
                    "time": state.time,
                    "sapling": tree(display(state.sapling_root)),
                    "orchard": tree(hex::encode(state.orchard_root)),
                    "ironwood": tree(hex::encode(state.ironwood_root)),
                }))
            }
            "getrawmempool" => Ok(json!(query
                .mempool_txids()
                .into_iter()
                .map(display)
                .collect::<Vec<_>>())),
            "sendrawtransaction" | "sendprivatetransaction" => {
                let Some(Value::String(hexdata)) = params.first() else {
                    return Err(err(
                        codes::INVALID_PARAMETER,
                        format!("{method} takes the transaction as a hex string"),
                    ));
                };
                let bytes = hex::decode(hexdata)
                    .map_err(|_| err(codes::DESERIALIZATION, "TX decode failed"))?;
                query
                    .send_transaction(Bytes::from(bytes), method == "sendprivatetransaction")
                    .map(|txid| json!(display(txid)))
                    .map_err(|reason| err(codes::VERIFY_REJECTED, reason))
            }
            other => unreachable!("{other} is not a query method"),
        }
    }

    fn submit_block(&self, params: &[Value]) -> Result<Value, RpcError> {
        let Some(Value::String(hexdata)) = params.first() else {
            return Err(err(
                codes::INVALID_PARAMETER,
                "submitblock takes the block as a hex string",
            ));
        };
        let work_id = match params.get(1) {
            Some(Value::Object(o)) => match o.get("workid") {
                Some(Value::String(s)) => s.parse::<u64>().ok(),
                _ => None,
            },
            _ => None,
        };
        let bytes =
            hex::decode(hexdata).map_err(|_| err(codes::DESERIALIZATION, "Block decode failed"))?;
        let block = self.parse_block(Bytes::from(bytes))?;
        let submitted = match work_id.and_then(|id| self.rebuild_from_template(id, &block)) {
            Some(rebuilt) => SubmittedBlock::FromTemplate(rebuilt),
            None => SubmittedBlock::Full(block),
        };
        Ok(match self.submit.submit(submitted) {
            SubmitOutcome::Accepted => Value::Null,
            SubmitOutcome::Duplicate => json!("duplicate"),
            SubmitOutcome::Inconclusive => json!("inconclusive"),
            SubmitOutcome::Rejected(reason) => {
                tracing::info!(reason, "submitblock rejected");
                json!("rejected")
            }
        })
    }

    /// Parses a submitted block with the consensus branch id of its own height. The height
    /// is the one that the coinbase scriptSig states. The first parse uses the branch id of
    /// the block after the tip. A block of a height with another branch id is parsed again
    /// with that one. A height without a rule set is an error.
    fn parse_block(&self, bytes: Bytes) -> Result<RawBlock, RpcError> {
        let decode = |e: &dyn std::fmt::Display| {
            err(codes::DESERIALIZATION, format!("Block decode failed: {e}"))
        };
        let branch_at = |height: u32| {
            rules_at(self.config.network, height)
                .map(|rules| rules.branch_id)
                .map_err(|e| decode(&e))
        };
        let next = branch_at(self.tip.tip().0 + 1)?;
        let block = RawBlock::parse(bytes.clone(), next).map_err(|e| decode(&e))?;
        let Some(height) = coinbase_height(&block) else {
            return Err(decode(&"the coinbase states no height"));
        };
        let branch = branch_at(height)?;
        if branch == next {
            return Ok(block);
        }
        RawBlock::parse(bytes, branch).map_err(|e| decode(&e))
    }

    /// The own-block path: the submitted block is template `id` with the header fields and
    /// the coinbase of the pool. `rebuild_block` must reproduce it byte for byte.
    fn rebuild_from_template(&self, id: u64, block: &RawBlock) -> Option<RebuiltBlock> {
        let coinbase = block.txs.first()?;
        let submit = Submit {
            template_id: id,
            time: block.header.time,
            nonce: Hash32(block.header.nonce),
            solution: HexBytes(Bytes::from(block.header.solution.clone())),
            coinbase: Some(HexBytes(coinbase.bytes.clone())),
        };
        let rebuilt = self
            .feed
            .with_store(|store| rebuild_block(store, &submit))
            .ok()?;
        if rebuilt.bytes != block.bytes {
            return None;
        }
        Some(rebuilt)
    }
}

/// A hash in display hex: the bytes in reverse order.
pub(crate) fn display(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

/// The hash of the block that `param` names: a height (a number, or a string of digits as
/// zcashd takes it) of the committed chain, or a hash in display hex.
pub(crate) fn block_param(
    query: &dyn NodeQuery,
    param: Option<&Value>,
) -> Result<BlockHash, RpcError> {
    let height = match param {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) if s.len() == 64 => {
            let mut bytes: [u8; 32] = hex::decode(s)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| err(codes::INVALID_PARAMETER, "block hash is not hex"))?;
            bytes.reverse();
            return Ok(BlockHash(bytes));
        }
        Some(Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    };
    let height = height
        .and_then(|h| u32::try_from(h).ok())
        .ok_or_else(|| err(codes::INVALID_PARAMETER, "a block height or hash is needed"))?;
    query
        .block_hash(height)
        .ok_or_else(|| err(codes::INVALID_PARAMETER, "Block height out of range"))
}

/// The height that the scriptSig of the first transaction of `block` starts with
/// (`CScript() << height`, BIP 34). `None` when the scriptSig starts with no number or
/// with a number that is no height.
fn coinbase_height(block: &RawBlock) -> Option<u32> {
    let input = block.txs.first()?.tx.transparent_bundle()?.vin.first()?;
    let script = Code(input.script_sig().0 .0.clone());
    let Some(Ok(PossiblyBad::Good(Opcode::PushValue(push)))) = script.parse().next() else {
        return None;
    };
    u32::try_from(push.to_num().ok()?).ok()
}

fn error_response(id: &Value, v2: bool, code: i64, message: impl Into<String>) -> Value {
    let error = json!({ "code": code, "message": message.into() });
    if v2 {
        json!({ "jsonrpc": "2.0", "error": error, "id": id })
    } else {
        json!({ "result": Value::Null, "error": error, "id": id })
    }
}

fn unix_time() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}
