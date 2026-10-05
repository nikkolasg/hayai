//! Position-weighted Sinsemilla tables.
//!
//! The specification evaluates `A_0 = Q`, `A_(i+1) = [2] A_i + S[m_i]` over the `N = 52`
//! message words and outputs `A_N`. With `B_i = [2^(N-i)] A_i`, the recurrence becomes
//! `B_(i+1) = B_i + [2^(N-1-i)] S[m_i]` and `B_N = A_N`. A hash is therefore 52 additions of
//! table points and no doublings. Position `i` (1-based after the fused first step) reads the
//! row `W_i[j] = [2^(N-1-i)] S[j]`.
//!
//! The rows also carry the x-coordinate of `[2] W_i[j] = [2^(N-i)] S[j]`. The specification
//! returns `⊥` when `A_i = ±S[m_i]`, which is `B_i = ±[2] W_i[m_i]`. The evaluator detects
//! this case when it compares x-coordinates. The chord addition itself has two more
//! exceptional cases (`B_i = W_i[m_i]`: doubling; `B_i = -W_i[m_i]`: `A_(i+1) = O`, which is
//! `⊥`). With these three cases, the evaluation equals the specification on every input. See
//! `scalar.rs` for the derivation of the three cases.
//!
//! Memory: 51 rows x 1024 entries x 96 bytes = 5,013,504 bytes (4.8 MiB) for the rows. Every
//! table shares the rows. Each table adds 32 start points. The rows are position-major, so the
//! lanes of one addition column read from one 96 KiB row.

use std::sync::OnceLock;

use hayai_crypto::{group, pasta_curves, sinsemilla};

use group::{Curve, Group};
use pasta_curves::{
    arithmetic::{Coordinates, CurveAffine, CurveExt},
    pallas,
};

use crate::words::{K, WORDS};

/// Number of Sinsemilla generators `S[j]`.
pub(crate) const GENERATORS: usize = 1 << K;

/// Levels that a MerkleCRH^Orchard input can carry: `0..MERKLE_DEPTH_ORCHARD`.
pub const MERKLE_DEPTH_ORCHARD: usize = 32;

/// Personalization of the Q point of MerkleCRH^Orchard.
pub const MERKLE_CRH_PERSONALIZATION: &str = "z.cash:Orchard-MerkleCRH";

/// One table point `T = [2^e] S[j]` with the x-coordinate of `[2] T`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub x: pallas::Base,
    pub y: pallas::Base,
    /// x-coordinate of `[2] T`. The evaluator compares it against the accumulator for the
    /// `A_i = ±S[m_i]` case.
    pub x2: pallas::Base,
}

/// The Q-independent rows for positions `1..WORDS`.
pub(crate) struct Rows(Box<[Entry]>);

impl Rows {
    fn build() -> Self {
        // `affine[e][j] = [2^e] S[j]` for `e` in `0..=WORDS-1`.
        let affine = weighted_generators();
        let mut entries = Vec::with_capacity((WORDS - 1) * GENERATORS);
        for position in 1..WORDS {
            let exponent = WORDS - 1 - position;
            for (point, doubled) in affine[exponent].iter().zip(&affine[exponent + 1]) {
                let (x, y) = xy(point);
                let (x2, _) = xy(doubled);
                entries.push(Entry { x, y, x2 });
            }
        }
        Rows(entries.into_boxed_slice())
    }

    /// The point that the evaluator adds at `position` (in `1..WORDS`) for message word `word`.
    #[inline(always)]
    pub fn entry(&self, position: usize, word: u16) -> &Entry {
        debug_assert!((1..WORDS).contains(&position));
        &self.0[(position - 1) * GENERATORS + usize::from(word)]
    }

    pub fn bytes(&self) -> usize {
        std::mem::size_of_val::<[Entry]>(&self.0)
    }
}

/// The accumulator after the first word, `[2^N] Q + [2^(N-1)] S[l]`, or `⊥` when the
/// specification already fails on the first step for this level.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Start {
    Point { x: pallas::Base, y: pallas::Base },
    Bottom,
}

/// A MerkleCRH table for one Q point.
pub struct Table {
    rows: &'static Rows,
    start: [Start; MERKLE_DEPTH_ORCHARD],
}

static ROWS: OnceLock<Rows> = OnceLock::new();
static ORCHARD: OnceLock<Table> = OnceLock::new();

impl Table {
    /// The table of the `z.cash:Orchard-MerkleCRH` domain. It is built on first use.
    pub fn orchard() -> &'static Table {
        ORCHARD.get_or_init(|| {
            let q = pallas::Point::hash_to_curve(sinsemilla::Q_PERSONALIZATION)(
                MERKLE_CRH_PERSONALIZATION.as_bytes(),
            );
            Table::new(q)
        })
    }

    /// Builds the table for an arbitrary Q. The tests use this to reach the exceptional cases.
    pub fn new(q: pallas::Point) -> Self {
        let rows = ROWS.get_or_init(Rows::build);
        let q_x: Option<pallas::Base> = Option::from(q.to_affine().coordinates().map(|c| *c.x()));
        let mut q_weighted = q;
        for _ in 0..WORDS {
            q_weighted = q_weighted.double();
        }
        let start = std::array::from_fn(|level| {
            let s = generator(level);
            // Step 0 of the specification: `(Q + S[l]) + Q` is `⊥` when Q is the identity, when
            // `Q = ±S[l]`, or when `[2] Q + S[l] = O`.
            let Some(q_x) = q_x else {
                return Start::Bottom;
            };
            if q_x == xy(&s).0 {
                return Start::Bottom;
            }
            let mut s_weighted = pallas::Point::from(s);
            for _ in 0..WORDS - 1 {
                s_weighted = s_weighted.double();
            }
            let sum: Option<Coordinates<pallas::Affine>> =
                Option::from((q_weighted + s_weighted).to_affine().coordinates());
            match sum {
                None => Start::Bottom,
                Some(c) => Start::Point {
                    x: *c.x(),
                    y: *c.y(),
                },
            }
        });
        Table { rows, start }
    }

    #[inline(always)]
    pub(crate) fn start(&self, level: u8) -> Start {
        self.start[usize::from(level)]
    }

    #[inline(always)]
    pub(crate) fn entry(&self, position: usize, word: u16) -> &Entry {
        self.rows.entry(position, word)
    }

    /// Heap bytes of the table: the shared rows plus the start points of this table.
    pub fn bytes(&self) -> usize {
        self.rows.bytes() + std::mem::size_of_val(&self.start)
    }
}

fn generator(j: usize) -> pallas::Affine {
    let (x, y) = sinsemilla::SINSEMILLA_S[j];
    Option::from(pallas::Affine::from_xy(x, y)).expect("SINSEMILLA_S holds curve points")
}

fn xy(p: &pallas::Affine) -> (pallas::Base, pallas::Base) {
    // pasta_curves only gives out coordinates through `CtOption`. The table never holds the
    // identity (a doubling of a non-identity point of a prime-order group is never the
    // identity).
    let c: Coordinates<pallas::Affine> =
        Option::from(p.coordinates()).expect("table point is not the identity");
    (*c.x(), *c.y())
}

/// `[2^e] S[j]` for every `e` in `0..WORDS` and `j` in `0..GENERATORS`, in affine form.
fn weighted_generators() -> Vec<Vec<pallas::Affine>> {
    let mut rows = Vec::with_capacity(WORDS);
    let row0: Vec<pallas::Affine> = (0..GENERATORS).map(generator).collect();
    let mut projective: Vec<pallas::Point> = row0.iter().map(pallas::Point::from).collect();
    rows.push(row0);
    for _ in 1..WORDS {
        projective.iter_mut().for_each(|p| *p = p.double());
        let mut affine = vec![pallas::Affine::from(pallas::Point::identity()); GENERATORS];
        pallas::Point::batch_normalize(&projective, &mut affine);
        assert!(
            affine
                .iter()
                .all(|p| !bool::from(pallas::Point::from(p).is_identity())),
            "weighted generator is the identity"
        );
        rows.push(affine);
    }
    rows
}
