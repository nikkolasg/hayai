//! The smoke run of the differential fuzzer: a fixed seed and a small budget for each
//! class, and the cases that show that the oracle rejects what it must reject.

use std::sync::Once;

use hayai_fuzz::classes::Class;
use hayai_fuzz::context::{Coin, Net};
use hayai_fuzz::header::{self, HeaderClass};
use hayai_fuzz::known;
use hayai_fuzz::model::TxOut;
use hayai_fuzz::mutate::{HeaderField, Op, RawOp, Recipe, Spend};
use hayai_fuzz::run::{self, Outcome, Plan, Report};
use hayai_fuzz::seeds::{FixtureId, SeedSpec};
use hayai_fuzz::verdict::{Comparison, RuleClass};

/// The seed of the smoke run. A change of the seed changes the cases, not the test.
const SMOKE_SEED: u64 = 0x6861_7961_6900_0c03;

/// Cases of each class in the smoke run. A debug build verifies an Orchard proof about 50
/// times slower than a release build, so its budget is smaller.
const SMOKE_CASES: u64 = if cfg!(debug_assertions) { 6 } else { 400 };

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(run::init);
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

fn outcome(recipe: &Recipe) -> Outcome {
    run::check_recipe(recipe).expect("the seed exists")
}

/// A spend of a new coin with the locking script `OP_1`.
fn open_spend(height: u32) -> Spend {
    Spend {
        coin: Coin {
            value: 1_000_000,
            script: vec![0x51],
            height: height - 1_000,
            coinbase: false,
        },
        in_context: true,
        parent: None,
        script_sig: Vec::new(),
        sequence: u32::MAX,
        outputs: vec![TxOut {
            value: 990_000,
            script: vec![0x51],
        }],
        lock_time: 0,
        expiry: 0,
        version: 0,
        branch: None,
        claim_fee: true,
        at: 1,
    }
}

#[test]
fn every_seed_is_valid_for_both_implementations() {
    init();
    let mut seeds: Vec<SeedSpec> = FixtureId::ALL.into_iter().map(SeedSpec::Fixture).collect();
    // One height in each epoch from Canopy to NU6.3 on Mainnet and on Testnet, and the
    // NU6.1 activation block of Mainnet with its lockbox disbursement outputs.
    for height in [
        1_100_000, 1_700_000, 2_800_000, 3_146_400, 3_200_000, 3_400_000, 3_450_000,
    ] {
        seeds.push(SeedSpec::Synthetic {
            network: Net::Mainnet,
            height,
        });
    }
    for height in [
        1_100_000, 1_900_000, 2_980_000, 3_600_000, 4_100_000, 4_200_000,
    ] {
        seeds.push(SeedSpec::Synthetic {
            network: Net::Testnet,
            height,
        });
    }
    for seed in seeds {
        let outcome = outcome(&recipe(seed, Vec::new()));
        assert_eq!(
            outcome.comparison,
            Comparison::BothAccept,
            "{seed:?}: {outcome:?}"
        );
    }
}

#[test]
fn a_spend_without_a_signature_is_valid_for_both_implementations() {
    init();
    for seed in [
        SeedSpec::Fixture(FixtureId::Mixed),
        SeedSpec::Fixture(FixtureId::Nu63),
        SeedSpec::Synthetic {
            network: Net::Mainnet,
            height: 1_100_000,
        },
        SeedSpec::Synthetic {
            network: Net::Testnet,
            height: 4_200_000,
        },
    ] {
        let height = hayai_fuzz::seeds::seed(seed)
            .expect("the seed exists")
            .ctx
            .height;
        let outcome = outcome(&recipe(seed, vec![Op::Spend(open_spend(height))]));
        assert_eq!(
            outcome.comparison,
            Comparison::BothAccept,
            "{seed:?}: {outcome:?}"
        );
    }
}

/// Each mutation breaks one rule. Both implementations must reject the block, and both
/// must name the class of the rule. A case that fails here shows a rule that the oracle
/// or the class map lost.
#[test]
fn both_implementations_reject_each_broken_rule() {
    init();
    let mixed = SeedSpec::Fixture(FixtureId::Mixed);
    let nu63 = SeedSpec::Fixture(FixtureId::Nu63);
    let height = hayai_fuzz::seeds::seed(mixed)
        .expect("the seed exists")
        .ctx
        .height;
    let mut cases: Vec<(&str, Recipe, RuleClass)> = Vec::new();
    let mut case = |name, seed, ops, class| cases.push((name, recipe(seed, ops), class));

    let miner_output = |delta| Op::OutAdd {
        tx: 0,
        output: 0,
        delta,
    };
    case(
        "the coinbase pays 1 zatoshi more",
        mixed,
        vec![miner_output(1)],
        RuleClass::CoinbaseTerms,
    );
    case(
        "the coinbase pays 1 zatoshi less",
        mixed,
        vec![miner_output(-1)],
        RuleClass::CoinbaseTerms,
    );
    case(
        "a funding stream output pays 1 zatoshi less",
        mixed,
        vec![
            Op::OutAdd {
                tx: 0,
                output: 1,
                delta: -1,
            },
            miner_output(1),
        ],
        RuleClass::CoinbaseTerms,
    );
    case(
        "a funding stream output has another script",
        mixed,
        vec![Op::OutScriptFlip {
            tx: 0,
            output: 1,
            bit: 30,
        }],
        RuleClass::CoinbaseTerms,
    );
    case(
        "the coinbase has the expiry height of another block",
        mixed,
        vec![Op::Expiry {
            tx: 0,
            value: height + 1,
        }],
        RuleClass::TxTime,
    );
    case(
        "a transaction is in the block two times",
        mixed,
        vec![Op::DuplicateTx { tx: 1, at: 2 }],
        RuleClass::Merkle,
    );
    case(
        "a spend expired at the block before",
        mixed,
        vec![Op::Spend(Spend {
            expiry: height - 1,
            ..open_spend(height)
        })],
        RuleClass::TxTime,
    );
    case(
        "a spend has a lock time of the next height",
        mixed,
        vec![Op::Spend(Spend {
            lock_time: height + 1,
            sequence: 0,
            ..open_spend(height)
        })],
        RuleClass::TxTime,
    );
    case(
        "a spend pays more than its input",
        mixed,
        vec![Op::Spend(Spend {
            outputs: vec![TxOut {
                value: 1_000_001,
                script: vec![0x51],
            }],
            claim_fee: false,
            ..open_spend(height)
        })],
        RuleClass::Value,
    );
    case(
        "a spend of a coin that the chain does not have",
        mixed,
        vec![Op::Spend(Spend {
            in_context: false,
            ..open_spend(height)
        })],
        RuleClass::TransparentInput,
    );
    case(
        "a spend of a coinbase output to a transparent output",
        mixed,
        vec![Op::Spend(Spend {
            coin: Coin {
                value: 1_000_000,
                script: vec![0x51],
                height: height - 99,
                coinbase: true,
            },
            ..open_spend(height)
        })],
        RuleClass::TransparentInput,
    );
    case(
        "a locking script that leaves a false value",
        mixed,
        vec![Op::Spend(Spend {
            coin: Coin {
                value: 1_000_000,
                script: vec![0x00],
                height: height - 1_000,
                coinbase: false,
            },
            ..open_spend(height)
        })],
        RuleClass::Script,
    );
    case(
        "a signature of a seed transaction with one changed bit",
        mixed,
        vec![Op::InScriptFlip {
            tx: 1,
            input: 0,
            bit: 80,
        }],
        RuleClass::Script,
    );
    case(
        "an Orchard proof with one changed bit",
        mixed,
        vec![Op::ProofFlip {
            tx: 4,
            section: 0,
            bit: 4_001,
        }],
        RuleClass::ShieldedProof,
    );
    case(
        "an Ironwood signature with one changed bit",
        nu63,
        vec![Op::SigFlip {
            tx: 1,
            section: 0,
            bit: 77,
        }],
        RuleClass::ShieldedProof,
    );
    case(
        "a nullifier that the chain has",
        nu63,
        vec![Op::ChainNullifier {
            tx: 1,
            section: 0,
            action: 0,
        }],
        RuleClass::Nullifier,
    );
    for (name, recipe, class) in &cases {
        let outcome = outcome(recipe);
        assert_eq!(
            (outcome.comparison, run::reject_classes(&outcome)),
            (Comparison::BothReject, (Some(*class), Some(*class))),
            "{name}: {outcome:?}"
        );
    }

    // The two header fields that commit to the body, without the correction of the
    // header.
    let stale = |op, class| {
        let mut recipe = recipe(mixed, vec![op]);
        recipe.fix_merkle = false;
        recipe.fix_commitments = false;
        let outcome = outcome(&recipe);
        assert_eq!(
            (outcome.comparison, run::reject_classes(&outcome)),
            (Comparison::BothReject, (Some(class), Some(class))),
            "{recipe:?}: {outcome:?}"
        );
    };
    stale(
        Op::HeaderFlip {
            field: HeaderField::Merkle,
            bit: 9,
        },
        RuleClass::Merkle,
    );
    stale(
        Op::HeaderFlip {
            field: HeaderField::Commitments,
            bit: 9,
        },
        RuleClass::Commitments,
    );
    // A changed signature changes the authorizing data root and not the merkle root. The
    // block breaks two rules, the signature and the commitments: both must reject it.
    let mut changed = recipe(
        mixed,
        vec![Op::SigFlip {
            tx: 4,
            section: 0,
            bit: 3,
        }],
    );
    changed.fix_merkle = false;
    changed.fix_commitments = false;
    let outcome = outcome(&changed);
    assert!(
        matches!(
            run::reject_classes(&outcome),
            (
                Some(RuleClass::Commitments | RuleClass::ShieldedProof),
                Some(RuleClass::Commitments | RuleClass::ShieldedProof)
            )
        ),
        "{outcome:?}"
    );
}

/// The limits at their exact values: 20,000 signature operations and 2,000,000 bytes are
/// valid, one more is not.
#[test]
fn both_implementations_agree_at_the_block_limits() {
    init();
    let seed = SeedSpec::Synthetic {
        network: Net::Mainnet,
        height: 3_400_000,
    };
    // The synthetic coinbase has one signature operation in its miner output.
    let with_sigops = |count: usize| {
        let mut spend = open_spend(3_400_000);
        spend.outputs = vec![TxOut {
            value: 1_000,
            script: vec![0xac; count],
        }];
        outcome(&recipe(seed, vec![Op::Spend(spend)]))
    };
    assert_eq!(with_sigops(19_999).comparison, Comparison::BothAccept);
    let over = with_sigops(20_000);
    assert_eq!(
        (over.comparison, run::reject_classes(&over)),
        (
            Comparison::BothReject,
            (Some(RuleClass::Limits), Some(RuleClass::Limits))
        ),
        "{over:?}"
    );

    let with_size = |size: usize| {
        let mut spend = open_spend(3_400_000);
        spend.outputs = vec![TxOut {
            value: 1_000,
            script: vec![0x6a],
        }];
        let small = hayai_fuzz::mutate::build(&recipe(seed, vec![Op::Spend(spend.clone())]))
            .expect("the seed exists")
            .bytes
            .len();
        // The length prefix of the script grows from 1 byte to 5 bytes.
        spend.outputs[0].script.resize(1 + size - small - 4, 0);
        let recipe = recipe(seed, vec![Op::Spend(spend)]);
        assert_eq!(
            hayai_fuzz::mutate::build(&recipe)
                .expect("the seed exists")
                .bytes
                .len(),
            size
        );
        outcome(&recipe)
    };
    assert_eq!(with_size(2_000_000).comparison, Comparison::BothAccept);
    let over = with_size(2_000_001);
    assert_eq!(over.comparison, Comparison::BothReject, "{over:?}");
}

/// `docs/fuzz-findings.md`, K1: bytes after the block.
#[test]
fn bytes_after_the_block_are_a_known_difference() {
    init();
    let mut recipe = recipe(SeedSpec::Fixture(FixtureId::Transparent), Vec::new());
    recipe.raw.push(RawOp::Append(vec![0]));
    let outcome = outcome(&recipe);
    assert_eq!(outcome.comparison, Comparison::HayaiRejects, "{outcome:?}");
    assert_eq!(outcome.known.as_deref(), Some(known::TRAILING_BYTES));
}

/// `docs/fuzz-findings.md`, K2: the header version rule does not run in a block case.
#[test]
fn the_header_version_is_a_known_difference_of_the_block_cases() {
    init();
    for version in [3, 0x8000_0004] {
        let outcome = outcome(&recipe(
            SeedSpec::Fixture(FixtureId::Transparent),
            vec![Op::HeaderVersion(version)],
        ));
        assert_eq!(outcome.comparison, Comparison::HayaiAccepts, "{outcome:?}");
        assert_eq!(outcome.known.as_deref(), Some(known::HEADER_VERSION));
    }
}

fn smoke_report() -> Report {
    init();
    let plan = Plan {
        seed: SMOKE_SEED,
        classes: Class::ALL.to_vec(),
        iterations: Some(SMOKE_CASES),
        seconds: None,
        out_dir: Some(run::default_out_dir().join("smoke")),
        per_signature: 2,
    };
    let mut report = run::run(&plan);
    header::run(&plan, &HeaderClass::ALL, &mut report);
    report
}

#[test]
fn smoke_run_has_no_finding() {
    let report = smoke_report();
    assert!(
        report.findings.is_empty(),
        "findings of the smoke run (case files in target/fuzz/findings/smoke): {:#?}",
        report.findings
    );
    for (class, stats) in &report.classes {
        assert_eq!(stats.cases, SMOKE_CASES, "{class}");
        assert_eq!(stats.findings, 0, "{class}");
        if cfg!(debug_assertions) {
            continue;
        }
        // Each class reaches valid blocks and invalid blocks.
        let count = |comparison| stats.comparisons.get(&comparison).copied().unwrap_or(0);
        assert!(count(Comparison::BothAccept) > 0, "{class}: {stats:?}");
        assert!(
            count(Comparison::BothReject) + count(Comparison::BothRejectOtherClass) > 0,
            "{class}: {stats:?}"
        );
    }
}

#[test]
fn a_case_seed_gives_the_same_case_two_times() {
    init();
    for class in Class::ALL {
        let seed = hayai_fuzz::rng::case_seed(SMOKE_SEED, class.name(), 3);
        let first = run::recipe_of(class, seed);
        assert_eq!(first, run::recipe_of(class, seed), "{}", class.name());
        let bytes = |recipe| {
            hayai_fuzz::mutate::build(recipe)
                .expect("the seed exists")
                .bytes
        };
        assert_eq!(bytes(&first), bytes(&first.clone()), "{}", class.name());
        // The recipe of a case file gives the block of the case again.
        let json = serde_json::to_string(&first).expect("a recipe is JSON");
        let read: Recipe = serde_json::from_str(&json).expect("the JSON is a recipe");
        assert_eq!(read, first, "{}", class.name());
    }
}
