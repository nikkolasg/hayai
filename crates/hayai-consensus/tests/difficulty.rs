//! The difficulty rule against a reference implementation, against Zakura's vector, at its
//! boundaries, and on the published block vectors.
//!
//! The reference implementation in this file follows the formulas of the protocol
//! specification §7.7.3 with big integers and with blocks indexed by height. It shares no
//! code with `hayai_consensus::difficulty`: it has its own compact encoding, its own
//! constants and its own median.

use hayai_consensus::difficulty::{expected_bits, DifficultyError};
use hayai_consensus::{ConsensusError, ContextTooShort, Network, ParentChain, RuleSet, Upgrade};
use hayai_wire::header::BlockHeader;
use num_bigint::BigUint;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

// ----- the reference implementation -----

/// What the reference knows about a network: constants copied from the specification and
/// the ZIPs, not from hayai-consensus.
struct RefNet {
    /// Exponent `e` of the limit `2^e - 1`.
    limit_bits: u32,
    blossom: u32,
    /// First height of ZIP 218: 25 s, a window of 102 blocks.
    nu7: Option<u32>,
    /// First height of the Testnet minimum-difficulty rule.
    min_difficulty: Option<u32>,
}

impl RefNet {
    /// The target spacing in seconds, the averaging window in blocks and the gap of the
    /// minimum-difficulty rule in seconds, at `height`.
    fn era(&self, height: u32) -> (i64, u32, i64) {
        match self.nu7 {
            Some(nu7) if height >= nu7 => (25, 102, 450),
            _ if height >= self.blossom => (75, 17, 450),
            _ => (150, 17, 900),
        }
    }
}

const REF_MAINNET: RefNet = RefNet {
    limit_bits: 243,
    blossom: 653_600,
    nu7: None,
    min_difficulty: None,
};
const REF_TESTNET: RefNet = RefNet {
    limit_bits: 251,
    blossom: 584_000,
    nu7: Some(4_465_026),
    min_difficulty: Some(299_188),
};

fn ref_net(network: Network) -> &'static RefNet {
    match network {
        Network::Mainnet => &REF_MAINNET,
        Network::Testnet => &REF_TESTNET,
        Network::Regtest | Network::Custom(_) => {
            panic!("Regtest has no difficulty rule")
        }
    }
}

/// `ToTarget`: mantissa times `256^(size - 3)`. The reference decodes valid values with a
/// size of 3 or more only.
fn ref_target(bits: u32) -> BigUint {
    let size = bits >> 24;
    assert!(size >= 3 && bits & 0x0080_0000 == 0);
    BigUint::from(bits & 0x007f_ffff) << (8 * (size - 3))
}

/// `ToCompact`: the three most significant bytes and the byte length, with the shift that
/// keeps the sign bit clear.
fn ref_compact(target: &BigUint) -> u32 {
    let bytes = target.to_bytes_be();
    let mut size = bytes.len() as u32;
    let mut mantissa = 0u32;
    for i in 0..3 {
        mantissa = mantissa << 8 | u32::from(bytes.get(i).copied().unwrap_or(0));
    }
    if mantissa >= 0x0080_0000 {
        mantissa >>= 8;
        size += 1;
    }
    size << 24 | mantissa
}

/// A chain as the reference reads it: `(nTime, nBits)` of the blocks from height `base`.
struct RefChain {
    base: u32,
    blocks: Vec<(u32, u32)>,
}

impl RefChain {
    fn next_height(&self) -> u32 {
        self.base + self.blocks.len() as u32
    }

    fn block(&self, height: u32) -> (u32, u32) {
        self.blocks[(height - self.base) as usize]
    }

    /// `MedianTime(height)`: the median of the times of the heights
    /// `max(0, height - 11) ..= height - 1`.
    fn median_time(&self, height: u32) -> i64 {
        let mut times: Vec<u32> = (height.saturating_sub(11)..height)
            .map(|h| self.block(h).0)
            .collect();
        times.sort();
        i64::from(times[times.len() / 2])
    }

    /// `ThresholdBits(height)` with the Testnet minimum-difficulty rule, for the next
    /// height of the chain and a block time `time`.
    fn expected(&self, network: Network, time: u32) -> u32 {
        let net = ref_net(network);
        let height = self.next_height();
        let limit = (BigUint::from(1u8) << net.limit_bits) - 1u8;
        let (spacing, blocks, gap) = net.era(height);
        if let Some(start) = net.min_difficulty {
            let parent_time = i64::from(self.block(height - 1).0);
            if height >= start && i64::from(time) > parent_time + gap {
                return ref_compact(&limit);
            }
        }
        if height <= blocks {
            return ref_compact(&limit);
        }
        let sum: BigUint = (height - blocks..height)
            .map(|h| ref_target(self.block(h).1))
            .sum();
        let mean = sum / blocks;
        let window = i64::from(blocks) * spacing;
        let actual = self.median_time(height) - self.median_time(height - blocks);
        // `trunc`: toward zero.
        let difference = actual - window;
        let quarter = difference.abs() / 4 * difference.signum();
        let damped = window + quarter;
        let min = window * 84 / 100;
        let max = window * 132 / 100;
        let bounded = damped.max(min).min(max);
        let threshold = mean / BigUint::from(window as u64) * BigUint::from(bounded as u64);
        ref_compact(&threshold.min(limit))
    }

    /// The context of the next height as hayai reads it: newest first, at most `count`.
    fn context(&self, count: usize) -> (Vec<u32>, Vec<u32>) {
        let newest = self.blocks.iter().rev().take(count);
        (
            newest.clone().map(|(time, _)| *time).collect(),
            newest.map(|(_, bits)| *bits).collect(),
        )
    }
}

fn hayai_expected(network: Network, chain: &RefChain, time: u32) -> Result<u32, DifficultyError> {
    let (times, bits) = chain.context(113);
    expected_bits(
        network,
        time,
        &ParentChain {
            height: chain.next_height(),
            times: &times,
            bits: &bits,
        },
    )
}

// ----- generated chains -----

/// How the block times of a generated chain move.
#[derive(Clone, Copy, Debug)]
enum Pace {
    /// Around the target spacing.
    OnTarget,
    /// A few seconds per block: the upward clamp.
    Fast,
    /// Many spacings per block: the downward clamp and the limit.
    Slow,
    /// Every kind of gap, also backwards in time and above 6 spacings.
    Mixed,
}

fn next_time(rng: &mut StdRng, pace: Pace, parent: u32, spacing: i64) -> u32 {
    let delta: i64 = match pace {
        Pace::OnTarget => rng.gen_range(spacing / 2..=spacing * 3 / 2),
        Pace::Fast => rng.gen_range(1..=10),
        Pace::Slow => rng.gen_range(spacing * 2..=spacing * 5),
        Pace::Mixed => match rng.gen_range(0..10) {
            0 => -rng.gen_range(1..=600),
            1 => rng.gen_range(spacing * 6 - 2..=spacing * 6 + 2),
            2 => rng.gen_range(spacing * 6..=spacing * 40),
            3 => 0,
            _ => rng.gen_range(1..=spacing * 3),
        },
    };
    u32::try_from(i64::from(parent) + delta).expect("the test times stay in range")
}

/// Extends a chain by `blocks` blocks. Each block takes the `nBits` that hayai computes,
/// after the comparison with the reference. Returns the distinct `nBits` values seen.
fn run_chain(
    network: Network,
    base: u32,
    start_bits: u32,
    pace: Pace,
    blocks: u32,
    seed: u64,
) -> Vec<u32> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut chain = RefChain {
        base,
        blocks: Vec::new(),
    };
    // 113 blocks of context at the start, with targets around `start_bits`.
    let mut time = 1_500_000_000u32;
    for _ in 0..113 {
        time += 75;
        let mantissa = (start_bits & 0x007f_ffff).max(0x1_0000);
        let jitter = rng.gen_range(0..=mantissa / 4);
        chain
            .blocks
            .push((time, start_bits & 0xff00_0000 | (mantissa - jitter)));
    }
    let mut seen = Vec::new();
    for _ in 0..blocks {
        let height = chain.next_height();
        let (spacing, _, _) = ref_net(network).era(height);
        let parent = chain.blocks.last().expect("the chain has a context").0;
        let time = next_time(&mut rng, pace, parent, spacing);
        let reference = chain.expected(network, time);
        assert_eq!(
            hayai_expected(network, &chain, time),
            Ok(reference),
            "{network:?} height {height} time {time} ({pace:?}, seed {seed})"
        );
        if !seen.contains(&reference) {
            seen.push(reference);
        }
        chain.blocks.push((time, reference));
    }
    seen
}

/// Generated Testnet chains that start 200 blocks before NU7 and end 300 blocks after it,
/// against the reference: the window of 102 blocks and the spacing of 25 s start at the
/// NU7 height, with a window that still holds blocks of the old spacing. Without the NU7
/// rule set the function gives an error from the NU7 height.
#[test]
fn generated_chains_across_nu7_match_the_reference() {
    let nu7 = Network::Testnet
        .activation_height(Upgrade::Nu7)
        .expect("Testnet has an NU7 height");
    assert_eq!(Some(nu7), REF_TESTNET.nu7);
    let Some(_) = RuleSet::of(Upgrade::Nu7) else {
        let context = spaced(1_600_000_000, 75, 75, 113, 0x1e10_0000);
        for height in [nu7, nu7 + 1] {
            let chain = ParentChain {
                height,
                times: &context.0,
                bits: &context.1,
            };
            assert_eq!(
                expected_bits(Network::Testnet, 1_600_000_000, &chain),
                Err(DifficultyError::Rules(ConsensusError::UnsupportedUpgrade {
                    upgrade: Upgrade::Nu7,
                    height,
                }))
            );
        }
        return;
    };
    let mut distinct = 0;
    for (p, pace) in [Pace::OnTarget, Pace::Fast, Pace::Slow, Pace::Mixed]
        .into_iter()
        .enumerate()
    {
        for (b, bits) in [0x1e3f_ffff, 0x1d00_ffff, 0x2007_ffff]
            .into_iter()
            .enumerate()
        {
            let seed = 1_000 + (b * 10 + p) as u64;
            distinct += run_chain(Network::Testnet, nu7 - 313, bits, pace, 500, seed).len();
        }
    }
    assert!(distinct > 1_000, "the chains exercise many targets");
}

#[test]
fn generated_chains_match_the_reference() {
    let mainnet_limit = 0x1f07_ffff;
    let testnet_limit = 0x2007_ffff;
    let cases: [(Network, u32, u32, u32); 9] = [
        // Before Blossom, across the Blossom activation, and long after it.
        (Network::Mainnet, 1_000, 0x1d12_3456, 400),
        (Network::Mainnet, 653_500, 0x1c2a_1b3c, 400),
        (Network::Mainnet, 3_000_000, 0x1b01_2345, 400),
        // At the limit: the cap applies when the blocks are slow.
        (Network::Mainnet, 2_000_000, mainnet_limit, 400),
        // Testnet: before and across the start of the minimum-difficulty rule, across
        // Blossom, after the start of the maximum-time rule, and at the limit.
        (Network::Testnet, 200_000, 0x1e3f_ffff, 400),
        (Network::Testnet, 299_100, 0x1f01_2345, 400),
        (Network::Testnet, 583_900, 0x1e7f_ff00, 400),
        (Network::Testnet, 2_500_000, 0x1d00_ffff, 400),
        (Network::Testnet, 1_000_000, testnet_limit, 400),
    ];
    let mut distinct = 0;
    for (case, (network, base, bits, blocks)) in cases.into_iter().enumerate() {
        for (p, pace) in [Pace::OnTarget, Pace::Fast, Pace::Slow, Pace::Mixed]
            .into_iter()
            .enumerate()
        {
            let seed = (case * 10 + p) as u64;
            let seen = run_chain(network, base, bits, pace, blocks, seed);
            distinct += seen.len();
            let limit = match network {
                Network::Mainnet => mainnet_limit,
                _ => testnet_limit,
            };
            match pace {
                // Slow blocks reach the limit and stay there.
                Pace::Slow if bits == limit => assert!(seen.contains(&limit)),
                // Fast blocks never make the target easier.
                Pace::Fast => assert!(seen.len() > 100, "the target moves at every block"),
                _ => {}
            }
        }
    }
    assert!(distinct > 5_000, "the chains exercise many targets");
}

/// A chain from the genesis block: the limit up to height 17, then windows whose older
/// median reads fewer than 11 times (heights 18 to 27).
#[test]
fn a_chain_from_genesis_matches_the_reference() {
    for (network, limit) in [
        (Network::Mainnet, 0x1f07_ffffu32),
        (Network::Testnet, 0x2007_ffff),
    ] {
        for (seed, pace) in [Pace::OnTarget, Pace::Fast, Pace::Slow]
            .into_iter()
            .enumerate()
        {
            let mut rng = StdRng::seed_from_u64(seed as u64);
            let mut chain = RefChain {
                base: 0,
                blocks: vec![(1_477_641_360, limit)],
            };
            for height in 1..=80u32 {
                let parent = chain.blocks[height as usize - 1].0;
                let time = next_time(&mut rng, pace, parent, 150);
                let reference = chain.expected(network, time);
                assert_eq!(
                    hayai_expected(network, &chain, time),
                    Ok(reference),
                    "{network:?} height {height} ({pace:?})"
                );
                if height <= 17 {
                    assert_eq!(reference, limit, "height {height} uses the limit");
                }
                chain.blocks.push((time, reference));
            }
            if let Pace::Fast = pace {
                let (_, bits) = chain.blocks[18];
                assert_ne!(bits, limit, "height 18 is the first adjusted height");
            }
        }
    }
}

// ----- vectors and boundaries -----

/// A context of `len` blocks, one `spacing` apart, that end `gap` seconds before `time`.
fn spaced(time: u32, gap: u32, spacing: u32, len: u32, bits: u32) -> (Vec<u32>, Vec<u32>) {
    let times = (0..len).map(|i| time - gap - i * spacing).collect();
    (times, vec![bits; len as usize])
}

fn expected(network: Network, height: u32, time: u32, context: &(Vec<u32>, Vec<u32>)) -> u32 {
    let chain = ParentChain {
        height,
        times: &context.0,
        bits: &context.1,
    };
    match expected_bits(network, time, &chain) {
        Ok(bits) => bits,
        Err(e) => panic!("{network:?} height {height}: {e}"),
    }
}

/// Zakura's vector (`zakura-header-chain/src/validation/contextual/tests/validation.rs`,
/// `candidate_height_window_preserves_existing_targets`): nine targets of `2^216` and
/// eight of `2^217`, blocks exactly one spacing apart, give `0x1c017878` at every height
/// above the window, before and after Blossom.
#[test]
fn zakura_vector_of_a_mixed_window() {
    let time = 2_000_000_000u32;
    for network in [Network::Mainnet, Network::Testnet] {
        let blossom = network.activation_height(Upgrade::Blossom).unwrap();
        for height in [99, 100, 101, 199, 200, 201, blossom - 1, blossom, 700_000] {
            let spacing = if height >= blossom { 75 } else { 150 };
            let len = height.min(28);
            let (times, mut bits) = spaced(time, spacing, spacing, len, 0x1c02_0000);
            for (index, target) in bits.iter_mut().enumerate().take(17) {
                if index % 2 == 0 {
                    *target = 0x1c01_0000;
                }
            }
            assert_eq!(
                expected(network, height, time, &(times, bits)),
                0x1c01_7878,
                "{network:?} at {height}"
            );
        }
    }
}

/// The same context gives another result on each side of the Blossom activation, because
/// the target spacing changes from 150 s to 75 s at the activation height.
#[test]
fn the_target_spacing_changes_at_blossom() {
    let time = 1_600_000_000u32;
    let bits = 0x1c10_0000;
    for network in [Network::Mainnet, Network::Testnet] {
        let blossom = network.activation_height(Upgrade::Blossom).unwrap();
        // Blocks 150 s apart: on target before Blossom, slow after it.
        let context = spaced(time, 150, 150, 28, bits);
        // The division before the multiplication loses the low bits of the mean, so the
        // unchanged target is one unit of the mantissa below the mean.
        assert_eq!(expected(network, blossom - 1, time, &context), 0x1c0f_ffff);
        // After Blossom: actual 2550 against 1275, damped 1275 + 1275 / 4 = 1593.
        let after = expected(network, blossom, time, &context);
        let target = (BigUint::from(0x10_0000u32) << 200) / 1275u32 * 1593u32;
        assert_eq!(after, ref_compact(&target));
        assert!(after > 0x1c13_0000 && after < 0x1c14_0000, "{after:#x}");
        // Blocks 75 s apart: fast before Blossom (damped 2550 - 1275 / 4 = 2232), on target
        // after it.
        let context = spaced(time, 75, 75, 28, bits);
        let before = expected(network, blossom - 1, time, &context);
        let target = (BigUint::from(0x10_0000u32) << 200) / 2550u32 * 2232u32;
        assert_eq!(before, ref_compact(&target));
        assert!(before > 0x1c0e_0000 && before < 0x1c0e_1000, "{before:#x}");
        assert_eq!(expected(network, blossom, time, &context), 0x1c0f_ffff);
    }
}

/// ZIP 218 at the Testnet NU7 height: the averaging window is 102 blocks and the target
/// spacing is 25 s from the NU7 height, and the minimum-difficulty gap stays 450 s (18
/// spacings). The block before NU7 has the rule of the old spacing.
#[test]
fn the_window_and_the_spacing_change_at_nu7() {
    let Some(_) = RuleSet::of(Upgrade::Nu7) else {
        return;
    };
    let nu7 = Network::Testnet
        .activation_height(Upgrade::Nu7)
        .expect("Testnet has an NU7 height");
    let time = 1_800_000_000u32;
    let bits = 0x1c10_0000;
    let limit = 0x2007_ffff;
    let mean: BigUint = BigUint::from(0x10_0000u32) << 200u32;
    // Blocks 75 s apart: on target before NU7.
    let old = spaced(time, 75, 75, 113, bits);
    assert_eq!(expected(Network::Testnet, nu7 - 1, time, &old), 0x1c0f_ffff);
    // From NU7: actual 102 * 75 = 7,650 against 2,550, damped 2,550 + 5,100 / 4 = 3,825,
    // bounded to 132 % of 2,550 = 3,366.
    let slow = ref_compact(&(mean.clone() / 2550u32 * 3366u32));
    for height in [nu7, nu7 + 1] {
        assert_eq!(expected(Network::Testnet, height, time, &old), slow);
    }
    // Blocks 25 s apart: on target from NU7, and fast before it (actual 17 * 25 = 425
    // against 1,275, damped 1,275 - 850 / 4 = 1,063, bounded to 84 % of 1,275 = 1,071).
    let new = spaced(time, 25, 25, 113, bits);
    for height in [nu7, nu7 + 1] {
        assert_eq!(expected(Network::Testnet, height, time, &new), 0x1c0f_ffff);
    }
    let fast = ref_compact(&(mean / 1275u32 * 1071u32));
    assert_eq!(expected(Network::Testnet, nu7 - 1, time, &new), fast);
    // The minimum-difficulty gap: more than 450 s on both sides of the boundary. A gap of
    // 6 new spacings is not enough from NU7.
    for height in [nu7 - 1, nu7, nu7 + 1] {
        let with_gap = |gap| {
            expected(
                Network::Testnet,
                height,
                time,
                &spaced(time, gap, 25, 113, bits),
            )
        };
        assert_ne!(with_gap(450), limit, "{height}");
        assert_eq!(with_gap(451), limit, "{height}");
        assert_ne!(with_gap(6 * 25 + 1), limit, "{height}");
    }
    // The rule reads 113 times and 102 nBits from NU7, and 28 and 17 before it.
    let run = |height: u32, times: usize, count: usize| {
        expected_bits(
            Network::Testnet,
            time,
            &ParentChain {
                height,
                times: &new.0[..times],
                bits: &new.1[..count],
            },
        )
    };
    assert_eq!(run(nu7 - 1, 28, 17), Ok(fast));
    assert_eq!(run(nu7, 113, 102), Ok(0x1c0f_ffff));
    for (times, count) in [(28, 17), (112, 102), (113, 101)] {
        assert_eq!(
            run(nu7, times, count),
            Err(DifficultyError::ContextTooShort(ContextTooShort {
                times,
                needed_times: 113,
                bits: count,
                needed_bits: 102,
            }))
        );
    }
}

/// `PoWMaxAdjustUp` (16 %) and `PoWMaxAdjustDown` (32 %): the first timespans that reach
/// each bound, and the timespans one step inside it.
#[test]
fn the_clamp_limits() {
    let network = Network::Mainnet;
    let height = 700_000;
    let time = 1_600_000_000u32;
    let bits = 0x1c10_0000;
    let mean: BigUint = BigUint::from(0x10_0000u32) << 200u32;
    let result =
        |spacing: u32| expected(network, height, time, &spaced(time, 75, spacing, 28, bits));
    let scaled = |timespan: u32| ref_compact(&(&mean / 1275u32 * timespan));
    // Blocks `s` apart: the two medians are 17 * s apart. Window timespan 1275.
    // s = 1: actual 17, damped 1275 - 1258 / 4 = 961, bounded to 1071.
    assert_eq!(result(1), scaled(1071));
    // s = 27: actual 459, damped 1275 - 204 = 1071: the bound exactly.
    assert_eq!(result(27), scaled(1071));
    // s = 28: actual 476, damped 1275 - 199 = 1076: inside the bound.
    assert_eq!(result(28), scaled(1076));
    assert_ne!(scaled(1071), scaled(1076));
    // s = 171: actual 2907, damped 1275 + 408 = 1683: the bound exactly.
    assert_eq!(result(171), scaled(1683));
    // s = 170: actual 2890, damped 1275 + 403 = 1678: inside the bound.
    assert_eq!(result(170), scaled(1678));
    // s = 10,000: bounded to 1683.
    assert_eq!(result(10_000), scaled(1683));
    assert_ne!(scaled(1678), scaled(1683));
}

/// A target that the adjustment moves above the proof-of-work limit is the limit.
#[test]
fn the_result_is_at_most_the_pow_limit() {
    let time = 1_600_000_000u32;
    for (network, limit) in [
        (Network::Mainnet, 0x1f07_ffffu32),
        (Network::Testnet, 0x2007_ffff),
    ] {
        // Slow blocks at the limit: 1.32 times the limit is capped.
        let context = spaced(time, 75, 10_000, 28, limit);
        assert_eq!(expected(network, 250_000, time, &context), limit);
        // Slow blocks just below the limit are capped too.
        let context = spaced(time, 75, 10_000, 28, limit - 0x1_0000);
        assert_eq!(expected(network, 250_000, time, &context), limit);
        // On-target blocks at the limit: the division loses the low bits, so the result is
        // just below the limit.
        let context = spaced(time, 150, 150, 28, limit);
        let below = expected(network, 250_000, time, &context);
        assert!(below < limit && below > limit - 0x100, "{below:#x}");
        // Up to the averaging window the result is the limit, whatever the context.
        for height in 1..=17 {
            let context = spaced(time, 1, 1, height, 0x1c10_0000);
            assert_eq!(expected(network, height, time, &context), limit);
        }
        let context = spaced(time, 1, 1, 18, 0x1c10_0000);
        assert_ne!(expected(network, 18, time, &context), limit);
    }
}

/// ZIP 205 and ZIP 208: from Testnet height 299,188 a block more than 6 target spacings
/// after its parent has the limit. The gap is strict. Mainnet has no such rule.
#[test]
fn testnet_minimum_difficulty_at_the_gap_boundary() {
    let time = 1_600_000_000u32;
    let bits = 0x1e10_0000;
    let limit = 0x2007_ffff;
    let blossom = Network::Testnet
        .activation_height(Upgrade::Blossom)
        .unwrap();
    assert_eq!(blossom, 584_000);
    for (height, spacing, active) in [
        (299_187, 150, false),
        (299_188, 150, true),
        (blossom - 1, 150, true),
        (blossom, 75, true),
        (2_000_000, 75, true),
    ] {
        let with_gap = |gap: u32| {
            expected(
                Network::Testnet,
                height,
                time,
                &spaced(time, gap, spacing, 28, bits),
            )
        };
        let at_the_gap = with_gap(6 * spacing);
        let above_the_gap = with_gap(6 * spacing + 1);
        assert_ne!(at_the_gap, limit, "height {height}: the gap is strict");
        assert_eq!(above_the_gap == limit, active, "height {height}");
        // On Mainnet the same context never gives the limit.
        let mainnet = expected(
            Network::Mainnet,
            height,
            time,
            &spaced(time, 6 * spacing + 1, spacing, 28, 0x1c10_0000),
        );
        assert_ne!(mainnet, 0x1f07_ffff);
    }
    // The rule needs the parent time only: a short context is enough when it applies.
    let short = (vec![time - 901], vec![]);
    assert_eq!(expected(Network::Testnet, 299_188, time, &short), limit);
    // A block time before the parent time is a negative gap, not a large one.
    let context = spaced(time, 75, 75, 28, bits);
    let chain = ParentChain {
        height: 2_000_000,
        times: &context.0,
        bits: &context.1,
    };
    let earlier = expected_bits(Network::Testnet, context.0[0] - 5_000, &chain);
    assert!(matches!(earlier, Ok(b) if b != limit));
}

/// A context shorter than the rule reads is an error with the lengths, never a value.
#[test]
fn a_short_context_is_an_error() {
    let time = 1_600_000_000u32;
    let (times, bits) = spaced(time, 75, 75, 28, 0x1c10_0000);
    let run = |height: u32, times: &[u32], bits: &[u32]| {
        expected_bits(
            Network::Mainnet,
            time,
            &ParentChain {
                height,
                times,
                bits,
            },
        )
    };
    assert_eq!(
        run(700_000, &times, &bits[..17]),
        run(700_000, &times, &bits)
    );
    let Ok(_) = run(700_000, &times, &bits[..17]) else {
        panic!("28 times and 17 bits are the whole context");
    };
    for (t, b) in [(27, 28), (28, 16), (0, 0), (11, 0), (17, 17)] {
        assert_eq!(
            run(700_000, &times[..t], &bits[..b]),
            Err(DifficultyError::ContextTooShort(ContextTooShort {
                times: t,
                needed_times: 28,
                bits: b,
                needed_bits: 17,
            })),
            "{t} times, {b} bits"
        );
    }
    // Height 20 has 20 blocks before it: the rule reads 20 times.
    let Ok(_) = run(20, &times[..20], &bits[..17]) else {
        panic!("height 20 reads 20 times");
    };
    assert_eq!(
        run(20, &times[..19], &bits[..17]),
        Err(DifficultyError::ContextTooShort(ContextTooShort {
            times: 19,
            needed_times: 20,
            bits: 17,
            needed_bits: 17,
        }))
    );
    // Up to height 17 the rule reads no context.
    assert_eq!(run(17, &[], &[]), Ok(0x1f07_ffff));
    assert_eq!(run(0, &[], &[]), Err(DifficultyError::Genesis));
    // The Testnet rule reads the parent time first.
    let testnet = expected_bits(
        Network::Testnet,
        time,
        &ParentChain {
            height: 300_000,
            times: &[],
            bits: &[],
        },
    );
    assert!(matches!(testnet, Err(DifficultyError::ContextTooShort(_))));
    // A context value that encodes no target is an error.
    let mut invalid = bits.clone();
    invalid[3] = 0x1c80_0001;
    assert_eq!(
        run(700_000, &times, &invalid),
        Err(DifficultyError::InvalidContextBits(0x1c80_0001))
    );
}

// ----- published block vectors -----

fn vector_header(network: &str, height: u32) -> BlockHeader {
    let name = format!(
        "{}/../hayai-bench/tests/vectors/block-{network}-{}-{:03}-{:03}.hex",
        env!("CARGO_MANIFEST_DIR"),
        height / 1_000_000,
        height / 1_000 % 1_000,
        height % 1_000
    );
    let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let bytes = hex::decode(hex.trim()).expect("hex");
    BlockHeader::parse(&bytes).expect("a header")
}

/// The first blocks of Mainnet and Testnet (Zebra's vectors): every block up to height 17
/// has the limit, and the rule gives it from the real context.
#[test]
fn the_first_blocks_of_both_networks_have_the_limit() {
    for (network, name, last) in [
        (Network::Mainnet, "main", 10),
        (Network::Testnet, "test", 9),
    ] {
        let headers: Vec<BlockHeader> = (0..=last).map(|h| vector_header(name, h)).collect();
        for height in 1..=last {
            let before = &headers[..height as usize];
            let times: Vec<u32> = before.iter().rev().map(|h| h.time).collect();
            let bits: Vec<u32> = before.iter().rev().map(|h| h.bits).collect();
            let header = &headers[height as usize];
            assert_eq!(header.prev_hash, before[before.len() - 1].hash());
            assert_eq!(
                expected(network, height, header.time, &(times, bits)),
                header.bits,
                "{network:?} {height}"
            );
            assert_eq!(header.bits, network.params().pow_limit_bits);
        }
    }
}

/// The Testnet minimum-difficulty blocks of Zebra's vectors (Zakura
/// `zakura-chain/src/work/difficulty/tests/vectors.rs`, `MINIMUM_DIFFICULTY_HEIGHTS`): the
/// rule gives their `nBits` from the parent time alone. The blocks around them that are
/// not minimum-difficulty blocks need the full context.
#[test]
fn testnet_minimum_difficulty_blocks_of_the_vectors() {
    let limit = Network::Testnet.params().pow_limit_bits;
    for height in [
        299_188, 299_189, 299_202, 584_000, 903_800, 903_801, 1_028_500,
    ] {
        let parent = vector_header("test", height - 1);
        let header = vector_header("test", height);
        assert_eq!(header.prev_hash, parent.hash());
        assert_eq!(header.bits, limit, "{height}");
        let context = (vec![parent.time], vec![parent.bits]);
        assert_eq!(
            expected(Network::Testnet, height, header.time, &context),
            limit,
            "{height}"
        );
    }
    // Blocks with a parent in the set and a gap of at most 6 spacings.
    for height in [280_000, 280_001, 584_001, 1_028_501, 1_116_000, 1_116_001] {
        let parent = vector_header("test", height - 1);
        let header = vector_header("test", height);
        assert_ne!(header.bits, limit, "{height}");
        let chain = ParentChain {
            height,
            times: &[parent.time],
            bits: &[parent.bits],
        };
        assert!(matches!(
            expected_bits(Network::Testnet, header.time, &chain),
            Err(DifficultyError::ContextTooShort(_))
        ));
    }
    // Height 299,187 is the last height without the rule. Its gap is above 6 spacings and
    // its target is not the limit.
    let parent_gap = {
        let header = vector_header("test", 299_188);
        let parent = vector_header("test", 299_187);
        assert_ne!(parent.bits, limit);
        header.time - parent.time
    };
    assert!(parent_gap > 900);
}
