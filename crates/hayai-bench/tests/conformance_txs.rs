//! Transaction conformance: published transaction-level vectors through hayai's prepare
//! path (`hayai_prepared::draft`, `Draft::check_input`).
//!
//! - The script vectors of `zcash_script` 0.6 (`test_vectors.rs`, the port of zcashd's
//!   `script_tests.json`): each case is a scriptSig, a scriptPubKey, the verification flags
//!   and the expected result. The test spends a coin with the scriptPubKey in a transaction
//!   with the scriptSig and evaluates the input with `Draft::check_input`. It also reads the
//!   sigop count of the scriptPubKey from a prepared transaction.
//! - The transactions of the ZIP 143, ZIP 243 and ZIP 244 vectors (zcash-test-vectors):
//!   `draft` with the spent coins of the vector, against
//!   `tests/vectors/expected-txs.json`. hayai-wire tests the parse, the transaction id and
//!   the authorizing digest of the same transactions (`hayai-wire/tests/scan.rs`). The
//!   sighash values of the vectors are not tested: `docs/conformance.md`.

#[path = "conformance/expected.rs"]
mod expected;

use std::path::PathBuf;

use bytes::Bytes;
use hayai_coins::Coin;
use hayai_crypto::zcash_encoding::CompactSize;
use hayai_crypto::zcash_protocol::consensus::BranchId;
use hayai_prepared::{draft, Draft, PrepareError, RuleEpoch};
use hayai_wire::RawTx;
use serde::{Deserialize, Serialize};
use zcash_script::interpreter::{CallbackTransactionSignatureChecker, Flags};
use zcash_script::script::{Code, Raw};
use zcash_script::test_vectors::test_vectors;
use zcash_script::testing::missing_sighash;

use expected::{Outcome, Verdict};

/// A v4 transaction with one transparent input and one transparent output, in wire bytes.
/// The lock time is 0 and the input sequence is not final, as the script vectors assume.
fn v4_transaction(script_sig: &[u8], output_script: &[u8]) -> RawTx {
    let mut bytes = Vec::new();
    // Header and version group id of a Sapling v4 transaction.
    bytes.extend_from_slice(&0x8000_0004u32.to_le_bytes());
    bytes.extend_from_slice(&0x892f_2085u32.to_le_bytes());
    // One input: outpoint, scriptSig, sequence.
    bytes.push(1);
    bytes.extend_from_slice(&[0x11; 32]);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    CompactSize::write(&mut bytes, script_sig.len()).expect("write to a vector");
    bytes.extend_from_slice(script_sig);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    // One output of value 0.
    bytes.push(1);
    bytes.extend_from_slice(&0u64.to_le_bytes());
    CompactSize::write(&mut bytes, output_script.len()).expect("write to a vector");
    bytes.extend_from_slice(output_script);
    // Lock time, expiry height, valueBalanceSapling, and no spend, output or JoinSplit.
    bytes.extend_from_slice(&[0; 4 + 4 + 8 + 3]);
    RawTx::parse(Bytes::from(bytes), BranchId::Canopy).expect("the transaction parses")
}

/// The draft of a transaction that spends a coin of value 0 with `script_pubkey`.
fn spend(script_sig: &[u8], script_pubkey: &[u8], output_script: &[u8], flags: Flags) -> Draft {
    let epoch = RuleEpoch {
        branch_id: BranchId::Canopy,
        script_flags: flags,
    };
    let coin = Coin {
        value: 0,
        script_pubkey: Bytes::copy_from_slice(script_pubkey),
        height: 1,
        is_coinbase: false,
    };
    draft(v4_transaction(script_sig, output_script), epoch, vec![coin])
        .expect("the structural rules accept the transaction")
}

/// Every case of the `zcash_script` vectors gives the published result through
/// `Draft::check_input`, and the published sigop count through `draft`.
///
/// `check_input` reports an error as text, so the test evaluates each case with the
/// interpreter of the crate too (with no sighash, as the tests of the crate do), compares the
/// two results, and gives the typed result to the vector for the comparison with the
/// published one. No signature of the vectors is valid for the transaction of the test, so
/// a signature check fails in both evaluations.
#[test]
fn script_vectors_through_check_input() {
    let vectors = test_vectors();
    let mut failures = Vec::new();
    for (i, vector) in vectors.iter().enumerate() {
        let through_hayai = |script: &Raw, flags: Flags| {
            let checker = CallbackTransactionSignatureChecker {
                sighash: &missing_sighash,
                lock_time: 0,
                is_final: false,
            };
            let reference = script.eval(flags, &checker);
            let hayai = spend(&script.sig.0, &script.pub_key.0, &[], flags).check_input(0);
            let reference_as_hayai = match &reference {
                Ok(true) => Ok(()),
                Ok(false) => Err(PrepareError::Script(0, "evaluated to false".into())),
                Err((component, error)) => {
                    Err(PrepareError::Script(0, format!("{component:?}: {error:?}")))
                }
            };
            assert_eq!(hayai, reference_as_hayai, "case {i}: {vector:?}");
            reference.map_err(|(component, error)| (Some(component), error))
        };
        // The sigop count of a scriptPubKey, as hayai counts the output scripts of a
        // transaction. The input of the transaction has an empty scriptSig and spends a
        // coin with an empty script, so no other script adds to the count.
        let sigops = |script_pubkey: &Code| {
            spend(&[], &[], &script_pubkey.0, Flags::empty())
                .tx()
                .sigops
        };
        if let Err((result, count)) = vector.run(&through_hayai, &sigops) {
            failures.push(format!(
                "case {i}: {vector:?}\n  actual: {result:?}, {count} sigops"
            ));
        }
    }
    assert!(vectors.len() > 1_000, "{} script vectors", vectors.len());
    assert!(
        failures.is_empty(),
        "{} of {} script vectors differ:\n{}",
        failures.len(),
        vectors.len(),
        failures.join("\n")
    );
}

/// The spent coin of a ZIP 143 or ZIP 243 vector. The vector holds the coin of the signed
/// input only.
#[derive(Deserialize)]
struct V4Inputs {
    script_code: String,
    transparent_input: Option<usize>,
    amount: u64,
    branch: String,
    tx_bytes: usize,
}

/// The spent coins of a ZIP 244 vector: one for each transparent input.
#[derive(Deserialize)]
struct V5Inputs {
    amounts: Vec<u64>,
    script_pubkeys: Vec<String>,
    tx_bytes: usize,
}

/// `tests/vectors/tx-sighash-inputs.json`: the inputs of the sighash vectors of
/// `zcash_primitives` 0.30.1 (`src/transaction/tests/data.rs`), without the transactions.
/// The transactions are in `hayai-wire/tests/vectors/tx-zip0*.hex`, in the same order.
#[derive(Deserialize)]
struct SighashInputs {
    zip0143: Vec<V4Inputs>,
    zip0243: Vec<V4Inputs>,
    zip0244: Vec<V5Inputs>,
}

/// One vector transaction and the coins it spends, when the vector holds all of them.
struct VectorTx {
    name: String,
    branch: BranchId,
    bytes: Vec<u8>,
    coins: Vec<(u64, String)>,
    /// Transparent inputs whose coin the vector holds.
    coins_complete: bool,
}

/// The result of one vector transaction. `outcome` is absent when `draft` did not run.
#[derive(Serialize)]
struct TxResult {
    vector: String,
    outcome: Option<Outcome>,
    note: Option<&'static str>,
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The transactions of `hayai-wire/tests/vectors/<file>`: the first field of each line.
fn wire_transactions(file: &str) -> Vec<Vec<u8>> {
    let path = manifest_dir()
        .join("../hayai-wire/tests/vectors")
        .join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|field| hex::decode(field).expect("a transaction is hex"))
        .collect()
}

/// Number of transparent inputs of a transaction whose input count is at `offset` and
/// fits one byte.
fn input_count(bytes: &[u8], offset: usize) -> usize {
    assert!(bytes[offset] < 0xfd, "the input count fits one byte");
    usize::from(bytes[offset])
}

fn vector_transactions() -> Vec<VectorTx> {
    let path = manifest_dir().join("tests/vectors/tx-sighash-inputs.json");
    let text = std::fs::read_to_string(&path).expect("read tx-sighash-inputs.json");
    let inputs: SighashInputs = serde_json::from_str(&text).expect("parse tx-sighash-inputs.json");
    let mut txs = Vec::new();
    let v4_sets = [
        ("zip0143", "tx-zip0143.hex", &inputs.zip0143),
        ("zip0243", "tx-zip0243.hex", &inputs.zip0243),
    ];
    for (set, file, vectors) in v4_sets {
        let transactions = wire_transactions(file);
        assert_eq!(transactions.len(), vectors.len(), "{set}");
        for (i, (bytes, vector)) in transactions.into_iter().zip(vectors).enumerate() {
            assert_eq!(
                bytes.len(),
                vector.tx_bytes,
                "{set}-{i}: another transaction"
            );
            let branch = match vector.branch.as_str() {
                "overwinter" => BranchId::Overwinter,
                "sapling" => BranchId::Sapling,
                other => panic!("{set}-{i}: unknown branch {other}"),
            };
            // The header and the version group id come before the input count.
            let inputs = input_count(&bytes, 8);
            let coins: Vec<(u64, String)> = match (inputs, vector.transparent_input) {
                (1, Some(0)) => vec![(vector.amount, vector.script_code.clone())],
                _ => Vec::new(),
            };
            txs.push(VectorTx {
                name: format!("{set}-{i}"),
                branch,
                bytes,
                coins_complete: coins.len() == inputs,
                coins,
            });
        }
    }
    let transactions = wire_transactions("tx-zip0244.hex");
    assert_eq!(transactions.len(), inputs.zip0244.len(), "zip0244");
    for (i, (bytes, vector)) in transactions.into_iter().zip(inputs.zip0244).enumerate() {
        assert_eq!(
            bytes.len(),
            vector.tx_bytes,
            "zip0244-{i}: another transaction"
        );
        txs.push(VectorTx {
            name: format!("zip0244-{i}"),
            branch: BranchId::Nu5,
            bytes,
            coins: vector
                .amounts
                .into_iter()
                .zip(vector.script_pubkeys)
                .collect(),
            // A ZIP 244 vector holds the coin of every input. A coinbase has none.
            coins_complete: true,
        });
    }
    txs
}

/// `draft` on every vector transaction whose spent coins the vector holds.
///
/// The transactions are generated: they test digests, not validity. A rejection is an
/// outcome to record, not a defect.
#[test]
fn sighash_vector_transactions_through_draft() {
    let mut results = Vec::new();
    for vector in vector_transactions() {
        let outcome = if vector.coins_complete {
            let epoch = RuleEpoch::consensus(vector.branch);
            let raw = RawTx::parse(Bytes::from(vector.bytes), vector.branch)
                .unwrap_or_else(|e| panic!("{}: {e}", vector.name));
            let coins = vector
                .coins
                .iter()
                .map(|(value, script)| Coin {
                    value: *value,
                    script_pubkey: Bytes::from(hex::decode(script).expect("a script is hex")),
                    height: 1,
                    is_coinbase: false,
                })
                .collect();
            let (verdict, error) = match draft(raw, epoch, coins) {
                Ok(_) => (Verdict::Valid, None),
                Err(e @ PrepareError::Unsupported(_)) => (Verdict::Unsupported, Some(e)),
                Err(e) => (Verdict::Rejected, Some(e)),
            };
            Some(Outcome {
                stage: "draft".to_string(),
                verdict,
                error: error.map(|e| e.to_string()),
            })
        } else {
            None
        };
        let note = match outcome {
            Some(_) => None,
            None => Some("not run: the vector holds the spent coin of the signed input only"),
        };
        results.push(TxResult {
            vector: vector.name,
            outcome,
            note,
        });
    }

    let path = expected::results_dir().join("txs.results.json");
    let mut text = serde_json::to_string_pretty(&results).expect("serialize the results");
    text.push('\n');
    std::fs::write(&path, text).expect("write the results");

    let count = |v: Option<Verdict>| {
        results
            .iter()
            .filter(|r| r.outcome.as_ref().map(|o| o.verdict) == v)
            .count()
    };
    println!(
        "sighash vector transactions: {}\naccepted by draft: {}\nunsupported: {}\n\
         rejected: {}\nnot run: {}\nresults: {}",
        results.len(),
        count(Some(Verdict::Valid)),
        count(Some(Verdict::Unsupported)),
        count(Some(Verdict::Rejected)),
        count(None),
        path.display()
    );

    let actual: Vec<(String, Outcome)> = results
        .into_iter()
        .filter_map(|r| r.outcome.map(|outcome| (r.vector, outcome)))
        .collect();
    let differences = expected::differences("expected-txs.json", &actual);
    assert!(
        differences.is_empty(),
        "outcomes differ from tests/vectors/expected-txs.json:\n{}",
        differences.join("\n")
    );
}
