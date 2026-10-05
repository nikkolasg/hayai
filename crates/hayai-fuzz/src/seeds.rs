//! The seed blocks: valid blocks that the mutations start from.
//!
//! - A fixture seed is a generated block of hayai-bench with real signatures and real
//!   Orchard and Ironwood proofs, at its Mainnet height, on a parent with a history tree.
//! - A synthetic seed is a block with one coinbase at a chosen height of Mainnet or
//!   Testnet. Its coinbase pays the terms that hayai-consensus gives for the height, so
//!   the seed is also a test of these terms against the reference.

use std::sync::{Arc, OnceLock};

use hayai_bench::fixtures::{self, Fixture};
use hayai_consensus::coinbase::CoinbaseTerms;
use hayai_consensus::rules_at;
use hayai_prepared::RuleEpoch;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::context::{Coin, Context, Net, OutPoint};
use crate::model::{
    Block, Header, Tx, TxIn, TxOut, TxParts, COMMITMENTS_OFFSET, HEADER_PREFIX, MERKLE_OFFSET,
};
use crate::{hayai_side, reference};

/// A generated fixture block of hayai-bench.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FixtureId {
    /// NU6.2: 5 transparent transactions with 2 inputs each.
    Transparent,
    /// NU6.2: 3 transparent transactions and 2 Orchard transactions.
    Mixed,
    /// NU6.2: 2 Orchard transactions.
    Orchard,
    /// NU6.3: 2 Ironwood transactions, 1 Orchard transaction, 1 transaction with both.
    Nu63,
    /// [`FixtureId::Nu63`] with an Ironwood output in the coinbase.
    Nu63ShieldedCoinbase,
}

impl FixtureId {
    pub const ALL: [FixtureId; 5] = [
        FixtureId::Transparent,
        FixtureId::Mixed,
        FixtureId::Orchard,
        FixtureId::Nu63,
        FixtureId::Nu63ShieldedCoinbase,
    ];

    fn generate(self) -> Fixture {
        match self {
            FixtureId::Transparent => fixtures::transparent_block(5, 2),
            FixtureId::Mixed => fixtures::mixed_block(3, 1, 2, 2),
            FixtureId::Orchard => fixtures::orchard_block(2, 2),
            FixtureId::Nu63 => fixtures::nu6_3_block(2, 2, 1, 1, false),
            FixtureId::Nu63ShieldedCoinbase => fixtures::nu6_3_block(2, 2, 1, 1, true),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SeedSpec {
    Fixture(FixtureId),
    Synthetic { network: Net, height: u32 },
}

/// A valid block and the context that it is valid on.
#[derive(Clone, Debug)]
pub struct Seed {
    pub block: Block,
    pub ctx: Context,
}

/// Why a height has no synthetic seed.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("hayai has no rule set for the height: {0}")]
    NoRules(String),
    #[error("the height is at or below the mandatory checkpoint {0}")]
    BelowMandatoryCheckpoint(u32),
}

/// The script of the miner output of a synthetic coinbase: pay to a public key hash of
/// 20 bytes of 0x11.
pub fn miner_script() -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&[0x11; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

/// The script that pushes `height` as the coinbase of a block at that height does: the
/// shortest signed little-endian number, behind its length.
pub fn height_script(height: u32) -> Vec<u8> {
    match height {
        0 => vec![0x00],
        1..=16 => vec![0x50 + height as u8],
        _ => {
            let mut number: Vec<u8> = height.to_le_bytes().to_vec();
            while let Some(0) = number.last() {
                number.pop();
            }
            if matches!(number.last(), Some(top) if top & 0x80 != 0) {
                number.push(0);
            }
            let mut script = vec![number.len() as u8];
            script.extend_from_slice(&number);
            script
        }
    }
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// The merkle root and the commitments field that the header of `bytes` must have, as
/// the code of hayai computes them. `None`: hayai does not parse the block.
fn hayai_roots(bytes: &[u8], ctx: &Context) -> Option<([u8; 32], Option<[u8; 32]>)> {
    let rules = rules_at(hayai_side::network(ctx.network), ctx.height).ok()?;
    let block =
        hayai_wire::RawBlock::parse(bytes::Bytes::copy_from_slice(bytes), rules.branch_id).ok()?;
    let merkle = hayai_wire::merkle_root(&block.txids());
    let commitments = ctx.history_root.map(|root| {
        hayai_wire::block_commitments(&root, &hayai_wire::auth_data_root(&block.auth_digests()))
    });
    Some((merkle, commitments))
}

/// Writes into the header of `bytes` the merkle root of the transactions, when `merkle`
/// is set, and the commitments to the history root of `ctx` and to the authorizing data,
/// when `commitments` is set and the context has a history root.
///
/// The reference code computes both values. When the reference does not parse the block,
/// the code of hayai computes them: a block that only hayai parses then reaches the rules
/// of hayai behind the merkle root, and the case shows whether hayai accepts it. A block
/// that no implementation parses stays as it is.
pub fn fix_header(bytes: &mut [u8], ctx: &Context, merkle: bool, commitments: bool) {
    if bytes.len() < HEADER_PREFIX || !(merkle || commitments) {
        return;
    }
    let (merkle_root, commitments_hash) = match reference::merkle_root(bytes) {
        Some(root) => (
            root,
            ctx.history_root
                .and_then(|history_root| reference::block_commitments(bytes, history_root)),
        ),
        None => match hayai_roots(bytes, ctx) {
            Some(roots) => roots,
            None => return,
        },
    };
    if merkle {
        bytes[MERKLE_OFFSET..MERKLE_OFFSET + 32].copy_from_slice(&merkle_root);
    }
    if let (true, Some(hash)) = (commitments, commitments_hash) {
        bytes[COMMITMENTS_OFFSET..COMMITMENTS_OFFSET + 32].copy_from_slice(&hash);
    }
}

fn fixture_seed(id: FixtureId) -> Seed {
    let fixture = id.generate();
    let parsed = fixture.parse();
    let lengths: Vec<usize> = parsed.txs.iter().map(|tx| tx.bytes.len()).collect();
    let mut block = Block::parse(&fixture.bytes, &lengths);
    let coins = fixture
        .funding
        .iter()
        .map(|(outpoint, coin)| {
            (
                OutPoint {
                    hash: *outpoint.hash(),
                    index: outpoint.n(),
                },
                Coin {
                    value: coin.value,
                    script: coin.script_pubkey.0 .0.clone(),
                    height: coin.height,
                    coinbase: coin.is_coinbase,
                },
            )
        })
        .collect();
    let ctx = Context {
        network: Net::Mainnet,
        height: fixture.height,
        parent: block.header.prev,
        coins,
        nullifiers: Vec::new(),
        history_root: Some(hayai_side::history(fixture.branch_id, fixture.height).root()),
    };
    // The fixture header has a commitments field of zero. The seed commits to the history
    // tree of its context.
    let mut bytes = block.bytes();
    fix_header(&mut bytes, &ctx, false, true);
    block.header = Header::parse(&bytes).0;
    Seed { block, ctx }
}

fn synthetic_seed(network: Net, height: u32) -> Result<Seed, SeedError> {
    let hayai_network = hayai_side::network(network);
    let mandatory = hayai_network.mandatory_checkpoint_height();
    if height <= mandatory {
        return Err(SeedError::BelowMandatoryCheckpoint(mandatory));
    }
    let rules = rules_at(hayai_network, height).map_err(|e| SeedError::NoRules(e.to_string()))?;
    let terms =
        CoinbaseTerms::at(hayai_network, height).map_err(|e| SeedError::NoRules(e.to_string()))?;
    let nu5 = RuleEpoch::of(rules).nu5_active();
    let mut coinbase = TxParts::transparent(if nu5 { 5 } else { 4 }, u32::from(rules.branch_id));
    // From NU5 the expiry height of a coinbase is the height of its block.
    coinbase.expiry = if nu5 { height } else { 0 };
    coinbase.vin.push(TxIn {
        prev_hash: [0; 32],
        prev_index: u32::MAX,
        script: height_script(height),
        sequence: u32::MAX,
    });
    coinbase.vout.push(TxOut {
        value: terms.miner_subsidy(),
        script: miner_script(),
    });
    for required in &terms.required {
        coinbase.vout.push(TxOut {
            value: required.value,
            script: required.script.clone(),
        });
    }
    let parent = sha256(&[
        b"hayai-fuzz parent",
        &[network as u8],
        &height.to_le_bytes(),
    ]);
    let mut block = Block {
        header: Header {
            version: 4,
            prev: parent,
            merkle: [0; 32],
            commitments: [0; 32],
            time: 1_700_000_000,
            bits: 0x1f07_ffff,
            nonce: [0; 32],
            solution: vec![0; 1344],
        },
        txs: vec![Tx::Parts(coinbase)],
        stated_count: None,
        count_width: None,
    };
    let ctx = Context {
        network,
        height,
        parent,
        coins: Vec::new(),
        nullifiers: Vec::new(),
        history_root: None,
    };
    let mut bytes = block.bytes();
    fix_header(&mut bytes, &ctx, true, false);
    block.header = Header::parse(&bytes).0;
    Ok(Seed { block, ctx })
}

/// The fixture seeds, in the order of [`FixtureId::ALL`].
static FIXTURES: OnceLock<Vec<Arc<Seed>>> = OnceLock::new();

/// Builds the fixture seeds. A caller that is not a rayon worker calls it once before the
/// first case: the generation of a fixture uses the rayon pool, and a lock around it
/// would stop the pool.
pub fn build_fixtures() {
    FIXTURES.get_or_init(|| {
        FixtureId::ALL
            .into_iter()
            .map(|id| Arc::new(fixture_seed(id)))
            .collect()
    });
}

/// The seed of `spec`. [`build_fixtures`] must run before the first fixture seed.
pub fn seed(spec: SeedSpec) -> Result<Arc<Seed>, SeedError> {
    match spec {
        SeedSpec::Fixture(id) => {
            let fixtures = FIXTURES
                .get()
                .expect("build_fixtures runs before the first case");
            let index = FixtureId::ALL
                .iter()
                .position(|other| *other == id)
                .expect("ALL has every fixture");
            Ok(fixtures[index].clone())
        }
        SeedSpec::Synthetic { network, height } => synthetic_seed(network, height).map(Arc::new),
    }
}
