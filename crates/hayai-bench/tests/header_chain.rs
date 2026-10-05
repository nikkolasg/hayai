//! The header chain of hayai-sync on real headers: the block vectors of heights 0 to 10 of
//! Mainnet and Testnet (`tests/vectors/`), with the proof-of-work check and Equihash
//! (200, 9) of each network.
//!
//! Each range runs two times: with a contextual rule that accepts every header, and with
//! the header rules of hayai-consensus (`check_contextual`, `check_local_time`).

use std::path::PathBuf;

use hayai_consensus::header::{check_contextual, check_local_time};
use hayai_consensus::{HeaderRuleError, HeaderVerdict, Network, ParentChain};
use hayai_sync::headers::{
    ChainConfig, HeaderChain, HeaderContextView, HeaderRules, RejectReason, Status, Tip,
};
use hayai_wire::header::{BlockHash, BlockHeader, PowError};

/// The contextual rule that accepts every header.
struct Permissive;

impl HeaderRules for Permissive {
    fn check(&self, _: &BlockHeader, _: &HeaderContextView<'_>) -> Result<(), HeaderRuleError> {
        Ok(())
    }
}

/// The header rules of hayai-consensus. The chain starts at the genesis block, so the
/// context always has the blocks that a rule reads.
struct Consensus;

impl HeaderRules for Consensus {
    fn check(
        &self,
        header: &BlockHeader,
        context: &HeaderContextView<'_>,
    ) -> Result<(), HeaderRuleError> {
        let chain = ParentChain {
            height: context.height,
            times: context.times,
            bits: context.bits,
        };
        match check_contextual(context.network, header, &chain)? {
            HeaderVerdict::Checked => check_local_time(header, context.now),
            HeaderVerdict::ContextTooShort(unchecked) => {
                panic!("a chain from the genesis block has a short context: {unchecked:?}")
            }
        }
    }
}

/// The headers of the block vectors of heights 0 to 10.
fn headers(network: Network) -> Vec<BlockHeader> {
    let name = match network {
        Network::Mainnet => "main",
        Network::Testnet => "test",
        Network::Regtest | Network::ConfiguredRegtest(_) => {
            panic!("Regtest has no block vectors")
        }
    };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    (0..=10)
        .map(|height| {
            let file = dir.join(format!("block-{name}-0-000-{height:03}.hex"));
            let text = std::fs::read_to_string(&file).expect("read a block vector");
            let bytes = hex::decode(text.trim()).expect("a block vector is hex");
            BlockHeader::parse(&bytes).expect("a block vector starts with a header")
        })
        .collect()
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir under target/")
}

fn open(dir: &tempfile::TempDir, network: Network) -> HeaderChain {
    let (chain, report) =
        HeaderChain::open(ChainConfig::new(network), &dir.path().join("headers.log")).unwrap();
    assert_eq!(report.torn_bytes, 0);
    chain
}

fn real_headers_connect(network: Network) {
    let headers = headers(network);
    let params = network.params();
    // The chain makes the genesis entry from these three values.
    assert_eq!(headers[0].hash(), params.genesis_hash);
    assert_eq!(headers[0].time, params.genesis_time);
    assert_eq!(headers[0].bits, params.pow_limit_bits);
    let now = headers[10].time;
    let tip = Tip {
        height: 10,
        hash: headers[10].hash(),
    };
    let zero = BlockHash([0; 32]);

    let rules: [&dyn HeaderRules; 2] = [&Permissive, &Consensus];
    for rules in rules {
        let dir = scratch();
        let mut chain = open(&dir, network);
        // A header whose parent is not in the chain.
        let error = chain
            .accept_headers(&headers[2..3], rules, now)
            .unwrap_err();
        assert!(matches!(
            error.reason,
            RejectReason::Unconnected(parent) if parent == headers[1].hash()
        ));
        // A changed solution: the hash is above the target, or Equihash fails.
        let mut changed = headers[1].clone();
        changed.solution[0] ^= 1;
        let error = chain.accept_headers(&[changed], rules, now).unwrap_err();
        assert!(matches!(
            error.reason,
            RejectReason::Rule(
                HeaderRuleError::Pow(PowError::HashAboveTarget) | HeaderRuleError::Equihash(_)
            )
        ));

        let accepted = chain.accept_headers(&headers[1..], rules, now).unwrap();
        assert_eq!((accepted.added, accepted.known), (10, 0));
        let change = accepted.tip_change.unwrap();
        assert_eq!(change.new, tip);
        assert_eq!(change.fork_point.hash, params.genesis_hash);
        assert!(!change.is_reorg());
        for (height, header) in headers.iter().enumerate().skip(1) {
            let entry = chain.entry(&header.hash()).unwrap();
            assert_eq!(entry.height as usize, height);
            assert_eq!(entry.status, Status::HeaderValid);
            assert!(entry.on_best_chain);
        }
        // The answer to `getheaders` has the bytes of the vectors.
        let served = chain
            .headers_after(&[params.genesis_hash], &zero, 160)
            .unwrap();
        assert_eq!(served, headers[1..].to_vec());

        // A start from the log gives the same chain.
        drop(chain);
        let chain = open(&dir, network);
        assert_eq!(chain.best_tip(), tip);
        assert_eq!(chain.locator().len(), 11);
    }

    // The rule against the clock of the node: the clock is 3 h before block 1.
    let dir = scratch();
    let mut chain = open(&dir, network);
    let early = headers[1].time - 3 * 60 * 60;
    let error = chain
        .accept_headers(&headers[1..], &Consensus, early)
        .unwrap_err();
    assert_eq!(error.index, 0);
    assert!(matches!(
        error.reason,
        RejectReason::Rule(HeaderRuleError::TimeTooFarAhead { .. })
    ));
}

#[test]
fn mainnet_headers_connect() {
    real_headers_connect(Network::Mainnet);
}

#[test]
fn testnet_headers_connect() {
    real_headers_connect(Network::Testnet);
}
