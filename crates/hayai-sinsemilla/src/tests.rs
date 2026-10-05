use hayai_crypto::rng::{RngCore, SeedableRng, StdRng};
use hayai_crypto::{ff, group, incrementalmerkletree, orchard, pasta_curves, sinsemilla};

use ff::{Field, FromUniformBytes, PrimeField};
use group::{Curve, Group};
use incrementalmerkletree::{Hashable, Level};
use orchard::tree::MerkleHashOrchard;
use pasta_curves::{
    arithmetic::{CurveAffine, CurveExt},
    pallas,
};
use proptest::prelude::*;

use crate::table::{Start, Table, GENERATORS};
use crate::{
    lanes, merkle_crh_orchard, merkle_crh_orchard_lanes, merkle_crh_orchard_many, merkle_crh_words,
    scalar, table_bytes, LANES_PER_THREAD_MIN, MERKLE_DEPTH_ORCHARD, WORDS,
};

fn upstream(level: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let node = |x: pallas::Base| MerkleHashOrchard::from_bytes(&x.to_repr()).unwrap();
    let out = MerkleHashOrchard::combine(Level::from(level), &node(left), &node(right));
    pallas::Base::from_repr(out.to_bytes()).unwrap()
}

fn edge_values() -> Vec<pallas::Base> {
    vec![
        pallas::Base::ZERO,
        pallas::Base::ONE,
        pallas::Base::from(2),
        -pallas::Base::ONE,
        -pallas::Base::from(2),
        pallas::Base::from(2).invert().unwrap(),
        pallas::Base::from_u128(u128::MAX),
    ]
}

fn random_level(rng: &mut StdRng) -> u8 {
    (rng.next_u32() % MERKLE_DEPTH_ORCHARD as u32) as u8
}

fn random_pairs(rng: &mut StdRng, n: usize) -> Vec<(pallas::Base, pallas::Base)> {
    (0..n)
        .map(|_| {
            (
                pallas::Base::random(&mut *rng),
                pallas::Base::random(&mut *rng),
            )
        })
        .collect()
}

#[test]
fn s_table_matches_the_specification() {
    let s_hash = pallas::Point::hash_to_curve(sinsemilla::S_PERSONALIZATION);
    for (j, (x, y)) in sinsemilla::SINSEMILLA_S.iter().enumerate() {
        let expected = s_hash(&(j as u32).to_le_bytes()).to_affine();
        assert_eq!(pallas::Affine::from_xy(*x, *y).unwrap(), expected, "S[{j}]");
    }
}

#[test]
fn table_size_is_as_documented() {
    let rows = (WORDS - 1) * GENERATORS * 96;
    assert_eq!(rows, 5_013_504);
    assert_eq!(
        table_bytes(),
        rows + MERKLE_DEPTH_ORCHARD * std::mem::size_of::<Start>()
    );
    for level in 0..MERKLE_DEPTH_ORCHARD as u8 {
        assert!(matches!(Table::orchard().start(level), Start::Point { .. }));
    }
}

#[test]
fn scalar_matches_upstream_on_random_inputs_at_every_level() {
    let mut rng = StdRng::seed_from_u64(0x5a11);
    for level in 0..MERKLE_DEPTH_ORCHARD as u8 {
        for _ in 0..8 {
            let (l, r) = (
                pallas::Base::random(&mut rng),
                pallas::Base::random(&mut rng),
            );
            assert_eq!(merkle_crh_orchard(level, l, r), upstream(level, l, r));
        }
    }
}

#[test]
fn scalar_matches_upstream_on_edge_inputs() {
    let edge = edge_values();
    for level in [0u8, 1, 15, 31] {
        for l in &edge {
            for r in &edge {
                assert_eq!(merkle_crh_orchard(level, *l, *r), upstream(level, *l, *r));
            }
        }
    }
    let mut rng = StdRng::seed_from_u64(3);
    for level in 0..MERKLE_DEPTH_ORCHARD as u8 {
        let v = pallas::Base::random(&mut rng);
        assert_eq!(merkle_crh_orchard(level, v, v), upstream(level, v, v));
    }
}

#[test]
fn empty_roots_match_upstream() {
    let mut node = pallas::Base::from(2);
    for level in 0..MERKLE_DEPTH_ORCHARD as u8 {
        node = merkle_crh_orchard(level, node, node);
        let expected = MerkleHashOrchard::empty_root(Level::from(level + 1));
        assert_eq!(node.to_repr(), expected.to_bytes(), "level {level}");
    }
}

#[test]
fn lanes_match_scalar_for_every_width() {
    let mut rng = StdRng::seed_from_u64(0x1a4e);
    for n in [1usize, 2, 3, 31, 32, 33, 64, 100, 257] {
        let level = random_level(&mut rng);
        let pairs = random_pairs(&mut rng, n);
        let got = merkle_crh_orchard_lanes(level, &pairs);
        for (i, (l, r)) in pairs.iter().enumerate() {
            assert_eq!(got[i], merkle_crh_orchard(level, *l, *r), "n={n} lane={i}");
        }
    }
    let edge = edge_values();
    let pairs: Vec<_> = edge
        .iter()
        .flat_map(|l| edge.iter().map(move |r| (*l, *r)))
        .collect();
    let got = merkle_crh_orchard_lanes(0, &pairs);
    for (i, (l, r)) in pairs.iter().enumerate() {
        assert_eq!(got[i], upstream(0, *l, *r), "edge lane {i}");
    }
}

#[test]
fn many_matches_upstream_across_both_evaluators() {
    let mut rng = StdRng::seed_from_u64(0xa11);
    let threads = rayon::current_num_threads();
    for n in [0usize, 1, 3, 4, 65, LANES_PER_THREAD_MIN * threads + 7] {
        let level = random_level(&mut rng);
        let pairs = random_pairs(&mut rng, n);
        let got = merkle_crh_orchard_many(level, &pairs);
        assert_eq!(got.len(), n);
        // The test checks the large case against the scalar path (already proven equal to
        // upstream). It spot-checks against upstream, so the test stays fast.
        for (i, (l, r)) in pairs.iter().enumerate() {
            assert_eq!(got[i], merkle_crh_orchard(level, *l, *r), "n={n} i={i}");
        }
        for (i, (l, r)) in pairs.iter().enumerate().step_by(97) {
            assert_eq!(got[i], upstream(level, *l, *r), "n={n} i={i}");
        }
    }
}

#[test]
#[should_panic(expected = "outside 0..32")]
fn level_out_of_range_panics() {
    let _ = merkle_crh_orchard(32, pallas::Base::ZERO, pallas::Base::ZERO);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn prop_scalar_and_lanes_match_upstream(
        level in 0u8..MERKLE_DEPTH_ORCHARD as u8,
        l in any::<[u8; 32]>(),
        r in any::<[u8; 32]>(),
    ) {
        let (l, r) = (pallas::Base::from_uniform_bytes(&pad(l)), pallas::Base::from_uniform_bytes(&pad(r)));
        let expected = upstream(level, l, r);
        prop_assert_eq!(merkle_crh_orchard(level, l, r), expected);
        prop_assert_eq!(merkle_crh_orchard_lanes(level, &[(l, r); 2])[1], expected);
    }
}

fn pad(bytes: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&bytes);
    out[32..].copy_from_slice(&bytes);
    out
}

// Exceptional cases. For a message `m` and a position `i`, `A_i = [2^i] Q + sum` where
// `sum = Σ_{t<i} [2^(i-1-t)] S[m_t]`. The selection `Q = [2^-i] (target - sum)` therefore
// puts the specification's accumulator exactly on `target` at step `i`.

#[derive(Clone, Copy, Debug)]
enum Target {
    /// `A_i = S[m_i]`: `⊥`.
    PlusS,
    /// `A_i = -S[m_i]`: `⊥`.
    MinusS,
    /// `[2] A_i + S[m_i] = O`: `⊥`.
    MinusHalfS,
    /// `[2] A_i = S[m_i]`: a doubling in the weighted evaluation. It is not `⊥`.
    HalfS,
}

fn generator(j: u16) -> pallas::Point {
    let (x, y) = sinsemilla::SINSEMILLA_S[usize::from(j)];
    pallas::Point::from(pallas::Affine::from_xy(x, y).unwrap())
}

fn q_for(words: &[u16; WORDS], position: usize, target: Target) -> pallas::Point {
    let mut sum = pallas::Point::identity();
    for &w in &words[..position] {
        sum = sum.double() + generator(w);
    }
    let s = generator(words[position]);
    let half = pallas::Scalar::from(2).invert().unwrap();
    let target = match target {
        Target::PlusS => s,
        Target::MinusS => -s,
        Target::MinusHalfS => -(s * half),
        Target::HalfS => s * half,
    };
    let inv_2i = pallas::Scalar::from(2)
        .pow_vartime([position as u64])
        .invert()
        .unwrap();
    (target - sum) * inv_2i
}

fn reference(q: pallas::Point, words: &[u16; WORDS]) -> Option<pallas::Base> {
    let bits = words
        .iter()
        .flat_map(|w| (0..crate::K).map(move |b| (w >> b) & 1 == 1));
    sinsemilla::HashDomain::from_Q(q).hash(bits).into()
}

#[test]
fn exceptional_cases_follow_the_specification() {
    let mut rng = StdRng::seed_from_u64(0xb0770);
    for position in [0usize, 1, 2, 26, 50, 51] {
        for target in [
            Target::PlusS,
            Target::MinusS,
            Target::MinusHalfS,
            Target::HalfS,
        ] {
            let level = random_level(&mut rng);
            let (l, r) = (
                pallas::Base::random(&mut rng),
                pallas::Base::random(&mut rng),
            );
            let words = merkle_crh_words(level, &l, &r);
            let q = q_for(&words, position, target);
            let table = Table::new(q);
            let expected = reference(q, &words);
            let context = format!("position {position} target {target:?}");
            let bottom = !matches!(target, Target::HalfS);
            assert!(
                matches!((bottom, &expected), (true, None) | (false, Some(_))),
                "{context}"
            );
            let expected = expected.unwrap_or(pallas::Base::ZERO);

            assert_eq!(scalar::hash(&table, &words), expected, "scalar {context}");

            // The exceptional lane sits among ordinary lanes of the same custom-Q domain.
            let mut pairs = random_pairs(&mut rng, 5);
            pairs.insert(2, (l, r));
            let mut out = vec![pallas::Base::ZERO; pairs.len()];
            lanes::hash_lanes(&table, level, &pairs, &mut out);
            for (i, (pl, pr)) in pairs.iter().enumerate() {
                let w = merkle_crh_words(level, pl, pr);
                let e = reference(q, &w).unwrap_or(pallas::Base::ZERO);
                assert_eq!(out[i], e, "lane {i} {context}");
            }
        }
    }
}

#[test]
fn identity_q_is_bottom_everywhere() {
    let table = Table::new(pallas::Point::identity());
    for level in 0..MERKLE_DEPTH_ORCHARD as u8 {
        assert!(matches!(table.start(level), Start::Bottom));
    }
    let words = merkle_crh_words(3, &pallas::Base::ONE, &pallas::Base::ONE);
    assert_eq!(scalar::hash(&table, &words), pallas::Base::ZERO);
    assert_eq!(reference(pallas::Point::identity(), &words), None);
}
