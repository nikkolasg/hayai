//! Orchard verifying keys and the batch verification of Orchard bundles.
//!
//! Orchard verifying keys depend on the circuit version, so [`crate::ScopedBatch`] groups
//! the bundles per version. Only [`crate::VerifyingKeys::prebuild`] builds a key.

use std::sync::{Arc, OnceLock};

use hayai_crypto::rng::os_rng;
use hayai_crypto::{orchard, zcash_protocol};
use orchard::circuit::{OrchardCircuitVersion, VerifyingKey};
use zcash_protocol::consensus::BranchId;

use crate::nu5_or_later;
use crate::shielded::{Item, Queued};

/// Orchard verifying keys, one per circuit version. [`VerifyingKeys::prebuild`] builds them.
pub struct OrchardKeys {
    keys: [OnceLock<Arc<VerifyingKey>>; 3],
}

/// The keys that this process built, by slot. A key depends only on its circuit version,
/// so two key sets of one process (two nodes of a test) share one build.
static BUILT: [OnceLock<Arc<VerifyingKey>>; 3] =
    [OnceLock::new(), OnceLock::new(), OnceLock::new()];

pub(crate) const ORCHARD_VERSIONS: [OrchardCircuitVersion; 3] = [
    OrchardCircuitVersion::InsecurePreNu6_2,
    OrchardCircuitVersion::FixedPostNu6_2,
    OrchardCircuitVersion::PostNu6_3,
];

pub(crate) fn slot(version: OrchardCircuitVersion) -> usize {
    match version {
        OrchardCircuitVersion::InsecurePreNu6_2 => 0,
        OrchardCircuitVersion::FixedPostNu6_2 => 1,
        OrchardCircuitVersion::PostNu6_3 => 2,
    }
}

impl Default for OrchardKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl OrchardKeys {
    pub fn new() -> Self {
        Self {
            keys: [OnceLock::new(), OnceLock::new(), OnceLock::new()],
        }
    }

    /// Builds the key of `version`. Only the build thread of [`VerifyingKeys::prebuild`]
    /// calls it; that thread is not a rayon worker.
    pub(crate) fn build(&self, version: OrchardCircuitVersion) {
        self.keys[slot(version)].get_or_init(|| {
            BUILT[slot(version)]
                .get_or_init(|| Arc::new(VerifyingKey::build(version)))
                .clone()
        });
    }

    /// The key of `version` when it is built; never builds it.
    pub fn built(&self, version: OrchardCircuitVersion) -> Option<&VerifyingKey> {
        self.keys[slot(version)].get().map(Arc::as_ref)
    }
}

/// The Orchard circuit version whose keys verify the Orchard and Ironwood bundles of
/// `branch`: `None` before NU5 (no Orchard pool). Mirrors upstream
/// `BundleVersion::circuit_version`.
///
/// Spec §4.6, ZIP 257, ZIP 258: the key of the epoch (NU5 to NU6.1 `InsecurePreNU6_2`,
/// NU6.2 `FixedPostNU6_2`, from NU6.3 `PostNU6_3`).
pub fn circuit_version(branch: BranchId) -> Option<OrchardCircuitVersion> {
    if !nu5_or_later(branch) {
        return None;
    }
    Some(match branch {
        BranchId::Nu5 | BranchId::Nu6 | BranchId::Nu6_1 => OrchardCircuitVersion::InsecurePreNu6_2,
        BranchId::Nu6_2 => OrchardCircuitVersion::FixedPostNu6_2,
        _ => OrchardCircuitVersion::PostNu6_3,
    })
}

/// Whether every Orchard or Ironwood bundle of `items` is valid under `vk`. The upstream
/// validator derives the public inputs of each bundle from its flags, so a bundle with
/// `enableCrossAddress = 0` is verified against the restricted instance.
///
/// Spec §4.6: the proofs and the spend authorization signatures; Spec §3.7, §7.1.2: the
/// binding signature of each pool.
pub(crate) fn verify_orchard(vk: &VerifyingKey, items: &[&Item]) -> bool {
    let mut validator = orchard::bundle::BatchValidator::new(vk);
    for item in items {
        let bundle = match item.bundle {
            Queued::Orchard => item.tx.orchard_bundle(),
            Queued::Ironwood => item.tx.ironwood_bundle(),
            Queued::Sapling | Queued::Sprout => None,
        };
        let Some(bundle) = bundle else {
            unreachable!("queued under an Orchard circuit group");
        };
        let Ok(()) = validator.add_bundle(bundle, item.sighash) else {
            return false;
        };
    }
    validator.validate(os_rng())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circuit_version_follows_the_branch() {
        assert_eq!(circuit_version(BranchId::Canopy), None);
        for branch in [BranchId::Nu5, BranchId::Nu6, BranchId::Nu6_1] {
            assert_eq!(
                circuit_version(branch),
                Some(OrchardCircuitVersion::InsecurePreNu6_2)
            );
        }
        assert_eq!(
            circuit_version(BranchId::Nu6_2),
            Some(OrchardCircuitVersion::FixedPostNu6_2)
        );
        assert_eq!(
            circuit_version(BranchId::Nu6_3),
            Some(OrchardCircuitVersion::PostNu6_3)
        );
        // NU7 keeps the circuit of NU6.3 (Zakura `zakura-consensus/src/primitives/
        // halo2.rs:405`).
        if let Some(nu7) = hayai_crypto::nu7_branch() {
            assert_eq!(circuit_version(nu7), Some(OrchardCircuitVersion::PostNu6_3));
        }
    }
}
