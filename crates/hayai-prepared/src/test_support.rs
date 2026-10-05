//! Synthetic prepared transactions for the policy tests and the store tests. The scripts
//! and the signatures are not valid: the policy and the store do not verify them.

use std::sync::Arc;

use bytes::Bytes;
use hayai_coins::{Coin, OutPoint};
use hayai_crypto::{zcash_primitives, zcash_protocol, zcash_script04, zcash_transparent};
use hayai_wire::RawTx;
use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::Zatoshis;
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Authorized as TAuthorized, Bundle, TxIn, TxOut};

use crate::{Commitments, PreparedTx, RuleEpoch};

pub const BRANCH: BranchId = BranchId::Nu6_2;

/// A push of `data` with the shortest push opcode.
pub fn push(data: &[u8]) -> Vec<u8> {
    let mut script = match data.len() {
        0 => return vec![0x00],
        n @ 1..=75 => vec![n as u8],
        n @ 76..=255 => vec![0x4c, n as u8],
        n => vec![0x4d, (n & 0xff) as u8, (n >> 8) as u8],
    };
    script.extend_from_slice(data);
    script
}

/// `OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG`.
pub fn p2pkh(tag: u8) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9];
    script.extend(push(&[tag; 20]));
    script.extend([0x88, 0xac]);
    script
}

/// `OP_HASH160 <20 bytes> OP_EQUAL`.
pub fn p2sh(tag: u8) -> Vec<u8> {
    let mut script = vec![0xa9];
    script.extend(push(&[tag; 20]));
    script.push(0x87);
    script
}

/// `OP_<required> <keys compressed public keys> OP_<keys> OP_CHECKMULTISIG`.
pub fn multisig(required: u8, keys: usize) -> Vec<u8> {
    let mut script = vec![0x50 + required];
    for key in 0..keys {
        let mut public_key = [key as u8; 33];
        public_key[0] = 0x02;
        script.extend(push(&public_key));
    }
    script.extend([0x50 + keys as u8, 0xae]);
    script
}

/// An `OP_RETURN` script of `len` bytes (3 or more, at most 258).
pub fn op_return(len: usize) -> Vec<u8> {
    let mut script = vec![0x6a];
    let data = if len - 1 <= 76 { len - 2 } else { len - 3 };
    script.extend(push(&vec![0xee; data]));
    assert_eq!(script.len(), len);
    script
}

/// A scriptSig with the two pushes of a P2PKH spend: a 71-byte signature and a 33-byte key.
pub const P2PKH_SIG: [u8; 106] = {
    let mut script = [7u8; 106];
    script[0] = 71;
    script[72] = 33;
    script
};

/// The shape of a synthetic transparent v5 transaction.
#[derive(Clone)]
pub struct TxSpec {
    /// `(scriptSig, scriptPubKey of the spent coin, value of the spent coin)`.
    pub inputs: Vec<(Vec<u8>, Vec<u8>, u64)>,
    /// `(scriptPubKey, value)`.
    pub outputs: Vec<(Vec<u8>, u64)>,
    /// The outpoint of each input. Empty: input `i` spends output `i` of a transaction
    /// whose id comes from `id`.
    pub spends: Vec<OutPoint>,
    /// Makes the default outpoints, and so the transaction id, distinct.
    pub id: u32,
    pub lock_time: u32,
    pub expiry: u32,
    pub sequence: u32,
}

impl TxSpec {
    /// One P2PKH input of 100,000 zatoshis and one P2PKH output of 50,000 zatoshis: the
    /// fee of 50,000 zatoshis is above the conventional fee of 800 zatoshis.
    pub fn standard() -> Self {
        Self {
            inputs: vec![(P2PKH_SIG.to_vec(), p2pkh(1), 100_000)],
            outputs: vec![(p2pkh(9), 50_000)],
            spends: Vec::new(),
            id: 0,
            lock_time: 0,
            expiry: 0,
            sequence: u32::MAX,
        }
    }

    /// [`TxSpec::standard`] with distinct outpoints for each `id` and the given fee.
    pub fn paying(id: u32, fee: u64) -> Self {
        Self {
            inputs: vec![(P2PKH_SIG.to_vec(), p2pkh(1), 50_000 + fee)],
            id,
            ..Self::standard()
        }
    }
}

fn script(bytes: &[u8]) -> Script {
    Script(zcash_script04::script::Code(bytes.to_vec()))
}

/// The prepared form of `spec`, marked as verified, with the fee of its coins.
pub fn prepared(spec: &TxSpec) -> PreparedTx {
    let mut hash = [0xa5u8; 32];
    hash[..4].copy_from_slice(&spec.id.to_le_bytes());
    let vin = spec
        .inputs
        .iter()
        .enumerate()
        .map(|(i, (script_sig, _, _))| {
            let prevout = match spec.spends.get(i) {
                Some(outpoint) => outpoint.clone(),
                None => OutPoint::new(hash, i as u32),
            };
            TxIn::from_parts(prevout, script(script_sig), spec.sequence)
        })
        .collect();
    let vout = spec
        .outputs
        .iter()
        .map(|(script_pubkey, value)| {
            TxOut::new(Zatoshis::const_from_u64(*value), script(script_pubkey))
        })
        .collect();
    let tx = TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        BRANCH,
        spec.lock_time,
        BlockHeight::from_u32(spec.expiry),
        Some(Bundle {
            vin,
            vout,
            authorization: TAuthorized,
        }),
        None,
        None,
        None,
    )
    .freeze()
    .expect("v5 freezes");
    let mut bytes = Vec::new();
    tx.write(&mut bytes).expect("vec write");
    let raw = RawTx::parse(Bytes::from(bytes), BRANCH).expect("round trip");
    let spent: Vec<Coin> = spec
        .inputs
        .iter()
        .map(|(_, script_pubkey, value)| Coin {
            value: *value,
            script_pubkey: Bytes::copy_from_slice(script_pubkey),
            height: 1,
            is_coinbase: false,
        })
        .collect();
    let paid_in: u64 = spent.iter().map(|coin| coin.value).sum();
    let paid_out: u64 = spec.outputs.iter().map(|(_, value)| value).sum();
    PreparedTx {
        raw: Arc::new(raw),
        epoch: RuleEpoch::consensus(BRANCH),
        spent,
        fee: paid_in - paid_out,
        sigops: 0,
        nullifiers: Vec::new(),
        commitments: Commitments::default(),
        anchors: Vec::new(),
        orchard_actions: 0,
        ironwood_actions: 0,
        sapling_ios: 0,
        joinsplits: 0,
        scripts_ok: true,
        shielded_ok: true,
        is_coinbase: false,
        expiry_height: spec.expiry,
        lock_time: spec.lock_time,
    }
}
