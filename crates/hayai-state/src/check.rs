//! Contextual validation of a prepared block against a chain view.
//!
//! Every rule here depends on the chain: existence and unspentness of inputs, coinbase
//! maturity, nullifier uniqueness, anchor validity, expiry and lock time, block totals, the
//! pools of the height, the coinbase terms, the chain value pools, and the header commitment
//! to the ZIP 221 history tree of the parent (`crate::history`). The context-free rules ran
//! in hayai-prepared. Reads are batched: one coin round for every input not created in the
//! block ([`resolve_inputs`], which the validator runs once for the drafts and the check),
//! one nullifier round per pool.
//!
//! The coinbase terms come from `hayai_consensus::coinbase::CoinbaseTerms` for the network
//! and the height: the required outputs (founders' reward, funding streams, lockbox
//! disbursement), the value rule (ZIP 236 from NU6) and the change of the deferred pool.
//!
//! The chain value pools are the transparent, Sprout, Sapling, Orchard, Ironwood and
//! deferred pools. No pool can be negative after a block, and their total is at most
//! `MAX_MONEY` (Zakura `ValueBalance::add_chain_value_pool_change`,
//! `zakura-chain/src/value_balance.rs:360-376`). From the block before NU7 their total also
//! has the NSM rules (`hayai_consensus::nsm::check_balance`), and from the NSM reissuance
//! height the coinbase terms depend on the total after the parent.
//!
//! The Sprout rules: the nullifiers of a JoinSplit are in the Sprout nullifier set; the
//! anchor of a JoinSplit is the final Sprout treestate of an earlier block or the output
//! treestate of an earlier JoinSplit of the same transaction (Zakura
//! `sprout_anchors_refer_to_treestates`, `zakura-state/src/service/check/anchors.rs:230`);
//! the two commitments of each JoinSplit go to the Sprout tree in block order; `vpub_old`
//! enters the Sprout pool and `vpub_new` leaves it. No header commits to the Sprout root.
//!
//! The Ironwood pool (NU6.3) has the rules of the Orchard pool: its own nullifier set, its
//! own tree and anchors, its own value pool (Zakura `zakura-state/src/service/check/
//! nullifier.rs`, `anchors.rs`, `zakura-chain/src/value_balance.rs`).
//!
//! Zcash rule sources: zcashd `ContextualCheckBlock`, `ContextualCheckTransaction`,
//! `ConnectBlock` and `IsFinalTx`; the protocol specification §3.5 to §3.7 (anchors must
//! refer to some earlier block's final treestate, so a root produced inside this block is
//! not valid for its own transactions).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_coins::{Coin, CoinsView, OutPoint, Pool};
use hayai_consensus::coinbase::{CoinbaseError, CoinbaseOutput, CoinbaseTerms, ShieldedBalances};
use hayai_consensus::{
    nsm, BlockLimits, Network, RuleSet, ShieldedPools, COINBASE_MATURITY, LOCKTIME_THRESHOLD,
};
use hayai_crypto::{zcash_primitives, zcash_protocol, zcash_script};
use hayai_prepared::{Commitments, PreparedTx};
use hayai_trees::{IronwoodFrontier, OrchardFrontier, SaplingFrontier, SproutFrontier, TreeError};
use hayai_wire::header::BlockHash;
use hayai_wire::{RawBlock, RawTx, WtxId};
use zcash_primitives::transaction::components::sprout;
use zcash_primitives::transaction::TxId;
use zcash_protocol::value::MAX_MONEY;
use zcash_script::pattern::push_num;
use zcash_script::Opcode;

use crate::history::{
    header_commitment, history_after, HeaderCommitment, HistoryError, HistoryLeaf, HistoryState,
};
use crate::{Anchors, ChainView, Frontiers, Layer, Map, Set, ValuePools};

mod checkpoint;

pub use checkpoint::checkpoint_layer;

/// A block whose transactions are all prepared, in block order.
pub struct PreparedBlock {
    pub raw: RawBlock,
    pub txs: Vec<Arc<PreparedTx>>,
    /// ZIP 244 root of the authorizing data of `raw`, input of the `hashBlockCommitments`
    /// rule. [`PreparedBlock::new`] computes it; the validator passes the root of its
    /// roots stage.
    pub auth_data_root: [u8; 32],
}

impl PreparedBlock {
    pub fn new(raw: RawBlock, txs: Vec<Arc<PreparedTx>>) -> Self {
        let auth_data_root = hayai_wire::auth_data_root(&raw.auth_digests());
        Self {
            raw,
            txs,
            auth_data_root,
        }
    }
}

/// What a contextual check needs besides the block and the view.
pub struct CheckConfig<'a> {
    /// The network: with the height of the block it gives the coinbase terms.
    pub network: Network,
    /// The rule set of the block's height (`hayai_consensus::rules_at`).
    pub rules: &'a RuleSet,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ContextTimings {
    /// Every rule except the tree appends.
    pub context: Duration,
    /// Note commitment tree appends and roots.
    pub trees: Duration,
    /// The header commitment rule and the history tree append.
    pub history: Duration,
}

pub struct Checked {
    pub layer: Layer,
    pub timings: ContextTimings,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("block parent {found} is not the tip {expected}")]
    WrongParent {
        expected: hayai_wire::header::BlockHash,
        found: hayai_wire::header::BlockHash,
    },
    #[error("first transaction is not a coinbase")]
    NoCoinbase,
    #[error("transaction {0} is a second coinbase")]
    ExtraCoinbase(usize),
    #[error("coinbase scriptSig does not start with the block height")]
    CoinbaseHeight,
    /// The block holds a txid twice. Such a body can have the merkle root of a valid block
    /// (`hayai_wire::duplicate_txid`): the body is at fault, not the header.
    #[error("duplicate txid {0}")]
    DuplicateTxid(TxId),
    #[error("transaction {tx} input {input} spends a coin that does not exist or was spent")]
    MissingInput { tx: usize, input: usize },
    #[error("transaction {tx} input {input} is spent twice in the block")]
    DoubleSpend { tx: usize, input: usize },
    #[error("transaction {tx} input {input} was prepared against a different coin")]
    SpentMismatch { tx: usize, input: usize },
    #[error("transaction {tx} input {input} spends a coinbase from height {created} at {height}")]
    ImmatureCoinbase {
        tx: usize,
        input: usize,
        created: u32,
        height: u32,
    },
    #[error("transaction {tx} input {input} spends a coinbase and has transparent outputs")]
    UnshieldedCoinbaseSpend { tx: usize, input: usize },
    #[error("transaction {tx} reveals a {pool} nullifier already revealed")]
    DuplicateNullifier { pool: Pool, tx: usize },
    #[error("transaction {tx} uses a {pool} anchor that is not an earlier block's treestate")]
    BadAnchor { pool: Pool, tx: usize },
    #[error("transaction {tx} expired at height {expiry}")]
    Expired { tx: usize, expiry: u32 },
    #[error("coinbase expiry height {found} is not the block height {expected}")]
    CoinbaseExpiry { expected: u32, found: u32 },
    #[error("transaction {0} is not final")]
    NotFinal(usize),
    #[error("block has {0} sigops")]
    TooManySigops(u32),
    #[error("block has {0} Orchard actions")]
    TooManyOrchardActions(u32),
    #[error("block has {0} Ironwood actions")]
    TooManyIronwoodActions(u32),
    #[error("block has {0} Sapling spends and outputs")]
    TooManySaplingIos(u32),
    /// ZIP 218, `GlobalShieldedBudget`: the Orchard actions, the Ironwood actions and the
    /// Sapling spends and outputs of the block together.
    #[error("block has a shielded cost of {0}")]
    ShieldedCostAboveBudget(u32),
    #[error("transaction {tx} has a {pool} bundle, and the {pool} pool is off at this height")]
    PoolNotActive { pool: Pool, tx: usize },
    /// The coinbase breaks its terms: a required output, the value rule or the deferred
    /// pool.
    #[error("coinbase: {0}")]
    Coinbase(#[from] CoinbaseError),
    #[error("{0} value pool would be negative")]
    NegativeValuePool(Pool),
    #[error("transparent value pool would be negative")]
    NegativeTransparentPool,
    #[error("value overflow")]
    ValueOverflow,
    #[error("tree: {0}")]
    Tree(String),
    #[error("the header commitment does not match the parent's history tree")]
    BlockCommitments,
    #[error("history tree: {0}")]
    History(#[from] HistoryError),
    /// The base of the view does not know the Sprout state
    /// (`Base::set_sprout_unknown`), and the block changes it.
    #[error("transaction {tx} has a JoinSplit, and the node does not know the Sprout state")]
    SproutStateUnknown { tx: usize },
}

impl From<TreeError> for ContextError {
    fn from(e: TreeError) -> Self {
        ContextError::Tree(e.to_string())
    }
}

/// `CScript() << height`: the push the coinbase scriptSig must start with.
fn height_push(height: u32) -> Vec<u8> {
    Vec::from(&Opcode::PushValue(push_num(i64::from(height))))
}

/// zcashd `IsFinalTx` with the block's own height and time (block validation does not use
/// the median-time-past; `nLockTimeFlags` is zero in `ContextualCheckBlock`), split by what
/// it needs: a transaction whose lock time is a time and whose inputs are not all final is
/// final only when the block time is after its lock time.
enum Finality {
    Final,
    NotFinal,
    TimeAfter(u32),
}

fn finality(tx: &PreparedTx, height: u32) -> Finality {
    if tx.lock_time == 0 {
        return Finality::Final;
    }
    let all_final = || {
        tx.raw
            .tx
            .transparent_bundle()
            .into_iter()
            .flat_map(|b| &b.vin)
            .all(|txin| txin.sequence() == u32::MAX)
    };
    if tx.lock_time < LOCKTIME_THRESHOLD {
        if tx.lock_time < height || all_final() {
            return Finality::Final;
        }
        return Finality::NotFinal;
    }
    if all_final() {
        return Finality::Final;
    }
    Finality::TimeAfter(tx.lock_time)
}

fn checked_sum<I: Iterator<Item = u64>>(values: I) -> Result<u64, ContextError> {
    let mut total = 0u64;
    for v in values {
        total = total.checked_add(v).ok_or(ContextError::ValueOverflow)?;
        if total > MAX_MONEY {
            return Err(ContextError::ValueOverflow);
        }
    }
    Ok(total)
}

/// Every transparent output `raw` creates when connected at `height`, keyed by outpoint:
/// the block's own coins, which its later transactions may spend and which become the
/// layer's `created` map. The coinbase is the first transaction.
pub fn block_outputs(raw: &RawBlock, height: u32) -> Map<OutPoint, Coin> {
    let txs: Vec<&RawTx> = raw.txs.iter().collect();
    outputs_of(&txs, true, height)
}

/// The transparent outputs of `txs` at `height`; the first one is a coinbase when
/// `coinbase_first`.
fn outputs_of(txs: &[&RawTx], coinbase_first: bool, height: u32) -> Map<OutPoint, Coin> {
    let outputs: usize = txs
        .iter()
        .filter_map(|t| t.tx.transparent_bundle())
        .map(|b| b.vout.len())
        .sum();
    let mut created = Map::with_capacity_and_hasher(outputs, Default::default());
    for (i, t) in txs.iter().enumerate() {
        let Some(bundle) = t.tx.transparent_bundle() else {
            continue;
        };
        for (n, out) in bundle.vout.iter().enumerate() {
            created.insert(
                OutPoint::new(*t.txid.as_ref(), n as u32),
                Coin {
                    value: out.value().into_u64(),
                    script_pubkey: Bytes::copy_from_slice(&out.script_pubkey().0 .0),
                    height,
                    is_coinbase: coinbase_first && i == 0,
                },
            );
        }
    }
    created
}

/// The coin every transparent input of `raw` spends, in one view round: `inputs[i][j]` is
/// the coin of input `j` of transaction `i`, from the block's own outputs (`created`, see
/// [`block_outputs`]) or from `view`. The coinbase and transactions without transparent
/// inputs get an empty list. An input that neither source resolves is
/// [`ContextError::MissingInput`]. In-block parent order is not checked here; the
/// contextual check rejects a spend of a later transaction's output.
pub fn resolve_inputs(
    view: &ChainView,
    raw: &RawBlock,
    created: &Map<OutPoint, Coin>,
) -> Result<Vec<Vec<Coin>>, ContextError> {
    let txs: Vec<&RawTx> = raw.txs.iter().collect();
    inputs_of(view, &txs, 0, created)
}

/// [`resolve_inputs`] for `txs`, the transactions of a block from position `first` on.
fn inputs_of(
    view: &ChainView,
    txs: &[&RawTx],
    first: usize,
    created: &Map<OutPoint, Coin>,
) -> Result<Vec<Vec<Coin>>, ContextError> {
    // Slot filler for the inputs the view round resolves below.
    let placeholder = Coin {
        value: 0,
        script_pubkey: Bytes::new(),
        height: 0,
        is_coinbase: false,
    };
    let mut inputs: Vec<Vec<Coin>> = Vec::with_capacity(txs.len());
    let mut pending: Vec<(usize, usize)> = Vec::new();
    let mut outpoints: Vec<OutPoint> = Vec::new();
    for (i, t) in txs.iter().enumerate() {
        let coins = match t.tx.transparent_bundle() {
            Some(b) if !b.is_coinbase() => b
                .vin
                .iter()
                .enumerate()
                .map(|(j, txin)| match created.get(txin.prevout()) {
                    Some(coin) => coin.clone(),
                    None => {
                        pending.push((i, j));
                        outpoints.push(txin.prevout().clone());
                        placeholder.clone()
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        inputs.push(coins);
    }
    for ((i, j), coin) in pending.into_iter().zip(view.get_coins(&outpoints)) {
        let Some(coin) = coin else {
            return Err(ContextError::MissingInput {
                tx: first + i,
                input: j,
            });
        };
        inputs[i][j] = coin;
    }
    Ok(inputs)
}

/// The first rule: `raw` extends the view's tip. Returns the height the block will have.
/// The validator runs it before it reads the block's inputs, so a block for another tip
/// costs no state round and no cryptography.
///
/// Spec §7.6: `hashPrevBlock` names the parent; the parent is the tip, so the height is
/// the height of the parent plus 1.
pub fn check_parent(view: &ChainView, raw: &RawBlock) -> Result<u32, ContextError> {
    let tip = view.tip();
    if raw.header.prev_hash != tip.hash {
        return Err(ContextError::WrongParent {
            expected: tip.hash,
            found: raw.header.prev_hash,
        });
    }
    Ok(tip.height + 1)
}

/// Applies every contextual rule to `block` on top of `view` and builds its layer.
pub fn contextual_check(
    view: &ChainView,
    block: &PreparedBlock,
    cfg: &CheckConfig<'_>,
) -> Result<Checked, ContextError> {
    let height = check_parent(view, &block.raw)?;
    let created = block_outputs(&block.raw, height);
    let inputs = resolve_inputs(view, &block.raw, &created)?;
    contextual_check_with_outputs(view, block, created, inputs, cfg)
}

/// The coinbase rules that need only the coinbase and the height: placement, the height
/// commitment of the scriptSig, and the expiry height from NU5.
///
/// Spec §7.1.2: the coinbase script starts with the height (BIP 34 encoding). ZIP 203: from
/// NU5 the coinbase expiry height is the block height.
fn check_coinbase(
    coinbase: Option<&Arc<PreparedTx>>,
    height: u32,
    rules: &RuleSet,
) -> Result<(), ContextError> {
    // Spec §7.6: the first transaction is a coinbase.
    let Some(coinbase) = coinbase.filter(|t| t.is_coinbase) else {
        return Err(ContextError::NoCoinbase);
    };
    let Some(script_sig) = coinbase
        .raw
        .tx
        .transparent_bundle()
        .and_then(|b| b.vin.first())
        .map(|i| &i.script_sig().0 .0)
    else {
        return Err(ContextError::NoCoinbase);
    };
    if !script_sig.starts_with(&height_push(height)) {
        return Err(ContextError::CoinbaseHeight);
    }
    if rules.coinbase.expiry_is_height && coinbase.expiry_height != height {
        return Err(ContextError::CoinbaseExpiry {
            expected: height,
            found: coinbase.expiry_height,
        });
    }
    Ok(())
}

/// What the per-transaction rules of a run of transactions produce.
struct TxsChecked {
    spent: Set<OutPoint>,
    nullifiers: [Set<[u8; 32]>; 4],
    totals: Totals,
    /// The largest lock time that only the block time can satisfy, with the position of
    /// its transaction; `None` when no transaction needs the block time. Set only when the
    /// block time is not known yet.
    time_lock: Option<(u32, usize)>,
}

/// Block totals of a run of transactions.
#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    sigops: u32,
    orchard_actions: u32,
    ironwood_actions: u32,
    sapling_ios: u32,
    fees: u64,
    /// The change of the transparent pool: the value of the outputs minus the value of the
    /// spent coins.
    transparent_change: i128,
    /// The value that leaves the Sprout pool: `vpub_new` minus `vpub_old` of every
    /// JoinSplit.
    sprout_balance: i128,
    sapling_balance: i128,
    orchard_balance: i128,
    ironwood_balance: i128,
}

/// The per-transaction rules of `txs`, the transactions of a block from position `first`
/// on: double spends, in-block parent order and each input against the view's coin;
/// coinbase maturity, and on a network with `coinbase_must_be_shielded` no transparent
/// output in a transaction that spends a coinbase;
/// nullifier uniqueness in the block and against the view; anchors; expiry; finality;
/// the pools of the height; the limits; the fees and the value balances. `positions` maps
/// the txid of every transaction of the block (or of the body being prebuilt) to its
/// position, and
/// `created` holds their outputs. With `block_time` absent, a transaction whose lock time
/// is a time and whose inputs are not all final is recorded in
/// [`TxsChecked::time_lock`] instead of being judged.
#[allow(clippy::too_many_arguments)]
fn check_txs(
    view: &ChainView,
    txs: &[Arc<PreparedTx>],
    first: usize,
    created: &Map<OutPoint, Coin>,
    positions: &Map<TxId, usize>,
    inputs: &[Vec<Coin>],
    height: u32,
    block_time: Option<u32>,
    cfg: &CheckConfig<'_>,
) -> Result<TxsChecked, ContextError> {
    let rules = cfg.rules;
    let must_shield = cfg.network.core().coinbase_must_be_shielded;
    // Inputs: double spends inside the block, in-block parents (which must come earlier in
    // the block), then each input against the coin the view resolved for it.
    assert_eq!(inputs.len(), txs.len(), "one input list per transaction");
    let input_count: usize = txs.iter().map(|t| t.spent.len()).sum();
    let mut spent: Set<OutPoint> = Set::with_capacity_and_hasher(input_count, Default::default());
    // Spec §7.1.2: each prevout is a unique unspent output of an earlier block or of an
    // earlier transaction of this block.
    for (k, tx) in txs.iter().enumerate() {
        if tx.is_coinbase {
            continue;
        }
        let i = first + k;
        assert_eq!(
            inputs[k].len(),
            tx.spent.len(),
            "one resolved coin per input"
        );
        for (j, outpoint) in tx.spent_outpoints().enumerate() {
            if !spent.insert(outpoint.clone()) {
                return Err(ContextError::DoubleSpend { tx: i, input: j });
            }
            if created.contains_key(outpoint) {
                let Some(&parent) = positions.get(&TxId::from_bytes(*outpoint.hash())) else {
                    unreachable!("an in-block output has a creating transaction");
                };
                if parent >= i {
                    return Err(ContextError::MissingInput { tx: i, input: j });
                }
            }
            let coin = &inputs[k][j];
            let prepared = &tx.spent[j];
            if prepared.value != coin.value || prepared.script_pubkey != coin.script_pubkey {
                return Err(ContextError::SpentMismatch { tx: i, input: j });
            }
            if !coin.is_coinbase {
                continue;
            }
            // Spec §7.1.2: no spend of a coinbase output less than 100 blocks old.
            if height < coin.height.saturating_add(COINBASE_MATURITY) {
                return Err(ContextError::ImmatureCoinbase {
                    tx: i,
                    input: j,
                    created: coin.height,
                    height,
                });
            }
            // Spec §7.1.2: a transaction that spends a coinbase output has no transparent
            // output. zcashd `bad-txns-coinbase-spend-has-transparent-outputs` (coinbase outputs
            // must be shielded on Mainnet and Testnet); Zakura `DisallowCoinbaseSpend`,
            // which Regtest does not have (`zakura-chain/src/transaction.rs:552-564`).
            let bundle = tx.raw.tx.transparent_bundle();
            if must_shield && matches!(bundle, Some(b) if !b.vout.is_empty()) {
                return Err(ContextError::UnshieldedCoinbaseSpend { tx: i, input: j });
            }
        }
    }

    // Nullifiers: unique within the block, then absent from the view, one round per pool.
    // Spec §3.9: a nullifier never repeats in the chain; each pool has its own set.
    let mut nullifiers: [Set<[u8; 32]>; 4] = Default::default();
    let mut owners: [Vec<usize>; 4] = Default::default();
    for (k, tx) in txs.iter().enumerate() {
        for (pool, nf) in &tx.nullifiers {
            if !nullifiers[pool.index()].insert(*nf) {
                return Err(ContextError::DuplicateNullifier {
                    pool: *pool,
                    tx: first + k,
                });
            }
            owners[pool.index()].push(first + k);
        }
    }
    for pool in Pool::ALL {
        let set = &nullifiers[pool.index()];
        if set.is_empty() {
            continue;
        }
        let keys: Vec<[u8; 32]> = txs
            .iter()
            .flat_map(|t| t.nullifiers.iter())
            .filter(|(p, _)| *p == pool)
            .map(|(_, nf)| *nf)
            .collect();
        let found = view.contains_nullifier_many(pool, &keys);
        if let Some(pos) = found.iter().position(|present| *present) {
            return Err(ContextError::DuplicateNullifier {
                pool,
                tx: owners[pool.index()][pos],
            });
        }
    }

    // Anchors: an earlier block's final treestate (never this block's).
    let mut anchors_seen: Set<(Pool, [u8; 32])> = Set::default();
    for (k, tx) in txs.iter().enumerate() {
        for (pool, root) in &tx.anchors {
            if !anchors_seen.insert((*pool, *root)) {
                continue;
            }
            if !view.has_anchor(*pool, root) {
                return Err(ContextError::BadAnchor {
                    pool: *pool,
                    tx: first + k,
                });
            }
        }
        check_sprout_anchors(view, tx, first + k)?;
    }

    // Per-transaction height rules and block totals.
    let mut totals = Totals::default();
    let mut time_lock: Option<(u32, usize)> = None;
    for (k, tx) in txs.iter().enumerate() {
        let i = first + k;
        // ZIP 203, Spec §7.1.2: a non-coinbase transaction is not mined above its nonzero
        // expiry height.
        if !tx.is_coinbase && tx.expiry_height != 0 && height > tx.expiry_height {
            return Err(ContextError::Expired {
                tx: i,
                expiry: tx.expiry_height,
            });
        }
        match (finality(tx, height), block_time) {
            (Finality::Final, _) => {}
            (Finality::NotFinal, _) => return Err(ContextError::NotFinal(i)),
            (Finality::TimeAfter(lock), Some(time)) => {
                if lock >= time {
                    return Err(ContextError::NotFinal(i));
                }
            }
            (Finality::TimeAfter(lock), None) => {
                if !matches!(time_lock, Some((known, _)) if known >= lock) {
                    time_lock = Some((lock, i));
                }
            }
        }
        check_pools(tx, i, &rules.pools)?;
        let spent_value: i128 = inputs[k].iter().map(|coin| i128::from(coin.value)).sum();
        totals.transparent_change -= spent_value;
        add_totals(&mut totals, tx, &rules.limits)?;
    }
    Ok(TxsChecked {
        spent,
        nullifiers,
        totals,
        time_lock,
    })
}

/// The Sprout anchor rule for `tx`, at position `position` of the block: the anchor of
/// each JoinSplit is the final Sprout treestate of an earlier block, or the output
/// treestate of an earlier JoinSplit of `tx` (an interstitial treestate). The output
/// treestate of a JoinSplit is the tree of its anchor plus its two commitments. A
/// treestate of this block, or of another transaction of this block, is not valid.
///
/// The contextual check applies the rule to every transaction of a block. A mempool
/// applies it to a transaction on the tip (`position` is then 0).
pub fn check_sprout_anchors(
    view: &ChainView,
    tx: &PreparedTx,
    position: usize,
) -> Result<(), ContextError> {
    let Some(bundle) = tx.raw.tx.sprout_bundle() else {
        return Ok(());
    };
    if !view.sprout_known() {
        return Err(ContextError::SproutStateUnknown { tx: position });
    }
    let mut interstitial: Map<[u8; 32], SproutFrontier> = Map::default();
    for joinsplit in &bundle.joinsplits {
        let anchor = joinsplit.anchor();
        let mut tree = match (interstitial.get(anchor), view.sprout_tree(anchor)) {
            (Some(tree), _) => tree.clone(),
            (None, Some(tree)) => (*tree).clone(),
            (None, None) => {
                return Err(ContextError::BadAnchor {
                    pool: Pool::Sprout,
                    tx: position,
                })
            }
        };
        let root = tree.append_many(joinsplit.commitments())?;
        interstitial.insert(root, tree);
    }
    Ok(())
}

/// The pool rule of the height: `tx`, at position `position` of the block, has a bundle
/// only for a pool that the rule set of the height names. hayai-prepared applies the rule
/// with the rule set of the consensus branch. A rule set that depends on the height inside
/// one branch (the Orchard soft fork, `hayai_consensus::rules_at`) is visible only here.
fn check_pools(
    tx: &PreparedTx,
    position: usize,
    pools: &ShieldedPools,
) -> Result<(), ContextError> {
    let raw = &tx.raw.tx;
    for (present, active, pool) in [
        (raw.sprout_bundle().map(|_| ()), pools.sprout, Pool::Sprout),
        (
            raw.sapling_bundle().map(|_| ()),
            pools.sapling,
            Pool::Sapling,
        ),
        (
            raw.orchard_bundle().map(|_| ()),
            pools.orchard,
            Pool::Orchard,
        ),
        (
            raw.ironwood_bundle().map(|_| ()),
            pools.ironwood,
            Pool::Ironwood,
        ),
    ] {
        if let (Some(()), false) = (present, active) {
            return Err(ContextError::PoolNotActive { pool, tx: position });
        }
    }
    Ok(())
}

/// Adds `tx` to the block totals and checks the limits. The caller subtracts the value of
/// the coins that `tx` spends from [`Totals::transparent_change`].
fn add_totals(
    totals: &mut Totals,
    tx: &PreparedTx,
    limits: &BlockLimits,
) -> Result<(), ContextError> {
    // zcashd `MAX_BLOCK_SIGOPS` (20,000); ZIP 218: the shielded limits of NU7.
    totals.sigops = totals.sigops.saturating_add(tx.sigops);
    if totals.sigops > limits.sigops {
        return Err(ContextError::TooManySigops(totals.sigops));
    }
    // ZIP 218: per-block limits of Orchard actions, Ironwood actions, Sapling spends and
    // outputs, and the shielded budget.
    totals.orchard_actions = totals.orchard_actions.saturating_add(tx.orchard_actions);
    if totals.orchard_actions > limits.orchard_actions {
        return Err(ContextError::TooManyOrchardActions(totals.orchard_actions));
    }
    totals.ironwood_actions = totals.ironwood_actions.saturating_add(tx.ironwood_actions);
    if totals.ironwood_actions > limits.ironwood_actions {
        return Err(ContextError::TooManyIronwoodActions(
            totals.ironwood_actions,
        ));
    }
    totals.sapling_ios = totals.sapling_ios.saturating_add(tx.sapling_ios);
    if totals.sapling_ios > limits.sapling_ios {
        return Err(ContextError::TooManySaplingIos(totals.sapling_ios));
    }
    let cost = totals
        .orchard_actions
        .saturating_add(totals.ironwood_actions)
        .saturating_add(totals.sapling_ios);
    if cost > limits.shielded_cost {
        return Err(ContextError::ShieldedCostAboveBudget(cost));
    }
    totals.fees = checked_sum([totals.fees, tx.fee].into_iter())?;
    if let Some(b) = tx.raw.tx.transparent_bundle() {
        let created = checked_sum(b.vout.iter().map(|o| o.value().into_u64()))?;
        totals.transparent_change += i128::from(created);
    }
    if let Some(b) = tx.raw.tx.sprout_bundle() {
        totals.sprout_balance += sprout_balance(b);
    }
    if let Some(b) = tx.raw.tx.sapling_bundle() {
        totals.sapling_balance += i128::from(i64::from(*b.value_balance()));
    }
    if let Some(b) = tx.raw.tx.orchard_bundle() {
        totals.orchard_balance += i128::from(i64::from(*b.value_balance()));
    }
    if let Some(b) = tx.raw.tx.ironwood_bundle() {
        totals.ironwood_balance += i128::from(i64::from(*b.value_balance()));
    }
    Ok(())
}

/// The value that the JoinSplits of `bundle` take out of the Sprout pool: `vpub_new` minus
/// `vpub_old` of each JoinSplit.
///
/// ZIP 209: `vpub_old` enters the Sprout pool and `vpub_new` leaves it.
fn sprout_balance(bundle: &sprout::Bundle) -> i128 {
    bundle
        .joinsplits
        .iter()
        .map(|joinsplit| i128::from(i64::from(joinsplit.net_value())))
        .sum()
}

/// The coinbase terms of the block at `height` on the network of `cfg`, whose parent
/// leaves the chain value pools `pools`.
fn coinbase_terms(
    cfg: &CheckConfig<'_>,
    height: u32,
    pools: ValuePools,
) -> Result<CoinbaseTerms, ContextError> {
    // ZIP 237: the total of the pools after the parent gives NSMValueBalance(height - 1).
    Ok(
        hayai_consensus::coinbase::terms_after(cfg.network, height, pools.total())
            .map_err(CoinbaseError::from)?,
    )
}

/// The coinbase against its terms in a block with `fees` zatoshis of fees: every required
/// output, and the value rule (`CoinbaseTerms::check`).
fn check_coinbase_value(
    coinbase: &PreparedTx,
    fees: u64,
    terms: &CoinbaseTerms,
) -> Result<(), ContextError> {
    let tx = &coinbase.raw.tx;
    let outputs: Vec<CoinbaseOutput> = tx.transparent_bundle().map_or_else(Vec::new, |b| {
        b.vout
            .iter()
            .map(|o| CoinbaseOutput {
                value: o.value().into_u64(),
                script: o.script_pubkey().0 .0.to_vec(),
            })
            .collect()
    });
    let shielded = ShieldedBalances {
        sapling: tx
            .sapling_bundle()
            .map_or(0, |b| i64::from(*b.value_balance())),
        orchard: tx
            .orchard_bundle()
            .map_or(0, |b| i64::from(*b.value_balance())),
        ironwood: tx
            .ironwood_bundle()
            .map_or(0, |b| i64::from(*b.value_balance())),
    };
    // ZIP 2001: the total output value is the transparent outputs minus the shielded value
    // balances.
    Ok(terms.check(&outputs, shielded, fees)?)
}

/// The chain value pools after a block with the changes of `totals` and the coinbase
/// `terms`, from the pools `pools` of its parent. Each pool that the block changes must
/// stay at or above zero, and the total of the pools must stay at or below `MAX_MONEY`.
///
/// ZIP 209: no chain value pool is negative after the block, and the total of the pools is
/// at most `MAX_MONEY`.
/// Spec §4.17: the same rules for the transparent, Sprout, Sapling, Orchard, deferred and
/// Ironwood pools, and for `IssuedSupply`.
fn value_pools_after(
    pools: ValuePools,
    totals: &Totals,
    terms: &CoinbaseTerms,
) -> Result<ValuePools, ContextError> {
    let pool_after =
        |before: u64, change: i128, negative: ContextError| -> Result<u64, ContextError> {
            let after = i128::from(before) + change;
            if after < 0 {
                return Err(negative);
            }
            u64::try_from(after)
                .ok()
                .filter(|v| *v <= MAX_MONEY)
                .ok_or(ContextError::ValueOverflow)
        };
    // A positive value balance leaves its shielded pool.
    let shielded = |before: u64, balance: i128, pool: Pool| {
        pool_after(before, -balance, ContextError::NegativeValuePool(pool))
    };
    let after = ValuePools {
        transparent: pool_after(
            pools.transparent,
            totals.transparent_change,
            ContextError::NegativeTransparentPool,
        )?,
        sprout: shielded(pools.sprout, totals.sprout_balance, Pool::Sprout)?,
        sapling: shielded(pools.sapling, totals.sapling_balance, Pool::Sapling)?,
        orchard: shielded(pools.orchard, totals.orchard_balance, Pool::Orchard)?,
        // ZIP 258: the Ironwood pool is a chain value pool of ZIP 209.
        ironwood: shielded(pools.ironwood, totals.ironwood_balance, Pool::Ironwood)?,
        // ZIP 2001, ZIP 271: the deferred pool gains totalDeferredOutput, loses
        // totalDeferredInput, and stays at or above 0.
        deferred: terms.deferred_pool_after(pools.deferred)?,
    };
    checked_sum(
        [
            after.transparent,
            after.sprout,
            after.sapling,
            after.orchard,
            after.ironwood,
            after.deferred,
        ]
        .into_iter(),
    )?;
    Ok(after)
}

/// The chain value pools after the block at `height` ([`value_pools_after`]), with the NSM
/// rules of the height on their total (`hayai_consensus::nsm::check_balance`): the seed
/// in the block before NU7, and no negative NSM value balance from NU7.
fn block_pools_after(
    cfg: &CheckConfig<'_>,
    height: u32,
    pools: ValuePools,
    totals: &Totals,
    terms: &CoinbaseTerms,
) -> Result<ValuePools, ContextError> {
    let after = value_pools_after(pools, totals, terms)?;
    // ZIP 237: the seed at NU7 - 1, and no negative NSMValueBalance from NU7.
    nsm::check_balance(cfg.network, height, after.total()).map_err(CoinbaseError::from)?;
    Ok(after)
}

/// The note commitment trees after appending the commitments of `txs` to the view's. A tree
/// without a new commitment keeps the frontier and the root of the parent. The Ironwood tree
/// has the hash of the Orchard tree (MerkleCRH^Orchard, `hayai_trees::IronwoodFrontier`).
fn append_trees(view: &ChainView, txs: &[Arc<PreparedTx>]) -> Result<Frontiers, ContextError> {
    let mut leaves = Commitments::default();
    for tx in txs {
        leaves.orchard.extend_from_slice(&tx.commitments.orchard);
        leaves.sapling.extend_from_slice(&tx.commitments.sapling);
        leaves.ironwood.extend_from_slice(&tx.commitments.ironwood);
        leaves.sprout.extend_from_slice(&tx.commitments.sprout);
    }
    append_leaves(view, &leaves)
}

/// The note commitment trees after appending `leaves`, the commitments of a block or of a
/// body in block order, to the view's.
///
/// Spec §3.4: the treestates chain transaction by transaction, so the commitments go to the
/// trees in block order.
/// Spec §3.8: a commitment past the capacity of a tree is an error (`TreeError::Full`).
fn append_leaves(view: &ChainView, leaves: &Commitments) -> Result<Frontiers, ContextError> {
    let parent = view.frontiers();
    let Commitments {
        orchard: orchard_leaves,
        sapling: sapling_leaves,
        ironwood: ironwood_leaves,
        sprout: sprout_leaves,
    } = leaves;
    let (orchard, orchard_root) = if orchard_leaves.is_empty() {
        (parent.orchard, parent.anchors.orchard)
    } else {
        let mut f = (*parent.orchard).clone();
        let root = f.append_many(orchard_leaves)?.to_bytes();
        (Arc::new(f), root)
    };
    let (sapling, sapling_root) = if sapling_leaves.is_empty() {
        (parent.sapling, parent.anchors.sapling)
    } else {
        let mut f = (*parent.sapling).clone();
        let root = f.append_many(sapling_leaves)?.to_bytes();
        (Arc::new(f), root)
    };
    let (ironwood, ironwood_root) = if ironwood_leaves.is_empty() {
        (parent.ironwood, parent.anchors.ironwood)
    } else {
        let mut f = (*parent.ironwood).clone();
        let root = f.append_many(ironwood_leaves)?.to_bytes();
        (Arc::new(f), root)
    };
    let sprout = if sprout_leaves.is_empty() {
        parent.sprout
    } else {
        assert!(
            view.sprout_known(),
            "the caller refuses a JoinSplit on an unknown Sprout state"
        );
        let mut f = (*parent.sprout).clone();
        f.append_many(sprout_leaves)?;
        Arc::new(f)
    };
    Ok(Frontiers {
        orchard,
        sapling,
        ironwood,
        sprout,
        anchors: Anchors {
            sapling: sapling_root,
            orchard: orchard_root,
            ironwood: ironwood_root,
        },
    })
}

/// The header commitment to the parent's history tree, then the history tree after the
/// block. `branch` is the epoch of the block's coinbase.
///
/// ZIP 221: the header field of the block commits to the history tree of its parent.
fn check_history(
    view: &ChainView,
    raw: &RawBlock,
    height: u32,
    branch: zcash_protocol::consensus::BranchId,
    auth_data_root: &[u8; 32],
    anchors: &Anchors,
) -> Result<Option<Arc<HistoryState>>, ContextError> {
    let parent_history = view.history();
    match header_commitment(
        branch,
        parent_history.as_deref(),
        &anchors.sapling,
        auth_data_root,
    ) {
        HeaderCommitment::Expected(expected) if expected != raw.header.block_commitments => {
            return Err(ContextError::BlockCommitments);
        }
        // `ParentUnknown`: the rule needs the parent's peaks. The layer then records an
        // unknown history (`Layer::history` is `None`).
        // ZIP 221: not checked on an unknown parent tree (a shadow seed: docs/hayaid.md,
        // Trust limits).
        HeaderCommitment::Expected(_)
        | HeaderCommitment::Reserved
        | HeaderCommitment::ParentUnknown => {}
    }
    let leaf = HistoryLeaf::from_block(raw, height, anchors);
    Ok(history_after(parent_history.as_deref(), branch, &leaf)?.map(Arc::new))
}

/// [`contextual_check`] for a caller that already built [`block_outputs`] of `block` at the
/// height it will have and resolved its inputs with [`resolve_inputs`] (the validator needs
/// both before this check, to prepare the unknown transactions); the map becomes the
/// layer's `created`, and `inputs` becomes its `spent_coins`.
pub fn contextual_check_with_outputs(
    view: &ChainView,
    block: &PreparedBlock,
    created: Map<OutPoint, Coin>,
    inputs: Vec<Vec<Coin>>,
    cfg: &CheckConfig<'_>,
) -> Result<Checked, ContextError> {
    let started = Instant::now();
    let raw = &block.raw;
    let txs = &block.txs;
    assert_eq!(
        txs.len(),
        raw.txs.len(),
        "one prepared transaction per raw one"
    );
    let height = check_parent(view, raw)?;
    let block_time = raw.header.time;

    // Coinbase placement, height commitment and expiry. zcashd `bad-cb-missing`,
    // `bad-cb-multiple`: the first transaction is the only coinbase.
    check_coinbase(txs.first(), height, cfg.rules)?;
    // Spec §7.6: no transaction after the first is a coinbase.
    if let Some(i) = txs.iter().skip(1).position(|t| t.is_coinbase) {
        return Err(ContextError::ExtraCoinbase(i + 1));
    }
    let coinbase = &txs[0];

    // Txid uniqueness and each transaction's position, for the in-block parent order check.
    let positions = positions_of(txs, 0, raw)?;
    let TxsChecked {
        spent,
        nullifiers,
        totals,
        time_lock: _,
    } = check_txs(
        view,
        txs,
        0,
        &created,
        &positions,
        &inputs,
        height,
        Some(block_time),
        cfg,
    )?;
    let terms = coinbase_terms(cfg, height, view.value_pools())?;
    // ZIP 235: the coinbase rule takes the aggregate fees of the block.
    check_coinbase_value(coinbase, totals.fees, &terms)?;
    let value_pools = block_pools_after(cfg, height, view.value_pools(), &totals, &terms)?;
    let context = started.elapsed();

    let trees_started = Instant::now();
    let Frontiers {
        orchard: orchard_frontier,
        sapling: sapling_frontier,
        ironwood: ironwood_frontier,
        sprout: sprout_frontier,
        anchors,
    } = append_trees(view, txs)?;
    let trees = trees_started.elapsed();

    // The coinbase was prepared under the block's epoch.
    let history_started = Instant::now();
    let history = check_history(
        view,
        raw,
        height,
        coinbase.epoch.branch_id,
        &block.auth_data_root,
        &anchors,
    )?;
    let history_took = history_started.elapsed();

    let layer = Layer {
        height,
        hash: raw.hash(),
        parent: raw.header.prev_hash,
        time: block_time,
        bits: raw.header.bits,
        wtxids: txs.iter().map(|t| t.wtxid()).collect(),
        created,
        spent,
        spent_coins: inputs,
        nullifiers,
        orchard_frontier,
        sapling_frontier,
        ironwood_frontier,
        sprout_frontier,
        anchors,
        value_pools,
        history,
    };
    Ok(Checked {
        layer,
        timings: ContextTimings {
            context,
            trees,
            history: history_took,
        },
    })
}

/// The position of each txid of `txs` (the transactions of a block from position `first`
/// on), with the duplicate rule. `raw`, when given, is the block the transactions must
/// match in order. zcashd `bad-txns-duplicate` (CVE-2012-2459): no txid twice in a block.
fn positions_of(
    txs: &[Arc<PreparedTx>],
    first: usize,
    raw: &RawBlock,
) -> Result<Map<TxId, usize>, ContextError> {
    let mut positions: Map<TxId, usize> =
        Map::with_capacity_and_hasher(txs.len(), Default::default());
    for (k, tx) in txs.iter().enumerate() {
        assert_eq!(
            tx.raw.txid,
            raw.txs[first + k].txid,
            "prepared block is out of order"
        );
        match positions.insert(tx.raw.txid, first + k) {
            None => {}
            Some(_) => return Err(ContextError::DuplicateTxid(tx.raw.txid)),
        }
    }
    Ok(positions)
}

/// The contextual work of a block body, every transaction after the coinbase, done on the
/// parent's view before the block exists: the outputs, the inputs against the view, the
/// nullifiers, the anchors, every rule that needs only the height, the totals, the value
/// pools of the body, the tree appends, and the branches of position 0 in both header
/// roots. A block whose transactions after the coinbase are exactly these, in this order, commits with
/// [`PrebuiltBody::commit`]: the header and coinbase rules, then the layer is the prebuilt
/// state plus the coinbase outputs, with no further pass over the body.
///
/// A prebuilt body is only valid on the view it was built on: the commit checks that the
/// view's tip is still the parent. The coinbase must be transparent: a shielded coinbase
/// adds note commitments before the body's, which changes the trees.
pub struct PrebuiltBody {
    pub parent: BlockHash,
    pub height: u32,
    /// The transactions after the coinbase, in block order.
    pub wtxids: Vec<WtxId>,
    positions: Map<TxId, usize>,
    created: Map<OutPoint, Coin>,
    spent: Set<OutPoint>,
    /// The coins of the inputs of the transactions after the coinbase.
    spent_coins: Vec<Vec<Coin>>,
    nullifiers: [Set<[u8; 32]>; 4],
    totals: Totals,
    time_lock: Option<(u32, usize)>,
    orchard_frontier: Arc<OrchardFrontier>,
    sapling_frontier: Arc<SaplingFrontier>,
    ironwood_frontier: Arc<IronwoodFrontier>,
    sprout_frontier: Arc<SproutFrontier>,
    anchors: Anchors,
    merkle_branch: Vec<[u8; 32]>,
    auth_branch: Vec<[u8; 32]>,
}

/// Why a prebuilt body does not commit a block.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SwapError {
    /// The block is not the one prebuilt (another parent, another body, a shielded
    /// coinbase). Nothing is wrong with the block: validate it on the full path.
    #[error("block does not match the prebuilt body: {0}")]
    Mismatch(&'static str),
    /// The merkle root of the header does not match the coinbase and the body.
    #[error("merkle root does not match the transactions")]
    MerkleRoot,
    /// The block breaks a contextual rule of its header or coinbase.
    #[error("{0}")]
    Context(#[from] ContextError),
}

/// Prebuilds the body `txs` (prepared, in block order, without a coinbase) on top of
/// `view`. The caller provides transactions whose scripts and proofs are verified.
pub fn prebuild_body(
    view: &ChainView,
    txs: &[Arc<PreparedTx>],
    cfg: &CheckConfig<'_>,
) -> Result<PrebuiltBody, ContextError> {
    let tip = view.tip();
    let height = tip.height + 1;
    if let Some(i) = txs.iter().position(|t| t.is_coinbase) {
        return Err(ContextError::ExtraCoinbase(i + 1));
    }
    let raws: Vec<&RawTx> = txs.iter().map(|t| t.raw.as_ref()).collect();
    let mut positions: Map<TxId, usize> =
        Map::with_capacity_and_hasher(txs.len(), Default::default());
    for (k, tx) in txs.iter().enumerate() {
        match positions.insert(tx.raw.txid, k + 1) {
            None => {}
            Some(_) => return Err(ContextError::DuplicateTxid(tx.raw.txid)),
        }
    }
    let created = outputs_of(&raws, false, height);
    let inputs = inputs_of(view, &raws, 1, &created)?;
    let TxsChecked {
        spent,
        nullifiers,
        totals,
        time_lock,
    } = check_txs(
        view, txs, 1, &created, &positions, &inputs, height, None, cfg,
    )?;
    // The pools of the body alone: a body that makes a pool negative fails here, before a
    // block has it. The commit computes the pools again with the coinbase.
    let pools = view.value_pools();
    value_pools_after(pools, &totals, &coinbase_terms(cfg, height, pools)?)?;
    let Frontiers {
        orchard: orchard_frontier,
        sapling: sapling_frontier,
        ironwood: ironwood_frontier,
        sprout: sprout_frontier,
        anchors,
    } = append_trees(view, txs)?;
    let txids: Vec<TxId> = raws.iter().map(|t| t.txid).collect();
    let digests: Vec<[u8; 32]> = raws.iter().map(|t| t.auth_digest).collect();
    Ok(PrebuiltBody {
        parent: tip.hash,
        height,
        wtxids: txs.iter().map(|t| t.wtxid()).collect(),
        positions,
        created,
        spent,
        spent_coins: inputs,
        nullifiers,
        totals,
        time_lock,
        orchard_frontier,
        sapling_frontier,
        ironwood_frontier,
        sprout_frontier,
        anchors,
        merkle_branch: hayai_wire::merkle_branch_first(&txids),
        auth_branch: hayai_wire::auth_branch_first(&digests),
    })
}

impl PrebuiltBody {
    /// Whether `raw` has this body after its coinbase and this parent. A match is the
    /// condition of [`PrebuiltBody::commit`]; the header and coinbase rules come there.
    pub fn matches(&self, raw: &RawBlock) -> bool {
        raw.header.prev_hash == self.parent
            && raw.txs.len() == self.wtxids.len() + 1
            && raw.txs[1..]
                .iter()
                .zip(&self.wtxids)
                .all(|(t, id)| t.wtxid() == *id)
    }

    /// Commits `raw` on top of `view` with this body: the header roots from the coinbase
    /// and the branches, the coinbase rules (placement, height, expiry, finality, value,
    /// sigops, txid uniqueness), the value pools with the coinbase, the lock times that
    /// need the block time, the header commitment and the history append. The layer takes the prebuilt maps and trees as
    /// they are, plus the coinbase outputs. `coinbase` is the prepared first transaction of
    /// `raw`.
    pub fn commit(
        self,
        view: &ChainView,
        raw: &RawBlock,
        coinbase: &Arc<PreparedTx>,
        cfg: &CheckConfig<'_>,
    ) -> Result<Checked, SwapError> {
        let started = Instant::now();
        if view.tip().hash != self.parent {
            return Err(SwapError::Mismatch(
                "the view moved off the prebuilt parent",
            ));
        }
        if !self.matches(raw) {
            return Err(SwapError::Mismatch("another parent or another body"));
        }
        let Some(first) = raw.txs.first() else {
            return Err(SwapError::Mismatch("a block without transactions"));
        };
        assert_eq!(coinbase.raw.txid, first.txid, "coinbase of another block");
        if !matches!(
            (
                first.tx.sapling_bundle(),
                first.tx.orchard_bundle(),
                first.tx.ironwood_bundle()
            ),
            (None, None, None)
        ) {
            return Err(SwapError::Mismatch("a shielded coinbase"));
        }
        let height = self.height;
        if hayai_wire::merkle_root_from_branch(&first.txid, &self.merkle_branch)
            != raw.header.merkle_root
        {
            return Err(SwapError::MerkleRoot);
        }
        check_coinbase(Some(coinbase), height, cfg.rules)?;
        let None = self.positions.get(&first.txid) else {
            return Err(ContextError::DuplicateTxid(first.txid).into());
        };
        let block_time = raw.header.time;
        match finality(coinbase, height) {
            Finality::Final => {}
            Finality::TimeAfter(lock) if lock < block_time => {}
            Finality::TimeAfter(_) | Finality::NotFinal => {
                return Err(ContextError::NotFinal(0).into())
            }
        }
        if let Some((lock, i)) = self.time_lock {
            if lock >= block_time {
                return Err(ContextError::NotFinal(i).into());
            }
        }
        let mut totals = self.totals;
        check_pools(coinbase, 0, &cfg.rules.pools)?;
        add_totals(&mut totals, coinbase, &cfg.rules.limits)?;
        let terms = coinbase_terms(cfg, height, view.value_pools())?;
        // ZIP 235: the coinbase rule takes the aggregate fees of the block.
        check_coinbase_value(coinbase, totals.fees, &terms)?;
        let value_pools = block_pools_after(cfg, height, view.value_pools(), &totals, &terms)?;
        let context = started.elapsed();

        let history_started = Instant::now();
        let auth_data_root =
            hayai_wire::auth_root_from_branch(&first.auth_digest, &self.auth_branch);
        let history = check_history(
            view,
            raw,
            height,
            coinbase.epoch.branch_id,
            &auth_data_root,
            &self.anchors,
        )?;
        let history_took = history_started.elapsed();

        let mut created = self.created;
        created.extend(outputs_of(&[first], true, height));
        let mut wtxids = Vec::with_capacity(self.wtxids.len() + 1);
        wtxids.push(first.wtxid());
        wtxids.extend(self.wtxids);
        let mut spent_coins = Vec::with_capacity(self.spent_coins.len() + 1);
        spent_coins.push(Vec::new());
        spent_coins.extend(self.spent_coins);
        Ok(Checked {
            layer: Layer {
                height,
                hash: raw.hash(),
                parent: raw.header.prev_hash,
                time: block_time,
                bits: raw.header.bits,
                wtxids,
                created,
                spent: self.spent,
                spent_coins,
                nullifiers: self.nullifiers,
                orchard_frontier: self.orchard_frontier,
                sapling_frontier: self.sapling_frontier,
                ironwood_frontier: self.ironwood_frontier,
                sprout_frontier: self.sprout_frontier,
                anchors: self.anchors,
                value_pools,
                history,
            },
            timings: ContextTimings {
                context,
                trees: std::time::Duration::ZERO,
                history: history_took,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use hayai_consensus::Upgrade;

    use super::*;

    const ZEC: u64 = 100_000_000;

    fn terms(network: Network, height: u32) -> CoinbaseTerms {
        hayai_consensus::coinbase::terms_at(network, height).expect("a rule set")
    }

    /// Mainnet in NU6.2: each block adds 0.1875 ZEC to the deferred pool.
    const HEIGHT: u32 = 3_400_000;

    fn pools() -> ValuePools {
        ValuePools {
            transparent: 1_000,
            sprout: 2_000,
            sapling: 3_000,
            orchard: 4_000,
            ironwood: 5_000,
            deferred: 6_000,
        }
    }

    /// Every pool follows its change: the transparent pool the outputs minus the spent
    /// coins, a shielded pool the negated value balance (the Sprout pool `vpub_old` minus
    /// `vpub_new`), the deferred pool the terms.
    #[test]
    fn each_value_pool_follows_its_change() {
        let terms = terms(Network::Mainnet, HEIGHT);
        assert_eq!(terms.subsidy.deferred, 18_750_000);
        let totals = Totals {
            transparent_change: -400,
            sprout_balance: 150,
            sapling_balance: 300,
            orchard_balance: -50,
            ironwood_balance: -70,
            ..Totals::default()
        };
        assert_eq!(
            value_pools_after(pools(), &totals, &terms),
            Ok(ValuePools {
                transparent: 600,
                sprout: 1_850,
                sapling: 2_700,
                orchard: 4_050,
                ironwood: 5_070,
                deferred: 6_000 + 18_750_000,
            })
        );
        // Before NU6 the deferred pool does not change.
        let before_nu6 = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap() - 1;
        let after = value_pools_after(
            pools(),
            &Totals::default(),
            &self::terms(Network::Mainnet, before_nu6),
        );
        assert_eq!(after, Ok(pools()));
    }

    /// No pool can be negative after a block. A pool at exactly zero is valid.
    #[test]
    fn no_value_pool_can_be_negative() {
        let terms = terms(Network::Mainnet, HEIGHT);
        let after = |totals: Totals| value_pools_after(pools(), &totals, &terms);
        type Case = (Totals, Totals, ContextError);
        let cases: [Case; 5] = [
            (
                Totals {
                    transparent_change: -1_000,
                    ..Totals::default()
                },
                Totals {
                    transparent_change: -1_001,
                    ..Totals::default()
                },
                ContextError::NegativeTransparentPool,
            ),
            (
                Totals {
                    sprout_balance: 2_000,
                    ..Totals::default()
                },
                Totals {
                    sprout_balance: 2_001,
                    ..Totals::default()
                },
                ContextError::NegativeValuePool(Pool::Sprout),
            ),
            (
                Totals {
                    sapling_balance: 3_000,
                    ..Totals::default()
                },
                Totals {
                    sapling_balance: 3_001,
                    ..Totals::default()
                },
                ContextError::NegativeValuePool(Pool::Sapling),
            ),
            (
                Totals {
                    orchard_balance: 4_000,
                    ..Totals::default()
                },
                Totals {
                    orchard_balance: 4_001,
                    ..Totals::default()
                },
                ContextError::NegativeValuePool(Pool::Orchard),
            ),
            (
                Totals {
                    ironwood_balance: 5_000,
                    ..Totals::default()
                },
                Totals {
                    ironwood_balance: 5_001,
                    ..Totals::default()
                },
                ContextError::NegativeValuePool(Pool::Ironwood),
            ),
        ];
        for (to_zero, below_zero, error) in cases {
            let Ok(emptied) = after(to_zero) else {
                panic!("a pool at zero is valid: {error}");
            };
            let values = [
                emptied.transparent,
                emptied.sprout,
                emptied.sapling,
                emptied.orchard,
                emptied.ironwood,
            ];
            assert_eq!(values.iter().filter(|v| **v == 0).count(), 1, "{error}");
            assert_eq!(after(below_zero), Err(error));
        }
    }

    /// The deferred pool at the NU6.1 activation block: the block pays 78,750 ZEC out of
    /// the pool. A pool that holds less is an error, never a pool of zero.
    #[test]
    fn the_deferred_pool_pays_the_disbursement_or_the_block_fails() {
        for network in [Network::Mainnet, Network::Testnet] {
            let nu6_1 = network.activation_height(Upgrade::Nu6_1).unwrap();
            let terms = terms(network, nu6_1);
            assert_eq!(terms.disbursed, 78_750 * ZEC);
            let with = |deferred: u64| {
                let before = ValuePools {
                    deferred,
                    ..ValuePools::default()
                };
                value_pools_after(before, &Totals::default(), &terms).map(|after| after.deferred)
            };
            // The pool of a chain from NU6: 420,000 blocks of 0.1875 ZEC.
            assert_eq!(with(78_750 * ZEC), Ok(18_750_000));
            assert_eq!(with(78_750 * ZEC - 18_750_000), Ok(0));
            // A base that does not hold the pool (a pool of zero) cannot pass the block.
            for short in [0, 78_750 * ZEC - 18_750_001] {
                assert_eq!(
                    with(short),
                    Err(ContextError::Coinbase(
                        CoinbaseError::NegativeDeferredPool {
                            before: short,
                            disbursed: 78_750 * ZEC,
                        }
                    ))
                );
            }
        }
    }

    /// The NSM rules on the total of the pools, on Testnet (NU7 at 4,465,026) and on a
    /// configured Regtest (NU7 at 9). The block before NU7: the scheduled issuance minus
    /// the total is the seed of the network, and Regtest has no seed. From NU7: the total
    /// is at most the scheduled issuance. Before that block: no rule.
    #[test]
    fn the_nsm_rules_apply_from_the_block_before_nu7() {
        use hayai_consensus::subsidy::scheduled_issuance;
        use hayai_consensus::{ConsensusError, RegtestConfig};

        let rules = RuleSet::of(Upgrade::Nu6_3).expect("a rule set");
        let regtest = RegtestConfig::new(&[(Upgrade::Nu7, 9)], Vec::new(), 0)
            .expect("a valid configuration")
            .network();
        // The terms of a block without a deferred part: the pools change by the totals.
        let terms = terms(Network::Regtest, 1);
        let after = |network: Network, height: u32, total: u128| {
            let cfg = CheckConfig { network, rules };
            let Ok(total) = u64::try_from(total) else {
                panic!("the total fits in 64 bits");
            };
            let before = ValuePools {
                transparent: total,
                ..ValuePools::default()
            };
            block_pools_after(&cfg, height, before, &Totals::default(), &terms)
                .map(|pools| pools.total())
        };
        let nsm =
            |error: ConsensusError| Err(ContextError::Coinbase(CoinbaseError::Consensus(error)));

        let nu7 = 4_465_026;
        let seed = 55_768_414_957u128;
        let scheduled = |height| scheduled_issuance(Network::Testnet, height);
        // Two blocks before NU7: no rule, for any total.
        let total = scheduled(nu7 - 2) + 1;
        assert_eq!(after(Network::Testnet, nu7 - 2, total), Ok(total as u64));
        // The block before NU7: the seed, exactly.
        let total = scheduled(nu7 - 1) - seed;
        assert_eq!(after(Network::Testnet, nu7 - 1, total), Ok(total as u64));
        for wrong in [total - 1, total + 1] {
            let found = (scheduled(nu7 - 1) - wrong) as u64;
            assert_eq!(
                after(Network::Testnet, nu7 - 1, wrong),
                nsm(ConsensusError::NsmSeedMismatch {
                    expected: seed as u64,
                    found,
                })
            );
        }
        // From NU7: a total up to the scheduled issuance.
        for height in [nu7, nu7 + 1] {
            let all = scheduled(height);
            assert_eq!(after(Network::Testnet, height, 0), Ok(0));
            assert_eq!(after(Network::Testnet, height, all), Ok(all as u64));
            assert_eq!(
                after(Network::Testnet, height, all + 1),
                nsm(ConsensusError::NegativeNsmBalance {
                    height,
                    scheduled: all,
                    issued: all as u64 + 1,
                })
            );
        }

        // Regtest, NU7 at 9: 625,000,000 zatoshis for each block before NU7, a third from
        // NU7.
        let scheduled = |height| scheduled_issuance(regtest, height);
        assert_eq!(scheduled(8), 8 * 625_000_000);
        assert_eq!(scheduled(10), 8 * 625_000_000 + 2 * 208_333_333);
        // The block before NU7 has no seed rule: any total passes.
        for total in [0, scheduled(8), scheduled(8) + 1] {
            assert_eq!(after(regtest, 8, total), Ok(total as u64));
        }
        for height in [9, 10] {
            let all = scheduled(height);
            assert_eq!(after(regtest, height, all), Ok(all as u64));
            assert_eq!(
                after(regtest, height, all + 1),
                nsm(ConsensusError::NegativeNsmBalance {
                    height,
                    scheduled: all,
                    issued: all as u64 + 1,
                })
            );
        }
        // Regtest without an NU7 height: no rule.
        assert_eq!(
            after(Network::Regtest, 9, u128::from(MAX_MONEY)),
            Ok(MAX_MONEY)
        );
    }

    /// The total of the pools is at most `MAX_MONEY`, and so is each pool.
    #[test]
    fn the_total_of_the_pools_is_bounded() {
        let terms = terms(Network::Regtest, 1);
        let before = ValuePools {
            transparent: MAX_MONEY - 10,
            sapling: 10,
            ..ValuePools::default()
        };
        assert_eq!(
            value_pools_after(before, &Totals::default(), &terms),
            Ok(before)
        );
        let grows = Totals {
            orchard_balance: -1,
            ..Totals::default()
        };
        assert_eq!(
            value_pools_after(before, &grows, &terms),
            Err(ContextError::ValueOverflow)
        );
        let one_pool = Totals {
            transparent_change: 11,
            ..Totals::default()
        };
        assert_eq!(
            value_pools_after(before, &one_pool, &terms),
            Err(ContextError::ValueOverflow)
        );
    }
}
