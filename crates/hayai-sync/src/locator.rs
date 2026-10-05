//! Block locator heights.
//!
//! A locator names blocks of the best chain from the tip back to the genesis block: the tip
//! and the 9 blocks before it, then blocks at steps that double (2, 4, 8, ...), then the
//! genesis block. A peer finds the newest block that it has in the locator and sends the
//! headers after it. This is the locator of Bitcoin Core (`CChain::GetLocator`) and zcashd.

/// Heights with a step of one block, the tip included.
const DENSE: usize = 10;

/// The heights of a locator for a chain whose tip is at `tip`, newest first. The last
/// height is 0. A chain of 2^32 blocks gives 41 heights, below the limit of 101 hashes of a
/// `getheaders` message.
pub fn locator_heights(tip: u32) -> Vec<u32> {
    let mut heights = Vec::new();
    let mut height = tip;
    let mut step = 1u32;
    loop {
        heights.push(height);
        if height == 0 {
            return heights;
        }
        if heights.len() >= DENSE {
            step = step.saturating_mul(2);
        }
        height = height.saturating_sub(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_then_doubling_steps_to_genesis() {
        assert_eq!(locator_heights(0), vec![0]);
        assert_eq!(locator_heights(3), vec![3, 2, 1, 0]);
        assert_eq!(
            locator_heights(100),
            vec![100, 99, 98, 97, 96, 95, 94, 93, 92, 91, 89, 85, 77, 61, 29, 0]
        );
        let all = locator_heights(u32::MAX);
        assert_eq!(all.len(), 41);
        assert_eq!(all.last(), Some(&0));
        assert!(all.windows(2).all(|w| w[0] > w[1]));
    }
}
