//! The block vectors of `tests/vectors/` and the network facts the harness needs.
//!
//! The files are copies of Zebra's published test vectors (`zebra-test/src/vectors/`):
//! `block-<network>-<height>[-bad].hex` (hex text) or `.bin` (raw bytes), the final tree
//! roots of `block.rs` in `final-roots.json`, and the Sapling tree state files.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bytes::Bytes;
use hayai_consensus::{rules_at, HistoryVersion, Network, RuleSet, Upgrade};
use hayai_wire::header::BlockHeader;
use serde::Deserialize;

/// A public network with block vectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Net {
    Main,
    Test,
}

impl Net {
    pub const ALL: [Net; 2] = [Net::Main, Net::Test];

    /// The network name in the vector file names.
    pub fn name(self) -> &'static str {
        match self {
            Net::Main => "main",
            Net::Test => "test",
        }
    }

    pub fn network(self) -> Network {
        match self {
            Net::Main => Network::Mainnet,
            Net::Test => Network::Testnet,
        }
    }

    /// The rule set of a block at `height`. Every vector is at a height with a rule set.
    pub fn rules(self, height: u32) -> &'static RuleSet {
        rules_at(self.network(), height).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The activation height of `upgrade`. Every upgrade the harness asks for has one.
    pub fn activation(self, upgrade: Upgrade) -> u32 {
        let Some(height) = self.network().activation_height(upgrade) else {
            panic!("{upgrade:?} has no activation height on {self:?}");
        };
        height
    }

    /// The height of the first block with a JoinSplit, as `zebra-chain` publishes it with
    /// the Sprout roots (`src/tests/vectors.rs`, `MAINNET_FIRST_JOINSPLIT_HEIGHT`,
    /// `TESTNET_FIRST_JOINSPLIT_HEIGHT`). Below it the Sprout tree is empty, the Sprout
    /// nullifier set is empty and the Sprout pool is zero.
    pub fn first_joinsplit_height(self) -> u32 {
        match self {
            Net::Main => 396,
            Net::Test => 2_259,
        }
    }
}

/// Whether blocks of `rules` have a leaf in the ZIP 221 history tree (from Heartwood).
pub fn has_history_leaf(rules: &RuleSet) -> bool {
    rules.history != HistoryVersion::None
}

/// Whether the history leaf of a block of `rules` holds the Orchard root (from NU5).
pub fn orchard_in_history_leaf(rules: &RuleSet) -> bool {
    matches!(rules.history, HistoryVersion::V2 | HistoryVersion::V3)
}

/// One block vector.
pub struct BlockVector {
    /// The file name without `block-` and the extension, for example `main-0-419-200`.
    pub name: String,
    pub net: Net,
    pub height: u32,
    /// Zebra publishes the vector as an invalid block (`-bad`).
    pub invalid: bool,
    pub bytes: Bytes,
}

/// The final roots of the note commitment trees after a block, in header byte order.
#[derive(Clone, Copy, Default)]
pub struct FinalRoots {
    pub sprout: Option<[u8; 32]>,
    pub sapling: Option<[u8; 32]>,
    pub orchard: Option<[u8; 32]>,
}

/// One entry of `final-roots.json`. The roots are in the byte order that `zcash-cli` prints.
#[derive(Deserialize)]
struct RootsEntry {
    height: u32,
    sprout: Option<String>,
    sapling: Option<String>,
    orchard: Option<String>,
}

/// The whole vector set.
pub struct VectorSet {
    /// The vectors in run order: by network, then height, a published-invalid vector after
    /// the valid vector of the same height.
    pub blocks: Vec<BlockVector>,
    /// The header of each published-valid vector.
    headers: BTreeMap<(Net, u32), BlockHeader>,
    roots: BTreeMap<(Net, u32), FinalRoots>,
    /// Sapling tree states in the `zcashd` commitment tree encoding.
    sapling_tree_states: BTreeMap<(Net, u32), Vec<u8>>,
}

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

/// A root that `zcash-cli` prints, in header byte order.
fn root_from_display(text: &str) -> [u8; 32] {
    let mut root: [u8; 32] = hex::decode(text)
        .expect("a root is hex")
        .try_into()
        .expect("a root has 32 bytes");
    root.reverse();
    root
}

/// The network and the height of a `<network>-<d>-<ddd>-<ddd>` name.
fn network_and_height(name: &str) -> (Net, u32) {
    let (net, digits) = match name.split_once('-') {
        Some(("main", rest)) => (Net::Main, rest),
        Some(("test", rest)) => (Net::Test, rest),
        _ => panic!("vector {name} names no network"),
    };
    let height: String = digits.split('-').take(3).collect();
    let Ok(height) = height.parse() else {
        panic!("vector {name} names no height");
    };
    (net, height)
}

impl VectorSet {
    /// Reads every vector file of `tests/vectors/`.
    pub fn load() -> Self {
        let dir = vectors_dir();
        let mut blocks = Vec::new();
        let mut sapling_tree_states = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("read the vector directory") {
            let path = entry.expect("read a directory entry").path();
            let file = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a UTF-8 file name");
            let Some((stem, extension)) = file.rsplit_once('.') else {
                panic!("vector file {file} has no extension");
            };
            let read_hex = || {
                let text = std::fs::read_to_string(&path).expect("read a hex vector");
                hex::decode(text.trim()).unwrap_or_else(|e| panic!("{file}: {e}"))
            };
            if let Some(name) = stem.strip_prefix("block-") {
                let bytes = match extension {
                    "hex" => read_hex(),
                    "bin" => std::fs::read(&path).expect("read a binary vector"),
                    other => panic!("block vector {file} has the unknown extension {other}"),
                };
                let (net, height) = network_and_height(name);
                blocks.push(BlockVector {
                    name: name.to_string(),
                    net,
                    height,
                    invalid: name.ends_with("-bad"),
                    bytes: Bytes::from(bytes),
                });
            } else if let Some(name) = stem.strip_prefix("sapling-treestate-") {
                sapling_tree_states.insert(network_and_height(name), read_hex());
            }
        }
        blocks.sort_by_key(|b| (b.net, b.height, b.invalid));

        let headers = blocks
            .iter()
            .filter(|b| !b.invalid)
            .map(|b| {
                let header = BlockHeader::parse(&b.bytes)
                    .unwrap_or_else(|e| panic!("header of {}: {e}", b.name));
                ((b.net, b.height), header)
            })
            .collect();

        let text =
            std::fs::read_to_string(dir.join("final-roots.json")).expect("read final-roots.json");
        let by_network: BTreeMap<String, Vec<RootsEntry>> =
            serde_json::from_str(&text).expect("parse final-roots.json");
        let mut roots = BTreeMap::new();
        for net in Net::ALL {
            for entry in &by_network[net.name()] {
                roots.insert(
                    (net, entry.height),
                    FinalRoots {
                        sprout: entry.sprout.as_deref().map(root_from_display),
                        sapling: entry.sapling.as_deref().map(root_from_display),
                        orchard: entry.orchard.as_deref().map(root_from_display),
                    },
                );
            }
        }
        Self {
            blocks,
            headers,
            roots,
            sapling_tree_states,
        }
    }

    /// The header of the published-valid vector at `height`.
    pub fn header(&self, net: Net, height: u32) -> Option<&BlockHeader> {
        self.headers.get(&(net, height))
    }

    /// The published final roots after the block at `height`.
    pub fn roots(&self, net: Net, height: u32) -> FinalRoots {
        self.roots.get(&(net, height)).copied().unwrap_or_default()
    }

    /// The published Sapling tree state after the block at `height`.
    pub fn sapling_tree_state(&self, net: Net, height: u32) -> Option<&[u8]> {
        self.sapling_tree_states
            .get(&(net, height))
            .map(Vec::as_slice)
    }
}
