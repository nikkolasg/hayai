//! Peer management: which peers the node accepts, which addresses it dials, and where it
//! finds addresses.
//!
//! [`PeerManager`] holds the address book ([`crate::addrbook`]), the misbehaviour scores
//! (`hayai_sync::score`), the limits and the clock. The relay asks it before it accepts a
//! connection and tells it what each peer does. [`PeerManager::maintain`] is one step of the
//! connection manager: it dials addresses from the book until the node has its target of
//! outbound peers, asks the DNS seeders when the book cannot fill the target, and saves the
//! book. [`PeerManager::spawn`] runs that step on a thread.
//!
//! The clock and the DNS resolver are injected ([`PeerEnv`]), so that tests use a fixed
//! time and never touch the public network.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hayai_sync::score::{Misbehaviour, ScoreBoard, Verdict, BAN_SECS, STALL_LIMIT};
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::addrbook::{group, ip_key, write_file, AddrBook, AddrBookConfig, Group, MAX_BANS};
use crate::codec::{Network, TimedNetAddr};
use crate::protocol::NODE_NETWORK;
use crate::relay::Relay;
use crate::session::Direction;

/// DNS seeders of Mainnet with the P2P port (Zakura `zakura-network/src/config.rs:874-877`,
/// Zebra `zebra-network/src/config.rs:549-552`).
pub const MAINNET_SEEDERS: [&str; 4] = [
    "dnsseed.str4d.xyz:8233",
    "dnsseed.z.cash:8233",
    "mainnet.seeder.shieldedinfra.net:8233",
    "mainnet.seeder.zfnd.org:8233",
];
/// DNS seeders of Testnet with the P2P port (Zakura `zakura-network/src/config.rs:883-884`,
/// Zebra `zebra-network/src/config.rs:559-560`).
pub const TESTNET_SEEDERS: [&str; 2] = [
    "dnsseed.testnet.z.cash:18233",
    "testnet.seeder.zfnd.org:18233",
];

/// Outbound peers that the node keeps (zcashd `MAX_OUTBOUND_CONNECTIONS`).
pub const DEFAULT_OUTBOUND_TARGET: usize = 8;
/// Inbound peers that the node accepts, at most.
pub const DEFAULT_MAX_INBOUND: usize = 64;
/// Connections with one IP address on a public network, at most (Zebra
/// `DEFAULT_MAX_CONNS_PER_IP`, `zebra-network/src/constants.rs:81`).
pub const DEFAULT_MAX_PER_IP: usize = 1;
/// Seconds between two saves of the address book (Zebra `PEER_DISK_CACHE_UPDATE_INTERVAL`,
/// `zebra-network/src/constants.rs:176`).
pub const DEFAULT_SAVE_INTERVAL_SECS: u64 = 5 * 60;
/// Seconds between two rounds of DNS seeder queries, at least.
pub const DEFAULT_SEED_INTERVAL_SECS: u64 = 10 * 60;

/// Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
/// Resolves a `host:port` seeder name.
pub type Resolver = Arc<dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

/// What a [`PeerManager`] reads from outside the process.
pub struct PeerEnv {
    pub clock: Clock,
    pub resolver: Resolver,
    /// Seed of the generator that orders the address selection. `None`: system entropy.
    pub rng_seed: Option<u64>,
}

impl PeerEnv {
    /// The system clock and the system resolver (`ToSocketAddrs`).
    pub fn system() -> Self {
        Self {
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
            }),
            resolver: Arc::new(|name| Ok(name.to_socket_addrs()?.collect())),
            rng_seed: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PeerConfig {
    /// Outbound peers that [`PeerManager::maintain`] keeps.
    pub outbound_target: usize,
    /// Inbound peers accepted, at most.
    pub max_inbound: usize,
    /// Connections with one IP address, at most, in both directions together.
    pub max_per_ip: usize,
    /// Outbound peers in one network group ([`Group`]), at most.
    pub outbound_per_group: usize,
    /// DNS seeders as `host:port`.
    pub seeders: Vec<String>,
    /// Seconds between two rounds of seeder queries, at least.
    pub seed_interval_secs: u64,
    /// Duration of a ban in seconds.
    pub ban_secs: u64,
    pub book: AddrBookConfig,
    /// File of the address book. `None`: the book is not saved.
    pub book_path: Option<PathBuf>,
    /// Seconds between two saves of the address book.
    pub save_interval_secs: u64,
    /// Period of [`PeerManager::maintain`] on the thread of [`PeerManager::spawn`].
    pub maintain_interval: Duration,
}

impl PeerConfig {
    /// The defaults of `network`. Regtest has no seeders, accepts local addresses, and has
    /// no bound per IP address or per group, because its nodes share the loopback address.
    pub fn new(network: Network) -> Self {
        let (seeders, local): (&[&str], bool) = match network {
            Network::Mainnet => (&MAINNET_SEEDERS, false),
            Network::Testnet => (&TESTNET_SEEDERS, false),
            Network::Regtest => (&[], true),
        };
        Self {
            outbound_target: DEFAULT_OUTBOUND_TARGET,
            max_inbound: DEFAULT_MAX_INBOUND,
            max_per_ip: if local {
                usize::MAX
            } else {
                DEFAULT_MAX_PER_IP
            },
            outbound_per_group: if local { usize::MAX } else { 1 },
            seeders: seeders.iter().map(|s| s.to_string()).collect(),
            seed_interval_secs: DEFAULT_SEED_INTERVAL_SECS,
            ban_secs: BAN_SECS,
            book: if local {
                AddrBookConfig::local()
            } else {
                AddrBookConfig::public()
            },
            book_path: None,
            save_interval_secs: DEFAULT_SAVE_INTERVAL_SECS,
            maintain_interval: Duration::from_secs(1),
        }
    }
}

/// Why the node refuses a connection.
#[derive(thiserror::Error, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    #[error("the IP address is banned")]
    Banned,
    #[error("the node has its maximum of inbound peers")]
    InboundFull,
    #[error("the node has its maximum of connections with this IP address")]
    PerIp,
    /// The peer left unanswered requests up to the stall limit. The node accepts it again
    /// when one stall decayed.
    #[error("the IP address has stalls up to the limit")]
    Stalled,
}

pub struct PeerManager {
    config: PeerConfig,
    clock: Clock,
    resolver: Resolver,
    book: Mutex<AddrBook>,
    scores: Mutex<ScoreBoard<IpAddr>>,
    rng: Mutex<StdRng>,
    /// Addresses that reached this node itself.
    own_addrs: Mutex<HashSet<SocketAddr>>,
    last_seed: Mutex<Option<u64>>,
    last_save: Mutex<u64>,
    /// Held for one [`PeerManager::maintain`] step, so that two steps never dial the same
    /// address.
    maintaining: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl PeerManager {
    /// A manager over `book` (from [`AddrBook::load`] or [`AddrBook::new`], with
    /// `config.book`).
    pub fn new(config: PeerConfig, book: AddrBook, env: PeerEnv) -> Arc<Self> {
        let rng = match env.rng_seed {
            Some(seed) => StdRng::seed_from_u64(seed),
            None => StdRng::from_entropy(),
        };
        let now = (env.clock)();
        Arc::new(Self {
            config,
            clock: env.clock,
            resolver: env.resolver,
            book: Mutex::new(book),
            scores: Mutex::new(ScoreBoard::new(MAX_BANS)),
            rng: Mutex::new(rng),
            own_addrs: Mutex::new(HashSet::new()),
            last_seed: Mutex::new(None),
            last_save: Mutex::new(now),
            maintaining: Mutex::new(()),
        })
    }

    /// A manager with the defaults of `network`, an empty book that is not saved, the
    /// system clock and the system resolver.
    pub fn with_defaults(network: Network) -> Arc<Self> {
        let config = PeerConfig::new(network);
        let book = AddrBook::new(config.book.clone());
        Self::new(config, book, PeerEnv::system())
    }

    pub fn config(&self) -> &PeerConfig {
        &self.config
    }

    /// Unix seconds of the injected clock.
    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    /// The address book, locked.
    pub fn book(&self) -> MutexGuard<'_, AddrBook> {
        lock(&self.book)
    }

    /// The misbehaviour points of `ip` at this time.
    pub fn score(&self, ip: IpAddr) -> u32 {
        lock(&self.scores).points(&ip_key(ip), self.now())
    }

    pub fn is_banned(&self, ip: IpAddr) -> bool {
        self.book().is_banned(ip, self.now())
    }

    /// Whether the node accepts one more connection with `ip`, when it has `same_ip`
    /// connections with the same [`ip_key`] and `inbound` inbound peers.
    pub fn admit(
        &self,
        ip: IpAddr,
        direction: Direction,
        same_ip: usize,
        inbound: usize,
    ) -> Result<(), Refusal> {
        if self.is_banned(ip) {
            return Err(Refusal::Banned);
        }
        if lock(&self.scores).stalls(&ip_key(ip), self.now()) >= STALL_LIMIT {
            return Err(Refusal::Stalled);
        }
        if same_ip >= self.config.max_per_ip {
            return Err(Refusal::PerIp);
        }
        if direction == Direction::Inbound && inbound >= self.config.max_inbound {
            return Err(Refusal::InboundFull);
        }
        Ok(())
    }

    /// Records one misbehaviour of the peer at `ip`. On [`Verdict::Ban`] the address book
    /// bans `ip` for `ban_secs`; the caller closes its connections.
    pub fn record(&self, ip: IpAddr, reason: Misbehaviour) -> Verdict {
        let now = self.now();
        let verdict = lock(&self.scores).record(ip_key(ip), reason, now);
        if let Verdict::Ban = verdict {
            self.book()
                .ban(ip, now.saturating_add(self.config.ban_secs), now);
        }
        verdict
    }

    /// The node has an outbound connection to `addr` and starts the handshake.
    pub(crate) fn on_attempt(&self, addr: &SocketAddr) {
        self.book().mark_attempt(addr, self.now());
    }

    /// The handshake with a peer completed. The book records an outbound peer only: the
    /// source port of an inbound peer is not the port that it listens on.
    pub(crate) fn on_established(&self, addr: &SocketAddr, direction: Direction, services: u64) {
        if direction == Direction::Outbound {
            self.book().mark_success(addr, services, self.now());
        }
    }

    /// An outbound connection ended before its handshake completed.
    pub(crate) fn on_failed(&self, addr: &SocketAddr) {
        self.book().mark_failed(addr, self.now());
    }

    /// `addr` is this node: the book drops it, does not take it again from a peer, and
    /// [`PeerManager::maintain`] never dials it.
    pub(crate) fn on_self_connection(&self, addr: &SocketAddr) {
        lock(&self.own_addrs).insert(*addr);
        self.book().remove(addr);
    }

    /// Addresses that the peer at `source` told. Returns those that are new to the book or
    /// newer than the book had them.
    pub(crate) fn on_addrs(&self, source: IpAddr, addrs: &[TimedNetAddr]) -> Vec<TimedNetAddr> {
        let own = lock(&self.own_addrs).clone();
        let addrs: Vec<TimedNetAddr> = addrs
            .iter()
            .filter(|a| !own.contains(&a.net.addr))
            .copied()
            .collect();
        self.book().add_gossiped(&addrs, source, self.now())
    }

    /// The addresses of one `getaddr` answer.
    pub(crate) fn getaddr_answer(&self) -> Vec<TimedNetAddr> {
        let now = self.now();
        let mut rng = lock(&self.rng);
        self.book().sample(now, &mut *rng)
    }

    /// Adds addresses from the configuration to the book.
    pub fn add_peers(&self, addrs: &[SocketAddr]) -> usize {
        self.book().add_local(addrs, NODE_NETWORK, self.now())
    }

    /// Asks every seeder and adds the answers to the book. Returns the number of new
    /// addresses. A seeder that fails is logged and the others are still asked.
    pub fn seed(&self) -> usize {
        *lock(&self.last_seed) = Some(self.now());
        let mut added = 0;
        for seeder in &self.config.seeders {
            match (self.resolver)(seeder) {
                Ok(addrs) => {
                    let new = self.add_peers(&addrs);
                    tracing::info!(seeder, resolved = addrs.len(), new, "DNS seeder answered");
                    added += new;
                }
                Err(e) => tracing::warn!(seeder, error = %e, "DNS seeder failed"),
            }
        }
        added
    }

    fn seed_due(&self, now: u64) -> bool {
        if self.config.seeders.is_empty() {
            return false;
        }
        match *lock(&self.last_seed) {
            Some(last) => now.saturating_sub(last) >= self.config.seed_interval_secs,
            None => true,
        }
    }

    /// The addresses to dial now so that `relay` reaches the outbound target.
    fn candidates(&self, relay: &Relay, now: u64) -> Vec<SocketAddr> {
        let peers = relay.peers();
        let outbound: Vec<SocketAddr> = peers
            .iter()
            .filter(|p| p.direction == Direction::Outbound)
            .map(|p| p.addr)
            .collect();
        let needed = self.config.outbound_target.saturating_sub(outbound.len());
        if needed == 0 {
            return Vec::new();
        }
        let used_groups: Vec<Group> = outbound.iter().map(|a| group(a.ip())).collect();
        let own = lock(&self.own_addrs).clone();
        let in_use = |addr: &SocketAddr| {
            own.contains(addr)
                || outbound.contains(addr)
                || peers
                    .iter()
                    .filter(|p| ip_key(p.addr.ip()) == ip_key(addr.ip()))
                    .count()
                    >= self.config.max_per_ip
        };
        let select = || {
            let mut rng = lock(&self.rng);
            self.book().select(
                now,
                needed,
                &in_use,
                &used_groups,
                self.config.outbound_per_group,
                &mut *rng,
            )
        };
        let picks = select();
        if picks.len() < needed && self.seed_due(now) && self.seed() > 0 {
            return select();
        }
        picks
    }

    /// One step of the connection manager.
    ///
    /// 1. When `relay` has fewer outbound peers than the target, the step selects addresses
    ///    from the book. When the book gives too few and the seeders are due, it asks them.
    /// 2. It dials the selected addresses at the same time and waits for the connections
    ///    (not for the handshakes). The relay records each attempt and each failure in the
    ///    book.
    /// 3. It saves the book when the save interval is over.
    pub fn maintain(&self, relay: &Arc<Relay>) {
        let _step = lock(&self.maintaining);
        let now = self.now();
        let picks = self.candidates(relay, now);
        thread::scope(|scope| {
            for addr in picks {
                scope.spawn(move || {
                    if let Err(e) = relay.connect(addr) {
                        tracing::debug!(%addr, error = %e, "outbound connection failed");
                    }
                });
            }
        });
        let due = {
            let mut last = lock(&self.last_save);
            let due = now.saturating_sub(*last) >= self.config.save_interval_secs;
            if due {
                *last = now;
            }
            due
        };
        if due {
            self.save();
        }
    }

    /// Saves the address book to `book_path`. A failure is logged: the file is a cache, and
    /// the node works without it.
    pub fn save(&self) {
        let Some(path) = &self.config.book_path else {
            return;
        };
        // The book is locked for the copy only, not for the write and the sync.
        let bytes = self.book().to_bytes();
        if let Err(e) = write_file(path, &bytes) {
            tracing::warn!(path = %path.display(), error = %e, "address book not saved");
        }
    }

    /// Runs [`PeerManager::maintain`] each `maintain_interval` on a thread, until `relay`
    /// shuts down or is dropped. The thread saves the book when it ends.
    pub fn spawn(self: &Arc<Self>, relay: &Arc<Relay>) -> io::Result<JoinHandle<()>> {
        let manager = Arc::clone(self);
        let weak = Arc::downgrade(relay);
        thread::Builder::new()
            .name("net-connect".into())
            .spawn(move || {
                loop {
                    let Some(relay) = weak.upgrade() else {
                        break;
                    };
                    if relay.is_stopped() {
                        break;
                    }
                    manager.maintain(&relay);
                    drop(relay);
                    thread::sleep(manager.config.maintain_interval);
                }
                manager.save();
            })
    }
}
