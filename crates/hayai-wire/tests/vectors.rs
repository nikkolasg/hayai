//! Mainnet block vectors (from Zebra's test vectors): genesis, block 1, and three
//! consecutive NU5 blocks with v5 transactions. The zcashd Regtest genesis block (Zebra's
//! `block-regtest-0-000-000.txt`, the same file as Zakura's) carries a real Equihash
//! (48, 5) solution.

use bytes::Bytes;
use hayai_crypto::{zcash_primitives, zcash_protocol};
use hayai_wire::header::{check_equihash, check_pow, BlockHeader, PowError, PowParams};
use hayai_wire::{auth_data_root, merkle_root, ParseError, RawBlock, RawTx, PRE_V5_AUTH_DIGEST};
use zcash_primitives::transaction::TxVersion;
use zcash_protocol::consensus::BranchId;

fn vector(name: &str) -> Bytes {
    vector_of("main", name)
}

fn vector_of(network: &str, name: &str) -> Bytes {
    let hex = std::fs::read_to_string(format!(
        "{}/tests/vectors/block-{network}-{name}.hex",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    Bytes::from(hex::decode(hex.trim()).unwrap())
}

/// Length of a Mainnet header.
const MAINNET_HEADER_LEN: usize = PowParams::MAINNET.header_len();

/// The Mainnet proof-of-work limit `2^243 - 1`, little-endian.
const MAINNET_POW_LIMIT: [u8; 32] = {
    let mut limit = [0xff; 32];
    limit[30] = 0x07;
    limit[31] = 0x00;
    limit
};

const NU5_BLOCKS: [&str; 3] = ["1-687-106", "1-687-107", "1-687-108"];

fn all_blocks() -> Vec<(Bytes, BranchId)> {
    let mut v = vec![
        (vector("0-000-000"), BranchId::Sprout),
        (vector("0-000-001"), BranchId::Sprout),
    ];
    v.extend(NU5_BLOCKS.iter().map(|n| (vector(n), BranchId::Nu5)));
    v
}

#[test]
fn merkle_root_matches_header() {
    for (bytes, branch) in all_blocks() {
        let block = RawBlock::parse(bytes, branch).unwrap();
        assert_eq!(merkle_root(&block.txids()), block.header.merkle_root);
    }
}

#[test]
fn header_hash_chains() {
    let blocks: Vec<RawBlock> = NU5_BLOCKS
        .iter()
        .map(|n| RawBlock::parse(vector(n), BranchId::Nu5).unwrap())
        .collect();
    for pair in blocks.windows(2) {
        assert_eq!(pair[0].hash(), pair[1].header.prev_hash);
    }
    let genesis = RawBlock::parse(vector("0-000-000"), BranchId::Sprout).unwrap();
    assert_eq!(
        genesis.hash().to_string(),
        "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08"
    );
}

#[test]
fn pow_and_equihash_pass_on_mainnet_headers() {
    for (bytes, branch) in all_blocks() {
        let block = RawBlock::parse(bytes, branch).unwrap();
        assert_eq!(check_pow(&block.header, &MAINNET_POW_LIMIT), Ok(()));
        check_equihash(&block.header, PowParams::MAINNET)
            .unwrap_or_else(|e| panic!("equihash {}: {e}", block.hash()));
        let Err(_) = check_equihash(&block.header, PowParams::REGTEST) else {
            panic!("a (200, 9) solution verified under (48, 5)");
        };

        let mut bad = block.header.clone();
        bad.solution[100] ^= 1;
        let Err(_) = check_equihash(&bad, PowParams::MAINNET) else {
            panic!("corrupted solution verified");
        };
        let mut bad = block.header.clone();
        bad.nonce[0] ^= 1;
        let Err(_) = check_equihash(&bad, PowParams::MAINNET) else {
            panic!("changed nonce verified");
        };
        let mut hard = block.header.clone();
        hard.bits = 0x0100_0001;
        assert_eq!(
            check_pow(&hard, &MAINNET_POW_LIMIT),
            Err(PowError::InvalidBits(0x0100_0001))
        );
        // A target above the limit fails before the hash is compared.
        let mut easy = block.header.clone();
        easy.bits = 0x2007_ffff;
        assert_eq!(
            check_pow(&easy, &MAINNET_POW_LIMIT),
            Err(PowError::TargetAboveLimit(0x2007_ffff))
        );
    }
}

/// The zcashd Regtest genesis block: a 177-byte header with a 36-byte solution that
/// verifies under Equihash (48, 5), the hash that `zcash-cli -regtest getblockhash 0`
/// prints, and an exact round trip of the whole block.
#[test]
fn regtest_genesis_parses_hashes_and_verifies_under_48_5() {
    let bytes = vector_of("regtest", "0-000-000");
    let block = RawBlock::parse(bytes.clone(), BranchId::Sprout).unwrap();
    let header = &block.header;
    assert_eq!(header.solution.len(), 36);
    assert_eq!(header.serialized_len(), PowParams::REGTEST.header_len());
    assert_eq!(&header.serialize()[..], &bytes[..177]);
    assert_eq!(
        block.hash().to_string(),
        "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327"
    );
    assert_eq!(block.txs.len(), 1);
    assert_eq!(merkle_root(&block.txids()), header.merkle_root);
    // The Regtest target 0x200f0f0f admits the genesis hash.
    assert_eq!(check_pow(header, &[0x0f; 32]), Ok(()));

    check_equihash(header, PowParams::REGTEST).expect("the genesis solution is valid");
    let Err(_) = check_equihash(header, PowParams::MAINNET) else {
        panic!("a (48, 5) solution verified under (200, 9)");
    };
    for i in [0, 17, 35] {
        let mut bad = header.clone();
        bad.solution[i] ^= 1;
        let Err(_) = check_equihash(&bad, PowParams::REGTEST) else {
            panic!("solution with byte {i} changed verified");
        };
    }
    let mut bad = header.clone();
    bad.nonce[31] ^= 1;
    let Err(_) = check_equihash(&bad, PowParams::REGTEST) else {
        panic!("changed nonce verified");
    };
}

#[test]
fn header_round_trips_through_bytes() {
    for (bytes, branch) in all_blocks() {
        let block = RawBlock::parse(bytes.clone(), branch).unwrap();
        assert_eq!(&block.header.serialize()[..], &bytes[..MAINNET_HEADER_LEN]);
        assert_eq!(BlockHeader::parse(&bytes).unwrap(), block.header);
    }
}

#[test]
fn transaction_bytes_are_sub_slices_of_the_block() {
    for (bytes, branch) in all_blocks() {
        let block = RawBlock::parse(bytes, branch).unwrap();
        let range = block.bytes.as_ptr_range();
        let mut expected_start = block.bytes.as_ptr() as usize + MAINNET_HEADER_LEN;
        // CompactSize of the transaction count: one byte for the small vectors here.
        assert!(block.txs.len() < 253);
        expected_start += 1;
        for tx in &block.txs {
            let r = tx.bytes.as_ptr_range();
            assert!(r.start >= range.start && r.end <= range.end);
            assert_eq!(
                r.start as usize, expected_start,
                "transactions are contiguous"
            );
            expected_start = r.end as usize;
        }
        assert_eq!(expected_start, range.end as usize);
    }
}

#[test]
fn standalone_tx_parse_agrees_with_block_parse() {
    for (bytes, branch) in all_blocks() {
        let block = RawBlock::parse(bytes, branch).unwrap();
        for tx in &block.txs {
            let alone = RawTx::parse(tx.bytes.clone(), branch).unwrap();
            assert_eq!(alone.txid, tx.txid);
            assert_eq!(alone.auth_digest, tx.auth_digest);
            assert_eq!(alone.wtxid(), tx.wtxid());
            let mut reserialized = Vec::new();
            tx.tx.write(&mut reserialized).unwrap();
            assert_eq!(reserialized, tx.bytes, "write(read(x)) == x");
        }
    }
}

#[test]
fn auth_digest_follows_zip_239() {
    let genesis = RawBlock::parse(vector("0-000-000"), BranchId::Sprout).unwrap();
    assert_eq!(genesis.txs[0].auth_digest, PRE_V5_AUTH_DIGEST);
    assert_eq!(auth_data_root(&genesis.auth_digests()), PRE_V5_AUTH_DIGEST);

    let nu5 = RawBlock::parse(vector("1-687-107"), BranchId::Nu5).unwrap();
    let mut saw_v5 = false;
    for tx in &nu5.txs {
        match tx.tx.version() {
            TxVersion::V5 => {
                saw_v5 = true;
                assert_ne!(tx.auth_digest, PRE_V5_AUTH_DIGEST);
                assert_eq!(tx.auth_digest, tx.tx.auth_commitment().as_bytes());
            }
            _ => assert_eq!(tx.auth_digest, PRE_V5_AUTH_DIGEST),
        }
    }
    assert!(saw_v5);
    // The root is a function of the digests only. A different digest changes it.
    let mut digests = nu5.auth_digests();
    let root = auth_data_root(&digests);
    digests[1][0] ^= 1;
    assert_ne!(auth_data_root(&digests), root);
}

#[test]
fn malformed_blocks_are_rejected() {
    let good = vector("1-687-107");

    let mut trailing = good.to_vec();
    trailing.push(0);
    assert!(matches!(
        RawBlock::parse(Bytes::from(trailing), BranchId::Nu5),
        Err(ParseError::Trailing)
    ));

    let truncated = good.slice(..good.len() - 1);
    assert!(matches!(
        RawBlock::parse(truncated, BranchId::Nu5),
        Err(ParseError::Transaction(_))
    ));

    let mut empty = good[..MAINNET_HEADER_LEN].to_vec();
    empty.push(0);
    assert!(matches!(
        RawBlock::parse(Bytes::from(empty), BranchId::Nu5),
        Err(ParseError::Empty)
    ));

    let header_only = good.slice(..MAINNET_HEADER_LEN);
    assert!(matches!(
        RawBlock::parse(header_only, BranchId::Nu5),
        Err(ParseError::TxCount(_))
    ));

    let short = good.slice(..100);
    assert!(matches!(
        RawBlock::parse(short, BranchId::Nu5),
        Err(ParseError::Header(_))
    ));

    let block = RawBlock::parse(good, BranchId::Nu5).unwrap();
    let mut tx_trailing = block.txs[1].bytes.to_vec();
    tx_trailing.push(0);
    assert!(matches!(
        RawTx::parse(Bytes::from(tx_trailing), BranchId::Nu5),
        Err(ParseError::Trailing)
    ));
}
