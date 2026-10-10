//! The P2SH addresses of a spec and their scripts. The core reads scripts only
//! ([`P2shScript`]): this module decodes the Base58Check addresses of a spec one time, at
//! compile time for the built-in networks and in [`crate::ChainSpec::network`] for a
//! custom chain, and encodes a script back to an address for the RPC methods.

use hayai_consensus_core::P2shScript;
use hayai_crypto::zcash_address::{ToAddress, ZcashAddress};
use hayai_crypto::zcash_protocol::consensus::NetworkType;
use hayai_crypto::zcash_transparent::address::TransparentAddress;

const OP_HASH160: u8 = 0xa9;
const OP_EQUAL: u8 = 0x87;

/// The `scriptPubKey` that pays the 20-byte script hash `hash` in the prescribed way
/// (spec §7.10): `OP_HASH160 <script hash> OP_EQUAL`.
const fn p2sh_of_hash(hash: [u8; 20]) -> P2shScript {
    let mut script = [0u8; 23];
    script[0] = OP_HASH160;
    script[1] = 20;
    let mut i = 0;
    while i < 20 {
        script[2 + i] = hash[i];
        i += 1;
    }
    script[22] = OP_EQUAL;
    script
}

/// The `scriptPubKey` that pays the Base58Check P2SH `address` in the prescribed way:
/// `OP_HASH160 <script hash> OP_EQUAL`.
///
/// Spec §7.10: the prescribed way to pay a P2SH address. No funding stream and no
/// disbursement has a Sapling or Orchard recipient, and this crate pays none. The address has
/// the encoding of `network_type` ([`crate::ChainSpec::network_type`]). Regtest takes an
/// address of any network, as Zakura does for the addresses of its Regtest parameters: the
/// script has the hash only.
pub(crate) fn p2sh_script(network_type: NetworkType, address: &str) -> Result<P2shScript, String> {
    let decoded = ZcashAddress::try_from_encoded(address).map_err(|error| error.to_string())?;
    let decoded = match network_type {
        NetworkType::Main => decoded.convert_if_network::<TransparentAddress>(NetworkType::Main),
        NetworkType::Test => decoded.convert_if_network::<TransparentAddress>(NetworkType::Test),
        NetworkType::Regtest => decoded.convert::<TransparentAddress>(),
    };
    let hash = match decoded {
        Ok(TransparentAddress::ScriptHash(hash)) => hash,
        Ok(other) => return Err(format!("{other:?} is not a script hash")),
        Err(error) => {
            let name = match network_type {
                NetworkType::Main => "mainnet",
                NetworkType::Test => "testnet",
                NetworkType::Regtest => "regtest",
            };
            return Err(format!("not an address of {name}: {error:?}"));
        }
    };
    Ok(p2sh_of_hash(hash))
}

/// [`p2sh_script`] for an address of the spec of `network`: a founders' address, a lockbox
/// disbursement address or a funding stream address.
///
/// # Panics
///
/// When `address` is not a P2SH address of the network type of `network`. This does not
/// occur: [`crate::ChainSpec::network`] decodes each address of a spec, and a test decodes
/// each address of the built-in specs.
#[cfg(test)]
pub(crate) fn address_script(network: crate::Network, address: &str) -> P2shScript {
    match p2sh_script(network.network_type(), address) {
        Ok(script) => script,
        Err(reason) => panic!("{address} is not a P2SH address: {reason}"),
    }
}

/// The Base58Check P2SH address of `network` with the hash of `script`, for the RPC
/// methods that name a recipient. A Regtest spec can hold addresses of another network;
/// the result then has the Regtest encoding of the same hash.
pub fn address_of(network: crate::Network, script: &P2shScript) -> String {
    let mut hash = [0u8; 20];
    hash.copy_from_slice(&script[2..22]);
    ZcashAddress::from_transparent_p2sh(network.network_type(), hash).encode()
}

/// The Base58 alphabet of Bitcoin.
const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// The value of the Base58 digit `digit`.
const fn digit_value(digit: u8) -> u8 {
    let mut i = 0;
    while i < 58 {
        if ALPHABET[i] == digit {
            return i as u8;
        }
        i += 1;
    }
    panic!("an address of a built-in network is Base58")
}

/// The P2SH script of a Base58Check transparent address at compile time: the 26 decoded
/// bytes are the 2-byte prefix, the 20-byte script hash and the 4-byte checksum. The
/// prefix and the checksum are not checked here: the test `the_const_scripts_are_the_decoded_addresses`
/// compares each script of the built-in specs with [`p2sh_script`].
pub(crate) const fn script_of(address: &str) -> P2shScript {
    let digits = address.as_bytes();
    // The number in base 256, big-endian.
    let mut bytes = [0u8; 26];
    let mut i = 0;
    while i < digits.len() {
        let mut carry = digit_value(digits[i]) as u32;
        let mut j = 26;
        while j > 0 {
            j -= 1;
            let value = bytes[j] as u32 * 58 + carry;
            bytes[j] = (value & 0xff) as u8;
            carry = value >> 8;
        }
        assert!(carry == 0, "an address of a built-in network has 26 bytes");
        i += 1;
    }
    let mut hash = [0u8; 20];
    let mut i = 0;
    while i < 20 {
        hash[i] = bytes[2 + i];
        i += 1;
    }
    p2sh_of_hash(hash)
}

/// [`script_of`] for each address of a list.
pub(crate) const fn scripts_of<const N: usize>(addresses: &[&str; N]) -> [P2shScript; N] {
    let mut scripts = [[0u8; 23]; N];
    let mut i = 0;
    while i < N {
        scripts[i] = script_of(addresses[i]);
        i += 1;
    }
    scripts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Network;

    #[test]
    fn address_script_is_the_p2sh_script_of_the_address() {
        // Mainnet block 1 pays the founders' reward to this script (Zebra vector
        // `block-main-0-000-001`).
        let script = address_script(Network::Mainnet, "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd");
        let hex: String = script.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, "a9147d46a730d31f97b1930d3368a967c309bd4d136a87");
        assert_eq!(script_of("t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd"), script);
        assert_eq!(
            address_of(Network::Mainnet, &script),
            "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd"
        );
        // A Testnet address re-encodes on Testnet and on Regtest with the Regtest prefix,
        // which is the Testnet prefix.
        let testnet = address_script(Network::Testnet, "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi");
        assert_eq!(
            address_of(Network::Testnet, &testnet),
            "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi"
        );
        assert_eq!(
            address_of(Network::Regtest, &testnet),
            "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi"
        );
    }

    #[test]
    #[should_panic(expected = "not an address of testnet")]
    fn an_address_of_another_network_is_refused() {
        address_script(Network::Testnet, "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd");
    }

    /// Each script of a built-in spec, decoded by the compiler, is the script of the
    /// address that the upstream decoder gives.
    #[test]
    fn the_const_scripts_are_the_decoded_addresses() {
        for network in Network::ALL {
            let spec = network.spec();
            let core = network.core();
            assert_eq!(spec.funding_streams.len(), core.funding_streams.len());
            for (set, core_set) in spec.funding_streams.iter().zip(&core.funding_streams) {
                assert_eq!((set.start, set.end), (core_set.start, core_set.end));
                assert_eq!(set.ends_at_third_halving, core_set.ends_at_third_halving);
                assert_eq!(set.streams.len(), core_set.streams.len());
                for (stream, core_stream) in set.streams.iter().zip(&core_set.streams) {
                    assert_eq!(stream.receiver, core_stream.receiver);
                    assert_eq!(stream.numerator, core_stream.numerator);
                    let scripts: Vec<P2shScript> = stream
                        .addresses
                        .iter()
                        .map(|address| address_script(network, address))
                        .collect();
                    assert_eq!(scripts, core_stream.scripts, "{network:?} {}", set.start);
                }
            }
            assert_eq!(
                spec.lockbox_disbursements.len(),
                core.lockbox_disbursements.len()
            );
            for (disbursement, core_disbursement) in spec
                .lockbox_disbursements
                .iter()
                .zip(&core.lockbox_disbursements)
            {
                assert_eq!(disbursement.count, core_disbursement.count);
                assert_eq!(disbursement.value, core_disbursement.value);
                assert_eq!(
                    address_script(network, disbursement.address),
                    core_disbursement.script
                );
            }
            let founders: Vec<P2shScript> = spec
                .founders_addresses
                .iter()
                .map(|address| address_script(network, address))
                .collect();
            assert_eq!(founders, core.founders_scripts, "{network:?}");
        }
    }
}
