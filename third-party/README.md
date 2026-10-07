# Third-party sources

hayai is `Copyright (c) 2026 hayai contributors`, under the MIT license (`../LICENSE-MIT`) or
the Apache License 2.0 (`../LICENSE-APACHE`), at the option of the user.

hayai takes code, data or designs from the sources below. Each folder holds the licence files
of one source, as the source publishes them. Where a source offers a choice of licences, hayai
uses it under the MIT license. The libraries that hayai only calls as dependencies are not
listed: their own crates carry their licences.

The authors used the source code of Zakura, Zebra, zcashd and librustzcash as a reference for
the whole tree. The notices of these four projects therefore apply to the whole tree, in
addition to the files that the first table names.

## Code and data in the tree

| Source | Folder | Licence | Path in hayai | Content |
|---|---|---|---|---|
| [Zakura](https://github.com/zakura-core/zakura) | `zakura/` | MIT OR Apache-2.0 | `crates/hayai-fuzz/src/reference/zakura_consensus/` | Copy of the block and transaction checks of `zakura-consensus`, the oracle of the differential fuzzer |
| | | | `crates/hayai-bench/src/zakura_*.rs` | Ports of Zakura code, the baselines of the benchmarks |
| | | | `crates/hayai-consensus/src/checkpoints/` | The checkpoint lists of Zakura |
| | | | `crates/hayai-consensus/src/funding.rs`, `crates/hayai-consensus/src/founders.rs` | The tables of funding stream and founders' reward addresses |
| | | | `crates/hayaid/tests/fixtures/` | Configuration files that `zakurad` writes |
| | | | `docker/config/zakurad.*.toml`, `docker/race/config/zakurad.*.toml` | Configuration files of `zakurad` |
| [Zebra](https://github.com/ZcashFoundation/zebra) | `zebra/` | MIT OR Apache-2.0 | `crates/hayai-bench/src/zebra_*.rs` | Ports of Zebra code, the baselines of the benchmarks |
| | | | `crates/hayai-bench/tests/vectors/` (except `expected-*.json` and `tx-sighash-inputs.json`) | Block vectors, final tree roots and Sapling tree states of `zebra-test` |
| | | | `crates/hayai-wire/tests/vectors/block-*.hex` | Block vectors of `zebra-test` |
| | | | `crates/hayai-state/tests/vectors/block-*.hex`, `crates/hayai-state/tests/vectors/header-*.hex` | Block and header vectors of `zebra-test` |
| | | | `crates/hayai-net/tests/codec.rs` | Network address vectors of `zebra-test` |
| | | | `crates/hayai-trees/src/sprout.rs` | Sprout empty roots, as `zebra-chain` publishes them |
| | | | `crates/hayai-prepared/src/sprout.rs` | `h_sig` vectors of `zebra-consensus` |
| [zcashd](https://github.com/zcash/zcash) | `zcashd/` | MIT | `crates/hayai-trees/src/sprout.rs` | Sprout empty roots of `IncrementalMerkleTree.cpp` |
| [librustzcash](https://github.com/zcash/librustzcash) | `librustzcash/` | MIT OR Apache-2.0 | `crates/hayai-state/tests/vectors/zip_0221_*.rs` | ZIP 221 vectors of `zcash_history` |
| | | | `crates/hayai-wire/tests/vectors/tx-*.hex` | Transaction vectors of the tests of `zcash_primitives` |
| | | | `crates/hayai-bench/tests/vectors/tx-sighash-inputs.json` | Inputs of the sighash vectors of the tests of `zcash_primitives` |
| [zcash-test-vectors](https://github.com/zcash/zcash-test-vectors) | `zcash-test-vectors/` | MIT OR Apache-2.0 | `crates/hayai-state/tests/vectors/zip_0221_*.rs` | ZIP 221 vectors, the origin of the copy in `zcash_history` |
| | | | `crates/hayai-wire/tests/vectors/tx-zip0*.hex` | ZIP 143, ZIP 243 and ZIP 244 vectors, the origin of the copy in `zcash_primitives` |
| [libsecp256k1](https://github.com/bitcoin-core/secp256k1) | `libsecp256k1/` | MIT | `crates/hayai-sinsemilla/src/invert.rs` | Port of the variable-time `modinv64` field inversion |
| [Bitcoin Core](https://github.com/bitcoin/bitcoin) | `bitcoin-core/` | MIT | `crates/hayai-net/tests/codec.rs` | An `addrv2` vector of `net_tests.cpp` |

## Designs

| Source | Folder | Licence | Designs in hayai |
|---|---|---|---|
| [Zakura](https://github.com/zakura-core/zakura) | `zakura/` | MIT OR Apache-2.0 | The consensus rules of NU7, NSM and Ironwood, the Regtest rules, the command line, the configuration keys, the metric names, the JSONL trace format, the fee policy of the mempool, the network constants |
| [Zakura common](https://github.com/zakura-core/common) | `zakura-common/` | MIT OR Apache-2.0 | The position-weighted table of the Sinsemilla hash (`crates/hayai-sinsemilla`) |
| [Zebra](https://github.com/ZcashFoundation/zebra) | `zebra/` | MIT OR Apache-2.0 | The consensus checks and the network constants of the address book |
| [zcashd](https://github.com/zcash/zcash) | `zcashd/` | MIT | The consensus checks of the transaction inputs, the long poll of `getblocktemplate`, the network constants |
| [Bitcoin Core](https://github.com/bitcoin/bitcoin) | `bitcoin-core/` | MIT | The coins cache (`CCoinsViewCache`), the script compression of the coins (`ScriptCompression`), the address book limits, the compact block relay (BIP 152) |
| [librustzcash](https://github.com/zcash/librustzcash) | `librustzcash/` | MIT OR Apache-2.0 | The transaction format and the ZIP 244 digests that `crates/hayai-wire` computes |

## Specifications

| Source | Folder | Licence | Use |
|---|---|---|---|
| [Zcash protocol specification and ZIPs](https://github.com/zcash/zips) | `zips/` | MIT | The consensus rules, the network protocol and the RPC interface |

The Bitcoin Improvement Proposals below are protocol references. hayai holds no text of them.

| BIP | Title | Licence |
|---|---|---|
| 11 | M-of-N Standard Transactions | Not stated |
| 14 | Protocol Version and User Agent | Not stated |
| 16 | Pay to Script Hash | Not stated |
| 22 | getblocktemplate - Fundamentals | BSD-2-Clause |
| 30 | Duplicate transactions | BSD-2-Clause |
| 31 | Pong message | Not stated |
| 32 | Hierarchical Deterministic Wallets | BSD-2-Clause |
| 34 | Block v2, Height in Coinbase | Not stated |
| 35 | mempool message | Not stated |
| 37 | Connection Bloom filtering | Public domain |
| 61 | Reject P2P message | Not stated |
| 65 | OP_CHECKLOCKTIMEVERIFY | Public domain |
| 66 | Strict DER signatures | BSD-2-Clause |
| 70 | Payment Protocol | Not stated |
| 111 | NODE_BLOOM service bit | Public domain |
| 152 | Compact Block Relay | Public domain |
| 155 | addrv2 message | BSD-2-Clause |

## Publications

These publications are design references. hayai holds no text or code of them.

- Danezis, Kokoris-Kogias, Sonnino, Spiegelman. "Narwhal and Tusk: a DAG-based mempool and
  efficient BFT consensus". EuroSys 2022.
- Spiegelman, Giridharan, Sonnino, Kokoris-Kogias. "Bullshark: DAG BFT protocols made
  practical". CCS 2022.
- Giridharan, Suri-Payer, Abraham, Alvisi, Crooks. "Autobahn: seamless high speed BFT".
  SOSP 2024.
- Aptos Quorum Store: batch dissemination by workers, consensus over batch digests.
- The documentation of Geth and Reth (Ethereum nodes): state layers over recent blocks.
- The fjall 3 benchmark post (`fjall-rs.github.io`).
