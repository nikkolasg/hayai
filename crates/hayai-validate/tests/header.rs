//! The contextual header rules inside a validation: `check_block_header` on the context of
//! a view, with Zebra's Mainnet block vectors.

use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{MemBacking, MemConfig};
use hayai_consensus::header::HeaderRuleError;
use hayai_consensus::{Network, RuleSet, Upgrade};
use hayai_state::{Base, Chain, ChainView, ContextError};
use hayai_validate::{check_block_header, BlockError, HeaderPolicy};
use hayai_wire::header::PowError;
use hayai_wire::RawBlock;

/// A block of Zebra's Mainnet vectors.
fn mainnet_block(height: u32) -> RawBlock {
    let name = format!(
        "{}/../hayai-bench/tests/vectors/block-main-0-000-{height:03}.hex",
        env!("CARGO_MANIFEST_DIR")
    );
    let hex = std::fs::read_to_string(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let bytes = hex::decode(hex.trim()).expect("hex");
    let Some(rules) = RuleSet::of(Upgrade::Sprout) else {
        panic!("Sprout has a rule set");
    };
    RawBlock::parse(Bytes::from(bytes), rules.branch_id).expect("a block")
}

/// A view whose tip is `parent`, with the time of `parent` as its only context. The
/// directory holds the files of the backing while the view lives.
fn view_at(parent: &RawBlock, height: u32) -> (ChainView, tempfile::TempDir) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("scratch dir in target/");
    let (backing, _) = MemBacking::open(dir.path(), &MemConfig::default()).expect("a backing");
    let base = Base::new(Arc::new(backing), height, parent.hash(), parent.header.time);
    (Chain::new(base).view(), dir)
}

/// Block 1 of Mainnet on the genesis block: the view holds the whole context, so every
/// contextual rule runs under [`HeaderPolicy::Enforce`].
#[test]
fn the_header_rules_run_on_the_context_of_the_view() {
    let genesis = mainnet_block(0);
    let block = mainnet_block(1);
    let (view, _dir) = view_at(&genesis, 0);
    let enforce = HeaderPolicy::Enforce;
    assert!(matches!(
        check_block_header(&block, &view, Network::Mainnet, enforce),
        Ok(())
    ));

    let mut early = block.clone();
    early.header.time = genesis.header.time;
    assert!(matches!(
        check_block_header(&early, &view, Network::Mainnet, enforce),
        Err(BlockError::Header(HeaderRuleError::TimeTooEarly { .. }))
    ));
    let mut hard = block.clone();
    hard.header.bits = 0x1f07_fffe;
    for policy in [enforce, HeaderPolicy::TrustShortContext] {
        assert!(matches!(
            check_block_header(&hard, &view, Network::Mainnet, policy),
            Err(BlockError::Header(HeaderRuleError::WrongBits {
                expected: 0x1f07_ffff,
                got: 0x1f07_fffe
            }))
        ));
    }
    let mut easy = block.clone();
    easy.header.bits = 0x2007_ffff;
    assert!(matches!(
        check_block_header(&easy, &view, Network::Mainnet, enforce),
        Err(BlockError::Header(HeaderRuleError::Pow(
            PowError::TargetAboveLimit(0x2007_ffff)
        )))
    ));
    let mut old = block.clone();
    old.header.version = 3;
    assert!(matches!(
        check_block_header(&old, &view, Network::Mainnet, enforce),
        Err(BlockError::Header(HeaderRuleError::Version(3)))
    ));
    // A block for another tip fails the parent rule, not a header rule.
    assert!(matches!(
        check_block_header(&mainnet_block(2), &view, Network::Mainnet, enforce),
        Err(BlockError::Context(ContextError::WrongParent { .. }))
    ));
    // Generated blocks: no rule runs.
    assert!(matches!(
        check_block_header(&old, &view, Network::Mainnet, HeaderPolicy::GeneratedBlocks),
        Ok(())
    ));
}

/// A view that starts at block 1 holds one time, and block 2 reads two: the result
/// depends on the policy, and it is never a pass under [`HeaderPolicy::Enforce`].
#[test]
fn a_short_context_passes_only_when_the_policy_trusts_it() {
    let parent = mainnet_block(1);
    let block = mainnet_block(2);
    let (view, _dir) = view_at(&parent, 1);
    let Err(BlockError::HeaderContext(unchecked)) =
        check_block_header(&block, &view, Network::Mainnet, HeaderPolicy::Enforce)
    else {
        panic!("a short context is an error under Enforce");
    };
    assert!(unchecked.time);
    assert!(!unchecked.bits, "height 2 has the limit without a context");
    assert_eq!(unchecked.context.times, 1);
    assert_eq!(unchecked.context.needed_times, 2);
    let trust = HeaderPolicy::TrustShortContext;
    assert!(matches!(
        check_block_header(&block, &view, Network::Mainnet, trust),
        Ok(())
    ));
    // The rules that need no context still run.
    let mut hard = block.clone();
    hard.header.bits = 0x1f07_fffe;
    assert!(matches!(
        check_block_header(&hard, &view, Network::Mainnet, trust),
        Err(BlockError::Header(HeaderRuleError::WrongBits { .. }))
    ));
}
