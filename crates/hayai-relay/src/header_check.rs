//! The header check that gates forwarding (`docs/protocol-compact-relay.md`, Terminology):
//! block not already in the chain, parent known, then every header rule of
//! `hayai_consensus::header::check_header` with the network's parameters and the clock of
//! the node.
//!
//! The rules live in hayai-consensus. This module adds what only the relay knows: whether
//! the chain holds the block, and the blocks before the parent. That context comes from a
//! [`HeaderContext`] that the node implements. The rules run cheapest first, so a garbage
//! header costs one SHA-256d at most before Equihash verification.

use hayai_consensus::header::{check_header, HeaderRuleError, HeaderVerdict, Unchecked};
use hayai_consensus::{Network, ParentChain};
use hayai_wire::header::{BlockHash, BlockHeader};

pub trait HeaderCheck {
    fn check(&self, header: &BlockHeader) -> Result<(), HeaderError>;
}

/// What the chain knows about a header's parent: the context of the header rules
/// (`hayai_consensus::ParentChain`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParentInfo {
    /// Height of the parent.
    pub height: u32,
    /// `nTime` of the parent and of the blocks before it, newest first. The rules read
    /// 28 times at most.
    pub times: Vec<u32>,
    /// `nBits` of the parent and of the blocks before it, newest first. The rules read
    /// 17 values at most.
    pub bits: Vec<u32>,
}

/// Chain state needed by the contextual parts of the header check.
pub trait HeaderContext {
    /// Whether the chain already holds the block with this hash (on any fork). Such a block
    /// is never forwarded again.
    fn has_block(&self, hash: &BlockHash) -> bool;
    /// `None` when the parent is not known to the chain.
    fn parent(&self, hash: &BlockHash) -> Option<ParentInfo>;
    /// Current network-adjusted time in seconds.
    fn now(&self) -> u32;
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum HeaderError {
    #[error("block {0} is already in the chain")]
    AlreadyInChain(BlockHash),
    #[error("parent {0} unknown")]
    ParentUnknown(BlockHash),
    /// A header rule of hayai-consensus failed.
    #[error(transparent)]
    Rule(#[from] HeaderRuleError),
    /// The chain holds fewer blocks before the header than a rule reads, so that rule did
    /// not run. The header is not accepted.
    #[error("the context is too short for a header rule: {}", .0.context)]
    ContextTooShort(Unchecked),
}

/// Known-block and parent from the context, then the header rules of `network`. A context
/// that is too short for a rule rejects the header: this check trusts no source.
pub struct StandardHeaderCheck<C> {
    pub context: C,
    pub network: Network,
}

impl<C: HeaderContext> HeaderCheck for StandardHeaderCheck<C> {
    fn check(&self, header: &BlockHeader) -> Result<(), HeaderError> {
        let hash = header.hash();
        if self.context.has_block(&hash) {
            return Err(HeaderError::AlreadyInChain(hash));
        }
        let Some(parent) = self.context.parent(&header.prev_hash) else {
            return Err(HeaderError::ParentUnknown(header.prev_hash));
        };
        let chain = ParentChain {
            height: parent.height + 1,
            times: &parent.times,
            bits: &parent.bits,
        };
        let now = Some(self.context.now());
        match check_header(self.network, header, &chain, now)? {
            HeaderVerdict::Checked => Ok(()),
            HeaderVerdict::ContextTooShort(unchecked) => {
                Err(HeaderError::ContextTooShort(unchecked))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use hayai_consensus::header::{MAX_FUTURE_BLOCK_TIME_LOCAL, MAX_FUTURE_BLOCK_TIME_MTP};
    use hayai_wire::header::{PowError, PowParams};

    use super::*;

    fn vector_header(name: &str) -> BlockHeader {
        let path = format!(
            "{}/../hayai-bench/tests/vectors/block-main-0-000-{name}.hex",
            env!("CARGO_MANIFEST_DIR")
        );
        let hex = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let bytes: Vec<u8> = (0..PowParams::MAINNET.header_len())
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        BlockHeader::parse(&bytes).unwrap()
    }

    /// Block 1 of Mainnet (Zebra's vectors): a real header with a real parent.
    fn block_1() -> BlockHeader {
        vector_header("001")
    }

    struct Context {
        parent_hash: BlockHash,
        parent: Option<ParentInfo>,
        now: u32,
        known: Vec<BlockHash>,
    }

    impl HeaderContext for Context {
        fn has_block(&self, hash: &BlockHash) -> bool {
            self.known.contains(hash)
        }
        fn parent(&self, hash: &BlockHash) -> Option<ParentInfo> {
            self.parent.clone().filter(|_| *hash == self.parent_hash)
        }
        fn now(&self) -> u32 {
            self.now
        }
    }

    /// The chain at the Mainnet genesis block, with the clock at the time of block 1.
    fn genesis_context() -> Context {
        let genesis = vector_header("000");
        Context {
            parent_hash: genesis.hash(),
            parent: Some(ParentInfo {
                height: 0,
                times: vec![genesis.time],
                bits: vec![genesis.bits],
            }),
            now: block_1().time,
            known: Vec::new(),
        }
    }

    fn check_on(context: Context) -> StandardHeaderCheck<Context> {
        StandardHeaderCheck {
            context,
            network: Network::Mainnet,
        }
    }

    #[test]
    fn a_block_already_in_the_chain_is_rejected_first() {
        let header = block_1();
        let mut context = genesis_context();
        context.known.push(header.hash());
        // Even with an unknown parent the known-block check comes first.
        context.parent = None;
        assert_eq!(
            check_on(context).check(&header),
            Err(HeaderError::AlreadyInChain(header.hash()))
        );
    }

    #[test]
    fn mainnet_block_1_passes_on_the_genesis_block() {
        let header = block_1();
        assert_eq!(header.bits, 0x1f07_ffff);
        assert_eq!(check_on(genesis_context()).check(&header), Ok(()));
    }

    #[test]
    fn contextual_failures_in_order() {
        let header = block_1();
        let genesis_time = genesis_context().parent.unwrap().times[0];
        let check = check_on(genesis_context());

        let mut orphan = header.clone();
        orphan.prev_hash = BlockHash([1; 32]);
        // The parent is looked up before any rule: the version of an orphan is not read.
        orphan.version = 3;
        assert_eq!(
            check.check(&orphan),
            Err(HeaderError::ParentUnknown(BlockHash([1; 32])))
        );

        let mut old = header.clone();
        old.version = 3;
        assert_eq!(
            check.check(&old),
            Err(HeaderError::Rule(HeaderRuleError::Version(3)))
        );

        // The chain requires the limit at height 1. A harder target is not the expected one.
        let mut hard = header.clone();
        hard.bits = 0x1f07_fffe;
        assert_eq!(
            check.check(&hard),
            Err(HeaderError::Rule(HeaderRuleError::WrongBits {
                expected: 0x1f07_ffff,
                got: 0x1f07_fffe
            }))
        );
        // A target above the Mainnet limit fails the limit rule.
        let mut easy = header.clone();
        easy.bits = 0x2007_ffff;
        assert_eq!(
            check.check(&easy),
            Err(HeaderError::Rule(HeaderRuleError::Pow(
                PowError::TargetAboveLimit(0x2007_ffff)
            )))
        );

        let mut early = header.clone();
        early.time = genesis_time;
        assert_eq!(
            check.check(&early),
            Err(HeaderError::Rule(HeaderRuleError::TimeTooEarly {
                time: genesis_time,
                median_time_past: genesis_time
            }))
        );

        // The clock of the node: at most 2 h behind the header.
        let mut context = genesis_context();
        context.now = header.time - MAX_FUTURE_BLOCK_TIME_LOCAL - 1;
        assert_eq!(
            check_on(context).check(&header),
            Err(HeaderError::Rule(HeaderRuleError::TimeTooFarAhead {
                time: header.time,
                limit: header.time - 1
            }))
        );
        let mut context = genesis_context();
        context.now = header.time - MAX_FUTURE_BLOCK_TIME_LOCAL;
        assert_eq!(check_on(context).check(&header), Ok(()));
    }

    /// The median-time-past maximum applies from Mainnet height 2: block 2 on a context
    /// whose median is more than 90 min before it fails, block 1 does not.
    #[test]
    fn the_median_time_past_maximum_starts_at_height_2() {
        let genesis = vector_header("000");
        let first = block_1();
        let second = vector_header("002");
        let far = first.time - MAX_FUTURE_BLOCK_TIME_MTP - 1;
        let mut context = genesis_context();
        context.parent = Some(ParentInfo {
            height: 0,
            times: vec![far],
            bits: vec![genesis.bits],
        });
        assert_eq!(check_on(context).check(&first), Ok(()));

        let on_block_1 = |times: Vec<u32>| Context {
            parent_hash: first.hash(),
            parent: Some(ParentInfo {
                height: 1,
                times,
                bits: vec![first.bits, genesis.bits],
            }),
            now: second.time,
            known: Vec::new(),
        };
        let real = on_block_1(vec![first.time, genesis.time]);
        assert_eq!(check_on(real).check(&second), Ok(()));
        // The median of two times is the newer one.
        let far = second.time - MAX_FUTURE_BLOCK_TIME_MTP - 1;
        let late = on_block_1(vec![far, genesis.time]);
        assert_eq!(
            check_on(late).check(&second),
            Err(HeaderError::Rule(HeaderRuleError::TimeTooLate {
                time: second.time,
                limit: second.time - 1
            }))
        );
    }

    /// A context that is too short for a rule rejects the header with a distinct error.
    #[test]
    fn a_short_context_is_rejected() {
        let first = block_1();
        let second = vector_header("002");
        let context = Context {
            parent_hash: first.hash(),
            parent: Some(ParentInfo {
                height: 1,
                times: vec![first.time],
                bits: vec![first.bits],
            }),
            now: second.time,
            known: Vec::new(),
        };
        let Err(HeaderError::ContextTooShort(unchecked)) = check_on(context).check(&second) else {
            panic!("the median-time-past of height 2 reads two times");
        };
        assert!(unchecked.time);
        assert_eq!(unchecked.context.times, 1);
        assert_eq!(unchecked.context.needed_times, 2);
    }

    /// A Mainnet header under the Regtest parameters fails on its solution length before
    /// any hash work, and a Regtest-length solution under the Mainnet parameters too.
    #[test]
    fn solution_length_follows_the_network() {
        let header = block_1();
        let check = StandardHeaderCheck {
            context: genesis_context(),
            network: Network::Regtest,
        };
        // The Mainnet limit is below the Regtest limit, so the contextual rules pass.
        assert_eq!(
            check.check(&header),
            Err(HeaderError::Rule(HeaderRuleError::SolutionLength {
                expected: 36,
                got: 1344
            }))
        );
        let mut regtest = header.clone();
        regtest.solution = vec![0; 36];
        assert_eq!(
            check_on(genesis_context()).check(&regtest),
            Err(HeaderError::Rule(HeaderRuleError::SolutionLength {
                expected: 1344,
                got: 36
            }))
        );
    }

    #[test]
    fn pow_and_equihash_failures() {
        let header = block_1();
        let check = check_on(genesis_context());
        // Any change to the solution breaks it, and also changes the header hash, which at
        // the Mainnet limit passes the hash filter once in about 8192 tries. The filter
        // runs first, so the test searches a variant that passes it to reach Equihash.
        let mut bad_solution = header.clone();
        let mut counter = 0u32;
        loop {
            bad_solution.solution[..4].copy_from_slice(&counter.to_le_bytes());
            match check.check(&bad_solution) {
                Err(HeaderError::Rule(HeaderRuleError::Pow(PowError::HashAboveTarget))) => {
                    counter += 1;
                }
                other => {
                    assert!(matches!(
                        other,
                        Err(HeaderError::Rule(HeaderRuleError::Equihash(_)))
                    ));
                    break;
                }
            }
        }
        let mut nonce_changed = header.clone();
        nonce_changed.nonce[0] ^= 1;
        assert!(matches!(
            check.check(&nonce_changed),
            Err(HeaderError::Rule(
                HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_)
            ))
        ));
    }
}
