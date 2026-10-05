//! Flat append-only block files with a RocksDB index by hash and by height.
//!
//! Layout under the store directory:
//!
//! - `blk-NNNNN.dat`: records of `[len u32 LE][wire bytes]`, appended in arrival order. The
//!   store starts a new file when the current one would exceed [`MAX_FILE_BYTES`].
//! - `index/`: a RocksDB database with three column families, `hash_to_loc` (block hash to
//!   [`Loc`]), `hash_to_height` (block hash to big-endian height) and `height_to_loc`
//!   (big-endian height to the [`Loc`] of the best-chain block of that height).
//!
//! To serve a block, the store does one positioned read of its record. It parses nothing.
//!
//! Side branches: the store keeps every appended block by hash. The index by height names one
//! block for each height: the block of the newest append at that height. After a reorg the
//! node appends the blocks of the new branch, and the blocks of the old branch stay readable
//! by hash. An entry by height above the tip of a shorter new branch still names a block of
//! the old branch, until the new branch reaches that height.
//!
//! Durability: the store writes a record to the data file first, and its index entries second,
//! in one RocksDB write batch. The store does not fsync the data files per block. The node
//! commits a block to the memory state before the block reaches this store, and the node can
//! fetch the chain again. [`BlockStore::sync`] fsyncs the current data file and the index WAL
//! for callers that want a durable point. A crash between the data write and the index write
//! leaves an unreferenced tail in the data file. Later appends follow that tail. The index
//! never points at bytes that the store did not write before the index entry.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use hayai_wire::header::BlockHash;
use hayai_wire::RawBlock;
use parking_lot::{Mutex, RwLock};
use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, WriteBatch, DB};

/// Maximum size of one `blk-NNNNN.dat` file. A record that would cross this limit starts the
/// next file. A file can therefore be slightly below the limit. A file is above the limit only
/// when a single record is larger than the limit.
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

const CF_HEIGHT_TO_LOC: &str = "height_to_loc";
const CF_HASH_TO_HEIGHT: &str = "hash_to_height";
const CF_HASH_TO_LOC: &str = "hash_to_loc";
const RECORD_HEADER: u64 = 4;

/// Position of a block record: data file number, offset of the 4-byte length prefix, and the
/// length of the wire bytes that follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loc {
    pub file: u32,
    pub offset: u64,
    pub len: u32,
}

impl Loc {
    const ENCODED_LEN: usize = 16;

    fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut out = [0u8; Self::ENCODED_LEN];
        out[..4].copy_from_slice(&self.file.to_le_bytes());
        out[4..12].copy_from_slice(&self.offset.to_le_bytes());
        out[12..].copy_from_slice(&self.len.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let Ok(arr) = <[u8; Self::ENCODED_LEN]>::try_from(bytes) else {
            return Err(Error::Corrupt(format!(
                "index value is {} bytes, expected {}",
                bytes.len(),
                Self::ENCODED_LEN
            )));
        };
        Ok(Self {
            file: u32::from_le_bytes(arr[..4].try_into().expect("4")),
            offset: u64::from_le_bytes(arr[4..12].try_into().expect("8")),
            len: u32::from_le_bytes(arr[12..].try_into().expect("4")),
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] rocksdb::Error),
    #[error("block of {0} bytes exceeds the u32 record length")]
    TooLarge(usize),
    #[error("corrupt store: {0}")]
    Corrupt(String),
}

struct Writer {
    file_no: u32,
    file: File,
    size: u64,
    tip: Option<u32>,
}

pub struct BlockStore {
    dir: PathBuf,
    db: DB,
    max_file_bytes: u64,
    writer: Mutex<Writer>,
    readers: RwLock<HashMap<u32, Arc<File>>>,
}

fn data_file_name(file_no: u32) -> String {
    format!("blk-{file_no:05}.dat")
}

fn height_key(height: u32) -> [u8; 4] {
    height.to_be_bytes()
}

fn decode_height(bytes: &[u8]) -> Result<u32, Error> {
    let Ok(arr) = <[u8; 4]>::try_from(bytes) else {
        return Err(Error::Corrupt(format!(
            "height key is {} bytes, expected 4",
            bytes.len()
        )));
    };
    Ok(u32::from_be_bytes(arr))
}

/// Fills `hash_to_loc` of an index that an earlier version wrote. That version kept one block
/// for each height, so the position of a hash is the position of its height.
fn index_by_hash(db: &DB) -> Result<(), Error> {
    let cf = |name| db.cf_handle(name).expect("cf created");
    let None = db
        .iterator_cf(cf(CF_HASH_TO_LOC), IteratorMode::Start)
        .next()
    else {
        return Ok(());
    };
    let mut batch = WriteBatch::default();
    for entry in db.iterator_cf(cf(CF_HASH_TO_HEIGHT), IteratorMode::Start) {
        let (hash, height) = entry?;
        let Some(loc) = db.get_pinned_cf(cf(CF_HEIGHT_TO_LOC), &height)? else {
            return Err(Error::Corrupt(format!(
                "block {} has the height {} and the height has no block",
                hex_of(&hash),
                decode_height(&height)?
            )));
        };
        batch.put_cf(cf(CF_HASH_TO_LOC), &hash, &loc);
    }
    db.write(batch)?;
    Ok(())
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl BlockStore {
    /// Opens or creates a store in `dir` with the default [`MAX_FILE_BYTES`] file limit.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, Error> {
        Self::open_with_file_limit(dir, MAX_FILE_BYTES)
    }

    /// Opens or creates a store whose data files roll over at `max_file_bytes`.
    pub fn open_with_file_limit(dir: impl AsRef<Path>, max_file_bytes: u64) -> Result<Self, Error> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cfs = [CF_HEIGHT_TO_LOC, CF_HASH_TO_HEIGHT, CF_HASH_TO_LOC]
            .map(|name| ColumnFamilyDescriptor::new(name, Options::default()));
        let db = DB::open_cf_descriptors(&opts, dir.join("index"), cfs)?;
        index_by_hash(&db)?;
        Self::with_index(
            dir,
            db,
            max_file_bytes,
            OpenOptions::new().create(true).append(true),
        )
    }

    /// Opens an existing store for reads. The open writes nothing to `dir` and takes no
    /// lock of the index, so it is safe while a node has the store open: the store then
    /// holds the blocks that the index had at the open. An append is an error.
    pub fn open_read_only(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let dir = dir.as_ref().to_path_buf();
        let db = DB::open_cf_for_read_only(
            &Options::default(),
            dir.join("index"),
            [CF_HEIGHT_TO_LOC, CF_HASH_TO_HEIGHT, CF_HASH_TO_LOC],
            false,
        )?;
        Self::with_index(dir, db, MAX_FILE_BYTES, OpenOptions::new().read(true))
    }

    /// The store over the open index `db`. `data_file` opens the current data file.
    fn with_index(
        dir: PathBuf,
        db: DB,
        max_file_bytes: u64,
        data_file: &OpenOptions,
    ) -> Result<Self, Error> {
        let tip = {
            let cf = db.cf_handle(CF_HEIGHT_TO_LOC).expect("cf created");
            match db.iterator_cf(cf, IteratorMode::End).next() {
                Some(entry) => Some(decode_height(&entry?.0)?),
                None => None,
            }
        };

        // The current data file is the highest-numbered one present. The store reuses a file
        // that it started just before a crash, even if nothing in the index points into it yet.
        let mut file_no = 0u32;
        for entry in std::fs::read_dir(&dir)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(num) = name
                .strip_prefix("blk-")
                .and_then(|rest| rest.strip_suffix(".dat"))
            else {
                continue;
            };
            if let Ok(n) = num.parse::<u32>() {
                file_no = file_no.max(n);
            }
        }
        let file = data_file.open(dir.join(data_file_name(file_no)))?;
        let size = file.metadata()?.len();

        Ok(Self {
            dir,
            db,
            max_file_bytes,
            writer: Mutex::new(Writer {
                file_no,
                file,
                size,
                tip,
            }),
            readers: RwLock::new(HashMap::new()),
        })
    }

    /// Appends a parsed block under its own hash.
    pub fn append(&self, height: u32, block: &RawBlock) -> Result<Loc, Error> {
        self.append_bytes(height, &block.hash(), &block.bytes)
    }

    /// Appends the wire bytes of the block at `height` with hash `hash` and makes it the
    /// best-chain block of `height`. A block that the store already holds is not written
    /// again: it only becomes the best-chain block of `height`.
    pub fn append_bytes(&self, height: u32, hash: &BlockHash, bytes: &[u8]) -> Result<Loc, Error> {
        let Ok(len) = u32::try_from(bytes.len()) else {
            return Err(Error::TooLarge(bytes.len()));
        };
        let mut w = self.writer.lock();
        if let Some(loc) = self.loc_of(hash)? {
            self.db
                .put_cf(self.cf(CF_HEIGHT_TO_LOC), height_key(height), loc.encode())?;
            w.tip = Some(w.tip.map_or(height, |t| t.max(height)));
            return Ok(loc);
        }

        let record_len = RECORD_HEADER + u64::from(len);
        if w.size > 0 && w.size + record_len > self.max_file_bytes {
            let next = w.file_no + 1;
            w.file = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(self.dir.join(data_file_name(next)))?;
            w.file_no = next;
            w.size = 0;
        }

        let loc = Loc {
            file: w.file_no,
            offset: w.size,
            len,
        };
        w.file.write_all(&len.to_le_bytes())?;
        w.file.write_all(bytes)?;
        w.size += record_len;

        let mut batch = WriteBatch::default();
        batch.put_cf(self.cf(CF_HEIGHT_TO_LOC), height_key(height), loc.encode());
        batch.put_cf(self.cf(CF_HASH_TO_HEIGHT), hash.0, height_key(height));
        batch.put_cf(self.cf(CF_HASH_TO_LOC), hash.0, loc.encode());
        self.db.write(batch)?;

        w.tip = Some(w.tip.map_or(height, |t| t.max(height)));
        Ok(loc)
    }

    /// Highest stored height, if any.
    pub fn tip_height(&self) -> Option<u32> {
        self.writer.lock().tip
    }

    /// Position of the best-chain block of `height`.
    pub fn loc(&self, height: u32) -> Result<Option<Loc>, Error> {
        match self
            .db
            .get_pinned_cf(self.cf(CF_HEIGHT_TO_LOC), height_key(height))?
        {
            Some(v) => Ok(Some(Loc::decode(&v)?)),
            None => Ok(None),
        }
    }

    /// Position of the block `hash`, on any branch.
    pub fn loc_of(&self, hash: &BlockHash) -> Result<Option<Loc>, Error> {
        match self.db.get_pinned_cf(self.cf(CF_HASH_TO_LOC), hash.0)? {
            Some(v) => Ok(Some(Loc::decode(&v)?)),
            None => Ok(None),
        }
    }

    /// Height of the block `hash`, on any branch.
    pub fn height_of(&self, hash: &BlockHash) -> Result<Option<u32>, Error> {
        match self.db.get_pinned_cf(self.cf(CF_HASH_TO_HEIGHT), hash.0)? {
            Some(v) => Ok(Some(decode_height(&v)?)),
            None => Ok(None),
        }
    }

    /// Wire bytes of the best-chain block of `height`.
    pub fn get_bytes(&self, height: u32) -> Result<Option<Bytes>, Error> {
        match self.loc(height)? {
            Some(loc) => Ok(Some(self.read_record(loc)?)),
            None => Ok(None),
        }
    }

    /// Wire bytes of the block `hash`, on any branch.
    pub fn get_by_hash(&self, hash: &BlockHash) -> Result<Option<Bytes>, Error> {
        match self.loc_of(hash)? {
            Some(loc) => Ok(Some(self.read_record(loc)?)),
            None => Ok(None),
        }
    }

    /// Fsyncs the current data file and the index write-ahead log.
    pub fn sync(&self) -> Result<(), Error> {
        let w = self.writer.lock();
        w.file.sync_data()?;
        self.db.flush_wal(true)?;
        Ok(())
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(name)
            .expect("column family created at open")
    }

    fn reader(&self, file_no: u32) -> Result<Arc<File>, Error> {
        if let Some(f) = self.readers.read().get(&file_no) {
            return Ok(Arc::clone(f));
        }
        let f = Arc::new(File::open(self.dir.join(data_file_name(file_no)))?);
        Ok(Arc::clone(self.readers.write().entry(file_no).or_insert(f)))
    }

    fn read_record(&self, loc: Loc) -> Result<Bytes, Error> {
        let file = self.reader(loc.file)?;
        let mut buf = vec![0u8; RECORD_HEADER as usize + loc.len as usize];
        file.read_exact_at(&mut buf, loc.offset)?;
        let stored_len = u32::from_le_bytes(buf[..4].try_into().expect("4"));
        if stored_len != loc.len {
            return Err(Error::Corrupt(format!(
                "record at file {} offset {} has length {stored_len}, index says {}",
                loc.file, loc.offset, loc.len
            )));
        }
        Ok(Bytes::from(buf).slice(RECORD_HEADER as usize..))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_crypto::zcash_protocol::consensus::BranchId;

    fn mainnet_block(name: &str) -> RawBlock {
        let hex = std::fs::read_to_string(format!(
            "{}/../hayai-wire/tests/vectors/block-main-{name}.hex",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        RawBlock::parse(Bytes::from(hex::decode(hex.trim()).unwrap()), BranchId::Nu5).unwrap()
    }

    /// Scratch directories are under the workspace target directory and not under /tmp.
    fn tempdir() -> tempfile::TempDir {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&base).unwrap();
        tempfile::tempdir_in(base).unwrap()
    }

    fn hash(i: u32) -> BlockHash {
        let mut h = [0u8; 32];
        h[..4].copy_from_slice(&i.to_le_bytes());
        BlockHash(h)
    }

    #[test]
    fn append_and_get_round_trip() {
        let dir = tempdir();
        let store = BlockStore::open(dir.path()).unwrap();
        assert_eq!(store.tip_height(), None);
        assert_eq!(store.get_bytes(0).unwrap(), None);

        let blocks = [
            mainnet_block("1-687-106"),
            mainnet_block("1-687-107"),
            mainnet_block("1-687-108"),
        ];
        for (i, b) in blocks.iter().enumerate() {
            let loc = store.append(1_687_106 + i as u32, b).unwrap();
            assert_eq!(loc.len as usize, b.bytes.len());
        }
        assert_eq!(store.tip_height(), Some(1_687_108));
        for (i, b) in blocks.iter().enumerate() {
            let h = 1_687_106 + i as u32;
            assert_eq!(store.get_bytes(h).unwrap().unwrap(), b.bytes);
            assert_eq!(store.get_by_hash(&b.hash()).unwrap().unwrap(), b.bytes);
            assert_eq!(store.height_of(&b.hash()).unwrap(), Some(h));
        }
        assert_eq!(store.get_by_hash(&hash(99)).unwrap(), None);
    }

    /// A reorg: the block of the newest append is the block of its height, and each block
    /// stays readable by hash. A block that the store holds is not written again.
    #[test]
    fn side_branches_stay_readable_by_hash() {
        let dir = tempdir();
        let store = BlockStore::open(dir.path()).unwrap();
        store.append_bytes(1, &hash(1), b"one").unwrap();
        let old = store.append_bytes(2, &hash(2), b"two-old").unwrap();
        let new = store.append_bytes(2, &hash(20), b"two-new").unwrap();
        assert_ne!(old, new);
        assert_eq!(store.get_bytes(2).unwrap().unwrap(), &b"two-new"[..]);
        assert_eq!(
            store.get_by_hash(&hash(2)).unwrap().unwrap(),
            &b"two-old"[..]
        );
        assert_eq!(
            store.get_by_hash(&hash(20)).unwrap().unwrap(),
            &b"two-new"[..]
        );
        assert_eq!(store.height_of(&hash(2)).unwrap(), Some(2));
        assert_eq!(store.height_of(&hash(20)).unwrap(), Some(2));
        // The old branch is the best chain again: no second record.
        assert_eq!(store.append_bytes(2, &hash(2), b"two-old").unwrap(), old);
        assert_eq!(store.get_bytes(2).unwrap().unwrap(), &b"two-old"[..]);
        drop(store);
        let store = BlockStore::open(dir.path()).unwrap();
        assert_eq!(store.tip_height(), Some(2));
        assert_eq!(store.get_bytes(2).unwrap().unwrap(), &b"two-old"[..]);
        assert_eq!(
            store.get_by_hash(&hash(20)).unwrap().unwrap(),
            &b"two-new"[..]
        );
    }

    /// An index without `hash_to_loc` entries (an earlier version of the store) gets them at
    /// the next open.
    #[test]
    fn an_index_of_the_earlier_version_is_completed_at_open() {
        let dir = tempdir();
        {
            let store = BlockStore::open(dir.path()).unwrap();
            store.append_bytes(1, &hash(1), b"one").unwrap();
            store.append_bytes(2, &hash(2), b"two").unwrap();
            for h in [hash(1), hash(2)] {
                store.db.delete_cf(store.cf(CF_HASH_TO_LOC), h.0).unwrap();
            }
            assert_eq!(store.get_by_hash(&hash(2)).unwrap(), None);
        }
        let store = BlockStore::open(dir.path()).unwrap();
        assert_eq!(store.get_by_hash(&hash(1)).unwrap().unwrap(), &b"one"[..]);
        assert_eq!(store.get_by_hash(&hash(2)).unwrap().unwrap(), &b"two"[..]);
    }

    #[test]
    fn files_roll_over_at_the_limit_and_survive_reopen() {
        let dir = tempdir();
        let payload = |i: u32| vec![i as u8; 1000];
        {
            let store = BlockStore::open_with_file_limit(dir.path(), 2500).unwrap();
            for i in 0..7 {
                let loc = store.append_bytes(i, &hash(i), &payload(i)).unwrap();
                // Two 1004-byte records fit per file.
                assert_eq!(loc.file, i / 2, "block {i}");
                assert_eq!(loc.offset, u64::from(i % 2) * 1004);
            }
            store.sync().unwrap();
        }
        let names: Vec<String> = {
            let mut v: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.starts_with("blk-"))
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            names,
            [
                "blk-00000.dat",
                "blk-00001.dat",
                "blk-00002.dat",
                "blk-00003.dat"
            ]
        );

        let store = BlockStore::open_with_file_limit(dir.path(), 2500).unwrap();
        assert_eq!(store.tip_height(), Some(6));
        for i in 0..7 {
            assert_eq!(store.get_bytes(i).unwrap().unwrap(), payload(i));
        }
        let loc = store.append_bytes(7, &hash(7), &payload(7)).unwrap();
        assert_eq!(
            loc,
            Loc {
                file: 3,
                offset: 1004,
                len: 1000
            }
        );
        assert_eq!(store.get_bytes(7).unwrap().unwrap(), payload(7));
    }

    #[test]
    fn unindexed_tail_is_skipped_by_later_appends() {
        let dir = tempdir();
        {
            let store = BlockStore::open(dir.path()).unwrap();
            store.append_bytes(0, &hash(0), b"genesis").unwrap();
        }
        // Simulate a crash after the data write and before the index write. A partial record
        // is at the end of the file.
        {
            let mut f = OpenOptions::new()
                .append(true)
                .open(dir.path().join("blk-00000.dat"))
                .unwrap();
            f.write_all(&[9, 0, 0, 0, 1, 2, 3]).unwrap();
        }
        let store = BlockStore::open(dir.path()).unwrap();
        assert_eq!(store.tip_height(), Some(0));
        let loc = store.append_bytes(1, &hash(1), b"one").unwrap();
        assert_eq!(loc.offset, 4 + 7 + 7);
        assert_eq!(store.get_bytes(0).unwrap().unwrap(), &b"genesis"[..]);
        assert_eq!(store.get_bytes(1).unwrap().unwrap(), &b"one"[..]);
    }

    #[test]
    fn corrupt_record_length_is_reported() {
        let dir = tempdir();
        let store = BlockStore::open(dir.path()).unwrap();
        store.append_bytes(5, &hash(5), b"hello").unwrap();
        drop(store);
        {
            let f = OpenOptions::new()
                .write(true)
                .open(dir.path().join("blk-00000.dat"))
                .unwrap();
            f.write_all_at(&[4, 0, 0, 0], 0).unwrap();
        }
        let store = BlockStore::open(dir.path()).unwrap();
        assert!(matches!(store.get_bytes(5), Err(Error::Corrupt(_))));
    }

    #[test]
    fn heights_need_not_be_contiguous() {
        let dir = tempdir();
        let store = BlockStore::open(dir.path()).unwrap();
        store.append_bytes(10, &hash(10), b"ten").unwrap();
        store.append_bytes(3, &hash(3), b"three").unwrap();
        assert_eq!(store.tip_height(), Some(10));
        assert_eq!(store.get_bytes(3).unwrap().unwrap(), &b"three"[..]);
        assert_eq!(store.get_bytes(4).unwrap(), None);
        assert_eq!(
            store.loc(10).unwrap(),
            Some(Loc {
                file: 0,
                offset: 0,
                len: 3
            })
        );
    }

    /// Each file below `dir` with its length, its modification time and its content.
    fn listing(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime, Vec<u8>)> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = std::fs::metadata(&path).unwrap();
            match meta.is_dir() {
                true => files.extend(listing(&path)),
                false => files.push((
                    path.clone(),
                    meta.len(),
                    meta.modified().unwrap(),
                    std::fs::read(&path).unwrap(),
                )),
            }
        }
        files.sort();
        files
    }

    /// A read-only open reads the blocks of a store that was not closed with a sync, and
    /// also while the store is open for writes. It changes no file, it does not append,
    /// and it does not make a store.
    #[test]
    fn a_read_only_open_reads_the_blocks_and_writes_nothing() {
        let dir = tempdir();
        let store = BlockStore::open(dir.path()).unwrap();
        store.append_bytes(1, &hash(1), b"one").unwrap();
        store.append_bytes(2, &hash(2), b"two").unwrap();
        let live = BlockStore::open_read_only(dir.path()).unwrap();
        assert_eq!(live.tip_height(), Some(2));
        drop((live, store));

        let before = listing(dir.path());
        let read = BlockStore::open_read_only(dir.path()).unwrap();
        assert_eq!(read.tip_height(), Some(2));
        assert_eq!(read.get_bytes(1).unwrap().unwrap(), &b"one"[..]);
        assert_eq!(read.get_by_hash(&hash(2)).unwrap().unwrap(), &b"two"[..]);
        let Err(_) = read.append_bytes(3, &hash(3), b"three") else {
            panic!("an append to a read-only store");
        };
        drop(read);
        assert_eq!(listing(dir.path()), before);

        let empty = tempdir();
        let Err(_) = BlockStore::open_read_only(empty.path()) else {
            panic!("a read-only open of a directory without a store");
        };
        assert_eq!(listing(empty.path()), Vec::new());
    }

    #[test]
    fn loc_encoding_round_trips() {
        let loc = Loc {
            file: 7,
            offset: 1 << 40,
            len: u32::MAX,
        };
        assert_eq!(Loc::decode(&loc.encode()).unwrap(), loc);
        assert!(matches!(Loc::decode(&[0; 3]), Err(Error::Corrupt(_))));
    }
}
