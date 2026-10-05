//! Transactions and blocks of the pair harness.
//!
//! Each coin of the harness has the pay-to-script-hash script of the redeem script
//! `OP_TRUE`. Its scriptSig is the push of that redeem script, so no transaction needs a
//! key. A shielded transaction has one Orchard or Ironwood output with a real proof.

use std::sync::OnceLock;

use bytes::Bytes;
use hayai_crypto::rng::os_rng;
use hayai_crypto::{
    orchard, sapling_crypto, zcash_primitives, zcash_protocol, zcash_script04, zcash_transparent,
};
use hayai_wire::header::{BlockHash, BlockHeader};
use hayai_wire::RawTx;
use orchard::builder::{Builder, BundleType, InProgress, Unauthorized, Unproven};
use orchard::bundle::BundleVersion;
use orchard::circuit::ProvingKey;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::value::NoteValue;
use orchard::Anchor;
use serde_json::Value;
use zcash_primitives::transaction::sighash::{signature_hash, SignableInput};
use zcash_primitives::transaction::txid::TxIdDigester;
use zcash_primitives::transaction::{Authorization, Authorized, TransactionData, TxVersion};
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::value::{ZatBalance, Zatoshis};
use zcash_transparent::address::Script;
use zcash_transparent::bundle::{Authorized as TAuthorized, Bundle, OutPoint, TxIn, TxOut};
use zcash_transparent::sighash::TransparentAuthorizingContext;

use crate::nodes::{nu7_height, ACTIVATIONS};

/// The script of each coin of the harness: `OP_HASH160 <hash of OP_TRUE> OP_EQUAL`.
pub const COIN_SCRIPT: [u8; 23] = [
    0xa9, 0x14, 0xda, 0x17, 0x45, 0xe9, 0xb5, 0x49, 0xbd, 0x0b, 0xfa, 0x1a, 0x56, 0x99, 0x71, 0xc7,
    0x7e, 0xba, 0x30, 0xcd, 0x5a, 0x4b, 0x87,
];
/// The scriptSig of such a coin: the push of the redeem script `OP_TRUE`.
pub const COIN_SCRIPT_SIG: [u8; 2] = [0x01, 0x51];

/// The consensus branch of the block at `height` on the network of the pair.
pub fn branch_at(height: u32) -> BranchId {
    let [nu6, nu6_1, nu6_2, nu6_3] = ACTIVATIONS;
    if let Some(nu7) = nu7_height() {
        if height >= nu7 {
            return hayai_crypto::nu7_branch()
                .expect("the pair has an NU7 height only with a crypto backend that has NU7");
        }
    }
    match height {
        0 => BranchId::Sprout,
        h if h < nu6 => BranchId::Nu5,
        h if h < nu6_1 => BranchId::Nu6,
        h if h < nu6_2 => BranchId::Nu6_1,
        h if h < nu6_3 => BranchId::Nu6_2,
        _ => BranchId::Nu6_3,
    }
}

/// An unspent output with the coin script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coin {
    /// The transaction id in the byte order of the wire.
    pub txid: [u8; 32],
    pub index: u32,
    pub value: u64,
}

fn compact_size(out: &mut Vec<u8>, n: usize) {
    match n {
        0..=252 => out.push(n as u8),
        253..=0xffff => {
            out.push(253);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        _ => {
            out.push(254);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
    }
}

/// A v5 transaction with transparent parts only. Each input spends a coin with
/// `script_sig`. Each output has the coin script.
pub fn transparent_tx(
    branch: BranchId,
    coins: &[Coin],
    outputs: &[u64],
    expiry_height: u32,
    script_sig: &[u8],
) -> Bytes {
    let mut tx = Vec::new();
    tx.extend_from_slice(&0x8000_0005u32.to_le_bytes());
    tx.extend_from_slice(&0x26A7_270Au32.to_le_bytes());
    tx.extend_from_slice(&u32::from(branch).to_le_bytes());
    tx.extend_from_slice(&0u32.to_le_bytes());
    tx.extend_from_slice(&expiry_height.to_le_bytes());
    compact_size(&mut tx, coins.len());
    for coin in coins {
        tx.extend_from_slice(&coin.txid);
        tx.extend_from_slice(&coin.index.to_le_bytes());
        compact_size(&mut tx, script_sig.len());
        tx.extend_from_slice(script_sig);
        tx.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    compact_size(&mut tx, outputs.len());
    for value in outputs {
        tx.extend_from_slice(&value.to_le_bytes());
        compact_size(&mut tx, COIN_SCRIPT.len());
        tx.extend_from_slice(&COIN_SCRIPT);
    }
    // No Sapling spend, no Sapling output, no Orchard action.
    tx.extend_from_slice(&[0, 0, 0]);
    Bytes::from(tx)
}

/// The transparent part of [`Unsigned`]: the spent coins, to which the sighash commits.
#[derive(Debug)]
struct SpentCoins(Vec<TxOut>);

impl zcash_transparent::bundle::Authorization for SpentCoins {
    type ScriptSig = Script;
}

impl TransparentAuthorizingContext for SpentCoins {
    fn input_amounts(&self) -> Vec<Zatoshis> {
        self.0.iter().map(TxOut::value).collect()
    }

    fn input_scriptpubkeys(&self) -> Vec<Script> {
        self.0.iter().map(|o| o.script_pubkey().clone()).collect()
    }
}

struct Unsigned;

impl Authorization for Unsigned {
    type TransparentAuth = SpentCoins;
    type SaplingAuth = sapling_crypto::bundle::Authorized;
    type OrchardAuth = InProgress<Unproven, Unauthorized>;
}

fn script(bytes: &[u8]) -> Script {
    Script(zcash_script04::script::Code(bytes.to_vec()))
}

/// The shielded pool of a shielding transaction in the epoch `branch`: Orchard before
/// NU6.3, Ironwood from NU6.3.
pub fn shielded_pool(branch: BranchId) -> &'static str {
    match branch {
        BranchId::Nu5 | BranchId::Nu6 | BranchId::Nu6_1 | BranchId::Nu6_2 => "orchard",
        _ => "ironwood",
    }
}

fn transaction_data<A: Authorization>(
    branch: BranchId,
    coin: &Coin,
    change: u64,
    expiry_height: u32,
    authorization: A::TransparentAuth,
    shielded: orchard::Bundle<A::OrchardAuth, ZatBalance>,
) -> TransactionData<A>
where
    A::TransparentAuth: zcash_transparent::bundle::Authorization<ScriptSig = Script>,
{
    let transparent = Some(Bundle {
        vin: vec![TxIn::from_parts(
            OutPoint::new(coin.txid, coin.index),
            script(&COIN_SCRIPT_SIG),
            u32::MAX,
        )],
        vout: vec![TxOut::new(
            Zatoshis::from_u64(change).expect("a valid amount"),
            script(&COIN_SCRIPT),
        )],
        authorization,
    });
    let expiry = BlockHeight::from_u32(expiry_height);
    match branch {
        BranchId::Nu5 | BranchId::Nu6 | BranchId::Nu6_1 | BranchId::Nu6_2 => {
            TransactionData::from_parts(
                TxVersion::V5,
                branch,
                0,
                expiry,
                transparent,
                None,
                None,
                Some(shielded),
            )
        }
        _ => TransactionData::from_parts_v6(
            branch,
            0,
            expiry,
            transparent,
            None,
            None,
            Some(shielded),
        ),
    }
}

/// A transaction of the epoch `branch` that spends `coin` into one shielded output of
/// `shielded` zatoshis and one transparent output of `change` zatoshis (output 0). The pool
/// is [`shielded_pool`]. The fee is the rest of the value of the coin.
pub fn shielding_tx(
    branch: BranchId,
    coin: &Coin,
    shielded: u64,
    change: u64,
    expiry_height: u32,
) -> Bytes {
    static KEYS: [OnceLock<ProvingKey>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let (version, key) = match branch {
        BranchId::Nu5 | BranchId::Nu6 | BranchId::Nu6_1 => {
            (BundleVersion::orchard_insecure_v1(), &KEYS[0])
        }
        BranchId::Nu6_2 => (BundleVersion::orchard_v2(), &KEYS[1]),
        // NU6.3 and the upgrades after it.
        _ => (BundleVersion::ironwood_v3(), &KEYS[2]),
    };
    let recipient = {
        let sk = SpendingKey::from_bytes([7; 32]).expect("a valid spending key");
        FullViewingKey::from(&sk).address_at(0u32, Scope::External)
    };
    let mut builder = Builder::new(
        BundleType::DEFAULT,
        version,
        version.default_flags(),
        Anchor::empty_tree(),
    )
    .expect("default flags are representable");
    builder
        .add_output(None, recipient, NoteValue::from_raw(shielded), [0; 512])
        .expect("outputs enabled");
    let mut rng = os_rng();
    let (bundle, _meta) = builder
        .build::<ZatBalance>(&mut rng)
        .expect("bundle builds")
        .expect("bundle has an output");

    let spent = TxOut::new(
        Zatoshis::from_u64(coin.value).expect("a valid amount"),
        script(&COIN_SCRIPT),
    );
    let unsigned = transaction_data::<Unsigned>(
        branch,
        coin,
        change,
        expiry_height,
        SpentCoins(vec![spent]),
        bundle.clone(),
    );
    let txid_parts = unsigned.digest(TxIdDigester);
    let sighash = *signature_hash(&unsigned, &SignableInput::Shielded, &txid_parts).as_ref();
    let key = key.get_or_init(|| ProvingKey::build(version.circuit_version()));
    let bundle = bundle
        .create_proof(key, &mut rng)
        .expect("proof")
        .apply_signatures(rng, sighash, &[])
        .expect("only dummy spends to sign");
    let tx =
        transaction_data::<Authorized>(branch, coin, change, expiry_height, TAuthorized, bundle)
            .freeze()
            .expect("the bundle matches the transaction version");
    let mut bytes = Vec::new();
    tx.write(&mut bytes).expect("vec write");
    Bytes::from(bytes)
}

/// The transaction id of `tx` in the byte order of the wire.
pub fn txid(tx: &Bytes, branch: BranchId) -> Result<[u8; 32], String> {
    let parsed = RawTx::parse(tx.clone(), branch).map_err(|e| format!("transaction: {e}"))?;
    Ok(*parsed.txid.as_ref())
}

/// Display hex (bytes reversed) of a 32-byte value.
pub fn display(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

fn from_display(text: &str) -> Result<[u8; 32], String> {
    let mut bytes: [u8; 32] = hex::decode(text)
        .map_err(|e| format!("{text}: {e}"))?
        .try_into()
        .map_err(|_| format!("{text} is not 32 bytes"))?;
    bytes.reverse();
    Ok(bytes)
}

/// The miner output of the coinbase of a block, as a coin of the harness: the first output
/// that pays the coin script. A coinbase can have funding stream outputs before it.
pub fn coinbase_coin(block_hex: &str, height: u32) -> Result<Coin, String> {
    let bytes = Bytes::from(hex::decode(block_hex).map_err(|e| e.to_string())?);
    let block = hayai_wire::RawBlock::parse(bytes, branch_at(height))
        .map_err(|e| format!("block {height}: {e}"))?;
    let coinbase = &block.txs[0];
    let outputs = coinbase
        .tx
        .transparent_bundle()
        .map_or(&[][..], |bundle| bundle.vout.as_slice());
    let Some(index) = outputs
        .iter()
        .position(|output| output.script_pubkey().0 .0 == COIN_SCRIPT)
    else {
        return Err(format!(
            "the coinbase of block {height} does not pay the coin script"
        ));
    };
    Ok(Coin {
        txid: *coinbase.txid.as_ref(),
        index: index as u32,
        value: u64::from(outputs[index].value()),
    })
}

/// The parts of a block that the harness builds on a `getblocktemplate` answer.
#[derive(Clone)]
pub struct Draft {
    pub height: u32,
    pub version: u32,
    pub prev: BlockHash,
    pub time: u32,
    pub bits: u32,
    pub nonce: [u8; 32],
    /// `hashChainHistoryRoot` of the template, in the byte order of the wire.
    pub history_root: [u8; 32],
    pub coinbase: Bytes,
    pub txs: Vec<Bytes>,
    /// Replaces the merkle root or the header commitment of the body.
    pub merkle_root: Option<[u8; 32]>,
    pub commitments: Option<[u8; 32]>,
}

impl Draft {
    /// The block of a template with its coinbase and without its other transactions.
    pub fn from_template(template: &Value) -> Result<Self, String> {
        let text = |v: &Value, name: &str| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("the template has no {name}"))
        };
        let height = template["height"]
            .as_u64()
            .ok_or("the template has no height")? as u32;
        let bits = u32::from_str_radix(&text(&template["bits"], "bits")?, 16)
            .map_err(|e| e.to_string())?;
        let coinbase = hex::decode(text(&template["coinbasetxn"]["data"], "coinbase")?)
            .map_err(|e| e.to_string())?;
        let mut nonce = [0; 32];
        nonce[..4].copy_from_slice(&height.to_le_bytes());
        Ok(Self {
            height,
            version: template["version"].as_u64().ok_or("no version")? as u32,
            prev: BlockHash(from_display(&text(
                &template["previousblockhash"],
                "previousblockhash",
            )?)?),
            time: template["curtime"].as_u64().ok_or("no curtime")? as u32,
            bits,
            nonce,
            history_root: from_display(&text(
                &template["defaultroots"]["chainhistoryroot"],
                "chainhistoryroot",
            )?)?,
            coinbase: Bytes::from(coinbase),
            txs: Vec::new(),
            merkle_root: None,
            commitments: None,
        })
    }

    /// The wire bytes of the block and its hash. The header commits to the body, except
    /// for a root that the draft replaces.
    pub fn build(&self) -> Result<(String, BlockHash), String> {
        let branch = branch_at(self.height);
        let mut txids = Vec::new();
        let mut digests = Vec::new();
        for tx in std::iter::once(&self.coinbase).chain(&self.txs) {
            let parsed = RawTx::parse(tx.clone(), branch).map_err(|e| format!("draft: {e}"))?;
            txids.push(parsed.txid);
            digests.push(parsed.auth_digest);
        }
        let auth_root = hayai_wire::auth_data_root(&digests);
        let header = BlockHeader {
            version: self.version,
            prev_hash: self.prev,
            merkle_root: self
                .merkle_root
                .unwrap_or_else(|| hayai_wire::merkle_root(&txids)),
            block_commitments: self
                .commitments
                .unwrap_or_else(|| hayai_wire::block_commitments(&self.history_root, &auth_root)),
            time: self.time,
            bits: self.bits,
            nonce: self.nonce,
            solution: vec![0; 36],
        };
        let mut bytes = header.serialize();
        compact_size(&mut bytes, 1 + self.txs.len());
        bytes.extend_from_slice(&self.coinbase);
        for tx in &self.txs {
            bytes.extend_from_slice(tx);
        }
        Ok((hex::encode(bytes), header.hash()))
    }

    /// Adds `delta` zatoshis to the first output of the v5 coinbase: the miner output.
    pub fn change_coinbase_value(&mut self, delta: i64) -> Result<(), String> {
        self.change_coinbase_output(0, delta)
    }

    /// Adds `delta` zatoshis to output `index` of the v5 coinbase.
    pub fn change_coinbase_output(&mut self, index: usize, delta: i64) -> Result<(), String> {
        let mut bytes = self.coinbase.to_vec();
        if bytes[..4] != 0x8000_0005u32.to_le_bytes() || bytes[20] != 1 {
            return Err("the coinbase is not a v5 transaction with one input".into());
        }
        // Header fields (20 bytes), input count, outpoint, scriptSig, sequence, output count.
        let script_len = usize::from(bytes[57]);
        let count = usize::from(bytes[58 + script_len + 4]);
        if index >= count {
            return Err(format!(
                "the coinbase has {count} outputs, no output {index}"
            ));
        }
        let mut offset = 58 + script_len + 4 + 1;
        for _ in 0..index {
            // Value (8 bytes), script length (below 253), script.
            offset += 8 + 1 + usize::from(bytes[offset + 8]);
        }
        let value = u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes"));
        let changed = value
            .checked_add_signed(delta)
            .ok_or("the value leaves its range")?;
        bytes[offset..offset + 8].copy_from_slice(&changed.to_le_bytes());
        self.coinbase = Bytes::from(bytes);
        Ok(())
    }
}
