//! The deferred pool (lockbox): what a block adds to it and takes from it.
//!
//! From NU6 a funding stream pays a share of the block subsidy to the deferred pool
//! (ZIP 1015, ZIP 2001; [`crate::subsidy::Subsidy::deferred`]). The coinbase of the NU6.1
//! activation block takes 78,750 ZEC out of the pool in ten equal outputs (ZIP 271,
//! ZIP 1016). Zakura checks the same outputs (`zakura-consensus/src/block/check.rs:
//! 268-290`) with the constants of `zakura-chain/src/parameters/network/subsidy/constants/
//! {mainnet.rs:25-33,testnet.rs:34-42}`. Regtest has no NU6.1 height and no disbursement.

use crate::{Network, Upgrade};

/// The one-time lockbox disbursement outputs of one coinbase: `count` outputs of `value`
/// zatoshis each to `address`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disbursement {
    /// `ZIP271DisbursementChunks`.
    pub count: usize,
    /// `ZIP271DisbursementAmount / ZIP271DisbursementChunks` in zatoshis.
    pub value: u64,
    /// `ZIP271DisbursementAddress`: the Base58Check P2SH address.
    pub address: &'static str,
}

impl Disbursement {
    /// The value that all outputs take out of the deferred pool.
    pub fn total(&self) -> u64 {
        self.value * self.count as u64
    }
}

/// 78,750 ZEC in ten outputs of 7,875 ZEC.
const fn nu6_1_disbursement(address: &'static str) -> Disbursement {
    Disbursement {
        count: 10,
        value: 787_500_000_000,
        address,
    }
}

/// The lockbox disbursement that the coinbase at `height` must pay. `None` at every height
/// but the NU6.1 activation height of Mainnet and Testnet.
pub fn disbursement(network: Network, height: u32) -> Option<Disbursement> {
    if network.activation_height(Upgrade::Nu6_1) != Some(height) {
        return None;
    }
    match network {
        Network::Mainnet => Some(nu6_1_disbursement("t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo")),
        Network::Testnet => Some(nu6_1_disbursement("t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8")),
        Network::Regtest | Network::ConfiguredRegtest(_) => None,
    }
}

/// The deferred pool after a block that adds `deferred` zatoshis and pays `disbursed`
/// zatoshis out of the pool, from a pool of `before` zatoshis. `None` when the block pays
/// out more than the pool holds.
pub fn deferred_pool_after(before: u64, deferred: u64, disbursed: u64) -> Option<u64> {
    before.checked_add(deferred)?.checked_sub(disbursed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coinbase::address_script;

    #[test]
    fn the_disbursement_is_in_the_nu6_1_activation_block_only() {
        for (network, height, address) in [
            (
                Network::Mainnet,
                3_146_400,
                "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo",
            ),
            (
                Network::Testnet,
                3_536_500,
                "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8",
            ),
        ] {
            assert_eq!(disbursement(network, height - 1), None);
            assert_eq!(disbursement(network, height + 1), None);
            let disbursement = disbursement(network, height).unwrap();
            assert_eq!(disbursement.count, 10);
            assert_eq!(disbursement.value, 7_875 * 100_000_000);
            assert_eq!(disbursement.total(), 78_750 * 100_000_000);
            assert_eq!(disbursement.address, address);
            let script = address_script(network, address);
            assert_eq!((script.len(), script[0], script[22]), (23, 0xa9, 0x87));
        }
        for height in [0, 1, 3_146_400, 3_536_500] {
            assert_eq!(disbursement(Network::Regtest, height), None);
        }
    }

    #[test]
    fn the_pool_grows_by_the_deferred_part_and_shrinks_by_the_disbursement() {
        assert_eq!(deferred_pool_after(5, 7, 0), Some(12));
        assert_eq!(deferred_pool_after(5, 7, 12), Some(0));
        assert_eq!(deferred_pool_after(5, 7, 13), None);
        assert_eq!(deferred_pool_after(u64::MAX, 1, 1), None);
    }
}
