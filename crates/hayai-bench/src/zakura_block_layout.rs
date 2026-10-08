//! A port of Zakura's finalized block layout on RocksDB, the baseline for
//! `benches/blockstore.rs`.
//!
//! Zakura (upstream Zebra's schema) stores a block as one header row,
//! `block_header_by_height[height] -> Header`, and one row per transaction,
//! `tx_by_loc[TransactionLocation] -> Transaction` (3-byte big-endian height plus 2-byte
//! big-endian index), with `hash_by_height` and `height_by_hash` for lookups. Serving a block
//! (`ZebraDb::block`) reads the header, iterates the transaction range, deserializes each
//! row into a `Transaction`, assembles a `Block`, and the network or RPC layer serializes it
//! again. Source: `zebra-state/src/service/finalized_state/zebra_db/block.rs` (`block`,
//! `transactions_by_height`) and `disk_format/block.rs`.

use std::path::Path;
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, Direction, IteratorMode, Options, WriteBatch, DB};
use zk_chain::block::{Block, Header};
use zk_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zk_chain::transaction::Transaction;

use crate::zakura_utxo_layout::TxLocation;

const CF_HEADER_BY_HEIGHT: &str = "block_header_by_height";
const CF_TX_BY_LOC: &str = "tx_by_loc";
const CF_HASH_BY_HEIGHT: &str = "hash_by_height";
const CF_HEIGHT_BY_HASH: &str = "height_by_hash";

pub struct ZakuraBlockDb {
    db: DB,
}

fn height_key(height: u32) -> [u8; 3] {
    assert!(height < 1 << 24, "height {height} does not fit in 3 bytes");
    let b = height.to_be_bytes();
    [b[1], b[2], b[3]]
}

impl ZakuraBlockDb {
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let cfs = [
            CF_HEADER_BY_HEIGHT,
            CF_TX_BY_LOC,
            CF_HASH_BY_HEIGHT,
            CF_HEIGHT_BY_HASH,
        ]
        .map(|name| ColumnFamilyDescriptor::new(name, Options::default()));
        let db = DB::open_cf_descriptors(&opts, path, cfs)?;
        Ok(Self { db })
    }

    fn cf(&self, name: &str) -> &rocksdb::ColumnFamily {
        self.db
            .cf_handle(name)
            .expect("column family created at open")
    }

    /// Writes a block the way Zakura's `prepare_block_header_and_transaction_data_batch`
    /// does: header row, hash rows, one row per transaction.
    pub fn write_block(&self, height: u32, block: &Block) -> Result<(), rocksdb::Error> {
        let mut batch = WriteBatch::default();
        let hash = block.hash();
        batch.put_cf(
            self.cf(CF_HEADER_BY_HEIGHT),
            height_key(height),
            block.header.zcash_serialize_to_vec().expect("vec write"),
        );
        batch.put_cf(self.cf(CF_HASH_BY_HEIGHT), height_key(height), hash.0);
        batch.put_cf(self.cf(CF_HEIGHT_BY_HASH), hash.0, height_key(height));
        for (index, tx) in block.transactions.iter().enumerate() {
            let loc = TxLocation {
                height,
                index: u16::try_from(index).expect("fewer than 65536 transactions"),
            };
            batch.put_cf(
                self.cf(CF_TX_BY_LOC),
                loc.to_bytes(),
                tx.zcash_serialize_to_vec().expect("vec write"),
            );
        }
        self.db.write(batch)
    }

    /// Rebuilds the block from its rows (`ZebraDb::block`) and serializes it for the wire.
    pub fn serve_block(&self, height: u32) -> Option<Vec<u8>> {
        let header_bytes = self
            .db
            .get_pinned_cf(self.cf(CF_HEADER_BY_HEIGHT), height_key(height))
            .expect("db read")?;
        let header = Header::zcash_deserialize(&header_bytes[..]).expect("header row");

        let start = TxLocation { height, index: 0 }.to_bytes();
        let mut transactions = Vec::new();
        for entry in self.db.iterator_cf(
            self.cf(CF_TX_BY_LOC),
            IteratorMode::From(&start, Direction::Forward),
        ) {
            let (key, value) = entry.expect("db read");
            if key[..3] != start[..3] {
                break;
            }
            transactions.push(Arc::new(
                Transaction::zcash_deserialize(&value[..]).expect("tx row"),
            ));
        }
        let block = Block {
            header: Arc::new(header),
            transactions,
        };
        Some(block.zcash_serialize_to_vec().expect("vec write"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayai_fixtures as fixtures;

    #[test]
    fn served_block_equals_wire_bytes() {
        let dir = crate::scratch_dir();
        let db = ZakuraBlockDb::open(dir.path()).unwrap();
        let fixture = fixtures::mixed_block(3, 1, 2, 2);
        let block = Block::zcash_deserialize(&fixture.bytes[..]).unwrap();
        db.write_block(fixture.height, &block).unwrap();
        db.write_block(fixture.height + 1, &block).unwrap();
        assert_eq!(db.serve_block(fixture.height).unwrap(), fixture.bytes);
        assert_eq!(db.serve_block(fixture.height + 2), None);
    }
}
