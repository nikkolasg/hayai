use std::path::PathBuf;
use std::sync::Arc;

use hayai_crypto::incrementalmerkletree::{Hashable, Level};
use hayai_crypto::sapling_crypto::Node;

use super::*;

fn tempdir() -> tempfile::TempDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
}

fn address(i: u8) -> AddressKey {
    AddressKey::p2pkh([i; 20])
}

fn hash(i: u32) -> [u8; 32] {
    let mut h = [0xbb; 32];
    h[..4].copy_from_slice(&i.to_le_bytes());
    h
}

fn txid(height: u32, i: u16) -> [u8; 32] {
    let mut t = [0x11; 32];
    t[..4].copy_from_slice(&height.to_be_bytes());
    t[4..6].copy_from_slice(&i.to_be_bytes());
    t
}

/// A delta of block `height` with `txs` transactions.
fn delta(height: u32, txs: u16, created: Vec<Created>, spent: Vec<Spent>) -> Delta {
    let mut d = Delta {
        height,
        hash: hash(height),
        parent: hash(height - 1),
        txids: (0..txs).map(|i| txid(height, i)).collect(),
        created,
        spent,
        subtrees: vec![(
            SubtreePool::Sapling,
            Subtree {
                index: height as u16,
                root: [height as u8; 32],
                end_height: height,
            },
        )],
        undo: Vec::new(),
    };
    d.encode_undo();
    d
}

/// Every entry of each column family, with the balances as values: the state of the index
/// as a value. A balance of zero is the absence of a balance.
fn content(index: &WalletIndex) -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for name in CFS {
        for entry in index.db.iterator_cf(index.cf(name), IteratorMode::Start) {
            let (k, v) = entry.unwrap();
            if name == CF_ADDR_BALANCE && v[..] == [0; 16] {
                continue;
            }
            out.push((name, k.to_vec(), v.to_vec()));
        }
    }
    out
}

#[test]
fn only_p2pkh_and_p2sh_scripts_have_an_address() {
    let a = AddressKey::p2pkh([7; 20]);
    assert_eq!(AddressKey::of_script(&a.script()), Some(a));
    let b = AddressKey::p2sh([9; 20]);
    assert_eq!(AddressKey::of_script(&b.script()), Some(b));
    assert!(b.is_p2sh() && !a.is_p2sh());
    assert_eq!(a.hash(), [7; 20]);
    // OP_TRUE, a P2PK script, and a P2PKH script with a short hash.
    assert_eq!(AddressKey::of_script(&[0x51]), None);
    assert_eq!(
        AddressKey::of_script(&[[0x21].as_slice(), &[2; 33], &[0xac]].concat()),
        None
    );
    assert_eq!(
        AddressKey::of_script(&[[0x76, 0xa9, 0x13].as_slice(), &[1; 19], &[0x88, 0xac]].concat()),
        None
    );
}

#[test]
fn the_subtree_boundaries_of_a_block() {
    assert!(!crosses(0, 0));
    assert!(!crosses(10, 100));
    assert!(crosses(65_535, 1));
    assert!(!crosses(65_536, 1));
    assert_eq!(boundaries(65_000, 1_000).collect::<Vec<_>>(), [(0, 536)]);
    assert_eq!(
        boundaries(65_000, 70_000).collect::<Vec<_>>(),
        [(0, 536), (1, 536 + 65_536)]
    );
}

/// The root of level 16 of the frontier at the last leaf of a subtree is the subtree root:
/// with the empty roots above it, it gives the root of a tree that has only that subtree.
#[test]
fn the_level_root_is_the_root_of_the_completed_subtree() {
    let leaves: Vec<Node> = (0..70_000u64)
        .map(|i| {
            // A small integer is a canonical field element.
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&i.to_le_bytes());
            Option::from(Node::from_bytes(b)).expect("a canonical encoding")
        })
        .collect();
    // The frontier before the block holds 65,000 leaves; the block adds 5,000.
    let mut before = SaplingFrontier::empty();
    before.append_many(&leaves[..65_000]).unwrap();
    let (index, prefix) = boundaries(65_000, 5_000).next().unwrap();
    assert_eq!((index, prefix), (0, 536));
    let mut at = before.clone();
    at.append_many(&leaves[65_000..65_000 + prefix]).unwrap();
    let root = level_root(at.frontier().value()).unwrap();

    let mut only = SaplingFrontier::empty();
    only.append_many(&leaves[..65_536]).unwrap();
    let mut expected = root;
    for level in SUBTREE_LEVEL..32 {
        expected = Node::combine(
            Level::from(level),
            &expected,
            &Node::empty_root(Level::from(level)),
        );
    }
    assert_eq!(expected, only.frontier().root());
}

#[test]
fn an_undo_record_round_trips() {
    let d = delta(
        5,
        2,
        vec![Created {
            address: address(1),
            tx: 1,
            index: 3,
            value: 77,
        }],
        vec![Spent {
            address: AddressKey::p2sh([2; 20]),
            height: 2,
            txid: txid(2, 0),
            index: 1,
            value: 50,
            spender: 1,
        }],
    );
    let mut back = Delta::default();
    back.decode_undo(5, &d.undo).unwrap();
    assert_eq!(
        (
            back.hash,
            back.parent,
            &back.created,
            &back.spent,
            &back.subtrees
        ),
        (d.hash, d.parent, &d.created, &d.spent, &d.subtrees)
    );
    let Err(Error::Corrupt(_)) = back.decode_undo(5, &d.undo[..d.undo.len() - 1]) else {
        panic!("a cut record");
    };
}

/// Three blocks: outputs to two addresses, a spend of an earlier block and a spend of an
/// output of the same block. The queries give the values of the blocks, and an undo of the
/// last two blocks gives the content after the first block.
#[test]
fn the_queries_follow_the_blocks_and_an_undo_restores_the_content() {
    let dir = tempdir();
    let index = WalletIndex::open(dir.path()).unwrap();
    index.start_at_genesis(&hash(0)).unwrap();
    let (a, b) = (address(1), AddressKey::p2sh([2; 20]));
    let one = delta(
        1,
        1,
        vec![
            Created {
                address: a,
                tx: 0,
                index: 0,
                value: 1_000,
            },
            Created {
                address: b,
                tx: 0,
                index: 1,
                value: 500,
            },
        ],
        Vec::new(),
    );
    index.write(std::slice::from_ref(&one), 0).unwrap();
    let after_one = content(&index);
    let two = delta(
        2,
        2,
        vec![Created {
            address: b,
            tx: 1,
            index: 0,
            value: 900,
        }],
        vec![Spent {
            address: a,
            height: 1,
            txid: txid(1, 0),
            index: 0,
            value: 1_000,
            spender: 1,
        }],
    );
    // Block 3 pays `a` and spends that output in its next transaction.
    let three = delta(
        3,
        3,
        vec![Created {
            address: a,
            tx: 1,
            index: 0,
            value: 40,
        }],
        vec![Spent {
            address: a,
            height: 3,
            txid: txid(3, 1),
            index: 0,
            value: 40,
            spender: 2,
        }],
    );
    index.write(&[two, three], 0).unwrap();
    assert_eq!(index.tip().unwrap(), Some((3, hash(3))));
    assert_eq!(
        index.balance(&[a]).unwrap(),
        Balance {
            balance: 0,
            received: 1_040
        }
    );
    assert_eq!(
        index.balance(&[a, b]).unwrap(),
        Balance {
            balance: 1_400,
            received: 2_440
        }
    );
    assert_eq!(
        index.address_txids(&[a], 0, 10).unwrap(),
        [txid(1, 0), txid(2, 1), txid(3, 1), txid(3, 2)]
    );
    assert_eq!(index.address_txids(&[a, b], 2, 2).unwrap(), [txid(2, 1)]);
    let (utxos, tip) = index.address_utxos(&[a, b]).unwrap();
    assert_eq!(tip, Some((3, hash(3))));
    let keys: Vec<_> = utxos
        .iter()
        .map(|u| (u.height, u.txid, u.index, u.value))
        .collect();
    assert_eq!(keys, [(1, txid(1, 0), 1, 500), (2, txid(2, 1), 0, 900)]);
    assert_eq!(
        index.tx_location(&txid(3, 2)).unwrap(),
        Some(TxLoc {
            height: 3,
            index: 2
        })
    );
    let subtrees = index.subtrees(SubtreePool::Sapling, 1, Some(5)).unwrap();
    assert_eq!(
        subtrees.iter().map(|t| t.index).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        index
            .subtrees(SubtreePool::Sapling, 2, Some(1))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(index.subtrees(SubtreePool::Orchard, 0, None).unwrap(), []);

    assert_eq!(index.rewind_to(1, &hash(1)).unwrap(), 2);
    assert_eq!(content(&index), after_one);
    assert_eq!(index.tx_location(&txid(3, 2)).unwrap(), None);
    // A rewind to a block that the index does not hold is an error.
    let Err(Error::Chain(_)) = index.rewind_to(1, &hash(9)) else {
        panic!("a rewind to another block");
    };

    // A persist removes the undo records at or below its height: no rewind goes there.
    let two = delta(2, 1, Vec::new(), Vec::new());
    let three = delta(3, 1, Vec::new(), Vec::new());
    index.write(&[two, three], 0).unwrap();
    index.persist(2).unwrap();
    assert_eq!(index.rewind_to(2, &hash(2)).unwrap(), 1);
    let Err(Error::Corrupt(_)) = index.rewind_to(1, &hash(1)) else {
        panic!("a rewind below the persisted height");
    };
}

/// A Mainnet block of NU5 on a tree that lacks one leaf to its first subtree: the delta has
/// the subtree of the first leaf of the block, with the root that the upstream frontier
/// gives, for each pool with leaves in the block.
#[test]
fn a_block_that_completes_a_subtree_has_its_root() {
    use bytes::Bytes;
    use hayai_coins::Coin;
    use hayai_crypto::zcash_protocol::consensus::BranchId;
    use hayai_wire::RawBlock;

    let mut checked = 0;
    for name in ["1-687-106", "1-687-107", "1-687-108"] {
        let text = std::fs::read_to_string(format!(
            "{}/../hayai-wire/tests/vectors/block-main-{name}.hex",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let raw = Arc::new(
            RawBlock::parse(
                Bytes::from(hex::decode(text.trim()).unwrap()),
                BranchId::Nu5,
            )
            .unwrap(),
        );
        let sapling: Vec<Node> = raw
            .txs
            .iter()
            .filter_map(|t| t.tx.sapling_bundle())
            .flat_map(|b| b.shielded_outputs().iter().map(|o| Node::from_cmu(o.cmu())))
            .collect();
        let orchard: Vec<MerkleHashOrchard> = raw
            .txs
            .iter()
            .filter_map(|t| t.tx.orchard_bundle())
            .flat_map(|b| {
                b.actions()
                    .iter()
                    .map(|a| MerkleHashOrchard::from_cmx(a.cmx()))
            })
            .collect();
        let filler = |i: u64| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&i.to_le_bytes());
            b
        };
        let mut sapling_before = SaplingFrontier::empty();
        let leaves: Vec<Node> = (0..SUBTREE_LEAVES - 1)
            .map(|i| Option::from(Node::from_bytes(filler(i))).unwrap())
            .collect();
        sapling_before.append_many(&leaves).unwrap();
        let mut orchard_before = OrchardFrontier::empty();
        let leaves: Vec<MerkleHashOrchard> = (0..SUBTREE_LEAVES - 1)
            .map(|i| Option::from(MerkleHashOrchard::from_bytes(&filler(i))).unwrap())
            .collect();
        orchard_before.append_many(&leaves).unwrap();

        let spent_coins = raw
            .txs
            .iter()
            .map(|t| match t.tx.transparent_bundle() {
                Some(b) if !b.is_coinbase() => vec![
                    Coin {
                        value: 1,
                        script_pubkey: Bytes::from_static(&[0x51]),
                        height: 1,
                        is_coinbase: false,
                    };
                    b.vin.len()
                ],
                _ => Vec::new(),
            })
            .collect();
        let job = BlockJob {
            height: 1_687_000,
            hash: raw.hash().0,
            parent: raw.header.prev_hash.0,
            raw: raw.clone(),
            spent_coins,
            trees_before: TreesBefore {
                sapling: Arc::new(sapling_before.clone()),
                orchard: Arc::new(orchard_before.clone()),
                ironwood: Arc::new(OrchardFrontier::empty()),
            },
        };
        let mut delta = Delta::default();
        delta.build(&job).unwrap();

        let mut expected = Vec::new();
        if let Some(first) = sapling.first() {
            let mut f = sapling_before.into_frontier();
            assert!(f.append(*first));
            let root = f.value().unwrap().root(Some(Level::from(SUBTREE_LEVEL)));
            expected.push((SubtreePool::Sapling, root.to_bytes()));
        }
        if let Some(first) = orchard.first() {
            let mut f = orchard_before.into_frontier();
            assert!(f.append(*first));
            let root = f.value().unwrap().root(Some(Level::from(SUBTREE_LEVEL)));
            expected.push((SubtreePool::Orchard, root.to_bytes()));
        }
        checked += expected.len();
        let found: Vec<_> = delta
            .subtrees
            .iter()
            .map(|(pool, t)| {
                assert_eq!((t.index, t.end_height), (0, 1_687_000));
                (*pool, t.root)
            })
            .collect();
        assert_eq!(found, expected, "{name}");
        assert_eq!(delta.txids.len(), raw.txs.len());
    }
    assert!(checked >= 1, "no vector block has shielded outputs");
}

/// An undo of a block that the index does not hold stops the writer, and each later call
/// returns the error.
#[test]
fn the_writer_stops_at_an_undo_of_a_block_that_it_does_not_hold() {
    let dir = tempdir();
    let index = Arc::new(WalletIndex::open(dir.path()).unwrap());
    index.start_at_genesis(&hash(0)).unwrap();
    let mut writer = IndexWriter::spawn(index.clone()).unwrap();
    writer.undo(1, hash(1)).unwrap();
    let Err(_) = writer.persist(0, 0) else {
        panic!("an undo of a block that the index does not hold");
    };
    let Err(_) = writer.close() else {
        panic!("the writer failed");
    };
}

/// A persist returns after the sync that the persist before started when that sync holds
/// the base: its tip is at or above the base, and no later undo went to or below the base.
/// Else the persist waits for a new sync, and fails when the index tip is below the base.
#[test]
fn a_persist_waits_for_a_new_sync_only_when_the_sync_in_the_background_lacks_the_base() {
    use std::sync::atomic::Ordering;

    use bytes::Bytes;
    use hayai_crypto::zcash_protocol::consensus::BranchId;
    use hayai_wire::RawBlock;

    let text = std::fs::read_to_string(format!(
        "{}/../hayai-wire/tests/vectors/block-main-0-000-001.hex",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let raw = Arc::new(
        RawBlock::parse(
            Bytes::from(hex::decode(text.trim()).unwrap()),
            BranchId::Sprout,
        )
        .unwrap(),
    );
    let job = |height: u32, hash: [u8; 32], parent: [u8; 32]| BlockJob {
        height,
        hash,
        parent,
        raw: raw.clone(),
        spent_coins: vec![Vec::new()],
        trees_before: TreesBefore {
            sapling: Arc::new(SaplingFrontier::empty()),
            orchard: Arc::new(OrchardFrontier::empty()),
            ironwood: Arc::new(OrchardFrontier::empty()),
        },
    };
    let dir = tempdir();
    let index = Arc::new(WalletIndex::open(dir.path()).unwrap());
    index.start_at_genesis(&hash(0)).unwrap();
    let mut writer = IndexWriter::spawn(index.clone()).unwrap();
    let stalls = |w: &IndexWriter| w.stats.persist_stalls.load(Ordering::Relaxed);
    for height in 1..=4 {
        writer
            .apply(job(height, hash(height), hash(height - 1)))
            .unwrap();
    }
    // No sync runs in the background before the first persist.
    writer.persist(1, 0).unwrap();
    assert_eq!(stalls(&writer), 1);
    // The sync in the background holds the tip 4.
    writer.persist(4, 1).unwrap();
    assert_eq!(stalls(&writer), 1);
    // After the undo of block 4 the sync in the background still holds block 3.
    writer.undo(4, hash(4)).unwrap();
    writer.persist(3, 1).unwrap();
    assert_eq!(stalls(&writer), 1);
    // A reorg replaces block 3: the sync in the background holds the old block 3.
    let other = [0xcc; 32];
    writer.undo(3, hash(3)).unwrap();
    writer.apply(job(3, other, hash(2))).unwrap();
    writer.persist(3, 1).unwrap();
    assert_eq!(stalls(&writer), 2);
    assert_eq!(index.tip().unwrap(), Some((3, other)));
    // A base above the index tip is an error.
    let Err(_) = writer.persist(4, 1) else {
        panic!("a persist of a base above the index tip");
    };
    writer.close().unwrap();
}
