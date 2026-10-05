//! Packing of a MerkleCRH^Orchard input into Sinsemilla words.
//!
//! The message is `I2LEBSP_10(l) || I2LEBSP_255(left) || I2LEBSP_255(right)`: 520 bits.
//! Sinsemilla consumes them as 52 little-endian 10-bit words. Word 0 is the level. Words
//! 1..=25 are the low 250 bits of `left`. Word 26 covers the top 5 bits of `left` and the low
//! 5 bits of `right`. Words 27..=51 are the top 250 bits of `right`.

use hayai_crypto::{ff, pasta_curves};

use ff::PrimeField;
use pasta_curves::pallas;

/// Bits per Sinsemilla word.
pub const K: usize = 10;

/// Bits of one child encoding (`I2LEBSP_255` of a Pallas base field element).
pub const L_ORCHARD_MERKLE: usize = 255;

/// Sinsemilla words in one MerkleCRH^Orchard message.
pub const WORDS: usize = (K + 2 * L_ORCHARD_MERKLE) / K;

const _: () = assert!((K + 2 * L_ORCHARD_MERKLE).is_multiple_of(K));

/// 64-bit limbs that hold the whole 520-bit message.
const LIMBS: usize = (WORDS * K).div_ceil(64);

const WORD_MASK: u64 = (1 << K) - 1;

/// Packs `(level, left, right)` into the 52 Sinsemilla words of `MerkleCRH^Orchard`.
///
/// `level` is the Sinsemilla word `l` of the specification, that is, the
/// `incrementalmerkletree::Level` of the two children (`0` for two leaves). Only its low
/// 10 bits are representable. Callers pass values below `MERKLE_DEPTH_ORCHARD`.
pub fn merkle_crh_words(level: u8, left: &pallas::Base, right: &pallas::Base) -> [u16; WORDS] {
    let mut limbs = [0u64; LIMBS];
    limbs[0] = u64::from(level);
    or_shifted(&mut limbs, left, K);
    or_shifted(&mut limbs, right, K + L_ORCHARD_MERKLE);

    let mut words = [0u16; WORDS];
    for (i, word) in words.iter_mut().enumerate() {
        let bit = i * K;
        let (limb, offset) = (bit / 64, bit % 64);
        let mut value = limbs[limb] >> offset;
        if offset > 64 - K {
            value |= limbs[limb + 1] << (64 - offset);
        }
        *word = (value & WORD_MASK) as u16;
    }
    words
}

/// ORs the canonical little-endian encoding of `element` into `limbs` starting at bit `shift`.
///
/// A canonical Pallas base element is below `2^255`. Its encoding therefore has bit 255
/// clear, and the 255-bit window of the specification is exactly its 256-bit little-endian
/// representation.
fn or_shifted(limbs: &mut [u64; LIMBS], element: &pallas::Base, shift: usize) {
    let bytes = element.to_repr();
    let (limb, offset) = (shift / 64, shift % 64);
    for (k, chunk) in bytes.as_chunks::<8>().0.iter().enumerate() {
        let value = u64::from_le_bytes(*chunk);
        limbs[limb + k] |= value << offset;
        if offset > 0 {
            limbs[limb + k + 1] |= value >> (64 - offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ff::{Field, PrimeFieldBits};
    use hayai_crypto::rng::{SeedableRng, StdRng};

    /// The word layout that upstream `MerkleHashOrchard::combine` feeds to Sinsemilla, built
    /// the slow way from the bit iterators.
    fn reference_words(level: u8, left: &pallas::Base, right: &pallas::Base) -> [u16; WORDS] {
        let bits: Vec<bool> = (0..K)
            .map(|b| (u16::from(level) >> b) & 1 == 1)
            .chain(left.to_le_bits().iter().by_vals().take(L_ORCHARD_MERKLE))
            .chain(right.to_le_bits().iter().by_vals().take(L_ORCHARD_MERKLE))
            .collect();
        let mut words = [0u16; WORDS];
        for (word, chunk) in words.iter_mut().zip(bits.chunks(K)) {
            *word = chunk
                .iter()
                .enumerate()
                .fold(0u16, |acc, (i, bit)| acc | (u16::from(*bit) << i));
        }
        words
    }

    #[test]
    fn matches_bit_iterator_layout() {
        let mut rng = StdRng::seed_from_u64(7);
        let edge = [
            pallas::Base::ZERO,
            pallas::Base::ONE,
            -pallas::Base::ONE,
            pallas::Base::from_u128(u128::MAX),
        ];
        for level in 0..32u8 {
            for _ in 0..16 {
                let (l, r) = (
                    pallas::Base::random(&mut rng),
                    pallas::Base::random(&mut rng),
                );
                assert_eq!(
                    merkle_crh_words(level, &l, &r),
                    reference_words(level, &l, &r)
                );
            }
            for l in &edge {
                for r in &edge {
                    assert_eq!(merkle_crh_words(level, l, r), reference_words(level, l, r));
                }
            }
        }
    }
}
