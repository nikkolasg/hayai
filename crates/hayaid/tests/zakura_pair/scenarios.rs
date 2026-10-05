//! The scenarios of the pair (`docs/regtest-pair.md`).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use serde_json::{json, Value};

use crate::nodes::{best, generate, height, wait_tip, Pair};
use crate::txs::{
    branch_at, coinbase_coin, display, shielded_pool, shielding_tx, transparent_tx, txid, Coin,
    Draft, COIN_SCRIPT_SIG,
};
use crate::{rpc, Args, Report};

type R<T> = Result<T, String>;

/// The fee of a transaction of the harness: above the ZIP 317 conventional fee of each.
const FEE: u64 = 20_000;
/// Seconds that a node has to follow the other one.
const FOLLOW_S: u64 = 90;

fn pools(addr: SocketAddr) -> R<BTreeMap<String, u64>> {
    let info = rpc::call(addr, "getblockchaininfo", json!([]))?;
    let list = info["valuePools"].as_array().ok_or("no valuePools")?;
    list.iter()
        .map(|p| {
            let id = p["id"].as_str().ok_or("pool without id")?.to_string();
            let zat = p["chainValueZat"].as_u64().ok_or("pool without value")?;
            Ok((id, zat))
        })
        .collect()
}

/// Compares what both RPC servers show of the state after the common tip: the tip hash,
/// the six value pools, the roots of the note commitment trees and the chain history root
/// of the next template.
fn compare_state(pair: &Pair) -> R<String> {
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let tip = best(z)?;
    let tip_h = best(h)?;
    if tip != tip_h {
        return Err(format!("tips differ: hayaid {tip_h}, zakurad {tip}"));
    }
    let at = height(z)?;
    let (pools_h, pools_z) = (pools(h)?, pools(z)?);
    if pools_h != pools_z {
        return Err(format!(
            "value pools differ at {at}: hayaid {pools_h:?}, zakurad {pools_z:?}"
        ));
    }
    let trees_h = rpc::call(h, "z_gettreestate", json!([at]))?;
    let trees_z = rpc::call(z, "z_gettreestate", json!([at.to_string()]))?;
    let mut roots = Vec::new();
    for pool in ["sapling", "orchard", "ironwood"] {
        let root_h = &trees_h[pool]["commitments"]["finalRoot"];
        // Zakura gives no Ironwood root before NU6.3.
        let Value::String(root_z) = &trees_z[pool]["commitments"]["finalRoot"] else {
            continue;
        };
        if root_h != root_z {
            return Err(format!(
                "{pool} roots differ at {at}: hayaid {root_h}, zakurad {root_z}"
            ));
        }
        roots.push(pool);
    }
    let history = |addr| -> R<Value> {
        Ok(
            rpc::call(addr, "getblocktemplate", json!([]))?["defaultroots"]["chainhistoryroot"]
                .clone(),
        )
    };
    let (history_h, history_z) = (history(h)?, history(z)?);
    if history_h != history_z || !history_h.is_string() {
        return Err(format!(
            "chain history roots differ at {at}: hayaid {history_h}, zakurad {history_z}"
        ));
    }
    let shielded: u64 = ["orchard", "ironwood"].iter().map(|p| pools_z[*p]).sum();
    Ok(format!(
        "height {at}, tip {}, pools equal (transparent {} zat, shielded {shielded} zat), roots equal ({}), history root equal",
        &tip[..12],
        pools_z["transparent"],
        roots.join(", ")
    ))
}

/// The coinbase coin of the block at `at`. It is mature 100 blocks later.
fn coin_at(pair: &Pair, at: u32) -> R<Coin> {
    let block = rpc::call(pair.zakura_rpc(), "getblock", json!([at.to_string(), 0]))?;
    coinbase_coin(block.as_str().ok_or("getblock: no hex")?, at)
}

fn send(addr: SocketAddr, tx: &Bytes) -> R<String> {
    rpc::call(addr, "sendrawtransaction", json!([hex::encode(tx)]))?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "sendrawtransaction: no txid".into())
}

fn mempool(addr: SocketAddr) -> R<Vec<String>> {
    let ids = rpc::call(addr, "getrawmempool", json!([]))?;
    Ok(ids
        .as_array()
        .ok_or("getrawmempool: no list")?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect())
}

fn wait_mempool(addr: SocketAddr, id: &str, seconds: u64) -> R<Duration> {
    let start = Instant::now();
    loop {
        if mempool(addr)?.iter().any(|t| t == id) {
            return Ok(start.elapsed());
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err(format!(
                "{addr} does not have {id} in its mempool after {seconds} s"
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A transparent transaction of the next block that spends `coin` to one output.
fn plain_tx(pair: &Pair, coin: &Coin, fee: u64) -> R<(Bytes, String)> {
    let branch = branch_at(height(pair.zakura_rpc())? as u32 + 1);
    let tx = transparent_tx(
        branch,
        std::slice::from_ref(coin),
        &[coin.value - fee],
        0,
        &COIN_SCRIPT_SIG,
    );
    let id = display(txid(&tx, branch)?);
    Ok((tx, id))
}

/// A transaction of the next block that shields half of `coin`.
fn shield_tx(pair: &Pair, coin: &Coin) -> R<(Bytes, String, BranchId)> {
    let branch = branch_at(height(pair.zakura_rpc())? as u32 + 1);
    let half = coin.value / 2;
    let tx = shielding_tx(branch, coin, half, coin.value - half - FEE, 0);
    let id = display(txid(&tx, branch)?);
    Ok((tx, id, branch))
}

/// Waits until the template of `addr` has `count` transactions. A transaction is in the
/// mempool before it is in the template. Returns the template and the time that it took.
fn wait_template_txs(addr: SocketAddr, count: usize, seconds: u64) -> R<(Value, Duration)> {
    let start = Instant::now();
    loop {
        let template = rpc::call(addr, "getblocktemplate", json!([]))?;
        let has = template["transactions"].as_array().map_or(0, Vec::len);
        if has == count {
            return Ok((template, start.elapsed()));
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err(format!(
                "the template of {addr} has {has} transactions after {seconds} s, not {count}"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn transactions_of_tip(pair: &Pair) -> R<u64> {
    let tip = best(pair.zakura_rpc())?;
    rpc::call(pair.zakura_rpc(), "getblock", json!([tip, 1]))?["tx"]
        .as_array()
        .map(|a| a.len() as u64)
        .ok_or_else(|| "getblock: no tx list".into())
}

fn finish(pair: &mut Pair, report: &mut Report) -> R<()> {
    let sockets = pair.check_loopback_only().map(|lines| {
        format!(
            "{} sockets of the two processes, each on loopback",
            lines.len()
        )
    });
    report.result("sockets on loopback only", sockets);
    let stopped = pair
        .stop_all()
        .map(|()| "both processes stopped on SIGINT".to_string());
    report.result("clean stop", stopped);
    Ok(())
}

/// Scenario a, with the restarts of scenario f: zakurad mines 320 blocks with transparent
/// and shielded transactions, hayaid follows from the genesis block.
pub fn a(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("a", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let mut next_coin = 1u32;
    for target in (20..=320u64).step_by(20) {
        let tip = height(z)?;
        let mut sent = 0;
        if tip >= 101 {
            let (plain, _) = plain_tx(&pair, &coin_at(&pair, next_coin)?, FEE)?;
            let (shield, _, branch) = shield_tx(&pair, &coin_at(&pair, next_coin + 1)?)?;
            next_coin += 2;
            let pool = shielded_pool(branch);
            for (name, tx) in [
                ("transparent".to_string(), plain),
                (format!("{pool} shielding"), shield),
            ] {
                let result = send(z, &tx).map(|id| format!("txid {id}"));
                if report.result(
                    &format!("zakurad takes a {name} transaction at {tip}"),
                    result,
                ) {
                    sent += 1;
                }
            }
        }
        // Scenario f: hayaid is down while zakurad mines, and starts again after it.
        let down = match target {
            120 => Some("INT"),
            180 => Some("KILL"),
            _ => None,
        };
        if let Some(signal) = down {
            pair.stop_hayaid(signal)?;
        }
        let mined = generate(z, target - tip)?;
        if let Some(signal) = down {
            let start = Instant::now();
            pair.start_hayaid()?;
            report.check(
                &format!("hayaid starts again after SIG{signal} at {tip}"),
                true,
                format!("RPC ready after {} ms", start.elapsed().as_millis()),
            );
        }
        let followed = wait_tip(h, &mined, FOLLOW_S)
            .map(|d| format!("hayaid at {target} after {} ms", d.as_millis()));
        if !report.result(&format!("hayaid follows to {target}"), followed) {
            return Err(format!("hayaid did not reach height {target}"));
        }
        report.result(&format!("state at {target}"), compare_state(&pair));
        if target == 240 {
            // Scenario f: zakurad stops and starts again. hayaid dials it again. Zakura
            // writes its non-finalized blocks to disk from time to time, so it can start
            // below its last tip: it then reads the blocks from hayaid.
            pair.stop_zakurad()?;
            pair.start_zakurad()?;
            let resumed = height(z)?;
            let again = pair
                .wait_connected(60)
                .map(|()| "connected again".to_string());
            report.result("zakurad restarts at 240 and hayaid connects again", again);
            let back = wait_tip(z, &mined, FOLLOW_S).map(|d| {
                format!(
                    "zakurad started at {resumed} and is at 240 again after {} ms",
                    d.as_millis()
                )
            });
            report.result("zakurad resumes and reaches the tip of hayaid", back);
            report.result("state after the restart of zakurad", compare_state(&pair));
        }
        if sent > 0 {
            let left = mempool(z)?.len();
            report.check(
                &format!("the {sent} transactions are in the chain at {target}"),
                left == 0,
                format!("{left} left in the mempool of zakurad"),
            );
        }
    }
    finish(&mut pair, report)
}

/// Scenario b: hayaid produces the blocks, with transparent and shielded transactions from
/// its mempool, and zakurad accepts each one. zakurad mines only the NU6.1 activation
/// block (`docs/regtest-pair-findings.md`).
pub fn b(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("b", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let first = generate(h, 99)?;
    let followed = wait_tip(z, &first, FOLLOW_S).map(|d| format!("after {} ms", d.as_millis()));
    if !report.result("zakurad follows the first 99 blocks of hayaid", followed) {
        return Err("zakurad did not follow".into());
    }
    let activation = generate(z, 1)?;
    wait_tip(h, &activation, FOLLOW_S)?;
    report.check(
        "zakurad mines the NU6.1 activation block 100",
        true,
        "hayaid follows",
    );
    // The coinbase of block 2 is mature at height 102.
    let block_101 = generate(h, 1)?;
    wait_tip(z, &block_101, FOLLOW_S)?;
    let mut next_coin = 1u32;
    for target in (120..=320u64).step_by(20) {
        let tip = height(h)?;
        let (plain, _) = plain_tx(&pair, &coin_at(&pair, next_coin)?, FEE)?;
        let (shield, _, branch) = shield_tx(&pair, &coin_at(&pair, next_coin + 1)?)?;
        next_coin += 2;
        let pool = shielded_pool(branch);
        let mut sent = 0;
        for (name, tx) in [
            ("transparent".to_string(), plain),
            (format!("{pool} shielding"), shield),
        ] {
            let result = send(h, &tx).map(|id| format!("txid {id}"));
            if report.result(
                &format!("hayaid takes a {name} transaction at {tip}"),
                result,
            ) {
                sent += 1;
            }
        }
        let (_, waited) = wait_template_txs(h, sent as usize, 20)?;
        report.check(
            &format!("the template of hayaid has the {sent} transactions at {tip}"),
            true,
            format!("after {} ms", waited.as_millis()),
        );
        let first = generate(h, 1)?;
        wait_tip(z, &first, FOLLOW_S)
            .map_err(|e| format!("zakurad refuses the block {} of hayaid: {e}", tip + 1))?;
        let in_block = transactions_of_tip(&pair)?;
        report.check(
            &format!(
                "zakurad accepts block {} of hayaid with {sent} transactions",
                tip + 1
            ),
            in_block == sent + 1,
            format!("{in_block} transactions in the block"),
        );
        let mined = generate(h, target - tip - 1)?;
        let followed = wait_tip(z, &mined, FOLLOW_S)
            .map(|d| format!("zakurad at {target} after {} ms", d.as_millis()));
        if !report.result(&format!("zakurad follows to {target}"), followed) {
            return Err(format!("zakurad did not reach height {target}"));
        }
        report.result(&format!("state at {target}"), compare_state(&pair));
    }
    // A block that the harness builds on the template of hayaid, with a transaction of
    // the template, submitted through hayaid.
    let (plain, id) = plain_tx(&pair, &coin_at(&pair, next_coin)?, FEE)?;
    send(h, &plain)?;
    let (template, _) = wait_template_txs(h, 1, 20)
        .map_err(|e| format!("the template of hayaid does not have {id}: {e}"))?;
    let mut draft = Draft::from_template(&template)?;
    let data = template["transactions"][0]["data"]
        .as_str()
        .ok_or("no data")?;
    draft
        .txs
        .push(Bytes::from(hex::decode(data).map_err(|e| e.to_string())?));
    let (block, hash) = draft.build()?;
    let verdict = rpc::call(h, "submitblock", json!([block]))?;
    report.check(
        "hayaid accepts a block on its template through submitblock",
        verdict.is_null(),
        verdict.to_string(),
    );
    let followed =
        wait_tip(z, &hash.to_string(), FOLLOW_S).map(|d| format!("after {} ms", d.as_millis()));
    report.result("zakurad accepts that block", followed);
    report.result("state at the end", compare_state(&pair));
    finish(&mut pair, report)
}

/// The NU6.1 activation block without a lockbox disbursement in the configuration of
/// Zakura: hayaid accepts its own block 100, zakurad refuses it.
pub fn nu61(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("nu61", None)?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let before = generate(h, 99)?;
    wait_tip(z, &before, FOLLOW_S)?;
    let own = rpc::call(z, "generate", json!([1]));
    report.check(
        "zakurad cannot mine block 100 without a configured disbursement",
        own.is_err(),
        format!("{own:?}"),
    );
    let block_100 = generate(h, 1)?;
    std::thread::sleep(Duration::from_secs(10));
    let (tip_h, tip_z) = (height(h)?, height(z)?);
    let block = rpc::call(h, "getblock", json!([block_100, 0]))?;
    let verdict = rpc::call(z, "submitblock", json!([block]));
    // The split is the finding. The row passes while both nodes behave as the finding says.
    report.check(
        "known difference: hayaid accepts block 100 of hayaid, zakurad refuses it",
        tip_h == 100 && tip_z == 99,
        format!("hayaid at {tip_h}, zakurad at {tip_z}, zakurad submitblock: {verdict:?}"),
    );
    finish(&mut pair, report)
}

/// Scenario c: a transaction that one node takes reaches the mempool of the other node,
/// which mines it. Then the transactions that only one policy takes.
pub fn c(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("c", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let tip = generate(z, 112)?;
    wait_tip(h, &tip, FOLLOW_S)?;
    let mut next_coin = 1u32;
    let mut relay = |pair: &Pair, report: &mut Report, shielded: bool, to_hayai: bool| -> R<()> {
        let coin = coin_at(pair, next_coin)?;
        next_coin += 1;
        let (tx, id, kind) = if shielded {
            let (tx, id, branch) = shield_tx(pair, &coin)?;
            (tx, id, format!("{} shielding", shielded_pool(branch)))
        } else {
            let (tx, id) = plain_tx(pair, &coin, FEE)?;
            (tx, id, "transparent".to_string())
        };
        let (from, to, from_name, to_name) = if to_hayai {
            (h, z, "hayaid", "zakurad")
        } else {
            (z, h, "zakurad", "hayaid")
        };
        let label = format!("{kind} transaction from {from_name} to {to_name}");
        send(from, &tx).map_err(|e| format!("{label}: {from_name} refuses it: {e}"))?;
        let arrived = wait_mempool(to, &id, 30)
            .map(|d| format!("in the mempool of {to_name} after {} ms", d.as_millis()));
        if !report.result(&label, arrived) {
            return Ok(());
        }
        if to == h {
            wait_template_txs(h, 1, 20)?;
        }
        let mined = generate(to, 1)?;
        wait_tip(from, &mined, FOLLOW_S)?;
        let count = transactions_of_tip(pair)?;
        let left = mempool(h)?.len() + mempool(z)?.len();
        report.check(
            &format!("{to_name} mines it and {from_name} accepts the block"),
            count == 2 && left == 0,
            format!("{count} transactions in the block, {left} left in the two mempools"),
        );
        Ok(())
    };
    for shielded in [false, true] {
        for to_hayai in [false, true] {
            relay(&pair, report, shielded, to_hayai)?;
        }
    }
    report.result("state after the relayed transactions", compare_state(&pair));

    // The policies. Both nodes take no unpaid action. hayai has the ZIP 317 marginal fee of
    // 5,000 zatoshis for each logical action, Zakura has 400 zatoshis (`zakura-chain`,
    // `unmined/zip317.rs`). The count of unpaid actions in the name of a row is the count
    // of ZIP 317.
    for (outputs, fee) in [
        (1u64, 0u64),
        (1, 1_000),
        (1, 9_999),
        (1, 10_000),
        (2, 5_000),
        (40, 5_000),
        (52, 5_000),
        (60, 5_000),
        (60, 23_000),
    ] {
        let coin = coin_at(&pair, next_coin)?;
        next_coin += 1;
        let branch = branch_at(height(z)? as u32 + 1);
        let values = vec![(coin.value - fee) / outputs; outputs as usize];
        let rest = (coin.value - fee) % outputs;
        let tx = transparent_tx(
            branch,
            std::slice::from_ref(&coin),
            &values,
            0,
            &COIN_SCRIPT_SIG,
        );
        let id = display(txid(&tx, branch)?);
        // The division rest goes to the fee.
        let fee = fee + rest;
        // ZIP 317: the outputs count as their bytes (32 for each) divided by 34, rounded up.
        let unpaid = (outputs * 32)
            .div_ceil(34)
            .max(2)
            .saturating_sub(fee / 5_000);
        // The second node can have the transaction from the first one: that is an accept
        // of its policy too.
        let known = |verdict: R<String>| match verdict {
            Err(e) if e.contains("already") => Ok(format!("accepted from the peer ({e})")),
            other => other,
        };
        let verdict_z = known(send(z, &tx));
        let verdict_h = known(send(h, &tx));
        let (took_h, took_z) = (verdict_h.is_ok(), verdict_z.is_ok());
        // Each policy decides for itself: the row records the pair of verdicts.
        report.check(
            &format!(
                "policy verdicts: {outputs} outputs, fee {fee} zatoshis, {unpaid} unpaid actions"
            ),
            true,
            format!("hayaid: {verdict_h:?}; zakurad: {verdict_z:?}"),
        );
        if !took_h && !took_z {
            continue;
        }
        // The node that took it mines it. The block is valid for the other node.
        let (miner, other, name) = if took_h {
            wait_template_txs(h, 1, 20)?;
            (h, z, "hayaid")
        } else {
            (z, h, "zakurad")
        };
        let mined = generate(miner, 1)?;
        let followed = wait_tip(other, &mined, FOLLOW_S).map(|_| ());
        let count = transactions_of_tip(&pair)?;
        report.check(
            &format!("the block of {name} with the transaction of {unpaid} unpaid actions"),
            followed.is_ok() && count == 2,
            format!("{count} transactions in the block ({id}), other node follows: {followed:?}"),
        );
    }
    report.result("state at the end", compare_state(&pair));
    finish(&mut pair, report)
}

/// Scenario d: each node mines alone, then they connect. Both end on the chain with the
/// most work. Depths 1, 3 and 10, with each node as the winner.
pub fn d(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("d", Some(0))?;
    pair.start_both()?;
    let common = generate(pair.zakura_rpc(), 5)?;
    wait_tip(pair.hayai_rpc(), &common, FOLLOW_S)?;
    for depth in [1u64, 3, 10] {
        for hayai_wins in [true, false] {
            let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
            let fork = height(h)?;
            let (blocks_h, blocks_z) = if hayai_wins {
                (depth + 1, depth)
            } else {
                (depth, depth + 1)
            };
            // zakurad runs all the time: a Zakura node that stops can lose its newest
            // blocks. hayaid mines its blocks with a configuration without a peer.
            pair.stop_hayaid("INT")?;
            let tip_z = generate(z, blocks_z)?;
            pair.start_hayaid_alone()?;
            let tip_h = generate(h, blocks_h)?;
            pair.stop_hayaid("INT")?;
            pair.start_hayaid()?;
            pair.wait_connected(60)?;
            let (winner, name) = if hayai_wins {
                (tip_h, "hayaid")
            } else {
                (tip_z, "zakurad")
            };
            let label = format!(
                "fork at {fork}: hayaid {blocks_h} blocks, zakurad {blocks_z} blocks, the chain of {name} wins"
            );
            let both = wait_tip(h, &winner, 120).and_then(|a| {
                let b = wait_tip(z, &winner, 120)?;
                Ok(format!(
                    "both at {} after {} ms",
                    &winner[..12],
                    (a + b).as_millis()
                ))
            });
            if !report.result(&label, both) {
                return Err("the nodes did not converge".into());
            }
            report.result(
                &format!("state after the depth {depth} reorg"),
                compare_state(&pair),
            );
        }
    }
    finish(&mut pair, report)
}

fn verdict(result: &R<Value>) -> String {
    match result {
        Ok(Value::Null) => "accepted".into(),
        Ok(other) => other.to_string(),
        Err(e) => format!("error {e}"),
    }
}

/// The last line of the log of a node that holds `needle`, after the byte `from`.
fn log_reason(pair: &Pair, node: &str, needle: &str, from: u64) -> String {
    let path = pair.setup.dir.join(format!("{node}.log"));
    let Ok(text) = std::fs::read(&path) else {
        return String::new();
    };
    let tail = String::from_utf8_lossy(&text[(from as usize).min(text.len())..]).to_string();
    tail.lines()
        .rev()
        .find(|l| l.contains(needle))
        .map(|l| {
            let at = l.find(needle).unwrap_or(0);
            l[at..].chars().take(230).collect()
        })
        .unwrap_or_default()
}

fn log_len(pair: &Pair, node: &str) -> u64 {
    std::fs::metadata(pair.setup.dir.join(format!("{node}.log"))).map_or(0, |m| m.len())
}

/// Scenario e: blocks that break one rule each, through `submitblock` of both nodes.
pub fn e(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("e", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let tip = generate(z, 112)?;
    wait_tip(h, &tip, FOLLOW_S)?;
    let next = 113u32;
    let branch = branch_at(next);
    let coin = |at: u32| coin_at(&pair, at);
    let spend = |coin: &Coin, out: u64, expiry: u32, sig: &[u8]| {
        transparent_tx(branch, std::slice::from_ref(coin), &[out], expiry, sig)
    };
    let c1 = coin(1)?;
    let good = spend(&c1, c1.value - FEE, 0, &COIN_SCRIPT_SIG);
    let shield = shielding_tx(branch, &coin(2)?, c1.value / 2, c1.value / 2 - FEE, 0);
    let mut bad_shield = shield.to_vec();
    let last = bad_shield.len() - 1;
    bad_shield[last] ^= 1;
    let mut bad_proof = shield.to_vec();
    // A byte in the last 200 bytes before the binding signature is a byte of the proof.
    bad_proof[last - 200] ^= 1;
    let missing = Coin {
        txid: [0x99; 32],
        index: 0,
        value: c1.value,
    };
    let immature = coin(105)?;
    type Change = Box<dyn Fn(&mut Draft, &Value) -> R<()>>;
    // From NU6 the coinbase pays the subsidy and the fees exactly, so a block with one more
    // transaction pays `fee` more: the block then breaks one rule only.
    let with_tx = |tx: Vec<u8>, fee: u64| -> Change {
        Box::new(move |d: &mut Draft, _: &Value| {
            d.txs.push(Bytes::from(tx.clone()));
            d.change_coinbase_value(fee as i64)
        })
    };
    let time = |name: &'static str, delta: i64| -> Change {
        Box::new(move |d: &mut Draft, t: &Value| {
            let base = t[name]
                .as_u64()
                .ok_or_else(|| format!("the template has no {name}"))?;
            d.time = (base as i64 + delta) as u32;
            Ok(())
        })
    };
    let cases: Vec<(&str, Change)> = vec![
        (
            "coinbase one zatoshi too much",
            Box::new(|d, _| d.change_coinbase_value(1)),
        ),
        (
            "coinbase one zatoshi too little (NU6: exact value)",
            Box::new(|d, _| d.change_coinbase_value(-1)),
        ),
        (
            "wrong merkle root",
            Box::new(|d, _| {
                d.merkle_root = Some([0x42; 32]);
                Ok(())
            }),
        ),
        (
            "wrong header commitment",
            Box::new(|d, _| {
                d.commitments = Some([0x42; 32]);
                Ok(())
            }),
        ),
        (
            "time equal to the median-time-past (mintime - 1)",
            time("mintime", -1),
        ),
        ("time one second above maxtime", time("maxtime", 1)),
        (
            "bits easier than the limit (0x207fffff)",
            Box::new(|d, _| {
                d.bits = 0x207f_ffff;
                Ok(())
            }),
        ),
        (
            "bits zero",
            Box::new(|d, _| {
                d.bits = 0;
                Ok(())
            }),
        ),
        (
            "bits negative (0x1f800001)",
            Box::new(|d, _| {
                d.bits = 0x1f80_0001;
                Ok(())
            }),
        ),
        (
            "header version 3",
            Box::new(|d, _| {
                d.version = 3;
                Ok(())
            }),
        ),
        (
            "unknown parent",
            Box::new(|d, _| {
                d.prev = hayai_wire::header::BlockHash([0x33; 32]);
                Ok(())
            }),
        ),
        ("two transactions spend one coin", {
            let (a, b) = (
                good.to_vec(),
                spend(&c1, c1.value - 2 * FEE, 0, &COIN_SCRIPT_SIG).to_vec(),
            );
            Box::new(move |d, _| {
                d.txs = vec![Bytes::from(a.clone()), Bytes::from(b.clone())];
                d.change_coinbase_value(3 * FEE as i64)
            })
        }),
        ("one transaction twice", {
            let a = good.to_vec();
            Box::new(move |d, _| {
                d.txs = vec![Bytes::from(a.clone()), Bytes::from(a.clone())];
                d.change_coinbase_value(2 * FEE as i64)
            })
        }),
        (
            "spend of a coin that does not exist",
            with_tx(
                spend(&missing, c1.value - FEE, 0, &COIN_SCRIPT_SIG).to_vec(),
                FEE,
            ),
        ),
        (
            "script fails (wrong redeem script)",
            with_tx(spend(&c1, c1.value - FEE, 0, &[0x01, 0x52]).to_vec(), FEE),
        ),
        (
            "outputs above inputs",
            with_tx(spend(&c1, c1.value + 1, 0, &COIN_SCRIPT_SIG).to_vec(), 0),
        ),
        (
            "spend of a coinbase after 8 blocks",
            with_tx(
                spend(&immature, immature.value - FEE, 0, &COIN_SCRIPT_SIG).to_vec(),
                FEE,
            ),
        ),
        (
            "expired transaction",
            with_tx(
                spend(&c1, c1.value - FEE, next - 1, &COIN_SCRIPT_SIG).to_vec(),
                FEE,
            ),
        ),
        (
            "Orchard binding signature with one bit changed",
            with_tx(bad_shield, FEE),
        ),
        (
            "Orchard proof with one bit changed",
            with_tx(bad_proof, FEE),
        ),
    ];
    let template = || rpc::call(h, "getblocktemplate", json!([]));
    for (name, change) in &cases {
        let t = template()?;
        let mut draft = Draft::from_template(&t)?;
        change(&mut draft, &t)?;
        let (block, _) = draft.build()?;
        let (from_h, from_z) = (log_len(&pair, "hayaid"), log_len(&pair, "zakurad"));
        let tip = best(z)?;
        // zakurad first: hayaid sends a block with a valid header to its peers before
        // its validation.
        let vz = verdict(&rpc::call(z, "submitblock", json!([block])));
        let vh = verdict(&rpc::call(h, "submitblock", json!([block])));
        let (tip_h, tip_z) = (best(h)?, best(z)?);
        let refused = vz != "accepted" && vh != "accepted" && tip_h == tip && tip_z == tip;
        let why_h = log_reason(&pair, "hayaid", "reason=", from_h);
        let why_z = log_reason(&pair, "zakurad", "error=", from_z);
        report.check(
            &format!("invalid block: {name}"),
            refused,
            format!("hayaid: {vh} ({why_h}); zakurad: {vz} ({why_z})"),
        );
    }
    std::thread::sleep(Duration::from_secs(3));
    let peers = pair.peer_counts()?;
    report.check(
        "the two nodes are still connected",
        peers == (1, 1),
        format!("{peers:?}"),
    );

    // Controls: valid blocks on the same template pass on both nodes.
    let t = template()?;
    let mut draft = Draft::from_template(&t)?;
    draft.txs = vec![good.clone(), shield.clone()];
    draft.change_coinbase_value(2 * FEE as i64)?;
    let (block, hash) = draft.build()?;
    let vz = verdict(&rpc::call(z, "submitblock", json!([block])));
    let followed = wait_tip(h, &hash.to_string(), FOLLOW_S);
    report.check(
        "control: a valid block with the two good transactions, through zakurad",
        vz == "accepted" && followed.is_ok(),
        format!("zakurad: {vz}; hayaid follows: {followed:?}"),
    );
    // A target below the limit is valid without proof of work, and it has more work.
    let t = template()?;
    let mut draft = Draft::from_template(&t)?;
    draft.bits = 0x1f07_ffff;
    let (block, hash) = draft.build()?;
    let vh = verdict(&rpc::call(h, "submitblock", json!([block])));
    let followed = wait_tip(z, &hash.to_string(), FOLLOW_S);
    report.check(
        "control: a valid block with harder bits (0x1f07ffff), through hayaid",
        vh == "accepted" && followed.is_ok(),
        format!("hayaid: {vh}; zakurad follows: {followed:?}"),
    );
    for (miner, other, name) in [(z, h, "zakurad"), (h, z, "hayaid")] {
        let mined = generate(miner, 1)?;
        let followed =
            wait_tip(other, &mined, FOLLOW_S).map(|d| format!("after {} ms", d.as_millis()));
        report.result(
            &format!("after the invalid blocks a block of {name} reaches the other node"),
            followed,
        );
    }
    report.result("state at the end", compare_state(&pair));
    finish(&mut pair, report)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn summary(values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    format!(
        "n {}, median {:.1} ms, p90 {:.1} ms, p99 {:.1} ms, max {:.1} ms",
        values.len(),
        percentile(values, 0.5),
        percentile(values, 0.9),
        percentile(values, 0.99),
        values.last().copied().unwrap_or(f64::NAN)
    )
}

/// Waits until the template of `addr` is on `tip`. Returns the time that it took.
fn wait_template(addr: SocketAddr, tip: &str, seconds: u64) -> R<Duration> {
    let start = Instant::now();
    loop {
        if let Ok(template) = rpc::call(addr, "getblocktemplate", json!([])) {
            if template["previousblockhash"] == tip {
                return Ok(start.elapsed());
            }
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err(format!("{addr} has no template on {tip} after {seconds} s"));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Scenario g: the two nodes mine in turn for `--minutes`, with one transaction for each
/// block. The harness measures from outside, with the same method for both nodes.
pub fn g(args: &Args, report: &mut Report) -> R<()> {
    let mut pair = args.pair("g", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let tip = generate(z, 210)?;
    wait_tip(h, &tip, FOLLOW_S)?;
    let dir = pair.setup.dir.clone();
    let pids = [&pair.hayaid, &pair.zakurad].map(|p| p.as_ref().map_or(0, |p| p.pid()).to_string());
    let mut sampler = Command::new("python3")
        .arg(args.repo.join("scripts/sample_procs.py"))
        .args(&pids)
        .arg("--out")
        .arg(dir.join("procs.csv"))
        .args(["--interval", "1"])
        .args([
            "--metrics",
            &format!("http://{}/metrics", pair.hayai_metrics()),
        ])
        .args([
            "--metrics",
            &format!("http://{}/metrics", pair.zakura_metrics()),
        ])
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("sample_procs.py: {e}"))?;

    let end = Instant::now() + Duration::from_secs(args.minutes * 60);
    // [miner is hayaid][0: follow, 1: template of the follower, 2: template of the miner]
    let mut times: [[Vec<f64>; 3]; 2] = Default::default();
    let mut tx_times: [Vec<f64>; 2] = Default::default();
    let (mut round, mut stalls, mut blocks, mut txs, mut compared) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut failure = None;
    while Instant::now() < end {
        let round_start = Instant::now();
        let hayai_mines = round % 2 == 0;
        let (miner, other) = if hayai_mines { (h, z) } else { (z, h) };
        let coin = coin_at(&pair, round + 1)?;
        let (tx, id) = if round % 5 == 4 {
            let (tx, id, _) = shield_tx(&pair, &coin)?;
            (tx, id)
        } else {
            plain_tx(&pair, &coin, FEE)?
        };
        let (tx_from, tx_to) = if round % 4 < 2 { (h, z) } else { (z, h) };
        let step = send(tx_from, &tx)
            // Zakura announces a transaction one time, to the peers that are ready. For
            // the others hayaid reads the mempool of its peer each 60 s.
            .and_then(|_| wait_mempool(tx_to, &id, FOLLOW_S))
            .and_then(|relay| {
                tx_times[usize::from(tx_from == h)].push(relay.as_secs_f64() * 1e3);
                txs += 1;
                let mined = generate(miner, 1)?;
                let follow = wait_tip(other, &mined, 30)?;
                let template_other = wait_template(other, &mined, 30)?;
                let template_miner = wait_template(miner, &mined, 30)?;
                Ok((follow, follow + template_other, template_miner))
            });
        match step {
            Ok((follow, template_other, template_miner)) => {
                blocks += 1;
                let set = &mut times[usize::from(hayai_mines)];
                set[0].push(follow.as_secs_f64() * 1e3);
                set[1].push(template_other.as_secs_f64() * 1e3);
                set[2].push(template_miner.as_secs_f64() * 1e3);
            }
            Err(e) => {
                stalls += 1;
                report.check(&format!("round {round}"), false, e.clone());
                if stalls >= 5 {
                    failure = Some(format!("5 failed rounds, the last one: {e}"));
                    break;
                }
            }
        }
        round += 1;
        if round % 50 == 0 {
            match compare_state(&pair) {
                Ok(_) => compared += 1,
                Err(e) => {
                    failure = Some(format!("divergence in round {round}: {e}"));
                    break;
                }
            }
        }
        if let Some(rest) = Duration::from_millis(1_500).checked_sub(round_start.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    report.check(
        "no stall and no divergence",
        stalls == 0 && failure.is_none(),
        format!(
            "{} min, {blocks} blocks and {txs} transactions in {round} rounds, {stalls} failed rounds, {compared} state comparisons equal{}",
            args.minutes,
            failure.map(|f| format!("; stopped: {f}")).unwrap_or_default()
        ),
    );
    report.result("state at the end", compare_state(&pair));
    for (index, miner, follower) in [(1, "hayaid", "zakurad"), (0, "zakurad", "hayaid")] {
        let set = &mut times[index];
        report.check(
            &format!("latency: block of {miner}, {follower} has it as its tip"),
            true,
            summary(&mut set[0]),
        );
        report.check(
            &format!("latency: block of {miner}, {follower} serves a template on it"),
            true,
            summary(&mut set[1]),
        );
        report.check(
            &format!("latency: block of {miner}, {miner} serves a template on it (after generate)"),
            true,
            summary(&mut set[2]),
        );
    }
    for (index, from, to) in [(1, "hayaid", "zakurad"), (0, "zakurad", "hayaid")] {
        report.check(
            &format!("latency: transaction sent to {from}, in the mempool of {to}"),
            true,
            summary(&mut tx_times[index]),
        );
    }
    let stop = Command::new("kill")
        .args(["-INT", &sampler.id().to_string()])
        .status()
        .map_err(|e| e.to_string())?;
    if !stop.success() {
        sampler.kill().map_err(|e| e.to_string())?;
    }
    sampler.wait().map_err(|e| e.to_string())?;
    report.result("memory of hayaid", memory(&dir.join("procs.csv"), &pids[0]));
    report.result(
        "memory of zakurad",
        memory(&dir.join("procs.csv"), &pids[1]),
    );
    finish(&mut pair, report)?;
    let joined = Command::new("python3")
        .arg(args.repo.join("scripts/join_traces.py"))
        .arg("--hayai")
        .arg(dir.join("hayai-trace"))
        .arg("--zakura")
        .arg(dir.join("zakura-trace"))
        .arg("--out")
        .arg(dir.join("report.csv"))
        .output()
        .map_err(|e| format!("join_traces.py: {e}"))?;
    std::fs::write(dir.join("join_traces.txt"), &joined.stdout).map_err(|e| e.to_string())?;
    report.check(
        "join_traces.py",
        joined.status.success(),
        format!("summary in {}", dir.join("join_traces.txt").display()),
    );
    Ok(())
}

/// The resident memory of `pid` in `procs.csv` at four times of the run.
fn memory(csv: &std::path::Path, pid: &str) -> R<String> {
    let text = std::fs::read_to_string(csv).map_err(|e| format!("{}: {e}", csv.display()))?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().ok_or("empty procs.csv")?.split(',').collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|c| *c == name)
            .ok_or(format!("no column {name}"))
    };
    let (pid_col, rss_col) = (column("pid")?, column("rss_kb")?);
    let rss: Vec<f64> = lines
        .map(|l| l.split(',').collect::<Vec<_>>())
        .filter(|f| f.get(pid_col) == Some(&pid))
        .filter_map(|f| f.get(rss_col)?.parse::<f64>().ok())
        .collect();
    if rss.len() < 8 {
        return Err(format!("{} samples of pid {pid}", rss.len()));
    }
    let at = |p: f64| rss[((rss.len() - 1) as f64 * p) as usize] / 1024.0;
    Ok(format!(
        "RSS at 0 %, 25 %, 50 %, 75 %, 100 % of the run: {:.0}, {:.0}, {:.0}, {:.0}, {:.0} MB ({} samples)",
        at(0.0),
        at(0.25),
        at(0.5),
        at(0.75),
        at(1.0),
        rss.len()
    ))
}

/// NU7 at height 250, with the hayaid of the Zakura backend: zakurad mines across the
/// activation with transactions, hayaid follows; then hayaid mines blocks with
/// transactions (the coinbase has the miner share of the fees), and zakurad accepts them.
pub fn nu7(args: &Args, report: &mut Report) -> R<()> {
    crate::nodes::NU7.store(250, std::sync::atomic::Ordering::Relaxed);
    let mut pair = args.pair("nu7", Some(0))?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    let mut next_coin = 1u32;
    for (target, miner_is_hayai) in [
        (100u64, false),
        (240, false),
        (248, false),
        (249, false),
        (250, false),
        (251, false),
        (260, false),
        (261, true),
        (262, true),
        (270, true),
        (271, false),
        (280, true),
    ] {
        let (miner, other, name, other_name) = if miner_is_hayai {
            (h, z, "hayaid", "zakurad")
        } else {
            (z, h, "zakurad", "hayaid")
        };
        let tip = height(z)?;
        let mut sent = 0u64;
        if tip >= 240 {
            let (plain, _) = plain_tx(&pair, &coin_at(&pair, next_coin)?, FEE)?;
            let (shield, _, branch) = shield_tx(&pair, &coin_at(&pair, next_coin + 1)?)?;
            next_coin += 2;
            for (kind, tx) in [("transparent", plain), (shielded_pool(branch), shield)] {
                let result = send(miner, &tx).map(|id| format!("txid {id}"));
                if report.result(
                    &format!("{name} takes a {kind} transaction at {tip}"),
                    result,
                ) {
                    sent += 1;
                }
            }
            if miner_is_hayai {
                wait_template_txs(h, sent as usize, 20)?;
            }
        }
        let first = generate(miner, 1)?;
        let followed =
            wait_tip(other, &first, FOLLOW_S).map(|d| format!("after {} ms", d.as_millis()));
        let block = tip + 1;
        if !report.result(
            &format!("{other_name} accepts block {block} of {name}"),
            followed,
        ) {
            return Err(format!("{other_name} did not accept block {block}"));
        }
        if sent > 0 {
            let count = transactions_of_tip(&pair)?;
            report.check(
                &format!("block {block} of {name} has the {sent} transactions"),
                count == sent + 1,
                format!("{count} transactions in the block"),
            );
        }
        if target > block {
            let mined = generate(miner, target - block)?;
            wait_tip(other, &mined, FOLLOW_S)?;
        }
        report.result(&format!("state at {target}"), compare_state(&pair));
    }
    finish(&mut pair, report)
}
