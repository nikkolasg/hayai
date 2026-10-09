//! The methods of the wallet index, with the request and answer shapes of Zakura
//! (`zakura-rpc/src/methods.rs`): `getrawtransaction`, `gettxout`, `getaddressbalance`,
//! `getaddresstxids`, `getaddressutxos` and `z_getsubtreesbyindex`. `gettxout`, and
//! `getrawtransaction` for a transaction of the mempool or of a named block, need no index.
//! `docs/hayaid.md` has the table of the differences.

use bytes::Bytes;
use hayai_consensus::{rules_at, Network};
use hayai_crypto::zcash_address::{ConversionError, ToAddress, TryFromAddress, ZcashAddress};
use hayai_crypto::zcash_protocol::consensus::NetworkType;
use hayai_wire::header::BlockHash;
use hayai_wire::{RawBlock, RawTx};
use serde_json::{json, Map, Value};

use crate::rpc::{
    codes, display, err, IndexError, NodeQuery, Rpc, RpcError, SubtreePool, TransparentAddress,
};

/// The methods of [`Rpc::indexed`].
pub(crate) const METHODS: [&str; 6] = [
    "getrawtransaction",
    "gettxout",
    "getaddressbalance",
    "getaddresstxids",
    "getaddressutxos",
    "z_getsubtreesbyindex",
];

/// Zatoshis as ZEC, as Zakura prints them.
fn zec(zatoshis: i64) -> f64 {
    zatoshis as f64 / 100_000_000.0
}

fn index_error(method: &str, e: IndexError) -> RpcError {
    match e {
        IndexError::Off => err(
            codes::MISC,
            format!(
                "{method} needs the wallet index: set [state] wallet_index = true and start \
                 the node with an empty cache_dir"
            ),
        ),
        IndexError::Failed(reason) => err(codes::MISC, format!("wallet index: {reason}")),
    }
}

/// The transparent receiver of an address text: P2PKH, P2SH, or the P2PKH hash of a TEX
/// address.
impl TryFromAddress for TransparentAddress {
    type Error = &'static str;

    fn try_from_transparent_p2pkh(
        _network: NetworkType,
        hash: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self { p2sh: false, hash })
    }

    fn try_from_transparent_p2sh(
        _network: NetworkType,
        hash: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self { p2sh: true, hash })
    }

    fn try_from_tex(
        _network: NetworkType,
        hash: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        Ok(Self { p2sh: false, hash })
    }
}

/// The network type of the transparent addresses of `network`: the test networks share one
/// encoding.
fn transparent_network(network: Network) -> NetworkType {
    match network.network_type() {
        NetworkType::Main => NetworkType::Main,
        NetworkType::Test | NetworkType::Regtest => NetworkType::Test,
    }
}

fn parse_address(network: Network, text: &str) -> Result<TransparentAddress, RpcError> {
    let invalid = || {
        err(
            codes::INVALID_ADDRESS_OR_KEY,
            format!("invalid address: {text}"),
        )
    };
    let address = ZcashAddress::try_from_encoded(text).map_err(|_| invalid())?;
    let expected = transparent_network(network);
    address
        .convert_if_network::<TransparentAddress>(expected)
        .map_err(|_| invalid())
}

pub(crate) fn encode_address(network: Network, address: &TransparentAddress) -> String {
    let net = transparent_network(network);
    match address.p2sh {
        true => ZcashAddress::from_transparent_p2sh(net, address.hash),
        false => ZcashAddress::from_transparent_p2pkh(net, address.hash),
    }
    .encode()
}

/// The addresses of a request, and its object when the request is an object.
type AddressRequest = (Vec<TransparentAddress>, Option<Map<String, Value>>);

/// The address list of a request: one address text, or an object with `addresses`. Zakura
/// takes both forms (`GetAddressBalanceRequest`, `GetAddressTxIdsRequest`,
/// `GetAddressUtxosRequest`).
fn addresses_of(
    network: Network,
    params: &[Value],
    method: &str,
) -> Result<AddressRequest, RpcError> {
    let bad = || {
        err(
            codes::MISC,
            format!("{method} takes an address or {{\"addresses\": [...]}}"),
        )
    };
    match params {
        [Value::String(text)] => Ok((vec![parse_address(network, text)?], None)),
        [Value::Object(object)] => {
            let Some(Value::Array(list)) = object.get("addresses") else {
                return Err(bad());
            };
            let mut addresses = Vec::with_capacity(list.len());
            for entry in list {
                let Value::String(text) = entry else {
                    return Err(bad());
                };
                addresses.push(parse_address(network, text)?);
            }
            Ok((addresses, Some(object.clone())))
        }
        _ => Err(bad()),
    }
}

fn txid_param(params: &[Value], method: &str, code: i64) -> Result<[u8; 32], RpcError> {
    let Some(Value::String(text)) = params.first() else {
        return Err(err(codes::MISC, format!("{method} takes a txid")));
    };
    let mut bytes: [u8; 32] = hex::decode(text)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| err(code, "invalid txid"))?;
    bytes.reverse();
    Ok(bytes)
}

/// A height bound of `getaddresstxids`.
fn height_param(object: &Map<String, Value>, key: &str) -> Result<Option<u32>, RpcError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .map(Some)
            .ok_or_else(|| err(codes::MISC, format!("{key} is a height"))),
    }
}

/// The script type and the address of a scriptPubKey, as Zakura names them.
fn script_object(network: Network, script: &[u8]) -> Value {
    let address = match script {
        [0x76, 0xa9, 0x14, hash @ .., 0x88, 0xac] if hash.len() == 20 => Some(TransparentAddress {
            p2sh: false,
            hash: hash.try_into().expect("20 bytes"),
        }),
        [0xa9, 0x14, hash @ .., 0x87] if hash.len() == 20 => Some(TransparentAddress {
            p2sh: true,
            hash: hash.try_into().expect("20 bytes"),
        }),
        _ => None,
    };
    let kind = match (address, script) {
        (Some(TransparentAddress { p2sh: false, .. }), _) => "pubkeyhash",
        (Some(TransparentAddress { p2sh: true, .. }), _) => "scripthash",
        (None, [0x6a, ..]) => "nulldata",
        (None, [33, key @ .., 0xac]) if key.len() == 33 => "pubkey",
        (None, [65, key @ .., 0xac]) if key.len() == 65 => "pubkey",
        (None, _) => "nonstandard",
    };
    let mut object = json!({ "hex": hex::encode(script), "type": kind });
    if let Some(address) = address {
        object["reqSigs"] = json!(1);
        object["addresses"] = json!([encode_address(network, &address)]);
    }
    object
}

/// Where a transaction of `getrawtransaction` is.
enum Place {
    Mempool,
    Block {
        height: u32,
        hash: BlockHash,
        time: u32,
        confirmations: i64,
        active: bool,
    },
}

impl Rpc {
    /// The methods of [`METHODS`].
    pub(crate) fn indexed(
        &self,
        query: &dyn NodeQuery,
        method: &str,
        params: &[Value],
    ) -> Result<Value, RpcError> {
        let network = self.config.network;
        let index = |e| index_error(method, e);
        match method {
            "getrawtransaction" => self.raw_transaction(query, params),
            "gettxout" => {
                let txid = txid_param(params, method, codes::INVALID_PARAMETER)?;
                let Some(index) = params.get(1).and_then(Value::as_u64) else {
                    return Err(err(
                        codes::MISC,
                        "gettxout takes a txid and an output index",
                    ));
                };
                let index = u32::try_from(index)
                    .map_err(|_| err(codes::INVALID_PARAMETER, "invalid output index"))?;
                let include_mempool = match params.get(2) {
                    None | Some(Value::Null) => true,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(err(codes::MISC, "include_mempool is a boolean")),
                };
                let Some(out) = query.tx_out(&txid, index, include_mempool) else {
                    return Ok(Value::Null);
                };
                let (tip_height, tip_hash) = self.tip.tip();
                Ok(json!({
                    "bestblock": tip_hash.to_string(),
                    "confirmations": out.height.map_or(0, |h| tip_height.saturating_sub(h) + 1),
                    "value": zec(out.value as i64),
                    "scriptPubKey": script_object(network, &out.script),
                    "coinbase": out.coinbase,
                }))
            }
            "getaddressbalance" => {
                let (addresses, _) = addresses_of(network, params, method)?;
                let (balance, received) = query.address_balance(&addresses).map_err(index)?;
                Ok(json!({ "balance": balance, "received": received }))
            }
            "getaddresstxids" => {
                let (addresses, object) = addresses_of(network, params, method)?;
                let (tip, _) = query.index_tip().map_err(index)?;
                let (start, end) = match &object {
                    Some(object) => (height_param(object, "start")?, height_param(object, "end")?),
                    None => (None, None),
                };
                // Zakura `build_height_range`: no start is 0, no end or 0 is the tip, and
                // both bounds stop at the tip.
                let start = start.unwrap_or(0).min(tip);
                let end = match end {
                    None | Some(0) => tip,
                    Some(end) => end.min(tip),
                };
                if start > end {
                    return Err(err(
                        codes::INVALID_PARAMS,
                        format!("start {start:?} must be less than or equal to end {end:?}"),
                    ));
                }
                let txids = query.address_txids(&addresses, start, end).map_err(index)?;
                Ok(json!(txids.into_iter().map(display).collect::<Vec<_>>()))
            }
            "getaddressutxos" => {
                let (addresses, object) = addresses_of(network, params, method)?;
                let chain_info = match object.as_ref().and_then(|o| o.get("chainInfo")) {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(err(codes::MISC, "chainInfo is a boolean")),
                };
                let (utxos, (height, hash)) = query.address_utxos(&addresses).map_err(index)?;
                let utxos: Vec<Value> = utxos
                    .iter()
                    .map(|u| {
                        let script = match u.address.p2sh {
                            true => [&[0xa9, 0x14][..], &u.address.hash, &[0x87]].concat(),
                            false => {
                                [&[0x76, 0xa9, 0x14][..], &u.address.hash, &[0x88, 0xac]].concat()
                            }
                        };
                        json!({
                            "address": encode_address(network, &u.address),
                            "txid": display(u.txid),
                            "outputIndex": u.index,
                            "script": hex::encode(script),
                            "satoshis": u.value,
                            "height": u.height,
                        })
                    })
                    .collect();
                match chain_info {
                    false => Ok(Value::Array(utxos)),
                    true => Ok(json!({
                        "utxos": utxos,
                        "hash": hash.to_string(),
                        "height": height,
                    })),
                }
            }
            "z_getsubtreesbyindex" => {
                let pool = match params.first() {
                    Some(Value::String(name)) if name == "sapling" => SubtreePool::Sapling,
                    Some(Value::String(name)) if name == "orchard" => SubtreePool::Orchard,
                    Some(Value::String(name)) if name == "ironwood" => SubtreePool::Ironwood,
                    _ => {
                        return Err(err(
                            codes::MISC,
                            "invalid pool name, must be one of: [\"sapling\", \"orchard\", \"ironwood\"]",
                        ))
                    }
                };
                let u16_param = |at: usize| match params.get(at) {
                    None | Some(Value::Null) => Ok(None),
                    Some(value) => value
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .map(Some)
                        .ok_or_else(|| err(codes::MISC, format!("parameter {} is a u16", at + 1))),
                };
                let Some(start) = u16_param(1)? else {
                    return Err(err(codes::MISC, "z_getsubtreesbyindex takes a start_index"));
                };
                let limit = u16_param(2)?;
                let subtrees = query.subtrees(pool, start, limit).map_err(index)?;
                Ok(json!({
                    "pool": params[0],
                    "start_index": start,
                    "subtrees": subtrees
                        .iter()
                        .map(|t| json!({ "root": hex::encode(t.root), "end_height": t.end_height }))
                        .collect::<Vec<_>>(),
                }))
            }
            other => unreachable!("{other} is not a method of the wallet index"),
        }
    }

    /// `getrawtransaction txid [verbose] [blockhash]`: the mempool first (without a block
    /// hash), then the named block or the wallet index.
    fn raw_transaction(&self, query: &dyn NodeQuery, params: &[Value]) -> Result<Value, RpcError> {
        let not_found = || {
            err(
                codes::INVALID_ADDRESS_OR_KEY,
                "Transaction not found in mempool or best chain",
            )
        };
        let txid = txid_param(params, "getrawtransaction", codes::INVALID_ADDRESS_OR_KEY)?;
        let verbose = match params.get(1) {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => matches!(n.as_u64(), Some(n) if n != 0),
            Some(_) => return Err(err(codes::MISC, "verbose is a number")),
        };
        let blockhash = match params.get(2) {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => {
                let mut bytes: [u8; 32] = hex::decode(text)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| err(codes::INVALID_ADDRESS_OR_KEY, "invalid block hash"))?;
                bytes.reverse();
                Some(BlockHash(bytes))
            }
            Some(_) => return Err(err(codes::INVALID_ADDRESS_OR_KEY, "invalid block hash")),
        };
        let (tip_height, _) = self.tip.tip();
        let (bytes, place) = match blockhash {
            None => match query.mempool_transaction(&txid) {
                Some(bytes) => (bytes, Place::Mempool),
                None => {
                    let Some((height, position)) = query
                        .transaction_location(&txid)
                        .map_err(|e| index_error("getrawtransaction", e))?
                    else {
                        return Err(not_found());
                    };
                    let hash = query.block_hash(height).ok_or_else(not_found)?;
                    let block = query.block_bytes(&hash).ok_or_else(not_found)?;
                    let bytes = RawBlock::tx_bytes(&block, usize::from(position))
                        .map_err(|e| err(codes::INTERNAL, format!("stored block: {e}")))?;
                    let place = self.block_place(query, &block, height, hash, tip_height)?;
                    (bytes, place)
                }
            },
            Some(hash) => {
                let block = query
                    .block_bytes(&hash)
                    .ok_or_else(|| err(codes::INVALID_ADDRESS_OR_KEY, "block not found"))?;
                let info = query
                    .block_info(&hash)
                    .ok_or_else(|| err(codes::INVALID_ADDRESS_OR_KEY, "block not found"))?;
                let branch = rules_at(self.config.network, info.height)
                    .map_err(|e| err(codes::INTERNAL, e.to_string()))?
                    .branch_id;
                let parsed = RawBlock::parse(block.clone(), branch)
                    .map_err(|e| err(codes::INTERNAL, format!("stored block: {e}")))?;
                let Some(tx) = parsed.txs.iter().find(|t| *t.txid.as_ref() == txid) else {
                    return Err(err(codes::INVALID_ADDRESS_OR_KEY, "txid not found"));
                };
                let place = self.block_place(query, &block, info.height, hash, tip_height)?;
                (tx.bytes.clone(), place)
            }
        };
        let height = match place {
            Place::Mempool => tip_height + 1,
            Place::Block { height, .. } => height,
        };
        let branch = rules_at(self.config.network, height)
            .map_err(|e| err(codes::INTERNAL, e.to_string()))?
            .branch_id;
        let tx = RawTx::parse(bytes.clone(), branch)
            .map_err(|e| err(codes::INTERNAL, format!("stored transaction: {e}")))?;
        if *tx.txid.as_ref() != txid {
            return Err(err(
                codes::INTERNAL,
                "the wallet index and the block store disagree on the transaction",
            ));
        }
        match verbose {
            false => Ok(json!(hex::encode(&bytes))),
            true => Ok(self.transaction_object(&tx, &bytes, &place)),
        }
    }

    fn block_place(
        &self,
        query: &dyn NodeQuery,
        block: &Bytes,
        height: u32,
        hash: BlockHash,
        tip_height: u32,
    ) -> Result<Place, RpcError> {
        let header = hayai_wire::header::BlockHeader::parse(block)
            .map_err(|e| err(codes::INTERNAL, format!("stored block: {e}")))?;
        let active = query.block_hash(height) == Some(hash);
        Ok(Place::Block {
            height,
            hash,
            time: header.time,
            confirmations: match active {
                true => i64::from(tip_height) - i64::from(height) + 1,
                false => 0,
            },
            active,
        })
    }

    /// The verbose answer of `getrawtransaction`, with the fields of Zakura's
    /// `TransactionObject` that `docs/hayaid.md` lists.
    fn transaction_object(&self, tx: &RawTx, bytes: &Bytes, place: &Place) -> Value {
        let network = self.config.network;
        let data = &tx.tx;
        let version = data.version();
        let overwintered = version.has_overwinter();
        let mut vin = Vec::new();
        let mut vout = Vec::new();
        if let Some(bundle) = data.transparent_bundle() {
            for input in &bundle.vin {
                let script = hex::encode(&input.script_sig().0 .0);
                vin.push(match bundle.is_coinbase() {
                    true => json!({ "coinbase": script, "sequence": input.sequence() }),
                    false => json!({
                        "txid": display(*input.prevout().hash()),
                        "vout": input.prevout().n(),
                        "scriptSig": { "hex": script },
                        "sequence": input.sequence(),
                    }),
                });
            }
            for (n, output) in bundle.vout.iter().enumerate() {
                let value = output.value().into_u64() as i64;
                vout.push(json!({
                    "value": zec(value),
                    "valueZat": value,
                    "n": n,
                    "scriptPubKey": script_object(network, &output.script_pubkey().0 .0),
                }));
            }
        }
        let value_balance = i64::from(data.sapling_value_balance());
        let mut object = json!({
            "in_active_chain": matches!(place, Place::Block { active: true, .. }),
            "hex": hex::encode(bytes),
        });
        if let Place::Block {
            height,
            confirmations,
            active,
            ..
        } = place
        {
            object["height"] = match active {
                true => json!(height),
                false => json!(-1),
            };
            object["confirmations"] = json!(confirmations);
        }
        object["vin"] = Value::Array(vin);
        object["vout"] = Value::Array(vout);
        object["valueBalance"] = json!(zec(value_balance));
        object["valueBalanceZat"] = json!(value_balance);
        object["size"] = json!(bytes.len());
        if let Place::Block { time, .. } = place {
            object["time"] = json!(time);
        }
        object["txid"] = json!(display(*tx.txid.as_ref()));
        // Zakura has an authorizing digest from v5 on.
        if version.header() & 0x7fff_ffff >= 5 {
            object["authdigest"] = json!(display(tx.auth_digest));
        }
        object["overwintered"] = json!(overwintered);
        object["version"] = json!(version.header() & 0x7fff_ffff);
        if overwintered {
            object["versiongroupid"] = json!(format!("{:08x}", version.version_group_id()));
        }
        object["locktime"] = json!(data.lock_time());
        if overwintered {
            object["expiryheight"] = json!(u32::from(data.expiry_height()));
        }
        if let Place::Block { hash, time, .. } = place {
            object["blockhash"] = json!(hash.to_string());
            object["blocktime"] = json!(time);
        }
        object
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_addresses_round_trip_and_follow_the_network() {
        let p2pkh = TransparentAddress {
            p2sh: false,
            hash: [3; 20],
        };
        let p2sh = TransparentAddress {
            p2sh: true,
            hash: [4; 20],
        };
        for network in [Network::Mainnet, Network::Testnet, Network::Regtest] {
            for address in [p2pkh, p2sh] {
                let text = encode_address(network, &address);
                assert_eq!(parse_address(network, &text).ok(), Some(address));
            }
        }
        let main = encode_address(Network::Mainnet, &p2pkh);
        let Err(e) = parse_address(Network::Testnet, &main) else {
            panic!("a Mainnet address on Testnet");
        };
        assert_eq!(e.code, codes::INVALID_ADDRESS_OR_KEY);
    }

    #[test]
    fn script_types_follow_zakura() {
        let p2pkh = [&[0x76, 0xa9, 0x14][..], &[1; 20], &[0x88, 0xac]].concat();
        let object = script_object(Network::Testnet, &p2pkh);
        assert_eq!(object["type"], "pubkeyhash");
        assert_eq!(object["reqSigs"], 1);
        assert_eq!(object["addresses"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            script_object(Network::Testnet, &[0x51])["type"],
            "nonstandard"
        );
        assert_eq!(
            script_object(Network::Testnet, &[0x6a, 1, 2])["type"],
            "nulldata"
        );
        assert_eq!(
            script_object(Network::Testnet, &[0x51]).get("addresses"),
            None
        );
    }
}
