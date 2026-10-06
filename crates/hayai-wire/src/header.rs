//! Block headers: parsing, hashing, proof-of-work and Equihash checks.
//!
//! A Zcash header is version, previous hash, merkle root, block commitments, time, bits,
//! nonce, and the Equihash solution with its CompactSize length prefix. The solution length
//! depends on the Equihash parameters of the network ([`PowParams`]): 1344 bytes on Mainnet
//! and Testnet (200, 9), 36 bytes on Regtest (48, 5). A Mainnet header is 1487 bytes, a
//! Regtest header 177 bytes.

use std::fmt;
use std::io::Cursor;

use hayai_crypto::{equihash, zcash_encoding};

use sha2::{Digest, Sha256};
use zcash_encoding::CompactSize;

use crate::ParseError;

/// Block hash (double SHA-256 of the serialized header). Its display form is in reverse byte
/// order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockHash(pub [u8; 32]);

impl fmt::Display for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0.iter().rev() {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockHash({self})")
    }
}

/// The Equihash parameters `(n, k)` of a network. The caller takes them from the network
/// that it runs on; a header does not carry them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PowParams {
    pub n: u32,
    pub k: u32,
}

impl PowParams {
    /// Mainnet: Equihash (200, 9).
    /// Spec §7.7.1: n = 200, k = 9 on Mainnet and Testnet.
    pub const MAINNET: Self = Self { n: 200, k: 9 };
    /// Testnet: the Mainnet parameters.
    pub const TESTNET: Self = Self::MAINNET;
    /// Regtest of zcashd and Zakura: Equihash (48, 5).
    pub const REGTEST: Self = Self { n: 48, k: 5 };
    /// The parameter sets whose solution lengths [`BlockHeader::parse`] accepts.
    pub const KNOWN: [Self; 2] = [Self::MAINNET, Self::REGTEST];

    /// Length of a solution in its minimal encoding: `2^k` indices of `n / (k + 1) + 1`
    /// bits each. 1344 bytes for (200, 9), 36 bytes for (48, 5).
    pub const fn solution_len(self) -> usize {
        (1usize << self.k) * (self.n as usize / (self.k as usize + 1) + 1) / 8
    }

    /// Length of a serialized header under these parameters.
    pub const fn header_len(self) -> usize {
        let solution = self.solution_len();
        BlockHeader::SOLUTION_OFFSET + compact_size_len(solution) + solution
    }
}

/// Length of the CompactSize encoding of `n`.
const fn compact_size_len(n: usize) -> usize {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    pub prev_hash: BlockHash,
    pub merkle_root: [u8; 32],
    pub block_commitments: [u8; 32],
    pub time: u32,
    pub bits: u32,
    pub nonce: [u8; 32],
    /// The solution without its CompactSize length prefix.
    pub solution: Vec<u8>,
}

impl BlockHeader {
    /// Length of the header fields that precede the nonce. This is the Equihash input.
    pub const EQUIHASH_INPUT_LEN: usize = 4 + 32 * 3 + 4 * 2;
    const NONCE_OFFSET: usize = Self::EQUIHASH_INPUT_LEN;
    /// Offset of the CompactSize length prefix of the solution.
    const SOLUTION_OFFSET: usize = Self::NONCE_OFFSET + 32;

    /// Parses the header at the start of `bytes`. The bytes after the header are not read;
    /// [`BlockHeader::serialized_len`] gives the length that the header occupies.
    ///
    /// The solution length must be the length of one of the [`PowParams::KNOWN`] sets, in
    /// the canonical CompactSize encoding. The parser rejects other lengths. Whether the
    /// length matches the network is a check of the caller.
    ///
    /// Spec §7.6: `solutionSize` has the minimal encoding, and the parser refuses each
    /// other encoding (`CompactSize::read_t` of `zcash_encoding`).
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let Some(fixed) = bytes.get(..Self::SOLUTION_OFFSET) else {
            return Err(ParseError::Header(format!(
                "need {} bytes before the solution, have {}",
                Self::SOLUTION_OFFSET,
                bytes.len()
            )));
        };
        let mut cursor = Cursor::new(&bytes[Self::SOLUTION_OFFSET..]);
        let len: usize = CompactSize::read_t(&mut cursor)
            .map_err(|e| ParseError::Header(format!("solution length: {e}")))?;
        if !PowParams::KNOWN.iter().any(|p| p.solution_len() == len) {
            return Err(ParseError::Header(format!(
                "solution of {len} bytes matches no known Equihash parameters"
            )));
        }
        let start = Self::SOLUTION_OFFSET + cursor.position() as usize;
        let Some(solution) = bytes.get(start..start + len) else {
            return Err(ParseError::Header(format!(
                "need {} bytes, have {}",
                start + len,
                bytes.len()
            )));
        };
        let u32_at = |off: usize| u32::from_le_bytes(fixed[off..off + 4].try_into().expect("4"));
        let arr_at = |off: usize| -> [u8; 32] { fixed[off..off + 32].try_into().expect("32") };
        Ok(Self {
            version: u32_at(0),
            prev_hash: BlockHash(arr_at(4)),
            merkle_root: arr_at(36),
            block_commitments: arr_at(68),
            time: u32_at(100),
            bits: u32_at(104),
            nonce: arr_at(Self::NONCE_OFFSET),
            solution: solution.to_vec(),
        })
    }

    /// Length of the serialized header.
    pub fn serialized_len(&self) -> usize {
        Self::SOLUTION_OFFSET + compact_size_len(self.solution.len()) + self.solution.len()
    }

    /// Serializes the header. The CompactSize prefix is canonical, so the output equals the
    /// bytes that [`BlockHeader::parse`] read. It panics if the solution length matches no
    /// [`PowParams::KNOWN`] set, because the parser rejects such a header.
    pub fn serialize(&self) -> Vec<u8> {
        assert!(
            PowParams::KNOWN
                .iter()
                .any(|p| p.solution_len() == self.solution.len()),
            "BlockHeader::serialize: a solution of {} bytes matches no known Equihash parameters",
            self.solution.len()
        );
        let mut out = Vec::with_capacity(self.serialized_len());
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.prev_hash.0);
        out.extend_from_slice(&self.merkle_root);
        out.extend_from_slice(&self.block_commitments);
        out.extend_from_slice(&self.time.to_le_bytes());
        out.extend_from_slice(&self.bits.to_le_bytes());
        out.extend_from_slice(&self.nonce);
        CompactSize::write(&mut out, self.solution.len()).expect("writing to a Vec cannot fail");
        out.extend_from_slice(&self.solution);
        out
    }

    /// Double SHA-256 of the serialized header.
    pub fn hash(&self) -> BlockHash {
        let first = Sha256::digest(self.serialize());
        BlockHash(Sha256::digest(first).into())
    }
}

/// Expands the compact `bits` encoding to a 256-bit little-endian target.
///
/// Returns `None` for encodings that denote no reachable target: a negative sign bit, a
/// target of zero (a zero mantissa, or a mantissa that a small exponent shifts out), or an
/// exponent that overflows 256 bits (`arith_uint256::SetCompact` semantics with the
/// rejections of zcashd's `CheckProofOfWork`).
///
/// Spec §7.7.4: `ToTarget`; a negative or zero target is no target.
pub fn expand_target(bits: u32) -> Option<[u8; 32]> {
    let exponent = (bits >> 24) as usize;
    let mantissa = bits & 0x007f_ffff;
    let negative = bits & 0x0080_0000 != 0;
    if negative || mantissa == 0 {
        return None;
    }
    // Overflow condition of arith_uint256::SetCompact.
    if exponent > 34 || (mantissa > 0xff && exponent > 33) || (mantissa > 0xffff && exponent > 32) {
        return None;
    }
    let mut target = [0u8; 32];
    if exponent <= 3 {
        let shifted = mantissa >> (8 * (3 - exponent));
        if shifted == 0 {
            return None;
        }
        target[..4].copy_from_slice(&shifted.to_le_bytes());
    } else {
        let offset = exponent - 3;
        for (i, b) in mantissa.to_le_bytes()[..3].iter().enumerate() {
            // Bytes past the end are zero by the overflow condition above.
            if let Some(slot) = target.get_mut(offset + i) {
                *slot = *b;
            }
        }
    }
    Some(target)
}

/// Encodes a 256-bit little-endian target in the compact `bits` form
/// (`arith_uint256::GetCompact` of zcashd, for a value that is not negative).
///
/// The compact form keeps the three most significant bytes. A mantissa whose top bit is
/// set would read as the sign bit, so it moves down one byte and the size goes up by one.
/// The target zero encodes as zero, which [`expand_target`] rejects.
///
/// Spec §7.7.4: `ToCompact`.
pub fn compact_from_target(target: &[u8; 32]) -> u32 {
    let Some(top) = target.iter().rposition(|b| *b != 0) else {
        return 0;
    };
    let mut size = top as u32 + 1;
    let byte = |i: usize| u32::from(target[i]);
    let mut mantissa = match top {
        0 => byte(0) << 16,
        1 => byte(1) << 16 | byte(0) << 8,
        _ => byte(top) << 16 | byte(top - 1) << 8 | byte(top - 2),
    };
    if mantissa & 0x0080_0000 != 0 {
        mantissa >>= 8;
        size += 1;
    }
    size << 24 | mantissa
}

/// Whether `a` is at most `b`, both as 256-bit little-endian integers.
fn le_at_most(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().rev().cmp(b.iter().rev()) != std::cmp::Ordering::Greater
}

/// Why a header fails the proof-of-work rules of [`check_target`] and [`check_pow`].
#[derive(thiserror::Error, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowError {
    #[error("bits {0:#010x} encode no target")]
    InvalidBits(u32),
    #[error("the target of bits {0:#010x} is above the proof-of-work limit")]
    TargetAboveLimit(u32),
    #[error("the header hash is above the target")]
    HashAboveTarget,
}

/// The target that `bits` encodes, when it is at most `pow_limit` (the network's
/// proof-of-work limit as a 256-bit little-endian integer).
pub fn check_target(bits: u32, pow_limit: &[u8; 32]) -> Result<[u8; 32], PowError> {
    let Some(target) = expand_target(bits) else {
        return Err(PowError::InvalidBits(bits));
    };
    if !le_at_most(&target, pow_limit) {
        return Err(PowError::TargetAboveLimit(bits));
    }
    Ok(target)
}

/// Proof-of-work check: the target that `bits` encodes is at most `pow_limit`, and the
/// header hash, read as a little-endian 256-bit integer, is at most that target. Whether
/// `bits` itself is the correct target for this height is a contextual rule
/// (`hayai_consensus::difficulty`). This function does not check that rule.
pub fn check_pow(header: &BlockHeader, pow_limit: &[u8; 32]) -> Result<(), PowError> {
    let target = check_target(header.bits, pow_limit)?;
    // Spec §7.7.2: SHA-256d of the whole header, as a little-endian integer, is at most
    // `ToTarget(nBits)`.
    if !le_at_most(&header.hash().0, &target) {
        return Err(PowError::HashAboveTarget);
    }
    Ok(())
}

/// Equihash verification with the network's parameters `params`: the header's solution
/// against the header fields before the nonce, with the nonce as the Equihash nonce, as in
/// `zcashd`'s `CheckEquihashSolution`. A solution whose length does not match `params`
/// fails.
///
/// Spec §7.7.1: the Equihash input is the header up to `nBits`, then `nNonce`.
pub fn check_equihash(header: &BlockHeader, params: PowParams) -> Result<(), equihash::Error> {
    let bytes = header.serialize();
    equihash::is_valid_solution(
        params.n,
        params.k,
        &bytes[..BlockHeader::EQUIHASH_INPUT_LEN],
        &header.nonce,
        &header.solution,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_header() -> impl Strategy<Value = BlockHeader> {
        (
            any::<u32>(),
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
            any::<[u8; 32]>(),
            any::<u32>(),
            any::<u32>(),
            any::<[u8; 32]>(),
            prop_oneof![
                Just(PowParams::MAINNET.solution_len()),
                Just(PowParams::REGTEST.solution_len()),
            ]
            .prop_flat_map(|len| proptest::collection::vec(any::<u8>(), len)),
        )
            .prop_map(
                |(version, prev, merkle, commitments, time, bits, nonce, solution)| BlockHeader {
                    version,
                    prev_hash: BlockHash(prev),
                    merkle_root: merkle,
                    block_commitments: commitments,
                    time,
                    bits,
                    nonce,
                    solution,
                },
            )
    }

    proptest! {
        #[test]
        fn header_round_trip(h in arb_header()) {
            let bytes = h.serialize();
            let parsed = BlockHeader::parse(&bytes).unwrap();
            prop_assert_eq!(&parsed, &h);
            prop_assert_eq!(parsed.serialized_len(), bytes.len());
            prop_assert_eq!(parsed.serialize(), bytes);
        }

        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..1600)) {
            let _ = BlockHeader::parse(&bytes);
        }

        #[test]
        fn pow_never_panics(h in arb_header()) {
            let _ = check_pow(&h, &NO_LIMIT);
        }

        /// A target that the compact form holds exactly survives the round trip, and the
        /// compact form of a decoded target is canonical: it decodes to the same target.
        #[test]
        fn compact_round_trip(bits in any::<u32>()) {
            if let Some(target) = expand_target(bits) {
                let canonical = compact_from_target(&target);
                prop_assert_eq!(expand_target(canonical), Some(target));
                prop_assert_eq!(compact_from_target(&target), canonical);
            }
        }

        /// The compact form keeps the three most significant bytes of any target, or two
        /// when the third would set the sign bit.
        #[test]
        fn compact_truncates_to_the_top_bytes(target in any::<[u8; 32]>(), len in 1usize..=32) {
            let mut target = target;
            target[len..].fill(0);
            prop_assume!(target != [0u8; 32]);
            let bits = compact_from_target(&target);
            let Some(decoded) = expand_target(bits) else {
                return Err(TestCaseError::fail(format!("{bits:#x} must decode")));
            };
            prop_assert!(le_at_most(&decoded, &target));
            let top = target.iter().rposition(|b| *b != 0).unwrap();
            let kept = if target[top] & 0x80 != 0 { 2 } else { 3 };
            let mut expected = [0u8; 32];
            let low = (top + 1).saturating_sub(kept);
            expected[low..=top].copy_from_slice(&target[low..=top]);
            prop_assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn expand_target_matches_known_values() {
        // Genesis bits on mainnet: 0x1f07ffff → 0x0007ffff << (8 * (0x1f - 3)).
        let t = expand_target(0x1f07_ffff).unwrap();
        let mut expected = [0u8; 32];
        expected[28] = 0xff;
        expected[29] = 0xff;
        expected[30] = 0x07;
        assert_eq!(t, expected);
        // Exponent at or below 3 shifts the mantissa right.
        let t = expand_target(0x0300_1234).unwrap();
        assert_eq!(&t[..3], &[0x34, 0x12, 0x00]);
        let t = expand_target(0x0200_1234).unwrap();
        assert_eq!(&t[..3], &[0x12, 0x00, 0x00]);
        assert_eq!(expand_target(0x1f80_0001), None, "negative");
        assert_eq!(expand_target(0x1f00_0000), None, "zero mantissa");
        assert_eq!(expand_target(0xff00_0001), None, "overflow");
        assert_eq!(
            expand_target(0x2200_01ff),
            None,
            "two bytes do not fit at exponent 34"
        );
        let Some(t) = expand_target(0x2200_00ff) else {
            panic!("one byte fits at exponent 34");
        };
        assert_eq!(t[31], 0xff);
    }

    /// A limit that no target exceeds.
    const NO_LIMIT: [u8; 32] = [0xff; 32];

    fn target_of(value: u64) -> [u8; 32] {
        let mut target = [0u8; 32];
        target[..8].copy_from_slice(&value.to_le_bytes());
        target
    }

    /// The vectors of Bitcoin Core's `arith_uint256_tests.cpp` (`bignum_SetCompact`), which
    /// zcashd keeps: the compact value, the target it decodes to, and the compact form of
    /// that target (`GetCompact`).
    #[test]
    fn compact_vectors_of_bitcoin_and_zcashd() {
        // (bits, decoded target, canonical bits). `None`: zero, negative or overflow.
        let cases: [(u32, Option<u64>, u32); 21] = [
            (0x0000_0000, None, 0),
            (0x0012_3456, None, 0),
            (0x0100_3456, None, 0),
            (0x0200_0056, None, 0),
            (0x0300_0000, None, 0),
            (0x0400_0000, None, 0),
            (0x0092_3456, None, 0),
            (0x0180_3456, None, 0),
            (0x0280_0056, None, 0),
            (0x0380_0000, None, 0),
            (0x0480_0000, None, 0),
            (0x0112_3456, Some(0x12), 0x0112_0000),
            (0x0212_3456, Some(0x1234), 0x0212_3400),
            (0x0312_3456, Some(0x12_3456), 0x0312_3456),
            (0x0412_3456, Some(0x1234_5600), 0x0412_3456),
            (0x0500_9234, Some(0x9234_0000), 0x0500_9234),
            (0x0200_8000, Some(0x80), 0x0200_8000),
            // Negative: the sign bit with a mantissa that is not zero.
            (0x0492_3456, None, 0),
            (0x01fe_dcba, None, 0),
            // The mantissa 0x008000 at size 1 shifts out to zero.
            (0x0100_8000, None, 0),
            (0x0300_ffff, Some(0xffff), 0x0300_ffff),
        ];
        for (bits, target, canonical) in cases {
            let decoded = expand_target(bits);
            assert_eq!(decoded, target.map(target_of), "{bits:#010x}");
            if let Some(decoded) = decoded {
                assert_eq!(compact_from_target(&decoded), canonical, "{bits:#010x}");
            }
        }
        assert_eq!(compact_from_target(&[0u8; 32]), 0);
        assert_eq!(compact_from_target(&target_of(0x80)), 0x0200_8000);
        assert_eq!(compact_from_target(&target_of(0x7f)), 0x017f_0000);
        assert_eq!(compact_from_target(&target_of(0x8000)), 0x0300_8000);
        assert_eq!(compact_from_target(&target_of(0x80_0000)), 0x0400_8000);

        // 0x20123456 is 0x123456 followed by 29 zero bytes (Bitcoin Core's vector).
        let mut high = [0u8; 32];
        high[29..].copy_from_slice(&[0x56, 0x34, 0x12]);
        assert_eq!(expand_target(0x2012_3456), Some(high));
        assert_eq!(compact_from_target(&high), 0x2012_3456);
        // Overflow.
        assert_eq!(expand_target(0xff12_3456), None);
        assert_eq!(expand_target(0x2101_0000), None);
        assert_eq!(expand_target(0x2200_0100), None);
    }

    /// The limits of Bitcoin and of the three Zcash networks: the compact form of the full
    /// limit, and the target that the compact form decodes to.
    #[test]
    fn compact_form_of_the_known_limits() {
        fn ones(bits: usize) -> [u8; 32] {
            let mut bytes = [0u8; 32];
            bytes[..bits / 8].fill(0xff);
            if let Some(partial) = bytes.get_mut(bits / 8) {
                *partial = (1 << (bits % 8)) - 1;
            }
            bytes
        }
        // Bitcoin Mainnet: 2^224 - 1 encodes as 0x1d00ffff.
        assert_eq!(compact_from_target(&ones(224)), 0x1d00_ffff);
        let mut bitcoin = [0u8; 32];
        bitcoin[26..28].fill(0xff);
        assert_eq!(expand_target(0x1d00_ffff), Some(bitcoin));
        // Zcash Mainnet: 2^243 - 1. Zcash Testnet: 2^251 - 1. Regtest: 0x0f0f...0f.
        assert_eq!(compact_from_target(&ones(243)), 0x1f07_ffff);
        assert_eq!(compact_from_target(&ones(251)), 0x2007_ffff);
        assert_eq!(compact_from_target(&[0x0f; 32]), 0x200f_0f0f);
        // Bitcoin Regtest: 2^255 - 1 encodes as 0x207fffff.
        assert_eq!(compact_from_target(&ones(255)), 0x207f_ffff);
        for bits in [
            0x1d00_ffff,
            0x1f07_ffff,
            0x2007_ffff,
            0x200f_0f0f,
            0x207f_ffff,
        ] {
            let Some(target) = expand_target(bits) else {
                panic!("{bits:#x} decodes");
            };
            assert_eq!(compact_from_target(&target), bits);
        }
    }

    #[test]
    fn check_target_applies_the_limit() {
        let Some(limit) = expand_target(0x1f07_ffff) else {
            panic!("the Mainnet limit decodes");
        };
        assert_eq!(check_target(0x1f07_ffff, &limit), Ok(limit));
        assert_eq!(
            check_target(0x1f07_fffe, &limit),
            Ok(expand_target(0x1f07_fffe).unwrap())
        );
        assert_eq!(
            check_target(0x1f08_0000, &limit),
            Err(PowError::TargetAboveLimit(0x1f08_0000))
        );
        assert_eq!(
            check_target(0x2007_ffff, &limit),
            Err(PowError::TargetAboveLimit(0x2007_ffff))
        );
        assert_eq!(
            check_target(0x1f80_0001, &limit),
            Err(PowError::InvalidBits(0x1f80_0001))
        );
        assert_eq!(check_target(0, &limit), Err(PowError::InvalidBits(0)));
    }

    #[test]
    fn check_pow_compares_little_endian() {
        let mut h = BlockHeader {
            version: 4,
            prev_hash: BlockHash([0; 32]),
            merkle_root: [0; 32],
            block_commitments: [0; 32],
            time: 0,
            bits: 0x0100_0001,
            nonce: [0; 32],
            solution: vec![0; PowParams::MAINNET.solution_len()],
        };
        assert_eq!(
            check_pow(&h, &NO_LIMIT),
            Err(PowError::InvalidBits(0x0100_0001)),
            "target 0 (mantissa shifted out) never passes"
        );
        h.bits = 0x1f80_0001;
        assert_eq!(
            check_pow(&h, &NO_LIMIT),
            Err(PowError::InvalidBits(0x1f80_0001)),
            "negative target never passes"
        );

        // Target 0x80 << 248 (exponent 34 puts mantissa byte 0 in the most significant
        // position). The test searches nonces for a hash on each side of it. Bits is part of
        // the hashed header, so the test fixes it before the search.
        h.bits = 0x2200_0080;
        while h.hash().0[31] >= 0x80 {
            h.nonce[0] = h.nonce[0].wrapping_add(1);
        }
        assert_eq!(check_pow(&h, &NO_LIMIT), Ok(()), "hash below target passes");
        // The same header under a limit below its target: the limit rule fails first.
        let Some(mainnet_limit) = expand_target(0x1f07_ffff) else {
            panic!("the Mainnet limit decodes");
        };
        assert_eq!(
            check_pow(&h, &mainnet_limit),
            Err(PowError::TargetAboveLimit(0x2200_0080))
        );
        while h.hash().0[31] <= 0x80 {
            h.nonce[0] = h.nonce[0].wrapping_add(1);
        }
        assert_eq!(
            check_pow(&h, &NO_LIMIT),
            Err(PowError::HashAboveTarget),
            "hash above target fails"
        );
    }

    #[test]
    fn serialize_rejects_wrong_solution_length() {
        let h = BlockHeader {
            version: 4,
            prev_hash: BlockHash([0; 32]),
            merkle_root: [0; 32],
            block_commitments: [0; 32],
            time: 0,
            bits: 0,
            nonce: [0; 32],
            solution: vec![0; 10],
        };
        let Err(_) = std::panic::catch_unwind(|| h.serialize()) else {
            panic!("serialize must panic on a solution of no known length");
        };
    }

    #[test]
    fn known_parameter_sets_have_the_specified_lengths() {
        assert_eq!(PowParams::MAINNET.solution_len(), 1344);
        assert_eq!(PowParams::MAINNET.header_len(), 1487);
        assert_eq!(PowParams::TESTNET, PowParams::MAINNET);
        assert_eq!(PowParams::REGTEST.solution_len(), 36);
        assert_eq!(PowParams::REGTEST.header_len(), 177);
    }

    #[test]
    fn parse_rejects_short_unknown_and_non_canonical_lengths() {
        assert!(matches!(
            BlockHeader::parse(&[0u8; 100]),
            Err(ParseError::Header(_))
        ));
        // Solution length 0.
        let mut bytes = vec![0u8; PowParams::MAINNET.header_len()];
        assert!(matches!(
            BlockHeader::parse(&bytes),
            Err(ParseError::Header(_))
        ));
        bytes[140..143].copy_from_slice(&[0xfd, 0x40, 0x05]);
        let parsed = BlockHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.solution, vec![0u8; 1344]);
        assert_eq!(parsed.serialized_len(), 1487);
        // One byte short of the solution.
        assert!(matches!(
            BlockHeader::parse(&bytes[..1486]),
            Err(ParseError::Header(_))
        ));
        // The 36-byte Regtest solution, with trailing bytes that the parser does not read.
        bytes[140..143].copy_from_slice(&[36, 0, 0]);
        let parsed = BlockHeader::parse(&bytes).unwrap();
        assert_eq!(parsed.solution, vec![0u8; 36]);
        assert_eq!(parsed.serialized_len(), 177);
        assert_eq!(parsed.serialize(), bytes[..177].to_vec());
        // 36 in the three-byte form is not canonical.
        bytes[140..143].copy_from_slice(&[0xfd, 36, 0]);
        assert!(matches!(
            BlockHeader::parse(&bytes),
            Err(ParseError::Header(_))
        ));
        // A length between the known ones.
        bytes[140] = 37;
        assert!(matches!(
            BlockHeader::parse(&bytes),
            Err(ParseError::Header(_))
        ));
    }

    /// A Regtest header with a 36-byte solution: round trip, and the hash over the exact
    /// 177 bytes against a value computed outside Rust.
    #[test]
    fn regtest_header_hashes_its_177_bytes() {
        let h = BlockHeader {
            version: 4,
            prev_hash: BlockHash([1; 32]),
            merkle_root: [2; 32],
            block_commitments: [3; 32],
            time: 0x0506_0708,
            bits: REGTEST_BITS,
            nonce: [4; 32],
            solution: vec![0; 36],
        };
        let mut expected = Vec::new();
        expected.extend_from_slice(&[4, 0, 0, 0]);
        expected.extend_from_slice(&[1; 32]);
        expected.extend_from_slice(&[2; 32]);
        expected.extend_from_slice(&[3; 32]);
        expected.extend_from_slice(&[0x08, 0x07, 0x06, 0x05]);
        expected.extend_from_slice(&[0x0f, 0x0f, 0x0f, 0x20]);
        expected.extend_from_slice(&[4; 32]);
        expected.push(36);
        expected.extend_from_slice(&[0; 36]);
        assert_eq!(h.serialize(), expected);
        assert_eq!(BlockHeader::parse(&expected).unwrap(), h);
        // Python: sha256(sha256(bytes)) of the same 177 bytes, in display order.
        assert_eq!(
            h.hash().to_string(),
            "1b067103e30b6f4e4abce5f5a7adf0f7e0d9960ad95afe479c4e691f179def48"
        );
    }

    const REGTEST_BITS: u32 = 0x200f_0f0f;

    #[test]
    fn display_is_reversed_hex() {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        h[31] = 0x01;
        assert_eq!(BlockHash(h).to_string(), format!("01{}ab", "00".repeat(30)));
    }
}
