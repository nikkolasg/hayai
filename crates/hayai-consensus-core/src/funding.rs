//! Funding streams (protocol specification §7.10, ZIP 207, ZIP 214, ZIP 1014, ZIP 1015).
//!
//! A funding stream gives a share of the block subsidy to a recipient in a range of
//! heights. The recipient is a script that changes with the address period, or the
//! deferred pool (lockbox). The stream sets of a chain are data of its spec
//! ([`CoreSpec::funding_streams`]).
//!
//! NU7 changes two things (ZIP 218, ZIP 214 revision 3): the end of the last stream set
//! moves with the third halving ([`nu7_adjusted_end`]), and an address period has 3 times
//! the blocks from the NU7 height ([`address_period`]). The tables hold the heights
//! before NU7. ZIP 2008 changes the Mainnet recipient of the last stream set at NU7:
//! Mainnet has no NU7 height, and this module has no code for ZIP 2008.

use crate::chain_spec::SpecError;
use crate::{
    money, ConsensusError, CoreSpec, P2shScript, Upgrade, POST_BLOSSOM_TARGET_SPACING,
    POST_NU7_TARGET_SPACING,
};

/// The recipient of a funding stream. The names are those of Zakura
/// (`FundingStreamReceiver`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Receiver {
    /// Electric Coin Company (ZIP 1014).
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
    /// The script of the recipient at the height. `None` for [`Receiver::Deferred`].
    pub script: Option<P2shScript>,
}

/// `fs.Denominator` of every stream.
///
/// ZIP 214: every stream of revisions 0 to 2 has the denominator 100.
pub const DENOMINATOR: u64 = 100;
/// Address periods in one post-Blossom halving interval.
///
/// Spec §7.10: `FSRecipientChangeInterval = PostBlossomHalvingInterval / 48`.
pub const PERIODS_PER_HALVING_INTERVAL: u32 = 48;

/// One funding stream of a [`StreamSet`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stream {
    pub receiver: Receiver,
    /// `fs.Numerator`.
    pub numerator: u64,
    /// The script of each address period of the stream, from the period of the start
    /// height. A list with one script gives that script to every period. Empty for the
    /// deferred pool.
    ///
    /// ZIP 214: a list of one address repeated `n` times (`[a] * n`) is one entry here.
    pub scripts: Vec<P2shScript>,
}

/// The streams of one range of heights.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamSet {
    /// `fs.StartHeight`.
    pub start: u32,
    /// `fs.EndHeight`: the first height after the streams, before NU7.
    pub end: u32,
    /// The set is the one of ZIP 214 revision 2, which ends at the third halving: NU7
    /// moves its end ([`nu7_adjusted_end`]).
    pub ends_at_third_halving: bool,
    pub streams: Vec<Stream>,
}

/// `NU7PoWTargetSpacingRatio` of ZIP 218: 3.
///
/// ZIP 207 r2: `R` of the NU7 `AddressPeriod` is 3.
const NU7_SPACING_RATIO: u32 = POST_BLOSSOM_TARGET_SPACING / POST_NU7_TARGET_SPACING;

/// The end height `end` of a stream set that ends at the third halving, on a chain with
/// the NU7 height `nu7` (ZIP 214 revision 3; Zakura `nu7_adjusted_funding_stream_height`,
/// `subsidy.rs:388-402`): an end above the NU7 height `A` moves to `A + 3 * (end - A)`.
///
/// ZIP 214 r3: the revision 2 streams end at `HeightForHalving(3)` after NU7, `A + 3 *
/// (H_3 - A)`. An NU7 height at or after `H_3` does not reactivate an expired stream.
/// [`ConsensusError::UncheckedSpec`] when the moved end is above the largest height.
pub fn nu7_adjusted_end(end: u32, nu7: Option<u32>) -> Result<u32, ConsensusError> {
    let Some(nu7) = nu7 else {
        return Ok(end);
    };
    if nu7 >= end {
        return Ok(end);
    }
    let Some(blocks) = (end - nu7).checked_mul(NU7_SPACING_RATIO) else {
        return Err(ConsensusError::UncheckedSpec);
    };
    let Some(moved) = nu7.checked_add(blocks) else {
        return Err(ConsensusError::UncheckedSpec);
    };
    Ok(moved)
}

/// The end of `set` on a chain with the NU7 height `nu7`.
fn set_end(set: &StreamSet, nu7: Option<u32>) -> Result<u32, ConsensusError> {
    match set.ends_at_third_halving {
        true => nu7_adjusted_end(set.end, nu7),
        false => Ok(set.end),
    }
}

/// Checks the stream sets of a spec with the NU7 height `nu7`: a range that ends below its
/// start, a receiver two times in one range, numerators above 100 in total, a script for
/// [`Receiver::Deferred`], and an end that NU7 moves above the largest height. The adapter
/// decodes the addresses before it builds the scripts.
pub fn check_sets(nu7: Option<u32>, sets: &[StreamSet]) -> Result<(), SpecError> {
    let mut failure: Option<SpecError> = None;
    for s in 0..sets.len() {
        if let Err(e) = check_set(nu7, &sets[s]) {
            failure = Some(e);
            break;
        }
    }
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// The checks of [`check_sets`] on one stream set.
fn check_set(nu7: Option<u32>, set: &StreamSet) -> Result<(), SpecError> {
    let (start, end) = (set.start, set.end);
    if end < start {
        return Err(SpecError::StreamRange { start, end });
    }
    let mut numerators = 0u128;
    let mut failure: Option<SpecError> = None;
    for index in 0..set.streams.len() {
        let stream = &set.streams[index];
        let receiver = stream.receiver;
        if receiver_repeats(&set.streams, index) {
            failure = Some(SpecError::StreamReceiver(receiver));
            break;
        }
        let Some(sum) = numerators.checked_add(u128::from(stream.numerator)) else {
            failure = Some(SpecError::Rule(ConsensusError::Overflow));
            break;
        };
        numerators = sum;
        if receiver == Receiver::Deferred && stream.scripts.len() > 0 {
            failure = Some(SpecError::DeferredScript);
            break;
        }
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    if numerators > u128::from(DENOMINATOR) {
        return Err(SpecError::StreamNumerators(numerators));
    }
    if set.ends_at_third_halving {
        let Ok(_) = nu7_adjusted_end(end, nu7) else {
            return Err(SpecError::StreamEnd(end));
        };
    }
    Ok(())
}

/// Whether a stream before `index` in `streams` has the receiver of the stream at `index`.
fn receiver_repeats(streams: &[Stream], index: usize) -> bool {
    let mut found = false;
    for earlier in 0..index {
        if streams[earlier].receiver == streams[index].receiver {
            found = true;
        }
    }
    found
}

/// Checks that each stream with a script has one script for each address period of its
/// range. With `one_script_repeats`, a list of one script is valid for any number of
/// periods: it is that script repeated (ZIP 214 `[a] * n`), as in the tables of Mainnet
/// and Testnet. A Regtest configuration has one script for each period, as in Zakura.
///
/// Spec §7.10: `fs.Recipients` has `fs.NumRecipients` elements. Zakura and this check
/// accept more. Zakura stops at the first block of a period without an address
/// (`funding_stream_address_index`, `zakura-consensus/src/block/subsidy.rs:18-43`).
pub fn check_script_counts(spec: &CoreSpec, one_script_repeats: bool) -> Result<(), SpecError> {
    let sets = &spec.funding_streams;
    if sets.len() == 0 {
        return Ok(());
    }
    let Some(first_halving) = spec.first_halving else {
        return Err(SpecError::NoFirstHalving);
    };
    let nu7 = spec.activation_height(Upgrade::Nu7);
    let mut failure: Option<SpecError> = None;
    for s in 0..sets.len() {
        if let Err(e) = check_set_scripts(spec, first_halving, nu7, &sets[s], one_script_repeats) {
            failure = Some(e);
            break;
        }
    }
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// The check of [`check_script_counts`] on one stream set.
fn check_set_scripts(
    spec: &CoreSpec,
    first_halving: u32,
    nu7: Option<u32>,
    set: &StreamSet,
    one_script_repeats: bool,
) -> Result<(), SpecError> {
    let end = set_end(set, nu7)?;
    if set.start >= end {
        return Ok(());
    }
    let Some(last_height) = end.checked_sub(1) else {
        return Err(SpecError::Rule(ConsensusError::Overflow));
    };
    let last = address_period(spec, first_halving, last_height)?;
    let first = address_period(spec, first_halving, set.start)?;
    let Some(periods) = last.checked_sub(first) else {
        return Err(SpecError::Rule(ConsensusError::Overflow));
    };
    let Ok(required) = usize::try_from(periods) else {
        return Err(SpecError::Rule(ConsensusError::Overflow));
    };
    let Some(required) = required.checked_add(1) else {
        return Err(SpecError::Rule(ConsensusError::Overflow));
    };
    let mut failure: Option<SpecError> = None;
    for index in 0..set.streams.len() {
        let receiver = set.streams[index].receiver;
        let found = set.streams[index].scripts.len();
        let repeated = one_script_repeats && found == 1;
        if receiver != Receiver::Deferred && found < required && !repeated {
            failure = Some(SpecError::StreamScripts {
                receiver,
                start: set.start,
                end,
                required,
                found,
            });
            break;
        }
    }
    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// `floor(a / b)`: the quotient rounded toward negative infinity.
fn div_floor(a: i64, b: i64) -> Result<i64, ConsensusError> {
    let (Some(quotient), Some(remainder)) = (a.checked_div(b), a.checked_rem(b)) else {
        return Err(ConsensusError::DivisionByZero);
    };
    if remainder != 0 && ((remainder < 0) != (b < 0)) {
        let Some(floor) = quotient.checked_sub(1) else {
            return Err(ConsensusError::Overflow);
        };
        return Ok(floor);
    }
    Ok(quotient)
}

/// The address period of `height`.
///
/// Spec §7.10: `FSRecipientPeriod(height) = floor((height + PostBlossomHalvingInterval -
/// HeightForHalving(1)) / FSRecipientChangeInterval)`.
///
/// ZIP 207 r2: from the NU7 height `A` a period has 3 times the blocks (Zakura
/// `funding_stream_address_period`, `subsidy.rs:341-370`):
/// `floor((3 * (A + PostBlossomHalvingInterval - HeightForHalving(1)) + (height - A)) /
/// (3 * FSRecipientChangeInterval))`.
pub fn address_period(
    spec: &CoreSpec,
    first_halving: u32,
    height: u32,
) -> Result<i64, ConsensusError> {
    let interval = spec.post_blossom_halving_interval()?;
    let change_interval = i64::from(interval / PERIODS_PER_HALVING_INTERVAL);
    // `height + interval - first_halving`, in 64 bits: no 32-bit value overflows it.
    let offset_of =
        |height: u32| i64::from(height) + i64::from(interval) - i64::from(first_halving);
    match spec.activation_height(Upgrade::Nu7) {
        Some(nu7) if height >= nu7 => {
            let ratio = i64::from(NU7_SPACING_RATIO);
            let Some(scaled) = ratio.checked_mul(offset_of(nu7)) else {
                return Err(ConsensusError::Overflow);
            };
            let Some(numerator) = scaled.checked_add(i64::from(height - nu7)) else {
                return Err(ConsensusError::Overflow);
            };
            let Some(denominator) = ratio.checked_mul(change_interval) else {
                return Err(ConsensusError::Overflow);
            };
            div_floor(numerator, denominator)
        }
        _ => div_floor(offset_of(height), change_interval),
    }
}

/// The funding streams that are active at `height`, with the values for a block subsidy
/// of `subsidy` zatoshis: `fs.Value(height) = floor(subsidy * numerator / 100)`. A subsidy
/// of 0 has no funding stream, and a height before Canopy has none (Zakura
/// `funding_stream_values`).
///
/// Spec §7.8: `fs.Value(height) = floor(BlockSubsidy(height) * fs.Numerator /
/// fs.Denominator)`, 0 before Canopy. ZIP 207: a stream is active from its start height to
/// the height before its end height. Spec §7.10: `fs.Recipient(height)` is the address of
/// `fs.RecipientIndex(height)`.
pub fn funding_streams(
    spec: &CoreSpec,
    height: u32,
    subsidy: u64,
) -> Result<Vec<FundingStream>, ConsensusError> {
    let subsidy = money(subsidy)?;
    let canopy_active = match spec.activation_height(Upgrade::Canopy) {
        Some(canopy) => canopy <= height,
        None => false,
    };
    if subsidy == 0 || !canopy_active {
        return Ok(Vec::new());
    }
    let sets = &spec.funding_streams;
    if sets.len() == 0 {
        return Ok(Vec::new());
    }
    // Zakura `height_for_first_halving`.
    let Some(first_halving) = spec.first_halving else {
        return Err(ConsensusError::UncheckedSpec);
    };
    let nu7 = spec.activation_height(Upgrade::Nu7);
    let mut active: Option<usize> = None;
    let mut failure: Option<ConsensusError> = None;
    for s in 0..sets.len() {
        if let Some(_) = active {
            continue;
        }
        let set = &sets[s];
        let end = match set_end(set, nu7) {
            Ok(end) => end,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        if set.start <= height && height < end {
            active = Some(s);
        }
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    let Some(s) = active else {
        return Ok(Vec::new());
    };
    let set = &sets[s];
    let Some(period) = address_period(spec, first_halving, height)?.checked_sub(address_period(
        spec,
        first_halving,
        set.start,
    )?) else {
        return Err(ConsensusError::Overflow);
    };
    let Ok(period) = usize::try_from(period) else {
        return Err(ConsensusError::UncheckedSpec);
    };
    let mut streams = Vec::with_capacity(set.streams.len());
    let mut failure: Option<ConsensusError> = None;
    for index in 0..set.streams.len() {
        let stream = &set.streams[index];
        let Some(product) = subsidy.checked_mul(stream.numerator) else {
            failure = Some(ConsensusError::MoneyOverflow);
            break;
        };
        let script = match stream.scripts.len() {
            0 => None,
            1 => Some(stream.scripts[0]),
            _ => match stream.scripts.get(period) {
                Some(script) => Some(*script),
                None => {
                    failure = Some(ConsensusError::UncheckedSpec);
                    break;
                }
            },
        };
        streams.push(FundingStream {
            receiver: stream.receiver,
            value: product / DENOMINATOR,
            script,
        });
    }
    if let Some(failure) = failure {
        return Err(failure);
    }
    Ok(streams)
}

/// The value of the stream to the deferred pool at `height` for a block subsidy of
/// `subsidy` zatoshis. 0 when no such stream is active.
///
/// Spec §7.8: `totalDeferredOutput(height)`.
pub fn deferred_value(spec: &CoreSpec, height: u32, subsidy: u64) -> Result<u64, ConsensusError> {
    let streams = funding_streams(spec, height, subsidy)?;
    let mut total = 0u64;
    for i in 0..streams.len() {
        if streams[i].receiver == Receiver::Deferred {
            total = crate::add_money(total, streams[i].value)?;
        }
    }
    Ok(total)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::chain_spec::tests::regtest;
    use crate::MAX_MONEY;

    pub(crate) const SCRIPT_A: P2shScript = [
        0xa9, 0x14, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0x87,
    ];
    pub(crate) const SCRIPT_B: P2shScript = [
        0xa9, 0x14, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0x87,
    ];
    pub(crate) const SCRIPT_C: P2shScript = [
        0xa9, 0x14, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 0x87,
    ];

    /// 12 % to the deferred pool and 8 % to Major Grants, one script for each of the 3
    /// address periods of the heights 10 to 21 (6 blocks each on Regtest), then 7 % to
    /// ECC from 23 to 28.
    pub(crate) fn streams() -> Vec<StreamSet> {
        vec![
            StreamSet {
                start: 10,
                end: 22,
                ends_at_third_halving: false,
                streams: vec![
                    Stream {
                        receiver: Receiver::Deferred,
                        numerator: 12,
                        scripts: vec![],
                    },
                    Stream {
                        receiver: Receiver::MajorGrants,
                        numerator: 8,
                        scripts: vec![SCRIPT_A, SCRIPT_C, SCRIPT_B],
                    },
                ],
            },
            StreamSet {
                start: 23,
                end: 29,
                ends_at_third_halving: false,
                streams: vec![Stream {
                    receiver: Receiver::Ecc,
                    numerator: 7,
                    scripts: vec![SCRIPT_B],
                }],
            },
        ]
    }

    /// Regtest with NU6 at height 20 and the streams of [`streams`].
    pub(crate) fn regtest_with_streams() -> CoreSpec {
        let mut spec = regtest();
        spec.activation_heights[Upgrade::Nu6.index()] = Some(20);
        spec.funding_streams = streams();
        spec.checked().expect("a valid spec")
    }

    #[test]
    fn a_configured_chain_pays_its_streams_by_period() {
        let spec = regtest_with_streams();
        let subsidy = 625_000_000;
        let grants = |script| FundingStream {
            receiver: Receiver::MajorGrants,
            value: 50_000_000,
            script: Some(script),
        };
        let deferred = FundingStream {
            receiver: Receiver::Deferred,
            value: 75_000_000,
            script: None,
        };
        let ecc = FundingStream {
            receiver: Receiver::Ecc,
            value: 43_750_000,
            script: Some(SCRIPT_B),
        };
        for (height, expected) in [
            (9, Vec::new()),
            (10, vec![deferred, grants(SCRIPT_A)]),
            (11, vec![deferred, grants(SCRIPT_C)]),
            (16, vec![deferred, grants(SCRIPT_C)]),
            (17, vec![deferred, grants(SCRIPT_B)]),
            (21, vec![deferred, grants(SCRIPT_B)]),
            (22, Vec::new()),
            (23, vec![ecc]),
            (28, vec![ecc]),
            (29, Vec::new()),
        ] {
            assert_eq!(
                funding_streams(&spec, height, subsidy),
                Ok(expected),
                "{height}"
            );
        }
        assert_eq!(deferred_value(&spec, 10, subsidy), Ok(75_000_000));
        assert_eq!(deferred_value(&spec, 23, subsidy), Ok(0));
        assert_eq!(funding_streams(&spec, 10, 0), Ok(Vec::new()));
        // Before Canopy no stream is active.
        let mut late_canopy = spec.clone();
        late_canopy.activation_heights[Upgrade::Canopy.index()] = Some(15);
        assert_eq!(funding_streams(&late_canopy, 14, subsidy), Ok(Vec::new()));
    }

    /// An end at or below the NU7 height, and every end on a chain without NU7, does not
    /// move (the values of Zakura's doc of `nu7_adjusted_funding_stream_height`).
    #[test]
    fn only_an_end_above_the_nu7_height_moves() {
        assert_eq!(nu7_adjusted_end(4_406_400, None), Ok(4_406_400));
        assert_eq!(nu7_adjusted_end(100, Some(100)), Ok(100));
        assert_eq!(nu7_adjusted_end(100, Some(101)), Ok(100));
        assert_eq!(nu7_adjusted_end(101, Some(100)), Ok(103));
        assert_eq!(nu7_adjusted_end(4_476_000, Some(4_465_026)), Ok(4_497_948));
        assert_eq!(
            nu7_adjusted_end(u32::MAX, Some(4_300_000)),
            Err(ConsensusError::UncheckedSpec)
        );
    }

    fn range() -> Vec<StreamSet> {
        vec![StreamSet {
            start: 10,
            end: 9,
            ends_at_third_halving: false,
            streams: vec![],
        }]
    }
    fn receivers() -> Vec<StreamSet> {
        vec![StreamSet {
            start: 11,
            end: 17,
            ends_at_third_halving: false,
            streams: vec![
                Stream {
                    receiver: Receiver::MajorGrants,
                    numerator: 1,
                    scripts: vec![SCRIPT_A],
                },
                Stream {
                    receiver: Receiver::MajorGrants,
                    numerator: 1,
                    scripts: vec![SCRIPT_A],
                },
            ],
        }]
    }
    fn numerators() -> Vec<StreamSet> {
        vec![StreamSet {
            start: 11,
            end: 17,
            ends_at_third_halving: false,
            streams: vec![
                Stream {
                    receiver: Receiver::Deferred,
                    numerator: 93,
                    scripts: vec![],
                },
                Stream {
                    receiver: Receiver::MajorGrants,
                    numerator: 8,
                    scripts: vec![SCRIPT_A],
                },
            ],
        }]
    }
    fn deferred_script() -> Vec<StreamSet> {
        vec![StreamSet {
            start: 11,
            end: 17,
            ends_at_third_halving: false,
            streams: vec![Stream {
                receiver: Receiver::Deferred,
                numerator: 1,
                scripts: vec![SCRIPT_A],
            }],
        }]
    }
    /// A set that ends at the third halving, so that NU7 moves its end.
    fn late_end() -> Vec<StreamSet> {
        vec![StreamSet {
            start: 4_300_000,
            end: u32::MAX,
            ends_at_third_halving: true,
            streams: vec![],
        }]
    }

    #[test]
    fn the_checks_of_the_stream_sets() {
        assert_eq!(check_sets(None, &streams()), Ok(()));
        assert_eq!(
            check_sets(None, &range()),
            Err(SpecError::StreamRange { start: 10, end: 9 })
        );
        assert_eq!(
            check_sets(None, &receivers()),
            Err(SpecError::StreamReceiver(Receiver::MajorGrants))
        );
        assert_eq!(
            check_sets(None, &numerators()),
            Err(SpecError::StreamNumerators(101))
        );
        assert_eq!(
            check_sets(None, &deferred_script()),
            Err(SpecError::DeferredScript)
        );
        assert_eq!(check_sets(None, &late_end()), Ok(()));
        assert_eq!(
            check_sets(Some(4_200_000), &late_end()),
            Err(SpecError::StreamEnd(u32::MAX))
        );
    }

    #[test]
    fn each_stream_needs_one_script_for_each_period() {
        let two: Vec<StreamSet> = vec![StreamSet {
            start: 10,
            end: 22,
            ends_at_third_halving: false,
            streams: vec![Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                scripts: vec![SCRIPT_A, SCRIPT_B],
            }],
        }];
        let one: Vec<StreamSet> = vec![StreamSet {
            start: 10,
            end: 22,
            ends_at_third_halving: false,
            streams: vec![Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                scripts: vec![SCRIPT_A],
            }],
        }];
        let mut spec = regtest();
        spec.funding_streams = two.clone();
        assert_eq!(
            spec.clone().checked(),
            Err(SpecError::StreamScripts {
                receiver: Receiver::MajorGrants,
                start: 10,
                end: 22,
                required: 3,
                found: 2,
            })
        );
        spec.funding_streams = one.clone();
        let Ok(spec) = spec.checked() else {
            panic!("one script repeats");
        };
        assert_eq!(
            check_script_counts(&spec, false),
            Err(SpecError::StreamScripts {
                receiver: Receiver::MajorGrants,
                start: 10,
                end: 22,
                required: 3,
                found: 1,
            })
        );
        // Without a first halving the stream sets have no address period.
        let mut no_halving = spec.clone();
        no_halving.first_halving = None;
        assert_eq!(
            check_script_counts(&no_halving, true),
            Err(SpecError::NoFirstHalving)
        );
        assert_eq!(
            funding_streams(&no_halving, 10, 100),
            Err(ConsensusError::UncheckedSpec)
        );
    }

    /// A rule error during the checks is `SpecError::Rule`: a halving interval of 0 gives
    /// an address period of 0 blocks, and `check_script_counts` meets the division by 0.
    /// `CoreSpec::checked` refuses the interval before this check (`HalvingInterval`).
    #[test]
    fn a_rule_error_during_the_checks_is_a_spec_error() {
        let mut spec = regtest_with_streams();
        spec.pre_blossom_halving_interval = 0;
        assert_eq!(
            check_script_counts(&spec, true),
            Err(SpecError::Rule(ConsensusError::DivisionByZero))
        );
        assert_eq!(spec.checked(), Err(SpecError::HalvingInterval(0)));
    }

    /// A subsidy above `MAX_MONEY` is refused before the share arithmetic.
    #[test]
    fn a_subsidy_above_max_money_is_an_error() {
        let spec = regtest_with_streams();
        assert_eq!(
            funding_streams(&spec, 10, MAX_MONEY + 1),
            Err(ConsensusError::MoneyOverflow)
        );
        let Ok(streams) = funding_streams(&spec, 10, MAX_MONEY) else {
            panic!("MAX_MONEY is a valid subsidy");
        };
        assert_eq!(streams[0].value, MAX_MONEY / 100 * 12);
    }

    /// A chain whose scripts list is shorter than its periods: the error of the rule, not
    /// a panic. `checked` refuses the spec; the rule still returns an error.
    #[test]
    fn a_period_without_a_script_is_an_error() {
        let short: Vec<StreamSet> = vec![StreamSet {
            start: 10,
            end: 22,
            ends_at_third_halving: false,
            streams: vec![Stream {
                receiver: Receiver::MajorGrants,
                numerator: 8,
                scripts: vec![SCRIPT_A, SCRIPT_B],
            }],
        }];
        let mut spec = regtest();
        spec.funding_streams = short.clone();
        let Ok(streams) = funding_streams(&spec, 16, 100) else {
            panic!("period 1 has a script");
        };
        assert_eq!(streams[0].script, Some(SCRIPT_B));
        assert_eq!(
            funding_streams(&spec, 17, 100),
            Err(ConsensusError::UncheckedSpec)
        );
    }

    #[test]
    fn div_floor_rounds_toward_negative_infinity() {
        assert_eq!(div_floor(7, 2), Ok(3));
        assert_eq!(div_floor(-7, 2), Ok(-4));
        assert_eq!(div_floor(-8, 2), Ok(-4));
        assert_eq!(div_floor(7, -2), Ok(-4));
        assert_eq!(div_floor(0, 5), Ok(0));
        assert_eq!(div_floor(1, 0), Err(ConsensusError::DivisionByZero));
    }
}
