//! Mempool policy: the rules that decide whether the node stores and relays a valid
//! transaction.
//!
//! [`prepare`](crate::prepare) rejects a transaction that breaks a context-free consensus
//! rule. [`MempoolPolicy::admit`] rejects a transaction that the next block cannot contain
//! (expiry, lock time, coinbase maturity) or that the public network does not relay
//! (ZIP 317 fee rules, zcashd standardness). `docs/mempool-policy.md` lists each rule, its
//! source and its constant.
//!
//! Inputs that are not available. `prepare` fails with
//! [`PrepareError::MissingInput`](crate::PrepareError::MissingInput) when the coins view
//! does not hold an input. The node keeps no orphan pool, as Zebra and Zakura.
//!
//! The rules that need the chain state (nullifiers and anchors against the tip) are not
//! here: the caller checks them against its chain view.

use hayai_coins::Coin;
use hayai_consensus::{Network, RuleSet, COINBASE_MATURITY, LOCKTIME_THRESHOLD};
use hayai_crypto::zcash_script;
use hayai_template::zip317::{logical_actions, Zip317Params, BLOCK_UNPAID_ACTION_LIMIT};
use zcash_script::opcode::PossiblyBad;
use zcash_script::script::{Code, Evaluable};
use zcash_script::solver::{self, ScriptKind};
use zcash_script::Opcode;

use crate::{PreparedTx, RuleEpoch};

/// The mempool rejects a transaction whose expiry height is less than the next block
/// height plus this value (zcashd `TX_EXPIRING_SOON_THRESHOLD`).
pub const TX_EXPIRING_SOON_THRESHOLD: u32 = 3;
/// Largest standard scriptSig in bytes (zcashd `IsStandardTx`: a 15-of-15 P2SH multisig).
pub const MAX_STANDARD_SCRIPTSIG_SIZE: usize = 1_650;
/// Largest sigop count of a redeem script that is not a standard script (zcashd
/// `MAX_P2SH_SIGOPS`).
pub const MAX_P2SH_SIGOPS: u32 = 15;
/// Largest sigop count (legacy plus P2SH) of a standard transaction (zcashd
/// `MAX_STANDARD_TX_SIGOPS`).
pub const MAX_STANDARD_TX_SIGOPS: u32 = 4_000;
/// Largest number of public keys of a standard multisig script (zcashd `IsStandard`).
pub const MAX_STANDARD_MULTISIG_PUBKEYS: usize = 3;
/// Largest standard `OP_RETURN` script in bytes, opcode and push overhead included (zcashd
/// `MAX_OP_RETURN_RELAY`).
pub const MAX_DATACARRIER_BYTES: usize = 83;
/// One third of the dust rate, in zatoshis per 1,000 bytes (zcashd
/// `ONE_THIRD_DUST_THRESHOLD_RATE`).
pub const ONE_THIRD_DUST_THRESHOLD_RATE: u64 = 100;
/// Bytes that zcashd adds to the size of an output for the input that spends it.
const DUST_SPEND_BYTES: u64 = 148;
/// The minimum relay fee rate, in zatoshis per 1,000 bytes (zcashd
/// `DEFAULT_MIN_RELAY_TX_FEE`). It is also the minimum relay fee of a transaction.
pub const MIN_RELAY_FEE_RATE: u64 = 100;
/// The upper bound of the minimum relay fee of a transaction, in zatoshis (zcashd
/// `LEGACY_DEFAULT_FEE`).
pub const MIN_RELAY_FEE_CAP: u64 = 1_000;

/// The chain facts that the policy needs.
#[derive(Clone, Copy, Debug)]
pub struct PolicyContext<'a> {
    /// Height of the next block: the tip height plus 1.
    pub next_height: u32,
    /// Median of the last 11 block times that end at the tip. It is the lock time
    /// reference of the next block (zcashd `LOCKTIME_MEDIAN_TIME_PAST`).
    pub median_time_past: u32,
    /// The rule set of the next block.
    pub rules: &'a RuleSet,
}

/// The parameters of the mempool policy. The constants of this module are the same on
/// every network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MempoolPolicy {
    /// The ZIP 317 fee parameters. They must equal the parameters of the store and of the
    /// template.
    pub zip317: Zip317Params,
    /// A transaction with more unpaid actions is not stored and not relayed (zcashd
    /// `-txunpaidactionlimit`).
    pub unpaid_action_limit: u32,
    /// zcashd `fRequireStandard`: with `false`, the script and dust rules of
    /// `IsStandardTx` and `AreInputsStandard` do not apply.
    pub require_standard: bool,
    /// zcashd `-permitbaremultisig`: with `false`, a bare multisig output is not standard.
    pub permit_bare_multisig: bool,
    /// Largest standard `OP_RETURN` script in bytes (zcashd `-datacarriersize`).
    pub max_datacarrier_bytes: usize,
    /// The consensus rule of the network: a transaction that spends a coinbase output has
    /// no transparent output (`NetworkParams::coinbase_must_be_shielded`).
    pub coinbase_must_be_shielded: bool,
}

/// Why the policy does not admit a transaction. Each variant is one rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyReject {
    /// zcashd: `coinbase`.
    #[error("a coinbase is not a mempool transaction")]
    Coinbase,
    #[error("transaction epoch {tx:?} differs from the epoch {next:?} of the next block")]
    Epoch { tx: RuleEpoch, next: RuleEpoch },
    /// ZIP 203. zcashd: `tx-overwinter-expired`.
    #[error("expiry height {expiry} is below the next block height {next_height}")]
    Expired { expiry: u32, next_height: u32 },
    /// zcashd: `tx-expiring-soon`.
    #[error(
        "expiry height {expiry} is less than {TX_EXPIRING_SOON_THRESHOLD} blocks above the \
         next block height {next_height}"
    )]
    ExpiringSoon { expiry: u32, next_height: u32 },
    /// zcashd: `scriptsig-size`.
    #[error(
        "scriptSig of input {input} has {size} bytes, more than {MAX_STANDARD_SCRIPTSIG_SIZE}"
    )]
    ScriptSigSize { input: usize, size: usize },
    /// zcashd: `scriptsig-not-pushonly`.
    #[error("scriptSig of input {input} is not push-only")]
    ScriptSigNotPushOnly { input: usize },
    /// zcashd: `scriptpubkey`.
    #[error("scriptPubKey of output {output} is not a standard script")]
    ScriptPubKey { output: usize },
    /// zcashd: `scriptpubkey` (the size limit is part of `IsStandard`).
    #[error("OP_RETURN script of output {output} has {size} bytes, more than {limit}")]
    DataCarrierSize {
        output: usize,
        size: usize,
        limit: usize,
    },
    /// zcashd: `bare-multisig`.
    #[error("output {output} is a bare multisig")]
    BareMultisig { output: usize },
    /// zcashd: `dust`.
    #[error("output {output} pays {value} zatoshis, less than the dust threshold {threshold}")]
    Dust {
        output: usize,
        value: u64,
        threshold: u64,
    },
    /// zcashd: `multi-op-return`.
    #[error("transaction has {0} OP_RETURN outputs, more than 1")]
    MultiOpReturn(usize),
    /// zcashd: `non-final`.
    #[error(
        "lock time {lock_time} is not reached at height {next_height}, time {median_time_past}"
    )]
    NonFinal {
        lock_time: u32,
        next_height: u32,
        median_time_past: u32,
    },
    /// zcashd: `bad-txns-premature-spend-of-coinbase`.
    #[error("input {input} spends a coinbase of height {created}, not mature at {next_height}")]
    ImmatureCoinbase {
        input: usize,
        created: u32,
        next_height: u32,
    },
    /// zcashd: `bad-txns-coinbase-spend-has-transparent-outputs`.
    #[error("input {input} spends a coinbase and the transaction has transparent outputs")]
    UnshieldedCoinbaseSpend { input: usize },
    /// zcashd: `bad-txns-nonstandard-inputs`.
    #[error("input {input} is not a standard spend")]
    NonStandardInput { input: usize },
    /// zcashd: `bad-txns-too-many-sigops`.
    #[error("{sigops} sigops, more than {MAX_STANDARD_TX_SIGOPS}")]
    TooManySigops { sigops: u32 },
    /// ZIP 317. zcashd: `tx-unpaid-action-limit-exceeded`.
    #[error("{unpaid} unpaid actions, more than {limit}")]
    UnpaidActions { unpaid: u32, limit: u32 },
    /// zcashd: `min relay fee not met`.
    #[error("fee {fee} is below the minimum relay fee {minimum}")]
    FeeBelowMinimumRelay { fee: u64, minimum: u64 },
    /// No block of the next rule set can contain the transaction.
    #[error("{what} count {count} is above the block limit {limit}")]
    AboveBlockLimit {
        what: &'static str,
        count: u32,
        limit: u32,
    },
}

/// The standard kind of a script, `None` for a script that is not standard.
fn script_kind(script: &[u8]) -> Option<ScriptKind> {
    let component = Code(script.to_vec()).to_component().ok()?.refine().ok()?;
    solver::standard(&component)
}

/// zcashd `ScriptSigArgsExpected`: the number of stack items that spend a script of `kind`.
fn script_sig_args_expected(kind: &ScriptKind) -> Option<usize> {
    match kind {
        ScriptKind::PubKey { .. } | ScriptKind::ScriptHash { .. } => Some(1),
        ScriptKind::PubKeyHash { .. } => Some(2),
        ScriptKind::MultiSig { required, .. } => Some(usize::from(*required) + 1),
        ScriptKind::NullData { .. } => None,
    }
}

/// The data of each push of a push-only script, in order: the stack after its evaluation.
fn pushes(script_sig: &[u8]) -> Vec<Vec<u8>> {
    Code(script_sig.to_vec())
        .parse()
        .filter_map(|op| match op {
            Ok(PossiblyBad::Good(Opcode::PushValue(pv))) => Some(pv.value()),
            _ => None,
        })
        .collect()
}

/// zcashd `AreInputsStandard` for one input, as Zebra implements it
/// (`zebrad/src/components/mempool/storage/policy.rs`, `are_inputs_standard`). The
/// scriptSig is push-only.
fn input_is_standard(script_sig: &[u8], coin: &Coin) -> bool {
    let Some(kind) = script_kind(&coin.script_pubkey) else {
        return false;
    };
    let Some(mut expected) = script_sig_args_expected(&kind) else {
        return false;
    };
    let stack = pushes(script_sig);
    if let ScriptKind::ScriptHash { .. } = kind {
        let Some(redeem) = stack.last() else {
            return false;
        };
        match script_kind(redeem) {
            Some(inner) => {
                let Some(inner) = script_sig_args_expected(&inner) else {
                    return false;
                };
                expected += inner;
            }
            // A redeem script that is not standard is accepted with a small sigop count.
            // More data on the stack is accepted too.
            None => return Code(redeem.clone()).sig_op_count(true) <= MAX_P2SH_SIGOPS,
        }
    }
    stack.len() == expected
}

/// Bytes of the CompactSize encoding of `n`.
fn compact_size_len(n: usize) -> u64 {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// zcashd `CTxOut::GetDustThreshold` with the default minimum relay fee: the value below
/// which an output with a script of `script_len` bytes is dust.
pub fn dust_threshold(script_len: usize) -> u64 {
    let output_size = 8 + compact_size_len(script_len) + script_len as u64;
    3 * (ONE_THIRD_DUST_THRESHOLD_RATE * (output_size + DUST_SPEND_BYTES) / 1_000)
}

/// The minimum relay fee of a transaction of `size` bytes (zcashd
/// `CFeeRate::GetFeeForRelay`; Zebra `zip317::mempool_checks`).
pub fn min_relay_fee(size: usize) -> u64 {
    (MIN_RELAY_FEE_RATE * size as u64 / 1_000).clamp(MIN_RELAY_FEE_RATE, MIN_RELAY_FEE_CAP)
}

/// Whether the node relays a stored transaction with `expiry_height` when the next block
/// has height `next_height`. zcashd does not announce and does not send a transaction that
/// expires soon.
pub fn is_relayable(expiry_height: u32, next_height: u32) -> bool {
    expiry_height == 0 || expiry_height >= next_height.saturating_add(TX_EXPIRING_SOON_THRESHOLD)
}

impl MempoolPolicy {
    /// The policy of `network`: ZIP 317 as published and zcashd standardness. Regtest does
    /// not require standard transactions.
    pub fn of(network: Network) -> Self {
        Self {
            zip317: Zip317Params::ZIP317,
            unpaid_action_limit: BLOCK_UNPAID_ACTION_LIMIT,
            require_standard: !network.is_regtest(),
            permit_bare_multisig: false,
            max_datacarrier_bytes: MAX_DATACARRIER_BYTES,
            coinbase_must_be_shielded: network.params().coinbase_must_be_shielded,
        }
    }

    /// Applies every policy rule to a prepared transaction. The order of the rules is the
    /// order of zcashd `AcceptToMemoryPool`.
    pub fn admit(&self, tx: &PreparedTx, context: &PolicyContext<'_>) -> Result<(), PolicyReject> {
        if tx.is_coinbase {
            return Err(PolicyReject::Coinbase);
        }
        let next = RuleEpoch::of(context.rules);
        if tx.epoch != next {
            return Err(PolicyReject::Epoch { tx: tx.epoch, next });
        }
        self.check_expiry(tx, context.next_height)?;
        if self.require_standard {
            self.check_standard_tx(tx)?;
        }
        check_final(tx, context)?;
        check_coinbase_spends(tx, context.next_height, self.coinbase_must_be_shielded)?;
        if self.require_standard {
            check_standard_inputs(tx)?;
        }
        if tx.sigops > MAX_STANDARD_TX_SIGOPS {
            return Err(PolicyReject::TooManySigops { sigops: tx.sigops });
        }
        self.check_fee(tx)?;
        check_block_limits(tx, context.rules)
    }

    /// ZIP 203 and the zcashd rule for a transaction that expires soon.
    fn check_expiry(&self, tx: &PreparedTx, next_height: u32) -> Result<(), PolicyReject> {
        let expiry = tx.expiry_height;
        if expiry == 0 {
            return Ok(());
        }
        if next_height > expiry {
            return Err(PolicyReject::Expired {
                expiry,
                next_height,
            });
        }
        if !is_relayable(expiry, next_height) {
            return Err(PolicyReject::ExpiringSoon {
                expiry,
                next_height,
            });
        }
        Ok(())
    }

    /// zcashd `IsStandardTx`: the scriptSig rules and the output rules.
    fn check_standard_tx(&self, tx: &PreparedTx) -> Result<(), PolicyReject> {
        let Some(bundle) = tx.raw.tx.transparent_bundle() else {
            return Ok(());
        };
        for (input, txin) in bundle.vin.iter().enumerate() {
            let script_sig = &txin.script_sig().0 .0;
            if script_sig.len() > MAX_STANDARD_SCRIPTSIG_SIZE {
                return Err(PolicyReject::ScriptSigSize {
                    input,
                    size: script_sig.len(),
                });
            }
            if !Code(script_sig.clone()).is_push_only() {
                return Err(PolicyReject::ScriptSigNotPushOnly { input });
            }
        }
        let mut data_outputs = 0;
        for (output, txout) in bundle.vout.iter().enumerate() {
            let script = &txout.script_pubkey().0 .0;
            match script_kind(script) {
                None => return Err(PolicyReject::ScriptPubKey { output }),
                Some(ScriptKind::NullData { .. }) => {
                    if script.len() > self.max_datacarrier_bytes {
                        return Err(PolicyReject::DataCarrierSize {
                            output,
                            size: script.len(),
                            limit: self.max_datacarrier_bytes,
                        });
                    }
                    data_outputs += 1;
                    continue;
                }
                Some(ScriptKind::MultiSig { pubkeys, .. }) => {
                    if pubkeys.len() > MAX_STANDARD_MULTISIG_PUBKEYS {
                        return Err(PolicyReject::ScriptPubKey { output });
                    }
                    if !self.permit_bare_multisig {
                        return Err(PolicyReject::BareMultisig { output });
                    }
                }
                Some(_) => {}
            }
            let value = txout.value().into_u64();
            let threshold = dust_threshold(script.len());
            if value < threshold {
                return Err(PolicyReject::Dust {
                    output,
                    value,
                    threshold,
                });
            }
        }
        if data_outputs > 1 {
            return Err(PolicyReject::MultiOpReturn(data_outputs));
        }
        Ok(())
    }

    /// ZIP 317 unpaid actions and the minimum relay fee.
    fn check_fee(&self, tx: &PreparedTx) -> Result<(), PolicyReject> {
        let conventional_fee = self.zip317.conventional_fee(logical_actions(&tx.raw.tx));
        let unpaid = self.zip317.unpaid_actions(tx.fee, conventional_fee);
        if unpaid > self.unpaid_action_limit {
            return Err(PolicyReject::UnpaidActions {
                unpaid,
                limit: self.unpaid_action_limit,
            });
        }
        let minimum = min_relay_fee(tx.raw.bytes.len());
        if tx.fee < minimum {
            return Err(PolicyReject::FeeBelowMinimumRelay {
                fee: tx.fee,
                minimum,
            });
        }
        Ok(())
    }
}

/// zcashd `CheckFinalTx`: `IsFinalTx` with the height of the next block and the
/// median-time-past of the tip.
fn check_final(tx: &PreparedTx, context: &PolicyContext<'_>) -> Result<(), PolicyReject> {
    let lock_time = tx.lock_time;
    if lock_time == 0 {
        return Ok(());
    }
    let reference = if lock_time < LOCKTIME_THRESHOLD {
        context.next_height
    } else {
        context.median_time_past
    };
    if lock_time < reference {
        return Ok(());
    }
    let all_final = tx
        .raw
        .tx
        .transparent_bundle()
        .into_iter()
        .flat_map(|b| &b.vin)
        .all(|txin| txin.sequence() == u32::MAX);
    if all_final {
        return Ok(());
    }
    Err(PolicyReject::NonFinal {
        lock_time,
        next_height: context.next_height,
        median_time_past: context.median_time_past,
    })
}

/// The two rules of a transaction that spends a coinbase output, at the height of the next
/// block: maturity, and with `must_shield` no transparent output.
fn check_coinbase_spends(
    tx: &PreparedTx,
    next_height: u32,
    must_shield: bool,
) -> Result<(), PolicyReject> {
    let has_transparent_output =
        must_shield && matches!(tx.raw.tx.transparent_bundle(), Some(b) if !b.vout.is_empty());
    for (input, coin) in tx.spent.iter().enumerate() {
        if !coin.is_coinbase {
            continue;
        }
        if next_height < coin.height.saturating_add(COINBASE_MATURITY) {
            return Err(PolicyReject::ImmatureCoinbase {
                input,
                created: coin.height,
                next_height,
            });
        }
        if has_transparent_output {
            return Err(PolicyReject::UnshieldedCoinbaseSpend { input });
        }
    }
    Ok(())
}

/// zcashd `AreInputsStandard`.
fn check_standard_inputs(tx: &PreparedTx) -> Result<(), PolicyReject> {
    let Some(bundle) = tx.raw.tx.transparent_bundle() else {
        return Ok(());
    };
    assert_eq!(
        bundle.vin.len(),
        tx.spent.len(),
        "a prepared transaction holds one spent coin for each input"
    );
    for (input, (txin, coin)) in bundle.vin.iter().zip(&tx.spent).enumerate() {
        if !input_is_standard(&txin.script_sig().0 .0, coin) {
            return Err(PolicyReject::NonStandardInput { input });
        }
    }
    Ok(())
}

/// A transaction above a per-block limit of the next rule set is in no block (Zakura
/// applies `shielded_action_limits_are_valid` to one transaction,
/// `zakura-consensus/src/transaction.rs:474-482`).
fn check_block_limits(tx: &PreparedTx, rules: &RuleSet) -> Result<(), PolicyReject> {
    let limits = &rules.limits;
    let cost = tx
        .orchard_actions
        .saturating_add(tx.ironwood_actions)
        .saturating_add(tx.sapling_ios);
    for (what, count, limit) in [
        ("Orchard action", tx.orchard_actions, limits.orchard_actions),
        (
            "Ironwood action",
            tx.ironwood_actions,
            limits.ironwood_actions,
        ),
        (
            "Sapling spend and output",
            tx.sapling_ios,
            limits.sapling_ios,
        ),
        ("shielded cost", cost, limits.shielded_cost),
    ] {
        if count > limit {
            return Err(PolicyReject::AboveBlockLimit { what, count, limit });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use hayai_consensus::{BlockLimits, TX_EXPIRY_HEIGHT_THRESHOLD};
    use hayai_crypto::zcash_protocol::consensus::BranchId;

    use super::*;
    use crate::test_support::{
        multisig, op_return, p2pkh, p2sh, prepared, push, TxSpec, BRANCH, P2PKH_SIG,
    };

    const NEXT: u32 = 1_000;
    const MTP: u32 = 1_700_000_000;

    fn rules() -> &'static RuleSet {
        RuleSet::of_branch(BRANCH).expect("the test branch has a rule set")
    }

    fn admit_with(policy: &MempoolPolicy, spec: &TxSpec) -> Result<(), PolicyReject> {
        policy.admit(
            &prepared(spec),
            &PolicyContext {
                next_height: NEXT,
                median_time_past: MTP,
                rules: rules(),
            },
        )
    }

    fn admit(spec: &TxSpec) -> Result<(), PolicyReject> {
        admit_with(&MempoolPolicy::of(Network::Mainnet), spec)
    }

    #[test]
    fn a_standard_transaction_is_admitted_and_a_coinbase_is_not() {
        assert_eq!(admit(&TxSpec::standard()), Ok(()));
        let mut coinbase = prepared(&TxSpec::standard());
        coinbase.is_coinbase = true;
        let context = PolicyContext {
            next_height: NEXT,
            median_time_past: MTP,
            rules: rules(),
        };
        assert_eq!(
            MempoolPolicy::of(Network::Mainnet).admit(&coinbase, &context),
            Err(PolicyReject::Coinbase)
        );
    }

    #[test]
    fn the_epoch_is_the_epoch_of_the_next_block() {
        let mut tx = prepared(&TxSpec::standard());
        tx.epoch = RuleEpoch::consensus(BranchId::Canopy);
        let context = PolicyContext {
            next_height: NEXT,
            median_time_past: MTP,
            rules: rules(),
        };
        let Err(PolicyReject::Epoch { .. }) =
            MempoolPolicy::of(Network::Mainnet).admit(&tx, &context)
        else {
            panic!("an old epoch is rejected");
        };
    }

    #[test]
    fn expiry_boundaries() {
        let with = |expiry| TxSpec {
            expiry,
            ..TxSpec::standard()
        };
        // No expiry, and the largest expiry height.
        assert_eq!(admit(&with(0)), Ok(()));
        assert_eq!(admit(&with(TX_EXPIRY_HEIGHT_THRESHOLD - 1)), Ok(()));
        // The first height that does not expire soon.
        assert_eq!(admit(&with(NEXT + TX_EXPIRING_SOON_THRESHOLD)), Ok(()));
        assert_eq!(
            admit(&with(NEXT + TX_EXPIRING_SOON_THRESHOLD - 1)),
            Err(PolicyReject::ExpiringSoon {
                expiry: NEXT + 2,
                next_height: NEXT
            })
        );
        // The next block can contain a transaction whose expiry is its height.
        assert_eq!(
            admit(&with(NEXT)),
            Err(PolicyReject::ExpiringSoon {
                expiry: NEXT,
                next_height: NEXT
            })
        );
        assert_eq!(
            admit(&with(NEXT - 1)),
            Err(PolicyReject::Expired {
                expiry: NEXT - 1,
                next_height: NEXT
            })
        );
    }

    #[test]
    fn script_sig_size_boundary() {
        // A P2SH spend whose redeem script is not standard has no item count rule, so
        // only the size rule applies.
        let with = |size: usize| {
            // `OP_PUSHDATA2 <len>` + data, then a 1-byte redeem script (`OP_1`).
            let data = size - 3 - 2;
            let mut script_sig = vec![0x4d, (data & 0xff) as u8, (data >> 8) as u8];
            script_sig.extend(std::iter::repeat_n(0u8, data));
            script_sig.extend(push(&[0x51]));
            assert_eq!(script_sig.len(), size);
            // The value pays the logical actions of the large scriptSig.
            TxSpec {
                inputs: vec![(script_sig, p2sh(1), 200_000)],
                ..TxSpec::standard()
            }
        };
        assert_eq!(admit(&with(MAX_STANDARD_SCRIPTSIG_SIZE)), Ok(()));
        assert_eq!(
            admit(&with(MAX_STANDARD_SCRIPTSIG_SIZE + 1)),
            Err(PolicyReject::ScriptSigSize {
                input: 0,
                size: MAX_STANDARD_SCRIPTSIG_SIZE + 1
            })
        );
    }

    #[test]
    fn script_sig_is_push_only() {
        let mut script_sig = P2PKH_SIG.to_vec();
        // OP_NOP after the two pushes.
        script_sig.push(0x61);
        let spec = TxSpec {
            inputs: vec![(script_sig, p2pkh(1), 100_000)],
            ..TxSpec::standard()
        };
        assert_eq!(
            admit(&spec),
            Err(PolicyReject::ScriptSigNotPushOnly { input: 0 })
        );
        // Regtest does not require standard transactions.
        assert_eq!(
            admit_with(&MempoolPolicy::of(Network::Regtest), &spec),
            Ok(())
        );
    }

    #[test]
    fn output_scripts_are_standard() {
        let with = |script: Vec<u8>| TxSpec {
            outputs: vec![(script, 50_000)],
            ..TxSpec::standard()
        };
        assert_eq!(admit(&with(p2pkh(9))), Ok(()));
        assert_eq!(admit(&with(p2sh(9))), Ok(()));
        // `OP_1`: anyone can spend.
        assert_eq!(
            admit(&with(vec![0x51])),
            Err(PolicyReject::ScriptPubKey { output: 0 })
        );
    }

    #[test]
    fn bare_multisig_follows_the_policy_flag() {
        let with = |keys: usize| TxSpec {
            outputs: vec![(multisig(1, keys), 50_000)],
            ..TxSpec::standard()
        };
        assert_eq!(
            admit(&with(3)),
            Err(PolicyReject::BareMultisig { output: 0 })
        );
        let permit = MempoolPolicy {
            permit_bare_multisig: true,
            ..MempoolPolicy::of(Network::Mainnet)
        };
        assert_eq!(
            admit_with(&permit, &with(MAX_STANDARD_MULTISIG_PUBKEYS)),
            Ok(())
        );
        assert_eq!(
            admit_with(&permit, &with(MAX_STANDARD_MULTISIG_PUBKEYS + 1)),
            Err(PolicyReject::ScriptPubKey { output: 0 })
        );
    }

    #[test]
    fn data_carrier_size_and_count_boundaries() {
        let with = |scripts: Vec<Vec<u8>>| {
            let mut outputs = vec![(p2pkh(9), 50_000)];
            outputs.extend(scripts.into_iter().map(|s| (s, 0)));
            TxSpec {
                outputs,
                ..TxSpec::standard()
            }
        };
        let at_limit = op_return(MAX_DATACARRIER_BYTES);
        assert_eq!(at_limit.len(), MAX_DATACARRIER_BYTES);
        // A data output has no dust rule: its value is 0.
        assert_eq!(admit(&with(vec![at_limit.clone()])), Ok(()));
        assert_eq!(
            admit(&with(vec![op_return(MAX_DATACARRIER_BYTES + 1)])),
            Err(PolicyReject::DataCarrierSize {
                output: 1,
                size: MAX_DATACARRIER_BYTES + 1,
                limit: MAX_DATACARRIER_BYTES
            })
        );
        assert_eq!(
            admit(&with(vec![at_limit, op_return(10)])),
            Err(PolicyReject::MultiOpReturn(2))
        );
    }

    #[test]
    fn dust_boundary() {
        // A P2PKH output has 34 bytes: 3 * (100 * (34 + 148) / 1000) = 54 zatoshis.
        let threshold = dust_threshold(p2pkh(9).len());
        assert_eq!(threshold, 54);
        let with = |value| TxSpec {
            outputs: vec![(p2pkh(9), value)],
            ..TxSpec::standard()
        };
        assert_eq!(admit(&with(threshold)), Ok(()));
        assert_eq!(
            admit(&with(threshold - 1)),
            Err(PolicyReject::Dust {
                output: 0,
                value: threshold - 1,
                threshold
            })
        );
    }

    #[test]
    fn lock_time_boundaries() {
        let with = |lock_time, sequence| TxSpec {
            lock_time,
            sequence,
            ..TxSpec::standard()
        };
        // A height lock: final when the lock time is below the next height.
        assert_eq!(admit(&with(NEXT - 1, 0)), Ok(()));
        assert_eq!(
            admit(&with(NEXT, 0)),
            Err(PolicyReject::NonFinal {
                lock_time: NEXT,
                next_height: NEXT,
                median_time_past: MTP
            })
        );
        // A time lock: final when the lock time is below the median-time-past.
        assert_eq!(admit(&with(MTP - 1, 0)), Ok(()));
        let Err(PolicyReject::NonFinal { .. }) = admit(&with(MTP, 0)) else {
            panic!("a lock time equal to the median-time-past is not final");
        };
        // Final sequence numbers disable the lock time.
        assert_eq!(admit(&with(NEXT, u32::MAX)), Ok(()));
        assert_eq!(admit(&with(MTP, u32::MAX)), Ok(()));
    }

    #[test]
    fn coinbase_spend_boundaries() {
        let on = |network, coin_height, outputs: Vec<(Vec<u8>, u64)>| {
            let mut tx = prepared(&TxSpec {
                outputs,
                ..TxSpec::standard()
            });
            tx.spent[0].is_coinbase = true;
            tx.spent[0].height = coin_height;
            MempoolPolicy::of(network).admit(
                &tx,
                &PolicyContext {
                    next_height: NEXT,
                    median_time_past: MTP,
                    rules: rules(),
                },
            )
        };
        let with = |coin_height, outputs| on(Network::Mainnet, coin_height, outputs);
        // The synthetic transaction has no shielded output. The policy does not look at
        // the sink of funds: `prepare` does.
        assert_eq!(with(NEXT - COINBASE_MATURITY, vec![]), Ok(()));
        assert_eq!(
            with(NEXT - COINBASE_MATURITY + 1, vec![]),
            Err(PolicyReject::ImmatureCoinbase {
                input: 0,
                created: NEXT - COINBASE_MATURITY + 1,
                next_height: NEXT
            })
        );
        assert_eq!(
            with(NEXT - COINBASE_MATURITY, vec![(p2pkh(9), 50_000)]),
            Err(PolicyReject::UnshieldedCoinbaseSpend { input: 0 })
        );
        assert_eq!(
            on(
                Network::Testnet,
                NEXT - COINBASE_MATURITY,
                vec![(p2pkh(9), 50_000)]
            ),
            Err(PolicyReject::UnshieldedCoinbaseSpend { input: 0 })
        );
        // Regtest has the maturity rule and not the shielding rule (Zakura
        // `zakura-chain/src/transaction.rs:552-564`).
        assert_eq!(
            on(
                Network::Regtest,
                NEXT - COINBASE_MATURITY,
                vec![(p2pkh(9), 50_000)]
            ),
            Ok(())
        );
        assert_eq!(
            on(
                Network::Regtest,
                NEXT - COINBASE_MATURITY + 1,
                vec![(p2pkh(9), 50_000)]
            ),
            Err(PolicyReject::ImmatureCoinbase {
                input: 0,
                created: NEXT - COINBASE_MATURITY + 1,
                next_height: NEXT
            })
        );
    }

    #[test]
    fn inputs_are_standard_spends() {
        let with = |script_sig: Vec<u8>, script_pubkey: Vec<u8>| TxSpec {
            inputs: vec![(script_sig, script_pubkey, 100_000)],
            ..TxSpec::standard()
        };
        let reject = Err(PolicyReject::NonStandardInput { input: 0 });
        // P2PKH: exactly two stack items.
        assert_eq!(admit(&with(P2PKH_SIG.to_vec(), p2pkh(1))), Ok(()));
        assert_eq!(admit(&with(push(&[1]), p2pkh(1))), reject);
        // A spent script that is not standard.
        assert_eq!(admit(&with(push(&[1]), vec![0x51])), reject);
        // P2SH with a standard redeem script (1-of-1 multisig): one item for the redeem
        // script and two for the multisig.
        let redeem = multisig(1, 1);
        let mut good = push(&[]);
        good.extend(push(&[7; 71]));
        good.extend(push(&redeem));
        assert_eq!(admit(&with(good.clone(), p2sh(1))), Ok(()));
        let mut extra = push(&[1]);
        extra.extend(good);
        assert_eq!(admit(&with(extra, p2sh(1))), reject);
        // P2SH without a redeem script.
        assert_eq!(admit(&with(vec![], p2sh(1))), reject);
    }

    #[test]
    fn redeem_script_sigops_boundary() {
        // A redeem script that is not standard: n times OP_CHECKSIG.
        let with = |sigops: usize| TxSpec {
            inputs: vec![(push(&vec![0xac; sigops]), p2sh(1), 100_000)],
            ..TxSpec::standard()
        };
        assert_eq!(admit(&with(MAX_P2SH_SIGOPS as usize)), Ok(()));
        assert_eq!(
            admit(&with(MAX_P2SH_SIGOPS as usize + 1)),
            Err(PolicyReject::NonStandardInput { input: 0 })
        );
    }

    #[test]
    fn transaction_sigops_boundary() {
        let with = |sigops| {
            let mut tx = prepared(&TxSpec::standard());
            tx.sigops = sigops;
            MempoolPolicy::of(Network::Mainnet).admit(
                &tx,
                &PolicyContext {
                    next_height: NEXT,
                    median_time_past: MTP,
                    rules: rules(),
                },
            )
        };
        assert_eq!(with(MAX_STANDARD_TX_SIGOPS), Ok(()));
        assert_eq!(
            with(MAX_STANDARD_TX_SIGOPS + 1),
            Err(PolicyReject::TooManySigops {
                sigops: MAX_STANDARD_TX_SIGOPS + 1
            })
        );
    }

    #[test]
    fn unpaid_actions_boundary() {
        // 60 P2PKH outputs are 60 logical actions. The fee pays for `paid` of them.
        let with = |paid: u64| TxSpec {
            inputs: vec![(P2PKH_SIG.to_vec(), p2pkh(1), 60 * 1_000 + paid * 5_000)],
            outputs: (0..60).map(|i| (p2pkh(i), 1_000)).collect(),
            ..TxSpec::standard()
        };
        // The limit is 0, the value of Zakura and Zebra: the policy admits only a
        // transaction that pays for each of its logical actions (Zakura `mempool_checks`,
        // `zakura-chain/src/transaction/unmined/zip317.rs:166-175`).
        assert_eq!(BLOCK_UNPAID_ACTION_LIMIT, 0);
        assert_eq!(MempoolPolicy::of(Network::Mainnet).unpaid_action_limit, 0);
        assert_eq!(admit(&with(60)), Ok(()));
        assert_eq!(admit(&with(61)), Ok(()));
        for (paid, unpaid) in [(59, 1), (58, 2), (0, 60)] {
            assert_eq!(
                admit(&with(paid)),
                Err(PolicyReject::UnpaidActions { unpaid, limit: 0 })
            );
        }
        // The rule with another limit: the value 50 that ZIP 317 gives as the default.
        let zip317_default = MempoolPolicy {
            unpaid_action_limit: 50,
            ..MempoolPolicy::of(Network::Mainnet)
        };
        assert_eq!(admit_with(&zip317_default, &with(10)), Ok(()));
        assert_eq!(
            admit_with(&zip317_default, &with(9)),
            Err(PolicyReject::UnpaidActions {
                unpaid: 51,
                limit: 50
            })
        );
    }

    #[test]
    fn minimum_relay_fee_boundary() {
        // One input and one output: 2 grace actions. With the unpaid action limit of 0 a
        // fee below 10,000 zatoshis fails the unpaid action rule first, so the minimum
        // relay fee decides only under a policy with a higher limit (Zakura keeps the rule
        // in the same way, `zip317.rs:177-200`).
        let lenient = MempoolPolicy {
            unpaid_action_limit: 2,
            ..MempoolPolicy::of(Network::Mainnet)
        };
        let admit = |spec: &TxSpec| admit_with(&lenient, spec);
        let with = |fee: u64| TxSpec {
            inputs: vec![(P2PKH_SIG.to_vec(), p2pkh(1), 50_000 + fee)],
            ..TxSpec::standard()
        };
        let size = prepared(&with(0)).raw.bytes.len();
        assert!(size < 1_000);
        assert_eq!(min_relay_fee(size), MIN_RELAY_FEE_RATE);
        assert_eq!(admit(&with(MIN_RELAY_FEE_RATE)), Ok(()));
        assert_eq!(
            admit(&with(MIN_RELAY_FEE_RATE - 1)),
            Err(PolicyReject::FeeBelowMinimumRelay {
                fee: MIN_RELAY_FEE_RATE - 1,
                minimum: MIN_RELAY_FEE_RATE
            })
        );
        // The rate applies between the two bounds.
        assert_eq!(min_relay_fee(1_000), 100);
        assert_eq!(min_relay_fee(5_500), 550);
        assert_eq!(min_relay_fee(10_000), MIN_RELAY_FEE_CAP);
        assert_eq!(min_relay_fee(2_000_000), MIN_RELAY_FEE_CAP);
    }

    #[test]
    fn block_limits_boundary() {
        let nu7 = RuleSet {
            limits: BlockLimits::NU7,
            ..*rules()
        };
        let with = |orchard, ironwood, sapling| {
            let mut tx = prepared(&TxSpec::standard());
            tx.orchard_actions = orchard;
            tx.ironwood_actions = ironwood;
            tx.sapling_ios = sapling;
            MempoolPolicy::of(Network::Mainnet).admit(
                &tx,
                &PolicyContext {
                    next_height: NEXT,
                    median_time_past: MTP,
                    rules: &nu7,
                },
            )
        };
        let l = BlockLimits::NU7;
        // Each pool at its limit, alone: the shielded cost is at most the budget.
        for (orchard, ironwood, sapling) in [
            (l.orchard_actions, 0, 0),
            (0, l.ironwood_actions, 0),
            (0, 0, l.sapling_ios),
            (110, 110, 110),
        ] {
            assert_eq!(with(orchard, ironwood, sapling), Ok(()));
        }
        for (orchard, ironwood, sapling, what) in [
            (l.orchard_actions + 1, 0, 0, "Orchard action"),
            (0, l.ironwood_actions + 1, 0, "Ironwood action"),
            (0, 0, l.sapling_ios + 1, "Sapling spend and output"),
            // Every pool within its limit, and the three together above the budget.
            (110, 110, 111, "shielded cost"),
            (l.orchard_actions, 1, 0, "shielded cost"),
        ] {
            let Err(PolicyReject::AboveBlockLimit { what: found, .. }) =
                with(orchard, ironwood, sapling)
            else {
                panic!("one past a block limit is rejected");
            };
            assert_eq!(found, what);
        }
    }

    #[test]
    fn relay_skips_a_transaction_that_expires_soon() {
        assert!(is_relayable(0, NEXT));
        assert!(is_relayable(NEXT + TX_EXPIRING_SOON_THRESHOLD, NEXT));
        assert!(!is_relayable(NEXT + TX_EXPIRING_SOON_THRESHOLD - 1, NEXT));
    }
}
