//! Transaction boundary scanner and ZIP 244 authorizing-data digest over byte ranges.
//!
//! [`tx_wire_len`] walks the serialized layout of a v1–v6 transaction with length arithmetic
//! and bounds checks only (no allocation, no field decoding). It returns the number of bytes
//! that `zcash_primitives::transaction::Transaction::read` consumes for the same input.
//! [`crate::RawBlock::parse`] knows every transaction boundary of a block in advance. It can
//! therefore run the (expensive) upstream parser on all transactions in parallel.
//!
//! The layout is a transcription of `zcash_primitives` 0.30.1 (`src/transaction/mod.rs`
//! `Transaction::read_v4`/`read_v5`/`read_v6`, `components/sapling.rs`
//! `read_v4_components`/`read_v5_bundle`, `components/orchard.rs` `read_bundle`,
//! `components/sprout.rs` `JsDescription::read`) and of `zcash_transparent` 0.10.0
//! `bundle.rs` `TxIn::read`/`TxOut::read`. That layout matches protocol specification §7.1.
//! The upstream parser does the checks that need the decoded value (curve points, field
//! elements, amounts, branch id, flags). The scanner only has to agree with the parser on the
//! length of every transaction that the parser accepts. `tests/scan.rs` verifies this against
//! the mainnet vectors, the ZIP 143/243/244 vectors and proptest-generated transactions of
//! every version.
//!
//! The same walk records the byte ranges that the ZIP 244 authorizing-data digest covers
//! (`scriptSig`s, Sapling proofs and signatures, Orchard proof and signatures). Therefore
//! [`auth_digest`] produces `Transaction::auth_commitment()` from the wire bytes without a
//! second traversal of the parsed form.

use std::io;
use std::ops::Range;

use hayai_crypto::{zcash_encoding, zcash_protocol};
use zcash_encoding::MAX_COMPACT_SIZE;
use zcash_protocol::constants::{
    V3_TX_VERSION, V3_VERSION_GROUP_ID, V4_TX_VERSION, V4_VERSION_GROUP_ID, V5_TX_VERSION,
    V5_VERSION_GROUP_ID, V6_TX_VERSION, V6_VERSION_GROUP_ID,
};

use crate::ParseError;

/// Groth16 proof (`zcash_primitives::transaction::components::GROTH_PROOF_SIZE`).
const GROTH_PROOF_SIZE: usize = 48 + 96 + 48;
/// PHGR13 proof (`components/sprout.rs` `PHGR_PROOF_SIZE`).
const PHGR_PROOF_SIZE: usize = 33 + 33 + 65 + 33 + 33 + 33 + 33 + 33;
/// JoinSplit description without its proof: vpub_old, vpub_new, anchor, 2 nullifiers,
/// 2 commitments, ephemeral key, random seed, 2 MACs, 2 ciphertexts of 601 bytes.
const JOINSPLIT_FIXED: usize = 8 + 8 + 32 + 2 * 32 + 2 * 32 + 32 + 32 + 2 * 32 + 2 * 601;
/// Sapling v4 spend: cv, anchor, nullifier, rk, proof, spend auth signature.
const SAPLING_SPEND_V4: usize = 32 + 32 + 32 + 32 + GROTH_PROOF_SIZE + 64;
/// Sapling v4 output: cv, cmu, ephemeral key, enc_ciphertext, out_ciphertext, proof.
const SAPLING_OUTPUT_V4: usize = 32 + 32 + 32 + 580 + 80 + GROTH_PROOF_SIZE;
/// Sapling v5 spend without witness data: cv, nullifier, rk.
const SAPLING_SPEND_V5: usize = 32 + 32 + 32;
/// Sapling v5 output without proof: cv, cmu, ephemeral key, enc_ciphertext, out_ciphertext.
const SAPLING_OUTPUT_V5: usize = 32 + 32 + 32 + 580 + 80;
/// Orchard action without authorization: cv_net, nullifier, rk, cmx, epk, enc, out.
const ORCHARD_ACTION: usize = 32 + 32 + 32 + 32 + 32 + 580 + 80;
const SIGNATURE: usize = 64;
const OUTPOINT: usize = 32 + 4;
const AMOUNT: usize = 8;

/// Transaction format family for the purposes of the layout and the authorizing digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// v1–v4: the txid is SHA-256d of the bytes, and the authorizing digest is ZIP 239's
    /// all-ones.
    Legacy,
    V5,
    V6,
}

/// Byte ranges of one Orchard-protocol bundle that the authorizing digest covers.
#[derive(Clone, Debug)]
pub(crate) struct OrchardRanges {
    /// `proof || spendAuthSigs || bindingSig`, contiguous on the wire (proof bytes without
    /// their CompactSize prefix).
    auth: Range<usize>,
    /// Offset of the 32-byte anchor (hashed into the v6 authorizing digest only).
    anchor: usize,
}

/// Layout of one transaction: its total length and the ranges that the authorizing digest
/// needs.
#[derive(Clone, Debug)]
pub(crate) struct TxLayout {
    pub(crate) len: usize,
    pub(crate) format: Format,
    /// Raw `nConsensusBranchId` field (v5 and v6 only; zero for legacy formats).
    branch_id: u32,
    /// Offset of the first transparent input and the number of inputs.
    vin_start: usize,
    vin_count: usize,
    /// `spendProofs || spendAuthSigs || outputProofs || bindingSig` of a present Sapling
    /// bundle (contiguous on the wire in the v5 layout). `None` when the bundle is absent.
    sapling_auth: Option<Range<usize>>,
    /// Offset of the Sapling anchor when there is at least one spend (v6 digest only).
    sapling_anchor: Option<usize>,
    orchard: Option<OrchardRanges>,
    ironwood: Option<OrchardRanges>,
}

/// Returns the number of bytes that the transaction at the start of `bytes` occupies on the
/// wire. Returns an error if `bytes` does not start with a structurally complete transaction.
///
/// For every input that `Transaction::read` accepts, this is exactly the number of bytes that
/// the parser reads. The scanner can accept inputs that the upstream parser rejects for a
/// reason that the layout does not show (an invalid curve point, an out-of-range amount, an
/// unknown branch id, bad Orchard flags). The caller still runs the parser on the delimited
/// slice.
pub fn tx_wire_len(bytes: &[u8]) -> Result<usize, ParseError> {
    scan(bytes).map(|l| l.len)
}

pub(crate) fn scan(bytes: &[u8]) -> Result<TxLayout, ParseError> {
    let mut s = Scanner { bytes, pos: 0 };
    let header = s.u32()?;
    let overwintered = header >> 31 == 1;
    let version = header & 0x7FFF_FFFF;
    let layout = if overwintered {
        // `TxVersion::read`: the version group id must match the version exactly.
        match (version, s.u32()?) {
            (V3_TX_VERSION, V3_VERSION_GROUP_ID) => scan_legacy(&mut s, LegacyKind::V3)?,
            (V4_TX_VERSION, V4_VERSION_GROUP_ID) => scan_legacy(&mut s, LegacyKind::V4)?,
            (V5_TX_VERSION, V5_VERSION_GROUP_ID) => scan_v5_v6(&mut s, Format::V5)?,
            (V6_TX_VERSION, V6_VERSION_GROUP_ID) => scan_v5_v6(&mut s, Format::V6)?,
            _ => return Err(invalid("unknown transaction format")),
        }
    } else if version >= 2 {
        scan_legacy(&mut s, LegacyKind::Sprout)?
    } else if version == 1 {
        scan_legacy(&mut s, LegacyKind::V1)?
    } else {
        return Err(invalid("unknown transaction format"));
    };
    Ok(layout)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LegacyKind {
    /// Non-overwintered version 1: no JoinSplits.
    V1,
    /// Non-overwintered version >= 2: JoinSplits with PHGR13 proofs.
    Sprout,
    /// Overwinter: expiry height, JoinSplits with PHGR13 proofs.
    V3,
    /// Sapling: expiry height, Sapling value balance/spends/outputs, JoinSplits with Groth16
    /// proofs, binding signature.
    V4,
}

/// `Transaction::read_v4` (zcash_primitives-0.30.1 `src/transaction/mod.rs:750-820`).
fn scan_legacy(s: &mut Scanner<'_>, kind: LegacyKind) -> Result<TxLayout, ParseError> {
    let (vin_start, vin_count) = s.transparent()?;
    s.skip(4)?; // lock_time
    if matches!(kind, LegacyKind::V3 | LegacyKind::V4) {
        s.skip(4)?; // expiry_height
    }
    let mut sapling_present = false;
    if kind == LegacyKind::V4 {
        s.skip(AMOUNT)?; // valueBalanceSapling
        let n_spends = s.vector_fixed(SAPLING_SPEND_V4)?;
        let n_outputs = s.vector_fixed(SAPLING_OUTPUT_V4)?;
        sapling_present = n_spends + n_outputs > 0;
    }
    if kind != LegacyKind::V1 {
        let proof = if kind == LegacyKind::V4 {
            GROTH_PROOF_SIZE
        } else {
            PHGR_PROOF_SIZE
        };
        let n_joinsplits = s.vector_fixed(JOINSPLIT_FIXED + proof)?;
        if n_joinsplits > 0 {
            s.skip(32 + SIGNATURE)?; // joinSplitPubKey, joinSplitSig
        }
    }
    if sapling_present {
        s.skip(SIGNATURE)?; // bindingSigSapling
    }
    Ok(TxLayout {
        len: s.pos,
        format: Format::Legacy,
        branch_id: 0,
        vin_start,
        vin_count,
        sapling_auth: None,
        sapling_anchor: None,
        orchard: None,
        ironwood: None,
    })
}

/// `valueBalanceSapling` of a v4 transaction that `Transaction::read` already accepted.
/// The field sits after lock_time and expiry_height. This function reads it back because
/// `read_v4_components` (`components/sapling.rs:384-404`) replaces it with zero when the
/// transaction has no Sapling spends and no outputs. §7.1.2 requires the field to be zero in
/// that case.
pub(crate) fn v4_value_balance(bytes: &[u8]) -> Result<i64, ParseError> {
    let mut s = Scanner { bytes, pos: 0 };
    let header = s.u32()?;
    if (header, s.u32()?) != (V4_TX_VERSION | 0x8000_0000, V4_VERSION_GROUP_ID) {
        return Err(invalid("not a v4 transaction"));
    }
    s.transparent()?;
    s.skip(4 + 4)?; // lock_time, expiry_height
    let start = s.pos;
    s.skip(AMOUNT)?;
    Ok(i64::from_le_bytes(
        bytes[start..start + AMOUNT]
            .try_into()
            .expect("8 bytes were bounds-checked"),
    ))
}

/// `Transaction::read_v5` / `read_v6` (`src/transaction/mod.rs:850-910`). The two formats
/// differ only in the second (Ironwood) Orchard-protocol bundle. Upstream compiles the
/// ZIP 233 amount of the v6 header only under the `zcash_unstable = "nu7"` cfg. This
/// workspace does not set that cfg (`read_v6_header_fragment`, `mod.rs:933-945`).
fn scan_v5_v6(s: &mut Scanner<'_>, format: Format) -> Result<TxLayout, ParseError> {
    let branch_id = s.u32()?;
    s.skip(4 + 4)?; // lock_time, expiry_height
    let (vin_start, vin_count) = s.transparent()?;
    let (sapling_auth, sapling_anchor) = s.sapling_v5()?;
    let orchard = s.orchard_bundle()?;
    let ironwood = match format {
        Format::V6 => s.orchard_bundle()?,
        Format::V5 | Format::Legacy => None,
    };
    Ok(TxLayout {
        len: s.pos,
        format,
        branch_id,
        vin_start,
        vin_count,
        sapling_auth,
        sapling_anchor,
        orchard,
        ironwood,
    })
}

struct Scanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Scanner<'_> {
    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(eof)?;
        self.pos = end;
        Ok(())
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let start = self.pos;
        self.skip(4)?;
        Ok(u32::from_le_bytes(
            self.bytes[start..start + 4]
                .try_into()
                .expect("4 bytes were bounds-checked"),
        ))
    }

    /// `zcash_encoding::CompactSize::read`: canonical encoding, at most `MAX_COMPACT_SIZE`.
    fn compact_size(&mut self) -> Result<usize, ParseError> {
        let flag = self.bytes.get(self.pos).copied().ok_or_else(eof)?;
        self.pos += 1;
        let value = match flag {
            0..=252 => u64::from(flag),
            253 => {
                let start = self.pos;
                self.skip(2)?;
                let n = u16::from_le_bytes([self.bytes[start], self.bytes[start + 1]]);
                if n < 253 {
                    return Err(invalid("non-canonical CompactSize"));
                }
                u64::from(n)
            }
            254 => {
                let n = self.u32()?;
                if n < 0x1_0000 {
                    return Err(invalid("non-canonical CompactSize"));
                }
                u64::from(n)
            }
            255 => {
                let start = self.pos;
                self.skip(8)?;
                let n = u64::from_le_bytes(
                    self.bytes[start..start + 8]
                        .try_into()
                        .expect("8 bytes were bounds-checked"),
                );
                if n < 0x1_0000_0000 {
                    return Err(invalid("non-canonical CompactSize"));
                }
                n
            }
        };
        if value > u64::from(MAX_COMPACT_SIZE) {
            return Err(invalid("CompactSize too large"));
        }
        Ok(value as usize)
    }

    /// A `Vector` of fixed-size elements. Skips the elements and returns the element count.
    fn vector_fixed(&mut self, elem: usize) -> Result<usize, ParseError> {
        let count = self.compact_size()?;
        self.skip(count.checked_mul(elem).ok_or_else(eof)?)?;
        Ok(count)
    }

    /// A `Vector` of bytes (`Script::read`, Orchard proof): returns the range of the bytes.
    fn vector_bytes(&mut self) -> Result<Range<usize>, ParseError> {
        let n = self.compact_size()?;
        let start = self.pos;
        self.skip(n)?;
        Ok(start..self.pos)
    }

    /// `Transaction::read_transparent`: vin then vout. Returns the offset of the first input
    /// and the input count.
    fn transparent(&mut self) -> Result<(usize, usize), ParseError> {
        let vin_count = self.compact_size()?;
        let vin_start = self.pos;
        for _ in 0..vin_count {
            self.skip(OUTPOINT)?;
            self.vector_bytes()?; // scriptSig
            self.skip(4)?; // nSequence
        }
        let vout_count = self.compact_size()?;
        for _ in 0..vout_count {
            self.skip(AMOUNT)?;
            self.vector_bytes()?; // scriptPubKey
        }
        Ok((vin_start, vin_count))
    }

    /// `sapling_serialization::read_v5_bundle` (`components/sapling.rs`): returns the
    /// authorizing range (proofs, spend auth signatures, binding signature) and the anchor
    /// offset.
    #[allow(clippy::type_complexity)]
    fn sapling_v5(&mut self) -> Result<(Option<Range<usize>>, Option<usize>), ParseError> {
        let n_spends = self.vector_fixed(SAPLING_SPEND_V5)?;
        let n_outputs = self.vector_fixed(SAPLING_OUTPUT_V5)?;
        if n_spends + n_outputs == 0 {
            return Ok((None, None));
        }
        self.skip(AMOUNT)?; // valueBalanceSapling
        let anchor = if n_spends > 0 {
            let at = self.pos;
            self.skip(32)?;
            Some(at)
        } else {
            None
        };
        let auth_start = self.pos;
        self.skip(
            n_spends
                .checked_mul(GROTH_PROOF_SIZE + SIGNATURE)
                .and_then(|s| s.checked_add(n_outputs.checked_mul(GROTH_PROOF_SIZE)?))
                .and_then(|s| s.checked_add(SIGNATURE))
                .ok_or_else(eof)?,
        )?;
        Ok((Some(auth_start..self.pos), anchor))
    }

    /// `orchard_serialization::read_bundle` (`components/orchard.rs`): actions; then, if there
    /// are actions: flags, value balance, anchor, proof, spend auth signatures, binding
    /// signature.
    fn orchard_bundle(&mut self) -> Result<Option<OrchardRanges>, ParseError> {
        let n_actions = self.vector_fixed(ORCHARD_ACTION)?;
        if n_actions == 0 {
            return Ok(None);
        }
        self.skip(1 + AMOUNT)?; // flags, valueBalanceOrchard
        let anchor = self.pos;
        self.skip(32)?;
        let proof = self.vector_bytes()?;
        self.skip(
            n_actions
                .checked_mul(SIGNATURE)
                .and_then(|s| s.checked_add(SIGNATURE))
                .ok_or_else(eof)?,
        )?;
        Ok(Some(OrchardRanges {
            auth: proof.start..self.pos,
            anchor,
        }))
    }
}

fn eof() -> ParseError {
    ParseError::Transaction(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "transaction truncated",
    ))
}

fn invalid(msg: &'static str) -> ParseError {
    ParseError::Transaction(io::Error::new(io::ErrorKind::InvalidData, msg))
}

// ZIP 244 authorizing-data digest personalizations (`src/transaction/txid.rs` and
// orchard-0.15.5 `src/bundle/commitments.rs`).
const AUTH_PREFIX: &[u8; 12] = b"ZTxAuthHash_";
const TRANSPARENT_SCRIPTS: &[u8; 16] = b"ZTxAuthTransHash";
const SAPLING_SIGS_V5: &[u8; 16] = b"ZTxAuthSapliHash";
const SAPLING_SIGS_V6: &[u8; 16] = b"ZTxAuthSapliH_v6";
const ORCHARD_SIGS_V5: &[u8; 16] = b"ZTxAuthOrchaHash";
const ORCHARD_SIGS_V6: &[u8; 16] = b"ZTxAuthOrchaH_v6";
const IRONWOOD_SIGS_V6: &[u8; 16] = b"ZTxAuthIrnwdH_v6";

fn hasher(personal: &[u8; 16]) -> blake2b_simd::State {
    blake2b_simd::Params::new()
        .hash_length(32)
        .personal(personal)
        .to_state()
}

fn finish(state: blake2b_simd::State) -> [u8; 32] {
    state
        .finalize()
        .as_bytes()
        .try_into()
        .expect("BLAKE2b-256 digest is 32 bytes")
}

/// ZIP 244 authorizing-data commitment of a v5 or v6 transaction. It is byte-for-byte what
/// `Transaction::auth_commitment()` (`BlockTxCommitmentDigester`, `txid.rs`) returns. The
/// function computes it from the wire bytes that the scanner made `layout` from. Legacy
/// formats return the ZIP 239 placeholder.
///
/// Absent bundles hash to the bare personalization, as upstream does. `digest_transparent`
/// hashes nothing for a missing transparent bundle. `digest_sapling` hashes nothing for a
/// missing Sapling bundle. `digest_orchard`/`digest_ironwood` call `hash_bundle_auth_empty`.
pub(crate) fn auth_digest(bytes: &[u8], layout: &TxLayout) -> Result<[u8; 32], ParseError> {
    let (sapling_personal, orchard_personal) = match layout.format {
        Format::Legacy => return Ok(crate::PRE_V5_AUTH_DIGEST),
        Format::V5 => (SAPLING_SIGS_V5, ORCHARD_SIGS_V5),
        Format::V6 => (SAPLING_SIGS_V6, ORCHARD_SIGS_V6),
    };

    // T.2 of the auth digest: every scriptSig with its CompactSize prefix (`Script::write`).
    let mut transparent = hasher(TRANSPARENT_SCRIPTS);
    let mut s = Scanner {
        bytes,
        pos: layout.vin_start,
    };
    for _ in 0..layout.vin_count {
        s.skip(OUTPOINT)?;
        let start = s.pos;
        let script = s.vector_bytes()?;
        transparent.update(&bytes[start..script.end]);
        s.skip(4)?;
    }

    let mut sapling = hasher(sapling_personal);
    if let Some(range) = &layout.sapling_auth {
        sapling.update(&bytes[range.clone()]);
        if let (Format::V6, Some(anchor)) = (layout.format, layout.sapling_anchor) {
            sapling.update(&bytes[anchor..anchor + 32]);
        }
    }

    let orchard_bundle = |personal: &[u8; 16], ranges: Option<&OrchardRanges>| {
        let mut h = hasher(personal);
        if let Some(r) = ranges {
            h.update(&bytes[r.auth.clone()]);
            if layout.format == Format::V6 {
                h.update(&bytes[r.anchor..r.anchor + 32]);
            }
        }
        finish(h)
    };

    let mut personal = [0u8; 16];
    personal[..12].copy_from_slice(AUTH_PREFIX);
    personal[12..].copy_from_slice(&layout.branch_id.to_le_bytes());
    let mut h = hasher(&personal);
    h.update(&finish(transparent));
    h.update(&finish(sapling));
    h.update(&orchard_bundle(orchard_personal, layout.orchard.as_ref()));
    if layout.format == Format::V6 {
        h.update(&orchard_bundle(IRONWOOD_SIGS_V6, layout.ironwood.as_ref()));
    }
    Ok(finish(h))
}
