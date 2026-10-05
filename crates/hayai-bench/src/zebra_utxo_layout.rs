//! Upstream Zebra's finalized UTXO reads on RocksDB, the Zebra baseline for
//! `benches/coins.rs`.
//!
//! Zebra and Zakura store UTXOs in the same layout: `tx_loc_by_hash[txid] ->
//! TransactionLocation` and `utxo_by_out_loc[OutputLocation] -> Output`, with the same key
//! and value encodings (`zebra-state` 14.0.0,
//! `src/service/finalized_state/disk_format/{block,transparent}.rs`; `zebra_db/transparent.rs`
//! lines 145–186). So this module reads the database that
//! [`crate::zakura_utxo_layout::ZakuraUtxoDb`] writes, with Zebra's options and Zebra's
//! number of reads.
//!
//! Zebra reads each spent coin of a semantically verified block three times, in sequence:
//!
//! 1. The transaction verifier: `block_spent_utxos` awaits one `AwaitUtxo` per input, one
//!    input after the other (`zebra-consensus` 16.0.0, `src/transaction.rs` lines 388–433).
//!    `AwaitUtxo` ends in `ZebraDb::utxo` (`zebra-state` `src/service/read/block.rs` line
//!    350): two gets.
//! 2. The contextual check: `check::utxo::transparent_spend` falls back to
//!    `finalized_state.utxo` (`src/service/check/utxo.rs` line 161): two gets.
//! 3. Finalization: `write_block` calls `output_location` and then `utxo` for each input
//!    (`src/service/finalized_state/zebra_db/block.rs` lines 488 and 491): three gets.
//!
//! That is seven gets per input. Zakura has an added `CheckParentInputs` round before the
//! verifier, and it reads the coin once (two gets) at finalization
//! (`zakura-state/.../zakura_db/block.rs` lines 1176–1197).

use std::path::Path;

use hayai_crypto::zcash_transparent::bundle::OutPoint;
use rocksdb::{BlockBasedOptions, ColumnFamilyDescriptor, DBCompressionType, Options, DB};
use zb_chain::serialization::ZcashDeserialize;
use zb_chain::transparent::Output;

use crate::zakura_utxo_layout::{OutputLocation, TxLocation};

const CF_TX_LOC_BY_HASH: &str = "tx_loc_by_hash";
const CF_UTXO_BY_OUT_LOC: &str = "utxo_by_out_loc";

/// Zebra's `DiskDb::options` (`zebra-state` 14.0.0, `src/service/finalized_state/disk_db.rs`
/// lines 1297–1339): a Ribbon filter at 9.9 bits, LZ4, the level-style compaction preset with
/// a 128 MiB memtable budget, half of a 1024 file limit minus 48, and RocksDB's default
/// block cache. Each column family takes a clone of these options. Zakura's options are the
/// same plus a 4 GiB bound on the write-ahead log (`set_max_total_wal_size`).
pub fn zebra_options() -> Options {
    let mut opts = Options::default();
    let mut table = BlockBasedOptions::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    table.set_ribbon_filter(9.9);
    opts.set_compression_type(DBCompressionType::Lz4);
    opts.optimize_level_style_compaction(128 * 1024 * 1024);
    opts.set_max_open_files((1024 - 48) / 2);
    opts.set_block_based_table_factory(&table);
    opts
}

/// The two UTXO column families, opened with Zebra's options.
pub struct ZebraUtxoDb {
    db: DB,
}

impl ZebraUtxoDb {
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let opts = zebra_options();
        let cfs = [CF_TX_LOC_BY_HASH, CF_UTXO_BY_OUT_LOC]
            .map(|name| ColumnFamilyDescriptor::new(name, opts.clone()));
        Ok(Self {
            db: DB::open_cf_descriptors(&opts, path, cfs)?,
        })
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        let Some(cf) = self.db.cf_handle(name) else {
            panic!("column family {name} missing");
        };
        cf
    }

    /// `ZebraDb::output_location`: one get on `tx_loc_by_hash`.
    fn output_location(&self, outpoint: &OutPoint) -> Option<OutputLocation> {
        let tx_loc = self
            .db
            .get_pinned_cf(self.cf(CF_TX_LOC_BY_HASH), outpoint.hash())
            .expect("unexpected database failure")?;
        Some(OutputLocation {
            tx: TxLocation::from_bytes(&tx_loc),
            output_index: outpoint.n(),
        })
    }

    /// `ZebraDb::utxo`: `output_location`, then one get on `utxo_by_out_loc`.
    pub fn utxo(&self, outpoint: &OutPoint) -> Option<(OutputLocation, Output)> {
        let location = self.output_location(outpoint)?;
        let bytes = self
            .db
            .get_pinned_cf(self.cf(CF_UTXO_BY_OUT_LOC), location.to_bytes())
            .expect("unexpected database failure")?;
        let output = Output::zcash_deserialize(&bytes[..]).expect("stored output parses");
        Some((location, output))
    }

    /// The reads of one block as Zebra does them: the verifier round, the contextual round,
    /// and the finalization round (`output_location`, then `utxo`). Returns the last round's
    /// result.
    pub fn lookup_block_inputs_zebra_style(
        &self,
        outpoints: &[OutPoint],
    ) -> Vec<Option<(OutputLocation, Output)>> {
        let verifier: Vec<_> = outpoints.iter().map(|o| self.utxo(o)).collect();
        let contextual: Vec<_> = outpoints.iter().map(|o| self.utxo(o)).collect();
        let finalize: Vec<_> = outpoints
            .iter()
            .map(|o| {
                self.output_location(o)?;
                self.utxo(o)
            })
            .collect();
        assert_eq!(verifier.len(), finalize.len());
        assert_eq!(contextual.len(), finalize.len());
        finalize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zakura_utxo_layout::{BlockTx, ZakuraUtxoDb};
    use zb_chain::serialization::ZcashSerialize as _;
    use zk_chain::serialization::ZcashSerialize as _;

    /// A database that the Zakura layout writes reads the same through Zebra's path.
    #[test]
    fn zebra_reads_the_zakura_layout() {
        let dir = crate::scratch_dir();
        let output = zk_chain::transparent::Output {
            value: 7u64.try_into().expect("non-negative"),
            lock_script: zk_chain::transparent::Script::new(&[0x51]),
        };
        let expected = output.zcash_serialize_to_vec().expect("vec write");
        {
            let zakura = ZakuraUtxoDb::open(dir.path()).expect("open");
            let txs = [BlockTx {
                txid: [4; 32],
                outputs: vec![output],
            }];
            zakura.write_block(9, &txs, &[]).expect("write");
        }
        let zebra = ZebraUtxoDb::open(dir.path()).expect("open");
        let found = zebra.lookup_block_inputs_zebra_style(&[
            OutPoint::new([4; 32], 0),
            OutPoint::new([5; 32], 0),
        ]);
        let Some((_, read)) = &found[0] else {
            panic!("the written output must be present");
        };
        assert_eq!(read.zcash_serialize_to_vec().expect("vec write"), expected);
        let None = found[1] else {
            panic!("unknown txid must be absent");
        };
    }
}
