//! What a restart needs besides the coins store and the block files.
//!
//! The coins store records its best block: the block whose state it holds after the last
//! generation (`BestBlock`). The base of the chain at that block also holds the note
//! commitment frontiers, the anchor sets, the value pools, the history tree and the block
//! times. The node writes them to `state.log` before each coins flush. A restart finds the
//! record of the best block, rebuilds the base from it and replays the later blocks from
//! the block files (`docs/architecture.md`, recovery rule).
//!
//! `state.log` is an append-only log of checksummed records. A record holds the state of
//! the base and the anchors that were new since the record before. The log is not a
//! snapshot, because the anchor sets grow with the chain and a snapshot per flush would
//! write all of them each time. The first record is the start record: the base at the first
//! start of the node (genesis, or the shadow seed). A record is written before the flush
//! that it belongs to, so the log always holds the record of the best block. A crash
//! between the two writes leaves one record more than the coins store. The restart drops
//! the records after the best block.
//!
//! [`RecordLog`] is the file format of the log. The shadow node also keeps its set of spent
//! outpoints in a [`RecordLog`] (`backing.rs`).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hayai_coins::{BestBlock, Pool};
use hayai_crypto::zcash_primitives::merkle_tree::{read_frontier_v1, write_frontier_v1};
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_state::{BaseState, HistoryState, ValuePools};
use hayai_trees::{IronwoodFrontier, OrchardFrontier, SaplingFrontier, SproutFrontier};
use hayai_wire::header::BlockHash;

use crate::config::Mode;
use crate::params::NetworkKind;

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Corrupt(String),
    /// The record to resume from is of a version that does not hold every value pool.
    #[error(
        "{path}: the state record at height {height} has version {version}. A record before \
         version {RECORD_VERSION_3} does not hold the transparent and the deferred value pool, \
         and the node does not start from a state without them. Remove the data directory \
         {dir} and start the node again: a full node validates from the genesis block, and a \
         shadow node reads a new start state from upstream"
    )]
    OutdatedRecord {
        path: PathBuf,
        dir: PathBuf,
        height: u32,
        version: u8,
    },
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> PersistError + '_ {
    move |source| PersistError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn corrupt<T>(message: impl Into<String>) -> Result<T, PersistError> {
    Err(PersistError::Corrupt(message.into()))
}

// ----- record log -----

const RECORD_MAGIC: u32 = 0x4859_5354;
/// Magic, payload length and payload CRC32C, each 4 bytes little-endian.
const RECORD_HEADER: usize = 12;

/// An append-only file of checksummed records. A record that the last write left torn (a
/// short header, a payload past the end of the file, or a last record that fails its
/// checksum) is cut at open. Any other damage is an error.
pub struct RecordLog {
    file: File,
    path: PathBuf,
    /// End offset of each record.
    ends: Vec<u64>,
}

/// The payloads of the records of `bytes`, oldest first, the end offset of each record,
/// and whether a torn record follows the last one.
#[allow(clippy::type_complexity)]
fn scan(bytes: &[u8], path: &Path) -> Result<(Vec<Vec<u8>>, Vec<u64>, bool), PersistError> {
    let mut records = Vec::new();
    let mut ends = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let Some(header) = bytes.get(pos..pos + RECORD_HEADER) else {
            return Ok((records, ends, true));
        };
        let word = |i: usize| u32::from_le_bytes(header[i..i + 4].try_into().expect("4"));
        if word(0) != RECORD_MAGIC {
            return corrupt(format!("{}: bad record magic at {pos}", path.display()));
        }
        let len = word(4) as usize;
        let end = pos + RECORD_HEADER + len;
        let Some(payload) = bytes.get(pos + RECORD_HEADER..end) else {
            return Ok((records, ends, true));
        };
        if crc32c::crc32c(payload) != word(8) {
            if end == bytes.len() {
                return Ok((records, ends, true));
            }
            return corrupt(format!(
                "{}: record at {pos} fails its checksum",
                path.display()
            ));
        }
        records.push(payload.to_vec());
        ends.push(end as u64);
        pos = end;
    }
    Ok((records, ends, false))
}

impl RecordLog {
    /// Opens `path`, creating it when it does not exist. Returns the log and the payloads
    /// of its records, oldest first.
    pub fn open(path: &Path) -> Result<(Self, Vec<Vec<u8>>), PersistError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io(path))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io(path))?;
        let (records, ends, torn) = scan(&bytes, path)?;
        let pos = ends.last().copied().unwrap_or(0);
        if torn {
            tracing::warn!(path = %path.display(), offset = pos, "torn last record cut");
            file.set_len(pos).map_err(io(path))?;
            file.sync_all().map_err(io(path))?;
        }
        Ok((
            Self {
                file,
                path: path.to_path_buf(),
                ends,
            },
            records,
        ))
    }

    /// Appends one record and syncs it. It fails with [`PersistError::Io`] only.
    pub fn append(&mut self, payload: &[u8]) -> Result<(), PersistError> {
        let len = u32::try_from(payload.len()).map_err(|_| {
            PersistError::Corrupt(format!("{}: record too large", self.path.display()))
        })?;
        let mut record = Vec::with_capacity(RECORD_HEADER + payload.len());
        record.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(&crc32c::crc32c(payload).to_le_bytes());
        record.extend_from_slice(payload);
        let start = self.ends.last().copied().unwrap_or(0);
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(io(&self.path))?;
        self.file.write_all(&record).map_err(io(&self.path))?;
        self.file.sync_data().map_err(io(&self.path))?;
        self.ends.push(start + record.len() as u64);
        Ok(())
    }

    /// Keeps the first `count` records and drops the rest.
    pub fn truncate_to(&mut self, count: usize) -> Result<(), PersistError> {
        if count >= self.ends.len() {
            return Ok(());
        }
        let end = match count.checked_sub(1) {
            Some(last) => self.ends[last],
            None => 0,
        };
        self.file.set_len(end).map_err(io(&self.path))?;
        self.file.sync_all().map_err(io(&self.path))?;
        self.ends.truncate(count);
        Ok(())
    }
}

// ----- encoding -----

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(u32::try_from(v.len()).expect("a record field fits in 4 GiB"));
        self.0.extend_from_slice(v);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PersistError> {
        let Some((head, tail)) = self.buf.split_at_checked(n) else {
            return corrupt("state record is truncated");
        };
        self.buf = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, PersistError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, PersistError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn u64(&mut self) -> Result<u64, PersistError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }
    fn array32(&mut self) -> Result<[u8; 32], PersistError> {
        Ok(self.take(32)?.try_into().expect("32"))
    }
    fn bytes(&mut self) -> Result<&'a [u8], PersistError> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn finish(&self) -> Result<(), PersistError> {
        match self.buf.is_empty() {
            true => Ok(()),
            false => corrupt("state record has trailing bytes"),
        }
    }
}

/// The version that [`StateRecord::encode`] writes. It adds the Sprout state to version 3:
/// the Sprout frontier of the base and the Sprout treestates that were new since the
/// record before. A record of an earlier version above the genesis block loads as a base
/// that does not know the Sprout state.
const RECORD_VERSION: u8 = 4;
/// Version 3 has the layout of version 2. It tells that the build that wrote the record
/// maintains every value pool: a record of an earlier version has a transparent and a
/// deferred pool of zero at every height.
const RECORD_VERSION_3: u8 = 3;
/// Version 2 adds the `bits` of the newest blocks, the transparent, Sprout, Ironwood and
/// deferred value pools, the Ironwood frontier and the Ironwood anchors to version 1.
const RECORD_VERSION_2: u8 = 2;
/// The first version. [`StateRecord::decode`] still reads it: the fields that version 2
/// adds are then empty.
const RECORD_VERSION_1: u8 = 1;

fn network_tag(kind: NetworkKind) -> u8 {
    match kind {
        NetworkKind::Regtest | NetworkKind::ConfiguredRegtest(_) => 0,
        NetworkKind::Testnet => 1,
        NetworkKind::Mainnet => 2,
    }
}

fn mode_tag(mode: Mode) -> u8 {
    match mode {
        Mode::Full => 0,
        Mode::Shadow => 1,
    }
}

fn pool_tag(pool: Pool) -> u8 {
    match pool {
        Pool::Sapling => 1,
        Pool::Orchard => 2,
        Pool::Ironwood => 3,
        Pool::Sprout => unreachable!("the base keeps no Sprout anchors"),
    }
}

fn pool_from_tag(tag: u8) -> Result<Pool, PersistError> {
    match tag {
        1 => Ok(Pool::Sapling),
        2 => Ok(Pool::Orchard),
        3 => Ok(Pool::Ironwood),
        _ => corrupt(format!("anchor pool tag {tag}")),
    }
}

/// One record of `state.log`.
pub struct StateRecord {
    pub network: NetworkKind,
    pub mode: Mode,
    pub base: BaseState,
    /// `(hash, time)` of the base block and up to 27 blocks before it, oldest first: with
    /// the `bits` of the base, the seed of the header index.
    pub ancestors: Vec<(BlockHash, u32)>,
    /// Anchors that were new since the record before.
    pub new_anchors: Vec<(Pool, [u8; 32])>,
    /// Sprout treestates that were new since the record before.
    pub new_sprout_trees: Vec<Arc<SproutFrontier>>,
}

impl StateRecord {
    fn encode(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u8(RECORD_VERSION);
        w.u8(network_tag(self.network));
        w.u8(mode_tag(self.mode));
        w.u32(self.base.height);
        w.0.extend_from_slice(&self.base.hash.0);
        w.u32(self.base.times.len() as u32);
        for t in &self.base.times {
            w.u32(*t);
        }
        w.u32(self.base.bits.len() as u32);
        for b in &self.base.bits {
            w.u32(*b);
        }
        w.u32(self.ancestors.len() as u32);
        for (hash, time) in &self.ancestors {
            w.0.extend_from_slice(&hash.0);
            w.u32(*time);
        }
        let pools = &self.base.value_pools;
        for pool in [
            pools.transparent,
            pools.sprout,
            pools.sapling,
            pools.orchard,
            pools.ironwood,
            pools.deferred,
        ] {
            w.u64(pool);
        }
        let mut frontier = Vec::new();
        write_frontier_v1(&mut frontier, self.base.sapling_frontier.frontier())
            .expect("a vector write does not fail");
        w.bytes(&frontier);
        frontier.clear();
        write_frontier_v1(&mut frontier, self.base.orchard_frontier.frontier())
            .expect("a vector write does not fail");
        w.bytes(&frontier);
        // The tag is 1: the frontier follows. A record of a build that kept no Ironwood
        // tree has the tag 0 and no frontier.
        w.u8(1);
        frontier.clear();
        write_frontier_v1(&mut frontier, self.base.ironwood_frontier.frontier())
            .expect("a vector write does not fail");
        w.bytes(&frontier);
        match &self.base.history {
            None => w.u8(0),
            Some(history) => {
                w.u8(1);
                w.u32(u32::from(history.upgrade()));
                w.u32(history.length());
                w.u32(history.peaks().len() as u32);
                for (position, node) in history.peaks() {
                    w.u32(*position);
                    w.bytes(node);
                }
            }
        }
        w.u32(self.new_anchors.len() as u32);
        for (pool, root) in &self.new_anchors {
            w.u8(pool_tag(*pool));
            w.0.extend_from_slice(root);
        }
        // The tag is 0 when the base does not know the Sprout state.
        match &self.base.sprout_frontier {
            None => w.u8(0),
            Some(sprout) => {
                w.u8(1);
                for tree in std::iter::once(sprout).chain(&self.new_sprout_trees) {
                    frontier.clear();
                    tree.write(&mut frontier)
                        .expect("a vector write does not fail");
                    w.bytes(&frontier);
                }
            }
        }
        w.0
    }

    fn decode(bytes: &[u8]) -> Result<Self, PersistError> {
        let mut r = Reader { buf: bytes };
        let version = r.u8()?;
        let (RECORD_VERSION_1 | RECORD_VERSION_2 | RECORD_VERSION_3 | RECORD_VERSION) = version
        else {
            return corrupt(format!("state record version {version}"));
        };
        // Version 3 has the layout of version 2. Version 4 adds the Sprout state at the end.
        let v2 = version >= RECORD_VERSION_2;
        let network = match r.u8()? {
            0 => NetworkKind::Regtest,
            1 => NetworkKind::Testnet,
            2 => NetworkKind::Mainnet,
            tag => return corrupt(format!("network tag {tag}")),
        };
        let mode = match r.u8()? {
            0 => Mode::Full,
            1 => Mode::Shadow,
            tag => return corrupt(format!("mode tag {tag}")),
        };
        let height = r.u32()?;
        let hash = BlockHash(r.array32()?);
        let times = (0..r.u32()?)
            .map(|_| r.u32())
            .collect::<Result<Vec<_>, _>>()?;
        let bits = match v2 {
            true => (0..r.u32()?)
                .map(|_| r.u32())
                .collect::<Result<Vec<_>, _>>()?,
            false => Vec::new(),
        };
        let ancestors = (0..r.u32()?)
            .map(|_| Ok((BlockHash(r.array32()?), r.u32()?)))
            .collect::<Result<Vec<_>, PersistError>>()?;
        let value_pools = match v2 {
            true => ValuePools {
                transparent: r.u64()?,
                sprout: r.u64()?,
                sapling: r.u64()?,
                orchard: r.u64()?,
                ironwood: r.u64()?,
                deferred: r.u64()?,
            },
            false => ValuePools {
                sapling: r.u64()?,
                orchard: r.u64()?,
                ..ValuePools::default()
            },
        };
        let sapling_frontier = SaplingFrontier::from_frontier(
            read_frontier_v1(r.bytes()?)
                .map_err(|e| PersistError::Corrupt(format!("Sapling frontier: {e}")))?,
        );
        let orchard_frontier = OrchardFrontier::from_frontier(
            read_frontier_v1(r.bytes()?)
                .map_err(|e| PersistError::Corrupt(format!("Orchard frontier: {e}")))?,
        );
        // A version 1 record, and a version 2 record with the tag 0, come from a build that
        // accepted no block from NU6.3: the Ironwood tree of their base is empty.
        let ironwood_frontier = match (v2, v2.then(|| r.u8()).transpose()?) {
            (false, _) | (true, Some(0)) => IronwoodFrontier::empty(),
            (true, Some(1)) => IronwoodFrontier::from_frontier(
                read_frontier_v1(r.bytes()?)
                    .map_err(|e| PersistError::Corrupt(format!("Ironwood frontier: {e}")))?,
            ),
            (true, tag) => return corrupt(format!("Ironwood frontier tag {tag:?}")),
        };
        let history = match r.u8()? {
            0 => None,
            1 => {
                let branch = r.u32()?;
                let Ok(upgrade) = BranchId::try_from(branch) else {
                    return corrupt(format!("history branch id {branch:#x}"));
                };
                let length = r.u32()?;
                let peaks = (0..r.u32()?)
                    .map(|_| Ok((r.u32()?, r.bytes()?.to_vec())))
                    .collect::<Result<Vec<_>, PersistError>>()?;
                Some(Arc::new(
                    HistoryState::from_peaks(upgrade, length, peaks)
                        .map_err(|e| PersistError::Corrupt(format!("history tree: {e}")))?,
                ))
            }
            tag => return corrupt(format!("history tag {tag}")),
        };
        let new_anchors = (0..r.u32()?)
            .map(|_| Ok((pool_from_tag(r.u8()?)?, r.array32()?)))
            .collect::<Result<Vec<_>, PersistError>>()?;
        let sprout_tree = |r: &mut Reader<'_>| -> Result<Arc<SproutFrontier>, PersistError> {
            SproutFrontier::read(r.bytes()?)
                .map(Arc::new)
                .map_err(|e| PersistError::Corrupt(format!("Sprout frontier: {e}")))
        };
        let (sprout_frontier, new_sprout_trees) = match (version, height) {
            (RECORD_VERSION, _) => match r.u8()? {
                0 => (None, Vec::new()),
                1 => {
                    let frontier = sprout_tree(&mut r)?;
                    let mut trees = Vec::new();
                    while !r.buf.is_empty() {
                        trees.push(sprout_tree(&mut r)?);
                    }
                    (Some(frontier), trees)
                }
                tag => return corrupt(format!("Sprout state tag {tag}")),
            },
            // The Sprout tree of the genesis block is empty. Above it, a build before
            // version 4 kept no Sprout state.
            (_, 0) => (Some(Arc::new(SproutFrontier::empty())), Vec::new()),
            (_, _) => (None, Vec::new()),
        };
        r.finish()?;
        Ok(Self {
            network,
            mode,
            base: BaseState {
                height,
                hash,
                times,
                bits,
                value_pools,
                history,
                sapling_frontier: Arc::new(sapling_frontier),
                orchard_frontier: Arc::new(orchard_frontier),
                ironwood_frontier: Arc::new(ironwood_frontier),
                sprout_frontier,
            },
            ancestors,
            new_anchors,
            new_sprout_trees,
        })
    }
}

// ----- state log -----

/// The state to resume from.
pub struct Recovered {
    pub base: BaseState,
    pub ancestors: Vec<(BlockHash, u32)>,
    /// Every anchor of the base, in insertion order.
    pub anchors: Vec<(Pool, [u8; 32])>,
    /// Every Sprout treestate of the base, in insertion order.
    pub sprout_trees: Vec<Arc<SproutFrontier>>,
    /// Height of the start record: the shadow seed height, or 0.
    pub start_height: u32,
}

/// The block that a restart resumes from, and the first start of the node.
pub struct ResumePoint {
    /// Network of the start record.
    pub network: NetworkKind,
    /// Height of the start record: the shadow seed height, or 0.
    pub start_height: u32,
    pub height: u32,
    pub hash: BlockHash,
}

/// Decodes the records of `path` and selects the state of `best`: the newest record at
/// that block, or the start record when no generation reached the coins store. Returns
/// the records, which hold a start record, and the index of the selected one.
///
/// A selected record of a version before [`RECORD_VERSION_3`] above the genesis block is
/// [`PersistError::OutdatedRecord`]: its transparent and deferred value pools are zero,
/// which is the right value only at the genesis block. The node never takes a pool of
/// zero in place of a value that it does not know.
fn select(
    path: &Path,
    dir: &Path,
    payloads: &[Vec<u8>],
    best: Option<BestBlock>,
) -> Result<(Vec<StateRecord>, usize), PersistError> {
    let records = payloads
        .iter()
        .map(|p| StateRecord::decode(p))
        .collect::<Result<Vec<_>, _>>()?;
    if records.is_empty() {
        return corrupt(format!("{} holds no start record", path.display()));
    }
    let selected = match best {
        None => 0,
        Some(best) => {
            let found = records
                .iter()
                .rposition(|r| (r.base.height, r.base.hash.0) == (best.height, best.hash));
            let Some(found) = found else {
                return corrupt(format!(
                    "{} has no record of the coins store's best block {} at height {}",
                    path.display(),
                    BlockHash(best.hash),
                    best.height
                ));
            };
            found
        }
    };
    // `decode` accepted the payload, so it has the version byte.
    let version = payloads[selected][0];
    let height = records[selected].base.height;
    if version < RECORD_VERSION_3 && height > 0 {
        return Err(PersistError::OutdatedRecord {
            path: path.to_path_buf(),
            dir: dir.to_path_buf(),
            height,
            version,
        });
    }
    Ok((records, selected))
}

/// `state.log`: the start record, then one record per flush.
pub struct StateLog {
    log: RecordLog,
    /// Height of the newest record.
    last_height: u32,
}

impl StateLog {
    pub const FILE: &'static str = "state.log";

    pub fn exists(dir: &Path) -> bool {
        dir.join(Self::FILE).exists()
    }

    /// The state that a restart on `dir` resumes from, for the best block `best` of the
    /// coins store: the selection of [`StateLog::open`]. The function does not change the
    /// file, so it is safe while a node runs on `dir`.
    pub fn resume_point(dir: &Path, best: Option<BestBlock>) -> Result<ResumePoint, PersistError> {
        let path = dir.join(Self::FILE);
        let bytes = std::fs::read(&path).map_err(io(&path))?;
        let (payloads, _, _) = scan(&bytes, &path)?;
        let (records, selected) = select(&path, dir, &payloads, best)?;
        Ok(ResumePoint {
            network: records[0].network,
            start_height: records[0].base.height,
            height: records[selected].base.height,
            hash: records[selected].base.hash,
        })
    }

    /// Creates the log with the start record. The file must not exist.
    pub fn create(dir: &Path, start: &StateRecord) -> Result<Self, PersistError> {
        let path = dir.join(Self::FILE);
        let (mut log, existing) = RecordLog::open(&path)?;
        if !existing.is_empty() {
            return corrupt(format!("{} exists already", path.display()));
        }
        log.append(&start.encode())?;
        Ok(Self {
            log,
            last_height: start.base.height,
        })
    }

    /// Opens the log of a data directory and selects the state of `best`: the newest record
    /// at that block, or the start record when no generation reached the coins store.
    /// Records after the selected one are dropped. The network and the mode must be the
    /// ones of the first start. [`select`] has the selection and its errors.
    pub fn open(
        dir: &Path,
        network: NetworkKind,
        mode: Mode,
        best: Option<BestBlock>,
    ) -> Result<(Self, Recovered), PersistError> {
        let path = dir.join(Self::FILE);
        let (mut log, payloads) = RecordLog::open(&path)?;
        let (records, selected) = select(&path, dir, &payloads, best)?;
        let start = &records[0];
        // A record does not hold the values of a configured Regtest: both are Regtest here.
        if (network_tag(start.network), start.mode) != (network_tag(network), mode) {
            return corrupt(format!(
                "{} belongs to a {}/{} node; the configuration is {}/{}",
                dir.display(),
                start.network.name(),
                mode_name(start.mode),
                network.name(),
                mode_name(mode),
            ));
        }
        log.truncate_to(selected + 1)?;
        let start_height = records[0].base.height;
        let anchors = records[..=selected]
            .iter()
            .flat_map(|r| r.new_anchors.iter().copied())
            .collect();
        let sprout_trees = records[..=selected]
            .iter()
            .flat_map(|r| r.new_sprout_trees.iter().cloned())
            .collect();
        let StateRecord {
            base, ancestors, ..
        } = records.into_iter().nth(selected).expect("selected exists");
        let last_height = base.height;
        Ok((
            Self { log, last_height },
            Recovered {
                base,
                ancestors,
                anchors,
                sprout_trees,
                start_height,
            },
        ))
    }

    /// Appends the record of a base that the next flush writes to the coins store. A base
    /// at the height of the newest record needs no new record.
    pub fn write(&mut self, record: &StateRecord) -> Result<(), PersistError> {
        if record.base.height == self.last_height && record.new_anchors.is_empty() {
            return Ok(());
        }
        self.log.append(&record.encode())?;
        self.last_height = record.base.height;
        Ok(())
    }
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Full => "full",
        Mode::Shadow => "shadow",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(height: u32, anchor: u8) -> StateRecord {
        let sapling = SaplingFrontier::empty();
        let orchard = OrchardFrontier::empty();
        StateRecord {
            network: NetworkKind::Regtest,
            mode: Mode::Full,
            base: BaseState {
                height,
                hash: BlockHash([height as u8; 32]),
                times: vec![10, 20, height],
                bits: vec![0x1f07_ffff, 0x1f07_fffe],
                value_pools: ValuePools {
                    transparent: 1,
                    sprout: 2,
                    sapling: 7,
                    orchard: u64::from(height),
                    ironwood: 3,
                    deferred: 4,
                },
                history: Some(Arc::new(HistoryState::empty(BranchId::Nu5))),
                sapling_frontier: Arc::new(sapling),
                orchard_frontier: Arc::new(orchard),
                ironwood_frontier: Arc::new(IronwoodFrontier::empty()),
                sprout_frontier: Some(Arc::new(SproutFrontier::empty())),
            },
            ancestors: vec![(BlockHash([1; 32]), 5), (BlockHash([height as u8; 32]), 6)],
            new_anchors: vec![(Pool::Sapling, [anchor; 32]), (Pool::Orchard, [anchor; 32])],
            new_sprout_trees: Vec::new(),
        }
    }

    fn dir() -> tempfile::TempDir {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/hayaid-persist");
        std::fs::create_dir_all(&base).expect("scratch base");
        tempfile::tempdir_in(base).expect("scratch dir")
    }

    /// `StateLog::resume_point` selects the record that `StateLog::open` selects, and it
    /// leaves a torn record and the records after the selected one in the file.
    #[test]
    fn the_resume_point_is_read_without_a_change_of_the_file() {
        let dir = dir();
        let Err(PersistError::Io { .. }) = StateLog::resume_point(dir.path(), None) else {
            panic!("a directory without a state log");
        };
        let mut log = StateLog::create(dir.path(), &record(0, 1)).expect("create");
        log.write(&record(8, 2)).expect("write");
        log.write(&record(9, 3)).expect("write");
        drop(log);
        let path = dir.path().join(StateLog::FILE);
        let mut bytes = std::fs::read(&path).expect("read");
        bytes.extend_from_slice(&RECORD_MAGIC.to_le_bytes());
        std::fs::write(&path, &bytes).expect("torn record");
        let best = |height: u32| {
            Some(BestBlock {
                height,
                hash: [height as u8; 32],
            })
        };
        for (best, height) in [(None, 0), (best(8), 8), (best(9), 9)] {
            let point = StateLog::resume_point(dir.path(), best).expect("resume point");
            assert_eq!(
                (point.network, point.start_height, point.height, point.hash),
                (
                    NetworkKind::Regtest,
                    0,
                    height,
                    BlockHash([height as u8; 32])
                )
            );
        }
        let Err(PersistError::Corrupt(_)) = StateLog::resume_point(dir.path(), best(7)) else {
            panic!("a best block without a record");
        };
        assert_eq!(std::fs::read(&path).expect("read"), bytes);
        let (_, recovered) =
            StateLog::open(dir.path(), NetworkKind::Regtest, Mode::Full, best(8)).expect("open");
        assert_eq!(recovered.base.height, 8);
    }

    #[test]
    fn a_record_round_trips() {
        let original = record(9, 3);
        let decoded = StateRecord::decode(&original.encode()).expect("decodes");
        assert_eq!(decoded.base.height, 9);
        assert_eq!(decoded.base.hash, original.base.hash);
        assert_eq!(decoded.base.times, original.base.times);
        assert_eq!(decoded.base.bits, original.base.bits);
        assert_eq!(
            *decoded.base.ironwood_frontier,
            IronwoodFrontier::empty(),
            "the empty Ironwood frontier"
        );
        assert_eq!(decoded.base.value_pools, original.base.value_pools);
        assert_eq!(decoded.base.history, original.base.history);
        assert_eq!(decoded.ancestors, original.ancestors);
        assert_eq!(decoded.new_anchors, original.new_anchors);
        assert_eq!(
            decoded.base.sapling_frontier.root().to_bytes(),
            original.base.sapling_frontier.root().to_bytes()
        );
        let mut truncated = original.encode();
        truncated.pop();
        let Err(_) = StateRecord::decode(&truncated) else {
            panic!("a truncated record is an error");
        };
    }

    #[test]
    fn a_non_empty_frontier_and_history_survive_the_encoding() {
        let mut record = record(4, 1);
        let mut orchard = OrchardFrontier::empty();
        let leaves = [[7u8; 32], [9u8; 32], [11u8; 32]];
        let nodes: Vec<_> = leaves
            .iter()
            .map(|l| {
                hayai_crypto::orchard::tree::MerkleHashOrchard::from_bytes(l)
                    .expect("small values are field elements")
            })
            .collect();
        orchard.append_many(&nodes).expect("the frontier has room");
        record.base.orchard_frontier = Arc::new(orchard.clone());
        let decoded = StateRecord::decode(&record.encode()).expect("decodes");
        assert_eq!(*decoded.base.orchard_frontier, orchard);
        assert_ne!(
            decoded.base.orchard_frontier.root(),
            OrchardFrontier::empty().root()
        );
        // The Ironwood frontier and an Ironwood anchor survive too.
        record.base.ironwood_frontier = Arc::new(orchard.clone());
        record.new_anchors.push((Pool::Ironwood, [5; 32]));
        let decoded = StateRecord::decode(&record.encode()).expect("decodes");
        assert_eq!(
            *decoded.base.ironwood_frontier, orchard,
            "Ironwood frontier"
        );
        assert_eq!(decoded.new_anchors.last(), Some(&(Pool::Ironwood, [5; 32])));
    }

    /// The layout of a version 1 record: the encoder that hayaid had before version 2.
    fn encode_v1(record: &StateRecord) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u8(RECORD_VERSION_1);
        w.u8(network_tag(record.network));
        w.u8(mode_tag(record.mode));
        w.u32(record.base.height);
        w.0.extend_from_slice(&record.base.hash.0);
        w.u32(record.base.times.len() as u32);
        for t in &record.base.times {
            w.u32(*t);
        }
        w.u32(record.ancestors.len() as u32);
        for (hash, time) in &record.ancestors {
            w.0.extend_from_slice(&hash.0);
            w.u32(*time);
        }
        w.u64(record.base.value_pools.sapling);
        w.u64(record.base.value_pools.orchard);
        let mut frontier = Vec::new();
        write_frontier_v1(&mut frontier, record.base.sapling_frontier.frontier())
            .expect("a vector write does not fail");
        w.bytes(&frontier);
        frontier.clear();
        write_frontier_v1(&mut frontier, record.base.orchard_frontier.frontier())
            .expect("a vector write does not fail");
        w.bytes(&frontier);
        let Some(history) = &record.base.history else {
            panic!("the test records have a history tree");
        };
        w.u8(1);
        w.u32(u32::from(history.upgrade()));
        w.u32(history.length());
        w.u32(history.peaks().len() as u32);
        for (position, node) in history.peaks() {
            w.u32(*position);
            w.bytes(node);
        }
        w.u32(record.new_anchors.len() as u32);
        for (pool, root) in &record.new_anchors {
            w.u8(pool_tag(*pool));
            w.0.extend_from_slice(root);
        }
        w.0
    }

    #[test]
    fn a_version_1_record_still_loads() {
        let original = record(9, 3);
        let old = encode_v1(&original);
        assert_eq!(old[0], 1);
        assert_eq!(original.encode()[0], 4);
        let decoded = StateRecord::decode(&old).expect("a version 1 record decodes");
        assert_eq!(decoded.network, original.network);
        assert_eq!(decoded.base.height, 9);
        assert_eq!(decoded.base.hash, original.base.hash);
        assert_eq!(decoded.base.times, original.base.times);
        assert_eq!(decoded.base.history, original.base.history);
        assert_eq!(decoded.ancestors, original.ancestors);
        assert_eq!(decoded.new_anchors, original.new_anchors);
        assert_eq!(
            decoded.base.sapling_frontier.root().to_bytes(),
            original.base.sapling_frontier.root().to_bytes()
        );
        // The fields that version 2 adds are empty.
        assert_eq!(decoded.base.bits, Vec::<u32>::new());
        assert_eq!(
            decoded.base.value_pools,
            ValuePools {
                sapling: 7,
                orchard: 9,
                ..ValuePools::default()
            }
        );
        assert_eq!(
            *decoded.base.ironwood_frontier,
            IronwoodFrontier::empty(),
            "a version 1 record has the empty Ironwood frontier"
        );
        // A version 1 record is not a prefix of a later record, and an unknown version is
        // an error.
        let mut truncated = old.clone();
        truncated.pop();
        let Err(_) = StateRecord::decode(&truncated) else {
            panic!("a truncated record is an error");
        };
        let mut future = old;
        future[0] = 5;
        let Err(PersistError::Corrupt(message)) = StateRecord::decode(&future).map(|_| ()) else {
            panic!("an unknown version is an error");
        };
        assert!(message.contains("version 5"), "{message}");
    }

    /// The layout of a version 3 record: the layout of the current version without the
    /// Sprout state, which is the last field. `record` has the empty Sprout frontier and
    /// no new Sprout treestate.
    fn encode_v3(record: &StateRecord) -> Vec<u8> {
        // The tag 1, the length 1 and the one byte of an empty frontier.
        let sprout = [1u8, 1, 0, 0, 0, 0];
        let mut bytes = record.encode();
        assert!(
            bytes.ends_with(&sprout),
            "the Sprout state of a test record"
        );
        bytes.truncate(bytes.len() - sprout.len());
        bytes[0] = RECORD_VERSION_3;
        bytes
    }

    /// The layout of a version 2 record: the layout of version 3 with the version byte 2.
    fn encode_v2(record: &StateRecord) -> Vec<u8> {
        let mut bytes = encode_v3(record);
        bytes[0] = RECORD_VERSION_2;
        bytes
    }

    /// The Sprout frontier of the base and the new Sprout treestates survive the
    /// encoding. A base that does not know the Sprout state has the tag 0. A record of
    /// version 3 holds no Sprout state: the genesis block has the empty tree, and a base
    /// above it does not know the Sprout state.
    #[test]
    fn the_sprout_state_survives_the_encoding() {
        let mut first = SproutFrontier::empty();
        first.append_many(&[[1; 32], [2; 32]]).expect("room");
        let mut second = first.clone();
        second.append_many(&[[3; 32]]).expect("room");
        let mut original = record(9, 3);
        original.base.sprout_frontier = Some(Arc::new(second.clone()));
        original.new_sprout_trees = vec![Arc::new(first.clone()), Arc::new(second.clone())];
        let decoded = StateRecord::decode(&original.encode()).expect("decodes");
        assert_eq!(decoded.base.sprout_frontier.as_deref(), Some(&second));
        assert_eq!(decoded.new_sprout_trees, original.new_sprout_trees);
        assert_eq!(decoded.new_anchors, original.new_anchors);

        original.base.sprout_frontier = None;
        original.new_sprout_trees.clear();
        let unknown = original.encode();
        assert_eq!(unknown.last(), Some(&0));
        let decoded = StateRecord::decode(&unknown).expect("decodes");
        assert_eq!(decoded.base.sprout_frontier, None);
        let mut bad_tag = unknown;
        *bad_tag.last_mut().expect("not empty") = 2;
        let Err(PersistError::Corrupt(message)) = StateRecord::decode(&bad_tag).map(|_| ()) else {
            panic!("an unknown tag is an error");
        };
        assert!(message.contains("Sprout state tag"), "{message}");

        let genesis = StateRecord::decode(&encode_v3(&record(0, 0))).expect("decodes");
        assert_eq!(
            genesis.base.sprout_frontier.as_deref(),
            Some(&SproutFrontier::empty())
        );
        let above = StateRecord::decode(&encode_v3(&record(9, 3))).expect("decodes");
        assert_eq!(above.base.sprout_frontier, None);
        assert_eq!(above.base.value_pools, record(9, 3).base.value_pools);
    }

    /// The state log gives every Sprout treestate up to the selected record, and
    /// `Base::restore` makes each one a valid anchor.
    #[test]
    fn the_state_log_restores_the_sprout_treestates() {
        use hayai_coins::{MemBacking, MemConfig};
        use hayai_state::{Base, Chain};
        let tree = |leaf: u8| {
            let mut tree = SproutFrontier::empty();
            tree.append_many(&[[leaf; 32]]).expect("room");
            Arc::new(tree)
        };
        let d = dir();
        let mut log = StateLog::create(d.path(), &record(0, 0)).expect("create");
        for (height, leaf) in [(4u32, 1u8), (8, 2)] {
            let mut next = record(height, height as u8);
            next.base.sprout_frontier = Some(tree(leaf));
            next.new_sprout_trees = vec![tree(leaf)];
            log.write(&next).expect("write");
        }
        drop(log);
        let best = BestBlock {
            height: 4,
            hash: [4; 32],
        };
        let (_, recovered) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best)).expect("open");
        assert_eq!(recovered.sprout_trees, vec![tree(1)]);
        let (backing, _) =
            MemBacking::open(&d.path().join("coins"), &MemConfig::default()).expect("coins");
        let base = Base::restore(
            Arc::new(backing),
            recovered.base,
            recovered.anchors,
            recovered.sprout_trees,
        );
        assert_eq!(base.sprout_frontier, tree(1));
        let view = Chain::new(base).view();
        assert!(view.sprout_known());
        for (root, known) in [
            (tree(1).root(), true),
            (SproutFrontier::empty().root(), true),
            (tree(2).root(), false),
        ] {
            assert_eq!(view.has_anchor(Pool::Sprout, &root), known);
        }
    }

    /// A state log of an earlier build. Its record above the genesis block does not hold
    /// the transparent and the deferred value pool: the node refuses it and tells the
    /// operator what to do. It never takes the pools as zero.
    #[test]
    fn an_old_record_above_the_genesis_block_is_refused() {
        let best = |height: u32| BestBlock {
            height,
            hash: [height as u8; 32],
        };
        type Encoder = fn(&StateRecord) -> Vec<u8>;
        let encoders: [(u8, Encoder); 2] = [(1, encode_v1), (2, encode_v2)];
        for (version, encode) in encoders {
            let d = dir();
            let path = d.path().join(StateLog::FILE);
            let (mut log, _) = RecordLog::open(&path).expect("create");
            log.append(&encode(&record(0, 0))).expect("append");
            log.append(&encode(&record(4, 1))).expect("append");
            drop(log);
            let refused = StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best(4)));
            let Err(error) = refused.map(|_| ()) else {
                panic!("a version {version} record at height 4 is refused");
            };
            assert!(
                matches!(
                    &error,
                    PersistError::OutdatedRecord { height: 4, version: v, .. } if *v == version
                ),
                "{error}"
            );
            let message = error.to_string();
            for part in [
                "deferred value pool",
                "Remove the data directory",
                &d.path().display().to_string(),
            ] {
                assert!(message.contains(part), "{message}");
            }
            // The refusal changed nothing: the log still holds both records.
            let (_, payloads) = RecordLog::open(&path).expect("reopen");
            assert_eq!(payloads.len(), 2);

            // The start record at the genesis block has pools of zero in every version. The
            // node opens it and continues with records of the current version.
            let (mut log, recovered) =
                StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, None)
                    .expect("an old start record at height 0");
            assert_eq!(recovered.base.height, 0);
            log.write(&record(8, 2)).expect("write a current record");
            drop(log);
            let (_, recovered) =
                StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best(8)))
                    .expect("open the mixed log");
            assert_eq!(recovered.base.height, 8);
            assert_eq!(recovered.base.bits, record(8, 2).base.bits);
            assert_eq!(recovered.base.value_pools.deferred, 4);
            assert_eq!(recovered.base.value_pools.transparent, 1);
            assert_eq!(recovered.anchors.len(), 4);
        }
    }

    /// A restart with Ironwood state: the frontier, the value pool and the anchors of the
    /// records rebuild the base.
    #[test]
    fn the_ironwood_state_survives_a_restart() {
        use hayai_coins::{MemBacking, MemConfig};
        use hayai_state::{Base, Chain};

        let leaf = |byte: u8| {
            hayai_crypto::orchard::tree::MerkleHashOrchard::from_bytes(&[byte; 32])
                .expect("small values are field elements")
        };
        let mut frontier = IronwoodFrontier::empty();
        let first_root = frontier.append_many(&[leaf(3)]).expect("room").to_bytes();
        let mut start = record(0, 0);
        start.base.ironwood_frontier = Arc::new(frontier.clone());
        start.new_anchors.push((Pool::Ironwood, first_root));
        let second_root = frontier
            .append_many(&[leaf(4), leaf(5)])
            .expect("room")
            .to_bytes();
        let mut next = record(4, 1);
        next.base.ironwood_frontier = Arc::new(frontier.clone());
        next.base.value_pools.ironwood = 77;
        next.new_anchors.push((Pool::Ironwood, second_root));

        let d = dir();
        let mut log = StateLog::create(d.path(), &start).expect("create");
        log.write(&next).expect("write");
        drop(log);
        let best = BestBlock {
            height: 4,
            hash: [4; 32],
        };
        let (_, recovered) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best)).expect("open");
        assert_eq!(*recovered.base.ironwood_frontier, frontier);
        assert_eq!(recovered.base.value_pools.ironwood, 77);
        let (backing, _) =
            MemBacking::open(&d.path().join("coins"), &MemConfig::default()).expect("coins");
        let base = Base::restore(
            Arc::new(backing),
            recovered.base,
            recovered.anchors,
            recovered.sprout_trees,
        );
        assert_eq!(base.anchors.ironwood, second_root);
        assert_eq!(base.value_pools.ironwood, 77);
        let view = Chain::new(base).view();
        for root in [
            first_root,
            second_root,
            IronwoodFrontier::empty().root().to_bytes(),
        ] {
            assert!(view.has_anchor(Pool::Ironwood, &root));
        }
        assert!(!view.has_anchor(Pool::Ironwood, &[0; 32]));
        assert!(!view.has_anchor(Pool::Orchard, &second_root));
        assert_eq!(*view.frontiers().ironwood, frontier);
    }

    /// A version 2 record of a build that kept no Ironwood tree has the tag 0 in place of
    /// the Ironwood frontier. It loads with the empty tree.
    #[test]
    fn a_version_2_record_without_an_ironwood_frontier_loads_with_the_empty_tree() {
        let original = record(9, 3);
        let encoded = encode_v3(&original);
        // The Ironwood field of the record: the tag 1, the length 1 and the one byte of an
        // empty frontier.
        let field = [1u8, 1, 0, 0, 0, 0];
        let positions: Vec<usize> = encoded
            .windows(field.len())
            .enumerate()
            .filter(|(_, window)| *window == field)
            .map(|(at, _)| at)
            .collect();
        let [at] = positions[..] else {
            panic!("the Ironwood field is at one position, found {positions:?}");
        };
        let mut old = encoded[..at].to_vec();
        old.push(0);
        old.extend_from_slice(&encoded[at + field.len()..]);
        old[0] = RECORD_VERSION_2;
        let decoded = StateRecord::decode(&old).expect("a record with the tag 0 decodes");
        assert_eq!(*decoded.base.ironwood_frontier, IronwoodFrontier::empty());
        assert_eq!(decoded.base.value_pools, original.base.value_pools);
        assert_eq!(decoded.base.history, original.base.history);
        assert_eq!(decoded.new_anchors, original.new_anchors);
        // Another tag is an error.
        let mut unknown = old;
        unknown[at] = 2;
        let Err(PersistError::Corrupt(message)) = StateRecord::decode(&unknown).map(|_| ()) else {
            panic!("an unknown tag is an error");
        };
        assert!(message.contains("Ironwood frontier tag"), "{message}");
    }

    #[test]
    fn the_record_log_cuts_a_torn_tail_and_refuses_other_damage() {
        let d = dir();
        let path = d.path().join("log");
        let (mut log, existing) = RecordLog::open(&path).expect("create");
        assert!(existing.is_empty());
        log.append(b"one").expect("append");
        log.append(b"two").expect("append");
        drop(log);
        let whole = std::fs::read(&path).expect("read");
        // A last record cut short is dropped.
        std::fs::write(&path, &whole[..whole.len() - 2]).expect("write");
        let (mut log, records) = RecordLog::open(&path).expect("torn tail");
        assert_eq!(records, vec![b"one".to_vec()]);
        log.append(b"three").expect("append after the cut");
        drop(log);
        let (_, records) = RecordLog::open(&path).expect("reopen");
        assert_eq!(records, vec![b"one".to_vec(), b"three".to_vec()]);
        // A damaged record before the last one is an error.
        let mut damaged = std::fs::read(&path).expect("read");
        damaged[RECORD_HEADER] ^= 0xff;
        std::fs::write(&path, &damaged).expect("write");
        let Err(PersistError::Corrupt(_)) = RecordLog::open(&path).map(|_| ()) else {
            panic!("mid-file damage must be an error");
        };
        // A truncate drops the records after the kept ones.
        std::fs::write(&path, &whole).expect("restore");
        let (mut log, _) = RecordLog::open(&path).expect("open");
        log.truncate_to(1).expect("truncate");
        drop(log);
        let (_, records) = RecordLog::open(&path).expect("reopen");
        assert_eq!(records, vec![b"one".to_vec()]);
    }

    #[test]
    fn the_state_log_selects_the_record_of_the_best_block() {
        let d = dir();
        let mut log = StateLog::create(d.path(), &record(0, 0)).expect("create");
        log.write(&record(4, 1)).expect("write");
        log.write(&record(8, 2)).expect("write");
        // The same height and no new anchors writes nothing.
        let mut same = record(8, 2);
        same.new_anchors.clear();
        log.write(&same).expect("write");
        drop(log);

        let best = |height: u32| BestBlock {
            height,
            hash: [height as u8; 32],
        };
        // The coins store reached height 4 only: the record of 8 is dropped.
        let (mut log, recovered) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best(4)))
                .expect("open at 4");
        assert_eq!(recovered.base.height, 4);
        assert_eq!(recovered.anchors.len(), 4, "anchors of the records 0..=4");
        assert_eq!(recovered.start_height, 0);
        log.write(&record(8, 9)).expect("write again");
        drop(log);
        let (_, recovered) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best(8)))
                .expect("open at 8");
        assert_eq!(recovered.anchors.len(), 6);
        assert_eq!(recovered.anchors[4], (Pool::Sapling, [9; 32]));

        // No generation reached the coins store: the start record.
        let (_, recovered) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, None).expect("start");
        assert_eq!(recovered.base.height, 0);

        let Err(e) =
            StateLog::open(d.path(), NetworkKind::Regtest, Mode::Full, Some(best(5))).map(|_| ())
        else {
            panic!("a best block without a record is an error");
        };
        assert!(e.to_string().contains("no record"), "{e}");
        let Err(e) = StateLog::open(d.path(), NetworkKind::Mainnet, Mode::Full, None).map(|_| ())
        else {
            panic!("another network is an error");
        };
        assert!(e.to_string().contains("regtest/full"), "{e}");
    }
}
