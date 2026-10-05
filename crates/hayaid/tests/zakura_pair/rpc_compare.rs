//! Scenario rpc: each method that pools and operators use, on hayaid and on zakurad with
//! the same chain. The answers are compared field by field. [`MAY_DIFFER`] has the fields
//! that can differ, with the reason of each one.

use std::net::SocketAddr;

use serde_json::{json, Value};

use crate::nodes::{best, generate, height, wait_tip};
use crate::scenarios::{coin_at, plain_tx, send, shield_tx, wait_mempool, FEE, FOLLOW_S};
use crate::{rpc, Args, Report};

type R<T> = Result<T, String>;

/// The fields that can differ, as `(method, path, reason)`. A path is the list of the
/// keys from the result, with `/` between them. Two more differences have no entry: each
/// answer of `getblock` and `getblockheader` for the genesis block, and `getblock` with
/// verbosity 2.
const MAY_DIFFER: [(&str, &str, &str); 17] = [
    ("getinfo", "version", "the version of each program"),
    ("getinfo", "build", "the version of each program"),
    ("getinfo", "subversion", "the user agent of each program"),
    (
        "getinfo",
        "protocolversion",
        "170,160 in a hayaid without the NU7 rule set, 170,190 in zakurad",
    ),
    (
        "getinfo",
        "errors",
        "hayaid keeps no record of its log messages",
    ),
    (
        "getinfo",
        "errorstimestamp",
        "hayaid keeps no record of its log messages",
    ),
    ("getnetworkinfo", "version", "the version of each program"),
    (
        "getnetworkinfo",
        "subversion",
        "the user agent of each program",
    ),
    (
        "getnetworkinfo",
        "protocolversion",
        "170,160 in a hayaid without the NU7 rule set, 170,190 in zakurad",
    ),
    (
        "getnetworkinfo",
        "localservices",
        "zakurad prints NODE_NETWORK only; hayaid prints the service bits of its version message",
    ),
    (
        "getpeerinfo",
        "0/addr",
        "each node lists the address of the other one",
    ),
    (
        "getpeerinfo",
        "0/inbound",
        "hayaid dials zakurad: the connection is outbound for hayaid and inbound for zakurad",
    ),
    (
        "getpeerinfo",
        "0/subver",
        "each node lists the user agent of the other one",
    ),
    (
        "getpeerinfo",
        "0/version",
        "each node lists the protocol version of the other one",
    ),
    (
        "getpeerinfo",
        "0/pingtime",
        "a time measurement of each node",
    ),
    (
        "getpeerinfo",
        "0/pingwait",
        "a time measurement of each node",
    ),
    ("stop", "", "the name of each program"),
];

/// The differences of `hayai` and `zakura` at `path`: one line for each leaf and for each
/// key that one answer does not have.
fn differences(path: &str, hayai: &Value, zakura: &Value, out: &mut Vec<(String, String)>) {
    let join = |key: &str| match path {
        "" => key.to_string(),
        _ => format!("{path}/{key}"),
    };
    match (hayai, zakura) {
        (Value::Object(h), Value::Object(z)) => {
            let mut keys: Vec<&String> = h.keys().chain(z.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                match (h.get(key), z.get(key)) {
                    (Some(a), Some(b)) => differences(&join(key), a, b, out),
                    (Some(a), None) => out.push((join(key), format!("hayaid {a}, zakurad none"))),
                    (None, Some(b)) => out.push((join(key), format!("hayaid none, zakurad {b}"))),
                    (None, None) => unreachable!("the key is a key of one answer"),
                }
            }
        }
        (Value::Array(h), Value::Array(z)) if h.len() == z.len() => {
            for (i, (a, b)) in h.iter().zip(z).enumerate() {
                differences(&join(&i.to_string()), a, b, out);
            }
        }
        (a, b) if a == b => {}
        (a, b) => out.push((path.to_string(), format!("hayaid {a}, zakurad {b}"))),
    }
}

/// The answer of a node as one value: the result, or the code of the error.
fn answer(addr: SocketAddr, method: &str, params: &Value) -> Value {
    match rpc::call(addr, method, params.clone()) {
        Ok(result) => json!({ "result": result }),
        Err(error) => {
            let code = serde_json::from_str::<Value>(&error)
                .map(|e| e["code"].clone())
                .unwrap_or(Value::Null);
            json!({ "error": { "code": code } })
        }
    }
}

/// Calls `method` on both nodes and records the differences. A difference that is not in
/// [`MAY_DIFFER`] fails the check.
fn compare(report: &mut Report, h: SocketAddr, z: SocketAddr, method: &str, params: Value) {
    let (hayai, zakura) = (answer(h, method, &params), answer(z, method, &params));
    let mut found = Vec::new();
    differences("", &hayai, &zakura, &mut found);
    // hayaid has the hash of the genesis block and not the block.
    let genesis = matches!(method, "getblock" | "getblockheader") && params[0] == "0";
    let verbosity_2 = method == "getblock" && params[1] == 2;
    let mut unexpected = 0;
    let lines: Vec<String> = found
        .iter()
        .map(|(path, what)| {
            let path = path.strip_prefix("result/").unwrap_or(match path.as_str() {
                "result" => "",
                other => other,
            });
            let reason = match (genesis, verbosity_2) {
                (true, _) => Some("hayaid does not store the genesis block"),
                (_, true) => Some("hayaid has no verbosity 2: no transaction object"),
                _ => MAY_DIFFER
                    .iter()
                    .find(|(m, p, _)| *m == method && *p == path)
                    .map(|(_, _, reason)| *reason),
            };
            match reason {
                Some(reason) => format!("`{path}`: {what} (expected: {reason})"),
                None => {
                    unexpected += 1;
                    format!("`{path}`: {what} (NOT EXPECTED)")
                }
            }
        })
        .collect();
    let detail = match lines.is_empty() {
        true => "equal".to_string(),
        false => lines.join("; "),
    };
    report.check(&format!("{method} {params}"), unexpected == 0, detail);
}

/// Scenario rpc.
pub fn run(args: &Args, report: &mut Report) -> R<()> {
    // The funding streams of the pair: `getblocksubsidy` has its stream fields.
    let mut pair = args.pair_with("rpc", Some(0), true)?;
    pair.start_both()?;
    let (h, z) = (pair.hayai_rpc(), pair.zakura_rpc());
    // zakurad mines 205 blocks, in steps of at most 20 as in scenario a: each upgrade of
    // the pair is active. Blocks 120 and 204 have a transparent and a shielding
    // transaction (Orchard, then Ironwood).
    for (target, coin) in [
        (119u64, None),
        (120, Some(1u32)),
        (203, None),
        (204, Some(3)),
        (205, None),
    ] {
        if let Some(coin) = coin {
            let (plain, _) = plain_tx(&pair, &coin_at(&pair, coin)?, FEE)?;
            let (shield, _, _) = shield_tx(&pair, &coin_at(&pair, coin + 1)?)?;
            for tx in [plain, shield] {
                send(z, &tx)?;
            }
        }
        loop {
            let at = height(z)?;
            if at >= target {
                break;
            }
            let mined = generate(z, (target - at).min(20))?;
            wait_tip(h, &mined, FOLLOW_S)?;
        }
    }
    // hayaid mines one block, so each node has a block of the other one.
    let mined = generate(h, 1)?;
    wait_tip(z, &mined, FOLLOW_S)?;
    // One transaction in both mempools.
    let (plain, id) = plain_tx(&pair, &coin_at(&pair, 5)?, FEE)?;
    send(z, &plain)?;
    wait_mempool(h, &id, FOLLOW_S)?;
    let tip = best(z)?;
    report.check(
        "common chain",
        best(h)? == tip,
        format!("height 206, tip {tip}"),
    );

    let mut check = |method: &str, params: Value| compare(report, h, z, method, params);
    check("getinfo", json!([]));
    check("getmininginfo", json!([]));
    check("getdifficulty", json!([]));
    check("getnetworkinfo", json!([]));
    check("getpeerinfo", json!([]));
    check("getmempoolinfo", json!([]));
    check("getchaintips", json!([]));
    check("getbestblockheightandhash", json!([]));
    check("getdeprecationinfo", json!([]));
    check("getblockcount", json!([]));
    check("getbestblockhash", json!([]));
    check("getrawmempool", json!([]));
    for params in [
        json!([]),
        json!([1]),
        json!([49]),
        json!([50]),
        json!([100]),
        json!([150]),
        json!([199]),
        json!([200]),
        json!([207]),
        json!([288]),
        json!([289]),
        json!([100_000]),
    ] {
        check("getblocksubsidy", params);
    }
    for params in [
        json!([]),
        json!([10]),
        json!([0]),
        json!([-1]),
        json!([10, 50]),
        json!([500, 100]),
        json!([5, 0]),
        json!([5, -1]),
        json!([5, 100_000]),
    ] {
        check("getnetworksolps", params.clone());
        check("getnetworkhashps", params);
    }
    for block in [
        json!("0"),
        json!("1"),
        json!("50"),
        json!("100"),
        json!("120"),
        json!("204"),
        json!("206"),
        json!(tip),
        json!("207"),
        json!("00".repeat(32)),
    ] {
        check("getblockheader", json!([block]));
        check("getblockheader", json!([block, true]));
        check("getblockheader", json!([block, false]));
        check("getblock", json!([block]));
        check("getblock", json!([block, 0]));
        check("getblock", json!([block, 1]));
    }
    for address in [
        "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh",
        "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV",
        "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs",
        "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd",
        // The default Sapling address of the seed `[7; 32]`, for Mainnet, Testnet and
        // Regtest. The last one has one changed character.
        "zs1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupqu4uesc",
        "ztestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupq5zw6hv",
        "zregtestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupqtx8hxt",
        // Unified Addresses with one Orchard receiver, of Mainnet, Testnet and Regtest.
        "utest1j6uf5arygvvnypvh7n9pgcmt70xapzx70t3nemk2yy6646ekn2d296wyc355pvjtwnftcaukuqv687wnecvs47h9w82z4v498c58d6w8",
        "u1u4zdvtcrjt4cnffw3shrx440754g0mhr4a7fenck686tfr6rdqy9wu5v39ydpgm4ut37qnlh9kpw9fsp8wcwyu2y2r5stjhj0qwfnmrq",
        "uregtest14re0eqzj6guwjy8fc2hvdjswh5pqvnty480846uervx9jdqn6cmq784ene8nu9spkl22mghk8nc6d6jsy3j452vdz90nak8yzyqj4j9q",
        "zregtestsapling1f0x0t0dgnpyt0au5wl06g7ylnzwanhs5pegkpuvnnly6l4aeactlwdp2479ne6h8zvupqtx8hxq",
        "x",
        "",
    ] {
        check("validateaddress", json!([address]));
        check("z_validateaddress", json!([address]));
    }
    check("validateaddress", json!([]));
    check("ping", json!([]));
    check("addnode", json!(["127.0.0.1:1", "add"]));
    check("addnode", json!(["127.0.0.1:1", "add"]));
    check("addnode", json!(["127.0.0.1:1", "remove"]));
    check("addnode", json!(["node.example:1", "add"]));
    check("addnode", json!(["127.0.0.1:1"]));
    check("getblock", json!(["206", 2]));
    check("getblock", json!(["206", 3]));
    check("nosuchmethod", json!([]));

    // Both nodes have the cookie authentication. Without the credentials hayaid answers
    // 401, as zcashd. zakurad closes the connection without an answer: its error 401
    // does not reach the client (`zakura-rpc/src/server/http_request_compatibility.rs`,
    // the TODO in `call`). No node runs the method.
    let h_status = rpc::status_without_cookie(h, "generate", json!([1]))?;
    let z_status = rpc::status_without_cookie(z, "generate", json!([1]))?;
    report.check(
        "no method runs without the credentials",
        h_status == Some(401)
            && matches!(z_status, None | Some(401))
            && best(h)? == tip
            && best(z)? == tip,
        format!("hayaid {h_status:?}, zakurad {z_status:?}, tip {tip}"),
    );

    let sockets = pair.check_loopback_only().map(|lines| {
        format!(
            "{} sockets of the two processes, each on loopback",
            lines.len()
        )
    });
    report.result("sockets on loopback only", sockets);
    // `stop` ends each process with the exit status 0.
    compare(report, h, z, "stop", json!([]));
    for (name, proc) in [
        ("hayaid", pair.hayaid.take()),
        ("zakurad", pair.zakurad.take()),
    ] {
        let ended = match proc {
            Some(proc) => proc.wait_exit(90),
            None => Err("the process does not run".into()),
        };
        report.check(
            &format!("{name} stops after `stop`"),
            ended == Ok(true),
            match ended {
                Ok(true) => "exit status 0".to_string(),
                Ok(false) => "exit status not 0".to_string(),
                Err(e) => e,
            },
        );
    }
    Ok(())
}
