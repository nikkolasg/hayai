//! The zcashd `getblocktemplate` result built from a stored template.
//!
//! Field reference: zcashd `src/rpc/mining.cpp` (`getblocktemplate`) and Zakura's
//! `BlockTemplateResponse`. Every 32-byte hash is hex in display (byte-reversed) order, as
//! zcashd prints `uint256`. Transaction `data` is plain hex of the wire bytes.

use std::collections::HashMap;
use std::sync::Arc;

use hayai_consensus::header::MAX_FUTURE_BLOCK_TIME_MTP;
use hayai_consensus::{rules_at, ConsensusError, Network};
use hayai_template::live::{BLOCK_VERSION, MAX_BLOCK_BYTES};
use hayai_template::{Candidate, StoredTemplate};
use hayai_wire::header::BlockHash;
use serde::Serialize;

pub const NONCE_RANGE: &str = "00000000ffffffff";
pub const MUTABLE: [&str; 3] = ["time", "transactions", "prevblock"];

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct DefaultRoots {
    pub merkleroot: String,
    pub chainhistoryroot: String,
    pub authdataroot: String,
    pub blockcommitmentshash: String,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct TransactionTemplate {
    pub data: String,
    pub hash: String,
    pub authdigest: String,
    /// 1-based indexes into `transactions`.
    pub depends: Vec<usize>,
    pub fee: i64,
    pub sigops: u32,
    pub required: bool,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct BlockTemplate {
    pub capabilities: Vec<&'static str>,
    pub version: u32,
    pub previousblockhash: String,
    pub blockcommitmentshash: String,
    pub lightclientroothash: String,
    pub finalsaplingroothash: String,
    pub defaultroots: DefaultRoots,
    pub transactions: Vec<TransactionTemplate>,
    pub coinbasetxn: TransactionTemplate,
    pub longpollid: String,
    pub target: String,
    pub mintime: u32,
    pub mutable: Vec<&'static str>,
    pub noncerange: &'static str,
    pub sigoplimit: u32,
    pub sizelimit: usize,
    pub curtime: u32,
    pub bits: String,
    pub height: u32,
    pub maxtime: u32,
    pub workid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitold: Option<bool>,
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum TemplateError {
    #[error("template bits {0:#010x} encode no target")]
    Bits(u32),
    /// The height of the template has no rule set.
    #[error(transparent)]
    Rules(#[from] ConsensusError),
}

/// Hex of a 32-byte value in display order.
pub fn display_hex(bytes: &[u8; 32]) -> String {
    BlockHash(*bytes).to_string()
}

/// Builds the result for `template` at wall-clock time `now` (seconds). `network` is the
/// network of the template: it gives the sigop limit and the start height of the rule for
/// `maxtime`.
pub fn block_template(
    template: &StoredTemplate,
    network: Network,
    now: u32,
    submitold: Option<bool>,
) -> Result<BlockTemplate, TemplateError> {
    let rules = rules_at(network, template.tip.height)?;
    let index: HashMap<_, _> = template
        .txs
        .iter()
        .enumerate()
        .map(|(i, c)| (c.wtxid, i + 1))
        .collect();
    let transactions = template
        .txs
        .iter()
        .map(|c| transaction_template(c, &index))
        .collect();
    // The fees that the coinbase collects: the miner share from NU7 (Zakura
    // `TransactionTemplate::new_coinbase`, `zakura-rpc/src/methods/types/
    // transaction.rs:401,517`).
    let fees = i64::try_from(template.coinbase.miner_fees).unwrap_or(i64::MAX);
    let coinbasetxn = TransactionTemplate {
        data: hex::encode(&template.coinbase.bytes),
        hash: display_hex(template.coinbase.txid.as_ref()),
        authdigest: display_hex(&template.coinbase.auth_digest),
        depends: Vec::new(),
        fee: -fees,
        sigops: template.coinbase.sigops,
        required: true,
    };
    let commitments = display_hex(&template.block_commitments);
    // zcashd and Zakura: `mintime` is the lowest time of the header rules, and `curtime`
    // is a time that these rules accept. A clock above `maxtime` (a chain whose newest
    // blocks are old) must not give a `curtime` that makes the block invalid.
    let maxtime = max_time(network, template.tip.height, template.tip.median_time_past);
    let mintime = template.tip.median_time_past.saturating_add(1);
    let curtime = now.max(template.tip.time).min(maxtime);
    let id = template.id.to_string();
    Ok(BlockTemplate {
        capabilities: Vec::new(),
        version: BLOCK_VERSION,
        previousblockhash: template.tip.parent_hash.to_string(),
        blockcommitmentshash: commitments.clone(),
        lightclientroothash: commitments.clone(),
        finalsaplingroothash: commitments.clone(),
        defaultroots: DefaultRoots {
            merkleroot: display_hex(&template.merkle_root),
            chainhistoryroot: display_hex(&template.tip.history_root),
            authdataroot: display_hex(&template.auth_data_root),
            blockcommitmentshash: commitments,
        },
        transactions,
        coinbasetxn,
        longpollid: id.clone(),
        target: target_hex(template.tip.bits).ok_or(TemplateError::Bits(template.tip.bits))?,
        mintime,
        mutable: MUTABLE.to_vec(),
        noncerange: NONCE_RANGE,
        sigoplimit: rules.limits.sigops,
        sizelimit: MAX_BLOCK_BYTES,
        curtime,
        bits: format!("{:08x}", template.tip.bits),
        height: template.tip.height,
        maxtime,
        workid: id,
        submitold,
    })
}

fn transaction_template(
    c: &Arc<Candidate>,
    index: &HashMap<hayai_wire::WtxId, usize>,
) -> TransactionTemplate {
    TransactionTemplate {
        data: hex::encode(&c.bytes),
        hash: display_hex(c.wtxid.txid.as_ref()),
        authdigest: display_hex(&c.wtxid.auth_digest),
        depends: c
            .depends_on
            .iter()
            .filter_map(|p| index.get(p).copied())
            .collect(),
        fee: i64::try_from(c.fee).unwrap_or(i64::MAX),
        sigops: c.sigops,
        required: false,
    }
}

/// The 256-bit target of a compact `bits` value as 64 big-endian hex digits. Returns `None`
/// when the encoding denotes no target (negative, zero mantissa, overflow), as
/// `arith_uint256::SetCompact`.
pub fn target_hex(bits: u32) -> Option<String> {
    let exponent = (bits >> 24) as usize;
    let mantissa = bits & 0x007f_ffff;
    if bits & 0x0080_0000 != 0 || mantissa == 0 {
        return None;
    }
    if exponent > 34 || (mantissa > 0xff && exponent > 33) || (mantissa > 0xffff && exponent > 32) {
        return None;
    }
    // Little-endian target. The output prints the most significant byte first.
    let mut le = [0u8; 32];
    if exponent <= 3 {
        let shifted = mantissa >> (8 * (3 - exponent));
        le[..4].copy_from_slice(&shifted.to_le_bytes());
    } else {
        let offset = exponent - 3;
        for (i, b) in mantissa.to_le_bytes()[..3].iter().enumerate() {
            if let Some(slot) = le.get_mut(offset + i) {
                *slot = *b;
            }
        }
    }
    le.reverse();
    Some(hex::encode(le))
}

/// The highest time that the header rules allow for the block at `height` whose
/// median-time-past is `median_time_past`: the median-time-past plus 90 min (protocol
/// specification §7.6, the rule of `hayai_consensus::header::check_contextual`). Below the
/// start height of the rule on `network`, no consensus rule limits the time.
pub fn max_time(network: Network, height: u32, median_time_past: u32) -> u32 {
    if height < network.params().max_time_start_height {
        return u32::MAX;
    }
    median_time_past.saturating_add(MAX_FUTURE_BLOCK_TIME_MTP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_matches_known_values() {
        assert_eq!(
            target_hex(0x1f07_ffff).unwrap(),
            "0007ffff00000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            target_hex(0x1d00_ffff).unwrap(),
            "00000000ffff0000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            target_hex(0x0300_1234).unwrap(),
            format!("{}001234", "00".repeat(29))
        );
        assert_eq!(target_hex(0x1f80_0001), None);
        assert_eq!(target_hex(0x1f00_0000), None);
        assert_eq!(target_hex(0xff00_0001), None);
    }

    /// `maxtime` is the limit of the header rule: a header at the limit passes
    /// `check_contextual`, and a header 1 s later fails with `TimeTooLate`.
    #[test]
    fn max_time_is_the_limit_of_the_header_rule() {
        use hayai_consensus::header::{check_contextual, HeaderRuleError};
        use hayai_consensus::ParentChain;
        use hayai_wire::header::BlockHeader;

        // Newest first. The median of the 11 times is 1,000,050.
        let times: Vec<u32> = (0..11).map(|i| 1_000_100 - 10 * i).collect();
        let median_time_past = hayai_consensus::difficulty::median_time_past(&times).unwrap();
        assert_eq!(median_time_past, 1_000_050);
        let net = Network::Regtest.params();
        let height = 20;
        let limit = max_time(Network::Regtest, height, median_time_past);
        assert_eq!(limit, median_time_past + 90 * 60);
        let verdict = |time: u32| {
            let header = BlockHeader {
                version: 4,
                prev_hash: BlockHash([0; 32]),
                merkle_root: [0; 32],
                block_commitments: [0; 32],
                time,
                bits: net.pow_limit_bits,
                nonce: [0; 32],
                solution: Vec::new(),
            };
            let chain = ParentChain {
                height,
                times: &times,
                bits: &[],
            };
            check_contextual(Network::Regtest, &header, &chain)
        };
        let Ok(_) = verdict(limit) else {
            panic!("a header at the limit passes the time rules");
        };
        assert!(matches!(
            verdict(limit + 1),
            Err(HeaderRuleError::TimeTooLate { time, limit: l }) if time == limit + 1 && l == limit
        ));

        // The rule starts at height 2 on Mainnet and Regtest, and later on Testnet.
        assert_eq!(max_time(Network::Regtest, 1, 500), u32::MAX);
        assert_eq!(max_time(Network::Mainnet, 2, 500), 500 + 5_400);
        let start = Network::Testnet.params().max_time_start_height;
        assert!(start > 2);
        assert_eq!(max_time(Network::Testnet, start - 1, 500), u32::MAX);
        assert_eq!(max_time(Network::Testnet, start, 500), 500 + 5_400);
    }

    #[test]
    fn display_hex_is_reversed() {
        let mut b = [0u8; 32];
        b[0] = 0xab;
        assert_eq!(display_hex(&b), format!("{}ab", "00".repeat(31)));
    }
}
