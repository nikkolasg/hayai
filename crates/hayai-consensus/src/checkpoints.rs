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
//! first call of [`Network::checkpoints`] for a network decodes its list.

use std::sync::{Arc, LazyLock};

use hayai_wire::header::BlockHash;

use crate::{Network, Upgrade};

/// Bytes of one embedded checkpoint: the height (`u32`, little-endian), then the hash.
const RECORD_BYTES: usize = 36;

static MAINNET: LazyLock<Checkpoints> = LazyLock::new(|| {
    decode(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/main-checkpoints.bin"
    )))
});
static TESTNET: LazyLock<Checkpoints> = LazyLock::new(|| {
    decode(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/test-checkpoints.bin"
    )))
});
static REGTEST: LazyLock<Checkpoints> = LazyLock::new(|| Checkpoints {
    entries: Arc::new([(0, Network::Regtest.params().genesis_hash)]),
});

/// Decodes an embedded list. The build script wrote the records in height order.
fn decode(bytes: &[u8]) -> Checkpoints {
    let (records, []) = bytes.as_chunks::<RECORD_BYTES>() else {
        unreachable!("the build script writes complete records");
    };
    let entries = records
        .iter()
        .map(|record| {
            let (height, hash) = record.split_at(4);
            (
                u32::from_le_bytes(height.try_into().expect("4 bytes")),
                BlockHash(hash.try_into().expect("32 bytes")),
            )
        })
        .collect();
    Checkpoints { entries }
}

/// Two checkpoints of a list have the same height.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("two checkpoints have the height {0}")]
pub struct DuplicateCheckpoint(pub u32);

/// A checkpoint list. A clone shares the entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Checkpoints {
    /// In height order, one checkpoint for each height.
    entries: Arc<[(u32, BlockHash)]>,
}

impl Checkpoints {
    /// A list from `entries`, in any order. A network has its list in
    /// [`Network::checkpoints`]. This function is for a chain of generated blocks.
    pub fn new(mut entries: Vec<(u32, BlockHash)>) -> Result<Self, DuplicateCheckpoint> {
        entries.sort_by_key(|(height, _)| *height);
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(DuplicateCheckpoint(pair[0].0));
        }
        Ok(Self {
            entries: entries.into(),
        })
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
    /// The checkpoint list of the network.
    pub fn checkpoints(self) -> &'static Checkpoints {
        match self {
            Network::Mainnet => &MAINNET,
            Network::Testnet => &TESTNET,
            Network::Regtest => &REGTEST,
            Network::ConfiguredRegtest(config) => config.checkpoints(),
        }
    }

    /// The height of the mandatory checkpoint: the last block before the Canopy
    /// activation (Zakura `mandatory_checkpoint_height`,
    /// `zakura-chain/src/parameters/network.rs:271`). A block at or below this height has
    /// no full validation: the node accepts it only on the checkpointed chain. A configured
    /// Regtest has the height of its [`crate::RegtestConfig`].
    pub fn mandatory_checkpoint_height(self) -> u32 {
        if let Network::ConfiguredRegtest(config) = self {
            return config.mandatory_checkpoint_height();
        }
        let Some(canopy) = self.activation_height(Upgrade::Canopy) else {
            unreachable!("each network has a Canopy activation height");
        };
        canopy - 1
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
