//! A faithful port of Zakura's finalized UTXO layout on RocksDB, used as the baseline for
//! `benches/coins.rs`.
//!
//! Zakura (upstream Zebra's schema) does not key UTXOs by outpoint. A lookup is two point
//! reads: `tx_loc_by_hash[txid] -> TransactionLocation` (3-byte big-endian height plus
//! 2-byte big-endian transaction index) and then `utxo_by_out_loc[OutputLocation] -> Output`
//! where the 8-byte `OutputLocation` is the transaction location followed by a 3-byte
//! big-endian output index. The value is the consensus serialization of the output
//! (`zakura_chain::transparent::Output`). Source: `zakura-state/src/service/finalized_state/`
//! `disk_format/{block,transparent}.rs` and `zakura_db/transparent.rs` (`utxo`,
//! `utxo_by_location`, `prepare_new_transparent_outputs_batch`,
//! `prepare_spent_transparent_outputs_batch`).
//!
//! The node reads every spent coin three times per block through this path (transaction
//! verifier, contextual check, finalization) and has no in-memory coins cache, so the
//! baseline offers both the single round and the three-round lookup.

use std::path::Path;

use hayai_crypto::zcash_transparent::bundle::OutPoint;
use rocksdb::{
    BlockBasedOptions, ColumnFamilyDescriptor, DBCompressionType, Options, WriteBatch, DB,
};
use zk_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zk_chain::transparent::Output;

const CF_TX_LOC_BY_HASH: &str = "tx_loc_by_hash";
const CF_UTXO_BY_OUT_LOC: &str = "utxo_by_out_loc";

/// Height (3 bytes) and transaction index (2 bytes), both big-endian, as on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxLocation {
    pub height: u32,
    pub index: u16,
}

impl TxLocation {
    pub fn to_bytes(self) -> [u8; 5] {
        assert!(
            self.height < 1 << 24,
            "height {} exceeds 3 bytes",
            self.height
        );
        let height = self.height.to_be_bytes();
        let index = self.index.to_be_bytes();
        [height[1], height[2], height[3], index[0], index[1]]
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        assert_eq!(bytes.len(), 5, "transaction location is 5 bytes");
        TxLocation {
            height: u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]),
            index: u16::from_be_bytes([bytes[3], bytes[4]]),
        }
    }
}

/// Transaction location followed by a 3-byte big-endian output index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLocation {
    pub tx: TxLocation,
    pub output_index: u32,
}

impl OutputLocation {
    pub fn to_bytes(self) -> [u8; 8] {
        assert!(
            self.output_index < 1 << 24,
            "output index {} exceeds 3 bytes",
            self.output_index
        );
        let tx = self.tx.to_bytes();
        let index = self.output_index.to_be_bytes();
        [
            tx[0], tx[1], tx[2], tx[3], tx[4], index[1], index[2], index[3],
        ]
    }
}

/// One transaction of a block as the layout stores it: its hash and its outputs.
pub struct BlockTx {
    pub txid: [u8; 32],
    pub outputs: Vec<Output>,
}

/// The two UTXO column families of Zakura's finalized state.
pub struct ZakuraUtxoDb {
    db: DB,
}

impl ZakuraUtxoDb {
    /// Opens or creates the database with Zakura's options
    /// (`DiskDb::options` in `zakura-state/src/service/finalized_state/disk_db.rs`).
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        let mut table = BlockBasedOptions::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        // Zakura: Ribbon filter at 9.9 bits for every column family, LZ4 everywhere, the
        // level-style compaction preset with a 128 MiB memtable budget, a 4 GiB WAL bound
        // and half of a 1024 file limit minus a reserve for the database. No block cache is
        // configured, so RocksDB's default 32 MiB LRU cache applies.
        table.set_ribbon_filter(9.9);
        opts.set_compression_type(DBCompressionType::Lz4);
        opts.optimize_level_style_compaction(128 * 1024 * 1024);
        opts.set_max_total_wal_size(4 * 1024 * 1024 * 1024);
        opts.set_max_open_files((1024 - 48) / 2);
        opts.set_block_based_table_factory(&table);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_TX_LOC_BY_HASH, opts.clone()),
            ColumnFamilyDescriptor::new(CF_UTXO_BY_OUT_LOC, opts.clone()),
        ];
        let db = DB::open_cf_descriptors(&opts, path, cfs)?;
        Ok(ZakuraUtxoDb { db })
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        let Some(cf) = self.db.cf_handle(name) else {
            panic!("column family {name} missing");
        };
        cf
    }

    /// One block's finalization write: a transaction location per transaction, an output
    /// record per output, and a delete per spent output, in one batch.
    pub fn write_block(
        &self,
        height: u32,
        txs: &[BlockTx],
        spends: &[OutputLocation],
    ) -> Result<(), rocksdb::Error> {
        let tx_loc_by_hash = self.cf(CF_TX_LOC_BY_HASH);
        let utxo_by_out_loc = self.cf(CF_UTXO_BY_OUT_LOC);
        let mut batch = WriteBatch::default();
        for (index, tx) in txs.iter().enumerate() {
            let index = u16::try_from(index).expect("block has fewer than 65536 transactions");
            let tx_loc = TxLocation { height, index };
            batch.put_cf(tx_loc_by_hash, tx.txid, tx_loc.to_bytes());
            for (output_index, output) in tx.outputs.iter().enumerate() {
                let location = OutputLocation {
                    tx: tx_loc,
                    output_index: u32::try_from(output_index).expect("output index fits u32"),
                };
                let bytes = output
                    .zcash_serialize_to_vec()
                    .expect("serializing into a Vec cannot fail");
                batch.put_cf(utxo_by_out_loc, location.to_bytes(), bytes);
            }
        }
        for location in spends {
            batch.delete_cf(utxo_by_out_loc, location.to_bytes());
        }
        self.db.write(batch)
    }

    /// `ZakuraDb::utxo`: `output_location` (first get) then `utxo_by_location` (second get),
    /// each through `get_pinned_cf` as `DiskDb::zs_get` does.
    pub fn utxo(&self, outpoint: &OutPoint) -> Option<(OutputLocation, Output)> {
        let tx_loc = self
            .db
            .get_pinned_cf(self.cf(CF_TX_LOC_BY_HASH), outpoint.hash())
            .expect("unexpected database failure")?;
        let location = OutputLocation {
            tx: TxLocation::from_bytes(&tx_loc),
            output_index: outpoint.n(),
        };
        let bytes = self
            .db
            .get_pinned_cf(self.cf(CF_UTXO_BY_OUT_LOC), location.to_bytes())
            .expect("unexpected database failure")?;
        let output = Output::zcash_deserialize(&bytes[..]).expect("stored output parses");
        Some((location, output))
    }

    /// The inputs of one block looked up once each: the "layout only" comparison.
    pub fn lookup_block_inputs_1round(
        &self,
        outpoints: &[OutPoint],
    ) -> Vec<Option<(OutputLocation, Output)>> {
        outpoints
            .iter()
            .map(|outpoint| self.utxo(outpoint))
            .collect()
    }

    /// The inputs of one block as the node reads them: three serial rounds (transaction
    /// verifier, contextual check, finalization), each of two gets per input. Returns the
    /// last round's result.
    pub fn lookup_block_inputs_zakura_style(
        &self,
        outpoints: &[OutPoint],
    ) -> Vec<Option<(OutputLocation, Output)>> {
        let verifier = self.lookup_block_inputs_1round(outpoints);
        let contextual = self.lookup_block_inputs_1round(outpoints);
        let finalize = self.lookup_block_inputs_1round(outpoints);
        assert_eq!(verifier.len(), finalize.len());
        assert_eq!(contextual.len(), finalize.len());
        finalize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zk_chain::transparent::Script;

    fn output(value: u64, tag: u8) -> Output {
        Output {
            value: value.try_into().expect("non-negative"),
            lock_script: Script::new(&[0x76, 0xa9, 0x14, tag, 0x88, 0xac]),
        }
    }

    #[test]
    fn locations_encode_big_endian_truncated() {
        let tx = TxLocation {
            height: 0x0a0b0c,
            index: 0x0102,
        };
        assert_eq!(tx.to_bytes(), [0x0a, 0x0b, 0x0c, 0x01, 0x02]);
        assert_eq!(TxLocation::from_bytes(&tx.to_bytes()), tx);
        let out = OutputLocation {
            tx,
            output_index: 0x030405,
        };
        assert_eq!(
            out.to_bytes(),
            [0x0a, 0x0b, 0x0c, 0x01, 0x02, 0x03, 0x04, 0x05]
        );
    }

    #[test]
    fn write_then_two_get_lookup_and_spend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = ZakuraUtxoDb::open(dir.path()).expect("open");
        let txs = vec![
            BlockTx {
                txid: [1; 32],
                outputs: vec![output(10, 1), output(20, 2)],
            },
            BlockTx {
                txid: [2; 32],
                outputs: vec![output(30, 3)],
            },
        ];
        db.write_block(7, &txs, &[]).expect("write");

        let (loc, out) = db.utxo(&OutPoint::new([1; 32], 1)).expect("present");
        assert_eq!(out, output(20, 2));
        assert_eq!(
            loc,
            OutputLocation {
                tx: TxLocation {
                    height: 7,
                    index: 0
                },
                output_index: 1
            }
        );
        let outpoints = [
            OutPoint::new([2; 32], 0),
            OutPoint::new([2; 32], 1),
            OutPoint::new([9; 32], 0),
        ];
        let found = db.lookup_block_inputs_zakura_style(&outpoints);
        assert_eq!(
            found[0].as_ref().map(|(_, o)| o.clone()),
            Some(output(30, 3))
        );
        let None = found[1] else {
            panic!("output index past the end must be absent");
        };
        let None = found[2] else {
            panic!("unknown txid must be absent");
        };

        db.write_block(8, &[], &[loc]).expect("spend");
        let None = db.utxo(&OutPoint::new([1; 32], 1)) else {
            panic!("spent output must be deleted");
        };
        let Some(_) = db.utxo(&OutPoint::new([1; 32], 0)) else {
            panic!("sibling output must remain");
        };
    }
}
