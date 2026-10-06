//! The wallet index on Regtest chains of hayaid nodes: the answers of its RPC methods
//! against a recomputation from the blocks, a reorg, stops as a crash does, and the
//! default without the index.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_coins::OutPoint;
use hayai_crypto::zcash_address::{ToAddress, ZcashAddress};
use hayai_crypto::zcash_protocol::consensus::{BranchId, NetworkType};
use hayai_index::AddressKey;
use hayai_wire::{RawBlock, RawTx};
use serde_json::{json, Value};

use crate::node::Node;
use crate::sync_tests::{
    addr, config_with, disconnect_all, generate, parse, rpc_call, scratch, wait_for, wait_template,
    wait_tip,
};
use crate::test_support::{spend_tx, Spend};

/// P2SH of the redeem script `OP_TRUE`: an address that a test spends without a key.
const P2SH_TRUE: [u8; 23] = [
    0xa9, 0x14, 0xda, 0x17, 0x45, 0xe9, 0xb5, 0x49, 0xbd, 0x0b, 0xfa, 0x1a, 0x56, 0x99, 0x71, 0xc7,
    0x7e, 0xba, 0x30, 0xcd, 0x5a, 0x4b, 0x87,
];
/// The scriptSig of a P2SH_TRUE coin: a push of the redeem script.
const P2SH_TRUE_SIG: [u8; 2] = [0x01, 0x51];

fn p2pkh() -> Vec<u8> {
    AddressKey::p2pkh([9; 20]).script()
}

/// A Regtest node with the RPC server. `index`: the wallet index is on. The coinbase pays
/// P2SH_TRUE when `p2sh_miner`, else `OP_TRUE`.
fn start(dir: &Path, name: &str, produce: bool, index: bool, p2sh_miner: bool) -> Node {
    let mut config = config_with(
        dir,
        name,
        produce,
        false,
        "[rpc]\nlisten_addr = \"127.0.0.1:0\"\n",
    );
    config.state.wallet_index = index;
    if p2sh_miner {
        config.mining.miner_script = Some(hex::encode(P2SH_TRUE));
    }
    Node::start(&config).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn result(node: &Node, method: &str, params: Value) -> Value {
    let answer = rpc_call(node, method, params);
    assert_eq!(answer["error"], Value::Null, "{method}: {answer}");
    answer["result"].clone()
}

/// The blocks of heights 1 to the tip of `node`, from its RPC server.
fn chain(node: &Node) -> Vec<RawBlock> {
    let (tip, _) = node.tip.tip();
    (1..=tip)
        .map(|height| {
            let text = result(node, "getblock", json!([height, 0]));
            parse(&Bytes::from(hex::decode(text.as_str().unwrap()).unwrap()))
        })
        .collect()
}

fn display(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

fn address_text(address: &AddressKey) -> String {
    match address.is_p2sh() {
        true => ZcashAddress::from_transparent_p2sh(NetworkType::Test, address.hash()),
        false => ZcashAddress::from_transparent_p2pkh(NetworkType::Test, address.hash()),
    }
    .encode()
}

/// A spend of the P2SH_TRUE coin `coin` (outpoint, value) into `outputs`, with a shielded
/// output of `shielded`.
fn spend(coin: (OutPoint, u64), outputs: &[(u64, &[u8])], shielded: Option<u64>) -> Arc<RawTx> {
    let bytes = spend_tx(&Spend {
        outpoint: coin.0,
        value: coin.1,
        coin_script: &P2SH_TRUE,
        script_sig: &P2SH_TRUE_SIG,
        outputs,
        shielded,
        expiry_height: 0,
        branch: BranchId::Nu5,
    });
    Arc::new(RawTx::parse(bytes, BranchId::Nu5).expect("a transaction"))
}

/// The output `n` of `tx` as a coin.
fn coin_of(tx: &RawTx, n: u32) -> (OutPoint, u64) {
    let out = &tx.tx.transparent_bundle().expect("transparent").vout[n as usize];
    (OutPoint::new(*tx.txid.as_ref(), n), out.value().into_u64())
}

/// The coinbase output of the block at `height`, which pays P2SH_TRUE.
fn coinbase(node: &Node, height: u32) -> (OutPoint, u64) {
    let block = &chain(node)[height as usize - 1];
    let coinbase = &block.txs[0];
    let out = &coinbase.tx.transparent_bundle().expect("transparent").vout[0];
    assert_eq!(out.script_pubkey().0 .0, P2SH_TRUE);
    coin_of(coinbase, 0)
}

/// Submits `tx` to the producer `node` and mines it in one block.
fn mine(node: &Node, tx: &Arc<RawTx>) {
    node.submit_tx(tx.clone()).expect("a valid transaction");
    wait_template(node, 1);
    generate(node, 1);
}

/// The index content that the blocks give, recomputed in the test: the outputs to
/// P2PKH and P2SH addresses and their spends.
#[derive(Default)]
struct Model {
    balance: BTreeMap<AddressKey, (i64, i64)>,
    txs: BTreeMap<AddressKey, BTreeSet<(u32, u16)>>,
    /// The unspent outputs: outpoint to address, value, height, transaction index.
    coins: HashMap<OutPoint, (AddressKey, u64, u32, u16)>,
    /// Each transaction with its height and index.
    located: Vec<([u8; 32], u32, u16, Bytes)>,
    txid_at: HashMap<(u32, u16), [u8; 32]>,
    hashes: Vec<String>,
}

impl Model {
    fn of(blocks: &[RawBlock]) -> Self {
        let mut m = Model::default();
        for (k, block) in blocks.iter().enumerate() {
            let height = k as u32 + 1;
            m.hashes.push(block.hash().to_string());
            for (i, tx) in block.txs.iter().enumerate() {
                let i = i as u16;
                let txid = *tx.txid.as_ref();
                m.located.push((txid, height, i, tx.bytes.clone()));
                m.txid_at.insert((height, i), txid);
                let Some(bundle) = tx.tx.transparent_bundle() else {
                    continue;
                };
                if !bundle.is_coinbase() {
                    for input in &bundle.vin {
                        if let Some((address, value, ..)) = m.coins.remove(input.prevout()) {
                            m.balance.entry(address).or_default().0 -= value as i64;
                            m.txs.entry(address).or_default().insert((height, i));
                        }
                    }
                }
                for (n, out) in bundle.vout.iter().enumerate() {
                    let Some(address) = AddressKey::of_script(&out.script_pubkey().0 .0) else {
                        continue;
                    };
                    let value = out.value().into_u64();
                    let entry = m.balance.entry(address).or_default();
                    entry.0 += value as i64;
                    entry.1 += value as i64;
                    m.txs.entry(address).or_default().insert((height, i));
                    m.coins
                        .insert(OutPoint::new(txid, n as u32), (address, value, height, i));
                }
            }
        }
        m
    }

    /// The answers that the index must give for the addresses and the transactions of the
    /// model, with the tip at the last block of the model.
    fn expected(&self) -> Vec<Value> {
        let tip = self.hashes.len() as u32;
        let mut out = Vec::new();
        for (address, (balance, received)) in &self.balance {
            out.push(json!({ "balance": balance, "received": received }));
            let txids: Vec<String> = self.txs[address]
                .iter()
                .map(|loc| display(self.txid_at[loc]))
                .collect();
            out.push(json!(txids));
            let mut utxos: Vec<_> = self
                .coins
                .iter()
                .filter(|(_, (a, ..))| a == address)
                .map(|(outpoint, (_, value, height, i))| {
                    (*height, *i, outpoint.n(), outpoint, *value)
                })
                .collect();
            utxos.sort_by_key(|u| (u.0, u.1, u.2));
            out.push(json!({
                "utxos": utxos
                    .iter()
                    .map(|(height, _, n, outpoint, value)| json!({
                        "address": address_text(address),
                        "txid": display(*outpoint.hash()),
                        "outputIndex": n,
                        "script": hex::encode(address.script()),
                        "satoshis": value,
                        "height": height,
                    }))
                    .collect::<Vec<_>>(),
                "hash": self.hashes.last().unwrap(),
                "height": tip,
            }));
        }
        for (txid, height, _, bytes) in &self.located {
            out.push(json!({
                "hex": hex::encode(bytes),
                "height": height,
                "confirmations": tip - height + 1,
                "blockhash": self.hashes[*height as usize - 1],
                "txid": display(*txid),
            }));
        }
        for pool in ["sapling", "orchard"] {
            out.push(json!({ "pool": pool, "start_index": 0, "subtrees": [] }));
        }
        out
    }

    /// The answers of the RPC server of `node` for the addresses and the transactions of the
    /// model.
    fn answers(&self, node: &Node) -> Vec<Value> {
        let mut out = Vec::new();
        for address in self.balance.keys() {
            let addresses = json!({ "addresses": [address_text(address)] });
            out.push(result(node, "getaddressbalance", json!([addresses])));
            out.push(result(node, "getaddresstxids", json!([addresses])));
            let mut request = addresses.clone();
            request["chainInfo"] = json!(true);
            out.push(result(node, "getaddressutxos", json!([request])));
        }
        for (txid, ..) in &self.located {
            let tx = result(node, "getrawtransaction", json!([display(*txid), 1]));
            out.push(json!({
                "hex": tx["hex"],
                "height": tx["height"],
                "confirmations": tx["confirmations"],
                "blockhash": tx["blockhash"],
                "txid": tx["txid"],
            }));
        }
        for pool in ["sapling", "orchard"] {
            out.push(result(node, "z_getsubtreesbyindex", json!([pool, 0])));
        }
        out
    }
}

/// Waits until the wallet index of `node` holds its committed tip.
fn wait_index(node: &Node) {
    let (tip, _) = node.tip.tip();
    let request =
        json!([{ "addresses": [address_text(&AddressKey::p2sh([0; 20]))], "chainInfo": true }]);
    wait_for("the wallet index at the tip", || {
        result(node, "getaddressutxos", request.clone())["height"] == tip
    });
}

/// The answers of `node` equal the recomputation from its own chain.
fn assert_index_of_chain(node: &Node) {
    wait_index(node);
    let model = Model::of(&chain(node));
    assert_eq!(model.answers(node), model.expected());
}

/// A chain with transparent spends between P2SH and P2PKH addresses and a shielded output:
/// each method of the index against the recomputation from the blocks, the other forms of
/// the requests, and the same answers after a restart.
#[test]
fn the_wallet_index_answers_as_a_recomputation_from_the_blocks() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, true, true);
    generate(&a, 101);
    // A coinbase of Regtest pays 6.25 ZEC. Each spend pays a fee of 100,000 zatoshis.
    let first = spend(
        coinbase(&a, 1),
        &[(400_000_000, &P2SH_TRUE), (100_000_000, &p2pkh())],
        Some(124_900_000),
    );
    mine(&a, &first);
    let second = spend(
        coin_of(&first, 0),
        &[(40_000_000, &p2pkh()), (359_900_000, &P2SH_TRUE)],
        None,
    );
    mine(&a, &second);
    let third = spend(coin_of(&second, 1), &[(359_800_000, &P2SH_TRUE)], None);
    mine(&a, &third);
    assert_eq!(a.tip.tip().0, 104);
    assert_index_of_chain(&a);

    // A height range, and the order of the bounds.
    let pkh = address_text(&AddressKey::p2pkh([9; 20]));
    assert_eq!(
        result(
            &a,
            "getaddresstxids",
            json!([{ "addresses": [pkh], "start": 103, "end": 103 }])
        ),
        json!([display(*second.txid.as_ref())])
    );
    assert_eq!(
        rpc_call(
            &a,
            "getaddresstxids",
            json!([{ "addresses": [pkh], "start": 104, "end": 103 }])
        )["error"]["code"],
        -32602
    );
    // One address as a string, and the answer without `chainInfo`.
    assert_eq!(
        result(&a, "getaddressbalance", json!([pkh])),
        json!({ "balance": 140_000_000u64, "received": 140_000_000u64 })
    );
    let utxos = result(&a, "getaddressutxos", json!([pkh]));
    assert_eq!(utxos.as_array().map(Vec::len), Some(2));
    // The raw form, and an unknown transaction.
    assert_eq!(
        result(
            &a,
            "getrawtransaction",
            json!([display(*third.txid.as_ref())])
        ),
        json!(hex::encode(&third.bytes))
    );
    assert_eq!(
        rpc_call(&a, "getrawtransaction", json!([display([5; 32])]))["error"]["code"],
        -5
    );
    // `gettxout`: an unspent output and a spent one.
    let out = result(&a, "gettxout", json!([display(*first.txid.as_ref()), 1]));
    assert_eq!(out["value"], 1.0);
    assert_eq!(out["confirmations"], 3);
    assert_eq!(out["scriptPubKey"]["type"], "pubkeyhash");
    assert_eq!(
        result(&a, "gettxout", json!([display(*first.txid.as_ref()), 0])),
        Value::Null
    );
    // A transaction of the mempool has no block.
    let pending = spend(coin_of(&third, 0), &[(359_700_000, &P2SH_TRUE)], None);
    a.submit_tx(pending.clone()).expect("a valid transaction");
    let tx = result(
        &a,
        "getrawtransaction",
        json!([display(*pending.txid.as_ref()), 1]),
    );
    assert_eq!(
        (tx["in_active_chain"].clone(), tx.get("height")),
        (json!(false), None)
    );
    let out = result(&a, "gettxout", json!([display(*pending.txid.as_ref()), 0]));
    assert_eq!(out["confirmations"], 0);
    assert_eq!(
        result(&a, "gettxout", json!([display(*third.txid.as_ref()), 0])),
        Value::Null
    );
    let expected = Model::of(&chain(&a)).expected();
    a.shutdown().expect("clean shutdown");

    // The clean stop and the start give the same index.
    let a = start(dir.path(), "a", false, true, true);
    wait_index(&a);
    assert_eq!(Model::of(&chain(&a)).answers(&a), expected);
    a.shutdown().expect("clean shutdown");
}

/// A reorg of depth 3 undoes the blocks of the old branch in the index, and the blocks of
/// the new branch and a later block with the returned transaction redo it.
#[test]
fn a_reorg_of_depth_three_undoes_and_redoes_the_index() {
    let dir = scratch();
    let a = start(dir.path(), "a", true, true, true);
    let b = start(dir.path(), "b", true, false, false);
    generate(&a, 105);
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&b, a.tip.tip());
    disconnect_all(&a);
    disconnect_all(&b);
    wait_for("the partition", || {
        a.relay.peers().is_empty() && b.relay.peers().is_empty()
    });

    let tx = spend(
        coinbase(&a, 1),
        &[(100_000_000, &p2pkh()), (524_900_000, &P2SH_TRUE)],
        None,
    );
    mine(&a, &tx);
    generate(&a, 2);
    assert_index_of_chain(&a);
    generate(&b, 4);
    let tip = b.tip.tip();
    b.relay.connect(addr(&a)).expect("b dials a");
    wait_tip(&a, tip);
    assert_index_of_chain(&a);
    let pkh = address_text(&AddressKey::p2pkh([9; 20]));
    assert_eq!(
        result(&a, "getaddressbalance", json!([pkh])),
        json!({ "balance": 0, "received": 0 })
    );
    // The transaction of the disconnected block is in the mempool, and the next block of
    // the node has it.
    wait_for("the transaction in the mempool", || {
        a.mempool.contains(&tx.wtxid())
    });
    wait_template(&a, 1);
    generate(&a, 1);
    assert_eq!(a.tip.tip().0, 110);
    assert_index_of_chain(&a);
    a.shutdown().expect("clean shutdown");
    b.shutdown().expect("clean shutdown");
}

/// A node with the index synchronizes a chain with transactions and stops as a crash does
/// at random heights (the seed is fixed). At each start the index undoes its blocks above
/// the base and the replay indexes them again. The final answers equal the answers of a
/// node that synchronizes without a stop, and the recomputation from the blocks.
#[test]
fn stops_at_random_points_give_the_index_of_an_uninterrupted_synchronization() {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    let dir = scratch();
    let a = start(dir.path(), "a", true, false, true);
    generate(&a, 101);
    let first = spend(
        coinbase(&a, 1),
        &[(400_000_000, &P2SH_TRUE), (100_000_000, &p2pkh())],
        Some(124_900_000),
    );
    mine(&a, &first);
    generate(&a, 20);
    let second = spend(
        coin_of(&first, 0),
        &[(200_000_000, &p2pkh()), (199_900_000, &P2SH_TRUE)],
        None,
    );
    mine(&a, &second);
    generate(&a, 40);
    let tip = a.tip.tip();
    assert_eq!(tip.0, 163);

    let mut rng = StdRng::seed_from_u64(0x0000_7761_6c6c_6574);
    let mut stops = Vec::new();
    loop {
        let x = start(dir.path(), "x", false, true, true);
        let resumed = x.tip.tip().0;
        let point = resumed + rng.gen_range(2..=8);
        if point >= tip.0 {
            x.abandon().expect("abandon");
            break;
        }
        x.relay.connect(addr(&a)).expect("x dials a");
        let deadline = Instant::now() + Duration::from_secs(60);
        while x.tip.tip().0 < point {
            assert!(Instant::now() < deadline, "no synchronization to {point}");
            thread::sleep(Duration::from_micros(200));
        }
        stops.push((resumed, x.tip.tip().0));
        x.abandon().expect("abandon");
    }
    assert!(stops.len() >= 8, "{} stops: {stops:?}", stops.len());

    let x = start(dir.path(), "x", false, true, true);
    x.relay.connect(addr(&a)).expect("x dials a");
    wait_tip(&x, tip);
    let y = start(dir.path(), "y", false, true, true);
    y.relay.connect(addr(&a)).expect("y dials a");
    wait_tip(&y, tip);
    wait_index(&x);
    wait_index(&y);
    let model = Model::of(&chain(&a));
    let expected = model.expected();
    assert_eq!(model.answers(&y), expected);
    assert_eq!(model.answers(&x), expected);
    for node in [a, x, y] {
        node.shutdown().expect("clean shutdown");
    }
}

/// The index is off by default: the node makes no file of it, and its methods answer the
/// setting that turns it on. A shadow node refuses the setting.
#[test]
fn the_wallet_index_is_off_by_default() {
    let dir = scratch();
    let config = config_with(
        dir.path(),
        "a",
        true,
        false,
        "[rpc]\nlisten_addr = \"127.0.0.1:0\"\n",
    );
    assert!(!config.state.wallet_index);
    let a = Node::start(&config).expect("the node starts");
    generate(&a, 3);
    assert!(!dir.path().join("a-data").join("wallet-index").exists());
    let answer = rpc_call(
        &a,
        "getaddressbalance",
        json!([address_text(&AddressKey::p2sh([1; 20]))]),
    );
    assert_eq!(answer["error"]["code"], -1);
    a.shutdown().expect("clean shutdown");

    let shadow = "[network]\nnetwork = \"Testnet\"\nmode = \"shadow\"\n\n[shadow]\nrpc_addr = \"127.0.0.1:1\"\n\n[state]\nwallet_index = true\n\n[mining]\nminer_script = \"51\"\n";
    let Err(e) = crate::config::Config::parse(shadow) else {
        panic!("a shadow node with the wallet index");
    };
    assert!(e.to_string().contains("wallet index"), "{e}");
}
