//! The relay as a node runs it: the TCP transports, the acceptor, the dialler of the peer
//! manager and the ticker, around the sans-IO [`RelayPolicy`] of `crate::policy`.
//!
//! [`Relay`] is the [`Io`] of the policy: a send goes to the transport of the peer, a
//! close removes it. Each peer has one reader thread that hands its messages to the policy
//! in order. The policy and its rules are in `crate::policy`; the public types of the relay
//! (`PeerId`, `RelayConfig`, the sinks and sources) are re-exported here.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hayai_relay::{BatchAnnounce, Publication, ResolvedCandidate};
use hayai_sync::score::Misbehaviour;
use hayai_wire::header::BlockHash;
use hayai_wire::{RawBlock, RawTx};

use crate::codec::LegacyMessage;
use crate::connect::{PeerManager, Refusal};
pub use crate::policy::{
    BlockSink, ChainSource, HistoryRootSource, IncomingBlock, PeerId, PeerInfo, RelayConfig,
    RelayCounters, RelayDeps, Source, SyncEvent, SyncSink, TxSink,
};
use crate::policy::{Io, RelayPolicy};
use crate::session::Direction;
use crate::transport::{ByteCounters, Control, Incoming, TcpTransport, Transport, TransportError};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub struct Relay {
    policy: RelayPolicy,
    /// The transport of each connection. The policy knows the same peers; a peer leaves
    /// the policy through [`Io::close`], which removes its transport.
    transports: Mutex<HashMap<PeerId, Arc<dyn Transport>>>,
    next_peer: AtomicU64,
    /// Bytes of the frames of all peers.
    bytes: Arc<ByteCounters>,
    listen_addr: Mutex<Option<SocketAddr>>,
    stopped: AtomicBool,
}

impl Relay {
    /// Builds the relay with a [`PeerManager`] that has the defaults of the network: an
    /// empty address book that is not saved, the system clock and the system resolver.
    pub fn new(config: RelayConfig, deps: RelayDeps) -> Arc<Self> {
        let peer_manager = PeerManager::with_defaults(config.network);
        Self::with_peer_manager(config, deps, peer_manager)
    }

    /// Builds the relay and starts its keepalive sweep. Nothing listens or connects until
    /// [`Relay::listen`], [`Relay::connect`] or [`PeerManager::maintain`] is called.
    pub fn with_peer_manager(
        config: RelayConfig,
        deps: RelayDeps,
        peer_manager: Arc<PeerManager>,
    ) -> Arc<Self> {
        let tick = config.tick;
        let relay = Arc::new(Self {
            policy: RelayPolicy::new(config, deps, peer_manager),
            transports: Mutex::new(HashMap::new()),
            next_peer: AtomicU64::new(1),
            bytes: Arc::default(),
            listen_addr: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&relay);
        thread::Builder::new()
            .name("net-ticker".into())
            .spawn(move || loop {
                thread::sleep(tick);
                let Some(relay) = weak.upgrade() else {
                    break;
                };
                if relay.stopped.load(Ordering::Acquire) {
                    break;
                }
                relay.policy.tick(&*relay, Instant::now());
            })
            .expect("spawn ticker thread");
        relay
    }

    pub fn config(&self) -> &RelayConfig {
        self.policy.config()
    }

    /// Bytes of the frames of all peers since the start: received, sent.
    pub fn bytes(&self) -> (u64, u64) {
        (
            self.bytes.received.load(Ordering::Relaxed),
            self.bytes.sent.load(Ordering::Relaxed),
        )
    }

    pub fn metrics(&self) -> RelayCounters {
        self.policy.metrics()
    }

    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        self.policy.peer_manager()
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Sets the oldest peer protocol version accepted and disconnects the established peers
    /// below it. The node calls this at each tip change with the version of the upgrade of
    /// the tip (`crate::protocol::min_peer_version`). A call with the current value does
    /// nothing: no peer below it can complete its handshake.
    ///
    /// ZIP 204, ZIP 201: at an upgrade activation, disconnect the peers below its version.
    pub fn set_min_peer_version(&self, version: u32) {
        self.policy.set_min_peer_version(self, version);
    }

    /// Binds `addr` and accepts peers on a background thread. Returns the bound address.
    pub fn listen(self: &Arc<Self>, addr: impl ToSocketAddrs) -> io::Result<SocketAddr> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        *lock(&self.listen_addr) = Some(local);
        let weak = Arc::downgrade(self);
        thread::Builder::new()
            .name(format!("net-accept-{local}"))
            .spawn(move || {
                for stream in listener.incoming() {
                    let Some(relay) = weak.upgrade() else {
                        break;
                    };
                    if relay.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    match stream {
                        Ok(stream) => {
                            if let Err(e) = relay.add_peer(stream, Direction::Inbound) {
                                tracing::debug!(error = %e, "inbound peer setup failed");
                            }
                        }
                        Err(e) => tracing::debug!(error = %e, "accept failed"),
                    }
                }
            })?;
        Ok(local)
    }

    pub fn listen_addr(&self) -> Option<SocketAddr> {
        *lock(&self.listen_addr)
    }

    /// Connects to a peer and starts its handshake. The address book records the attempt,
    /// and a failure before the handshake completes. A banned address is refused.
    pub fn connect(self: &Arc<Self>, addr: SocketAddr) -> io::Result<PeerId> {
        if self.peer_manager().is_banned(addr.ip()) {
            return Err(io::Error::other(Refusal::Banned));
        }
        let stream = match TcpStream::connect_timeout(&addr, self.config().connect_timeout) {
            Ok(stream) => stream,
            Err(e) => {
                self.peer_manager().on_failed(&addr);
                return Err(e);
            }
        };
        self.add_peer(stream, Direction::Outbound)
    }

    fn add_peer(self: &Arc<Self>, stream: TcpStream, direction: Direction) -> io::Result<PeerId> {
        let id = PeerId(self.next_peer.fetch_add(1, Ordering::Relaxed));
        let transport = TcpTransport::new(
            stream,
            self.config().network,
            self.config().outbound_queue,
            self.bytes.clone(),
        )?;
        let addr = transport.peer_addr();
        let admitted = self
            .policy
            .admit_peer(id, addr, direction, Instant::now(), unix_time());
        let (peer, version) = match admitted {
            Ok(admitted) => admitted,
            Err(refusal) => {
                transport.close();
                tracing::debug!(%addr, ?direction, %refusal, "connection refused");
                return Err(io::Error::other(refusal));
            }
        };
        lock(&self.transports).insert(id, transport.clone());
        if let Err(e) = transport.send(&version) {
            self.policy.remove_peer(&**self, id, &e.to_string());
            return Err(io::Error::other(e.to_string()));
        }
        let relay = Arc::clone(self);
        let started = transport.run_reader(Box::new(move |incoming| match incoming {
            Incoming::Message(m) => relay.policy.on_message(&*relay, &peer, m),
            Incoming::Closed(reason) => {
                relay.policy.reader_ended(&*relay, &peer, &reason, false);
                Control::Close
            }
            Incoming::Malformed(reason) => {
                relay.policy.reader_ended(&*relay, &peer, &reason, true);
                Control::Close
            }
        }));
        if let Err(e) = started {
            self.policy.remove_peer(&**self, id, &e.to_string());
            return Err(e);
        }
        tracing::debug!(%id, %addr, ?direction, "peer added");
        Ok(id)
    }

    /// Reports a misbehaviour that the node found outside the relay
    /// ([`RelayPolicy::misbehaved`]).
    pub fn misbehaved(&self, source: Source, reason: Misbehaviour) {
        self.policy.misbehaved(self, source, reason);
    }

    pub fn disconnect(&self, id: PeerId) {
        self.policy.disconnect(self, id);
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        self.policy.peers()
    }

    /// Sends a `ping` to each established peer that has no `ping` without a `pong`.
    /// [`Relay::peers`] shows the times.
    pub fn ping_peers(&self) {
        self.policy.ping_peers(self);
    }

    /// Closes every connection and stops the background threads.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let transports: Vec<Arc<dyn Transport>> =
            lock(&self.transports).drain().map(|(_, t)| t).collect();
        for t in transports {
            t.close();
        }
        self.policy.forget_all();
        // Wakes the acceptor so that it observes the stop flag.
        if let Some(addr) = self.listen_addr() {
            let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        }
    }

    /// Publishes a template change of this node's lane ([`RelayPolicy::publish_candidate`]).
    pub fn publish_candidate(&self, publication: Publication) {
        self.policy.publish_candidate(self, publication);
    }

    /// The received candidates on `parent` ([`RelayPolicy::candidates_on`]).
    pub fn candidates_on(&self, parent: &BlockHash, limit: usize) -> Vec<ResolvedCandidate> {
        self.policy.candidates_on(parent, limit)
    }

    /// Announces a locally accepted transaction on both paths.
    pub fn announce_tx(&self, tx: &RawTx) {
        self.policy.announce_tx(self, tx);
    }

    /// Publishes a batch of this node's lane ([`RelayPolicy::announce_batch`]).
    pub fn announce_batch(&self, announce: BatchAnnounce) {
        self.policy.announce_batch(self, announce);
    }

    /// A block this node found.
    pub fn block_found(&self, block: RawBlock) {
        self.policy.block_found(self, block);
    }

    /// A block that the node downloaded ([`RelayPolicy::forward_block`]).
    pub fn forward_block(&self, block: Arc<RawBlock>, source: Source) {
        self.policy.forward_block(self, block, source);
    }

    /// The node validated the block `hash` ([`RelayPolicy::block_validated`]).
    pub fn block_validated(&self, hash: &BlockHash) {
        self.policy.block_validated(self, hash);
    }

    /// Sends `getheaders` with `locator` to the peer `id`. Returns `false` when the peer is
    /// not connected.
    pub fn send_getheaders(&self, id: PeerId, locator: Vec<BlockHash>) -> bool {
        self.policy.send_getheaders(self, id, locator)
    }

    /// Sends one `getdata` for the blocks `hashes` to the peer `id`. Returns `false` when
    /// the peer is not connected.
    pub fn request_blocks(&self, id: PeerId, hashes: &[BlockHash]) -> bool {
        self.policy.request_blocks(self, id, hashes)
    }
}

/// The transports as the policy sees them. A transport is looked up under the lock and
/// used after its release, so a slow send never holds the lock.
impl Io for Relay {
    fn send(&self, id: PeerId, message: &LegacyMessage) -> Result<(), TransportError> {
        let transport = lock(&self.transports).get(&id).cloned();
        match transport {
            Some(transport) => transport.send(message),
            None => Err(TransportError::Closed),
        }
    }

    fn queued_bytes(&self, id: PeerId) -> usize {
        let transport = lock(&self.transports).get(&id).cloned();
        transport.map_or(0, |t| t.queued_bytes())
    }

    fn close(&self, id: PeerId) {
        let transport = lock(&self.transports).remove(&id);
        if let Some(transport) = transport {
            transport.close();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}
