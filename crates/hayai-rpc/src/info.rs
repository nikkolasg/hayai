//! The methods that pools and operators use, with the fields of Zakura
//! (`zakura-rpc/src/methods.rs`): `getinfo`, `getmininginfo`, `getblocksubsidy`,
//! `getnetworksolps`, `getnetworkhashps`, `getdifficulty`, `getnetworkinfo`, `getpeerinfo`,
//! `getmempoolinfo`, `getblockheader`, `getblock` with verbosity 1, `getchaintips`,
//! `validateaddress`, `z_validateaddress`, `addnode`, `ping`, `stop`,
//! `getbestblockheightandhash` and `getdeprecationinfo`. `docs/hayaid.md` has the table of
//! the differences.

use std::net::SocketAddr;

use hayai_consensus::coinbase::OutputKind;
use hayai_consensus::difficulty::{block_work, target_from_compact};
use hayai_consensus::funding::{funding_streams, Receiver};
use hayai_consensus::{address_of, rules_at, Network, Upgrade};
use hayai_crypto::primitive_types::U256;
use hayai_crypto::zcash_address::unified::{self, Container};
use hayai_crypto::zcash_address::{ConversionError, TryFromAddress, ZcashAddress};
use hayai_crypto::zcash_encoding::CompactSize;
use hayai_crypto::zcash_protocol::consensus::NetworkType;
use hayai_crypto::{orchard, sapling_crypto};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::RawBlock;
use serde_json::{json, Map, Value};

use crate::rpc::{block_param, codes, display, err, BlockInfo, NodeQuery, Pools, Rpc, RpcError};

/// The methods of [`Rpc::info`].
pub(crate) const METHODS: [&str; 18] = [
    "getinfo",
    "getmininginfo",
    "getblocksubsidy",
    "getnetworksolps",
    "getnetworkhashps",
    "getdifficulty",
    "getnetworkinfo",
    "getpeerinfo",
    "getmempoolinfo",
    "getblockheader",
    "getchaintips",
    "validateaddress",
    "z_validateaddress",
    "addnode",
    "ping",
    "stop",
    "getbestblockheightandhash",
    "getdeprecationinfo",
];

/// Blocks of the solution rate without a parameter (zcashd and Zakura).
const SOLUTION_RATE_BLOCKS: i64 = 120;
/// The `specification` of a funding stream before NU6 and from NU6.1, and in NU6 (Zakura
/// `FundingStreamReceiver::info`).
const FUNDING_STREAM_SPECIFICATION: &str = "https://zips.z.cash/zip-0214";
const LOCKBOX_SPECIFICATION: &str = "https://zips.z.cash/zip-1015";

/// Zatoshis as ZEC, as Zakura prints them (`Zec::lossy_zec`).
fn zec(zatoshis: u64) -> f64 {
    zatoshis as f64 / 100_000_000.0
}

/// The name of the network in `chain` fields (Zakura `bip70_network_name`).
fn chain_name(network: Network) -> &'static str {
    match network.network_type() {
        NetworkType::Main => "main",
        NetworkType::Test | NetworkType::Regtest => "test",
    }
}

fn is_testnet(network: Network) -> bool {
    !matches!(network.network_type(), NetworkType::Main)
}

/// The difficulty of `bits` as zcashd computes it from the compact forms
/// (Zakura `CompactDifficulty::relative_to_network`).
fn difficulty_of_bits(network: Network, bits: u32) -> f64 {
    let limit = network.params().pow_limit_bits;
    let mut ratio = f64::from(limit << 8) / f64::from(bits << 8);
    let (mut exponent, limit_exponent) = (bits >> 24, limit >> 24);
    while exponent < limit_exponent {
        ratio *= 256.0;
        exponent += 1;
    }
    while exponent > limit_exponent {
        ratio /= 256.0;
        exponent -= 1;
    }
    ratio
}

/// The difficulty of the target of `bits` against the limit of the network, from the high
/// 128 bits of each target (Zakura `chain_tip_difficulty`). The limit is the target of its
/// compact form, as in Zakura. 0 when `bits` encode no target.
fn difficulty_of_target(network: Network, bits: u32) -> f64 {
    let limit = target_from_compact(network.params().pow_limit_bits);
    let (Some(limit), Some(target)) = (limit, target_from_compact(bits)) else {
        return 0.0;
    };
    (limit >> 128).as_u128() as f64 / (target >> 128).as_u128() as f64
}

/// A parameter that the method does not take. Zakura and zcashd answer it with the code
/// -1, not with the code -32602 of JSON-RPC.
fn bad_params(message: impl Into<String>) -> RpcError {
    err(codes::MISC, message)
}

/// The chain value pools as Zakura prints them (`GetBlockchainInfoBalance::value_pools`),
/// with the change of each pool in the block when `delta` has the pools before it.
fn pools_json(pools: &Pools, before: Option<&Pools>) -> Value {
    let list = |p: &Pools| {
        [
            ("transparent", p.transparent),
            ("sprout", p.sprout),
            ("sapling", p.sapling),
            ("orchard", p.orchard),
            ("lockbox", p.lockbox),
            ("ironwood", p.ironwood),
        ]
    };
    let before = before.map(list);
    let pools: Vec<Value> = list(pools)
        .into_iter()
        .enumerate()
        .map(|(i, (id, value))| {
            let mut pool = balance_json(value);
            pool["id"] = json!(id);
            if let Some(before) = &before {
                let delta = i128::from(value) - i128::from(before[i].1);
                pool["valueDelta"] = json!(delta as f64 / 100_000_000.0);
                pool["valueDeltaZat"] = json!(delta as i64);
            }
            pool
        })
        .collect();
    Value::Array(pools)
}

fn balance_json(zatoshis: u64) -> Value {
    json!({
        "chainValue": zec(zatoshis),
        "chainValueZat": zatoshis,
        "monitored": zatoshis != 0,
    })
}

fn total(pools: &Pools) -> u64 {
    [
        pools.transparent,
        pools.sprout,
        pools.sapling,
        pools.orchard,
        pools.lockbox,
        pools.ironwood,
    ]
    .into_iter()
    .fold(0, u64::saturating_add)
}

/// What an address is, for the two validation methods.
struct Address {
    network: NetworkType,
    kind: &'static str,
}

impl Address {
    fn transparent(&self) -> bool {
        matches!(self.kind, "p2pkh" | "p2sh")
    }
}

fn valid_sapling(data: &[u8; 43]) -> bool {
    let Some(_) = sapling_crypto::PaymentAddress::from_bytes(data) else {
        return false;
    };
    true
}

/// The address kinds that Zakura takes (`zakura-chain/src/primitives/address.rs`): the
/// transparent kinds with TEX, Sapling, and a Unified Address whose receivers are all
/// valid and known.
impl TryFromAddress for Address {
    type Error = &'static str;

    fn try_from_transparent_p2pkh(
        network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self {
            network,
            kind: "p2pkh",
        })
    }

    fn try_from_transparent_p2sh(
        network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self {
            network,
            kind: "p2sh",
        })
    }

    fn try_from_tex(
        network: NetworkType,
        _data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self {
            network,
            kind: "p2pkh",
        })
    }

    fn try_from_sapling(
        network: NetworkType,
        data: [u8; 43],
    ) -> Result<Self, ConversionError<Self::Error>> {
        if !valid_sapling(&data) {
            return Err(ConversionError::User("not a valid Sapling address"));
        }
        Ok(Self {
            network,
            kind: "sapling",
        })
    }

    fn try_from_unified(
        network: NetworkType,
        address: unified::Address,
    ) -> Result<Self, ConversionError<Self::Error>> {
        for receiver in address.items() {
            let valid = match receiver {
                unified::Receiver::Orchard(data) => {
                    let address: Option<orchard::Address> =
                        orchard::Address::from_raw_address_bytes(&data).into();
                    let Some(_) = address else {
                        return Err(ConversionError::User("not a valid Orchard receiver"));
                    };
                    true
                }
                unified::Receiver::Sapling(data) => valid_sapling(&data),
                unified::Receiver::P2pkh(_) | unified::Receiver::P2sh(_) => true,
                unified::Receiver::Unknown { .. } => false,
            };
            if !valid {
                return Err(ConversionError::User(
                    "a receiver of the Unified Address is not valid",
                ));
            }
        }
        Ok(Self {
            network,
            kind: "unified",
        })
    }
}

fn parse_address(text: &str) -> Option<Address> {
    ZcashAddress::try_from_encoded(text)
        .ok()?
        .convert::<Address>()
        .ok()
}

/// `validateaddress`: a transparent address of the network. The test networks share one
/// encoding.
fn validate_address(network: Network, text: &str) -> Value {
    match parse_address(text) {
        Some(address)
            if address.transparent()
                && matches!(address.network, NetworkType::Main) != is_testnet(network) =>
        {
            json!({ "isvalid": true, "address": text, "isscript": address.kind == "p2sh" })
        }
        _ => json!({ "isvalid": false }),
    }
}

/// `z_validateaddress`: a transparent, Sapling or Unified address of the network. A
/// transparent address of Regtest has the Testnet encoding.
fn z_validate_address(network: Network, text: &str) -> Value {
    let Some(address) = parse_address(text) else {
        return json!({ "isvalid": false });
    };
    let expected = match network.network_type() {
        NetworkType::Regtest if address.transparent() => NetworkType::Test,
        network_type => network_type,
    };
    match address.network == expected {
        true => json!({
            "isvalid": true,
            "address": text,
            "address_type": address.kind,
            // The node has no wallet.
            "ismine": false,
        }),
        false => json!({ "isvalid": false }),
    }
}

/// The number of transactions of the block `bytes`.
fn transaction_count(bytes: &[u8]) -> Option<u64> {
    let header = BlockHeader::parse(bytes).ok()?;
    CompactSize::read(bytes.get(header.serialized_len()..)?).ok()
}

fn string_param<'a>(params: &'a [Value], method: &str) -> Result<&'a str, RpcError> {
    match params.first() {
        Some(Value::String(text)) => Ok(text),
        _ => Err(bad_params(format!("{method} takes one string"))),
    }
}

/// An optional integer parameter. A parameter of another type is an error.
fn int_param(params: &[Value], at: usize, method: &str) -> Result<Option<i64>, RpcError> {
    match params.get(at) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| bad_params(format!("parameter {} of {method} is an integer", at + 1))),
    }
}

impl Rpc {
    /// The bits of the next block: the bits of the template on the tip. Without such a
    /// template (the node synchronizes), the bits of the tip block.
    fn next_bits(&self, query: &dyn NodeQuery) -> Result<u32, RpcError> {
        let (height, hash) = self.tip.tip();
        match self.feed.current() {
            Some(template) if template.tip.parent_hash == hash => Ok(template.tip.bits),
            _ => query
                .header_context(height)
                .map(|(_, bits)| bits)
                .ok_or_else(|| err(codes::MISC, "the header of the tip is not stored")),
        }
    }

    /// The solution rate of the network in solutions for each second: the work of
    /// `blocks` blocks that end at `height`, over the time that they took (Zakura
    /// `zakura-state/src/service/read/difficulty.rs`, `solution_rate`).
    fn solution_rate(
        &self,
        query: &dyn NodeQuery,
        blocks: Option<i64>,
        height: Option<i64>,
    ) -> Result<u64, RpcError> {
        let tip = self.tip.tip().0;
        // A height below 0, at the tip or above it is the tip.
        let start = height
            .and_then(|h| u32::try_from(h).ok())
            .map_or(tip, |h| h.min(tip));
        let blocks = match blocks.unwrap_or(SOLUTION_RATE_BLOCKS) {
            n if n >= 1 => n as u64,
            // The averaging window of the difficulty rule at the height.
            _ => u64::from(
                rules_at(self.config.network, start)
                    .map_err(|e| err(codes::MISC, e.to_string()))?
                    .difficulty
                    .averaging_window,
            ),
        };
        // One more header gives the time at which the work on the first block started.
        // Its work is not in the total.
        let mut times = Vec::new();
        let mut work = U256::zero();
        let mut last = U256::zero();
        for height in (0..=start).rev().take(blocks.saturating_add(1) as usize) {
            let Some((time, bits)) = query.header_context(height) else {
                break;
            };
            times.push(time);
            last = block_work(bits).unwrap_or_default();
            work = work.saturating_add(last);
        }
        let (Some(min), Some(max)) = (times.iter().min(), times.iter().max()) else {
            return Ok(0);
        };
        // One block, or blocks with one time: zcashd and Zakura answer 0.
        if max == min {
            return Ok(0);
        }
        let rate = (work - last) / U256::from(max - min);
        Ok(rate.min(U256::from(u64::MAX)).as_u64())
    }

    /// `getblocksubsidy`.
    fn block_subsidy(&self, query: &dyn NodeQuery, params: &[Value]) -> Result<Value, RpcError> {
        let network = self.config.network;
        let tip = self.tip.tip().0;
        let height = match int_param(params, 0, "getblocksubsidy")? {
            None => tip,
            Some(h) => u32::try_from(h).map_err(|_| bad_params("the height is out of range"))?,
        };
        // From the NSM reissuance height the subsidy depends on the value pools after the
        // parent. The node has them for the block after the tip only.
        let terms = match height == tip + 1 {
            true => {
                let pools = query.tip_state().value_pools;
                let issued = pools
                    .iter()
                    .fold(0u64, |sum, (_, v)| sum.saturating_add(*v));
                hayai_consensus::coinbase::terms_after(network, height, issued)
            }
            false => hayai_consensus::coinbase::terms_at(network, height),
        }
        .map_err(|e| err(codes::MISC, e.to_string()))?;
        let founders: u64 = terms
            .required
            .iter()
            .filter(|output| output.kind == OutputKind::FoundersReward)
            .map(|output| output.value)
            .sum();
        // Zakura names the streams by the upgrade of the height: the NU6 names in NU6 only.
        let nu6 = network.upgrade_at(height) == Upgrade::Nu6;
        let specification = match nu6 {
            true => LOCKBOX_SPECIFICATION,
            false => FUNDING_STREAM_SPECIFICATION,
        };
        let mut streams = funding_streams(network, height, terms.subsidy.total)
            .map_err(|e| err(codes::MISC, e.to_string()))?;
        // The order of zcashd.
        streams.sort_by_key(|stream| match stream.receiver {
            Receiver::Ecc => 0,
            Receiver::ZcashFoundation => 1,
            Receiver::MajorGrants => 2,
            Receiver::Deferred => 3,
        });
        let (mut funding, mut lockbox) = (Vec::new(), Vec::new());
        let (mut funding_total, mut lockbox_total) = (0u64, 0u64);
        for stream in streams {
            let recipient = match (stream.receiver, nu6) {
                (Receiver::Ecc, _) => "Electric Coin Company",
                (Receiver::ZcashFoundation, _) => "Zcash Foundation",
                (Receiver::MajorGrants, true) => "Zcash Community Grants NU6",
                (Receiver::MajorGrants, false) => "Major Grants",
                (Receiver::Deferred, _) => "Lockbox NU6",
            };
            let mut entry = json!({
                "recipient": recipient,
                "specification": specification,
                "value": zec(stream.value),
                "valueZat": stream.value,
            });
            match stream.script {
                Some(script) => {
                    entry["address"] = json!(address_of(network, &script));
                    funding_total += stream.value;
                    funding.push(entry);
                }
                None => {
                    lockbox_total += stream.value;
                    lockbox.push(entry);
                }
            }
        }
        let mut result = json!({
            "miner": zec(terms.miner_subsidy),
            "founders": zec(founders),
            "fundingstreamstotal": zec(funding_total),
            "lockboxtotal": zec(lockbox_total),
            "totalblocksubsidy": zec(terms.subsidy.total),
        });
        if !funding.is_empty() {
            result["fundingstreams"] = Value::Array(funding);
        }
        if !lockbox.is_empty() {
            result["lockboxstreams"] = Value::Array(lockbox);
        }
        Ok(result)
    }

    /// The stored block that the first parameter names, with its place in the chain.
    fn stored_block(
        &self,
        query: &dyn NodeQuery,
        hash: &BlockHash,
    ) -> Result<(bytes::Bytes, BlockInfo), RpcError> {
        // The message and the codes of Zakura: -5 for a hash, -8 for a height.
        let missing = || {
            err(
                codes::INVALID_ADDRESS_OR_KEY,
                "block height not in best chain",
            )
        };
        let bytes = query.block_bytes(hash).ok_or_else(missing)?;
        let info = query.block_info(hash).ok_or_else(missing)?;
        Ok((bytes, info))
    }

    /// The verbose form of `getblockheader`. `finalsaplingroot` needs the state after the
    /// block: the object has it when the node holds that state, and for a block before
    /// Sapling, where it is zero.
    fn header_object(&self, hash: &BlockHash, header: &BlockHeader, info: &BlockInfo) -> Value {
        let network = self.config.network;
        let sapling = matches!(
            network.activation_height(Upgrade::Sapling),
            Some(activation) if info.height >= activation
        );
        let mut nonce = header.nonce;
        nonce.reverse();
        // Zakura prints the commitment of a block before Sapling in the order of the
        // header, and each later commitment as a hash.
        let commitments = match sapling {
            true => display(header.block_commitments),
            false => hex::encode(header.block_commitments),
        };
        let mut object = json!({
            "hash": hash.to_string(),
            "confirmations": info.confirmations,
            "height": info.height,
            "version": header.version,
            "merkleroot": display(header.merkle_root),
            "blockcommitments": commitments,
            "time": header.time,
            "nonce": hex::encode(nonce),
            "solution": hex::encode(&header.solution),
            "bits": format!("{:08x}", header.bits),
            "difficulty": difficulty_of_bits(network, header.bits),
            "previousblockhash": header.prev_hash.to_string(),
        });
        match (sapling, &info.state) {
            (false, _) => object["finalsaplingroot"] = json!(hex::encode([0u8; 32])),
            (true, Some(state)) => object["finalsaplingroot"] = json!(display(state.sapling_root)),
            (true, None) => {}
        }
        if let Some(next) = info.next {
            object["nextblockhash"] = json!(next.to_string());
        }
        object
    }

    /// `getblockheader`.
    fn block_header(&self, query: &dyn NodeQuery, params: &[Value]) -> Result<Value, RpcError> {
        let verbose = match params.get(1) {
            None | Some(Value::Null) => true,
            Some(Value::Bool(verbose)) => *verbose,
            Some(_) => return Err(bad_params("parameter 2 of getblockheader is a boolean")),
        };
        let hash = block_param(query, params.first())?;
        let (bytes, info) = self.stored_block(query, &hash)?;
        let header = BlockHeader::parse(&bytes)
            .map_err(|e| err(codes::INTERNAL, format!("stored block: {e}")))?;
        Ok(match verbose {
            true => self.header_object(&hash, &header, &info),
            false => json!(hex::encode(header.serialize())),
        })
    }

    /// `getblock` with verbosity 1: the fields of the header object, the size, the
    /// transaction ids and, when the node holds the state after the block, the Orchard
    /// root, the tree sizes and the value pools.
    pub(crate) fn block_object(
        &self,
        query: &dyn NodeQuery,
        hash: &BlockHash,
    ) -> Result<Value, RpcError> {
        let network = self.config.network;
        let (bytes, info) = self.stored_block(query, hash)?;
        let stored = |e: &dyn std::fmt::Display| err(codes::INTERNAL, format!("stored block: {e}"));
        let branch = rules_at(network, info.height)
            .map_err(|e| stored(&e))?
            .branch_id;
        let block = RawBlock::parse(bytes.clone(), branch).map_err(|e| stored(&e))?;
        let active = |upgrade| {
            matches!(
                network.activation_height(upgrade),
                Some(activation) if info.height >= activation
            )
        };
        let mut object = self.header_object(hash, &block.header, &info);
        object["size"] = json!(bytes.len());
        object["nTx"] = json!(block.txs.len());
        object["tx"] = block
            .txs
            .iter()
            .map(|tx| json!(display(*tx.txid.as_ref())))
            .collect();
        let Some(state) = &info.state else {
            return Ok(object);
        };
        if active(Upgrade::Nu5) {
            object["finalorchardroot"] = json!(hex::encode(state.orchard_root));
        }
        object["chainSupply"] = balance_json(total(&state.pools));
        object["valuePools"] = pools_json(&state.pools, state.parent_pools.as_ref());
        // Zakura leaves an empty Sapling or Orchard tree out, and has the Ironwood tree
        // from NU6.3.
        let mut trees = Map::new();
        if state.sapling_size > 0 {
            trees.insert("sapling".into(), json!({ "size": state.sapling_size }));
        }
        if state.orchard_size > 0 {
            trees.insert("orchard".into(), json!({ "size": state.orchard_size }));
        }
        if active(Upgrade::Nu6_3) {
            trees.insert("ironwood".into(), json!({ "size": state.ironwood_size }));
        }
        object["trees"] = Value::Object(trees);
        Ok(object)
    }

    /// The methods of [`METHODS`].
    pub(crate) fn info(
        &self,
        query: &dyn NodeQuery,
        method: &str,
        params: &[Value],
    ) -> Result<Value, RpcError> {
        let network = self.config.network;
        let (tip_height, tip_hash) = self.tip.tip();
        let version = |node: &crate::rpc::NodeState| {
            let (major, minor, patch) = node.version;
            1_000_000 * major + 10_000 * minor + 100 * patch
        };
        let relay_fee = |node: &crate::rpc::NodeState| zec(node.relay_fee_rate);
        // `stop` and `addnode` are methods of Regtest only, as in Zakura.
        let regtest_only = |message: &str, code: i64| match network.is_regtest() {
            true => Ok(()),
            false => Err(err(code, message)),
        };
        match method {
            "getinfo" => {
                let node = query.node_state();
                let (major, minor, patch) = node.version;
                Ok(json!({
                    "version": version(&node),
                    "build": format!("v{major}.{minor}.{patch}"),
                    "subversion": node.user_agent,
                    "protocolversion": node.protocol_version,
                    "blocks": tip_height,
                    "connections": node.connections,
                    "difficulty": difficulty_of_target(network, self.next_bits(query)?),
                    "testnet": is_testnet(network),
                    // The node has no wallet: no fee of its own transactions.
                    "paytxfee": 0.0,
                    "relayfee": relay_fee(&node),
                }))
            }
            "getnetworkinfo" => {
                let node = query.node_state();
                // hayai-net dials IPv4 and IPv6 addresses, has no proxy and no onion
                // transport, changes no clock by the time of its peers, and states no
                // address of its own in `version`.
                let net = |name: &str, reachable: bool| {
                    json!({
                        "name": name,
                        "limited": false,
                        "reachable": reachable,
                        "proxy": "",
                        "proxy_randomize_credentials": false,
                    })
                };
                Ok(json!({
                    "version": version(&node),
                    "subversion": node.user_agent,
                    "protocolversion": node.protocol_version,
                    "localservices": format!("{:016x}", node.services),
                    "timeoffset": 0,
                    "connections": node.connections,
                    "networks": [net("ipv4", true), net("ipv6", true), net("onion", false)],
                    "relayfee": relay_fee(&node),
                    "localaddresses": [],
                    "warnings": "",
                }))
            }
            "getpeerinfo" => Ok(query
                .peers()
                .into_iter()
                .map(|peer| {
                    let mut row = json!({ "addr": peer.addr.to_string(), "inbound": peer.inbound });
                    for (key, value) in [
                        ("subver", peer.user_agent.map(Value::from)),
                        ("version", peer.version.map(Value::from)),
                        ("pingtime", peer.ping_time.map(Value::from)),
                        ("pingwait", peer.ping_wait.map(Value::from)),
                    ] {
                        if let Some(value) = value {
                            row[key] = value;
                        }
                    }
                    row
                })
                .collect()),
            "getmempoolinfo" => {
                let (size, bytes) = query.mempool_size();
                // Zakura gives the wire bytes as `usage` too.
                Ok(json!({ "size": size, "bytes": bytes, "usage": bytes }))
            }
            "getmininginfo" => {
                let rate = self.solution_rate(query, None, None)?;
                let mut result = json!({
                    "blocks": tip_height,
                    "networksolps": rate,
                    "networkhashps": rate,
                    "chain": chain_name(network),
                    "testnet": is_testnet(network),
                });
                // The size and the transactions without the coinbase of the tip block,
                // above the genesis block.
                let tip_block = query.block_bytes(&tip_hash).filter(|_| tip_height > 0);
                if let Some(bytes) = tip_block {
                    result["currentblocksize"] = json!(bytes.len());
                    if let Some(count) = transaction_count(&bytes) {
                        result["currentblocktx"] = json!(count.saturating_sub(1));
                    }
                }
                Ok(result)
            }
            "getnetworksolps" | "getnetworkhashps" => {
                let blocks = int_param(params, 0, method)?;
                let height = int_param(params, 1, method)?;
                Ok(json!(self.solution_rate(query, blocks, height)?))
            }
            "getdifficulty" => Ok(json!(difficulty_of_target(network, self.next_bits(query)?))),
            "getblocksubsidy" => self.block_subsidy(query, params),
            "getblockheader" => self.block_header(query, params),
            "getchaintips" => Ok(query
                .chain_tips()
                .into_iter()
                .map(|tip| {
                    json!({
                        "height": tip.height,
                        "hash": tip.hash.to_string(),
                        "branchlen": tip.branch_len,
                        "status": tip.status,
                    })
                })
                .collect()),
            "validateaddress" => Ok(validate_address(network, string_param(params, method)?)),
            "z_validateaddress" => Ok(z_validate_address(network, string_param(params, method)?)),
            // Zakura prints the hash of this method as a list of its 32 bytes.
            "getbestblockheightandhash" => Ok(json!({ "height": tip_height, "hash": tip_hash.0 })),
            // The node has no end-of-service height on any network.
            "getdeprecationinfo" => Ok(json!({})),
            "ping" => {
                query.ping();
                Ok(Value::Null)
            }
            "stop" => {
                regtest_only(
                    "stop is only available on regtest networks",
                    codes::METHOD_NOT_FOUND,
                )?;
                query.stop();
                Ok(json!("hayaid server stopping"))
            }
            "addnode" => {
                let (Some(Value::String(addr)), Some(Value::String(command))) =
                    (params.first(), params.get(1))
                else {
                    return Err(bad_params("addnode takes an address and a command"));
                };
                let addr: SocketAddr = addr
                    .parse()
                    .map_err(|_| bad_params("the address is not ip:port"))?;
                if command != "add" {
                    return Err(bad_params("the command is not \"add\""));
                }
                regtest_only("addnode command is only supported on regtest", codes::MISC)?;
                match query.add_node(addr) {
                    true => Ok(Value::Null),
                    false => Err(err(
                        codes::NODE_ALREADY_ADDED,
                        format!("peer address was already present in the address book: {addr}"),
                    )),
                }
            }
            other => unreachable!("{other} is not a method of this module"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_difficulty_of_the_limit_is_1_and_follows_the_target() {
        for network in [Network::Mainnet, Network::Testnet, Network::Regtest] {
            let limit = network.params().pow_limit_bits;
            assert_eq!(difficulty_of_bits(network, limit), 1.0);
            assert_eq!(difficulty_of_target(network, limit), 1.0);
        }
        // Mainnet limit 0x1f07ffff. A mantissa of 0x03ffff is the difficulty
        // 0x07ffff / 0x03ffff, and one exponent byte less is 256 times that.
        let half = 524_287.0 / 262_143.0;
        assert_eq!(difficulty_of_bits(Network::Mainnet, 0x1f03_ffff), half);
        assert_eq!(
            difficulty_of_bits(Network::Mainnet, 0x1e03_ffff),
            half * 256.0
        );
        assert_eq!(
            difficulty_of_target(Network::Mainnet, 0x1e03_ffff),
            half * 256.0
        );
        // Bits that encode no target.
        assert_eq!(difficulty_of_target(Network::Mainnet, 0), 0.0);
    }

    #[test]
    fn zec_values_print_as_in_zakura() {
        assert_eq!(json!(zec(625_000_000)).to_string(), "6.25");
        assert_eq!(json!(zec(100)).to_string(), "1e-6");
        assert_eq!(json!(zec(0)).to_string(), "0.0");
        // Zakura `lossy_zec_round_trip_accepts_the_value_it_emitted`.
        assert_eq!(zec(14_903_462_499_999), 149_034.624_999_99);
    }

    #[test]
    fn address_validation_follows_the_network_and_the_kind() {
        // The Regtest and Testnet pay-to-script-hash and pay-to-public-key-hash addresses of
        // the tests, and a Mainnet address.
        let p2sh = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";
        let p2pkh = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV";
        let main = "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs";
        let invalid = json!({ "isvalid": false });
        for network in [Network::Regtest, Network::Testnet] {
            assert_eq!(
                validate_address(network, p2sh),
                json!({ "isvalid": true, "address": p2sh, "isscript": true })
            );
            assert_eq!(
                validate_address(network, p2pkh),
                json!({ "isvalid": true, "address": p2pkh, "isscript": false })
            );
            assert_eq!(validate_address(network, main), invalid);
            assert_eq!(
                z_validate_address(network, p2sh),
                json!({ "isvalid": true, "address": p2sh, "address_type": "p2sh", "ismine": false })
            );
            assert_eq!(z_validate_address(network, main), invalid);
        }
        assert_eq!(
            validate_address(Network::Mainnet, main),
            json!({ "isvalid": true, "address": main, "isscript": false })
        );
        assert_eq!(validate_address(Network::Mainnet, p2sh), invalid);
        assert_eq!(
            z_validate_address(Network::Mainnet, main),
            json!({ "isvalid": true, "address": main, "address_type": "p2pkh", "ismine": false })
        );
        for text in ["", "x", "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbt"] {
            assert_eq!(validate_address(Network::Mainnet, text), invalid);
            assert_eq!(z_validate_address(Network::Mainnet, text), invalid);
        }

        // A Sapling address and a Unified Address with an Orchard receiver, encoded for
        // each network: `z_validateaddress` takes the one of its network, and
        // `validateaddress` takes no shielded address.
        use hayai_crypto::zcash_address::unified::Encoding;
        use hayai_crypto::zcash_address::ToAddress;
        let sapling_bytes = {
            let sk = sapling_crypto::zip32::ExtendedSpendingKey::master(&[7; 32]);
            sk.default_address().1.to_bytes()
        };
        let orchard_bytes = {
            let sk = orchard::keys::SpendingKey::from_bytes([7; 32]).expect("a spending key");
            orchard::keys::FullViewingKey::from(&sk)
                .address_at(0u32, orchard::keys::Scope::External)
                .to_raw_address_bytes()
        };
        let unified =
            unified::Address::try_from_items(vec![unified::Receiver::Orchard(orchard_bytes)])
                .expect("a unified address");
        for (network, kind) in [
            (Network::Mainnet, NetworkType::Main),
            (Network::Testnet, NetworkType::Test),
            (Network::Regtest, NetworkType::Regtest),
        ] {
            let sapling = ZcashAddress::from_sapling(kind, sapling_bytes).encode();
            let unified = unified.encode(&kind);
            for (text, address_type) in [(&sapling, "sapling"), (&unified, "unified")] {
                assert_eq!(
                    z_validate_address(network, text),
                    json!({
                        "isvalid": true,
                        "address": text,
                        "address_type": address_type,
                        "ismine": false,
                    }),
                    "{text}"
                );
                assert_eq!(validate_address(network, text), invalid);
                let other = match network {
                    Network::Mainnet => Network::Testnet,
                    _ => Network::Mainnet,
                };
                assert_eq!(z_validate_address(other, text), invalid);
            }
        }
        // A Sapling address whose bytes are no valid address.
        let broken = ZcashAddress::from_sapling(NetworkType::Main, [0xff; 43]).encode();
        assert_eq!(z_validate_address(Network::Mainnet, &broken), invalid);
    }
}
