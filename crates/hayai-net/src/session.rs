//! Per-peer protocol state machine: legacy handshake, keepalive, and the compact-relay
//! upgrade. Pure logic over an injected clock; the transport feeds it messages and ticks
//! and performs the actions it returns.
//!
//! States: `Handshaking` (version/verack exchange) → `Established(Legacy)` → optionally
//! `Established(CompactRelay(v))` once both `zcmpctver` are in. A peer that never sends
//! `zcmpctver` stays legacy for the life of the connection.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::codec::{InvItem, LegacyMessage, NetAddr, VersionMessage};
use crate::protocol::{negotiate, CompactVer, PeerProtocol};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub protocol_version: u32,
    /// Oldest peer protocol version accepted (`crate::protocol::min_peer_version`).
    pub min_peer_version: u32,
    /// Service bits advertised in `version`.
    pub services: u64,
    pub user_agent: String,
    /// `None` disables the extension entirely: no service bit, no `zcmpctver`, and
    /// incoming `zcmpctver`/`zcmpct` are ignored like any unknown command.
    pub compact_relay: Option<CompactVer>,
    pub handshake_timeout: Duration,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Outbound,
    Inbound,
}

/// What the transport must do after feeding a message or a tick.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Send(LegacyMessage),
    /// The legacy handshake completed; the peer's protocol is `Legacy` at this point.
    Established,
    /// `zcmpctver` negotiation succeeded.
    Upgraded(PeerProtocol),
    /// An application-level message for the relay.
    Deliver(LegacyMessage),
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum SessionError {
    #[error("handshake did not complete within {0:?}")]
    HandshakeTimeout(Duration),
    #[error("peer protocol version {version} is below the minimum {min}")]
    VersionTooOld { version: u32, min: u32 },
    #[error("version nonce equals ours: connected to self")]
    SelfConnection,
    #[error("second version message")]
    DuplicateVersion,
    #[error("{0} before the handshake completed")]
    BeforeHandshake(String),
    #[error("no pong within {0:?}")]
    PingTimeout(Duration),
}

pub struct PeerSession {
    config: SessionConfig,
    direction: Direction,
    nonce: u64,
    started: Instant,
    peer_version: Option<VersionMessage>,
    got_verack: bool,
    established: bool,
    protocol: PeerProtocol,
    compact_sent: bool,
    compact_done: bool,
    outstanding_ping: Option<(u64, Instant)>,
    last_ping_at: Instant,
}

impl PeerSession {
    /// Starts a session and returns the `version` message to send first; both sides send
    /// theirs without waiting (zcashd and Zebra accept that).
    pub fn new(
        config: SessionConfig,
        direction: Direction,
        peer_addr: SocketAddr,
        start_height: u32,
        now: Instant,
        unix_time: i64,
    ) -> (Self, LegacyMessage) {
        let nonce = rand::random::<u64>();
        let version = LegacyMessage::Version(VersionMessage {
            version: config.protocol_version,
            services: config.services,
            timestamp: unix_time,
            addr_recv: NetAddr {
                services: 0,
                addr: peer_addr,
            },
            addr_from: NetAddr {
                services: config.services,
                addr: SocketAddr::from(([0, 0, 0, 0], 0)),
            },
            nonce,
            user_agent: config.user_agent.clone(),
            start_height,
            relay: true,
        });
        let session = Self {
            config,
            direction,
            nonce,
            started: now,
            peer_version: None,
            got_verack: false,
            established: false,
            protocol: PeerProtocol::Legacy,
            compact_sent: false,
            compact_done: false,
            outstanding_ping: None,
            last_ping_at: now,
        };
        (session, version)
    }

    /// The nonce of this session's `version` message.
    pub fn nonce(&self) -> u64 {
        self.nonce
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn protocol(&self) -> PeerProtocol {
        self.protocol
    }

    pub fn is_established(&self) -> bool {
        self.established
    }

    pub fn peer_version(&self) -> Option<&VersionMessage> {
        self.peer_version.as_ref()
    }

    /// Largest payload to accept from this peer at the moment.
    pub fn max_body_len(&self) -> usize {
        if self.established {
            usize::MAX
        } else {
            crate::codec::MAX_HANDSHAKE_BODY_LEN
        }
    }

    pub fn on_message(&mut self, message: LegacyMessage) -> Result<Vec<Action>, SessionError> {
        let mut actions = Vec::new();
        match message {
            LegacyMessage::Version(v) => {
                let None = self.peer_version else {
                    return Err(SessionError::DuplicateVersion);
                };
                if v.version < self.config.min_peer_version {
                    return Err(SessionError::VersionTooOld {
                        version: v.version,
                        min: self.config.min_peer_version,
                    });
                }
                if v.nonce == self.nonce {
                    return Err(SessionError::SelfConnection);
                }
                self.peer_version = Some(v);
                actions.push(Action::Send(LegacyMessage::Verack));
                self.try_establish(&mut actions);
            }
            LegacyMessage::Verack => {
                let Some(_) = self.peer_version else {
                    return Err(SessionError::BeforeHandshake("verack".into()));
                };
                if self.established {
                    return Ok(actions);
                }
                self.got_verack = true;
                self.try_establish(&mut actions);
            }
            LegacyMessage::Ping(nonce) => actions.push(Action::Send(LegacyMessage::Pong(nonce))),
            LegacyMessage::Pong(nonce) => {
                if matches!(self.outstanding_ping, Some((n, _)) if n == nonce) {
                    self.outstanding_ping = None;
                }
            }
            // BIP 155 requires `sendaddrv2` before `verack`; unknown commands and rejects
            // are harmless at any time.
            LegacyMessage::SendAddrV2
            | LegacyMessage::Unknown { .. }
            | LegacyMessage::Reject(_) => {}
            LegacyMessage::CompactVer(theirs) => self.on_compact_ver(theirs, &mut actions)?,
            LegacyMessage::Compact(m) => {
                if !self.established {
                    return Err(SessionError::BeforeHandshake("zcmpct".into()));
                }
                // From a legacy peer this is an unknown command: ignored.
                if let PeerProtocol::CompactRelay(_) = self.protocol {
                    actions.push(Action::Deliver(LegacyMessage::Compact(m)));
                }
            }
            other => {
                if !self.established {
                    return Err(SessionError::BeforeHandshake(other.command_name()));
                }
                actions.push(Action::Deliver(other));
            }
        }
        Ok(actions)
    }

    fn try_establish(&mut self, actions: &mut Vec<Action>) {
        let Some(peer) = &self.peer_version else {
            return;
        };
        if !self.got_verack || self.established {
            return;
        }
        self.established = true;
        actions.push(Action::Established);
        let Some(ours) = self.config.compact_relay else {
            return;
        };
        if peer.services & crate::protocol::NODE_COMPACT_RELAY != 0 {
            actions.push(Action::Send(LegacyMessage::CompactVer(ours)));
            self.compact_sent = true;
        }
    }

    fn on_compact_ver(
        &mut self,
        theirs: CompactVer,
        actions: &mut Vec<Action>,
    ) -> Result<(), SessionError> {
        if !self.established {
            return Err(SessionError::BeforeHandshake("zcmpctver".into()));
        }
        // Extension disabled, or already negotiated: an unknown or repeated command.
        let Some(ours) = self.config.compact_relay else {
            return Ok(());
        };
        if self.compact_done {
            return Ok(());
        }
        self.compact_done = true;
        if !self.compact_sent {
            actions.push(Action::Send(LegacyMessage::CompactVer(ours)));
            self.compact_sent = true;
        }
        if let Some(negotiated) = negotiate(&ours, &theirs) {
            self.protocol = PeerProtocol::CompactRelay(negotiated);
            actions.push(Action::Upgraded(self.protocol));
        }
        Ok(())
    }

    /// Timeouts and keepalive; call periodically.
    pub fn on_tick(&mut self, now: Instant) -> Result<Vec<Action>, SessionError> {
        if !self.established {
            if now.duration_since(self.started) > self.config.handshake_timeout {
                return Err(SessionError::HandshakeTimeout(
                    self.config.handshake_timeout,
                ));
            }
            return Ok(Vec::new());
        }
        match self.outstanding_ping {
            Some((_, sent)) => {
                if now.duration_since(sent) > self.config.ping_timeout {
                    return Err(SessionError::PingTimeout(self.config.ping_timeout));
                }
                Ok(Vec::new())
            }
            None => {
                if now.duration_since(self.last_ping_at) < self.config.ping_interval {
                    return Ok(Vec::new());
                }
                let nonce = rand::random::<u64>();
                self.outstanding_ping = Some((nonce, now));
                self.last_ping_at = now;
                Ok(vec![Action::Send(LegacyMessage::Ping(nonce))])
            }
        }
    }
}

/// Inventory item announcing a transaction: `MSG_WTX` for v5+, `MSG_TX` for v4 and
/// earlier (ZIP 239).
pub fn tx_inv_item(id: &hayai_wire::WtxId) -> InvItem {
    if id.auth_digest == hayai_wire::PRE_V5_AUTH_DIGEST {
        InvItem::Tx(id.txid)
    } else {
        InvItem::Wtx(*id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{features, Negotiated, NODE_COMPACT_RELAY, NODE_NETWORK};

    fn config(compact: Option<CompactVer>) -> SessionConfig {
        SessionConfig {
            protocol_version: 170_160,
            min_peer_version: 170_150,
            services: NODE_NETWORK | compact.map_or(0, |_| NODE_COMPACT_RELAY),
            user_agent: "/test/".into(),
            compact_relay: compact,
            handshake_timeout: Duration::from_secs(5),
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(5),
        }
    }

    fn peer_version(services: u64, nonce: u64) -> LegacyMessage {
        LegacyMessage::Version(VersionMessage {
            version: 170_150,
            services,
            timestamp: 0,
            addr_recv: NetAddr {
                services: 0,
                addr: ([127, 0, 0, 1], 1).into(),
            },
            addr_from: NetAddr {
                services,
                addr: ([0, 0, 0, 0], 0).into(),
            },
            nonce,
            user_agent: "/peer/".into(),
            start_height: 0,
            relay: true,
        })
    }

    fn start(compact: Option<CompactVer>) -> (PeerSession, Instant) {
        let now = Instant::now();
        let (s, first) = PeerSession::new(
            config(compact),
            Direction::Outbound,
            ([127, 0, 0, 1], 8233).into(),
            0,
            now,
            0,
        );
        assert!(matches!(first, LegacyMessage::Version(_)));
        (s, now)
    }

    fn handshake(s: &mut PeerSession, peer_services: u64) -> Vec<Action> {
        let mut actions = s.on_message(peer_version(peer_services, 7)).unwrap();
        assert_eq!(actions, vec![Action::Send(LegacyMessage::Verack)]);
        actions = s.on_message(LegacyMessage::Verack).unwrap();
        assert!(s.is_established());
        actions
    }

    #[test]
    fn legacy_peer_without_bit_never_sees_zcmpctver() {
        let (mut s, _) = start(Some(CompactVer::CURRENT));
        let actions = handshake(&mut s, NODE_NETWORK);
        assert_eq!(actions, vec![Action::Established]);
        assert_eq!(s.protocol(), PeerProtocol::Legacy);
        // A `zcmpct` from a legacy peer is ignored, a block is delivered.
        let got = s
            .on_message(LegacyMessage::Compact(hayai_relay::Message::TxAnnounce(
                hayai_relay::TxAnnounce { ids: vec![] },
            )))
            .unwrap();
        assert!(got.is_empty());
        let got = s.on_message(LegacyMessage::Mempool).unwrap();
        assert_eq!(got, vec![Action::Deliver(LegacyMessage::Mempool)]);
    }

    #[test]
    fn peer_with_bit_gets_zcmpctver_and_upgrades() {
        let (mut s, _) = start(Some(CompactVer::CURRENT));
        let actions = handshake(&mut s, NODE_NETWORK | NODE_COMPACT_RELAY);
        assert_eq!(
            actions,
            vec![
                Action::Established,
                Action::Send(LegacyMessage::CompactVer(CompactVer::CURRENT))
            ]
        );
        assert_eq!(s.protocol(), PeerProtocol::Legacy);
        let theirs = CompactVer {
            max_version: 1,
            min_version: 1,
            features: features::COMPACT_BLOCKS_V1 | 1 << 63,
        };
        let got = s.on_message(LegacyMessage::CompactVer(theirs)).unwrap();
        let expected = PeerProtocol::CompactRelay(Negotiated {
            version: 1,
            features: features::COMPACT_BLOCKS_V1,
        });
        assert_eq!(got, vec![Action::Upgraded(expected)]);
        assert_eq!(s.protocol(), expected);
        // A repeated zcmpctver changes nothing.
        let got = s.on_message(LegacyMessage::CompactVer(theirs)).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn zcmpctver_without_the_bit_is_answered() {
        let (mut s, _) = start(Some(CompactVer::CURRENT));
        handshake(&mut s, NODE_NETWORK);
        let got = s
            .on_message(LegacyMessage::CompactVer(CompactVer::CURRENT))
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0],
            Action::Send(LegacyMessage::CompactVer(CompactVer::CURRENT))
        );
        assert!(matches!(got[1], Action::Upgraded(_)));
    }

    #[test]
    fn version_mismatch_stays_legacy() {
        let (mut s, _) = start(Some(CompactVer::CURRENT));
        handshake(&mut s, NODE_NETWORK | NODE_COMPACT_RELAY);
        let theirs = CompactVer {
            max_version: 5,
            min_version: 3,
            features: features::KNOWN,
        };
        let got = s.on_message(LegacyMessage::CompactVer(theirs)).unwrap();
        assert!(got.is_empty());
        assert_eq!(s.protocol(), PeerProtocol::Legacy);
    }

    #[test]
    fn disabled_extension_is_a_legacy_node() {
        let (mut s, _) = start(None);
        let actions = handshake(&mut s, NODE_NETWORK | NODE_COMPACT_RELAY);
        assert_eq!(actions, vec![Action::Established]);
        let got = s
            .on_message(LegacyMessage::CompactVer(CompactVer::CURRENT))
            .unwrap();
        assert!(got.is_empty());
        assert_eq!(s.protocol(), PeerProtocol::Legacy);
    }

    #[test]
    fn handshake_errors() {
        let (mut s, _) = start(None);
        assert_eq!(
            s.on_message(LegacyMessage::Verack),
            Err(SessionError::BeforeHandshake("verack".into()))
        );
        assert_eq!(
            s.on_message(LegacyMessage::Mempool),
            Err(SessionError::BeforeHandshake("mempool".into()))
        );
        // sendaddrv2 before verack is allowed (BIP 155), as are pings.
        assert_eq!(s.on_message(LegacyMessage::SendAddrV2), Ok(vec![]));
        assert_eq!(
            s.on_message(LegacyMessage::Ping(3)),
            Ok(vec![Action::Send(LegacyMessage::Pong(3))])
        );
        let LegacyMessage::Version(mut old) = peer_version(NODE_NETWORK, 1) else {
            unreachable!()
        };
        old.version = 170_149;
        assert_eq!(
            s.on_message(LegacyMessage::Version(old)),
            Err(SessionError::VersionTooOld {
                version: 170_149,
                min: 170_150
            })
        );
        let (mut s, _) = start(None);
        assert_eq!(
            s.on_message(peer_version(NODE_NETWORK, s.nonce)),
            Err(SessionError::SelfConnection)
        );
        let (mut s, _) = start(None);
        handshake(&mut s, NODE_NETWORK);
        assert_eq!(
            s.on_message(peer_version(NODE_NETWORK, 9)),
            Err(SessionError::DuplicateVersion)
        );
        let (mut s, now) = start(None);
        assert_eq!(s.on_tick(now + Duration::from_secs(4)), Ok(vec![]));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(6)),
            Err(SessionError::HandshakeTimeout(Duration::from_secs(5)))
        );
    }

    #[test]
    fn ping_keepalive_and_timeout() {
        let (mut s, now) = start(None);
        handshake(&mut s, NODE_NETWORK);
        assert_eq!(s.on_tick(now + Duration::from_secs(5)), Ok(vec![]));
        let got = s.on_tick(now + Duration::from_secs(11)).unwrap();
        let [Action::Send(LegacyMessage::Ping(nonce))] = got.as_slice() else {
            panic!("expected a ping, got {got:?}");
        };
        let nonce = *nonce;
        assert_eq!(s.on_message(LegacyMessage::Pong(nonce ^ 1)), Ok(vec![]));
        assert_eq!(
            s.on_tick(now + Duration::from_secs(17)),
            Err(SessionError::PingTimeout(Duration::from_secs(5)))
        );
        let (mut s, now) = start(None);
        handshake(&mut s, NODE_NETWORK);
        let got = s.on_tick(now + Duration::from_secs(11)).unwrap();
        let [Action::Send(LegacyMessage::Ping(nonce))] = got.as_slice() else {
            panic!("expected a ping");
        };
        assert_eq!(s.on_message(LegacyMessage::Pong(*nonce)), Ok(vec![]));
        assert_eq!(s.on_tick(now + Duration::from_secs(17)), Ok(vec![]));
    }

    #[test]
    fn tx_inv_item_follows_zip239() {
        let v4 = hayai_wire::WtxId {
            txid: hayai_crypto::zcash_protocol::TxId::from_bytes([1; 32]),
            auth_digest: hayai_wire::PRE_V5_AUTH_DIGEST,
        };
        let v5 = hayai_wire::WtxId {
            txid: hayai_crypto::zcash_protocol::TxId::from_bytes([1; 32]),
            auth_digest: [2; 32],
        };
        assert!(matches!(tx_inv_item(&v4), InvItem::Tx(_)));
        assert!(matches!(tx_inv_item(&v5), InvItem::Wtx(_)));
    }
}
