//! The fuzz loop: cases in parallel, the counts of a run, the minimisation of a finding
//! and its case file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::classes::Class;
use crate::mutate::{build, Case, Recipe};
use crate::rng::{case_seed, Rng};
use crate::seeds::SeedError;
use crate::verdict::{compare, Comparison, RuleClass, Verdict};
use crate::{hayai_side, reference};

/// Builds the verifying keys of both implementations and the fixture seeds, and silences the panic messages of
/// the cases. A caller that is not a rayon worker calls it once before the first case.
pub fn init() {
    hayai_side::build_keys();
    reference::build_keys();
    crate::seeds::build_fixtures();
    // A case can panic in one implementation. The verdict holds the text.
    crate::quiet_guarded_panics();
}

/// The two verdicts of a case and their comparison.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub hayai: Verdict,
    pub reference: Verdict,
    pub comparison: Comparison,
    /// The name of the known difference that explains a finding. A case with a known
    /// difference is counted, and it is not written as a finding.
    pub known: Option<String>,
}

pub fn check(case: &Case) -> Outcome {
    let hayai = hayai_side::check_block(&case.bytes, &case.ctx);
    let reference = reference::check_block(&case.bytes, &case.ctx);
    let comparison = compare(&hayai, &reference);
    let known = if comparison.is_finding() {
        crate::known::known_difference(case, &hayai, &reference).map(str::to_string)
    } else {
        None
    };
    Outcome {
        hayai,
        reference,
        comparison,
        known,
    }
}

/// The outcome of the recipe. `Err`: the recipe names a seed that does not exist.
pub fn check_recipe(recipe: &Recipe) -> Result<Outcome, SeedError> {
    Ok(check(&build(recipe)?))
}

/// A finding as its case file holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding {
    pub class: String,
    /// The seed of the case: `hayai-fuzz --class <class> --case-seed <case_seed>` makes
    /// the recipe before the minimisation again.
    pub case_seed: u64,
    /// The recipe after the minimisation. A finding of a header class has none.
    pub recipe: Option<Recipe>,
    /// The case of a header class, as text.
    pub header_case: Option<String>,
    pub outcome: Outcome,
    /// SHA-256 of the block of the recipe.
    pub block_sha256: String,
    pub block_bytes: usize,
}

/// The classes of a reject pair, or the kind of the verdict.
fn verdict_key(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Accept => "accept".to_string(),
        Verdict::Reject { class, .. } => format!("reject:{class:?}"),
        Verdict::Panic(_) => "panic".to_string(),
        Verdict::NotCovered(_) => "not-covered".to_string(),
    }
}

/// The counts of the cases of one class.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClassStats {
    pub cases: u64,
    pub comparisons: BTreeMap<Comparison, u64>,
    /// Cases by the pair of the verdict of hayai and the verdict of the reference.
    pub pairs: BTreeMap<String, u64>,
    /// Findings that a known difference explains, by the name of the difference.
    pub known: BTreeMap<String, u64>,
    /// Findings without a known difference.
    pub findings: u64,
    pub seconds: f64,
}

impl ClassStats {
    pub fn record(&mut self, outcome: &Outcome) {
        self.cases += 1;
        *self.comparisons.entry(outcome.comparison).or_default() += 1;
        let pair = format!(
            "{} / {}",
            verdict_key(&outcome.hayai),
            verdict_key(&outcome.reference)
        );
        *self.pairs.entry(pair).or_default() += 1;
        if outcome.comparison.is_finding() {
            match &outcome.known {
                Some(name) => *self.known.entry(name.clone()).or_default() += 1,
                None => self.findings += 1,
            }
        }
    }
}

/// The result of one case of a batch.
struct CaseResult {
    case_seed: u64,
    recipe: Recipe,
    outcome: Outcome,
}

fn run_case(class: Class, seed: u64) -> CaseResult {
    let mut rng = Rng::new(seed);
    let recipe = class.recipe(&mut rng);
    let outcome = check_recipe(&recipe).expect("a class makes a recipe with a seed that exists");
    CaseResult {
        case_seed: seed,
        recipe,
        outcome,
    }
}

/// The recipe of the case `case_seed` of `class`.
pub fn recipe_of(class: Class, case_seed: u64) -> Recipe {
    class.recipe(&mut Rng::new(case_seed))
}

/// The signature of a finding: findings with the same signature are one defect in most
/// cases, so the run minimises and writes a limited number of each.
fn signature(class: Class, outcome: &Outcome) -> String {
    let class_of = |verdict: &Verdict| match verdict {
        Verdict::Reject { class, .. } => Some(*class),
        _ => None,
    };
    format!(
        "{}:{:?}:{:?}:{:?}",
        class.name(),
        outcome.comparison,
        class_of(&outcome.hayai),
        class_of(&outcome.reference)
    )
}

/// Removes every operation and flag of `recipe` that the finding does not need: the
/// result has the same comparison.
pub fn minimise(recipe: &Recipe, comparison: Comparison) -> Recipe {
    let still = |candidate: &Recipe| {
        matches!(
            check_recipe(candidate),
            Ok(Outcome { comparison: found, known: None, .. }) if found == comparison
        )
    };
    let mut best = recipe.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for index in (0..best.ops.len()).rev() {
            let mut candidate = best.clone();
            candidate.ops.remove(index);
            if still(&candidate) {
                best = candidate;
                changed = true;
            }
        }
        for index in (0..best.raw.len()).rev() {
            let mut candidate = best.clone();
            candidate.raw.remove(index);
            if still(&candidate) {
                best = candidate;
                changed = true;
            }
        }
    }
    best
}

fn hex_sha256(bytes: &[u8]) -> String {
    hex::encode(crate::seeds::sha256(&[bytes]))
}

/// The finding of the case `case_seed` of `class` with the recipe `recipe`, minimised.
pub fn finding(class: Class, case_seed: u64, recipe: &Recipe, comparison: Comparison) -> Finding {
    let recipe = minimise(recipe, comparison);
    let case = build(&recipe).expect("the seed of a finding exists");
    Finding {
        class: class.name().to_string(),
        case_seed,
        outcome: check(&case),
        block_sha256: hex_sha256(&case.bytes),
        block_bytes: case.bytes.len(),
        recipe: Some(recipe),
        header_case: None,
    }
}

/// What a run does.
#[derive(Clone, Debug)]
pub struct Plan {
    pub seed: u64,
    pub classes: Vec<Class>,
    /// Cases for each class. With `seconds`, the run stops at the first of the two limits.
    pub iterations: Option<u64>,
    /// Wall time for each class.
    pub seconds: Option<u64>,
    /// The directory of the case files. `None`: the run writes no file.
    pub out_dir: Option<PathBuf>,
    /// Findings with the same signature that the run minimises and writes.
    pub per_signature: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub seed: u64,
    pub classes: BTreeMap<String, ClassStats>,
    pub findings: Vec<Finding>,
}

/// Cases of one batch: enough for every core, small enough for a time limit to hold.
const BATCH: u64 = 512;

/// Runs the plan on the rayon pool. [`init`] must run before.
pub fn run(plan: &Plan) -> Report {
    let mut report = Report {
        seed: plan.seed,
        ..Report::default()
    };
    for &class in &plan.classes {
        let started = Instant::now();
        let deadline = plan.seconds.map(|s| started + Duration::from_secs(s));
        let mut stats = ClassStats::default();
        let mut kept: BTreeMap<String, usize> = BTreeMap::new();
        let mut next = 0u64;
        loop {
            let end = match plan.iterations {
                Some(limit) => (next + BATCH).min(limit),
                None => next + BATCH,
            };
            if next >= end || matches!(deadline, Some(deadline) if Instant::now() >= deadline) {
                break;
            }
            let results: Vec<CaseResult> = (next..end)
                .into_par_iter()
                .map(|index| run_case(class, case_seed(plan.seed, class.name(), index)))
                .collect();
            next = end;
            for result in results {
                stats.record(&result.outcome);
                let (true, None) = (
                    result.outcome.comparison.is_finding(),
                    &result.outcome.known,
                ) else {
                    continue;
                };
                let count = kept.entry(signature(class, &result.outcome)).or_default();
                if *count >= plan.per_signature {
                    continue;
                }
                *count += 1;
                let finding = finding(
                    class,
                    result.case_seed,
                    &result.recipe,
                    result.outcome.comparison,
                );
                if let Some(dir) = &plan.out_dir {
                    write_finding(dir, &finding);
                }
                report.findings.push(finding);
            }
        }
        stats.seconds = started.elapsed().as_secs_f64();
        report.classes.insert(class.name().to_string(), stats);
    }
    report
}

/// Writes the case file of `finding` into `dir` and returns its path.
pub fn write_finding(dir: &Path, finding: &Finding) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create the findings directory");
    let path = dir.join(format!("{}-{:016x}.json", finding.class, finding.case_seed));
    let json = serde_json::to_string_pretty(finding).expect("a finding is JSON");
    std::fs::write(&path, json).expect("write the case file");
    path
}

/// The default directory of the case files: `target/fuzz/findings` of the workspace.
pub fn default_out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fuzz/findings")
}

/// The rule classes of the two verdicts of an outcome, for a test.
pub fn reject_classes(outcome: &Outcome) -> (Option<RuleClass>, Option<RuleClass>) {
    let class_of = |verdict: &Verdict| match verdict {
        Verdict::Reject { class, .. } => Some(*class),
        _ => None,
    };
    (class_of(&outcome.hayai), class_of(&outcome.reference))
}
