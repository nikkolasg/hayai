//! The Orchard verifying key and the rayon pool.
//!
//! A validation used to build the Orchard key on first use, inside `OnceLock::get_or_init`
//! on a rayon worker, with the multicore keygen. While that worker waited in the keygen, it
//! could take another validation task that waited on the same `OnceLock`, and the process
//! stopped (6 of 8 parallel runs of `tests/validate.rs`). Only `VerifyingKeys::prebuild`
//! builds a key now, on its own thread. This test runs the old pattern, parallel
//! validations of an Orchard block with a cold key set, several times in one process under
//! a timeout. The validations must fail at once with the missing key, then pass with the
//! prebuilt key.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use hayai_bench::chain_fixture::{harness, Harness};
use hayai_bench::fixtures::{orchard_block, FIXTURE_BRANCH};
use hayai_prepared::{PrepareError, RuleEpoch, VerifyingKeys};
use hayai_validate::{validate_block, BlockError};
use rayon::prelude::*;

const ROUNDS: usize = 4;
const PARALLEL: usize = 8;
/// Far above the time of 8 validations and one key build in a debug build.
const TIMEOUT: Duration = Duration::from_secs(600);

/// `PARALLEL` validations of the harness block on the rayon pool, on a thread of their own.
/// The test fails when they do not return within `TIMEOUT`.
fn validate_in_parallel(h: Harness) -> Vec<Result<(), BlockError>> {
    let h = Arc::new(h);
    let (tx, rx) = mpsc::channel();
    let helper = std::thread::spawn(move || {
        let results = (0..PARALLEL)
            .into_par_iter()
            .map(|_| validate_block(h.block.clone(), &h.store, &h.chain.view(), &h.cfg).map(|_| ()))
            .collect();
        tx.send(results).expect("the test waits for the results");
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(results) => {
            helper
                .join()
                .expect("the helper thread ends without a panic");
            results
        }
        Err(e) => panic!("parallel validations did not return within {TIMEOUT:?}: {e}"),
    }
}

#[test]
fn parallel_validation_with_cold_keys_fails_fast_and_never_hangs() {
    let fixture = orchard_block(2, 2);
    for round in 0..ROUNDS {
        let mut h = harness(&fixture);
        h.cfg.keys = Arc::new(VerifyingKeys::new());
        for result in validate_in_parallel(h) {
            let Err(BlockError::Prepare {
                error: PrepareError::Unsupported(_),
                ..
            }) = result
            else {
                panic!("round {round}: a cold key set gives the missing key, got {result:?}");
            };
        }
    }
    let keys = VerifyingKeys::prebuild(RuleEpoch::consensus(FIXTURE_BRANCH), None);
    keys.ready();
    for round in 0..ROUNDS {
        let mut h = harness(&fixture);
        h.cfg.keys = keys.clone();
        for result in validate_in_parallel(h) {
            let Ok(()) = result else {
                panic!("round {round}: the prebuilt key verifies the block, got {result:?}");
            };
        }
    }
}
