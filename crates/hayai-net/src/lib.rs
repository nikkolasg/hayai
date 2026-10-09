//! Networking for a hayai node: the legacy Zcash peer-to-peer protocol, the compact-relay
//! extension negotiated inside it, and the relay policy that always serves both.
//!
//! - [`codec`]: Bitcoin-style framing and every legacy message layout, plus the two
//!   extension commands `zcmpctver` and `zcmpct`.
//! - [`protocol`]: service bit, versions, feature bits and the negotiation rule.
//! - [`session`]: the per-peer handshake and keepalive state machine.
//! - [`transport`]: blocking-socket transport with one reader thread per peer.
//! - [`policy`]: the relay policy, the peer set and the both-paths relay of blocks,
//!   transactions and lanes, without a socket: events in, messages out through [`policy::Io`].
//! - [`relay`]: the shell of the policy: the TCP transports, the acceptor and the ticker.
//! - [`addrbook`]: the bounded address book, its selection rule and its file.
//! - [`connect`]: the peer manager: limits, bans, DNS seeders and outbound connections.
//!
//! Protocol contract: `docs/protocol-compact-relay.md`, section Negotiation and legacy
//! coexistence.

#![forbid(unsafe_code)]

pub mod addrbook;
pub mod codec;
pub mod connect;
pub mod policy;
pub mod protocol;
pub mod relay;
pub mod session;
pub mod transport;

pub use addrbook::{AddrBook, AddrBookConfig, AddrBookError};
pub use codec::{InvItem, LegacyMessage, Network, VersionMessage};
pub use connect::{PeerConfig, PeerEnv, PeerManager};
pub use hayai_sync::score::Misbehaviour;
pub use protocol::{
    features, min_peer_version, CompactVer, Negotiated, PeerProtocol, FULL_IDS_VERSION,
    NODE_COMPACT_RELAY,
};
pub use relay::{
    BlockSink, ChainSource, HistoryRootSource, IncomingBlock, PeerId, PeerInfo, Relay, RelayConfig,
    RelayCounters, RelayDeps, Source, SyncEvent, SyncSink, TxSink,
};
pub use session::{Direction, PeerSession, SessionConfig};
pub use transport::{TcpTransport, Transport};
