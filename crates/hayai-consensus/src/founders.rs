//! The founders' reward (protocol specification §7.9, before Canopy): the address tables
//! of Mainnet and Testnet, and the wrapper of `hayai_consensus_core::founders` for a
//! caller with a [`Network`].
//!
//! No block reaches the check in a hayai node. On Mainnet and Testnet each height with a
//! founders' reward is at or below the mandatory checkpoint, and the checkpoint path does
//! not check the coinbase terms. Regtest has no founders' reward: Canopy activates at
//! height 1, and a `RegtestConfig` sets only the upgrades after NU5.
//!
//! The code stays for three users: [`crate::coinbase::terms_at`] gives the terms of every
//! height with one function, the coinbase builder of hayai-template pays the terms of a
//! height before Canopy in its tests, and the conformance tests compare the amounts and
//! the addresses with Zakura and Zebra and check the coinbase of the published Mainnet
//! block 1.

use hayai_consensus_core::founders as core;
pub use hayai_consensus_core::founders::FoundersReward;
use hayai_consensus_core::P2shScript;
use hayai_crypto::zcash_protocol::consensus::NetworkType;

use crate::address::{p2sh_script, scripts_of};
use crate::network::{check_address, checked};
use crate::{ChainSpecError, Network};

/// The founders' reward that the coinbase at `height` on `network` must pay
/// (`hayai_consensus_core::founders::founders_reward`). `None` when the rule does not
/// apply.
pub fn founders_reward(network: Network, height: u32) -> Option<FoundersReward> {
    checked(core::founders_reward(network.core(), height))
}

/// The scripts of the founders' addresses of a spec with the network type `network_type`.
/// Each address must be a P2SH address of the network type. The table stays in memory
/// until the process ends, as the spec does.
pub(crate) fn core_scripts(
    network_type: NetworkType,
    addresses: &[&str],
) -> Result<Vec<P2shScript>, ChainSpecError> {
    let mut scripts = Vec::with_capacity(addresses.len());
    for address in addresses {
        check_address(network_type, address)?;
        let Ok(script) = p2sh_script(network_type, address) else {
            unreachable!("check_address decoded the address");
        };
        scripts.push(script);
    }
    Ok(scripts)
}

/// The scripts of [`MAINNET_ADDRESSES`], for the core.
pub(crate) static MAINNET_SCRIPTS: [P2shScript; 48] = scripts_of(&MAINNET_ADDRESSES);
/// The scripts of [`TESTNET_ADDRESSES`], for the core.
pub(crate) static TESTNET_SCRIPTS: [P2shScript; 48] = scripts_of(&TESTNET_ADDRESSES);

/// Zakura `mainnet::FOUNDER_ADDRESS_LIST` (`constants/mainnet.rs:113-162`).
///
/// Spec §7.9: `FounderAddressList` of Mainnet.
pub(crate) static MAINNET_ADDRESSES: [&str; 48] = [
    "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd",
    "t3cL9AucCajm3HXDhb5jBnJK2vapVoXsop3",
    "t3fqvkzrrNaMcamkQMwAyHRjfDdM2xQvDTR",
    "t3TgZ9ZT2CTSK44AnUPi6qeNaHa2eC7pUyF",
    "t3SpkcPQPfuRYHsP5vz3Pv86PgKo5m9KVmx",
    "t3Xt4oQMRPagwbpQqkgAViQgtST4VoSWR6S",
    "t3ayBkZ4w6kKXynwoHZFUSSgXRKtogTXNgb",
    "t3adJBQuaa21u7NxbR8YMzp3km3TbSZ4MGB",
    "t3K4aLYagSSBySdrfAGGeUd5H9z5Qvz88t2",
    "t3RYnsc5nhEvKiva3ZPhfRSk7eyh1CrA6Rk",
    "t3Ut4KUq2ZSMTPNE67pBU5LqYCi2q36KpXQ",
    "t3ZnCNAvgu6CSyHm1vWtrx3aiN98dSAGpnD",
    "t3fB9cB3eSYim64BS9xfwAHQUKLgQQroBDG",
    "t3cwZfKNNj2vXMAHBQeewm6pXhKFdhk18kD",
    "t3YcoujXfspWy7rbNUsGKxFEWZqNstGpeG4",
    "t3bLvCLigc6rbNrUTS5NwkgyVrZcZumTRa4",
    "t3VvHWa7r3oy67YtU4LZKGCWa2J6eGHvShi",
    "t3eF9X6X2dSo7MCvTjfZEzwWrVzquxRLNeY",
    "t3esCNwwmcyc8i9qQfyTbYhTqmYXZ9AwK3X",
    "t3M4jN7hYE2e27yLsuQPPjuVek81WV3VbBj",
    "t3gGWxdC67CYNoBbPjNvrrWLAWxPqZLxrVY",
    "t3LTWeoxeWPbmdkUD3NWBquk4WkazhFBmvU",
    "t3P5KKX97gXYFSaSjJPiruQEX84yF5z3Tjq",
    "t3f3T3nCWsEpzmD35VK62JgQfFig74dV8C9",
    "t3Rqonuzz7afkF7156ZA4vi4iimRSEn41hj",
    "t3fJZ5jYsyxDtvNrWBeoMbvJaQCj4JJgbgX",
    "t3Pnbg7XjP7FGPBUuz75H65aczphHgkpoJW",
    "t3WeKQDxCijL5X7rwFem1MTL9ZwVJkUFhpF",
    "t3Y9FNi26J7UtAUC4moaETLbMo8KS1Be6ME",
    "t3aNRLLsL2y8xcjPheZZwFy3Pcv7CsTwBec",
    "t3gQDEavk5VzAAHK8TrQu2BWDLxEiF1unBm",
    "t3Rbykhx1TUFrgXrmBYrAJe2STxRKFL7G9r",
    "t3aaW4aTdP7a8d1VTE1Bod2yhbeggHgMajR",
    "t3YEiAa6uEjXwFL2v5ztU1fn3yKgzMQqNyo",
    "t3g1yUUwt2PbmDvMDevTCPWUcbDatL2iQGP",
    "t3dPWnep6YqGPuY1CecgbeZrY9iUwH8Yd4z",
    "t3QRZXHDPh2hwU46iQs2776kRuuWfwFp4dV",
    "t3enhACRxi1ZD7e8ePomVGKn7wp7N9fFJ3r",
    "t3PkLgT71TnF112nSwBToXsD77yNbx2gJJY",
    "t3LQtHUDoe7ZhhvddRv4vnaoNAhCr2f4oFN",
    "t3fNcdBUbycvbCtsD2n9q3LuxG7jVPvFB8L",
    "t3dKojUU2EMjs28nHV84TvkVEUDu1M1FaEx",
    "t3aKH6NiWN1ofGd8c19rZiqgYpkJ3n679ME",
    "t3MEXDF9Wsi63KwpPuQdD6by32Mw2bNTbEa",
    "t3WDhPfik343yNmPTqtkZAoQZeqA83K7Y3f",
    "t3PSn5TbMMAEw7Eu36DYctFezRzpX1hzf3M",
    "t3R3Y5vnBLrEn8L6wFjPjBLnxSUQsKnmFpv",
    "t3Pcm737EsVkGTbhsu2NekKtJeG92mvYyoN",
];

/// Zakura `testnet::FOUNDER_ADDRESS_LIST` (`constants/testnet.rs:115-164`).
///
/// Spec §7.9: `FounderAddressList` of Testnet, after the change of the addresses from
/// index 4 at height 53,127.
pub(crate) static TESTNET_ADDRESSES: [&str; 48] = [
    "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi",
    "t2N9PH9Wk9xjqYg9iin1Ua3aekJqfAtE543",
    "t2NGQjYMQhFndDHguvUw4wZdNdsssA6K7x2",
    "t2ENg7hHVqqs9JwU5cgjvSbxnT2a9USNfhy",
    "t2BkYdVCHzvTJJUTx4yZB8qeegD8QsPx8bo",
    "t2J8q1xH1EuigJ52MfExyyjYtN3VgvshKDf",
    "t2Crq9mydTm37kZokC68HzT6yez3t2FBnFj",
    "t2EaMPUiQ1kthqcP5UEkF42CAFKJqXCkXC9",
    "t2F9dtQc63JDDyrhnfpzvVYTJcr57MkqA12",
    "t2LPirmnfYSZc481GgZBa6xUGcoovfytBnC",
    "t26xfxoSw2UV9Pe5o3C8V4YybQD4SESfxtp",
    "t2D3k4fNdErd66YxtvXEdft9xuLoKD7CcVo",
    "t2DWYBkxKNivdmsMiivNJzutaQGqmoRjRnL",
    "t2C3kFF9iQRxfc4B9zgbWo4dQLLqzqjpuGQ",
    "t2MnT5tzu9HSKcppRyUNwoTp8MUueuSGNaB",
    "t2AREsWdoW1F8EQYsScsjkgqobmgrkKeUkK",
    "t2Vf4wKcJ3ZFtLj4jezUUKkwYR92BLHn5UT",
    "t2K3fdViH6R5tRuXLphKyoYXyZhyWGghDNY",
    "t2VEn3KiKyHSGyzd3nDw6ESWtaCQHwuv9WC",
    "t2F8XouqdNMq6zzEvxQXHV1TjwZRHwRg8gC",
    "t2BS7Mrbaef3fA4xrmkvDisFVXVrRBnZ6Qj",
    "t2FuSwoLCdBVPwdZuYoHrEzxAb9qy4qjbnL",
    "t2SX3U8NtrT6gz5Db1AtQCSGjrpptr8JC6h",
    "t2V51gZNSoJ5kRL74bf9YTtbZuv8Fcqx2FH",
    "t2FyTsLjjdm4jeVwir4xzj7FAkUidbr1b4R",
    "t2EYbGLekmpqHyn8UBF6kqpahrYm7D6N1Le",
    "t2NQTrStZHtJECNFT3dUBLYA9AErxPCmkka",
    "t2GSWZZJzoesYxfPTWXkFn5UaxjiYxGBU2a",
    "t2RpffkzyLRevGM3w9aWdqMX6bd8uuAK3vn",
    "t2JzjoQqnuXtTGSN7k7yk5keURBGvYofh1d",
    "t2AEefc72ieTnsXKmgK2bZNckiwvZe3oPNL",
    "t2NNs3ZGZFsNj2wvmVd8BSwSfvETgiLrD8J",
    "t2ECCQPVcxUCSSQopdNquguEPE14HsVfcUn",
    "t2JabDUkG8TaqVKYfqDJ3rqkVdHKp6hwXvG",
    "t2FGzW5Zdc8Cy98ZKmRygsVGi6oKcmYir9n",
    "t2DUD8a21FtEFn42oVLp5NGbogY13uyjy9t",
    "t2UjVSd3zheHPgAkuX8WQW2CiC9xHQ8EvWp",
    "t2TBUAhELyHUn8i6SXYsXz5Lmy7kDzA1uT5",
    "t2Tz3uCyhP6eizUWDc3bGH7XUC9GQsEyQNc",
    "t2NysJSZtLwMLWEJ6MH3BsxRh6h27mNcsSy",
    "t2KXJVVyyrjVxxSeazbY9ksGyft4qsXUNm9",
    "t2J9YYtH31cveiLZzjaE4AcuwVho6qjTNzp",
    "t2QgvW4sP9zaGpPMH1GRzy7cpydmuRfB4AZ",
    "t2NDTJP9MosKpyFPHJmfjc5pGCvAU58XGa4",
    "t29pHDBWq7qN4EjwSEHg8wEqYe9pkmVrtRP",
    "t2Ez9KM8VJLuArcxuEkNRAkhNvidKkzXcjJ",
    "t2D5y7J5fpXajLbGrMBQkFg2mFN8fo3n8cX",
    "t2UV2wr1PTaUiybpkV3FdSdGxUJeZdZztyt",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::address_script;
    use crate::address_of;

    #[test]
    fn the_reward_is_a_fifth_of_the_subsidy_until_canopy() {
        let value = |network, height| founders_reward(network, height).map(|reward| reward.value);
        assert_eq!(value(Network::Mainnet, 0), None);
        assert_eq!(value(Network::Mainnet, 1), Some(12_500));
        assert_eq!(value(Network::Mainnet, 20_000), Some(250_000_000));
        assert_eq!(value(Network::Mainnet, 653_599), Some(250_000_000));
        assert_eq!(value(Network::Mainnet, 653_600), Some(125_000_000));
        assert_eq!(value(Network::Mainnet, 1_046_399), Some(125_000_000));
        assert_eq!(value(Network::Mainnet, 1_046_400), None);

        assert_eq!(value(Network::Testnet, 1), Some(12_500));
        assert_eq!(value(Network::Testnet, 583_999), Some(250_000_000));
        assert_eq!(value(Network::Testnet, 584_000), Some(125_000_000));
        // Canopy activates on Testnet before the first halving at 1,116,000.
        assert_eq!(value(Network::Testnet, 1_028_499), Some(125_000_000));
        assert_eq!(value(Network::Testnet, 1_028_500), None);

        assert_eq!(value(Network::Regtest, 0), None);
        assert_eq!(value(Network::Regtest, 1), None);
    }

    #[test]
    fn the_address_changes_every_17709_adjusted_blocks() {
        let address = |network, height| {
            address_of(network, &founders_reward(network, height).unwrap().script)
        };
        for (network, addresses, blossom, last) in [
            (
                Network::Mainnet,
                &MAINNET_ADDRESSES,
                653_600u32,
                1_046_399u32,
            ),
            (Network::Testnet, &TESTNET_ADDRESSES, 584_000, 1_028_499),
        ] {
            assert_eq!(address(network, 1), addresses[0]);
            assert_eq!(address(network, 17_708), addresses[0]);
            assert_eq!(address(network, 17_709), addresses[1]);
            // Before Blossom the adjusted height is the height.
            let index = blossom / 17_709;
            assert_eq!(
                address(network, blossom - 1),
                addresses[((blossom - 1) / 17_709) as usize]
            );
            assert_eq!(address(network, blossom), addresses[index as usize]);
            // From Blossom two blocks are one adjusted block.
            let boundary = blossom + 2 * ((index + 1) * 17_709 - blossom);
            assert_eq!(address(network, boundary - 1), addresses[index as usize]);
            assert_eq!(address(network, boundary), addresses[index as usize + 1]);
            let last_index = (blossom + (last - blossom) / 2) / 17_709;
            assert_eq!(address(network, last), addresses[last_index as usize]);
        }
        // The Mainnet reward ends in the last address. Testnet ends earlier: Canopy.
        assert_eq!(address(Network::Mainnet, 1_046_399), MAINNET_ADDRESSES[47]);
        assert_eq!(
            address(Network::Mainnet, 1),
            "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd"
        );
        assert_eq!(
            address(Network::Testnet, 1),
            "t2UNzUUx8mWBCRYPRezvA363EYXyEpHokyi"
        );
    }

    #[test]
    fn every_address_is_a_p2sh_address_of_its_network() {
        for (network, addresses) in [
            (Network::Mainnet, &MAINNET_ADDRESSES),
            (Network::Testnet, &TESTNET_ADDRESSES),
        ] {
            for address in addresses {
                let script = address_script(network, address);
                assert_eq!((script[0], script[1], script[22]), (0xa9, 0x14, 0x87));
            }
        }
    }
}
