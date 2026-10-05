//! The coinbase script and the Regtest block producer.
//!
//! Zakura's Regtest waives proof of work (`disable_pow`): it checks the shape of the
//! solution (36 bytes, Equihash (48, 5)) and the compact target, and runs neither the
//! hash-to-target filter nor Equihash. The producer therefore takes the template, sets a
//! counter as the nonce and an all-zero solution of the network's length (Zakura's internal
//! miner sends the same "null solution"), and hands the block to the relay as a found
//! block. No solver is involved.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hayai_crypto::zcash_address::{ConversionError, TryFromAddress, ZcashAddress};
use hayai_crypto::zcash_protocol::consensus::NetworkType;
use hayai_net::Relay;
use hayai_rpc::{BlockGenerator, TemplateFeed};
use hayai_template::messages::{Hash32, HexBytes, Submit};
use hayai_template::submission::rebuild_block;
use hayai_wire::header::BlockHash;
use hayai_wire::RawBlock;

use crate::config::MiningSection;
use crate::node::TipWatch;
use crate::params::{NetParams, NetworkKind};

/// How long `generate` waits for a template on the new tip and for the commit of a block.
const WAIT: Duration = Duration::from_secs(30);

struct TransparentScript(Vec<u8>);

impl TryFromAddress for TransparentScript {
    type Error = String;

    fn try_from_transparent_p2pkh(
        net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        let _ = net;
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend_from_slice(&data);
        script.extend_from_slice(&[0x88, 0xac]);
        Ok(Self(script))
    }

    fn try_from_transparent_p2sh(
        net: NetworkType,
        data: [u8; 20],
    ) -> Result<Self, ConversionError<Self::Error>> {
        let _ = net;
        let mut script = vec![0xa9, 0x14];
        script.extend_from_slice(&data);
        script.push(0x87);
        Ok(Self(script))
    }
}

/// The scriptPubKey of the miner output from `[mining]`. An address must be a transparent
/// address of the network (Regtest uses the Testnet encoding).
pub fn miner_script(mining: &MiningSection, network: NetworkKind) -> Result<Vec<u8>, String> {
    let address_network = match network {
        NetworkKind::Mainnet => NetworkType::Main,
        NetworkKind::Testnet | NetworkKind::Regtest | NetworkKind::ConfiguredRegtest(_) => {
            NetworkType::Test
        }
    };
    match (&mining.miner_address, &mining.miner_script) {
        (Some(address), None) => {
            let parsed = ZcashAddress::try_from_encoded(address)
                .map_err(|e| format!("miner_address {address}: {e}"))?;
            let script = parsed
                .convert_if_network::<TransparentScript>(address_network)
                .map_err(|e: ConversionError<String>| format!("miner_address {address}: {e}"))?;
            Ok(script.0)
        }
        (None, Some(script)) => {
            hex::decode(script).map_err(|e| format!("miner_script {script}: {e}"))
        }
        _ => Err("set exactly one of miner_address and miner_script".into()),
    }
}

/// `generate n` for Regtest.
pub struct Producer {
    pub params: NetParams,
    pub feed: Arc<TemplateFeed>,
    pub relay: Arc<Relay>,
    pub tip: Arc<TipWatch>,
    nonce: AtomicU64,
}

impl Producer {
    pub fn new(
        params: NetParams,
        feed: Arc<TemplateFeed>,
        relay: Arc<Relay>,
        tip: Arc<TipWatch>,
    ) -> Self {
        Self {
            params,
            feed,
            relay,
            tip,
            nonce: AtomicU64::new(0),
        }
    }

    /// One block on the current tip: waits for a template on it, completes the header,
    /// hands the block to the relay and waits for its commit.
    fn produce_one(&self) -> Result<BlockHash, String> {
        let deadline = Instant::now() + WAIT;
        let (height, parent) = self.tip.tip();
        let template = loop {
            match self.feed.current() {
                Some(t) if t.tip.parent_hash == parent => break t,
                _ if Instant::now() >= deadline => {
                    return Err(format!("no template on tip {parent} after {WAIT:?}"));
                }
                _ => thread::sleep(Duration::from_millis(2)),
            }
        };
        let mut nonce = [0u8; 32];
        nonce[..8].copy_from_slice(&self.nonce.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let submit = Submit {
            template_id: template.id,
            time: template.tip.time,
            nonce: Hash32(nonce),
            solution: HexBytes(Bytes::from(vec![0u8; self.params.pow().solution_len()])),
            coinbase: None,
        };
        let branch = self
            .params
            .branch_at(height + 1)
            .map_err(|e| e.to_string())?;
        let rebuilt = self
            .feed
            .with_store(|store| rebuild_block(store, &submit))
            .map_err(|e| format!("template {}: {e}", template.id))?;
        let block = RawBlock::parse(rebuilt.bytes, branch)
            .map_err(|e| format!("produced block does not parse: {e}"))?;
        let hash = block.hash();
        self.relay.block_found(block);
        self.tip.wait_for(&hash, WAIT)?;
        Ok(hash)
    }
}

impl BlockGenerator for Producer {
    fn generate(&self, n: u32) -> Result<Vec<BlockHash>, String> {
        (0..n).map(|_| self.produce_one()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(address: Option<&str>, script: Option<&str>) -> MiningSection {
        MiningSection {
            miner_address: address.map(str::to_string),
            miner_script: script.map(str::to_string),
            ..MiningSection::default()
        }
    }

    #[test]
    fn miner_script_from_an_address_or_hex() {
        // Zakura's Regtest e2e miner address.
        let script = miner_script(
            &section(Some("tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"), None),
            NetworkKind::Regtest,
        )
        .expect("a Testnet P2PKH address");
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
        assert_eq!(
            miner_script(&section(None, Some("51")), NetworkKind::Regtest),
            Ok(vec![0x51])
        );
        // A Mainnet address on a test network is an error, not a silent conversion.
        let mainnet = section(Some("t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs"), None);
        let Err(e) = miner_script(&mainnet, NetworkKind::Testnet) else {
            panic!("Mainnet address accepted");
        };
        assert!(e.contains("miner_address"));
        let script = miner_script(&mainnet, NetworkKind::Mainnet).expect("a Mainnet address");
        assert_eq!(script.len(), 25);
        // A Testnet address on Mainnet is an error as well.
        let testnet = section(Some("tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV"), None);
        let Err(_) = miner_script(&testnet, NetworkKind::Mainnet) else {
            panic!("Testnet address accepted on Mainnet");
        };
        let Err(_) = miner_script(&section(None, Some("zz")), NetworkKind::Regtest) else {
            panic!("bad hex accepted");
        };
    }
}
