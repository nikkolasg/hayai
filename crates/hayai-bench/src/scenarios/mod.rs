//! The benchmark scenarios as functions: the bodies the criterion benches time (each bench
//! calls these so there is one copy of every scenario) and, for the `sysbench` binary, a
//! catalogue of (scenario, parameter, implementation) triples that build a repeatable
//! timed step.
//!
//! A step runs one iteration: untimed per-iteration preparation, then the timed body inside
//! [`Meter::timed`], then checks on the result. The binary runs one warm-up step and then
//! `iterations` recorded steps in a fresh child process per triple.

use std::fmt;
use std::path::PathBuf;

use crate::sysmetrics::Meter;

// The scenarios below with the `baselines` feature run the zakura-* crates or a layout
// that uses their types. `state`, `template` and `validate` have models without a reference
// crate, and `chain_fixture` uses the synthetic chain of `state`.
#[cfg(feature = "baselines")]
pub mod coins;
#[cfg(feature = "baselines")]
pub mod relay;
pub mod state;
pub mod template;
#[cfg(feature = "baselines")]
pub mod trees;
pub mod validate;
#[cfg(feature = "baselines")]
pub mod wire;

/// Which implementation of a scenario runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impl {
    Hayai,
    Zakura,
    /// Upstream Zebra. Only the scenarios whose catalogue entry lists it have it.
    Zebra,
}

/// The message of the `Impl::Zebra` arms that [`build`] never reaches: it refuses a Zebra
/// triple that the catalogue does not list.
pub(crate) const NO_ZEBRA: &str = "scenarios::build refuses a Zebra triple outside the catalogue";

impl Impl {
    /// The implementations of every scenario.
    pub const ALL: [Impl; 2] = [Impl::Hayai, Impl::Zakura];

    pub fn as_str(self) -> &'static str {
        match self {
            Impl::Hayai => "hayai",
            Impl::Zakura => "zakura",
            Impl::Zebra => "zebra",
        }
    }

    pub fn parse(s: &str) -> Result<Impl, String> {
        match s {
            "hayai" => Ok(Impl::Hayai),
            "zakura" => Ok(Impl::Zakura),
            "zebra" => Ok(Impl::Zebra),
            other => Err(format!("unknown impl {other:?}: hayai, zakura or zebra")),
        }
    }
}

impl fmt::Display for Impl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of the catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub name: &'static str,
    pub param: String,
    /// The implementations that exist for this (name, param).
    pub impls: Vec<Impl>,
}

/// A built scenario: the step to repeat, and the directory whose growth is the scenario's
/// disk footprint (none for in-memory scenarios).
pub struct Built {
    step: Box<dyn FnMut(&mut Meter)>,
    pub scratch: Option<PathBuf>,
}

impl Built {
    pub fn new(step: impl FnMut(&mut Meter) + 'static) -> Built {
        Built {
            step: Box::new(step),
            scratch: None,
        }
    }

    #[cfg(feature = "baselines")]
    fn with_scratch(mut self, dir: PathBuf) -> Built {
        self.scratch = Some(dir);
        self
    }

    /// Runs one iteration.
    pub fn step(&mut self, meter: &mut Meter) {
        (self.step)(meter)
    }
}

#[cfg(feature = "baselines")]
const PARSE_FIXTURES: [&str; 2] = ["transparent-6500x1", "orchard-165x2"];
#[cfg(feature = "baselines")]
const VALIDATE_FIXTURES: [&str; 3] = ["transparent-6500x1", "orchard-165x2", "mixed-2000x1-100x2"];
#[cfg(feature = "baselines")]
const RELAY_FIXTURES: [&str; 3] = ["transparent-2000x2", "orchard-200x2", "mixed-1000x2-100x2"];

/// Every (scenario, parameter) the binary runs with `--all`, with the implementations each
/// one has.
#[cfg(feature = "baselines")]
pub fn catalogue() -> Vec<Spec> {
    let both = Impl::ALL.to_vec();
    let mut out = Vec::new();
    for f in PARSE_FIXTURES {
        out.push(Spec {
            name: "parse_block",
            param: f.to_string(),
            impls: both.clone(),
        });
    }
    for f in VALIDATE_FIXTURES {
        // The Zakura and Zebra models exist for cold validation of transparent blocks only.
        let modelled = f.starts_with("transparent-");
        out.push(Spec {
            name: "validate_block_cold",
            param: f.to_string(),
            impls: if modelled {
                vec![Impl::Hayai, Impl::Zakura, Impl::Zebra]
            } else {
                vec![Impl::Hayai]
            },
        });
        out.push(Spec {
            name: "validate_block_warm",
            param: f.to_string(),
            impls: vec![Impl::Hayai],
        });
    }
    for name in ["coins_lookup_13000", "coins_commit_13000"] {
        out.push(Spec {
            name,
            param: "13000".to_string(),
            impls: both.clone(),
        });
    }
    out.push(Spec {
        name: "state_push_1000_window",
        param: "1000".to_string(),
        impls: both.clone(),
    });
    out.push(Spec {
        name: "tree_append_2048",
        param: "2048".to_string(),
        impls: both.clone(),
    });
    out.push(Spec {
        name: "template_build_8000",
        param: "8000".to_string(),
        impls: both.clone(),
    });
    for f in RELAY_FIXTURES {
        out.push(Spec {
            name: "relay_reconstruct",
            param: f.to_string(),
            impls: both.clone(),
        });
    }
    out
}

/// Builds the step of one catalogue triple; an error names an unknown scenario or an
/// implementation the scenario does not have.
#[cfg(feature = "baselines")]
pub fn build(name: &str, param: &str, imp: Impl) -> Result<Built, String> {
    if let Impl::Zebra = imp {
        let listed = catalogue()
            .iter()
            .any(|s| s.name == name && s.param == param && s.impls.contains(&imp));
        if !listed {
            return Err(format!("no zebra implementation of {name} {param}"));
        }
    }
    match name {
        "parse_block" => wire::build(param, imp),
        "validate_block_cold" => validate::build(param, false, imp),
        "validate_block_warm" => validate::build(param, true, imp),
        "coins_lookup_13000" => Ok(coins::build_lookup(imp)),
        "coins_commit_13000" => Ok(coins::build_commit(imp)),
        "state_push_1000_window" => Ok(state::build_push(1_000, imp)),
        "tree_append_2048" => Ok(trees::build_append(2048, imp)),
        "template_build_8000" => Ok(template::build_from_scratch(8_000, imp)),
        "relay_reconstruct" => relay::build_reconstruct(param, imp),
        other => Err(format!("unknown scenario {other:?}")),
    }
}

#[cfg(all(test, feature = "baselines"))]
mod tests {
    use super::*;

    #[test]
    fn catalogue_entries_are_distinct_and_unknown_names_are_errors() {
        let specs = catalogue();
        for (i, s) in specs.iter().enumerate() {
            assert!(!s.impls.is_empty(), "{s:?}");
            assert!(
                !specs[..i]
                    .iter()
                    .any(|t| t.name == s.name && t.param == s.param),
                "duplicate {s:?}"
            );
        }
        let Err(e) = build("nothing", "", Impl::Hayai) else {
            panic!("unknown scenario is an error");
        };
        assert!(e.contains("unknown scenario"));
        let Err(e) = build("parse_block", "no-such-fixture", Impl::Hayai) else {
            panic!("unknown fixture is an error");
        };
        assert!(e.contains("no fixture"), "{e}");
    }

    #[test]
    fn zakura_model_is_refused_for_shielded_and_warm_validation() {
        let Err(e) = validate::build("orchard-165x2", false, Impl::Zakura) else {
            panic!("shielded fixture has no Zakura model");
        };
        assert!(e.contains("transparent"), "{e}");
    }

    #[test]
    fn zebra_exists_only_where_the_catalogue_lists_it() {
        let Err(e) = build("validate_block_cold", "orchard-165x2", Impl::Zebra) else {
            panic!("shielded fixture has no Zebra model");
        };
        assert!(e.contains("no zebra implementation"), "{e}");
        let Err(e) = build("coins_lookup_13000", "13000", Impl::Zebra) else {
            panic!("coins_lookup has no Zebra implementation");
        };
        assert!(e.contains("no zebra implementation"), "{e}");
        let Err(e) = validate::build("transparent-6500x1", true, Impl::Zebra) else {
            panic!("warm validation has no Zebra model");
        };
        assert!(e.contains("transparent"), "{e}");
    }
}
