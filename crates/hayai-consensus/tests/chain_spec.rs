//! A chain that a crate outside hayai-consensus defines as data, with the public interface
//! only: the spec of Testnet with other activation heights, another genesis block and
//! another start height of the minimum-difficulty rule.

use hayai_consensus::difficulty::expected_bits;
use hayai_consensus::funding::{funding_streams, Receiver, Stream, StreamSet};
use hayai_consensus::lockbox::Disbursement;
use hayai_consensus::{
    address_of, founders, lockbox, nsm, rules_at, subsidy, ChainSpec, ChainSpecError, Checkpoints,
    ConsensusError, Network, ParentChain, RuleSet, Upgrade,
};
use hayai_wire::header::BlockHash;

const GENESIS: BlockHash = BlockHash([0x42; 32]);
const NU6_2: u32 = 4_060_000;
const NU6_3: u32 = 4_100_000;
const NU7: u32 = 4_200_000;
/// The first height of the minimum-difficulty rule.
const MIN_DIFFICULTY_START: u32 = 1_000;

/// Testnet with NU6.2 at 4,060,000, NU6.3 at 4,100,000, NU7 at 4,200,000, another genesis
/// block and the minimum-difficulty rule from height 1,000.
fn forknet_spec() -> ChainSpec {
    let mut spec = Network::Testnet.spec().clone();
    spec.name = "forknet";
    spec.params.genesis_hash = GENESIS;
    spec.params.genesis_time = 1_700_000_000;
    spec.params.min_difficulty_start_height = Some(MIN_DIFFICULTY_START);
    for (upgrade, height) in [
        (Upgrade::Nu6_2, NU6_2),
        (Upgrade::Nu6_3, NU6_3),
        (Upgrade::Nu7, NU7),
    ] {
        spec.activation_heights[upgrade as usize] = Some(height);
    }
    spec.checkpoints = Checkpoints::new(vec![(0, GENESIS)]).expect("one checkpoint");
    spec.mandatory_checkpoint_height = 0;
    spec
}

fn forknet() -> Network {
    forknet_spec().network().expect("a valid spec")
}

#[test]
fn a_custom_chain_has_the_values_of_its_spec() {
    let network = forknet();
    let Network::Custom(_) = network else {
        panic!("a spec gives a custom network");
    };
    assert_eq!(network.name(), "forknet");
    assert!(!network.is_regtest());
    assert_eq!(network.network_type(), Network::Testnet.network_type());
    assert_eq!(network.params().genesis_hash, GENESIS);
    assert_eq!(network.params().pow, Network::Testnet.params().pow);
    assert_eq!(network.checkpoints().hash_at(0), Some(GENESIS));
    assert_eq!(network.checkpoints().len(), 1);
    assert_eq!(network.mandatory_checkpoint_height(), 0);
    assert_ne!(Network::Testnet.checkpoints().hash_at(0), Some(GENESIS));
}

#[test]
fn a_custom_chain_has_the_rule_sets_of_its_heights() {
    let network = forknet();
    // Testnet activates NU6.2 at 4,052,000. On the custom chain the height is in NU6.1 and
    // in the Orchard soft fork of Testnet.
    let height = 4_055_000;
    assert_eq!(Network::Testnet.upgrade_at(height), Upgrade::Nu6_2);
    assert_eq!(network.upgrade_at(height), Upgrade::Nu6_1);
    assert!(!Network::Testnet.orchard_disabled(height));
    assert!(network.orchard_disabled(height));
    assert_eq!(
        rules_at(network, height),
        rules_at(Network::Testnet, 4_050_000)
    );
    assert!(!network.orchard_disabled(NU6_2));

    let Ok(nu6_3) = rules_at(network, NU6_3) else {
        panic!("NU6.3 has a rule set");
    };
    assert_eq!(nu6_3.upgrade, Upgrade::Nu6_3);
    assert_eq!(
        rules_at(Network::Testnet, NU6_3).map(|r| r.upgrade),
        Ok(Upgrade::Nu6_2)
    );
    assert_eq!(
        rules_at(network, NU7 - 1).map(|r| r.upgrade),
        Ok(Upgrade::Nu6_3)
    );
    assert_eq!(network.next_upgrade(NU6_3), Some(Upgrade::Nu7));
    // NU7 has a rule set when the crypto backend has the NU7 branch id.
    match RuleSet::of(Upgrade::Nu7) {
        Some(nu7) => assert_eq!(rules_at(network, NU7), Ok(nu7)),
        None => assert_eq!(
            rules_at(network, NU7),
            Err(ConsensusError::UnsupportedUpgrade {
                upgrade: Upgrade::Nu7,
                height: NU7,
            })
        ),
    }
}

#[test]
fn a_custom_chain_has_the_subsidy_and_the_funding_streams_of_its_heights() {
    let network = forknet();
    // Before NU7 the schedule is the schedule of Testnet.
    assert_eq!(
        subsidy::block_subsidy(network, NU6_3),
        subsidy::block_subsidy(Network::Testnet, NU6_3)
    );
    // NU7 at 4,200,000 moves the third halving: 276,000 blocks at 75 s before it are
    // 828,000 blocks at 25 s, so the halving is at 5,028,000 and not at 4,476,000.
    let height = 4_500_000;
    assert_eq!(subsidy::halving(Network::Testnet, height), 3);
    assert_eq!(subsidy::halving(network, height), 2);
    assert_eq!(subsidy::halving(network, 5_027_999), 2);
    assert_eq!(subsidy::halving(network, 5_028_000), 3);

    // The last stream set of Testnet ends at the third halving, so it moves too.
    assert_eq!(funding_streams(Network::Testnet, height, 100), Ok(vec![]));
    let streams: Vec<_> = funding_streams(network, height, 100)
        .expect("a subsidy of 100")
        .into_iter()
        .map(|stream| {
            (
                stream.receiver,
                stream.value,
                stream.script.map(|script| address_of(network, &script)),
            )
        })
        .collect();
    assert_eq!(
        streams,
        [
            (Receiver::Deferred, 12, None),
            (
                Receiver::MajorGrants,
                8,
                Some("t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu".to_string())
            ),
        ]
    );
    assert_eq!(funding_streams(network, 5_028_000, 100), Ok(vec![]));

    // The NU6.1 height and its lockbox disbursement are the ones of Testnet.
    let nu6_1 = network.activation_height(Upgrade::Nu6_1);
    assert_eq!(nu6_1, Some(3_536_500));
    let [disbursement] = lockbox::disbursements(network, 3_536_500) else {
        panic!("one disbursement at the NU6.1 activation height");
    };
    assert_eq!(
        address_of(network, &disbursement.script),
        "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8"
    );
}

#[test]
fn a_custom_chain_has_the_header_rules_of_its_params() {
    let network = forknet();
    let height = 2_000;
    let spacing = 150;
    // The 28 blocks before the header, newest first, one target spacing apart.
    let parent_time = 1_700_000_000 + spacing * 28;
    let times: Vec<u32> = (0..28).map(|i| parent_time - spacing * i).collect();
    let bits = vec![0x1f07_ffff; 17];
    let chain = ParentChain {
        height,
        times: &times,
        bits: &bits,
    };
    let limit = network.params().pow_limit_bits;
    // A header one spacing after its parent has the adjusted `nBits` on both networks.
    let on_time = parent_time + spacing;
    let adjusted = expected_bits(Network::Testnet, on_time, &chain);
    assert_eq!(expected_bits(network, on_time, &chain), adjusted);
    assert_ne!(adjusted, Ok(limit));
    // A header more than 6 spacings after its parent has the proof-of-work limit from the
    // minimum-difficulty height of the custom chain. Testnet starts the rule at 299,188.
    let late = parent_time + 6 * spacing + 1;
    assert_eq!(expected_bits(network, late, &chain), Ok(limit));
    assert_eq!(expected_bits(Network::Testnet, late, &chain), adjusted);
    let before = ParentChain {
        height: MIN_DIFFICULTY_START - 1,
        ..chain
    };
    assert_ne!(expected_bits(network, late, &before), Ok(limit));
}

/// A clone of the spec of a built-in network gives a network with the same values.
#[test]
fn a_clone_of_a_built_in_spec_is_the_same_chain() {
    for builtin in Network::ALL {
        let network = builtin.spec().clone().network().expect("a valid spec");
        assert_ne!(network, builtin);
        assert_eq!(network.spec(), builtin.spec());
        assert_eq!(network.name(), builtin.name());
        assert_eq!(network.is_regtest(), builtin.is_regtest());
        assert_eq!(network.checkpoints(), builtin.checkpoints());
        assert_eq!(
            network.mandatory_checkpoint_height(),
            builtin.mandatory_checkpoint_height()
        );
        assert_eq!(nsm::expected_seed(network), nsm::expected_seed(builtin));
        assert_eq!(
            nsm::reissuance_height(network),
            nsm::reissuance_height(builtin)
        );
        let mut heights: Vec<u32> = Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| builtin.activation_height(upgrade))
            .flat_map(|height| [height.saturating_sub(1), height, height + 1])
            .collect();
        heights.extend([20_000, 600_000, 1_116_000, 2_796_000, 3_000_000, 4_476_000]);
        for height in heights {
            assert_eq!(
                rules_at(network, height),
                rules_at(builtin, height),
                "{height}"
            );
            assert_eq!(
                subsidy::halving(network, height),
                subsidy::halving(builtin, height)
            );
            assert_eq!(
                funding_streams(network, height, 1_000_000),
                funding_streams(builtin, height, 1_000_000),
                "{builtin:?} {height}"
            );
            assert_eq!(
                lockbox::disbursements(network, height),
                lockbox::disbursements(builtin, height)
            );
            assert_eq!(
                founders::founders_reward(network, height),
                founders::founders_reward(builtin, height)
            );
        }
    }
}

/// Two Testnet addresses for the first stream set of Testnet, which has 51 address
/// periods.
static TWO_ADDRESSES: [StreamSet; 1] = [StreamSet {
    start: 1_028_500,
    end: 2_796_000,
    ends_at_third_halving: false,
    streams: &[Stream {
        receiver: Receiver::Ecc,
        numerator: 7,
        addresses: &[
            "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
            "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
        ],
    }],
}];

#[test]
fn a_spec_is_checked_before_it_gives_a_network() {
    let refused = |change: fn(&mut ChainSpec)| {
        let mut spec = forknet_spec();
        change(&mut spec);
        spec.network().expect_err("refused")
    };
    assert_eq!(
        refused(|spec| spec.activation_heights[Upgrade::Sprout as usize] = Some(1)),
        ChainSpecError::Sprout
    );
    assert_eq!(
        refused(|spec| spec.activation_heights[Upgrade::Nu6 as usize] = Some(1_000)),
        ChainSpecError::Order {
            upgrade: Upgrade::Nu6,
            height: 1_000
        }
    );
    assert_eq!(
        refused(|spec| spec.params.genesis_hash = BlockHash([1; 32])),
        ChainSpecError::Genesis
    );
    assert_eq!(
        refused(|spec| spec.mandatory_checkpoint_height = 1),
        ChainSpecError::Coverage(1)
    );
    // The Orchard soft fork of Testnet starts at 4,048,500, before this NU6.1 height.
    assert_eq!(
        refused(|spec| spec.activation_heights[Upgrade::Nu6_1 as usize] = Some(4_050_000)),
        ChainSpecError::OrchardSoftFork(4_048_500)
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &TWO_ADDRESSES),
        ChainSpecError::StreamAddresses {
            receiver: Receiver::Ecc,
            start: 1_028_500,
            end: 2_796_000,
            required: 51,
            found: 2,
        }
    );
}

/// Two networks from equal specs are two networks: a custom network is equal only to
/// itself.
#[test]
fn a_custom_network_is_equal_to_itself_only() {
    let (one, two) = (forknet(), forknet());
    assert_eq!(one, one);
    assert_ne!(one, two);
    assert_eq!(one.spec(), two.spec());
}

/// The Testnet address of the Major Grants stream from NU6.1.
const GRANTS: &str = "t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu";
/// The Testnet lockbox disbursement address.
const LOCKBOX: &str = "t2RnBRiqrN1nW4ecZs1Fj3WWjNdnSs4kiX8";

/// A stream set of the range of the last Testnet set, with `streams`.
const fn last_range(streams: &'static [Stream]) -> StreamSet {
    StreamSet {
        start: 3_536_500,
        end: 4_476_000,
        ends_at_third_halving: false,
        streams,
    }
}

const fn stream(receiver: Receiver, numerator: u64, addresses: &'static [&'static str]) -> Stream {
    Stream {
        receiver,
        numerator,
        addresses,
    }
}

static NUMERATORS: [StreamSet; 1] = [last_range(&[
    stream(Receiver::Deferred, 93, &[]),
    stream(Receiver::MajorGrants, 8, &[GRANTS]),
])];
static RECEIVERS: [StreamSet; 1] = [last_range(&[
    stream(Receiver::MajorGrants, 1, &[GRANTS]),
    stream(Receiver::MajorGrants, 1, &[GRANTS]),
])];
static DEFERRED_ADDRESS: [StreamSet; 1] =
    [last_range(&[stream(Receiver::Deferred, 12, &[GRANTS])])];
static MAINNET_ADDRESS: [StreamSet; 1] = [last_range(&[stream(
    Receiver::MajorGrants,
    8,
    &["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"],
)])];
static RANGE: [StreamSet; 1] = [StreamSet {
    start: 10,
    end: 9,
    ends_at_third_halving: false,
    streams: &[],
}];
/// A set that ends at the third halving, so that NU7 moves its end.
static LATE_END: [StreamSet; 1] = [StreamSet {
    start: 4_300_000,
    end: u32::MAX,
    ends_at_third_halving: true,
    streams: &[stream(Receiver::Deferred, 12, &[])],
}];
/// 21,000,000 ZEC in zatoshis.
const MAX_MONEY: u64 = 2_100_000_000_000_000;
static DISBURSEMENT_SUM: [Disbursement; 2] = [
    Disbursement {
        count: 1,
        value: MAX_MONEY,
        address: LOCKBOX,
    },
    Disbursement {
        count: 1,
        value: 1,
        address: LOCKBOX,
    },
];
static DISBURSEMENT_PRODUCT: [Disbursement; 1] = [Disbursement {
    count: 2,
    value: u64::MAX / 2 + 1,
    address: LOCKBOX,
}];
static DISBURSEMENT_MAINNET: [Disbursement; 1] = [Disbursement {
    count: 10,
    value: 787_500_000_000,
    address: "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo",
}];
static FOUNDERS_P2PKH: [&str; 1] = ["tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"];

/// Each value that a rule would later meet as a panic, or as a rule without code, is an
/// error of [`ChainSpec::network`].
#[test]
fn a_spec_that_a_rule_cannot_apply_is_refused() {
    let refused = |change: fn(&mut ChainSpec)| {
        let mut spec = forknet_spec();
        change(&mut spec);
        spec.network().expect_err("refused")
    };
    // 23 blocks before Blossom are 46 blocks after it: 0 blocks for each of 48 periods.
    assert_eq!(
        refused(|spec| spec.params.pre_blossom_halving_interval = 23),
        ChainSpecError::HalvingInterval(23)
    );
    assert_eq!(
        refused(|spec| spec.params.pre_blossom_halving_interval = u32::MAX / 2 + 1),
        ChainSpecError::HalvingInterval(u32::MAX / 2 + 1)
    );
    assert_eq!(
        refused(|spec| spec.params.slow_start_interval = 1),
        ChainSpecError::SlowStart(1)
    );
    // A halving interval of 24 blocks has the first halving inside the slow start.
    assert_eq!(
        refused(|spec| spec.params.pre_blossom_halving_interval = 24),
        ChainSpecError::SlowStart(20_000)
    );
    // With the 25 s blocks from NU7, no height reaches a halving of 2,000,000,000 blocks
    // of 150 s.
    assert_eq!(
        refused(|spec| spec.params.pre_blossom_halving_interval = 2_000_000_000),
        ChainSpecError::NoFirstHalving
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &LATE_END),
        ChainSpecError::StreamEnd(u32::MAX)
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &RANGE),
        ChainSpecError::StreamRange { start: 10, end: 9 }
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &RECEIVERS),
        ChainSpecError::StreamReceiver(Receiver::MajorGrants)
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &NUMERATORS),
        ChainSpecError::StreamNumerators(101)
    );
    assert_eq!(
        refused(|spec| spec.funding_streams = &DEFERRED_ADDRESS),
        ChainSpecError::DeferredAddress
    );
    assert_eq!(
        refused(|spec| spec.lockbox_disbursements = &DISBURSEMENT_SUM),
        ChainSpecError::DisbursementAmount
    );
    assert_eq!(
        refused(|spec| spec.lockbox_disbursements = &DISBURSEMENT_PRODUCT),
        ChainSpecError::DisbursementAmount
    );
    // Each address of a spec has the encoding of its network type.
    let address = |change: fn(&mut ChainSpec)| match refused(change) {
        ChainSpecError::Address { address, .. } => address,
        other => panic!("{other:?} is not an address error"),
    };
    assert_eq!(
        address(|spec| spec.funding_streams = &MAINNET_ADDRESS),
        "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"
    );
    assert_eq!(
        address(|spec| spec.lockbox_disbursements = &DISBURSEMENT_MAINNET),
        "t3ev37Q2uL1sfTsiJQJiWJoFzQpDhmnUwYo"
    );
    assert_eq!(
        address(|spec| spec.founders_addresses = &FOUNDERS_P2PKH),
        "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"
    );

    // Mainnet needs the ZIP 2008 rule at NU7, and this crate has no code for it.
    let mut mainnet = Network::Mainnet.spec().clone();
    mainnet.activation_heights[Upgrade::Nu7 as usize] = Some(4_000_000);
    assert_eq!(mainnet.network(), Err(ChainSpecError::MainnetNu7));
}
