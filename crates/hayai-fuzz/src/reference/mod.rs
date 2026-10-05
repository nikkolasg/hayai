//! The reference verdict of a block: the Zakura library code, run in this process.
//!
//! [`check_block`] follows the semantic block verifier and the transaction verifier of
//! zakura-consensus (`block.rs`, `transaction.rs`). The node runs these checks as async
//! tower services with a state service. Here they run in the same order, in one thread,
//! with the context of the case in place of the state service.
//!
//! Three kinds of code give the verdict. `docs/conformance.md` (Differential fuzzer)
//! has the rule table.
//!
//! - Linked reference crates: zakura-chain (parse, transaction ids, merkle root, subsidy
//!   and funding stream schedules, value balances, sighash), zakura-header-chain (header
//!   encoding), zakura-script (script verification, signature operation counts) and
//!   zakura-orchard (proofs and signatures of the Orchard and Ironwood bundles).
//! - Copied reference code: [`zakura_consensus`].
//! - A model of the state rules of zakura-state that are plain set rules: [`model`]
//!   comments mark each of them. zakura-state does not link into this process.
//!
//! The oracle has no rule for: proof of work and the contextual header rules (the header
//! classes compare them), Sapling and Sprout proofs and signatures, anchors, the chain
//! value pools, the history tree append.

pub mod zakura_consensus;

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use zakura_chain::amount::{Amount, NonNegative};
use zakura_chain::block::{
    self, Block, ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Commitment, Height,
};
use zakura_chain::parameters::subsidy::block_subsidy;
use zakura_chain::parameters::{Network, NetworkUpgrade};
use zakura_chain::serialization::ZcashDeserialize;
use zakura_chain::transaction::{self, HashType, Transaction};
use zakura_chain::transparent;
use zakura_script::{CachedFfiTransaction, Sigops};
use zk_orchard::bundle::BatchValidator;
use zk_orchard::circuit::{OrchardCircuitVersion, VerifyingKey};

use crate::context::{Context, Net, ShieldedPool};
use crate::verdict::{RuleClass, Verdict};
use zakura_consensus::error::{BlockError, TransactionError};
use zakura_consensus::{block_check, transaction_check as check, MAX_BLOCK_SIGOPS};

/// The Zakura network of `net`.
pub fn network(net: Net) -> Network {
    match net {
        Net::Mainnet => Network::Mainnet,
        Net::Testnet => Network::new_default_testnet(),
    }
}

/// The Orchard verifying key of the circuit of `upgrade`: the mapping of
/// `zakura-consensus/src/primitives/halo2.rs`, `lazy_verifier_for`.
fn verifying_key(upgrade: NetworkUpgrade) -> &'static VerifyingKey {
    use NetworkUpgrade::*;
    static PRE_NU6_2: OnceLock<VerifyingKey> = OnceLock::new();
    static NU6_2: OnceLock<VerifyingKey> = OnceLock::new();
    static NU6_3_ONWARD: OnceLock<VerifyingKey> = OnceLock::new();
    match upgrade {
        Genesis | BeforeOverwinter | Overwinter | Sapling | Blossom | Heartwood | Canopy | Nu5
        | Nu6 | Nu6_1 => {
            PRE_NU6_2.get_or_init(|| VerifyingKey::build(OrchardCircuitVersion::InsecurePreNu6_2))
        }
        Nu6_2 => NU6_2.get_or_init(|| VerifyingKey::build(OrchardCircuitVersion::FixedPostNu6_2)),
        Nu6_3 | Nu7 => {
            NU6_3_ONWARD.get_or_init(|| VerifyingKey::build(OrchardCircuitVersion::PostNu6_3))
        }
    }
}

/// Builds the verifying keys of the upgrades from NU5 to NU6.3. A caller that is not a
/// rayon worker calls it once before the first case: the key build uses the rayon pool.
pub fn build_keys() {
    for upgrade in [
        NetworkUpgrade::Nu5,
        NetworkUpgrade::Nu6_2,
        NetworkUpgrade::Nu6_3,
    ] {
        verifying_key(upgrade);
    }
}

/// A rejection of the reference: the rule class and the error text.
struct Reject(RuleClass, String);

fn reject<T>(class: RuleClass, detail: impl ToString) -> Result<T, Reject> {
    Err(Reject(class, detail.to_string()))
}

/// The rule class of a transaction error of zakura-consensus.
fn transaction_class(error: &TransactionError) -> RuleClass {
    use TransactionError::*;
    match error {
        CoinbasePosition
        | CoinbaseAfterFirst
        | NonCoinbaseHasCoinbaseInput
        | NotCoinbase
        | CoinbaseInMempool => RuleClass::CoinbaseForm,
        CoinbaseHasJoinSplit
        | CoinbaseHasSpend
        | CoinbaseHasOutputPreHeartwood
        | CoinbaseHasEnableSpendsOrchard
        | CoinbaseHasEnableSpendsIronwood
        | CoinbaseHasOrchardShieldedData
        | NoInputs
        | NoOutputs
        | BadBalance
        | BothVPubsNonZero
        | DisabledAddToSproutPool
        | DisabledAddToOrchardPool
        | NotEnoughFlags
        | NotEnoughIronwoodFlags
        | OrchardHasEnableCrossAddress
        | SmallOrder
        | OrchardProofSize
        | IronwoodProofSize => RuleClass::TxStructure,
        CoinbaseOutputsNotDecryptable | Subsidy(_) | CoinbaseConstruction(_) => {
            RuleClass::CoinbaseTerms
        }
        LockedUntilAfterBlockHeight(_)
        | LockedUntilAfterBlockTime(_)
        | CoinbaseExpiryBlockHeight { .. }
        | MaximumExpiryHeight { .. }
        | ExpiredTransaction { .. } => RuleClass::TxTime,
        WrongVersion
        | UnsupportedByNetworkUpgrade(..)
        | WrongConsensusBranchId
        | WrongConsensusBranchIdNu6_3GracePeriod
        | MissingConsensusBranchId => RuleClass::TxVersion,
        Script(_) => RuleClass::Script,
        SaplingVerificationFailed
        | Halo2VerificationFailed
        | Groth16(_)
        | MalformedGroth16(_)
        | Ed25519(_)
        | RedJubjub(_)
        | RedPallas(_) => RuleClass::ShieldedProof,
        IncorrectFee | Amount(_) | Balance(_) => RuleClass::Value,
        DuplicateTransparentSpend(_)
        | TransparentInputNotFound
        | ImmatureTransparentCoinbaseSpend { .. }
        | UnshieldedTransparentCoinbaseSpend { .. } => RuleClass::TransparentInput,
        DuplicateSproutNullifier(_)
        | DuplicateSaplingNullifier(_)
        | DuplicateOrchardNullifier(_)
        | DuplicateIronwoodNullifier(_) => RuleClass::Nullifier,
        OrchardActionsExceedBlockLimit { .. }
        | IronwoodActionsExceedBlockLimit { .. }
        | SaplingIOsExceedBlockLimit { .. }
        | SproutJoinSplitsExceedBlockLimit { .. }
        | ShieldedCostExceedsBlockBudget { .. } => RuleClass::Limits,
        _ => RuleClass::Other,
    }
}

impl From<TransactionError> for Reject {
    fn from(error: TransactionError) -> Self {
        Reject(transaction_class(&error), error.to_string())
    }
}

impl From<BlockError> for Reject {
    fn from(error: BlockError) -> Self {
        use BlockError::*;
        let class = match &error {
            InvalidHeaderEncoding(_)
            | InvalidDifficulty(..)
            | TargetDifficultyLimit(..)
            | DifficultyFilter(..) => RuleClass::Header,
            Transaction(inner) => transaction_class(inner),
            NoTransactions | MissingHeight(_) | MaxHeight(..) => RuleClass::CoinbaseForm,
            BadMerkleRoot { .. } | DuplicateTransaction => RuleClass::Merkle,
            WrongTransactionConsensusBranchId => RuleClass::TxVersion,
            TooManyTransparentSignatureOperations { .. } => RuleClass::Limits,
            SummingMinerFees { .. } => RuleClass::Value,
            Other(_) => RuleClass::Other,
        };
        Reject(class, error.to_string())
    }
}

/// The transparent outputs that the chain of `ctx` holds before the block.
fn chain_utxos(ctx: &Context) -> Result<HashMap<transparent::OutPoint, transparent::Utxo>, String> {
    let mut utxos = HashMap::with_capacity(ctx.coins.len());
    for (outpoint, coin) in &ctx.coins {
        let value = Amount::<NonNegative>::try_from(coin.value)
            .map_err(|e| format!("the value of a context coin is not an amount: {e}"))?;
        let output = transparent::Output::new(value, transparent::Script::new(&coin.script));
        utxos.insert(
            transparent::OutPoint {
                hash: transaction::Hash(outpoint.hash),
                index: outpoint.index,
            },
            transparent::Utxo::new(output, Height(coin.height), coin.coinbase),
        );
    }
    Ok(utxos)
}

/// The parsed block of `bytes`, as the block message decoder of Zakura reads it: bytes
/// after the block are not an error (`zakura-network/src/protocol/external/codec.rs:534`).
pub fn parse(bytes: &[u8]) -> Result<Block, String> {
    Block::zcash_deserialize(&mut Cursor::new(bytes)).map_err(|e| e.to_string())
}

/// The length in bytes of the block at the start of `bytes`. `None`: the bytes do not
/// start with a block.
pub fn block_len(bytes: &[u8]) -> Option<usize> {
    let mut cursor = Cursor::new(bytes);
    Block::zcash_deserialize(&mut cursor).ok()?;
    Some(cursor.position() as usize)
}

/// The merkle root of the transactions of the block `bytes`. `None`: the block does not
/// parse, or it has no transaction (the merkle code of zakura-chain panics on an empty
/// list; the node checks the coinbase height before it).
pub fn merkle_root(bytes: &[u8]) -> Option<[u8; 32]> {
    let block = parse(bytes).ok()?;
    if block.transactions.is_empty() {
        return None;
    }
    let root: block::merkle::Root = block.transactions.iter().map(|t| t.hash()).collect();
    Some(root.0)
}

/// The commitments field that the header of the block `bytes` must have from NU5 on a
/// parent with the history root `history_root`. `None`: the block does not parse.
pub fn block_commitments(bytes: &[u8], history_root: [u8; 32]) -> Option<[u8; 32]> {
    let block = parse(bytes).ok()?;
    let hash = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
        &ChainHistoryMmrRootHash::from(history_root),
        &block.auth_data_root(),
    );
    Some(hash.into())
}

/// The transaction id of the transaction `bytes`. `None`: the transaction does not parse.
pub fn txid(bytes: &[u8]) -> Option<[u8; 32]> {
    let tx = Transaction::zcash_deserialize(&mut Cursor::new(bytes)).ok()?;
    Some(tx.hash().0)
}

/// The reference verdict of the block `bytes` on the chain of `ctx`. A panic of the
/// reference code is [`Verdict::Panic`].
pub fn check_block(bytes: &[u8], ctx: &Context) -> Verdict {
    match crate::guarded(|| run(bytes, ctx)) {
        Ok(Ok(None)) => Verdict::Accept,
        Ok(Ok(Some(not_covered))) => Verdict::NotCovered(not_covered),
        Ok(Err(Reject(class, detail))) => Verdict::Reject { class, detail },
        Err(panic) => Verdict::Panic(panic),
    }
}

/// `Ok(None)`: the block is valid. `Ok(Some(part))`: no rule rejected the block, and the
/// oracle has no reference for `part`.
fn run(bytes: &[u8], ctx: &Context) -> Result<Option<String>, Reject> {
    let network = network(ctx.network);
    let block = match parse(bytes) {
        Ok(block) => Arc::new(block),
        Err(e) => return reject(RuleClass::Parse, e),
    };

    // zakura-consensus `block.rs`, `SemanticBlockVerifier::call`.
    let hash = zakura_header_chain::validate_encoding_version_hash(&block.header)
        .map_err(BlockError::from)?;
    let Some(height) = block.coinbase_height() else {
        return Err(BlockError::MissingHeight(hash).into());
    };
    if height > Height::MAX {
        return Err(BlockError::MaxHeight(height, hash, Height::MAX).into());
    }
    // Proof of work: not checked. The blocks of the cases have generated headers.

    let transaction_hashes: Arc<[transaction::Hash]> =
        block.transactions.iter().map(|t| t.hash()).collect();
    block_check::merkle_root_validity(&network, &block, &transaction_hashes)?;
    // The rule of the clock of the node is not a consensus rule and does not run.
    let coinbase_tx = block_check::coinbase_is_first(&block)?;

    // Model of zakura-state: the block is the child of the tip
    // (`zakura-state/src/service/check.rs`, `block_is_valid_for_recent_chain`: the
    // height is the parent height plus 1; the non-finalized state finds the parent by
    // the hash of the header).
    model::parent(&block, height, ctx)?;

    block_check::shielded_action_limits_are_valid(&block.transactions, height, &network)?;
    if zakura_chain::parameters::subsidy::is_zip234_active(&network, height) {
        return Ok(Some(
            "the ZIP 234 subsidy needs the NSM value balance".into(),
        ));
    }
    let expected_block_subsidy = block_subsidy(height, &network, None)
        .map_err(|e| Reject(RuleClass::CoinbaseTerms, e.to_string()))?;
    let deferred_pool_balance_change =
        block_check::subsidy_is_valid(&block, &network, expected_block_subsidy)?;
    check::coinbase_outputs_are_decryptable(&coinbase_tx, &network, height)?;

    let known_utxos = transparent::new_ordered_outputs(&block, &transaction_hashes);
    let chain = chain_utxos(ctx).map_err(|e| Reject(RuleClass::Other, e))?;

    // Model of zakura-state: the transparent spends of the block
    // (`zakura-state/src/service/check/utxo.rs`, `transparent_spend`).
    model::transparent_spends(&block, &known_utxos, &chain)?;

    let mut sigops: u32 = 0;
    let mut fees: Result<Amount<NonNegative>, zakura_chain::amount::Error> = Ok(Amount::zero());
    let mut not_covered = None;
    for transaction in block.transactions.iter() {
        let checked = check_transaction(
            transaction,
            height,
            &block.header,
            &network,
            &known_utxos,
            &chain,
        )?;
        sigops = sigops.saturating_add(checked.sigops);
        if let Some(fee) = checked.miner_fee {
            fees = fees.and_then(|sum| sum + fee);
        }
        not_covered = not_covered.or(checked.not_covered);
    }

    if sigops > MAX_BLOCK_SIGOPS {
        return Err(BlockError::TooManyTransparentSignatureOperations {
            height,
            hash,
            sigops,
        }
        .into());
    }
    let block_transaction_fees = fees.map_err(|source| BlockError::SummingMinerFees {
        height,
        hash,
        source,
    })?;
    block_check::miner_fees_are_valid(
        &coinbase_tx,
        height,
        block_transaction_fees,
        expected_block_subsidy,
        deferred_pool_balance_change,
        &network,
    )?;

    // The contextual rules of zakura-state that the oracle has.
    model::nullifiers(&block, ctx)?;
    commitments(&block, &network, ctx)?;
    Ok(not_covered)
}

/// The commitments field of the header against the history root of the context and the
/// authorizing data root of the block (`zakura-state/src/service/check.rs`,
/// `block_commitment_is_valid_for_chain_history`). The parse of the field, the
/// authorizing data root and the hash of the two roots are zakura-chain code. The
/// history root is a value of the context.
fn commitments(block: &Block, network: &Network, ctx: &Context) -> Result<(), Reject> {
    let Some(history_root) = ctx.history_root else {
        return Ok(());
    };
    let commitment = block
        .commitment(network)
        .map_err(|e| Reject(RuleClass::Commitments, e.to_string()))?;
    let Commitment::ChainHistoryBlockTxAuthCommitment(actual) = commitment else {
        return Ok(());
    };
    let expected = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
        &ChainHistoryMmrRootHash::from(history_root),
        &block.auth_data_root(),
    );
    if actual != expected {
        return reject(
            RuleClass::Commitments,
            "the header does not commit to the history root and the authorizing data root",
        );
    }
    Ok(())
}

struct CheckedTransaction {
    sigops: u32,
    miner_fee: Option<Amount<NonNegative>>,
    not_covered: Option<String>,
}

/// zakura-consensus `transaction.rs`, `Verifier::call`, for a block request.
fn check_transaction(
    tx: &Arc<Transaction>,
    height: Height,
    header: &block::Header,
    network: &Network,
    known_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    chain: &HashMap<transparent::OutPoint, transparent::Utxo>,
) -> Result<CheckedTransaction, Reject> {
    check::has_inputs_and_outputs(tx)?;
    check::has_enough_orchard_flags(tx)?;
    check::has_enough_ironwood_flags(tx)?;
    check::orchard_cross_address_disabled(tx)?;
    check::consensus_branch_id(tx, height, network)?;
    check::sapling_point_encodings_are_valid(tx)?;
    block_check::shielded_action_limits_are_valid(std::iter::once(tx), height, network)?;
    if network.is_orchard_temporarily_disabled(height) {
        let None = tx.orchard_shielded_data() else {
            return Err(TransactionError::Other(
                "transaction has Orchard actions (temporarily disabled)".into(),
            )
            .into());
        };
    }
    zakura_consensus::shielded_proof_size_is_canonical(tx)?;

    if tx.is_coinbase() {
        check::coinbase_tx_no_prevout_joinsplit_spend(tx)?;
        check::coinbase_has_no_orchard_shielded_data(tx, height, network)?;
    } else if !tx.is_valid_non_coinbase() {
        return Err(TransactionError::NonCoinbaseHasCoinbaseInput.into());
    }
    if tx.is_coinbase() {
        check::coinbase_expiry_height(&height, tx, network)?;
    } else {
        check::non_coinbase_expiry_height(&height, tx)?;
    }
    check::joinsplit_has_vpub_zero(tx)?;
    check::disabled_add_to_sprout_pool(tx, height, network)?;
    check::disabled_add_to_orchard_pool(tx, height, network)?;
    check::spend_conflicts(tx)?;
    check::lock_time_has_passed(tx, height, header.time)?;

    // `Verifier::spent_utxos`: the outputs of the block first, then the chain.
    let mut spent_utxos = HashMap::new();
    let mut spent_outputs = Vec::new();
    for input in tx.inputs() {
        let transparent::Input::PrevOut { outpoint, .. } = input else {
            continue;
        };
        let utxo = match known_utxos.get(outpoint) {
            Some(ordered) => ordered.utxo.clone(),
            None => match chain.get(outpoint) {
                Some(utxo) => utxo.clone(),
                None => return Err(TransactionError::TransparentInputNotFound.into()),
            },
        };
        spent_outputs.push(utxo.output.clone());
        spent_utxos.insert(*outpoint, utxo);
    }

    // The rule of zakura-state for a block (`check/utxo.rs`, `transparent_spend`), with
    // the same function that the mempool path of the verifier calls.
    for (outpoint, utxo) in &spent_utxos {
        zakura_consensus::state_check::transparent_coinbase_spend(
            *outpoint,
            tx.coinbase_spend_restriction(network, height),
            utxo,
        )?;
    }

    let nu = NetworkUpgrade::current(network, height);
    let cached = CachedFfiTransaction::new(tx.clone(), Arc::new(spent_outputs), nu)
        .map_err(|_| TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu))?;

    let mut not_covered = None;
    match tx.as_ref() {
        Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {
            return Err(TransactionError::WrongVersion.into());
        }
        Transaction::V4 { joinsplit_data, .. } => {
            // `verify_v4_transaction_network_upgrade`.
            let nu7 = matches!(
                NetworkUpgrade::Nu7.activation_height(network),
                Some(nu7_height) if height >= nu7_height
            );
            if nu7 || nu < NetworkUpgrade::Sapling {
                return Err(TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu).into());
            }
            scripts(tx, &cached)?;
            not_covered = joinsplit_data
                .as_ref()
                .map(|_| "Sprout JoinSplit proofs and signature".to_string());
        }
        Transaction::V5 { .. } => {
            // `verify_v5_transaction_network_upgrade`.
            if nu < NetworkUpgrade::Nu5 {
                return Err(TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu).into());
            }
            scripts(tx, &cached)?;
        }
        Transaction::V6 { .. } => {
            // `verify_v6_transaction_network_upgrade`.
            if nu < NetworkUpgrade::Nu6_3 {
                return Err(TransactionError::UnsupportedByNetworkUpgrade(tx.version(), nu).into());
            }
            scripts(tx, &cached)?;
        }
    }
    let sapling = cached.sighasher().sapling_bundle();
    not_covered = not_covered.or(sapling.map(|_| "Sapling proofs and signatures".to_string()));
    let sighash = cached.sighasher().sighash(HashType::ALL, None);
    for bundle in [
        cached.sighasher().orchard_bundle(),
        cached.sighasher().ironwood_bundle(),
    ]
    .into_iter()
    .flatten()
    {
        // `primitives/halo2.rs`, `Item::verify_single`.
        let mut batch = BatchValidator::new(verifying_key(nu));
        let valid = match batch.add_bundle(&bundle, sighash.0) {
            Ok(()) => batch.validate(zk_rand::rng()),
            Err(_) => false,
        };
        if !valid {
            return Err(TransactionError::Halo2VerificationFailed.into());
        }
    }

    let miner_fee = if tx.is_coinbase() {
        None
    } else {
        // `Verifier::miner_fee`.
        let fee = tx
            .value_balance(&spent_utxos)
            .map_err(|_| TransactionError::IncorrectFee)?
            .remaining_transaction_value()
            .map_err(|_| TransactionError::IncorrectFee)?;
        Some(fee)
    };
    let sigops = tx
        .sigops()
        .map_err(|e| Reject(RuleClass::Script, format!("sigop count: {e}")))?;
    Ok(CheckedTransaction {
        sigops: sigops.saturating_add(cached.p2sh_sigops()),
        miner_fee,
        not_covered,
    })
}

/// `verify_transparent_inputs_and_outputs`: every input script of a transaction that is
/// not a coinbase.
fn scripts(tx: &Transaction, cached: &CachedFfiTransaction) -> Result<(), Reject> {
    if tx.is_coinbase() {
        return Ok(());
    }
    for input_index in 0..tx.inputs().len() {
        cached
            .is_valid(input_index)
            .map_err(TransactionError::Script)?;
    }
    Ok(())
}

/// The state rules that the oracle models. Each function names the rule of zakura-state
/// that it follows. zakura-state does not link into this process, and these rules read
/// the state through types of zakura-state.
mod model {
    use super::*;

    /// The block is the child of the tip of the context.
    pub fn parent(block: &Block, height: Height, ctx: &Context) -> Result<(), Reject> {
        if block.header.previous_block_hash.0 != ctx.parent {
            return reject(RuleClass::Header, "the parent of the block is not the tip");
        }
        if height.0 != ctx.height {
            return reject(
                RuleClass::CoinbaseForm,
                format!(
                    "coinbase height {} on a tip at {}",
                    height.0,
                    ctx.height - 1
                ),
            );
        }
        Ok(())
    }

    /// `check/utxo.rs`, `transparent_spend`: each input spends an output of the chain or
    /// of an earlier transaction of the block, and no output is spent two times.
    pub fn transparent_spends(
        block: &Block,
        known_utxos: &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        chain: &HashMap<transparent::OutPoint, transparent::Utxo>,
    ) -> Result<(), Reject> {
        let mut spent = HashSet::new();
        for (index, tx) in block.transactions.iter().enumerate() {
            for outpoint in tx.spent_outpoints() {
                if !spent.insert(outpoint) {
                    return reject(
                        RuleClass::TransparentInput,
                        format!("{outpoint:?} is spent two times in the block"),
                    );
                }
                match known_utxos.get(&outpoint) {
                    Some(created) if created.tx_index_in_block >= index => {
                        return reject(
                            RuleClass::TransparentInput,
                            format!("{outpoint:?} is spent before the block creates it"),
                        );
                    }
                    Some(_) => {}
                    None => {
                        let Some(_) = chain.get(&outpoint) else {
                            return reject(
                                RuleClass::TransparentInput,
                                format!("{outpoint:?} is not an unspent output"),
                            );
                        };
                    }
                }
            }
        }
        Ok(())
    }

    /// `check/nullifier.rs`: no nullifier of the block is in the chain or two times in
    /// the block.
    pub fn nullifiers(block: &Block, ctx: &Context) -> Result<(), Reject> {
        let mut seen: HashSet<(ShieldedPool, [u8; 32])> = ctx.nullifiers.iter().copied().collect();
        for tx in block.transactions.iter() {
            let orchard = tx
                .orchard_nullifiers()
                .map(|n| (ShieldedPool::Orchard, <[u8; 32]>::from(*n)));
            let ironwood = tx
                .ironwood_nullifiers()
                .map(|n| (ShieldedPool::Ironwood, <[u8; 32]>::from(*n)));
            for nullifier in orchard.chain(ironwood) {
                if !seen.insert(nullifier) {
                    return reject(
                        RuleClass::Nullifier,
                        format!("duplicate {:?} nullifier", nullifier.0),
                    );
                }
            }
        }
        Ok(())
    }
}
