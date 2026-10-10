//! Conformance of the subsidy, funding stream, lockbox and coinbase rules of
//! hayai-consensus.
//!
//! 1. The coinbase of every published-valid block vector of `tests/vectors/` passes
//!    `CoinbaseTerms::check`: the founders' reward output before Canopy, the funding
//!    stream outputs from Canopy, the value rule.
//! 2. The schedules agree with the baselines `zakura-chain` and `zebra-chain` at every
//!    boundary height and at a sample of the heights between them: the block subsidy, the
//!    founders' reward and its script, each funding stream value and script, the lockbox
//!    disbursement outputs.

// The block test uses every item of the module. This test uses the vector loader only.
#[allow(dead_code)]
#[path = "conformance/vectors.rs"]
mod vectors;

use std::collections::BTreeMap;

use hayai_consensus::coinbase::{CoinbaseOutput, OutputKind, ShieldedBalances};
use hayai_consensus::funding::Receiver;
use hayai_wire::RawBlock;

use vectors::VectorSet;

/// More than every fee total: 21 million ZEC in zatoshis.
const MAX_MONEY: u64 = 2_100_000_000_000_000;

#[test]
fn coinbases_of_the_block_vectors_pass_the_coinbase_check() {
    let set = VectorSet::load();
    let mut checked = 0;
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for vector in set.blocks.iter().filter(|v| !v.invalid && v.height > 0) {
        let network = vector.net.network();
        let rules = vector.net.rules(vector.height);
        let raw = RawBlock::parse(vector.bytes.clone(), rules.branch_id)
            .unwrap_or_else(|e| panic!("{}: {e}", vector.name));
        let coinbase = &raw.txs[0].tx;
        let outputs: Vec<CoinbaseOutput> = coinbase
            .transparent_bundle()
            .expect("a coinbase has a transparent bundle")
            .vout
            .iter()
            .map(|out| CoinbaseOutput {
                value: out.value().into_u64(),
                script: out.script_pubkey().0 .0.to_vec(),
            })
            .collect();
        let shielded = ShieldedBalances {
            sapling: coinbase
                .sapling_bundle()
                .map_or(0, |bundle| i64::from(*bundle.value_balance())),
            orchard: coinbase
                .orchard_bundle()
                .map_or(0, |bundle| i64::from(*bundle.value_balance())),
            ironwood: 0,
        };
        let terms = hayai_consensus::coinbase::terms_at(network, vector.height).unwrap();
        // Every vector is before NU6, so the value rule is a limit. A block with one
        // transaction has no fees. The fees of the other blocks need the spent coins, which
        // the vector set does not have: the limit is then open and the required outputs
        // are the check.
        assert!(!terms.exact_value, "{}", vector.name);
        let fees = if raw.txs.len() == 1 { 0 } else { MAX_MONEY };
        if let Err(e) = terms.check(&outputs, shielded, fees) {
            panic!("{}: {e}", vector.name);
        }
        assert!(!terms.required.is_empty(), "{}", vector.name);
        // A coinbase without one of its outputs fails when the output is a required one.
        for required in &terms.required {
            let index = outputs
                .iter()
                .position(|output| {
                    output.value == required.value && output.script[..] == required.script[..]
                })
                .expect("the check passed");
            let mut short = outputs.clone();
            short.remove(index);
            let Err(_) = terms.check(&short, shielded, fees) else {
                panic!(
                    "{}: a coinbase without {:?} passes",
                    vector.name, required.kind
                );
            };
            *by_kind.entry(format!("{:?}", required.kind)).or_default() += 1;
        }
        checked += 1;
    }
    // 43 Mainnet and 47 Testnet vectors, less the two genesis blocks and one invalid block.
    assert_eq!(checked, 87);
    eprintln!("required outputs in the vectors: {by_kind:?}");
    assert!(by_kind[&format!("{:?}", OutputKind::FoundersReward)] >= 50);
    for receiver in [
        Receiver::Ecc,
        Receiver::ZcashFoundation,
        Receiver::MajorGrants,
    ] {
        assert!(by_kind[&format!("{:?}", OutputKind::FundingStream(receiver))] >= 15);
    }
}

/// The comparison of the schedules with `zakura-chain` and `zebra-chain`.
#[cfg(feature = "baselines")]
mod baselines {
    use hayai_consensus::{Network, Upgrade};

    use super::*;

    /// The heights of the comparison: every height that changes a schedule with its two
    /// neighbours, the address period boundaries, and one height in each 9,973 blocks up to
    /// `end`.
    fn heights(network: Network, end: u32) -> Vec<u32> {
        let mut boundaries: Vec<u32> = vec![1, 10_000, 20_000, 17_709, 2 * 17_709];
        boundaries.extend(
            Upgrade::ALL
                .into_iter()
                .filter_map(|upgrade| network.activation_height(upgrade)),
        );
        boundaries.extend(match network {
            // Halvings, stream ranges, founders' address changes after Blossom.
            Network::Mainnet => vec![
                1_046_400, 2_726_400, 3_146_400, 4_406_400, 6_086_400, 655_582, 691_000,
            ],
            Network::Testnet => vec![
                1_116_000, 2_796_000, 2_976_000, 3_396_000, 3_536_500, 4_476_000, 601_876, 637_294,
            ],
            Network::Regtest | Network::Custom(_) => vec![],
        });
        // Address period boundaries: every 35,000 blocks from the first halving, in both
        // directions.
        let first_halving = match network {
            Network::Mainnet => 1_046_400,
            _ => 1_116_000,
        };
        boundaries.extend((first_halving % 35_000..end).step_by(35_000));
        let mut heights: Vec<u32> = boundaries
            .into_iter()
            .flat_map(|height| [height.saturating_sub(1).max(1), height, height + 1])
            .chain((1..end).step_by(9_973))
            .filter(|height| *height < end)
            .collect();
        heights.sort_unstable();
        heights.dedup();
        heights
    }

    /// What hayai-consensus and a baseline say about one height, in one form.
    #[derive(Debug, PartialEq, Eq)]
    struct Facts {
        subsidy: u64,
        deferred: u64,
        /// (value, script) of the founders' reward output.
        founders: Option<(u64, Vec<u8>)>,
        /// (value, script) of each funding stream with an address, sorted.
        streams: Vec<(u64, Vec<u8>)>,
        /// (value, script) of each lockbox disbursement output.
        disbursements: Vec<(u64, Vec<u8>)>,
    }

    fn hayai_facts(network: Network, height: u32) -> Facts {
        let terms = hayai_consensus::coinbase::terms_at(network, height).unwrap();
        let of_kind = |wanted: fn(&OutputKind) -> bool| -> Vec<(u64, Vec<u8>)> {
            let mut outputs: Vec<(u64, Vec<u8>)> = terms
                .required
                .iter()
                .filter(|output| wanted(&output.kind))
                .map(|output| (output.value, output.script.to_vec()))
                .collect();
            outputs.sort();
            outputs
        };
        Facts {
            subsidy: terms.subsidy.total,
            deferred: terms.subsidy.deferred,
            founders: of_kind(|kind| matches!(kind, OutputKind::FoundersReward)).pop(),
            streams: of_kind(|kind| matches!(kind, OutputKind::FundingStream(_))),
            disbursements: of_kind(|kind| matches!(kind, OutputKind::LockboxDisbursement)),
        }
    }

    /// The facts of a baseline crate. `zakura-chain` and `zebra-chain` have the same
    /// interface except for `block_subsidy`, which `$subsidy` adapts. The funding stream
    /// address index is the formula of their consensus crates
    /// (`zakura-consensus/src/block/subsidy.rs:18-43`), which the chain crates do not export.
    macro_rules! baseline_facts {
        ($name:ident, $chain:ident, $subsidy:expr) => {
            fn $name(network: &$chain::parameters::Network, height: u32) -> Facts {
                use $chain::amount::{Amount, NonNegative};
                use $chain::block::Height;
                use $chain::parameters::subsidy::{
                    founders_reward, founders_reward_address, funding_stream_address_period,
                    funding_stream_values, FundingStreamReceiver,
                };
                use $chain::parameters::NetworkUpgrade;

                let zatoshis = |amount: Amount<NonNegative>| u64::from(amount);
                let height = Height(height);
                let block_subsidy: fn(Height, &$chain::parameters::Network) -> Amount<NonNegative> =
                    $subsidy;
                let subsidy = block_subsidy(height, network);

                // Zakura and Zebra check the founders' reward in `subsidy_is_valid` before
                // Canopy, above the genesis block, below the first halving, when the subsidy
                // is not 0.
                let founders = (zatoshis(subsidy) > 0
                    && NetworkUpgrade::current(network, height) < NetworkUpgrade::Canopy)
                    .then(|| {
                        let address = founders_reward_address(network, height)
                            .expect("a founders' address exists before the first halving");
                        (
                            zatoshis(founders_reward(network, height)),
                            address.script().as_raw_bytes().to_vec(),
                        )
                    });

                let mut deferred = 0;
                let mut streams = Vec::new();
                for (receiver, value) in funding_stream_values(height, network, subsidy).unwrap() {
                    if receiver == FundingStreamReceiver::Deferred {
                        deferred += zatoshis(value);
                        continue;
                    }
                    let set = network.funding_streams(height).unwrap();
                    let index = funding_stream_address_period(height, network)
                        - funding_stream_address_period(set.height_range().start, network);
                    let address = &set.recipient(receiver).unwrap().addresses()[index as usize];
                    streams.push((zatoshis(value), address.script().as_raw_bytes().to_vec()));
                }
                streams.sort();

                let disbursements = if zatoshis(subsidy) > 0 {
                    network
                        .lockbox_disbursements(height)
                        .into_iter()
                        .map(|(address, value)| {
                            (zatoshis(value), address.script().as_raw_bytes().to_vec())
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                Facts {
                    subsidy: zatoshis(subsidy),
                    deferred,
                    founders,
                    streams,
                    disbursements,
                }
            }
        };
    }

    baseline_facts!(zakura_facts, zk_chain, |height, network| {
        zk_chain::parameters::subsidy::block_subsidy(height, network, None).unwrap()
    });
    baseline_facts!(zebra_facts, zb_chain, |height, network| {
        zb_chain::parameters::subsidy::block_subsidy(height, network).unwrap()
    });

    /// The first height that the comparison leaves out: the NU7 activation height of the
    /// baseline `nu7`, or of hayai, or a height above the last funding stream and the third
    /// halving.
    fn comparison_end(network: Network, baseline_nu7: Option<u32>) -> u32 {
        [baseline_nu7, network.activation_height(Upgrade::Nu7)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(6_100_000)
    }

    #[test]
    fn schedules_match_zakura_chain() {
        use zk_chain::parameters::{Network as ZkNetwork, NetworkUpgrade};
        for (network, baseline) in [
            (Network::Mainnet, ZkNetwork::Mainnet),
            (Network::Testnet, ZkNetwork::new_default_testnet()),
        ] {
            let nu7 = NetworkUpgrade::Nu7
                .activation_height(&baseline)
                .map(|height| height.0);
            let heights = heights(network, comparison_end(network, nu7));
            assert!(heights.len() > 400);
            for height in heights {
                assert_eq!(
                    hayai_facts(network, height),
                    zakura_facts(&baseline, height),
                    "{network:?} {height}"
                );
            }
        }
    }

    #[test]
    fn schedules_match_zebra_chain() {
        use zb_chain::parameters::{Network as ZbNetwork, NetworkUpgrade};
        for (network, baseline) in [
            (Network::Mainnet, ZbNetwork::Mainnet),
            (Network::Testnet, ZbNetwork::new_default_testnet()),
        ] {
            let nu7 = NetworkUpgrade::Nu7
                .activation_height(&baseline)
                .map(|height| height.0);
            let heights = heights(network, comparison_end(network, nu7));
            assert!(heights.len() > 400);
            for height in heights {
                assert_eq!(
                    hayai_facts(network, height),
                    zebra_facts(&baseline, height),
                    "{network:?} {height}"
                );
            }
        }
    }
}
