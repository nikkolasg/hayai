//! Variable-time inversion in the Pallas base field by Bernstein–Yang divsteps.
//!
//! Upstream `pasta_curves::Fp::invert` is a square-and-multiply exponentiation (about 380
//! multiplications, 4 µs). This module is the "safegcd" algorithm with 62-bit divstep batches,
//! the variable-time variant of libsecp256k1's `modinv64` (MIT). Its constants are specialised
//! to `p = 0x40000000000000000000000000000000224698fc094cf91b992d30ed00000001`. The module
//! handles the Montgomery form of `Fp` at the boundary: the input leaves Montgomery form
//! through `to_repr`, the module computes the inverse on plain integers, and `Fp::from_raw`
//! brings it back.
//!
//! The module verifies every result with one multiplication. On a mismatch, it uses the
//! upstream inversion and counts the event. The routine is therefore correct independently of
//! the fast path.
//!
//! Variable time is acceptable here. The inputs are Merkle-tree hash intermediates, which are
//! public data.

use std::sync::atomic::{AtomicU64, Ordering};

use hayai_crypto::{ff, pasta_curves};

use ff::{Field, PrimeField};
use pasta_curves::pallas;

/// Mask of one 62-bit limb.
const M62: u64 = u64::MAX >> 2;

/// A signed integer in five 62-bit limbs: limbs 0..4 in `[0, 2^62)`, and limb 4 carries the
/// sign.
#[derive(Clone, Copy)]
struct Signed62([i64; 5]);

/// Transition matrix of 62 divsteps.
struct Trans2x2 {
    u: i64,
    v: i64,
    q: i64,
    r: i64,
}

/// The Pallas base field modulus, little-endian 64-bit limbs.
const MODULUS: [u64; 4] = [
    0x992d_30ed_0000_0001,
    0x2246_98fc_094c_f91b,
    0x0000_0000_0000_0000,
    0x4000_0000_0000_0000,
];

const MODULUS_62: Signed62 = to_signed62(MODULUS);

/// `p^-1 mod 2^62`. `update_de` uses it to select the modulus multiple that clears the low
/// 62 bits. The module does not need `-p^-1 mod 2^62`.
const MODULUS_INV62: u64 = inverse_mod_2_62(MODULUS[0]);

const fn inverse_mod_2_62(p0: u64) -> u64 {
    // Newton iteration doubles the number of correct bits. p0 is odd, so it starts with 1 bit.
    let mut x = p0;
    let mut i = 0;
    while i < 6 {
        x = x.wrapping_mul(2u64.wrapping_sub(p0.wrapping_mul(x)));
        i += 1;
    }
    x & M62
}

const fn to_signed62(a: [u64; 4]) -> Signed62 {
    Signed62([
        (a[0] & M62) as i64,
        ((a[0] >> 62 | a[1] << 2) & M62) as i64,
        ((a[1] >> 60 | a[2] << 4) & M62) as i64,
        ((a[2] >> 58 | a[3] << 6) & M62) as i64,
        (a[3] >> 56) as i64,
    ])
}

fn from_signed62(v: &Signed62) -> [u64; 4] {
    let a = v.0.map(|limb| limb as u64);
    [
        a[0] | a[1] << 62,
        a[1] >> 2 | a[2] << 60,
        a[2] >> 4 | a[3] << 58,
        a[3] >> 6 | a[4] << 56,
    ]
}

static FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// Number of times the fast path produced a wrong inverse and the module used the upstream
/// inversion.
pub fn invert_fallbacks() -> u64 {
    FALLBACKS.load(Ordering::Relaxed)
}

/// The inverse of `x`, `None` for zero.
pub fn invert_vartime(x: &pallas::Base) -> Option<pallas::Base> {
    if x.is_zero_vartime() {
        return None;
    }
    let repr = x.to_repr();
    let mut limbs = [0u64; 4];
    for (limb, chunk) in limbs.iter_mut().zip(repr.as_chunks::<8>().0) {
        *limb = u64::from_le_bytes(*chunk);
    }
    let candidate = pallas::Base::from_raw(from_signed62(&modinv(to_signed62(limbs))));
    if candidate * x == pallas::Base::ONE {
        return Some(candidate);
    }
    FALLBACKS.fetch_add(1, Ordering::Relaxed);
    Option::from(x.invert())
}

/// Inverse of `x` (in `[1, p)`) modulo `p`, normalised to `[0, p)`.
fn modinv(x: Signed62) -> Signed62 {
    let mut d = Signed62([0; 5]);
    let mut e = Signed62([1, 0, 0, 0, 0]);
    let mut f = MODULUS_62;
    let mut g = x;
    let mut len = 5usize;
    // eta = -delta. delta starts at 1.
    let mut eta: i64 = -1;

    loop {
        let (t, next_eta) = divsteps_62_var(eta, f.0[0] as u64, g.0[0] as u64);
        eta = next_eta;
        update_de_62(&mut d, &mut e, &t);
        update_fg_62_var(len, &mut f, &mut g, &t);

        if g.0[0] == 0 && g.0[1..len].iter().all(|limb| *limb == 0) {
            break;
        }

        // Drop the top limb once both top limbs of f and g only repeat the sign below.
        let fn_ = f.0[len - 1];
        let gn = g.0[len - 1];
        let mut cond = ((len as i64) - 2) >> 63;
        cond |= fn_ ^ (fn_ >> 63);
        cond |= gn ^ (gn >> 63);
        if cond == 0 {
            f.0[len - 2] |= ((fn_ as u64) << 62) as i64;
            g.0[len - 2] |= ((gn as u64) << 62) as i64;
            len -= 1;
        }
    }

    // g is zero, so f is ±gcd(x, p) = ±1. Its sign decides whether the code must negate d.
    normalize_62(&mut d, f.0[len - 1]);
    d
}

/// 62 divsteps on the low limbs. Returns the transition matrix and the new eta.
fn divsteps_62_var(mut eta: i64, f0: u64, g0: u64) -> (Trans2x2, i64) {
    let (mut u, mut v, mut q, mut r) = (1u64, 0u64, 0u64, 1u64);
    let (mut f, mut g) = (f0, g0);
    let mut i = 62i32;

    loop {
        // A sentinel bit limits the zero count to the divsteps that remain.
        let zeros = (g | (u64::MAX << i)).trailing_zeros() as i32;
        g >>= zeros;
        u <<= zeros;
        v <<= zeros;
        eta -= i64::from(zeros);
        i -= zeros;
        if i == 0 {
            break;
        }
        debug_assert!(f & 1 == 1 && g & 1 == 1);

        let (limit, m, w);
        if eta < 0 {
            eta = -eta;
            (f, g) = (g, f.wrapping_neg());
            (u, q) = (q, u.wrapping_neg());
            (v, r) = (r, v.wrapping_neg());
            // At most min(eta + 1, i) bits can be cancelled before the sign of eta flips again.
            // This formula clears up to 6 of them (f * (f*f - 2) is f^-1 mod 64).
            limit = if eta + 1 > i64::from(i) {
                i
            } else {
                (eta + 1) as i32
            };
            m = (u64::MAX >> (64 - limit)) & 63;
            w = f
                .wrapping_mul(g)
                .wrapping_mul(f.wrapping_mul(f).wrapping_sub(2))
                & m;
        } else {
            // The same with up to 4 bits. eta is usually small here.
            limit = if eta + 1 > i64::from(i) {
                i
            } else {
                (eta + 1) as i32
            };
            m = (u64::MAX >> (64 - limit)) & 15;
            let w0 = f.wrapping_add((f.wrapping_add(1) & 4) << 1);
            w = w0.wrapping_neg().wrapping_mul(g) & m;
        }
        g = g.wrapping_add(f.wrapping_mul(w));
        q = q.wrapping_add(u.wrapping_mul(w));
        r = r.wrapping_add(v.wrapping_mul(w));
        debug_assert_eq!(g & m, 0);
    }

    (
        Trans2x2 {
            u: u as i64,
            v: v as i64,
            q: q as i64,
            r: r as i64,
        },
        eta,
    )
}

/// `[d, e] = (t * [d, e] + p * [md, me]) / 2^62` with `md, me` chosen so the division is exact.
/// Keeps `d, e` in `(-2p, p)`.
fn update_de_62(d: &mut Signed62, e: &mut Signed62, t: &Trans2x2) {
    let (u, v, q, r) = (
        i128::from(t.u),
        i128::from(t.v),
        i128::from(t.q),
        i128::from(t.r),
    );
    let p = MODULUS_62.0.map(i128::from);
    let dv = d.0.map(i128::from);
    let ev = e.0.map(i128::from);

    // [md, me] start as [u, q] if d is negative plus [v, r] if e is negative.
    let sd = d.0[4] >> 63;
    let se = e.0[4] >> 63;
    let mut md = (t.u & sd).wrapping_add(t.v & se);
    let mut me = (t.q & sd).wrapping_add(t.r & se);

    let mut cd = u * dv[0] + v * ev[0];
    let mut ce = q * dv[0] + r * ev[0];
    // Correct md, me so that the sums have 62 zero bottom bits.
    md = md.wrapping_sub(
        (MODULUS_INV62
            .wrapping_mul(cd as u64)
            .wrapping_add(md as u64)
            & M62) as i64,
    );
    me = me.wrapping_sub(
        (MODULUS_INV62
            .wrapping_mul(ce as u64)
            .wrapping_add(me as u64)
            & M62) as i64,
    );
    let (md, me) = (i128::from(md), i128::from(me));
    cd += p[0] * md;
    ce += p[0] * me;
    debug_assert_eq!(cd as u64 & M62, 0);
    debug_assert_eq!(ce as u64 & M62, 0);
    cd >>= 62;
    ce >>= 62;

    for i in 1..5 {
        cd += u * dv[i] + v * ev[i] + p[i] * md;
        ce += q * dv[i] + r * ev[i] + p[i] * me;
        d.0[i - 1] = (cd as u64 & M62) as i64;
        e.0[i - 1] = (ce as u64 & M62) as i64;
        cd >>= 62;
        ce >>= 62;
    }
    d.0[4] = cd as i64;
    e.0[4] = ce as i64;
}

/// `[f, g] = t * [f, g] / 2^62` over the low `len` limbs. The division is exact by
/// construction of `t`.
fn update_fg_62_var(len: usize, f: &mut Signed62, g: &mut Signed62, t: &Trans2x2) {
    let (u, v, q, r) = (
        i128::from(t.u),
        i128::from(t.v),
        i128::from(t.q),
        i128::from(t.r),
    );
    let (fi, gi) = (i128::from(f.0[0]), i128::from(g.0[0]));
    let mut cf = u * fi + v * gi;
    let mut cg = q * fi + r * gi;
    debug_assert_eq!(cf as u64 & M62, 0);
    debug_assert_eq!(cg as u64 & M62, 0);
    cf >>= 62;
    cg >>= 62;
    for i in 1..len {
        let (fi, gi) = (i128::from(f.0[i]), i128::from(g.0[i]));
        cf += u * fi + v * gi;
        cg += q * fi + r * gi;
        f.0[i - 1] = (cf as u64 & M62) as i64;
        g.0[i - 1] = (cg as u64 & M62) as i64;
        cf >>= 62;
        cg >>= 62;
    }
    f.0[len - 1] = cf as i64;
    g.0[len - 1] = cg as i64;
}

/// Brings `r` from `(-2p, p)` to `[0, p)`. It negates `r` first when `sign` is negative.
fn normalize_62(r: &mut Signed62, sign: i64) {
    let p = MODULUS_62.0;
    let v = &mut r.0;

    // Add p if negative, then negate if `sign` is negative: (-2p, p) -> (-p, p).
    let cond_add = v[4] >> 63;
    for i in 0..5 {
        v[i] = v[i].wrapping_add(p[i] & cond_add);
    }
    let cond_negate = sign >> 63;
    for limb in v.iter_mut() {
        *limb = (*limb ^ cond_negate).wrapping_sub(cond_negate);
    }
    propagate(v);

    // Add p once more if still negative: (-p, p) -> [0, p).
    let cond_add = v[4] >> 63;
    for i in 0..5 {
        v[i] = v[i].wrapping_add(p[i] & cond_add);
    }
    propagate(v);
    debug_assert!(v[4] >= 0);
}

/// Carries the top bits of each limb into the next limb. Limbs 0..4 then return to
/// `[0, 2^62)`.
fn propagate(v: &mut [i64; 5]) {
    for i in 0..4 {
        v[i + 1] = v[i + 1].wrapping_add(v[i] >> 62);
        v[i] &= M62 as i64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_crypto::rng::{SeedableRng, StdRng};
    use proptest::prelude::*;

    fn check(x: pallas::Base) {
        let expected: Option<pallas::Base> = Option::from(x.invert());
        assert_eq!(invert_vartime(&x), expected, "{x:?}");
    }

    #[test]
    fn constants() {
        assert_eq!(MODULUS_INV62.wrapping_mul(MODULUS[0]) & M62, 1);
        assert_eq!(from_signed62(&MODULUS_62), MODULUS);
        let minus_one = -pallas::Base::ONE;
        let mut limbs = [0u64; 4];
        for (limb, chunk) in limbs.iter_mut().zip(minus_one.to_repr().as_chunks::<8>().0) {
            *limb = u64::from_le_bytes(*chunk);
        }
        assert_eq!(limbs, [MODULUS[0] - 1, MODULUS[1], MODULUS[2], MODULUS[3]]);
    }

    #[test]
    fn zero_is_none() {
        assert_eq!(invert_vartime(&pallas::Base::ZERO), None);
    }

    #[test]
    fn edge_values() {
        check(pallas::Base::ONE);
        check(-pallas::Base::ONE);
        check(pallas::Base::from(2));
        check(-pallas::Base::from(2));
        for small in 1u64..=64 {
            check(pallas::Base::from(small));
            check(-pallas::Base::from(small));
        }
        let mut power = pallas::Base::ONE;
        for _ in 0..=256 {
            check(power);
            check(-power);
            power = power.double();
        }
        // Elements whose Montgomery representation is small (R^-1, R^-2: limbs 1 and R^-1) or
        // whose value is R, R^2.
        let r = pallas::Base::from(2).pow_vartime([256u64]);
        let one_over_r = r.invert().unwrap();
        check(one_over_r);
        check(one_over_r.square());
        check(r);
        check(r.square());
        check(pallas::Base::from_raw([
            MODULUS[0] - 1,
            MODULUS[1],
            MODULUS[2],
            MODULUS[3],
        ]));
        check(pallas::Base::from_u128(u128::MAX));
        check(pallas::Base::from_raw([
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX >> 2,
        ]));
        assert_eq!(invert_fallbacks(), 0);
    }

    #[test]
    fn random_values_match_upstream() {
        let mut rng = StdRng::seed_from_u64(0x1234_5678);
        let count = if cfg!(debug_assertions) {
            2_000
        } else {
            100_000
        };
        for _ in 0..count {
            check(pallas::Base::random(&mut rng));
        }
        assert_eq!(invert_fallbacks(), 0);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn prop_matches_upstream(limbs in any::<[u64; 4]>()) {
            check(pallas::Base::from_raw(limbs));
            prop_assert_eq!(invert_fallbacks(), 0);
        }
    }
}
