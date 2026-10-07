# Third-party sources

hayai is `Copyright (c) 2026 hayai contributors`, under the MIT license (`../LICENSE-MIT`) or
the Apache License 2.0 (`../LICENSE-APACHE`), at the option of the user.

The authors of hayai used the sources below during development. Each folder holds the licence
files of one source, as the source publishes them. Where a source offers a choice of licences,
hayai uses it under the MIT license.

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

The licence of zcashd also carries the copyright of the Bitcoin Core developers. That notice
covers the designs of Bitcoin Core that hayai follows, for example the coins cache.

## Source code read during development

| Source | Folder | Licence | Parts read |
|---|---|---|---|
| [Zakura](https://github.com/zakura-core/zakura) | `zakura/` | MIT OR Apache-2.0 | The node: consensus, state, network, RPC, block production, configuration, metrics, traces |
| [Zakura common](https://github.com/zakura-core/common) | `zakura-common/` | MIT OR Apache-2.0 | The `zakura-*` forks of the Zcash cryptography crates (Orchard, Sapling, Sinsemilla, Pasta, Equihash, transparent) |
| [Zebra](https://github.com/ZcashFoundation/zebra) | `zebra/` | MIT OR Apache-2.0 | `zebra-chain`, `zebra-consensus`, `zebra-state`, `zebra-network`, `zebra-rpc` |
| [zcashd](https://github.com/zcash/zcash) | `zcashd/` | MIT | Consensus checks, the Sprout tree, network constants |
| [librustzcash](https://github.com/zcash/librustzcash) | `librustzcash/` | MIT OR Apache-2.0 | `zcash_primitives`, `zcash_protocol`, `zcash_transparent`, `zcash_history`, `zcash_encoding`, `zcash_proofs`, `zcash_address`, `zcash_note_encryption`, `equihash` |
| [orchard](https://github.com/zcash/orchard) | `orchard/` | MIT OR Apache-2.0 | Bundles, batch verification, note encryption |
| [sapling-crypto](https://github.com/zcash/sapling-crypto) | `sapling-crypto/` | MIT OR Apache-2.0 | Batch verification |
| [pasta_curves](https://github.com/zcash/pasta_curves) | `pasta_curves/` | MIT OR Apache-2.0 | Field arithmetic |
| [sinsemilla](https://github.com/zcash/sinsemilla) | `sinsemilla/` | MIT OR Apache-2.0 | The Sinsemilla hash |
| [incrementalmerkletree](https://github.com/zcash/incrementalmerkletree) | `incrementalmerkletree/` | MIT OR Apache-2.0 | Frontiers |
| [zcash_script](https://github.com/ZcashFoundation/zcash_script) | `zcash_script/` | Apache-2.0 | The script interpreter and its test vectors |
| [ed25519-zebra](https://github.com/ZcashFoundation/ed25519-zebra) | `ed25519-zebra/` | MIT OR Apache-2.0 | Signature verification |
| [reddsa](https://github.com/ZcashFoundation/reddsa) | `reddsa/` | MIT OR Apache-2.0 | Batch verification |
| [ff](https://github.com/zkcrypto/ff) | `ff/` | MIT OR Apache-2.0 | Field traits |
| [group](https://github.com/zkcrypto/group) | `group/` | MIT OR Apache-2.0 | Group traits |
| [bellman](https://github.com/zkcrypto/bellman) | `bellman/` | MIT OR Apache-2.0 | Groth16 verification |
| [rust-rocksdb](https://github.com/rust-rocksdb/rust-rocksdb) | `rust-rocksdb/` | Apache-2.0 | The RocksDB bindings |
| [RocksDB](https://github.com/facebook/rocksdb) | `rocksdb/` | GPL-2.0 OR Apache-2.0, LevelDB parts BSD-3-Clause | Options of the C++ library |
| [fjall](https://github.com/fjall-rs/fjall) | `fjall/` | MIT OR Apache-2.0 | Storage engine, measured as a candidate |
| [lsm-tree](https://github.com/fjall-rs/lsm-tree) | `lsm-tree/` | MIT OR Apache-2.0 | Storage engine, measured as a candidate |
| [redb](https://github.com/cberner/redb) | `redb/` | MIT OR Apache-2.0 | Storage engine, measured as a candidate |
| [heed](https://github.com/meilisearch/heed) | `heed/` | MIT | LMDB bindings, measured as a candidate |
| [papaya](https://github.com/ibraheemdev/papaya) | `papaya/` | MIT | Concurrent map, measured as a candidate |
| [scc](https://codeberg.org/wvwwvwwv/scalable-concurrent-containers) | `scc/` | Apache-2.0 | Concurrent map, measured as a candidate |
| [rapidhash](https://github.com/hoxxep/rapidhash) | `rapidhash/` | MIT OR Apache-2.0 | Hash function, measured as a candidate |
| [hashbrown](https://github.com/rust-lang/hashbrown) | `hashbrown/` | MIT OR Apache-2.0 | Hash table |
| [multitable](https://github.com/maksympetkus/multitable) | `multitable/` | MIT | Hash table, measured as a candidate |
| [crossbeam](https://github.com/crossbeam-rs/crossbeam) | `crossbeam/` | MIT OR Apache-2.0 | Channels |
| [tokio](https://github.com/tokio-rs/tokio) | `tokio/` | MIT | `tokio-util` |
| [rand](https://github.com/rust-random/rand) | `rand/` | MIT OR Apache-2.0 | Random number generators |
| [proptest](https://github.com/proptest-rs/proptest) | `proptest/` | MIT OR Apache-2.0 | Property tests |
| [RustCrypto hashes](https://github.com/RustCrypto/hashes) | `rustcrypto-hashes/` | MIT OR Apache-2.0 | SHA-2 |
| [siphasher](https://github.com/jedisct1/rust-siphash) | `siphasher/` | MIT OR Apache-2.0 | SipHash, for the short ids |
| [rust-secp256k1](https://github.com/rust-bitcoin/rust-secp256k1) | `rust-secp256k1/` | CC0-1.0 | Signature verification |
| [tracing](https://github.com/tokio-rs/tracing) | `tracing/` | MIT | Logs |
| [metrics](https://github.com/metrics-rs/metrics) | `metrics/` | MIT | The Prometheus exporter |
| [mset](https://github.com/lonnen/mset) | `mset/` | MIT OR Apache-2.0 | Multiset |
| [parity-common](https://github.com/paritytech/parity-common) | `parity-common/` | MIT OR Apache-2.0 | `uint`, 256-bit integers |

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
