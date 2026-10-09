//! Shared benchmark helpers and the JSON result writer used by every bench target.
//!
//! The fixture blocks come from `hayai-fixtures`: deterministic synthetic blocks with real
//! signatures and proofs, cached under `bench-fixtures/` at the repository root.
//! Timed measurements are criterion's own output under `target/criterion/`; non-timed tables
//! (bytes on the wire) go to `bench-results/`. `scripts/collect_bench.py` and
//! `scripts/report.py` turn both into `docs/report.html`.

pub mod chain_fixture;
pub mod scenarios;
pub mod sysmetrics;
// Models without a reference crate. `chain_fixture` uses the block shape of the first.
pub mod zakura_chain_clone;
pub mod zakura_zip317;

#[cfg(feature = "baselines")]
pub mod zakura_block_layout;
#[cfg(feature = "baselines")]
pub mod zakura_utxo_layout;
#[cfg(feature = "baselines")]
pub mod zakura_wire;
#[cfg(feature = "baselines")]
pub mod zebra_block_layout;
#[cfg(feature = "baselines")]
pub mod zebra_chain_clone;
#[cfg(feature = "baselines")]
pub mod zebra_utxo_layout;
#[cfg(feature = "baselines")]
pub mod zebra_wire;
#[cfg(feature = "baselines")]
pub mod zebra_zip317;

#[cfg(feature = "mimalloc")]
pub use mimalloc;

/// Base allocator of a benchmark process: mimalloc with the `mimalloc` feature, otherwise the
/// system allocator (glibc malloc). The two implementations under comparison share it, so
/// allocator-level differences a deployment could make are not part of the comparison.
pub const ALLOCATOR: &str = if cfg!(feature = "mimalloc") {
    "mimalloc"
} else {
    "system"
};

/// Installs the `#[global_allocator]` of a criterion bench: mimalloc under the `mimalloc`
/// feature, nothing (the system allocator) otherwise. The `sysbench` binary wraps the same
/// choice in its counting allocator instead.
#[macro_export]
macro_rules! bench_allocator {
    () => {
        #[cfg(feature = "mimalloc")]
        #[global_allocator]
        static GLOBAL_ALLOCATOR: $crate::mimalloc::MiMalloc = $crate::mimalloc::MiMalloc;
    };
}

/// Criterion function id of a hayai-side measurement: `hayai` on the upstream backend,
/// `hayai-zk` on the zakura backend, then `-<variant>` when `variant` is not empty.
pub fn hayai_id(variant: &str) -> String {
    let base = format!("hayai{}", hayai_crypto::BACKEND_SUFFIX);
    if variant.is_empty() {
        base
    } else {
        format!("{base}-{variant}")
    }
}

/// Criterion function id of the backend library's own routine measured as a baseline:
/// `upstream` on the upstream backend, `zakura-lib` on the zakura backend (where the hayai
/// side is built on the same forks as the `zakura` baselines).
pub fn library_id() -> &'static str {
    if hayai_crypto::BACKEND_SUFFIX.is_empty() {
        "upstream"
    } else {
        "zakura-lib"
    }
}

/// A fresh scratch directory under the workspace `target/` directory (never `/tmp`), for
/// benchmark and test databases.
pub fn scratch_dir() -> tempfile::TempDir {
    let base =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-scratch");
    std::fs::create_dir_all(&base).expect("create scratch base");
    tempfile::tempdir_in(base).expect("create scratch dir")
}

/// The `bench-results/` directory at the repository root, for non-timed measurements.
pub fn results_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench-results");
    std::fs::create_dir_all(&dir).expect("create bench-results");
    dir
}
