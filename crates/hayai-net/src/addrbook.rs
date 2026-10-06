//! The address book: the peer addresses that the node knows, with a bound on their number.
//!
//! The book is pure data. It has no clock: each method takes the time as Unix seconds.
//! File I/O is only in [`AddrBook::save`] and [`AddrBook::load`].
//!
//! - Each [`AddrEntry`] has a state ([`AddrState`]): never tried, responded, or failed.
//! - [`AddrBook::select`] gives the addresses for new outbound connections. It prefers the
//!   responded addresses, then the failed addresses that responded before, then the
//!   addresses that were never tried, then the failed addresses that never responded.
//!   It spreads the connections across network groups ([`Group`]: /16 for IPv4, /32 for
//!   IPv6), as Bitcoin Core does.
//! - Addresses from peers are not trusted. [`AddrBook::add_gossiped`] applies the timestamp
//!   rule of zcashd, refuses addresses that cannot be dialled, and bounds the entries of
//!   one source peer. [`AddrBudget`] bounds the addresses that one connection can add.
//! - A full book never drops an address that responded at any time for a new one. See
//!   [`AddrBook::add_gossiped`].
//! - Bans are held with their end time for each [`ip_key`]: an IPv4 address, or the /64 of
//!   an IPv6 address.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

use rand::seq::SliceRandom;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::codec::{NetAddr, TimedNetAddr, MAX_ADDR_ENTRIES};

/// Addresses in the book, at most.
pub const DEFAULT_CAPACITY: usize = 4096;
/// Addresses that one source peer can hold in the book: 1/16 of the default capacity.
pub const DEFAULT_MAX_PER_SOURCE: usize = 256;
/// Banned IP addresses kept, at most (Zakura `MAX_BANNED_IPS`,
/// `zakura-network/src/constants.rs:415`).
pub const MAX_BANS: usize = 20_000;

/// A timestamp at or below this value is not a time (zcashd `ProcessMessage`, `addr`).
pub const MIN_VALID_TIME: u64 = 100_000_000;
/// A timestamp more than this many seconds after the local time is not accepted (zcashd).
pub const FUTURE_LIMIT_SECS: u64 = 10 * 60;
/// The age given to an address whose timestamp is not accepted: 5 days (zcashd).
pub const UNKNOWN_TIME_AGE_SECS: u64 = 5 * 24 * 60 * 60;
/// The age added to every address that a peer tells about another node: 2 hours (zcashd
/// `addrman.Add(vAddrOk, pfrom->addr, 2 * 60 * 60)`).
pub const GOSSIP_PENALTY_SECS: u64 = 2 * 60 * 60;
/// An unsolicited address not older than this is sent on to other peers (zcashd: 10
/// minutes).
pub const RELAY_MAX_AGE_SECS: u64 = 10 * 60;

/// A `getaddr` answer holds addresses seen within this time: 3 hours (Zebra
/// `MAX_PEER_ACTIVE_FOR_GOSSIP`, `zebra-network/src/constants.rs:195`).
pub const GOSSIP_MAX_AGE_SECS: u64 = 3 * 60 * 60;
/// A `getaddr` answer holds this share of the book, at most (zcashd
/// `ADDRMAN_GETADDR_MAX_PCT`).
pub const GETADDR_MAX_PERCENT: usize = 23;
/// Timestamps in a `getaddr` answer are rounded down to this interval: 30 minutes (Zebra
/// `TIMESTAMP_TRUNCATION_SECONDS`, `zebra-network/src/constants.rs:330`).
pub const TIMESTAMP_TRUNCATION_SECS: u64 = 30 * 60;

/// Seconds between two attempts on one address, at least (Zebra
/// `MIN_PEER_RECONNECTION_DELAY`, `zebra-network/src/constants.rs:148`). Each failure
/// doubles the delay.
pub const RETRY_BASE_SECS: u64 = 119;
/// The longest delay between two attempts on one address: 6 hours.
pub const RETRY_MAX_SECS: u64 = 6 * 60 * 60;
/// An address that never responded leaves the book after this many failures (Bitcoin Core
/// `ADDRMAN_RETRIES`).
pub const MAX_FAILURES_UNTRIED: u32 = 3;
/// An address leaves the book after this many failures in a row (Bitcoin Core
/// `ADDRMAN_MAX_FAILURES`).
pub const MAX_FAILURES: u32 = 10;

/// Addresses that one connection can add at once (Bitcoin Core
/// `MAX_ADDR_PROCESSING_TOKEN_BUCKET`). A `getaddr` that the node sends adds this allowance.
/// ZIP 204: the address rate limit of zcashd, a bucket of 1,000 at 0.1 address per second.
pub const ADDR_BUDGET_BURST: u64 = MAX_ADDR_ENTRIES as u64;
/// Seconds for one more address in the budget of a connection (Bitcoin Core
/// `MAX_ADDR_RATE_PER_SECOND`: 0.1).
pub const ADDR_BUDGET_SECS_PER_ADDR: u64 = 10;

const FILE_MAGIC: &[u8; 4] = b"HYAB";
const FILE_VERSION: u32 = 1;
const ENTRY_LEN: usize = 16 + 2 + 8 + 8 + 8 + 8 + 4 + 16;
const BAN_LEN: usize = 16 + 8;
const HASH_LEN: usize = 32;

/// The network group of an address: connections are spread across groups.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Group {
    /// The /16 of an IPv4 address.
    V4([u8; 2]),
    /// The /32 of an IPv6 address.
    V6([u8; 4]),
}

/// The group of `ip`.
pub fn group(ip: IpAddr) -> Group {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            Group::V4([o[0], o[1]])
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            Group::V6([o[0], o[1], o[2], o[3]])
        }
    }
}

/// The identity of a peer for the bans, the scores and the limit of connections: the IPv4
/// address, or the /64 of an IPv6 address (one host holds a whole /64).
pub fn ip_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

/// Whether a node on the public network can dial `addr`.
pub fn is_routable(addr: &SocketAddr) -> bool {
    if addr.port() == 0 {
        return false;
    }
    match addr.ip() {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_documentation()
                // 100.64.0.0/10 (RFC 6598) and 198.18.0.0/15 (RFC 2544).
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] == 0
                || o[0] >= 240)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                // fc00::/7 (unique local), fe80::/10 (link local), 2001:db8::/32
                // (documentation).
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

/// The time that the book keeps for an address with the timestamp `time` (zcashd
/// `ProcessMessage`, `addr`): a timestamp at or below [`MIN_VALID_TIME`], or more than
/// [`FUTURE_LIMIT_SECS`] after `now`, becomes `now` minus [`UNKNOWN_TIME_AGE_SECS`].
pub fn clamp_time(time: u32, now: u64) -> u64 {
    let time = u64::from(time);
    if time <= MIN_VALID_TIME || time > now + FUTURE_LIMIT_SECS {
        now.saturating_sub(UNKNOWN_TIME_AGE_SECS)
    } else {
        time
    }
}

/// The addresses of an unsolicited `addr` message that the node sends on to other peers
/// (zcashd): the message had at most 10 entries (`message_len`), and the address can be
/// dialled and is not older than [`RELAY_MAX_AGE_SECS`] after [`clamp_time`].
pub fn relayable(addrs: &[TimedNetAddr], message_len: usize, now: u64) -> Vec<TimedNetAddr> {
    if message_len > 10 {
        return Vec::new();
    }
    addrs
        .iter()
        .filter(|a| {
            is_routable(&a.net.addr)
                && clamp_time(a.time, now) > now.saturating_sub(RELAY_MAX_AGE_SECS)
        })
        .copied()
        .collect()
}

/// The addresses that one connection can still add: a token bucket (Bitcoin Core
/// `m_addr_token_bucket`).
#[derive(Clone, Copy, Debug)]
pub struct AddrBudget {
    tokens: u64,
    /// Time from which the next token accrues.
    since: u64,
}

impl AddrBudget {
    /// A new connection can add one address.
    pub fn new(now: u64) -> Self {
        Self {
            tokens: 1,
            since: now,
        }
    }

    /// The node sent `getaddr` on this connection: the answer can hold a full message.
    pub fn grant_getaddr(&mut self) {
        self.tokens += ADDR_BUDGET_BURST;
    }

    /// How many of `wanted` addresses the connection can add at `now`. The budget
    /// decreases by the result.
    pub fn take(&mut self, wanted: usize, now: u64) -> usize {
        let accrued = now.saturating_sub(self.since) / ADDR_BUDGET_SECS_PER_ADDR;
        self.since += accrued * ADDR_BUDGET_SECS_PER_ADDR;
        if self.tokens < ADDR_BUDGET_BURST {
            self.tokens = (self.tokens + accrued).min(ADDR_BUDGET_BURST);
        }
        let granted = (wanted as u64).min(self.tokens);
        self.tokens -= granted;
        granted as usize
    }
}

/// What the node did with an address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum AddrState {
    /// The last attempt completed a handshake.
    Responded,
    /// No attempt yet.
    NeverTried,
    /// The last attempt failed.
    Failed,
}

/// One address of the book.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AddrEntry {
    pub addr: SocketAddr,
    /// Service bits: from the `version` of the peer when it responded, else as told.
    pub services: u64,
    /// Unix seconds: the last handshake, else the time that a peer told, after the rules.
    pub last_seen: u64,
    pub last_attempt: Option<u64>,
    pub last_success: Option<u64>,
    /// Failed attempts since the last success.
    pub failures: u32,
    /// The IP address of the peer that told this address. Unspecified for an address from a
    /// seeder or from the configuration.
    pub source: IpAddr,
}

impl AddrEntry {
    pub fn state(&self) -> AddrState {
        match (self.failures, self.last_success) {
            (0, Some(_)) => AddrState::Responded,
            (0, None) => AddrState::NeverTried,
            _ => AddrState::Failed,
        }
    }

    /// The order of [`AddrBook::select`]: responded, failed after a success, never tried,
    /// failed without a success.
    fn rank(&self) -> u8 {
        match (self.state(), self.last_success) {
            (AddrState::Responded, _) => 0,
            (AddrState::Failed, Some(_)) => 1,
            (AddrState::NeverTried, _) => 2,
            (AddrState::Failed, None) => 3,
        }
    }

    /// The first time at which the node can dial this address again.
    fn next_attempt(&self, retry_base_secs: u64) -> u64 {
        let Some(last) = self.last_attempt else {
            return 0;
        };
        let delay = retry_base_secs
            .saturating_mul(1u64 << self.failures.min(20))
            .min(RETRY_MAX_SECS.max(retry_base_secs));
        last.saturating_add(delay)
    }
}

/// The source of the addresses that the node adds itself.
const LOCAL_SOURCE: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddrBookConfig {
    /// Addresses in the book, at most.
    pub capacity: usize,
    /// Addresses that one source peer can hold in the book, at most.
    pub max_per_source: usize,
    /// Accept loopback and private addresses (Regtest).
    pub allow_local: bool,
    /// Seconds between two attempts on one address, at least. Each failure doubles it.
    pub retry_base_secs: u64,
}

impl AddrBookConfig {
    /// The defaults for a public network.
    pub fn public() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY,
            max_per_source: DEFAULT_MAX_PER_SOURCE,
            allow_local: false,
            retry_base_secs: RETRY_BASE_SECS,
        }
    }

    /// The defaults for Regtest: every address that has a port is accepted.
    pub fn local() -> Self {
        Self {
            allow_local: true,
            ..Self::public()
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum AddrBookError {
    #[error("address book file: {0}")]
    Io(#[from] io::Error),
    #[error("address book file is damaged: {0}")]
    Damaged(&'static str),
}

pub struct AddrBook {
    config: AddrBookConfig,
    entries: BTreeMap<SocketAddr, AddrEntry>,
    /// Banned IP address and the Unix second at which the ban ends.
    bans: BTreeMap<IpAddr, u64>,
}

impl AddrBook {
    pub fn new(config: AddrBookConfig) -> Self {
        assert!(config.capacity > 0, "an address book needs a capacity");
        Self {
            config,
            entries: BTreeMap::new(),
            bans: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, addr: &SocketAddr) -> Option<&AddrEntry> {
        self.entries.get(addr)
    }

    pub fn entries(&self) -> impl Iterator<Item = &AddrEntry> {
        self.entries.values()
    }

    fn acceptable(&self, addr: &SocketAddr) -> bool {
        if self.config.allow_local {
            addr.port() != 0 && !addr.ip().is_unspecified()
        } else {
            is_routable(addr)
        }
    }

    /// Adds addresses that the peer at `source` told, at `now`. Returns the addresses that
    /// are new to the book or newer than the book had them.
    ///
    /// - The timestamp follows [`clamp_time`], then loses [`GOSSIP_PENALTY_SECS`].
    /// - An address that cannot be dialled, or whose IP address is banned, is dropped.
    /// - A known address keeps its history. Its `last_seen` moves forward only.
    /// - `source` holds at most `max_per_source` entries.
    /// - When the book is full, the new address replaces the failed entry with the most
    ///   failures among the entries that never responded. Without such an entry it replaces
    ///   the oldest never-tried entry of the source peer that holds the most never-tried
    ///   entries. An entry that responded at any time, and an entry from a seeder or from
    ///   the configuration, is never replaced: without another entry to replace, the new
    ///   address is dropped.
    pub fn add_gossiped(
        &mut self,
        addrs: &[TimedNetAddr],
        source: IpAddr,
        now: u64,
    ) -> Vec<TimedNetAddr> {
        // The limit is for the /64 of an IPv6 source, as the bans and the scores are.
        let mut from_source = self
            .entries
            .values()
            .filter(|e| ip_key(e.source) == ip_key(source))
            .count();
        let mut news = Vec::new();
        for a in addrs {
            let addr = a.net.addr;
            if !self.acceptable(&addr) || self.is_banned(addr.ip(), now) {
                continue;
            }
            let last_seen = clamp_time(a.time, now).saturating_sub(GOSSIP_PENALTY_SECS);
            if let Some(known) = self.entries.get_mut(&addr) {
                if last_seen > known.last_seen {
                    known.last_seen = last_seen;
                    news.push(*a);
                }
                continue;
            }
            if from_source >= self.config.max_per_source {
                continue;
            }
            let entry = AddrEntry {
                addr,
                services: a.net.services,
                last_seen,
                last_attempt: None,
                last_success: None,
                failures: 0,
                source,
            };
            if self.insert(entry) {
                from_source += 1;
                news.push(*a);
            }
        }
        news
    }

    /// Adds addresses from a seeder or from the configuration, at `now`. They count for no
    /// source peer. Their `last_seen` is `now` minus [`UNKNOWN_TIME_AGE_SECS`], so that a
    /// `getaddr` answer never holds an address that no node confirmed.
    pub fn add_local(&mut self, addrs: &[SocketAddr], services: u64, now: u64) -> usize {
        let mut added = 0;
        for addr in addrs {
            if !self.acceptable(addr)
                || self.is_banned(addr.ip(), now)
                || self.entries.contains_key(addr)
            {
                continue;
            }
            let entry = AddrEntry {
                addr: *addr,
                services,
                last_seen: now.saturating_sub(UNKNOWN_TIME_AGE_SECS),
                last_attempt: None,
                last_success: None,
                failures: 0,
                source: LOCAL_SOURCE,
            };
            if self.insert(entry) {
                added += 1;
            }
        }
        added
    }

    /// Inserts a new entry, with a replacement when the book is full. `false`: dropped.
    fn insert(&mut self, entry: AddrEntry) -> bool {
        if self.entries.len() >= self.config.capacity {
            let Some(victim) = self.victim() else {
                return false;
            };
            self.entries.remove(&victim);
        }
        self.entries.insert(entry.addr, entry);
        true
    }

    /// The entry that a new address replaces in a full book.
    fn victim(&self) -> Option<SocketAddr> {
        let failed = self
            .entries
            .values()
            .filter(|e| e.rank() == 3)
            .max_by_key(|e| (e.failures, std::cmp::Reverse(e.last_seen)));
        if let Some(e) = failed {
            return Some(e.addr);
        }
        let mut untried_per_source: HashMap<IpAddr, usize> = HashMap::new();
        for e in self.entries.values() {
            if e.state() == AddrState::NeverTried && e.source != LOCAL_SOURCE {
                *untried_per_source.entry(e.source).or_default() += 1;
            }
        }
        // The IP address breaks a tie, so that the choice does not depend on map order.
        let (largest, _) = untried_per_source
            .into_iter()
            .max_by_key(|(ip, count)| (*count, *ip))?;
        self.entries
            .values()
            .filter(|e| e.state() == AddrState::NeverTried && e.source == largest)
            .min_by_key(|e| e.last_seen)
            .map(|e| e.addr)
    }

    /// Up to `count` addresses for new outbound connections at `now`.
    ///
    /// An address is a candidate when its retry delay is over, its IP address is not banned
    /// and `in_use` is false for it. The responded addresses come first, then the failed
    /// that responded before, then the never-tried, then the failed that never responded;
    /// the order inside a class is random. At most `per_group`
    /// addresses share a [`Group`], counting the groups in `used_groups` (the groups of the
    /// current outbound peers).
    pub fn select(
        &self,
        now: u64,
        count: usize,
        in_use: &dyn Fn(&SocketAddr) -> bool,
        used_groups: &[Group],
        per_group: usize,
        rng: &mut dyn RngCore,
    ) -> Vec<SocketAddr> {
        let mut candidates: Vec<&AddrEntry> = self
            .entries
            .values()
            .filter(|e| {
                e.next_attempt(self.config.retry_base_secs) <= now
                    && !self.is_banned(e.addr.ip(), now)
                    && !in_use(&e.addr)
            })
            .collect();
        candidates.shuffle(rng);
        candidates.sort_by_key(|e| e.rank());
        let mut groups: HashMap<Group, usize> = HashMap::new();
        for g in used_groups {
            *groups.entry(*g).or_default() += 1;
        }
        let mut selected = Vec::new();
        for e in candidates {
            if selected.len() >= count {
                break;
            }
            let used = groups.entry(group(e.addr.ip())).or_default();
            if *used >= per_group {
                continue;
            }
            *used += 1;
            selected.push(e.addr);
        }
        selected
    }

    /// The node starts a connection to `addr`.
    pub fn mark_attempt(&mut self, addr: &SocketAddr, now: u64) {
        if let Some(e) = self.entries.get_mut(addr) {
            e.last_attempt = Some(now);
        }
    }

    /// The handshake with `addr` completed; `services` are those of its `version`.
    pub fn mark_success(&mut self, addr: &SocketAddr, services: u64, now: u64) {
        if let Some(e) = self.entries.get_mut(addr) {
            e.services = services;
            e.last_seen = now;
            e.last_success = Some(now);
            e.failures = 0;
        }
    }

    /// The connection to `addr` failed before the handshake completed. The entry leaves the
    /// book after [`MAX_FAILURES_UNTRIED`] failures when it never responded, and after
    /// [`MAX_FAILURES`] failures in a row otherwise.
    pub fn mark_failed(&mut self, addr: &SocketAddr, now: u64) {
        let Some(e) = self.entries.get_mut(addr) else {
            return;
        };
        e.last_attempt = Some(now);
        e.failures = e.failures.saturating_add(1);
        let limit = match e.last_success {
            Some(_) => MAX_FAILURES,
            None => MAX_FAILURES_UNTRIED,
        };
        if e.failures >= limit {
            self.entries.remove(addr);
        }
    }

    pub fn remove(&mut self, addr: &SocketAddr) {
        self.entries.remove(addr);
    }

    /// Bans `ip` until the Unix second `until` and removes its addresses. When the ban list
    /// is full, the expired bans go first, then the ban that ends first.
    pub fn ban(&mut self, ip: IpAddr, until: u64, now: u64) {
        let ip = ip_key(ip);
        self.entries.retain(|addr, _| ip_key(addr.ip()) != ip);
        if !self.bans.contains_key(&ip) && self.bans.len() >= MAX_BANS {
            self.bans.retain(|_, end| *end > now);
            if self.bans.len() >= MAX_BANS {
                let first = self
                    .bans
                    .iter()
                    .min_by_key(|(_, end)| **end)
                    .map(|(ip, _)| *ip);
                if let Some(first) = first {
                    self.bans.remove(&first);
                }
            }
        }
        let end = self.bans.entry(ip).or_insert(until);
        *end = (*end).max(until);
    }

    pub fn is_banned(&self, ip: IpAddr, now: u64) -> bool {
        matches!(self.bans.get(&ip_key(ip)), Some(until) if *until > now)
    }

    /// A random sample for a `getaddr` answer at `now`: addresses that responded to this
    /// node, seen within [`GOSSIP_MAX_AGE_SECS`], not failed and not banned, at most
    /// [`GETADDR_MAX_PERCENT`] percent of the book (rounded up) and at most
    /// [`MAX_ADDR_ENTRIES`]. The timestamps are rounded down to
    /// [`TIMESTAMP_TRUNCATION_SECS`].
    pub fn sample(&self, now: u64, rng: &mut dyn RngCore) -> Vec<TimedNetAddr> {
        let mut recent: Vec<&AddrEntry> = self
            .entries
            .values()
            .filter(|e| {
                e.state() == AddrState::Responded
                    && e.last_seen.saturating_add(GOSSIP_MAX_AGE_SECS) >= now
                    && !self.is_banned(e.addr.ip(), now)
            })
            .collect();
        recent.shuffle(rng);
        let limit = (self.entries.len() * GETADDR_MAX_PERCENT)
            .div_ceil(100)
            .min(MAX_ADDR_ENTRIES);
        recent
            .into_iter()
            .take(limit)
            .map(|e| {
                let time =
                    e.last_seen.min(now) / TIMESTAMP_TRUNCATION_SECS * TIMESTAMP_TRUNCATION_SECS;
                TimedNetAddr {
                    time: u32::try_from(time).unwrap_or(u32::MAX),
                    net: NetAddr {
                        services: e.services,
                        addr: e.addr,
                    },
                }
            })
            .collect()
    }

    /// Writes the book to `path`: the bytes go to `<path>.tmp`, are synced, and the file is
    /// renamed over `path`, so that a crash leaves the old file or the new one.
    ///
    /// Layout (integers little-endian): `"HYAB"`, version `u32`, entry count `u32`, the
    /// entries, ban count `u32`, the bans, then SHA-256 of every byte before it. An entry is
    /// `ip [16] (IPv4 mapped), port u16, services u64, last_seen u64, last_attempt u64,
    /// last_success u64 (0: none), failures u32, source ip [16]`. A ban is `ip [16], end u64`.
    pub fn save(&self, path: &Path) -> Result<(), AddrBookError> {
        write_file(path, &self.to_bytes())
    }

    /// The bytes of the book file. See [`AddrBook::save`].
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            16 + self.entries.len() * ENTRY_LEN + self.bans.len() * BAN_LEN + HASH_LEN,
        );
        out.extend_from_slice(FILE_MAGIC);
        out.extend_from_slice(&FILE_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for e in self.entries.values() {
            out.extend_from_slice(&ip_bytes(e.addr.ip()));
            out.extend_from_slice(&e.addr.port().to_le_bytes());
            out.extend_from_slice(&e.services.to_le_bytes());
            out.extend_from_slice(&e.last_seen.to_le_bytes());
            out.extend_from_slice(&e.last_attempt.unwrap_or(0).to_le_bytes());
            out.extend_from_slice(&e.last_success.unwrap_or(0).to_le_bytes());
            out.extend_from_slice(&e.failures.to_le_bytes());
            out.extend_from_slice(&ip_bytes(e.source));
        }
        out.extend_from_slice(&(self.bans.len() as u32).to_le_bytes());
        for (ip, until) in &self.bans {
            out.extend_from_slice(&ip_bytes(*ip));
            out.extend_from_slice(&until.to_le_bytes());
        }
        let hash = Sha256::digest(&out);
        out.extend_from_slice(&hash);
        out
    }

    /// Reads the book that [`AddrBook::save`] wrote. A missing file gives an empty book. A
    /// file with a wrong magic, version, length or hash gives [`AddrBookError::Damaged`]:
    /// the caller decides (the file is a cache; the seeders fill an empty book again).
    /// Entries that `config` does not accept, and entries above its capacity, are dropped.
    pub fn load(path: &Path, config: AddrBookConfig) -> Result<Self, AddrBookError> {
        let mut book = Self::new(config);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(book),
            Err(e) => return Err(e.into()),
        };
        let Some(body_len) = bytes.len().checked_sub(HASH_LEN) else {
            return Err(AddrBookError::Damaged("shorter than its hash"));
        };
        let (body, hash) = bytes.split_at(body_len);
        if Sha256::digest(body).as_slice() != hash {
            return Err(AddrBookError::Damaged("hash mismatch"));
        }
        let mut r = FileReader(body);
        if r.take::<4>()? != *FILE_MAGIC {
            return Err(AddrBookError::Damaged("wrong magic"));
        }
        if r.u32()? != FILE_VERSION {
            return Err(AddrBookError::Damaged("unknown version"));
        }
        let entries = r.u32()? as usize;
        if entries.saturating_mul(ENTRY_LEN) > r.0.len() {
            return Err(AddrBookError::Damaged("entry count exceeds the file"));
        }
        for _ in 0..entries {
            let addr = SocketAddr::new(r.ip()?, u16::from_le_bytes(r.take()?));
            let entry = AddrEntry {
                addr,
                services: r.u64()?,
                last_seen: r.u64()?,
                last_attempt: Some(r.u64()?).filter(|t| *t != 0),
                last_success: Some(r.u64()?).filter(|t| *t != 0),
                failures: r.u32()?,
                source: r.ip()?,
            };
            if book.acceptable(&addr) && book.entries.len() < book.config.capacity {
                book.entries.insert(addr, entry);
            }
        }
        let bans = r.u32()? as usize;
        if bans.saturating_mul(BAN_LEN) != r.0.len() {
            return Err(AddrBookError::Damaged("ban count does not match the file"));
        }
        for _ in 0..bans.min(MAX_BANS) {
            let ip = r.ip()?;
            book.bans.insert(ip, r.u64()?);
        }
        Ok(book)
    }
}

/// Writes `bytes` to `<path>.tmp`, syncs the file and renames it over `path`.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), AddrBookError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let mut file = File::create(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)?;
    Ok(())
}

/// An IP address as 16 bytes, IPv4 mapped.
fn ip_bytes(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

struct FileReader<'a>(&'a [u8]);

impl FileReader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], AddrBookError> {
        let Some((head, rest)) = self.0.split_first_chunk::<N>() else {
            return Err(AddrBookError::Damaged("ends early"));
        };
        self.0 = rest;
        Ok(*head)
    }
    fn u32(&mut self) -> Result<u32, AddrBookError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, AddrBookError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn ip(&mut self) -> Result<IpAddr, AddrBookError> {
        let v6 = Ipv6Addr::from(self.take::<16>()?);
        Ok(match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const NOW: u64 = 1_800_000_000;

    /// A public address: `group` selects the /16, `host` the address inside it.
    fn addr(group: u16, host: u16) -> SocketAddr {
        let [g0, g1] = group.to_be_bytes();
        let [h0, h1] = host.to_be_bytes();
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(11 + g0, g1, h0, h1)), 8233)
    }

    fn source(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(9, 9, 9, n))
    }

    fn timed(addr: SocketAddr, time: u64) -> TimedNetAddr {
        TimedNetAddr {
            time: time as u32,
            net: NetAddr { services: 1, addr },
        }
    }

    fn book(capacity: usize, max_per_source: usize) -> AddrBook {
        AddrBook::new(AddrBookConfig {
            capacity,
            max_per_source,
            ..AddrBookConfig::public()
        })
    }

    fn rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    fn select(book: &AddrBook, now: u64, count: usize, used: &[Group]) -> Vec<SocketAddr> {
        book.select(now, count, &|_| false, used, 1, &mut rng())
    }

    #[test]
    fn timestamps_follow_the_zcashd_rule() {
        let unknown = NOW - UNKNOWN_TIME_AGE_SECS;
        assert_eq!(clamp_time(0, NOW), unknown);
        assert_eq!(clamp_time(MIN_VALID_TIME as u32, NOW), unknown);
        assert_eq!(
            clamp_time(MIN_VALID_TIME as u32 + 1, NOW),
            MIN_VALID_TIME + 1
        );
        assert_eq!(clamp_time(NOW as u32, NOW), NOW);
        assert_eq!(clamp_time((NOW + 600) as u32, NOW), NOW + 600);
        assert_eq!(clamp_time((NOW + 601) as u32, NOW), unknown);
        assert_eq!(clamp_time(u32::MAX, NOW), unknown);
    }

    #[test]
    fn gossip_gets_the_penalty_and_the_time_rule() {
        let mut b = book(16, 16);
        let news = b.add_gossiped(
            &[
                timed(addr(1, 1), NOW - 100),
                timed(addr(2, 1), NOW + 5_000),
                timed(addr(3, 1), 5),
            ],
            source(1),
            NOW,
        );
        assert_eq!(news.len(), 3);
        assert_eq!(
            b.get(&addr(1, 1)).unwrap().last_seen,
            NOW - 100 - GOSSIP_PENALTY_SECS
        );
        let unknown = NOW - UNKNOWN_TIME_AGE_SECS - GOSSIP_PENALTY_SECS;
        assert_eq!(b.get(&addr(2, 1)).unwrap().last_seen, unknown);
        assert_eq!(b.get(&addr(3, 1)).unwrap().last_seen, unknown);
        let e = b.get(&addr(1, 1)).unwrap();
        assert_eq!(e.state(), AddrState::NeverTried);
        assert_eq!(e.source, source(1));
        assert_eq!(e.services, 1);
    }

    #[test]
    fn a_known_address_is_news_only_when_it_is_newer() {
        let mut b = book(16, 16);
        assert_eq!(
            b.add_gossiped(&[timed(addr(1, 1), NOW - 100)], source(1), NOW)
                .len(),
            1
        );
        // The same announcement again, and an older one, are not news.
        assert_eq!(
            b.add_gossiped(&[timed(addr(1, 1), NOW - 100)], source(2), NOW)
                .len(),
            0
        );
        assert_eq!(
            b.add_gossiped(&[timed(addr(1, 1), NOW - 900)], source(2), NOW)
                .len(),
            0
        );
        assert_eq!(
            b.add_gossiped(&[timed(addr(1, 1), NOW - 50)], source(2), NOW)
                .len(),
            1
        );
        let e = b.get(&addr(1, 1)).unwrap();
        assert_eq!(e.last_seen, NOW - 50 - GOSSIP_PENALTY_SECS);
        // The first source stays.
        assert_eq!(e.source, source(1));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn addresses_that_cannot_be_dialled_are_refused() {
        let bad: Vec<SocketAddr> = [
            "127.0.0.1:8233",
            "10.1.2.3:8233",
            "172.16.0.1:8233",
            "192.168.1.1:8233",
            "169.254.0.1:8233",
            "100.64.0.1:8233",
            "198.18.0.1:8233",
            "224.0.0.1:8233",
            "255.255.255.255:8233",
            "0.0.0.0:8233",
            "0.1.2.3:8233",
            "240.0.0.1:8233",
            "192.0.2.1:8233",
            "8.8.8.8:0",
            "[::1]:8233",
            "[::]:8233",
            "[fe80::1]:8233",
            "[fc00::1]:8233",
            "[fd12::1]:8233",
            "[ff02::1]:8233",
            "[2001:db8::1]:8233",
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();
        for a in &bad {
            assert!(!is_routable(a), "{a}");
        }
        let good: Vec<SocketAddr> = ["8.8.8.8:8233", "100.128.0.1:1", "[2606:4700::1]:8233"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        for a in &good {
            assert!(is_routable(a), "{a}");
        }
        let mut b = book(64, 64);
        let all: Vec<TimedNetAddr> = bad.iter().chain(&good).map(|a| timed(*a, NOW)).collect();
        assert_eq!(b.add_gossiped(&all, source(1), NOW).len(), good.len());
        assert_eq!(b.add_local(&bad, 1, NOW), 0);
        // Regtest accepts every address that has an IP address and a port.
        let mut local = AddrBook::new(AddrBookConfig {
            capacity: 64,
            ..AddrBookConfig::local()
        });
        assert_eq!(local.add_local(&bad, 1, NOW), bad.len() - 3);
    }

    #[test]
    fn one_source_cannot_flood_the_book() {
        let mut b = book(4096, 256);
        let flood: Vec<TimedNetAddr> = (0..10_000u32)
            .map(|i| timed(addr((i >> 8) as u16, i as u16 & 0xff), NOW))
            .collect();
        for message in flood.chunks(MAX_ADDR_ENTRIES) {
            b.add_gossiped(message, source(1), NOW);
        }
        assert_eq!(b.len(), 256);
        // Another source still has its own allowance.
        assert_eq!(
            b.add_gossiped(&flood[5_000..5_010], source(2), NOW).len(),
            10
        );
        assert_eq!(b.len(), 266);
    }

    #[test]
    fn the_book_is_bounded_and_keeps_responded_addresses() {
        let mut b = book(8, 8);
        let first: Vec<SocketAddr> = (0..8).map(|i| addr(i, 1)).collect();
        assert_eq!(b.add_local(&first, 1, NOW), 8);
        for a in &first[..6] {
            b.mark_success(a, 1, NOW);
        }
        // Two never-tried entries are left to replace. 16 sources then send 8 each.
        for s in 0..16u8 {
            let flood: Vec<TimedNetAddr> = (0..8)
                .map(|i| timed(addr(100 + u16::from(s), i), NOW))
                .collect();
            b.add_gossiped(&flood, source(s), NOW);
            assert_eq!(b.len(), 8);
        }
        for a in &first[..6] {
            assert_eq!(b.get(a).unwrap().state(), AddrState::Responded);
        }
        // A book of responded addresses only drops every new address.
        for e in b.entries().map(|e| e.addr).collect::<Vec<_>>() {
            b.mark_success(&e, 1, NOW);
        }
        assert!(b
            .add_gossiped(&[timed(addr(900, 1), NOW)], source(99), NOW)
            .is_empty());
        assert_eq!(b.add_local(&[addr(901, 1)], 1, NOW), 0);
        assert_eq!(b.len(), 8);
    }

    #[test]
    fn a_full_book_replaces_failed_entries_first_then_the_largest_source() {
        let mut b = book(6, 6);
        b.add_gossiped(
            &[
                timed(addr(1, 1), NOW - 30),
                timed(addr(1, 2), NOW - 20),
                timed(addr(1, 3), NOW - 10),
            ],
            source(1),
            NOW,
        );
        b.add_gossiped(
            &[timed(addr(2, 1), NOW - 500), timed(addr(2, 2), NOW - 400)],
            source(2),
            NOW,
        );
        b.add_gossiped(&[timed(addr(3, 1), NOW - 900)], source(3), NOW);
        // One entry responded and then failed. Two entries failed without a success.
        b.mark_success(&addr(3, 1), 1, NOW);
        b.mark_failed(&addr(3, 1), NOW);
        b.mark_failed(&addr(2, 1), NOW);
        b.mark_failed(&addr(2, 1), NOW);
        b.mark_failed(&addr(2, 2), NOW);
        assert_eq!(b.len(), 6);
        // The failed entry with the most failures goes first, then the other one.
        b.add_gossiped(&[timed(addr(4, 1), NOW)], source(4), NOW);
        assert!(b.get(&addr(2, 1)).is_none());
        b.add_gossiped(&[timed(addr(4, 2), NOW)], source(4), NOW);
        assert!(b.get(&addr(2, 2)).is_none());
        // Then the oldest never-tried entry of the largest source. The entry that
        // responded before stays, with its failure.
        b.add_gossiped(&[timed(addr(4, 3), NOW)], source(4), NOW);
        assert!(b.get(&addr(1, 1)).is_none());
        for i in 0..8 {
            b.add_gossiped(&[timed(addr(5, i), NOW)], source(5), NOW);
            assert_eq!(b.len(), 6);
        }
        assert_eq!(b.get(&addr(3, 1)).unwrap().state(), AddrState::Failed);
        // Entries from a seeder are not replaced by gossip.
        let mut b = book(2, 2);
        b.add_local(&[addr(1, 1), addr(1, 2)], 1, NOW);
        assert!(b
            .add_gossiped(&[timed(addr(4, 1), NOW)], source(4), NOW)
            .is_empty());
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn selection_prefers_responded_then_once_good_then_never_tried_then_failed() {
        let mut b = book(64, 64);
        let all: Vec<SocketAddr> = (0..12).map(|i| addr(i, 1)).collect();
        b.add_local(&all, 1, NOW);
        for a in &all[..3] {
            b.mark_attempt(a, NOW);
            b.mark_success(a, 1, NOW);
        }
        for a in &all[6..9] {
            b.mark_success(a, 1, NOW);
            b.mark_failed(a, NOW);
        }
        for a in &all[9..] {
            b.mark_failed(a, NOW);
        }
        // After every retry delay.
        let later = NOW + RETRY_MAX_SECS;
        let picks = select(&b, later, 12, &[]);
        assert_eq!(picks.len(), 12);
        let class = |a: &SocketAddr| {
            let e = b.get(a).unwrap();
            (e.state(), e.last_success.map(|_| ()))
        };
        let classes: Vec<_> = picks.iter().map(class).collect();
        assert_eq!(classes[..3], [(AddrState::Responded, Some(())); 3]);
        assert_eq!(classes[3..6], [(AddrState::Failed, Some(())); 3]);
        assert_eq!(classes[6..9], [(AddrState::NeverTried, None); 3]);
        assert_eq!(classes[9..], [(AddrState::Failed, None); 3]);
        // The same seed gives the same order; a count below the candidates truncates.
        assert_eq!(select(&b, later, 12, &[]), picks);
        assert_eq!(select(&b, later, 2, &[]), picks[..2]);
    }

    #[test]
    fn selection_spreads_across_groups() {
        let mut b = book(64, 64);
        // Five addresses in group 1, one in each of groups 2 and 3.
        let mut all: Vec<SocketAddr> = (0..5).map(|i| addr(1, i)).collect();
        all.push(addr(2, 1));
        all.push(addr(3, 1));
        b.add_local(&all, 1, NOW);
        let picks = select(&b, NOW, 8, &[]);
        assert_eq!(picks.len(), 3);
        let mut groups: Vec<Group> = picks.iter().map(|a| group(a.ip())).collect();
        groups.sort();
        groups.dedup();
        assert_eq!(groups.len(), 3);
        // A group of a current outbound peer is not used again.
        let picks = select(&b, NOW, 8, &[group(addr(1, 99).ip())]);
        assert_eq!(picks.len(), 2);
        assert!(picks
            .iter()
            .all(|a| group(a.ip()) != group(addr(1, 0).ip())));
        // Two per group.
        let picks = b.select(NOW, 8, &|_| false, &[], 2, &mut rng());
        assert_eq!(picks.len(), 4);
        // IPv6 groups are /32.
        let a: IpAddr = "2606:4700:1::1".parse().unwrap();
        let c: IpAddr = "2606:4700:ffff::1".parse().unwrap();
        let d: IpAddr = "2606:4701::1".parse().unwrap();
        assert_eq!(group(a), group(c));
        assert_ne!(group(a), group(d));
    }

    #[test]
    fn selection_skips_addresses_in_use_banned_or_in_their_retry_delay() {
        let mut b = book(64, 64);
        let all: Vec<SocketAddr> = (0..4).map(|i| addr(i, 1)).collect();
        b.add_local(&all, 1, NOW);
        let picks = b.select(NOW, 8, &|a| *a == all[0], &[], 1, &mut rng());
        assert_eq!(picks.len(), 3);
        assert!(!picks.contains(&all[0]));
        b.ban(all[1].ip(), NOW + 100, NOW);
        b.add_local(&all, 1, NOW);
        assert_eq!(b.len(), 3);
        // The retry delay starts at the attempt and doubles with each failure.
        b.mark_attempt(&all[2], NOW);
        assert!(!select(&b, NOW + RETRY_BASE_SECS - 1, 8, &[]).contains(&all[2]));
        assert!(select(&b, NOW + RETRY_BASE_SECS, 8, &[]).contains(&all[2]));
        b.mark_failed(&all[2], NOW);
        assert!(!select(&b, NOW + 2 * RETRY_BASE_SECS - 1, 8, &[]).contains(&all[2]));
        assert!(select(&b, NOW + 2 * RETRY_BASE_SECS, 8, &[]).contains(&all[2]));
        b.mark_failed(&all[2], NOW);
        assert!(!select(&b, NOW + 4 * RETRY_BASE_SECS - 1, 8, &[]).contains(&all[2]));
        assert!(select(&b, NOW + 4 * RETRY_BASE_SECS, 8, &[]).contains(&all[2]));
    }

    #[test]
    fn failures_remove_an_address() {
        let mut b = book(64, 64);
        b.add_local(&[addr(1, 1), addr(2, 1)], 1, NOW);
        // Never responded: three failures.
        b.mark_failed(&addr(1, 1), NOW);
        b.mark_failed(&addr(1, 1), NOW);
        assert_eq!(b.get(&addr(1, 1)).unwrap().state(), AddrState::Failed);
        b.mark_failed(&addr(1, 1), NOW);
        assert!(b.get(&addr(1, 1)).is_none());
        // Responded once: ten failures in a row. A success resets the count.
        b.mark_success(&addr(2, 1), 5, NOW);
        assert_eq!(b.get(&addr(2, 1)).unwrap().services, 5);
        for _ in 0..9 {
            b.mark_failed(&addr(2, 1), NOW);
        }
        b.mark_success(&addr(2, 1), 5, NOW + 1);
        assert_eq!(b.get(&addr(2, 1)).unwrap().failures, 0);
        for _ in 0..9 {
            b.mark_failed(&addr(2, 1), NOW);
        }
        assert!(b.get(&addr(2, 1)).is_some());
        b.mark_failed(&addr(2, 1), NOW);
        assert!(b.get(&addr(2, 1)).is_none());
        // The retry delay has an upper bound.
        b.add_local(&[addr(3, 1)], 1, NOW);
        b.mark_success(&addr(3, 1), 1, NOW);
        for _ in 0..9 {
            b.mark_failed(&addr(3, 1), NOW);
        }
        assert!(select(&b, NOW + RETRY_MAX_SECS, 1, &[]).contains(&addr(3, 1)));
    }

    #[test]
    fn a_ban_removes_the_addresses_of_the_ip_and_ends() {
        let mut b = book(64, 64);
        let other_port = SocketAddr::new(addr(1, 1).ip(), 18233);
        b.add_local(&[addr(1, 1), other_port, addr(2, 1)], 1, NOW);
        b.ban(addr(1, 1).ip(), NOW + 100, NOW);
        assert_eq!(b.len(), 1);
        assert!(b.is_banned(addr(1, 1).ip(), NOW));
        assert!(b.is_banned(addr(1, 1).ip(), NOW + 99));
        assert!(!b.is_banned(addr(1, 1).ip(), NOW + 100));
        assert!(!b.is_banned(addr(2, 1).ip(), NOW));
        // A banned address is not taken from gossip; after the ban it is.
        let told = [timed(addr(1, 1), NOW)];
        assert!(b.add_gossiped(&told, source(1), NOW).is_empty());
        assert_eq!(b.add_gossiped(&told, source(1), NOW + 100).len(), 1);
        // A shorter ban does not shorten a ban.
        b.ban(addr(1, 1).ip(), NOW + 50, NOW);
        assert!(b.is_banned(addr(1, 1).ip(), NOW + 99));
        // A ban of an IPv6 address holds for its /64.
        let v6: SocketAddr = "[2606:4700:1:2:3::1]:8233".parse().unwrap();
        let same: SocketAddr = "[2606:4700:1:2:ffff::9]:8233".parse().unwrap();
        let next: SocketAddr = "[2606:4700:1:3::1]:8233".parse().unwrap();
        b.add_local(&[v6, same, next], 1, NOW);
        b.ban(v6.ip(), NOW + 100, NOW);
        assert!(b.is_banned(same.ip(), NOW));
        assert!(!b.is_banned(next.ip(), NOW));
        assert!(b.get(&same).is_none());
        assert!(b.get(&next).is_some());
        // The failure count does not wrap.
        b.entries.get_mut(&next).unwrap().failures = u32::MAX - 1;
        b.entries.get_mut(&next).unwrap().last_success = None;
        b.mark_failed(&next, NOW);
        assert!(b.get(&next).is_none());
    }

    #[test]
    fn the_ban_list_is_bounded() {
        let mut b = book(4, 4);
        for i in 0..MAX_BANS as u32 {
            b.ban(
                IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + i)),
                NOW + 10 + u64::from(i),
                NOW,
            );
        }
        assert_eq!(b.bans.len(), MAX_BANS);
        // Full, none expired: the ban that ends first goes.
        let extra = IpAddr::V4(Ipv4Addr::new(12, 0, 0, 1));
        b.ban(extra, NOW + 1_000_000, NOW);
        assert_eq!(b.bans.len(), MAX_BANS);
        assert!(!b.is_banned(IpAddr::V4(Ipv4Addr::from(0x0b00_0000)), NOW));
        assert!(b.is_banned(extra, NOW));
        // Full, some expired: the expired bans go. The bans 1 to 101 end at or before
        // NOW + 111.
        b.ban(
            IpAddr::V4(Ipv4Addr::new(12, 0, 0, 2)),
            NOW + 1_000_000,
            NOW + 111,
        );
        assert_eq!(b.bans.len(), MAX_BANS - 101 + 1);
    }

    #[test]
    fn the_getaddr_sample_holds_recent_good_addresses_only() {
        let mut b = book(4096, 4096);
        let recent: Vec<SocketAddr> = (0..40).map(|i| addr(i, 1)).collect();
        b.add_local(&recent, 1, NOW);
        for a in &recent {
            b.mark_success(a, 1, NOW - 100);
        }
        // Seen too long ago, failed, banned, and never confirmed (from the configuration
        // and from a peer with a fresh timestamp): not in the answer.
        b.add_gossiped(&[timed(addr(104, 1), NOW)], source(1), NOW);
        b.add_local(
            &[addr(100, 1), addr(101, 1), addr(102, 1), addr(103, 1)],
            1,
            NOW,
        );
        b.mark_success(&addr(100, 1), 1, NOW - GOSSIP_MAX_AGE_SECS - 1);
        b.mark_success(&addr(101, 1), 1, NOW);
        b.mark_failed(&addr(101, 1), NOW);
        b.mark_success(&addr(102, 1), 1, NOW);
        b.bans.insert(addr(102, 1).ip(), NOW + 100);
        let sample = b.sample(NOW, &mut rng());
        // 23 % of 45, rounded up.
        assert_eq!(sample.len(), 11);
        let truncated =
            ((NOW - 100) / TIMESTAMP_TRUNCATION_SECS * TIMESTAMP_TRUNCATION_SECS) as u32;
        for a in &sample {
            assert!(recent.contains(&a.net.addr), "{:?}", a.net.addr);
            assert_eq!(a.time, truncated);
            assert_eq!(a.time as u64 % TIMESTAMP_TRUNCATION_SECS, 0);
        }
        // The sample is random: another seed gives another set.
        let other = b.sample(NOW, &mut StdRng::seed_from_u64(8));
        assert_ne!(sample, other);
        // Never more than one message.
        let mut big = book(8192, 8192);
        let many: Vec<SocketAddr> = (0..6_000u32)
            .map(|i| addr((i >> 8) as u16, i as u16 & 0xff))
            .collect();
        big.add_local(&many, 1, NOW);
        for a in &many {
            big.mark_success(a, 1, NOW);
        }
        assert_eq!(big.sample(NOW, &mut rng()).len(), MAX_ADDR_ENTRIES);
        assert!(book(8, 8).sample(NOW, &mut rng()).is_empty());
    }

    #[test]
    fn the_budget_of_a_connection_bounds_its_addresses() {
        let mut budget = AddrBudget::new(NOW);
        // One address at the start.
        assert_eq!(budget.take(1_000, NOW), 1);
        assert_eq!(budget.take(1_000, NOW), 0);
        // One more each ten seconds.
        assert_eq!(budget.take(1_000, NOW + 9), 0);
        assert_eq!(budget.take(1_000, NOW + 10), 1);
        assert_eq!(budget.take(1_000, NOW + 45), 3);
        assert_eq!(budget.take(1_000, NOW + 50), 1);
        // A `getaddr` of this node permits one full answer.
        budget.grant_getaddr();
        assert_eq!(budget.take(1_000, NOW + 50), 1_000);
        assert_eq!(budget.take(1_000, NOW + 50), 0);
        // The budget never accrues above one message.
        assert_eq!(budget.take(5_000, NOW + 10_000_000), 1_000);
    }

    #[test]
    fn only_small_fresh_messages_are_sent_on() {
        let fresh = timed("8.8.8.8:8233".parse().unwrap(), NOW - 599);
        let old = timed("8.8.4.4:8233".parse().unwrap(), NOW - 600);
        let future = timed("1.1.1.1:8233".parse().unwrap(), NOW + 601);
        let local = timed("127.0.0.1:8233".parse().unwrap(), NOW);
        assert_eq!(relayable(&[fresh, old, future, local], 4, NOW), vec![fresh]);
        assert_eq!(relayable(&[fresh; 10], 10, NOW).len(), 10);
        assert!(relayable(&[fresh; 11], 11, NOW).is_empty());
        // A long message that the budget cut to a few addresses is not sent on.
        assert!(relayable(&[fresh], 1000, NOW).is_empty());
    }
}
