//! The layout scanner against the upstream parser.
//!
//! Property: for every transaction that `Transaction::read` accepts, `tx_wire_len` returns
//! exactly the number of bytes that the parser consumed. The authorizing digest from the
//! scanned ranges equals `Transaction::auth_commitment()`. The tests check this on the
//! mainnet block vectors, on the ZIP 143/243/244 transaction vectors (from
//! zcash-test-vectors, as embedded in zcash_primitives' own tests), on transactions of every
//! version v1–v6 that this file generates with random valid field and group encodings, and on
//! the cached benchmark fixtures when they are present. Malformed input (random bytes,
//! truncations) must return an error and must never panic.

use std::io::Cursor;

use bytes::Bytes;
use ff::{Field, PrimeField};
use group::{Group, GroupEncoding};
use hayai_crypto::rng::StdRng as ElementRng;
use hayai_crypto::{ff, group, jubjub, orchard, pasta_curves, zcash_primitives, zcash_protocol};
use hayai_wire::{tx_wire_len, ParseError, RawBlock, RawTx, PARALLEL_PARSE_THRESHOLD};
use pasta_curves::pallas;
use proptest::prelude::*;
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;
use zcash_protocol::constants::{
    V3_TX_VERSION, V3_VERSION_GROUP_ID, V4_TX_VERSION, V4_VERSION_GROUP_ID, V5_TX_VERSION,
    V5_VERSION_GROUP_ID, V6_TX_VERSION, V6_VERSION_GROUP_ID,
};
use zcash_protocol::value::MAX_MONEY;

fn vector_lines(name: &str) -> Vec<Vec<Vec<u8>>> {
    let text = std::fs::read_to_string(format!(
        "{}/tests/vectors/{name}.hex",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            l.split_whitespace()
                .map(|field| hex::decode(field).unwrap())
                .collect()
        })
        .collect()
}

/// Parses `bytes` with the upstream parser and checks every scanner property on it.
/// Returns the parsed transaction and the number of bytes it consumed.
fn check_tx(bytes: &[u8], branch: BranchId) -> (Transaction, usize) {
    let mut cursor = Cursor::new(bytes);
    let tx = Transaction::read(&mut cursor, branch).expect("upstream parses the transaction");
    let consumed = cursor.position() as usize;
    assert_eq!(tx_wire_len(bytes).unwrap(), consumed, "wire length");

    // Trailing bytes do not change the length. The scanner rejects every strict prefix.
    let mut padded = bytes[..consumed].to_vec();
    padded.extend_from_slice(&[0xAB; 7]);
    assert_eq!(tx_wire_len(&padded).unwrap(), consumed);
    for cut in 0..consumed {
        let Err(ParseError::Transaction(_)) = tx_wire_len(&bytes[..cut]) else {
            panic!("prefix of {cut} bytes accepted");
        };
    }

    let raw = RawTx::parse(Bytes::copy_from_slice(&bytes[..consumed]), branch).unwrap();
    assert_eq!(raw.txid, tx.txid());
    assert_eq!(raw.auth_digest, expected_auth_digest(&tx));
    let mut reserialized = Vec::new();
    tx.write(&mut reserialized).unwrap();
    assert_eq!(reserialized, &bytes[..consumed], "write(read(x)) == x");
    (tx, consumed)
}

fn expected_auth_digest(tx: &Transaction) -> [u8; 32] {
    use zcash_primitives::transaction::TxVersion;
    match tx.version() {
        TxVersion::Sprout(_) | TxVersion::V3 | TxVersion::V4 => hayai_wire::PRE_V5_AUTH_DIGEST,
        TxVersion::V5 | TxVersion::V6 => tx.auth_commitment().as_bytes().try_into().unwrap(),
    }
}

#[test]
fn mainnet_block_transactions() {
    for (name, branch) in [
        ("0-000-000", BranchId::Sprout),
        ("0-000-001", BranchId::Sprout),
        ("1-687-106", BranchId::Nu5),
        ("1-687-107", BranchId::Nu5),
        ("1-687-108", BranchId::Nu5),
    ] {
        let bytes = Bytes::from(vector_lines(&format!("block-main-{name}"))[0][0].clone());
        let block = RawBlock::parse(bytes.clone(), branch).unwrap();
        for tx in &block.txs {
            let (_, consumed) = check_tx(&tx.bytes, branch);
            assert_eq!(consumed, tx.bytes.len());
        }
        for (i, tx) in block.txs.iter().enumerate() {
            assert_eq!(RawBlock::tx_bytes(&bytes, i).unwrap(), tx.bytes);
        }
        let Err(hayai_wire::ParseError::NoTransaction { .. }) =
            RawBlock::tx_bytes(&bytes, block.txs.len())
        else {
            panic!("a transaction after the last one");
        };
        assert_blocks_equal(&block, &RawBlock::parse_sequential(bytes, branch).unwrap());
    }
}

#[test]
fn upstream_transaction_vectors() {
    // v4 with JoinSplits (zcash_primitives `tx_read_write`).
    let (tx, _) = check_tx(&vector_lines("tx-read-write-v4")[0][0], BranchId::Canopy);
    assert_eq!(
        tx.txid().to_string(),
        "64f0bd7fe30ce23753358fe3a2dc835b8fba9c0274c4e2c54a6f73114cb55639"
    );
    // ZIP 143: v3 (Overwinter) transactions, several with JoinSplits (PHGR13 proofs).
    for row in vector_lines("tx-zip0143") {
        check_tx(&row[0], BranchId::Overwinter);
    }
    // ZIP 243: v4 transactions with Sapling spends/outputs and Groth16 JoinSplits.
    for row in vector_lines("tx-zip0243") {
        check_tx(&row[0], BranchId::Sapling);
    }
    // ZIP 244: v5 transactions with Sapling and Orchard bundles, with the expected txid
    // and authorizing digest.
    let mut with_orchard = 0;
    let mut with_sapling = 0;
    for row in vector_lines("tx-zip0244") {
        let (tx, consumed) = check_tx(&row[0], BranchId::Nu5);
        assert_eq!(consumed, row[0].len());
        let raw = RawTx::parse(Bytes::from(row[0].clone()), BranchId::Nu5).unwrap();
        assert_eq!(raw.txid.as_ref(), &row[1][..]);
        assert_eq!(&raw.auth_digest[..], &row[2][..]);
        with_orchard += tx.orchard_bundle().map_or(0, |b| b.actions().len());
        with_sapling += tx
            .sapling_bundle()
            .map_or(0, |b| b.shielded_outputs().len());
    }
    assert!(with_orchard > 0 && with_sapling > 0);
}

/// Cached benchmark fixtures (`<repo>/bench-fixtures/*.bin`, from hayai-bench) when they are
/// present: v5 blocks with thousands of transparent transactions and real Orchard bundles.
#[test]
fn cached_bench_fixtures() {
    let dir = format!("{}/../../bench-fixtures", env!("CARGO_MANIFEST_DIR"));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("no fixture cache at {dir}; skipping");
        return;
    };
    let mut checked = 0;
    for entry in entries {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("bin") {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        // hayai-bench cache framing: 8-byte magic, 4-byte generator version, block bytes.
        assert_eq!(&data[..8], b"HAYAIFIX", "{}", path.display());
        let bytes = Bytes::from(data[12..].to_vec());
        let branch = BranchId::Nu6_2;
        let block = RawBlock::parse(bytes.clone(), branch).unwrap();
        for tx in &block.txs {
            let (_, consumed) = check_tx(&tx.bytes, branch);
            assert_eq!(consumed, tx.bytes.len());
        }
        assert_blocks_equal(&block, &RawBlock::parse_sequential(bytes, branch).unwrap());
        checked += 1;
    }
    eprintln!("checked {checked} cached fixtures");
}

fn assert_blocks_equal(a: &RawBlock, b: &RawBlock) {
    assert_eq!(a.header, b.header);
    assert_eq!(a.txs.len(), b.txs.len());
    for (x, y) in a.txs.iter().zip(&b.txs) {
        assert_eq!(x.bytes, y.bytes);
        assert_eq!(x.txid, y.txid);
        assert_eq!(x.auth_digest, y.auth_digest);
        assert_eq!(x.tx.version(), y.tx.version());
    }
}

// ---------------------------------------------------------------------------------------
// Generated transactions: the generator assembles the wire bytes directly, with random valid
// encodings for every field that the upstream parser decodes. `Transaction::read` is the
// oracle. It must accept the bytes, so a layout mistake in the generator fails the test.

#[derive(Clone, Debug)]
struct Spec {
    version: u8,
    branch: BranchId,
    vin: usize,
    vout: usize,
    joinsplits: usize,
    spends: usize,
    outputs: usize,
    actions: usize,
    ironwood_actions: usize,
    seed: u64,
}

fn arb_spec() -> impl Strategy<Value = Spec> {
    (
        1u8..=6,
        0usize..4,
        0usize..4,
        0usize..3,
        0usize..3,
        0usize..3,
        0usize..4,
        0usize..4,
        any::<u64>(),
        any::<bool>(),
    )
        .prop_map(
            |(version, vin, vout, joinsplits, spends, outputs, actions, ironwood, seed, alt)| {
                let branch = match version {
                    1 | 2 => BranchId::Sprout,
                    3 => BranchId::Overwinter,
                    4 => BranchId::Canopy,
                    // Nu5: historical Orchard pool, proof length not enforced; Nu6.2: enforced.
                    5 if alt => BranchId::Nu5,
                    5 => BranchId::Nu6_2,
                    _ => BranchId::Nu6_3,
                };
                Spec {
                    version,
                    branch,
                    vin,
                    vout,
                    joinsplits: if (2..=4).contains(&version) {
                        joinsplits
                    } else {
                        0
                    },
                    spends: if version >= 4 { spends } else { 0 },
                    outputs: if version >= 4 { outputs } else { 0 },
                    actions: if version >= 5 { actions } else { 0 },
                    ironwood_actions: if version == 6 { ironwood } else { 0 },
                    seed,
                }
            },
        )
}

struct Gen {
    rng: StdRng,
    /// Field and group elements come from the backend's own RNG type.
    elements: ElementRng,
    out: Vec<u8>,
}

impl Gen {
    fn bytes(&mut self, n: usize) {
        let start = self.out.len();
        self.out.resize(start + n, 0);
        self.rng.fill_bytes(&mut self.out[start..]);
    }
    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn compact_size(&mut self, n: usize) {
        match n {
            0..=252 => self.out.push(n as u8),
            253..=0xFFFF => {
                self.out.push(253);
                self.out.extend_from_slice(&(n as u16).to_le_bytes());
            }
            _ => {
                self.out.push(254);
                self.out.extend_from_slice(&(n as u32).to_le_bytes());
            }
        }
    }
    fn amount(&mut self) {
        let v: u64 = self.rng.gen_range(0..=MAX_MONEY);
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn balance(&mut self) {
        let v: i64 = self.rng.gen_range(-(MAX_MONEY as i64)..=MAX_MONEY as i64);
        self.out.extend_from_slice(&v.to_le_bytes());
    }
    fn script(&mut self) {
        // Random lengths including ones that need the 3-byte CompactSize form.
        let n = if self.rng.gen_bool(0.1) {
            self.rng.gen_range(253..600)
        } else {
            self.rng.gen_range(0..120)
        };
        self.compact_size(n);
        self.bytes(n);
    }
    fn pallas_point(&mut self) {
        self.out
            .extend_from_slice(&pallas::Point::random(&mut self.elements).to_bytes());
    }
    fn pallas_base(&mut self) {
        self.out
            .extend_from_slice(&pallas::Base::random(&mut self.elements).to_repr());
    }
    fn jubjub_point(&mut self) {
        let p = jubjub::ExtendedPoint::from(jubjub::SubgroupPoint::random(&mut self.elements));
        self.out.extend_from_slice(&p.to_bytes());
    }
    fn jubjub_base(&mut self) {
        self.out
            .extend_from_slice(&jubjub::Base::random(&mut self.elements).to_repr());
    }

    fn transparent(&mut self, spec: &Spec) {
        self.compact_size(spec.vin);
        for _ in 0..spec.vin {
            self.bytes(36);
            self.script();
            self.bytes(4);
        }
        self.compact_size(spec.vout);
        for _ in 0..spec.vout {
            self.amount();
            self.script();
        }
    }

    fn joinsplits(&mut self, n: usize, proof_len: usize) {
        self.compact_size(n);
        for _ in 0..n {
            self.amount();
            self.amount();
            self.bytes(32 + 2 * 32 + 2 * 32 + 32 + 32 + 2 * 32);
            self.bytes(proof_len);
            self.bytes(2 * 601);
        }
        if n > 0 {
            self.bytes(32 + 64);
        }
    }

    fn sapling_v4(&mut self, spec: &Spec) {
        // A v4 transaction without spends or outputs has no Sapling bundle after parsing. The
        // upstream writer therefore emits a zero value balance. The input must stay
        // round-trippable.
        if spec.spends + spec.outputs > 0 {
            self.balance();
        } else {
            self.out.extend_from_slice(&[0u8; 8]);
        }
        self.compact_size(spec.spends);
        for _ in 0..spec.spends {
            self.jubjub_point(); // cv
            self.jubjub_base(); // anchor
            self.bytes(32); // nullifier
            self.jubjub_point(); // rk
            self.bytes(192 + 64);
        }
        self.compact_size(spec.outputs);
        for _ in 0..spec.outputs {
            self.jubjub_point(); // cv
            self.jubjub_base(); // cmu
            self.bytes(32 + 580 + 80 + 192);
        }
    }

    fn sapling_v5(&mut self, spec: &Spec) {
        self.compact_size(spec.spends);
        for _ in 0..spec.spends {
            self.jubjub_point();
            self.bytes(32);
            self.jubjub_point();
        }
        self.compact_size(spec.outputs);
        for _ in 0..spec.outputs {
            self.jubjub_point();
            self.jubjub_base();
            self.bytes(32 + 580 + 80);
        }
        if spec.spends + spec.outputs > 0 {
            self.balance();
        }
        if spec.spends > 0 {
            self.jubjub_base();
        }
        self.bytes(spec.spends * (192 + 64) + spec.outputs * 192);
        if spec.spends + spec.outputs > 0 {
            self.bytes(64);
        }
    }

    fn orchard(&mut self, n: usize, ironwood: bool, canonical_proof: bool) {
        self.compact_size(n);
        for _ in 0..n {
            self.pallas_point(); // cv_net
            self.pallas_base(); // nullifier
            self.pallas_point(); // rk
            self.pallas_base(); // cmx
            self.pallas_point(); // epk
            self.bytes(580 + 80);
        }
        if n == 0 {
            return;
        }
        let mut flags: u8 = self.rng.gen_range(0..4);
        if ironwood && self.rng.gen_bool(0.5) {
            flags |= 0b100;
        }
        self.out.push(flags);
        self.balance();
        self.pallas_base(); // anchor
        let proof_len = if canonical_proof {
            orchard::Proof::expected_proof_size(n)
        } else {
            self.rng.gen_range(0..6000)
        };
        self.compact_size(proof_len);
        self.bytes(proof_len);
        self.bytes(n * 64 + 64);
    }
}

fn generate(spec: &Spec) -> Vec<u8> {
    let mut g = Gen {
        rng: StdRng::seed_from_u64(spec.seed),
        elements: hayai_crypto::rng::seeded(spec.seed),
        out: Vec::new(),
    };
    match spec.version {
        1 | 2 => {
            g.u32(u32::from(spec.version));
            g.transparent(spec);
            g.bytes(4); // lock_time
            if spec.version == 2 {
                g.joinsplits(spec.joinsplits, 296);
            }
        }
        3 => {
            g.u32(V3_TX_VERSION | 0x8000_0000);
            g.u32(V3_VERSION_GROUP_ID);
            g.transparent(spec);
            g.bytes(8); // lock_time, expiry_height
            g.joinsplits(spec.joinsplits, 296);
        }
        4 => {
            g.u32(V4_TX_VERSION | 0x8000_0000);
            g.u32(V4_VERSION_GROUP_ID);
            g.transparent(spec);
            g.bytes(8);
            g.sapling_v4(spec);
            g.joinsplits(spec.joinsplits, 192);
            if spec.spends + spec.outputs > 0 {
                g.bytes(64);
            }
        }
        5 | 6 => {
            let (version, group) = if spec.version == 5 {
                (V5_TX_VERSION, V5_VERSION_GROUP_ID)
            } else {
                (V6_TX_VERSION, V6_VERSION_GROUP_ID)
            };
            g.u32(version | 0x8000_0000);
            g.u32(group);
            g.u32(u32::from(spec.branch));
            g.bytes(8);
            g.transparent(spec);
            g.sapling_v5(spec);
            g.orchard(spec.actions, false, spec.branch != BranchId::Nu5);
            if spec.version == 6 {
                g.orchard(spec.ironwood_actions, true, true);
            }
        }
        _ => unreachable!(),
    }
    g.out
}

fn generated_block(specs: &[Spec]) -> Vec<u8> {
    // Header: version 4, zero fields, and a zero Equihash solution of the mandatory length
    // (the header parser checks the solution length only).
    let mut out = vec![0u8; hayai_wire::header::PowParams::MAINNET.header_len()];
    out[0..4].copy_from_slice(&4u32.to_le_bytes());
    out[4 + 32 + 32 + 32 + 4 + 4 + 32..][..3].copy_from_slice(&[0xfd, 0x40, 0x05]);
    let mut g = Gen {
        rng: StdRng::seed_from_u64(0),
        elements: hayai_crypto::rng::seeded(0),
        out,
    };
    g.compact_size(specs.len());
    for spec in specs {
        g.out.extend_from_slice(&generate(spec));
    }
    g.out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn generated_transactions_of_every_version(spec in arb_spec()) {
        let bytes = generate(&spec);
        let (_, consumed) = check_tx(&bytes, spec.branch);
        prop_assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..3000)) {
        let _ = tx_wire_len(&bytes);
    }

    #[test]
    fn corrupted_transactions_never_panic(spec in arb_spec(), flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8)) {
        let mut bytes = generate(&spec);
        for (at, value) in flips {
            let i = at % bytes.len();
            bytes[i] = value;
        }
        let _ = tx_wire_len(&bytes);
        let _ = RawTx::parse(Bytes::from(bytes), spec.branch);
    }
}

#[test]
fn parallel_block_parse_matches_sequential() {
    // One v5 block and one v6 block, each above the parallel threshold, with every bundle
    // shape mixed in (the parser ignores its branch argument for v5 and v6 transactions).
    for (version, branch) in [(5u8, BranchId::Nu6_2), (6u8, BranchId::Nu6_3)] {
        let mut rng = StdRng::seed_from_u64(u64::from(version));
        let specs: Vec<Spec> = (0..PARALLEL_PARSE_THRESHOLD * 2 + 3)
            .map(|_| Spec {
                version,
                branch,
                vin: rng.gen_range(0..4),
                vout: rng.gen_range(0..4),
                joinsplits: 0,
                spends: rng.gen_range(0..3),
                outputs: rng.gen_range(0..3),
                actions: rng.gen_range(0..4),
                ironwood_actions: if version == 6 { rng.gen_range(0..4) } else { 0 },
                seed: rng.gen(),
            })
            .collect();
        let bytes = Bytes::from(generated_block(&specs));
        let parallel = RawBlock::parse(bytes.clone(), branch).unwrap();
        let sequential = RawBlock::parse_sequential(bytes.clone(), branch).unwrap();
        assert_eq!(parallel.txs.len(), specs.len());
        assert_blocks_equal(&parallel, &sequential);

        let mut trailing = bytes.to_vec();
        trailing.push(0);
        assert!(matches!(
            RawBlock::parse(Bytes::from(trailing), branch),
            Err(ParseError::Trailing)
        ));
        assert!(matches!(
            RawBlock::parse(bytes.slice(..bytes.len() - 1), branch),
            Err(ParseError::Transaction(_))
        ));
    }
}

/// Upstream `read_v4_components` replaces valueBalanceSapling by zero when a v4 transaction
/// has no spends and no outputs. The wire bytes keep it, and `RawTx` reads it back.
#[test]
fn v4_value_balance_is_recovered_from_the_wire() {
    let spec = |spends, outputs, seed| Spec {
        version: 4,
        branch: BranchId::Canopy,
        vin: 2,
        vout: 1,
        joinsplits: 0,
        spends,
        outputs,
        actions: 0,
        ironwood_actions: 0,
        seed,
    };
    let mut bytes = generate(&spec(0, 0, 1));
    // The generator wrote a zero balance. The field is the 8 bytes before the two empty
    // vector counts and the empty JoinSplit count.
    let at = bytes.len() - 3 - 8;
    assert_eq!(&bytes[at..at + 8], &[0u8; 8]);
    bytes[at..at + 8].copy_from_slice(&(-123_456i64).to_le_bytes());
    let raw = RawTx::parse(Bytes::from(bytes), BranchId::Canopy).unwrap();
    let None = raw.tx.sapling_bundle() else {
        panic!("no bundle without components");
    };
    assert_eq!(raw.v4_value_balance_without_components(), Some(-123_456));

    let with_components =
        RawTx::parse(Bytes::from(generate(&spec(1, 1, 2))), BranchId::Canopy).unwrap();
    assert_eq!(with_components.v4_value_balance_without_components(), None);
    let v5 = Spec {
        version: 5,
        branch: BranchId::Nu6_2,
        ..spec(0, 0, 3)
    };
    let v5 = RawTx::parse(Bytes::from(generate(&v5)), BranchId::Nu6_2).unwrap();
    assert_eq!(v5.v4_value_balance_without_components(), None);
}
