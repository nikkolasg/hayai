//! In-process differential fuzzer of hayai against the Zakura library code.
//!
//! A case is a block and a chain context. The fuzzer makes a case from a seed block (a
//! generated fixture of hayai-fixtures, or a generated transparent block at a chosen height)
//! and a list of mutations ([`mutate::Recipe`]). hayai ([`hayai_side`]) and the reference
//! ([`reference`]) each give a verdict, and [`verdict::compare`] compares the two.
//!
//! Every case comes from a 64-bit case seed and a class name, so a finding is the pair,
//! and its case file holds the recipe that the minimisation kept. `docs/conformance.md`
//! (Differential fuzzer) has the coverage of the oracle. `docs/fuzz-findings.md` has the
//! findings.

pub mod classes;
pub mod context;
pub mod hayai_side;
pub mod header;
pub mod known;
pub mod model;
pub mod mutate;
pub mod reference;
pub mod rng;
pub mod run;
pub mod script;
pub mod seeds;
pub mod verdict;

/// The text of a caught panic.
fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic without a text".to_string()
    }
}

thread_local! {
    /// Whether this thread runs an implementation under [`guarded`].
    static GUARDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the panic hook print no message for a panic under [`guarded`] on the thread of
/// the call. Every other panic prints as before: it is an error of the fuzzer.
pub fn quiet_guarded_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !GUARDED.with(|guarded| guarded.get()) {
            default(info);
        }
    }));
}

/// Runs `f` and returns the text of its panic as the error.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    let before = GUARDED.with(|guarded| guarded.replace(true));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    GUARDED.with(|guarded| guarded.set(before));
    outcome.map_err(|panic| panic_text(&panic))
}
