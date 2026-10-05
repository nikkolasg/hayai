//! The random source of the fuzzer: SplitMix64. The crate owns the generator, so a case
//! seed gives the same case with every version of every dependency.

/// A deterministic generator. Two generators with the same seed give the same values.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

/// One SplitMix64 step: the output for `state`.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The seed of case `index` of class `class` in the run with the seed `run_seed`.
pub fn case_seed(run_seed: u64, class: &str, index: u64) -> u64 {
    let mut h = mix(run_seed ^ 0x6861_7961_695f_667a);
    for byte in class.bytes() {
        h = mix(h ^ u64::from(byte));
    }
    mix(h ^ mix(index))
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.state)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// A value in `0..n`. `n` must not be 0.
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "the range is not empty");
        (self.next_u64() % n as u64) as usize
    }

    /// True with the probability `num / den`.
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        self.next_u64() % u64::from(den) < u64::from(num)
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }

    /// Between `min` and `min + spread - 1` random bytes.
    pub fn some_bytes(&mut self, min: usize, spread: usize) -> Vec<u8> {
        let len = min + self.below(spread);
        self.bytes(len)
    }

    /// A value at or near one of `anchors` (the anchor, or 1 or 2 away from it), or with
    /// the probability 1/8 any value.
    pub fn near_u32(&mut self, anchors: &[u32]) -> u32 {
        if self.chance(1, 8) {
            return self.next_u32();
        }
        let anchor = *self.pick(anchors);
        let delta = [0i64, 0, 1, -1, 2, -2][self.below(6)];
        (i64::from(anchor) + delta).clamp(0, i64::from(u32::MAX)) as u32
    }

    /// As [`Rng::near_u32`] for 64-bit values.
    pub fn near_u64(&mut self, anchors: &[u64]) -> u64 {
        if self.chance(1, 8) {
            return self.next_u64();
        }
        let anchor = *self.pick(anchors);
        match self.below(6) {
            0 | 1 => anchor,
            2 => anchor.saturating_add(1),
            3 => anchor.saturating_sub(1),
            4 => anchor.saturating_add(2),
            _ => anchor.saturating_sub(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generator_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let first: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let second: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(first, second);
        // The first output of SplitMix64 for the seed 0.
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
        assert_ne!(case_seed(1, "bytes", 0), case_seed(1, "bytes", 1));
        assert_ne!(case_seed(1, "bytes", 0), case_seed(1, "script", 0));
    }
}
