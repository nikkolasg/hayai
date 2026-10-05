//! Rebuild of a block from a `Submit` message and the template that it names.
//!
//! The node keeps every template that it served for the retention window of
//! [`TemplateStore`]. A late solution against a previous parent is therefore still a block if
//! the chain has not moved. The function returns the rebuilt block as wire bytes plus its
//! header. The caller does the proof-of-work check, the Equihash check and the contextual
//! checks.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_crypto::zcash_primitives;
use hayai_wire::header::BlockHeader;
use hayai_wire::RawTx;
use zcash_primitives::transaction::TxId;

use crate::live::{block_commitments, roots, StoredTemplate};
use crate::messages::Submit;

/// Templates served recently, by id.
pub struct TemplateStore {
    by_id: BTreeMap<u64, Arc<StoredTemplate>>,
    retention: Duration,
}

impl TemplateStore {
    pub fn new(retention: Duration) -> Self {
        Self {
            by_id: BTreeMap::new(),
            retention,
        }
    }

    pub fn insert(&mut self, template: Arc<StoredTemplate>) {
        self.by_id.insert(template.id, template);
    }

    pub fn get(&self, id: u64) -> Option<&Arc<StoredTemplate>> {
        self.by_id.get(&id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Drops templates older than the retention window. The newest one always stays.
    pub fn prune(&mut self, now: Instant) {
        let Some(newest) = self.by_id.keys().next_back().copied() else {
            return;
        };
        let retention = self.retention;
        self.by_id
            .retain(|id, t| *id == newest || now.duration_since(t.created) <= retention);
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SubmitError {
    #[error("unknown template id {0}")]
    UnknownTemplate(u64),
    #[error("coinbase override does not parse: {0}")]
    CoinbaseParse(#[from] hayai_wire::ParseError),
    #[error("coinbase override is not a coinbase transaction")]
    NotCoinbase,
    #[error("coinbase override of {len} bytes exceeds the reserved {reserved} bytes")]
    CoinbaseTooLarge { len: usize, reserved: usize },
    #[error("solution of {got} bytes, the network's Equihash parameters need {expected}")]
    SolutionLength { expected: usize, got: usize },
}

/// A block that the node assembled from a template and a submission.
#[derive(Clone, Debug)]
pub struct RebuiltBlock {
    pub template: Arc<StoredTemplate>,
    pub header: BlockHeader,
    pub bytes: Bytes,
    pub coinbase_txid: TxId,
}

/// Rebuilds the block for `submit` from the stored template. With a coinbase override, the
/// function recomputes the roots and the block commitments for the new coinbase. It parses
/// the override with the consensus branch id of the template height. The transaction set is
/// always the set of the template.
pub fn rebuild_block(store: &TemplateStore, submit: &Submit) -> Result<RebuiltBlock, SubmitError> {
    let template = store
        .get(submit.template_id)
        .ok_or(SubmitError::UnknownTemplate(submit.template_id))?
        .clone();
    let expected = template.pow.solution_len();
    if submit.solution.0.len() != expected {
        return Err(SubmitError::SolutionLength {
            expected,
            got: submit.solution.0.len(),
        });
    }
    let (coinbase_bytes, coinbase_txid, merkle_root, block_commitments) = match &submit.coinbase {
        None => (
            template.coinbase.bytes.clone(),
            template.coinbase.txid,
            template.merkle_root,
            template.block_commitments,
        ),
        Some(bytes) => {
            let reserved = template.coinbase.bytes.len() + template.coinbase.script_slack;
            if bytes.0.len() > reserved {
                return Err(SubmitError::CoinbaseTooLarge {
                    len: bytes.0.len(),
                    reserved,
                });
            }
            let raw = RawTx::parse(bytes.0.clone(), template.coinbase.branch_id)?;
            let is_coinbase = match raw.tx.transparent_bundle() {
                Some(bundle) => bundle.is_coinbase(),
                None => false,
            };
            if !is_coinbase {
                return Err(SubmitError::NotCoinbase);
            }
            let mut coinbase = template.coinbase.clone();
            coinbase.bytes = raw.bytes.clone();
            coinbase.txid = raw.txid;
            coinbase.auth_digest = raw.auth_digest;
            let (merkle_root, auth_data_root) = roots(&coinbase, &template.txs);
            (
                raw.bytes,
                raw.txid,
                merkle_root,
                block_commitments(&template.tip.history_root, &auth_data_root),
            )
        }
    };
    let mut header = template.header_template(submit.time);
    header.merkle_root = merkle_root;
    header.block_commitments = block_commitments;
    header.nonce = submit.nonce.0;
    header.solution = submit.solution.0.to_vec();
    let body_len: usize = template.txs.iter().map(|c| c.bytes.len()).sum();
    let mut bytes = header.serialize();
    bytes.reserve(5 + coinbase_bytes.len() + body_len);
    write_compact_size(template.txs.len() + 1, &mut bytes);
    bytes.extend_from_slice(&coinbase_bytes);
    for tx in &template.txs {
        bytes.extend_from_slice(&tx.bytes);
    }
    Ok(RebuiltBlock {
        template,
        header,
        bytes: Bytes::from(bytes),
        coinbase_txid,
    })
}

fn write_compact_size(n: usize, out: &mut Vec<u8>) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&(n as u64).to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::SetEvent;
    use crate::live::{LiveTemplate, TemplateConfig};
    use crate::messages::{Hash32, HexBytes};
    use crate::test_support::{candidate, coinbase_spec, tip};
    use hayai_wire::header::PowParams;
    use hayai_wire::RawBlock;

    fn submit(template_id: u64, coinbase: Option<Bytes>) -> Submit {
        Submit {
            template_id,
            time: 1_700_000_777,
            nonce: Hash32([0xab; 32]),
            solution: HexBytes(Bytes::from(vec![0u8; 1344])),
            coinbase: coinbase.map(HexBytes),
        }
    }

    #[test]
    fn rebuilt_block_has_header_count_coinbase_and_template_txs() {
        let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
        live.on_tip(tip(3), &[], &[], |_| {}).unwrap();
        let a = candidate(1, 10_000, 1, &[]);
        let b = candidate(2, 30_000, 1, &[]);
        live.apply(SetEvent::Added(a.clone())).unwrap();
        live.apply(SetEvent::Added(b.clone())).unwrap();
        let template = live.current().unwrap().clone();
        let block = rebuild_block(live.store(), &submit(template.id, None)).unwrap();
        assert_eq!(block.header.time, 1_700_000_777);
        assert_eq!(block.header.nonce, [0xab; 32]);
        assert_eq!(block.header.merkle_root, template.merkle_root);
        assert_eq!(block.header.block_commitments, template.block_commitments);
        let bytes = &block.bytes;
        assert_eq!(
            bytes.len(),
            PowParams::MAINNET.header_len()
                + 1
                + template.coinbase.bytes.len()
                + a.bytes.len()
                + b.bytes.len()
        );
        let header = block.header.serialize();
        assert_eq!(&bytes[..header.len()], &header[..]);
        assert_eq!(bytes[header.len()], 3);
        let body = &bytes[header.len() + 1..];
        assert_eq!(
            &body[..template.coinbase.bytes.len()],
            &template.coinbase.bytes[..]
        );
        // The template order is canonical: here, by txid.
        let rest = &body[template.coinbase.bytes.len()..];
        let (first, second) = match a.wtxid.txid.as_ref() < b.wtxid.txid.as_ref() {
            true => (&a, &b),
            false => (&b, &a),
        };
        assert_eq!(&rest[..first.bytes.len()], &first.bytes[..]);
        assert_eq!(&rest[first.bytes.len()..], &second.bytes[..]);
        assert!(matches!(
            rebuild_block(live.store(), &submit(999, None)),
            Err(SubmitError::UnknownTemplate(999))
        ));
        let mut short = submit(template.id, None);
        short.solution = HexBytes(Bytes::from(vec![0u8; 10]));
        assert!(matches!(
            rebuild_block(live.store(), &short),
            Err(SubmitError::SolutionLength {
                expected: 1344,
                got: 10
            })
        ));
    }

    /// On Regtest a submission carries the 36-byte (48, 5) solution and the block starts
    /// with a 177-byte header; a 1344-byte solution is the wrong length there.
    #[test]
    fn regtest_template_takes_the_regtest_solution_length() {
        let mut config = TemplateConfig::new(coinbase_spec());
        config.pow = PowParams::REGTEST;
        let mut live = LiveTemplate::new(config);
        live.on_tip(tip(3), &[], &[], |_| {}).unwrap();
        let a = candidate(1, 10_000, 1, &[]);
        live.apply(SetEvent::Added(a.clone())).unwrap();
        let template = live.current().unwrap().clone();
        assert_eq!(template.pow, PowParams::REGTEST);
        let mut regtest = submit(template.id, None);
        regtest.solution = HexBytes(Bytes::from(vec![0u8; 36]));
        let block = rebuild_block(live.store(), &regtest).unwrap();
        assert_eq!(block.header.serialized_len(), 177);
        assert_eq!(
            block.bytes.len(),
            177 + 1 + template.coinbase.bytes.len() + a.bytes.len()
        );
        let parsed = RawBlock::parse(block.bytes.clone(), template.coinbase.branch_id).unwrap();
        assert_eq!(parsed.header, block.header);
        assert_eq!(parsed.txs.len(), 2);
        assert!(matches!(
            rebuild_block(live.store(), &submit(template.id, None)),
            Err(SubmitError::SolutionLength {
                expected: 36,
                got: 1344
            })
        ));
    }

    #[test]
    fn coinbase_override_recomputes_the_roots() {
        let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
        live.on_tip(tip(3), &[], &[], |_| {}).unwrap();
        live.apply(SetEvent::Added(candidate(1, 10_000, 1, &[])))
            .unwrap();
        let template = live.current().unwrap().clone();
        let mut spec = coinbase_spec();
        spec.miner_data = b"pool-tag-with-extra-nonce".to_vec();
        let override_cb = spec.build(3, template.fees_total).unwrap();
        let block = rebuild_block(
            live.store(),
            &submit(template.id, Some(override_cb.bytes.clone())),
        )
        .unwrap();
        assert_eq!(block.coinbase_txid, override_cb.txid);
        // A v5 txid does not commit to the scriptSig, so the merkle root is unchanged. The
        // auth data root moves, and the block commitments move with it.
        assert_eq!(block.header.merkle_root, template.merkle_root);
        assert_ne!(block.header.block_commitments, template.block_commitments);
        let mut expected = template.coinbase.clone();
        expected.bytes = override_cb.bytes.clone();
        expected.txid = override_cb.txid;
        expected.auth_digest = override_cb.auth_digest;
        let (merkle_root, auth_root) = roots(&expected, &template.txs);
        assert_eq!(block.header.merkle_root, merkle_root);
        assert_eq!(
            block.header.block_commitments,
            block_commitments(&template.tip.history_root, &auth_root)
        );

        let not_coinbase = candidate(5, 1, 1, &[]).bytes;
        assert!(matches!(
            rebuild_block(live.store(), &submit(template.id, Some(not_coinbase)),),
            Err(SubmitError::NotCoinbase)
        ));
        // A 200-byte miner script makes the override larger than the reserved size, although
        // its scriptSig is within the limit.
        spec.script_pubkey = vec![0u8; 200];
        let too_large = spec.build(3, 0).unwrap();
        assert!(matches!(
            rebuild_block(live.store(), &submit(template.id, Some(too_large.bytes)),),
            Err(SubmitError::CoinbaseTooLarge { .. })
        ));
    }

    #[test]
    fn store_keeps_the_newest_and_prunes_the_rest_after_retention() {
        let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
        live.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        live.on_tip(tip(2), &[], &[], |_| {}).unwrap();
        assert_eq!(live.store().len(), 4);
        let mut store = TemplateStore::new(Duration::from_secs(60));
        for id in 1..=4 {
            store.insert(live.store().get(id).unwrap().clone());
        }
        store.prune(Instant::now() + Duration::from_secs(61));
        assert_eq!(store.len(), 1);
        let Some(newest) = store.get(4) else {
            panic!("the newest template must survive pruning");
        };
        assert_eq!(newest.tip.height, 2);
    }
}
