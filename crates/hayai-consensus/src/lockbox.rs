//! The deferred pool (lockbox): the disbursement tables of Mainnet and Testnet with their
//! addresses, and the wrappers of `hayai_consensus_core::lockbox` for a caller with a
//! [`Network`].
//!
//! The coinbase of the NU6.1 activation block takes 78,750 ZEC out of the pool in ten
//! equal outputs (ZIP 271, ZIP 1016). Zakura checks the same outputs
//! (`zakura-consensus/src/block/check.rs:268-290`) with the constants of
//! `zakura-chain/src/parameters/network/subsidy/constants/{mainnet.rs:25-33,testnet.rs:34-42}`.
//! The disbursements of a network are data of its spec
//! ([`crate::ChainSpec::lockbox_disbursements`]). A Regtest network takes its
//! disbursements from its configuration ([`crate::RegtestConfig::with_lockbox_disbursements`]).

use hayai_consensus_core::lockbox as core;
pub use hayai_consensus_core::lockbox::{deferred_pool_after, Disbursement as CoreDisbursement};
use hayai_crypto::zcash_protocol::consensus::NetworkType;

use crate::address::{p2sh_script, script_of};
use crate::network::check_address;
use crate::{ChainSpecError, Network};

/// Lockbox disbursement outputs of one coinbase: `count` outputs of `value` zatoshis each
/// to `address`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Disbursement {
    /// `ZIP271DisbursementChunks`.
    pub count: usize,
    /// `ZIP271DisbursementAmount / ZIP271DisbursementChunks` in zatoshis.
    pub value: u64,
    /// `ZIP271DisbursementAddress`: the Base58Check P2SH address.
    pub address: &'static str,
}

/// 78,750 ZEC in ten outputs of 7,875 ZEC.
///
/// ZIP 271: `ZIP271DisbursementAmount` is 78,750 ZEC on Mainnet and Testnet, in
/// `ZIP271DisbursementChunks` = 10 equal outputs.
const fn nu6_1_disbursement(address: &'static str) -> Disbursement {
    Disbursement {
        count: 10,
        value: 787_500_000_000,
        address,
    }
}

/// [`nu6_1_disbursement`] with the script of the address.
const fn nu6_1_core(address: &'static str) -> CoreDisbursement {
    CoreDisbursement {
        count: 10,
        value: 787_500_000_000,
        script: script_of(address),
    }
}

/// The disbursements of a spec with the network type `network_type`, with each address
/// decoded to its script. Each address must be a P2SH address of the network type, and
/// the sum of the values a valid amount of money (Zakura `check_lockbox_disbursements`).
pub(crate) fn core_disbursements(
    network_type: NetworkType,
    disbursements: &[Disbursement],
) -> Result<Vec<CoreDisbursement>, ChainSpecError> {
    let mut outputs = Vec::with_capacity(disbursements.len());
    for disbursement in disbursements {
        check_address(network_type, disbursement.address)?;
        let Ok(script) = p2sh_script(network_type, disbursement.address) else {
            unreachable!("check_address decoded the address");
        };
        outputs.push(CoreDisbursement {
            count: disbursement.count,
            value: disbursement.value,
            script,
        });
    }
    core::check_disbursements(&outputs)?;
    Ok(outputs)
}

/// ZIP 271: `ZIP271DisbursementAddress` of Mainnet.
const MAINNET_ADDRESS: &str = "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo";
/// ZIP 271: `ZIP271DisbursementAddress` of Testnet.
const TESTNET_ADDRESS: &str = "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8";

pub(crate) static MAINNET: [Disbursement; 1] = [nu6_1_disbursement(MAINNET_ADDRESS)];
pub(crate) static TESTNET: [Disbursement; 1] = [nu6_1_disbursement(TESTNET_ADDRESS)];
pub(crate) static MAINNET_CORE: [CoreDisbursement; 1] = [nu6_1_core(MAINNET_ADDRESS)];
pub(crate) static TESTNET_CORE: [CoreDisbursement; 1] = [nu6_1_core(TESTNET_ADDRESS)];

/// The lockbox disbursements that the coinbase at `height` must pay
/// (`hayai_consensus_core::lockbox::disbursements`): empty at every height but the NU6.1
/// activation height.
pub fn disbursements(network: Network, height: u32) -> &'static [CoreDisbursement] {
    core::disbursements(network.core(), height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::address_script;
    use crate::address_of;

    #[test]
    fn the_disbursement_is_in_the_nu6_1_activation_block_only() {
        for (network, height, address) in [
            (Network::Mainnet, 3_146_400, MAINNET_ADDRESS),
            (Network::Testnet, 3_536_500, TESTNET_ADDRESS),
        ] {
            assert_eq!(disbursements(network, height - 1), &[]);
            assert_eq!(disbursements(network, height + 1), &[]);
            let [disbursement] = disbursements(network, height) else {
                panic!("one disbursement at the NU6.1 activation height");
            };
            assert_eq!(disbursement.count, 10);
            assert_eq!(disbursement.value, 7_875 * 100_000_000);
            assert_eq!(disbursement.total(), Ok(78_750 * 100_000_000));
            assert_eq!(address_of(network, &disbursement.script), address);
            let script = address_script(network, address);
            assert_eq!(disbursement.script, script);
            assert_eq!((script.len(), script[0], script[22]), (23, 0xa9, 0x87));
        }
        for height in [0, 1, 3_146_400, 3_536_500] {
            assert_eq!(disbursements(Network::Regtest, height), &[]);
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
