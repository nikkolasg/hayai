//! The checkpoint lists: trusted `(height, hash)` pairs of the best chain of a network.
//!
//! A header at a checkpoint height must have the checkpoint hash. A block at or below the
//! last checkpoint is verified by its hash only: the node does not check its proofs, its
//! signatures or its scripts (`hayai_validate::apply_checkpointed`). This is the behaviour
//! of Zebra and Zakura.
//!
//! # Source
//!
//! The Mainnet and Testnet lists are the files `main-checkpoints.txt` and
//! `test-checkpoints.txt` of the Zakura repository, directory
//! `crates/zakura-chain/src/parameters/checkpoint/`, at revision
//! `13779158253cfe315f73eadffb9b4c93c25e82a5`. The copies are in `src/checkpoints/`. Zakura
//! puts a checkpoint at each 400 blocks, or earlier when the blocks since the last
//! checkpoint have 32 MB.
//!
//! | Network | Checkpoints | Last height | Bytes in the binary |
//! |---|---|---|---|
//! | Mainnet | 14,385 | 3,499,045 | 517,860 |
//! | Testnet | 10,059 | 4,023,200 | 362,124 |
//!
//! Regtest has one checkpoint: the genesis block.
//!
//! The build script converts each file to 36 bytes for each checkpoint (`build.rs`). The
//! compiler decodes each list into a static array ([`decode`]), so a list needs no work at
//! run time.

use std::borrow::Cow;

use hayai_wire::header::BlockHash;

use crate::Network;

/// Bytes of one embedded checkpoint: the height (`u32`, little-endian), then the hash.
const RECORD_BYTES: usize = 36;

const MAINNET_RECORDS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/main-checkpoints.bin"));
const TESTNET_RECORDS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/test-checkpoints.bin"));

/// The Mainnet list, in height order.
pub(crate) static MAINNET: [(u32, BlockHash); MAINNET_RECORDS.len() / RECORD_BYTES] =
    decode(MAINNET_RECORDS);
/// The Testnet list, in height order.
pub(crate) static TESTNET: [(u32, BlockHash); TESTNET_RECORDS.len() / RECORD_BYTES] =
    decode(TESTNET_RECORDS);

/// Decodes an embedded list at compile time. The build script wrote complete records in
/// height order.
const fn decode<const N: usize>(bytes: &[u8]) -> [(u32, BlockHash); N] {
    let (records, rest) = bytes.as_chunks::<RECORD_BYTES>();
    assert!(
        rest.is_empty() && records.len() == N,
        "the build script writes complete records"
    );
    let mut entries = [(0, BlockHash([0; 32])); N];
    let mut i = 0;
    while i < N {
        let Some((height, hash)) = records[i].split_first_chunk::<4>() else {
            unreachable!();
        };
        let Some(hash) = hash.first_chunk::<32>() else {
            unreachable!();
        };
        entries[i] = (u32::from_le_bytes(*height), BlockHash(*hash));
        i += 1;
    }
    entries
}

/// Two checkpoints of a list have the same height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("two checkpoints have the height {0}")]
pub struct DuplicateCheckpoint(pub u32);

/// A checkpoint list. A clone of an embedded list shares the entries. A clone of a list
/// from [`Checkpoints::new`] copies them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Checkpoints {
    /// In height order, one checkpoint for each height.
    entries: Cow<'static, [(u32, BlockHash)]>,
}

impl Checkpoints {
    /// A list from `entries`, in any order. A network has its list in
    /// [`Network::checkpoints`]. This function is for a chain of generated blocks and for
    /// the list of a [`crate::ChainSpec`].
    pub fn new(mut entries: Vec<(u32, BlockHash)>) -> Result<Self, DuplicateCheckpoint> {
        entries.sort_by_key(|(height, _)| *height);
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(DuplicateCheckpoint(pair[0].0));
        }
        Ok(Self {
            entries: Cow::Owned(entries),
        })
    }

    /// The list with its entries in memory until the process ends. A clone of the result
    /// shares the entries.
    pub(crate) fn leak(self) -> Self {
        match self.entries {
            Cow::Owned(entries) => Self::embedded(Box::leak(entries.into_boxed_slice())),
            Cow::Borrowed(_) => self,
        }
    }

    /// The list of a built-in network: `entries` are in height order, one for each height.
    pub(crate) const fn embedded(entries: &'static [(u32, BlockHash)]) -> Self {
        Self {
            entries: Cow::Borrowed(entries),
        }
    }

    /// The checkpoint hash at `height`. `None`: the height has no checkpoint.
    pub fn hash_at(&self, height: u32) -> Option<BlockHash> {
        let at = self
            .entries
            .binary_search_by_key(&height, |(h, _)| *h)
            .ok()?;
        Some(self.entries[at].1)
    }

    /// The height of the last checkpoint. `None`: the list is empty.
    pub fn last_height(&self) -> Option<u32> {
        self.entries.last().map(|(height, _)| *height)
    }

    /// The height of the last checkpoint at or below `height`.
    pub fn last_at_or_below(&self, height: u32) -> Option<u32> {
        let reached = self.entries.partition_point(|(h, _)| *h <= height);
        Some(self.entries[reached.checked_sub(1)?].0)
    }

    /// The checkpoints in height order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, BlockHash)> + '_ {
        self.entries.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Network {
    /// The checkpoint list of the network ([`crate::ChainSpec::checkpoints`]).
    pub fn checkpoints(self) -> &'static Checkpoints {
        &self.spec().checkpoints
    }

    /// The height of the mandatory checkpoint ([`crate::ChainSpec::mandatory_checkpoint_height`]).
    /// A block at or below this height has no full validation: the node accepts it only on
    /// the checkpointed chain. On Mainnet, Testnet and Regtest it is the last block before
    /// the Canopy activation (Zakura `mandatory_checkpoint_height`,
    /// `zakura-chain/src/parameters/network.rs:271`). A configured Regtest has the height
    /// of its [`crate::RegtestConfig`].
    pub fn mandatory_checkpoint_height(self) -> u32 {
        self.spec().mandatory_checkpoint_height
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// The checkpoints of a source file.
    fn parse(text: &str) -> Vec<(u32, BlockHash)> {
        text.lines()
            .map(|line| {
                let (height, hash) = line.split_once(' ').expect("`height hash`");
                let mut bytes: [u8; 32] = hex::decode(hash)
                    .expect("hex")
                    .try_into()
                    .expect("32 bytes");
                bytes.reverse();
                (height.parse().expect("a height"), BlockHash(bytes))
            })
            .collect()
    }

    const SOURCES: [(Network, &str, &str); 2] = [
        (
            Network::Mainnet,
            "main-checkpoints.txt",
            include_str!("checkpoints/main-checkpoints.txt"),
        ),
        (
            Network::Testnet,
            "test-checkpoints.txt",
            include_str!("checkpoints/test-checkpoints.txt"),
        ),
    ];

    #[test]
    fn each_list_starts_at_genesis_and_increases() {
        for network in Network::ALL {
            let list = network.checkpoints();
            let entries: Vec<_> = list.iter().collect();
            assert_eq!(
                entries[0],
                (0, network.params().genesis_hash),
                "{network:?}"
            );
            assert!(
                entries.windows(2).all(|pair| pair[0].0 < pair[1].0),
                "{network:?}"
            );
            // Zakura `check_checkpoint_coverage`: the list covers the blocks that have no
            // full validation.
            let last = list.last_height().expect("a genesis checkpoint");
            assert!(last >= network.mandatory_checkpoint_height(), "{network:?}");
        }
        assert_eq!(Network::Regtest.checkpoints().len(), 1);
    }

    #[test]
    fn the_embedded_lists_are_the_source_files() {
        for (network, _, text) in SOURCES {
            let list = network.checkpoints();
            assert!(list.iter().eq(parse(text)), "{network:?}");
        }
        let mainnet = Network::Mainnet.checkpoints();
        assert_eq!(
            (mainnet.len(), mainnet.last_height()),
            (14_385, Some(3_499_045))
        );
        let testnet = Network::Testnet.checkpoints();
        assert_eq!(
            (testnet.len(), testnet.last_height()),
            (10_059, Some(4_023_200))
        );
    }

    /// The source files against the files of the Zakura clone beside this repository. The
    /// test has no clone on a machine without one, and then it compares nothing.
    #[test]
    fn the_source_files_are_the_files_of_the_zakura_clone() {
        let clone = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../zakura-src/zakura/crates/zakura-chain/src/parameters/checkpoint");
        if !clone.is_dir() {
            eprintln!("no Zakura clone at {}", clone.display());
            return;
        }
        for (_, name, text) in SOURCES {
            let theirs = std::fs::read_to_string(clone.join(name)).expect("a checkpoint file");
            assert!(theirs == text, "{name} differs from the clone");
        }
    }

    #[test]
    fn lookups() {
        let hash = |n: u8| BlockHash([n; 32]);
        let list = Checkpoints::new(vec![(50, hash(5)), (20, hash(2))]).expect("two heights");
        assert_eq!(list.hash_at(20), Some(hash(2)));
        assert_eq!(list.hash_at(21), None);
        assert_eq!(list.last_height(), Some(50));
        assert_eq!(list.last_at_or_below(19), None);
        assert_eq!(list.last_at_or_below(20), Some(20));
        assert_eq!(list.last_at_or_below(49), Some(20));
        assert_eq!(list.last_at_or_below(u32::MAX), Some(50));
        assert_eq!(
            Checkpoints::new(vec![(7, hash(1)), (3, hash(2)), (7, hash(3))]),
            Err(DuplicateCheckpoint(7))
        );
        let empty = Checkpoints::new(Vec::new()).expect("no heights");
        assert_eq!((empty.last_height(), empty.is_empty()), (None, true));
    }

    #[test]
    fn the_mandatory_checkpoint_is_the_block_before_canopy() {
        assert_eq!(Network::Mainnet.mandatory_checkpoint_height(), 1_046_399);
        assert_eq!(Network::Testnet.mandatory_checkpoint_height(), 1_028_499);
        assert_eq!(Network::Regtest.mandatory_checkpoint_height(), 0);
    }
}
