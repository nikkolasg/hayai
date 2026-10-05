//! The outcome of one conformance vector and the expected-outcome files.
//!
//! An expected-outcome file (`tests/vectors/expected-*.json`) lists, for each vector, the
//! stage hayai reaches, the verdict and the error today. An entry that is not `valid` also
//! gives the reason and the plan item that changes it (`docs/plan-consensus-and-sync.md`).
//! A test fails when an actual outcome differs from the file, in either direction. A work
//! item that makes a vector pass must update the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What hayai decided about a vector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every stage passed.
    Valid,
    /// Every context-free stage passed. The contextual stage needs state that the vector
    /// set cannot supply.
    ContextFree,
    /// hayai stopped with an `Unsupported` error: the rule is not implemented.
    Unsupported,
    /// hayai rejected the vector.
    Rejected,
}

/// The stage reached, the verdict, and the error when the verdict is not `valid`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The stage that stopped the vector, or the last stage when every stage passed.
    pub stage: String,
    pub verdict: Verdict,
    pub error: Option<String>,
}

/// One entry of an expected-outcome file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expected {
    pub vector: String,
    #[serde(flatten)]
    pub outcome: Outcome,
    /// Why the vector is not `valid` today.
    pub reason: Option<String>,
    /// The plan item that changes the outcome.
    pub item: Option<String>,
}

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

/// The directory of the machine-readable results: `target/conformance/` of the workspace.
pub fn results_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance");
    std::fs::create_dir_all(&dir).expect("create the results directory");
    dir
}

/// Reads the expected-outcome file `file` of `tests/vectors/`.
fn load(file: &str) -> Vec<Expected> {
    let path = vectors_dir().join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Compares `actual` (vector name and outcome, in run order) with the expected-outcome file
/// `file`. Returns one line for each difference.
///
/// The function also writes the actual outcomes to `target/conformance/<file>` in the format
/// of the expected file. An entry whose outcome did not change keeps its reason and its plan
/// item. After a review, that file replaces the expected file.
pub fn differences(file: &str, actual: &[(String, Outcome)]) -> Vec<String> {
    let expected = load(file);
    let by_name: BTreeMap<&str, &Expected> =
        expected.iter().map(|e| (e.vector.as_str(), e)).collect();
    let mut lines = Vec::new();
    if by_name.len() != expected.len() {
        lines.push(format!("{file}: a vector has more than one entry"));
    }
    let mut next = Vec::with_capacity(actual.len());
    for (vector, outcome) in actual {
        let known = by_name.get(vector.as_str()).copied();
        let unchanged = known.filter(|e| e.outcome == *outcome);
        match (known, unchanged) {
            (None, _) => lines.push(format!("{vector}: no entry in {file}; actual {outcome:?}")),
            (Some(e), None) => lines.push(format!(
                "{vector}: expected {:?}, actual {outcome:?}",
                e.outcome
            )),
            (Some(e), Some(_)) => {
                let annotated = matches!((&e.reason, &e.item), (Some(_), Some(_)));
                if outcome.verdict != Verdict::Valid && !annotated {
                    lines.push(format!(
                        "{vector}: the entry needs a reason and a plan item"
                    ));
                }
            }
        }
        next.push(Expected {
            vector: vector.clone(),
            outcome: outcome.clone(),
            reason: unchanged.and_then(|e| e.reason.clone()),
            item: unchanged.and_then(|e| e.item.clone()),
        });
    }
    for e in &expected {
        if !actual.iter().any(|(vector, _)| *vector == e.vector) {
            lines.push(format!(
                "{}: in {file}, but the vector did not run",
                e.vector
            ));
        }
    }
    let path = results_dir().join(file);
    let mut text = serde_json::to_string_pretty(&next).expect("serialize the outcomes");
    text.push('\n');
    std::fs::write(&path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    lines
}
