//! One hash at a time: a Jacobian accumulator and 51 mixed additions of table points.
//!
//! Exceptional cases. With `B_i = [2^(N-i)] A_i` and `T = W_i[m_i]`, the specification's step
//! `A_(i+1) = (A_i + S[m_i]) + A_i` (incomplete additions, `⊥` on identity or equal
//! x-coordinates) fails exactly in these cases:
//!
//! 1. `A_i = ±S[m_i]`, i.e. `x(B_i) = x([2] T)`;
//! 2. `[2] A_i + S[m_i] = O`, i.e. `B_i = -T`;
//! 3. `A_i = O`, which is `B_i = O` and only comes from case 2 one step earlier.
//!
//! `B_i = T` (so `[2] A_i = S[m_i]`) is not a failure of the specification. The chord formula
//! is undefined there, and the step is a doubling instead. `⊥` propagates. The hash of a
//! `⊥` message is 0 (`MerkleHashOrchard::combine` semantics).

use hayai_crypto::{ff, group, pasta_curves};

use ff::Field;
use group::Group;
use pasta_curves::{arithmetic::CurveExt, pallas};

use crate::invert::invert_vartime;
use crate::table::{Entry, Start, Table};
use crate::words::WORDS;

/// Table entries that the evaluator requests before their addition.
const PREFETCH_AHEAD: usize = 4;

#[inline(always)]
pub(crate) fn prefetch(entry: &Entry) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: prefetch is a hint without side effects on memory or control flow.
    unsafe {
        use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
        let p = entry as *const Entry as *const i8;
        _mm_prefetch(p, _MM_HINT_T0);
        _mm_prefetch(p.add(64), _MM_HINT_T0);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = entry;
}

/// Evaluates the hash of one 52-word message, 0 on `⊥`.
pub(crate) fn hash(table: &Table, words: &[u16; WORDS]) -> pallas::Base {
    let level = u8::try_from(words[0]).expect("level word fits in a byte");
    let Start::Point { x, y } = table.start(level) else {
        return pallas::Base::ZERO;
    };
    let (mut x1, mut y1, mut z1) = (x, y, pallas::Base::ONE);

    // The mixed Jacobian addition is written out here instead of pasta_curves' `Point + Affine`.
    // The `Z1^2` that it computes then also serves the case 1 test, and the code skips pasta's
    // per-operand identity checks (the accumulator and the table points are never the identity
    // here). The table is larger than L2, so the evaluator requests the entry of each step a few
    // steps ahead. The addresses depend only on the message words and not on the accumulator.
    for (position, &word) in words.iter().enumerate().take(PREFETCH_AHEAD + 1).skip(1) {
        prefetch(table.entry(position, word));
    }
    for (position, &word) in words.iter().enumerate().skip(1) {
        if position + PREFETCH_AHEAD < WORDS {
            prefetch(table.entry(position + PREFETCH_AHEAD, words[position + PREFETCH_AHEAD]));
        }
        let t = table.entry(position, word);
        let z1z1 = z1.square();
        if t.x2 * z1z1 == x1 {
            return pallas::Base::ZERO;
        }
        let u2 = t.x * z1z1;
        let s2 = t.y * z1z1 * z1;
        if u2 == x1 {
            if s2 != y1 {
                return pallas::Base::ZERO;
            }
            let doubled = pallas::Point::new_jacobian(x1, y1, z1)
                .expect("accumulator is on the curve")
                .double();
            (x1, y1, z1) = doubled.jacobian_coordinates();
            continue;
        }
        let h = u2 - x1;
        let r = s2 - y1;
        let hh = h.square();
        let hhh = h * hh;
        let v = x1 * hh;
        let x3 = r.square() - hhh - v.double();
        y1 = r * (v - x3) - y1 * hhh;
        z1 *= h;
        x1 = x3;
    }

    // Every step excluded case 2, so the accumulator is not the identity.
    let z_inv = invert_vartime(&z1).expect("accumulator is not the identity");
    x1 * z_inv.square()
}
