//! Lane evaluation: many hashes advance in lockstep on affine accumulators.
//!
//! Every lane performs its position-`i` addition in the same column. The kernel therefore
//! inverts the chord denominators `x(T) - x(B_i)` of all lanes together with Montgomery's
//! trick (one field inversion plus three multiplications per lane). Each addition is the
//! affine chord formula (2M + 1S). The accumulators finish in affine form, so the result is
//! their x-coordinate with no final normalization.
//!
//! The kernel detects the exceptional cases of `scalar.rs` per lane before the batched
//! inversion. A lane whose denominator is zero, or whose accumulator matches `x([2] T)`, gets
//! the denominator 1, so it does not poison the batch. The kernel settles that lane after the
//! column. It flags `⊥` lanes, and they output 0. It doubles the doubling lane (`B_i = T`) with
//! its own inversion.
//!
//! The layout is structure-of-arrays (one array per coordinate, words position-major). A SIMD
//! field backend can therefore process a column as vectors later.

use hayai_crypto::{ff, pasta_curves};

use ff::Field;
use pasta_curves::pallas;

use crate::invert::invert_vartime;
use crate::table::{Start, Table};
use crate::words::{merkle_crh_words, WORDS};

/// Hashes `pairs` at `level` into `out`, one chunk of lanes on the calling thread.
pub(crate) fn hash_lanes(
    table: &Table,
    level: u8,
    pairs: &[(pallas::Base, pallas::Base)],
    out: &mut [pallas::Base],
) {
    let n = pairs.len();
    assert_eq!(out.len(), n, "one output per pair");
    let (start_x, start_y) = match table.start(level) {
        Start::Bottom => {
            out.fill(pallas::Base::ZERO);
            return;
        }
        Start::Point { x, y } => (x, y),
    };

    // Words position-major: `words[position * n + lane]`.
    let mut words = vec![0u16; WORDS * n];
    for (lane, (left, right)) in pairs.iter().enumerate() {
        for (position, word) in merkle_crh_words(level, left, right).into_iter().enumerate() {
            words[position * n + lane] = word;
        }
    }

    let mut x = vec![start_x; n];
    let mut y = vec![start_y; n];
    let mut tx = vec![pallas::Base::ZERO; n];
    let mut ty = vec![pallas::Base::ZERO; n];
    let mut den = vec![pallas::Base::ZERO; n];
    let mut scratch = vec![pallas::Base::ZERO; n];
    let mut bottom = vec![false; n];
    // Lanes removed from the current column, with their accumulator from before the column ran.
    let mut exceptional: Vec<(usize, pallas::Base, pallas::Base)> = Vec::new();

    for position in 1..WORDS {
        let column = &words[position * n..(position + 1) * n];

        for lane in 0..n {
            if bottom[lane] {
                den[lane] = pallas::Base::ONE;
                continue;
            }
            let t = table.entry(position, column[lane]);
            tx[lane] = t.x;
            ty[lane] = t.y;
            let d = t.x - x[lane];
            if d.is_zero_vartime() || t.x2 == x[lane] {
                exceptional.push((lane, x[lane], y[lane]));
                den[lane] = pallas::Base::ONE;
            } else {
                den[lane] = d;
            }
        }

        batch_invert(&mut den, &mut scratch);

        let next_column = &words[(position + 1).min(WORDS - 1) * n..(position + 2).min(WORDS) * n];
        for lane in 0..n {
            if position + 1 < WORDS {
                crate::scalar::prefetch(table.entry(position + 1, next_column[lane]));
            }
            let lambda = (ty[lane] - y[lane]) * den[lane];
            let x3 = lambda.square() - x[lane] - tx[lane];
            y[lane] = lambda * (x[lane] - x3) - y[lane];
            x[lane] = x3;
        }

        for (lane, ax, ay) in exceptional.drain(..) {
            let t = table.entry(position, column[lane]);
            // Case 1 (`x(B_i) = x([2] T)`) and case 2 (`B_i = -T`) are `⊥`. `B_i = T` doubles.
            if t.x2 == ax || ay != t.y {
                bottom[lane] = true;
                continue;
            }
            let Some(inv_2y) = Option::<pallas::Base>::from(ay.double().invert()) else {
                // A point with `y = 0` has order 2, and Pallas has no such point. The kernel
                // treats it as `⊥` and does not assume it away.
                bottom[lane] = true;
                continue;
            };
            let lambda = (ax.square() * pallas::Base::from(3)) * inv_2y;
            let x3 = lambda.square() - ax.double();
            y[lane] = lambda * (ax - x3) - ay;
            x[lane] = x3;
        }
    }

    for lane in 0..n {
        out[lane] = if bottom[lane] {
            pallas::Base::ZERO
        } else {
            x[lane]
        };
    }
}

/// Inverts every element of `values` in place with one field inversion (Montgomery's trick).
///
/// Every element must be nonzero. The caller guarantees this when it substitutes 1 for the
/// exceptional lanes. The prefix products run as `CHAINS` independent strided chains. The
/// multiplications of different chains therefore overlap in the pipeline instead of one serial
/// dependency chain per pass.
fn batch_invert(values: &mut [pallas::Base], scratch: &mut [pallas::Base]) {
    const CHAINS: usize = 4;
    let n = values.len();
    debug_assert_eq!(scratch.len(), n);
    let main = n - n % CHAINS;

    let mut acc = [pallas::Base::ONE; CHAINS];
    for (vals, prefixes) in values[..main]
        .as_chunks::<CHAINS>()
        .0
        .iter()
        .zip(scratch[..main].as_chunks_mut::<CHAINS>().0.iter_mut())
    {
        for ((v, prefix), a) in vals.iter().zip(prefixes).zip(acc.iter_mut()) {
            *prefix = *a;
            *a *= v;
        }
    }
    for ((v, prefix), a) in values[main..]
        .iter()
        .zip(&mut scratch[main..])
        .zip(acc.iter_mut())
    {
        *prefix = *a;
        *a *= v;
    }

    // One inversion of the product of the chain totals, then the inverse total of each chain.
    let p01 = acc[0] * acc[1];
    let p012 = p01 * acc[2];
    let total = p012 * acc[3];
    let inv_total = invert_vartime(&total).expect("lane denominators are nonzero");
    let mut inv = [pallas::Base::ZERO; CHAINS];
    inv[3] = inv_total * p012;
    let t = inv_total * acc[3];
    inv[2] = t * p01;
    let t = t * acc[2];
    inv[1] = t * acc[0];
    inv[0] = t * acc[1];

    for ((v, prefix), a) in values[main..]
        .iter_mut()
        .zip(&scratch[main..])
        .zip(inv.iter_mut())
        .rev()
    {
        let v_inv = *a * prefix;
        *a *= *v;
        *v = v_inv;
    }
    for (vals, prefixes) in values[..main]
        .as_chunks_mut::<CHAINS>()
        .0
        .iter_mut()
        .zip(scratch[..main].as_chunks::<CHAINS>().0.iter())
        .rev()
    {
        for ((v, prefix), a) in vals.iter_mut().zip(prefixes).zip(inv.iter_mut()) {
            let v_inv = *a * prefix;
            *a *= *v;
            *v = v_inv;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_crypto::rng::{SeedableRng, StdRng};

    #[test]
    fn batch_invert_matches_single_inversions() {
        let mut rng = StdRng::seed_from_u64(1);
        for n in [1usize, 2, 3, 4, 5, 7, 8, 64, 257] {
            let values: Vec<pallas::Base> =
                (0..n).map(|_| pallas::Base::random(&mut rng)).collect();
            let mut inverted = values.clone();
            let mut scratch = vec![pallas::Base::ZERO; n];
            batch_invert(&mut inverted, &mut scratch);
            for (v, i) in values.iter().zip(&inverted) {
                assert_eq!(*i, v.invert().unwrap());
            }
        }
    }
}
