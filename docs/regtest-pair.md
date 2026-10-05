# Regtest pair: one hayaid and one zakurad

Date: 2026-10-05. Scope: how to build the Zakura node, the configuration that the two nodes
share, how to run each scenario, and the result that each scenario must give.
`docs/regtest-pair-findings.md` has the differences that the first runs found.

## Build of zakurad

| Item | Value |
|---|---|
| Source | the Zakura repository, checked out next to this repository |
| Commit | `13779158253cfe315f73eadffb9b4c93c25e82a5` (`zakurad 1.6.0+g13779158253c`) |
| Toolchain | `rust-toolchain.toml` of Zakura: `stable` (rustc 1.98.1 on the build machine) |
| Command | `cargo build --release --locked --bin zakurad`, in the Zakura repository |
| Binary | `target/release/zakurad` of the Zakura repository (310 MB with debug information) |
| Disk | 2.5 GB for the `target` directory of Zakura |

The build needs `protoc`, `clang` and `cmake` on the host. The build changes no file of the
Zakura source.

## Network of the pair

Both processes run on this machine, bound to 127.0.0.1. Each data directory is below
`target/regtest-pair/`.

| Parameter | hayaid | zakurad |
|---|---|---|
| Network | `network = "regtest"` | `network = "Regtest"` |
| Genesis block | `029f11d8…e327` (zcashd Regtest) | the same block |
| Overwinter to Canopy | height 1 | height 1 (default of `new_regtest`) |
| NU5 | height 1 | `NU5 = 1` |
| NU6, NU6.1, NU6.2, NU6.3 | `[regtest] activation_heights = { nu6 = 50, nu6_1 = 100, nu6_2 = 150, nu6_3 = 200 }` | `[network.testnet_parameters.activation_heights]` with the same heights |
| NU7 | not set | not set |
| Proof of work | the Regtest waiver: a solution of 36 bytes, and `bits` that encode a target at or below the limit `0x200f0f0f` | `disable_pow` (set by `new_regtest`): the same two checks |
| Expected `bits` | no rule | no rule |
| Subsidy | 6.25 ZEC, no slow start, halving intervals of 144 blocks before Blossom and 288 blocks after it | the same values (`new_regtest`) |
| Funding streams | none; scenarios e and nu61: `[[regtest.funding_streams]]` (the next section) | none; scenarios e and nu61: `[[network.testnet_parameters.funding_streams]]` with the same values |
| Lockbox disbursement at NU6.1 | `[regtest] lockbox_disbursements`: one entry with the amount 0; scenario nu61: 10 ZEC | `[[network.testnet_parameters.lockbox_disbursements]]` with the same entry |
| Coinbase spend | a transparent spend is valid; maturity 100 blocks | the same rules |
| Time rule | median-time-past + 90 min from height 2 | the same rule |
| Miner address | `t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh` | the same address |
| Seeders | `seeders = []` | `initial_mainnet_peers = []`, `initial_testnet_peers = []`, `cache_dir = false`, `p2p_stack = "legacy"`, `[network.zakura] bootstrap_peers = []` |
| Peer | `peers = ["127.0.0.1:<zakurad port>"]` | none: hayaid dials |

The miner address is the pay-to-script-hash address of the redeem script `OP_TRUE`. The
harness spends each coinbase with the scriptSig `0x01 0x51`, without a key.

### NU6.1 activation block and funding streams

Zakura refuses each block at the NU6.1 activation height when its configuration has no
lockbox disbursement. hayai has the same rule, and hayaid does not start with such a
configuration. Both nodes of the pair have one disbursement to the miner address:

```
# hayaid
[regtest]
lockbox_disbursements = [{ address = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh", amount = 0 }]

# zakurad
[[network.testnet_parameters.lockbox_disbursements]]
address = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh"
amount = 0
```

The block 100 of each node has one more coinbase output of 0 zatoshis, and the other node
accepts it.

Scenarios e and nu61 have funding streams on both nodes, from height 50 to height 199:
12 % of the block subsidy to the deferred pool and 8 % to an address. The heights 50 to
199 are 26 address periods of 6 blocks, and the configuration has two addresses in turn.
The table name is `regtest.funding_streams` for hayaid and
`network.testnet_parameters.funding_streams` for zakurad, with the same keys:

```
[[regtest.funding_streams]]
height_range = { start = 50, end = 200 }
[[regtest.funding_streams.recipients]]
receiver = "Deferred"
numerator = 12
[[regtest.funding_streams.recipients]]
receiver = "MajorGrants"
numerator = 8
addresses = ["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu", "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v", ...]
```

### No connection to a public network

- The configuration of zakurad has no seeder, no initial peer, no peer cache and no
  bootstrap peer. With `p2p_stack = "legacy"` it starts no Zakura (iroh) endpoint. On
  Regtest Zakura keeps only the loopback addresses of its initial peers.
- The configuration of hayaid has `seeders = []` and one peer on 127.0.0.1.
- Each scenario reads the sockets of the two processes with `ss -H -tunap` before it stops
  them, and fails when one address is not a loopback address. The row
  `sockets on loopback only` of the report has the count.

### Credentials of the RPC servers

- Both nodes have the cookie authentication. hayaid writes the file `.cookie` to
  `hayai-data`, and zakurad writes it to `zakura-cookie`.
- The harness reads the file for each request and sends its content as HTTP Basic
  credentials. One client code serves both nodes.

## How to run

```
cargo build --release -p hayaid
cargo test --release -p hayaid --test zakura_pair -- \
    --zakurad /path/to/zakura/target/release/zakurad --scenario all
```

| Argument | Default | Meaning |
|---|---|---|
| `--zakurad PATH` | none | The Zakura binary. Without it the test binary runs no scenario |
| `--scenario LIST` | `all` | `a`, `b`, `c`, `d`, `e`, `g`, `nu61`, `rpc`, with commas. `nu7` is not in `all` |
| `--hayaid PATH` | the `hayaid` of the same build | The hayaid binary, for example the binary of the Zakura backend |
| `--work DIR` | `target/regtest-pair/run-<time>` | Configurations, logs, traces, PID files and the report |
| `--port-base N` | `28100` | hayaid uses N to N+2, zakurad uses N+20 to N+23 |
| `--minutes N` | `30` | Duration of scenario g |
| `--hayaid-log LEVEL` | `info` | Log level of hayaid |

The harness reads no environment variable. Each process has a PID file in the directory of
its scenario. The harness stops only these PIDs, with SIGINT, and removes the PID files.
`scripts/regtest_pair.sh --zakurad PATH` runs scenario a.

Scenario nu7 needs the NU7 rule set, which only the Zakura backend has. Build the node and
the test binary with that backend:

```
CARGO_TARGET_DIR=target/review cargo build --release -p hayaid --no-default-features --features zakura
CARGO_TARGET_DIR=target/review cargo test --release -p hayaid --no-default-features --features zakura \
    --test zakura_pair -- --zakurad /path/to/zakurad --scenario nu7
```

The report is `report-<scenarios>.md` in the work directory: one row for each check. The
exit status is 1 when a check failed.

## Scenarios

Each transaction of the harness pays 20,000 zatoshis, which is above the conventional fee
of each one.

| Scenario | Steps | Expected result |
|---|---|---|
| a | zakurad mines 320 blocks in steps of 20. From height 101 each step has one transparent transaction and one shielding transaction (Orchard to height 199, Ironwood from height 200), sent to zakurad. hayaid follows from the genesis block | After each step: the same tip, the same six value pools, the same Sapling, Orchard and Ironwood roots, the same chain history root in the two templates |
| f (part of a) | hayaid stops with SIGINT before block 101 and with SIGKILL before block 161, and starts again after zakurad mined 20 blocks. zakurad stops and starts at height 240 | hayaid resumes and follows. zakurad resumes, reads the blocks that it lost from hayaid, and has the same state |
| b | hayaid mines blocks 1 to 99, zakurad mines block 100, hayaid mines up to 320. Each step has one transparent and one shielding transaction, sent to hayaid. The last block is a block that the harness builds on `getblocktemplate` of hayaid and submits through `submitblock` of hayaid | zakurad accepts each block. The state is the same after each step |
| c | A transaction that one node takes: it must reach the mempool of the other node, which mines it. Four cases: transparent and Orchard, in the two directions. Then 9 fee policy cases, sent to both nodes | Each transaction is in the mempool of the other node and then in a block that both nodes accept. Both nodes give the same verdict for each policy case (`docs/mempool-policy.md`) |
| d | hayaid stops. zakurad mines its blocks. hayaid starts without a peer, mines its blocks, stops, and starts with zakurad as its peer. Depths 1, 3 and 10, with each node as the one with more blocks | Both nodes end on the chain with the most work, with the same state |
| e | Both nodes have the funding streams. 21 blocks on the template of hayaid at height 113 that break one rule each, through `submitblock` of zakurad and then of hayaid. Then two valid blocks | Both nodes refuse each invalid block and keep their tip. The nodes stay connected. Both accept the valid blocks, and the next block of each node reaches the other one |
| g | From height 210 the nodes mine in turn for `--minutes`, one block each 1.5 s, with one transaction for each block (one in five is an Ironwood shielding). `scripts/sample_procs.py` samples both processes | No failed round, the same state at each 50th round, and a resident memory of hayaid that does not grow |
| nu61 | Part 1, no lockbox disbursement: zakurad mines blocks 1 to 99 and tries block 100; hayaid starts on the same network. Part 2, both nodes have the funding streams and a disbursement of 10 ZEC: hayaid mines blocks 1 to 99, the harness submits a block 100 whose disbursement output has 1 zatoshi too little, hayaid mines block 100, then each node mines 5 blocks | Part 1: zakurad has no block 100, and hayaid refuses the configuration at its start. Part 2: both nodes refuse the invalid block, zakurad accepts block 100 of hayaid, the deferred pool has 51 x 0.75 ZEC minus 10 ZEC, and the state is the same at the heights 99, 100 and 110 |
| nu7 | NU7 at height 250 on both nodes. zakurad mines to 260 with two transactions in the blocks 241 and 249 to 252. Then hayaid mines 261 to 270 and 272 to 280, zakurad mines 271; the blocks 261 to 263, 271 and 272 have two transactions | Each node accepts each block of the other one. The same state at each compared height |
| rpc | Both nodes have the funding streams. zakurad mines 205 blocks, with a transparent and a shielding transaction in the blocks 120 and 204. hayaid mines block 206. One transaction is in both mempools. The harness then calls each method of the table below on both nodes, and `stop` last | Each answer is the same, field by field, but for the fields of the list below. Each process ends with the exit status 0 after `stop` |

Invalid blocks of scenario e: coinbase with 1 zatoshi too much and too little; funding
stream output with 1 zatoshi too little; wrong merkle
root; wrong header commitment; time at the median-time-past; time above the median-time-past
plus 90 min; `bits` above the limit, zero and negative; header version 3; unknown parent;
two spends of one coin; one transaction twice; spend of a missing coin; a script that fails;
outputs above inputs; spend of a coinbase after 8 blocks; an expired transaction; an Orchard
binding signature and an Orchard proof with one bit changed.

## Methods of scenario rpc

The harness compares the result of a call, or the code of its error. The messages of the
errors are not compared.

| Method | Parameters |
|---|---|
| `getinfo`, `getmininginfo`, `getdifficulty`, `getnetworkinfo`, `getpeerinfo`, `getmempoolinfo`, `getchaintips`, `getbestblockheightandhash`, `getdeprecationinfo`, `getblockcount`, `getbestblockhash`, `getrawmempool`, `ping` | none |
| `getblocksubsidy` | none, and the heights 1, 49, 50, 100, 150, 199, 200, 207, 288, 289 and 100,000. The pair has the funding streams from height 50 to 199 |
| `getnetworksolps`, `getnetworkhashps` | none, `[10]`, `[0]`, `[-1]`, `[10, 50]`, `[500, 100]`, `[5, 0]`, `[5, -1]`, `[5, 100000]` |
| `getblockheader` (default, verbose, hex), `getblock` (default, 0, 1) | the heights 0, 1, 50, 100, 120, 204, 206 and 207, the hash of the tip, and a hash of no block |
| `getblock` | verbosity 2 and 3 |
| `validateaddress`, `z_validateaddress` | 4 transparent addresses, Sapling and Unified addresses of Mainnet, Testnet and Regtest, a Sapling address with a changed character, text that is no address, the empty text, no parameter |
| `addnode` | an address two times, the command `remove`, a host name, one parameter |
| an unknown method | none |
| `generate` without the credentials | `[1]`. hayaid answers with the HTTP status 401, zakurad closes the connection without an answer, and no node has a new block |
| `stop` | none |

Fields that can differ (`MAY_DIFFER` in `crates/hayaid/tests/zakura_pair/rpc_compare.rs`).
A difference in another field fails the check:

| Method | Field | Reason |
|---|---|---|
| `getinfo`, `getnetworkinfo` | `version`, `build`, `subversion` | The version and the user agent of each program |
| `getinfo`, `getnetworkinfo` | `protocolversion` | 170,160 in a hayaid without the NU7 rule set, 170,190 in zakurad |
| `getinfo` | `errors`, `errorstimestamp` | hayaid keeps no record of its log messages |
| `getnetworkinfo` | `localservices` | zakurad prints `NODE_NETWORK` only. hayaid prints the service bits of its `version` message. The pair runs hayaid without the compact relay, so the two values are equal in the run |
| `getpeerinfo` | `addr`, `inbound`, `subver`, `version`, `pingtime`, `pingwait` | Each node lists the other one, and each node measures its own times |
| `getblockheader`, `getblock` | each field, for the height 0 | hayaid has the hash of the genesis block and does not store the block: error -5, and error -8 for verbosity 0 |
| `getblock` | each field, for verbosity 2 | hayaid has no verbosity 2: error -8 |
| `stop` | the result | `hayaid server stopping` and `Zakura server stopping` |

Result of the run of 2026-10-05 (hayaid of the default backend, zakurad `1.6.0+g13779158253c`):
143 checks, no difference outside the list. In the list, these fields differed: `version`,
`build`, `subversion`, `protocolversion`, `errors` and `errorstimestamp` of `getinfo`;
`version`, `subversion` and `protocolversion` of `getnetworkinfo`; `addr`, `inbound`,
`subver` and `version` of `getpeerinfo`; the genesis block; verbosity 2; the result of
`stop`.

## Measurements of scenario g

The harness measures both nodes from outside with one method, on RPC polls of 1 to 2 ms:

- block of node A, node B has it as its tip: from the answer of `generate` on A to the first
  `getbestblockhash` of B with the new hash;
- node B serves a template on it: to the first `getblocktemplate` of B with the new
  `previousblockhash`;
- transaction sent to A, in the mempool of B: from the answer of `sendrawtransaction` to the
  first `getrawmempool` of B with the transaction.

`scripts/join_traces.py` joins the trace rows of hayaid. A Zakura node with the legacy stack
writes `legacy_sync.jsonl` and `legacy_peer_request.jsonl` only: it has no `commit_start`
and no `commit_finish` row, so the join has no Zakura column for this pair.

The values are a Regtest loopback measurement with blocks of 1 to 3 transactions. They are
not a benchmark result.
