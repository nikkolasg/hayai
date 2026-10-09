//! Difficulty adjustment (protocol specification §7.7): the wrappers of
//! `hayai_consensus_core::difficulty_rules` for a caller with a [`Network`] or with the
//! `primitive_types::U256` of the ZIP 221 history tree. The rules are in the core.

use hayai_consensus_core::difficulty_rules as core;
pub use hayai_consensus_core::difficulty_rules::{
    median_time, median_time_past, DifficultyError, Uint256,
};
use hayai_crypto::primitive_types::U256;

use crate::network::checked;
use crate::rules::core_rules_at;
use crate::{Network, ParentChain};

/// The `U256` of the history tree with the value of `value`. Both types hold 4
/// little-endian 64-bit limbs.
pub fn u256(value: Uint256) -> U256 {
    U256(value.0)
}

/// The [`Uint256`] of the core with the value of `value`.
pub fn uint256(value: U256) -> Uint256 {
    Uint256(value.0)
}

/// The target that `bits` encodes. `None` when `bits` encode no target (negative, zero or
/// overflow).
///
/// Spec §7.7.4: `ToTarget`.
pub fn target_from_compact(bits: u32) -> Option<U256> {
    core::target_from_compact(bits).map(u256)
}

/// The compact form of `target` (zcashd `arith_uint256::GetCompact`).
///
/// Spec §7.7.4: `ToCompact`.
pub fn compact_from_u256(target: U256) -> u32 {
    checked(uint256(target).to_compact())
}

/// The work of a block with target `bits`: `floor(2^256 / (target + 1))` (protocol
/// specification §7.7.5, the ZIP 221 field `nSubTreeTotalWork`). The cumulative work of a
/// chain is the sum of the work of its blocks. `None` when `bits` encode no target.
pub fn block_work(bits: u32) -> Option<U256> {
    checked(core::block_work(bits)).map(u256)
}

/// The `nBits` that the block at `chain.height` with time `time` must have on `network`
/// (`hayai_consensus_core::difficulty_rules::expected_bits`). An upgrade without a rule set in
/// this build is [`DifficultyError::Rules`].
///
/// Regtest has no such rule in hayai (`NetworkParams::disable_pow`): the header rules do
/// not call this function there.
pub fn expected_bits(
    network: Network,
    time: u32,
    chain: &ParentChain<'_>,
) -> Result<u32, DifficultyError> {
    let rules = core_rules_at(network, chain.height)?;
    core::expected_bits(network.core(), &rules, time, *chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_wire::header::{compact_from_target, expand_target};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    /// Bitcoin's known values and the golden values of Zakura's
    /// `zakura-chain/src/work/difficulty/tests/vectors.rs` (`COMPACT_DIFFICULTY_CASES`).
    #[test]
    fn block_work_vectors() {
        assert_eq!(block_work(0x1d00_ffff), Some(U256::from(0x1_0001_0001u64)));
        assert_eq!(block_work(0x207f_ffff), Some(U256::from(2u64)));
        assert_eq!(block_work(0x2000_7fff), Some(U256::from(512u64)));
        // Mainnet limit 0x0007ffff << 216: floor(2^256 / (target + 1)) = 8192.
        assert_eq!(block_work(0x1f07_ffff), Some(U256::from(8_192u64)));
        let golden = [
            (
                0x0112_3456,
                "0d79435e50d79435e50d79435e50d79435e50d79435e50d79435e50d79435e50",
            ),
            (
                0x0200_8000,
                "01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f01fc07f0",
            ),
            (
                0x0500_9234,
                "00000001c040c95a099201bcaf85db4e7f2e21e18707c8d55a887643b95afb2f",
            ),
            (
                0x0412_3456,
                "0000000e10005c64415f04ef3e387b97db388404db9fdfaab2b1918f6783471d",
            ),
        ];
        for (bits, work) in golden {
            let expected = U256::from_str_radix(work, 16).expect("hex");
            assert_eq!(block_work(bits), Some(expected), "{bits:#010x}");
        }
        for invalid in [0x0100_3456, 0x0492_3456, 0x0100_0001, 0x1d80_0000, 0] {
            assert_eq!(block_work(invalid), None, "{invalid:#010x}");
        }
    }

    #[test]
    fn compact_and_u256_round_trip() {
        for bits in [
            0x1d00_ffff,
            0x1f07_ffff,
            0x2007_ffff,
            0x200f_0f0f,
            0x1c01_7878,
        ] {
            let Some(target) = target_from_compact(bits) else {
                panic!("{bits:#x} decodes");
            };
            assert_eq!(compact_from_u256(target), bits);
        }
        assert_eq!(
            target_from_compact(0x0300_1234),
            Some(U256::from(0x1234u64))
        );
        assert_eq!(compact_from_u256(U256::from(0x80u64)), 0x0200_8000);
        // The compact form of the full limit is the network's compact limit.
        for network in Network::ALL {
            let params = network.params();
            let limit = U256::from_little_endian(&params.pow_limit);
            assert_eq!(compact_from_u256(limit), params.pow_limit_bits);
        }
    }

    /// The work of a block in `U256`: `floor(2^256 / (target + 1))` as
    /// `floor((2^256 - 1 - target) / (target + 1)) + 1` (Bitcoin's `GetBlockProof`).
    fn reference_work(target: U256) -> U256 {
        (!target / (target + 1)) + 1
    }

    fn random_u256(rng: &mut StdRng) -> U256 {
        // Values of every size: a random number of random limbs, the rest zero.
        let limbs = rng.gen_range(1..=4);
        let mut words = [0u64; 4];
        for word in &mut words[..limbs] {
            *word = rng.gen();
        }
        if rng.gen_bool(0.2) {
            words[limbs - 1] >>= rng.gen_range(0..64);
        }
        U256(words)
    }

    /// The 256-bit type of the core against `primitive_types::U256` and the compact codec
    /// of hayai-wire, on random and edge inputs: the compact forms, the work, the sum, the
    /// difference, the product by a small integer and the division by a small integer.
    #[test]
    fn the_core_arithmetic_matches_u256() {
        let mut rng = StdRng::seed_from_u64(0x7_7_3);
        let mut edges = vec![
            U256::zero(),
            U256::one(),
            U256::MAX,
            U256::MAX - 1,
            U256::one() << 255,
            U256::one() << 64,
            (U256::one() << 64) - 1,
            U256::from(0x7f_ffffu64) << 232,
        ];
        for _ in 0..2_000 {
            edges.push(random_u256(&mut rng));
        }
        for a in &edges {
            let core_a = uint256(*a);
            assert_eq!(u256(core_a), *a);
            let mut le = [0u8; 32];
            a.to_little_endian(&mut le);
            assert_eq!(core_a.to_le_bytes(), le);
            assert_eq!(Uint256::from_le_bytes(&le), core_a);
            assert_eq!(compact_from_u256(*a), compact_from_target(&le), "{a:#x}");
            assert_eq!(core_a.bit_length(), 256 - a.leading_zeros(), "{a:#x}");
            for b in edges.iter().take(16) {
                let core_b = uint256(*b);
                assert_eq!(
                    core_a.checked_add(core_b).map(u256),
                    a.checked_add(*b),
                    "{a:#x} + {b:#x}"
                );
                assert_eq!(
                    core_a.checked_sub(core_b).map(u256),
                    a.checked_sub(*b),
                    "{a:#x} - {b:#x}"
                );
                assert_eq!(core_a.cmp(&core_b), a.cmp(b));
            }
            for small in [1u64, 2, 3, 17, 102, 1275, 2550, 0xffff_ffff, u64::MAX] {
                assert_eq!(
                    core_a.checked_mul_u64(small).map(u256),
                    a.checked_mul(U256::from(small)),
                    "{a:#x} * {small}"
                );
                let (quotient, remainder) = core_a.div_rem_u64(small).expect("a divisor above 0");
                assert_eq!(u256(quotient), *a / U256::from(small), "{a:#x} / {small}");
                assert_eq!(
                    U256::from(remainder),
                    *a % U256::from(small),
                    "{a:#x} % {small}"
                );
            }
        }
        // The compact codec on random bits, and the work on every valid target.
        let mut bits: Vec<u32> = (0..20_000).map(|_| rng.gen()).collect();
        bits.extend([
            0x1d00_ffff,
            0x1f07_ffff,
            0x2007_ffff,
            0x200f_0f0f,
            0x0101_0000,
            0x2200_00ff,
            0x2200_01ff,
            0x1f80_0001,
            0x1f00_0000,
            0,
            u32::MAX,
        ]);
        for bits in bits {
            let expected = expand_target(bits).map(|le| U256::from_little_endian(&le));
            assert_eq!(target_from_compact(bits), expected, "{bits:#010x}");
            assert_eq!(
                block_work(bits),
                expected.map(reference_work),
                "{bits:#010x}"
            );
        }
    }
}
