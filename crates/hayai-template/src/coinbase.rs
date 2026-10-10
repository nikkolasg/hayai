//! Coinbase construction for a template height.
//!
//! The block subsidy and its split between the miner and the required outputs are consensus
//! rules of hayai-consensus. [`CoinbaseSpec::build`] takes the terms of the height from
//! `CoinbaseTerms::at`, the function that block validation uses: the founders' reward, the
//! funding streams and the lockbox disbursement, with the deferred (lockbox) part left out
//! of the miner share. The template only adds the collected fees to the miner output, so
//! the coinbase pays the exact value that ZIP 236 requires from NU6. The coinbase is a v5
//! transaction with one transparent output per subsidy recipient. This module produces no
//! shielded coinbase output. The consensus branch id is the one of the rule set of the
//! height (`hayai_consensus::rules_at`).

use std::fmt;

use bytes::Bytes;
use hayai_consensus::{rules_at, ConsensusError, Network};
use hayai_crypto::{
    zcash_primitives, zcash_protocol, zcash_script04 as zcash_script, zcash_transparent,
};
use zcash_primitives::transaction::{Authorized, TransactionData, TxId, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::Zatoshis;
use zcash_script::pv::push_value;
use zcash_script::script::Code;
use zcash_transparent::address::Script;
use zcash_transparent::builder::Coinbase as CoinbaseAuth;
use zcash_transparent::bundle::{Bundle, TxIn, TxOut};
use zcash_transparent::coinbase::MAX_COINBASE_SCRIPT_LEN;

/// Everything necessary to build the coinbase of any height.
#[derive(Clone)]
pub struct CoinbaseSpec {
    /// Script that the miner output pays to.
    pub script_pubkey: Vec<u8>,
    /// Pool tag or extra-nonce bytes that follow the height in the coinbase scriptSig.
    pub miner_data: Vec<u8>,
    /// The network: with the height it gives the coinbase terms
    /// (`hayai_consensus::coinbase::CoinbaseTerms`) and the consensus branch id.
    pub network: Network,
}

impl fmt::Debug for CoinbaseSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoinbaseSpec")
            .field("script_pubkey", &hex::encode(&self.script_pubkey))
            .field("miner_data", &hex::encode(&self.miner_data))
            .field("network", &self.network)
            .finish()
    }
}

/// A built coinbase: wire bytes plus the ids that the block roots need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseTx {
    pub bytes: Bytes,
    pub txid: TxId,
    pub auth_digest: [u8; 32],
    /// Bytes that a pool can still add to the scriptSig before it exceeds the consensus maximum.
    /// The block byte budget reserves them, so that an override still fits.
    pub script_slack: usize,
    /// Legacy sigop count of the scriptSig and the output scripts (zcashd
    /// `GetLegacySigOpCount`). The block sigop budget reserves it.
    pub sigops: u32,
    /// Consensus branch id of the height that the coinbase is built for.
    pub branch_id: BranchId,
    /// The part of the fees of the block that the miner output holds: all of them before
    /// NU7, the miner share from NU7 (`CoinbaseTerms::miner_fees`).
    pub miner_fees: u64,
}

#[derive(thiserror::Error, Debug)]
pub enum CoinbaseError {
    #[error("coinbase scriptSig: {0}")]
    Script(zcash_transparent::coinbase::Error),
    #[error("miner data of {0} bytes is not a single push")]
    MinerData(usize),
    #[error("coinbase output value {0} zatoshis exceeds MAX_MONEY")]
    Value(u64),
    #[error("coinbase serialization: {0}")]
    Io(#[from] std::io::Error),
    /// The height has no rule set, and thus no branch id and no coinbase terms.
    #[error("coinbase terms: {0}")]
    Terms(#[from] ConsensusError),
}

impl CoinbaseSpec {
    /// The coinbase for `height` for a caller without the chain value pools
    /// ([`CoinbaseSpec::build_on`] with no issued supply). It is an error from the NSM
    /// reissuance height.
    pub fn build(&self, height: u32, fees_total: u64) -> Result<CoinbaseTx, CoinbaseError> {
        self.build_on(height, fees_total, None)
    }

    /// Builds the coinbase for `height`: the miner output with the miner's part of the
    /// subsidy and the part of `fees_total` that the terms give to the miner (all of the
    /// fees before NU7, the miner share from NU7), then every required output of the
    /// terms of the height, in the order of the terms. `issued` is the total of the chain
    /// value pools after the parent block: with it the terms are those of block
    /// validation (`CoinbaseTerms::after`), with the reissuance bonus from the NSM
    /// reissuance height. The coinbase passes `CoinbaseTerms::check` at `height` with
    /// fees of `fees_total`. A height without a rule set is an error, and so is a height
    /// from the NSM reissuance height without `issued`.
    ///
    /// ZIP 236: the miner output takes the rest of the total input value, so the coinbase
    /// pays it exactly. ZIP 235: from NU7 the miner output has `MinerFees`, not the fees.
    pub fn build_on(
        &self,
        height: u32,
        fees_total: u64,
        issued: Option<u64>,
    ) -> Result<CoinbaseTx, CoinbaseError> {
        let branch_id = rules_at(self.network, height)?.branch_id;
        let terms = match issued {
            Some(issued) => hayai_consensus::coinbase::terms_after(self.network, height, issued)?,
            None => hayai_consensus::coinbase::terms_at(self.network, height)?,
        };
        let miner_fees = terms.miner_fees(fees_total)?;
        let miner_data = match self.miner_data.as_slice() {
            [] => None,
            data => Some(push_value(data).ok_or(CoinbaseError::MinerData(data.len()))?),
        };
        let input = TxIn::<CoinbaseAuth>::coinbase(BlockHeight::from_u32(height), miner_data)
            .map_err(CoinbaseError::Script)?;
        let miner_value = terms
            .miner_subsidy
            .checked_add(miner_fees)
            .ok_or(CoinbaseError::Value(u64::MAX))?;
        let mut vout = vec![TxOut::new(
            zatoshis(miner_value)?,
            Script(Code(self.script_pubkey.clone())),
        )];
        for output in terms.required {
            vout.push(TxOut::new(
                zatoshis(output.value)?,
                Script(Code(output.script.to_vec())),
            ));
        }
        let bundle = Bundle {
            vin: vec![input],
            vout,
            authorization: CoinbaseAuth,
        }
        .map_authorization(CoinbaseAuth);
        let script_len = bundle.vin[0].script_sig().0 .0.len();
        let sigops = bundle.vin[0].script_sig().0.sig_op_count(false)
            + bundle
                .vout
                .iter()
                .map(|out| out.script_pubkey().0.sig_op_count(false))
                .sum::<u32>();
        let tx = TransactionData::<Authorized>::from_parts(
            TxVersion::V5,
            branch_id,
            0,
            BlockHeight::from_u32(height),
            Some(bundle),
            None,
            None,
            None,
        )
        .freeze()?;
        let mut bytes = Vec::with_capacity(256);
        tx.write(&mut bytes)?;
        Ok(CoinbaseTx {
            bytes: Bytes::from(bytes),
            txid: tx.txid(),
            auth_digest: tx
                .auth_commitment()
                .as_bytes()
                .try_into()
                .expect("ZIP 244 digests are 32 bytes"),
            script_slack: MAX_COINBASE_SCRIPT_LEN.saturating_sub(script_len),
            sigops,
            branch_id,
            miner_fees,
        })
    }
}

fn zatoshis(value: u64) -> Result<Zatoshis, CoinbaseError> {
    Zatoshis::from_u64(value).map_err(|_| CoinbaseError::Value(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::coinbase_spec;
    use hayai_consensus::coinbase::{
        CoinbaseError as ConsensusCoinbaseError, CoinbaseOutput, CoinbaseTerms, ShieldedBalances,
    };
    use hayai_consensus::Upgrade;
    use hayai_crypto::zcash_primitives::transaction::Transaction;

    #[test]
    fn coinbase_is_a_valid_v5_transaction_with_fees_added() {
        // Mainnet in NU6: the miner's part and one funding stream output.
        let spec = coinbase_spec();
        assert_eq!(spec.network, Network::Mainnet);
        let cb = spec.build(3_000_000, 1234).unwrap();
        assert_eq!(cb.branch_id, BranchId::Nu6);
        let tx = Transaction::read(cb.bytes.as_ref(), cb.branch_id).unwrap();
        assert_eq!(tx.txid(), cb.txid);
        assert_eq!(tx.auth_commitment().as_bytes(), &cb.auth_digest[..]);
        assert_eq!(u32::from(tx.expiry_height()), 3_000_000);
        let bundle = tx.transparent_bundle().unwrap();
        assert!(bundle.is_coinbase());
        assert_eq!(bundle.vout.len(), 2);
        assert_eq!(bundle.vout[0].value().into_u64(), 125_000_000 + 1234);
        assert_eq!(bundle.vout[1].value().into_u64(), 12_500_000);
        assert!(cb.script_slack < MAX_COINBASE_SCRIPT_LEN);
        // The script of the fixture ends in the middle of a push, and the funding stream
        // pays a P2SH script: no sigop.
        assert_eq!(cb.sigops, 0);
        // The subsidy-only coinbase differs only in the miner output value. The size is the same.
        let empty = spec.build(3_000_000, 0).unwrap();
        assert_eq!(empty.bytes.len(), cb.bytes.len());
        assert_ne!(empty.txid, cb.txid);
    }

    /// The outputs of the coinbase `bytes` as hayai-consensus takes them.
    fn outputs_of(bytes: &[u8], branch_id: BranchId) -> Vec<(u64, Vec<u8>)> {
        let tx = Transaction::read(bytes, branch_id).unwrap();
        let bundle = tx.transparent_bundle().unwrap();
        bundle
            .vout
            .iter()
            .map(|out| (out.value().into_u64(), out.script_pubkey().0 .0.clone()))
            .collect()
    }

    fn check(
        terms: &CoinbaseTerms,
        outputs: &[(u64, Vec<u8>)],
        fees: u64,
    ) -> Result<(), ConsensusCoinbaseError> {
        let outputs: Vec<CoinbaseOutput> = outputs
            .iter()
            .map(|(value, script)| CoinbaseOutput {
                value: *value,
                script: script.clone(),
            })
            .collect();
        terms.check(&outputs, ShieldedBalances::default(), fees)
    }

    /// Heights from NU5 (the coinbase is a v5 transaction): each funding stream range,
    /// the gaps without a stream, the NU6.1 activation blocks and the current upgrades.
    const HEIGHTS: [(Network, u32); 12] = [
        (Network::Mainnet, 1_687_104),
        (Network::Mainnet, 2_726_399),
        (Network::Mainnet, 2_726_400),
        (Network::Mainnet, 3_146_400),
        (Network::Mainnet, 3_428_143),
        (Network::Mainnet, 3_505_115),
        (Network::Testnet, 1_842_420),
        (Network::Testnet, 2_800_000),
        (Network::Testnet, 2_976_000),
        (Network::Testnet, 3_396_000),
        (Network::Testnet, 3_536_500),
        (Network::Testnet, 4_134_000),
    ];

    fn spec_on(network: Network) -> CoinbaseSpec {
        CoinbaseSpec {
            network,
            ..coinbase_spec()
        }
    }

    /// One spec builds the coinbase of each height with the branch id of the rule set of
    /// that height.
    #[test]
    fn the_branch_id_is_the_one_of_the_rule_set_of_the_height() {
        let mut branches = Vec::new();
        for (network, height) in HEIGHTS {
            let cb = spec_on(network).build(height, 0).unwrap();
            assert_eq!(cb.branch_id, rules_at(network, height).unwrap().branch_id);
            let tx = Transaction::read(cb.bytes.as_ref(), BranchId::Sprout).unwrap();
            assert_eq!(tx.consensus_branch_id(), cb.branch_id);
            branches.push(cb.branch_id);
        }
        branches.dedup();
        assert!(branches.len() > 3, "the heights cover several upgrades");
        // The last block of an upgrade and the first block of the next one.
        let nu6 = Network::Mainnet.activation_height(Upgrade::Nu6).unwrap();
        let spec = spec_on(Network::Mainnet);
        assert_eq!(spec.build(nu6 - 1, 0).unwrap().branch_id, BranchId::Nu5);
        assert_eq!(spec.build(nu6, 0).unwrap().branch_id, BranchId::Nu6);
    }

    #[test]
    fn the_coinbase_of_the_consensus_rule_passes_the_coinbase_check() {
        for (network, height) in HEIGHTS {
            let spec = spec_on(network);
            let terms = hayai_consensus::coinbase::terms_at(network, height).unwrap();
            for fees in [0, 98_765] {
                let cb = spec.build(height, fees).unwrap();
                let outputs = outputs_of(&cb.bytes, cb.branch_id);
                assert_eq!(outputs.len(), 1 + terms.required.len());
                assert_eq!(outputs[0].0, terms.miner_subsidy + fees);
                assert_eq!(outputs[0].1, spec.script_pubkey);
                assert_eq!(
                    check(&terms, &outputs, fees),
                    Ok(()),
                    "{network:?} {height} fees {fees}"
                );
            }
        }
        // The NU6.1 activation block pays the stream and the ten disbursement outputs.
        let cb = spec_on(Network::Mainnet).build(3_146_400, 0).unwrap();
        let outputs = outputs_of(&cb.bytes, cb.branch_id);
        let values: Vec<u64> = outputs.iter().map(|(value, _)| *value).collect();
        let mut expected = vec![125_000_000, 12_500_000];
        expected.extend([787_500_000_000; 10]);
        assert_eq!(values, expected);
    }

    #[test]
    fn a_changed_coinbase_of_the_consensus_rule_fails_the_coinbase_check() {
        let fees = 4_321;
        for (network, height) in HEIGHTS {
            let spec = spec_on(network);
            let terms = hayai_consensus::coinbase::terms_at(network, height).unwrap();
            let cb = spec.build(height, fees).unwrap();
            let outputs = outputs_of(&cb.bytes, cb.branch_id);
            let context = format!("{network:?} {height}");

            // One zatoshi more in the miner output.
            let mut more = outputs.clone();
            more[0].0 += 1;
            match (check(&terms, &more, fees), terms.exact_value) {
                (Err(ConsensusCoinbaseError::ValueNotExact { .. }), true)
                | (Err(ConsensusCoinbaseError::ValueAboveLimit { .. }), false) => {}
                other => panic!("{context}: {other:?}"),
            }
            // One zatoshi less, and the fees of another block: an error from NU6 only.
            let mut less = outputs.clone();
            less[0].0 -= 1;
            for result in [
                check(&terms, &less, fees),
                check(&terms, &outputs, fees + 1),
            ] {
                match (result, terms.exact_value) {
                    (Err(ConsensusCoinbaseError::ValueNotExact { .. }), true) | (Ok(()), false) => {
                    }
                    other => panic!("{context}: {other:?}"),
                }
            }

            for index in 1..outputs.len() {
                // One required output short.
                let mut short = outputs.clone();
                short.remove(index);
                assert!(
                    matches!(
                        check(&terms, &short, fees),
                        Err(ConsensusCoinbaseError::MissingOutput { .. })
                    ),
                    "{context} output {index}"
                );
                // A wrong amount. The miner output keeps the total exact.
                let mut amount = outputs.clone();
                amount[index].0 -= 1;
                amount[0].0 += 1;
                assert!(
                    matches!(
                        check(&terms, &amount, fees),
                        Err(ConsensusCoinbaseError::WrongAmount { .. })
                    ),
                    "{context} output {index}"
                );
                // A wrong script.
                let mut script = outputs.clone();
                script[index].1[2] ^= 0x80;
                assert!(
                    matches!(
                        check(&terms, &script, fees),
                        Err(ConsensusCoinbaseError::WrongScript { .. })
                    ),
                    "{context} output {index}"
                );
            }
        }
    }

    /// The coinbase across NU7 on Testnet and on a configured Regtest. With the NU7 rule
    /// set: the NU7 branch id, a third of the subsidy, and the miner share of the fees;
    /// the coinbase passes the coinbase check of the height. Without it: the build
    /// returns the error of the terms. The block before NU7 has the NU6.3 coinbase.
    #[test]
    fn the_coinbase_follows_the_rules_at_the_nu7_boundary() {
        use hayai_consensus::{RegtestConfig, RuleSet};

        let regtest = RegtestConfig::new(&[(Upgrade::Nu6_3, 2), (Upgrade::Nu7, 9)], Vec::new(), 0)
            .expect("a valid configuration")
            .network();
        let Some(testnet_nu7) = Network::Testnet.activation_height(Upgrade::Nu7) else {
            panic!("Testnet has an NU7 height on every backend");
        };
        let fees = 1_001;
        for (network, nu7) in [(Network::Testnet, testnet_nu7), (regtest, 9)] {
            let spec = spec_on(network);
            let before = spec.build(nu7 - 1, fees).unwrap();
            assert_eq!(before.branch_id, BranchId::Nu6_3);
            let terms = hayai_consensus::coinbase::terms_at(network, nu7 - 1).unwrap();
            let outputs = outputs_of(&before.bytes, before.branch_id);
            assert_eq!(outputs[0].0, terms.miner_subsidy + fees);
            assert_eq!(check(&terms, &outputs, fees), Ok(()));
            for height in [nu7, nu7 + 1] {
                let Some(rules) = RuleSet::of(Upgrade::Nu7) else {
                    assert!(matches!(
                        spec.build(height, fees),
                        Err(CoinbaseError::Terms(ConsensusError::UnsupportedUpgrade {
                            upgrade: Upgrade::Nu7,
                            height: refused,
                        })) if refused == height
                    ));
                    continue;
                };
                let cb = spec.build(height, fees).unwrap();
                assert_eq!(cb.branch_id, rules.branch_id);
                let tx = Transaction::read(cb.bytes.as_ref(), BranchId::Sprout).unwrap();
                assert_eq!(tx.consensus_branch_id(), rules.branch_id);
                let terms = hayai_consensus::coinbase::terms_at(network, height).unwrap();
                let outputs = outputs_of(&cb.bytes, cb.branch_id);
                assert_eq!(outputs.len(), 1 + terms.required.len());
                // 600 of the 1,001 zatoshis of fees stay out of the coinbase.
                assert_eq!(outputs[0].0, terms.miner_subsidy + 401);
                assert_eq!(
                    check(&terms, &outputs, fees),
                    Ok(()),
                    "{network:?} {height}"
                );
                // The same outputs break the terms of a block with other fees.
                let Err(ConsensusCoinbaseError::ValueNotExact { .. }) =
                    check(&terms, &outputs, fees + 10)
                else {
                    panic!("{network:?} {height}: the value rule is exact");
                };
            }
        }
        if let Some(rules) = RuleSet::of(Upgrade::Nu7) {
            let cb = spec_on(regtest).build(9, 0).unwrap();
            assert_eq!(cb.branch_id, rules.branch_id);
            assert_eq!(outputs_of(&cb.bytes, cb.branch_id)[0].0, 208_333_333);
        }
    }

    /// The coinbase at the NSM reissuance height of a test Regtest (NU7 at 9, reissuance
    /// at 12), at the height before and at the height after. With the issued supply
    /// after the parent, the miner output has the reissuance bonus of the NSM value
    /// balance, `ceil(balance * 1,375 / 10,000,000,000)`, and the coinbase passes the
    /// terms of block validation (`CoinbaseTerms::after`). Without the issued supply the
    /// build is an error from the reissuance height.
    #[test]
    fn the_coinbase_has_the_reissuance_bonus_from_the_reissuance_height() {
        use hayai_consensus::{nsm, subsidy, RegtestConfig, RuleSet};

        let network = RegtestConfig::new(&[(Upgrade::Nu7, 9)], Vec::new(), 0)
            .expect("a valid configuration")
            .with_test_reissuance_height(12)
            .network();
        assert_eq!(nsm::reissuance_height(network), Some(12));
        let spec = spec_on(network);
        let Some(_) = RuleSet::of(Upgrade::Nu7) else {
            assert!(matches!(
                spec.build_on(12, 0, Some(0)),
                Err(CoinbaseError::Terms(
                    ConsensusError::UnsupportedUpgrade { .. }
                ))
            ));
            return;
        };
        let fees = 1_001;
        let balance = 2_000_000_001u64;
        for (height, bonus) in [(11, 0), (12, 276), (13, 276)] {
            let scheduled = subsidy::scheduled_issuance(network, height - 1);
            let issued = u64::try_from(scheduled).expect("fits") - balance;
            let cb = spec.build_on(height, fees, Some(issued)).unwrap();
            assert_eq!(cb.miner_fees, 401);
            let outputs = outputs_of(&cb.bytes, cb.branch_id);
            assert_eq!(outputs.len(), 1, "Regtest has no funding stream");
            assert_eq!(outputs[0].0, 208_333_333 + bonus + 401, "{height}");
            let terms = hayai_consensus::coinbase::terms_after(network, height, issued).unwrap();
            assert_eq!(terms.subsidy.total, 208_333_333 + bonus);
            assert_eq!(check(&terms, &outputs, fees), Ok(()), "{height}");
            // The terms of other pools refuse the coinbase when the bonus differs.
            let other =
                hayai_consensus::coinbase::terms_after(network, height, issued - 1_000_000_000)
                    .unwrap();
            assert_eq!(
                check(&other, &outputs, fees).is_ok(),
                bonus == 0,
                "{height}"
            );
            // Without the issued supply: the same coinbase before the reissuance height,
            // an error from it.
            match bonus {
                0 => assert_eq!(spec.build(height, fees).unwrap(), cb),
                _ => assert!(matches!(
                    spec.build(height, fees),
                    Err(CoinbaseError::Terms(ConsensusError::IssuedSupplyUnknown { height: h }))
                        if h == height
                )),
            }
        }
        // A balance of zero gives no bonus, and pools above the schedule are an error.
        let scheduled = u64::try_from(subsidy::scheduled_issuance(network, 11)).expect("fits");
        let cb = spec.build_on(12, 0, Some(scheduled)).unwrap();
        assert_eq!(outputs_of(&cb.bytes, cb.branch_id)[0].0, 208_333_333);
        assert!(matches!(
            spec.build_on(12, 0, Some(scheduled + 1)),
            Err(CoinbaseError::Terms(
                ConsensusError::NegativeNsmBalance { .. }
            ))
        ));
    }

    /// The sigop count is the legacy count of the output scripts: 1 for `OP_CHECKSIG`, 20
    /// for `OP_CHECKMULTISIG`.
    #[test]
    fn coinbase_sigops_are_the_legacy_count() {
        let mut spec = coinbase_spec();
        spec.script_pubkey = [&[0x76, 0xa9, 0x14][..], &[7; 20], &[0x88, 0xac]].concat();
        assert_eq!(spec.build(3_000_000, 0).unwrap().sigops, 1);
        spec.script_pubkey = [&[0x51, 0x21][..], &[2; 33], &[0x51, 0xae]].concat();
        assert_eq!(spec.build(3_000_000, 0).unwrap().sigops, 20);
    }

    #[test]
    fn miner_data_must_be_pushable() {
        let mut spec = coinbase_spec();
        spec.miner_data = vec![7u8; 600];
        assert!(matches!(
            spec.build(10, 0),
            Err(CoinbaseError::MinerData(600))
        ));
        spec.miner_data = vec![7u8; 99];
        assert!(matches!(
            spec.build(10, 0),
            Err(CoinbaseError::Script(
                zcash_transparent::coinbase::Error::OversizedScript
            ))
        ));
    }

    /// The coinbase on a Regtest network with configured funding streams and lockbox
    /// disbursements: the template has each required output of the terms, and the
    /// coinbase passes the coinbase check of the height. A network without a disbursement
    /// has no coinbase at its NU6.1 height, as it has no valid block there.
    #[test]
    fn the_coinbase_has_the_streams_and_the_disbursements_of_a_configured_regtest() {
        use hayai_consensus::funding::Receiver;
        use hayai_consensus::{
            RegtestConfig, RegtestDisbursement, RegtestFundingStreams, RegtestRecipient,
        };

        const ADDRESS: &str = "t2SRyAR26tXTnZHfpa3jPqeyYmxCbAZxUnh";
        let config = || {
            RegtestConfig::new(&[(Upgrade::Nu6, 5), (Upgrade::Nu6_1, 13)], Vec::new(), 0)
                .expect("a valid configuration")
        };
        let recipient = |receiver, numerator, addresses: &[&str]| RegtestRecipient {
            receiver,
            numerator,
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
        };
        let disbursement = |amount| RegtestDisbursement {
            address: ADDRESS.to_string(),
            amount,
        };
        // The heights 11 to 16 are one address period.
        let streams = [RegtestFundingStreams {
            height_range: 11..17,
            recipients: vec![
                recipient(Receiver::Deferred, 12, &[]),
                recipient(Receiver::MajorGrants, 8, &[ADDRESS]),
            ],
        }];
        let network = config()
            .with_lockbox_disbursements(vec![disbursement(150_000_000), disbursement(0)])
            .and_then(|config| config.with_funding_streams(&streams))
            .expect("a valid configuration")
            .network();
        let spec = spec_on(network);
        let fees = 1_000;
        // (height, outputs after the miner output, value of the miner output)
        for (height, required, miner) in [
            (10, vec![], 625_000_000),
            (11, vec![50_000_000], 500_000_000),
            (12, vec![50_000_000], 500_000_000),
            (13, vec![50_000_000, 150_000_000, 0], 500_000_000),
            (14, vec![50_000_000], 500_000_000),
            (17, vec![], 625_000_000),
        ] {
            let cb = spec.build(height, fees).unwrap();
            let outputs = outputs_of(&cb.bytes, cb.branch_id);
            let values: Vec<u64> = outputs.iter().map(|(value, _)| *value).collect();
            let mut expected = vec![miner + fees];
            expected.extend(required);
            assert_eq!(values, expected, "{height}");
            let terms = hayai_consensus::coinbase::terms_at(network, height).unwrap();
            assert_eq!(check(&terms, &outputs, fees), Ok(()), "{height}");
        }
        // The deferred pool gets 2 times 75,000,000 zatoshis before the NU6.1 block and
        // pays the disbursement in it.
        let terms = hayai_consensus::coinbase::terms_at(network, 13).unwrap();
        assert_eq!(terms.deferred_pool_after(150_000_000), Ok(75_000_000));

        let none = spec_on(config().network());
        assert!(matches!(
            none.build(13, fees),
            Err(CoinbaseError::Terms(
                ConsensusError::NoLockboxDisbursement { height: 13 }
            ))
        ));
        let Ok(_) = none.build(12, fees) else {
            panic!("the block before the NU6.1 height has a coinbase");
        };
    }
}
