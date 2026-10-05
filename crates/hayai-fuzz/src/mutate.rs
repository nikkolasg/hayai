//! The mutations of a seed block, and the recipe of a case.
//!
//! A [`Recipe`] is a seed, a list of [`Op`]s on the block model and the context, a list
//! of [`RawOp`]s on the bytes of the block, and two flags for the header fields that
//! commit to the body. [`build`] makes the case of a recipe. An operation with an index
//! outside the block takes the index modulo the length, so a recipe stays valid when the
//! minimisation removes an operation before it.

use serde::{Deserialize, Serialize};

use crate::context::{Coin, Context, OutPoint, ShieldedPool};
use crate::model::{Block, Section, Tx, TxIn, TxOut, TxParts};
use crate::reference;
use crate::seeds::{self, fix_header, sha256, SeedError, SeedSpec};

/// A 32-byte field of the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeaderField {
    Prev,
    Merkle,
    Commitments,
    Nonce,
}

/// A new transaction that spends one transparent output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// The output that the transaction spends. With `parent`, the output of the block
    /// replaces it.
    pub coin: Coin,
    /// Whether the chain of the context has the coin.
    pub in_context: bool,
    /// Spends the output `.1` of the transaction `.0` of the block in place of `coin`.
    pub parent: Option<(usize, usize)>,
    #[serde(with = "hex::serde")]
    pub script_sig: Vec<u8>,
    pub sequence: u32,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
    pub expiry: u32,
    /// 4, 5 or 6. Another value is version 5 from NU5 and version 4 before NU5.
    pub version: u32,
    /// The consensus branch id. `None` is the branch of the height of the context.
    pub branch: Option<u32>,
    /// Adds the fee of the transaction to the first output of the coinbase.
    pub claim_fee: bool,
    /// The position of the transaction in the block.
    pub at: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    HeaderVersion(u32),
    HeaderTime(u32),
    HeaderBits(u32),
    HeaderFlip {
        field: HeaderField,
        bit: u8,
    },
    SolutionResize(u32),
    SolutionFlip(u32),

    SwapTxs(usize, usize),
    DuplicateTx {
        tx: usize,
        at: usize,
    },
    RemoveTx(usize),
    /// Repeats the last transactions so that the merkle root stays (CVE-2012-2459).
    MerkleDuplicateTail,
    StatedCount(u64),
    CountWidth(u8),

    LockTime {
        tx: usize,
        value: u32,
    },
    Expiry {
        tx: usize,
        value: u32,
    },
    TxHeader {
        tx: usize,
        value: u32,
    },
    TxGroup {
        tx: usize,
        value: u32,
    },
    TxBranch {
        tx: usize,
        value: u32,
    },
    /// Writes the counts and the script lengths of the transaction in a long encoding.
    TxCountWidth {
        tx: usize,
        width: u8,
    },
    Sequence {
        tx: usize,
        input: usize,
        value: u32,
    },
    InRemove {
        tx: usize,
        input: usize,
    },
    InDuplicate {
        tx: usize,
        input: usize,
    },
    InScript {
        tx: usize,
        input: usize,
        #[serde(with = "hex::serde")]
        script: Vec<u8>,
    },
    InScriptFlip {
        tx: usize,
        input: usize,
        bit: u32,
    },
    InPrev {
        tx: usize,
        input: usize,
        hash: [u8; 32],
        index: u32,
    },
    /// Copies the input `input` of the transaction `from` into the transaction `tx`.
    InCopy {
        tx: usize,
        from: usize,
        input: usize,
    },
    InPush {
        tx: usize,
        input: TxIn,
    },
    OutValue {
        tx: usize,
        output: usize,
        value: u64,
    },
    OutAdd {
        tx: usize,
        output: usize,
        delta: i64,
    },
    OutRemove {
        tx: usize,
        output: usize,
    },
    OutDuplicate {
        tx: usize,
        output: usize,
    },
    OutSwap {
        tx: usize,
        a: usize,
        b: usize,
    },
    OutScript {
        tx: usize,
        output: usize,
        #[serde(with = "hex::serde")]
        script: Vec<u8>,
    },
    OutScriptFlip {
        tx: usize,
        output: usize,
        bit: u32,
    },
    OutPush {
        tx: usize,
        output: TxOut,
    },

    Flags {
        tx: usize,
        section: usize,
        value: u8,
    },
    ValueBalance {
        tx: usize,
        section: usize,
        value: i64,
    },
    AnchorFlip {
        tx: usize,
        section: usize,
        bit: u8,
    },
    /// Copies the nullifier of the action `from` to the action `to`.
    NullifierCopy {
        tx: usize,
        section: usize,
        from: usize,
        to: usize,
    },
    ActionFlip {
        tx: usize,
        section: usize,
        bit: u32,
    },
    ProofFlip {
        tx: usize,
        section: usize,
        bit: u32,
    },
    ProofLen {
        tx: usize,
        section: usize,
        value: u64,
    },
    SigFlip {
        tx: usize,
        section: usize,
        bit: u32,
    },
    TailFlip {
        tx: usize,
        bit: u32,
    },
    TailTruncate {
        tx: usize,
        len: u32,
    },
    TailAppend {
        tx: usize,
        #[serde(with = "hex::serde")]
        bytes: Vec<u8>,
    },

    TxRawFlip {
        tx: usize,
        bit: u32,
    },
    TxRawSet {
        tx: usize,
        at: u32,
        value: u8,
    },
    TxRawInsert {
        tx: usize,
        at: u32,
        #[serde(with = "hex::serde")]
        bytes: Vec<u8>,
    },
    TxRawDelete {
        tx: usize,
        at: u32,
        len: u32,
    },

    Spend(Spend),

    CoinRemove(usize),
    CoinHeight {
        coin: usize,
        height: u32,
    },
    CoinCoinbase {
        coin: usize,
        value: bool,
    },
    CoinValue {
        coin: usize,
        value: u64,
    },
    CoinScriptFlip {
        coin: usize,
        bit: u32,
    },
    /// Puts the nullifier of an action of the block into the chain of the context.
    ChainNullifier {
        tx: usize,
        section: usize,
        action: usize,
    },
    ContextHeight(u32),
    ContextParentFlip(u8),
}

/// A change of the bytes of the block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RawOp {
    Flip(u32),
    Set {
        at: u32,
        value: u8,
    },
    Insert {
        at: u32,
        #[serde(with = "hex::serde")]
        bytes: Vec<u8>,
    },
    Delete {
        at: u32,
        len: u32,
    },
    Truncate(u32),
    Append(#[serde(with = "hex::serde")] Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    pub seed: SeedSpec,
    pub ops: Vec<Op>,
    pub raw: Vec<RawOp>,
    /// After the operations, the header gets the merkle root of the body.
    pub fix_merkle: bool,
    /// After the operations, the header gets the commitments to the history root and to
    /// the authorizing data of the body.
    pub fix_commitments: bool,
}

/// A block and the context on which the two implementations check it.
#[derive(Clone, Debug)]
pub struct Case {
    pub bytes: Vec<u8>,
    pub ctx: Context,
}

fn flip(bytes: &mut [u8], bit: u32) {
    if bytes.is_empty() {
        return;
    }
    let at = (bit as usize / 8) % bytes.len();
    bytes[at] ^= 1 << (bit % 8);
}

fn parts(block: &mut Block, tx: usize) -> Option<&mut TxParts> {
    let len = block.txs.len();
    if len == 0 {
        return None;
    }
    block.txs[tx % len].parts_mut()
}

fn section(parts: &TxParts, index: usize) -> Option<Section> {
    let sections = parts.sections();
    if sections.is_empty() {
        return None;
    }
    Some(sections[index % sections.len()])
}

/// The outpoint of the coin that the operation number `n` of a recipe adds.
fn fresh_outpoint(n: usize) -> OutPoint {
    OutPoint {
        hash: sha256(&[b"hayai-fuzz coin", &(n as u64).to_le_bytes()]),
        index: (n % 3) as u32,
    }
}

fn apply_spend(spend: &Spend, n: usize, block: &mut Block, ctx: &mut Context, branch: u32) {
    let (outpoint, value) = match spend.parent {
        Some((tx, output)) => {
            if block.txs.is_empty() {
                return;
            }
            let tx = tx % block.txs.len();
            let Some(txid) = reference::txid(&block.txs[tx].bytes()) else {
                return;
            };
            let Tx::Parts(parent) = &block.txs[tx] else {
                return;
            };
            if parent.vout.is_empty() {
                return;
            }
            let output = output % parent.vout.len();
            (
                OutPoint {
                    hash: txid,
                    index: output as u32,
                },
                parent.vout[output].value,
            )
        }
        None => {
            let outpoint = fresh_outpoint(n);
            let mut coin = spend.coin.clone();
            coin.value = coin.value.min(crate::context::MAX_COIN);
            let value = coin.value;
            if spend.in_context {
                ctx.coins.push((outpoint, coin));
            }
            (outpoint, value)
        }
    };
    let version = match spend.version {
        4..=6 => spend.version,
        // The newest version that every epoch of the height allows.
        _ if nu5_active(ctx) => 5,
        _ => 4,
    };
    let mut tx = TxParts::transparent(version, spend.branch.unwrap_or(branch));
    tx.lock_time = spend.lock_time;
    tx.expiry = spend.expiry;
    tx.vin.push(TxIn {
        prev_hash: outpoint.hash,
        prev_index: outpoint.index,
        script: spend.script_sig.clone(),
        sequence: spend.sequence,
    });
    tx.vout = spend.outputs.clone();
    if spend.claim_fee {
        let paid = spend
            .outputs
            .iter()
            .fold(0u64, |sum, output| sum.saturating_add(output.value));
        let fee = value.saturating_sub(paid);
        if let Some(coinbase) = parts(block, 0) {
            if let Some(miner) = coinbase.vout.first_mut() {
                miner.value = miner.value.wrapping_add(fee);
            }
        }
    }
    let at = spend.at.clamp(1, block.txs.len().max(1));
    block.txs.insert(at.min(block.txs.len()), Tx::Parts(tx));
}

/// Applies the operation number `n` of a recipe. `branch` is the consensus branch id of
/// the height of the seed.
fn apply(op: &Op, n: usize, block: &mut Block, ctx: &mut Context, branch: u32) {
    let txs = block.txs.len();
    match op {
        Op::HeaderVersion(value) => block.header.version = *value,
        Op::HeaderTime(value) => block.header.time = *value,
        Op::HeaderBits(value) => block.header.bits = *value,
        Op::HeaderFlip { field, bit } => {
            let field = match field {
                HeaderField::Prev => &mut block.header.prev,
                HeaderField::Merkle => &mut block.header.merkle,
                HeaderField::Commitments => &mut block.header.commitments,
                HeaderField::Nonce => &mut block.header.nonce,
            };
            flip(field, u32::from(*bit));
        }
        Op::SolutionResize(len) => {
            // A solution above 3 times its valid length adds no case.
            block.header.solution.resize((*len as usize).min(4_096), 0)
        }
        Op::SolutionFlip(bit) => flip(&mut block.header.solution, *bit),

        Op::SwapTxs(a, b) if txs > 0 => block.txs.swap(a % txs, b % txs),
        Op::DuplicateTx { tx, at } if txs > 0 => {
            let copy = block.txs[tx % txs].clone();
            block.txs.insert(at % (txs + 1), copy);
        }
        Op::RemoveTx(tx) if txs > 0 => {
            block.txs.remove(tx % txs);
        }
        Op::MerkleDuplicateTail => {
            // With n = m * 2^j and m odd above 1, the tree repeats the last 2^j hashes on
            // one level, so a block that repeats the last 2^j transactions has the same
            // root.
            let tail = 1usize << txs.trailing_zeros().min(20);
            if txs > 0 && tail < txs {
                let copies: Vec<Tx> = block.txs[txs - tail..].to_vec();
                block.txs.extend(copies);
            }
        }
        Op::StatedCount(count) => block.stated_count = Some(*count),
        Op::CountWidth(width) => block.count_width = Some(*width),

        Op::LockTime { tx, value } => {
            if let Some(parts) = parts(block, *tx) {
                parts.lock_time = *value;
            }
        }
        Op::Expiry { tx, value } => {
            if let Some(parts) = parts(block, *tx) {
                parts.expiry = *value;
            }
        }
        Op::TxHeader { tx, value } => {
            if let Some(parts) = parts(block, *tx) {
                parts.header = *value;
            }
        }
        Op::TxGroup { tx, value } => {
            if let Some(parts) = parts(block, *tx) {
                parts.group = *value;
            }
        }
        Op::TxBranch { tx, value } => {
            if let Some(parts) = parts(block, *tx) {
                parts.branch = *value;
            }
        }
        Op::TxCountWidth { tx, width } => {
            if let Some(parts) = parts(block, *tx) {
                parts.count_width = Some(*width);
            }
        }
        Op::Sequence { tx, input, value } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    parts.vin[input % len].sequence = *value;
                }
            }
        }
        Op::InRemove { tx, input } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    parts.vin.remove(input % len);
                }
            }
        }
        Op::InDuplicate { tx, input } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    let copy = parts.vin[input % len].clone();
                    parts.vin.push(copy);
                }
            }
        }
        Op::InScript { tx, input, script } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    parts.vin[input % len].script = script.clone();
                }
            }
        }
        Op::InScriptFlip { tx, input, bit } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    flip(&mut parts.vin[input % len].script, *bit);
                }
            }
        }
        Op::InPrev {
            tx,
            input,
            hash,
            index,
        } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vin.len();
                if len > 0 {
                    parts.vin[input % len].prev_hash = *hash;
                    parts.vin[input % len].prev_index = *index;
                }
            }
        }
        Op::InCopy { tx, from, input } if txs > 0 => {
            let copy = match &block.txs[from % txs] {
                Tx::Parts(source) if !source.vin.is_empty() => {
                    source.vin[input % source.vin.len()].clone()
                }
                _ => return,
            };
            if let Some(parts) = parts(block, *tx) {
                parts.vin.push(copy);
            }
        }
        Op::InPush { tx, input } => {
            if let Some(parts) = parts(block, *tx) {
                parts.vin.push(input.clone());
            }
        }
        Op::OutValue { tx, output, value } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    parts.vout[output % len].value = *value;
                }
            }
        }
        Op::OutAdd { tx, output, delta } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    let value = &mut parts.vout[output % len].value;
                    *value = value.wrapping_add(*delta as u64);
                }
            }
        }
        Op::OutRemove { tx, output } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    parts.vout.remove(output % len);
                }
            }
        }
        Op::OutDuplicate { tx, output } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    let copy = parts.vout[output % len].clone();
                    parts.vout.push(copy);
                }
            }
        }
        Op::OutSwap { tx, a, b } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    parts.vout.swap(a % len, b % len);
                }
            }
        }
        Op::OutScript { tx, output, script } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    parts.vout[output % len].script = script.clone();
                }
            }
        }
        Op::OutScriptFlip { tx, output, bit } => {
            if let Some(parts) = parts(block, *tx) {
                let len = parts.vout.len();
                if len > 0 {
                    flip(&mut parts.vout[output % len].script, *bit);
                }
            }
        }
        Op::OutPush { tx, output } => {
            if let Some(parts) = parts(block, *tx) {
                parts.vout.push(output.clone());
            }
        }

        Op::Flags {
            tx,
            section: s,
            value,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    parts.tail[section.flags_at] = *value;
                }
            }
        }
        Op::ValueBalance {
            tx,
            section: s,
            value,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    let at = section.flags_at + 1;
                    parts.tail[at..at + 8].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        Op::AnchorFlip {
            tx,
            section: s,
            bit,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    let at = section.flags_at + 9;
                    flip(&mut parts.tail[at..at + 32], u32::from(*bit));
                }
            }
        }
        Op::NullifierCopy {
            tx,
            section: s,
            from,
            to,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    let nullifier_at = |action: usize| {
                        section.actions_at
                            + (action % section.actions) * crate::model::ACTION_BYTES
                            + 32
                    };
                    let (from, to) = (nullifier_at(*from), nullifier_at(*to));
                    parts.tail.copy_within(from..from + 32, to);
                }
            }
        }
        Op::ActionFlip {
            tx,
            section: s,
            bit,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    flip(&mut parts.tail[section.actions_at..section.flags_at], *bit);
                }
            }
        }
        Op::ProofFlip {
            tx,
            section: s,
            bit,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    flip(&mut parts.tail[section.proof_at..section.sigs_at], *bit);
                }
            }
        }
        Op::ProofLen {
            tx,
            section: s,
            value,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    let mut encoded = Vec::new();
                    crate::model::write_compact(&mut encoded, *value);
                    parts
                        .tail
                        .splice(section.proof_len_at..section.proof_at, encoded);
                }
            }
        }
        Op::SigFlip {
            tx,
            section: s,
            bit,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    flip(&mut parts.tail[section.sigs_at..section.end], *bit);
                }
            }
        }
        Op::TailFlip { tx, bit } => {
            if let Some(parts) = parts(block, *tx) {
                flip(&mut parts.tail, *bit);
            }
        }
        Op::TailTruncate { tx, len } => {
            if let Some(parts) = parts(block, *tx) {
                let len = (*len as usize).min(parts.tail.len());
                parts.tail.truncate(len);
            }
        }
        Op::TailAppend { tx, bytes } => {
            if let Some(parts) = parts(block, *tx) {
                parts.tail.extend_from_slice(bytes);
            }
        }

        Op::TxRawFlip { tx, bit } if txs > 0 => flip(block.txs[tx % txs].raw_mut(), *bit),
        Op::TxRawSet { tx, at, value } if txs > 0 => {
            let raw = block.txs[tx % txs].raw_mut();
            if !raw.is_empty() {
                let at = *at as usize % raw.len();
                raw[at] = *value;
            }
        }
        Op::TxRawInsert { tx, at, bytes } if txs > 0 => {
            let raw = block.txs[tx % txs].raw_mut();
            let at = *at as usize % (raw.len() + 1);
            raw.splice(at..at, bytes.iter().copied());
        }
        Op::TxRawDelete { tx, at, len } if txs > 0 => {
            let raw = block.txs[tx % txs].raw_mut();
            if !raw.is_empty() {
                let at = *at as usize % raw.len();
                let end = (at + *len as usize).min(raw.len());
                raw.drain(at..end);
            }
        }

        Op::Spend(spend) => apply_spend(spend, n, block, ctx, branch),

        Op::CoinRemove(coin) if !ctx.coins.is_empty() => {
            ctx.coins.remove(coin % ctx.coins.len());
        }
        Op::CoinHeight { coin, height } if !ctx.coins.is_empty() => {
            let coin = coin % ctx.coins.len();
            ctx.coins[coin].1.height = *height;
        }
        Op::CoinCoinbase { coin, value } if !ctx.coins.is_empty() => {
            let coin = coin % ctx.coins.len();
            ctx.coins[coin].1.coinbase = *value;
        }
        Op::CoinValue { coin, value } if !ctx.coins.is_empty() => {
            let coin = coin % ctx.coins.len();
            // A coin of the chain holds an amount of money.
            ctx.coins[coin].1.value = (*value).min(crate::context::MAX_COIN);
        }
        Op::CoinScriptFlip { coin, bit } if !ctx.coins.is_empty() => {
            let coin = coin % ctx.coins.len();
            flip(&mut ctx.coins[coin].1.script, *bit);
        }
        Op::ChainNullifier {
            tx,
            section: s,
            action,
        } => {
            if let Some(parts) = parts(block, *tx) {
                if let Some(section) = section(parts, *s) {
                    let at = section.actions_at
                        + (action % section.actions) * crate::model::ACTION_BYTES
                        + 32;
                    let nullifier: [u8; 32] = parts.tail[at..at + 32].try_into().expect("32 bytes");
                    let pool = if section.ironwood {
                        ShieldedPool::Ironwood
                    } else {
                        ShieldedPool::Orchard
                    };
                    ctx.nullifiers.push((pool, nullifier));
                }
            }
        }
        Op::ContextHeight(height) => {
            ctx.height = (*height).max(4);
            // The history tree of the seed ends below the height of the seed.
            ctx.history_root = None;
        }
        Op::ContextParentFlip(bit) => flip(&mut ctx.parent, u32::from(*bit)),
        // An operation on a block without transactions or on a context without coins.
        Op::SwapTxs(..)
        | Op::DuplicateTx { .. }
        | Op::RemoveTx(_)
        | Op::InCopy { .. }
        | Op::TxRawFlip { .. }
        | Op::TxRawSet { .. }
        | Op::TxRawInsert { .. }
        | Op::TxRawDelete { .. }
        | Op::CoinRemove(_)
        | Op::CoinHeight { .. }
        | Op::CoinCoinbase { .. }
        | Op::CoinValue { .. }
        | Op::CoinScriptFlip { .. } => {}
    }
}

fn apply_raw(op: &RawOp, bytes: &mut Vec<u8>) {
    match op {
        RawOp::Flip(bit) => flip(bytes, *bit),
        RawOp::Set { at, value } => {
            if !bytes.is_empty() {
                let at = *at as usize % bytes.len();
                bytes[at] = *value;
            }
        }
        RawOp::Insert { at, bytes: new } => {
            let at = *at as usize % (bytes.len() + 1);
            bytes.splice(at..at, new.iter().copied());
        }
        RawOp::Delete { at, len } => {
            if !bytes.is_empty() {
                let at = *at as usize % bytes.len();
                let end = (at + *len as usize).min(bytes.len());
                bytes.drain(at..end);
            }
        }
        RawOp::Truncate(len) => {
            let len = (*len as usize).min(bytes.len());
            bytes.truncate(len);
        }
        RawOp::Append(new) => bytes.extend_from_slice(new),
    }
}

/// Whether NU5 is active at the height of `ctx`.
fn nu5_active(ctx: &Context) -> bool {
    let network = crate::hayai_side::network(ctx.network);
    matches!(
        network.activation_height(hayai_consensus::Upgrade::Nu5),
        Some(nu5) if ctx.height >= nu5
    )
}

/// The consensus branch id that hayai has for the height of `ctx`, or 0.
pub fn branch_of(ctx: &Context) -> u32 {
    match hayai_consensus::rules_at(crate::hayai_side::network(ctx.network), ctx.height) {
        Ok(rules) => u32::from(rules.branch_id),
        Err(_) => 0,
    }
}

/// The case of `recipe`.
pub fn build(recipe: &Recipe) -> Result<Case, SeedError> {
    let seed = seeds::seed(recipe.seed)?;
    let mut block = seed.block.clone();
    let mut ctx = seed.ctx.clone();
    let branch = branch_of(&ctx);
    for (n, op) in recipe.ops.iter().enumerate() {
        apply(op, n, &mut block, &mut ctx, branch);
    }
    let mut bytes = block.bytes();
    for op in &recipe.raw {
        apply_raw(op, &mut bytes);
    }
    fix_header(&mut bytes, &ctx, recipe.fix_merkle, recipe.fix_commitments);
    Ok(Case { bytes, ctx })
}
