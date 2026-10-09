//! Funding streams (protocol specification §7.10, ZIP 207, ZIP 214, ZIP 1014, ZIP 1015):
//! the stream tables of Mainnet and Testnet with their addresses, and the wrappers of
//! `hayai_consensus_core::funding` for a caller with a [`Network`].
//!
//! The constants are those of Zakura
//! (`zakura-chain/src/parameters/network/subsidy/constants/{mainnet,testnet}.rs`), which
//! are those of Zebra and zcashd. The stream sets of a network are data of its spec
//! ([`crate::ChainSpec::funding_streams`]), and the core reads the same sets with the
//! addresses decoded to scripts ([`crate::CoreSpec::funding_streams`]). A Regtest network
//! takes its funding streams from its configuration
//! ([`crate::RegtestConfig::with_funding_streams`]).

use hayai_consensus_core::funding as core;
pub use hayai_consensus_core::funding::{
    FundingStream, Receiver, Stream as CoreStream, StreamSet as CoreStreamSet,
};
use hayai_consensus_core::P2shScript;
use hayai_crypto::zcash_protocol::consensus::NetworkType;

use crate::address::{p2sh_script, script_of, scripts_of};
use crate::network::check_address;
use crate::{ChainSpecError, ConsensusError, Network, RegtestFundingStreams};

/// The names of a [`Receiver`] in a configuration file: those of Zakura
/// (`FundingStreamReceiver`).
#[derive(serde::Deserialize)]
#[serde(remote = "Receiver")]
pub(crate) enum ReceiverDef {
    #[serde(rename = "ECC")]
    Ecc,
    ZcashFoundation,
    MajorGrants,
    Deferred,
}

/// One funding stream of a [`StreamSet`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stream {
    pub receiver: Receiver,
    /// `fs.Numerator`.
    pub numerator: u64,
    /// The address of each address period of the stream, from the period of the start
    /// height. A list with one address gives that address to every period. Empty for the
    /// deferred pool.
    ///
    /// ZIP 214: a list of one address repeated `n` times (`[a] * n`) is one entry here.
    pub addresses: &'static [&'static str],
}

/// The streams of one range of heights.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamSet {
    /// `fs.StartHeight`.
    pub start: u32,
    /// `fs.EndHeight`: the first height after the streams, before NU7.
    pub end: u32,
    /// The set is the one of ZIP 214 revision 2, which ends at the third halving: NU7
    /// moves its end (`hayai_consensus_core::funding::nu7_adjusted_end`).
    pub ends_at_third_halving: bool,
    pub streams: &'static [Stream],
}

/// The funding streams that are active at `height` on `network`, with the values for a
/// block subsidy of `subsidy` zatoshis (`hayai_consensus_core::funding::funding_streams`).
/// A subsidy above `MAX_MONEY` is [`ConsensusError::MoneyOverflow`].
pub fn funding_streams(
    network: Network,
    height: u32,
    subsidy: u64,
) -> Result<Vec<FundingStream>, ConsensusError> {
    core::funding_streams(network.core(), height, subsidy)
}

/// The stream sets of a spec with the network type `network_type`, with each address
/// decoded to its script. Each address must be a P2SH address of the network type.
pub(crate) fn core_sets(
    network_type: NetworkType,
    sets: &[StreamSet],
) -> Result<Vec<CoreStreamSet>, ChainSpecError> {
    let mut core_sets = Vec::with_capacity(sets.len());
    for set in sets {
        let mut streams = Vec::with_capacity(set.streams.len());
        for stream in set.streams {
            let mut scripts = Vec::with_capacity(stream.addresses.len());
            for address in stream.addresses {
                check_address(network_type, address)?;
                let Ok(script) = p2sh_script(network_type, address) else {
                    unreachable!("check_address decoded the address");
                };
                scripts.push(script);
            }
            streams.push(CoreStream {
                receiver: stream.receiver,
                numerator: stream.numerator,
                scripts,
            });
        }
        core_sets.push(CoreStreamSet {
            start: set.start,
            end: set.end,
            ends_at_third_halving: set.ends_at_third_halving,
            streams,
        });
    }
    Ok(core_sets)
}

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

/// The stream sets of a Regtest configuration. The tables stay in memory until the
/// process ends.
pub(crate) fn regtest_sets(sets: &[RegtestFundingStreams]) -> &'static [StreamSet] {
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

/// The streams with the deferred pool, from NU6 (ZIP 1015) and from NU6.1 (ZIP 214
/// revision 2): 12 % to the lockbox and 8 % to Zcash Community Grants.
///
/// ZIP 1015, ZIP 214 r1: `FS_DEFERRED` 12 / 100 and `FS_FPF_ZCG` 8 / 100. ZIP 1016, ZIP 214
/// r2: `FS_CCF_H3` 12 / 100 and `FS_FPF_ZCG_H3` 8 / 100.
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

/// A funding stream of a built-in network with the scripts of its addresses: the form of
/// the tables that the compiler evaluates. [`core_sets_of`] gives the owned form of the
/// core.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ScriptStream {
    pub(crate) receiver: Receiver,
    pub(crate) numerator: u64,
    pub(crate) scripts: &'static [P2shScript],
}

/// A stream set of a built-in network with its [`ScriptStream`]s.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ScriptSet {
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) ends_at_third_halving: bool,
    pub(crate) streams: &'static [ScriptStream],
}

/// The stream sets of the core from the tables of a built-in network.
pub(crate) fn core_sets_of(sets: &[ScriptSet]) -> Vec<CoreStreamSet> {
    sets.iter()
        .map(|set| CoreStreamSet {
            start: set.start,
            end: set.end,
            ends_at_third_halving: set.ends_at_third_halving,
            streams: set
                .streams
                .iter()
                .map(|stream| CoreStream {
                    receiver: stream.receiver,
                    numerator: stream.numerator,
                    scripts: stream.scripts.to_vec(),
                })
                .collect(),
        })
        .collect()
}

/// [`lockbox_streams`] with the scripts of the addresses.
const fn lockbox_core_streams(fpf_scripts: &'static [P2shScript]) -> [ScriptStream; 2] {
    [
        ScriptStream {
            receiver: Receiver::Deferred,
            numerator: 12,
            scripts: &[],
        },
        ScriptStream {
            receiver: Receiver::MajorGrants,
            numerator: 8,
            scripts: fpf_scripts,
        },
    ]
}

/// The Mainnet address of Zcash Community Grants from NU6 (ZIP 214 r1 and r2).
const MAINNET_FPF: &str = "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow";
/// The Testnet address of Zcash Community Grants from NU6 (ZIP 214 r1 and r2).
const TESTNET_FPF: &str = "t2HifwjUj9uyxr9bknR8LFuQbc98c3vkXtu";
const MAINNET_ZF: &str = "t3dvVE3SQEi7kqNzwrfNePxZ1d4hUyztBA1";
const MAINNET_MG: &str = "t3XyYW8yBFRuMnfvm5KLGFbEVz25kckZXym";
const TESTNET_ZF: &str = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v";
const TESTNET_MG: &str = "t2Gvxv2uNM7hbbACjNox4H6DjByoKZ2Fa3P";

/// Zakura `mainnet::FUNDING_STREAMS` (`constants/mainnet.rs:233-289`).
///
/// ZIP 214: the Mainnet streams of revision 0 (1,046,400 to 2,726,400), revision 1
/// (2,726,400 to 3,146,400) and revision 2 (3,146,400 to 4,406,400 before NU7).
pub(crate) static MAINNET: [StreamSet; 3] = [
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
                addresses: &[MAINNET_ZF],
            },
            Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                addresses: &[MAINNET_MG],
            },
        ],
    },
    StreamSet {
        start: 2_726_400,
        end: 3_146_400,
        ends_at_third_halving: false,
        streams: &lockbox_streams(&[MAINNET_FPF]),
    },
    StreamSet {
        start: 3_146_400,
        end: 4_406_400,
        ends_at_third_halving: true,
        streams: &lockbox_streams(&[MAINNET_FPF]),
    },
];

/// [`MAINNET`] with the scripts of the addresses, for the core. The test
/// `address::tests::the_const_scripts_are_the_decoded_addresses` compares the two tables.
pub(crate) static MAINNET_CORE: [ScriptSet; 3] = [
    ScriptSet {
        start: 1_046_400,
        end: 2_726_400,
        ends_at_third_halving: false,
        streams: &[
            ScriptStream {
                receiver: Receiver::Ecc,
                numerator: 7,
                scripts: &scripts_of(&MAINNET_ECC_ADDRESSES),
            },
            ScriptStream {
                receiver: Receiver::ZcashFoundation,
                numerator: 5,
                scripts: &[script_of(MAINNET_ZF)],
            },
            ScriptStream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                scripts: &[script_of(MAINNET_MG)],
            },
        ],
    },
    ScriptSet {
        start: 2_726_400,
        end: 3_146_400,
        ends_at_third_halving: false,
        streams: &lockbox_core_streams(&[script_of(MAINNET_FPF)]),
    },
    ScriptSet {
        start: 3_146_400,
        end: 4_406_400,
        ends_at_third_halving: true,
        streams: &lockbox_core_streams(&[script_of(MAINNET_FPF)]),
    },
];

/// Zakura `testnet::FUNDING_STREAMS` (`constants/testnet.rs:212-262`). No stream exists
/// from 3,396,000 to the NU6.1 activation at 3,536,500.
///
/// ZIP 214: the Testnet streams of revision 0 (1,028,500 to 2,796,000), revision 1
/// (2,976,000 to 3,396,000) and revision 2 (3,536,500 to 4,476,000 before NU7).
pub(crate) static TESTNET: [StreamSet; 3] = [
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
                addresses: &[TESTNET_ZF],
            },
            Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                addresses: &[TESTNET_MG],
            },
        ],
    },
    StreamSet {
        start: 2_976_000,
        end: 3_396_000,
        ends_at_third_halving: false,
        streams: &lockbox_streams(&[TESTNET_FPF]),
    },
    StreamSet {
        start: 3_536_500,
        end: 4_476_000,
        ends_at_third_halving: true,
        streams: &lockbox_streams(&[TESTNET_FPF]),
    },
];

/// [`TESTNET`] with the scripts of the addresses, for the core.
pub(crate) static TESTNET_CORE: [ScriptSet; 3] = [
    ScriptSet {
        start: 1_028_500,
        end: 2_796_000,
        ends_at_third_halving: false,
        streams: &[
            ScriptStream {
                receiver: Receiver::Ecc,
                numerator: 7,
                scripts: &scripts_of(&TESTNET_ECC_ADDRESSES),
            },
            ScriptStream {
                receiver: Receiver::ZcashFoundation,
                numerator: 5,
                scripts: &[script_of(TESTNET_ZF)],
            },
            ScriptStream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                scripts: &[script_of(TESTNET_MG)],
            },
        ],
    },
    ScriptSet {
        start: 2_976_000,
        end: 3_396_000,
        ends_at_third_halving: false,
        streams: &lockbox_core_streams(&[script_of(TESTNET_FPF)]),
    },
    ScriptSet {
        start: 3_536_500,
        end: 4_476_000,
        ends_at_third_halving: true,
        streams: &lockbox_core_streams(&[script_of(TESTNET_FPF)]),
    },
];

/// Zakura `mainnet::FUNDING_STREAM_ECC_ADDRESSES` (`constants/mainnet.rs:58-107`).
///
/// ZIP 214 r0: `FS_ZIP214_BP.AddressList[0..47]` of Mainnet.
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
///
/// ZIP 214 r0: `FS_ZIP214_BP.AddressList[0..50]` of Testnet.
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
    use crate::address::address_script;
    use crate::subsidy::scheduled_subsidy;
    use crate::{address_of, Upgrade};
    use hayai_consensus_core::funding::{address_period, nu7_adjusted_end};

    fn stream(network: Network, height: u32, receiver: Receiver) -> Option<FundingStream> {
        funding_streams(network, height, 100)
            .unwrap()
            .into_iter()
            .find(|stream| stream.receiver == receiver)
    }

    /// The address of a stream, encoded for `network`.
    fn address(network: Network, stream: &FundingStream) -> Option<String> {
        stream.script.map(|script| address_of(network, &script))
    }

    /// The values of Zakura's `test_funding_stream_values`
    /// (`zakura-consensus/src/block/subsidy/tests.rs:19-111`).
    #[test]
    fn mainnet_values_match_the_zakura_test_values() {
        let values = |height| -> Vec<(Receiver, u64)> {
            let subsidy = scheduled_subsidy(Network::Mainnet, height);
            funding_streams(Network::Mainnet, height, subsidy)
                .unwrap()
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
            let subsidy = scheduled_subsidy(Network::Testnet, height);
            funding_streams(Network::Testnet, height, subsidy)
                .unwrap()
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
        // ZIP 214 r3, ZIP 259: the NU7 height is a multiple of 3.
        assert_eq!(nu7 % 3, 0);
        let values = |height| -> Vec<(Receiver, u64)> {
            let subsidy = scheduled_subsidy(Network::Testnet, height);
            funding_streams(Network::Testnet, height, subsidy)
                .unwrap()
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
        assert_eq!(nu7_adjusted_end(4_476_000, Some(nu7)), Ok(4_497_948));
        assert_eq!(crate::subsidy::halving(Network::Testnet, 4_497_947), 2);
        assert_eq!(crate::subsidy::halving(Network::Testnet, 4_497_948), 3);
        for height in [4_497_948, 4_497_949] {
            assert_eq!(values(height), vec![], "{height}");
        }
        // The recipient with an address keeps it across NU7.
        for height in [nu7 - 1, nu7, 4_497_947] {
            let streams = funding_streams(Network::Testnet, height, 100).unwrap();
            let grants = streams
                .iter()
                .find(|stream| stream.receiver == Receiver::MajorGrants)
                .expect("the grants stream");
            assert_eq!(
                address(Network::Testnet, grants),
                Some(TESTNET_FPF.to_string())
            );
        }
    }

    /// An end at or below the NU7 height, and every end on a network without NU7, does
    /// not move. Mainnet has no NU7 height: its last stream set keeps its end.
    #[test]
    fn only_an_end_above_the_nu7_height_moves() {
        assert_eq!(Network::Mainnet.activation_height(Upgrade::Nu7), None);
        let subsidy = scheduled_subsidy(Network::Mainnet, 4_406_399);
        assert_eq!(
            funding_streams(Network::Mainnet, 4_406_399, subsidy)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            funding_streams(Network::Mainnet, 4_406_400, subsidy),
            Ok(vec![])
        );
    }

    /// The address period before and from the NU7 height `A`: both formulas agree at `A`,
    /// and from `A` a period has 3 times the blocks.
    #[test]
    fn an_address_period_has_three_times_the_blocks_from_nu7() {
        let spec = Network::Testnet.core();
        let nu7 = 4_465_026;
        let first_halving = 1_116_000;
        let period = |height| address_period(spec, first_halving, height).unwrap();
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
    /// `subsidy/constants/mainnet.rs:196-221`). This crate has no code for it, and no
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
                Ok(vec![])
            );
        }
    }

    #[test]
    fn a_subsidy_of_zero_has_no_funding_stream() {
        assert_eq!(funding_streams(Network::Mainnet, 2_726_400, 0), Ok(vec![]));
    }

    #[test]
    fn the_ecc_address_changes_at_each_period_boundary() {
        // Mainnet: 35,000 blocks in a period, and the stream starts at a period start.
        let ecc = |height| {
            address(
                Network::Mainnet,
                &stream(Network::Mainnet, height, Receiver::Ecc).unwrap(),
            )
        };
        for index in 0..48u32 {
            let start = 1_046_400 + index * 35_000;
            let expected = Some(MAINNET_ECC_ADDRESSES[index as usize].to_string());
            assert_eq!(ecc(start), expected, "period {index}");
            assert_eq!(ecc(start + 34_999), expected, "period {index}");
        }
        assert_eq!(
            ecc(1_046_400),
            Some("t3LmX1cxWPPPqL4TZHx42HU3U5ghbFjRiif".to_string())
        );
        assert_eq!(
            ecc(2_726_399),
            Some("t3XHAGxRP2FNfhAjxGjxbrQPYtQQjc3RCQD".to_string())
        );

        // Testnet: the stream starts at Canopy, 17,500 blocks before the end of period
        // 45. The first halving at 1,116,000 is the start of period 48.
        let ecc = |height| {
            address(
                Network::Testnet,
                &stream(Network::Testnet, height, Receiver::Ecc).unwrap(),
            )
        };
        let testnet = |index: usize| Some(TESTNET_ECC_ADDRESSES[index].to_string());
        assert_eq!(ecc(1_028_500), testnet(0));
        assert_eq!(ecc(1_045_999), testnet(0));
        assert_eq!(ecc(1_046_000), testnet(1));
        assert_eq!(ecc(1_080_999), testnet(1));
        assert_eq!(ecc(1_081_000), testnet(2));
        assert_eq!(ecc(1_115_999), testnet(2));
        assert_eq!(ecc(1_116_000), testnet(3));
        assert_eq!(ecc(2_760_999), testnet(49));
        assert_eq!(ecc(2_761_000), testnet(50));
        assert_eq!(ecc(2_795_999), testnet(50));
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
            let spec = network.core();
            let first_halving = spec.first_halving.expect("a first halving");
            for (set, count) in network.spec().funding_streams.iter().zip(counts) {
                let periods = address_period(spec, first_halving, set.end - 1).unwrap()
                    - address_period(spec, first_halving, set.start).unwrap()
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
            let first_halving = network.core().first_halving.expect("a first halving");
            assert_eq!(crate::subsidy::halving(network, first_halving - 1), 0);
            assert_eq!(crate::subsidy::halving(network, first_halving), 1);
        }
    }

    #[test]
    fn the_ranges_are_in_order_and_start_at_the_upgrades() {
        for network in [Network::Mainnet, Network::Testnet] {
            let sets = network.spec().funding_streams;
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
            for stream in network
                .spec()
                .funding_streams
                .iter()
                .flat_map(|set| set.streams)
            {
                for address in stream.addresses {
                    let script = address_script(network, address);
                    assert_eq!((script[0], script[1], script[22]), (0xa9, 0x14, 0x87));
                }
            }
        }
    }

    #[test]
    fn the_deferred_stream_has_no_address() {
        let deferred = stream(Network::Mainnet, 2_726_400, Receiver::Deferred).unwrap();
        assert_eq!((deferred.value, deferred.script), (12, None));
        let grants = stream(Network::Mainnet, 3_146_400, Receiver::MajorGrants).unwrap();
        assert_eq!(grants.value, 8);
        assert_eq!(
            address(Network::Mainnet, &grants),
            Some(MAINNET_FPF.to_string())
        );
    }

    /// A subsidy above `MAX_MONEY` is an error of the wrapper, as of the core.
    #[test]
    fn a_subsidy_above_max_money_is_an_error() {
        assert_eq!(
            funding_streams(Network::Mainnet, 2_726_400, crate::MAX_MONEY + 1),
            Err(ConsensusError::MoneyOverflow)
        );
    }
}
