//! The cryptography backend of the workspace, behind one set of stable names.
//!
//! Every other crate reaches the Zcash protocol and curve crates through this facade
//! (`use hayai_crypto::orchard;`), so the backend is chosen once, by feature:
//!
//! - `upstream` (default): the upstream Zcash crates (`orchard`, `zcash_primitives`,
//!   `pasta_curves`, `halo2_proofs`, ff/group 0.13, rand 0.8);
//! - `zakura`: the `zakura-*` forks from zakura-core/common, which keep the upstream module
//!   paths but build on ff/group 0.14 and rand 0.10.
//!
//! The two are mutually exclusive. Crates that are identical under both backends
//! (`zcash_encoding`, `zcash_script`, `subtle`, `incrementalmerkletree`, `ed25519_zebra`,
//! `zcash_note_encryption`, `zcash_history` and its `primitive_types`) are re-exported unconditionally. The RNG types a backend's APIs
//! accept differ between the two rand lines; [`rng`] names them uniformly.

#![forbid(unsafe_code)]

#[cfg(all(feature = "upstream", feature = "zakura"))]
compile_error!("hayai-crypto: features `upstream` and `zakura` are mutually exclusive");

#[cfg(not(any(feature = "upstream", feature = "zakura")))]
compile_error!("hayai-crypto: one of the features `upstream` or `zakura` must be enabled");

/// Ed25519 with the ZIP 215 rules (the same crate under both backends).
pub use ed25519_zebra;
pub use incrementalmerkletree;
/// The integer type of `zcash_history::NodeData::subtree_total_work`.
pub use primitive_types;
pub use subtle;
pub use zcash_encoding;
/// The ZIP 221 chain history tree (upstream under both backends: Zakura's node uses the
/// same crate).
pub use zcash_history;
/// The note encryption scheme of Sapling and Orchard (upstream under both backends: the
/// `zakura-*` forks build on the same crate).
pub use zcash_note_encryption;
/// The Rust script interpreter (0.6 line), used for verification.
pub use zcash_script;
/// The 0.4 line of `zcash_script`, whose `Code` type `zcash_transparent::address::Script`
/// wraps under both backends.
pub use zcash_script04;

#[cfg(feature = "upstream")]
pub use {
    bellman, bls12_381, equihash, ff, group, halo2_proofs, jubjub, orchard, pasta_curves,
    sapling_crypto, sinsemilla, zcash_address, zcash_primitives, zcash_proofs, zcash_protocol,
    zcash_transparent,
};

#[cfg(feature = "zakura")]
pub use {
    zk_address as zcash_address, zk_bellman as bellman, zk_bls12_381 as bls12_381,
    zk_equihash as equihash, zk_ff as ff, zk_group as group, zk_halo2 as halo2_proofs,
    zk_jubjub as jubjub, zk_orchard as orchard, zk_pasta as pasta_curves,
    zk_primitives as zcash_primitives, zk_proofs as zcash_proofs, zk_protocol as zcash_protocol,
    zk_sapling as sapling_crypto, zk_sinsemilla as sinsemilla, zk_transparent as zcash_transparent,
};

/// Suffix appended to benchmark ids of code built on this backend: empty for `upstream`,
/// `-zk` for `zakura`.
#[cfg(feature = "upstream")]
pub const BACKEND_SUFFIX: &str = "";
#[cfg(feature = "zakura")]
pub const BACKEND_SUFFIX: &str = "-zk";

/// Human-readable backend name.
#[cfg(feature = "upstream")]
pub const BACKEND: &str = "upstream";
#[cfg(feature = "zakura")]
pub const BACKEND: &str = "zakura";

/// The consensus branch of NU7, when the backend knows NU7.
///
/// `zakura-protocol` 2.2.0 has `BranchId::Nu7` (`0x7719_0ad9`). Upstream `zcash_protocol`
/// 0.10 has the variant only behind `cfg(zcash_unstable = "nu7")`, so this build does not
/// see it. This function and [`nu7_activation`] are the only code of the workspace that
/// depends on the backend for NU7.
pub fn nu7_branch() -> Option<zcash_protocol::consensus::BranchId> {
    #[cfg(feature = "upstream")]
    {
        None
    }
    #[cfg(feature = "zakura")]
    {
        Some(zcash_protocol::consensus::BranchId::Nu7)
    }
}

/// The NU7 activation height of `network`, when the backend gives one.
///
/// `zakura-protocol` 2.2.0: Testnet 4,465,026, Mainnet none. Upstream: none. Regtest has no
/// NU7 height on either backend.
pub fn nu7_activation(network: zcash_protocol::consensus::NetworkType) -> Option<u32> {
    #[cfg(feature = "upstream")]
    {
        let _ = network;
        None
    }
    #[cfg(feature = "zakura")]
    {
        use zcash_protocol::consensus::{
            NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK,
        };
        let height = match network {
            NetworkType::Main => MAIN_NETWORK.activation_height(NetworkUpgrade::Nu7),
            NetworkType::Test => TEST_NETWORK.activation_height(NetworkUpgrade::Nu7),
            NetworkType::Regtest => None,
        };
        height.map(u32::from)
    }
}

/// Random number generators accepted by the backend's APIs (`Field::random`,
/// `BatchValidator::validate`, the bundle builders).
///
/// The upstream stack takes rand_core 0.6 generators, the zakura stack rand_core 0.10 ones,
/// and the two lines name their traits and OS generator differently. The names here are the
/// same under both: [`StdRng`] with [`SeedableRng::seed_from_u64`] / `from_seed`, the core
/// trait [`RngCore`] (`next_u32`, `next_u64`, `fill_bytes`), and [`os_rng`] for OS entropy.
/// Randomness that never reaches a backend API (sizes, choices in tests) does not need this
/// module and can use the workspace `rand` directly.
pub mod rng {
    #[cfg(feature = "upstream")]
    pub use rand::rngs::StdRng;
    #[cfg(feature = "upstream")]
    pub use rand_core::{RngCore, SeedableRng};
    /// The OS entropy generator.
    #[cfg(feature = "upstream")]
    pub type OsRng = rand::rngs::OsRng;

    #[cfg(feature = "zakura")]
    pub use zk_rand::rngs::StdRng;
    #[cfg(feature = "zakura")]
    pub use zk_rand_core::{Rng as RngCore, SeedableRng};
    /// The OS entropy generator; rand 0.10 only ships the fallible `SysRng`, and the
    /// backend's APIs take infallible generators, so it is wrapped in `UnwrapErr`.
    #[cfg(feature = "zakura")]
    pub type OsRng = zk_rand_core::UnwrapErr<zk_rand::rngs::SysRng>;

    /// A deterministic generator seeded from `seed`, without the trait import (the two rand
    /// lines' `SeedableRng` traits are distinct, so a file that also seeds a workspace `rand`
    /// generator cannot import both under one name).
    pub fn seeded(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    /// An OS entropy generator value.
    pub fn os_rng() -> OsRng {
        #[cfg(feature = "upstream")]
        {
            rand::rngs::OsRng
        }
        #[cfg(feature = "zakura")]
        {
            zk_rand_core::UnwrapErr(zk_rand::rngs::SysRng)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ff::Field;
    use super::pasta_curves::pallas;
    use super::rng::{os_rng, RngCore, SeedableRng, StdRng};

    #[test]
    fn rng_types_feed_the_backend() {
        let mut seeded = StdRng::seed_from_u64(7);
        let a = pallas::Base::random(&mut seeded);
        let b = pallas::Base::random(&mut seeded);
        assert_ne!(a, b);
        let mut bytes = [0u8; 16];
        seeded.fill_bytes(&mut bytes);
        assert_ne!(bytes, [0u8; 16]);
        let mut os = os_rng();
        let _ = pallas::Base::random(&mut os);
        assert_ne!(os.next_u64(), os.next_u64());
    }

    #[test]
    fn nu7_is_known_to_the_zakura_backend_only() {
        use super::zcash_protocol::consensus::NetworkType;
        let known = super::BACKEND == "zakura";
        assert_eq!(
            super::nu7_branch().map(u32::from),
            known.then_some(0x7719_0ad9)
        );
        assert_eq!(
            super::nu7_activation(NetworkType::Test),
            known.then_some(4_465_026)
        );
        assert_eq!(super::nu7_activation(NetworkType::Main), None);
        assert_eq!(super::nu7_activation(NetworkType::Regtest), None);
    }

    #[test]
    fn backend_suffix_matches_backend() {
        assert_eq!(
            super::BACKEND_SUFFIX.is_empty(),
            super::BACKEND == "upstream"
        );
    }
}
