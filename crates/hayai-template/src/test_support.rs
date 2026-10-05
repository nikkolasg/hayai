//! Deterministic fixtures for the tests of the crate: v5 transparent transactions built with
//! the upstream types, candidates derived from them, and a coinbase spec for Mainnet.

use std::sync::Arc;

use bytes::Bytes;
use hayai_consensus::Network;
use hayai_crypto::{
    zcash_primitives, zcash_protocol, zcash_script04 as zcash_script, zcash_transparent,
};
use hayai_wire::header::BlockHash;
use hayai_wire::{RawTx, WtxId};
use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::Zatoshis;
use zcash_script::script::Code;
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Bundle, OutPoint, TxIn, TxOut};

use crate::candidate::Candidate;
use crate::coinbase::CoinbaseSpec;
use crate::live::Tip;
use crate::zip317::Zip317Params;

pub const BRANCH: BranchId = BranchId::Nu6_1;

/// A v5 transaction with `n_in` inputs (scriptSig of `seed % 70 + 1` bytes) and `n_out`
/// P2PKH-sized outputs. Different seeds give different txids.
pub fn transparent_tx(seed: u64, n_in: usize, n_out: usize) -> Transaction {
    let outpoints = (0..n_in)
        .map(|i| {
            let mut txid = [0u8; 32];
            txid[..8].copy_from_slice(&seed.to_le_bytes());
            txid[8..16].copy_from_slice(&(i as u64).to_le_bytes());
            OutPoint::new(txid, i as u32)
        })
        .collect();
    spending_tx(seed, outpoints, n_out)
}

/// A v5 transaction that spends `outpoints` (scriptSig of `seed % 70 + 1` bytes each) and has
/// `n_out` P2PKH-sized outputs.
pub fn spending_tx(seed: u64, outpoints: Vec<OutPoint>, n_out: usize) -> Transaction {
    let script_len = (seed % 70 + 1) as usize;
    let vin = outpoints
        .into_iter()
        .map(|outpoint| TxIn::from_parts(outpoint, Script(Code(vec![0x51; script_len])), u32::MAX))
        .collect();
    let vout = (0..n_out)
        .map(|i| {
            let mut script = vec![0x76, 0xa9, 0x14];
            script.extend_from_slice(&[i as u8; 20]);
            script.extend_from_slice(&[0x88, 0xac]);
            TxOut::new(
                Zatoshis::from_u64(1000 + seed % 1000).unwrap(),
                Script(Code(script)),
            )
        })
        .collect();
    let bundle = Bundle {
        vin,
        vout,
        authorization: zcash_transparent::bundle::Authorized,
    };
    TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        BRANCH,
        0,
        BlockHeight::from_u32(0),
        Some(bundle),
        None,
        None,
        None,
    )
    .freeze()
    .unwrap()
}

pub fn raw_tx(seed: u64, n_in: usize, n_out: usize) -> RawTx {
    raw_of(transparent_tx(seed, n_in, n_out))
}

fn raw_of(tx: Transaction) -> RawTx {
    let mut bytes = Vec::new();
    tx.write(&mut bytes).unwrap();
    RawTx {
        bytes: Bytes::from(bytes),
        txid: tx.txid(),
        auth_digest: tx.auth_commitment().as_bytes().try_into().unwrap(),
        tx: Arc::new(tx),
    }
}

/// An `n_out`-output candidate that pays `fee` and depends on `parents`: it spends output 0
/// of each parent, or one outside outpoint when it has no parent.
pub fn candidate(seed: u64, fee: u64, n_out: usize, parents: &[WtxId]) -> Candidate {
    let raw = if parents.is_empty() {
        raw_tx(seed, 1, n_out)
    } else {
        let outpoints = parents
            .iter()
            .map(|p| OutPoint::new(*p.txid.as_ref(), 0))
            .collect();
        raw_of(spending_tx(seed, outpoints, n_out))
    };
    Candidate::from_raw(&raw, fee, 1, parents.to_vec(), &Zip317Params::ZIP317)
}

/// A coinbase with the Mainnet terms of its height: the miner output, and the founders'
/// reward or the funding stream outputs of the height.
pub fn coinbase_spec() -> CoinbaseSpec {
    CoinbaseSpec {
        script_pubkey: vec![0x76, 0xa9, 0x14, 0xab, 0xcd, 0xef, 0x88, 0xac],
        miner_data: b"hayai".to_vec(),
        network: Network::Mainnet,
    }
}

pub fn tip(height: u32) -> Tip {
    Tip {
        parent_hash: BlockHash([height as u8; 32]),
        height,
        time: 1_700_000_000 + height,
        median_time_past: 1_700_000_000 + height - 1,
        bits: 0x1d00_ffff,
        history_root: [0x11; 32],
        issued_supply: None,
    }
}
