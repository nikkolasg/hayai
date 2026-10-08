//! Network parameters of the networks hayaid runs on: a thin wrapper over hayai-consensus.
//!
//! hayai-consensus holds the parameters and the rule sets. This module adds what only the
//! node needs: the wire network and the epoch of a height for the prepared store.

use hayai_consensus::{rules_at, ConsensusError, RuleSet};
use hayai_crypto::zcash_protocol::consensus::{BranchId, NetworkType};
use hayai_net::Network;
use hayai_prepared::RuleEpoch;
use hayai_wire::header::{BlockHash, PowParams};

/// The networks hayaid supports.
pub use hayai_consensus::Network as NetworkKind;

/// Compact form of the Regtest proof-of-work limit `0x0f0f…0f`.
pub const REGTEST_POW_LIMIT_BITS: u32 = NetworkKind::Regtest.params().pow_limit_bits;

/// Everything hayaid derives from the network choice.
#[derive(Clone, Copy, Debug)]
pub struct NetParams {
    pub kind: NetworkKind,
}

impl NetParams {
    pub fn new(kind: NetworkKind) -> Self {
        Self { kind }
    }

    pub fn wire(&self) -> Network {
        match self.kind.network_type() {
            NetworkType::Regtest => Network::Regtest,
            NetworkType::Test => Network::Testnet,
            NetworkType::Main => Network::Mainnet,
        }
    }

    /// The Equihash parameters: the solution length of every header of the network.
    pub fn pow(&self) -> PowParams {
        self.kind.params().pow
    }

    /// The rule set of the block at `height`. An error means that an upgrade without a rule
    /// set is active at `height`: the node must stop.
    pub fn rules_at(&self, height: u32) -> Result<&'static RuleSet, ConsensusError> {
        rules_at(self.kind, height)
    }

    pub fn branch_at(&self, height: u32) -> Result<BranchId, ConsensusError> {
        Ok(self.rules_at(height)?.branch_id)
    }

    pub fn epoch_at(&self, height: u32) -> Result<RuleEpoch, ConsensusError> {
        Ok(RuleEpoch::of(self.rules_at(height)?))
    }

    /// The branch of the next upgrade after `height`, for the verifying key prebuild.
    pub fn next_branch(&self, height: u32) -> Option<BranchId> {
        hayai_consensus::branch_id(self.kind.next_upgrade(height)?)
    }

    /// The hash and time of the genesis block, the start of a full node.
    pub fn genesis(&self) -> (BlockHash, u32) {
        let params = self.kind.params();
        (params.genesis_hash, params.genesis_time)
    }
}

/// Parses a block hash in display (byte-reversed) hex.
pub fn parse_hash(display: &str) -> Result<BlockHash, String> {
    let mut bytes: [u8; 32] = hex::decode(display)
        .map_err(|e| format!("block hash {display}: {e}"))?
        .try_into()
        .map_err(|_| format!("block hash {display} is not 32 bytes"))?;
    bytes.reverse();
    Ok(BlockHash(bytes))
}

#[cfg(test)]
mod tests {
    use hayai_consensus::Upgrade;

    use super::*;

    /// The Regtest values that hayaid held before hayai-consensus.
    #[test]
    fn regtest_values_equal_the_previous_constants() {
        let p = NetParams::new(NetworkKind::Regtest);
        assert_eq!(p.branch_at(0), Ok(BranchId::Sprout));
        assert_eq!(p.branch_at(1), Ok(BranchId::Nu5));
        assert_eq!(p.branch_at(10_000), Ok(BranchId::Nu5));
        assert_eq!(p.epoch_at(1), Ok(RuleEpoch::consensus(BranchId::Nu5)));
        assert_eq!(p.next_branch(0), Some(BranchId::Nu5));
        assert_eq!(p.next_branch(5), None);
        assert_eq!(p.wire(), Network::Regtest);
        assert_eq!(p.pow(), PowParams::REGTEST);
        assert_eq!(p.pow(), p.wire().pow());
        assert_eq!(
            NetParams::new(NetworkKind::Testnet).pow(),
            PowParams::TESTNET
        );
        let (genesis, time) = p.genesis();
        assert_eq!(
            genesis.to_string(),
            "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327"
        );
        assert_eq!(time, 1_296_688_602);
        assert_eq!(REGTEST_POW_LIMIT_BITS, 0x200f_0f0f);
    }

    #[test]
    fn a_hash_round_trips_through_display() {
        let (genesis, _) = NetParams::new(NetworkKind::Regtest).genesis();
        assert_eq!(parse_hash(&genesis.to_string()), Ok(genesis));
        let Err(_) = parse_hash("00") else {
            panic!("a short hash is rejected");
        };
    }

    #[test]
    fn mainnet_parameters() {
        let p = NetParams::new(NetworkKind::Mainnet);
        let (genesis, time) = p.genesis();
        assert_eq!(
            genesis.to_string(),
            "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"
        );
        assert_eq!(time, 1_477_641_360);
        assert_eq!(p.wire(), Network::Mainnet);
        assert_eq!(p.wire().magic(), [0x24, 0xe9, 0x27, 0x64]);
        assert_eq!(p.pow(), PowParams::MAINNET);
        assert_eq!(p.branch_at(0), Ok(BranchId::Sprout));
        assert_eq!(p.branch_at(347_499), Ok(BranchId::Sprout));
        assert_eq!(p.branch_at(347_500), Ok(BranchId::Overwinter));
        assert_eq!(p.branch_at(1_687_103), Ok(BranchId::Canopy));
        assert_eq!(p.branch_at(1_687_104), Ok(BranchId::Nu5));
        assert_eq!(p.next_branch(1_687_103), Some(BranchId::Nu5));
    }

    /// The NU7 height of Testnet. With the NU7 rule set the node gets the NU7 branch and
    /// epoch from that height. Without it the node gets an error for the rule set, the
    /// branch and the epoch, and never the NU6.3 values. The key prebuild reads the branch
    /// of the next upgrade, which a backend without NU7 does not have.
    #[test]
    fn the_rules_at_the_nu7_height() {
        let p = NetParams::new(NetworkKind::Testnet);
        let nu7 = NetworkKind::Testnet
            .activation_height(Upgrade::Nu7)
            .expect("the NU7 height of Testnet");
        assert_eq!(nu7, 4_465_026);
        assert_eq!(p.branch_at(nu7 - 1), Ok(BranchId::Nu6_3));
        assert_eq!(
            p.next_branch(nu7 - 1),
            hayai_consensus::branch_id(Upgrade::Nu7)
        );
        for height in [nu7, nu7 + 1] {
            match RuleSet::of(Upgrade::Nu7) {
                Some(rules) => {
                    assert_eq!(p.rules_at(height), Ok(rules));
                    assert_eq!(p.branch_at(height), Ok(rules.branch_id));
                    assert_eq!(p.epoch_at(height), Ok(RuleEpoch::of(rules)));
                    assert_eq!(
                        Some(rules.branch_id),
                        hayai_consensus::branch_id(Upgrade::Nu7)
                    );
                }
                None => {
                    let refused = ConsensusError::UnsupportedUpgrade {
                        upgrade: Upgrade::Nu7,
                        height,
                    };
                    assert_eq!(p.rules_at(height).map(|_| ()), Err(refused));
                    assert_eq!(p.branch_at(height), Err(refused));
                    assert_eq!(p.epoch_at(height), Err(refused));
                }
            }
        }
    }
}
