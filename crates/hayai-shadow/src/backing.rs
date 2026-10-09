//! Coins backing of a shadow node: hayai's own store, with the coins created before the
//! start height read from the followed node.
//!
//! A shadow node starts at a recent height without the history before it. Reads go to the
//! inner store first. A miss on an outpoint that hayai did not spend itself goes upstream:
//! `getrawtransaction <txid> 1` gives the creating transaction and its height. A coin from
//! a transaction at or below the start height is returned (and counted in
//! `hayai_shadow_trusted_coins_total`); its unspentness before the start height is trusted
//! from upstream. A transaction above the start height is hayai's own history, so a miss
//! there is a real miss. `CoinsCache` keeps every returned coin, so each coin costs one
//! request.
//!
//! `gettxout` is in Zakura's restricted method set but answers for the upstream tip, which
//! runs ahead of hayai: a coin that the block under validation spends already reads as
//! spent. It also gives the value as a float and no height. Hence `getrawtransaction`.
//!
//! Nullifiers revealed before the start height are unknown to hayai. Every nullifier that
//! the inner store does not hold is counted in `hayai_shadow_trusted_nullifiers_total`: its
//! uniqueness against the history before the start rests on upstream.
//!
//! An upstream failure is retried three times, then it panics: the coins cache treats a
//! failed backing as fatal, because an absent coin would fail a valid block silently.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use bytes::Bytes;
use hayai_coins::{Coin, CoinsBacking, Error, FlushGeneration, OutPoint, Pool};
use hayai_crypto::zcash_primitives::transaction::TxId;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_wire::RawTx;
use parking_lot::Mutex;
use rayon::prelude::*;

use crate::upstream::{Upstream, UpstreamError};
use hayai_metrics::Counter;
use hayai_state::persist::{PersistError, RecordLog};

const ATTEMPTS: u32 = 3;

/// Outpoints that hayai spent and deleted from the inner store, and the file that keeps them
/// across a restart. A record is the height of the generation that deleted them followed by
/// the 36-byte outpoint keys. The log is written before the generation, so a restart drops
/// the records above the best block of the inner store: the blocks above it are replayed
/// and spend those outpoints again.
pub struct SpentLog {
    log: RecordLog,
    spent: HashSet<OutPoint>,
}

impl SpentLog {
    /// Opens `path`, creating it when it does not exist, and keeps the records of the
    /// generations up to `best_height`. `None`: no generation reached the inner store.
    pub fn open(path: &Path, best_height: Option<u32>) -> Result<Self, PersistError> {
        let (mut log, records) = RecordLog::open(path)?;
        let mut kept = 0;
        let mut spent = HashSet::new();
        for record in &records {
            let Some((height, keys)) = record.split_first_chunk::<4>() else {
                return Err(PersistError::Corrupt(format!(
                    "{}: spent record without height",
                    path.display()
                )));
            };
            if best_height.is_none_or(|best| u32::from_le_bytes(*height) > best) {
                break;
            }
            if keys.len() % 36 != 0 {
                return Err(PersistError::Corrupt(format!(
                    "{}: spent record of {} bytes",
                    path.display(),
                    keys.len()
                )));
            }
            for key in keys.as_chunks::<36>().0 {
                let (hash, n) = key.split_first_chunk::<32>().expect("36 bytes");
                spent.insert(OutPoint::new(
                    *hash,
                    u32::from_le_bytes(n.try_into().expect("4")),
                ));
            }
            kept += 1;
        }
        log.truncate_to(kept)?;
        Ok(Self { log, spent })
    }
}

pub struct UpstreamBacking {
    inner: Arc<dyn CoinsBacking>,
    upstream: Arc<Upstream>,
    start_height: u32,
    branch: BranchId,
    /// Outpoints that hayai spent and deleted from the inner store. Their misses are real.
    spent: Mutex<HashSet<OutPoint>>,
    /// The persistent copy of `spent`, written by `write_generation`. `write_batch` is not
    /// the path of the node (the chain flushes generations) and records in memory only.
    spent_log: Mutex<RecordLog>,
    /// Coins and nullifiers that upstream answered for: the node trusts them.
    trusted_coins: Arc<Counter>,
    trusted_nullifiers: Arc<Counter>,
}

impl UpstreamBacking {
    pub fn new(
        inner: Arc<dyn CoinsBacking>,
        upstream: Arc<Upstream>,
        start_height: u32,
        branch: BranchId,
        trusted_coins: Arc<Counter>,
        trusted_nullifiers: Arc<Counter>,
        spent: SpentLog,
    ) -> Self {
        Self {
            inner,
            upstream,
            start_height,
            branch,
            spent: Mutex::new(spent.spent),
            spent_log: Mutex::new(spent.log),
            trusted_coins,
            trusted_nullifiers,
        }
    }

    fn fetch(&self, txid: &TxId) -> Option<(Bytes, Option<u32>)> {
        let mut last: Option<UpstreamError> = None;
        for attempt in 0..ATTEMPTS {
            match self.upstream.transaction(txid) {
                Ok(found) => return found,
                Err(e) => {
                    tracing::warn!(%txid, attempt, error = %e, "upstream transaction read failed");
                    last = Some(e);
                    thread::sleep(Duration::from_millis(200 << attempt));
                }
            }
        }
        panic!(
            "upstream coins unavailable for {txid}: {}",
            last.map_or_else(String::new, |e| e.to_string())
        );
    }

    /// The outputs of `txid` as coins, when upstream holds it at or below the start height.
    fn upstream_outputs(&self, txid: &TxId) -> Option<Vec<Coin>> {
        let (bytes, height) = self.fetch(txid)?;
        let height = height?;
        if height > self.start_height {
            return None;
        }
        let raw = match RawTx::parse(bytes, self.branch) {
            Ok(raw) => raw,
            Err(e) => panic!("upstream transaction {txid} does not parse: {e}"),
        };
        if raw.txid != *txid {
            panic!("upstream answered transaction {} for {txid}", raw.txid);
        }
        let bundle = raw.tx.transparent_bundle()?;
        let is_coinbase = bundle.is_coinbase();
        Some(
            bundle
                .vout
                .iter()
                .map(|out| Coin {
                    value: out.value().into_u64(),
                    script_pubkey: Bytes::copy_from_slice(&out.script_pubkey().0 .0),
                    height,
                    is_coinbase,
                })
                .collect(),
        )
    }

    fn record_spends<'a>(&self, spends: impl Iterator<Item = &'a OutPoint>) {
        let mut spent = self.spent.lock();
        for outpoint in spends {
            spent.insert(outpoint.clone());
        }
    }
}

impl CoinsBacking for UpstreamBacking {
    fn get_many(&self, outpoints: &[OutPoint]) -> Result<Vec<Option<Coin>>, Error> {
        let mut found = self.inner.get_many(outpoints)?;
        let mut by_txid: HashMap<TxId, Vec<usize>> = HashMap::new();
        {
            let spent = self.spent.lock();
            for (i, (outpoint, coin)) in outpoints.iter().zip(&found).enumerate() {
                if let (None, false) = (coin, spent.contains(outpoint)) {
                    by_txid
                        .entry(TxId::from_bytes(*outpoint.hash()))
                        .or_default()
                        .push(i);
                }
            }
        }
        let fetched: Vec<(Vec<usize>, Option<Vec<Coin>>)> = by_txid
            .into_par_iter()
            .map(|(txid, positions)| (positions, self.upstream_outputs(&txid)))
            .collect();
        for (positions, outputs) in fetched {
            let Some(outputs) = outputs else {
                continue;
            };
            for i in positions {
                if let Some(coin) = outputs.get(outpoints[i].n() as usize) {
                    found[i] = Some(coin.clone());
                    self.trusted_coins.inc();
                }
            }
        }
        Ok(found)
    }

    fn write_batch(&self, adds: &[(&OutPoint, &Coin)], spends: &[&OutPoint]) -> Result<(), Error> {
        self.record_spends(spends.iter().copied());
        self.inner.write_batch(adds, spends)
    }

    fn contains_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<Vec<bool>, Error> {
        let found = self.inner.contains_many(pool, nullifiers)?;
        let unknown = found.iter().filter(|present| !**present).count();
        self.trusted_nullifiers.add(unknown as u64);
        Ok(found)
    }

    fn insert_many(&self, pool: Pool, nullifiers: &[[u8; 32]]) -> Result<(), Error> {
        self.inner.insert_many(pool, nullifiers)
    }

    fn write_generation(&self, generation: &FlushGeneration) -> Result<(), Error> {
        if !generation.spends.is_empty() {
            let mut record = generation.best_block.height.to_le_bytes().to_vec();
            for outpoint in &generation.spends {
                record.extend_from_slice(&hayai_coins::outpoint_key(outpoint));
            }
            self.spent_log.lock().append(&record).map_err(|e| match e {
                PersistError::Io { path, source } => {
                    Error::Persist(hayai_coins::PersistError::Io { path, source })
                }
                other @ (PersistError::Corrupt(_) | PersistError::OutdatedRecord { .. }) => {
                    unreachable!("an append reports I/O errors only: {other}")
                }
            })?;
        }
        self.record_spends(generation.spends.iter());
        self.inner.write_generation(generation)
    }
}
