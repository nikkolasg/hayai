//! The header check of the node: the relay's standard check
//! (`hayai_relay::StandardHeaderCheck`) on the best chain index
//! (`hayai_sync::index::HeaderIndex`), with the trace row and the counter of the node.

use std::sync::Arc;
use std::time::Instant;

use hayai_relay::{HeaderCheck, HeaderError, StandardHeaderCheck};
use hayai_sync::index::HeaderIndex;
use hayai_trace::{event, Table, Tracer};
use hayai_wire::header::BlockHeader;
use serde_json::json;

use crate::metrics::NodeMetrics;
use crate::params::NetworkKind;

/// The header check that gates forwarding and validation: block not already in the chain,
/// parent known, then every header rule of `hayai_consensus::header::check_header` on the
/// context of the index, with the clock of the node.
///
/// The index of a shadow node starts at a block above the genesis block. Its seed holds
/// the time and the `nBits` of the start block and of the blocks before it,
/// `DIFFICULTY_CONTEXT_BLOCKS` blocks in all (fewer only when the chain is shorter), so
/// every rule runs from the first block after the start. A context that is too short for
/// a rule (an index with a shorter seed) is never a pass: the header passes the rules that
/// did not run only when the node trusts a short context (shadow mode), and each such
/// header increments `hayai_shadow_trusted_bits_total`. Without it the header is rejected.
pub struct NodeHeaderCheck {
    check: StandardHeaderCheck<Arc<HeaderIndex>>,
    tracer: Tracer,
    metrics: Arc<NodeMetrics>,
}

impl NodeHeaderCheck {
    /// The check of `network` on `index`. `trust_short_context` is set by a shadow node:
    /// it trusts upstream for the rules that its context cannot check. A full node starts
    /// at the genesis block and never sets it.
    pub fn new(
        network: NetworkKind,
        index: Arc<HeaderIndex>,
        tracer: Tracer,
        metrics: Arc<NodeMetrics>,
        trust_short_context: bool,
    ) -> Self {
        Self {
            check: StandardHeaderCheck {
                context: index,
                network,
                trust_short_context,
            },
            tracer,
            metrics,
        }
    }

    /// The rules, with a `block_header_checked` row. Returns the block's height.
    pub fn verify(&self, header: &BlockHeader) -> Result<u32, HeaderError> {
        let started = Instant::now();
        let hash = header.hash();
        let verdict = self.check.verify(header).map(|verified| {
            // A trusted header: the rules that the context cannot check did not run.
            if let Some(_unchecked) = verified.unchecked {
                self.metrics.trusted_bits.inc();
            }
            verified.height
        });
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.tracer.emit(
            Table::BlockSync,
            event::BLOCK_HEADER_CHECKED,
            || match &verdict {
                Ok(height) => json!({
                    "hash": hash.to_string(),
                    "height": height,
                    "result": "ok",
                    "elapsed_us": elapsed_us,
                }),
                Err(e) => json!({
                    "hash": hash.to_string(),
                    "result": "rejected",
                    "reason": e.to_string(),
                    "elapsed_us": elapsed_us,
                }),
            },
        );
        verdict
    }
}

/// The relay's check: the rules, then the header becomes pending until the driver commits
/// or rejects its block.
impl HeaderCheck for NodeHeaderCheck {
    fn check(&self, header: &BlockHeader) -> Result<(), HeaderError> {
        let height = self.verify(header)?;
        self.check.context.add_pending(header, height);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{NetParams, NetworkKind, REGTEST_POW_LIMIT_BITS};
    use hayai_consensus::header::{check_solution_length, HeaderRuleError};
    use hayai_sync::index::{now_secs, SeedBlock};
    use hayai_wire::header::{BlockHash, PowError, PowParams};

    const REGTEST_GENESIS_TIME: u32 = 1_296_688_602;

    fn regtest_genesis_hash() -> BlockHash {
        NetParams::new(NetworkKind::Regtest).genesis().0
    }

    fn header(prev: BlockHash, time: u32, bits: u32) -> BlockHeader {
        BlockHeader {
            version: 4,
            prev_hash: prev,
            merkle_root: [1; 32],
            block_commitments: [2; 32],
            time,
            bits,
            nonce: [0; 32],
            solution: vec![0; PowParams::REGTEST.solution_len()],
        }
    }

    /// A seed block without `nBits`.
    fn seed_block(hash: BlockHash, time: u32) -> SeedBlock {
        SeedBlock {
            hash,
            time,
            bits: None,
        }
    }

    fn check(index: Arc<HeaderIndex>) -> NodeHeaderCheck {
        check_on(NetworkKind::Regtest, index, false)
    }

    fn check_on(
        kind: NetworkKind,
        index: Arc<HeaderIndex>,
        trust_short_context: bool,
    ) -> NodeHeaderCheck {
        NodeHeaderCheck::new(
            kind,
            index,
            Tracer::disabled(),
            Arc::new(NodeMetrics::new(&hayai_metrics::Registry::new())),
            trust_short_context,
        )
    }

    /// A header of Zebra's Mainnet vectors.
    fn mainnet_header(height: u32) -> BlockHeader {
        let name = format!(
            "{}/../hayai-bench/tests/vectors/block-main-0-000-{height:03}.hex",
            env!("CARGO_MANIFEST_DIR")
        );
        let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
        BlockHeader::parse(&hex::decode(hex.trim()).expect("hex")).expect("a header")
    }

    /// The relay's check leaves the header pending; the pure rules do not.
    #[test]
    fn the_relay_check_leaves_the_header_pending() {
        let genesis = regtest_genesis_hash();
        let index = Arc::new(HeaderIndex::new(0, &[seed_block(genesis, 100)]));
        let c = check(index.clone());
        let now = now_secs();
        let first = header(genesis, now, REGTEST_POW_LIMIT_BITS);
        let second = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        assert_eq!(
            c.check(&second),
            Err(HeaderError::ParentUnknown(first.hash()))
        );
        assert_eq!(c.check(&first), Ok(()));
        assert!(index.is_pending(&first.hash()));
        assert_eq!(c.check(&second), Ok(()));
        assert!(index.is_pending(&second.hash()));
        index.remove_pending(&second.hash());
        assert_eq!(c.verify(&second), Ok(2));
        assert!(!index.is_pending(&second.hash()));
    }

    #[test]
    fn regtest_check_applies_the_waiver_and_the_time_rules() {
        let genesis = regtest_genesis_hash();
        let index = Arc::new(HeaderIndex::new(
            0,
            &[seed_block(genesis, REGTEST_GENESIS_TIME)],
        ));
        let c = check(index.clone());
        let now = now_secs();
        // Height 1 is exempt from the median-time-past maximum. The solution is never
        // verified and the hash meets no filter.
        let first = header(genesis, now, REGTEST_POW_LIMIT_BITS);
        assert_eq!(c.check(&first), Ok(()));
        index.push(first.clone());
        assert_eq!(
            c.check(&first),
            Err(HeaderError::AlreadyInChain(first.hash()))
        );
        let early = header(first.hash(), now - 1, REGTEST_POW_LIMIT_BITS);
        let Err(HeaderError::Rule(HeaderRuleError::TimeTooEarly { .. })) = c.check(&early) else {
            panic!("time at or below the median time past");
        };
        // The median of the two times is the time of the first block. Height 2 is the
        // first height with the maximum.
        let late = header(first.hash(), now + 5_401, REGTEST_POW_LIMIT_BITS);
        let Err(HeaderError::Rule(HeaderRuleError::TimeTooLate { .. })) = c.check(&late) else {
            panic!("time beyond the median time past plus 90 min");
        };
        let easy = header(first.hash(), now + 1, 0x2010_0000);
        assert_eq!(
            c.check(&easy),
            Err(HeaderError::Rule(HeaderRuleError::Pow(
                PowError::TargetAboveLimit(0x2010_0000)
            )))
        );
        // Regtest has no expected bits: a harder target than the limit passes.
        assert_eq!(c.verify(&header(first.hash(), now + 1, 0x1f07_ffff)), Ok(2));
        let orphan = header(BlockHash([9; 32]), now + 1, REGTEST_POW_LIMIT_BITS);
        assert_eq!(
            c.check(&orphan),
            Err(HeaderError::ParentUnknown(BlockHash([9; 32])))
        );
        let mut old = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        old.version = 3;
        assert_eq!(
            c.check(&old),
            Err(HeaderError::Rule(HeaderRuleError::Version(3)))
        );
        // The shape rule of the waiver: a (200, 9) solution is not a Regtest solution.
        let mut mainnet = header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS);
        mainnet.solution = vec![0; PowParams::MAINNET.solution_len()];
        assert_eq!(
            c.check(&mainnet),
            Err(HeaderError::Rule(HeaderRuleError::SolutionLength {
                expected: 36,
                got: 1344
            }))
        );
        assert_eq!(
            c.check(&header(first.hash(), now + 1, REGTEST_POW_LIMIT_BITS)),
            Ok(())
        );
    }

    /// The recorded zcashd and Zakura Regtest genesis block parses, has the hash of the
    /// constant that the index starts from, and its 36-byte solution passes the shape rule.
    #[test]
    fn recorded_regtest_genesis_matches_the_index_seed() {
        let hex = include_str!("../../hayai-wire/tests/vectors/block-regtest-0-000-000.hex");
        let bytes = hex::decode(hex.trim()).expect("hex");
        let genesis = BlockHeader::parse(&bytes).expect("a Regtest header parses");
        assert_eq!(genesis.hash(), regtest_genesis_hash());
        assert_eq!(
            NetParams::new(NetworkKind::Regtest).genesis().1,
            REGTEST_GENESIS_TIME
        );
        assert_eq!(genesis.time, REGTEST_GENESIS_TIME);
        assert_eq!(genesis.bits, REGTEST_POW_LIMIT_BITS);
        assert_eq!(
            check_solution_length(NetworkKind::Regtest, &genesis),
            Ok(())
        );
        let Err(HeaderRuleError::SolutionLength { .. }) =
            check_solution_length(NetworkKind::Testnet, &genesis)
        else {
            panic!("a Regtest solution on Testnet");
        };
    }

    /// A node from the Mainnet genesis block: the real blocks 1 to 10 pass every rule
    /// with the context of the index, and no header is trusted.
    #[test]
    fn mainnet_headers_from_genesis_pass_without_trust() {
        let genesis = mainnet_header(0);
        let index = Arc::new(HeaderIndex::new(
            0,
            &[seed_block(genesis.hash(), genesis.time)],
        ));
        let c = check_on(NetworkKind::Mainnet, index.clone(), false);
        for height in 1..=10 {
            let h = mainnet_header(height);
            assert_eq!(c.verify(&h), Ok(height));
            // A harder target than the chain requires is not the expected one.
            let mut hard = h.clone();
            hard.bits = 0x1f07_fffe;
            assert_eq!(
                c.verify(&hard),
                Err(HeaderError::Rule(HeaderRuleError::WrongBits {
                    expected: 0x1f07_ffff,
                    got: 0x1f07_fffe
                }))
            );
            index.push(h);
        }
        assert_eq!(c.metrics.trusted_bits.get(), 0);
    }

    /// An index that starts above the genesis block without the ancestors of its start:
    /// the context of the next header is too short. The header passes only when the node
    /// trusts its source, and the counter records it.
    #[test]
    fn a_short_context_is_trusted_and_counted_or_rejected() {
        let start = mainnet_header(1);
        let next = mainnet_header(2);
        let seed = [seed_block(start.hash(), start.time)];
        let strict = check_on(
            NetworkKind::Mainnet,
            Arc::new(HeaderIndex::new(1, &seed)),
            false,
        );
        let Err(HeaderError::ContextTooShort(unchecked)) = strict.verify(&next) else {
            panic!("the median-time-past of height 2 reads two times");
        };
        assert!(unchecked.time);
        assert_eq!(strict.metrics.trusted_bits.get(), 0);

        let index = Arc::new(HeaderIndex::new(1, &seed));
        let trusting = check_on(NetworkKind::Mainnet, index.clone(), true);
        assert_eq!(trusting.verify(&next), Ok(2));
        assert_eq!(trusting.metrics.trusted_bits.get(), 1);
        // The rules that the context allows still run on a trusted header.
        let mut hard = next.clone();
        hard.bits = 0x1f07_fffe;
        assert!(matches!(
            trusting.verify(&hard),
            Err(HeaderError::Rule(HeaderRuleError::WrongBits { .. }))
        ));
        let mut bad = next.clone();
        bad.nonce[0] ^= 1;
        assert!(matches!(
            trusting.verify(&bad),
            Err(HeaderError::Rule(
                HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_)
            ))
        ));
        assert_eq!(trusting.metrics.trusted_bits.get(), 1);
    }

    /// A shadow start above the genesis block with the whole seed: every header rule runs
    /// on the first block after the start, with no trust, and the counter stays at 0.
    #[test]
    fn a_whole_seed_needs_no_trust() {
        let seed: Vec<SeedBlock> = (0..=5)
            .map(|height| {
                let header = mainnet_header(height);
                SeedBlock {
                    hash: header.hash(),
                    time: header.time,
                    bits: Some(header.bits),
                }
            })
            .collect();
        for trust in [false, true] {
            let index = Arc::new(HeaderIndex::new(5, &seed));
            let c = check_on(NetworkKind::Mainnet, index.clone(), trust);
            for height in 6..=10 {
                let h = mainnet_header(height);
                assert_eq!(c.verify(&h), Ok(height));
                index.push(h);
            }
            assert_eq!(c.metrics.trusted_bits.get(), 0);
        }
    }
}
