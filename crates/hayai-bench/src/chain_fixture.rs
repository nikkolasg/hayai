//! A chain, prepared store and validation config built around one fixture block, shared by
//! the prepared/state/validate tests and benches.
//!
//! The chain's base sits at `FIXTURE_HEIGHT - 1` with the fixture header's `prev_hash` as
//! tip, its coins cache seeded with the fixture's funding set, so that the fixture block is
//! the next block of that chain.

use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{Coin, CoinsView, Config, RocksBacking};
use hayai_consensus::RuleSet;
use hayai_mempool::PreparedStore;
use hayai_prepared::{prepare, PreparedTx, RuleEpoch, ScopedBatch, VerifyingKeys};
use hayai_state::{Base, Chain, HistoryLeaf, HistoryState};
use hayai_template::Zip317Params;
use hayai_validate::{HeaderPolicy, ValidateConfig};
use hayai_wire::RawBlock;

use crate::scenarios::state::{block_hash as synthetic_block_hash, layer as synthetic_layer};
use crate::zakura_chain_clone::{BlockEntries, BlockShape};
pub use hayai_fixtures::FIXTURE_NETWORK;
use hayai_fixtures::{Fixture, FundingCoin, FIXTURE_BRANCH, NU6_3_FIXTURE_BRANCH};

pub fn coin(funding: &FundingCoin) -> Coin {
    Coin {
        value: funding.value,
        script_pubkey: Bytes::copy_from_slice(&funding.script_pubkey.0 .0),
        height: funding.height,
        is_coinbase: funding.is_coinbase,
    }
}

/// Byte budget of the prepared store: a full block several times over.
const STORE_LIMIT: usize = 64 << 20;

pub struct Harness {
    pub block: RawBlock,
    pub chain: Chain,
    pub store: PreparedStore,
    pub cfg: ValidateConfig,
    _dir: tempfile::TempDir,
}

/// The verifying keys shared by every harness of a process (an Orchard key takes seconds
/// to build).
///
/// The key of the fixture epoch is built on its own thread before any validation uses it
/// (`VerifyingKeys::prebuild`, then `ready`). A validation never builds a key
/// (`tests/cold_keys.rs`).
pub fn keys() -> Arc<VerifyingKeys> {
    static KEYS: std::sync::OnceLock<Arc<VerifyingKeys>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let keys = VerifyingKeys::prebuild(RuleEpoch::consensus(FIXTURE_BRANCH), None);
        keys.ready();
        keys
    })
    .clone()
}

/// The verifying keys of the NU6.3 fixtures: the key of the NU6.3 circuit, which verifies
/// the Orchard and the Ironwood bundles. Built as [`keys`] builds the key of NU6.2.
pub fn keys_nu6_3() -> Arc<VerifyingKeys> {
    static KEYS: std::sync::OnceLock<Arc<VerifyingKeys>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let keys = VerifyingKeys::prebuild(RuleEpoch::consensus(NU6_3_FIXTURE_BRANCH), None);
        keys.ready();
        keys
    })
    .clone()
}

/// A harness whose chain holds no layer: the fixture block is the next block of the base.
pub fn harness(fixture: &Fixture) -> Harness {
    chain_with_layers(fixture, 0, BlockShape::TYPICAL)
}

/// A harness whose chain holds `n_layers` synthetic layers of `shape` (typically
/// [`BlockShape::TYPICAL`]) between the base and the fixture block. The base sits `n_layers + 1` blocks below the fixture and seeds the
/// funding set; each layer creates the coins the next one spends and reveals its own
/// nullifiers, so a lookup of a fixture input probes every layer before it reaches the base.
pub fn chain_with_layers(fixture: &Fixture, n_layers: usize, shape: BlockShape) -> Harness {
    let block = fixture.parse();
    let dir = crate::scratch_dir();
    let backing = Arc::new(
        RocksBacking::open(
            dir.path(),
            &Config {
                block_cache_bytes: 64 << 20,
                ..Config::default()
            },
        )
        .expect("open rocks"),
    );
    let layers = u32::try_from(n_layers).expect("layer count fits");
    let base_height = fixture.height - 1 - layers;
    let base_hash = if n_layers == 0 {
        block.header.prev_hash
    } else {
        synthetic_block_hash(base_height)
    };
    let mut base = Base::new(
        backing,
        base_height,
        base_hash,
        block.header.time - 75 * (layers + 1),
    );
    for (outpoint, funding) in &fixture.funding {
        base.coins
            .add(outpoint.clone(), coin(funding))
            .expect("funding outpoints are distinct");
    }
    // The transparent pool holds the funding coins: the fixture block spends them, and no
    // chain value pool can be negative after the block.
    base.value_pools.transparent = fixture.funding.iter().map(|(_, f)| f.value).sum();
    let mut chain = Chain::new(base);
    for height in base_height + 1..=fixture.height - 1 {
        let entries = BlockEntries::synthetic(height, shape);
        let mut l = synthetic_layer(&entries, &chain);
        if height == fixture.height - 1 {
            l.hash = block.header.prev_hash;
        }
        chain.push(l).expect("synthetic layers chain");
    }
    // The fixture heights are Mainnet heights of the fixture branches.
    let rules = *hayai_consensus::rules_at(FIXTURE_NETWORK, fixture.height)
        .expect("the fixture height has a rule set");
    assert_eq!(
        Ok(&rules),
        RuleSet::of_branch(fixture.branch_id),
        "the rule set of the fixture height is the rule set of the fixture branch"
    );
    let epoch = RuleEpoch::of(&rules);
    Harness {
        block,
        chain,
        store: PreparedStore::new(epoch, STORE_LIMIT, Zip317Params::ZAKURA),
        cfg: ValidateConfig {
            network: FIXTURE_NETWORK,
            rules,
            keys: match fixture.branch_id {
                NU6_3_FIXTURE_BRANCH => keys_nu6_3(),
                _ => keys(),
            },
            // Fixture blocks have generated headers without proof of work.
            header: HeaderPolicy::GeneratedBlocks,
        },
        _dir: dir,
    }
}

/// Leaves of the history tree that [`harness_with_history`] seeds: 2^12 - 1, so the append
/// of the fixture block merges twelve peaks, the most a tree of this size can.
const HISTORY_LEAVES: u32 = 4_095;

/// The history tree after the base block of a zero-layer harness: [`HISTORY_LEAVES`]
/// synthetic leaves of the fixture's upgrade, ending at `FIXTURE_HEIGHT - 1`.
fn history_seed(fixture: &Fixture) -> Arc<HistoryState> {
    let first = fixture.height - HISTORY_LEAVES;
    let mut state = HistoryState::empty(fixture.branch_id);
    for height in first..fixture.height {
        let leaf = HistoryLeaf {
            hash: synthetic_block_hash(height).0,
            time: 1_700_000_000 + height * 75,
            bits: 0x1c01_0000,
            height,
            sapling_root: [0x5a; 32],
            orchard_root: [0x0a; 32],
            ironwood_root: [0x1a; 32],
            sapling_tx: u64::from(height % 3),
            orchard_tx: u64::from(height % 5),
            ironwood_tx: u64::from(height % 7),
        };
        state = state
            .append(fixture.branch_id, &leaf)
            .expect("synthetic leaves follow each other");
    }
    Arc::new(state)
}

/// [`harness`] with a known history tree: the base holds a seeded tree, and the fixture
/// header's `hashBlockCommitments` is rewritten to commit to it, so validation runs the
/// commitment rule and the history append, and the layer carries the tree after the block.
pub fn harness_with_history(fixture: &Fixture) -> Harness {
    let mut h = harness(fixture);
    let seed = history_seed(fixture);
    h.block.header.block_commitments = hayai_wire::block_commitments(
        &seed.root(),
        &hayai_wire::auth_data_root(&h.block.auth_digests()),
    );
    h.chain.base().write().history = Some(seed);
    h
}

impl Harness {
    /// Prepares every non-coinbase transaction of the block the way a mempool would: one
    /// at a time against the chain view, one shielded batch for the round.
    pub fn prepare_all(&self) -> Vec<Arc<PreparedTx>> {
        let view = self.chain.view();
        let mut batch = ScopedBatch::new(&self.cfg.keys);
        let mut prepared: Vec<PreparedTx> = self.block.txs[1..]
            .iter()
            .map(|raw| {
                prepare(
                    raw.clone(),
                    self.cfg.epoch(),
                    &view as &dyn CoinsView,
                    &mut batch,
                )
                .expect("fixture transactions are valid")
            })
            .collect();
        let outcome = batch.finalize();
        assert!(outcome.failed.is_empty(), "fixture bundles are valid");
        for p in &mut prepared {
            if p.has_shielded() {
                p.set_shielded_ok();
            }
        }
        prepared.into_iter().map(Arc::new).collect()
    }

    /// Fills the store with every non-coinbase transaction.
    pub fn fill_store(&self) {
        for p in self.prepare_all() {
            self.store
                .insert(p)
                .expect("fixture transactions do not conflict");
        }
    }
}
