//! The header rules on the published block vectors and at their boundaries.

use std::time::Instant;

use hayai_consensus::difficulty::expected_bits;
use hayai_consensus::header::{
    check_contextual, check_header, check_local_time, check_proof_of_work, check_version,
    HeaderRuleError, HeaderVerdict, Unchecked, MAX_FUTURE_BLOCK_TIME_LOCAL,
    MAX_FUTURE_BLOCK_TIME_MTP,
};
use hayai_consensus::{ContextTooShort, Network, ParentChain};
use hayai_wire::header::{BlockHash, BlockHeader, PowError};

fn vector_header(path: &str) -> BlockHeader {
    let name = format!("{}/../{path}", env!("CARGO_MANIFEST_DIR"));
    let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let bytes = hex::decode(hex.trim()).expect("hex");
    BlockHeader::parse(&bytes).expect("a header")
}

fn bench_vector(network: &str, height: u32) -> BlockHeader {
    vector_header(&format!(
        "hayai-bench/tests/vectors/block-{network}-0-000-{height:03}.hex"
    ))
}

/// The first headers of a network from Zebra's vectors, from the genesis block.
fn first_headers(network: Network) -> Vec<BlockHeader> {
    match network {
        Network::Mainnet => (0..=10).map(|h| bench_vector("main", h)).collect(),
        Network::Testnet => (0..=9).map(|h| bench_vector("test", h)).collect(),
        Network::Regtest | Network::ConfiguredRegtest(_) => vec![vector_header(
            "hayai-wire/tests/vectors/block-regtest-0-000-000.hex",
        )],
    }
}

/// The genesis block of each network (Zebra's vectors) has the hash and the time of the
/// network parameters, and its `nBits` is the compact proof-of-work limit.
#[test]
fn the_genesis_blocks_match_the_network_parameters() {
    for network in Network::ALL {
        let genesis = &first_headers(network)[0];
        let params = network.params();
        assert_eq!(genesis.hash(), params.genesis_hash, "{network:?}");
        assert_eq!(genesis.time, params.genesis_time, "{network:?}");
        assert_eq!(genesis.bits, params.pow_limit_bits, "{network:?}");
        assert_eq!(genesis.prev_hash, BlockHash([0; 32]));
        assert_eq!(genesis.solution.len(), params.pow.solution_len());
    }
    assert_eq!(Network::Testnet.params().genesis_time, 1_477_648_033);
    assert_eq!(
        Network::Testnet.params().genesis_hash.to_string(),
        "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38"
    );
}

/// The context of the header at index `height` of `headers`, newest first.
fn context_of(headers: &[BlockHeader], height: usize) -> (Vec<u32>, Vec<u32>) {
    let before = headers[..height].iter().rev();
    (
        before.clone().map(|h| h.time).collect(),
        before.map(|h| h.bits).collect(),
    )
}

/// Every rule passes on the first real blocks of Mainnet and Testnet with their real
/// context, and the verdict is complete: these heights read no more than the set holds.
#[test]
fn the_first_real_blocks_pass_every_rule() {
    let mut equihash = Vec::new();
    for network in [Network::Mainnet, Network::Testnet] {
        let headers = first_headers(network);
        for height in 1..headers.len() {
            let (times, bits) = context_of(&headers, height);
            let chain = ParentChain {
                height: height as u32,
                times: &times,
                bits: &bits,
            };
            let header = &headers[height];
            assert_eq!(
                check_header(network, header, &chain, Some(header.time)),
                Ok(HeaderVerdict::Checked),
                "{network:?} {height}"
            );
            // The clock of the node is read only when it is given: a clock more than 2 h
            // behind the header fails the local rule, and a replay has no clock.
            let behind = header.time - MAX_FUTURE_BLOCK_TIME_LOCAL - 1;
            assert_eq!(
                check_header(network, header, &chain, Some(behind)),
                Err(HeaderRuleError::TimeTooFarAhead {
                    time: header.time,
                    limit: header.time - 1
                })
            );
            assert_eq!(
                check_header(network, header, &chain, None),
                Ok(HeaderVerdict::Checked)
            );
            let started = Instant::now();
            assert_eq!(check_proof_of_work(network, header), Ok(()));
            equihash.push(started.elapsed());
            assert_eq!(check_local_time(header, header.time), Ok(()));
        }
    }
    equihash.sort();
    println!(
        "check_proof_of_work (hash filter and Equihash (200, 9)): median {:?}",
        equihash[equihash.len() / 2]
    );
}

#[test]
fn a_real_header_with_one_broken_rule_fails_that_rule() {
    let network = Network::Mainnet;
    let headers = first_headers(network);
    let height = 5;
    let (times, bits) = context_of(&headers, height);
    let chain = ParentChain {
        height: height as u32,
        times: &times,
        bits: &bits,
    };
    let good = &headers[height];
    let check = |change: &dyn Fn(&mut BlockHeader)| {
        let mut header = good.clone();
        change(&mut header);
        check_header(network, &header, &chain, None)
    };

    // The version is a signed 32-bit integer for zcashd: a value with the high bit set is
    // below 4. Zakura rejects the same values (`zakura-chain/src/block/serialize.rs:51`).
    for version in [0, 3, 0x8000_0000, 0x8000_0004, u32::MAX] {
        assert_eq!(
            check(&|h| h.version = version),
            Err(HeaderRuleError::Version(version))
        );
        let mut header = good.clone();
        header.version = version;
        assert_eq!(
            check_version(&header),
            Err(HeaderRuleError::Version(version))
        );
    }
    // The versions that pass: 4, the largest positive value, and the bit-reversed 4 of
    // about 4,000 Mainnet blocks (the hash changes, so only the version rule runs).
    for version in [4, 5, 0x2000_0000, 0x7fff_ffff] {
        let mut header = good.clone();
        header.version = version;
        assert_eq!(check_version(&header), Ok(()));
        assert_eq!(
            check_contextual(network, &header, &chain),
            Ok(HeaderVerdict::Checked)
        );
    }
    // A target above the limit: the limit rule fails before the expected value is read.
    assert_eq!(
        check(&|h| h.bits = 0x2007_ffff),
        Err(HeaderRuleError::Pow(PowError::TargetAboveLimit(
            0x2007_ffff
        )))
    );
    assert_eq!(
        check(&|h| h.bits = 0x1f80_0001),
        Err(HeaderRuleError::Pow(PowError::InvalidBits(0x1f80_0001)))
    );
    // A harder target than the chain requires.
    assert_eq!(
        check(&|h| h.bits = 0x1f07_fffe),
        Err(HeaderRuleError::WrongBits {
            expected: 0x1f07_ffff,
            got: 0x1f07_fffe
        })
    );
    // Times 0..=4 of the chain: the median is the time of block 2.
    let median = headers[2].time;
    assert_eq!(
        check(&|h| h.time = median),
        Err(HeaderRuleError::TimeTooEarly {
            time: median,
            median_time_past: median
        })
    );
    let limit = median + MAX_FUTURE_BLOCK_TIME_MTP;
    assert_eq!(
        check(&|h| h.time = limit + 1),
        Err(HeaderRuleError::TimeTooLate {
            time: limit + 1,
            limit
        })
    );
    // The contextual rules pass; the proof of work breaks with the changed field.
    let changed = check(&|h| h.time = limit);
    assert!(
        matches!(
            changed,
            Err(HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_))
        ),
        "{changed:?}"
    );
    let changed = check(&|h| h.nonce[0] ^= 1);
    assert!(
        matches!(
            changed,
            Err(HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_))
        ),
        "{changed:?}"
    );
    // A solution that fails Equihash on a header whose hash meets the target: search a
    // changed solution whose hash passes the filter (about one try in 8192).
    let mut bad = good.clone();
    let mut counter = 0u32;
    loop {
        bad.solution[..4].copy_from_slice(&counter.to_le_bytes());
        match check_proof_of_work(network, &bad) {
            Err(HeaderRuleError::Pow(PowError::HashAboveTarget)) => counter += 1,
            other => {
                assert!(
                    matches!(other, Err(HeaderRuleError::Equihash(_))),
                    "{other:?}"
                );
                break;
            }
        }
    }
    // A header of another network: the solution length fails before any hash work.
    assert_eq!(
        check_proof_of_work(Network::Regtest, good),
        Err(HeaderRuleError::SolutionLength {
            expected: 36,
            got: 1344
        })
    );
    assert_eq!(
        check_header(network, good, &ParentChain { height: 0, ..chain }, None),
        Err(HeaderRuleError::Genesis)
    );
}

/// A header without proof of work, for the rules that read no hash.
fn plain_header(network: Network, time: u32, bits: u32) -> BlockHeader {
    BlockHeader {
        version: 4,
        prev_hash: BlockHash([7; 32]),
        merkle_root: [1; 32],
        block_commitments: [2; 32],
        time,
        bits,
        nonce: [0; 32],
        solution: vec![0; network.params().pow.solution_len()],
    }
}

/// `time > median-time-past` at every height, and `time <= median-time-past + 90 min`
/// from the start height of each network: Mainnet 2, Testnet 653,606, Regtest 2.
#[test]
fn the_time_rules_and_their_start_heights() {
    let base = 1_600_000_000u32;
    for (network, height, max_time_applies) in [
        (Network::Mainnet, 1, false),
        (Network::Mainnet, 2, true),
        (Network::Mainnet, 3_000_000, true),
        (Network::Testnet, 1, false),
        (Network::Testnet, 653_605, false),
        (Network::Testnet, 653_606, true),
        (Network::Regtest, 1, false),
        (Network::Regtest, 2, true),
    ] {
        // Every block before the header has the time `base`, so the median is `base`.
        let len = height.min(28) as usize;
        let times = vec![base; len];
        let bits = vec![0x1e01_0000; len];
        let chain = ParentChain {
            height,
            times: &times,
            bits: &bits,
        };
        let at = |time: u32| {
            let bits = match network {
                Network::Regtest => network.params().pow_limit_bits,
                _ => expected_bits(network, time, &chain).expect("a full context"),
            };
            check_contextual(network, &plain_header(network, time, bits), &chain)
        };
        let label = format!("{network:?} {height}");
        assert_eq!(
            at(base),
            Err(HeaderRuleError::TimeTooEarly {
                time: base,
                median_time_past: base
            }),
            "{label}"
        );
        assert_eq!(at(base + 1), Ok(HeaderVerdict::Checked), "{label}");
        let limit = base + MAX_FUTURE_BLOCK_TIME_MTP;
        assert_eq!(at(limit), Ok(HeaderVerdict::Checked), "{label}");
        let late = at(limit + 1);
        if max_time_applies {
            let expected = HeaderRuleError::TimeTooLate {
                time: limit + 1,
                limit,
            };
            assert_eq!(late, Err(expected), "{label}");
        } else {
            assert_eq!(late, Ok(HeaderVerdict::Checked), "{label}");
        }
    }
}

/// The median-time-past reads the newest 11 times, in any order.
#[test]
fn the_median_time_past_reads_eleven_blocks() {
    let network = Network::Regtest;
    let bits = network.params().pow_limit_bits;
    // Newest first: 11 times with the median 500, then older times that do not count.
    let mut times = vec![900, 100, 800, 200, 700, 300, 600, 400, 500, 450, 550];
    times.extend([10_000; 17]);
    let chain = ParentChain {
        height: 1_000,
        times: &times,
        bits: &[],
    };
    assert_eq!(
        check_contextual(network, &plain_header(network, 500, bits), &chain),
        Err(HeaderRuleError::TimeTooEarly {
            time: 500,
            median_time_past: 500
        })
    );
    assert_eq!(
        check_contextual(network, &plain_header(network, 501, bits), &chain),
        Ok(HeaderVerdict::Checked)
    );
}

/// `nBits` against the expected value on a full context, and the result when the context
/// is too short: a distinct verdict with the rules that did not run.
#[test]
fn bits_are_checked_or_reported_as_unchecked() {
    let network = Network::Mainnet;
    let height = 2_000_000;
    let time = 1_700_000_000u32;
    let times: Vec<u32> = (1..=28).map(|i| time - 75 * i).collect();
    let bits = vec![0x1c10_0000; 28];
    let full = ParentChain {
        height,
        times: &times,
        bits: &bits,
    };
    let expected = expected_bits(network, time, &full).expect("a full context");
    let good = plain_header(network, time, expected);
    assert_eq!(
        check_contextual(network, &good, &full),
        Ok(HeaderVerdict::Checked)
    );
    let wrong = plain_header(network, time, expected - 1);
    assert_eq!(
        check_contextual(network, &wrong, &full),
        Err(HeaderRuleError::WrongBits {
            expected,
            got: expected - 1
        })
    );

    // 27 times: the time rules run, the bits rule does not. Wrong bits are not detected,
    // and the verdict says so.
    let short = ParentChain {
        times: &times[..27],
        ..full
    };
    let unchecked_bits = HeaderVerdict::ContextTooShort(Unchecked {
        time: false,
        bits: true,
        context: ContextTooShort {
            times: 27,
            needed_times: 28,
            bits: 28,
            needed_bits: 17,
        },
    });
    assert_eq!(
        check_contextual(network, &wrong, &short),
        Ok(unchecked_bits)
    );
    // The time rules still fail on a short difficulty context.
    let early = plain_header(network, times[5], expected);
    assert!(matches!(
        check_contextual(network, &early, &short),
        Err(HeaderRuleError::TimeTooEarly { .. })
    ));
    // 28 times and 16 bits: the same verdict.
    let short = ParentChain {
        bits: &bits[..16],
        ..full
    };
    assert!(matches!(
        check_contextual(network, &good, &short),
        Ok(HeaderVerdict::ContextTooShort(Unchecked {
            time: false,
            bits: true,
            ..
        }))
    ));
    // 10 times: no rule that reads the context runs.
    let short = ParentChain {
        times: &times[..10],
        bits: &[],
        ..full
    };
    assert_eq!(
        check_contextual(network, &early, &short),
        Ok(HeaderVerdict::ContextTooShort(Unchecked {
            time: true,
            bits: true,
            context: ContextTooShort {
                times: 10,
                needed_times: 28,
                bits: 0,
                needed_bits: 17,
            },
        }))
    );
    // The rules without context still run on a short context.
    let mut old = good.clone();
    old.version = 3;
    assert_eq!(
        check_contextual(network, &old, &short),
        Err(HeaderRuleError::Version(3))
    );
    assert_eq!(
        check_contextual(network, &plain_header(network, time, 0x2007_ffff), &short),
        Err(HeaderRuleError::Pow(PowError::TargetAboveLimit(
            0x2007_ffff
        )))
    );
    // A block of the context with bits that encode no target.
    let mut invalid = bits.clone();
    invalid[0] = 0;
    let broken = ParentChain {
        bits: &invalid,
        ..full
    };
    assert_eq!(
        check_contextual(network, &good, &broken),
        Err(HeaderRuleError::InvalidContextBits(0))
    );
}

/// Regtest (Zakura's `disable_pow`): a null solution passes, any target at or below the
/// limit passes without a difficulty context, and the time rules apply.
#[test]
fn regtest_waives_the_proof_of_work_only() {
    let network = Network::Regtest;
    let limit = network.params().pow_limit_bits;
    let times = [1_000u32, 990, 980];
    let chain = ParentChain {
        height: 3,
        times: &times,
        bits: &[],
    };
    for bits in [limit, 0x200f_0f0e, 0x1f07_ffff, 0x1c10_0000] {
        let header = plain_header(network, 1_001, bits);
        assert_eq!(
            check_header(network, &header, &chain, None),
            Ok(HeaderVerdict::Checked),
            "{bits:#x}"
        );
    }
    for bits in [0x200f_0f10, 0x2010_0000] {
        let header = plain_header(network, 1_001, bits);
        assert_eq!(
            check_header(network, &header, &chain, None),
            Err(HeaderRuleError::Pow(PowError::TargetAboveLimit(bits)))
        );
        assert_eq!(
            check_proof_of_work(network, &header),
            Err(HeaderRuleError::Pow(PowError::TargetAboveLimit(bits)))
        );
    }
    for bits in [0x0480_0001, 0] {
        assert_eq!(
            check_header(network, &plain_header(network, 1_001, bits), &chain, None),
            Err(HeaderRuleError::Pow(PowError::InvalidBits(bits)))
        );
    }
    assert!(matches!(
        check_header(network, &plain_header(network, 990, limit), &chain, None),
        Err(HeaderRuleError::TimeTooEarly { .. })
    ));
    // The solution length of the network still applies.
    let mut mainnet = plain_header(network, 1_001, limit);
    mainnet.solution = vec![0; 1344];
    assert_eq!(
        check_header(network, &mainnet, &chain, None),
        Err(HeaderRuleError::SolutionLength {
            expected: 36,
            got: 1344
        })
    );
    // Two times at height 3: the median-time-past is not known.
    let short = ParentChain {
        times: &times[..2],
        ..chain
    };
    assert_eq!(
        check_header(network, &plain_header(network, 1, limit), &short, None),
        Ok(HeaderVerdict::ContextTooShort(Unchecked {
            time: true,
            bits: false,
            context: ContextTooShort {
                times: 2,
                needed_times: 3,
                bits: 0,
                needed_bits: 0,
            },
        }))
    );
}

/// The local rule: at most 2 h after the clock of the node. It is apart from the consensus
/// rules, so a replay does not apply it.
#[test]
fn the_local_time_rule() {
    let now = 1_700_000_000u32;
    let header = |time| plain_header(Network::Regtest, time, 0x200f_0f0f);
    let limit = now + MAX_FUTURE_BLOCK_TIME_LOCAL;
    assert_eq!(check_local_time(&header(0), now), Ok(()));
    assert_eq!(check_local_time(&header(limit), now), Ok(()));
    assert_eq!(
        check_local_time(&header(limit + 1), now),
        Err(HeaderRuleError::TimeTooFarAhead {
            time: limit + 1,
            limit
        })
    );
    assert_eq!(check_local_time(&header(u32::MAX), u32::MAX), Ok(()));
    // The consensus rules do not read the clock: a header far ahead of any clock passes
    // them when its context allows it.
    let chain = ParentChain {
        height: 1,
        times: &[u32::MAX - 10],
        bits: &[],
    };
    assert_eq!(
        check_contextual(Network::Regtest, &header(u32::MAX), &chain),
        Ok(HeaderVerdict::Checked)
    );
}
