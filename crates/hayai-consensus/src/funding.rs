//! Funding streams (protocol specification §7.10, ZIP 207, ZIP 214, ZIP 1014, ZIP 1015).
//!
//! A funding stream gives a share of the block subsidy to a recipient in a range of
//! heights. The recipient is an address that changes with the address period, or the
//! deferred pool (lockbox). The constants are those of Zakura
//! (`zakura-chain/src/parameters/network/subsidy/constants/{mainnet,testnet}.rs`), which
//! are those of Zebra and zcashd. A Regtest network takes its funding streams from its
//! configuration ([`crate::RegtestConfig::with_funding_streams`]).
//!
//! NU7 changes two things (ZIP 218, ZIP 214 revision 3): the end of the last stream set
//! moves with the third halving ([`nu7_adjusted_end`]), and an address period has 3 times
//! the blocks from the NU7 height ([`address_period`]). The tables hold the heights
//! before NU7. ZIP 2008 changes the Mainnet recipient of the last stream set at NU7:
//! Mainnet has no NU7 height, and this module has no code for ZIP 2008.

use crate::subsidy::halving_height;
use crate::{
    Network, RegtestConfigError, RegtestFundingStreams, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    POST_NU7_TARGET_SPACING,
};

/// The recipient of a funding stream. The names in a configuration file are those of
/// Zakura (`FundingStreamReceiver`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
pub enum Receiver {
    /// Electric Coin Company (ZIP 1014).
    #[serde(rename = "ECC")]
    Ecc,
    /// Zcash Foundation (ZIP 1014).
    ZcashFoundation,
    /// Major Grants (ZIP 1014), then Zcash Community Grants (ZIP 1015).
    MajorGrants,
    /// The deferred pool (ZIP 1015, `FS_DEFERRED`): no coinbase output.
    Deferred,
}

/// One funding stream at one height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundingStream {
    pub receiver: Receiver,
    /// `fs.Value(height)` in zatoshis.
    pub value: u64,
    /// The Base58Check address of the recipient at the height. `None` for
    /// [`Receiver::Deferred`].
    pub address: Option<&'static str>,
}

/// `fs.Denominator` of every stream.
pub(crate) const DENOMINATOR: u64 = 100;
/// Address periods in one post-Blossom halving interval:
/// `FSRecipientChangeInterval = PostBlossomHalvingInterval / 48`.
const PERIODS_PER_HALVING_INTERVAL: u32 = 48;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Stream {
    receiver: Receiver,
    /// `fs.Numerator`.
    numerator: u64,
    /// The address of each address period of the stream, from the period of the start
    /// height. A list with one address gives that address to every period. Empty for the
    /// deferred pool.
    addresses: &'static [&'static str],
}

/// The streams of one range of heights.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StreamSet {
    /// `fs.StartHeight`.
    start: u32,
    /// `fs.EndHeight`: the first height after the streams, before NU7.
    end: u32,
    /// The set is the one of ZIP 214 revision 2, which ends at the third halving: NU7
    /// moves its end ([`nu7_adjusted_end`]).
    ends_at_third_halving: bool,
    streams: &'static [Stream],
}

/// `NU7PoWTargetSpacingRatio` of ZIP 218: 3.
const NU7_SPACING_RATIO: u32 = POST_BLOSSOM_TARGET_SPACING / POST_NU7_TARGET_SPACING;

/// The end height `end` of a stream set that ends at the third halving, on a network with
/// the NU7 height `nu7` (ZIP 214 revision 3; Zakura `nu7_adjusted_funding_stream_height`,
/// `subsidy.rs:388-402`): an end above the NU7 height `A` moves to `A + 3 * (end - A)`.
fn nu7_adjusted_end(end: u32, nu7: Option<u32>) -> u32 {
    match nu7 {
        Some(nu7) if nu7 < end => {
            let moved = (end - nu7)
                .checked_mul(NU7_SPACING_RATIO)
                .and_then(|blocks| nu7.checked_add(blocks));
            let Some(moved) = moved else {
                unreachable!("the end heights of the tables are far below the largest height");
            };
            moved
        }
        _ => end,
    }
}

/// The streams with the deferred pool, from NU6 (ZIP 1015) and from NU6.1 (ZIP 214
/// revision 2): 12 % to the lockbox and 8 % to Zcash Community Grants.
const fn lockbox_streams(fpf_addresses: &'static [&'static str]) -> [Stream; 2] {
    [
        Stream {
            receiver: Receiver::Deferred,
            numerator: 12,
            addresses: &[],
        },
        Stream {
            receiver: Receiver::MajorGrants,
            numerator: 8,
            addresses: fpf_addresses,
        },
    ]
}

/// Zakura `mainnet::FUNDING_STREAMS` (`constants/mainnet.rs:233-289`).
static MAINNET: [StreamSet; 3] = [
    StreamSet {
        start: 1_046_400,
        end: 2_726_400,
        ends_at_third_halving: false,
        streams: &[
            Stream {
                receiver: Receiver::Ecc,
                numerator: 7,
                addresses: &MAINNET_ECC_ADDRESSES,
            },
            Stream {
                receiver: Receiver::ZcashFoundation,
                numerator: 5,
                addresses: &["t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1"],
            },
            Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                addresses: &["t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym"],
            },
        ],
    },
    StreamSet {
        start: 2_726_400,
        end: 3_146_400,
        ends_at_third_halving: false,
        streams: &lockbox_streams(&["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"]),
    },
    StreamSet {
        start: 3_146_400,
        end: 4_406_400,
        ends_at_third_halving: true,
        streams: &lockbox_streams(&["t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"]),
    },
];

/// Zakura `testnet::FUNDING_STREAMS` (`constants/testnet.rs:212-262`). No stream exists
/// from 3,396,000 to the NU6.1 activation at 3,536,500.
static TESTNET: [StreamSet; 3] = [
    StreamSet {
        start: 1_028_500,
        end: 2_796_000,
        ends_at_third_halving: false,
        streams: &[
            Stream {
                receiver: Receiver::Ecc,
                numerator: 7,
                addresses: &TESTNET_ECC_ADDRESSES,
            },
            Stream {
                receiver: Receiver::ZcashFoundation,
                numerator: 5,
                addresses: &["t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"],
            },
            Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                addresses: &["t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P"],
            },
        ],
    },
    StreamSet {
        start: 2_976_000,
        end: 3_396_000,
        ends_at_third_halving: false,
        streams: &lockbox_streams(&["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"]),
    },
    StreamSet {
        start: 3_536_500,
        end: 4_476_000,
        ends_at_third_halving: true,
        streams: &lockbox_streams(&["t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu"]),
    },
];

/// The stream sets of `network` and the height of its first halving
/// (`HeightForHalving(1)`: Canopy on Mainnet, 1,116,000 on Testnet).
fn schedule(network: Network) -> (&'static [StreamSet], u32) {
    match network {
        Network::Mainnet => (&MAINNET, 1_046_400),
        Network::Testnet => (&TESTNET, 1_116_000),
        Network::Regtest => (&[], 0),
        Network::ConfiguredRegtest(config) => match config.funding_streams() {
            [] => (&[], 0),
            sets => {
                // Zakura `height_for_first_halving` of a configured network.
                let Some(first_halving) = halving_height(network, 1, u32::MAX) else {
                    unreachable!("Regtest has a first halving below the largest height");
                };
                (sets, first_halving)
            }
        },
    }
}

/// The stream sets of a Regtest configuration. The tables stay in memory until the
/// process ends.
pub(crate) fn regtest_sets(sets: &[RegtestFundingStreams]) -> &'static [StreamSet] {
    fn leak<T>(items: Vec<T>) -> &'static [T] {
        Box::leak(items.into_boxed_slice())
    }
    let sets = sets.iter().map(|set| StreamSet {
        start: set.height_range.start,
        end: set.height_range.end,
        ends_at_third_halving: false,
        streams: leak(
            set.recipients
                .iter()
                .map(|recipient| Stream {
                    receiver: recipient.receiver,
                    numerator: recipient.numerator,
                    addresses: leak(
                        recipient
                            .addresses
                            .iter()
                            .map(|address| &*Box::leak(address.clone().into_boxed_str()))
                            .collect(),
                    ),
                })
                .collect(),
        ),
    });
    leak(sets.collect())
}

/// Checks that each stream with an address of the configured Regtest `network` has one
/// address for each address period of its range. Zakura stops at the first block of a
/// period without an address (`funding_stream_address_index`,
/// `zakura-consensus/src/block/subsidy.rs:18-43`).
pub(crate) fn check_address_counts(network: Network) -> Result<(), RegtestConfigError> {
    let (sets, first_halving) = schedule(network);
    for set in sets.iter().filter(|set| set.start < set.end) {
        let periods = address_period(network, first_halving, set.end - 1)
            - address_period(network, first_halving, set.start)
            + 1;
        let Ok(required) = usize::try_from(periods) else {
            unreachable!("the address period does not decrease with the height");
        };
        for stream in set.streams {
            let found = stream.addresses.len();
            if stream.receiver != Receiver::Deferred && found < required {
                return Err(RegtestConfigError::StreamAddresses {
                    receiver: stream.receiver,
                    start: set.start,
                    end: set.end,
                    required,
                    found,
                });
            }
        }
    }
    Ok(())
}

/// The address period of `height` (protocol specification §7.10):
/// `floor((height + PostBlossomHalvingInterval - HeightForHalving(1)) /
/// FSRecipientChangeInterval)`.
///
/// From the NU7 height `A` a period has 3 times the blocks (ZIP 218; Zakura
/// `funding_stream_address_period`, `subsidy.rs:341-370`):
/// `floor((3 * (A + PostBlossomHalvingInterval - HeightForHalving(1)) + (height - A)) /
/// (3 * FSRecipientChangeInterval))`.
fn address_period(network: Network, first_halving: u32, height: u32) -> i64 {
    let interval = network.params().post_blossom_halving_interval();
    let change_interval = i64::from(interval / PERIODS_PER_HALVING_INTERVAL);
    let offset = |height: u32| i64::from(height) + i64::from(interval) - i64::from(first_halving);
    match network.activation_height(Upgrade::Nu7) {
        Some(nu7) if height >= nu7 => {
            let ratio = i64::from(NU7_SPACING_RATIO);
            (ratio * offset(nu7) + i64::from(height - nu7)).div_euclid(ratio * change_interval)
        }
        _ => offset(height).div_euclid(change_interval),
    }
}

/// The funding streams that are active at `height`, with the values for a block subsidy
/// of `subsidy` zatoshis: `fs.Value(height) = floor(subsidy * numerator / 100)`. A subsidy
/// of 0 has no funding stream, and a height before Canopy has none (Zakura
/// `funding_stream_values`).
pub fn funding_streams(network: Network, height: u32, subsidy: u64) -> Vec<FundingStream> {
    let canopy = network.activation_height(Upgrade::Canopy);
    if subsidy == 0 || !matches!(canopy, Some(canopy) if canopy <= height) {
        return Vec::new();
    }
    let (sets, first_halving) = schedule(network);
    let nu7 = network.activation_height(Upgrade::Nu7);
    let end = |set: &StreamSet| match set.ends_at_third_halving {
        true => nu7_adjusted_end(set.end, nu7),
        false => set.end,
    };
    let Some(set) = sets
        .iter()
        .find(|set| (set.start..end(set)).contains(&height))
    else {
        return Vec::new();
    };
    let period = address_period(network, first_halving, height)
        - address_period(network, first_halving, set.start);
    let Ok(period) = usize::try_from(period) else {
        unreachable!("the address period does not decrease with the height");
    };
    set.streams
        .iter()
        .map(|stream| FundingStream {
            receiver: stream.receiver,
            value: subsidy * stream.numerator / DENOMINATOR,
            address: match stream.addresses {
                [] => None,
                [address] => Some(*address),
                addresses => Some(addresses[period]),
            },
        })
        .collect()
}

/// The value of the stream to the deferred pool at `height` for a block subsidy of
/// `subsidy` zatoshis. 0 when no such stream is active.
pub(crate) fn deferred_value(network: Network, height: u32, subsidy: u64) -> u64 {
    funding_streams(network, height, subsidy)
        .into_iter()
        .filter(|stream| stream.receiver == Receiver::Deferred)
        .map(|stream| stream.value)
        .sum()
}

/// Zakura `mainnet::FUNDING_STREAM_ECC_ADDRESSES` (`constants/mainnet.rs:58-107`).
static MAINNET_ECC_ADDRESSES: [&str; 48] = [
    "t3LmX1cxWPPPqL4TZHx42HU3U5ghbFjRiif",
    "t3Toxk1vJQ6UjWQ42tUJz2rV2feUWkpbTDs",
    "t3ZBdBe4iokmsjdhMuwkxEdqMCFN16YxKe6",
    "t3ZuaJziLM8xZ32rjDUzVjVtyYdDSz8GLWB",
    "t3bAtYWa4bi8VrtvqySxnbr5uqcG9czQGTZ",
    "t3dktADfb5Rmxncpe1HS5BRS5Gcj7MZWYBi",
    "t3hgskquvKKoCtvxw86yN7q8bzwRxNgUZmc",
    "t3R1VrLzwcxAZzkX4mX3KGbWpNsgtYtMntj",
    "t3ff6fhemqPMVujD3AQurxRxTdvS1pPSaa2",
    "t3cEUQFG3KYnFG6qYhPxSNgGi3HDjUPwC3J",
    "t3WR9F5U4QvUFqqx9zFmwT6xFqduqRRXnaa",
    "t3PYc1LWngrdUrJJbHkYPCKvJuvJjcm85Ch",
    "t3bgkjiUeatWNkhxY3cWyLbTxKksAfk561R",
    "t3Z5rrR8zahxUpZ8itmCKhMSfxiKjUp5Dk5",
    "t3PU1j7YW3fJ67jUbkGhSRto8qK2qXCUiW3",
    "t3S3yaT7EwNLaFZCamfsxxKwamQW2aRGEkh",
    "t3eutXKJ9tEaPSxZpmowhzKhPfJvmtwTEZK",
    "t3gbTb7brxLdVVghSPSd3ycGxzHbUpukeDm",
    "t3UCKW2LrHFqPMQFEbZn6FpjqnhAAbfpMYR",
    "t3NyHsrnYbqaySoQqEQRyTWkjvM2PLkU7Uu",
    "t3QEFL6acxuZwiXtW3YvV6njDVGjJ1qeaRo",
    "t3PdBRr2S1XTDzrV8bnZkXF3SJcrzHWe1wj",
    "t3ZWyRPpWRo23pKxTLtWsnfEKeq9T4XPxKM",
    "t3he6QytKCTydhpztykFsSsb9PmBT5JBZLi",
    "t3VWxWDsLb2TURNEP6tA1ZSeQzUmPKFNxRY",
    "t3NmWLvZkbciNAipauzsFRMxoZGqmtJksbz",
    "t3cKr4YxVPvPBG1mCvzaoTTdBNokohsRJ8n",
    "t3T3smGZn6BoSFXWWXa1RaoQdcyaFjMfuYK",
    "t3gkDUe9Gm4GGpjMk86TiJZqhztBVMiUSSA",
    "t3eretuBeBXFHe5jAqeSpUS1cpxVh51fAeb",
    "t3dN8g9zi2UGJdixGe9txeSxeofLS9t3yFQ",
    "t3S799pq9sYBFwccRecoTJ3SvQXRHPrHqvx",
    "t3fhYnv1S5dXwau7GED3c1XErzt4n4vDxmf",
    "t3cmE3vsBc5xfDJKXXZdpydCPSdZqt6AcNi",
    "t3h5fPdjJVHaH4HwynYDM5BB3J7uQaoUwKi",
    "t3Ma35c68BgRX8sdLDJ6WR1PCrKiWHG4Da9",
    "t3LokMKPL1J8rkJZvVpfuH7dLu6oUWqZKQK",
    "t3WFFGbEbhJWnASZxVLw2iTJBZfJGGX73mM",
    "t3L8GLEsUn4QHNaRYcX3EGyXmQ8kjpT1zTa",
    "t3PgfByBhaBSkH8uq4nYJ9ZBX4NhGCJBVYm",
    "t3WecsqKDhWXD4JAgBVcnaCC2itzyNZhJrv",
    "t3ZG9cSfopnsMQupKW5v9sTotjcP5P6RTbn",
    "t3hC1Ywb5zDwUYYV8LwhvF5rZ6m49jxXSG5",
    "t3VgMqDL15ZcyQDeqBsBW3W6rzfftrWP2yB",
    "t3LC94Y6BwLoDtBoK2NuewaEbnko1zvR9rm",
    "t3cWCUZJR3GtALaTcatrrpNJ3MGbMFVLRwQ",
    "t3YYF4rPLVxDcF9hHFsXyc5Yq1TFfbojCY6",
    "t3XHAGxRP2FNfhAjxGjxbrQPYtQQjc3RCQD",
];

/// Zakura `testnet::FUNDING_STREAM_ECC_ADDRESSES` (`constants/testnet.rs:57-109`). The
/// first three periods have the same address.
static TESTNET_ECC_ADDRESSES: [&str; 51] = [
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t26ovBdKAJLtrvBsE2QGF4nqBkEuptuPFZz",
    "t2NNHrgPpE388atmWSF4DxAb3xAoW5Yp45M",
    "t2VMN28itPyMeMHBEd9Z1hm6YLkQcGA1Wwe",
    "t2CHa1TtdfUV8UYhNm7oxbzRyfr8616BYh2",
    "t2F77xtr28U96Z2bC53ZEdTnQSUAyDuoa67",
    "t2ARrzhbgcpoVBDPivUuj6PzXzDkTBPqfcT",
    "t278aQ8XbvFR15mecRguiJDQQVRNnkU8kJw",
    "t2Dp1BGnZsrTXZoEWLyjHmg3EPvmwBnPDGB",
    "t2KzeqXgf4ju33hiSqCuKDb8iHjPCjMq9iL",
    "t2Nyxqv1BiWY1eUSiuxVw36oveawYuo18tr",
    "t2DKFk5JRsVoiuinK8Ti6eM4Yp7v8BbfTyH",
    "t2CUaBca4k1x36SC4q8Nc8eBoqkMpF3CaLg",
    "t296SiKL7L5wvFmEdMxVLz1oYgd6fTfcbZj",
    "t29fBCFbhgsjL3XYEZ1yk1TUh7eTusB6dPg",
    "t2FGofLJXa419A76Gpf5ncxQB4gQXiQMXjK",
    "t2ExfrnRVnRiXDvxerQ8nZbcUQvNvAJA6Qu",
    "t28JUffLp47eKPRHKvwSPzX27i9ow8LSXHx",
    "t2JXWPtrtyL861rFWMZVtm3yfgxAf4H7uPA",
    "t2QdgbJoWfYHgyvEDEZBjHmgkr9yNJff3Hi",
    "t2QW43nkco8r32ZGRN6iw6eSzyDjkMwCV3n",
    "t2DgYDXMJTYLwNcxighQ9RCgPxMVATRcUdC",
    "t2Bop7dg33HGZx3wunnQzi2R2ntfpjuti3M",
    "t2HVeEwovcLq9RstAbYkqngXNEsCe2vjJh9",
    "t2HxbP5keQSx7p592zWQ5bJ5GrMmGDsV2Xa",
    "t2TJzUg2matao3mztBRJoWnJY6ekUau6tPD",
    "t29pMzxmo6wod25YhswcjKv3AFRNiBZHuhj",
    "t2QBQMRiJKYjshJpE6RhbF7GLo51yE6d4wZ",
    "t2F5RqnqguzZeiLtYHFx4yYfy6pDnut7tw5",
    "t2CHvyZANE7XCtg8AhZnrcHCC7Ys1jJhK13",
    "t2BRzpMdrGWZJ2upsaNQv6fSbkbTy7EitLo",
    "t2BFixHGQMAWDY67LyTN514xRAB94iEjXp3",
    "t2Uvz1iVPzBEWfQBH1p7NZJsFhD74tKaG8V",
    "t2CmFDj5q6rJSRZeHf1SdrowinyMNcj438n",
    "t2ErNvWEReTfPDBaNizjMPVssz66aVZh1hZ",
    "t2GeJQ8wBUiHKDVzVM5ZtKfY5reCg7CnASs",
    "t2L2eFtkKv1G6j55kLytKXTGuir4raAy3yr",
    "t2EK2b87dpPazb7VvmEGc8iR6SJ289RywGL",
    "t2DJ7RKeZJxdA4nZn8hRGXE8NUyTzjujph9",
    "t2K1pXo4eByuWpKLkssyMLe8QKUbxnfFC3H",
    "t2TB4mbSpuAcCWkH94Leb27FnRxo16AEHDg",
    "t2Phx4gVL4YRnNsH3jM1M7jE4Fo329E66Na",
    "t2VQZGmeNomN8c3USefeLL9nmU6M8x8CVzC",
    "t2RicCvTVTY5y9JkreSRv3Xs8q2K67YxHLi",
    "t2JrSLxTGc8wtPDe9hwbaeUjCrCfc4iZnDD",
    "t2Uh9Au1PDDSw117sAbGivKREkmMxVC5tZo",
    "t2FDwoJKLeEBMTy3oP7RLQ1Fihhvz49a3Bv",
    "t2FY18mrgtb7QLeHA8ShnxLXuW8cNQ2n1v8",
    "t2L15TkDYum7dnQRBqfvWdRe8Yw3jVy9z7g",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coinbase::address_script;

    fn stream(network: Network, height: u32, receiver: Receiver) -> Option<FundingStream> {
        funding_streams(network, height, 100)
            .into_iter()
            .find(|stream| stream.receiver == receiver)
    }

    /// The values of Zakura's `test_funding_stream_values`
    /// (`zakura-consensus/src/block/subsidy/tests.rs:19-111`).
    #[test]
    fn mainnet_values_match_the_zakura_test_values() {
        let values = |height| -> Vec<(Receiver, u64)> {
            let subsidy = crate::subsidy::total_subsidy(Network::Mainnet, height);
            funding_streams(Network::Mainnet, height, subsidy)
                .into_iter()
                .map(|stream| (stream.receiver, stream.value))
                .collect()
        };
        let dev_fund = vec![
            (Receiver::Ecc, 21_875_000),
            (Receiver::ZcashFoundation, 15_625_000),
            (Receiver::MajorGrants, 25_000_000),
        ];
        let lockbox = vec![
            (Receiver::Deferred, 18_750_000),
            (Receiver::MajorGrants, 12_500_000),
        ];
        assert_eq!(values(1_046_399), vec![]);
        for height in [1_046_400, 1_046_401, 2_726_399] {
            assert_eq!(values(height), dev_fund, "{height}");
        }
        for height in [
            2_726_400, 2_726_401, 3_146_399, 3_146_400, 3_146_401, 4_406_399,
        ] {
            assert_eq!(values(height), lockbox, "{height}");
        }
        for height in [4_406_400, 4_406_401] {
            assert_eq!(values(height), vec![], "{height}");
        }
    }

    #[test]
    fn testnet_streams_have_their_ranges_and_values() {
        let values = |height| -> Vec<(Receiver, u64)> {
            let subsidy = crate::subsidy::total_subsidy(Network::Testnet, height);
            funding_streams(Network::Testnet, height, subsidy)
                .into_iter()
                .map(|stream| (stream.receiver, stream.value))
                .collect()
        };
        assert_eq!(values(1_028_499), vec![]);
        // Canopy to the first halving: shares of 6.25 ZEC.
        assert_eq!(
            values(1_028_500),
            vec![
                (Receiver::Ecc, 43_750_000),
                (Receiver::ZcashFoundation, 31_250_000),
                (Receiver::MajorGrants, 50_000_000),
            ]
        );
        assert_eq!(values(1_115_999), values(1_028_500));
        let dev_fund = vec![
            (Receiver::Ecc, 21_875_000),
            (Receiver::ZcashFoundation, 15_625_000),
            (Receiver::MajorGrants, 25_000_000),
        ];
        assert_eq!(values(1_116_000), dev_fund);
        assert_eq!(values(2_795_999), dev_fund);
        // The first streams end at the second halving. NU6 activates later.
        assert_eq!(values(2_796_000), vec![]);
        assert_eq!(values(2_975_999), vec![]);
        let lockbox = vec![
            (Receiver::Deferred, 18_750_000),
            (Receiver::MajorGrants, 12_500_000),
        ];
        assert_eq!(values(2_976_000), lockbox);
        assert_eq!(values(3_395_999), lockbox);
        assert_eq!(values(3_396_000), vec![]);
        assert_eq!(values(3_536_499), vec![]);
        assert_eq!(values(3_536_500), lockbox);
        assert_eq!(values(4_465_025), lockbox);
    }

    /// Testnet across NU7 (ZIP 218, ZIP 214 revision 3): the subsidy of one block is a
    /// third, so the values are a third; the last stream set ends at the third halving,
    /// which moves from 4,476,000 to `A + 3 * (4,476,000 - A)` = 4,497,948.
    #[test]
    fn the_last_testnet_streams_follow_nu7() {
        let nu7 = 4_465_026;
        assert_eq!(Network::Testnet.activation_height(Upgrade::Nu7), Some(nu7));
        let values = |height| -> Vec<(Receiver, u64)> {
            let subsidy = crate::subsidy::total_subsidy(Network::Testnet, height);
            funding_streams(Network::Testnet, height, subsidy)
                .into_iter()
                .map(|stream| (stream.receiver, stream.value))
                .collect()
        };
        let before = vec![
            (Receiver::Deferred, 18_750_000),
            (Receiver::MajorGrants, 12_500_000),
        ];
        // 12 % and 8 % of floor(1,250,000,000 * 25 / 150 / 4) = 52,083,333.
        let after = vec![
            (Receiver::Deferred, 6_249_999),
            (Receiver::MajorGrants, 4_166_666),
        ];
        assert_eq!(values(nu7 - 1), before);
        for height in [nu7, nu7 + 1, 4_476_000, 4_497_947] {
            assert_eq!(values(height), after, "{height}");
        }
        assert_eq!(nu7_adjusted_end(4_476_000, Some(nu7)), 4_497_948);
        assert_eq!(crate::subsidy::halving(Network::Testnet, 4_497_947), 2);
        assert_eq!(crate::subsidy::halving(Network::Testnet, 4_497_948), 3);
        for height in [4_497_948, 4_497_949] {
            assert_eq!(values(height), vec![], "{height}");
        }
        // The recipient with an address keeps it across NU7.
        for height in [nu7 - 1, nu7, 4_497_947] {
            assert_eq!(
                stream(Network::Testnet, height, Receiver::MajorGrants).and_then(|s| s.address),
                Some("t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu")
            );
        }
    }

    /// An end at or below the NU7 height, and every end on a network without NU7, does
    /// not move (the values of Zakura's doc of `nu7_adjusted_funding_stream_height`).
    #[test]
    fn only_an_end_above_the_nu7_height_moves() {
        assert_eq!(nu7_adjusted_end(4_406_400, None), 4_406_400);
        assert_eq!(nu7_adjusted_end(100, Some(100)), 100);
        assert_eq!(nu7_adjusted_end(100, Some(101)), 100);
        assert_eq!(nu7_adjusted_end(101, Some(100)), 103);
        // Mainnet has no NU7 height: its last stream set keeps its end.
        assert_eq!(Network::Mainnet.activation_height(Upgrade::Nu7), None);
        let subsidy = crate::subsidy::total_subsidy(Network::Mainnet, 4_406_399);
        assert_eq!(
            funding_streams(Network::Mainnet, 4_406_399, subsidy).len(),
            2
        );
        assert_eq!(
            funding_streams(Network::Mainnet, 4_406_400, subsidy),
            vec![]
        );
    }

    /// The address period before and from the NU7 height `A`: both formulas agree at `A`,
    /// and from `A` a period has 3 times the blocks.
    #[test]
    fn an_address_period_has_three_times_the_blocks_from_nu7() {
        let network = Network::Testnet;
        let nu7 = 4_465_026;
        let first_halving = 1_116_000;
        let period = |height| address_period(network, first_halving, height);
        // FSRecipientChangeInterval is 1,680,000 / 48 = 35,000 blocks.
        let at_nu7 = (i64::from(nu7) + 1_680_000 - 1_116_000) / 35_000;
        assert_eq!(period(nu7 - 1), at_nu7);
        assert_eq!(period(nu7), at_nu7);
        // The period of `A` started at this height, and it has this number of blocks left
        // at the old spacing.
        let start = 1_116_000 - 1_680_000 + at_nu7 * 35_000;
        let left = start + 35_000 - i64::from(nu7);
        let Ok(next) = u32::try_from(i64::from(nu7) + 3 * left) else {
            panic!("a height");
        };
        assert_eq!(period(next - 1), at_nu7);
        assert_eq!(period(next), at_nu7 + 1);
        assert_eq!(period(next + 3 * 35_000 - 1), at_nu7 + 1);
        assert_eq!(period(next + 3 * 35_000), at_nu7 + 2);
    }

    /// ZIP 2008 changes the Mainnet recipient of the last stream set from the first
    /// address period at or after NU7 (Zakura `nu7_fpf_addresses`,
    /// `subsidy/constants/mainnet.rs:196-221`). This module has no code for it, and no
    /// block needs it while Mainnet has no NU7 height. This test fails when Mainnet gets
    /// a height: add the rule then.
    #[test]
    fn zip_2008_has_no_code_while_mainnet_has_no_nu7_height() {
        assert_eq!(Network::Mainnet.activation_height(Upgrade::Nu7), None);
    }

    #[test]
    fn regtest_has_no_funding_stream() {
        for height in [0, 1, 287, 1_046_400, 2_726_400] {
            assert_eq!(
                funding_streams(Network::Regtest, height, 625_000_000),
                vec![]
            );
        }
    }

    #[test]
    fn a_subsidy_of_zero_has_no_funding_stream() {
        assert_eq!(funding_streams(Network::Mainnet, 2_726_400, 0), vec![]);
    }

    #[test]
    fn the_ecc_address_changes_at_each_period_boundary() {
        // Mainnet: 35,000 blocks in a period, and the stream starts at a period start.
        let ecc = |height| {
            stream(Network::Mainnet, height, Receiver::Ecc)
                .unwrap()
                .address
        };
        for index in 0..48u32 {
            let start = 1_046_400 + index * 35_000;
            let expected = Some(MAINNET_ECC_ADDRESSES[index as usize]);
            assert_eq!(ecc(start), expected, "period {index}");
            assert_eq!(ecc(start + 34_999), expected, "period {index}");
        }
        assert_eq!(ecc(1_046_400), Some("t3LmX1cxWPPPqL4TZHx42HU3U5ghbFjRiif"));
        assert_eq!(ecc(2_726_399), Some("t3XHAGxRP2FNfhAjxGjxbrQPYtQQjc3RCQD"));

        // Testnet: the stream starts at Canopy, 17,500 blocks before the end of period
        // 45. The first halving at 1,116,000 is the start of period 48.
        let ecc = |height| {
            stream(Network::Testnet, height, Receiver::Ecc)
                .unwrap()
                .address
        };
        assert_eq!(ecc(1_028_500), Some(TESTNET_ECC_ADDRESSES[0]));
        assert_eq!(ecc(1_045_999), Some(TESTNET_ECC_ADDRESSES[0]));
        assert_eq!(ecc(1_046_000), Some(TESTNET_ECC_ADDRESSES[1]));
        assert_eq!(ecc(1_080_999), Some(TESTNET_ECC_ADDRESSES[1]));
        assert_eq!(ecc(1_081_000), Some(TESTNET_ECC_ADDRESSES[2]));
        assert_eq!(ecc(1_115_999), Some(TESTNET_ECC_ADDRESSES[2]));
        assert_eq!(ecc(1_116_000), Some(TESTNET_ECC_ADDRESSES[3]));
        assert_eq!(ecc(2_760_999), Some(TESTNET_ECC_ADDRESSES[49]));
        assert_eq!(ecc(2_761_000), Some(TESTNET_ECC_ADDRESSES[50]));
        assert_eq!(ecc(2_795_999), Some(TESTNET_ECC_ADDRESSES[50]));
    }

    /// Zakura's address counts (`FUNDING_STREAMS_NUM_ADDRESSES`,
    /// `POST_NU6_FUNDING_STREAMS_NUM_ADDRESSES`, `POST_NU6_1_FUNDING_STREAMS_NUM_ADDRESSES`
    /// in `constants/{mainnet,testnet}.rs`): the periods of each range.
    #[test]
    fn each_range_has_the_periods_of_the_zakura_address_counts() {
        for (network, counts) in [
            (Network::Mainnet, [48, 12, 36]),
            (Network::Testnet, [51, 13, 27]),
        ] {
            let (sets, first_halving) = schedule(network);
            for (set, count) in sets.iter().zip(counts) {
                let periods = address_period(network, first_halving, set.end - 1)
                    - address_period(network, first_halving, set.start)
                    + 1;
                assert_eq!(periods, count, "{network:?} {}", set.start);
                for stream in set.streams {
                    let addresses = stream.addresses.len();
                    assert!(
                        addresses <= 1 || addresses == count as usize,
                        "{network:?} {} {:?}",
                        set.start,
                        stream.receiver
                    );
                }
            }
        }
    }

    #[test]
    fn the_first_halving_height_is_the_first_height_with_halving_one() {
        for network in [Network::Mainnet, Network::Testnet] {
            let (_, first_halving) = schedule(network);
            assert_eq!(crate::subsidy::halving(network, first_halving - 1), 0);
            assert_eq!(crate::subsidy::halving(network, first_halving), 1);
        }
    }

    #[test]
    fn the_ranges_are_in_order_and_start_at_the_upgrades() {
        use crate::Upgrade;
        for network in [Network::Mainnet, Network::Testnet] {
            let (sets, _) = schedule(network);
            assert!(sets.windows(2).all(|pair| pair[0].end <= pair[1].start));
            let starts: Vec<Option<u32>> = sets.iter().map(|set| Some(set.start)).collect();
            assert_eq!(
                starts,
                [Upgrade::Canopy, Upgrade::Nu6, Upgrade::Nu6_1]
                    .map(|upgrade| network.activation_height(upgrade))
            );
        }
    }

    #[test]
    fn every_address_is_a_p2sh_address_of_its_network() {
        for network in [Network::Mainnet, Network::Testnet] {
            let (sets, _) = schedule(network);
            for stream in sets.iter().flat_map(|set| set.streams) {
                for address in stream.addresses {
                    let script = address_script(network, address);
                    assert_eq!(script.len(), 23, "{address}");
                    assert_eq!((script[0], script[1], script[22]), (0xa9, 0x14, 0x87));
                }
            }
        }
    }

    #[test]
    fn the_deferred_stream_has_no_address() {
        let deferred = stream(Network::Mainnet, 2_726_400, Receiver::Deferred).unwrap();
        assert_eq!((deferred.value, deferred.address), (12, None));
        let grants = stream(Network::Mainnet, 3_146_400, Receiver::MajorGrants).unwrap();
        assert_eq!(grants.value, 8);
        assert_eq!(grants.address, Some("t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"));
    }
}
