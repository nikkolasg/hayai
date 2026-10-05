//! Conformance of the NU7 rules of hayai-consensus with the baseline `zakura-chain` and
//! `zakura-header-chain`, across the NU7 activation height of Testnet (4,465,026) and of a
//! configured Regtest (NU7 at height 9).
//!
//! 1. The schedule: the halving index, the subsidy of the halving schedule, the scheduled
//!    issuance, the funding streams, the address period and the end of the last stream
//!    set. These functions do not read the rule set, so the comparison runs on each
//!    crypto backend.
//! 2. The NSM: the miner share of the fees, the seed, the reissuance height and the
//!    subsidy with the reissuance bonus.
//! 3. The block limits and the difficulty parameters of ZIP 218.
//! 4. The expected `nBits` of generated chains that cross the NU7 height. This part needs
//!    the NU7 rule set.

use chrono::DateTime;
use hayai_consensus::coinbase::CoinbaseTerms;
use hayai_consensus::difficulty::expected_bits;
use hayai_consensus::funding::funding_streams;
use hayai_consensus::{
    nsm, rules_at, subsidy, BlockLimits, DifficultyParams, Network, ParentChain, RegtestConfig,
    RuleSet, Upgrade,
};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use zk_chain::amount::{Amount, NonNegative};
use zk_chain::block::Height;
use zk_chain::parameters::subsidy as zk_subsidy;
use zk_chain::parameters::testnet::{ConfiguredActivationHeights, RegtestParameters};
use zk_chain::parameters::{Network as ZkNetwork, NetworkUpgrade};
use zk_chain::work::difficulty::CompactDifficulty;
use zk_header_chain::AdjustedDifficulty;

const TESTNET_NU7: u32 = 4_465_026;
const REGTEST_NU7: u32 = 9;
const MAX_MONEY: u64 = 2_100_000_000_000_000;

/// The same Regtest in hayai and in Zakura: NU5 at height 1, NU6.3 at height 5, NU7 at
/// height 9.
fn regtests() -> (Network, ZkNetwork) {
    let hayai = RegtestConfig::new(
        &[(Upgrade::Nu6_3, 5), (Upgrade::Nu7, REGTEST_NU7)],
        Vec::new(),
        0,
    )
    .expect("a valid configuration")
    .network();
    let zakura = ZkNetwork::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6_3: Some(5),
            nu7: Some(REGTEST_NU7),
            ..Default::default()
        },
        ..Default::default()
    });
    (hayai, zakura)
}

/// Each network with its baseline and its NU7 height.
fn networks() -> Vec<(Network, ZkNetwork, u32)> {
    let (regtest, zk_regtest) = regtests();
    vec![
        (
            Network::Testnet,
            ZkNetwork::new_default_testnet(),
            TESTNET_NU7,
        ),
        (regtest, zk_regtest, REGTEST_NU7),
    ]
}

fn zatoshis(amount: Amount<NonNegative>) -> u64 {
    u64::from(amount)
}

fn amount(zatoshis: u64) -> Amount<NonNegative> {
    Amount::try_from(i64::try_from(zatoshis).expect("fits")).expect("a valid amount")
}

/// The subsidy of the halving schedule at `height`, from the public interface of hayai:
/// the step of the scheduled issuance.
fn halving_subsidy(network: Network, height: u32) -> u64 {
    let step = subsidy::scheduled_issuance(network, height)
        - subsidy::scheduled_issuance(network, height - 1);
    u64::try_from(step).expect("a subsidy fits in 64 bits")
}

/// The heights of the comparison for a network with NU7 at `nu7`: each height near NU7,
/// near the old and the new third halving and near the next halvings, and a sample of
/// the heights up to the fifth halving.
fn heights(network: Network, nu7: u32) -> Vec<u32> {
    let mut heights: Vec<u32> = (nu7.saturating_sub(40).max(1)..nu7 + 400).collect();
    for index in 1..=5 {
        if let Some(halving) = first_height_with_halving(network, index) {
            heights.extend(halving.saturating_sub(3).max(1)..halving + 3);
        }
    }
    if network == Network::Testnet {
        // The third halving before ZIP 218, and the address period boundaries.
        heights.extend(4_475_990..4_476_010);
        heights.extend((nu7..4_500_000).step_by(997));
        heights.extend((1..14_000_000).step_by(99_991));
    } else {
        heights.extend(1..3_000);
    }
    heights.sort_unstable();
    heights.dedup();
    heights
}

/// The first height whose halving index in hayai is `index`.
fn first_height_with_halving(network: Network, index: u32) -> Option<u32> {
    let (mut low, mut high) = (0u32, u32::MAX / 2);
    if subsidy::halving(network, high) < index {
        return None;
    }
    while low < high {
        let middle = low + (high - low) / 2;
        if subsidy::halving(network, middle) < index {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    Some(low)
}

/// The halving index, the subsidy of the halving schedule and the scheduled issuance of
/// hayai equal those of `zakura-chain` on both sides of NU7.
#[test]
fn the_schedule_across_nu7_matches_zakura_chain() {
    for (network, baseline, nu7) in networks() {
        assert_eq!(network.activation_height(Upgrade::Nu7), Some(nu7));
        assert_eq!(
            NetworkUpgrade::Nu7.activation_height(&baseline),
            Some(Height(nu7))
        );
        let heights = heights(network, nu7);
        assert!(heights.len() > 500);
        for height in heights {
            let context = format!("{network:?} {height}");
            assert_eq!(
                subsidy::halving(network, height),
                zk_subsidy::halving(Height(height), &baseline),
                "{context}"
            );
            assert_eq!(
                halving_subsidy(network, height),
                zatoshis(zk_subsidy::halving_block_subsidy(Height(height), &baseline).unwrap()),
                "{context}"
            );
            assert_eq!(
                subsidy::scheduled_issuance(network, height),
                zk_subsidy::scheduled_issuance_zatoshis(Height(height), &baseline).unwrap(),
                "{context}"
            );
        }
        // The halving heights, with the third one after ZIP 218.
        for index in 1..=6 {
            assert_eq!(
                first_height_with_halving(network, index),
                zk_subsidy::height_for_halving(index, &baseline).map(|height| height.0),
                "{network:?} halving {index}"
            );
        }
    }
    assert_eq!(
        first_height_with_halving(Network::Testnet, 3),
        Some(TESTNET_NU7 + 3 * (4_476_000 - TESTNET_NU7))
    );
}

/// The funding streams of Testnet on both sides of NU7: the values, the recipients, and
/// the end of the last stream set at the third halving of ZIP 218.
#[test]
fn the_testnet_funding_streams_across_nu7_match_zakura_chain() {
    use zk_subsidy::{funding_stream_address_period, funding_stream_values, FundingStreamReceiver};
    let network = Network::Testnet;
    let baseline = ZkNetwork::new_default_testnet();
    let third_halving = TESTNET_NU7 + 3 * (4_476_000 - TESTNET_NU7);
    let mut heights: Vec<u32> = (TESTNET_NU7 - 50..TESTNET_NU7 + 50).collect();
    heights.extend(4_475_990..4_476_010);
    heights.extend(third_halving - 50..third_halving + 50);
    heights.extend((TESTNET_NU7..third_halving).step_by(131));
    let mut with_streams = 0;
    for height in heights {
        let total = halving_subsidy(network, height);
        let mut hayai: Vec<(u64, Option<Vec<u8>>)> = funding_streams(network, height, total)
            .into_iter()
            .map(|stream| {
                let script = stream.address.map(|address| {
                    address
                        .parse::<zk_chain::transparent::Address>()
                        .expect("an address")
                        .script()
                        .as_raw_bytes()
                        .to_vec()
                });
                (stream.value, script)
            })
            .collect();
        hayai.sort();
        let mut zakura = Vec::new();
        for (receiver, value) in
            funding_stream_values(Height(height), &baseline, amount(total)).unwrap()
        {
            if receiver == FundingStreamReceiver::Deferred {
                zakura.push((zatoshis(value), None));
                continue;
            }
            let set = baseline.funding_streams(Height(height)).unwrap();
            let index = funding_stream_address_period(Height(height), &baseline)
                - funding_stream_address_period(set.height_range().start, &baseline);
            let address = &set.recipient(receiver).unwrap().addresses()[index as usize];
            zakura.push((
                zatoshis(value),
                Some(address.script().as_raw_bytes().to_vec()),
            ));
        }
        zakura.sort();
        assert_eq!(hayai, zakura, "{height}");
        assert_eq!(hayai.len() == 2, height < third_halving, "{height}");
        with_streams += hayai.len() / 2;
    }
    assert!(with_streams > 300);
}

/// With the NU7 rule set: the coinbase terms of hayai equal the facts of `zakura-chain`
/// from the NU7 height to the reissuance height. Without it: hayai gives no terms.
#[test]
fn the_coinbase_terms_across_nu7_match_zakura_chain() {
    for (network, baseline, nu7) in networks() {
        let Some(_) = RuleSet::of(Upgrade::Nu7) else {
            let Err(_) = CoinbaseTerms::at(network, nu7) else {
                panic!("{network:?}: terms without the NU7 rule set");
            };
            continue;
        };
        let end = nsm::reissuance_height(network).unwrap_or(u32::MAX);
        // The heights before NU7 with a founders' reward are in `conformance_subsidy`.
        let start = nu7.saturating_sub(40).max(1);
        let heights = heights(network, nu7)
            .into_iter()
            .filter(|height| (start..end).contains(height));
        for height in heights {
            let terms = CoinbaseTerms::at(network, height).unwrap();
            let subsidy = zk_subsidy::block_subsidy(Height(height), &baseline, None).unwrap();
            assert_eq!(
                terms.subsidy.total,
                zatoshis(subsidy),
                "{network:?} {height}"
            );
            let mut deferred = 0;
            let mut paid = 0;
            for (receiver, value) in
                zk_subsidy::funding_stream_values(Height(height), &baseline, subsidy).unwrap()
            {
                match receiver {
                    zk_subsidy::FundingStreamReceiver::Deferred => deferred += zatoshis(value),
                    _ => paid += zatoshis(value),
                }
            }
            assert_eq!(terms.subsidy.deferred, deferred, "{network:?} {height}");
            let required: u64 = terms.required.iter().map(|output| output.value).sum();
            assert_eq!(required, paid, "{network:?} {height}");
            // The fee share of the miner follows the upgrade of the height.
            for fees in [0, 1, 9, 10, 1_001, 123_456_789] {
                assert_eq!(
                    terms.miner_fees(fees),
                    zatoshis(zk_subsidy::miner_fee_share(
                        Height(height),
                        &baseline,
                        amount(fees)
                    )),
                    "{network:?} {height} fees {fees}"
                );
            }
            assert_eq!(terms.nsm_fee_share, height >= nu7, "{network:?} {height}");
        }
    }
}

/// The NSM values: the fee share for random fees, the seed, the reissuance height and
/// the reissuance bonus.
#[test]
fn the_nsm_values_match_zakura_chain() {
    use zk_subsidy::ParameterSubsidy;
    let testnet = ZkNetwork::new_default_testnet();
    let mut rng = StdRng::seed_from_u64(7);
    for _ in 0..20_000 {
        let fees = match rng.gen_range(0..3) {
            0 => rng.gen_range(0..1_000),
            1 => rng.gen_range(0..100_000_000),
            _ => rng.gen_range(0..=MAX_MONEY),
        };
        // From NU7 the miner gets the share. Before NU7 the miner gets all the fees.
        assert_eq!(
            nsm::miner_fee_share(fees),
            zatoshis(zk_subsidy::miner_fee_share(
                Height(TESTNET_NU7),
                &testnet,
                amount(fees)
            )),
            "{fees}"
        );
        assert_eq!(
            zatoshis(zk_subsidy::miner_fee_share(
                Height(TESTNET_NU7 - 1),
                &testnet,
                amount(fees)
            )),
            fees
        );
        let balance = fees;
        assert_eq!(
            nsm::reissuance_bonus(balance),
            zatoshis(zk_subsidy::reissuance_bonus(amount(balance)).unwrap()),
            "{balance}"
        );
    }
    assert_eq!(
        nsm::expected_seed(Network::Testnet),
        Some(zatoshis(testnet.initial_nsm_value_balance()))
    );
    assert_eq!(
        nsm::expected_seed(Network::Mainnet),
        Some(zatoshis(ZkNetwork::Mainnet.initial_nsm_value_balance()))
    );

    // The reissuance height: one on Testnet, none on Mainnet and on Regtest.
    let (regtest, zk_regtest) = regtests();
    for (network, baseline) in [
        (Network::Mainnet, ZkNetwork::Mainnet),
        (Network::Testnet, testnet.clone()),
        (regtest, zk_regtest),
    ] {
        let start = nsm::reissuance_height(network);
        assert_eq!(
            start,
            zk_subsidy::nsm_reissuance_height(&baseline).map(|height| height.0),
            "{network:?}"
        );
        for height in start.into_iter().flat_map(|h| [h - 1, h, h + 1]) {
            assert_eq!(
                nsm::reissuance_active(network, height),
                zk_subsidy::is_zip234_active(&baseline, Height(height)),
                "{network:?} {height}"
            );
        }
    }
    assert_eq!(nsm::reissuance_height(Network::Testnet), Some(7_305_222));
}

/// With the NU7 rule set: from the reissuance height the subsidy of hayai, for the pools
/// that give an NSM value balance, equals `block_subsidy` of `zakura-chain` with that
/// balance.
#[test]
fn the_subsidy_with_the_reissuance_bonus_matches_zakura_chain() {
    let Some(_) = RuleSet::of(Upgrade::Nu7) else {
        return;
    };
    let network = Network::Testnet;
    let baseline = ZkNetwork::new_default_testnet();
    let start = nsm::reissuance_height(network).expect("Testnet has a reissuance height");
    let mut rng = StdRng::seed_from_u64(11);
    for height in [start, start + 1, start + 100_000, start + 2_000_000] {
        let scheduled = u64::try_from(subsidy::scheduled_issuance(network, height - 1))
            .expect("the Testnet schedule is below MAX_MONEY");
        for _ in 0..200 {
            let balance = match rng.gen_range(0..3) {
                0 => rng.gen_range(0..10),
                1 => rng.gen_range(0..100_000_000_000),
                _ => rng.gen_range(0..=scheduled),
            };
            let terms = CoinbaseTerms::after(network, height, scheduled - balance).unwrap();
            let expected =
                zk_subsidy::block_subsidy(Height(height), &baseline, Some(amount(balance)))
                    .unwrap();
            assert_eq!(
                terms.subsidy.total,
                zatoshis(expected),
                "{height} {balance}"
            );
        }
    }
    // The height before: no bonus in either implementation.
    let before = CoinbaseTerms::after(network, start - 1, 0).unwrap();
    assert_eq!(
        before.subsidy.total,
        zatoshis(zk_subsidy::block_subsidy(Height(start - 1), &baseline, None).unwrap())
    );
}

/// The block limits and the difficulty parameters of ZIP 218 equal the constants of
/// `zakura-chain`, and the rule set of each height has the parameters that `zakura-chain`
/// gives for that height.
#[test]
fn the_limits_and_the_difficulty_parameters_match_zakura_chain() {
    use zk_chain::parameters::{
        GLOBAL_SHIELDED_BUDGET, ORCHARD_PROTOCOL_BLOCK_ACTION_LIMIT, SAPLING_BLOCK_IO_LIMIT,
        SPROUT_BLOCK_JOINSPLIT_LIMIT,
    };
    let limits = BlockLimits::NU7;
    assert_eq!(limits.orchard_actions, ORCHARD_PROTOCOL_BLOCK_ACTION_LIMIT);
    assert_eq!(limits.ironwood_actions, ORCHARD_PROTOCOL_BLOCK_ACTION_LIMIT);
    assert_eq!(limits.sapling_ios, SAPLING_BLOCK_IO_LIMIT);
    assert_eq!(limits.shielded_cost, GLOBAL_SHIELDED_BUDGET);
    // hayai has no JoinSplit limit: the NU7 rule set has no Sprout pool.
    assert_eq!(SPROUT_BLOCK_JOINSPLIT_LIMIT, 0);

    let params = DifficultyParams::POST_NU7;
    let nu7 = NetworkUpgrade::Nu7;
    assert_eq!(
        i64::from(params.target_spacing),
        nu7.target_spacing().num_seconds()
    );
    assert_eq!(params.averaging_window as usize, nu7.averaging_window());
    assert_eq!(
        i64::from(params.averaging_window * params.target_spacing),
        nu7.averaging_window_timespan().num_seconds()
    );
    assert_eq!(
        hayai_consensus::DIFFICULTY_CONTEXT_BLOCKS,
        zk_header_chain::MAX_POW_ADJUSTMENT_BLOCK_SPAN
    );

    let testnet = ZkNetwork::new_default_testnet();
    for height in [
        TESTNET_NU7 - 1,
        TESTNET_NU7,
        TESTNET_NU7 + 1,
        TESTNET_NU7 + 1_000_000,
    ] {
        let Ok(rules) = rules_at(Network::Testnet, height) else {
            // A build without the NU7 rule set has no parameters from the NU7 height.
            assert!(height >= TESTNET_NU7 && hayai_crypto::BACKEND == "upstream");
            continue;
        };
        let zk_height = Height(height);
        assert_eq!(rules.limits == limits, height >= TESTNET_NU7);
        assert_eq!(
            NetworkUpgrade::is_nu7_active(&testnet, zk_height),
            height >= TESTNET_NU7
        );
        let d = rules.difficulty;
        assert_eq!(
            i64::from(d.target_spacing),
            NetworkUpgrade::target_spacing_for_height(&testnet, zk_height).num_seconds()
        );
        assert_eq!(
            d.averaging_window as usize,
            NetworkUpgrade::averaging_window_for_height(&testnet, zk_height)
        );
        let gap = NetworkUpgrade::minimum_difficulty_spacing_for_height(&testnet, zk_height)
            .expect("Testnet has the minimum-difficulty rule at these heights");
        assert_eq!(
            i64::from(d.min_difficulty_gap_spacings * d.target_spacing),
            gap.num_seconds()
        );
    }
}

/// The `nBits` that Zakura expects for a block at `height` with time `time` after the
/// blocks `(time, nBits)` of `chain`, newest last.
fn zakura_expected_bits(height: u32, time: u32, chain: &[(u32, u32)]) -> u32 {
    let context = chain.iter().rev().map(|(time, bits)| {
        (
            CompactDifficulty::from_bytes_in_display_order(&bits.to_be_bytes())
                .expect("valid nBits"),
            DateTime::from_timestamp(i64::from(*time), 0).expect("a time"),
        )
    });
    let adjusted = AdjustedDifficulty::new_from_header_time(
        DateTime::from_timestamp(i64::from(time), 0).expect("a time"),
        Height(height - 1),
        &ZkNetwork::new_default_testnet(),
        context,
    )
    .expect("a context of the right length");
    u32::from_be_bytes(
        adjusted
            .expected_difficulty_threshold()
            .bytes_in_display_order(),
    )
}

/// Generated Testnet chains from 150 blocks before NU7 to 350 blocks after it. Each block
/// takes the `nBits` of hayai, after the comparison with Zakura's `AdjustedDifficulty`.
/// The block times are on target, fast, slow, and mixed with gaps around 450 s (the
/// minimum-difficulty rule) and steps back in time.
#[test]
fn the_expected_bits_across_nu7_match_zakura_header_chain() {
    let Some(_) = RuleSet::of(Upgrade::Nu7) else {
        return;
    };
    let mut distinct = std::collections::BTreeSet::new();
    for (case, start_bits) in [0x1e3f_ffffu32, 0x1d00_ffff, 0x2007_ffff]
        .into_iter()
        .enumerate()
    {
        for pace in 0..4 {
            let mut rng = StdRng::seed_from_u64((case * 10 + pace) as u64);
            let base = TESTNET_NU7 - 150 - 113;
            let mut time = 1_800_000_000u32;
            let mut chain: Vec<(u32, u32)> = Vec::new();
            for _ in 0..113 {
                time += 75;
                let mantissa = start_bits & 0x007f_ffff;
                let jitter = rng.gen_range(0..=mantissa / 4);
                chain.push((time, start_bits & 0xff00_0000 | (mantissa - jitter)));
            }
            for _ in 0..500 {
                let height = base + chain.len() as u32;
                let spacing: i64 = if height >= TESTNET_NU7 { 25 } else { 75 };
                let delta = match pace {
                    0 => rng.gen_range(spacing / 2..=spacing * 3 / 2),
                    1 => rng.gen_range(1..=5),
                    2 => rng.gen_range(spacing * 2..=spacing * 5),
                    _ => match rng.gen_range(0..10) {
                        0 => -rng.gen_range(1..=300),
                        1 => rng.gen_range(448..=452),
                        2 => rng.gen_range(450..=3_000),
                        3 => 0,
                        _ => rng.gen_range(1..=spacing * 3),
                    },
                };
                let parent = chain.last().expect("a context").0;
                let time = u32::try_from(i64::from(parent) + delta).expect("a time");
                let newest: Vec<(u32, u32)> = chain.iter().rev().take(113).copied().collect();
                let times: Vec<u32> = newest.iter().map(|(time, _)| *time).collect();
                let bits: Vec<u32> = newest.iter().map(|(_, bits)| *bits).collect();
                let hayai = expected_bits(
                    Network::Testnet,
                    time,
                    &ParentChain {
                        height,
                        times: &times,
                        bits: &bits,
                    },
                )
                .unwrap_or_else(|e| panic!("height {height}: {e}"));
                let tail = &chain[chain.len() - 113..];
                assert_eq!(
                    hayai,
                    zakura_expected_bits(height, time, tail),
                    "height {height} time {time} (case {case}, pace {pace})"
                );
                distinct.insert(hayai);
                chain.push((time, hayai));
            }
        }
    }
    assert!(distinct.len() > 1_000, "the chains exercise many targets");
}
