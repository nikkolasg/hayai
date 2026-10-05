//! The coinbase value rules of one block (protocol specification §7.1.2, §7.9, §7.10,
//! ZIP 236, ZIP 271).
//!
//! [`CoinbaseTerms::at`] collects what the coinbase of a height must pay: the founders'
//! reward before Canopy, one output for each funding stream with an address from Canopy,
//! and the lockbox disbursement outputs in the NU6.1 activation block.
//! [`CoinbaseTerms::check`] checks a coinbase against them. The block template takes its
//! outputs from the same terms, so the template and the validator agree.
//!
//! The checks follow Zakura's `subsidy_is_valid` and `miner_fees_are_valid`
//! (`zakura-consensus/src/block/check.rs:177-383`). From NU7 the coinbase gets the miner
//! share of the fees, and from the NSM reissuance height the subsidy has a bonus
//! ([`crate::nsm`]).

use hayai_crypto::zcash_address::ZcashAddress;
use hayai_crypto::zcash_protocol::consensus::NetworkType;
use hayai_crypto::zcash_transparent::address::TransparentAddress;

use crate::funding::Receiver;
use crate::subsidy::Subsidy;
use crate::{founders, funding, lockbox, nsm, rules_at, subsidy, ConsensusError, Network};

/// Why the coinbase must have an output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputKind {
    FoundersReward,
    FundingStream(Receiver),
    LockboxDisbursement,
}

/// One output that the coinbase must have: the exact value and the exact script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredOutput {
    pub kind: OutputKind,
    /// Zatoshis.
    pub value: u64,
    /// The `scriptPubKey`: `OP_HASH160 <script hash> OP_EQUAL`.
    pub script: Vec<u8>,
}

/// The value balances of the shielded bundles of a coinbase, as the transaction encodes
/// them. A negative balance is value that enters the pool. A coinbase without a bundle
/// has a balance of 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShieldedBalances {
    pub sapling: i64,
    pub orchard: i64,
    pub ironwood: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CoinbaseError {
    #[error(transparent)]
    Consensus(#[from] ConsensusError),
    /// No unmatched output has the script or the value of the required output.
    #[error("coinbase has no {kind:?} output of {value} zatoshis to script {script:02x?}")]
    MissingOutput {
        kind: OutputKind,
        value: u64,
        script: Vec<u8>,
    },
    /// An output pays the script of the required output with another value.
    #[error("coinbase {kind:?} output pays {found} zatoshis and must pay {expected}")]
    WrongAmount {
        kind: OutputKind,
        expected: u64,
        found: u64,
    },
    /// An output has the value of the required output and another script.
    #[error(
        "coinbase {kind:?} output of {value} zatoshis pays script {found:02x?} and must pay \
         {expected:02x?}"
    )]
    WrongScript {
        kind: OutputKind,
        value: u64,
        expected: Vec<u8>,
        found: Vec<u8>,
    },
    /// Before NU6: the coinbase pays more than the subsidy that it can pay out and the fees.
    #[error("coinbase pays {paid} zatoshis, more than the limit of {allowed}")]
    ValueAboveLimit { paid: i128, allowed: i128 },
    /// From NU6 (ZIP 236): the coinbase does not pay the subsidy that it can pay out and
    /// the fees exactly.
    #[error("coinbase pays {paid} zatoshis and must pay {required} exactly")]
    ValueNotExact { paid: i128, required: i128 },
    /// The block pays more out of the deferred pool than the pool holds.
    #[error("deferred pool of {before} zatoshis cannot pay a disbursement of {disbursed}")]
    NegativeDeferredPool { before: u64, disbursed: u64 },
}

/// What the coinbase of one height must pay and can pay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseTerms {
    pub subsidy: Subsidy,
    /// The outputs that the coinbase must have. Each one needs its own coinbase output: two
    /// equal required outputs need two coinbase outputs.
    pub required: Vec<RequiredOutput>,
    /// Zatoshis that the required lockbox disbursement outputs take out of the deferred
    /// pool.
    pub disbursed: u64,
    /// ZIP 236, from NU6: the coinbase pays its limit exactly.
    pub exact_value: bool,
    /// From NU7: the coinbase gets the miner share of the fees
    /// ([`CoinbaseTerms::miner_fees`]).
    pub nsm_fee_share: bool,
}

impl CoinbaseTerms {
    /// The terms of the coinbase at `height` on `network`, for a caller without the chain
    /// value pools.
    ///
    /// It fails with [`ConsensusError::UnsupportedUpgrade`] when the upgrade that is active
    /// at `height` has no rule set, and with [`ConsensusError::IssuedSupplyUnknown`] from
    /// the NSM reissuance height: [`CoinbaseTerms::after`] gives the terms there.
    pub fn at(network: Network, height: u32) -> Result<Self, ConsensusError> {
        Self::terms(network, height, None)
    }

    /// The terms of the coinbase at `height` on `network`, in a block whose parent leaves
    /// `issued` zatoshis in the chain value pools in total. Block validation calls this
    /// function.
    ///
    /// From the NSM reissuance height the subsidy is the subsidy of the halving schedule
    /// plus the reissuance bonus of the NSM value balance after the parent (Zakura
    /// `block_subsidy`, `zakura-chain/src/parameters/network/subsidy.rs:927-946`). It
    /// fails with [`ConsensusError::NegativeNsmBalance`] when that balance is negative.
    pub fn after(network: Network, height: u32, issued: u64) -> Result<Self, ConsensusError> {
        Self::terms(network, height, Some(issued))
    }

    fn terms(network: Network, height: u32, issued: Option<u64>) -> Result<Self, ConsensusError> {
        let rules = rules_at(network, height)?;
        let mut total = subsidy::total_subsidy(network, height);
        if nsm::reissuance_active(network, height) {
            let Some(issued) = issued else {
                return Err(ConsensusError::IssuedSupplyUnknown { height });
            };
            total += nsm::reissuance_bonus(nsm::balance(network, height - 1, issued)?);
        }
        let mut terms = CoinbaseTerms {
            subsidy: Subsidy { total, deferred: 0 },
            required: Vec::new(),
            disbursed: 0,
            exact_value: rules.coinbase.exact_value,
            nsm_fee_share: rules.coinbase.nsm_fee_share,
        };
        // A block without a subsidy has no required output (Zakura `subsidy_is_valid`).
        if total == 0 {
            return Ok(terms);
        }
        let mut require = |kind, value, address| {
            terms.required.push(RequiredOutput {
                kind,
                value,
                script: address_script(network, address),
            })
        };
        if let Some(reward) = founders::founders_reward(network, height) {
            require(OutputKind::FoundersReward, reward.value, reward.address);
        }
        for stream in funding::funding_streams(network, height, total) {
            match stream.address {
                Some(address) => require(
                    OutputKind::FundingStream(stream.receiver),
                    stream.value,
                    address,
                ),
                None => terms.subsidy.deferred += stream.value,
            }
        }
        if let Some(disbursement) = lockbox::disbursement(network, height) {
            for _ in 0..disbursement.count {
                require(
                    OutputKind::LockboxDisbursement,
                    disbursement.value,
                    disbursement.address,
                );
            }
            terms.disbursed = disbursement.total();
        }
        Ok(terms)
    }

    /// The part of the subsidy that the miner can pay to outputs of its choice: the
    /// subsidy without the deferred part, the founders' reward and the funding streams.
    /// The miner adds the fees of the block to it.
    pub fn miner_subsidy(&self) -> u64 {
        let required: u64 = self.required.iter().map(|output| output.value).sum();
        // The disbursement outputs are paid from the deferred pool, not from the subsidy.
        self.subsidy.total - self.subsidy.deferred - (required - self.disbursed)
    }

    /// The part of `fees`, the total fees of the block, that the coinbase gets: all of
    /// them before NU7, the miner share from NU7 (Zakura `miner_fee_share`,
    /// `zakura-chain/src/parameters/network/subsidy/fees.rs:20-41`).
    pub fn miner_fees(&self, fees: u64) -> u64 {
        match self.nsm_fee_share {
            true => nsm::miner_fee_share(fees),
            false => fees,
        }
    }

    /// The value that the coinbase takes out of the block with `fees` zatoshis of fees: the
    /// subsidy and the fees of the miner, without the deferred part, plus the lockbox
    /// disbursement.
    fn payable(&self, fees: u64) -> i128 {
        i128::from(self.subsidy.total) + i128::from(self.miner_fees(fees))
            - i128::from(self.subsidy.deferred)
            + i128::from(self.disbursed)
    }

    /// Checks the coinbase with the transparent `outputs` (value in zatoshis, script) and
    /// the `shielded` value balances in a block with `fees` zatoshis of fees.
    ///
    /// - Each required output matches one coinbase output of the same value and script
    ///   that no other required output matched.
    /// - The value that the coinbase pays is the value of its transparent outputs minus
    ///   its shielded value balances. From NU6 it equals the subsidy plus the fees of the
    ///   miner, without the deferred part, plus the lockbox disbursement. Before NU6 it is
    ///   at most that value.
    pub fn check(
        &self,
        outputs: &[(u64, &[u8])],
        shielded: ShieldedBalances,
        fees: u64,
    ) -> Result<(), CoinbaseError> {
        let mut unmatched: Vec<&(u64, &[u8])> = outputs.iter().collect();
        for required in &self.required {
            let matches = |output: &&(u64, &[u8])| {
                output.0 == required.value && output.1 == required.script.as_slice()
            };
            let Some(index) = unmatched.iter().position(matches) else {
                return Err(unmatched_error(required, &unmatched));
            };
            unmatched.swap_remove(index);
        }

        let transparent: i128 = outputs.iter().map(|(value, _)| i128::from(*value)).sum();
        let paid = transparent
            - i128::from(shielded.sapling)
            - i128::from(shielded.orchard)
            - i128::from(shielded.ironwood);
        let payable = self.payable(fees);
        if self.exact_value {
            if paid != payable {
                return Err(CoinbaseError::ValueNotExact {
                    paid,
                    required: payable,
                });
            }
        } else if paid > payable {
            return Err(CoinbaseError::ValueAboveLimit {
                paid,
                allowed: payable,
            });
        }
        Ok(())
    }

    /// The deferred pool after the block, from a pool of `before` zatoshis: the pool gains
    /// the deferred part of the subsidy and loses the lockbox disbursement.
    pub fn deferred_pool_after(&self, before: u64) -> Result<u64, CoinbaseError> {
        lockbox::deferred_pool_after(before, self.subsidy.deferred, self.disbursed).ok_or(
            CoinbaseError::NegativeDeferredPool {
                before,
                disbursed: self.disbursed,
            },
        )
    }
}

/// The error for a required output that no output of `unmatched` matches.
fn unmatched_error(required: &RequiredOutput, unmatched: &[&(u64, &[u8])]) -> CoinbaseError {
    let kind = required.kind;
    let same_script = unmatched
        .iter()
        .find(|output| output.1 == required.script.as_slice());
    let same_value = unmatched.iter().find(|output| output.0 == required.value);
    match (same_script, same_value) {
        (Some(output), _) => CoinbaseError::WrongAmount {
            kind,
            expected: required.value,
            found: output.0,
        },
        (None, Some(output)) => CoinbaseError::WrongScript {
            kind,
            value: required.value,
            expected: required.script.clone(),
            found: output.1.to_vec(),
        },
        (None, None) => CoinbaseError::MissingOutput {
            kind,
            value: required.value,
            script: required.script.clone(),
        },
    }
}

/// The `scriptPubKey` that pays the Base58Check P2SH `address` of `network` in the
/// prescribed way (protocol specification §7.10): `OP_HASH160 <script hash> OP_EQUAL`.
///
/// # Panics
///
/// When `address` is not a P2SH address of `network`. Every caller passes a constant of
/// this crate, and a test decodes each one.
pub(crate) fn address_script(network: Network, address: &str) -> Vec<u8> {
    let network_type = match network {
        Network::Mainnet => NetworkType::Main,
        Network::Testnet => NetworkType::Test,
        Network::Regtest | Network::ConfiguredRegtest(_) => NetworkType::Regtest,
    };
    let decoded = ZcashAddress::try_from_encoded(address)
        .map_err(|error| error.to_string())
        .and_then(|decoded| {
            decoded
                .convert_if_network::<TransparentAddress>(network_type)
                .map_err(|error| format!("{error:?}"))
        });
    let hash = match decoded {
        Ok(TransparentAddress::ScriptHash(hash)) => hash,
        Ok(other) => panic!("{address} is not a P2SH address: {other:?}"),
        Err(error) => panic!("{address} is not an address of {}: {error}", network.name()),
    };
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUAL: u8 = 0x87;
    let mut script = Vec::with_capacity(23);
    script.push(OP_HASH160);
    script.push(hash.len() as u8);
    script.extend_from_slice(&hash);
    script.push(OP_EQUAL);
    script
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Upgrade;

    const MINER: &[u8] = &[0x51];

    /// The outputs of a coinbase that pays `miner` zatoshis to the miner and every
    /// required output of `terms`.
    fn outputs(terms: &CoinbaseTerms, miner: u64) -> Vec<(u64, &[u8])> {
        let mut outputs = vec![(miner, MINER)];
        outputs.extend(
            terms
                .required
                .iter()
                .map(|output| (output.value, output.script.as_slice())),
        );
        outputs
    }

    fn check(
        terms: &CoinbaseTerms,
        outputs: &[(u64, &[u8])],
        fees: u64,
    ) -> Result<(), CoinbaseError> {
        terms.check(outputs, ShieldedBalances::default(), fees)
    }

    fn kinds(terms: &CoinbaseTerms) -> Vec<OutputKind> {
        terms.required.iter().map(|output| output.kind).collect()
    }

    #[test]
    fn address_script_is_the_p2sh_script_of_the_address() {
        // Mainnet block 1 pays the founders' reward to this script (Zebra vector
        // `block-main-0-000-001`).
        let script = address_script(Network::Mainnet, "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd");
        let hex: String = script.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, "a9147d46a730d31f97b1930d3368a967c309bd4d136a87");
    }

    #[test]
    #[should_panic(expected = "is not an address of testnet")]
    fn an_address_of_another_network_is_refused() {
        address_script(Network::Testnet, "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd");
    }

    #[test]
    fn terms_have_the_outputs_of_each_era() {
        const FOUNDERS: OutputKind = OutputKind::FoundersReward;
        const ECC: OutputKind = OutputKind::FundingStream(Receiver::Ecc);
        const ZF: OutputKind = OutputKind::FundingStream(Receiver::ZcashFoundation);
        const MG: OutputKind = OutputKind::FundingStream(Receiver::MajorGrants);
        const DISBURSEMENT: OutputKind = OutputKind::LockboxDisbursement;
        // (height, required kinds, miner subsidy, deferred, exact value)
        type Row = (u32, Vec<OutputKind>, u64, u64, bool);
        let mainnet: Vec<Row> = vec![
            (0, vec![], 0, 0, false),
            (1, vec![FOUNDERS], 50_000, 0, false),
            (653_600, vec![FOUNDERS], 500_000_000, 0, false),
            (1_046_399, vec![FOUNDERS], 500_000_000, 0, false),
            (1_046_400, vec![ECC, ZF, MG], 250_000_000, 0, false),
            (2_726_399, vec![ECC, ZF, MG], 250_000_000, 0, false),
            (2_726_400, vec![MG], 125_000_000, 18_750_000, true),
            (3_146_399, vec![MG], 125_000_000, 18_750_000, true),
            (4_406_399, vec![MG], 125_000_000, 18_750_000, true),
            (4_406_400, vec![], 78_125_000, 0, true),
        ];
        let testnet: Vec<Row> = vec![
            (1, vec![FOUNDERS], 50_000, 0, false),
            (1_028_499, vec![FOUNDERS], 500_000_000, 0, false),
            (1_028_500, vec![ECC, ZF, MG], 500_000_000, 0, false),
            (1_116_000, vec![ECC, ZF, MG], 250_000_000, 0, false),
            (2_796_000, vec![], 156_250_000, 0, false),
            (2_976_000, vec![MG], 125_000_000, 18_750_000, true),
            (3_396_000, vec![], 156_250_000, 0, true),
            (3_536_501, vec![MG], 125_000_000, 18_750_000, true),
        ];
        for (network, rows) in [(Network::Mainnet, mainnet), (Network::Testnet, testnet)] {
            for (height, required, miner, deferred, exact) in rows {
                let terms = CoinbaseTerms::at(network, height).unwrap();
                assert_eq!(kinds(&terms), required, "{network:?} {height}");
                assert_eq!(terms.miner_subsidy(), miner, "{network:?} {height}");
                assert_eq!(terms.subsidy.deferred, deferred, "{network:?} {height}");
                assert_eq!(terms.exact_value, exact, "{network:?} {height}");
                assert_eq!(terms.disbursed, 0);
                assert_eq!(
                    block_subsidy_of(network, height),
                    terms.subsidy,
                    "{network:?} {height}"
                );
            }
        }
        // The NU6.1 activation block: one funding stream output and ten disbursement
        // outputs. The disbursement does not change the miner's part.
        for (network, height) in [(Network::Mainnet, 3_146_400), (Network::Testnet, 3_536_500)] {
            let terms = CoinbaseTerms::at(network, height).unwrap();
            let mut required = vec![MG];
            required.extend([DISBURSEMENT; 10]);
            assert_eq!(kinds(&terms), required);
            assert_eq!(terms.disbursed, 7_875_000_000_000);
            assert_eq!(terms.miner_subsidy(), 125_000_000);
            assert_eq!(terms.subsidy.deferred, 18_750_000);
        }
        // Regtest: the miner receives the subsidy, and the value rule is the limit.
        let terms = CoinbaseTerms::at(Network::Regtest, 1).unwrap();
        assert_eq!(kinds(&terms), vec![]);
        assert_eq!(
            (terms.miner_subsidy(), terms.exact_value),
            (625_000_000, false)
        );
    }

    fn block_subsidy_of(network: Network, height: u32) -> Subsidy {
        subsidy::block_subsidy(network, height).unwrap()
    }

    #[test]
    fn a_coinbase_with_every_required_output_and_the_exact_value_is_valid() {
        for (network, height) in [
            (Network::Mainnet, 1),
            (Network::Mainnet, 1_046_400),
            (Network::Mainnet, 2_726_400),
            (Network::Mainnet, 3_146_400),
            (Network::Mainnet, 3_500_000),
            (Network::Testnet, 1_028_500),
            (Network::Testnet, 3_536_500),
            (Network::Testnet, 4_134_000),
            (Network::Regtest, 5),
        ] {
            let terms = CoinbaseTerms::at(network, height).unwrap();
            let fees = 1_234;
            let outputs = outputs(&terms, terms.miner_subsidy() + fees);
            assert_eq!(
                check(&terms, &outputs, fees),
                Ok(()),
                "{network:?} {height}"
            );
        }
    }

    #[test]
    fn a_missing_required_output_is_an_error() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 1_046_400).unwrap();
        let all = outputs(&terms, terms.miner_subsidy());
        // Output 0 is the miner output. Outputs 1 to 3 are the streams.
        for (index, receiver) in [
            (1, Receiver::Ecc),
            (2, Receiver::ZcashFoundation),
            (3, Receiver::MajorGrants),
        ] {
            let mut outputs = all.clone();
            let removed = outputs.remove(index);
            assert_eq!(
                check(&terms, &outputs, 0),
                Err(CoinbaseError::MissingOutput {
                    kind: OutputKind::FundingStream(receiver),
                    value: removed.0,
                    script: removed.1.to_vec(),
                })
            );
        }
        // The founders' reward before Canopy.
        let terms = CoinbaseTerms::at(Network::Mainnet, 20_000).unwrap();
        assert!(matches!(
            check(&terms, &[(1_250_000_000, MINER)], 0),
            Err(CoinbaseError::MissingOutput {
                kind: OutputKind::FoundersReward,
                value: 250_000_000,
                ..
            })
        ));
    }

    #[test]
    fn a_required_output_with_another_amount_is_an_error() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 2_726_400).unwrap();
        for delta in [-1i64, 1] {
            let mut outputs = outputs(&terms, terms.miner_subsidy());
            // The total stays exact: the miner output takes the difference.
            outputs[0].0 = outputs[0].0.checked_add_signed(-delta).unwrap();
            outputs[1].0 = outputs[1].0.checked_add_signed(delta).unwrap();
            assert_eq!(
                check(&terms, &outputs, 0),
                Err(CoinbaseError::WrongAmount {
                    kind: OutputKind::FundingStream(Receiver::MajorGrants),
                    expected: 12_500_000,
                    found: outputs[1].0,
                })
            );
        }
    }

    #[test]
    fn a_required_output_with_another_script_is_an_error() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 2_726_400).unwrap();
        let mut script = terms.required[0].script.clone();
        script[5] ^= 1;
        let mut outputs = outputs(&terms, terms.miner_subsidy());
        outputs[1].1 = &script;
        assert_eq!(
            check(&terms, &outputs, 0),
            Err(CoinbaseError::WrongScript {
                kind: OutputKind::FundingStream(Receiver::MajorGrants),
                value: 12_500_000,
                expected: terms.required[0].script.clone(),
                found: script.clone(),
            })
        );
    }

    #[test]
    fn each_required_output_needs_its_own_coinbase_output() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 3_146_400).unwrap();
        let all = outputs(&terms, terms.miner_subsidy());
        assert_eq!(all.len(), 12);
        assert_eq!(check(&terms, &all, 0), Ok(()));
        // Nine of the ten equal disbursement outputs. The miner output takes the value
        // of the tenth, so only the count is wrong.
        let mut nine = all.clone();
        let removed = nine.pop().unwrap();
        nine[0].0 += removed.0;
        assert_eq!(
            check(&terms, &nine, 0),
            Err(CoinbaseError::MissingOutput {
                kind: OutputKind::LockboxDisbursement,
                value: 787_500_000_000,
                script: removed.1.to_vec(),
            })
        );
    }

    #[test]
    fn from_nu6_the_value_is_exact() {
        let nu6 = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap();
        let terms = CoinbaseTerms::at(Network::Mainnet, nu6).unwrap();
        let fees = 500;
        let payable = i128::from(156_250_000u64 - 18_750_000 + fees);
        for (miner, expected) in [
            (terms.miner_subsidy() + fees, Ok(())),
            (
                terms.miner_subsidy() + fees + 1,
                Err(CoinbaseError::ValueNotExact {
                    paid: payable + 1,
                    required: payable,
                }),
            ),
            (
                terms.miner_subsidy() + fees - 1,
                Err(CoinbaseError::ValueNotExact {
                    paid: payable - 1,
                    required: payable,
                }),
            ),
            // The deferred part is not paid out.
            (
                terms.miner_subsidy() + fees + terms.subsidy.deferred,
                Err(CoinbaseError::ValueNotExact {
                    paid: payable + 18_750_000,
                    required: payable,
                }),
            ),
        ] {
            assert_eq!(check(&terms, &outputs(&terms, miner), fees), expected);
        }
    }

    #[test]
    fn before_nu6_the_value_is_a_limit() {
        let nu6 = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap();
        let terms = CoinbaseTerms::at(Network::Mainnet, nu6 - 1).unwrap();
        let fees = 500;
        let limit = terms.miner_subsidy() + fees;
        assert_eq!(check(&terms, &outputs(&terms, limit), fees), Ok(()));
        assert_eq!(check(&terms, &outputs(&terms, limit - 1), fees), Ok(()));
        assert_eq!(check(&terms, &outputs(&terms, 0), fees), Ok(()));
        assert_eq!(
            check(&terms, &outputs(&terms, limit + 1), fees),
            Err(CoinbaseError::ValueAboveLimit {
                paid: 312_500_501,
                allowed: 312_500_500,
            })
        );
    }

    #[test]
    fn value_that_enters_a_shielded_pool_is_paid_value() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 3_500_000).unwrap();
        let miner = terms.miner_subsidy();
        // The miner's part goes to three shielded pools and one transparent output.
        let shielded = ShieldedBalances {
            sapling: -100,
            orchard: -20,
            ironwood: -3,
        };
        let outputs = outputs(&terms, miner - 123);
        assert_eq!(terms.check(&outputs, shielded, 0), Ok(()));
        assert_eq!(
            check(&terms, &outputs, 0),
            Err(CoinbaseError::ValueNotExact {
                paid: 137_500_000 - 123,
                required: 137_500_000,
            })
        );
    }

    #[test]
    fn the_deferred_pool_follows_the_terms() {
        let terms = CoinbaseTerms::at(Network::Mainnet, 2_726_400).unwrap();
        assert_eq!(terms.deferred_pool_after(0), Ok(18_750_000));
        let before = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap() - 1;
        assert_eq!(
            CoinbaseTerms::at(Network::Mainnet, before)
                .unwrap()
                .deferred_pool_after(7),
            Ok(7)
        );
        // The pool at the NU6.1 activation: 420,000 blocks of 0.1875 ZEC = 78,750 ZEC.
        // The activation block adds its part and pays out 78,750 ZEC.
        let terms = CoinbaseTerms::at(Network::Mainnet, 3_146_400).unwrap();
        let pool = 420_000 * 18_750_000;
        assert_eq!(pool, terms.disbursed);
        assert_eq!(terms.deferred_pool_after(pool), Ok(18_750_000));
        assert_eq!(
            terms.deferred_pool_after(pool - 18_750_001),
            Err(CoinbaseError::NegativeDeferredPool {
                before: pool - 18_750_001,
                disbursed: 7_875_000_000_000,
            })
        );
    }

    /// A chain from the genesis block: the deferred pool is zero before NU6, it holds the
    /// whole disbursement at the NU6.1 activation block on Mainnet and on Testnet, and it
    /// is not zero after that block.
    #[test]
    fn the_deferred_pool_of_a_chain_pays_the_disbursement() {
        for network in [Network::Mainnet, Network::Testnet] {
            let nu6 = network.activation_height(Upgrade::Nu6).unwrap();
            let nu6_1 = network.activation_height(Upgrade::Nu6_1).unwrap();
            assert_eq!(block_subsidy_of(network, nu6 - 1).deferred, 0);
            let mut pool = 0u64;
            for height in nu6..nu6_1 {
                pool += block_subsidy_of(network, height).deferred;
            }
            let terms = CoinbaseTerms::at(network, nu6_1).unwrap();
            assert_eq!(pool, terms.disbursed, "{network:?}");
            let after = terms.deferred_pool_after(pool).unwrap();
            assert_eq!(after, terms.subsidy.deferred);
            assert!(after > 0, "{network:?}");
        }
    }

    /// The terms across NU7 on Testnet. With the NU7 rule set: a third of the subsidy,
    /// the miner share of the fees, and the exact value rule on that share. Without it:
    /// an error.
    #[test]
    fn the_terms_at_the_nu7_boundary() {
        let Some(nu7) = Network::Testnet.activation_height(Upgrade::Nu7) else {
            panic!("Testnet has an NU7 height on every backend");
        };
        let before = CoinbaseTerms::at(Network::Testnet, nu7 - 1).unwrap();
        assert!(!before.nsm_fee_share);
        assert_eq!(before.miner_fees(1_000), 1_000);
        assert_eq!(before.miner_subsidy(), 125_000_000);
        for height in [nu7, nu7 + 1] {
            let Some(_) = crate::RuleSet::of(Upgrade::Nu7) else {
                assert_eq!(
                    CoinbaseTerms::at(Network::Testnet, height),
                    Err(ConsensusError::UnsupportedUpgrade {
                        upgrade: Upgrade::Nu7,
                        height,
                    })
                );
                continue;
            };
            let terms = CoinbaseTerms::at(Network::Testnet, height).unwrap();
            assert_eq!(
                CoinbaseTerms::after(Network::Testnet, height, 0),
                Ok(terms.clone())
            );
            assert!(terms.nsm_fee_share && terms.exact_value);
            assert_eq!(terms.subsidy.total, 52_083_333);
            assert_eq!(terms.subsidy.deferred, 6_249_999);
            assert_eq!(
                kinds(&terms),
                [OutputKind::FundingStream(Receiver::MajorGrants)]
            );
            assert_eq!(terms.miner_subsidy(), 52_083_333 - 6_249_999 - 4_166_666);
            // Fees of 1,001: 600 stay out of the pools, the miner gets 401.
            assert_eq!(terms.miner_fees(1_001), 401);
            let miner = terms.miner_subsidy() + 401;
            assert_eq!(check(&terms, &outputs(&terms, miner), 1_001), Ok(()));
            for wrong in [miner - 1, miner + 1, terms.miner_subsidy() + 1_001] {
                let Err(CoinbaseError::ValueNotExact { .. }) =
                    check(&terms, &outputs(&terms, wrong), 1_001)
                else {
                    panic!("a coinbase that pays {wrong} at {height}");
                };
            }
        }
    }

    /// From the NSM reissuance height the subsidy has the bonus of the balance after the
    /// parent: `ceil(balance * 1,375 / 10,000,000,000)`.
    #[test]
    fn the_subsidy_has_the_reissuance_bonus_from_the_reissuance_height() {
        let network = Network::Testnet;
        let Some(start) = crate::nsm::reissuance_height(network) else {
            panic!("Testnet has a reissuance height");
        };
        let Some(_) = crate::RuleSet::of(Upgrade::Nu7) else {
            return;
        };
        let Ok(scheduled) = u64::try_from(subsidy::scheduled_issuance(network, start - 1)) else {
            panic!("the Testnet schedule is below MAX_MONEY");
        };
        let halving_subsidy = subsidy::total_subsidy(network, start);
        // Before the height: no bonus, with or without the pools.
        let before = CoinbaseTerms::at(network, start - 1).unwrap();
        assert_eq!(CoinbaseTerms::after(network, start - 1, 0), Ok(before));
        // At the height: the pools are necessary.
        assert_eq!(
            CoinbaseTerms::at(network, start),
            Err(ConsensusError::IssuedSupplyUnknown { height: start })
        );
        for (balance, bonus) in [
            (0, 0),
            (1, 1),
            (10_000_000_000, 1_375),
            (10_000_000_001, 1_376),
        ] {
            let terms = CoinbaseTerms::after(network, start, scheduled - balance).unwrap();
            assert_eq!(terms.subsidy.total, halving_subsidy + bonus, "{balance}");
            assert_eq!(terms.miner_subsidy(), halving_subsidy + bonus);
        }
        // Pools above the scheduled issuance: the balance after the parent is negative.
        assert_eq!(
            CoinbaseTerms::after(network, start, scheduled + 1),
            Err(ConsensusError::NegativeNsmBalance {
                height: start - 1,
                scheduled: u128::from(scheduled),
                issued: scheduled + 1,
            })
        );
    }
}
