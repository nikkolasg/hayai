//! MerkleCRH^Orchard with position-weighted tables and batch-affine lanes.
//!
//! `MerkleCRH^Orchard(l, left, right) = SinsemillaHash("z.cash:Orchard-MerkleCRH", l || left || right)`
//! with the x-coordinate extracted and `⊥` mapped to 0, exactly as
//! `orchard::tree::MerkleHashOrchard::combine` computes it. The table (`table.rs`) folds the
//! doublings of the Sinsemilla recurrence into position-weighted generators. A hash is
//! therefore 52 point additions. The evaluators detect and resolve the exceptional cases of
//! incomplete addition (`scalar.rs`). The result therefore equals the specification on every
//! input, and it does not rely on a discrete-logarithm argument.
//!
//! Two evaluators share the table. [`merkle_crh_orchard`] hashes one input with a Jacobian
//! accumulator. [`merkle_crh_orchard_lanes`] hashes a batch in lockstep on affine
//! accumulators with one batched inversion per addition column. [`merkle_crh_orchard_many`]
//! selects one of them per input size and spreads the work over the rayon pool.

mod invert;
mod lanes;
mod scalar;
mod table;
mod words;

use hayai_crypto::{ff, pasta_curves};

use ff::Field;
use pasta_curves::pallas;
use rayon::prelude::*;

pub use invert::{invert_fallbacks, invert_vartime};
pub use table::{Table, MERKLE_CRH_PERSONALIZATION, MERKLE_DEPTH_ORCHARD};
pub use words::{merkle_crh_words, K, L_ORCHARD_MERKLE, WORDS};

/// Lanes per thread below which the Jacobian path is faster than the lane path. Each lane
/// column pays its share of one field inversion. The lane path therefore wins only when that
/// share is smaller than the multiplications that it saves per addition. Measured on a Ryzen
/// 9950X with `invert_vartime`: 12 lanes break even with the scalar path, and 16 lanes are
/// 12 % faster.
const LANES_PER_THREAD_MIN: usize = 16;

/// Upper bound on the lanes of one chunk. It keeps the working set of the chunk (six
/// field-element arrays plus the words) in L2 next to the 96 KiB table row of the current
/// column.
const LANES_MAX: usize = 1024;

/// Pairs below which `merkle_crh_orchard_many` hashes sequentially and not through rayon.
const PARALLEL_MIN: usize = 4;

/// `MerkleCRH^Orchard(level, left, right)`.
///
/// `level` is the `incrementalmerkletree::Level` of the children (the word `l` of the
/// specification, `0` for two leaves). It must be below [`MERKLE_DEPTH_ORCHARD`].
pub fn merkle_crh_orchard(level: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    check_level(level);
    scalar::hash(Table::orchard(), &merkle_crh_words(level, &left, &right))
}

/// `MerkleCRH^Orchard` of every pair at `level`, in input order.
///
/// Selects the evaluator from the number of pairs per rayon thread. Small inputs hash with
/// [`merkle_crh_orchard`] in parallel. The function cuts large inputs into lane chunks of equal
/// size, one per thread, and evaluates each chunk with [`merkle_crh_orchard_lanes`].
pub fn merkle_crh_orchard_many(
    level: u8,
    pairs: &[(pallas::Base, pallas::Base)],
) -> Vec<pallas::Base> {
    check_level(level);
    let n = pairs.len();
    let threads = rayon::current_num_threads().max(1);
    let table = Table::orchard();
    let mut out = vec![pallas::Base::ZERO; n];
    if n < LANES_PER_THREAD_MIN * threads {
        let one = |(left, right): &(pallas::Base, pallas::Base)| {
            scalar::hash(table, &merkle_crh_words(level, left, right))
        };
        if n < PARALLEL_MIN {
            for (o, pair) in out.iter_mut().zip(pairs) {
                *o = one(pair);
            }
        } else {
            out.par_iter_mut()
                .zip(pairs.par_iter())
                .for_each(|(o, pair)| *o = one(pair));
        }
    } else {
        let chunk = n.div_ceil(threads).min(LANES_MAX);
        out.par_chunks_mut(chunk)
            .zip(pairs.par_chunks(chunk))
            .for_each(|(out, pairs)| lanes::hash_lanes(table, level, pairs, out));
    }
    out
}

/// `MerkleCRH^Orchard` of every pair at `level` on the calling thread: the lane kernel from
/// `LANES_PER_THREAD_MIN` pairs, the scalar path below.
///
/// This is what [`merkle_crh_orchard_many`] runs per thread. Callers that already distribute
/// work over a pool (one Merkle subtree per task) use it directly.
pub fn merkle_crh_orchard_local(
    level: u8,
    pairs: &[(pallas::Base, pallas::Base)],
) -> Vec<pallas::Base> {
    check_level(level);
    let table = Table::orchard();
    let mut out = vec![pallas::Base::ZERO; pairs.len()];
    if pairs.len() >= LANES_PER_THREAD_MIN {
        lanes::hash_lanes(table, level, pairs, &mut out);
    } else {
        for (o, (left, right)) in out.iter_mut().zip(pairs) {
            *o = scalar::hash(table, &merkle_crh_words(level, left, right));
        }
    }
    out
}

/// `MerkleCRH^Orchard` of every pair at `level` as one lane chunk on the calling thread, for
/// any lane count. It is public so that a benchmark can measure the per-hash cost of the
/// kernel.
pub fn merkle_crh_orchard_lanes(
    level: u8,
    pairs: &[(pallas::Base, pallas::Base)],
) -> Vec<pallas::Base> {
    check_level(level);
    let mut out = vec![pallas::Base::ZERO; pairs.len()];
    lanes::hash_lanes(Table::orchard(), level, pairs, &mut out);
    out
}

/// Bytes that the MerkleCRH^Orchard table holds (built on first use).
pub fn table_bytes() -> usize {
    Table::orchard().bytes()
}

fn check_level(level: u8) {
    assert!(
        usize::from(level) < MERKLE_DEPTH_ORCHARD,
        "MerkleCRH^Orchard level {level} is outside 0..{MERKLE_DEPTH_ORCHARD}"
    );
}

#[cfg(test)]
mod tests;
