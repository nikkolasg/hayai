//! Upstream Zebra's finalized block layout on RocksDB, the Zebra baseline for
//! `benches/blockstore.rs`.
//!
//! The layout is the one of [`crate::zakura_block_layout`]: one `block_header_by_height`
//! row and one `tx_by_loc[TransactionLocation]` row per transaction, each the consensus
//! serialization (`zebra-state` 14.0.0, `src/service/finalized_state/zebra_db/block.rs`
//! lines 705–746, `disk_format/block.rs` lines 208–224 and 282–302). `ZebraDb::block`
//! (lines 156–177) reads the header, iterates the transaction range of the height (lines
//! 314–349) and deserializes each row into a `Transaction`. The network or RPC layer then
//! serializes the block again.
//!
//! Two things differ from the Zakura baseline. The database has Zebra's options
//! ([`crate::zebra_utxo_layout::zebra_options`]). The rows are parsed and serialized with
//! `zebra-chain` 13.0.1, whose `Transaction` wraps a `zcash_primitives` transaction, not
//! with Zakura's own structs.

use std::path::Path;
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, Direction, IteratorMode, WriteBatch, DB};
use zb_chain::block::{Block, Header};
use zb_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zb_chain::transaction::Transaction;

use crate::zakura_utxo_layout::TxLocation;
use crate::zebra_utxo_layout::zebra_options;

const CF_HEADER_BY_HEIGHT: &str = "block_header_by_height";
const CF_TX_BY_LOC: &str = "tx_by_loc";
const CF_HASH_BY_HEIGHT: &str = "hash_by_height";
const CF_HEIGHT_BY_HASH: &str = "height_by_hash";

pub struct ZebraBlockDb {
    db: DB,
}

fn height_key(height: u32) -> [u8; 3] {
    assert!(height < 1 << 24, "height {height} does not fit in 3 bytes");
    let b = height.to_be_bytes();
    [b[1], b[2], b[3]]
}

impl ZebraBlockDb {
    pub fn open(path: &Path) -> Result<Self, rocksdb::Error> {
        let opts = zebra_options();
        let cfs = [
            CF_HEADER_BY_HEIGHT,
            CF_TX_BY_LOC,
            CF_HASH_BY_HEIGHT,
            CF_HEIGHT_BY_HASH,
        ]
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

    /// `prepare_block_header_and_transaction_data_batch`: the header row, the hash rows and
    /// one row per transaction.
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

    /// `ZebraDb::block` by height, then the serialization for the wire.
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
        let db = ZebraBlockDb::open(dir.path()).unwrap();
        let fixture = fixtures::mixed_block(3, 1, 2, 2);
        let block = Block::zcash_deserialize(&fixture.bytes[..]).unwrap();
        db.write_block(fixture.height, &block).unwrap();
        db.write_block(fixture.height + 1, &block).unwrap();
        assert_eq!(db.serve_block(fixture.height).unwrap(), fixture.bytes);
        let None = db.serve_block(fixture.height + 2) else {
            panic!("an unwritten height must be absent");
        };
    }
}
