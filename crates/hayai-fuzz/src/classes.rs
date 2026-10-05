//! The mutation classes: each class makes the recipe of a case from a random source.

use hayai_consensus::{Network, Upgrade};

use crate::context::{Coin, Net, MAX_COIN, MAX_MONEY};
use crate::model::{Tx, TxIn, TxOut};
use crate::mutate::{HeaderField, Op, RawOp, Recipe, Spend};
use crate::rng::Rng;
use crate::script::{self, OP_1};
use crate::seeds::{self, height_script, FixtureId, Seed, SeedSpec};

/// The time of the header of every seed.
const SEED_TIME: u32 = 1_700_000_000;
/// The block size limit in bytes.
const MAX_BLOCK_BYTES: usize = 2_000_000;
/// The limit of signature operations in a block.
const MAX_BLOCK_SIGOPS: usize = 20_000;

/// A mutation class of block cases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    /// Header fields: version, time, bits, nonce, solution, parent.
    Header,
    /// The transaction list: order, copies, removals, count, truncation, bytes after the
    /// block.
    Structure,
    /// The coinbase: output values and scripts, required outputs, height, lock time,
    /// expiry, version.
    Coinbase,
    /// A coinbase-only block at a height near a rule change, on Mainnet and Testnet.
    Height,
    /// Fields of transactions at rule limits: lock time, sequence, expiry, version,
    /// branch, values.
    TxFields,
    /// Transparent scripts.
    Script,
    /// Transparent inputs: missing coins, double spends, order in the block, coinbase
    /// maturity.
    Spend,
    /// Orchard and Ironwood sections: flags, value balance, anchor, nullifiers, proof and
    /// signature bits.
    Shielded,
    /// Signature operation count and block size at their limits.
    Limits,
    /// The merkle root and the commitments field of the header.
    Commitments,
    /// Byte mutations of the wire encoding.
    Bytes,
}

impl Class {
    pub const ALL: [Class; 11] = [
        Class::Header,
        Class::Structure,
        Class::Coinbase,
        Class::Height,
        Class::TxFields,
        Class::Script,
        Class::Spend,
        Class::Shielded,
        Class::Limits,
        Class::Commitments,
        Class::Bytes,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Class::Header => "header",
            Class::Structure => "structure",
            Class::Coinbase => "coinbase",
            Class::Height => "height",
            Class::TxFields => "txfields",
            Class::Script => "script",
            Class::Spend => "spend",
            Class::Shielded => "shielded",
            Class::Limits => "limits",
            Class::Commitments => "commitments",
            Class::Bytes => "bytes",
        }
    }

    pub fn from_name(name: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|class| class.name() == name)
    }

    /// The recipe of the case with the random source `rng`.
    pub fn recipe(self, rng: &mut Rng) -> Recipe {
        match self {
            Class::Header => header(rng),
            Class::Structure => structure(rng),
            Class::Coinbase => coinbase(rng),
            Class::Height => height(rng),
            Class::TxFields => tx_fields(rng),
            Class::Script => scripts(rng),
            Class::Spend => spend(rng),
            Class::Shielded => shielded(rng),
            Class::Limits => limits(rng),
            Class::Commitments => commitments(rng),
            Class::Bytes => bytes(rng),
        }
    }
}

fn recipe(seed: SeedSpec, ops: Vec<Op>) -> Recipe {
    Recipe {
        seed,
        ops,
        raw: Vec::new(),
        fix_merkle: true,
        fix_commitments: true,
    }
}

fn any_fixture(rng: &mut Rng) -> SeedSpec {
    SeedSpec::Fixture(*rng.pick(&FixtureId::ALL))
}

/// A fixture whose coinbase has no shielded output, so its outputs can change.
fn plain_coinbase_fixture(rng: &mut Rng) -> SeedSpec {
    SeedSpec::Fixture(*rng.pick(&[
        FixtureId::Transparent,
        FixtureId::Mixed,
        FixtureId::Orchard,
        FixtureId::Nu63,
    ]))
}

fn shielded_fixture(rng: &mut Rng) -> SeedSpec {
    SeedSpec::Fixture(*rng.pick(&[
        FixtureId::Mixed,
        FixtureId::Orchard,
        FixtureId::Nu63,
        FixtureId::Nu63ShieldedCoinbase,
    ]))
}

fn seed_of(spec: SeedSpec) -> std::sync::Arc<Seed> {
    seeds::seed(spec).expect("the classes choose heights that have a seed")
}

/// A height above the mandatory checkpoint at which hayai has a rule set: near a rule
/// change, or any height.
fn synthetic_height(rng: &mut Rng, net: Net) -> u32 {
    let network = crate::hayai_side::network(net);
    let first = network.mandatory_checkpoint_height() + 1;
    // The cases stay below the block before NU7. That block has the NSM seed rule on the
    // total of the chain value pools, and the pools of a context are not those of the
    // chain. The oracle has no rule for the pools.
    let end = match network.activation_height(Upgrade::Nu7) {
        Some(nu7) => nu7 - 2,
        None => 4_600_000,
    };
    let mut anchors: Vec<u32> = Upgrade::ALL
        .into_iter()
        .filter_map(|upgrade| network.activation_height(upgrade))
        .collect();
    // The halving heights and the limits of the funding stream periods.
    match network {
        Network::Mainnet => anchors.extend([
            1_046_400, 2_726_400, 3_146_400, 4_406_400, 4_476_000, 3_363_426, 3_428_143,
        ]),
        _ => anchors.extend([
            1_028_500, 1_116_000, 2_796_000, 2_976_000, 3_396_000, 3_536_500, 4_048_500, 4_476_000,
        ]),
    }
    let height = match rng.below(10) {
        0..=5 => {
            let anchor = *rng.pick(&anchors);
            let delta = [0i64, 0, 1, -1, 2, -2, 99, 100, 101][rng.below(9)];
            (i64::from(anchor) + delta) as u32
        }
        // A limit of a funding stream address period: a multiple of the period length
        // from an anchor.
        6 | 7 => {
            let anchor = *rng.pick(&anchors);
            let periods = rng.below(40) as i64 - 8;
            let period = *rng.pick(&[35_000i64, 42_000, 21_000, 840_000, 1_680_000]);
            let delta = [0i64, 1, -1][rng.below(3)];
            (i64::from(anchor) + periods * period + delta).clamp(0, i64::from(u32::MAX)) as u32
        }
        _ => first + rng.below((end - first) as usize) as u32,
    };
    height.clamp(first, end)
}

fn synthetic(rng: &mut Rng) -> SeedSpec {
    let network = if rng.chance(1, 2) {
        Net::Mainnet
    } else {
        Net::Testnet
    };
    SeedSpec::Synthetic {
        network,
        height: synthetic_height(rng, network),
    }
}

fn header(rng: &mut Rng) -> Recipe {
    let seed = any_fixture(rng);
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(2) {
        ops.push(match rng.below(8) {
            0 | 1 => {
                Op::HeaderVersion(rng.near_u32(&[0, 3, 4, 5, 0x7fff_ffff, 0x8000_0000, u32::MAX]))
            }
            2 | 3 => Op::HeaderTime(rng.near_u32(&[
                0,
                crate::hayai_side::PARENT_TIME,
                SEED_TIME,
                crate::hayai_side::PARENT_TIME + 5_400,
                0x7fff_ffff,
                0x8000_0000,
                u32::MAX,
            ])),
            4 => Op::HeaderBits(rng.near_u32(&[
                0,
                0x1f07_ffff,
                0x2007_ffff,
                0x1d00_ffff,
                0x0080_0000,
                0x0100_0000,
                0xff00_0001,
                u32::MAX,
            ])),
            5 => Op::HeaderFlip {
                field: *rng.pick(&[HeaderField::Nonce, HeaderField::Prev]),
                bit: rng.next_u64() as u8,
            },
            6 => Op::SolutionResize(rng.near_u32(&[0, 1, 36, 400, 1343, 1344, 1345, 2_000])),
            _ => Op::SolutionFlip(rng.next_u32()),
        });
    }
    recipe(seed, ops)
}

fn structure(rng: &mut Rng) -> Recipe {
    let seed = any_fixture(rng);
    let txs = seed_of(seed).block.txs.len();
    let mut out = recipe(seed, Vec::new());
    match rng.below(12) {
        0 | 1 => out.ops.push(Op::SwapTxs(rng.below(txs), rng.below(txs))),
        2 | 3 => out.ops.push(Op::DuplicateTx {
            tx: rng.below(txs),
            at: rng.below(txs + 1),
        }),
        4 | 5 => out.ops.push(Op::RemoveTx(rng.below(txs))),
        6 => {
            // The copies keep the merkle root, so the header is not changed.
            out.ops.push(Op::RemoveTx(1 + rng.below(txs - 1)));
            out.ops.push(Op::MerkleDuplicateTail);
            out.fix_merkle = false;
        }
        7 | 8 => out.ops.push(Op::StatedCount(rng.near_u64(&[
            0,
            1,
            txs as u64,
            0xfc,
            0xfd,
            0xffff,
            0x1_0000,
            0xffff_ffff,
            0x1_0000_0000,
            u64::MAX,
        ]))),
        9 => out.ops.push(Op::CountWidth(*rng.pick(&[3, 5, 9]))),
        10 => {
            let len = seed_of(seed).block.bytes().len() as u32;
            out.raw
                .push(RawOp::Truncate(len - 1 - rng.below(200) as u32));
        }
        _ => out.raw.push(RawOp::Append(rng.some_bytes(1, 40))),
    }
    // A block that only removes all but the coinbase is a valid block when the coinbase
    // claims no fee: most cases keep the merkle root of the new body.
    if rng.chance(1, 5) {
        out.fix_merkle = false;
    }
    out
}

/// One operation on the coinbase of a block at `height` with `outputs` outputs.
fn coinbase_op(rng: &mut Rng, height: u32, outputs: usize) -> Op {
    let output = rng.below(outputs.max(1));
    match rng.below(22) {
        0..=2 => Op::OutAdd {
            tx: 0,
            output,
            delta: *rng.pick(&[1i64, -1, 2, -2, 1_000, -1_000, 100_000_000]),
        },
        3 => Op::OutValue {
            tx: 0,
            output,
            value: rng.near_u64(&[0, MAX_MONEY, i64::MAX as u64, u64::MAX]),
        },
        4 => Op::OutRemove { tx: 0, output },
        5 => Op::OutDuplicate { tx: 0, output },
        6 => Op::OutSwap {
            tx: 0,
            a: output,
            b: rng.below(outputs.max(1)),
        },
        7 => Op::OutScriptFlip {
            tx: 0,
            output,
            bit: rng.next_u32(),
        },
        8 => Op::OutPush {
            tx: 0,
            output: TxOut {
                value: *rng.pick(&[0, 1, 1_000]),
                script: if rng.chance(1, 2) {
                    vec![OP_1]
                } else {
                    rng.some_bytes(0, 30)
                },
            },
        },
        9..=11 => {
            // The height in the input script, in other heights and other encodings.
            let stated = rng.near_u32(&[height]);
            let number = script::number(i64::from(stated));
            let mut script = match rng.below(6) {
                0 => height_script(stated),
                // A push with an OP_PUSHDATA1 opcode.
                1 => [vec![0x4c, number.len() as u8], number].concat(),
                // A number with a zero byte that its shortest form does not have.
                2 => script::push(&[number, vec![0]].concat()),
                // A 4-byte number.
                3 => script::push(&stated.to_le_bytes()),
                4 => Vec::new(),
                _ => height_script(height),
            };
            // The length limits of the script: 2 and 100 bytes.
            match rng.below(6) {
                0 => script.resize(*rng.pick(&[0usize, 1, 2]), 0),
                1 => script.resize(*rng.pick(&[99usize, 100, 101, 102]), 0x51),
                _ => {}
            }
            Op::InScript {
                tx: 0,
                input: 0,
                script,
            }
        }
        12 => Op::Sequence {
            tx: 0,
            input: 0,
            value: rng.near_u32(&[0, u32::MAX]),
        },
        13 | 14 => Op::LockTime {
            tx: 0,
            value: rng.near_u32(&[
                0,
                height,
                499_999_999,
                500_000_000,
                SEED_TIME,
                crate::hayai_side::PARENT_TIME,
                u32::MAX,
            ]),
        },
        15 | 16 => Op::Expiry {
            tx: 0,
            value: rng.near_u32(&[0, height, 499_999_999, 500_000_000, u32::MAX]),
        },
        17 => Op::InPush {
            tx: 0,
            input: TxIn {
                prev_hash: if rng.chance(1, 2) { [0; 32] } else { [7; 32] },
                prev_index: *rng.pick(&[u32::MAX, 0]),
                script: height_script(height),
                sequence: u32::MAX,
            },
        },
        18 => Op::InPrev {
            tx: 0,
            input: 0,
            hash: if rng.chance(1, 2) { [0; 32] } else { [1; 32] },
            index: rng.near_u32(&[0, u32::MAX]),
        },
        19 => Op::InRemove { tx: 0, input: 0 },
        20 => version_op(rng, 0),
        _ => Op::OutScript {
            tx: 0,
            output,
            script: rng.some_bytes(0, 40),
        },
    }
}

/// A change of the version, the version group or the consensus branch of a transaction.
fn version_op(rng: &mut Rng, tx: usize) -> Op {
    const BRANCHES: [u32; 9] = [
        0x5ba8_1b19,
        0x76b8_09bb,
        0x2bb4_0e60,
        0xf5b9_230b,
        0xe9ff_75a6,
        0xc2d6_d0b4,
        0xc8e7_1055,
        0x4dec_4df0,
        0x7719_0ad9,
    ];
    match rng.below(3) {
        0 => Op::TxHeader {
            tx,
            value: rng.near_u32(&[
                0x8000_0004,
                0x8000_0005,
                0x8000_0006,
                5,
                0x8000_0003,
                0x8000_0007,
            ]),
        },
        1 => Op::TxGroup {
            tx,
            value: *rng.pick(&[0x892F_2085, 0x26A7_270A, 0xD884_B698, 0x03C4_8270, 0]),
        },
        _ => Op::TxBranch {
            tx,
            value: if rng.chance(7, 8) {
                *rng.pick(&BRANCHES)
            } else {
                rng.next_u32()
            },
        },
    }
}

fn coinbase(rng: &mut Rng) -> Recipe {
    let seed = match rng.below(4) {
        0 => synthetic(rng),
        1 => any_fixture(rng),
        _ => plain_coinbase_fixture(rng),
    };
    let base = seed_of(seed);
    let outputs = match &base.block.txs[0] {
        Tx::Parts(parts) => parts.vout.len(),
        Tx::Raw(_) => 1,
    };
    let ops = (0..1 + rng.below(2))
        .map(|_| coinbase_op(rng, base.ctx.height, outputs))
        .collect();
    recipe(seed, ops)
}

fn height(rng: &mut Rng) -> Recipe {
    let seed = synthetic(rng);
    let base = seed_of(seed);
    let mut ops = Vec::new();
    if rng.chance(1, 2) {
        let outputs = match &base.block.txs[0] {
            Tx::Parts(parts) => parts.vout.len(),
            Tx::Raw(_) => 1,
        };
        ops.push(match rng.below(4) {
            0 => Op::OutAdd {
                tx: 0,
                output: rng.below(outputs),
                delta: *rng.pick(&[1, -1]),
            },
            1 => Op::OutRemove {
                tx: 0,
                output: rng.below(outputs),
            },
            2 => Op::ContextHeight(rng.near_u32(&[base.ctx.height])),
            _ => coinbase_op(rng, base.ctx.height, outputs),
        });
    }
    recipe(seed, ops)
}

/// A spend of a new coin with the locking script `OP_1`: valid without a signature.
fn open_spend(rng: &mut Rng, height: u32) -> Spend {
    let value = *rng.pick(&[10_000u64, 1_000_000, 100_000_000]);
    Spend {
        coin: Coin {
            value,
            script: vec![OP_1],
            height: height - 1_000,
            coinbase: false,
        },
        in_context: true,
        parent: None,
        script_sig: Vec::new(),
        sequence: u32::MAX,
        outputs: vec![TxOut {
            value: value - 1_000,
            script: vec![OP_1],
        }],
        lock_time: 0,
        expiry: 0,
        version: 0,
        branch: None,
        claim_fee: true,
        at: 1 + rng.below(6),
    }
}

/// A seed whose coinbase can claim a fee, and its height.
fn spend_seed(rng: &mut Rng) -> (SeedSpec, u32) {
    let seed = if rng.chance(1, 3) {
        synthetic(rng)
    } else {
        plain_coinbase_fixture(rng)
    };
    (seed, seed_of(seed).ctx.height)
}

fn tx_fields(rng: &mut Rng) -> Recipe {
    let (seed, height) = spend_seed(rng);
    let mut spend = open_spend(rng, height);
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(2) {
        match rng.below(12) {
            0 | 1 => {
                spend.lock_time = rng.near_u32(&[
                    0,
                    height,
                    499_999_999,
                    500_000_000,
                    SEED_TIME,
                    crate::hayai_side::PARENT_TIME,
                    u32::MAX,
                ]);
                spend.sequence = *rng.pick(&[0, u32::MAX - 1, u32::MAX, u32::MAX]);
            }
            2 | 3 => spend.expiry = rng.near_u32(&[0, height, 499_999_999, 500_000_000, u32::MAX]),
            4 => spend.version = *rng.pick(&[4, 5, 6]),
            5 => {
                spend.branch = Some(*rng.pick(&[
                    0x76b8_09bb,
                    0xe9ff_75a6,
                    0xc2d6_d0b4,
                    0xc8e7_1055,
                    0x4dec_4df0,
                    0x7719_0ad9,
                    0,
                ]))
            }
            6 => {
                // Output values at the limits of the amount type and of the input value.
                let value =
                    rng.near_u64(&[0, spend.coin.value, MAX_MONEY, i64::MAX as u64, u64::MAX]);
                spend.outputs = vec![TxOut {
                    value,
                    script: vec![OP_1],
                }];
            }
            7 => {
                spend.coin.value = rng.near_u64(&[0, 1, MAX_COIN]).min(MAX_COIN);
                spend.outputs = vec![TxOut {
                    value: spend.coin.value / 2,
                    script: vec![OP_1],
                }];
            }
            8 => spend.outputs.clear(),
            9 => {
                // Two outputs whose sum is above the money limit.
                let half = *rng.pick(&[MAX_MONEY, MAX_MONEY / 2 + 1, 1 << 62]);
                spend.outputs = vec![
                    TxOut {
                        value: half,
                        script: vec![OP_1],
                    };
                    2
                ];
            }
            10 => spend.claim_fee = false,
            _ => {
                // A field of a transaction of the seed.
                let tx = 1 + rng.below(6);
                ops.push(match rng.below(5) {
                    0 => Op::LockTime {
                        tx,
                        value: rng.near_u32(&[0, height, 500_000_000]),
                    },
                    1 => Op::Expiry {
                        tx,
                        value: rng.near_u32(&[0, height, 500_000_000]),
                    },
                    2 => Op::Sequence {
                        tx,
                        input: rng.below(3),
                        value: rng.near_u32(&[0, u32::MAX]),
                    },
                    3 => version_op(rng, tx),
                    _ => Op::OutAdd {
                        tx,
                        output: rng.below(3),
                        delta: *rng.pick(&[1, -1]),
                    },
                });
            }
        }
    }
    ops.insert(0, Op::Spend(spend));
    recipe(seed, ops)
}

fn scripts(rng: &mut Rng) -> Recipe {
    let (seed, height) = spend_seed(rng);
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(2) {
        let pair = script::pair(rng, height, SEED_TIME);
        let mut spend = open_spend(rng, height);
        spend.coin.script = pair.lock;
        spend.script_sig = pair.unlock;
        if let Some(lock_time) = pair.lock_time {
            spend.lock_time = lock_time;
            spend.sequence = *rng.pick(&[0, 0, u32::MAX - 1, u32::MAX]);
        }
        if rng.chance(1, 8) {
            spend.version = 4;
        }
        ops.push(Op::Spend(spend));
    }
    if rng.chance(1, 6) {
        // A bit of a signature script of the seed: the signatures of the fixtures.
        ops.push(Op::InScriptFlip {
            tx: 1 + rng.below(5),
            input: rng.below(2),
            bit: rng.next_u32(),
        });
    }
    recipe(seed, ops)
}

fn spend(rng: &mut Rng) -> Recipe {
    let (seed, height) = spend_seed(rng);
    let base = seed_of(seed);
    let txs = base.block.txs.len();
    let coins = base.ctx.coins.len();
    let mut ops = Vec::new();
    let mut open = open_spend(rng, height);
    match rng.below(14) {
        0 => {
            open.in_context = false;
            ops.push(Op::Spend(open));
        }
        1 | 2 => {
            // A coinbase output at the limit of its maturity. A transaction with a
            // transparent output must not spend it.
            open.coin.coinbase = true;
            open.coin.height =
                (i64::from(height) + [-101i64, -100, -99, -1, 0, 1][rng.below(6)]) as u32;
            if rng.chance(1, 2) {
                open.outputs.clear();
            }
            ops.push(Op::Spend(open));
        }
        3 => {
            // The same coin in two transactions.
            ops.push(Op::Spend(open));
            ops.push(Op::DuplicateTx {
                tx: 1,
                at: 1 + rng.below(txs + 1),
            });
            if rng.chance(1, 2) {
                // The copy gets another id.
                ops.push(Op::LockTime { tx: 1, value: 1 });
            }
        }
        4 => {
            ops.push(Op::Spend(open));
            ops.push(Op::InDuplicate { tx: 1, input: 0 });
        }
        5 | 6 => {
            // A chain in the block: the child spends an output of the parent.
            open.at = 1;
            let mut child = open_spend(rng, height);
            child.parent = Some((1, 0));
            child.outputs[0].value = open.outputs[0].value - 500;
            child.at = if rng.chance(2, 3) { 2 } else { 1 };
            ops.push(Op::Spend(open));
            ops.push(Op::Spend(child));
            if rng.chance(1, 4) {
                ops.push(Op::SwapTxs(1, 2));
            }
        }
        7 => {
            // An input of one transaction in another transaction.
            ops.push(Op::Spend(open));
            ops.push(Op::InCopy {
                tx: 1,
                from: 2 + rng.below(txs),
                input: rng.below(2),
            });
        }
        8 if coins > 0 => ops.push(Op::CoinRemove(rng.below(coins))),
        9 | 10 if coins > 0 => {
            // A coin of the seed as a coinbase output at the limit of its maturity. The
            // Orchard transactions of the seeds have no transparent output.
            let coin = rng.below(coins);
            ops.push(Op::CoinCoinbase { coin, value: true });
            ops.push(Op::CoinHeight {
                coin,
                height: (i64::from(height) + [-101i64, -100, -99, -1, 0][rng.below(5)]) as u32,
            });
        }
        11 if coins > 0 => ops.push(Op::CoinValue {
            coin: rng.below(coins),
            value: rng.near_u64(&[0, 1_000_000, MAX_COIN]),
        }),
        12 if coins > 0 => ops.push(Op::CoinScriptFlip {
            coin: rng.below(coins),
            bit: rng.next_u32(),
        }),
        _ => {
            // A spend of an output of the coinbase of the block.
            open.parent = Some((0, rng.below(3)));
            open.outputs[0].value = 1;
            open.claim_fee = false;
            ops.push(Op::Spend(open));
        }
    }
    recipe(seed, ops)
}

fn shielded(rng: &mut Rng) -> Recipe {
    let seed = shielded_fixture(rng);
    let base = seed_of(seed);
    // A transaction of the seed with a shielded section.
    let with_sections: Vec<usize> = base
        .block
        .txs
        .iter()
        .enumerate()
        .filter_map(|(index, tx)| match tx {
            Tx::Parts(parts) if !parts.sections().is_empty() => Some(index),
            _ => None,
        })
        .collect();
    let tx = *rng.pick(&with_sections);
    let section = rng.below(2);
    let mut ops = Vec::new();
    for _ in 0..1 + rng.below(2) {
        ops.push(match rng.below(16) {
            0 | 1 => Op::Flags {
                tx,
                section,
                value: rng.next_u64() as u8,
            },
            2 | 3 => Op::ValueBalance {
                tx,
                section,
                value: *rng.pick(&[
                    0i64,
                    1,
                    -1,
                    100_000,
                    -100_000,
                    MAX_MONEY as i64,
                    -(MAX_MONEY as i64),
                    MAX_MONEY as i64 + 1,
                    i64::MAX,
                    i64::MIN,
                ]),
            },
            4 => Op::AnchorFlip {
                tx,
                section,
                bit: rng.next_u64() as u8,
            },
            5 => Op::NullifierCopy {
                tx,
                section,
                from: 0,
                to: 1,
            },
            6 => Op::ActionFlip {
                tx,
                section,
                bit: rng.next_u32(),
            },
            7 | 8 => Op::ProofFlip {
                tx,
                section,
                bit: rng.next_u32(),
            },
            9 | 10 => Op::SigFlip {
                tx,
                section,
                bit: rng.next_u32(),
            },
            11 => Op::ProofLen {
                tx,
                section,
                value: rng.near_u64(&[0, 1, 4_992, 7_264, 0xfc, 0xfd, 0xffff, u64::MAX]),
            },
            12 => Op::ChainNullifier {
                tx,
                section,
                action: rng.below(2),
            },
            13 => match rng.below(3) {
                0 => Op::TailFlip {
                    tx,
                    bit: rng.next_u32(),
                },
                1 => Op::TailTruncate {
                    tx,
                    len: rng.next_u32() % 12_000,
                },
                _ => Op::TailAppend {
                    tx,
                    bytes: rng.some_bytes(1, 8),
                },
            },
            14 => Op::DuplicateTx {
                tx,
                at: 1 + rng.below(base.block.txs.len()),
            },
            _ => version_op(rng, tx),
        });
    }
    recipe(seed, ops)
}

/// A locking script with `count` signature operations.
fn sigop_script(rng: &mut Rng, count: usize) -> Vec<u8> {
    let mut script = Vec::new();
    let mut left = count;
    while left > 0 {
        if left >= 20 && rng.chance(1, 4) {
            // OP_CHECKMULTISIG counts as 20 operations in an output script.
            script.push(script::OP_CHECKMULTISIG);
            left -= 20;
        } else {
            script.push(script::OP_CHECKSIG);
            left -= 1;
        }
    }
    script
}

fn limits(rng: &mut Rng) -> Recipe {
    let (seed, height) = spend_seed(rng);
    let base = seed_of(seed);
    let mut open = open_spend(rng, height);
    if rng.chance(3, 4) {
        // Signature operations near the limit. The seed has some of its own (one for each
        // pay-to-public-key-hash output), so the range covers the limit for each seed.
        let own = MAX_BLOCK_SIGOPS - 30 + rng.below(40);
        let outputs = 1 + rng.below(3);
        open.outputs = (0..outputs)
            .map(|index| TxOut {
                value: 1_000,
                script: if index == 0 {
                    sigop_script(rng, own - (outputs - 1) * 3)
                } else {
                    sigop_script(rng, 3)
                },
            })
            .collect();
        if rng.chance(1, 3) {
            // Operations of a redeem script: the block limit counts them too.
            let count = 1 + rng.below(30);
            let redeem = [
                vec![script::OP_0, script::OP_IF],
                sigop_script(rng, count),
                vec![script::OP_ENDIF, OP_1],
            ]
            .concat();
            open.coin.script = script::p2sh(&redeem);
            open.script_sig = script::push(&redeem);
        }
        return recipe(seed, vec![Op::Spend(open)]);
    }
    // The size of the block near the limit: one output with a long script.
    open.outputs = vec![TxOut {
        value: 1_000,
        script: vec![0x6a],
    }];
    let at = open.at;
    let with_spend = recipe(seed, vec![Op::Spend(open.clone())]);
    let size = crate::mutate::build(&with_spend)
        .expect("the seed exists")
        .bytes
        .len();
    let target = MAX_BLOCK_BYTES + rng.below(5) - 2;
    // The length prefix of the script grows from 1 byte to 5 bytes.
    let padding = target - size - 4;
    let mut script = vec![0x6a];
    script.resize(1 + padding, 0);
    open.outputs[0].script = script;
    open.at = at;
    let _ = base;
    recipe(seed, vec![Op::Spend(open)])
}

fn commitments(rng: &mut Rng) -> Recipe {
    let seed = any_fixture(rng);
    let base = seed_of(seed);
    let txs = base.block.txs.len();
    let mut out = recipe(seed, Vec::new());
    match rng.below(8) {
        0 | 1 => {
            out.ops.push(Op::HeaderFlip {
                field: HeaderField::Merkle,
                bit: rng.next_u64() as u8,
            });
            out.fix_merkle = false;
        }
        2 | 3 => {
            out.ops.push(Op::HeaderFlip {
                field: HeaderField::Commitments,
                bit: rng.next_u64() as u8,
            });
            out.fix_commitments = false;
        }
        4 | 5 => {
            // A change of authorizing data. The transaction id stays, so the merkle root
            // stays, and the commitments field does not match.
            let tx = 1 + rng.below(txs - 1);
            out.ops.push(if rng.chance(1, 2) {
                Op::SigFlip {
                    tx,
                    section: rng.below(2),
                    bit: rng.next_u32(),
                }
            } else {
                Op::InScriptFlip {
                    tx,
                    input: 0,
                    bit: rng.next_u32(),
                }
            });
            out.fix_merkle = rng.chance(1, 2);
            out.fix_commitments = false;
        }
        6 => {
            // A body change without a header change.
            out.ops
                .push(Op::SwapTxs(1 + rng.below(txs - 1), 1 + rng.below(txs - 1)));
            out.fix_merkle = rng.chance(1, 2);
            out.fix_commitments = rng.chance(1, 2);
        }
        _ => {
            out.ops.push(Op::RemoveTx(1 + rng.below(txs - 1)));
            out.fix_merkle = rng.chance(1, 2);
            out.fix_commitments = rng.chance(1, 2);
        }
    }
    out
}

fn bytes(rng: &mut Rng) -> Recipe {
    let seed = if rng.chance(1, 6) {
        synthetic(rng)
    } else {
        any_fixture(rng)
    };
    let base = seed_of(seed);
    let len = base.block.bytes().len() as u32;
    let txs = base.block.txs.len();
    let mut out = recipe(seed, Vec::new());
    if rng.chance(1, 2) {
        // A change inside one transaction. The header then commits to the new body, so
        // the change reaches the rules behind the merkle root.
        let tx = rng.below(txs);
        for _ in 0..1 + rng.below(2) {
            out.ops.push(match rng.below(8) {
                0..=3 => Op::TxRawFlip {
                    tx,
                    bit: rng.next_u32(),
                },
                4 => Op::TxRawSet {
                    tx,
                    at: rng.next_u32(),
                    value: *rng.pick(&[0x00, 0x01, 0x7f, 0x80, 0xfc, 0xfd, 0xfe, 0xff]),
                },
                5 => Op::TxRawInsert {
                    tx,
                    at: rng.next_u32(),
                    bytes: rng.some_bytes(1, 4),
                },
                6 => Op::TxRawDelete {
                    tx,
                    at: rng.next_u32(),
                    len: 1 + rng.below(4) as u32,
                },
                // The first bytes of a transaction: version, group, branch, lock time,
                // expiry and counts.
                _ if rng.chance(2, 3) => Op::TxRawFlip {
                    tx,
                    bit: rng.below(8 * 24) as u32,
                },
                _ => Op::TxCountWidth {
                    tx,
                    width: *rng.pick(&[3, 5, 9]),
                },
            });
        }
        out.fix_merkle = rng.chance(4, 5);
        out.fix_commitments = rng.chance(4, 5);
        return out;
    }
    out.fix_merkle = false;
    out.fix_commitments = false;
    for _ in 0..1 + rng.below(2) {
        // Half of the positions are in the header and the first transaction.
        let at = if rng.chance(1, 2) {
            rng.below(1_700) as u32
        } else {
            rng.next_u32() % len
        };
        out.raw.push(match rng.below(8) {
            0..=3 => RawOp::Flip(at * 8 + rng.below(8) as u32),
            4 => RawOp::Set {
                at,
                value: *rng.pick(&[0x00, 0x01, 0x7f, 0x80, 0xfc, 0xfd, 0xfe, 0xff]),
            },
            5 => RawOp::Insert {
                at,
                bytes: rng.some_bytes(1, 4),
            },
            6 => RawOp::Delete {
                at,
                len: 1 + rng.below(4) as u32,
            },
            _ => {
                if rng.chance(1, 2) {
                    RawOp::Truncate(at)
                } else {
                    RawOp::Append(rng.some_bytes(1, 16))
                }
            }
        });
    }
    out
}
