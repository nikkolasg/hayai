//! The verdict of hayai for a block: `hayai_validate::validate_bytes` on a chain that
//! holds the context of the case.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use hayai_coins::{CoinsBacking, FlushGeneration, Pool};
use hayai_consensus::{rules_at, Network};
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_prepared::{PrepareError, PreparedStore, RuleEpoch, VerifyingKeys};
use hayai_state::{Base, Chain, ContextError, HistoryLeaf, HistoryState};
use hayai_template::Zip317Params;
use hayai_validate::{validate_bytes, BlockError, HeaderPolicy, ValidateConfig};
use hayai_wire::header::BlockHash;

use crate::context::{Context, Net, ShieldedPool};
use crate::verdict::{RuleClass, Verdict};

/// The time of the parent block of every case. It differs from the time of the seed
/// headers, so a rule that reads the wrong one of the two gives another verdict.
pub const PARENT_TIME: u32 = 1_699_999_925;

/// Byte budget of the prepared store of a case. The store stays empty.
const STORE_LIMIT: usize = 1 << 20;

pub fn network(net: Net) -> Network {
    match net {
        Net::Mainnet => Network::Mainnet,
        Net::Testnet => Network::Testnet,
    }
}

/// The coins and the nullifiers of a context, as the store behind the chain of a case.
/// A case never writes to it.
struct ContextBacking {
    coins: HashMap<hayai_coins::OutPoint, hayai_coins::Coin>,
    nullifiers: HashSet<(usize, [u8; 32])>,
}

impl CoinsBacking for ContextBacking {
    fn get_many(
        &self,
        outpoints: &[hayai_coins::OutPoint],
    ) -> Result<Vec<Option<hayai_coins::Coin>>, hayai_coins::Error> {
        Ok(outpoints
            .iter()
            .map(|outpoint| self.coins.get(outpoint).cloned())
            .collect())
    }
    fn write_batch(
        &self,
        _: &[(&hayai_coins::OutPoint, &hayai_coins::Coin)],
        _: &[&hayai_coins::OutPoint],
    ) -> Result<(), hayai_coins::Error> {
        panic!("a case does not write to its context")
    }
    fn contains_many(
        &self,
        pool: Pool,
        nullifiers: &[[u8; 32]],
    ) -> Result<Vec<bool>, hayai_coins::Error> {
        Ok(nullifiers
            .iter()
            .map(|nullifier| self.nullifiers.contains(&(pool.index(), *nullifier)))
            .collect())
    }
    fn insert_many(&self, _: Pool, _: &[[u8; 32]]) -> Result<(), hayai_coins::Error> {
        panic!("a case does not write to its context")
    }
    fn write_generation(&self, _: &FlushGeneration) -> Result<(), hayai_coins::Error> {
        panic!("a case does not write to its context")
    }
}

/// The verifying keys of every Orchard circuit version, shared by all cases.
static KEYS: OnceLock<Arc<VerifyingKeys>> = OnceLock::new();

/// Builds the verifying keys of the upgrades from NU5 to NU6.3. A caller that is not a
/// rayon worker calls it once before the first case: the key build uses the rayon pool.
pub fn build_keys() {
    KEYS.get_or_init(|| {
        let keys = VerifyingKeys::prebuild(RuleEpoch::consensus(BranchId::Nu5), None);
        keys.ready();
        for branch in [BranchId::Nu6_2, BranchId::Nu6_3] {
            keys.prebuild_more(&[branch]);
            keys.ready();
        }
        keys
    });
}

/// The history tree of the parent of a block at `height` in the epoch of `branch`: three
/// generated leaves that end at `height - 1`. One tree for each pair, built on first use.
pub fn history(branch: BranchId, height: u32) -> Arc<HistoryState> {
    /// History trees by consensus branch id and height.
    type Trees = Mutex<HashMap<(u32, u32), Arc<HistoryState>>>;
    static TREES: OnceLock<Trees> = OnceLock::new();
    let trees = TREES.get_or_init(Default::default);
    let mut trees = trees.lock().expect("no holder of the lock panics");
    trees
        .entry((u32::from(branch), height))
        .or_insert_with(|| {
            let mut state = HistoryState::empty(branch);
            for leaf_height in height - 3..height {
                let leaf = HistoryLeaf {
                    hash: [leaf_height as u8; 32],
                    time: 1_699_990_000 + leaf_height % 1_000,
                    bits: 0x1c01_0000,
                    height: leaf_height,
                    sapling_root: [0x5a; 32],
                    orchard_root: [0x0a; 32],
                    ironwood_root: [0x1a; 32],
                    sapling_tx: 1,
                    orchard_tx: 2,
                    ironwood_tx: 3,
                };
                state = state
                    .append(branch, &leaf)
                    .expect("generated leaves follow each other");
            }
            Arc::new(state)
        })
        .clone()
}

fn prepare_class(error: &PrepareError) -> RuleClass {
    use PrepareError::*;
    match error {
        Version(..) | BranchId { .. } | PoolNotActive(..) => RuleClass::TxVersion,
        NoSource
        | NoSink
        | CoinbaseShieldedSpend
        | CoinbaseJoinSplit
        | JoinSplitBothVpub(_)
        | SproutPoolDeposit(_)
        | V4ValueBalance(_)
        | OrchardFlags
        | IronwoodFlags
        | OrchardCrossAddress
        | OrchardPoolDeposit(_)
        | CoinbaseOrchardBundle => RuleClass::TxStructure,
        CoinbaseShieldedOutput(_) => RuleClass::CoinbaseTerms,
        CoinbaseScriptLength(_) | NullPrevout(_) => RuleClass::CoinbaseForm,
        DuplicateInput(_) | MissingInput(_) | SpentCoins { .. } => RuleClass::TransparentInput,
        ValueOverflow | NegativeFee => RuleClass::Value,
        Script(..) => RuleClass::Script,
        DuplicateNullifier(_) => RuleClass::Nullifier,
        ExpiryTooHigh(_) => RuleClass::TxTime,
        Unsupported(_) => RuleClass::Unsupported,
    }
}

fn context_class(error: &ContextError) -> RuleClass {
    use ContextError::*;
    match error {
        WrongParent { .. } => RuleClass::Header,
        NoCoinbase | ExtraCoinbase(_) | CoinbaseHeight => RuleClass::CoinbaseForm,
        DuplicateTxid(_) => RuleClass::Merkle,
        MissingInput { .. }
        | DoubleSpend { .. }
        | SpentMismatch { .. }
        | ImmatureCoinbase { .. }
        | UnshieldedCoinbaseSpend { .. } => RuleClass::TransparentInput,
        DuplicateNullifier { .. } => RuleClass::Nullifier,
        BadAnchor { .. } => RuleClass::Anchor,
        Expired { .. } | CoinbaseExpiry { .. } | NotFinal(_) => RuleClass::TxTime,
        TooManySigops(_)
        | TooManyOrchardActions(_)
        | TooManyIronwoodActions(_)
        | TooManySaplingIos(_)
        | ShieldedCostAboveBudget(_) => RuleClass::Limits,
        PoolNotActive { .. } => RuleClass::TxVersion,
        // The deferred pool is a chain value pool.
        Coinbase(hayai_consensus::coinbase::CoinbaseError::NegativeDeferredPool { .. }) => {
            RuleClass::ValuePool
        }
        Coinbase(_) => RuleClass::CoinbaseTerms,
        NegativeValuePool(_) | NegativeTransparentPool => RuleClass::ValuePool,
        ValueOverflow => RuleClass::Value,
        BlockCommitments => RuleClass::Commitments,
        History(_) | Tree(_) => RuleClass::History,
        SproutStateUnknown { .. } => RuleClass::Unsupported,
    }
}

fn block_class(error: &BlockError) -> RuleClass {
    match error {
        BlockError::Parse(_) => RuleClass::Parse,
        BlockError::Header(_) | BlockError::HeaderContext(_) => RuleClass::Header,
        BlockError::MerkleRoot => RuleClass::Merkle,
        BlockError::Prepare { error, .. } => prepare_class(error),
        BlockError::Shielded(_) => RuleClass::ShieldedProof,
        BlockError::Context(error) => context_class(error),
        BlockError::BelowMandatoryCheckpoint { .. } => RuleClass::Unsupported,
        BlockError::AboveLastCheckpoint { .. } | BlockError::NotOnCheckpointedChain { .. } => {
            RuleClass::Other
        }
    }
}

/// The verdict of hayai for the block `bytes` on the chain of `ctx`. A panic of hayai is
/// [`Verdict::Panic`].
pub fn check_block(bytes: &[u8], ctx: &Context) -> Verdict {
    match crate::guarded(|| run(bytes, ctx)) {
        Ok(verdict) => verdict,
        Err(panic) => Verdict::Panic(panic),
    }
}

fn run(bytes: &[u8], ctx: &Context) -> Verdict {
    let network = network(ctx.network);
    let rules = match rules_at(network, ctx.height) {
        Ok(rules) => *rules,
        Err(e) => return Verdict::reject(RuleClass::Unsupported, e),
    };
    let backing = ContextBacking {
        coins: ctx
            .coins
            .iter()
            .map(|(outpoint, coin)| {
                (
                    hayai_coins::OutPoint::new(outpoint.hash, outpoint.index),
                    hayai_coins::Coin {
                        value: coin.value,
                        script_pubkey: Bytes::copy_from_slice(&coin.script),
                        height: coin.height,
                        is_coinbase: coin.coinbase,
                    },
                )
            })
            .collect(),
        nullifiers: ctx
            .nullifiers
            .iter()
            .map(|(pool, nullifier)| {
                let pool = match pool {
                    ShieldedPool::Orchard => Pool::Orchard,
                    ShieldedPool::Ironwood => Pool::Ironwood,
                };
                (pool.index(), *nullifier)
            })
            .collect(),
    };
    let mut base = Base::new(
        Arc::new(backing),
        ctx.height - 1,
        BlockHash(ctx.parent),
        PARENT_TIME,
    );
    // The transparent pool holds the coins of the context: the block spends them, and no
    // chain value pool can be negative after the block.
    base.value_pools.transparent = ctx.coins.iter().map(|(_, coin)| coin.value).sum();
    base.value_pools.deferred = crate::context::DEFERRED_POOL;
    if let Some(root) = ctx.history_root {
        let tree = history(rules.branch_id, ctx.height);
        assert_eq!(tree.root(), root, "the context has the root of the tree");
        base.history = Some(tree);
    }
    let chain = Chain::new(base);
    let store = PreparedStore::new(RuleEpoch::of(&rules), STORE_LIMIT, Zip317Params::ZIP317);
    let cfg = ValidateConfig {
        network,
        rules,
        keys: KEYS
            .get()
            .expect("build_keys runs before the first case")
            .clone(),
        // The blocks of the cases have generated headers without proof of work.
        header: HeaderPolicy::GeneratedBlocks,
    };
    match validate_bytes(Bytes::copy_from_slice(bytes), &store, &chain.view(), &cfg) {
        Ok(_) => Verdict::Accept,
        Err(error) => Verdict::reject(block_class(&error), error),
    }
}
