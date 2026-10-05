//! Differences between hayai and the reference that are known and are not defects of the
//! block validation of hayai. `docs/fuzz-findings.md` has one entry for each.
//!
//! Each difference has a test on the case: the fuzzer removes the cause from the block,
//! and the difference is known only when the two implementations then agree. A text of
//! an error never decides.

use crate::mutate::Case;
use crate::verdict::{compare, Verdict};
use crate::{hayai_side, reference};

/// Bytes after the block. The block message decoder of Zakura ignores them. The parser of
/// hayai refuses them.
pub const TRAILING_BYTES: &str = "trailing-bytes";
/// A header version below 4 as a signed number. The block cases run hayai without its
/// header rules, and the parser of the reference has the version rule. The class
/// `header-context` compares the rule.
pub const HEADER_VERSION: &str = "header-version-in-block-case";

fn agree(bytes: &[u8], case: &Case) -> bool {
    let hayai = hayai_side::check_block(bytes, &case.ctx);
    let reference = reference::check_block(bytes, &case.ctx);
    !compare(&hayai, &reference).is_finding()
}

/// The name of the known difference that explains the two verdicts of `case`, if one
/// does.
pub fn known_difference(case: &Case, hayai: &Verdict, reference: &Verdict) -> Option<&'static str> {
    if let (Verdict::Reject { .. }, Verdict::Accept) = (hayai, reference) {
        if let Some(len) = reference::block_len(&case.bytes) {
            if len < case.bytes.len() && agree(&case.bytes[..len], case) {
                return Some(TRAILING_BYTES);
            }
        }
    }
    if let (Verdict::Accept, Verdict::Reject { .. }) = (hayai, reference) {
        if let Some(version) = case.bytes.get(..4) {
            let version = i32::from_le_bytes(version.try_into().expect("4 bytes"));
            if version < 4 {
                let mut patched = case.bytes.clone();
                patched[..4].copy_from_slice(&4u32.to_le_bytes());
                if agree(&patched, case) {
                    return Some(HEADER_VERSION);
                }
            }
        }
    }
    None
}
