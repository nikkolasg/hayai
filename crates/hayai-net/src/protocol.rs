//! Capability negotiation between the legacy protocol and the compact-relay extension
//! (`docs/protocol-compact-relay.md`, section Negotiation and legacy coexistence).
//!
//! A node that runs the extension sets [`NODE_COMPACT_RELAY`] in its `version` services and,
//! once the legacy handshake is complete, sends `zcmpctver` with the versions it speaks and
//! its feature bits. Both sides choose `min(max_a, max_b)` when that is at least both
//! minimums; otherwise, or when the peer never sends `zcmpctver`, the peer stays
//! [`PeerProtocol::Legacy`]. Feature bits are intersected; unknown bits are ignored.
//!
//! Versions: 1 is the short-id protocol with forwarding after reconstruction; 2 adds
//! `CompactBlockV2` (type 12, a compact block with full ids) and forwarding once the id list
//! verifies against the header. A version 1 peer never receives full ids.
//!
//! Feature bit 2 (candidates) adds `CandidateAnnounce` and `CandidateBlock`: template
//! candidates as lanes, and blocks sent as a candidate plus a difference. It is a feature
//! bit and not version 3: only lane owners publish candidates, any version can carry them,
//! and a peer that leaves the bit clear sees no change (`docs/protocol-compact-relay.md`,
//! section Candidates).

use hayai_consensus::Upgrade;

use crate::codec::Network;

/// Legacy protocol version of a build without the NU7 rule set: the NU6.3 version of
/// Mainnet and Testnet (Zakura `zakura-network/src/protocol/external/types.rs:123-126`).
/// ZIP 258: a node of NU6.3 advertises at least 170,160.
/// ZIP 205, 206, 250-253, 255, 257, 258: the version is at least
/// MIN_NETWORK_PROTOCOL_VERSION of each upgrade.
pub const PROTOCOL_VERSION: u32 = 170_160;
/// Legacy protocol version of a build with the NU7 rule set: the version of Zakura
/// (`zakura-network/src/constants.rs`, `CURRENT_NETWORK_PROTOCOL_VERSION`), which is the
/// NU7 version of Mainnet. ZIP 259: a node of NU7 advertises at least 170,190 on Mainnet
/// and 170,180 on Testnet.
pub const PROTOCOL_VERSION_NU7: u32 = 170_190;

/// The legacy protocol version that this node states. From the NU7 activation a Zakura
/// peer disconnects a node with a version below the NU7 version of the network (170,180
/// on Testnet and Regtest, 170,190 on Mainnet), so a build with the NU7 rule set states
/// the NU7 version. A build without that rule set does not state it: it cannot follow
/// the chain after NU7.
pub fn protocol_version() -> u32 {
    match hayai_crypto::nu7_branch() {
        Some(_) => PROTOCOL_VERSION_NU7,
        None => PROTOCOL_VERSION,
    }
}
/// Oldest peer protocol version accepted while the node does not know its height: the
/// NU6.2 version (Zakura `INITIAL_MIN_NETWORK_PROTOCOL_VERSION`,
/// `zakura-network/src/constants.rs:432-437`). ZIP 204: it is above `MIN_PEER_PROTO_VERSION`
/// (170,002) and `MIN_TESTNET_PEER_PROTO_VERSION` (170,040).
pub const INITIAL_MIN_PEER_VERSION: u32 = 170_150;

/// Oldest peer protocol version accepted when `upgrade` is the active network upgrade.
///
/// The values are those of Zakura's `Version::min_specified_for_upgrade`
/// (`zakura-network/src/protocol/external/types.rs:88-131`), and the result is never below
/// [`INITIAL_MIN_PEER_VERSION`], as in `Version::min_remote_for_height` (same file, lines
/// 33-50). Regtest uses the Testnet values. The versions of the upgrades before NU6.2 are
/// below the initial minimum on every network, so they have no row.
///
/// ZIP 204, ZIP 258, ZIP 259: the protocol version of each upgrade.
pub fn min_peer_version(network: Network, upgrade: Upgrade) -> u32 {
    let specified = match (network, upgrade) {
        (_, Upgrade::Nu6_3) => 170_160,
        (Network::Mainnet, Upgrade::Nu7) => 170_190,
        (Network::Testnet | Network::Regtest, Upgrade::Nu7) => 170_180,
        (
            _,
            Upgrade::Sprout
            | Upgrade::Overwinter
            | Upgrade::Sapling
            | Upgrade::Blossom
            | Upgrade::Heartwood
            | Upgrade::Canopy
            | Upgrade::Nu5
            | Upgrade::Nu6
            | Upgrade::Nu6_1
            | Upgrade::Nu6_2,
        ) => INITIAL_MIN_PEER_VERSION,
    };
    specified.max(INITIAL_MIN_PEER_VERSION)
}

/// Service bit of a full node (`NODE_NETWORK`). ZIP 204: bit 0.
pub const NODE_NETWORK: u64 = 1;
/// Service bit advertising the compact-relay extension. Zakura's P2P v2 uses `1 << 24`;
/// this stays clear of it. ZIP 204: bits 24 to 31 are for temporary experiments.
pub const NODE_COMPACT_RELAY: u64 = 1 << 26;

/// User agent sent in `version`: `/hayai:<crate version>/` (BIP 14 form).
pub const USER_AGENT: &str = concat!("/hayai:", env!("CARGO_PKG_VERSION"), "/");

/// Feature bits of `zcmpctver`.
pub mod features {
    /// Short-id compact blocks, `CompactBlock`/`BlockTxnRequest`/`BlockTxn`, version 1.
    pub const COMPACT_BLOCKS_V1: u64 = 1;
    /// Batch lanes, `BatchAnnounce`/`BatchRequest`, version 1.
    pub const LANES_V1: u64 = 1 << 1;
    /// Template candidates, `CandidateAnnounce`/`CandidateBlock`. In use only together with
    /// [`LANES_V1`]: a candidate is a list of batches.
    pub const CANDIDATES_V1: u64 = 1 << 2;
    /// Every bit this implementation understands.
    pub const KNOWN: u64 = COMPACT_BLOCKS_V1 | LANES_V1 | CANDIDATES_V1;
}

/// Payload of `zcmpctver`: `max_version u16 LE, min_version u16 LE, features u64 LE`.
/// Trailing bytes are accepted so that later versions can append fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompactVer {
    pub max_version: u16,
    pub min_version: u16,
    pub features: u64,
}

impl CompactVer {
    /// What this implementation offers: versions 1 and 2, both features.
    pub const CURRENT: Self = Self {
        max_version: FULL_IDS_VERSION,
        min_version: 1,
        features: features::KNOWN,
    };

    /// The offer of a node that stops at version 1 (tests and mixed deployments).
    pub const V1: Self = Self {
        max_version: 1,
        min_version: 1,
        features: features::KNOWN,
    };

    /// The offer of a version 2 node without the candidates feature (tests and mixed
    /// deployments).
    pub const V2_WITHOUT_CANDIDATES: Self = Self {
        max_version: FULL_IDS_VERSION,
        min_version: 1,
        features: features::COMPACT_BLOCKS_V1 | features::LANES_V1,
    };
}

/// First extension version with `CompactBlockV2` (full ids) and id-complete forwarding.
pub const FULL_IDS_VERSION: u16 = 2;

/// Outcome of a successful negotiation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Negotiated {
    pub version: u16,
    /// Features both sides offered, restricted to the bits this side knows.
    pub features: u64,
}

impl Negotiated {
    pub fn has(&self, feature: u64) -> bool {
        self.features & feature == feature
    }

    /// Whether `CompactBlock` may carry full ids and be forwarded on its ids.
    pub fn full_ids(&self) -> bool {
        self.version >= FULL_IDS_VERSION
    }

    /// Whether candidates and candidate blocks may be sent: both lanes and candidates.
    pub fn candidates(&self) -> bool {
        self.has(features::LANES_V1 | features::CANDIDATES_V1)
    }
}

/// What a peer speaks after the handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeerProtocol {
    /// The Bitcoin-derived Zcash protocol only.
    Legacy,
    /// The compact-relay extension, framed inside legacy `zcmpct` messages.
    CompactRelay(Negotiated),
}

impl PeerProtocol {
    pub fn compact_relay(&self) -> Option<&Negotiated> {
        match self {
            PeerProtocol::Legacy => None,
            PeerProtocol::CompactRelay(n) => Some(n),
        }
    }
}

/// Chooses the protocol from both sides' `zcmpctver`. `None` means the peer stays legacy.
pub fn negotiate(ours: &CompactVer, theirs: &CompactVer) -> Option<Negotiated> {
    let version = ours.max_version.min(theirs.max_version);
    if version < ours.min_version || version < theirs.min_version {
        return None;
    }
    Some(Negotiated {
        version,
        features: ours.features & theirs.features & features::KNOWN,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(min: u16, max: u16, features: u64) -> CompactVer {
        CompactVer {
            max_version: max,
            min_version: min,
            features,
        }
    }

    #[test]
    fn picks_the_highest_common_version() {
        let got = negotiate(&ver(1, 3, features::KNOWN), &ver(2, 5, features::KNOWN));
        assert_eq!(
            got,
            Some(Negotiated {
                version: 3,
                features: features::KNOWN
            })
        );
    }

    #[test]
    fn disjoint_ranges_stay_legacy() {
        assert_eq!(negotiate(&ver(1, 1, 0), &ver(2, 3, 0)), None);
        assert_eq!(negotiate(&ver(2, 3, 0), &ver(1, 1, 0)), None);
    }

    #[test]
    fn features_intersect_and_unknown_bits_drop() {
        let got = negotiate(
            &ver(1, 1, features::KNOWN),
            &ver(1, 1, features::COMPACT_BLOCKS_V1 | 1 << 40),
        )
        .expect("negotiates");
        assert_eq!(got.features, features::COMPACT_BLOCKS_V1);
        assert!(got.has(features::COMPACT_BLOCKS_V1));
        assert!(!got.has(features::LANES_V1));
    }

    #[test]
    fn current_offer_negotiates_v2_with_itself_and_v1_with_a_v1_peer() {
        let v2 = negotiate(&CompactVer::CURRENT, &CompactVer::CURRENT).expect("negotiates");
        assert_eq!(v2.version, 2);
        assert!(v2.full_ids());
        let v1 = negotiate(&CompactVer::CURRENT, &CompactVer::V1).expect("negotiates");
        assert_eq!(v1.version, 1);
        assert!(!v1.full_ids());
        assert_eq!(negotiate(&CompactVer::V1, &CompactVer::CURRENT), Some(v1));
    }

    #[test]
    fn candidates_need_both_bits() {
        let both = negotiate(&CompactVer::CURRENT, &CompactVer::CURRENT).unwrap();
        assert!(both.candidates());
        let without = negotiate(&CompactVer::CURRENT, &CompactVer::V2_WITHOUT_CANDIDATES).unwrap();
        assert_eq!(without.version, 2);
        assert!(!without.candidates());
        let no_lanes = negotiate(
            &CompactVer::CURRENT,
            &ver(1, 2, features::COMPACT_BLOCKS_V1 | features::CANDIDATES_V1),
        )
        .unwrap();
        assert!(!no_lanes.candidates());
    }

    #[test]
    fn minimum_peer_version_follows_the_upgrade() {
        for network in [Network::Mainnet, Network::Testnet, Network::Regtest] {
            assert_eq!(min_peer_version(network, Upgrade::Sprout), 170_150);
            assert_eq!(min_peer_version(network, Upgrade::Nu6), 170_150);
            assert_eq!(min_peer_version(network, Upgrade::Nu6_2), 170_150);
            assert_eq!(min_peer_version(network, Upgrade::Nu6_3), 170_160);
            // This node passes the minimum of every upgrade that it implements.
            assert!(PROTOCOL_VERSION >= min_peer_version(network, Upgrade::Nu6_3));
            // A build with the NU7 rule set passes the minimum of its peers after NU7.
            let passes = match hayai_crypto::nu7_branch() {
                Some(_) => protocol_version() >= min_peer_version(network, Upgrade::Nu7),
                None => true,
            };
            assert!(passes);
            assert!(protocol_version() >= PROTOCOL_VERSION);
        }
        assert_eq!(min_peer_version(Network::Mainnet, Upgrade::Nu7), 170_190);
        assert_eq!(min_peer_version(Network::Testnet, Upgrade::Nu7), 170_180);
        assert_eq!(min_peer_version(Network::Regtest, Upgrade::Nu7), 170_180);
        assert_eq!(USER_AGENT, "/hayai:0.1.0/");
    }

    #[test]
    fn service_bit_is_clear_of_zakura() {
        assert_eq!(NODE_COMPACT_RELAY, 1 << 26);
        assert_eq!(NODE_COMPACT_RELAY & (1 << 24), 0);
    }
}
