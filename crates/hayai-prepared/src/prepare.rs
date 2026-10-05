//! Context-free preparation of one transaction.
//!
//! [`draft`] runs every structural rule and computes every derived value except the
//! transparent script results and the shielded batch verdict; [`Draft::check_input`] runs one
//! script, [`check_scripts`] runs all scripts of many drafts as one parallel array, and
//! [`Draft::add_shielded`] queues the bundles on a batch. [`prepare`] is the sequence a
//! mempool uses for a single transaction.

use std::collections::HashSet;
use std::sync::Arc;

use blake2b_simd::Hash as Blake2bHash;
use hayai_coins::{Coin, CoinsView, OutPoint, Pool};
use hayai_crypto::{
    orchard, sapling_crypto, zcash_primitives, zcash_protocol, zcash_script, zcash_script04,
    zcash_transparent,
};
use hayai_wire::RawTx;
use orchard::tree::MerkleHashOrchard;
use rayon::prelude::*;
use sapling_crypto::Node;
use zcash_primitives::transaction::sighash::{signature_hash, SignableInput};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{
    Authorization, Transaction, TransactionData, TxDigests, TxVersion,
};
use zcash_protocol::consensus::BranchId;
use zcash_protocol::value::{Zatoshis, MAX_MONEY};
use zcash_script::interpreter::CallbackTransactionSignatureChecker;
use zcash_script::opcode::PossiblyBad;
use zcash_script::script::{Code, Raw};
use zcash_script::signature::HashType;
use zcash_script::Opcode;
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{self as transparent, MapAuth};
use zcash_transparent::coinbase::{MAX_COINBASE_SCRIPT_LEN, MIN_COINBASE_SCRIPT_LEN};
use zcash_transparent::sighash::{
    SighashType, SignableInput as TransparentInput, TransparentAuthorizingContext,
};

use hayai_consensus::{RuleSet, ShieldedPools, TX_EXPIRY_HEIGHT_THRESHOLD};

use crate::shielded::ScopedBatch;
use crate::{Commitments, PrepareError, PreparedTx, RuleEpoch};

/// Transparent authorization carrying the spent coins, which the ZIP 243 and ZIP 244
/// sighashes commit to (amounts and scriptPubKeys of every input). Shared slices: the
/// context is cloned into the retyped transaction (`map_authorization`) and held by the
/// [`SighashContext`] as well, and `TransparentAuthorizingContext` hands out owned vectors on
/// every sighash, so the one deep copy is made there and nowhere else.
#[derive(Clone, Debug)]
struct SpentInputs {
    amounts: Arc<[Zatoshis]>,
    scripts: Arc<[Script]>,
}

impl transparent::Authorization for SpentInputs {
    type ScriptSig = Script;
}

impl TransparentAuthorizingContext for SpentInputs {
    fn input_amounts(&self) -> Vec<Zatoshis> {
        self.amounts.to_vec()
    }

    fn input_scriptpubkeys(&self) -> Vec<Script> {
        self.scripts.to_vec()
    }
}

impl MapAuth<transparent::Authorized, SpentInputs> for SpentInputs {
    fn map_script_sig(&self, s: Script) -> Script {
        s
    }

    fn map_authorization(&self, _: transparent::Authorized) -> SpentInputs {
        self.clone()
    }
}

struct Signing;

impl Authorization for Signing {
    type TransparentAuth = SpentInputs;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = orchard::bundle::Authorized;
}

/// The transaction re-typed with its spent coins plus the txid digests, computed once per
/// transaction and shared by every input's sighash and the shielded sighash.
struct SighashContext {
    tx: TransactionData<Signing>,
    parts: TxDigests<Blake2bHash>,
    spent: SpentInputs,
}

fn script_from_bytes(bytes: &[u8]) -> Script {
    Script(zcash_script04::script::Code(bytes.to_vec()))
}

impl SighashContext {
    fn new(tx: &Transaction, spent: &[Coin]) -> Self {
        let spent = SpentInputs {
            amounts: spent
                .iter()
                .map(|c| Zatoshis::const_from_u64(c.value))
                .collect(),
            scripts: spent
                .iter()
                .map(|c| script_from_bytes(&c.script_pubkey))
                .collect(),
        };
        // The upstream sighash API takes the transaction retyped with an authorization that
        // knows the spent coins; `map_authorization` consumes a `TransactionData`, so the
        // parsed transaction is cloned once per preparation.
        let tx = (**tx)
            .clone()
            .map_authorization::<Signing>(spent.clone(), (), ());
        let parts = tx.digest(TxIdDigester);
        Self { tx, parts, spent }
    }

    fn shielded(&self) -> [u8; 32] {
        *signature_hash(&self.tx, &SignableInput::Shielded, &self.parts).as_ref()
    }

    /// The sighash the interpreter asks for while evaluating input `index`. `None` is a
    /// signature failure: an input index out of range or, for v5 and later, a hash type ZIP
    /// 244 does not define.
    fn transparent(
        &self,
        index: usize,
        script_code: &Code,
        hash_type: &HashType,
    ) -> Option<[u8; 32]> {
        let bundle = self.tx.transparent_bundle()?;
        let bits = hash_type.raw_bits() as u8;
        let hash_type = match self.tx.version() {
            TxVersion::V5 | TxVersion::V6 => SighashType::parse(bits)?,
            TxVersion::Sprout(_) | TxVersion::V3 | TxVersion::V4 => SighashType::from_raw(bits),
        };
        let script_code = script_from_bytes(&script_code.0);
        let input = TransparentInput::from_parts(
            bundle,
            hash_type,
            index,
            &script_code,
            self.spent.scripts.get(index)?,
            *self.spent.amounts.get(index)?,
        )
        .ok()?;
        Some(*signature_hash(&self.tx, &SignableInput::Transparent(input), &self.parts).as_ref())
    }
}

/// A transaction prepared up to, but not including, its script and shielded verdicts.
///
/// The prepared transaction is shared ([`Draft::shared`]) so that a block validator can
/// run the contextual check on it while the scripts and the shielded batch of the same
/// drafts are still running.
pub struct Draft {
    tx: Arc<PreparedTx>,
    sighash: SighashContext,
}

/// The version rule: the rule set of the epoch allows the transaction version, and the
/// transaction has the branch id of the epoch. A version that the rule set allows and that
/// hayai does not verify (v1 to v3) is [`PrepareError::Unsupported`]: Zakura rejects these
/// versions in its verifier too (`zakura-consensus/src/transaction.rs`, `WrongVersion`),
/// because every block that holds one is at or below the mandatory checkpoint. hayai has
/// no sighash for v1 and v2 and no verifier for the BCTV14 proofs of v2 and v3. The
/// checkpoint path applies such a block without this function.
fn check_version(tx: &Transaction, epoch: RuleEpoch, rules: &RuleSet) -> Result<(), PrepareError> {
    // The parser gives `Sprout(n)` to every transaction without the Overwinter flag. Only
    // the numbers 1 and 2 are versions of that format.
    let number = match tx.version() {
        TxVersion::Sprout(n @ (1 | 2)) => Some(n),
        TxVersion::Sprout(_) => None,
        TxVersion::V3 => Some(3),
        TxVersion::V4 => Some(4),
        TxVersion::V5 => Some(5),
        TxVersion::V6 => Some(6),
    };
    let Some(number) = number.filter(|n| rules.tx_versions.allows(*n)) else {
        return Err(PrepareError::Version(
            format!("{:?}", tx.version()),
            epoch.branch_id,
        ));
    };
    if tx.consensus_branch_id() != epoch.branch_id {
        return Err(PrepareError::BranchId {
            tx: tx.consensus_branch_id(),
            epoch: epoch.branch_id,
        });
    }
    if number < 4 {
        return Err(PrepareError::Unsupported(
            "transaction version before Sapling: only the checkpoint path applies it",
        ));
    }
    Ok(())
}

/// The pool rule: a transaction has a bundle only for a pool that the rule set of the epoch
/// names (`RuleSet::pools`). The transaction formats and the version rule give the same
/// result for the rule sets up to NU6.3. The rule does not depend on that.
fn check_pools(
    tx: &Transaction,
    pools: ShieldedPools,
    branch: BranchId,
) -> Result<(), PrepareError> {
    for (present, active, pool) in [
        (tx.sprout_bundle().map(|_| ()), pools.sprout, Pool::Sprout),
        (
            tx.sapling_bundle().map(|_| ()),
            pools.sapling,
            Pool::Sapling,
        ),
        (
            tx.orchard_bundle().map(|_| ()),
            pools.orchard,
            Pool::Orchard,
        ),
        (
            tx.ironwood_bundle().map(|_| ()),
            pools.ironwood,
            Pool::Ironwood,
        ),
    ] {
        if let (Some(()), false) = (present, active) {
            return Err(PrepareError::PoolNotActive(pool, branch));
        }
    }
    Ok(())
}

/// Sum of a transaction's transparent inputs minus outputs plus the shielded value balances
/// (Sapling, Orchard, Ironwood) and the value of its JoinSplits (`vpub_new` enters the
/// transparent value pool of the transaction, `vpub_old` leaves it),
/// checked against `MAX_MONEY` at every step as zcashd's `CheckTransaction` does.
fn fee(tx: &Transaction, spent: &[Coin]) -> Result<u64, PrepareError> {
    let sum = |values: &mut dyn Iterator<Item = u64>| -> Result<u64, PrepareError> {
        let mut total = 0u64;
        for v in values {
            if v > MAX_MONEY {
                return Err(PrepareError::ValueOverflow);
            }
            total = total.checked_add(v).ok_or(PrepareError::ValueOverflow)?;
            if total > MAX_MONEY {
                return Err(PrepareError::ValueOverflow);
            }
        }
        Ok(total)
    };
    let inputs = sum(&mut spent.iter().map(|c| c.value))?;
    let joinsplits = || tx.sprout_bundle().into_iter().flat_map(|b| &b.joinsplits);
    let vpub = |value| u64::try_from(value).expect("the parser reads vpub as an amount");
    // zcashd adds each `vpub_old` to the output total and each `vpub_new` to the input
    // total, with the range check at every step.
    let outputs = sum(&mut tx
        .transparent_bundle()
        .into_iter()
        .flat_map(|b| b.vout.iter().map(|o| o.value().into_u64()))
        .chain(joinsplits().map(|j| vpub(j.vpub_old()))))?;
    let from_sprout = sum(&mut joinsplits().map(|j| vpub(j.vpub_new())))?;
    let mut balance = i128::from(inputs) + i128::from(from_sprout) - i128::from(outputs);
    if let Some(b) = tx.sapling_bundle() {
        balance += i128::from(i64::from(*b.value_balance()));
    }
    if let Some(b) = tx.orchard_bundle() {
        balance += i128::from(i64::from(*b.value_balance()));
    }
    if let Some(b) = tx.ironwood_bundle() {
        balance += i128::from(i64::from(*b.value_balance()));
    }
    if balance < 0 {
        return Err(PrepareError::NegativeFee);
    }
    if balance > i128::from(MAX_MONEY) {
        return Err(PrepareError::ValueOverflow);
    }
    Ok(balance as u64)
}

/// `OP_HASH160 <20 bytes> OP_EQUAL`.
fn is_pay_to_script_hash(script: &[u8]) -> bool {
    script.len() == 23 && script[0] == 0xa9 && script[1] == 0x14 && script[22] == 0x87
}

/// The data of the last push of a push-only script, which is the redeem script of a P2SH
/// spend. Mirrors zcashd's `CScript::GetSigOpCount(const CScript& scriptSig)`: a parse error
/// or a non-push opcode means no redeem script (zero sigops).
fn last_push(script_sig: &[u8]) -> Option<Vec<u8>> {
    let mut last = None;
    for op in Code(script_sig.to_vec()).parse() {
        match op {
            Ok(PossiblyBad::Good(Opcode::PushValue(pv))) => last = Some(pv.value()),
            _ => return None,
        }
    }
    last
}

/// Block-level sigop count as zcashd counts it: `GetLegacySigOpCount` over the transaction's
/// own scriptSigs and scriptPubKeys (CHECKMULTISIG counted as 20) plus, for non-coinbase
/// transactions, `GetP2SHSigOpCount` over the redeem scripts of inputs spending P2SH outputs
/// (CHECKMULTISIG counted by its key count). The counting itself is upstream
/// `zcash_script::script::Code::sig_op_count`.
fn sigops(tx: &Transaction, spent: &[Coin], is_coinbase: bool) -> u32 {
    let Some(bundle) = tx.transparent_bundle() else {
        return 0;
    };
    let mut count = 0u32;
    for txin in &bundle.vin {
        count += Code(txin.script_sig().0 .0.clone()).sig_op_count(false);
    }
    for txout in &bundle.vout {
        count += Code(txout.script_pubkey().0 .0.clone()).sig_op_count(false);
    }
    if is_coinbase {
        return count;
    }
    for (txin, coin) in bundle.vin.iter().zip(spent) {
        if !is_pay_to_script_hash(&coin.script_pubkey) {
            continue;
        }
        if let Some(redeem) = last_push(&txin.script_sig().0 .0) {
            count += Code(redeem).sig_op_count(true);
        }
    }
    count
}

/// Runs every structural rule and derives every context-free value of `raw` under `epoch`,
/// given the coins its inputs spend (in input order; empty for a coinbase). A list of
/// another length is [`PrepareError::SpentCoins`]. Scripts and shielded bundles are left
/// to [`check_scripts`] and [`Draft::add_shielded`].
pub fn draft(raw: RawTx, epoch: RuleEpoch, spent: Vec<Coin>) -> Result<Draft, PrepareError> {
    let tx = &raw.tx;
    let Ok(rules) = RuleSet::of_branch(epoch.branch_id) else {
        return Err(PrepareError::Unsupported(
            "consensus branch without a rule set",
        ));
    };
    check_version(tx, epoch, rules)?;
    let transparent = tx.transparent_bundle();
    let sapling = tx.sapling_bundle();
    let orchard = tx.orchard_bundle();
    let ironwood = tx.ironwood_bundle();
    let pools = rules.pools;
    check_pools(tx, pools, epoch.branch_id)?;
    let sprout = tx.sprout_bundle();
    let is_coinbase = matches!(transparent, Some(b) if b.is_coinbase());
    let vin_len = transparent.map_or(0, |b| b.vin.len());
    let vout_len = transparent.map_or(0, |b| b.vout.len());
    let sapling_spends = sapling.map_or(0, |b| b.shielded_spends().len());
    let sapling_outputs = sapling.map_or(0, |b| b.shielded_outputs().len());
    let joinsplits = sprout.map_or(0, |b| b.joinsplits.len());
    // A bundle of the Orchard protocol always has one action or more.
    let orchard_actions = orchard.map_or(0, |b| b.actions().len());
    let ironwood_actions = ironwood.map_or(0, |b| b.actions().len());
    let orchard_spends = matches!(orchard, Some(b) if b.flags().spends_enabled());
    let orchard_outputs = matches!(orchard, Some(b) if b.flags().outputs_enabled());
    let ironwood_spends = matches!(ironwood, Some(b) if b.flags().spends_enabled());
    let ironwood_outputs = matches!(ironwood, Some(b) if b.flags().outputs_enabled());

    // §7.1.2: `tx_in_count > 0 or nSpendsSapling > 0 or (nActionsOrchard > 0 and
    // enableSpendsOrchard = 1) or (nActionsIronwood > 0 and enableSpendsIronwood = 1)`,
    // and the same rule for the outputs (Zakura `has_inputs_and_outputs`,
    // `zakura-consensus/src/transaction/check.rs:131`). A JoinSplit is a source and a sink
    // (Zakura `has_shielded_inputs`, `has_shielded_outputs`).
    if vin_len == 0 && sapling_spends == 0 && joinsplits == 0 && !orchard_spends && !ironwood_spends
    {
        return Err(PrepareError::NoSource);
    }
    if vout_len == 0
        && sapling_outputs == 0
        && joinsplits == 0
        && !orchard_outputs
        && !ironwood_outputs
    {
        return Err(PrepareError::NoSink);
    }
    // §7.1.2: a v4 transaction with no Sapling spends or outputs has valueBalanceSapling 0
    // (zcashd `bad-txns-valuebalance-nonzero`). The parsed form has lost the field.
    if let Some(balance) = raw.v4_value_balance_without_components() {
        if balance != 0 {
            return Err(PrepareError::V4ValueBalance(balance));
        }
    }
    // §7.1.2 [NU5 onward]: Orchard actions require enableSpends or enableOutputs (Zebra
    // `has_enough_orchard_flags`). [NU6.3 onward]: the same rule for Ironwood actions
    // (Zakura `has_enough_ironwood_flags`, `check.rs:165`).
    if orchard_actions > 0 && !orchard_spends && !orchard_outputs {
        return Err(PrepareError::OrchardFlags);
    }
    if ironwood_actions > 0 && !ironwood_spends && !ironwood_outputs {
        return Err(PrepareError::IronwoodFlags);
    }
    // The rules of the Orchard pool from the activation of the Ironwood pool (NU6.3).
    if let (true, Some(b)) = (pools.ironwood, orchard) {
        // `enableCrossAddress` is 0 (Zakura `orchard_cross_address_disabled`,
        // `check.rs:179`). The parser applies the same rule to the flag byte.
        if b.flags().cross_address_enabled() {
            return Err(PrepareError::OrchardCrossAddress);
        }
        // `valueBalanceOrchard` is not negative: no value enters the Orchard pool (Zakura
        // `disabled_add_to_orchard_pool`, `check.rs:338`).
        let balance = i64::from(*b.value_balance());
        if balance < 0 {
            return Err(PrepareError::OrchardPoolDeposit(balance));
        }
    }
    if is_coinbase {
        // Zakura `coinbase_tx_no_prevout_joinsplit_spend`, `check.rs:251`.
        if joinsplits > 0 {
            return Err(PrepareError::CoinbaseJoinSplit);
        }
        if sapling_spends > 0 || orchard_spends || ironwood_spends {
            return Err(PrepareError::CoinbaseShieldedSpend);
        }
        // From NU6.3 a shielded coinbase output is an Ironwood output (Zakura
        // `coinbase_has_no_orchard_shielded_data`, `check.rs:367`).
        if let (false, Some(_)) = (rules.coinbase.orchard_bundle, orchard) {
            return Err(PrepareError::CoinbaseOrchardBundle);
        }
        crate::coinbase::check_shielded_outputs(tx, epoch.branch_id)?;
        let script_len = transparent
            .and_then(|b| b.vin.first())
            .map_or(0, |txin| txin.script_sig().0 .0.len());
        if !(MIN_COINBASE_SCRIPT_LEN..=MAX_COINBASE_SCRIPT_LEN).contains(&script_len) {
            return Err(PrepareError::CoinbaseScriptLength(script_len));
        }
        if !spent.is_empty() {
            return Err(PrepareError::SpentCoins {
                inputs: 0,
                coins: spent.len(),
            });
        }
    } else {
        if spent.len() != vin_len {
            return Err(PrepareError::SpentCoins {
                inputs: vin_len,
                coins: spent.len(),
            });
        }
        let mut seen: HashSet<&OutPoint, ahash::RandomState> = HashSet::default();
        for (i, txin) in transparent.into_iter().flat_map(|b| &b.vin).enumerate() {
            if *txin.prevout() == OutPoint::NULL {
                return Err(PrepareError::NullPrevout(i));
            }
            if !seen.insert(txin.prevout()) {
                return Err(PrepareError::DuplicateInput(i));
            }
        }
    }
    let expiry_height = u32::from(tx.expiry_height());
    // zcashd `CheckTransactionWithoutProofVerification`, "tx-expiry-height-too-high".
    if expiry_height >= TX_EXPIRY_HEIGHT_THRESHOLD {
        return Err(PrepareError::ExpiryTooHigh(expiry_height));
    }

    let fee = if is_coinbase { 0 } else { fee(tx, &spent)? };
    let sigops = sigops(tx, &spent, is_coinbase);

    let mut nullifiers =
        Vec::with_capacity(2 * joinsplits + sapling_spends + orchard_actions + ironwood_actions);
    let mut anchors: Vec<(Pool, [u8; 32])> = Vec::new();
    let mut commitments = Commitments::default();
    if let Some(b) = sprout {
        let (revealed, added) = crate::sprout::check_joinsplits(b, rules)?;
        nullifiers.extend(revealed.into_iter().map(|nf| (Pool::Sprout, nf)));
        commitments.sprout = added;
    }
    if let Some(b) = sapling {
        let mut seen: HashSet<[u8; 32], ahash::RandomState> = HashSet::default();
        for spend in b.shielded_spends() {
            let nf = spend.nullifier().0;
            if !seen.insert(nf) {
                return Err(PrepareError::DuplicateNullifier(Pool::Sapling));
            }
            nullifiers.push((Pool::Sapling, nf));
            let anchor = (Pool::Sapling, spend.anchor().to_bytes());
            if !anchors.contains(&anchor) {
                anchors.push(anchor);
            }
        }
        commitments.sapling = b
            .shielded_outputs()
            .iter()
            .map(|o| Node::from_cmu(o.cmu()))
            .collect();
    }
    // The Orchard and the Ironwood bundle have one form. The two nullifier sets, the two
    // trees and the two anchor sets are distinct.
    for (bundle, pool) in [(orchard, Pool::Orchard), (ironwood, Pool::Ironwood)] {
        let Some(b) = bundle else {
            continue;
        };
        let mut seen: HashSet<[u8; 32], ahash::RandomState> = HashSet::default();
        for action in b.actions() {
            let nf = action.nullifier().to_bytes();
            if !seen.insert(nf) {
                return Err(PrepareError::DuplicateNullifier(pool));
            }
            nullifiers.push((pool, nf));
        }
        anchors.push((pool, b.anchor().to_bytes()));
        let leaves = b
            .actions()
            .iter()
            .map(|a| MerkleHashOrchard::from_cmx(a.cmx()))
            .collect();
        match pool {
            Pool::Orchard => commitments.orchard = leaves,
            Pool::Ironwood => commitments.ironwood = leaves,
            Pool::Sprout | Pool::Sapling => unreachable!("the loop names two pools"),
        }
    }

    let sighash = SighashContext::new(tx, &spent);
    let lock_time = tx.lock_time();
    let shielded_ok = matches!(
        (sprout, sapling, orchard, ironwood),
        (None, None, None, None)
    );
    Ok(Draft {
        tx: Arc::new(PreparedTx {
            raw: Arc::new(raw),
            epoch,
            spent,
            fee,
            sigops,
            nullifiers,
            commitments,
            anchors,
            orchard_actions: u32::try_from(orchard_actions).expect("fits"),
            ironwood_actions: u32::try_from(ironwood_actions).expect("fits"),
            sapling_ios: u32::try_from(sapling_spends + sapling_outputs).expect("fits"),
            joinsplits: u32::try_from(joinsplits).expect("fits"),
            scripts_ok: false,
            shielded_ok,
            is_coinbase,
            expiry_height,
            lock_time,
        }),
        sighash,
    })
}

impl Draft {
    pub fn tx(&self) -> &PreparedTx {
        &self.tx
    }

    /// The prepared transaction, shared; its `scripts_ok` and `shielded_ok` stay false.
    pub fn shared(&self) -> &Arc<PreparedTx> {
        &self.tx
    }

    /// Number of transparent inputs with a script to check (zero for a coinbase).
    pub fn input_count(&self) -> usize {
        self.tx.spent.len()
    }

    /// Evaluates `scriptSig || scriptPubKey` of input `index` with the upstream interpreter
    /// under the epoch's flags.
    pub fn check_input(&self, index: usize) -> Result<(), PrepareError> {
        let Some(bundle) = self.tx.raw.tx.transparent_bundle() else {
            unreachable!("inputs imply a transparent bundle");
        };
        let txin = &bundle.vin[index];
        let coin = &self.tx.spent[index];
        let sighash =
            |code: &Code, hash_type: &HashType| self.sighash.transparent(index, code, hash_type);
        let checker = CallbackTransactionSignatureChecker {
            sighash: &sighash,
            lock_time: i64::from(self.tx.lock_time),
            is_final: txin.sequence() == u32::MAX,
        };
        let script =
            Raw::from_raw_parts(txin.script_sig().0 .0.clone(), coin.script_pubkey.to_vec());
        match script.eval(self.tx.epoch.script_flags, &checker) {
            Ok(true) => Ok(()),
            Ok(false) => Err(PrepareError::Script(index, "evaluated to false".into())),
            Err((component, error)) => Err(PrepareError::Script(
                index,
                format!("{component:?}: {error:?}"),
            )),
        }
    }

    /// Queues the shielded bundles on `batch` under the transaction's shielded sighash.
    pub fn add_shielded(&self, batch: &mut ScopedBatch<'_>) -> Result<(), PrepareError> {
        if !self.tx.has_shielded() {
            return Ok(());
        }
        batch.add(
            self.tx.wtxid(),
            self.tx.raw.tx.clone(),
            self.sighash.shielded(),
        )
    }

    /// The prepared transaction, with its scripts recorded as verified. Call only after
    /// every input passed [`Draft::check_input`] (as [`check_scripts`] guarantees).
    pub fn finish(self) -> PreparedTx {
        let mut tx = Arc::unwrap_or_clone(self.tx);
        tx.scripts_ok = true;
        tx
    }
}

/// Inputs below which a script array is evaluated on the calling thread.
const PARALLEL_MIN_INPUTS: usize = 8;

/// Evaluates every transparent input of every draft as one flat parallel array. The error
/// names the draft index and the failing input.
pub fn check_scripts(drafts: &[Draft]) -> Result<(), (usize, PrepareError)> {
    let inputs: Vec<(usize, usize)> = drafts
        .iter()
        .enumerate()
        .flat_map(|(t, d)| (0..d.input_count()).map(move |i| (t, i)))
        .collect();
    let check = |&(t, i): &(usize, usize)| drafts[t].check_input(i).map_err(|e| (t, e));
    if inputs.len() < PARALLEL_MIN_INPUTS {
        inputs.iter().try_for_each(check)
    } else {
        inputs.par_iter().try_for_each(check)
    }
}

/// Prepares one transaction: fetches its spent coins from `coins` in one call, runs every
/// context-free rule and script, and queues its shielded bundles on `batch`. The batch
/// verdict arrives with [`ScopedBatch::finalize`].
pub fn prepare(
    raw: RawTx,
    epoch: RuleEpoch,
    coins: &dyn CoinsView,
    batch: &mut ScopedBatch<'_>,
) -> Result<PreparedTx, PrepareError> {
    let transparent = raw.tx.transparent_bundle();
    let spent = match transparent {
        Some(b) if !b.is_coinbase() => {
            let outpoints: Vec<OutPoint> = b.vin.iter().map(|i| i.prevout().clone()).collect();
            let mut spent = Vec::with_capacity(outpoints.len());
            for (i, coin) in coins.get_coins(&outpoints).into_iter().enumerate() {
                let Some(coin) = coin else {
                    return Err(PrepareError::MissingInput(i));
                };
                spent.push(coin);
            }
            spent
        }
        _ => Vec::new(),
    };
    let d = draft(raw, epoch, spent)?;
    check_scripts(std::slice::from_ref(&d)).map_err(|(_, e)| e)?;
    d.add_shielded(batch)?;
    Ok(d.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use hayai_crypto::{zcash_primitives, zcash_protocol, zcash_transparent};
    use zcash_primitives::transaction::Authorized;
    use zcash_protocol::consensus::BlockHeight;
    use zcash_transparent::bundle::{Authorized as TAuthorized, Bundle, TxIn, TxOut};

    use crate::sprout::tests::joinsplit_tx;

    const OP_2: u8 = 0x52;
    const OP_CHECKSIG: u8 = 0xac;
    const OP_CHECKMULTISIG: u8 = 0xae;

    fn build(vin: Vec<TxIn<TAuthorized>>, outputs: &[(Vec<u8>, u64)]) -> Transaction {
        let vout = outputs
            .iter()
            .map(|(script, value)| {
                TxOut::new(Zatoshis::const_from_u64(*value), script_from_bytes(script))
            })
            .collect();
        TransactionData::<Authorized>::from_parts(
            TxVersion::V5,
            BranchId::Nu6_2,
            0,
            BlockHeight::from_u32(0),
            Some(Bundle {
                vin,
                vout,
                authorization: TAuthorized,
            }),
            None,
            None,
            None,
        )
        .freeze()
        .expect("v5 freezes")
    }

    fn txin(prevout: OutPoint, script_sig: &[u8]) -> TxIn<TAuthorized> {
        TxIn::from_parts(prevout, script_from_bytes(script_sig), u32::MAX)
    }

    /// A transaction with one distinct prevout per `(scriptSig, spent value)` input.
    fn tx(inputs: &[(Vec<u8>, u64)], outputs: &[(Vec<u8>, u64)]) -> (Transaction, Vec<Coin>) {
        let vin = inputs
            .iter()
            .enumerate()
            .map(|(i, (script_sig, _))| txin(OutPoint::new([i as u8 + 1; 32], 0), script_sig))
            .collect();
        let coins = inputs
            .iter()
            .map(|(_, value)| Coin {
                value: *value,
                script_pubkey: Bytes::from_static(&[OP_CHECKSIG]),
                height: 1,
                is_coinbase: false,
            })
            .collect();
        (build(vin, outputs), coins)
    }

    fn raw(tx: &Transaction) -> RawTx {
        let mut bytes = Vec::new();
        tx.write(&mut bytes).expect("vec write");
        RawTx::parse(Bytes::from(bytes), BranchId::Nu6_2).expect("round trip")
    }

    #[test]
    fn coinbase_script_length_and_null_prevouts() {
        let epoch = RuleEpoch::consensus(BranchId::Nu6_2);
        let (_, coins) = tx(&[(vec![], 10), (vec![], 10)], &[(vec![], 1)]);
        let t = build(
            vec![
                txin(OutPoint::new([1; 32], 0), &[]),
                txin(OutPoint::NULL, &[]),
            ],
            &[(vec![], 1)],
        );
        let Err(PrepareError::NullPrevout(1)) = draft(raw(&t), epoch, coins).map(|_| ()) else {
            panic!("a null prevout outside the coinbase slot is rejected");
        };
        let coinbase =
            |script_sig: &[u8]| build(vec![txin(OutPoint::NULL, script_sig)], &[(vec![], 1)]);
        draft(raw(&coinbase(&[1, 2, 3, 4])), epoch, Vec::new())
            .expect("a 4-byte coinbase scriptSig is fine");
        let Err(PrepareError::CoinbaseScriptLength(1)) =
            draft(raw(&coinbase(&[1])), epoch, Vec::new()).map(|_| ())
        else {
            panic!("a 1-byte coinbase scriptSig is rejected");
        };
        let Err(PrepareError::CoinbaseScriptLength(101)) =
            draft(raw(&coinbase(&[0x51; 101])), epoch, Vec::new()).map(|_| ())
        else {
            panic!("a 101-byte coinbase scriptSig is rejected");
        };
    }

    /// `draft` takes the spent coins from its caller. A list that does not match the
    /// transaction is an error, not a panic: a transaction of a peer can be in the place
    /// of a coinbase.
    #[test]
    fn a_wrong_number_of_spent_coins_is_an_error() {
        let epoch = RuleEpoch::consensus(BranchId::Nu6_2);
        let (t, coins) = tx(&[(vec![], 10), (vec![], 10)], &[(vec![], 1)]);
        let result = |spent: Vec<Coin>| draft(raw(&t), epoch, spent).map(|_| ());
        assert_eq!(
            result(Vec::new()),
            Err(PrepareError::SpentCoins {
                inputs: 2,
                coins: 0
            })
        );
        assert_eq!(
            result(coins[..1].to_vec()),
            Err(PrepareError::SpentCoins {
                inputs: 2,
                coins: 1
            })
        );
        let mut three = coins.clone();
        three.push(coins[0].clone());
        assert_eq!(
            result(three),
            Err(PrepareError::SpentCoins {
                inputs: 2,
                coins: 3
            })
        );
        assert_eq!(result(coins.clone()), Ok(()));
        // A coinbase spends no coin.
        let coinbase = build(vec![txin(OutPoint::NULL, &[1, 2, 3, 4])], &[(vec![], 1)]);
        assert_eq!(
            draft(raw(&coinbase), epoch, coins[..1].to_vec()).map(|_| ()),
            Err(PrepareError::SpentCoins {
                inputs: 0,
                coins: 1
            })
        );
    }

    /// A transaction of `version` and `branch` with one input, one output and the expiry
    /// height `expiry`, and the coin that it spends.
    fn with_expiry(version: TxVersion, branch: BranchId, expiry: u32) -> (Transaction, Vec<Coin>) {
        let (template, coins) = tx(&[(vec![], 10)], &[(vec![], 1)]);
        let bundle = template.transparent_bundle().expect("a bundle").clone();
        let t = TransactionData::<Authorized>::from_parts(
            version,
            branch,
            0,
            BlockHeight::from_u32(expiry),
            Some(bundle),
            None,
            None,
            None,
        )
        .freeze()
        .expect("the version has a transparent bundle");
        (t, coins)
    }

    /// ZIP 2003 at the NU7 boundary: the rule set of NU6.3 allows the versions 4, 5 and 6,
    /// and the rule set of NU7 allows 5 and 6 only (Zakura
    /// `zakura-consensus/src/transaction.rs:1023-1040`). A v4 transaction has no branch id
    /// in its bytes, so the version rule is the rule that refuses it.
    #[test]
    fn a_v4_transaction_is_refused_from_nu7() {
        use hayai_consensus::{TxVersions, Upgrade};

        let nu6_3 = BranchId::Nu6_3;
        let before = RuleSet::of(Upgrade::Nu6_3).expect("a rule set");
        // The version rule of NU7 on a transaction of NU6.3: the rule does not depend on
        // the crypto backend.
        let from_nu7 = RuleSet {
            tx_versions: TxVersions::of(&[5, 6]),
            ..*before
        };
        let epoch = RuleEpoch::consensus(nu6_3);
        // The transaction as the parser gives it for a block of the epoch `branch`.
        let parsed = |t: &Transaction, branch: BranchId| {
            let mut bytes = Vec::new();
            t.write(&mut bytes).expect("vec write");
            parse(&bytes, branch).expect("the transaction parses")
        };
        let (v4, coins4) = with_expiry(TxVersion::V4, nu6_3, 0);
        let (v5, coins5) = with_expiry(TxVersion::V5, nu6_3, 0);
        for t in [&v4, &v5] {
            assert_eq!(check_version(t, epoch, before), Ok(()));
        }
        assert_eq!(check_version(&v5, epoch, &from_nu7), Ok(()));
        let Err(PrepareError::Version(_, BranchId::Nu6_3)) = check_version(&v4, epoch, &from_nu7)
        else {
            panic!("a v4 transaction under the versions of NU7");
        };
        draft(parsed(&v4, nu6_3), epoch, coins4.clone())
            .map(|_| ())
            .expect("v4 in NU6.3");

        // With the NU7 rule set: the same bytes on both sides of the NU7 activation.
        let Some(nu7) = hayai_crypto::nu7_branch() else {
            return;
        };
        let rules = RuleSet::of(Upgrade::Nu7).expect("the backend has the NU7 branch id");
        assert_eq!(rules.tx_versions, from_nu7.tx_versions);
        let nu7_epoch = RuleEpoch::of(rules);
        let reparsed = parse(&raw(&v4).bytes, nu7).expect("a v4 transaction parses");
        let Err(PrepareError::Version(_, branch)) = draft(reparsed, nu7_epoch, coins4).map(|_| ())
        else {
            panic!("a v4 transaction in NU7");
        };
        assert_eq!(branch, nu7);
        // A v5 transaction of NU7 passes, and a v5 transaction of NU6.3 has the wrong
        // branch id.
        let (v5_nu7, coins) = with_expiry(TxVersion::V5, nu7, 0);
        draft(parsed(&v5_nu7, nu7), nu7_epoch, coins)
            .map(|_| ())
            .expect("v5 in NU7");
        let Err(PrepareError::BranchId { tx, epoch }) =
            draft(parsed(&v5, nu7), nu7_epoch, coins5).map(|_| ())
        else {
            panic!("a transaction of NU6.3 in NU7");
        };
        assert_eq!((tx, epoch), (nu6_3, nu7));
    }

    /// zcashd `tx-expiry-height-too-high`: an expiry height is below 500,000,000.
    #[test]
    fn an_expiry_height_is_below_the_threshold() {
        let epoch = RuleEpoch::consensus(BranchId::Nu6_2);
        let result = |expiry: u32| {
            let (t, coins) = with_expiry(TxVersion::V5, BranchId::Nu6_2, expiry);
            draft(raw(&t), epoch, coins).map(|d| d.tx().expiry_height)
        };
        assert_eq!(TX_EXPIRY_HEIGHT_THRESHOLD, 500_000_000);
        assert_eq!(result(0), Ok(0));
        assert_eq!(result(499_999_999), Ok(499_999_999));
        assert_eq!(
            result(500_000_000),
            Err(PrepareError::ExpiryTooHigh(500_000_000))
        );
        assert_eq!(result(u32::MAX), Err(PrepareError::ExpiryTooHigh(u32::MAX)));
    }

    /// Protocol specification §7.1.2: a v4 transaction with no Sapling spend and no
    /// Sapling output has `valueBalanceSapling` 0 (zcashd `bad-txns-valuebalance-nonzero`).
    #[test]
    fn a_v4_value_balance_without_sapling_components_is_zero() {
        let epoch = RuleEpoch::consensus(BranchId::Canopy);
        let (t, coins) = with_expiry(TxVersion::V4, BranchId::Canopy, 0);
        let mut bytes = Vec::new();
        t.write(&mut bytes).expect("vec write");
        // The field is the 8 bytes before the two empty Sapling counts and the empty
        // JoinSplit count.
        let at = bytes.len() - 3 - 8;
        assert_eq!(bytes[at..], [0u8; 11]);
        let result = |balance: i64| {
            let mut bytes = bytes.clone();
            bytes[at..at + 8].copy_from_slice(&balance.to_le_bytes());
            let raw = RawTx::parse(Bytes::from(bytes), BranchId::Canopy).expect("parses");
            draft(raw, epoch, coins.clone()).map(|_| ())
        };
        assert_eq!(result(0), Ok(()));
        assert_eq!(result(1), Err(PrepareError::V4ValueBalance(1)));
        assert_eq!(result(-1), Err(PrepareError::V4ValueBalance(-1)));
    }

    /// `OP_HASH160 <20 bytes> OP_EQUAL`; the counting looks at the pattern, not the hash.
    fn p2sh(tag: u8) -> Vec<u8> {
        let mut script = vec![0xa9, 0x14];
        script.extend_from_slice(&[tag; 20]);
        script.push(0x87);
        script
    }

    #[test]
    fn fee_is_inputs_minus_outputs_with_bounds() {
        let (t, coins) = tx(&[(vec![], 1_000), (vec![], 2_000)], &[(vec![], 2_500)]);
        assert_eq!(fee(&t, &coins), Ok(500));
        let (t, coins) = tx(&[(vec![], 1_000)], &[(vec![], 1_001)]);
        assert_eq!(fee(&t, &coins), Err(PrepareError::NegativeFee));
        let (t, coins) = tx(&[(vec![], MAX_MONEY), (vec![], 1)], &[(vec![], 1)]);
        assert_eq!(fee(&t, &coins), Err(PrepareError::ValueOverflow));
        let (t, mut coins) = tx(&[(vec![], 1)], &[(vec![], 1)]);
        coins[0].value = MAX_MONEY + 1;
        assert_eq!(fee(&t, &coins), Err(PrepareError::ValueOverflow));
    }

    #[test]
    fn sigops_count_legacy_and_p2sh_like_zcashd() {
        // A bare CHECKMULTISIG in a scriptSig counts 20; CHECKSIG outputs count 1 each.
        let (t, coins) = tx(
            &[(vec![OP_CHECKMULTISIG], 10)],
            &[(vec![OP_CHECKSIG], 1), (vec![OP_CHECKSIG], 1)],
        );
        assert_eq!(sigops(&t, &coins, false), 22);
        // A P2SH input adds the accurate count of its redeem script (2-of-2 = 2 sigops);
        // the scriptSig is the pushed redeem script, counted as 0 legacy sigops itself.
        let mut redeem = vec![OP_2];
        for _ in 0..2 {
            redeem.push(33);
            redeem.extend_from_slice(&[0x02; 33]);
        }
        redeem.extend_from_slice(&[OP_2, OP_CHECKMULTISIG]);
        let mut script_sig = vec![redeem.len() as u8];
        script_sig.extend_from_slice(&redeem);
        let (t, mut coins) = tx(&[(script_sig, 10)], &[(vec![OP_CHECKSIG], 1)]);
        assert_eq!(
            sigops(&t, &coins, false),
            1,
            "non-P2SH coin: only the output"
        );
        coins[0].script_pubkey = Bytes::from(p2sh(1));
        assert_eq!(sigops(&t, &coins, false), 3);
        assert_eq!(sigops(&t, &coins, true), 1, "coinbase skips P2SH counting");
        // A non-push opcode in the scriptSig means no redeem script.
        let (t, mut coins) = tx(&[(vec![OP_2, OP_CHECKSIG], 10)], &[(vec![], 1)]);
        coins[0].script_pubkey = Bytes::from(p2sh(1));
        assert_eq!(sigops(&t, &coins, false), 1);
        assert_eq!(last_push(&[OP_2, OP_CHECKSIG]), None);
        assert_eq!(last_push(&[1, 7, 2, 8, 9]), Some(vec![8, 9]));
    }

    #[test]
    fn p2sh_pattern_is_exact() {
        assert!(is_pay_to_script_hash(&p2sh(9)));
        assert!(!is_pay_to_script_hash(&[0xa9, 0x14]));
        let mut longer = p2sh(9);
        longer.push(0x87);
        assert!(!is_pay_to_script_hash(&longer));
    }

    // ----- The rules of the Orchard and Ironwood bundles -----

    use hayai_crypto::orchard::builder::{Builder as OrchardBuilder, BundleType};
    use hayai_crypto::orchard::bundle::{Authorized as OrchardAuthorized, BundleVersion, Flags};
    use hayai_crypto::orchard::keys::{FullViewingKey, OutgoingViewingKey, Scope, SpendingKey};
    use hayai_crypto::orchard::value::NoteValue;
    use hayai_crypto::rng::seeded;
    use zcash_protocol::value::ZatBalance;

    type OrchardBundle = orchard::Bundle<OrchardAuthorized, ZatBalance>;

    /// Bytes of one action on the wire: cv, nullifier, rk, cmx, epk and the two ciphertexts.
    const ACTION_BYTES: usize = 5 * 32 + 580 + 80;
    const SPENDS: u8 = 0b001;
    const OUTPUTS: u8 = 0b010;
    const CROSS_ADDRESS: u8 = 0b100;

    /// A bundle whose only requested actions are padding: two actions, value balance 0.
    const PADDING: BundleType = BundleType::Transactional {
        bundle_required: true,
        pad_to_minimum: Some(2),
    };

    /// A bundle of `version` with the anchor of the empty tree. With `output`, it has one
    /// output of that value, encrypted to the zero outgoing viewing key, and a negative
    /// value balance. The proof and the signatures are zero bytes: `draft` reads neither.
    fn bundle(
        version: BundleVersion,
        bundle_type: BundleType,
        flags: Flags,
        output: Option<u64>,
        seed: u64,
    ) -> OrchardBundle {
        let mut builder =
            OrchardBuilder::new(bundle_type, version, flags, orchard::Anchor::empty_tree())
                .expect("the flags are valid for the version");
        if let Some(value) = output {
            let recipient = FullViewingKey::from(
                &SpendingKey::from_bytes([3u8; 32]).expect("the bytes are a spending key"),
            )
            .address_at(0u32, Scope::External);
            builder
                .add_output(
                    Some(OutgoingViewingKey::from([0u8; 32])),
                    recipient,
                    NoteValue::from_raw(value),
                    [0u8; 512],
                )
                .expect("the bundle takes an output");
        }
        let (bundle, _) = builder
            .build::<ZatBalance>(seeded(seed))
            .expect("the bundle builds")
            .expect("the bundle has actions");
        let actions = bundle.actions().len();
        bundle.map_authorization(
            &mut (),
            |_, _, _| [0u8; 64].into(),
            |_, _| {
                OrchardAuthorized::from_parts(
                    orchard::Proof::new(vec![0u8; orchard::Proof::expected_proof_size(actions)]),
                    [0u8; 64].into(),
                )
            },
        )
    }

    fn ironwood(output: u64) -> OrchardBundle {
        let version = BundleVersion::ironwood_v3();
        bundle(
            version,
            BundleType::DEFAULT,
            version.default_flags(),
            Some(output),
            1,
        )
    }

    /// An Orchard bundle of NU6.3: `enableCrossAddress` is 0, so it has padding only.
    fn orchard_nu6_3() -> OrchardBundle {
        let version = BundleVersion::orchard_v3();
        bundle(version, PADDING, version.default_flags(), None, 2)
    }

    /// An Orchard bundle of NU6.2 with one output.
    fn orchard_nu6_2(output: u64) -> OrchardBundle {
        let version = BundleVersion::orchard_v2();
        bundle(
            version,
            BundleType::DEFAULT,
            version.default_flags(),
            Some(output),
            3,
        )
    }

    /// A coinbase bundle of `version` with one output.
    fn coinbase_bundle(version: BundleVersion) -> OrchardBundle {
        bundle(
            version,
            BundleType::Coinbase,
            Flags::SPENDS_DISABLED,
            Some(700),
            4,
        )
    }

    /// The wire bytes of a transaction of `branch` with `inputs` transparent inputs (or a
    /// coinbase input) and `outputs` transparent outputs of value 1. It is a v6 transaction
    /// when `v6` is set and a v5 transaction if not.
    fn shielded(
        branch: BranchId,
        v6: bool,
        vin: Vec<TxIn<TAuthorized>>,
        outputs: usize,
        orchard: Option<OrchardBundle>,
        ironwood: Option<OrchardBundle>,
    ) -> Vec<u8> {
        let vout = (0..outputs)
            .map(|_| TxOut::new(Zatoshis::const_from_u64(1), script_from_bytes(&[])))
            .collect::<Vec<_>>();
        let transparent = if vin.is_empty() && vout.is_empty() {
            None
        } else {
            Some(Bundle {
                vin,
                vout,
                authorization: TAuthorized,
            })
        };
        let data = if v6 {
            TransactionData::<Authorized>::from_parts_v6(
                branch,
                0,
                BlockHeight::from_u32(0),
                transparent,
                None,
                orchard,
                ironwood,
            )
        } else {
            let None = ironwood else {
                panic!("a v5 transaction has no Ironwood bundle");
            };
            TransactionData::<Authorized>::from_parts(
                TxVersion::V5,
                branch,
                0,
                BlockHeight::from_u32(0),
                transparent,
                None,
                None,
                orchard,
            )
        };
        let mut bytes = Vec::new();
        data.freeze()
            .expect("the transaction freezes")
            .write(&mut bytes)
            .expect("vec write");
        bytes
    }

    fn inputs(n: u8) -> Vec<TxIn<TAuthorized>> {
        (0..n)
            .map(|i| txin(OutPoint::new([i + 1; 32], 0), &[]))
            .collect()
    }

    fn coinbase_input() -> Vec<TxIn<TAuthorized>> {
        vec![txin(OutPoint::NULL, &[1, 2, 3, 4])]
    }

    fn coins(n: u8, value: u64) -> Vec<Coin> {
        (0..n)
            .map(|_| Coin {
                value,
                script_pubkey: Bytes::from_static(&[OP_CHECKSIG]),
                height: 1,
                is_coinbase: false,
            })
            .collect()
    }

    /// The position of the flag byte of each Orchard-protocol bundle in `bytes`, in wire
    /// order (Orchard, then Ironwood). The flag byte, the value balance (8 bytes) and the
    /// anchor follow the actions. Every test bundle has the anchor of the empty tree.
    fn flag_positions(bytes: &[u8]) -> Vec<usize> {
        let anchor = orchard::Anchor::empty_tree().to_bytes();
        bytes
            .windows(32)
            .enumerate()
            .filter(|(_, window)| *window == anchor)
            .map(|(at, _)| at - 9)
            .collect()
    }

    fn parse(bytes: &[u8], branch: BranchId) -> Result<RawTx, hayai_wire::ParseError> {
        RawTx::parse(Bytes::copy_from_slice(bytes), branch)
    }

    fn drafted(bytes: &[u8], branch: BranchId, spent: Vec<Coin>) -> Result<Draft, PrepareError> {
        let raw = parse(bytes, branch).expect("the transaction parses");
        draft(raw, RuleEpoch::consensus(branch), spent)
    }

    fn rejected(bytes: &[u8], branch: BranchId, spent: Vec<Coin>) -> PrepareError {
        match drafted(bytes, branch, spent) {
            Ok(_) => panic!("the transaction is accepted"),
            Err(e) => e,
        }
    }

    /// `bytes` with the flag byte of bundle `nth` set to `flags`.
    fn with_flags(bytes: &[u8], nth: usize, flags: u8) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        let at = flag_positions(&bytes)[nth];
        bytes[at] = flags;
        bytes
    }

    #[test]
    fn a_source_and_a_sink_of_funds_respect_the_enable_flags() {
        let nu6_3 = BranchId::Nu6_3;
        for (label, branch, v6, orchard, ironwood) in [
            (
                "Orchard v5",
                BranchId::Nu6_2,
                false,
                Some(orchard_nu6_2(5)),
                None,
            ),
            ("Ironwood v6", nu6_3, true, None, Some(self::ironwood(5))),
        ] {
            // The bundle is the only source: no transparent input, one transparent output.
            let source = shielded(branch, v6, Vec::new(), 1, orchard.clone(), ironwood.clone());
            assert_eq!(flag_positions(&source).len(), 1, "{label}");
            for (flags, verdict) in [
                (SPENDS | OUTPUTS, None),
                (SPENDS, None),
                (OUTPUTS, Some(PrepareError::NoSource)),
            ] {
                let got = drafted(&with_flags(&source, 0, flags), branch, Vec::new())
                    .map(|_| ())
                    .err();
                // The fee rule comes after the rule under test: the outputs have value.
                let got = got.filter(|e| *e != PrepareError::NegativeFee);
                assert_eq!(got, verdict, "{label} source, flags {flags:#05b}");
            }
            // The bundle is the only sink: one transparent input, no transparent output.
            let sink = shielded(branch, v6, inputs(1), 0, orchard, ironwood);
            for (flags, verdict) in [
                (SPENDS | OUTPUTS, None),
                (OUTPUTS, None),
                (SPENDS, Some(PrepareError::NoSink)),
            ] {
                let got = drafted(&with_flags(&sink, 0, flags), branch, coins(1, 10))
                    .map(|_| ())
                    .err();
                assert_eq!(got, verdict, "{label} sink, flags {flags:#05b}");
            }
        }
        // A v6 transaction with both bundles: each one can be the source or the sink.
        let both = shielded(
            nu6_3,
            true,
            Vec::new(),
            0,
            Some(orchard_nu6_3()),
            Some(ironwood(5)),
        );
        for (orchard, ironwood, verdict) in [
            (SPENDS, OUTPUTS, None),
            (OUTPUTS, SPENDS, None),
            (SPENDS, SPENDS, Some(PrepareError::NoSink)),
            (OUTPUTS, OUTPUTS, Some(PrepareError::NoSource)),
        ] {
            let bytes = with_flags(&with_flags(&both, 0, orchard), 1, ironwood);
            let got = drafted(&bytes, nu6_3, Vec::new()).map(|_| ()).err();
            let got = got.filter(|e| *e != PrepareError::NegativeFee);
            assert_eq!(got, verdict, "flags {orchard:#05b} and {ironwood:#05b}");
        }
    }

    #[test]
    fn actions_need_an_enable_flag() {
        let nu6_3 = BranchId::Nu6_3;
        let tx = shielded(
            nu6_3,
            true,
            inputs(1),
            1,
            Some(orchard_nu6_3()),
            Some(ironwood(5)),
        );
        drafted(&tx, nu6_3, coins(1, 10)).expect("both bundles have both flags");
        assert_eq!(
            rejected(&with_flags(&tx, 0, 0), nu6_3, coins(1, 10)),
            PrepareError::OrchardFlags
        );
        assert_eq!(
            rejected(&with_flags(&tx, 1, 0), nu6_3, coins(1, 10)),
            PrepareError::IronwoodFlags
        );
        // `enableCrossAddress` alone does not enable an Ironwood action.
        assert_eq!(
            rejected(&with_flags(&tx, 1, CROSS_ADDRESS), nu6_3, coins(1, 10)),
            PrepareError::IronwoodFlags
        );
    }

    /// The valid flag bits. Orchard: bits 0 and 1, in a v5 and in a v6 transaction. Ironwood:
    /// bits 0, 1 and 2 (`enableCrossAddress`). The parser applies the rule
    /// (`Flags::from_byte` with the bundle version of the branch), so a transaction with
    /// another bit does not reach `draft`.
    #[test]
    fn the_flag_bits_of_each_pool() {
        let nu6_3 = BranchId::Nu6_3;
        let v6 = shielded(
            nu6_3,
            true,
            inputs(1),
            1,
            Some(orchard_nu6_3()),
            Some(ironwood(5)),
        );
        let v5 = shielded(nu6_3, false, inputs(1), 1, Some(orchard_nu6_3()), None);
        let old = shielded(
            BranchId::Nu6_2,
            false,
            inputs(1),
            0,
            Some(orchard_nu6_2(5)),
            None,
        );
        for flags in 0..=u8::MAX {
            let reserved = flags & !(SPENDS | OUTPUTS | CROSS_ADDRESS) != 0;
            let cross_address = flags & CROSS_ADDRESS != 0;
            let orchard_ok = !reserved && !cross_address;
            for (label, bytes, branch) in [
                ("Orchard v6", &v6, nu6_3),
                ("Orchard v5 at NU6.3", &v5, nu6_3),
                ("Orchard v5 at NU6.2", &old, BranchId::Nu6_2),
            ] {
                let parsed = parse(&with_flags(bytes, 0, flags), branch);
                assert_eq!(
                    parsed.map(|_| ()).ok(),
                    orchard_ok.then_some(()),
                    "{label} {flags:#010b}"
                );
            }
            let parsed = parse(&with_flags(&v6, 1, flags), nu6_3);
            assert_eq!(
                parsed.map(|_| ()).ok(),
                (!reserved).then_some(()),
                "Ironwood {flags:#010b}"
            );
        }
        // The parsed flags: an Orchard bundle of NU6.3 has no cross-address transfer, an
        // Ironwood bundle has the value of bit 2, an Orchard bundle of NU6.2 has no rule.
        let cross = |bytes: &[u8], branch| {
            let raw = parse(bytes, branch).expect("parses");
            (
                raw.tx
                    .orchard_bundle()
                    .map(|b| b.flags().cross_address_enabled()),
                raw.tx
                    .ironwood_bundle()
                    .map(|b| b.flags().cross_address_enabled()),
            )
        };
        assert_eq!(cross(&v6, nu6_3), (Some(false), Some(true)));
        assert_eq!(
            cross(&with_flags(&v6, 1, SPENDS | OUTPUTS), nu6_3),
            (Some(false), Some(false))
        );
        assert_eq!(cross(&v5, nu6_3), (Some(false), None));
        assert_eq!(cross(&old, BranchId::Nu6_2), (Some(true), None));
        // An Ironwood bundle with `enableCrossAddress = 0` is valid for `draft`.
        drafted(&with_flags(&v6, 1, SPENDS | OUTPUTS), nu6_3, coins(1, 10))
            .expect("an Ironwood bundle without cross-address transfers");
    }

    /// A proof that is longer or shorter than the canonical length of its action count does
    /// not parse, for an Ironwood bundle and for an Orchard bundle from NU6.2.
    #[test]
    fn a_proof_has_the_canonical_length() {
        let nu6_3 = BranchId::Nu6_3;
        let padded = |version: BundleVersion, bundle_type, output| {
            let good = bundle(version, bundle_type, version.default_flags(), output, 5);
            let actions = good.actions().len();
            good.map_authorization(
                &mut (),
                |_, _, sig| sig.clone(),
                |_, _| {
                    OrchardAuthorized::from_parts(
                        orchard::Proof::new(vec![
                            0u8;
                            orchard::Proof::expected_proof_size(actions) + 1
                        ]),
                        [0u8; 64].into(),
                    )
                },
            )
        };
        let ironwood = padded(BundleVersion::ironwood_v3(), BundleType::DEFAULT, Some(5));
        let bytes = shielded(nu6_3, true, inputs(1), 0, None, Some(ironwood));
        let Err(_) = parse(&bytes, nu6_3) else {
            panic!("an Ironwood proof with one more byte parses");
        };
        let orchard = padded(BundleVersion::orchard_v3(), PADDING, None);
        let bytes = shielded(nu6_3, false, inputs(1), 1, Some(orchard), None);
        let Err(_) = parse(&bytes, nu6_3) else {
            panic!("an Orchard proof with one more byte parses at NU6.3");
        };
        let orchard = padded(BundleVersion::orchard_v2(), BundleType::DEFAULT, Some(5));
        let bytes = shielded(BranchId::Nu6_2, false, inputs(1), 0, Some(orchard), None);
        let Err(_) = parse(&bytes, BranchId::Nu6_2) else {
            panic!("an Orchard proof with one more byte parses at NU6.2");
        };
    }

    #[test]
    fn no_value_enters_the_orchard_pool_from_nu6_3() {
        let nu6_3 = BranchId::Nu6_3;
        let with_balance = |bytes: &[u8], balance: i64| {
            let mut bytes = bytes.to_vec();
            let at = flag_positions(&bytes)[0] + 1;
            bytes[at..at + 8].copy_from_slice(&balance.to_le_bytes());
            bytes
        };
        for v6 in [false, true] {
            let tx = shielded(nu6_3, v6, inputs(1), 1, Some(orchard_nu6_3()), None);
            drafted(&tx, nu6_3, coins(1, 10)).expect("a value balance of zero");
            drafted(&with_balance(&tx, 4), nu6_3, coins(1, 10))
                .expect("a positive value balance takes value out of the pool");
            assert_eq!(
                rejected(&with_balance(&tx, -4), nu6_3, coins(1, 10)),
                PrepareError::OrchardPoolDeposit(-4)
            );
        }
        // Before NU6.3 value can enter the Orchard pool.
        let old = shielded(
            BranchId::Nu6_2,
            false,
            inputs(1),
            0,
            Some(orchard_nu6_2(5)),
            None,
        );
        let d = drafted(&old, BranchId::Nu6_2, coins(1, 10)).expect("a deposit before NU6.3");
        assert_eq!(d.tx().fee, 5);
        // Value can enter the Ironwood pool.
        let tx = shielded(nu6_3, true, inputs(1), 0, None, Some(ironwood(5)));
        drafted(&tx, nu6_3, coins(1, 10)).expect("a deposit to the Ironwood pool");
    }

    #[test]
    fn the_coinbase_rules_of_the_orchard_and_ironwood_bundles() {
        let nu6_3 = BranchId::Nu6_3;
        let ironwood_v3 = BundleVersion::ironwood_v3();
        // From NU6.3 a coinbase has no Orchard bundle, in a v5 and in a v6 transaction.
        // Before NU6.3 it can have one.
        let orchard_v3 = bundle(
            BundleVersion::orchard_v3(),
            PADDING,
            BundleVersion::orchard_v3().default_flags(),
            None,
            6,
        );
        for v6 in [false, true] {
            let tx = shielded(
                nu6_3,
                v6,
                coinbase_input(),
                1,
                Some(orchard_v3.clone()),
                None,
            );
            let tx = with_flags(&tx, 0, OUTPUTS);
            assert_eq!(
                rejected(&tx, nu6_3, Vec::new()),
                PrepareError::CoinbaseOrchardBundle
            );
        }
        let old = shielded(
            BranchId::Nu6_2,
            false,
            coinbase_input(),
            1,
            Some(coinbase_bundle(BundleVersion::orchard_v2())),
            None,
        );
        drafted(&old, BranchId::Nu6_2, Vec::new()).expect("an Orchard coinbase before NU6.3");
        // A shielded coinbase output of NU6.3 is an Ironwood output, with `enableSpends = 0`.
        let tx = shielded(
            nu6_3,
            true,
            coinbase_input(),
            1,
            None,
            Some(coinbase_bundle(ironwood_v3)),
        );
        let d = drafted(&tx, nu6_3, Vec::new()).expect("an Ironwood coinbase");
        assert!(d.tx().is_coinbase);
        assert_eq!(d.tx().ironwood_actions, 1);
        for flags in [SPENDS | OUTPUTS, SPENDS | OUTPUTS | CROSS_ADDRESS] {
            assert_eq!(
                rejected(&with_flags(&tx, 0, flags), nu6_3, Vec::new()),
                PrepareError::CoinbaseShieldedSpend
            );
        }
    }

    #[test]
    fn an_ironwood_bundle_gives_nullifiers_commitments_an_anchor_and_a_balance() {
        let nu6_3 = BranchId::Nu6_3;
        let bytes = shielded(
            nu6_3,
            true,
            inputs(1),
            0,
            Some(orchard_nu6_3()),
            Some(ironwood(600)),
        );
        let d = drafted(&bytes, nu6_3, coins(1, 1_000)).expect("a valid transaction");
        let tx = d.tx();
        // The input has 1,000, the Ironwood bundle takes 600 and the Orchard bundle 0.
        assert_eq!(tx.fee, 400);
        assert_eq!((tx.orchard_actions, tx.ironwood_actions), (2, 2));
        assert!(tx.has_shielded() && !tx.shielded_ok);
        let (orchard, ironwood) = (
            tx.raw.tx.orchard_bundle().expect("Orchard bundle"),
            tx.raw.tx.ironwood_bundle().expect("Ironwood bundle"),
        );
        let expected: Vec<(Pool, [u8; 32])> = orchard
            .actions()
            .iter()
            .map(|a| (Pool::Orchard, a.nullifier().to_bytes()))
            .chain(
                ironwood
                    .actions()
                    .iter()
                    .map(|a| (Pool::Ironwood, a.nullifier().to_bytes())),
            )
            .collect();
        assert_eq!(tx.nullifiers, expected);
        let cmxs = |b: &OrchardBundle| -> Vec<MerkleHashOrchard> {
            b.actions()
                .iter()
                .map(|a| MerkleHashOrchard::from_cmx(a.cmx()))
                .collect()
        };
        assert_eq!(tx.commitments.orchard, cmxs(orchard));
        assert_eq!(tx.commitments.ironwood, cmxs(ironwood));
        assert_ne!(tx.commitments.orchard, tx.commitments.ironwood);
        let empty = orchard::Anchor::empty_tree().to_bytes();
        assert_eq!(
            tx.anchors,
            vec![(Pool::Orchard, empty), (Pool::Ironwood, empty)]
        );
        // An Ironwood bundle alone makes the transaction a shielded one.
        let alone = shielded(nu6_3, true, inputs(1), 0, None, Some(self::ironwood(600)));
        let d = drafted(&alone, nu6_3, coins(1, 1_000)).expect("a valid transaction");
        assert!(d.tx().has_shielded() && !d.tx().shielded_ok);
        assert_eq!((d.tx().orchard_actions, d.tx().ironwood_actions), (0, 2));
        // The Ironwood balance is in the fee: outputs of more than the inputs are refused.
        assert_eq!(
            rejected(&alone, nu6_3, coins(1, 599)),
            PrepareError::NegativeFee
        );
    }

    #[test]
    fn a_nullifier_repeats_in_no_bundle() {
        let nu6_3 = BranchId::Nu6_3;
        let bytes = shielded(
            nu6_3,
            true,
            inputs(1),
            1,
            Some(orchard_nu6_3()),
            Some(ironwood(5)),
        );
        for (nth, pool) in [(0, Pool::Orchard), (1, Pool::Ironwood)] {
            // Copy the nullifier of the first action of the bundle to its second action.
            let mut bytes = bytes.clone();
            let first = flag_positions(&bytes)[nth] - 2 * ACTION_BYTES;
            let nullifier = bytes[first + 32..first + 64].to_vec();
            let second = first + ACTION_BYTES;
            bytes[second + 32..second + 64].copy_from_slice(&nullifier);
            assert_eq!(
                rejected(&bytes, nu6_3, coins(1, 10)),
                PrepareError::DuplicateNullifier(pool)
            );
        }
        // The same nullifier in the Orchard bundle and in the Ironwood bundle is no
        // repetition: the two nullifier sets are distinct.
        let mut same = bytes.clone();
        let positions = flag_positions(&same);
        let orchard = positions[0] - 2 * ACTION_BYTES;
        let ironwood = positions[1] - 2 * ACTION_BYTES;
        let nullifier = same[orchard + 32..orchard + 64].to_vec();
        same[ironwood + 32..ironwood + 64].copy_from_slice(&nullifier);
        let d = drafted(&same, nu6_3, coins(1, 10)).expect("distinct pools");
        assert_eq!(d.tx().nullifiers[0].1, d.tx().nullifiers[2].1);
        assert_ne!(d.tx().nullifiers[0].0, d.tx().nullifiers[2].0);
    }

    /// The versions come from `RuleSet::tx_versions`: v6 from NU6.3, v5 from NU5, v4 from
    /// Sapling to NU6.3.
    #[test]
    fn the_rule_set_names_the_transaction_versions() {
        let nu6_3 = BranchId::Nu6_3;
        let v6 = shielded(nu6_3, true, inputs(1), 1, None, None);
        drafted(&v6, nu6_3, coins(1, 10)).expect("v6 at NU6.3");
        let v5 = shielded(nu6_3, false, inputs(1), 1, None, None);
        drafted(&v5, nu6_3, coins(1, 10)).expect("v5 at NU6.3");
        // A v6 transaction with the branch id of NU6.2 parses. The version rule of the
        // NU6.2 rule set refuses it.
        let early = shielded(BranchId::Nu6_2, true, inputs(1), 1, None, None);
        let Err(PrepareError::Version(version, BranchId::Nu6_2)) =
            drafted(&early, BranchId::Nu6_2, coins(1, 10)).map(|_| ())
        else {
            panic!("v6 before NU6.3");
        };
        assert_eq!(version, "V6");
        let raw = parse(&v5, nu6_3).expect("parses");
        let Err(PrepareError::Version(_, BranchId::Canopy)) =
            draft(raw, RuleEpoch::consensus(BranchId::Canopy), coins(1, 10)).map(|_| ())
        else {
            panic!("v5 before NU5");
        };
        // An allowed version with the branch id of another epoch breaks the branch rule.
        let raw = parse(&v5, nu6_3).expect("parses");
        let Err(PrepareError::BranchId { tx, epoch }) =
            draft(raw, RuleEpoch::consensus(BranchId::Nu6_2), coins(1, 10)).map(|_| ())
        else {
            panic!("the branch id of another epoch");
        };
        assert_eq!((tx, epoch), (nu6_3, BranchId::Nu6_2));
    }

    /// The pools come from `RuleSet::pools`.
    #[test]
    fn the_rule_set_names_the_pools() {
        let nu6_3 = BranchId::Nu6_3;
        let rules = RuleSet::of_branch(nu6_3).expect("NU6.3 has a rule set");
        let bytes = shielded(
            nu6_3,
            true,
            inputs(1),
            1,
            Some(orchard_nu6_3()),
            Some(ironwood(5)),
        );
        let raw = parse(&bytes, nu6_3).expect("parses");
        assert_eq!(check_pools(&raw.tx, rules.pools, nu6_3), Ok(()));
        for (pools, pool) in [
            (
                ShieldedPools {
                    ironwood: false,
                    ..rules.pools
                },
                Pool::Ironwood,
            ),
            (
                ShieldedPools {
                    orchard: false,
                    ..rules.pools
                },
                Pool::Orchard,
            ),
        ] {
            assert_eq!(
                check_pools(&raw.tx, pools, nu6_3),
                Err(PrepareError::PoolNotActive(pool, nu6_3))
            );
        }
        // A transaction without a bundle of a pool needs no pool.
        let transparent =
            parse(&shielded(nu6_3, true, inputs(1), 1, None, None), nu6_3).expect("parses");
        let none = ShieldedPools {
            sprout: false,
            sapling: false,
            orchard: false,
            ironwood: false,
        };
        assert_eq!(check_pools(&transparent.tx, none, nu6_3), Ok(()));
    }

    fn outputs(n: usize) -> Vec<TxOut> {
        (0..n)
            .map(|_| TxOut::new(Zatoshis::const_from_u64(1), script_from_bytes(&[])))
            .collect()
    }

    /// A v4 transaction of `branch` with one JoinSplit for each `(tag, vpub_old, vpub_new)`.
    fn v4_joinsplits(
        branch: BranchId,
        vin: Vec<TxIn<TAuthorized>>,
        vout: usize,
        joinsplits: &[(u8, u64, u64)],
    ) -> Vec<u8> {
        joinsplit_tx(TxVersion::V4, branch, vin, outputs(vout), joinsplits)
    }

    /// A JoinSplit is a source and a sink of funds. The draft has its two nullifiers and
    /// its two commitments, and the transaction waits for the proof and the signature.
    #[test]
    fn a_joinsplit_is_a_source_and_a_sink() {
        let canopy = BranchId::Canopy;
        let bytes = v4_joinsplits(canopy, Vec::new(), 0, &[(7, 0, 0), (8, 0, 0)]);
        let d = drafted(&bytes, canopy, Vec::new()).expect("JoinSplits only");
        let tx = d.tx();
        assert_eq!(tx.joinsplits, 2);
        assert!(tx.has_shielded() && !tx.shielded_ok);
        let pools: Vec<Pool> = tx.nullifiers.iter().map(|(pool, _)| *pool).collect();
        assert_eq!(pools, [Pool::Sprout; 4]);
        assert_eq!(tx.nullifiers[0].1[..2], [1, 7]);
        assert_eq!(tx.nullifiers[3].1[..2], [2, 8]);
        assert_eq!(tx.commitments.sprout.len(), 4);
        assert_eq!(tx.commitments.sprout[0][..2], [3, 7]);
        assert_eq!(tx.commitments.sprout[3][..2], [4, 8]);
        // The Sprout anchors are not in the list of final treestates.
        assert_eq!(tx.anchors, Vec::new());
        assert_eq!(tx.fee, 0);
    }

    /// `vpub_old` or `vpub_new` is zero.
    #[test]
    fn one_of_vpub_old_and_vpub_new_is_zero() {
        let heartwood = BranchId::Heartwood;
        for (vpub_old, vpub_new) in [(1, 0), (0, 1), (0, 0)] {
            let bytes = v4_joinsplits(heartwood, inputs(1), 1, &[(7, vpub_old, vpub_new)]);
            drafted(&bytes, heartwood, coins(1, 10)).expect("one value is zero");
        }
        let bytes = v4_joinsplits(heartwood, inputs(1), 1, &[(7, 0, 1), (8, 1, 1)]);
        assert_eq!(
            rejected(&bytes, heartwood, coins(1, 10)),
            PrepareError::JoinSplitBothVpub(1)
        );
    }

    /// ZIP 211: until Heartwood a JoinSplit can add value to the Sprout pool. From Canopy
    /// `vpub_old` is zero.
    #[test]
    fn no_value_enters_the_sprout_pool_from_canopy() {
        for (branch, deposit) in [
            (BranchId::Sapling, true),
            (BranchId::Heartwood, true),
            (BranchId::Canopy, false),
            (BranchId::Nu6_3, false),
        ] {
            let bytes = v4_joinsplits(branch, inputs(1), 1, &[(7, 0, 1), (8, 1, 0)]);
            let result = drafted(&bytes, branch, coins(1, 10)).map(|_| ());
            let expected = match deposit {
                true => Ok(()),
                false => Err(PrepareError::SproutPoolDeposit(1)),
            };
            assert_eq!(result, expected, "{branch:?}");
            // A withdrawal is valid in every epoch.
            let bytes = v4_joinsplits(branch, inputs(1), 1, &[(7, 0, 1)]);
            drafted(&bytes, branch, coins(1, 10)).expect("vpub_old is zero");
        }
    }

    /// `vpub_new` is an input of the transparent value and `vpub_old` is an output. Each
    /// total is at most `MAX_MONEY`.
    #[test]
    fn the_fee_counts_the_joinsplit_values() {
        let heartwood = BranchId::Heartwood;
        let fee = |joinsplits: &[(u8, u64, u64)]| {
            let bytes = v4_joinsplits(heartwood, inputs(1), 1, joinsplits);
            drafted(&bytes, heartwood, coins(1, 10)).map(|d| d.tx().fee)
        };
        assert_eq!(fee(&[(7, 0, 0)]), Ok(9));
        assert_eq!(fee(&[(7, 4, 0)]), Ok(5));
        assert_eq!(fee(&[(7, 9, 0)]), Ok(0));
        assert_eq!(fee(&[(7, 10, 0)]), Err(PrepareError::NegativeFee));
        assert_eq!(fee(&[(7, 0, 7)]), Ok(16));
        assert_eq!(fee(&[(7, 0, 7), (8, 5, 0)]), Ok(11));
        // The total of `vpub_new`, and the total of the outputs with `vpub_old`.
        assert_eq!(
            fee(&[(7, 0, MAX_MONEY), (8, 0, 1)]),
            Err(PrepareError::ValueOverflow)
        );
        assert_eq!(fee(&[(7, MAX_MONEY, 0)]), Err(PrepareError::ValueOverflow));
        // The value of a JoinSplit alone can pay the fee.
        let bytes = v4_joinsplits(heartwood, Vec::new(), 1, &[(7, 0, 3)]);
        assert_eq!(
            drafted(&bytes, heartwood, Vec::new()).map(|d| d.tx().fee),
            Ok(2)
        );
    }

    #[test]
    fn a_coinbase_has_no_joinsplit() {
        let canopy = BranchId::Canopy;
        let bytes = v4_joinsplits(canopy, coinbase_input(), 1, &[(7, 0, 0)]);
        assert_eq!(
            rejected(&bytes, canopy, Vec::new()),
            PrepareError::CoinbaseJoinSplit
        );
    }

    /// A Sprout nullifier is revealed once in a transaction.
    #[test]
    fn a_sprout_nullifier_is_revealed_once_in_a_transaction() {
        let canopy = BranchId::Canopy;
        let bytes = v4_joinsplits(canopy, Vec::new(), 0, &[(7, 0, 0), (8, 0, 0), (7, 0, 0)]);
        assert_eq!(
            rejected(&bytes, canopy, Vec::new()),
            PrepareError::DuplicateNullifier(Pool::Sprout)
        );
    }

    /// A transaction version before Sapling has no verification: v1 and v2 in the Sprout
    /// epoch and v3 in the Overwinter epoch are `Unsupported`, with and without JoinSplits.
    /// The rule set allows them, so the error is not the version error.
    #[test]
    fn a_version_before_sapling_is_not_verified() {
        for (version, branch, joinsplits) in [
            (TxVersion::Sprout(1), BranchId::Sprout, &[][..]),
            (TxVersion::Sprout(2), BranchId::Sprout, &[][..]),
            (TxVersion::Sprout(2), BranchId::Sprout, &[(7, 0, 1)][..]),
            (TxVersion::V3, BranchId::Overwinter, &[][..]),
            (TxVersion::V3, BranchId::Overwinter, &[(7, 0, 1)][..]),
        ] {
            let bytes = joinsplit_tx(version, branch, inputs(1), outputs(1), joinsplits);
            let PrepareError::Unsupported(reason) = rejected(&bytes, branch, coins(1, 10)) else {
                panic!("{version:?} is not verified");
            };
            assert!(reason.contains("checkpoint path"), "{reason}");
        }
        // v2 in the Overwinter epoch breaks the version rule.
        let sprout = BranchId::Sprout;
        let bytes = joinsplit_tx(TxVersion::Sprout(2), sprout, inputs(1), outputs(1), &[]);
        let raw = parse(&bytes, sprout).expect("parses");
        let Err(PrepareError::Version(_, BranchId::Overwinter)) = draft(
            raw,
            RuleEpoch::consensus(BranchId::Overwinter),
            coins(1, 10),
        )
        .map(|_| ()) else {
            panic!("v2 after Overwinter");
        };
    }
}
