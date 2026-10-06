# Consensus rules: trace to code, tests and Zakura

Date: 2026-10-06. Sources:

- ZIPs: repository `github.com/zcash/zips`, commit `a735caf76ed8d3fa0a50e4f6b706ca9dac39079b`.
- Protocol specification: `protocol/protocol.tex` of the same commit. Section numbers are those
  of the rendered NU6.3 variant, version `v2026.7.0-236-ga735ca [NU6.3 proposal]`.
- Reference implementation: Zakura, commit `13779158253cfe315f73eadffb9b4c93c25e82a5`. Paths
  of Zakura are relative to `zakura/crates/`.
- Upstream crates of the default backend, as `Cargo.lock` pins them: `zcash_primitives`
  0.30.1, `zcash_transparent` 0.10.0, `zcash_script` 0.6.0, `orchard` 0.15.5,
  `sapling-crypto` 0.7.0.

Scope of the rows:

- ZIPs of class (a) consensus, (b) network protocol and (c) mining, RPC and mempool. Each
  normative requirement (MUST, MUST NOT, SHALL, a consensus rule, a constant that the ZIP
  defines) is one row. For class (b) and (c), a SHOULD that hayai follows is a row too.
- The specification: each item of a consensus rule marker (`\consensusrule`,
  `\begin{consensusrules}`), and each MUST sentence outside a marker in the sections that a
  table names.
- Rule ids: `ZIP-<n>-<k>` (k in text order; a revision when the rule depends on it, for
  example `ZIP-214-r3-2`), `SPEC-<section>-<k>` for a marker item, `SPEC-<section>-M<k>` for a
  MUST outside a marker, `BLK-<k>` or `BLOCK-<k>` for a block-level rule without a ZIP.

Rule groups. Six groups of rules were traced. A row or a finding names a group as P1 to P6,
and a finding id `F-P<n>-<k>` names its group:

| Group | Rules |
|---|---|
| P1 | Header, proof of work, difficulty, network upgrades and their constants, checkpoints, NU7 spacing |
| P2 | Subsidy, founders' reward, funding streams, lockbox, coinbase value, NSM |
| P3 | Transaction format, identifiers, signature hash, scripts, expiry, lock time |
| P4 | Shielded protocols: JoinSplit, Spend, Output and Action descriptions, proofs, signatures, note encryption |
| P5 | Chain state: note commitment trees, nullifiers, anchors, value pools, history tree, block-level rules |
| P6 | Network protocol, relay, mempool, block production, RPC |

Status values:

| Status | Meaning |
|---|---|
| implemented+tested | hayai enforces the rule, and a test or a published vector set exercises it (a refused case or the boundary) |
| implemented, no direct test | hayai enforces the rule; no test gives an input that the rule refuses |
| implemented differently | hayai gives the same verdict by another method; the row states how and why |
| checkpoint path only | the rule applies only to blocks at or below the mandatory checkpoint (Mainnet 1,046,399, Testnet 1,028,499); hayai, as Zakura, trusts the checkpoint list for them |
| not implemented | no code enforces the rule; the row states the effect |
| not applicable | the rule does not bind a node (wallet rule, process rule, a pointer to other rows); the row states the reason |

Code references give the file and the function. Line numbers change with each commit and
are given only where a function name is not enough. The code carries a mark at each place that
enforces a rule, in the form `// ZIP <n>: ...` or `// Spec §<section>: ...` (or the same text
in the doc comment of the function). A rule that comes only from zcashd behaviour carries a
mark `// zcashd ...`.

## Summary

Counts of rules per status. Columns: tested = implemented+tested, no direct test = implemented, no direct test, differently = implemented differently, checkpoint only = checkpoint path only, not implemented = not implemented, not applicable = not applicable.

| Source | Class | Rules | tested | no direct test | differently | checkpoint only | not implemented | not applicable |
|---|---|---|---|---|---|---|---|---|
| ZIP 143 | (a) | 3 | 0 | 0 | 0 | 3 | 0 | 0 |
| ZIP 155 | (b) | 13 | 6 | 1 | 1 | 0 | 1 | 4 |
| ZIP 200 | (a) | 15 | 9 | 1 | 0 | 0 | 2 | 3 |
| ZIP 201 | (b) | 7 | 4 | 0 | 1 | 0 | 2 | 0 |
| ZIP 202 | (a) | 3 | 1 | 0 | 0 | 2 | 0 | 0 |
| ZIP 203 | (a) | 5 | 4 | 0 | 1 | 0 | 0 | 0 |
| ZIP 204 | (b) | 81 | 48 | 5 | 18 | 0 | 5 | 5 |
| ZIP 205 | (a) | 8 | 7 | 0 | 1 | 0 | 0 | 0 |
| ZIP 206 | (a) | 7 | 6 | 0 | 1 | 0 | 0 | 0 |
| ZIP 207 | (a) | 16 | 13 | 0 | 0 | 1 | 0 | 2 |
| ZIP 208 | (a) | 16 | 10 | 0 | 0 | 2 | 0 | 4 |
| ZIP 209 | (a) | 7 | 5 | 1 | 0 | 0 | 0 | 1 |
| ZIP 211 | (a) | 2 | 1 | 0 | 0 | 0 | 0 | 1 |
| ZIP 212 | (a) | 5 | 2 | 1 | 0 | 0 | 0 | 2 |
| ZIP 213 | (a) | 6 | 6 | 0 | 0 | 0 | 0 | 0 |
| ZIP 214 | (a) | 26 | 22 | 0 | 0 | 0 | 0 | 4 |
| ZIP 215 | (a) | 4 | 0 | 3 | 1 | 0 | 0 | 0 |
| ZIP 216 | (a) | 4 | 0 | 4 | 0 | 0 | 0 | 0 |
| ZIP 218 | (a) | 28 | 17 | 1 | 5 | 0 | 1 | 4 |
| ZIP 221 | (a) | 24 | 19 | 2 | 2 | 0 | 0 | 1 |
| ZIP 224 | (a) | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| ZIP 225 | (a) | 5 | 4 | 1 | 0 | 0 | 0 | 0 |
| ZIP 229 | (a) | 22 | 19 | 2 | 1 | 0 | 0 | 0 |
| ZIP 234 | (d) | 5 | 0 | 0 | 0 | 0 | 0 | 5 |
| ZIP 235 | (a) | 7 | 5 | 1 | 1 | 0 | 0 | 0 |
| ZIP 236 | (a) | 4 | 4 | 0 | 0 | 0 | 0 | 0 |
| ZIP 237 | (a) | 17 | 8 | 1 | 6 | 0 | 0 | 2 |
| ZIP 239 | (b) | 11 | 6 | 2 | 3 | 0 | 0 | 0 |
| ZIP 243 | (a) | 3 | 3 | 0 | 0 | 0 | 0 | 0 |
| ZIP 244 | (a) | 10 | 9 | 1 | 0 | 0 | 0 | 0 |
| ZIP 250 | (a) | 7 | 6 | 0 | 1 | 0 | 0 | 0 |
| ZIP 251 | (a) | 7 | 6 | 0 | 1 | 0 | 0 | 0 |
| ZIP 252 | (a) | 8 | 6 | 0 | 2 | 0 | 0 | 0 |
| ZIP 253 | (a) | 6 | 5 | 0 | 1 | 0 | 0 | 0 |
| ZIP 254 | (d) | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| ZIP 255 | (a) | 4 | 3 | 0 | 1 | 0 | 0 | 0 |
| ZIP 256 | (a) | 11 | 7 | 3 | 1 | 0 | 0 | 0 |
| ZIP 257 | (a) | 11 | 11 | 0 | 0 | 0 | 0 | 0 |
| ZIP 258 | (a) | 13 | 11 | 1 | 0 | 0 | 0 | 1 |
| ZIP 259 | (a) | 9 | 9 | 0 | 0 | 0 | 0 | 0 |
| ZIP 271 | (a) | 20 | 15 | 1 | 2 | 0 | 1 | 1 |
| ZIP 301 | (c) | 20 | 0 | 0 | 0 | 0 | 0 | 20 |
| ZIP 317 | (c) | 26 | 15 | 2 | 6 | 0 | 0 | 3 |
| ZIP 323 | (c) | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| ZIP 401 | (c) | 13 | 10 | 1 | 1 | 0 | 1 | 0 |
| ZIP 1014 | (a) | 20 | 2 | 0 | 0 | 0 | 0 | 18 |
| ZIP 1015 | (a) | 26 | 4 | 0 | 0 | 0 | 0 | 22 |
| ZIP 1016 | (a) | 11 | 2 | 0 | 0 | 0 | 0 | 9 |
| ZIP 2001 | (a) | 8 | 8 | 0 | 0 | 0 | 0 | 0 |
| ZIP 2003 | (a) | 1 | 1 | 0 | 0 | 0 | 0 | 0 |
| ZIP 2005 | (a) | 3 | 2 | 0 | 0 | 0 | 0 | 1 |
| ZIP 2006 | (a) | 1 | 0 | 0 | 0 | 0 | 0 | 1 |
| ZIP 2008 | (a) | 2 | 0 | 0 | 0 | 0 | 1 | 1 |
| Spec §3.4 | spec | 2 | 2 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.5 | spec | 2 | 2 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.6 | spec | 2 | 2 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.7 | spec | 4 | 4 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.8 | spec | 4 | 4 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.9 | spec | 1 | 1 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.10 | spec | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| Spec §3.11 | spec | 2 | 1 | 0 | 0 | 1 | 0 | 0 |
| Spec §4.3 | spec | 4 | 3 | 1 | 0 | 0 | 0 | 0 |
| Spec §4.4 | spec | 4 | 2 | 2 | 0 | 0 | 0 | 0 |
| Spec §4.5 | spec | 3 | 1 | 2 | 0 | 0 | 0 | 0 |
| Spec §4.6 | spec | 7 | 5 | 2 | 0 | 0 | 0 | 0 |
| Spec §4.10 | spec | 9 | 4 | 3 | 0 | 2 | 0 | 0 |
| Spec §4.17 | spec | 7 | 7 | 0 | 0 | 0 | 0 | 0 |
| Spec §7.1 | spec | 7 | 4 | 1 | 0 | 1 | 0 | 1 |
| Spec §7.1.2 | spec | 60 | 43 | 8 | 4 | 4 | 0 | 1 |
| Spec §7.2 to §7.5 | spec | 5 | 2 | 3 | 0 | 0 | 0 | 0 |
| Spec §7.6 | spec | 18 | 15 | 2 | 0 | 0 | 0 | 1 |
| Spec §7.7 | spec | 14 | 12 | 0 | 2 | 0 | 0 | 0 |
| Spec §7.8 | spec | 8 | 8 | 0 | 0 | 0 | 0 | 0 |
| Spec §7.9 | spec | 8 | 7 | 0 | 0 | 1 | 0 | 0 |
| Spec §7.10 | spec | 11 | 8 | 0 | 2 | 0 | 1 | 0 |
| Spec §7.10.1 | spec | 6 | 6 | 0 | 0 | 0 | 0 | 0 |
| Spec §7.11 | spec | 7 | 2 | 3 | 1 | 0 | 0 | 1 |
| Rules without a ZIP (block-level, checkpoints, QUIC draft) | other | 15 | 14 | 0 | 0 | 0 | 1 | 0 |
| Total | | 802 | 520 | 62 | 67 | 17 | 16 | 120 |

## ZIP inventory

Every ZIP and draft of the repository. Classes: (a) consensus, active on Mainnet or Testnet at a deployed upgrade, or at NU7 as Zakura implements it on Testnet; (b) network protocol; (c) mining, RPC and mempool; (d) not applicable to a node. Counts: (a) 43, (b) 4, (c) 4, (d) 84, total 135.

| ZIP | Title | Status | Category | Class | Activation or reason |
|---|---|---|---|---|---|
| 0 | ZIP Process | Active | Process | (d) | process ZIP |
| 1 | Network Upgrade Policy and Scheduling | Reserved | Consensus Process | (d) | process ZIP (reserved) |
| 2 | Design Considerations for Network Upgrades | Reserved | Informational | (d) | informational (reserved) |
| 32 | Shielded Hierarchical Deterministic Wallets | Final | Standards / Wallet | (d) | wallet key derivation |
| 48 | Transparent Multisig Wallets | Draft | Wallet | (d) | wallet (transparent multisig) |
| 68 | Relative lock-time using consensus-enforced sequence numbers | Draft | Consensus | (d) | consensus draft (BIP 68) that no upgrade activates |
| 76 | Transaction Signature Validation before Overwinter | Reserved | Consensus | (d) | reserved; no text (pre-Overwinter sighash is checkpoint-only) |
| 112 | CHECKSEQUENCEVERIFY | Draft | Consensus | (d) | consensus draft (BIP 112) that no upgrade activates |
| 113 | Median Time Past as endpoint for lock-time calculations | Draft | Consensus | (d) | consensus draft (BIP 113) that no upgrade activates |
| 129 | Zcash Transparent Multisig Setup | Reserved | Wallet | (d) | wallet (reserved) |
| 143 | Transaction Signature Validation for Overwinter | Final | Consensus | (a) | Overwinter |
| 155 | addrv2 message | Proposed | Network | (b) | addrv2 message (no upgrade) |
| 173 | Bech32 Format | Final | Standards / Wallet | (d) | address encoding (wallet) |
| 200 | Network Upgrade Mechanism | Final | Consensus | (a) | Overwinter (mechanism of every upgrade) |
| 201 | Network Peer Management for Overwinter | Final | Network | (b) | Overwinter peer management (every upgrade) |
| 202 | Version 3 Transaction Format for Overwinter | Final | Consensus | (a) | Overwinter |
| 203 | Transaction Expiry | Final | Consensus | (a) | Overwinter; NU5 change |
| 204 | Zcash P2P Network Protocol | Draft | Network | (b) | P2P protocol versions |
| 205 | Deployment of the Sapling Network Upgrade | Final | Consensus / Network | (a) | Sapling |
| 206 | Deployment of the Blossom Network Upgrade | Final | Consensus / Network | (a) | Blossom |
| 207 | Funding Streams | [Revision 0: Canopy, Revision 1: NU6] Final, [Revision 2: NU7] Draft | Consensus | (a) | Canopy (rev 0), NU6 (rev 1), NU7 (rev 2) |
| 208 | Shorter Block Target Spacing | Final | Consensus | (a) | Blossom |
| 209 | Prohibit Out-of-Range Chain Value Pool Balances | Final | Consensus | (a) | from acceptance; ZIP 256, ZIP 258 changes |
| 210 | Sapling Anchor Deduplication within Transactions | Withdrawn | Consensus | (d) | withdrawn |
| 211 | Disabling Addition of New Value to the Sprout Chain Value Pool | Final | Consensus | (a) | Canopy |
| 212 | Allow Recipient to Derive Ephemeral Secret from Note Plaintext | Final | Consensus | (a) | Canopy |
| 213 | Shielded Coinbase | Final | Consensus | (a) | Heartwood; NU5, NU6.3 changes |
| 214 | Consensus rules for a Zcash Development Fund | [Revision 0: Canopy, Revision 1: NU6] Final, [Revision 2: NU6.1] Proposed, [Revision 3: NU7] Draft | Consensus | (a) | Canopy (rev 0), NU6 (rev 1), NU6.1 (rev 2), NU7 (rev 3) |
| 215 | Explicitly Defining and Modifying Ed25519 Validation Rules | Final | Consensus | (a) | Canopy |
| 216 | Require Canonical Jubjub Point Encodings | Final | Consensus | (a) | NU5 (retroactive) |
| 217 | Aggregate Signatures | Reserved | Consensus | (d) | reserved; no text |
| 218 | 25-second Block Target Spacing | Draft | Consensus | (a) | NU7 (Zakura Testnet 4,465,026) |
| 219 | Disabling Addition of New Value to the Sapling Chain Value Pool | Reserved | Consensus | (d) | reserved; no text |
| 220 | Zcash Shielded Assets | Withdrawn | Consensus | (d) | withdrawn |
| 221 | FlyClient - Consensus-Layer Changes | Final | Consensus | (a) | Heartwood; NU5, NU6.3 changes |
| 222 | Transparent Zcash Extensions | Draft | Consensus | (d) | consensus draft (TZE) that no upgrade activates |
| 224 | Orchard Shielded Protocol | Final | Consensus | (a) | NU5 |
| 225 | Version 5 Transaction Format | Final | Consensus | (a) | NU5 |
| 226 | Transfer and Burn of Zcash Shielded Assets | Draft | Consensus | (d) | consensus draft (ZSA) that no upgrade activates |
| 227 | Issuance of Zcash Shielded Assets | Draft | Consensus | (d) | consensus draft (ZSA) that no upgrade activates |
| 228 | Asset Swaps for Zcash Shielded Assets | Draft | Consensus | (d) | consensus draft (ZSA) that no upgrade activates |
| 229 | Version 6 Transaction Format | Draft | Consensus | (a) | NU6.3 |
| 230 | Withdrawn Version 6 Transaction Format | Withdrawn | Consensus | (d) | withdrawn |
| 231 | Memo Bundles | Draft | Consensus / Wallet | (d) | consensus draft (memo bundles) that no upgrade activates |
| 233 | Network Sustainability Mechanism: Removing Funds From Circulation | Draft | Consensus / Ecosystem | (d) | consensus draft (NSM burn) that NU7 does not deploy (ZIP 259) |
| 234 | Network Sustainability Mechanism: Issuance Smoothing | Draft | Consensus | (d) | consensus draft (NSM smoothing) that NU7 does not deploy (ZIP 259) |
| 235 | Remove 60% of Transaction Fees From Circulation | Draft | Consensus / Ecosystem | (a) | NU7 |
| 236 | Blocks should balance exactly | Final | Consensus | (a) | NU6 |
| 237 | Network Sustainability Mechanism: Halving-Preserving Issuance | Draft | Consensus | (a) | NU7 |
| 239 | Relay of Version 5 Transactions | Final | Network | (b) | NU5 relay of v5 transactions |
| 240 | Standard Transaction Rules | Reserved | Consensus | (d) | reserved; no text |
| 243 | Transaction Signature Validation for Sapling | Final | Consensus | (a) | Sapling |
| 244 | Transaction Identifier Non-Malleability | Final | Consensus | (a) | NU5 |
| 245 | Transaction Identifier Digests & Signature Validation for Transparent Zcash Extensions | Draft | Consensus | (d) | consensus draft (TZE digests) that no upgrade activates |
| 246 | Digests for the Withdrawn Version 6 Transaction Format | Withdrawn | Consensus | (d) | withdrawn |
| 248 | Extensible Transaction Format | Draft | Consensus / Wallet | (d) | consensus draft (extensible format) that no upgrade activates |
| 250 | Deployment of the Heartwood Network Upgrade | Final | Consensus / Network | (a) | Heartwood |
| 251 | Deployment of the Canopy Network Upgrade | Final | Consensus / Network | (a) | Canopy |
| 252 | Deployment of the NU5 Network Upgrade | Final | Consensus / Network | (a) | NU5 |
| 253 | Deployment of the NU6 Network Upgrade | Final | Consensus / Network | (a) | NU6 |
| 254 | Deployment of the NU7 Network Upgrade (Withdrawn) | Withdrawn | Consensus / Network | (d) | withdrawn (NU7 deployment, replaced by ZIP 259) |
| 255 | Deployment of the NU6.1 Network Upgrade | Final | Consensus / Network | (a) | NU6.1 |
| 256 | Deployment of Consensus Bug Fixes Between NU6.1 and NU6.2 | Final | Consensus / Network | (a) | between NU6.1 and NU6.2 (no height) |
| 257 | Deployment of the Orchard Temporary Vulnerability Mitigation and NU6.2 Network Upgrade | Final | Consensus / Network | (a) | Orchard mitigation and NU6.2 |
| 258 | Deployment of the NU6.3 Network Upgrade | Draft | Consensus / Network | (a) | NU6.3 |
| 259 | Deployment of the NU7 Network Upgrade | Draft | Consensus / Network | (a) | NU7 |
| 260 | Extending Block Messages with Additional Authentication Data | Reserved | Network | (d) | network ZIP, reserved; no text |
| 270 | Key Rotation for Tracked Signing Keys | Reserved | Consensus | (d) | reserved; no text |
| 271 | Dev Fund Extension and One-Time Disbursement | Proposed | Consensus / Process | (a) | NU6.1 |
| 300 | Cross-chain Atomic Transactions | Proposed | Informational | (d) | informational (atomic swaps) |
| 301 | Zcash Stratum Protocol | Active | Standards / Ecosystem | (c) | Stratum (mining) |
| 302 | Standardized Memo Field Format | Draft | Standards / RPC / Wallet | (d) | wallet memo format |
| 303 | Sprout Payment Disclosure | Withdrawn | Standards / RPC / Wallet | (d) | withdrawn |
| 304 | Sapling Address Signatures | Draft | Standards / RPC / Wallet | (d) | wallet signatures |
| 305 | Best Practices for Hardware Wallets supporting Sapling | Reserved | Wallet | (d) | wallet (reserved) |
| 306 | Security Considerations for Anchor Selection | Reserved | Informational | (d) | informational (reserved) |
| 307 | Light Client Protocol for Payment Detection | Draft | Standards / Ecosystem | (d) | light client protocol |
| 308 | Sprout to Sapling Migration | Active | Standards / RPC / Wallet | (d) | wallet migration |
| 309 | Blind Off-chain Lightweight Transactions (BOLT) | Reserved | Standards / Ecosystem | (d) | reserved |
| 310 | Security Properties of Sapling Viewing Keys | Draft | Informational | (d) | informational |
| 311 | Zcash Payment Disclosures | Draft | Standards / RPC / Wallet | (d) | wallet |
| 312 | FROST for Spend Authorization Multisignatures | Draft | Wallet | (d) | wallet (FROST) |
| 313 | Reduce Conventional Transaction Fee to 1000 zatoshis | Obsolete | Wallet | (d) | obsolete |
| 314 | Privacy upgrades to the Zcash light client protocol | Reserved | Standards / Wallet | (d) | wallet (reserved) |
| 315 | Best Practices for Wallet Implementations | Draft | Wallet | (d) | wallet |
| 316 | Unified Addresses and Unified Viewing Keys | [Revision 0] Active, [Revision 1] Withdrawn, [Revision 2] Draft | Standards / RPC / Wallet | (d) | address encoding (wallet) |
| 317 | Proportional Transfer Fee Mechanism | [Revision 0] Active, [Revision 1: NU6.3] Draft, [Revision 2] Draft | Standards / Wallet | (c) | block production and mempool fees |
| 318 | Orchard to Ironwood Migration | Draft | Wallet | (d) | wallet migration |
| 319 | Options for Shielded Pool Retirement | Reserved | Informational | (d) | informational (reserved) |
| 320 | Defining an Address Type to which funds can only be sent from Transparent Addresses | Active | Standards / Wallet | (d) | address encoding (wallet) |
| 321 | Payment Request URIs | Active | Standards / Wallet | (d) | wallet URIs |
| 322 | Generic Signed Message Format | Reserved | Standards / RPC / Wallet | (d) | wallet (reserved) |
| 323 | Specification of getblocktemplate for Zcash | Reserved | RPC / Mining | (c) | getblocktemplate (reserved) |
| 324 | URI-Encapsulated Payments | Draft | Standards / Wallet | (d) | wallet URIs |
| 325 | Account Metadata Keys | Draft | Standards / Wallet | (d) | wallet keys |
| 326 | NU6.3 Consequences for Wallets | Draft | Wallet | (d) | wallet consequences of NU6.3 |
| 332 | Wallet Recovery from zcashd HD Seeds | Reserved | Wallet | (d) | wallet (reserved) |
| 339 | Wallet Recovery Words | Reserved | Wallet | (d) | wallet (reserved) |
| 350 | Bech32m | Reserved | Standards / Wallet | (d) | address encoding (reserved) |
| 374 | Partially Created Zcash Transaction Format | [Revision 0] Draft | Standards / Wallet | (d) | wallet (PCZT) |
| 400 | Wallet.dat format | Draft | Wallet | (d) | wallet file format |
| 401 | Addressing Mempool Denial-of-Service | Active | Network | (c) | mempool denial-of-service |
| 402 | New Wallet Database Format | Reserved | Wallet | (d) | wallet (reserved) |
| 403 | Verification Behaviour of zcashd | Reserved | Informational | (d) | informational (reserved) |
| 416 | Spending Key Derivation in the `zcashd` wallet | Reserved | RPC / Wallet | (d) | wallet (reserved) |
| 1001 | Keep the Block Distribution as Initially Defined — 90% to Miners | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1002 | Opt-in Donation Feature | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1003 | 20% Split Evenly Between the ECC and the Zcash Foundation, and a Voting System Mandate | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1004 | Miner-Directed Dev Fund | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1005 | Zcash Community Funding System | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1006 | Development Fund of 10% to a 2-of-3 Multisig with Community-Involved Third Entity | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1007 | Enforce Development Fund Commitments with a Legal Charter | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1008 | Fund ECC for Two More Years | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1009 | Five-Entity Strategic Council | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1010 | Compromise Dev Fund Proposal With Diverse Funding Streams | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1011 | Decentralize the Dev Fee | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1012 | Dev Fund to ECC + ZF + Major Grants | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1013 | Keep It Simple, Zcashers: 10% to ECC, 10% to ZF | Obsolete | Consensus Process | (d) | obsolete dev fund proposal (process) |
| 1014 | Establishing a Dev Fund for ECC, ZF, and Major Grants | Active | Consensus Process | (a) | Canopy (via ZIP 214) |
| 1015 | Block Subsidy Allocation for Non-Direct Development Funding | Final | Consensus | (a) | NU6 |
| 1016 | Community and Coinholder Funding Model | Proposed | Consensus / Process | (a) | NU6.1 |
| 2001 | Lockbox Funding Streams | Final | Consensus | (a) | NU6 |
| 2002 | Explicit Fees | Draft | Consensus | (d) | consensus draft (explicit fees) that no upgrade activates |
| 2003 | Disallow version 4 transactions | Draft | Consensus | (a) | NU7 |
| 2004 | Remove the dependency of consensus on note encryption | Draft | Consensus | (d) | consensus draft that no upgrade activates |
| 2005 | Ironwood Quantum Recoverability | Proposed | Consensus | (a) | NU6.3 |
| 2006 | Restricting Transfers into the Orchard Pool | Reserved | Consensus | (a) | NU6.3 (rules in ZIP 229, ZIP 258) |
| 2007 | Quantum Recoverability for a Subset of Transparent Addresses | Reserved | Consensus | (d) | reserved; no text |
| 2008 | Update to `FS_FPF_ZCG_H3` address list | Draft | Consensus | (a) | NU7 |
| draft-arya-dairaemma-disable-addition-of-transparent-chain-value | Disabling Addition of New Value to the Transparent Chain Value Pool | Draft | Consensus | (d) | consensus draft that no upgrade activates |
| draft-arya-jvff-p2p-quic-transport | Version 2 Zcash P2P Network Protocol | Draft | Network | (d) | network draft that no node deploys |
| draft-ecc-authenticated-reply-addrs | Authenticated Reply Addresses | Draft | Standards / Wallet | (d) | wallet draft |
| draft-ecc-onchain-accountable-voting | On-chain Accountable Voting | Draft | Consensus / Process | (d) | process draft |
| draft-mcgee-keyholders-organizations | Update to ZIP 1016 & ZIP 271: Key-Holder Organizations | Draft | Process | (d) | process draft |
| draft-str4d-orchard-balance-proof | Air drops, Proof-of-Balance, and Stake-weighted Polling | Draft | Informational | (d) | informational draft |

## ZIP 143: Transaction Signature Validation for Overwinter
Class: (a) consensus. Activation: Overwinter.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-143-1 | v3 transactions use the ZIP 143 sighash | `hayai-prepared/src/prepare.rs` `check_version` returns `Unsupported` for v3 | `prepare::tests::a_version_before_sapling_is_not_verified` | `zakura-consensus/src/transaction.rs:610` (`WrongVersion` for v1-v3) | checkpoint path only |
| ZIP-143-2 | v1 and v2 formats are invalid from Overwinter | upstream `TxVersion::read`; `check_version` (`RuleSet::tx_versions`) | `prepare::tests::the_rule_set_names_the_transaction_versions` | `zakura-consensus/src/transaction.rs:610` | checkpoint path only |
| ZIP-143-3 | The sighash commits to the consensus branch id of the epoch | upstream `signature_hash` with the branch id of the parse (`SighashContext::new`) | block vectors (`hayai-bench/tests/conformance_blocks.rs` `block_vectors_match_the_expected_outcomes`) | `zakura-chain/src/transaction/sighash.rs:206` | checkpoint path only |

## ZIP 155: addrv2 message
Class: (b) network. Activation: none (deployment version not assigned).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-155-1 | `addrv2` entry: time, CompactSize services, network id, CompactSize length, address, big-endian port | `hayai-net/src/codec.rs` `Reader::addr_v2` (line 884), `encode_body` | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks`, `addrv2_encodes_ipv4_and_ipv6` (`hayai-net/tests/codec.rs`) | `zakura-network/src/protocol/external/addr/v2.rs:264-310` | implemented+tested |
| ZIP-155-2 | Port is 0 when the network has no port | none | none | none | not applicable: hayai sends no `addrv2` |
| ZIP-155-3 | MUST refuse a message with more than 1,000 addresses | `codec.rs` `decode_body` (`addrv2`), `MAX_ADDR_ENTRIES` | `addrv2_rejects_invalid_lengths` | `addr/v2.rs` (`TrustedPreallocate`) | implemented+tested |
| ZIP-155-4 | MUST refuse an `addr` field above 512 bytes, whatever the network id | `codec.rs` `Reader::addr_v2` | `addrv2_rejects_invalid_lengths` | `addr/v2.rs:281-285` | implemented+tested |
| ZIP-155-5 | Network id 0x03 MUST NOT be used | `codec.rs` `Reader::addr_v2` (dropped); never sent | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks` | `addr/v2.rs:298-305` | implemented+tested |
| ZIP-155-6 | SHOULD gossip valid addresses of all known networks | `codec.rs` `Reader::addr_v2` keeps IPv4 and IPv6 only | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks` | `addr/v2.rs:298-305` (IPv4 and IPv6 only) | not implemented: Tor, I2P and CJDNS addresses are not gossiped; as Zakura |
| ZIP-155-7 | MUST NOT gossip addresses of unknown networks | `codec.rs` `Reader::addr_v2` | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks` | `addr/v2.rs:298-305` | implemented+tested |
| ZIP-155-8 | MUST refuse a message with an address whose length is not the length of its known network id | `codec.rs` `Reader::addr_v2` (IPv4 and IPv6 only) | `addrv2_rejects_invalid_lengths` | `addr/v2.rs:292-305` | implemented differently: a TORV3, I2P or CJDNS entry of a wrong length is dropped, not refused (finding F-P6-4) |
| ZIP-155-9 | Tor v3 addresses MUST be sent with `TORV3` and the 32-byte key | none | none | none | not applicable: hayai sends no Tor address |
| ZIP-155-10 | I2P addresses MUST be sent with `I2P` and the 32-byte hash | none | none | none | not applicable: hayai sends no I2P address |
| ZIP-155-11 | CJDNS addresses MUST be sent with `CJDNS` | none | none | none | not applicable: hayai sends no CJDNS address |
| ZIP-155-12 | MUST NOT send `addrv2` below the deployment version | `hayai-net/src/relay.rs` `Relay::on_addr`, `Relay::on_getaddr` send `addr` only | none | Zakura sends `addr` only | implemented, no direct test |
| ZIP-155-13 | MUST handle a received `addr` as before | `relay.rs` `Relay::deliver` (`Addr` and `AddrV2` take one path) | `zebra_addr_v1_vectors` (`hayai-net/tests/codec.rs`), `a_fresh_unsolicited_address_goes_on_to_two_peers_once` (`hayai-net/tests/peers.rs`) | `codec.rs:714` | implemented+tested |

## ZIP 200: Network Upgrade Mechanism
Class: (a) consensus. Activation: Overwinter (the mechanism applies to every later upgrade).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-200-1 | `CONSENSUS_BRANCH_ID` is a unique non-zero 32-bit id for each upgrade | `hayai-consensus/src/network.rs` `Upgrade::branch_id` (l. 353), values from `zcash_protocol` and `hayai_crypto::nu7_branch` | `network::tests::the_deployment_constants_of_the_zips`, `network::tests::every_branch_id_maps_to_its_upgrade` (`hayai-consensus/src/network.rs`) | `zakura-chain/src/parameters/network_upgrade.rs:231-243` | implemented+tested |
| ZIP-200-2 | Branch id 0 MAY mark the Sprout rules | `Upgrade::branch_id`: `Upgrade::Sprout` gives `BranchId::Sprout` (0) | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:226-228` (no branch id before Overwinter) | implemented+tested |
| ZIP-200-3 | `ACTIVATION_HEIGHT` is not zero | `hayai-consensus/src/network.rs` `protocol_height` (l. 646); `RegtestConfig::new` (l. 161) refuses a configured height below 2 | `network::tests::the_deployment_constants_of_the_zips`, `network::tests::a_configured_regtest_has_its_heights_and_its_checkpoints` | `zakura-chain/src/parameters/constants.rs:59-108` | implemented+tested |
| ZIP-200-4 | Epochs are `[ACTIVATION_HEIGHT_N, ACTIVATION_HEIGHT_N+1)`; the block at `ACTIVATION_HEIGHT - 1` has the old rules | `network.rs` `Network::upgrade_at` (l. 590); `rules.rs` `rules_at` (l. 359) | `rules::tests::the_rule_set_changes_at_every_activation_height`, `network::tests::the_upgrade_changes_at_every_activation_height` | `NetworkUpgrade::current` (`network_upgrade.rs`) | implemented+tested |
| ZIP-200-5 | `ACTIVATION_HEIGHT` MUST be above the `DEPRECATION_HEIGHT` of the last release without the upgrade | none | none | none | not applicable (release process of a deployment; hayai has no End-of-Service halt) |
| ZIP-200-6 | `ACTIVATION_HEIGHT` SHOULD be about 3 months after the first release with the upgrade | none | none | none | not applicable (release process) |
| ZIP-200-7 | A changed `ACTIVATION_HEIGHT` MUST get a new `CONSENSUS_BRANCH_ID` | none | none | none | not applicable (rule for the authors of a deployment ZIP) |
| ZIP-200-8 | A rule of an upgrade MUST be gated by a block-height check | `rules.rs` `rules_at`: each rule comes from the `RuleSet` of the height | `rules::tests::the_rule_set_changes_at_every_activation_height`, `rules::tests::the_rules_of_each_upgrade` | `NetworkUpgrade::current` | implemented+tested |
| ZIP-200-9 | A block of known height MUST be validated under the rules of the branch of that height | `hayai-validate/src/lib.rs` `ValidateConfig::rules` (from `rules_at`), `validate_bytes` parses with `rules.branch_id`; `hayai-prepared/src/prepare.rs` `check_version` (l. 205, P3) refuses another branch id | `rules::tests::the_rule_set_changes_at_every_activation_height`; hayaid `params::tests::the_rules_at_the_nu7_height` (`hayaid/src/params.rs`) | `zakura-consensus/src/block/check.rs:528` | implemented+tested |
| ZIP-200-10 | A block of unknown height MAY be kept until its parents arrive | `hayai-relay/src/header_check.rs` `StandardHeaderCheck::check` and hayaid `NodeHeaderCheck::rules` return `ParentUnknown` | none | none | not implemented (MAY; effect: the node refuses the block and gets the parent through the header sync) |
| ZIP-200-11 | A reorg across an activation height follows the normal reorg rules | `hayai-sync/src/headers.rs` best tip by work; each block of the new branch gets `rules_at` of its height | hayaid `sync_tests::a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization` (crosses NU7, no reorg) | none | implemented, no direct test |
| ZIP-200-12 | A node that upgrades late SHOULD stop and alert on many invalid blocks | none (`ConsensusError::UnsupportedUpgrade` stops a build without the rule set) | none | none | not implemented (effect: the node refuses each invalid block and continues) |
| ZIP-200-13 | Below `ACTIVATION_HEIGHT` the mempool SHOULD NOT accept a transaction that is valid only after it | `hayaid/src/mempool.rs` `Mempool::check` (`prepare` under `rules_at(tip + 1)`); `hayai-prepared/src/policy.rs` `MempoolPolicy::admit` (`PolicyReject::Epoch`) | `policy::tests::the_epoch_is_the_epoch_of_the_next_block` (`hayai-prepared/src/policy.rs`, an epoch other than the next one is refused); `sync_tests::a_chain_crosses_two_upgrades_at_the_tip_and_during_the_synchronization` (`hayaid/src/sync_tests.rs`) | none | implemented+tested |
| ZIP-200-14 | At `ACTIVATION_HEIGHT` the mempool SHOULD drop transactions that are never valid after it | `hayaid/src/node.rs` `Driver::finish_commit` (`PreparedStore::set_epoch` with the epoch of `height + 1`); after a reorg `hayaid/src/mempool.rs` `Mempool::still_prepared` | `sync_tests::a_chain_crosses_two_upgrades_at_the_tip_and_during_the_synchronization` (`hayaid/src/sync_tests.rs`: "the drop of the NU5 transaction" at the block before NU6) | none | implemented+tested |
| ZIP-200-15 | From Overwinter a signature commits to `CONSENSUS_BRANCH_ID` (two-way replay protection) | hayai-prepared sighash with the branch of the epoch (P3) | hayai-bench `conformance_txs::sighash_vector_transactions_through_draft` (ZIP 143, 243, 244 vectors) | P3 | implemented+tested |

## ZIP 201: Network Peer Management for Overwinter
Class: (b) network. Activation: Overwinter (the peer rules apply to every later upgrade).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-201-1 | Refuse a peer whose version is below `MIN_PEER_PROTO_VERSION` (170,002) | `hayai-net/src/session.rs` `PeerSession::on_message` (line 167), `INITIAL_MIN_PEER_VERSION` 170,150 (`hayai-net/src/protocol.rs:49`) | `session::tests::handshake_errors` (`hayai-net/src/session.rs`), `a_peer_below_the_minimum_protocol_version_is_refused_without_a_ban` (`hayai-net/tests/peers.rs`) | `zakura-network/src/peer/handshake.rs:931-953` | implemented+tested |
| ZIP-201-2 | Overwinter constants: `PROTOCOL_VERSION` 170,003 (Testnet) and 170,005 (Mainnet), `MIN_PEER_PROTO_VERSION` 170,002 | `hayai-net/src/protocol.rs` `min_peer_version` (line 60): every version before NU6.2 is below the initial minimum 170,150, so it has no row | `protocol::tests::minimum_peer_version_follows_the_upgrade` (`hayai-net/src/protocol.rs`) | `zakura-network/src/protocol/external/types.rs:94-96`, `constants.rs:432-437` | implemented differently: the floor 170,150 refuses every pre-NU6.2 peer, as Zakura; the verdict on a peer of each version is the verdict of Zakura |
| ZIP-201-3 | In the 1,728 blocks before an activation, evict pre-upgrade peers first (`NETWORK_UPGRADE_PEER_PREFERENCE_BLOCK_PERIOD`) | none: a full inbound set refuses a new connection (`hayai-net/src/connect.rs` `PeerManager::admit`, `Refusal::InboundFull`) | none | none found | not implemented: before an activation the node does not prefer upgraded peers; no effect on any verdict |
| ZIP-201-4 | After the activation, refuse new connections from pre-upgrade peers | `hayaid/src/node.rs` `min_peer_version_at` (line 1853), `Node::start` (line 2526); `hayai-net/src/relay.rs` `Relay::perform` (line 1305) | `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected` (`hayaid/src/node.rs`) | `zakura-network/src/peer/minimum_peer_version.rs:73-80`, `handshake.rs:931-953` | implemented+tested (fixed, F-P6-1) |
| ZIP-201-5 | After the activation, disconnect existing pre-upgrade peers | `hayai-net/src/relay.rs` `Relay::set_min_peer_version` (line 800), called by `hayaid/src/node.rs` `finish_commit` (line 1254) and `disconnect_to` (line 1565) | `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected` (`hayaid/src/node.rs`) | `zakura-network/src/peer_set/set.rs:801-807` | implemented+tested (fixed, F-P6-1) |
| ZIP-201-6 | Send `reject` with `REJECT_OBSOLETE` before the disconnect of an obsolete peer | none (`hayai-net/src/relay.rs` `Relay::on_message`, line 1274, marked) | none | none: `handshake.rs:953` returns an error and sends no reject | not implemented: the peer gets no reason; no effect on any verdict |
| ZIP-201-7 | Overwinter `CONSENSUS_BRANCH_ID` 0x5ba81b19, `ACTIVATION_HEIGHT` Testnet 207,500, Mainnet 347,500 | `hayai-consensus/src/network.rs` (upstream `zcash_protocol` heights and `BranchId::Overwinter`, P1 file) | `network::tests::activation_heights_match_zcash_protocol`, `network::tests::every_branch_id_maps_to_its_upgrade` (`hayai-consensus/src/network.rs`) | `zakura-chain/src/parameters/network_upgrade.rs:232`, `constants.rs:90` | implemented+tested |

## ZIP 202: Version 3 Transaction Format for Overwinter
Class: (a) consensus. Activation: Overwinter.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-202-1 | An Overwinter transaction has version 3, version group 0x03C48270, `fOverwintered` set | upstream `TxVersion::read` (`zcash_primitives` 0.30.1 `transaction/mod.rs:82-105`); `hayai-wire/src/scan.rs` `scan` | `hayai-wire/tests/scan.rs` `generated_transactions_of_every_version` | `zakura-chain/src/transaction/serialize.rs` (version read) | checkpoint path only |
| ZIP-202-2 | Reject: `fOverwintered` not set, version group unknown, version unknown | upstream `TxVersion::read` (unknown group: parse error); `check_version` (versions of the rule set) | `hayai-wire/tests/scan.rs` `random_bytes_never_panic`, `prepare::tests::the_rule_set_names_the_transaction_versions` | `zakura-consensus/src/transaction.rs:610` | implemented+tested |
| ZIP-202-3 | v3 validation uses the ZIP 143 signature process | `check_version` returns `Unsupported` for v3 | `prepare::tests::a_version_before_sapling_is_not_verified` | `zakura-consensus/src/transaction.rs:610` | checkpoint path only |

## ZIP 203: Transaction Expiry
Class: (a) consensus. Activation: Overwinter; NU5 change.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-203-1 | `nExpiryHeight` ≤ 499,999,999 | `hayai-prepared/src/prepare.rs` `draft` (`ExpiryTooHigh`, `TX_EXPIRY_HEIGHT_THRESHOLD`) | `prepare::tests::an_expiry_height_is_below_the_threshold` | `zakura-consensus/src/transaction/check.rs:628` | implemented+tested |
| ZIP-203-2 | A transaction with a nonzero expiry is not valid in a block above its expiry | `hayai-state/src/check.rs` `check_txs` (`Expired`) | `hayai-bench/tests/state.rs` `expiry_and_lock_time_rules` | `check.rs:652` | implemented+tested |
| ZIP-203-3 | Coinbase expiry is ignored before NU5 | `check_coinbase` (rule only when `RuleSet::coinbase.expiry_is_height`) | `hayai-bench/tests/state.rs` `coinbase_placement_and_height` | `check.rs:542` | implemented+tested |
| ZIP-203-4 | From NU5 the coinbase `nExpiryHeight` equals the block height | `hayai-state/src/check.rs` `check_coinbase` (`CoinbaseExpiry`) | `hayai-bench/tests/state.rs` `coinbase_placement_and_height` | `check.rs:572` | implemented+tested |
| ZIP-203-5 | From NU5 the 499,999,999 bound does not apply to a coinbase | `draft` applies the bound to every transaction | `prepare::tests::an_expiry_height_is_below_the_threshold` | `check.rs:542-566` | implemented differently: a coinbase from NU5 has expiry = height, and every height is below 500,000,000, so the verdict is the same |

## ZIP 204: Zcash P2P Network Protocol
Class: (b) network. Activation: none (protocol in force); the version table follows each upgrade.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-204-1 | Refuse a message whose magic is not the magic of the network (Mainnet 24e92764, Testnet fa1af9bf, Regtest aae83f5f) | `hayai-net/src/codec.rs` `FrameHeader::parse` (line 393), `Network::magic` (line 83) | `frame_header_checks` (`hayai-net/tests/codec.rs`) | `zakura-network/src/protocol/external/codec.rs:422-424` | implemented+tested |
| ZIP-204-2 | Default ports: Mainnet 8233, Testnet 18233, Regtest 18344 | `hayaid/src/config.rs` `add_listen_port` (line ~800) | `the_output_of_zakurad_generate_gives_an_exact_report` and the other report tests (`hayaid/src/config.rs`) | `zakura-network/src/config.rs` (default listen ports) | implemented+tested |
| ZIP-204-3 | DNS seed names of Mainnet and Testnet | `hayai-net/src/connect.rs` `MAINNET_SEEDERS`, `TESTNET_SEEDERS` | `a_seeder_that_fails_does_not_stop_the_others` (`hayai-net/tests/peers.rs`, mechanism only) | `zakura-network/src/config.rs:874-884` | implemented differently: the lists of Zakura (`mainnet.seeder.shieldedinfra.net` in place of `mainnet.is.yolo.money`, no `testnet.is.yolo.money`); the list is informational in the ZIP |
| ZIP-204-4 | MAY fall back to a compiled-in list of seed nodes | none | none | none | not applicable: a MAY; hayai has no compiled-in list |
| ZIP-204-5 | SHOULD disconnect a peer that sends nothing within a timeout | `hayai-net/src/session.rs` `PeerSession::on_tick` (line 299): a ping without a pong in 20 min | `session::tests::ping_keepalive_and_timeout` (`hayai-net/src/session.rs`) | `zakura-network/src/peer/connection.rs` (request timeout) | implemented+tested |
| ZIP-204-6 | SHOULD close a connection without a `version` within a time | `PeerSession::on_tick` (handshake timeout 10 s, `RelayConfig::new`) | `session::tests::handshake_errors` | `zakura-network/src/constants.rs` `HANDSHAKE_TIMEOUT` | implemented+tested |
| ZIP-204-7 | SHOULD close a connection without a `pong` within a time | `PeerSession::on_tick` (`PingTimeout`) | `session::tests::ping_keepalive_and_timeout` | `zakura-network/src/peer/connection.rs` (heartbeat) | implemented+tested |
| ZIP-204-8 | The command is printable ASCII up to the first NUL and NUL after it; a message that breaks this MUST be ignored | `hayai-net/src/codec.rs` `decode_body` (line 734): each command that is not an exact known command is `Unknown`, and the session ignores it | `unknown_commands_pass_through` (`hayai-net/tests/codec.rs`), `simulated_legacy_peer_with_the_bit_gets_zcmpctver_then_legacy_relay` (`hayai-net/tests/loopback.rs`, a `garbage` command is ignored) | `zakura-network/src/protocol/external/codec.rs:512` | implemented differently: hayai does not test the bytes; a command with bad bytes never equals a known command, so it is ignored, as the ZIP requires |
| ZIP-204-9 | Payload at most 2,097,152 bytes; refuse a larger `length` | `codec.rs` `MAX_BODY_LEN` (line 33), `FrameHeader::parse`; smaller bounds for each command (`max_body_len`) | `frame_header_checks` (`hayai-net/tests/codec.rs`) | `codec.rs:425-427` | implemented+tested |
| ZIP-204-10 | Verify the checksum after the payload; refuse a message with a bad checksum | `codec.rs` `decode` (line 567), `read_message` (line 744) | `frame_header_checks` | `codec.rs:466-470` | implemented+tested |
| ZIP-204-11 | CompactSize is canonical; refuse a message with a non-canonical CompactSize | `codec.rs` `Reader::compact_size` (line 864, upstream `zcash_encoding` 0.4.0 `CompactSize::read`), `Reader::compact_u64` (line 869) | `inv_limits_apply_before_allocation`, `addrv2_rejects_invalid_lengths` (`hayai-net/tests/codec.rs`) | `zakura-chain/src/serialization/compact_size.rs` | implemented+tested |
| ZIP-204-12 | `CAddress` has the `time` field outside `version`, not inside it | `codec.rs` `Writer::net_addr`, `decode_body` (`addr`, `version`) | `zebra_addr_v1_vectors`, `version_frame_matches_the_bitcoin_layout` (`hayai-net/tests/codec.rs`) | `zakura-network/src/protocol/external/addr/v1.rs`, `addr/in_version.rs` | implemented+tested |
| ZIP-204-13 | Service flags: `NODE_NETWORK` bit 0; bits 24 to 31 are for temporary experiments | `hayai-net/src/protocol.rs` `NODE_NETWORK` (line 83), `NODE_COMPACT_RELAY` = bit 26 (line 86) | `protocol::tests::service_bit_is_clear_of_zakura` | `zakura-network/src/protocol/external/types.rs` (`PeerServices`) | implemented+tested |
| ZIP-204-14 | The size of `inv`/`getdata` comes from a parse of each entry (36 or 68 bytes) | `codec.rs` `Reader::inv` (line 982) | `zebra_msg_wtx_vector` (`hayai-net/tests/codec.rs`) | `zakura-network/src/protocol/external/inv.rs:158-175` | implemented+tested |
| ZIP-204-15 | Refuse an inventory entry with an unknown type code | `codec.rs` `Reader::inv` | `zebra_msg_wtx_vector` (type 4 refused) | `inv.rs:158-175` (type 0 accepted) | implemented differently: type 0 (`MSG_ERROR`) is accepted, as Zakura (finding F-P6-5); every other unknown type is refused |
| ZIP-204-16 | Refuse `MSG_WTX` when the negotiated version is below 170,014 | `protocol.rs` `INITIAL_MIN_PEER_VERSION` 170,150: no peer below 170,014 completes the handshake | `session::tests::handshake_errors` | `constants.rs:432-437` | implemented differently: the minimum peer version makes the case impossible |
| ZIP-204-17 | Send no message other than `version` before the `version` of the peer | `hayai-net/src/session.rs` `PeerSession::new` (version first), `PeerSession::on_message` (a ping before the handshake gets no pong, line 192) | `session::tests::a_ping_before_the_handshake_gets_no_pong` | `zakura-network/src/peer/handshake.rs:873-887` | implemented+tested (fixed, F-P6-2) |
| ZIP-204-18 | After the own `version`, send nothing other than `verack` before the `verack` of the peer | `PeerSession::on_message`, `PeerSession::try_establish` (`zcmpctver` and `getaddr` only after the handshake) | `session::tests::a_ping_before_the_handshake_gets_no_pong`, `session::tests::peer_with_bit_gets_zcmpctver_and_upgrades` | `handshake.rs:989-1010` | implemented+tested (fixed, F-P6-2) |
| ZIP-204-19 | Disconnect a peer whose `version` fails the validation | `hayai-net/src/relay.rs` `Relay::on_message` (line 1244, `remove_peer` on each `SessionError`) | `a_peer_below_the_minimum_protocol_version_is_refused_without_a_ban` (`hayai-net/tests/peers.rs`) | `handshake.rs:917-953` | implemented+tested |
| ZIP-204-20 | The negotiated version `min(local, remote)` selects the formats of the connection | none: every format of hayai is the format of a version at or above 170,150 | none | `handshake.rs:955` | implemented differently: the minimum peer version 170,150 is above each threshold of the ZIP (31,402, 60,000, 170,014), so one format serves every connection |
| ZIP-204-21 | A `version` with the own nonce MUST close the connection | `session.rs` `PeerSession::on_message` (line 174); `relay.rs` `Relay::on_message` (nonces of every live connection) | `session::tests::handshake_errors`, `a_peer_that_sends_the_nonce_back_cannot_remove_another_address` (`hayai-net/tests/peers.rs`) | `handshake.rs:917-924` | implemented+tested |
| ZIP-204-22 | `user_agent` at most 256 bytes; SHOULD disconnect a peer with a longer one | `codec.rs` `MAX_USER_AGENT_LEN` (line 55), `decode_body`: a decode error closes the connection | `user_agent_limit` (`hayai-net/tests/codec.rs`) | `codec.rs:607` | implemented+tested |
| ZIP-204-23 | An absent `relay` field is true | `codec.rs` `decode_body` (`version`) | `version_relay_byte_is_optional_and_lenient` (`hayai-net/tests/codec.rs`) | `codec.rs:616-621` | implemented+tested |
| ZIP-204-24 | SHOULD refuse a `version` whose `relay` field is not 0 or 1 | `codec.rs` `decode_body` (marked: takes each non-zero byte as true, as zcashd) | `version_relay_byte_is_optional_and_lenient` (accepts 2) | `codec.rs:616-621` (refuses a value above 1) | implemented differently: deliberate, zcashd behaviour; a peer with the byte 2 stays connected; no effect on any block verdict |
| ZIP-204-25 | SHOULD create `version` with the `relay` field | `codec.rs` `encode_body` | `version_frame_matches_the_bitcoin_layout` | `codec.rs:276` | implemented+tested |
| ZIP-204-26 | `version` at least 170,002 (`MIN_PEER_PROTO_VERSION`) | `session.rs` `PeerSession::on_message` with 170,150 | `session::tests::handshake_errors` | `handshake.rs:931-953` | implemented+tested |
| ZIP-204-27 | On Testnet, `version` at least 170,040 | the same check with 170,150 on every network | `session::tests::handshake_errors` | `types.rs:33-48` | implemented+tested |
| ZIP-204-28 | `version` at least the version of the current epoch | `hayaid/src/node.rs` `min_peer_version_at`; `relay.rs` `Relay::set_min_peer_version` | `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected` | `types.rs:33-48`, `minimum_peer_version.rs:73-80` | implemented+tested (fixed, F-P6-1) |
| ZIP-204-29 | The nonce MUST NOT be the local nonce (validation list) | as ZIP-204-21 | as ZIP-204-21 | `handshake.rs:917-924` | implemented+tested |
| ZIP-204-30 | One `version` for each connection; a duplicate costs a penalty | `session.rs` `PeerSession::on_message` (`DuplicateVersion`, line 163); `relay.rs` gives `Misbehaviour::Malformed` (50 points) and disconnects | `session::tests::handshake_errors` | `connection.rs` (duplicate version is an unexpected message) | implemented differently: 50 points and a disconnect in place of 1 point (finding F-P6-6) |
| ZIP-204-31 | SHOULD send `reject` `REJECT_OBSOLETE` (0x11) before the disconnect for a version failure | none (`relay.rs` `Relay::on_message`, line 1274, marked) | none | none (`handshake.rs:953`) | not implemented: the peer gets no reason; no effect on any verdict |
| ZIP-204-32 | Protocol versions: 209, 31,402, 60,000, 170,002, 170,004, 170,014, 170,040, current 170,160 | `protocol.rs` `PROTOCOL_VERSION` 170,160 (line 27); the others are below the minimum peer version | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` (170,190) | implemented+tested |
| ZIP-204-33 | When an upgrade activates, disconnect each peer below the version of the epoch | `relay.rs` `Relay::set_min_peer_version`; `hayaid/src/node.rs` `finish_commit`, `disconnect_to` | `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected` | `peer_set/set.rs:801-807` | implemented+tested (fixed, F-P6-1) |
| ZIP-204-34 | SHOULD send `reject` `REJECT_OBSOLETE` before that disconnect | none | none | none | not implemented: as ZIP-204-31 |
| ZIP-204-35 | Mainnet version of each upgrade (Overwinter 170,005 to NU6.3 170,160) | `protocol.rs` `min_peer_version` (NU6.3 170,160, NU7 170,190; earlier rows below the floor 170,150) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:88-131` | implemented+tested |
| ZIP-204-36 | Testnet version of each upgrade (Overwinter 170,003 to NU6.3 170,160); Regtest uses the Testnet values | `protocol.rs` `min_peer_version` (Testnet and Regtest NU7 170,180) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:88-131` | implemented+tested |
| ZIP-204-37 | Assigned versions are 170,000 + n (n at most 999) and strictly increase on each network | constants of `protocol.rs` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:88-131` | not applicable: a rule for the assignment of versions; the constants of hayai follow the tables |
| ZIP-204-38 | From NU7, Testnet `base` = least 170,000 + 20k above the Mainnet version before, Mainnet `base + 10` | `protocol.rs` (NU7: 170,180 and 170,190) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:121-127` | not applicable: a rule for the assignment; the NU7 values of hayai are the result of the procedure |
| ZIP-204-39 | Answer a `ping` with a `pong` of the same nonce | `session.rs` `PeerSession::on_message` (line 192) | `session::tests::a_ping_before_the_handshake_gets_no_pong`, `simulated_legacy_peer_without_the_bit_never_sees_the_extension` (`hayai-net/tests/loopback.rs`) | `connection.rs:1194-1196` | implemented+tested |
| ZIP-204-40 | The nonce of a `pong` is the nonce of its `ping` | `session.rs` `PeerSession::on_message` (`Pong`, line 199) | `session::tests::ping_keepalive_and_timeout` | `connection.rs:160` | implemented+tested |
| ZIP-204-41 | MAY not support `alert` | `codec.rs` `decode_body`: `alert` is `Unknown` and ignored | `unknown_commands_pass_through` | `codec.rs:512` | implemented, no direct test |
| ZIP-204-42 | A node that supports `alert` SHOULD check its signature | none | none | none | not applicable: hayai does not support `alert` |
| ZIP-204-43 | `addr` count at most 1,000 | `codec.rs` `MAX_ADDR_ENTRIES` (line 43), `Reader::count` | `frame_header_checks` (bound of `addr`) | `codec.rs:714-716` | implemented+tested |
| ZIP-204-44 | SHOULD give 20 points for an `addr` above 1,000 | `relay.rs` (`Incoming::Malformed`: 50 points and a disconnect) | `a_malformed_frame_disconnects_and_the_second_one_bans_until_the_ban_ends` (`hayai-net/tests/peers.rs`) | `codec.rs:714` (parse error, disconnect) | implemented differently: 50 points and a disconnect (finding F-P6-6) |
| ZIP-204-45 | SHOULD send `getaddr` only once for each connection | `relay.rs` `Relay::perform` (`Established`, outbound only) | `an_outbound_peer_is_asked_for_addresses_and_its_own_getaddr_is_ignored` (`hayai-net/tests/peers.rs`) | `zakura-network/src/peer/handshake.rs` (one `getaddr` after the handshake) | implemented+tested |
| ZIP-204-46 | SHOULD process `getaddr` only from inbound peers | `relay.rs` `Relay::on_getaddr` (line 1506) | `getaddr_is_answered_once_to_an_inbound_peer`, `an_outbound_peer_is_asked_for_addresses_and_its_own_getaddr_is_ignored` | `zakurad/src/components/inbound.rs` | implemented+tested |
| ZIP-204-47 | `addrv2` port is 0 when the network has no port | none | none | none | not applicable: hayai sends no `addrv2` |
| ZIP-204-48 | `addrv2` count at most 1,000; refuse a larger message | `codec.rs` `decode_body` (`addrv2`), `Reader::count` | `addrv2_rejects_invalid_lengths` | `zakura-network/src/protocol/external/addr/v2.rs` (`TrustedPreallocate`, 1,000) | implemented+tested |
| ZIP-204-49 | `addrv2` address at most 512 bytes; refuse a longer one, whatever the network id | `codec.rs` `Reader::addr_v2` (line 884) | `addrv2_rejects_invalid_lengths` | `addr/v2.rs:281-285` | implemented+tested |
| ZIP-204-50 | Network id 0x03 (Tor v2) MUST NOT be used | `codec.rs` `Reader::addr_v2` (an unknown id is dropped); hayai sends no `addrv2` | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks` | `addr/v2.rs:298-305` | implemented+tested |
| ZIP-204-51 | Refuse an address whose length is not the length of its network id | `codec.rs` `Reader::addr_v2`: IPv4 and IPv6 only | `addrv2_rejects_invalid_lengths` (IPv4 and IPv6) | `addr/v2.rs:292-305` (IPv4 and IPv6 only) | implemented differently: a TORV3, I2P or CJDNS entry of a wrong length is dropped, not refused, as Zakura (finding F-P6-4) |
| ZIP-204-52 | MUST NOT gossip an address of an unknown network id | `codec.rs` `Reader::addr_v2` (dropped at decode) | `addrv2_vectors_keep_ipv4_and_ipv6_and_drop_other_networks` | `addr/v2.rs:298-305` | implemented+tested |
| ZIP-204-53 | MUST NOT send `addrv2` below the deployment version | `relay.rs` `Relay::on_addr`, `Relay::on_getaddr` send `addr` only | none | Zakura sends `addr` only | implemented, no direct test |
| ZIP-204-54 | `inv` count at most 50,000 | `codec.rs` `MAX_INV_ENTRIES` (line 40), `Reader::inv` | `inv_limits_apply_before_allocation` | `inv.rs:190-210` | implemented+tested |
| ZIP-204-55 | SHOULD give 20 points for an `inv` above 50,000 | `relay.rs` (`Incoming::Malformed`: 50 points and a disconnect) | `a_malformed_frame_disconnects_and_the_second_one_bans_until_the_ban_ends` | `inv.rs:190-210` (parse error, disconnect) | implemented differently: 50 points and a disconnect (finding F-P6-6) |
| ZIP-204-56 | `getdata` count at most 50,000 | `codec.rs` `Reader::inv` | `inv_limits_apply_before_allocation` (the same reader) | `inv.rs:190-210` | implemented+tested |
| ZIP-204-57 | MUST give 20 points for a `getdata` above 50,000 | `relay.rs` (`Incoming::Malformed`: 50 points and a disconnect) | `a_malformed_frame_disconnects_and_the_second_one_bans_until_the_ban_ends` | `inv.rs:190-210` (parse error, disconnect) | implemented differently: 50 points and a disconnect (finding F-P6-6) |
| ZIP-204-58 | SHOULD answer `notfound` for an object that the node does not have | `relay.rs` `Relay::serve_getdata` (line 1576) | `simulated_legacy_peer_with_the_bit_gets_zcmpctver_then_legacy_relay` (`hayai-net/tests/loopback.rs`) | `zakurad/src/components/inbound.rs` | implemented+tested |
| ZIP-204-59 | Answer `getblocks` with at most 500 hashes | `relay.rs` `Relay::deliver` (`GetBlocks`, at most 160) | `getblocks_is_answered_with_the_block_hashes_after_the_locator` (no bound case) | `zakura-state/src/constants.rs:152` | implemented, no direct test |
| ZIP-204-60 | Answer `getheaders` with at most 160 headers | `relay.rs` `Relay::deliver` (`GetHeaders`); `hayaid/src/node.rs` `ChainServe::headers_after` (line 384) | none at the bound | `zakura-state/src/constants.rs:155` | implemented, no direct test |
| ZIP-204-61 | `headers` count at most 160 | `codec.rs` `MAX_HEADERS` (line 51), `decode_body` | `frame_header_checks` (bound of `headers`) | `codec.rs` (`read_headers`) | implemented+tested |
| ZIP-204-62 | The headers of a message form a chain; SHOULD give 20 points otherwise | `hayai-sync/src/headers.rs` `Dag::connect` (`Unconnected`); `hayaid/src/sync.rs` (`Misbehaviour::UnconnectedHeaders`, 20 points) | `context_free_checks_and_batch_order` (`hayai-sync/tests/headers.rs`) | `zakura-network/src/peer/connection.rs` (headers handling) | implemented+tested |
| ZIP-204-63 | MUST NOT send Bloom filter commands to a peer without `NODE_BLOOM` | `relay.rs`: hayai never sends one | none | `codec.rs:472-478` | implemented, no direct test |
| ZIP-204-64 | SHOULD give 100 points for a Bloom filter command to a node without `NODE_BLOOM` | `relay.rs` `Relay::deliver` ignores them (marked) | `filter_messages_decode_within_bounds` (decode only) | `codec.rs:472-478` (ignored, no penalty) | not implemented: a peer that sends filter commands keeps its score; as Zakura |
| ZIP-204-65 | `filteradd` data at most 520 bytes; SHOULD give 100 points | `codec.rs` `MAX_FILTER_ADD_LEN` (line 62): decode error, 50 points and a disconnect | `filter_messages_decode_within_bounds` | `codec.rs:472-478` | implemented differently: 50 points and a disconnect |
| ZIP-204-66 | MAY use headers-first synchronization | `hayai-sync/src/headers.rs`, `hayaid/src/sync.rs` | `a_node_synchronizes_from_three_peers_from_the_genesis_block` (`hayaid/src/sync_tests.rs`) | `zakurad/src/components/sync.rs` | implemented+tested |
| ZIP-204-67 | SHOULD NOT request blocks more than 1,024 ahead of the validated tip | `hayai-sync/src/download.rs` `DownloadConfig::window_blocks` 1,024 | `hayai-sync/tests/download.rs` (window tests) | `zakurad/src/components/sync.rs:164-200` | implemented+tested |
| ZIP-204-68 | SHOULD NOT have more than 16 blocks in transit from one peer | `hayai-sync/src/download.rs` `peer_in_flight_blocks` 64 (line 166) | none at the bound | `zakurad/src/components/sync.rs` | implemented differently: up to 64 blocks for each peer; a download parameter, no effect on any verdict |
| ZIP-204-69 | MAY ask another peer after a 2 s stall | `hayai-sync/src/download.rs` (request timeouts, `LATE_REQUEST_TIMEOUTS`) | `hayai-sync/tests/download.rs` | `zakurad/src/components/sync.rs` | implemented differently: a timeout of its own measure |
| ZIP-204-70 | Blocks are announced by `inv` at once, without the trickle delay | `relay.rs` `Relay::block_validated` (line 2273): the `inv` to a legacy peer follows the validation | `a_legacy_peer_gets_no_block_before_its_validation` (`hayai-net/tests/loopback.rs`) | `zakurad/src/components/inbound.rs` | implemented differently: the announcement waits for the validation (Zakura bans the sender of an invalid block) |
| ZIP-204-71 | A transaction of version 4 or earlier MUST be announced with `MSG_TX`, of version 5 or later with `MSG_WTX` | `hayai-net/src/session.rs` `tx_inv_item` (line 328); `relay.rs` `Relay::broadcast_tx`, `Relay::on_mempool` | `session::tests::tx_inv_item_follows_zip239` | `zakura-network/src/protocol/external/inv.rs` | implemented+tested |
| ZIP-204-72 | SHOULD give a penalty for the wrong inventory type of a transaction version | none | none | none found | not implemented: no score; the transaction then fails the lookup or the parse; no effect on any verdict |
| ZIP-204-73 | SHOULD NOT send a transaction `inv` at once; SHOULD trickle at random intervals | `relay.rs` `Relay::broadcast_tx` (line 2156, marked): an `inv` at once | none | `zakurad/src/components/mempool/gossip.rs:35,150` (batches every 2 s) | implemented differently: deliberate for relay latency; no effect on any verdict |
| ZIP-204-74 | SHOULD NOT relay a transaction that expires within 3 blocks of the tip | `hayai-prepared/src/policy.rs` `MempoolPolicy::check_expiry` (admission); `hayai-net/src/relay.rs` `Relay::on_mempool` with `TxLookup::for_each_relay_id` (`hayai-prepared/src/store.rs`, `hayaid/src/mempool.rs` `PublicTxs`) | `policy::tests::expiry_boundaries`; `hayai-net/tests/loopback.rs` `the_answer_to_mempool_leaves_out_a_transaction_that_expires_soon`; `store::tests::relay_ids_leave_out_a_transaction_that_expires_soon` | none (`docs/mempool-policy.md`: Zakura has no such rule) | implemented+tested (fixed in this change, F-P6-3) |
| ZIP-204-75 | SHOULD store at most 100 orphan transactions | none: no orphan pool (`hayai-prepared/src/policy.rs` module documentation) | none | none (no orphan pool) | implemented differently: 0 orphans, which is at most 100 |
| ZIP-204-76 | SHOULD NOT relay a transaction below the minimum relay fee | `policy.rs` `MempoolPolicy::check_fee` (line 392), `min_relay_fee` (line 259) | `policy::tests::minimum_relay_fee_boundary` | `zakura-chain/src/transaction/unmined/zip317.rs:177-200` | implemented+tested |
| ZIP-204-77 | SHOULD rate-limit address records (zcashd token bucket) | `hayai-net/src/addrbook.rs` `AddrBudget::take` (line 214); `relay.rs` `Relay::on_addr` | `addrbook::tests::the_budget_of_a_connection_bounds_its_addresses` (`hayai-net/src/addrbook.rs`) | `zakura-network/src/address_book.rs` | implemented+tested |
| ZIP-204-78 | SHOULD track a misbehaviour score for each peer | `hayai-sync/src/score.rs` `ScoreBoard`; `hayai-net/src/connect.rs` `PeerManager::record` | `reported_misbehaviour_disconnects_at_the_threshold_and_decays` (`hayai-net/tests/peers.rs`) | `zakura-network/src/peer_set/set.rs` (misbehaviour) | implemented+tested |
| ZIP-204-79 | Penalty table: 1 point for a duplicate `version`, a message before the handshake, a duplicate `verack`; 20 for the size limits and unconnected headers; 100 for Bloom faults and the wrong inventory type | `hayai-sync/src/score.rs` `Misbehaviour::points` (P1 file); `relay.rs` `Relay::on_message` | `a_message_before_the_handshake_disconnects_and_the_second_one_bans` (`hayai-net/tests/peers.rs`) | `zakura-network` (disconnect on a parse error) | implemented differently: 50 points and a disconnect for each decode fault and each message before the handshake; a duplicate `verack` is ignored (finding F-P6-6) |
| ZIP-204-80 | In the 1,728 blocks before an activation, SHOULD prefer peers with the new version | none | none | none found | not implemented: as ZIP-201-3 |
| ZIP-204-81 | After an activation, MUST disconnect peers below the version of the epoch; SHOULD send `reject` first | as ZIP-204-33 and ZIP-204-34 | as ZIP-204-33 | as ZIP-204-33 | implemented+tested (fixed, F-P6-1); the `reject` is not sent |

## ZIP 205: Deployment of the Sapling Network Upgrade
Class: (a) consensus, (b) network. Activation: Sapling; the Testnet difficulty change from Testnet height 299,188.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-205-1 | `CONSENSUS_BRANCH_ID` (Sapling) = `0x76b809bb` | `network.rs` `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:233` | implemented+tested |
| ZIP-205-2 | `ACTIVATION_HEIGHT` (Sapling): Testnet 280,000, Mainnet 419,200 | `network.rs` `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:61`, `zakura-protocol` heights | implemented+tested |
| ZIP-205-3 | Sapling nodes MUST advertise at least 170,007 (Mainnet and Testnet) | `hayai-net/src/protocol.rs` `PROTOCOL_VERSION` (line 27), `RelayConfig::new` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `zakura-network/src/constants.rs:353` | implemented+tested |
| ZIP-205-3b | After Sapling, SHOULD refuse new and close existing pre-Sapling peers | as ZIP-204-33 (the floor 170,150 is above 170,007) | `session::tests::handshake_errors` | `types.rs:33-48` | implemented+tested |
| ZIP-205-4 | The minimum peer protocol version stays 170,002 | `hayai-net/src/protocol.rs` `INITIAL_MIN_PEER_VERSION` 170,150, `min_peer_version` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `zakura-network/src/constants.rs:432-437` | implemented differently (hayai and Zakura refuse peers below 170,150: no such peer follows the chain after NU6.2; no block verdict depends on it) |
| ZIP-205-5 | After Sapling activates, a node SHOULD refuse and disconnect pre-Sapling peers | `min_peer_version` (P6) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `zakura-network/src/protocol/external/types.rs:88-131` | implemented+tested |
| ZIP-205-6 | Testnet from height 299,188: a block more than 15 min after its parent MUST have `nBits` = ToCompact(PoWLimit) | `hayai-consensus/src/difficulty.rs` `expected_bits` (l. 154); `network.rs` `TESTNET.min_difficulty_start_height` (l. 504) | `testnet_minimum_difficulty_at_the_gap_boundary` (299,187 and 299,188, gap 900 s and 901 s), `testnet_minimum_difficulty_blocks_of_the_vectors` (`hayai-consensus/tests/difficulty.rs`) | `network_upgrade.rs:341,522-570` | implemented+tested |
| ZIP-205-7 | The minimum-difficulty change does not apply to Mainnet | `MAINNET.min_difficulty_start_height` = `None` | `testnet_minimum_difficulty_at_the_gap_boundary` (Mainnet part) | `network_upgrade.rs:533` | implemented+tested |

## ZIP 206: Deployment of the Blossom Network Upgrade
Class: (a) consensus, (b) network. Activation: Blossom.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-206-1 | `CONSENSUS_BRANCH_ID` (Blossom) = `0x2BB40E60` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:234` | implemented+tested |
| ZIP-206-2 | `ACTIVATION_HEIGHT` (Blossom): Testnet 584,000, Mainnet 653,600 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:63,94` | implemented+tested |
| ZIP-206-3 | Blossom nodes MUST advertise at least 170,008 (Testnet) and 170,009 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-206-3b | After Blossom, SHOULD refuse and close pre-Blossom peers | as ZIP-205-PEERS | as ZIP-205-PEERS | `types.rs:33-48` | implemented+tested |
| ZIP-206-4 | The minimum peer protocol version is 170,002 | `INITIAL_MIN_PEER_VERSION` 170,150 | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `zakura-network/src/constants.rs:432-437` | implemented differently (as ZIP-205-4) |
| ZIP-206-5 | After Blossom activates, a node SHOULD refuse and disconnect pre-Blossom peers | `min_peer_version` (P6) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:88-131` | implemented+tested |
| ZIP-206-6 | A v4 transaction across Blossom: signatures MUST use the Blossom branch id | sighash with the branch of the epoch (P3) | hayai-bench `conformance_txs::sighash_vector_transactions_through_draft` | P3 | implemented+tested |

## ZIP 207: Funding Streams
Class: (a) consensus. Activation: Canopy (revision 0), NU6 (revision 1), NU7 (revision 2, draft, deployed by ZIP 259).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-207-1 | `fs.Value(height) = floor(BlockSubsidy(height) * Numerator / Denominator)` | `hayai-consensus/src/funding.rs` `funding_streams` (325) | `funding::tests::mainnet_values_match_the_zakura_test_values`, `testnet_streams_have_their_ranges_and_values`, `the_last_testnet_streams_follow_nu7` (`hayai-consensus/src/funding.rs`); `conformance_subsidy::baselines::schedules_match_zakura_chain` | `zk-subsidy:438-470` | implemented+tested |
| ZIP-207-2 | A stream is active for `StartHeight <= height < EndHeight` | `funding.rs` `funding_streams` (`(set.start..end(set)).contains`) | the same tests (2,726,399 / 2,726,400, 3,396,000, 4,406,400) | `zk-subsidy:438-470` (`network.funding_streams(height)`) | implemented+tested |
| ZIP-207-r0-3 | Each recipient is a P2SH or a Sapling address string | `funding.rs` tables; `coinbase.rs` `address_script` (369) refuses any other address | `funding::tests::every_address_is_a_p2sh_address_of_its_network` | `zk-main:234-289` (`transparent::Address`) | implemented+tested |
| ZIP-207-r1-4 | Each recipient is a P2SH address, a Sapling address, or `DEFERRED_POOL` | `funding.rs` `Receiver::Deferred` with an empty address list | `funding::tests::the_deferred_stream_has_no_address`, `every_address_is_a_p2sh_address_of_its_network` | `zk-subsidy:84` (`is_deferred`) | implemented+tested |
| ZIP-207-5 | `AddressChangeInterval = PostBlossomHalvingInterval / 48`; `AddressPeriod`, `AddressIndex`, `Address` | `funding.rs` `PERIODS_PER_HALVING_INTERVAL` (55), `address_period` (303), `funding_streams` | `funding::tests::the_ecc_address_changes_at_each_period_boundary`, `each_range_has_the_periods_of_the_zakura_address_counts` | `zk-subsidy:342-370`, `zk-fs:18-43` | implemented+tested |
| ZIP-207-r2-6 | From NU7, `AddressPeriod = floor((R(A + I - H1) + height - A) / (R C))`, also for the period of a start height | `funding.rs` `address_period` (NU7 branch), `NU7_SPACING_RATIO` (86) | `funding::tests::an_address_period_has_three_times_the_blocks_from_nu7`; `conformance_nu7::the_testnet_funding_streams_across_nu7_match_zakura_chain` | `zk-subsidy:362-367` | implemented+tested |
| ZIP-207-r1-7 | Full nodes track `ChainValuePoolBalance^Deferred` = sum of `totalDeferredOutput` | `coinbase.rs` `CoinbaseTerms::deferred_pool_after` (296), `lockbox.rs` `deferred_pool_after` (78); `hayai-state/src/check.rs` `value_pools_after` | `coinbase::tests::the_deferred_pool_follows_the_terms`, `the_deferred_pool_of_a_chain_pays_the_disbursement` | `zakura-chain/src/value_balance.rs:30,218` | implemented+tested |
| ZIP-207-8 | Before Canopy the founders' reward rule applies | see SPEC-7.9-1 | see SPEC-7.9-1 | `zk-check:207-229` | checkpoint path only |
| ZIP-207-9 | From Canopy the founders' reward rule does not apply, also on Testnet (Canopy before the first halving) | `founders.rs` `founders_reward` (46, `upgrade_at(height) >= Canopy`) | `founders::tests::the_reward_is_a_fifth_of_the_subsidy_until_canopy` (Testnet 1,028,500 gives none) | `zk-check:207` | implemented+tested |
| ZIP-207-r0-10 | From Canopy the coinbase has at least one output for each active stream, with its value, in the prescribed way | `coinbase.rs` `CoinbaseTerms::terms` (132), `CoinbaseTerms::check` (253) | `coinbase::tests::a_missing_required_output_is_an_error`, `a_required_output_with_another_amount_is_an_error`, `a_required_output_with_another_script_is_an_error`; `conformance_subsidy::coinbases_of_the_block_vectors_pass_the_coinbase_check` (Zebra block vectors) | `zk-check:300-313` | implemented+tested |
| ZIP-207-11 | `fs.Recipient(height) = fs.Recipients[fs.RecipientIndex(height)]` | `funding.rs` `funding_streams` (`addresses[period]`) | `funding::tests::the_ecc_address_changes_at_each_period_boundary` | `zk-fs:49-57` | implemented+tested |
| ZIP-207-12 | The prescribed way to pay a P2SH address is the standard P2SH script | `coinbase.rs` `p2sh_script` (340) | `coinbase::tests::address_script_is_the_p2sh_script_of_the_address` (Mainnet block 1 script) | `zk-check:54-56` | implemented+tested |
| ZIP-207-13 | The prescribed way to pay a Sapling address: ZIP 213, zero outgoing viewing key, lead byte 0x02 | none for funding streams; the coinbase output rule is in `hayai-prepared/src/coinbase.rs` (P4) | none | `zk-fs:45-48` (transparent only) | not applicable: no stream and no disbursement has a Sapling recipient (P4 owns the ZIP 213 output rule) |
| ZIP-207-r1-14 | From NU6, the output rule applies to each active stream other than `DEFERRED_POOL` | `coinbase.rs` `CoinbaseTerms::terms` (`None => terms.subsidy.deferred += stream.value`) | `coinbase::tests::terms_have_the_outputs_of_each_era`, `from_nu6_the_value_is_exact` (the deferred part is not paid out) | `zk-check:262-271` | implemented+tested |
| ZIP-207-15 | The prescribed way to pay a Sapling or Orchard address is ZIP 213 with the post-Heartwood rules | none | none | `zk-fs:45-48` | not applicable: no shielded recipient is defined (see SPEC-7.10-1d) |
| ZIP-207-r1-16 | A payment to `DEFERRED_POOL` adds `fs.Value(height)` to the deferred pool | `coinbase.rs` `CoinbaseTerms::deferred_pool_after`, `lockbox.rs` `deferred_pool_after` | `coinbase::tests::the_deferred_pool_follows_the_terms` | `zk-check:268-271`, `value_balance.rs:218` | implemented+tested |

## ZIP 208: Shorter Block Target Spacing
Class: (a) consensus. Activation: Blossom.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-208-1 | `PreBlossomHalvingInterval` = 840,000 and `PreBlossomPoWTargetSpacing` = 150 s | `hayai-consensus/src/network.rs` `pre_blossom_halving_interval` (l. 487, 512); `lib.rs` `PRE_BLOSSOM_TARGET_SPACING` (l. 73) | `network::tests::mainnet_and_testnet_values`, `rules::tests::the_rules_of_each_upgrade` | `network_upgrade.rs:249`, `zakura-chain/src/parameters/network/subsidy.rs` | implemented+tested |
| ZIP-208-2 | `PostBlossomPoWTargetSpacing` = 75 s | `lib.rs` `POST_BLOSSOM_TARGET_SPACING` (l. 76), `rules.rs` `DifficultyParams::POST_BLOSSOM` (l. 113) | `rules::tests::the_rules_of_each_upgrade`, `difficulty::tests::timespan_bounds_of_both_spacings` | `network_upgrade.rs:252` | implemented+tested |
| ZIP-208-3 | `PoWTargetSpacing(height)` is 150 s before Blossom and 75 s from Blossom | `rules.rs` `BLOSSOM.difficulty`; `difficulty.rs` `expected_bits` reads the params of `rules_at(height)` | `the_target_spacing_changes_at_blossom` (`hayai-consensus/tests/difficulty.rs`) | `network_upgrade.rs:470-485` | implemented+tested |
| ZIP-208-4 | `PostBlossomHalvingInterval` = floor(840,000 · 2) = 1,680,000 | `network.rs` `NetworkParams::post_blossom_halving_interval` (l. 426) | `network::tests::mainnet_and_testnet_values` | `subsidy.rs` | implemented+tested |
| ZIP-208-5 | `AveragingWindowTimespan`, `MinActualTimespan`, `MaxActualTimespan`, `ActualTimespanDamped/Bounded` and `Threshold` take the height | `difficulty.rs` `averaging_window_timespan`, `bounded_timespan` (l. 227) with the params of the height | `difficulty::tests::timespan_bounds_of_both_spacings`, `the_target_spacing_changes_at_blossom` | `adjusted_difficulty.rs:237-249` | implemented+tested |
| ZIP-208-6 | `Halving(height)` with the Blossom case | `hayai-consensus/src/subsidy.rs` `halving` (l. 83, P2) | `subsidy::tests::mainnet_subsidy_follows_the_schedule`, `subsidy::tests::testnet_subsidy_follows_the_schedule` | `subsidy.rs:523-563` | implemented+tested |
| ZIP-208-7 | `BlockSubsidy(height)` with the Blossom case | `subsidy.rs` `total_subsidy` (P2) | the same two tests | `subsidy.rs:948-984` | implemented+tested |
| ZIP-208-8 | `FounderAddressAdjustedHeight` replaces the height in `FounderAddressIndex` | `hayai-consensus/src/founders.rs` `founders_reward` (l. 63-76, P2) | `founders::tests::the_address_changes_every_17709_adjusted_blocks` | none (checkpoint) | checkpoint path only (code and test exist; every founders' reward height is at or below the mandatory checkpoint) |
| ZIP-208-9 | `FoundersRewardLastBlockHeight` = last height with `Halving < 1`; no reward after it or at height 0 | `founders.rs` `founders_reward` (P2) | `founders::tests::the_reward_is_a_fifth_of_the_subsidy_until_canopy` | none (checkpoint) | checkpoint path only (as ZIP-208-8) |
| ZIP-208-10 | `PoWAveragingWindow` and `PoWMedianBlockSpan` do not change at Blossom | `rules.rs` `DifficultyParams::POST_BLOSSOM` keeps the window 17; `lib.rs` `MEDIAN_TIME_SPAN` | `rules::tests::the_rules_of_each_upgrade` | `network_upgrade.rs:277` | implemented+tested |
| ZIP-208-11 | Testnet from 299,188: a block more than `6 · PoWTargetSpacing(height)` after its parent MUST have `nBits` = ToCompact(PoWLimit) | `difficulty.rs` `expected_bits` (l. 154), `DifficultyParams::min_difficulty_gap_spacings` | `testnet_minimum_difficulty_at_the_gap_boundary` (heights before and after Blossom, gaps `6 s` and `6 s + 1`) | `network_upgrade.rs:522-570` | implemented+tested |
| ZIP-208-12 | From Testnet NU7 the threshold is 18 spacings (ZIP 218) | `rules.rs` `DifficultyParams::POST_NU7` | `the_window_and_the_spacing_change_at_nu7` (`hayai-consensus/tests/difficulty.rs`, zakura backend only) | `network_upgrade.rs:336,537` | implemented+tested |
| ZIP-208-13 | The End-of-Service halt interval SHOULD follow Blossom | none | none | none | not applicable (hayai has no End-of-Service halt) |
| ZIP-208-14 | The default expiry delta SHOULD be 40 blocks after Blossom | none | none | none | not applicable (hayai makes no transaction except the coinbase, whose expiry is its height from NU5) |
| ZIP-208-15 | A set `-txexpirydelta` SHOULD apply before and after Blossom | none | none | none | not applicable (as ZIP-208-14) |
| ZIP-208-16 | A fingerprinting mitigation SHOULD use the target spacing of the height of the work estimate | none | none | none | not applicable (hayai has no such mitigation) |

## ZIP 209: Prohibit Out-of-Range Chain Value Pool Balances
Class: (a) consensus. Activation: no network upgrade (enforced from acceptance, every height); the Orchard pool from NU5, the transparent pool, the deferred pool and the total by the ZIP 256 change, the Ironwood pool from NU6.3 (ZIP 258 change).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-209-1 | No chain value pool (Sprout, Sapling, Orchard, Ironwood, transparent, deferred) is negative after a block | `hayai-state/src/check.rs` `value_pools_after` (l. 795; `NegativeValuePool`, `NegativeTransparentPool`), deferred pool in `hayai-consensus` `CoinbaseTerms::deferred_pool_after`; called by `block_pools_after` (l. 845) on the full path, `PrebuiltBody::commit` and `check/checkpoint.rs` `checkpoint_layer` | `hayai-state` `check::tests::no_value_pool_can_be_negative` (each pool at 0 and at -1), `check::tests::the_deferred_pool_pays_the_disbursement_or_the_block_fails`; `sprout.rs` `the_sprout_pool_is_never_negative`; `ironwood.rs` `the_ironwood_pool_does_not_go_negative` | `zakura-chain/src/value_balance.rs:360-375` (`add_chain_value_pool_change`, `constrain::<NonNegative>`), called at `zakura-state/src/service/non_finalized_state/chain.rs:2572` and `finalized_state/zakura_db/chain.rs:319` | implemented+tested |
| ZIP-209-2 | The total of the pools (computed without overflow) is at most MAX_MONEY | `value_pools_after` (`checked_sum`, `ValueOverflow`; each pool also at most MAX_MONEY, as Zakura `Amount<NonNegative>`) | `check::tests::the_total_of_the_pools_is_bounded` | `value_balance.rs:371-372` | implemented+tested |
| ZIP-209-3 | Sprout pool balance = sum of `vpub_old` minus sum of `vpub_new` | `check.rs` `sprout_balance` (l. 741), `add_totals` | `check::tests::each_value_pool_follows_its_change`; `sprout.rs` `the_sprout_pool_is_never_negative` | `value_balance.rs` (`chain_value_pool_change`) | implemented+tested |
| ZIP-209-4 | Sapling, Orchard, Ironwood pool balance = negated sum of the value balance of the pool | `add_totals` (`sapling_balance`, `orchard_balance`, `ironwood_balance`), `value_pools_after` (`-balance`) | `check::tests::each_value_pool_follows_its_change`; `state.rs` `the_value_pools_follow_the_block` | `value_balance.rs` | implemented+tested |
| ZIP-209-5 | Orchard pool balance is zero before NU5; Ironwood pool balance is zero before NU6.3 | no bundle of the pool before its upgrade (`check_pools`, hayai-prepared `RuleSet::pools`) | `state.rs` `the_orchard_pool_is_off_in_the_soft_fork_range` (the pool rule) | `zakura-consensus/src/transaction.rs:484-493` | implemented, no direct test |
| ZIP-209-6 | Nodes MAY relay transactions that cannot be mined because of the rule | — | — | — | not applicable: a MAY of relay policy |
| ZIP-209-7 | Deployment: nodes SHOULD enforce the rule from acceptance, without an upgrade | the rule applies at every height, on the full path and on the checkpoint path | `checkpoint.rs` `the_checkpoint_path_keeps_the_checks_that_guard_the_state` | `value_balance.rs:360` on every committed block | implemented+tested |

## ZIP 211: Disabling Addition of New Value to the Sprout Chain Value Pool
Class: (a) consensus. Activation: Canopy.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-211-1 | From Canopy `vpub_old` of each JoinSplit is zero | `hayai-prepared/src/sprout.rs` `check_joinsplits` (`RuleSet::sprout_deposit`) | `prepare::tests::no_value_enters_the_sprout_pool_from_canopy` | `zakura-consensus/src/transaction/check.rs:304` | implemented+tested |
| ZIP-211-2 | Nodes and wallets disable the creation of Sprout deposits | no wallet; the template takes no transaction that breaks ZIP-211-1 | — | — | not applicable: wallet rule |

## ZIP 212: Allow Recipient to Derive Ephemeral Secret from Note Plaintext
Class: (a) consensus. Activation: Canopy.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-212-1 | Senders use lead byte 0x02 and derive `rcm`, `esk` from `rseed` | — | — | — | not applicable: wallet rule |
| ZIP-212-2 | Receivers accept 0x01 or 0x02 in the grace period of 32,256 blocks, then 0x02 only | — | — | — | not applicable: wallet rule (only the coinbase rule below binds a node) |
| ZIP-212-3 | Receivers check `epk = [esk] g_d` for 0x02 notes | upstream `sapling-crypto` note decryption (used by the coinbase rule) | `coinbase::tests::a_sapling_coinbase_output_decrypts_only_with_the_zero_key` | `zakura-chain/src/primitives/zcash_note_encryption.rs` | implemented+tested (coinbase outputs) |
| ZIP-212-4 | A Sapling coinbase output decrypted per ZIP 213 has lead byte 0x02 from Canopy, also in the grace period | `hayai-prepared/src/coinbase.rs` `check_shielded_outputs` (`Zip212Enforcement::On` from Canopy) | `coinbase::tests::the_sapling_lead_byte_follows_the_upgrade` | `zakura-consensus/src/transaction/check.rs:503` | implemented+tested |
| ZIP-212-5 | Orchard coinbase outputs always need lead byte 0x02 | upstream `OrchardDomain` recovery | none for a wrong lead byte | `check.rs:503` | implemented, no direct test |

## ZIP 213: Shielded Coinbase
Class: (a) consensus. Activation: Heartwood; NU5 and NU6.3 changes.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-213-1 | A coinbase has no JoinSplit and no Spend description | `hayai-prepared/src/prepare.rs` `draft` (`CoinbaseJoinSplit`, `CoinbaseShieldedSpend`) | `prepare::tests::a_coinbase_has_no_joinsplit`; Sapling spend: none (bd hayai-cvi) | `check.rs:251` | implemented+tested |
| ZIP-213-2 | Pre-Heartwood: a coinbase has no Output description | `coinbase.rs` `check_shielded_outputs` (`ShieldedOutputBeforeHeartwood`) | `coinbase::tests::a_coinbase_has_no_shielded_output_before_heartwood` | `check.rs:503` | implemented+tested (Sapling, Blossom below the mandatory checkpoint: checkpoint path only) |
| ZIP-213-3 | The `valueBalance`, output and `bindingSig` rules of other transactions apply to a coinbase | the same verification path (`ScopedBatch::add` for every transaction) | `hayai-bench/tests/ironwood.rs` `the_coinbase_value_counts_the_ironwood_output` | `zakura-consensus/src/transaction.rs:1344` | implemented+tested |
| ZIP-213-4 | The coinbase-spend rules apply to transparent coinbase outputs only | `hayai-state/src/check.rs` `check_txs` (coins of the coinbase are transparent outputs) | `hayai-bench/tests/state.rs` `mature_coinbase_spent_to_shielded_outputs_is_accepted` | `check.rs:675` | implemented+tested |
| ZIP-213-5 | Every shielded coinbase output decrypts with the all-zero OVK and gives a valid note commitment | `coinbase.rs` `check_shielded_outputs` (upstream `try_output_recovery_with_ovk`) | `coinbase::tests::a_sapling_coinbase_output_decrypts_only_with_the_zero_key`, `an_orchard_coinbase_output_decrypts_only_with_the_zero_key`, `an_ironwood_coinbase_output_decrypts_only_with_the_zero_key`, `draft_checks_the_shielded_outputs_of_a_coinbase` | `check.rs:503` | implemented+tested |
| ZIP-213-6 | NU6.3 on: a coinbase has no Orchard-pool component | `draft` (`CoinbaseOrchardBundle`) | `prepare::tests::the_coinbase_rules_of_the_orchard_and_ironwood_bundles` | `check.rs:367` | implemented+tested |

## ZIP 214: Consensus rules for a Zcash Development Fund
Class: (a) consensus. Activation: Canopy (r0), NU6 (r1), NU6.1 (r2), NU7 (r3, draft, deployed by ZIP 259).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-214-1 | The Mainnet Canopy height is 1,046,400 | `hayai-consensus/src/network.rs` (heights of upstream `zcash_protocol`, P1) | `network::tests::activation_heights_match_zcash_protocol`; `funding::tests::the_ranges_are_in_order_and_start_at_the_upgrades` | `zakura-chain/src/parameters/constants.rs:98` | implemented+tested |
| ZIP-214-2 | Revision 0 of ZIP 207 activates at Canopy | `funding.rs` `funding_streams` (Canopy gate) | `funding::tests::testnet_streams_have_their_ranges_and_values` (1,028,499 none, 1,028,500 three streams) | `zk-subsidy:451` | implemented+tested |
| ZIP-214-3 | The Mainnet NU6 height is 2,726,400 | `network.rs` (upstream) | `network::tests::activation_heights_match_zcash_protocol`, `the_ranges_are_in_order_and_start_at_the_upgrades` | `constants.rs:102` | implemented+tested |
| ZIP-214-4 | Revision 1 of ZIP 207 activates at NU6 | `funding.rs` `MAINNET[1]`, `TESTNET[1]`; `coinbase.rs` `CoinbaseTerms::terms` | `coinbase::tests::terms_have_the_outputs_of_each_era` | `zk-main:254-267` | implemented+tested |
| ZIP-214-r3-5 | The NU7 height of each network is a multiple of 3 | `network.rs` `TESTNET_NU7_HEIGHT` (P1 mark exists) | `funding::tests::the_last_testnet_streams_follow_nu7` (new assertion `nu7 % 3 == 0`) | `constants.rs:80` | implemented+tested |
| ZIP-214-6 | A stream includes its start height and excludes its end height | `funding.rs` `funding_streams` | `funding::tests::mainnet_values_match_the_zakura_test_values` (2,726,399, 4,406,400) | `zk-subsidy:119` (`height_range`) | implemented+tested |
| ZIP-214-r0-7 | Mainnet `FS_ZIP214_BP` 7, `ZF` 5, `MG` 8 of 100, 1,046,400 to 2,726,400 | `funding.rs` `MAINNET[0]` (133) | `funding::tests::mainnet_values_match_the_zakura_test_values`; `conformance_subsidy::baselines::schedules_match_zakura_chain`, `schedules_match_zebra_chain` | `zk-main:235-252` | implemented+tested |
| ZIP-214-r0-8 | Testnet r0 streams 7, 5, 8 of 100, 1,028,500 to 2,796,000 | `funding.rs` `TESTNET[0]` (175) | `funding::tests::testnet_streams_have_their_ranges_and_values`; the two `baselines` tests | `zk-test:213-231` | implemented+tested |
| ZIP-214-r1-9 | Mainnet `FS_FPF_ZCG` 8 and `FS_DEFERRED` 12 of 100, 2,726,400 to 3,146,400 | `funding.rs` `MAINNET[1]`, `lockbox_streams` (114) | `funding::tests::mainnet_values_match_the_zakura_test_values`; `subsidy::tests::the_deferred_part_is_the_lockbox_stream` | `zk-main:254-267` | implemented+tested |
| ZIP-214-r1-10 | Testnet r1 streams, 2,976,000 to 3,396,000 | `funding.rs` `TESTNET[1]` | `funding::tests::testnet_streams_have_their_ranges_and_values`; `subsidy::tests::the_deferred_part_is_the_lockbox_stream` | `zk-test:232-246` | implemented+tested |
| ZIP-214-r2-11 | Mainnet `FS_FPF_ZCG_H3` 8 and `FS_CCF_H3` 12 of 100, 3,146,400 to 4,406,400 | `funding.rs` `MAINNET[2]` | `funding::tests::mainnet_values_match_the_zakura_test_values` | `zk-main:269-288` | implemented+tested |
| ZIP-214-r2-12 | Testnet r2 streams, 3,536,500 to 4,476,000 | `funding.rs` `TESTNET[2]` | `funding::tests::testnet_streams_have_their_ranges_and_values` | `zk-test:247-261` | implemented+tested |
| ZIP-214-r3-13 | The r2 end moves to `A + 3 (H3 - A)` (Testnet 4,497,948); an NU7 height at or after `H3` does not reactivate a stream | `funding.rs` `nu7_adjusted_end` (94) | `funding::tests::the_last_testnet_streams_follow_nu7`, `only_an_end_above_the_nu7_height_moves`; `conformance_nu7::the_testnet_funding_streams_across_nu7_match_zakura_chain` | `zk-subsidy:388-402`, `zakura-chain/src/parameters/network/testnet.rs:1167-1183` | implemented+tested |
| ZIP-214-r3-14 | A post-NU7 value is the floor of the post-NU7 subsidy share: 4,166,666 and 6,249,999 | `funding.rs` `funding_streams` | `funding::tests::the_last_testnet_streams_follow_nu7` | `zk-subsidy:438-470` | implemented+tested |
| ZIP-214-r3-15 | `FS_FPF_ZCG_H3` uses the NU7 `AddressPeriod` of ZIP 207 r2 | `funding.rs` `address_period` | `funding::tests::an_address_period_has_three_times_the_blocks_from_nu7` | `zk-subsidy:342-370` | implemented+tested |
| ZIP-214-r0-16 | ECC and ZF generate the address sequences | none | none | none | not applicable: process rule |
| ZIP-214-r0-17 | Each party takes account of key security (SHOULD) | none | none | none | not applicable: process rule |
| ZIP-214-r0-18 | Mainnet stream funds follow ZIP 1014 | none | none | none | not applicable: process rule |
| ZIP-214-r0-19 | Mainnet r0 lists: 48 BP addresses; ZF and MG one address 48 times | `funding.rs` `MAINNET_ECC_ADDRESSES` (376), `MAINNET[0]` | `funding::tests::the_ecc_address_changes_at_each_period_boundary`, `each_range_has_the_periods_of_the_zakura_address_counts`; the two `baselines` tests | `zk-main:58-107` | implemented+tested |
| ZIP-214-r1-20 | Mainnet r1: `FS_FPF_ZCG` t3cFfPt1… 12 times; `FS_DEFERRED` is `DEFERRED_POOL` | `funding.rs` `MAINNET[1]` | `funding::tests::the_deferred_stream_has_no_address`; the two `baselines` tests | `zk-main:180-182,254-267` | implemented+tested |
| ZIP-214-r2-21 | Mainnet r2: `FS_FPF_ZCG_H3` t3cFfPt1… 36 times; `FS_CCF_H3` is `DEFERRED_POOL` | `funding.rs` `MAINNET[2]` | `funding::tests::the_deferred_stream_has_no_address` (3,146,400); the two `baselines` tests | `zk-main:192-194,269-288` | implemented+tested |
| ZIP-214-r0-22 | Testnet r0 lists: 51 BP addresses; ZF and MG one address 51 times | `funding.rs` `TESTNET_ECC_ADDRESSES` (431), `TESTNET[0]` | `funding::tests::the_ecc_address_changes_at_each_period_boundary`; the two `baselines` tests | `zk-test:57-109` | implemented+tested |
| ZIP-214-r1-23 | Testnet r1: `FS_FPF_ZCG` t2HifwjU… 13 times | `funding.rs` `TESTNET[1]` | `funding::tests::each_range_has_the_periods_of_the_zakura_address_counts` (13); the two `baselines` tests | `zk-test:192-194` | implemented+tested |
| ZIP-214-r2-24 | Testnet r2: `FS_FPF_ZCG_H3` t2HifwjU… 27 times | `funding.rs` `TESTNET[2]` | `funding::tests::the_last_testnet_streams_follow_nu7` (address across NU7) | `zk-test:247-261` | implemented+tested |
| ZIP-214-r3-25 | The r2 lists stay valid under r3: the last block selects index 35 (Mainnet) and 26 (Testnet) | `funding.rs` `address_period` | `funding::tests::each_range_has_the_periods_of_the_zakura_address_counts` (36 and 27 periods, Testnet with the NU7 period) | `zk-subsidy:338-340` | implemented+tested |
| ZIP-214-r0-26 | Direct-grant option of ZIP 1014 | none | none | none | not applicable: the option was never used (ZIP 214) |

## ZIP 215: Explicitly Defining and Modifying Ed25519 Validation Rules
Class: (a) consensus. Activation: Canopy.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-215-1 | `A` and `R` are encodings of points of Ed25519 (non-canonical encodings allowed) | `hayai-prepared/src/sprout.rs` `verify_sprout` (`ed25519-zebra` `VerificationKey::try_from`, `verify`) | `hayai-bench/tests/sprout.rs` `the_embedded_key_verifies_the_published_joinsplits` | `zakura-consensus/src/transaction.rs:1275` (`ed25519-zebra`) | implemented, no direct test of the ZIP 215 edge cases |
| ZIP-215-2 | `S` < ℓ | `ed25519-zebra` `verify` | none | `transaction.rs:1275` | implemented, no direct test |
| ZIP-215-3 | The cofactored equation [8][S]B = [8]R + [8][k]A; the cofactorless equation is not used | `ed25519-zebra` `verify` | none | `transaction.rs:1275` | implemented, no direct test |
| ZIP-215-4 | The rules apply from Canopy | `ed25519-zebra` at every height | — | the same | implemented differently: ZIP 215 at every height, as Zebra and Zakura; no block before Canopy has full validation (mandatory checkpoint) |

## ZIP 216: Require Canonical Jubjub Point Encodings
Class: (a) consensus. Activation: NU5 (retroactive on Mainnet and Testnet).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-216-1 | Reject a non-canonical `R` of `spendAuthSig` | upstream `sapling-crypto` 0.7.0 `BatchValidator` (`redjubjub`) | none | `zakura-consensus/src/transaction/check.rs:209` | implemented, no direct test |
| ZIP-216-2 | Reject a non-canonical `R` of `bindingSigSapling` | the same | none | `check.rs:209` | implemented, no direct test |
| ZIP-216-3 | Reject a non-canonical `pk*_d`, or the zero point, in the `C^out` plaintext (coinbase outputs) | upstream `sapling-crypto` OVK recovery | none | `zcash_note_encryption.rs` | implemented, no direct test |
| ZIP-216-4 | `cv`, `rk` (and `epk`) encodings are canonical | upstream parser (`ValueCommitment::from_bytes_not_small_order`, `jubjub` decoding) | none | `check.rs:209` | implemented, no direct test |

## ZIP 218: 25-second Block Target Spacing
Class: (a) consensus. Activation: NU7 (Testnet 4,465,026; Mainnet none). The NU7 rule set exists on the zakura backend only; the upstream backend stops at NU7 with `ConsensusError::UnsupportedUpgrade`.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-218-1 | `PostNU7PoWTargetSpacing` = 25 s | `lib.rs` `POST_NU7_TARGET_SPACING` (l. 79); `rules.rs` `DifficultyParams::POST_NU7` (l. 120) | `rules::tests::the_nu7_rule_set_changes_these_rules` (both backends) | `network_upgrade.rs:257` | implemented+tested |
| ZIP-218-2 | `NU7ActivationHeight` comes from the deployment ZIP | `network.rs` `TESTNET_NU7_HEIGHT` (l. 639) | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:80` | implemented+tested |
| ZIP-218-3 | `NU7PoWTargetSpacingRatio` = 75 / 25 = 3 | `subsidy.rs` `SPACING_ERAS` (P2); `funding.rs` `address_period` (P2) | hayai-bench `conformance_nu7::the_schedule_across_nu7_matches_zakura_chain` | `network_upgrade.rs:264` | implemented+tested |
| ZIP-218-4 | `PoWTargetSpacing(height)`: 150 s, 75 s from Blossom, 25 s from NU7 | `rules.rs` rule sets, `difficulty.rs` `expected_bits` | `rules::tests::the_rules_of_each_upgrade`, `the_window_and_the_spacing_change_at_nu7` | `network_upgrade.rs:470-515` | implemented+tested |
| ZIP-218-5 | `PostNU7PoWAveragingWindow` = 102; `PoWAveragingWindow(height)` is 17 before NU7 and 102 from NU7 | `DifficultyParams::POST_NU7`; `lib.rs` `DIFFICULTY_CONTEXT_BLOCKS` = 113 | `rules::tests::the_nu7_rule_set_changes_these_rules`, `the_window_and_the_spacing_change_at_nu7`, `generated_chains_across_nu7_match_the_reference` | `network_upgrade.rs:277-292,595` | implemented+tested |
| ZIP-218-6 | Every use of `PoWAveragingWindow` in §7.7.3 (`MeanTarget`, `ActualTimespan`, the `height <= W` case) takes the window of the height | `difficulty.rs` `expected_bits` (l. 133-197) | `the_window_and_the_spacing_change_at_nu7`; hayai-bench `conformance_nu7::the_expected_bits_across_nu7_match_zakura_header_chain` | `adjusted_difficulty.rs:188-252` | implemented+tested |
| ZIP-218-7 | `PostNU7HalvingInterval` = 5,040,000 | `subsidy.rs` `halving` (block seconds, P2) | `conformance_nu7::the_schedule_across_nu7_matches_zakura_chain` | `subsidy.rs:523-563` | implemented differently (hayai counts block seconds over the pre-Blossom interval in seconds, as Zakura; the index equals the three-case formula at every height, which the conformance test checks) |
| ZIP-218-8 | `Halving(height)` with the NU7 case | `subsidy.rs` `halving` (P2) | the same test | `subsidy.rs:523-563` | implemented+tested |
| ZIP-218-9 | `BlockSubsidy(height)` from NU7 = floor(MaxBlockSubsidy / (2 · 3 · 2^Halving)) | `subsidy.rs` `total_subsidy` (P2) | `conformance_nu7::the_schedule_across_nu7_matches_zakura_chain`, `subsidy::tests::testnet_subsidy_follows_the_schedule` | `subsidy.rs:948-984` | implemented+tested |
| ZIP-218-10 | Constants: `GlobalShieldedBudget` 330, `OrchardProtocolBlockActionLimit` 330, `SaplingBlockIOLimit` 300, `SproutBlockJoinSplitLimit` 0 | `hayai-consensus/src/limits.rs` `BlockLimits::NU7` (l. 34) | `rules::tests::the_nu7_rule_set_changes_these_rules`; hayai-bench `conformance_nu7::the_limits_and_the_difficulty_parameters_match_zakura_chain` | `network_upgrade.rs:301-327` | implemented+tested |
| ZIP-218-11 | From NU7, the Orchard actions of a block MUST NOT exceed 330 | `hayai-state/src/check.rs` `add_totals` (l. 696, P5) | hayai-bench `state::block_totals` (`TooManyOrchardActions`) | `zakura-consensus/src/block/check.rs:451-503` | implemented+tested |
| ZIP-218-12 | From NU7, the Ironwood actions of a block MUST NOT exceed 330 | `add_totals` (P5) | hayai-bench `ironwood::ironwood_actions_count_for_the_block_limit` | `check.rs:451-503` | implemented+tested |
| ZIP-218-13 | From NU7, the Sapling spends plus outputs of a block MUST NOT exceed 300 | `add_totals` (P5, `TooManySaplingIos`) | none (no test reaches `TooManySaplingIos`) | `check.rs:451-503` | implemented, no direct test |
| ZIP-218-14 | From NU7, the JoinSplits of a block MUST NOT exceed 0 | `rules.rs` `nu7` (l. 300): `pools.sprout = false`; hayai-prepared and hayai-state `check_pools`; ZIP 2003 refuses v4 | `rules::tests::the_nu7_rule_set_changes_these_rules`; hayai-prepared `prepare::tests::a_v4_transaction_is_refused_from_nu7` | `check.rs:488-493` | implemented differently (no JoinSplit count: the pool rule refuses each transaction with a JoinSplit, so the verdict on a block with a JoinSplit is the same refusal) |
| ZIP-218-15 | From NU7, Orchard + Ironwood actions + Sapling spends and outputs + 2 · JoinSplits MUST NOT exceed 330 | `add_totals` (P5): the JoinSplit term is 0 because ZIP-218-14 refuses each JoinSplit | hayai-bench `ironwood::ironwood_actions_count_for_the_block_limit` (`ShieldedCostAboveBudget`) | `check.rs:451-503` | implemented+tested |
| ZIP-218-16 | The limits do not apply to the transparent parts; the 2 MB block limit stays | `hayai-wire/src/lib.rs` `MAX_BLOCK_BYTES` (P3) unchanged | hayai-wire `tests::oversized_block_is_rejected_before_parsing` | `zakura-chain/src/block/serialize.rs:24` | implemented+tested |
| ZIP-218-17 | `PoWMedianBlockSpan` does not change at NU7 | `lib.rs` `MEDIAN_TIME_SPAN` (l. 82) | `rules::tests::the_rules_of_each_upgrade` (`DIFFICULTY_CONTEXT_BLOCKS` = 102 + 11) | `contextual/constants.rs:9` | implemented+tested |
| ZIP-218-18 | Testnet from NU7: a block more than 18 · 25 s = 450 s after its parent (451 s qualifies, 450 s does not) MUST have `nBits` = ToCompact(PoWLimit) | `difficulty.rs` `expected_bits` (l. 154), `DifficultyParams::POST_NU7.min_difficulty_gap_spacings` | `the_window_and_the_spacing_change_at_nu7` (zakura backend; gaps 450, 451, 151) | `network_upgrade.rs:336,522-570` | implemented+tested |
| ZIP-218-19 | The default expiry delta SHOULD become 120 blocks after NU7 | none | none | none | not applicable (hayai makes no transaction except the coinbase) |
| ZIP-218-20 | A set `-txexpirydelta` SHOULD apply before and after NU7 | none | none | none | not applicable (as ZIP-218-19) |
| ZIP-218-21 | `COINBASE_MATURITY` SHOULD stay 100 | `lib.rs` `COINBASE_MATURITY` (l. 59) | hayai-bench `state::immature_coinbase_spend_is_rejected` | `zakura-chain/src/transparent.rs:55` | implemented+tested |
| ZIP-218-22 | `MAX_REORG_LENGTH` SHOULD become 600 | `lib.rs` `FINALITY_DEPTH` 1,000 (l. 65), `hayai-sync/src/headers.rs` `finalized_height` (l. 435) | hayai-sync `branches_below_the_finalized_height_are_removed_and_refused` (`hayai-sync/tests/headers.rs`) | `zakura-chain/src/parameters/constants.rs:30` (1,000) | implemented differently (1,000 blocks at every height, as Zakura; not a block rule) |
| ZIP-218-23 | `TX_EXPIRING_SOON_THRESHOLD` SHOULD stay 3 | `hayai-prepared/src/policy.rs` `TX_EXPIRING_SOON_THRESHOLD` (P6) | hayai-bench `mempool::a_real_transaction_meets_the_context_rules_at_their_boundaries` | none | implemented+tested |
| ZIP-218-24 | `MAX_BLOCKS_IN_TRANSIT_PER_PEER` SHOULD become 48 | `hayai-sync/src/download.rs` `DownloadConfig::default` (`peer_in_flight_blocks` 64, `peer_in_flight_bytes` 8 MB) | none | `zakurad/src/components/sync.rs:165-200` | implemented differently (one value at every height; a bound in bytes too; no block verdict depends on it) |
| ZIP-218-25 | `BLOCK_DOWNLOAD_WINDOW` SHOULD become 3,072 | `DownloadConfig::default` (`window_blocks` 1,024, `memory_budget_bytes` 1 GiB) | none | `zakurad/src/components/sync.rs:165-200` | implemented differently (as ZIP-218-24) |
| ZIP-218-26 | `MIN_BLOCKS_TO_KEEP` SHOULD become 864 | none | none | none | not applicable (hayai does not prune blocks) |
| ZIP-218-27 | `NETWORK_UPGRADE_PEER_PREFERENCE_BLOCK_PERIOD` SHOULD stay 1,728 | none (P6) | none | none | not implemented (effect: no preference for upgraded peers before an activation; peer selection only) |
| ZIP-218-28 | The recommended anchor depth SHOULD stay 3 blocks | none | none | none | not applicable (wallet rule) |

## ZIP 221: FlyClient - Consensus-Layer Changes (with the NU6.3 change of ZIP 258)
Class: (a) consensus. Activation: Heartwood (Mainnet 903,000, Testnet 903,800); Orchard fields from NU5; Ironwood fields from NU6.3 (ZIP 258).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-221-1 | The root of the MMR of the blocks from the last activation to the parent MUST be in the header of the block | `hayai-state/src/history.rs` `header_commitment` (l. 153); `check.rs` `check_history` (l. 934, `BlockCommitments`) | `hayai-state/tests/history.rs` `mainnet_canopy_activation_resets_the_tree`; `ironwood.rs` `the_header_commits_to_the_history_tree_of_version_3` (refused cases); `validate.rs` `tampered_blocks_fail_at_the_right_stage` | `zakura-state/src/service/check.rs:265-345` | implemented+tested (shadow seed: not checked, F-P5-1) |
| ZIP-221-2 | The tree holds the blocks from the preceding upgrade activation (height 0 counts as one); a new tree starts at each activation | `history.rs` `HistoryState::append` (l. 382, `fresh`), `history_after` (l. 409) | `tests/history.rs` `appends_check_heights_and_start_a_new_tree_per_upgrade`, `mainnet_canopy_activation_resets_the_tree` | `zakura-chain/src/history_tree.rs:313-321`, `:601-656` | implemented+tested |
| ZIP-221-3 | Node hashes use BLAKE2b-256 with personalization `ZcashHistory` plus the branch id of the epoch of the block | `history.rs` `leaf_v1` (`consensus_branch_id`), upstream `zcash_history` | `tests/history.rs` `zip_0221_v1_vectors`, `zip_0221_v2_vectors`, `zip_0221_v3_vectors` (zcash-test-vectors `zip_0221.py`) | `history_tree.rs` (upstream `zcash_history`) | implemented+tested |
| ZIP-221-4 | Field 1 `hashSubtreeCommitment`: leaf = block hash (no personalization); inner = hash of both serialized children | `leaf_v1` (`subtree_commitment: leaf.hash`), upstream combine | the three vector tests; `mainnet_heartwood_activation` (block 903,001 header) | `history_tree.rs` | implemented+tested |
| ZIP-221-5 | Fields 2, 3: `nEarliestTimestamp` / `nLatestTimestamp` (leaf: header time; inherit left / right) | `leaf_v1` (`start_time`, `end_time`) | the vector tests | `history_tree.rs` | implemented+tested |
| ZIP-221-6 | Fields 4, 5: `nEarliestTargetBits` / `nLatestTargetBits` | `leaf_v1` (`start_target`, `end_target`) | the vector tests | `history_tree.rs` | implemented+tested |
| ZIP-221-7 | Fields 6, 7: earliest / latest Sapling root = final Sapling root of the block | `leaf_v1`; `HistoryLeaf::from_block` (l. 92) takes the roots after the block | the vector tests; `mainnet_heartwood_activation` (a wrong Sapling root gives another root) | `history_tree.rs` | implemented+tested |
| ZIP-221-8 | Field 8: `nSubTreeTotalWork` = floor(2^256 / (ToTarget(nBits) + 1)); inner = sum modulo 2^256 | `history.rs` `block_work` (l. 130) over `hayai-consensus` `difficulty::block_work`; sum in upstream | the vector tests (the `work` of each leaf); `history::tests::work_of_known_targets` | `history_tree.rs` | implemented+tested |
| ZIP-221-9 | Fields 9, 10: `nEarliestHeight` / `nLatestHeight` | `leaf_v1` (`start_height`, `end_height`) | the vector tests; `appends_check_heights_and_start_a_new_tree_per_upgrade` (a gap in heights is `HistoryError::Height`) | `history_tree.rs:300-310` | implemented+tested |
| ZIP-221-10 | Field 11: `nSaplingTxCount` = transactions with Sapling spends or outputs | `HistoryLeaf::from_block` | the vector tests (count as input); no test of the count from a block | `history_tree.rs` (`HistoryTreeBlockParts::from_block`) | implemented, no direct test |
| ZIP-221-11 | [NU5 onward] fields 12, 13: earliest / latest Orchard root | `leaf_v2` (l. 228) | `zip_0221_v2_vectors` | `history_tree.rs` (`OrchardOnward`) | implemented+tested |
| ZIP-221-12 | [NU5 onward] field 14: `nOrchardTxCount` = transactions with Orchard actions | `HistoryLeaf::from_block`, `leaf_v2` | `zip_0221_v2_vectors` (count as input) | `history_tree.rs` | implemented, no direct test |
| ZIP-221-13 | NU5 fields absent before NU5; node size 147-171 bytes (212-244 from NU5) | `tree_version` by `RuleSet::history` (V1 Heartwood, Canopy; V2 NU5 to NU6.2) | `zip_0221_v1_vectors`, `zip_0221_v2_vectors` (serialized peaks) | `history_tree.rs:142-162` | implemented+tested |
| ZIP-221-14 | `hashChainHistoryRoot` = BLAKE2b-256 of the serialized root node (peaks bagged left to right) | `append_v` (`V::hash(tree.root_node())`) | the vector tests (`hash_chain_history_root`) | `history_tree.rs` (`hash`) | implemented+tested |
| ZIP-221-15 | Append: merge peaks right to left, then bag | upstream `Tree::append_leaf`; `peak_positions` | `history::tests::peak_positions_match_the_binary_decomposition`; vector tests (peaks) | upstream `zcash_history` | implemented+tested |
| ZIP-221-16 | Reorg: delete the rightmost leaves | each layer holds its own `HistoryState`; a pop returns to the state of the parent layer | `hayai-state` `tests::pop_restores_the_previous_view` (layer state) | `zakura-state` non-finalized chain fork | implemented differently: no delete; the state of the parent is kept, so the tree after a reorg is the same |
| ZIP-221-17 | [Sapling until Heartwood] `hashLightClientRoot` = final Sapling root of the block | `header_commitment` (Sapling, Blossom) | `tests/history.rs` `mainnet_heartwood_activation` (expected value) | `zakura-chain/src/block/commitment.rs:117-121` (parse only; `check.rs:282-296` does not compare) | implemented differently: hayai compares the root, Zakura only parses it; every such block is at or below the mandatory checkpoint, so the verdicts are the same |
| ZIP-221-18 | The Heartwood activation block has all zero bytes; they are not a root hash | `header_commitment` with the empty pre-Heartwood state (root all zeros); the layer computes its own tree | `mainnet_heartwood_activation` | `commitment.rs:122-140` | implemented+tested |
| ZIP-221-19 | Later blocks (Heartwood, Canopy): the field = `hashChainHistoryRoot` of the parent tree; from NU5 inside `hashBlockCommitments` (ZIP 244) | `header_commitment` | `mainnet_canopy_activation_resets_the_tree` (block 1,046,401 header), `nu5_and_later_commit_to_the_root_and_the_auth_data_root` | `zakura-state/src/service/check.rs:300-345` | implemented+tested |
| ZIP-221-20 | The header byte format and version do not change | `hayai-wire` header parser (P1) | — | — | not applicable: no rule to check |
| ZIP-221-21 | Deployment: Heartwood on Mainnet and Testnet; Orchard fields from NU5 | `hayai-consensus` activation heights (P1), `RuleSet::history` | `rules::tests` (P1) | `zakura-chain/src/parameters/network_upgrade.rs` | implemented+tested |
| ZIP-221-22 | ZIP 258: [NU6.3 onward] fields 15, 16: earliest / latest Ironwood root | `history.rs` `LeafVersion for TreeV3` (l. 245) | `zip_0221_v3_vectors` (and the Ironwood root changes the root) | `history_tree.rs:142` (`IronwoodOnward`) | implemented+tested |
| ZIP-221-23 | ZIP 258: [NU6.3 onward] field 17: `nIronwoodTxCount` | `HistoryLeaf::from_block`, `TreeV3` leaf | `zip_0221_v3_vectors` (another count gives another root) | `history_tree.rs` | implemented+tested |
| ZIP-221-24 | ZIP 258: NU6.3 fields absent before NU6.3; node size 277-317 bytes | `tree_version` (V3 from NU6.3, NU7 keeps V3) | `appends_check_heights_and_start_a_new_tree_per_upgrade` (V3 leaf 65 bytes longer) | `history_tree.rs:142,241` | implemented+tested |

## ZIP 224: Orchard Shielded Protocol
Class: (a) consensus. Activation: NU5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-224-1 | Orchard is implemented as the protocol specification says | the §4.6 and §7.5 rows below | — | — | not applicable: a pointer to the §4.6 and §7.5 rows |

## ZIP 225: Version 5 Transaction Format
Class: (a) consensus. Activation: NU5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-225-1 | v5 layout: header, branch id, lock time, expiry, transparent, Sapling, Orchard components | upstream `Transaction::read_v5`; `hayai-wire/src/scan.rs` `scan_v5_v6` | `hayai-wire/tests/scan.rs` `upstream_transaction_vectors`, `mainnet_block_transactions` | `zakura-chain/src/transaction/serialize.rs` | implemented+tested |
| ZIP-225-2 | `valueBalanceSapling`, `bindingSigSapling` present if and only if spends + outputs > 0; `anchorSapling` if and only if spends > 0 | upstream `read_v5_bundle`; `scan.rs` `sapling_v5` | `scan.rs` `generated_transactions_of_every_version` | `serialize.rs` | implemented+tested |
| ZIP-225-3 | Orchard fields present if and only if `nActionsOrchard > 0` | upstream `read_v5_bundle`; `scan.rs` `orchard_bundle` | `scan.rs` `generated_transactions_of_every_version` | `serialize.rs` | implemented+tested |
| ZIP-225-4 | Spend proofs and signatures, output proofs, Orchard proofs and signatures in the order of their descriptions | upstream parser (one proof per index) | `scan.rs` `upstream_transaction_vectors` | `serialize.rs` | implemented, no direct test |
| ZIP-225-5 | A coinbase has `enableSpendsOrchard = 0` | `hayai-prepared/src/prepare.rs` `draft` (`CoinbaseShieldedSpend`) | `prepare::tests::the_coinbase_rules_of_the_orchard_and_ironwood_bundles` | `check.rs:251` | implemented+tested |

## ZIP 229: Version 6 Transaction Format
Class: (a) consensus. Activation: NU6.3.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-229-1 | v6 header: `fOverwintered` set, version 6, group 0xD884B698 | upstream `TxVersion::read`; `scan.rs` `scan` | `scan.rs` `generated_transactions_of_every_version` | `zakura-chain/src/transaction/serialize.rs` | implemented+tested |
| ZIP-229-2 | `flagsOrchard`: bits 3..7 are 0 (bit 2 is `enableCrossAddress`) | upstream `Flags::from_byte` | `prepare::tests::the_flag_bits_of_each_pool` | `check.rs:179` | implemented+tested |
| ZIP-229-3 | `flagsIronwood`: bits 3..7 are 0 | upstream `Flags::from_byte` | `prepare::tests::the_flag_bits_of_each_pool` | `serialize.rs` | implemented+tested |
| ZIP-229-4 | `sizeProofs` of each pool is 2720 + 2272 × actions | upstream `Bundle::try_from_parts` | `prepare::tests::a_proof_has_the_canonical_length` | `serialize.rs` | implemented+tested |
| ZIP-229-5 | From NU6.3 the version is 4, 5 or 6 | `check_version` (`RuleSet::tx_versions`) | `prepare::tests::the_rule_set_names_the_transaction_versions` | `zakura-consensus/src/transaction.rs:1181-1240` | implemented+tested |
| ZIP-229-6 | v6 has version group 0xD884B698 | upstream `TxVersion::read` | `scan.rs` `generated_transactions_of_every_version` | `serialize.rs` | implemented+tested |
| ZIP-229-7 | `nActionsIronwood` < 2^16 | block size limit (`hayai-wire` `MAX_BLOCK_BYTES`): 2^16 actions need more than 53 MB | `hayai-wire` `oversized_block_is_rejected_before_parsing` | `serialize.rs` (count limit) | implemented differently: no transaction in a valid block can hold 2^16 actions |
| ZIP-229-8 | v5: a source of funds (inputs, Sapling spends, Orchard actions with `enableSpends`) | `draft` (`NoSource`) | `prepare::tests::a_source_and_a_sink_of_funds_respect_the_enable_flags` | `check.rs:131` | implemented+tested |
| ZIP-229-9 | v6: a source of funds, Ironwood actions with `enableSpends` included | `draft` (`NoSource`) | the same test | `check.rs:131` | implemented+tested |
| ZIP-229-10 | v5 and v6: the same rule for a sink of funds | `draft` (`NoSink`) | the same test | `check.rs:131` | implemented+tested |
| ZIP-229-11 | Ironwood actions need `enableSpends` or `enableOutputs` | `draft` (`IronwoodFlags`) | `prepare::tests::actions_need_an_enable_flag` | `check.rs:165` | implemented+tested |
| ZIP-229-12 | `bindingSigIronwood` is valid | `hayai-prepared/src/shielded.rs` (upstream `BatchValidator`) | `prepared.rs` `bisection_isolates_the_tampered_orchard_bundle`; `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | `zakura-consensus/src/transaction.rs:1428` | implemented+tested |
| ZIP-229-13 | A coinbase has no Orchard-pool actions from NU6.3 | `draft` (`CoinbaseOrchardBundle`, `RuleSet::coinbase.orchard_bundle`) | `prepare::tests::the_coinbase_rules_of_the_orchard_and_ironwood_bundles` | `check.rs:367` | implemented+tested |
| ZIP-229-14 | v5 or v6 coinbase: `enableSpendsOrchard = 0` | `draft` (`CoinbaseShieldedSpend`) | the same test | `check.rs:251` | implemented+tested |
| ZIP-229-15 | v6 coinbase: `enableSpendsIronwood = 0` | `draft` (`CoinbaseShieldedSpend`) | the same test; `hayai-bench/tests/ironwood.rs` `context_free_rules_reject_at_the_block_level` | `check.rs:251` | implemented+tested |
| ZIP-229-16 | v5 or v6: reserved bits 2..7 of `flagsOrchard` are 0 | upstream `Flags::from_byte`; `draft` (`OrchardCrossAddress`) from NU6.3 | `prepare::tests::the_flag_bits_of_each_pool` | `check.rs:179` | implemented+tested |
| ZIP-229-17 | v6: reserved bits 3..7 of `flagsIronwood` are 0 | upstream `Flags::from_byte` | the same test | `serialize.rs` | implemented+tested |
| ZIP-229-18 | Proofs and signatures in the order of the actions, for each pool | upstream parser | `scan.rs` `generated_transactions_of_every_version` | `serialize.rs` | implemented, no direct test |
| ZIP-229-19 | v6 txid: the ZIP 244 tree plus `ironwood_digest_v6`; anchors leave the effecting data | upstream `TxIdDigester` (txid of `Transaction::read`) | `scan.rs` `generated_transactions_of_every_version` (txid against upstream) | `zakura-chain/src/transaction/txid.rs` | implemented, no direct test: no published v6 txid vector |
| ZIP-229-20 | v6 auth digest: Ironwood auth digest last; anchors in the auth digests; v6 personalizations | `hayai-wire/src/scan.rs` `auth_digest` | `scan.rs` `generated_transactions_of_every_version` (against upstream `auth_commitment`) | `zakura-chain/src/transaction/auth_digest.rs` | implemented+tested |
| ZIP-229-21 | v6 sighash: the ZIP 244 tree with the v6 digests | upstream `signature_hash` (`sighash_v6.rs`) | `hayai-bench/tests/ironwood.rs` `a_nu6_3_block_validates_cold_and_warm_to_identical_layers` | `zakura-chain/src/transaction/sighash.rs` | implemented+tested |
| ZIP-229-22 | `hashAuthDataRoot` takes the v6 auth digests | `hayai-wire/src/lib.rs` `auth_data_root` | `hayai-bench/tests/ironwood.rs` `the_header_commits_to_the_history_tree_of_version_3` | `zakura-chain/src/block/commitment.rs` | implemented+tested |

## ZIP 234: Network Sustainability Mechanism: Issuance Smoothing
Class: (a) consensus. Activation: none. ZIP 259 states that NU7 does not deploy ZIP 234.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-234-1 | `BLOCK_SUBSIDY_FRACTION` = 4,126 / 10^10 | none | none | none | not applicable: not deployed |
| ZIP-234-2 | `DEPLOYMENT_BLOCK_HEIGHT`: after NU7, the first height after the second halving where NSM issuance is below the schedule | none | none | none | not applicable: not deployed |
| ZIP-234-3 | From that height `BlockSubsidy = ceiling(fraction * MoneyReserveAfter(height − 1))` in place of the halvings | none (hayai keeps the halving schedule of ZIP 237) | none | none (Zakura keeps it too) | not applicable: not deployed |
| ZIP-234-4 | The change applies to Mainnet and Testnet | none | none | none | not applicable: not deployed |
| ZIP-234-5 | Deployment with or after ZIP 233 | none | none | none | not applicable: not deployed |

## ZIP 235: Remove 60% of Transaction Fees From Circulation
Class: (a) consensus. Activation: NU7 (draft, deployed by ZIP 259).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-235-1 | `NSMFeeContribution = floor(6 * TransactionFees / 10)` on the aggregate fees of the block | `nsm.rs` `miner_fee_share` (50); `hayai-state/src/check.rs` passes `totals.fees` | `nsm::tests::the_miner_gets_the_fees_minus_six_tenths_rounded_down`; `conformance_nu7::the_nsm_values_match_zakura_chain` | `zk-fees:20-41` | implemented+tested |
| ZIP-235-2 | `MinerFees = TransactionFees − NSMFeeContribution` | `nsm.rs` `miner_fee_share`; `coinbase.rs` `CoinbaseTerms::miner_fees` (221) | the same tests | `zk-fees:20-41` | implemented+tested |
| ZIP-235-3 | From NU7 the coinbase total input value is `BlockSubsidy + MinerFees + totalDeferredInput` | `coinbase.rs` `payable`, `miner_fees`; `rules.rs` `nu7` (`nsm_fee_share: true`) | `coinbase::tests::the_terms_at_the_nu7_boundary`; hayai-template `coinbase::tests::the_coinbase_follows_the_rules_at_the_nu7_boundary`; `conformance_nu7::the_coinbase_terms_across_nu7_match_zakura_chain` | `zk-check:365-368` | implemented+tested |
| ZIP-235-4 | The exact value rule of ZIP 236 applies to that input value | `coinbase.rs` `CoinbaseTerms::check` | `coinbase::tests::the_terms_at_the_nu7_boundary` (miner ± 1 and the full fees refused) | `zk-check:377-381` | implemented+tested |
| ZIP-235-5 | The contribution enters no chain value pool and is not in `IssuedSupply` | implicit: the fees leave the pools and the coinbase adds only `MinerFees` (`hayai-state/src/check.rs` `value_pools_after`) | none | `zakura-chain/src/block.rs:359-416` | implemented, no direct test |
| ZIP-235-6 | With ZIP 237 the contribution is in `removed(height)` and is credited to the NSM value balance | `nsm.rs` `balance` (scheduled issuance minus the pools) | `nsm::tests::the_balance_is_the_scheduled_issuance_minus_the_pools`; `conformance_nu7::the_nsm_values_match_zakura_chain` | `zakura-chain/src/block.rs:359-416` | implemented differently: the closed form counts the contribution through the pools; same value |
| ZIP-235-7 | The change applies to Mainnet and Testnet from NU7 | `rules.rs` `nu7` | `rules::tests` (`nsm_fee_share == u >= Nu7`) | `zk-fees:20-41` | implemented+tested |

## ZIP 236: Blocks should balance exactly
Class: (a) consensus. Activation: NU6.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-236-1 | From NU6 the total output value of the coinbase equals its total input value | `coinbase.rs` `CoinbaseTerms::check` (`exact_value` branch, 278); `rules.rs` `NU6` (`exact_value: true`) | `coinbase::tests::from_nu6_the_value_is_exact`; `rules::tests` (`exact_value == u >= Nu6`); hayai-template `coinbase::tests::a_changed_coinbase_of_the_consensus_rule_fails_the_coinbase_check` | `zk-check:377-381` | implemented+tested |
| ZIP-236-2 | Before NU6 the total output value is at most the total input value | `coinbase.rs` `CoinbaseTerms::check` (`ValueAboveLimit`) | `coinbase::tests::before_nu6_the_value_is_a_limit` | `zk-check:377-378` | implemented+tested |
| ZIP-236-3 | §3.4: from NU6 the remaining value of the coinbase transparent value pool is zero | the same as ZIP-236-1 | the same | the same | implemented+tested |
| ZIP-236-4 | The change applies to Mainnet and Testnet | `rules.rs` (rule set of NU6 on each network) | `coinbase::tests::terms_have_the_outputs_of_each_era` (`exact` column, both networks) | `zk-check:377` | implemented+tested |

## ZIP 237: Network Sustainability Mechanism: Halving-Preserving Issuance
Class: (a) consensus. Activation: NU7, reissuance from `DEPLOYMENT_BLOCK_HEIGHT` (draft, deployed by ZIP 259).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-237-1 | `LN2_SCALED` = 6,931,680,000 | `nsm.rs` `REISSUANCE_NUMERATOR` (36, the derived numerator) | `nsm::tests::the_reissuance_fraction_is_the_one_of_zip_237` (new) | `zk-subsidy:575` | implemented differently: hayai stores the numerator 1,375; the test derives it from `LN2_SCALED` |
| ZIP-237-2 | `BLOCK_SUBSIDY_FRACTION(height) = floor(LN2_SCALED / HalvingInterval(height)) / 10^10`, 1,375 / 10^10 from NU7 | `nsm.rs` `REISSUANCE_NUMERATOR`, `REISSUANCE_DENOMINATOR` | `nsm::tests::the_reissuance_fraction_is_the_one_of_zip_237` | `zk-subsidy:587,592` | implemented differently: a constant 1,375; the reissuance height is at or above NU7 on Mainnet and Testnet, so the value is the same; a Regtest with another halving interval keeps 1,375, as Zakura |
| ZIP-237-3 | `INITIAL_NSM_VALUE_BALANCE` = `S(H) − IssuedSupply(H)` for H from the last pre-NU6 height to `A − 1` | `nsm.rs` `expected_seed` (60: Mainnet 36,858,445,520, Testnet 55,768,414,957), `check_balance` (99) | `nsm::tests::the_balance_rules_start_at_the_block_before_nu7`; `hayai-state` `check::tests::the_nsm_rules_apply_from_the_block_before_nu7` | `zk-main:44`, `zk-test:27`, `value_balance.rs:377-414` | implemented differently: the measured constants of Zakura, and an equality check of the pools at `A − 1` (`NsmSeedMismatch`), as Zakura; the ZIP gives an estimate only (see Unverified) |
| ZIP-237-4 | `DEPLOYMENT_BLOCK_HEIGHT` is the first `h` in `[max(A, H3 + 1), H4)` with `ceil(1375 (MAX_MONEY − S_A(h − 1)) / 10^10) < B_A(h)` | `nsm.rs` `reissuance_height` (129) | `nsm::tests::the_reissuance_height_of_each_network`; `conformance_nu7::the_nsm_values_match_zakura_chain` | `zk-subsidy:609-720` | implemented+tested |
| ZIP-237-5 | The closed form `h0 + ceil(max(0, R0 − Rmax) / b)` | `nsm.rs` `reissuance_height` | the same tests; Python check of the ZIP Mainnet example (A = 3,543,000 gives 8,940,474) with the same formula | `zk-subsidy:656-684` | implemented+tested |
| ZIP-237-6 | NU7 heights are chosen so that the height exists before `H4` | `nsm.rs` `reissuance_height` returns `None` when it does not exist | none | `zk-subsidy:609-650` | not applicable: a rule on the choice of a constant; Testnet satisfies it |
| ZIP-237-7 | Reissuance height `16,235,274 − 2A` on Testnet (7,305,222 by ZIP 259), `16,026,474 − 2A` on Mainnet | `nsm.rs` `reissuance_height` | `nsm::tests::the_reissuance_height_of_each_network` (7,305,222; Mainnet none) | `zk-subsidy:696-720` | implemented+tested |
| ZIP-237-8 | `ScheduledBlockSubsidy` is `BlockSubsidy` of §7.8 after ZIP 218 | `subsidy.rs` `total_subsidy` (198) | `subsidy::tests::the_testnet_schedule_follows_the_nu7_spacing`; `conformance_nu7::the_schedule_across_nu7_matches_zakura_chain` | `zk-subsidy:949-984` | implemented+tested |
| ZIP-237-9 | `AdditionalBlockSubsidy(height)`: 0 below the deployment height, else `ceiling(fraction * NSMValueBalance(height − 1))` | `coinbase.rs` `CoinbaseTerms::terms` (141), `nsm.rs` `reissuance_bonus` (168), `reissuance_active` | `coinbase::tests::the_subsidy_has_the_reissuance_bonus_from_the_reissuance_height`, `nsm::tests::the_bonus_rounds_up`; `conformance_nu7::the_subsidy_with_the_reissuance_bonus_matches_zakura_chain` | `zk-subsidy:748-785,927-946` | implemented+tested |
| ZIP-237-10 | `BlockSubsidy = ScheduledBlockSubsidy + AdditionalBlockSubsidy` | `coinbase.rs` `CoinbaseTerms::terms` | the same tests; hayai-template `coinbase::tests::the_coinbase_has_the_reissuance_bonus_from_the_reissuance_height` | `zk-subsidy:927-946` | implemented+tested |
| ZIP-237-11 | `FoundersReward`, `fs.Value`, `totalDeferredOutput`, `MinerSubsidy` and the coinbase input value use the new `BlockSubsidy` | `coinbase.rs` `CoinbaseTerms::terms` (passes the total with the bonus to `funding_streams`), `miner_subsidy` | none with a funding stream at a height with a bonus | `zk-check:262` | implemented, no direct test: on Testnet the streams end at 4,497,948, before 7,305,222 |
| ZIP-237-12 | `NSMValueBalance`: 0 below `A − 1`, the seed at `A − 1`, then `previous − AdditionalBlockSubsidy + removed` | `nsm.rs` `balance` (78, `S(height) − IssuedSupply(height)`) | `nsm::tests::the_balance_is_the_scheduled_issuance_minus_the_pools`; `conformance_nu7` (scheduled issuance) | `zakura-chain/src/block.rs:359-416` | implemented differently: the closed form; it equals the recursion because each block changes the pools by `Scheduled + Additional − removed` (ZIP 236 exact claim) |
| ZIP-237-13 | `removed(height)` is the value that a deployed mechanism removes (ZIP 235 only in NU7) | `nsm.rs` `balance` | see ZIP-235-6 | `zakura-chain/src/block.rs:359-416` | implemented differently: through the pools (ZIP-235-6) |
| ZIP-237-14 | The NSM value balance is consensus state, not a chain value pool, not in `IssuedSupply` | `nsm.rs` `balance` (no stored value) | `nsm::tests::the_balance_is_the_scheduled_issuance_minus_the_pools` | `value_balance.rs:416-433` (stored) | implemented differently: derived from the pools and the schedule, no stored value; same value |
| ZIP-237-15 | [NU7 onward] a block that makes `NSMValueBalance` negative is not valid | `nsm.rs` `check_balance` (99); `hayai-state/src/check.rs` `block_pools_after` | `nsm::tests::the_balance_rules_start_at_the_block_before_nu7`; `hayai-state` `check::tests::the_nsm_rules_apply_from_the_block_before_nu7` | `zakura-state/src/service/check.rs:65-98` | implemented+tested |
| ZIP-237-16 | The changes apply to Mainnet and Testnet | `nsm.rs` (one code path; network constants) | `nsm::tests::the_reissuance_height_of_each_network` | `zk-subsidy:696-720` | implemented+tested |
| ZIP-237-17 | ZIP 237 is not deployed together with ZIP 234 | no ZIP 234 code | none | none | not applicable: deployment rule; hayai has no ZIP 234 code |

## ZIP 239: Relay of Version 5 Transactions
Class: (b) network. Activation: protocol version 170,014, before NU5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-239-1 | `MSG_WTX` (5) in `inv` and `getdata`: the 64-byte `txid` and `auth_digest` | `hayai-net/src/codec.rs` `MSG_WTX`, `Reader::inv`, `encode_body` | `zebra_msg_wtx_vector` (`hayai-net/tests/codec.rs`) | `zakura-network/src/protocol/external/inv.rs:172` | implemented+tested |
| ZIP-239-2 | `MSG_WTX` MUST be used to announce a v5 transaction | `hayai-net/src/session.rs` `tx_inv_item` | `session::tests::tx_inv_item_follows_zip239` | `inv.rs` (`InventoryHash::from` an unmined id) | implemented+tested |
| ZIP-239-3 | A transaction that `getdata` `MSG_WTX` gets has the encoding of the specification | `hayai-net/src/relay.rs` `Relay::serve_getdata` (wire bytes of the store) | `simulated_legacy_peer_with_the_bit_gets_zcmpctver_then_legacy_relay` (`hayai-net/tests/loopback.rs`) | `zakurad/src/components/inbound.rs` | implemented+tested |
| ZIP-239-4 | MUST NOT use `MSG_WTX` for v4 and earlier transactions | `session.rs` `tx_inv_item` (`PRE_V5_AUTH_DIGEST` gives `MSG_TX`) | `session::tests::tx_inv_item_follows_zip239` | `inv.rs` | implemented+tested |
| ZIP-239-5 | MUST NOT use `MSG_WTX` on a connection below version 170,014 | `hayai-net/src/protocol.rs` `INITIAL_MIN_PEER_VERSION` 170,150 | `session::tests::handshake_errors` | `constants.rs:432-437` | implemented differently: no such connection completes the handshake |
| ZIP-239-6 | Deployment version 170,014 on Testnet and Mainnet | none (no constant; see ZIP-239-5) | none | none | implemented differently: the floor 170,150 is above it |
| ZIP-239-7 | Before NU5, MUST NOT advertise, fetch or provide v5 transactions | `relay.rs` `Relay::on_inv` (line 1519, fetch); the store holds no v5 transaction before NU5 (`hayai-prepared` version rule of the rule set), so `broadcast_tx` and `serve_getdata` have none | `before_nu5_a_wtxid_announcement_is_not_fetched` (`hayai-net/tests/loopback.rs`) | none found | implemented+tested (fixed, F-P6-8) |
| ZIP-239-8 | RECOMMENDED: parse the whole message and refuse it for an unknown entry type | `codec.rs` `Reader::inv` | `zebra_msg_wtx_vector` | `inv.rs:158-175` | implemented differently: type 0 is accepted (finding F-P6-5) |
| ZIP-239-9 | `getdata` types: `MSG_TX`, `MSG_BLOCK`, `MSG_FILTERED_BLOCK` (36 bytes), `MSG_WTX` (68 bytes) | `codec.rs` `Reader::inv`; `relay.rs` `Relay::serve_getdata` (`MSG_FILTERED_BLOCK` gets `notfound`) | `zebra_msg_wtx_vector` | `inv.rs:158-175` | implemented+tested |
| ZIP-239-10 | SHOULD NOT send `MSG_FILTERED_BLOCK` in `inv` | `relay.rs`: never sent | none | none | implemented, no direct test |
| ZIP-239-11 | SHOULD ignore a `MSG_FILTERED_BLOCK` entry of an `inv` | `relay.rs` `Relay::on_inv` (marked) | none | `zakurad/src/components/inbound.rs` | implemented, no direct test |

## ZIP 243: Transaction Signature Validation for Sapling
Class: (a) consensus. Activation: Sapling.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-243-1 | v4 signatures use the ZIP 243 sighash | `hayai-prepared/src/prepare.rs` `SighashContext::transparent`, `shielded` (upstream `sighash_v4.rs`) | block vectors (`conformance_blocks.rs` `block_vectors_match_the_expected_outcomes`, `prepared.rs` `every_fixture_transaction_prepares`); the ZIP 243 vectors are not run against `SighashContext` (bd hayai-xya) | `zakura-script/src/lib.rs:199-209` | implemented+tested |
| ZIP-243-2 | v4 hashes the raw hash type byte | `SighashContext::transparent` (`SighashType::from_raw`) | `prepared.rs` `a_flipped_signature_byte_is_a_script_error` | `zakura-script/src/lib.rs:203` | implemented+tested |
| ZIP-243-3 | The sighash commits to the consensus branch id | upstream `signature_hash` | block vectors | `sighash.rs` | implemented+tested |

## ZIP 244: Transaction Identifier Non-Malleability
Class: (a) consensus. Activation: NU5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-244-T | v5 txid: the T.1-T.4 digest tree, personalization `ZcashTxHash_` with the branch id | upstream `Transaction::read` (`TxIdDigester`) | `hayai-wire/tests/scan.rs` `upstream_transaction_vectors` (ZIP 244 vectors) | `zakura-chain/src/transaction/txid.rs` | implemented+tested |
| ZIP-244-S.1 | v5 sighash: the S.1-S.4 tree | upstream `signature_hash` (`sighash_v5.rs`) | block vectors (`conformance_blocks.rs`); bd hayai-xya | `zakura-chain/src/transaction/sighash.rs` | implemented+tested |
| ZIP-244-S.2a-1 | v5 and later: only the hash types 0x01, 0x02, 0x03, 0x81, 0x82, 0x83 | `SighashContext::transparent` (`SighashType::parse`) | none | `zakura-script/src/lib.rs:213` (`parse_zip244_hash_type`) | implemented, no direct test |
| ZIP-244-S.2a-2 | v5 and later: `SIGHASH_SINGLE` without an output at the input index fails validation | `SighashContext::transparent` | `prepare::tests::sighash_single_needs_the_output_of_its_index_from_v5` | `zakura-script/src/lib.rs:214-221` | implemented+tested (fixed in this change, F-P3-1) |
| ZIP-244-S.2 | The shielded sighash uses `SIGHASH_ALL` | `SighashContext::shielded` (`SignableInput::Shielded`) | block vectors | `sighash.rs` | implemented+tested |
| ZIP-244-A | Auth digest: transparent scripts, Sapling and Orchard auth digests, personalization `ZTxAuthHash_` with the branch id | `hayai-wire/src/scan.rs` `auth_digest` | `scan.rs` `upstream_transaction_vectors`, `hayai-wire/tests/vectors.rs` `auth_digest_follows_zip_239` | `zakura-chain/src/transaction/auth_digest.rs` | implemented+tested |
| ZIP-244-B1 | `hashAuthDataRoot`: Merkle tree of auth digests in txid order, padded with zero leaves, `ZcashAuthDatHash` | `hayai-wire/src/lib.rs` `auth_data_root` | `lib.rs` `auth_data_root_padding_is_zero_leaves`, `auth_data_root_matches_sequential` | `zakura-chain/src/block/commitment.rs` | implemented+tested |
| ZIP-244-B2 | `hashBlockCommitments` = BLAKE2b-256(`ZcashBlockCommit`, history root, auth data root, 32 zero bytes) | `hayai-wire/src/lib.rs` `block_commitments`; `hayai-state/src/history.rs` `header_commitment` | `lib.rs` `block_commitments_is_personalized_blake2b_over_both_roots`, `hayai-state/tests/history.rs` `nu5_and_later_commit_to_the_root_and_the_auth_data_root` | `commitment.rs` | implemented+tested |
| ZIP-244-B3 | The change applies in the NU5 activation block | `header_commitment` (branch of the block) | `history.rs` `nu5_and_later_commit_to_the_root_and_the_auth_data_root` | `commitment.rs` | implemented+tested |
| ZIP-244-B4 | Full nodes use the whole structure of the latest upgrade they support | `header_commitment` | the same test | `commitment.rs` | implemented+tested |

## ZIP 250: Deployment of the Heartwood Network Upgrade
Class: (a) consensus, (b) network. Activation: Heartwood.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-250-1 | `CONSENSUS_BRANCH_ID` (Heartwood) = `0xF5B9230B` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:235` | implemented+tested |
| ZIP-250-2 | `ACTIVATION_HEIGHT` (Heartwood): Testnet 903,800, Mainnet 903,000 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:65,96` | implemented+tested |
| ZIP-250-3 | Heartwood nodes MUST advertise at least 170,010 (Testnet) and 170,011 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-250-3b | After Heartwood, SHOULD refuse and close pre-Heartwood peers | as ZIP-205-PEERS | as ZIP-205-PEERS | `types.rs:33-48` | implemented+tested |
| ZIP-250-4 | The minimum peer protocol version is 170,002 | `INITIAL_MIN_PEER_VERSION` 170,150 | the same test | `constants.rs:432-437` | implemented differently (as ZIP-205-4) |
| ZIP-250-5 | After Heartwood activates, a node SHOULD refuse and disconnect pre-Heartwood peers | `min_peer_version` (P6) | the same test | `types.rs:88-131` | implemented+tested |
| ZIP-250-6 | Signatures MUST use the Heartwood branch id | sighash with the branch of the epoch (P3) | hayai-bench `conformance_txs::sighash_vector_transactions_through_draft` | P3 | implemented+tested |

## ZIP 251: Deployment of the Canopy Network Upgrade
Class: (a) consensus, (b) network. Activation: Canopy.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-251-1 | `CONSENSUS_BRANCH_ID` (Canopy) = `0xE9FF75A6` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:236` | implemented+tested |
| ZIP-251-2 | `ACTIVATION_HEIGHT` (Canopy): Testnet 1,028,500, Mainnet 1,046,400 | `protocol_height`; `checkpoints.rs` `mandatory_checkpoint_height` = Canopy − 1 (l. 145) | `network::tests::the_deployment_constants_of_the_zips`, `checkpoints::tests::the_mandatory_checkpoint_is_the_block_before_canopy` | `constants.rs:67,98`; `zakura-chain/src/parameters/network.rs:271` | implemented+tested |
| ZIP-251-3 | Canopy nodes MUST advertise at least 170,012 (Testnet) and 170,013 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-251-3b | After Canopy, SHOULD refuse and close pre-Canopy peers | as ZIP-205-PEERS | as ZIP-205-PEERS | `types.rs:33-48` | implemented+tested |
| ZIP-251-4 | The minimum peer protocol version is 170,002 | `INITIAL_MIN_PEER_VERSION` 170,150 | the same test | `constants.rs:432-437` | implemented differently (as ZIP-205-4) |
| ZIP-251-5 | After Canopy activates, a node SHOULD refuse and disconnect pre-Canopy peers | `min_peer_version` (P6) | the same test | `types.rs:88-131` | implemented+tested |
| ZIP-251-6 | Signatures MUST use the Canopy branch id | sighash (P3) | hayai-bench `conformance_txs::sighash_vector_transactions_through_draft` | P3 | implemented+tested |

## ZIP 252: Deployment of the NU5 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-252-1 | `CONSENSUS_BRANCH_ID` (NU5) = `0xc2d6d0b4` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:237` | implemented+tested |
| ZIP-252-2 | `ACTIVATION_HEIGHT` (NU5): Testnet (second activation) 1,842,420, Mainnet 1,687,104 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:69,100` | implemented+tested |
| ZIP-252-3 | `MIN_NETWORK_PROTOCOL_VERSION` (NU5): Testnet 170,050, Mainnet 170,100 | `hayai-net/src/protocol.rs` `min_peer_version` (floor 170,150) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:111-112` | implemented differently (the floor 170,150 is above both values, as Zakura `constants.rs:432-437`) |
| ZIP-252-4 | NU5 nodes MUST advertise at least `MIN_NETWORK_PROTOCOL_VERSION` (NU5): 170,050 (Testnet), 170,100 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-252-4b | After NU5, SHOULD refuse and close pre-NU5 peers | as ZIP-205-PEERS | as ZIP-205-PEERS | `types.rs:33-48` | implemented+tested |
| ZIP-252-5 | After NU5 activates, a node SHOULD refuse and disconnect pre-NU5 peers | `min_peer_version` (P6) | the same test | `types.rs:88-131` | implemented+tested |
| ZIP-252-6 | An NU5 node MUST accept both the v4 and the v5 transaction formats | `rules.rs` `NU5.tx_versions` = {4, 5} (l. 219) | `rules::tests::the_rules_of_each_upgrade` | `zakura-consensus/src/transaction.rs:1023-1040` | implemented+tested |
| ZIP-252-7 | The `MSG_WTX` change of ZIP 239 applies from peer version 170,014 | as ZIP-239-5 and ZIP-239-6: no peer below 170,150 completes the handshake | `session::tests::handshake_errors` (`hayai-net/src/session.rs`) | `zakura-network/src/constants.rs:432-437` | implemented differently: the floor 170,150 is above 170,014, so every connection has `MSG_WTX` |

## ZIP 253: Deployment of the NU6 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU6.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-253-1 | `CONSENSUS_BRANCH_ID` (NU6) = `0xC8E71055` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:238` | implemented+tested |
| ZIP-253-2 | `ACTIVATION_HEIGHT` (NU6): Testnet 2,976,000, Mainnet 2,726,400 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:71,102` | implemented+tested |
| ZIP-253-3 | `MIN_NETWORK_PROTOCOL_VERSION` (NU6): Testnet 170,110, Mainnet 170,120 | `min_peer_version` (floor 170,150) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:113-114` | implemented differently (as ZIP-252-3) |
| ZIP-253-4 | NU6 nodes MUST advertise at least 170,110 (Testnet) and 170,120 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-253-5 | NU6 has no new transaction version: v4 and v5 | `rules.rs` `NU6` keeps {4, 5} | `rules::tests::the_rules_of_each_upgrade` | `transaction.rs:1023-1040` | implemented+tested |
| ZIP-253-6 | Signatures MUST use the NU6 branch id | sighash (P3) | hayai-bench `conformance_txs::sighash_vector_transactions_through_draft` | P3 | implemented+tested |

## ZIP 254: Deployment of the NU7 Network Upgrade (Withdrawn)
Class: none. Withdrawn: ZIP 259 replaces it. No rule.

## ZIP 255: Deployment of the NU6.1 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU6.1.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-255-1 | `CONSENSUS_BRANCH_ID` (NU6.1) = `0x4DEC4DF0` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:239` | implemented+tested |
| ZIP-255-2 | `ACTIVATION_HEIGHT` (NU6.1): Testnet 3,536,500, Mainnet 3,146,400 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips` | `constants.rs:73,104` | implemented+tested |
| ZIP-255-3 | `MIN_NETWORK_PROTOCOL_VERSION` (NU6.1): Testnet 170,130, Mainnet 170,140 | `min_peer_version` (floor 170,150) | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:115-118` | implemented differently (as ZIP-252-3) |
| ZIP-255-4 | NU6.1 nodes MUST advertise at least 170,130 (Testnet) and 170,140 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |

## ZIP 256: Deployment of Consensus Bug Fixes Between NU6.1 and NU6.2
Class: (a) consensus. Activation: no height (immediate).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-256-1 | An Orchard `rk` is not the zero point | upstream `orchard` 0.15.5 `Action::from_parts` (`IdentityRk`), at parse | none | Zakura `orchard` fork (`Action::from_parts`) | implemented, no direct test |
| ZIP-256-2 | ZIP 209 change: transparent and deferred pools non-negative, total ≤ MAX_MONEY | P5 (ZIP-209-4, -5, -7) | P5 rows ZIP-209-4, -5, -7 | P5 | implemented+tested |
| ZIP-256-3 | Sprout proofs are verified on block connection | `sprout.rs` `verify_sprout` (Groth16); BCTV14 `Unsupported` | `hayai-bench/tests/sprout.rs` `the_embedded_key_verifies_the_published_joinsplits`, `a_changed_joinsplit_transaction_fails` | `transaction.rs:1275` | implemented+tested (BCTV14: checkpoint path only) |
| ZIP-256-4 | An Orchard `ephemeralKey` decodes to a valid non-zero Pallas point | upstream `orchard` 0.15.5 `Action::from_parts` (`InvalidEpk`), at parse | none | Zakura `orchard` fork | implemented, no direct test |
| ZIP-256-5 | v4 `valueBalanceSapling` with no spends and outputs is rejected when nonzero | P3 (SPEC-7.1.2-29) | P3 row SPEC-7.1.2-29 | P3 | implemented+tested |
| ZIP-256-6 | From NU5, a body change of the authorizing data does not mark the header invalid (block-body poisoning) | `hayaid/src/node/fault.rs` `check_body`, `fault_of` (`WrongBody` for `BlockCommitments`, `MerkleRoot`) | `fault.rs` tests (`MerkleRoot`, `BlockCommitments` give `WrongBody`) | `zakura-consensus` (block verifier order) | implemented+tested |
| ZIP-256-7 | The P2SH input sigop count matches zcashd | `hayai-prepared/src/prepare.rs` `sigops`, `last_push` | `prepare::tests::sigops_count_legacy_and_p2sh_like_zcashd`, `p2sh_pattern_is_exact` | `zakura-script/src/lib.rs:374` | implemented+tested |
| ZIP-256-8 | Each input is matched with its own spent output | `hayai-state/src/check.rs` `resolve_inputs` (index per input), `SpentMismatch` | `hayai-bench/tests/state.rs` `prepared_against_a_different_coin_is_rejected` | `utxo.rs:45` | implemented+tested |
| ZIP-256-9 | A coinbase has no Sapling spend | `draft` (`CoinbaseShieldedSpend`) | none for Sapling (bd hayai-cvi) | `check.rs:251` | implemented, no direct test |
| ZIP-256-10 | Transparent inputs and spent outputs are aligned before script verification | `draft` (`SpentCoins`) | `prepare::tests::a_wrong_number_of_spent_coins_is_an_error` | `transaction.rs:1244` | implemented+tested |
| ZIP-256-11 | A v5 transaction is not taken as verified on its txid alone | the cache of context-free results is keyed by wtxid (`hayai-prepared` `store.rs`) | `hayai-bench/tests/prepared.rs` `store_detects_conflicts_tracks_parents_and_streams_events` | `zakura-consensus` (mempool lookup by wtxid) | implemented differently: the key holds the auth digest, so a changed signature is a new entry |

## ZIP 257: Deployment of the Orchard Temporary Vulnerability Mitigation and NU6.2 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU6.2; the Orchard soft fork from Mainnet 3,363,426 and Testnet 4,048,500.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-257-C1 | `CONSENSUS_BRANCH_ID` (NU6.2) = `0x5437F330` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:240` | implemented+tested |
| ZIP-257-C2 | `ACTIVATION_HEIGHT` (NU6.2): Testnet 4,052,000, Mainnet 3,364,600 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips`, `network::tests::the_orchard_pool_is_off_from_the_soft_fork_until_nu6_2` | `constants.rs:75,106` | implemented+tested |
| ZIP-257-C3 | `MIN_NETWORK_PROTOCOL_VERSION` (NU6.2): 170,150 on both networks | `INITIAL_MIN_PEER_VERSION`, `min_peer_version` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:119-122` | implemented+tested |
| ZIP-257-C4 | NU6.2 nodes MUST advertise at least 170,150 (Mainnet and Testnet) | `protocol.rs` `PROTOCOL_VERSION` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `constants.rs:353` | implemented+tested |
| ZIP-257-C5 | Soft-fork start heights: Mainnet 3,363,426, Testnet 4,048,500 | `network.rs` `orchard_disabled_start_height` (l. 481, 506) | `network::tests::the_orchard_pool_is_off_from_the_soft_fork_until_nu6_2`, `rules::tests::the_orchard_pool_is_off_in_the_soft_fork_range` | `zakura-chain/src/parameters/network.rs:26,31` | implemented+tested |

### ZIP 257: Orchard mitigation and NU6.2 rules
Class: (a) consensus. Activation: Mainnet 3,363,426 to NU6.2 3,364,600; Testnet 4,048,500 to 4,052,000. Constants: P1.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-257-1 | From Mainnet 3,363,426 / Testnet 4,048,500 until NU6.2: no Orchard actions in v5 and later | `hayai-consensus/src/rules.rs` `rules_at` (Orchard pool off); `hayai-state/src/check.rs` `check_pools` | `hayai-bench/tests/state.rs` `the_orchard_pool_is_off_in_the_soft_fork_range` | `zakura-consensus/src/transaction.rs:484-493` | implemented+tested (blocks; the mempool uses the rule set of the branch) |
| ZIP-257-2 | NU5 to NU6.1: the key `OrchardCircuitVersion::InsecurePreNU6_2` | `hayai-prepared/src/orchard.rs` `circuit_version` | `orchard::tests::circuit_version_follows_the_branch` | `zakura-consensus/src/primitives/halo2.rs:405` | implemented+tested |
| ZIP-257-3 | NU6.2: the key `FixedPostNU6_2` | `circuit_version` | the same test | `halo2.rs:405` | implemented+tested |
| ZIP-257-4 | The proof is valid under the key of the epoch | `hayai-prepared/src/shielded.rs` `ScopedBatch::add`, `finalize` (groups per circuit version; upstream `BatchValidator`) | `hayai-bench/tests/prepared.rs` `bisection_isolates_the_tampered_orchard_bundle`; `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | `transaction.rs:1428` | implemented+tested |
| ZIP-257-5 | NU6.2 on: `proofsOrchard` has the length 2720 + 2272 × actions | upstream `Bundle::try_from_parts` | `prepare::tests::a_proof_has_the_canonical_length` | `zakura-chain` serializer | implemented+tested |
| ZIP-257-6 | From NU6.2, transactions with Orchard actions are valid again | `rules_at` (NU6.2 rule set has the Orchard pool) | `state.rs` `the_orchard_pool_is_off_in_the_soft_fork_range` | `transaction.rs:484-493` | implemented+tested |

## ZIP 258: Deployment of the NU6.3 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU6.3.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-258-C1 | `CONSENSUS_BRANCH_ID` (NU6.3) = `0x37A5165B` | `Upgrade::branch_id` | `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:241` | implemented+tested |
| ZIP-258-C2 | `ACTIVATION_HEIGHT` (NU6.3): Testnet 4,134,000, Mainnet 3,428,143 | `protocol_height` | `network::tests::the_deployment_constants_of_the_zips`, `network::tests::activation_heights_match_zcash_protocol` | `constants.rs:77,108` | implemented+tested |
| ZIP-258-C3 | `MIN_NETWORK_PROTOCOL_VERSION` (NU6.3): 170,160 on both networks | `min_peer_version` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:123-126` | implemented+tested |
| ZIP-258-C4 | NU6.3 nodes MUST advertise at least 170,160 (Mainnet and Testnet) | `protocol.rs` `PROTOCOL_VERSION` (marked) | `protocol::tests::minimum_peer_version_follows_the_upgrade` (asserts `PROTOCOL_VERSION >= min_peer_version(_, Nu6_3)`) | `constants.rs:353` | implemented+tested |

### ZIP 258: NU6.3 rules
Class: (a) consensus. Activation: NU6.3 (Testnet 4,134,000, Mainnet 3,428,143). Constants: P1. ZIP 209 and ZIP 221 changes: P5.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-258-1 | A coinbase has no Orchard-pool actions | `draft` (`CoinbaseOrchardBundle`) | `prepare::tests::the_coinbase_rules_of_the_orchard_and_ironwood_bundles` | `check.rs:367` | implemented+tested |
| ZIP-258-2 | Every Orchard-pool action has `enableCrossAddress = 0`, in v5 and v6, enforced by the key of the branch | `draft` (`OrchardCrossAddress`); `orchard.rs` `circuit_version` (NU6.3 key for every bundle of the NU6.3 branch) | `prepare::tests::the_flag_bits_of_each_pool`; `orchard::tests::circuit_version_follows_the_branch` | `check.rs:179`; `halo2.rs:405` | implemented+tested |
| ZIP-258-3 | `valueBalanceOrchard` ≥ 0: no value enters the Orchard pool | `draft` (`OrchardPoolDeposit`) | `prepare::tests::no_value_enters_the_orchard_pool_from_nu6_3`; `ironwood.rs` `context_free_rules_reject_at_the_block_level` | `check.rs:338` | implemented+tested |
| ZIP-258-4 | Ironwood actions are consistent with `valueBalanceIronwood` (binding signature) | `shielded.rs` (upstream `BatchValidator`, NU6.3 key group) | `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | `transaction.rs:1428` | implemented+tested |
| ZIP-258-5 | `anchorIronwood` is a final Ironwood root of an earlier block | `hayai-state/src/check.rs` `check_txs` (`BadAnchor`) | `ironwood.rs` `ironwood_anchors_are_roots_of_earlier_blocks` | `anchors.rs:353` | implemented+tested |
| ZIP-258-6 | The Ironwood tree does not pass 2^32 leaves | `hayai-trees` `append_many_with_block` (`TreeError::Full`) | `hayai-trees` `tests::full_tree_is_an_error_not_a_panic` | upstream `incrementalmerkletree` | implemented+tested |
| ZIP-258-7 | An Ironwood coinbase output has lead byte 0x03 | `coinbase.rs` `ironwood_action_decrypts` (upstream `IronwoodDomain`) | `coinbase::tests::an_ironwood_coinbase_output_decrypts_only_with_the_zero_key` (decryption; no wrong-lead-byte case) | `check.rs:503` | implemented, no direct test of the lead byte |
| ZIP-258-8 | ZIP 2005 activates at NU6.3 | `rules::nu6_3`; `IronwoodDomain` | see ZIP 2005 | — | implemented+tested |
| ZIP-258-9 | Nodes advertise a protocol version ≥ 170,160 | the row ZIP-258-C4 | the row ZIP-258-C4 | the row ZIP-258-C4 | not applicable: a pointer to the row ZIP-258-C4 |

## ZIP 259: Deployment of the NU7 Network Upgrade
Class: (a) consensus, (b) network. Activation: NU7 (Testnet 4,465,026; Mainnet not set). Status Draft.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-259-1 | NU7 deploys ZIP 207 r2, ZIP 214 r3, ZIP 218, ZIP 235, ZIP 237, ZIP 2003 and ZIP 2008, and not ZIP 233 or ZIP 234 | `rules.rs` `nu7` (l. 300) and the P2 modules | rows of ZIP 218 (P1) and of P2 and P3 | `network_upgrade.rs:242-243` | implemented+tested (each ZIP in its own rows) |
| ZIP-259-2 | Testnet minimum-difficulty threshold: 18 spacings (more than 450 s) from NU7 | as ZIP-218-18 | `the_window_and_the_spacing_change_at_nu7` | `network_upgrade.rs:336` | implemented+tested |
| ZIP-259-3 | `CONSENSUS_BRANCH_ID` (NU7) = `0x77190AD9` | `hayai-crypto/src/lib.rs` `nu7_branch`; `Upgrade::branch_id` | `tests::nu7_is_known_to_the_zakura_backend_only` (`hayai-crypto/src/lib.rs`), `network::tests::the_deployment_constants_of_the_zips` | `network_upgrade.rs:243` | implemented+tested (zakura backend; the upstream backend has no NU7 and stops at the NU7 height) |
| ZIP-259-4 | `ACTIVATION_HEIGHT` (NU7): Testnet 4,465,026; Mainnet not set | `network.rs` `TESTNET_NU7_HEIGHT` (l. 639); `protocol_height` gives `None` on Mainnet | `network::tests::the_deployment_constants_of_the_zips`, `rules::tests::the_nu7_boundary_on_testnet` | `constants.rs:80,83-112` | implemented+tested |
| ZIP-259-5 | Each `ACTIVATION_HEIGHT` (NU7) MUST be a multiple of 3 | the Testnet constant 4,465,026 = 3 · 1,488,342 | `network::tests::the_deployment_constants_of_the_zips` | none (no check) | implemented+tested (constant; a configured Regtest height is not checked, as in Zakura) |
| ZIP-259-6 | `NSM_REISSUANCE_HEIGHT`: Testnet 7,305,222; Mainnet from ZIP 237 | `hayai-consensus/src/nsm.rs` `reissuance_height` (P2) | hayai-bench `conformance_nu7::the_nsm_values_match_zakura_chain` | `subsidy.rs:609-720` | implemented+tested (P2 row) |
| ZIP-259-7 | `MIN_NETWORK_PROTOCOL_VERSION` (NU7): Testnet 170,180, Mainnet 170,190 | `hayai-net/src/protocol.rs` `min_peer_version` | `protocol::tests::minimum_peer_version_follows_the_upgrade` | `types.rs:130-131` | implemented+tested |
| ZIP-259-8 | NU7 nodes MUST advertise at least 170,180 (Testnet) and 170,190 (Mainnet) | `protocol.rs` `PROTOCOL_VERSION_NU7` (line 32, marked), `protocol_version` | `protocol::tests::minimum_peer_version_follows_the_upgrade` (asserts it on a build with the NU7 rule set) | `constants.rs:353` | implemented+tested: a build without the NU7 rule set advertises 170,160 and stops before NU7 (section NU7 and the crypto backends) |
| ZIP-259-9 | From NU7, a v4 transaction is not valid (ZIP 2003) | `rules.rs` `nu7` `tx_versions` = {5, 6} | hayai-prepared `prepare::tests::a_v4_transaction_is_refused_from_nu7` | `transaction.rs:1023-1040` | implemented+tested (P3 row) |

## ZIP 271: Dev Fund Extension and One-Time Disbursement
Class: (a) consensus. Activation: NU6.1.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-271-1 | The coinbase of the NU6.1 activation block has lockbox disbursement outputs to the 2-of-3 P2SH multisig | `coinbase.rs` `CoinbaseTerms::terms` (184), `lockbox.rs` `disbursements` (51) | `coinbase::tests::each_required_output_needs_its_own_coinbase_output`; `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only` | `zk-check:276-298` | implemented+tested |
| ZIP-271-2 | `ZIP271ActivationHeight` is the NU6.1 height | `lockbox.rs` `disbursements` (`activation_height(Upgrade::Nu6_1)`) | `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only` | `zk-check:276` | implemented+tested |
| ZIP-271-3 | `ZIP271DisbursementAmount` is the lockbox balance at the end of the block before | `lockbox.rs` `nu6_1_disbursement` (a constant; the pool check is ZIP-271-11) | `coinbase::tests::the_deferred_pool_of_a_chain_pays_the_disbursement` (pool = amount on Mainnet and Testnet) | `zk-main:32`, `zk-test:41` | implemented differently: a constant, as the ZIP note asks; the computed pool equals it on both networks |
| ZIP-271-4 | `ZIP271DisbursementChunks` = 10 | `lockbox.rs` `nu6_1_disbursement` (36) | `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only` | `zk-main:25-28` | implemented+tested |
| ZIP-271-5 | `ZIP271DisbursementAddress`: t3ev37Q2… (Mainnet), t2RnBRiq… (Testnet) | `lockbox.rs` `disbursements` | `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only`; `conformance_subsidy::baselines::*` (disbursements) | `zk-main:25-28`, `zk-test:34-37` | implemented+tested |
| ZIP-271-6 | The outputs pay the amount in 10 equal outputs with the standard P2SH script of the multisig (ZIP 48) | `coinbase.rs` `CoinbaseTerms::terms`, `check`, `p2sh_script` | `coinbase::tests::each_required_output_needs_its_own_coinbase_output` (nine of ten refused) | `zk-check:285-291` | implemented+tested |
| ZIP-271-7 | The amount enters the coinbase transparent value pool and leaves the lockbox | `coinbase.rs` `payable` (235, `+ disbursed`), `deferred_pool_after` | `coinbase::tests::terms_have_the_outputs_of_each_era` (NU6.1 rows), `a_coinbase_with_every_required_output_and_the_exact_value_is_valid` (3,146,400, 3,536,500) | `zk-check:292-296,358-364` | implemented+tested |
| ZIP-271-8 | The deduction comes before other lockbox changes and must not make the lockbox negative at that point | `lockbox.rs` `deferred_pool_after` (checks `before + deferred - disbursed`) | `hayai-state` `check::tests::the_deferred_pool_pays_the_disbursement_or_the_block_fails` (accepts a pool of 78,750 ZEC − 18,750,000 zatoshis) | `value_balance.rs:218` (after the block) | implemented differently: the check is after the block, as Spec §4.17 and Zakura; no Mainnet or Testnet input differs (F-P2-2) |
| ZIP-271-9 | The amount is 78,750 ZEC on Mainnet and Testnet | `lockbox.rs` `nu6_1_disbursement` | `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only` | `zk-main:32-33`, `zk-test:41-42` | implemented+tested |
| ZIP-271-10 | §4.17: the deferred pool is the sum of `totalDeferredOutput - totalDeferredInput` | `lockbox.rs` `deferred_pool_after`; `hayai-state/src/check.rs` `value_pools_after` | `coinbase::tests::the_deferred_pool_follows_the_terms` | `value_balance.rs:30,218` | implemented+tested |
| ZIP-271-11 | §4.17: a block that makes the deferred pool negative is not valid | `lockbox.rs` `deferred_pool_after`, `coinbase.rs` `CoinbaseTerms::deferred_pool_after` (`NegativeDeferredPool`) | `coinbase::tests::the_deferred_pool_follows_the_terms`; `hayai-state` `check::tests::the_deferred_pool_pays_the_disbursement_or_the_block_fails` | `value_balance.rs:218` | implemented+tested |
| ZIP-271-12 | §7.1.2: the 100-block maturity covers the disbursement outputs | `hayai-state/src/check.rs` (coinbase maturity for each coinbase output, P5) | none for a disbursement output | `zakura-state/src/service/check/utxo.rs` (P5) | implemented, no direct test |
| ZIP-271-13 | §7.8: `totalDeferredInput(height)` is the amount at `ZIP271ActivationHeight`, else 0 | `coinbase.rs` `CoinbaseTerms::terms` (`disbursed`) | `coinbase::tests::terms_have_the_outputs_of_each_era` (`disbursed` 0 and 7,875,000,000,000) | `zk-check:276-298` | implemented+tested |
| ZIP-271-14 | §7.10: the funding stream and disbursement output rule | see SPEC-7.10-1a and SPEC-7.10-1b | see there | `zk-check:244-313` | implemented+tested |
| ZIP-271-15 | §7.10: standard redeem script hashes come from ZIP 48 for multisig, no change for older addresses | `coinbase.rs` `p2sh_script` (hash of the Base58Check address) | `coinbase::tests::address_script_is_the_p2sh_script_of_the_address` | `zk-check:54-56` | implemented+tested |
| ZIP-271-16 | §7.10: the prescribed way to pay a Sapling or Orchard address | none | none | `zk-fs:45-48` | not implemented: no recipient is shielded; effect none on current networks (SPEC-7.10-1d) |
| ZIP-271-17 | Equal funding items need distinct outputs, one for each | `coinbase.rs` `CoinbaseTerms::check` (`swap_remove` of each match) | `coinbase::tests::each_required_output_needs_its_own_coinbase_output`, `a_configured_regtest_pays_its_disbursements_at_nu6_1` | `zk-check:43-56` | implemented+tested |
| ZIP-271-18 | §7.10.1: the r2 tables of ZIP 214 | see ZIP-214-r2-11, -12 | see there | see there | implemented+tested |
| ZIP-271-19 | `FS_CCF_H3` continues the 12 % lockbox stream, ended at the third halving by ZIP 214 r3 | `funding.rs` `MAINNET[2]`, `TESTNET[2]`, `nu7_adjusted_end` | `funding::tests::the_last_testnet_streams_follow_nu7` | `zk-main:269-288` | implemented+tested |
| ZIP-271-20 | The Key-Holders make a new multisig after a key loss | none | none | none | not applicable: process rule |

## ZIP 301: Zcash Stratum Protocol
Class: (c) mining. Activation: none. hayai has no Stratum server: it serves `getblocktemplate`
and its own template push protocol (`docs/protocol-template-push.md`). Each row is not applicable
for that reason.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-301-1 | LF only at the end of a message | none | none | none | not applicable: no Stratum server |
| ZIP-301-2 | The server sets the `id` of a response to the `id` of the request | none | none | none | not applicable: no Stratum server |
| ZIP-301-3 | The traceback of an error is `null` without more information | none | none | none | not applicable: no Stratum server |
| ZIP-301-4 | `len(NONCE_1)` below 32 bytes | none | none | none | not applicable: no Stratum server |
| ZIP-301-5 | `len(NONCE_2)` = 32 - `len(NONCE_1)` | none | none | none | not applicable: no Stratum server |
| ZIP-301-6 | A bignum nonce increments by `1 << len(NONCE_1)` | none | none | none | not applicable: miner rule |
| ZIP-301-7 | A server with a `SESSION_ID` caches the session, `NONCE_1` and the jobs | none | none | none | not applicable: no Stratum server |
| ZIP-301-8 | A resumed session gets the cached response | none | none | none | not applicable: no Stratum server |
| ZIP-301-9 | An unknown session gets another `SESSION_ID` | none | none | none | not applicable: no Stratum server |
| ZIP-301-10 | The miner authorizes again after a resume | none | none | none | not applicable: miner rule |
| ZIP-301-11 | A miner authorizes a worker before it submits | none | none | none | not applicable: miner rule |
| ZIP-301-12 | `AUTHORIZED` is true on success and `null` on an error | none | none | none | not applicable: no Stratum server |
| ZIP-301-13 | The error of `mining.authorize` is `null` on success, an error object on failure | none | none | none | not applicable: no Stratum server |
| ZIP-301-14 | A valid block hash is not above the target | none | none | none | not applicable: the header rule itself is P1's (§7.7) |
| ZIP-301-15 | The server refuses a submission above the target; an older job is checked against the older target | none | none | none | not applicable: no Stratum server |
| ZIP-301-16 | A miner ignores a job with an unknown block version | none | none | none | not applicable: miner rule |
| ZIP-301-17 | The server refuses a submission of an unauthenticated worker | none | none | none | not applicable: no Stratum server |
| ZIP-301-18 | `ACCEPTED` is true on success and `null` on an error | none | none | none | not applicable: no Stratum server |
| ZIP-301-19 | The error of `mining.submit` is `null` on success, an error object on refusal | none | none | none | not applicable: no Stratum server |
| ZIP-301-20 | SHOULD rules for miners and servers (messages, `set_target` answer, reconnect) | none | none | none | not applicable: no Stratum server |

## ZIP 317: Proportional Transfer Fee Mechanism
Class: (c) mining and mempool. Activation: Revision 0 now; Revision 1 at NU6.3; Revision 2 draft.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-317-1 | Conventional fee = `marginal_fee * max(grace_actions, logical_actions)` (wallet SHOULD) | `hayai-template/src/zip317.rs` `Zip317Params::conventional_fee` (line 51) | `zip317::tests::conventional_fee_applies_grace_actions` (`hayai-template/src/zip317.rs`) | `zakura-chain/src/transaction/unmined/zip317.rs:67-76` | implemented+tested |
| ZIP-317-2 | `marginal_fee` = 5,000 zatoshis | `zip317.rs` `Zip317Params::ZAKURA` (400, line 44) | `policy::tests::unpaid_actions_boundary` (`hayai-prepared/src/policy.rs`, asserts 400) | `zip317.rs:25` (400) | implemented differently: 400, the value of Zakura, so the node relays and mines what Zakura relays and mines |
| ZIP-317-3 | `grace_actions` = 2 | `zip317.rs` `Zip317Params::ZAKURA` | `policy::tests::grace_actions_boundary` | `zip317.rs:28` | implemented+tested |
| ZIP-317-4 | `p2pkh_standard_input_size` = 150 bytes | `zip317.rs` `P2PKH_STANDARD_INPUT_SIZE` | `zip317::tests::logical_actions_count_transparent_bytes` | `zip317.rs:31` | implemented+tested |
| ZIP-317-5 | `p2pkh_standard_output_size` = 34 bytes | `zip317.rs` `P2PKH_STANDARD_OUTPUT_SIZE` | `zip317::tests::logical_actions_count_transparent_bytes`, `policy::tests::the_policy_cases_of_the_regtest_pair_have_the_verdict_of_zakura` | `zip317.rs:34` | implemented+tested |
| ZIP-317-6 | Transparent contribution: `max(ceil(tx_in_total_size / 150), ceil(tx_out_total_size / 34))` | `zip317.rs` `logical_actions` (line 107) | `zip317::tests::logical_actions_count_transparent_bytes`, `policy::tests::the_policy_cases_of_the_regtest_pair_have_the_verdict_of_zakura` | `zip317.rs:131-163` | implemented+tested |
| ZIP-317-7 | Sprout contribution: `2 * nJoinSplit` | `zip317.rs` `logical_actions` | none | `zip317.rs:131-163` | implemented, no direct test |
| ZIP-317-8 | Sapling contribution: `max(nSpendsSapling, nOutputsSapling)` | `zip317.rs` `logical_actions` | none | `zip317.rs:131-163` | implemented, no direct test |
| ZIP-317-9 | Orchard contribution: `nActionsOrchard` | `zip317.rs` `logical_actions` | `test_support::tests::shielding_tx_is_valid_and_follows_the_policy` (`hayaid/src/test_support.rs`: 1 input and 2 actions, 1,200 passes, 1,199 fails) | `zip317.rs:131-163` | implemented+tested |
| ZIP-317-r1-10 | Revision 1 (NU6.3): Ironwood contribution `nActionsIronwood` | `zip317.rs` `logical_actions` | `a_template_candidate_counts_its_ironwood_actions` (`hayai-bench/tests/ironwood.rs`) | `zip317.rs:131-163` | implemented+tested |
| ZIP-317-r2-11 | Revision 2 (draft): memo chunk contribution `max(0, nMemoChunks - free_memo_chunks)` | none | none | none | not applicable: a draft that needs ZIP 231 and ZIP 248, which are not deployed |
| ZIP-317-12 | Wallets SHOULD create transactions that pay the conventional fee | none | none | none | not applicable: hayai builds no wallet transaction |
| ZIP-317-13 | Nodes MAY drop transactions with more unpaid actions than a limit | `hayai-prepared/src/policy.rs` `MempoolPolicy::check_fee` (line 392), limit 0 | `policy::tests::unpaid_actions_boundary` | `zip317.rs:166-175` | implemented+tested |
| ZIP-317-14 | The ZIP 401 `low_fee_penalty` threshold is the conventional fee | `hayai-prepared/src/store.rs` `PreparedStore::insert_at` (P3 file) | `store::tests::an_entry_holds_the_zip_317_and_zip_401_values` | `unmined.rs:521-529` | implemented+tested |
| ZIP-317-15 | `weight_ratio_cap` = 4 | `zip317.rs` `Zip317Params::ZAKURA` (13) | `zip317::tests::weight_ratio_is_capped_and_ordered` | `zip317.rs:37` (13) | implemented differently: 13, the value of Zakura; it changes the order of the template, not a verdict |
| ZIP-317-16 | `block_unpaid_action_limit` = 50 | `zip317.rs` `BLOCK_UNPAID_ACTION_LIMIT` (0, line 21) | `live::tests::the_unpaid_action_limit_of_zero_bounds_the_second_pass` (`hayai-template/src/live.rs`) | `zip317.rs:48` (0) | implemented differently: 0, the value of Zakura and Zebra |
| ZIP-317-17 | `unpaid_actions = max(0, max(grace, logical) - floor(fee / marginal_fee))`; 0 for a coinbase | `zip317.rs` `Zip317Params::unpaid_actions` (line 70); a coinbase is never a candidate | `zip317::tests::unpaid_actions_follow_the_marginal_fee` | `zip317.rs:81-98` | implemented+tested |
| ZIP-317-18 | `block_unpaid_actions` is the sum over the block | `hayai-template/src/live.rs` `Budget` (`unpaid_actions`), `Budget::fits` (line 266) | `live::tests::the_unpaid_action_limit_of_zero_bounds_the_second_pass` | `zakura-rpc/src/methods/types/get_block_template/zip317.rs:126-140` | implemented+tested |
| ZIP-317-19 | Step 1: a coinbase placeholder; reserve its space and sigops | `live.rs` `Budget::fresh` (line 247) | `live::tests::incremental_add_matches_from_scratch_under_a_tight_byte_limit`, `live::tests::the_limits_are_the_limits_of_the_rule_set_of_the_height` | `get_block_template/zip317.rs:54` | implemented+tested |
| ZIP-317-20 | Step 2: `weight_ratio = min(max(1, fee) / conventional_fee, cap)` | `zip317.rs` `Zip317Params::weight_ratio` (line 59), fixed point | `zip317::tests::weight_ratio_is_capped_and_ordered` | `zip317.rs:103-125` (`f32`) | implemented+tested |
| ZIP-317-21 | Step 3: pick each candidate that pays the conventional fee at random by weight ratio; add it when the block stays in the size and sigop limits | `live.rs` `LiveTemplate::walk` (line 840), `hayai-template/src/candidate.rs` `OrderKey` (marked) | `live::tests::random_event_histories_match_from_scratch`, `live::tests::child_is_selected_only_after_its_parents` | `get_block_template/zip317.rs:102-124` | implemented differently: the pick follows the weight ratio order, not a random draw (RECOMMENDED; `docs/mempool-policy.md`); the same set gives the same template on every node |
| ZIP-317-22 | Step 4: the other candidates, with the unpaid action limit too | `live.rs` `LiveTemplate::walk` | `live::tests::the_unpaid_action_limit_of_zero_bounds_the_second_pass` | `get_block_template/zip317.rs:126-140` | implemented differently: the same order rule as ZIP-317-21 |
| ZIP-317-23 | Steps 3b and 4b: the block size limit and the block sigop limit | `live.rs` `Budget::fits` | `live::tests::incremental_add_matches_from_scratch_under_a_tight_byte_limit`, `live::tests::the_limits_are_the_limits_of_the_rule_set_of_the_height` | `get_block_template/zip317.rs` (`BlockTemplateLimits::try_add`) | implemented+tested |
| ZIP-317-24 | Nodes SHOULD use the conventional fee for the `low_fee_penalty` | as ZIP-317-14 | as ZIP-317-14 | as ZIP-317-14 | implemented+tested |
| ZIP-317-25 | Nodes that build templates SHOULD use the recommended algorithm | `live.rs` | as ZIP-317-21 | `get_block_template/zip317.rs:83-140` | implemented differently: as ZIP-317-21 |
| ZIP-317-26 | Mempool and relay restrictions SHOULD NOT come before the template change | none | none | none | not applicable: a rule for the deployment order of 2023 |

## ZIP 323: Specification of getblocktemplate for Zcash
Class: (c) RPC. Status Reserved: the ZIP has no text and no rule. ZIP 317 does not point to the
`getblocktemplate` text of the specification, so no rule of the specification belongs to P6.

## ZIP 401: Addressing Mempool Denial-of-Service
Class: (b) mempool. Activation: none (node policy).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-401-1 | Cost = max(memory size, 10,000); MAY use the serialized size | `hayai-prepared/src/store.rs` `MEMPOOL_COST_THRESHOLD`, `PreparedStore::insert_at` (line 330, P3 file) | `store::tests::an_entry_holds_the_zip_317_and_zip_401_values` (`hayai-prepared/src/store.rs`) | `zakura-chain/src/transaction/unmined.rs:65,497-505` | implemented+tested |
| ZIP-401-2 | Eviction weight = cost + 40,000 when the fee is below the ZIP 317 conventional fee | `store.rs` `LOW_FEE_PENALTY`, `PreparedStore::insert_at` (line 379) | `store::tests::an_entry_holds_the_zip_317_and_zip_401_values` | `unmined.rs:73,521-529` | implemented+tested |
| ZIP-401-3 | MUST keep a FIFO `RecentlyEvicted` of (txid, time), by txid and not wtxid | `store.rs` `RecentlyEvicted` (line 95) | `store::tests::an_evicted_transaction_is_refused_until_the_memory_expires` | `zakurad/src/components/mempool/storage.rs:515-530` | implemented+tested |
| ZIP-401-4 | `RecentlyEvicted` SHOULD be empty at the start | `store.rs` `PreparedStore::with_rng` (in memory only) | none | `storage.rs:240` | implemented, no direct test |
| ZIP-401-5 | `RecentlyEvicted` SHOULD hold at most 40,000 entries | `store.rs` `EVICTION_MEMORY_ENTRIES`, `RecentlyEvicted::add` (line 127) | `store::tests::the_recently_evicted_list_is_bounded` | `storage.rs:51` | implemented+tested |
| ZIP-401-6 | MUST have the setting `mempooltxcostlimit`, SHOULD default to 80,000,000 | `hayaid/src/config.rs` `MempoolSection::tx_cost_limit` (line 495); `store.rs` `MEMPOOL_TX_COST_LIMIT` | `the_output_of_zakurad_generate_gives_an_exact_report` (`hayaid/src/config.rs`, the default) | `zakurad/src/components/mempool/config.rs:19,70` | implemented+tested |
| ZIP-401-7 | MUST have the setting `mempoolevictionmemoryminutes`, SHOULD default to 60 | `store.rs` `EVICTION_MEMORY` (fixed 60 min); `hayaid/src/config.rs` lists Zakura's `mempool.eviction_memory_time` as unused | none | `zakurad/src/components/mempool/config.rs:39,72` | not implemented: the operator cannot change the time (finding F-P6-7) |
| ZIP-401-8 | MUST drop a received transaction whose txid is in `RecentlyEvicted` | `hayaid/src/mempool.rs` `Mempool::check` (line 230); `store.rs` `PreparedStore::insert_at` (line 348) | `store::tests::an_evicted_transaction_is_refused_until_the_memory_expires` | `storage.rs:879` | implemented+tested |
| ZIP-401-9 | When the total cost with the new transaction exceeds the limit, MUST call `EvictTransaction` again until it fits, with the new transaction a candidate | `store.rs` `PreparedStore::insert_at` (line 382) | `store::tests::the_total_cost_is_never_above_the_limit` | `storage.rs:515-530` | implemented+tested |
| ZIP-401-10 | `EvictTransaction` MUST select at random in proportion to the eviction weight | `store.rs` `PreparedStore::select_victim` (line 418) | `store::tests::eviction_follows_the_eviction_weights` | `zakurad/src/components/mempool/storage/verified_set.rs:221-260` | implemented+tested |
| ZIP-401-11 | MUST add the txid and the time to `RecentlyEvicted`, with the oldest entry out when full | `store.rs` `PreparedStore::insert_at` (lines 385, 389), `RecentlyEvicted::add` | `store::tests::the_recently_evicted_list_is_bounded` | `storage.rs:526-530` | implemented+tested |
| ZIP-401-12 | MUST remove the selected transaction from the mempool | `store.rs` `PreparedStore::insert_at` | `store::tests::eviction_removes_the_descendants_of_the_victim`, `store::tests::eviction_never_selects_an_ancestor_of_the_new_transaction` | `verified_set.rs:225` | implemented differently: the descendants of the victim leave with it, and an ancestor of the new transaction is not a candidate (`docs/mempool-policy.md`); only the victim goes to `RecentlyEvicted`, as Zakura |
| ZIP-401-13 | SHOULD remove entries older than `mempoolevictionmemoryminutes` | `store.rs` `RecentlyEvicted::prune` (line 111) | `store::tests::an_evicted_transaction_is_refused_until_the_memory_expires` | `storage.rs` (`EvictionList`) | implemented+tested |

## ZIP 1014: Establishing a Dev Fund for ECC, ZF, and Major Grants
Class: (a) consensus (allocation only; the rest is process). Activation: Canopy (through ZIP 214 r0).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-1014-1 | From the first to the second halving, 20 % of the subsidy goes to the Dev Fund: BP 35 %, ZF 25 %, MG 40 % | `funding.rs` `MAINNET[0]` (7, 5, 8 of 100) | `funding::tests::mainnet_values_match_the_zakura_test_values` | `zk-main:235-252` | implemented+tested |
| ZIP-1014-2 | Similar changes apply to Testnet (SHOULD) | `funding.rs` `TESTNET[0]` | `funding::tests::testnet_streams_have_their_ranges_and_values` | `zk-test:213-231` | implemented+tested |
| ZIP-1014-3 | ZF receives and administers the MG slice | none | none | none | not applicable: process rule |
| ZIP-1014-4 | ZF disburses MG funds for Major Grants and their administration | none | none | none | not applicable: process rule |
| ZIP-1014-5 | MG funds go only to independent grantees and administration | none | none | none | not applicable: process rule |
| ZIP-1014-6 | MG funds do not pay ZF internal operations | none | none | none | not applicable: process rule |
| ZIP-1014-7 | The Community Advisory Panel selects the Major Grant Review Committee | none | none | none | not applicable: process rule |
| ZIP-1014-8 | Committee members have a one-year term | none | none | none | not applicable: process rule |
| ZIP-1014-9 | Committee members recuse themselves on a financial interest | none | none | none | not applicable: process rule |
| ZIP-1014-10 | ZF operates the Community Advisory Panel | none | none | none | not applicable: process rule |
| ZIP-1014-11 | The Panel decides the Discretionary Budget amount | none | none | none | not applicable: process rule |
| ZIP-1014-12 | The Committee approves each Discretionary Budget disbursement | none | none | none | not applicable: process rule |
| ZIP-1014-13 | ZF treats the MG slice as a Restricted Fund | none | none | none | not applicable: process rule |
| ZIP-1014-14 | ZF defines target metrics | none | none | none | not applicable: process rule |
| ZIP-1014-15 | Direct-grant option: ZF publishes grantee addresses; ECC and ZF implement them | none | none | none | not applicable: the option was never used (ZIP 214) |
| ZIP-1014-16 | BP, ECC, ZF and grantees accept the obligations | none | none | none | not applicable: process rule |
| ZIP-1014-17 | They disclose conflicts and the listed facts promptly | none | none | none | not applicable: process rule |
| ZIP-1014-18 | They disclose security and privacy risks | none | none | none | not applicable: process rule |
| ZIP-1014-19 | BP, ECC and ZF commit contractually to the conditions | none | none | none | not applicable: process rule |
| ZIP-1014-20 | ZF board members hold no ECC equity | none | none | none | not applicable: process rule |

## ZIP 1015: Block Subsidy Allocation for Non-Direct Development Funding
Class: (a) consensus (allocation only; the rest is process). Activation: NU6.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-1015-1 | 12 % of the subsidy goes to the lockbox | `funding.rs` `lockbox_streams` (114) | `subsidy::tests::the_deferred_part_is_the_lockbox_stream` | `zk-main:254-267` | implemented+tested |
| ZIP-1015-2 | 8 % of the subsidy goes to FPF for ZCG | `funding.rs` `lockbox_streams` | `funding::tests::mainnet_values_match_the_zakura_test_values` | `zk-main:254-267` | implemented+tested |
| ZIP-1015-3 | Mainnet streams 2,726,400 to 3,146,400 | `funding.rs` `MAINNET[1]` | `funding::tests::mainnet_values_match_the_zakura_test_values` | `zk-main:254-267` | implemented+tested |
| ZIP-1015-4 | Testnet streams 2,976,000 to 3,396,000 | `funding.rs` `TESTNET[1]` | `funding::tests::testnet_streams_have_their_ranges_and_values` | `zk-test:232-246` | implemented+tested |
| ZIP-1015-5 | FPF receives and administers the ZCG funds | none | none | none | not applicable: process rule |
| ZIP-1015-6 | FPF disburses for grants and administration | none | none | none | not applicable: process rule |
| ZIP-1015-7 | The funds go only to ZCG grants | none | none | none | not applicable: process rule |
| ZIP-1015-8 | The funds do not pay FPF internal operations | none | none | none | not applicable: process rule |
| ZIP-1015-9 | The Panel selects the ZCG Committee | none | none | none | not applicable: process rule |
| ZIP-1015-10 | Elections are staggered | none | none | none | not applicable: process rule |
| ZIP-1015-11 | Committee members have a one-year term | none | none | none | not applicable: process rule |
| ZIP-1015-12 | Committee members recuse themselves on a financial interest | none | none | none | not applicable: process rule |
| ZIP-1015-13 | The Panel decides the Discretionary Budget amount | none | none | none | not applicable: process rule |
| ZIP-1015-14 | The ZCG Committee approves each Discretionary Budget disbursement | none | none | none | not applicable: process rule |
| ZIP-1015-15 | Committee compensation is limited to the needed hours | none | none | none | not applicable: process rule |
| ZIP-1015-16 | FPF administers the compensation | none | none | none | not applicable: process rule |
| ZIP-1015-17 | The Panel sets the compensation rate and hours | none | none | none | not applicable: process rule |
| ZIP-1015-18 | FPF treats the ZCG slice as a Restricted Fund | none | none | none | not applicable: process rule |
| ZIP-1015-19 | ZCG defines target metrics | none | none | none | not applicable: process rule |
| ZIP-1015-20 | FPF reviews the ZCG program periodically | none | none | none | not applicable: process rule |
| ZIP-1015-21 | FPF explores an independent ZCG | none | none | none | not applicable: process rule |
| ZIP-1015-22 | A transition gives priority to decentralization | none | none | none | not applicable: process rule |
| ZIP-1015-23 | An independent organization keeps community-driven decisions | none | none | none | not applicable: process rule |
| ZIP-1015-24 | FPF accepts the obligations for ZCG | none | none | none | not applicable: process rule |
| ZIP-1015-25 | The parties disclose security and privacy risks | none | none | none | not applicable: process rule |
| ZIP-1015-26 | FPF commits contractually to the obligations | none | none | none | not applicable: process rule |

## ZIP 1016: Community and Coinholder Funding Model
Class: (a) consensus (allocation only; the rest is process). Activation: NU6.1 (through ZIP 214 r2), NU7 end (ZIP 214 r3).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-1016-1 | ZCG stream of 8 % from the end of `FS_FPF_ZCG` to 4,406,400, or to `A + 3 (4,406,400 − A)` with NU7 | `funding.rs` `MAINNET[2]`, `nu7_adjusted_end` | `funding::tests::mainnet_values_match_the_zakura_test_values`, `only_an_end_above_the_nu7_height_moves` | `zk-main:269-288` | implemented+tested |
| ZIP-1016-2 | Coinholder-Controlled Fund: the lockbox contents (ZIP 271) and a 12 % stream for the same period | `funding.rs` `lockbox_streams`; `lockbox.rs` | `subsidy::tests::the_deferred_part_is_the_lockbox_stream`; `coinbase::tests::the_deferred_pool_of_a_chain_pays_the_disbursement` | `zk-main:269-288` | implemented+tested |
| ZIP-1016-3 | A legal agreement binds the Key-Holder Organizations | none | none | none | not applicable: process rule |
| ZIP-1016-4 | The ZIP 1015 lockbox use rules apply to the fund | none | none | none | not applicable: process rule |
| ZIP-1016-5 | A coinholder vote needs 420,000 ZEC | none | none | none | not applicable: process rule |
| ZIP-1016-6 | Payments of a vetoed grant stop | none | none | none | not applicable: process rule |
| ZIP-1016-7 | A veto has a rationale (SHOULD) | none | none | none | not applicable: process rule |
| ZIP-1016-8 | The ZIP 1015 provisions continue for ZCG | none | none | none | not applicable: process rule |
| ZIP-1016-9 | The organizations safeguard the funds | none | none | none | not applicable: process rule |
| ZIP-1016-10 | A loss or key compromise is reported | none | none | none | not applicable: process rule |
| ZIP-1016-11 | Testnet rehearses the voting process (SHOULD) | none | none | none | not applicable: process rule |

## ZIP 2001: Lockbox Funding Streams
Class: (a) consensus. Activation: NU6.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-2001-1 | Each element of `fs.Recipients` is a P2SH address, a Sapling address, or `DEFERRED_POOL` | see ZIP-207-r1-4 | see there | see there | implemented+tested |
| ZIP-2001-2 | Full nodes track `ChainValuePoolBalance^Deferred` | see ZIP-207-r1-7 | see there | see there | implemented+tested |
| ZIP-2001-3 | `totalDeferredOutput(height)` is the sum of the `DEFERRED_POOL` stream values | `funding.rs` `deferred_value` (365); `coinbase.rs` `CoinbaseTerms::terms` | `subsidy::tests::the_deferred_part_is_the_lockbox_stream` | `zk-check:268-271` | implemented+tested |
| ZIP-2001-4 | The output rule applies to each active stream other than `DEFERRED_POOL` | see ZIP-207-r1-14 | see there | see there | implemented+tested |
| ZIP-2001-5 | `fs.Recipient(height)` definition | see ZIP-207-11 | see there | see there | implemented+tested |
| ZIP-2001-6 | `IssuedSupply` is the sum of the chain value pools, the deferred pool included | `hayai-state/src/check.rs` `ValuePools::total` (P5), read by `nsm::balance` | `hayai-state` `check::tests::the_nsm_rules_apply_from_the_block_before_nu7` | `value_balance.rs:449` | implemented+tested |
| ZIP-2001-7 | Coinbase total output value: transparent outputs − `vbalanceSapling` − `vbalanceOrchard` + `totalDeferredOutput`; total input value: subsidy + fees | `coinbase.rs` `CoinbaseTerms::check`, `payable` | `coinbase::tests::value_that_enters_a_shielded_pool_is_paid_value`, `from_nu6_the_value_is_exact` | `zk-check:358-368` | implemented+tested |
| ZIP-2001-8 | Pre-NU6 at most, NU6 onward equal (with ZIP 236) | see ZIP-236-1, ZIP-236-2 | see there | see there | implemented+tested |

## ZIP 2003: Disallow version 4 transactions
Class: (a) consensus. Activation: NU7 (Testnet 4,465,026 in Zakura).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-2003-1 | From NU7 the version is 5 or 6 | `check_version` (`RuleSet::tx_versions` of `rules::nu7`) | `prepare::tests::a_v4_transaction_is_refused_from_nu7` | `zakura-consensus/src/transaction.rs:1023-1040` | implemented+tested (zakura backend; the upstream backend stops at NU7) |

## ZIP 2005: Ironwood Quantum Recoverability
Class: (a) consensus. Activation: NU6.3 (ZIP 258).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-2005-1 | Every Ironwood output note uses the note plaintext format with lead byte 0x03 | coinbase outputs: `coinbase.rs` (`IronwoodDomain`); other outputs are encrypted for their recipient | `an_ironwood_coinbase_output_decrypts_only_with_the_zero_key` | `zcash_note_encryption.rs` | implemented+tested for coinbase outputs; not applicable for other outputs (a node cannot decrypt them) |
| ZIP-2005-2 | Decryption derives `esk`, `rcm`, `ψ` from `rseed` and ρ by the new rules | upstream `orchard` `IronwoodDomain` (OVK recovery of coinbase outputs) | the same test | the same | implemented+tested |
| ZIP-2005-3 | Key derivation, FROST and hardware wallet rules (`qsk`, `qk`) | — | — | — | not applicable: wallet rules |

## ZIP 2006: Restricting Transfers into the Orchard Pool
Class: (a) consensus (Reserved; its rules are in ZIP 229 and ZIP 258). Activation: NU6.3.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-2006-1 | The ZIP text is reserved; ZIP 258 rows ZIP-258-2 and ZIP-258-3 hold the rules | — | — | — | not applicable: no text |

## ZIP 2008: Update to `FS_FPF_ZCG_H3` address list
Class: (a) consensus. Activation: NU7 (draft, deployed by ZIP 259).

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| ZIP-2008-1 | Mainnet `FS_FPF_ZCG_H3.AddressList[N..35]` = t1MkHnkx… (P2PKH), `N = AddressIndex(A − 1) + 1` | none (`funding.rs` module doc names the gap) | `funding::tests::zip_2008_has_no_code_while_mainnet_has_no_nu7_height` fails when Mainnet gets an NU7 height | `zk-main:196-221`, `zk-check:600-618` | not implemented: Mainnet has no NU7 height, so no block reaches the rule today (F-P2-1) |
| ZIP-2008-2 | Implementations MAY use a fixed array once the height is known | none | none | `zk-main:196-221` (computes N) | not applicable: MAY |

## Spec §3.4: Transactions and Treestates
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.4-1 | The remaining value of the transparent transaction value pool is not negative | `hayai-prepared/src/prepare.rs` `fee` (`NegativeFee`); coinbase: `hayai-consensus` `CoinbaseTerms::check` | `prepare::tests::fee_is_inputs_minus_outputs_with_bounds` | `zakura-consensus/src/transaction.rs:766` (`miner_fee`) | implemented+tested |
| SPEC-3.4-M1 | From NU6 the coinbase consumes the whole available balance | `CoinbaseTerms::check` (P2) | P2 rows of ZIP 236 | `zakura-consensus/src/block/check.rs:326` | implemented+tested |

## Spec §3.5: JoinSplit Transfers and Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.5-1 | The anchor of the first JoinSplit is the output Sprout treestate of an earlier block | `hayai-state/src/check.rs` `check_sprout_anchors` | `hayai-bench/tests/sprout.rs` `a_sprout_anchor_is_an_earlier_final_treestate_or_an_interstitial_treestate` | `zakura-state/src/service/check/anchors.rs:424` | implemented+tested |
| SPEC-3.5-2 | Each JoinSplit anchor is a final Sprout treestate of an earlier block or an interstitial treestate of an earlier JoinSplit of the transaction | `check_sprout_anchors` | the same test | `anchors.rs:424` | implemented+tested |

## Spec §3.6: Spend Transfers, Output Transfers, and their Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.6-1 | Spends and outputs are consistent with `valueBalanceSapling` (binding signature) | `hayai-prepared/src/sapling.rs` `verify_sapling` (upstream `BatchValidator`) | `hayai-bench/tests/prepared.rs` `every_fixture_transaction_prepares`; `conformance_blocks.rs` | `transaction.rs:1344` | implemented+tested |
| SPEC-3.6-2 | Each Spend anchor is a final Sapling treestate of an earlier block | `check_txs` (`BadAnchor`) | `state.rs` `unknown_anchor_is_rejected` | `anchors.rs:353` | implemented+tested |

## Spec §3.7: Action Transfers and their Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.7-1 | Orchard actions are consistent with `valueBalanceOrchard` (binding signature) | `hayai-prepared/src/orchard.rs` `verify_orchard` (upstream `BatchValidator`) | `prepared.rs` `bisection_isolates_the_tampered_orchard_bundle` | `transaction.rs:1428` | implemented+tested |
| SPEC-3.7-2 | `anchorOrchard` is a final Orchard treestate of an earlier block | `check_txs` (`BadAnchor`) | `state.rs` `unknown_anchor_is_rejected` | `anchors.rs:353` | implemented+tested |
| SPEC-3.7-3 | Ironwood actions are consistent with `valueBalanceIronwood` | `verify_orchard` (NU6.3 group) | `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | `transaction.rs:1428` | implemented+tested |
| SPEC-3.7-4 | `anchorIronwood` is a final Ironwood treestate of an earlier block | `check_txs` | `ironwood.rs` `ironwood_anchors_are_roots_of_earlier_blocks` | `anchors.rs:353` | implemented+tested |

## Spec §3.8: Note Commitment Trees
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.8-1 | A block MUST NOT add Sprout commitments past 2^29 leaves | `hayai-trees/src/sprout.rs` `SproutFrontier::append_many` (l. 117, `TreeError::Full`), mapped to `ContextError::Tree` by `check.rs` `append_leaves` | `hayai-trees` `sprout::tests::a_full_tree_takes_no_leaf` | `zakura-chain/src/sprout/tree.rs:262-274` | implemented+tested |
| SPEC-3.8-2 | [Sapling onward] A block MUST NOT add Sapling commitments past 2^32 leaves | `hayai-trees/src/batch.rs` `append_many_with_block` (l. 63) | `hayai-trees` `tests::a_full_sapling_tree_is_an_error` (added in this change) | `zakura-chain/src/sapling/tree.rs:227-239,253-271` | implemented+tested |
| SPEC-3.8-3 | [NU5 onward] A block MUST NOT add Orchard commitments past 2^32 leaves | `batch.rs` `append_many_with_block` | `hayai-trees` `tests::full_tree_is_an_error_not_a_panic` | `zakura-chain/src/orchard/tree.rs:490-502,516` | implemented+tested |
| SPEC-3.8-4 | [NU6.3 onward] A block MUST NOT add Ironwood commitments past 2^MerkleDepth^Orchard leaves | `IronwoodFrontier` is `OrchardFrontier` (`hayai-trees/src/lib.rs`), same check | `tests::full_tree_is_an_error_not_a_panic` (the same type) | `zakura-chain/src/ironwood.rs:8-11` (the Orchard tree) | implemented+tested |

## Spec §3.9: Nullifier Sets
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.9-1 | A nullifier MUST NOT repeat in a transaction or across transactions of the chain; Sprout, Sapling, Orchard and Ironwood nullifiers are disjoint | `check.rs` `check_txs` (l. 520: block set, then `ChainView::contains_nullifier_many`, `DuplicateNullifier`); per-pool sets `hayai-coins/src/nullifiers.rs` `NullifierStore`; in-transaction duplicate also in hayai-prepared (P3, P4); checkpoint path `checkpoint_layer` (in the block only) | `state.rs` `duplicate_nullifiers_are_rejected_in_view_and_in_block`; `sprout.rs` `a_sprout_nullifier_is_revealed_once`; `ironwood.rs` `ironwood_nullifiers_are_unique_in_the_block_the_layers_and_the_base`, `a_nu6_3_block_validates_cold_and_warm_to_identical_layers` (disjoint from Orchard); `checkpoint.rs` `the_checkpoint_path_refuses_a_nullifier_twice_in_the_block` (Sapling, Orchard); `hayai-coins` `store.rs` `nullifier_sets_are_per_pool_and_persist_on_flush`; `hayai-state` `tests::index_against_walk` | `zakura-state/src/service/check/nullifier.rs:37-68,148,185-260` | implemented+tested (checkpoint path: against the chain by the checkpoint hash; shadow seed: trust limit, F-P5-1) |

## Spec §3.10: Block Subsidy, Funding Streams, and Founders' Reward
No rule marker and no MUST sentence (protocol.tex 4380-4395).

## Spec §3.11: Coinbase Transactions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-3.11-M1 | [Pre-Canopy] the coinbase pays the founders' reward (§7.9) | see SPEC-7.9-1 | see there | `zk-check:207-229` | checkpoint path only |
| SPEC-3.11-M2 | [Canopy onward] the coinbase pays the funding streams (§7.10) | see SPEC-7.10-1a | see there | `zk-check:244-313` | implemented+tested |

## Spec §4.3: JoinSplit Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.3-1 | Elements have their types; `vpub_old`, `vpub_new` in 0..MAX_MONEY | upstream parser (`JsDescription::read`) | `scan.rs` `generated_transactions_of_every_version` | `zakura-chain` serializer | implemented, no direct test |
| SPEC-4.3-2 | The JoinSplit proof is valid (Groth16 from Sapling; BCTV14 before) | `sprout.rs` `joinsplit_proof_is_valid`; BCTV14 `Unsupported` (`shielded.rs` `add`) | `sprout.rs` `a_wrong_proof_is_not_valid`; `hayai-bench/tests/sprout.rs` `the_embedded_key_verifies_the_published_joinsplits`, `a_changed_joinsplit_transaction_fails`; `shielded::tests::a_bctv14_joinsplit_is_an_error_and_a_wrong_groth16_joinsplit_fails` | `transaction.rs:1275` | implemented+tested (BCTV14: checkpoint path only) |
| SPEC-4.3-3 | `vpub_old` or `vpub_new` is zero | `sprout.rs` `check_joinsplits` (`JoinSplitBothVpub`) | `prepare::tests::one_of_vpub_old_and_vpub_new_is_zero` | `check.rs:279` | implemented+tested |
| SPEC-4.3-4 | Canopy on: `vpub_old` is zero | `check_joinsplits` (`SproutPoolDeposit`) | `prepare::tests::no_value_enters_the_sprout_pool_from_canopy` | `check.rs:304` | implemented+tested |

## Spec §4.4: Spend Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.4-1 | Elements are valid encodings of their types | upstream parser (`read_spend_v4`, `read_spend_v5`) | `scan.rs` `corrupted_transactions_never_panic` | serializer | implemented, no direct test |
| SPEC-4.4-2 | `cv` and `rk` are not of small order | `cv`: upstream parser (`from_bytes_not_small_order`); `rk`: `sapling-crypto` 0.7.0 `verifier.rs:49` | none | `check.rs:209` | implemented, no direct test |
| SPEC-4.4-3 | The Spend proof is valid | `sapling.rs` `verify_sapling` (`BatchValidator`) | `sapling::tests::the_embedded_keys_verify_a_proof_of_the_official_parameters`; block vectors | `transaction.rs:1344` | implemented+tested |
| SPEC-4.4-4 | `spendAuthSig` is valid over the shielded sighash; `R` canonical | `verify_sapling` | block vectors | `transaction.rs:1344` | implemented+tested |

## Spec §4.5: Output Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.5-1 | Elements are valid encodings of their types | upstream parser | `scan.rs` `corrupted_transactions_never_panic` | serializer | implemented, no direct test |
| SPEC-4.5-2 | `cv` and `epk` are not of small order | `cv`: parser; `epk`: `sapling-crypto` `verifier.rs:108` | none | `check.rs:209` | implemented, no direct test |
| SPEC-4.5-3 | The Output proof is valid | `verify_sapling` | block vectors | `transaction.rs:1344` | implemented+tested |

## Spec §4.6: Action Descriptions
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.6-1 | Elements are canonical encodings of their types | upstream parser (`read_action_without_auth`, `Action::from_parts`) | `scan.rs` `corrupted_transactions_never_panic` | serializer | implemented, no direct test |
| SPEC-4.6-2 | `spendAuthSig` is valid over the shielded sighash; `R` canonical | `orchard.rs` `verify_orchard` | `prepared.rs` `bisection_isolates_the_tampered_orchard_bundle` | `transaction.rs:1428` | implemented+tested |
| SPEC-4.6-3 | NU5 to NU6.1: `vk` is `InsecurePreNU6_2` | `orchard.rs` `circuit_version` | `orchard::tests::circuit_version_follows_the_branch` | `halo2.rs:405` | implemented+tested |
| SPEC-4.6-4 | NU6.2: `vk` is `FixedPostNU6_2` | `circuit_version` | the same test | `halo2.rs:405` | implemented+tested |
| SPEC-4.6-5 | NU6.3 on: `vk` is `PostNU6_3` | `circuit_version` | the same test | `halo2.rs:405` | implemented+tested |
| SPEC-4.6-6 | The proof is valid for `vk` with the primary input (cv, rt, nf, rk, cmx, enableSpends, enableOutputs) | `shielded.rs` `ScopedBatch::add`, `finalize` | `prepared.rs` `bisection_isolates_the_tampered_orchard_bundle`; `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | `transaction.rs:1428` | implemented+tested |
| SPEC-4.6-7 | `rk` is not the identity point | upstream `orchard` 0.15.5 `Action::from_parts` | none | Zakura `orchard` fork | implemented, no direct test |

## Spec §4.10: SIGHASH Transaction Hashing
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.10-M1 | Orchard-pool actions need a v5 or later transaction | the formats: v4 has no Orchard field | `scan.rs` | `serialize.rs` | implemented, no direct test |
| SPEC-4.10-M2 | Ironwood-pool actions need a v6 transaction | the formats: v5 has no Ironwood field | `scan.rs` | `serialize.rs` | implemented, no direct test |
| SPEC-4.10-1 | v5 and later: the hash type is canonical (0x01-0x03, 0x81-0x83) | `SighashContext::transparent` | none | `zakura-script/src/lib.rs:213` | implemented, no direct test |
| SPEC-4.10-2 | v1, v2 sighash (ZIP 76, not written) | `check_version` (`Unsupported`) | `prepare::tests::a_version_before_sapling_is_not_verified` | `transaction.rs:610` | checkpoint path only |
| SPEC-4.10-3 | v3 sighash is ZIP 143 | `check_version` (`Unsupported`) | the same test | `transaction.rs:610` | checkpoint path only |
| SPEC-4.10-4 | v4 sighash is ZIP 243 | `SighashContext` | block vectors | `zakura-script/src/lib.rs:199` | implemented+tested |
| SPEC-4.10-5 | v5 sighash is ZIP 244 | `SighashContext` | block vectors | `sighash.rs` | implemented+tested |
| SPEC-4.10-6 | v6 sighash is ZIP 229 | `SighashContext` (upstream `sighash_v6.rs`) | `hayai-bench/tests/ironwood.rs` | `sighash.rs` | implemented+tested |
| SPEC-4.10-7..16 | In each epoch every transaction uses the branch id of the epoch (Overwinter 0x5BA81B19, Sapling 0x76B809BB, Blossom 0x2BB40E60, Heartwood 0xF5B9230B, Canopy 0xE9FF75A6, NU5 0xC2D6D0B4 (ZIP 252; the text of §4.10 says 0xF919A198, finding F-P3-3), NU6 0xC8E71055, NU6.1 0x4DEC4DF0, NU6.2 0x5437F330, NU6.3 0x37A5165B) | `hayai-wire` parses with the branch id of the height (`RawTx::parse`); `check_version` (`BranchId`) for v5 and v6; the sighash of v3 and v4 takes the branch of the parse | `prepare::tests` (`BranchId` error), block vectors | `zakura-consensus/src/transaction/check.rs:930` | implemented+tested (Overwinter: checkpoint path only) |

## Spec §4.17: Chain Value Pool Balances
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-4.17-1 | The transparent pool (value of the UTXO set) MUST NOT become negative | `value_pools_after` (`transparent_change` = outputs minus spent coins; `NegativeTransparentPool`) | `check::tests::no_value_pool_can_be_negative` | `value_balance.rs:360-369` | implemented+tested |
| SPEC-4.17-2 | The Sprout pool MUST NOT become negative | `value_pools_after`, `sprout_balance` | the same; `sprout.rs` `the_sprout_pool_is_never_negative` | `value_balance.rs:360` | implemented+tested |
| SPEC-4.17-3 | The Sapling pool MUST NOT become negative | `value_pools_after` | `check::tests::no_value_pool_can_be_negative` | `value_balance.rs:360` | implemented+tested |
| SPEC-4.17-4 | The Orchard pool MUST NOT become negative | `value_pools_after` | the same | `value_balance.rs:360` | implemented+tested |
| SPEC-4.17-5 | [NU6.1] The deferred pool (sum of totalDeferredOutput minus totalDeferredInput) MUST NOT become negative | `CoinbaseTerms::deferred_pool_after` (P2 file) from `value_pools_after` | `check::tests::the_deferred_pool_pays_the_disbursement_or_the_block_fails` | `value_balance.rs:360` | implemented+tested |
| SPEC-4.17-6 | [NU6.3] The Ironwood pool MUST NOT become negative | `value_pools_after` | `check::tests::no_value_pool_can_be_negative`; `ironwood.rs` `the_ironwood_pool_does_not_go_negative` | `value_balance.rs:360` | implemented+tested |
| SPEC-4.17-7 | IssuedSupply (sum of the six pools, without overflow) MUST NOT be above MAX_MONEY | `value_pools_after` (`checked_sum`); `hayai-state/src/lib.rs` `ValuePools::total` | `check::tests::the_total_of_the_pools_is_bounded` | `value_balance.rs:371-372` | implemented+tested |

## Spec §7.1: Transaction Encoding and Consensus, and §7.1.1 Transaction Identifiers
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.1-M1 | With `effectiveVersion` ≥ 5 the rest of the transaction is parsed in the v5 format | upstream `Transaction::read`; `scan.rs` `scan_v5_v6` | `scan.rs` `upstream_transaction_vectors` | `serialize.rs` | implemented+tested |
| SPEC-7.1-M2 | The rules of each JoinSplit, Spend, Output and Action description apply | P4 | P4 | P4 | not applicable: a pointer to the rows of §4.3-§4.6 and §7.2-§7.5 |
| SPEC-7.1-M3 | Before Overwinter a version above 2 is treated as version 2 | `check_version` (`Unsupported` for v1 to v3) | `prepare::tests::a_version_before_sapling_is_not_verified` | `transaction.rs:610` | checkpoint path only |
| SPEC-7.1-M4 | Version 0x7FFFFFFF and group 0xFFFFFFFF are not used on Mainnet and Testnet | upstream `TxVersion::read` (unknown format) | `scan.rs` `random_bytes_never_panic` | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.1-1 | The txid of v4 and before is SHA-256d of the encoding | upstream `Transaction::read` | `hayai-wire/tests/vectors.rs` `merkle_root_matches_header` | `zakura-chain/src/transaction/hash.rs` | implemented+tested |
| SPEC-7.1.1-2 | The txid of v5 is ZIP 244; of v6 is ZIP 229 | upstream `TxIdDigester` | `scan.rs` `upstream_transaction_vectors` | `txid.rs` | implemented+tested |
| SPEC-7.1.1-3 | v5 and later have a wtxid (ZIP 239) | `hayai-wire/src/lib.rs` `RawTx::wtxid` | `lib.rs` `wtxid_bytes_are_txid_then_digest` | `zakura-chain/src/transaction/unmined.rs` | implemented+tested |

## Spec §7.1.2: Transaction Consensus Rules
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.1.2-1 | The version number is ≥ 1 | upstream `TxVersion::read`; `scan.rs` `scan` | `scan.rs` `random_bytes_never_panic` | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.2-2 | Pre-Overwinter: `fOverwintered` is not set | v1-v3 `Unsupported` | `a_version_before_sapling_is_not_verified` | `transaction.rs:610` | checkpoint path only |
| SPEC-7.1.2-3 | From Overwinter: `fOverwintered` is set | `check_version` (`Sprout(n)` not in `tx_versions`) | `the_rule_set_names_the_transaction_versions` | `transaction.rs:610` | implemented+tested |
| SPEC-7.1.2-4 | From Overwinter: the version group is recognized | upstream `TxVersion::read` | `scan.rs` `random_bytes_never_panic` | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.2-5 | Overwinter only: version 3, group 0x03C48270 | `check_version` (`Unsupported`) | `a_version_before_sapling_is_not_verified` | `transaction.rs:610` | checkpoint path only |
| SPEC-7.1.2-6 | Sapling to Canopy: version 4 | `check_version` (`RuleSet::tx_versions`) | `the_rule_set_names_the_transaction_versions` | `transaction.rs:987-1100` | implemented+tested (Sapling to Blossom below the mandatory checkpoint: checkpoint path only) |
| SPEC-7.1.2-7 | NU5 to NU6.2: version 4 or 5 | `check_version` | the same test | `transaction.rs:1101-1180` | implemented+tested |
| SPEC-7.1.2-8 | From NU6.3: version 4, 5 or 6 | `check_version` | the same test | `transaction.rs:1181-1240` | implemented+tested |
| SPEC-7.1.2-9 | Version 4: group 0x892F2085 | upstream `TxVersion::read` | `scan.rs` `generated_transactions_of_every_version` | `serialize.rs` | implemented+tested |
| SPEC-7.1.2-10 | Version 5: group 0x26A7270A | upstream `TxVersion::read` | the same test | `serialize.rs` | implemented+tested |
| SPEC-7.1.2-11 | Version 6: group 0xD884B698 | upstream `TxVersion::read` | the same test | `serialize.rs` | implemented+tested |
| SPEC-7.1.2-12 | `effectiveVersion` ≥ 5: `nConsensusBranchId` equals the branch of the sighash | `check_version` (`BranchId`) | `prepare::tests` (`PrepareError::BranchId`, lines 940, 1689) | `check.rs:930` | implemented+tested |
| SPEC-7.1.2-13 | Pre-Sapling: the size is ≤ 100,000 bytes | v1-v3 `Unsupported` | — | `transaction.rs:610` | checkpoint path only |
| SPEC-7.1.2-14 | NU5 on: `nSpendsSapling`, `nOutputsSapling`, `nActionsOrchard` < 2^16 | block size limit (`MAX_BLOCK_BYTES`) | `oversized_block_is_rejected_before_parsing` | `serialize.rs` | implemented differently: 2^16 spends need 6.3 MB, outputs 49 MB, actions 53 MB, and a block has at most 2 MB |
| SPEC-7.1.2-15 | NU6.3 on, v6: `nActionsIronwood` < 2^16 | block size limit | the same test | `serialize.rs` | implemented differently: the same reason |
| SPEC-7.1.2-16 | Pre-Sapling: v1 or no JoinSplit needs inputs and outputs | v1-v3 `Unsupported` | — | `transaction.rs:610` | checkpoint path only |
| SPEC-7.1.2-17 | Sapling on, v < 5: one of `tx_in_count`, `nSpendsSapling`, `nJoinSplit` is nonzero | `draft` (`NoSource`) | `a_source_and_a_sink_of_funds_respect_the_enable_flags`, `a_joinsplit_is_a_source_and_a_sink` | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-18 | Sapling on, v < 5: one of `tx_out_count`, `nOutputsSapling`, `nJoinSplit` is nonzero | `draft` (`NoSink`) | the same tests | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-19 | v5: inputs, Sapling spends, or Orchard actions with `enableSpends` | `draft` (`NoSource`) | `a_source_and_a_sink_of_funds_respect_the_enable_flags` | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-20 | v5: outputs, Sapling outputs, or Orchard actions with `enableOutputs` | `draft` (`NoSink`) | the same test | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-21 | v6: the source rule with Ironwood | `draft` (`NoSource`) | the same test | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-22 | v6: the sink rule with Ironwood | `draft` (`NoSink`) | the same test; `ironwood.rs` | `check.rs:131` | implemented+tested |
| SPEC-7.1.2-23 | Orchard actions need `enableSpends` or `enableOutputs` | `draft` (`OrchardFlags`) | `actions_need_an_enable_flag` | `check.rs:150` | implemented+tested |
| SPEC-7.1.2-24 | Ironwood actions need `enableSpends` or `enableOutputs` | `draft` (`IronwoodFlags`) | the same test | `check.rs:165` | implemented+tested |
| SPEC-7.1.2-25 | Mainnet 3,363,426 / Testnet 4,048,500 until NU6.2: no Orchard actions | `hayai-consensus` `rules_at`; `hayai-state/src/check.rs` `check_pools` | `hayai-bench/tests/state.rs` `the_orchard_pool_is_off_in_the_soft_fork_range` | `zakura-consensus/src/transaction.rs:484-493` | implemented+tested (blocks; see P4 ZIP 257) |
| SPEC-7.1.2-26 | A transaction that spends a coinbase output has no transparent outputs | `hayai-state/src/check.rs` `check_txs` (`UnshieldedCoinbaseSpend`, `coinbase_must_be_shielded`) | `state.rs` `mature_coinbase_spent_to_shielded_outputs_is_accepted`, `regtest_allows_a_coinbase_spend_with_transparent_outputs`; `mempool.rs` | `zakura-chain/src/transaction.rs:552-564` | implemented+tested |
| SPEC-7.1.2-27a | `joinSplitPubKey` is a valid Ed25519 key encoding | `hayai-prepared/src/sprout.rs` (P4) | none | P4 | implemented, no direct test |
| SPEC-7.1.2-27b | `joinSplitSig` is valid over the sighash | `sprout.rs` (P4) | `hayai-bench/tests/sprout.rs` `a_changed_joinsplit_transaction_fails` | P4 | implemented+tested |
| SPEC-7.1.2-28 | `bindingSigSapling` is valid; `R` canonical | `shielded.rs` (P4) | block vectors (`conformance_blocks.rs`) | P4 | implemented+tested |
| SPEC-7.1.2-29 | v4 without spends and outputs: `valueBalanceSapling` is 0 | `draft` (`V4ValueBalance`); `hayai-wire/src/scan.rs` `v4_value_balance` | `prepare::tests::a_v4_value_balance_without_sapling_components_is_zero`, `scan.rs` `v4_value_balance_is_recovered_from_the_wire` | `zakura-chain/src/transaction/serialize.rs` (v4 read) | implemented+tested |
| SPEC-7.1.2-30 | `bindingSigOrchard` is valid | `shielded.rs` (P4) | `prepared.rs` `bisection_isolates_the_tampered_orchard_bundle` | P4 | implemented+tested |
| SPEC-7.1.2-31 | `bindingSigIronwood` is valid | `shielded.rs` (P4) | `ironwood.rs` `invalid_proofs_and_keys_fail_the_shielded_stage` | P4 | implemented+tested |
| SPEC-7.1.2-32 | Coinbase value: before NU6 outputs ≤ inputs, from NU6 equal | `hayai-consensus` `CoinbaseTerms::check` (P2) | P2 rows of ZIP 236 | `block/check.rs:326` | implemented+tested |
| SPEC-7.1.2-33 | A coinbase has no JoinSplit | `draft` (`CoinbaseJoinSplit`) | `prepare::tests::a_coinbase_has_no_joinsplit` | `check.rs:251` | implemented+tested |
| SPEC-7.1.2-34 | A coinbase has no Sapling spend | `draft` (`CoinbaseShieldedSpend`) | none for Sapling (the test covers Orchard and Ironwood `enableSpends`) | `check.rs:251` | implemented, no direct test |
| SPEC-7.1.2-35 | Pre-Heartwood: a coinbase has no Sapling output | `hayai-prepared/src/coinbase.rs` `check_shielded_outputs` (P4) | `coinbase::tests::a_coinbase_has_no_shielded_output_before_heartwood` | P4 | implemented+tested |
| SPEC-7.1.2-36 | NU6.3 on: a coinbase has no Orchard-pool actions | `draft` (`CoinbaseOrchardBundle`) | `the_coinbase_rules_of_the_orchard_and_ironwood_bundles` | `check.rs:367` | implemented+tested |
| SPEC-7.1.2-37 | A coinbase has `enableSpendsOrchard = 0` | `draft` (`CoinbaseShieldedSpend`) | the same test | `check.rs:251` | implemented+tested |
| SPEC-7.1.2-38 | A v6 coinbase has `enableSpendsIronwood = 0` | `draft` (`CoinbaseShieldedSpend`) | the same test | `check.rs:251` | implemented+tested |
| SPEC-7.1.2-39 | v5 and v6: bits 2..7 of `flagsOrchard` are 0 | upstream `Flags::from_byte`; `draft` (`OrchardCrossAddress`) | `the_flag_bits_of_each_pool` | `check.rs:179` | implemented+tested |
| SPEC-7.1.2-40 | v6: bits 3..7 of `flagsIronwood` are 0 | upstream `Flags::from_byte` | the same test | `serialize.rs` | implemented+tested |
| SPEC-7.1.2-41 | The coinbase script of a block above 0 starts with the height (BIP 34 encoding) | `hayai-state/src/check.rs` `check_coinbase` (`height_push`) | `state.rs` `coinbase_placement_and_height` | `zakura-chain/src/transparent/serialize.rs:58` | implemented+tested |
| SPEC-7.1.2-42 | The coinbase script has 2 to 100 bytes | `draft` (`CoinbaseScriptLength`) | `coinbase_script_length_and_null_prevouts` | `zakura-chain/src/transparent/serialize.rs:194` | implemented+tested |
| SPEC-7.1.2-43 | A non-coinbase input has no null prevout | `draft` (`NullPrevout`) | the same test | `check.rs:251` | implemented+tested |
| SPEC-7.1.2-44 | Every prevout is a unique unspent output of an earlier block or of an earlier transaction of the block | `draft` (`DuplicateInput`); `check_txs` (`DoubleSpend`, `MissingInput`); `resolve_inputs` | `state.rs` `double_spend_within_a_block_is_rejected`, `double_spend_across_blocks_is_a_missing_input`, `spending_a_later_transaction_of_the_block_is_rejected`; `prepared.rs` `missing_input_and_duplicate_input_are_reported` | `check.rs:403`; `zakura-state/src/service/check/utxo.rs` | implemented+tested |
| SPEC-7.1.2-45 | No spend of a coinbase output less than 100 blocks old | `check_txs` (`ImmatureCoinbase`, `COINBASE_MATURITY`) | `state.rs` `immature_coinbase_spend_is_rejected`; `hayai-state` `coinbase_maturity_constant_matches_zcash` | `check.rs:675` | implemented+tested |
| SPEC-7.1.2-46 | No spend of the genesis coinbase output | `hayaid/src/node.rs` (the base starts at the genesis block with no coin: `Base::new`) | none | `zakura-state` (the genesis block adds no UTXO) | implemented differently: the genesis coinbase output never enters the coin set, so a spend is `MissingInput` |
| SPEC-7.1.2-47 | Overwinter to Canopy: expiry ≤ 499,999,999 | `draft` (`ExpiryTooHigh`) | `an_expiry_height_is_below_the_threshold` | `check.rs:628` | implemented+tested |
| SPEC-7.1.2-48 | NU5 on: expiry ≤ 499,999,999 for a non-coinbase | `draft` (`ExpiryTooHigh`, every transaction) | the same test | `check.rs:592` | implemented differently: see ZIP-203-5 |
| SPEC-7.1.2-49 | A non-coinbase with nonzero expiry is not mined above it | `check_txs` (`Expired`) | `state.rs` `expiry_and_lock_time_rules` | `check.rs:652` | implemented+tested |
| SPEC-7.1.2-50 | NU5 on: the coinbase expiry equals the height | `check_coinbase` (`CoinbaseExpiry`) | `state.rs` `coinbase_placement_and_height` | `check.rs:572` | implemented+tested |
| SPEC-7.1.2-51 | `valueBalanceSapling` in −MAX_MONEY..MAX_MONEY | upstream parser (`read_amount`, `ZatBalance::from_i64`) | none | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.2-52 | `valueBalanceOrchard` in −MAX_MONEY..MAX_MONEY | upstream parser | none | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.2-53 | `valueBalanceIronwood` in −MAX_MONEY..MAX_MONEY | upstream parser | none | `serialize.rs` | implemented, no direct test |
| SPEC-7.1.2-54 | Heartwood on: coinbase shielded outputs decrypt with the zero OVK | `coinbase.rs` (P4) | `coinbase::tests::a_sapling_coinbase_output_decrypts_only_with_the_zero_key` (and Orchard, Ironwood) | `check.rs:503` | implemented+tested |
| SPEC-7.1.2-55 | Canopy on: lead byte 0x02 for Sapling and Orchard coinbase outputs | `coinbase.rs` (P4) | `coinbase::tests::the_sapling_lead_byte_follows_the_upgrade` | `check.rs:503` | implemented+tested (Sapling; Orchard lead byte: no direct test) |
| SPEC-7.1.2-56 | NU6.3 on: lead byte 0x03 for Ironwood coinbase outputs | `coinbase.rs` (P4) | none for a wrong lead byte | `check.rs:503` | implemented, no direct test |
| SPEC-7.1.2-57 | Other rules inherited from Bitcoin (a `todo` of the specification) | §7.12 rows | — | — | not applicable: no rule text |
| SPEC-7.1.2-M1 | The types of §7.1 are consensus rules (field widths, CompactSize) | upstream parser; `scan.rs` (canonical CompactSize, ≤ 0x02000000) | `scan.rs` `corrupted_transactions_never_panic` | `zakura-chain/src/serialization` | implemented+tested |
| SPEC-7.1.2-L1 | Lock time: the transaction is final at the block height and block time (zcashd `IsFinalTx`) | `hayai-state/src/check.rs` `finality`, `check_txs` (`NotFinal`) | `state.rs` `a_lock_time_is_a_height_or_a_time`, `expiry_and_lock_time_rules` | `check.rs:72` | implemented+tested |

## Spec §7.2 to §7.5: Description Encoding and Consensus
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.3-1 | `anchorSapling`, if present, is < q_J | upstream parser (`read_v5_bundle`; v4 per spend) | none | serializer | implemented, no direct test |
| SPEC-7.4-1 | `cmu` < q_J | upstream parser | none | serializer | implemented, no direct test |
| SPEC-7.5-1 | `cmx` < q_P | upstream parser (`ExtractedNoteCommitment::from_bytes`) | none | serializer | implemented, no direct test |
| SPEC-7.5-2 | NU6.2 on: `proofsOrchard` has the length 2720 + 2272 × actions | upstream `Bundle::try_from_parts` | `prepare::tests::a_proof_has_the_canonical_length` | serializer | implemented+tested |
| SPEC-7.5-3 | NU6.3 on, v6: `proofsIronwood` has the canonical length | upstream `Bundle::try_from_parts` | the same test | serializer | implemented+tested |

## Spec §7.6: Block Header Encoding and Consensus
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.6-1 | The block version is at least 4 (as `int32`) | `hayai-consensus/src/header.rs` `check_version` (l. 208), called by `check_contextual` and `HeaderChain::check_context_free` | `a_real_header_with_one_broken_rule_fails_that_rule` (`hayai-consensus/tests/header.rs`: 0, 3, 0x80000000, 0x80000004, u32::MAX refused) | `zakura-chain/src/block/serialize.rs:36-62` | implemented+tested |
| SPEC-7.6-2 | `nBits` equals `ThresholdBits(height)` | `header.rs` `check_contextual` (l. 175) with `difficulty.rs` `expected_bits` | `a_real_header_with_one_broken_rule_fails_that_rule` (`WrongBits`), `bits_are_checked_or_reported_as_unchecked`, `generated_chains_match_the_reference`, `a_chain_from_genesis_matches_the_reference` | `contextual/validate.rs:85-91` | implemented+tested (Regtest waives it, as Zakura `disable_pow`) |
| SPEC-7.6-3 | The block passes the difficulty filter (§7.7.2) | `header.rs` `check_proof_of_work` (l. 242) → `hayai-wire/src/header.rs` `check_pow` (l. 280) | `a_real_header_with_one_broken_rule_fails_that_rule` (`HashAboveTarget`); hayai-wire `tests::check_pow_compares_little_endian` | `context_free/hash_filter.rs:15-22` | implemented+tested |
| SPEC-7.6-4 | `solution` is a valid Equihash solution (§7.7.1) | `header.rs` `check_proof_of_work` (l. 244) → `hayai-wire/src/header.rs` `check_equihash` (l. 296), upstream `equihash::is_valid_solution` | `a_real_header_with_one_broken_rule_fails_that_rule` (`Equihash`), `the_first_real_blocks_pass_every_rule` | `zakura-chain/src/work/equihash.rs:101-114,156` | implemented+tested |
| SPEC-7.6-5 | Each block other than the genesis block has `nTime` > median-time-past | `header.rs` `check_contextual` (l. 143) | `the_time_rules_and_their_start_heights`, `the_median_time_past_reads_eleven_blocks` | `contextual/validate.rs:56-61` | implemented+tested |
| SPEC-7.6-6 | `nTime` ≤ median-time-past + 90 · 60 s from height 2 (Mainnet) and 653,606 (Testnet) | `header.rs` `check_contextual` (l. 151); `network.rs` `max_time_start_height` (l. 482, 507) | `the_time_rules_and_their_start_heights` (Mainnet 1 and 2, Testnet 653,605 and 653,606, limit and limit + 1) | `contextual/validate.rs:63-75`, `network_upgrade.rs:347` | implemented+tested |
| SPEC-7.6-7 | A block has at most 2,000,000 bytes | `hayai-wire/src/lib.rs` `RawBlock::parse_prefix` (l. 287, P3) | hayai-wire `tests::oversized_block_is_rejected_before_parsing` | `zakura-chain/src/block/serialize.rs:24,158` | implemented+tested |
| SPEC-7.6-8 | Sapling and Blossom: `hashLightClientRoot` = final Sapling root of the block | `hayai-state/src/history.rs` `header_commitment` (l. 153, P5), on the checkpoint path (`checkpoint_layer`) | none at a Sapling-era height | `zakura-state/src/service/check.rs:272-287` (not checked: below the Canopy checkpoint) | implemented, no direct test (hayai checks it on the checkpoint path; Zakura trusts the checkpoint) |
| SPEC-7.6-9 | Heartwood and Canopy: `hashLightClientRoot` = `hashChainHistoryRoot` (ZIP 221) | `header_commitment` (P5) | hayai-state `mainnet_heartwood_activation`, `mainnet_canopy_activation_resets_the_tree` (`hayai-state/tests/history.rs`) | `check.rs:289-312` | implemented+tested |
| SPEC-7.6-10 | From NU5: `hashBlockCommitments` (ZIP 244) | `header_commitment` (P5), `hayai-state/src/check.rs` (`ContextError::BlockCommitments`) | hayai-state `nu5_and_later_commit_to_the_root_and_the_auth_data_root`; hayai-bench `validate::tampered_blocks_fail_at_the_right_stage` | `check.rs:313-340` | implemented+tested |
| SPEC-7.6-11 | A block has at least one transaction | `hayai-wire/src/lib.rs` `parse_prefix` (l. 294, `ParseError::Empty`, P3) | hayai-wire `malformed_blocks_are_rejected` (`hayai-wire/tests/vectors.rs`) | `zakura-consensus/src/block/check.rs:77` | implemented+tested |
| SPEC-7.6-12 | The first transaction is a coinbase; no later transaction is a coinbase | `hayai-state/src/check.rs` `check_coinbase` (l. 383), `contextual_check_with_outputs` (l. 989, P5) | hayai-bench `state::coinbase_placement_and_height` | `zakura-consensus/src/block/check.rs:78-89` | implemented+tested |
| SPEC-7.6-13 | "Other rules inherited from Bitcoin" (open TODO of the specification) | P5 block-level rules (merkle root, duplicate txid, sigops) | P5 | P5 | not applicable (the marker names no rule) |
| SPEC-7.6-M1 | A full validator MUST NOT accept a block with `nTime` more than 2 h after its clock | `header.rs` `check_local_time` (l. 251), called by `check_header` (relay, hayaid header check) and by the `HeaderRules` of hayaid sync | `the_local_time_rule` (`hayai-consensus/tests/header.rs`) | `zakura-chain/src/block/header.rs:106-125` | implemented+tested (block validation and the replay at a restart do not apply it: the header check before them did) |
| SPEC-7.6-M2 | Miners MUST NOT create blocks with a version other than 4 | `hayai-template/src/live.rs` `BLOCK_VERSION` = 4, used by `LiveTemplate`, `hayai-template/src/publisher.rs` and `hayai-rpc/src/template.rs` | none: no test asserts the version of a template; the Regtest tests mine templates through the header check (version at least 4), which does not catch a version above 4 | `zakura-rpc` template version 4 | implemented, no direct test |
| SPEC-7.6-M3 | A block with a version above 4 MUST be treated as a version 4 block | `header.rs` `check_version` accepts 4..=0x7fffffff | `a_real_header_with_one_broken_rule_fails_that_rule` (5, 0x20000000, 0x7fffffff pass) | `serialize.rs:36-62` | implemented+tested |
| SPEC-7.6-M4 | `solutionSize` MUST have the minimal CompactSize encoding; other encodings MUST be refused | `hayai-wire/src/header.rs` `BlockHeader::parse` (l. 110, `CompactSize::read_t` refuses non-canonical sizes) | hayai-wire `tests::parse_rejects_short_unknown_and_non_canonical_lengths` | `zakura-chain/src/block/serialize.rs` (CompactSize read) | implemented+tested |
| SPEC-7.6-M5 | `nTime` MUST be strictly greater than the median of the past 11 blocks | as SPEC-7.6-5 | as SPEC-7.6-5 | as SPEC-7.6-5 | implemented+tested |

## Spec §7.7: Proof of Work (§7.7.1 to §7.7.5)
Rows with an id `-D<k>` are definitions that the rules of §7.6 use; §7.7 has no marker item.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.7.1-D1 | Equihash n = 200, k = 9 on Mainnet and Testnet; solution of 1344 bytes | `hayai-wire/src/header.rs` `PowParams::MAINNET` (l. 50), `TESTNET`; `network.rs` `MAINNET.pow` (l. 476); `header.rs` `check_solution_length` (l. 219) | `network::tests::mainnet_and_testnet_values`; hayai-wire `tests::known_parameter_sets_have_the_specified_lengths`; `a_real_header_with_one_broken_rule_fails_that_rule` (`SolutionLength`) | `equihash.rs:45,101-114` | implemented+tested |
| SPEC-7.7.1-D2 | A valid solution: generalized birthday and algorithm binding conditions over `powheader` + nonce; 21-bit big-endian index encoding | `hayai-wire/src/header.rs` `check_equihash` (l. 296): upstream `equihash::is_valid_solution` with the first 108 header bytes and `nNonce` | `the_first_real_blocks_pass_every_rule`, `a_real_header_with_one_broken_rule_fails_that_rule` | `equihash.rs:156` (same upstream crate) | implemented+tested |
| SPEC-7.7.2-M1 | SHA-256d of the whole header, little-endian, MUST be ≤ `ToTarget(nBits)` | `hayai-wire/src/header.rs` `check_pow` (l. 280) | hayai-wire `tests::check_pow_compares_little_endian`, `tests::check_target_applies_the_limit` | `hash_filter.rs:15-22`, `context_free/target.rs:25-37` | implemented+tested |
| SPEC-7.7.3-D1 | `median(S)` = `sorted(S)` at 1-based index ceiling((len + 1) / 2) | `difficulty.rs` `median_time` (l. 111) | `difficulty::tests::median_is_the_upper_middle_element` | `adjusted_difficulty.rs:385-397` | implemented+tested |
| SPEC-7.7.3-D2 | `MedianTime(h)`: median of the `nTime` of the 11 blocks before `h` (all of them when fewer) | `difficulty.rs` `median_time_past` (l. 119); `header.rs` `check_contextual` | `the_median_time_past_reads_eleven_blocks`, `difficulty::tests::median_is_the_upper_middle_element` | `adjusted_difficulty.rs:366` | implemented+tested |
| SPEC-7.7.3-D3 | `MeanTarget(h)`: mean of `ToTarget(nBits)` of the `W` blocks before `h` | `difficulty.rs` `mean_target` (l. 209) | `difficulty::tests::mean_target_is_the_floor_of_the_exact_mean`, `zakura_vector_of_a_mixed_window` | `adjusted_difficulty.rs:258-302` | implemented+tested |
| SPEC-7.7.3-D4 | `ActualTimespan`, `ActualTimespanDamped` (trunc, `PoWDampingFactor` 4), bounds 84 % and 132 % of `AveragingWindowTimespan` | `difficulty.rs` `bounded_timespan` (l. 227) | `difficulty::tests::timespan_bounds_of_both_spacings`, `the_clamp_limits` | `adjusted_difficulty.rs:304-365` | implemented+tested |
| SPEC-7.7.3-D5 | `Threshold(h)` = min(PoWLimit, floor(MeanTarget / AveragingWindowTimespan) · ActualTimespanBounded); `ThresholdBits` = ToCompact(Threshold) | `difficulty.rs` `expected_bits` (l. 186-197) | `the_result_is_at_most_the_pow_limit`, `generated_chains_match_the_reference` | `adjusted_difficulty.rs:237-252` | implemented+tested |
| SPEC-7.7.3-D6 | `MeanTarget(h)` = PoWLimit for `h ≤ PoWAveragingWindow` | `difficulty.rs` `expected_bits` (l. 168) returns PoWLimit as the threshold | `the_result_is_at_most_the_pow_limit` (heights 1 to 17), `the_first_blocks_of_both_networks_have_the_limit` | `adjusted_difficulty.rs:227-235` | implemented differently (F-P1-2: hayai, zcashd and Zakura give PoWLimit; the formula of the specification has no `ActualTimespan` there; heights 1 to 17 are below the mandatory checkpoint and Regtest waives the rule) |
| SPEC-7.7.3-D7 | Testnet from 299,188: minimum-difficulty blocks (ZIP 205); Blossom sets the threshold to 6 spacings (ZIP 208); not Mainnet | as ZIP-205-6, ZIP-208-11 | as ZIP-205-6, ZIP-208-11 | `network_upgrade.rs:522-570` | implemented+tested |
| SPEC-7.7.4-D1 | `ToCompact(x)` | `hayai-wire/src/header.rs` `compact_from_target` (l. 230); `difficulty.rs` `compact_from_u256` (l. 88) | hayai-wire `tests::compact_vectors_of_bitcoin_and_zcashd`, `compact_round_trip`, `compact_truncates_to_the_top_bytes`; `difficulty::tests::compact_and_u256_round_trip` | `zakura-chain/src/work/difficulty.rs:471` | implemented+tested |
| SPEC-7.7.4-D2 | `ToTarget(x)`: 0 when the sign bit is set, else mantissa · 256^(exponent − 3) | `hayai-wire/src/header.rs` `expand_target` (l. 192); `difficulty.rs` `target_from_compact` (l. 81) | hayai-wire `tests::expand_target_matches_known_values`, `tests::compact_vectors_of_bitcoin_and_zcashd` | `difficulty.rs:193` | implemented differently (a zero or overflowing target is "no target" and the header fails, as zcashd `CheckProofOfWork`; for an exponent below 3 hayai shifts right, as zcashd `SetCompact`; a valid `nBits` equals `ThresholdBits`, so the verdict is the same) |
| SPEC-7.7.5-D1 | Work of a block = floor(2^256 / (ToTarget(nBits) + 1)) | `difficulty.rs` `block_work` (l. 99) | `difficulty::tests::block_work_vectors` (Zakura golden values) | `zakura-chain/src/work/difficulty.rs:266` | implemented+tested |
| SPEC-7.7.5-D2 | The best chain is the valid chain with the greatest total work | `hayai-sync/src/headers.rs` `HeaderChain::push` (first-seen on equal work) | hayai-sync `best_tip_is_the_most_work_not_the_most_blocks`, `equal_work_keeps_the_first_seen_tip` (`hayai-sync/tests/headers.rs`) | Zakura takes the larger hash on equal work | implemented+tested (the tie rule differs from Zakura; no block verdict depends on it) |

## Spec §7.8: Calculating Block Subsidy, Funding Streams, Lockbox Disbursement, and Founders' Reward
No rule marker and no MUST sentence. The definitions:

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.8-D1 | `SlowStartShift`, `SlowStartRate`; the ramp `rate * h` below the shift, `rate * (h + 1)` from it | `subsidy.rs` `total_subsidy` (198), `scheduled_issuance` (160) | `subsidy::tests::mainnet_subsidy_follows_the_schedule` (1, 9,999, 10,000, 19,999), `the_scheduled_issuance_is_the_sum_of_the_subsidies` | `zk-subsidy:955-975` | implemented+tested |
| SPEC-7.8-D2 | `Halving(height)` before and after Blossom | `subsidy.rs` `halving` (83) | `subsidy::tests::halvings_and_subsidies_without_the_nu7_era`; `conformance_subsidy::baselines::*` | `zk-subsidy:526-563` | implemented+tested |
| SPEC-7.8-D3 | `BlockSubsidy(height)`: slow start, then `floor(MaxBlockSubsidy / (BlossomRatio · 2^Halving))` | `subsidy.rs` `total_subsidy`, `MAX_BLOCK_SUBSIDY` (37) | `subsidy::tests::mainnet_subsidy_follows_the_schedule`, `testnet_subsidy_follows_the_schedule`, `halvings_and_subsidies_without_the_nu7_era` | `zk-subsidy:949-984` | implemented+tested |
| SPEC-7.8-D4 | `FoundersReward(height) = BlockSubsidy · FoundersFraction` while `Halving < 1` | `founders.rs` `founders_reward` (46), `FOUNDERS_FRACTION_DIVISOR` (37) | `founders::tests::the_reward_is_a_fifth_of_the_subsidy_until_canopy` | `zk-subsidy:1044-1060` | implemented+tested |
| SPEC-7.8-D5 | `fs.Value(height)`: 0 before Canopy, the floor share in the range, else 0 | `funding.rs` `funding_streams` | see ZIP-207-1 | `zk-subsidy:438-470` | implemented+tested |
| SPEC-7.8-D6 | `totalDeferredOutput(height)` | `funding.rs` `deferred_value`; `coinbase.rs` `CoinbaseTerms::terms` | `subsidy::tests::the_deferred_part_is_the_lockbox_stream` | `zk-check:268-271` | implemented+tested |
| SPEC-7.8-D7 | `totalDeferredInput(height)`: the amount at `ZIP271ActivationHeight`, else 0 | `coinbase.rs` `CoinbaseTerms::terms` (`disbursed`) | `coinbase::tests::terms_have_the_outputs_of_each_era` | `zk-check:276-298` | implemented+tested |
| SPEC-7.8-D8 | `MinerSubsidy = BlockSubsidy − FoundersReward − Σ fs.Value` | `coinbase.rs` `CoinbaseTerms::miner_subsidy` (210) | `coinbase::tests::terms_have_the_outputs_of_each_era` | `zk-subsidy:989-1004` | implemented+tested |

## Spec §7.9: Payment of Founders' Reward
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.9-D1 | `FounderAddressList` of Mainnet (48) and Testnet (48) | `founders.rs` `MAINNET_ADDRESSES` (83), `TESTNET_ADDRESSES` (138) | `founders::tests::every_address_is_a_p2sh_address_of_its_network`; `conformance_subsidy::baselines::*`; the lists equal the spec text (Python comparison of this package) | `zk-main:113-162`, `zk-test:115-164` | implemented+tested |
| SPEC-7.9-D2 | `FounderAddressChangeInterval = ceiling((SlowStartShift + PreBlossomHalvingInterval) / 48)` = 17,709 | `founders.rs` `founders_reward` | `founders::tests::the_address_changes_every_17709_adjusted_blocks` | `zk-subsidy:1006-1040` | implemented+tested |
| SPEC-7.9-D3 | `FounderAddressAdjustedHeight`: from Blossom, `Blossom + floor((h − Blossom) / 2)` | `founders.rs` `founders_reward` | the same test | `zk-subsidy:1006-1040` | implemented+tested |
| SPEC-7.9-D4 | `FounderAddressIndex = 1 + floor(adjusted / interval)` | `founders.rs` `founders_reward` (index from 0) | the same test | `zk-subsidy:1006-1040` | implemented+tested |
| SPEC-7.9-D5 | `FoundersRewardLastBlockHeight = max{h : Halving(h) < 1}` (1,046,399 from Blossom) | `founders.rs` `founders_reward` (`halving >= 1` gives none) | `founders::tests::the_reward_is_a_fifth_of_the_subsidy_until_canopy` (1,046,399 / 1,046,400) | `zk-check:219` | implemented+tested |
| SPEC-7.9-D6 | `FounderRedeemScriptHash(height)`: the standard redeem script hash of the address | `coinbase.rs` `p2sh_script` | `coinbase::tests::address_script_is_the_p2sh_script_of_the_address` | `zk-check:54-56` | implemented+tested |
| SPEC-7.9-D7 | `FounderAddressIndex(FoundersRewardLastBlockHeight) <= NumFounderAddresses` | `founders.rs` (comment at the index) | `founders::tests::the_address_changes_every_17709_adjusted_blocks` (last index 47) | none | implemented+tested |
| SPEC-7.9-1 | [Pre-Canopy] a coinbase at heights 1 to `FoundersRewardLastBlockHeight` has an output of exactly `FoundersReward(height)` to `OP_HASH160 FounderRedeemScriptHash OP_EQUAL` | `coinbase.rs` `CoinbaseTerms::terms`, `check`; `founders.rs` `founders_reward` | `coinbase::tests::a_missing_required_output_is_an_error` (Mainnet 20,000); `conformance_subsidy::coinbases_of_the_block_vectors_pass_the_coinbase_check` (50 or more founders' outputs) | `zk-check:207-229` | checkpoint path only: every height with the reward is at or below the mandatory checkpoint (Mainnet 1,046,399, Testnet 1,028,499) and Regtest has none; the code and the tests exist |

## Spec §7.10: Payment of Funding Streams, Deferred Lockbox, and Lockbox Disbursement
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.10-D1 | A stream: `Numerator`, `Denominator`, `StartHeight`, `EndHeight`, `Recipients` | `funding.rs` `Stream`, `StreamSet` | see ZIP 214 rows | `zk-subsidy:91-230` | implemented+tested |
| SPEC-7.10-D2 | `HeightForHalving`, `FSRecipientChangeInterval`, `FSRecipientPeriod` | `funding.rs` `schedule` (214), `address_period` (303); `subsidy.rs` `halving_height` (136) | `funding::tests::the_first_halving_height_is_the_first_height_with_halving_one`, `the_ecc_address_changes_at_each_period_boundary` | `zk-subsidy:342-370,409-430` | implemented+tested |
| SPEC-7.10-D3 | `fs.RecipientIndex`, `fs.Recipient`, `fs.NumRecipients` | `funding.rs` `funding_streams` | `funding::tests::the_ecc_address_changes_at_each_period_boundary` | `zk-fs:18-57` | implemented+tested |
| SPEC-7.10-M1 | `fs.Recipients` has `fs.NumRecipients` elements | `funding.rs` tables (one entry for a repeated address), `check_address_counts` (269) for Regtest | `funding::tests::each_range_has_the_periods_of_the_zakura_address_counts` | `zk-fs:36` (assert), `zk-main:180-190` | implemented differently: a list of one repeated address is one entry; a Regtest list can be longer, as Zakura; same outputs |
| SPEC-7.10-M2 | Each element is a P2SH address, a Sapling address, or `DEFERRED_POOL` | `funding.rs` tables; `coinbase.rs` `address_script`; `RegtestConfig` checks | `funding::tests::every_address_is_a_p2sh_address_of_its_network` | `zk-main:234-289` | implemented+tested |
| SPEC-7.10-D4 | A stream is active when `fs.Value(height) > 0` | `funding.rs` `funding_streams` (the height range; none when the subsidy is 0) | `funding::tests::a_subsidy_of_zero_has_no_funding_stream` | `zk-subsidy:438-470` | implemented differently: the range of ZIP 207, as Zakura; a stream in its range with `0 < subsidy * numerator < 100` needs a 0-zatoshi output; only a configured Regtest stream after about 26 halvings reaches this |
| SPEC-7.10-1a | [Canopy onward] for each active stream other than `DEFERRED_POOL`, one output of `fs.Value(height)` to its address in the prescribed way | `coinbase.rs` `CoinbaseTerms::terms`, `check` | see ZIP-207-r0-10; `conformance_subsidy::coinbases_of_the_block_vectors_pass_the_coinbase_check` | `zk-check:300-313` | implemented+tested |
| SPEC-7.10-1b | [NU6.1 onward] at `ZIP271ActivationHeight`, `ZIP271DisbursementChunks` equal outputs with a total of `ZIP271DisbursementAmount` to `ZIP271DisbursementAddress` | `coinbase.rs` `CoinbaseTerms::terms` (184), `lockbox.rs` `disbursements` | `coinbase::tests::each_required_output_needs_its_own_coinbase_output`; `lockbox::tests::the_disbursement_is_in_the_nu6_1_activation_block_only` | `zk-check:276-298` | implemented+tested; a block without subsidy skips it, as Zakura (`zk-check:183`), only on a Regtest |
| SPEC-7.10-1c | The prescribed way to pay a P2SH address: `OP_HASH160 fs.RedeemScriptHash OP_EQUAL` | `coinbase.rs` `p2sh_script` (340) | `coinbase::tests::address_script_is_the_p2sh_script_of_the_address`, `a_required_output_with_another_script_is_an_error` | `zk-check:54-56` | implemented+tested |
| SPEC-7.10-1d | The prescribed way to pay a Sapling or Orchard address (ZIP 213) | none (`p2sh_script` refuses other addresses) | none | `zk-fs:45-48` (transparent only) | not implemented: no stream and no disbursement has a shielded recipient; effect none on current networks |
| SPEC-7.10-M3 | Equal streams or disbursements need at least the given number of distinct outputs each | `coinbase.rs` `CoinbaseTerms::check` (`swap_remove`) | `coinbase::tests::each_required_output_needs_its_own_coinbase_output`, `a_configured_regtest_pays_its_disbursements_at_nu6_1` | `zk-check:43-56` | implemented+tested |

## Spec §7.10.1: ZIP 214 Funding Streams
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.10.1-D1 | r0 Mainnet table | see ZIP-214-r0-7 | see there | see there | implemented+tested |
| SPEC-7.10.1-D2 | r0 Testnet table | see ZIP-214-r0-8 | see there | see there | implemented+tested |
| SPEC-7.10.1-D3 | [NU6 onward] r1 Mainnet table | see ZIP-214-r1-9 | see there | see there | implemented+tested |
| SPEC-7.10.1-D4 | [NU6 onward] r1 Testnet table | see ZIP-214-r1-10 | see there | see there | implemented+tested |
| SPEC-7.10.1-D5 | [NU6.1 onward] r2 Mainnet table | see ZIP-214-r2-11 | see there | see there | implemented+tested |
| SPEC-7.10.1-D6 | [NU6.1 onward] r2 Testnet table | see ZIP-214-r2-12 | see there | see there | implemented+tested |

## Spec §7.11: Changes to the Script System, and §7.12: Bitcoin Improvement Proposals
| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| SPEC-7.11-1 | `OP_CODESEPARATOR` is disabled and does not change the sighash | upstream `zcash_script` 0.6 (`opcode/mod.rs:1553`, `Disabled`) | upstream tests; block vectors | `zakura-script` (C++ interpreter) | implemented, no direct test |
| SPEC-7.12-1 | BIP 16 (P2SH) from genesis | `hayai-prepared/src/lib.rs` `RuleEpoch::CONSENSUS_FLAGS` (`P2SH`); `hayai-consensus` `SCRIPT_FLAGS` | `prepared.rs` `every_fixture_transaction_prepares` | `zakura-script/src/lib.rs:173` | implemented+tested |
| SPEC-7.12-2 | BIP 30: no transaction overwrites an unspent transaction | no check | none | no check | implemented differently: BIP 34 heights and the NU5 coinbase expiry make each coinbase txid unique, and each other transaction spends an input or reveals a nullifier, so no new transaction has the txid of an unspent one |
| SPEC-7.12-3 | BIP 65 (`CHECKLOCKTIMEVERIFY`) from genesis | `CONSENSUS_FLAGS` (`CHECKLOCKTIMEVERIFY`) | block vectors | `zakura-script/src/lib.rs:173` | implemented, no direct test |
| SPEC-7.12-4 | BIP 66 (strict DER) from genesis | upstream `zcash_script` 0.6 `Signature::from_bytes` (DER check without a flag) | upstream tests | `zakura-script` | implemented, no direct test |
| SPEC-7.12-5 | BIP 34 height in the coinbase (except the genesis blocks) | `check_coinbase` | `coinbase_placement_and_height` | `transparent/serialize.rs:58` | implemented+tested |
| SPEC-7.12-6 | BIP 11, 14, 31, 35, 37, 61, 111 (network and standardness BIPs) | P6 | P6 | P6 | not applicable: network BIPs; see the ZIP 201 and ZIP 204 rows |

## Checkpoints (no ZIP, no specification rule)
Listed for the trace; Zakura has the same rules.

| Id | Rule | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|
| CHK-1 | A header at a checkpoint height has the checkpoint hash | `hayai-sync/src/headers.rs` `connect` (l. 471, `CheckpointMismatch`) | hayai-sync `checkpoints_fix_the_chain` | `zakura-header-chain/src/transition/planner/event_effects/header_validation.rs:213`, `zakura-consensus/src/checkpoint.rs:838` | implemented+tested |
| CHK-2 | The lists are the Zakura lists (Mainnet 14,385 to 3,499,045; Testnet 10,059 to 4,023,200) | `hayai-consensus/src/checkpoints.rs`, `build.rs` | `checkpoints::tests::the_embedded_lists_are_the_source_files`, `the_source_files_are_the_files_of_the_zakura_clone` | `zakura-chain/src/parameters/checkpoint/list.rs:179` | implemented+tested |
| CHK-3 | A block at or below the mandatory checkpoint (Canopy − 1) has the checkpoint path only | `hayai-validate/src/lib.rs` `check_above_mandatory_checkpoint` (l. 316), `apply_checkpointed` (l. 349) | hayai-bench `checkpoint::the_checkpoint_path_accepts_only_the_checkpointed_chain`; `checkpoints::tests::the_mandatory_checkpoint_is_the_block_before_canopy` | `zakura-chain/src/parameters/network.rs:271`, `zakura-state/src/service.rs:1545` | implemented+tested |
| CHK-4 | No branch leaves the best chain below the finalized height (last checkpoint, or tip − 1,000) | `hayai-sync/src/headers.rs` `finalized_height` (l. 435), `connect` (`ForkBelowFinalized`) | hayai-sync `branches_below_the_finalized_height_are_removed_and_refused` | `zakura-chain/src/parameters/constants.rs:30` | implemented+tested |

## Block-level rules without a ZIP
| Id | Rule | Source | hayai code | Test | Zakura | Status |
|---|---|---|---|---|---|---|
| BLOCK-1 | The parent of the block is the tip; the height is the height of the parent plus 1 | Spec §7.6 (`hashPrevBlock`); zcashd `ConnectBlock` | `check.rs` `check_parent` (l. 350, `WrongParent`); `Chain::push`, `push_speculative` (`NotOnTip`) | `state.rs` `double_spend_across_blocks_is_a_missing_input`; `checkpoint.rs` `the_checkpoint_path_accepts_only_the_checkpointed_chain`; `hayai-state` `tests::push_rejects_a_layer_off_the_tip` | `zakura-state/src/service/check.rs:371-380` | implemented+tested |
| BLOCK-2 | The first transaction is a coinbase and no other one is | Spec §7.6 (P1); zcashd `bad-cb-missing`, `bad-cb-multiple` | `check_coinbase` (l. 378, `NoCoinbase`), `contextual_check_with_outputs` (`ExtraCoinbase`), `prebuild_body` | `state.rs` `coinbase_placement_and_height`; `prebuilt.rs` `a_first_transaction_that_is_not_a_coinbase_is_refused` | `zakura-consensus/src/block/check.rs:68` | implemented+tested |
| BLOCK-3 | No txid twice in a block (CVE-2012-2459) | zcashd `CheckBlock` `bad-txns-duplicate` | `check.rs` `positions_of` (l. 1071), `prebuild_body`, `PrebuiltBody::commit` (`DuplicateTxid`); also hayai-wire and hayai-validate (P1, P3) | `state.rs` `coinbase_placement_and_height`; `validate.rs` `a_body_with_a_txid_twice_is_a_duplicate_txid_on_the_full_path`; `checkpoint.rs` `a_body_with_a_txid_twice_is_a_duplicate_txid_on_the_checkpoint_path` | `zakura-consensus/src/block/check.rs:523-559` | implemented+tested |
| BLOCK-4 | Each input spends a coin that exists and is unspent in the chain | Spec §3.4 (UTXO as in Bitcoin), §7.1.2 | `resolve_inputs` / `inputs_of` (`MissingInput`), `ChainView::get_coins` | `state.rs` `double_spend_across_blocks_is_a_missing_input`; `validate.rs` `tampered_blocks_fail_at_the_right_stage` | `zakura-state/src/service/check/utxo.rs:133-183` | implemented+tested |
| BLOCK-5 | No outpoint is spent twice in the block | Spec §3.4, §7.1.2 | `check_txs` (`DoubleSpend`); `checkpoint_layer` | `state.rs` `double_spend_within_a_block_is_rejected`; `checkpoint.rs` `the_checkpoint_path_keeps_the_checks_that_guard_the_state` | `utxo.rs:45-130` | implemented+tested |
| BLOCK-6 | An input spends an output of the same block only when its transaction comes earlier | Spec §3.4 (treestates and UTXO in block order) | `check_txs` (l. 487, `parent >= i` gives `MissingInput`) | `state.rs` `spending_a_later_transaction_of_the_block_is_rejected` | `utxo.rs:141-155` (`EarlyTransparentSpend`) | implemented+tested (checkpoint path: by the checkpoint hash) |
| BLOCK-7 | No spend of a coinbase output less than 100 blocks old | Spec §7.1.2 (P3 lists it too) | `check_txs` (l. 499, `ImmatureCoinbase`), `hayai-consensus` `COINBASE_MATURITY` | `state.rs` `immature_coinbase_spend_is_rejected` (99 refused, 100 passes), `regtest_allows_a_coinbase_spend_with_transparent_outputs` | `zakura-state/src/service/check/utxo.rs:200-225`, `zakura-chain/src/transparent.rs:55` | implemented+tested |
| BLOCK-8 | A transaction that spends a coinbase output has no transparent outputs (Mainnet, Testnet; not Regtest) | Spec §7.1.2 (P3 lists it too) | `check_txs` (l. 508, `UnshieldedCoinbaseSpend`) | `state.rs` `immature_coinbase_spend_is_rejected`, `regtest_allows_a_coinbase_spend_with_transparent_outputs` | `zakura-chain/src/transaction.rs:552-564` | implemented+tested |
| BLOCK-9 | At most 20,000 sigops in the block (legacy plus P2SH) | zcashd `ConnectBlock` `MAX_BLOCK_SIGOPS` | `add_totals` (l. 686, `TooManySigops`); count in hayai-prepared (P3) | `state.rs` `a_block_has_at_most_20_000_sigops` (20,000 passes, 20,001 refused) | `zakura-consensus/src/block.rs:303,611-665` | implemented+tested |
| BLOCK-10 | The note commitments of the block go to the trees in block order | Spec §3.4 (treestate chain) | `append_trees`, `append_leaves` (l. 877) | `sprout.rs` `the_sprout_tree_gives_the_published_final_roots`; `ironwood.rs` `a_nu6_3_block_validates_cold_and_warm_to_identical_layers` (upstream root) | `zakura-state/src/service/non_finalized_state/chain.rs:1686` | implemented+tested |
| BLOCK-11 | No transaction overwrites the unspent outputs of an earlier transaction with the same txid (BIP 30) | zcashd `ConnectBlock` `bad-txns-BIP30` (from Bitcoin Core 0.11; not read here) | none | none | none | not implemented: effect none (F-P5-4) |

Rules of other packages that hayai-state enforces (for their rows): anchors of §3.5 to §3.7 and ZIP 258 (`check_txs` `BadAnchor`, `check_sprout_anchors`, `ChainView::has_anchor`, P4); expiry and coinbase expiry (ZIP 203, P3); lock time (`finality`, P3); the pools of the height (`check_pools`, Orchard soft fork, P4); block limits of NU7 (`add_totals`, ZIP 218, P1); coinbase terms (`check_coinbase_value`, P2); NSM (`block_pools_after`, P2); `hashBlockCommitments` (ZIP 244, P3) through `header_commitment`.

## draft-arya-jvff-p2p-quic-transport: Version 2 Zcash P2P Network Protocol
Class: (b) network. Status Draft, not deployed on Mainnet or Testnet: no row.

## Validation paths, checkpoints and backends

### Header rule paths

`hayai_consensus::header::check_header` is the one function of the header rules: the
contextual rules (`check_contextual`), the local time rule when the caller gives a clock, and
the proof of work (`check_proof_of_work`). The local time rule (time ≤ clock of the node +
2 h) is a local rule, not a consensus rule: the header checks of hayai-relay and hayaid apply
it; block validation and the replay at a restart do not.

| Path | Rules | Context |
|---|---|---|
| Relay header check (`hayai_relay::StandardHeaderCheck`) | `check_header` with the clock | `HeaderContext::parent` |
| hayaid header check (`NodeHeaderCheck`: relay, `submitblock`, shadow follower) | `check_header` with the clock | the header index: committed and pending headers |
| Block validation (`validate_block`, `build_layer`, `commit_prebuilt`) | `check_contextual` (`hayai_validate::check_block_header`) | the view: `ChainView::recent_times`, `difficulty_context` |
| Header chain (`hayai_sync::HeaderChain::accept_headers`) | `check_version`, `check_proof_of_work`, then the `HeaderRules` of the node. A chain whose work is 2^256 or more is `HeaderRuleError::WorkOverflow` (possible on Regtest only) | the ancestors of the branch |
| Replay at a restart | `check_proof_of_work`, then block validation | the view |
| Checkpoint path (`apply_checkpointed`) | none: the header chain applied the rules to the header before the download | none |

The contextual rules read the times of the 28 blocks before the header and the `bits` of the
17 blocks before it before NU7, and 113 and 102 blocks from NU7 (ZIP 218); fewer near the
genesis block. A context that holds
fewer blocks than a rule reads gives the result `HeaderVerdict::ContextTooShort` with the
rules that did not run. That result is never a pass:

- A full node starts at the genesis block and has the whole context. It rejects such a
  header (`HeaderPolicy::Enforce`).
- A shadow node starts from the state of upstream. Its seed holds the time and the `bits`
  of the start block and of the blocks before it, 113 blocks in all, in the header index and in the base of
  the view. Every header rule therefore runs from the first block after the start, in the
  header check and in block validation. A seed without these blocks fails. The policy of a
  shadow node is `HeaderPolicy::TrustShortContext`: a header whose context is too short
  passes the rules that did not run and is counted in `hayai_shadow_trusted_bits_total`.
  With a whole seed the counter reads 0.
- `HeaderPolicy::GeneratedBlocks` runs no header rule. Only the generated blocks of
  hayai-bench use it: their headers have no proof of work.

### History tree state

The history tree (MMR peaks, upstream `zcash_history`) is in every layer and in the base, and
a new tree starts at each upgrade activation. The rule set names the tree version
(`RuleSet::history`): version 1 (Heartwood, Canopy), version 2 with the Orchard root and count
(NU5 to NU6.2), version 3 with the Ironwood root and count (from NU6.3). Before Heartwood the
tree is empty and needs no seed. A base at or after Heartwood comes back from the state log
with `HistoryState::from_peaks`. The start state of a shadow node has no peaks (they cannot be
derived from headers), so its tree stays unknown. A block on an unknown tree is not checked
and its layer records `history: None` (finding F-P5-1). An upgrade without a rule set returns
`HistoryError::Unsupported`.

### Checkpoints

hayai follows Zebra and Zakura: a dense checkpoint list, and a block at or below the last
checkpoint is verified by its hash.

- Lists (`hayai_consensus::Network::checkpoints`): the files `main-checkpoints.txt` and
  `test-checkpoints.txt` of the Zakura repository
  (`crates/zakura-chain/src/parameters/checkpoint/`, revision
  `13779158253cfe315f73eadffb9b4c93c25e82a5`), copied to
  `crates/hayai-consensus/src/checkpoints/`. Mainnet: 14,385 checkpoints, last height
  3,499,045. Testnet: 10,059 checkpoints, last height 4,023,200. The gap between two
  checkpoints is at most 400 blocks. Regtest: the genesis block only. A test compares the
  copies with the files of a Zakura clone beside the repository.
- Header chain: a header at a checkpoint height with another hash is `CheckpointMismatch`.
  The finalized height is at least the last checkpoint at or below the best tip, so a branch
  that leaves the best chain below it is `ForkBelowFinalized`. Each header has the full
  header rules (proof of work, Equihash, difficulty, time): the list does not replace them.
- Checkpoint path (`hayai_validate::apply_checkpointed`), for a block at or below the last
  checkpoint whose header is on the best header chain below a checkpoint that the chain
  reached.

  | Checked | Not checked |
  |---|---|
  | The parent is the tip of the state | Scripts and transparent signatures |
  | The block hash is `expected`, and it is the checkpoint hash at a checkpoint height | Sapling, Orchard, Ironwood and Sprout proofs and signatures |
  | The merkle root of the header matches the transactions, and no txid is in the block twice | Equihash and the contextual header rules (the header chain applied them) |
  | The header commitment (offset 68) to the Sapling root, to the history tree of the parent, and from NU5 to the authorizing data | Coinbase rules and terms, coinbase maturity, ZIP 213 |
  | Each transparent input spends a coin that exists; no outpoint is spent twice in the block | The order of a parent and its child in the block |
  | No nullifier is revealed twice in the block | Nullifiers against earlier blocks, anchors |
  | No value pool is negative, and the total is at most `MAX_MONEY` | Expiry, lock time, the pools of the height, the block limits, the context-free transaction rules |

  The caller supplies `expected`: the hash of the best header chain at the height of the
  block. The function compares the block hash with `expected` and with the checkpoint of
  the height, when the height has one. It does not read the header chain. A height
  between two checkpoints has no checkpoint, so `expected` is the only bond between the
  block and the checkpointed chain there. A caller that passes the hash of the block
  itself as `expected` removes that comparison.

  The state update is complete: coins, nullifiers, the note commitment trees, the history
  tree, the value pools and the header context. A test runs a generated chain through both
  paths and compares the states. A JoinSplit adds its nullifiers, its note
  commitments and its value to the Sprout state, and the path reads no proof of it.
- Mandatory checkpoint (`Network::mandatory_checkpoint_height`): the last block before
  Canopy (Mainnet 1,046,399, Testnet 1,028,499, Regtest 0), as Zakura. Full validation
  (`validate_block`, `build_layer`, `commit_prebuilt`) refuses a block at or below it with
  `BlockError::BelowMandatoryCheckpoint`: such a block has only the checkpoint path. Every
  row with the status `checkpoint path only` names a rule of these blocks.

### NU7 and the crypto backends

The NU7 rule set exists when the crypto backend has the NU7 branch id `0x77190ad9`. The
`zakura` backend has it. The default `upstream` backend does not: `zcash_protocol` 0.10.5
`BranchId` has no such value (`src/consensus.rs`, `impl TryFrom<u32> for BranchId`; the `Nu7`
variant is behind `cfg(zcash_unstable = "nu7")` with the value `0xffffffff`), and
`zcash_primitives` 0.30.1 `Transaction::read` (`src/transaction/mod.rs`, the header read of
`read_v5` and `read_v6`) refuses a v5 or v6 transaction with another branch id. The txid and
the signature hash also take a `BranchId`. A node of the default backend stops with
`ConsensusError::UnsupportedUpgrade` when its next block is the first block of NU7.

NU7 heights: Testnet 4,465,026 (Zakura `zakura-chain/src/parameters/constants.rs:80`, ZIP 259).
Mainnet: none in Zakura (ZIP 259: to be set). Regtest: none, or the `nu7` value of
`[regtest] activation_heights`.

The chain crosses NU7 and the NSM reissuance height in one node test: hayaid
`sync_tests::a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization` (Regtest,
NU6.3 at 104, NU7 at 108, reissuance at 110). The rules give Regtest no reissuance height. A
test names one with `RegtestConfig::with_test_reissuance_height`, as Zakura does for its tests
(`ParametersBuilder::with_test_nsm_reissuance_height`,
`zakura-chain/src/parameters/network/testnet.rs:1095-1101`). No configuration file sets the
value.

Node values of Zakura that follow the spacing and are not consensus rules have no code in
hayai: the stall interval and the checkpoint lag of the sync progress
(`zakurad/src/components/sync/progress.rs:178,377-392`), the download window
(`zakurad/src/components/sync.rs:164-200`), the mempool crawler and gossip intervals
(`zakurad/src/components/mempool/crawler.rs:82`, `gossip.rs:28`), the `nsm` entry of
`getblockchaininfo` (`zakura-rpc/src/methods.rs:4416`), and the averaging window of
`getnetworksolps` (`zakura-rpc/src/methods.rs:759`).

### Implementation notes

- Script verification uses the Rust interpreter in `zcash_script` 0.6, and not the C++
  `zcash_script` library. ECC maintains the Rust interpreter and tests it against the C++
  implementation. hayai's differential tests against Zakura (C++ interpreter) are the
  acceptance gate for consensus parity. The flags are those of zcashd's `ConnectBlock` and
  Zakura's verifier (`zakura-script/src/lib.rs:173`): `P2SH | CHECKLOCKTIMEVERIFY`.
- The Sapling verifying keys are the first 1,636 bytes of `sapling-spend.params` and the
  first 1,444 bytes of `sapling-output.params` (`crates/hayai-prepared/src/sapling_vk/`).
  `scripts/extract-sapling-vk.sh` writes them from files with the BLAKE2b-512 hashes of
  `zcash_proofs`. A test compares them with the parameters of the `wagyu-zcash-parameters`
  crate.
- The Sprout verifying key is the first 1,828 bytes of `sprout-groth16.params`
  (`crates/hayai-prepared/src/sprout_vk/`). `scripts/extract-sprout-vk.sh` writes it from a
  file with the size and the BLAKE2b-512 hash of `zcash_proofs` (`scripts/fetch-params.sh
  --sprout` downloads the file). The file is equal to `sprout-groth16.vk` of Zakura. A test
  pins its hash, and `hayai-bench/tests/sprout.rs` verifies the 5 Groth16 JoinSplits of the
  published block vectors that need no spent coin (Mainnet 419,201 and 903,000, Testnet
  925,483) with it.
- No header commits to the Sprout root. Before Sapling the header field at offset 68 is
  reserved, and hayai does not check it, as Zakura (`zakura-state/src/service/check.rs:272`,
  `PreSaplingReserved`).
- The Sprout treestates are in memory: the base holds the frontier of the final treestate of
  every block that changed the tree, by root (about 1 kB each). hayaid writes the new
  treestates of each flush to `state.log` and reads all of them at a restart.
- The upstream Sapling batch validator applies the canonical point encodings of ZIP 216 at
  every height. ZIP 216 activates with Canopy. Zebra and Zakura do the same, because no block
  before Canopy has a non-canonical encoding.
- The sigop count follows zcashd (`GetLegacySigOpCount` plus `GetP2SHSigOpCount`). Zebra
  counts only the legacy sigops.
- Block validation evaluates lock times against the height and header time of the block
  itself (zcashd `ContextualCheckBlock` with `nLockTimeFlags = 0`). The median-time-past rule
  applies to mempool admission only.
- The cache of context-free results has one entry set per
  `RuleEpoch { branch_id, script_flags }`. An epoch change drops the cache.
- The finalized anchors are an in-memory set per pool (Sapling, Orchard, Ironwood). The set
  starts with the empty-tree root (zcashd's `GetSaplingAnchorAt` / `GetOrchardAnchorAt` treat
  it as always present). hayaid persists the set in `state.log` (the new anchors of each
  flush) and rebuilds it at a restart (`docs/hayaid.md`, Restart).
- The Orchard soft fork of ZIP 257 applies to blocks (`rules_at`). The mempool admission of
  hayaid uses the rule set of the branch and does not apply the range (plan item B10).

## ZIPs of class (d)

| ZIP | Title | Status | Reason |
|---|---|---|---|
| 0 | ZIP Process | Active | process ZIP |
| 1 | Network Upgrade Policy and Scheduling | Reserved | process ZIP (reserved) |
| 2 | Design Considerations for Network Upgrades | Reserved | informational (reserved) |
| 32 | Shielded Hierarchical Deterministic Wallets | Final | wallet key derivation |
| 48 | Transparent Multisig Wallets | Draft | wallet (transparent multisig) |
| 68 | Relative lock-time using consensus-enforced sequence numbers | Draft | consensus draft (BIP 68) that no upgrade activates |
| 76 | Transaction Signature Validation before Overwinter | Reserved | reserved; no text (pre-Overwinter sighash is checkpoint-only) |
| 112 | CHECKSEQUENCEVERIFY | Draft | consensus draft (BIP 112) that no upgrade activates |
| 113 | Median Time Past as endpoint for lock-time calculations | Draft | consensus draft (BIP 113) that no upgrade activates |
| 129 | Zcash Transparent Multisig Setup | Reserved | wallet (reserved) |
| 173 | Bech32 Format | Final | address encoding (wallet) |
| 210 | Sapling Anchor Deduplication within Transactions | Withdrawn | withdrawn |
| 217 | Aggregate Signatures | Reserved | reserved; no text |
| 219 | Disabling Addition of New Value to the Sapling Chain Value Pool | Reserved | reserved; no text |
| 220 | Zcash Shielded Assets | Withdrawn | withdrawn |
| 222 | Transparent Zcash Extensions | Draft | consensus draft (TZE) that no upgrade activates |
| 226 | Transfer and Burn of Zcash Shielded Assets | Draft | consensus draft (ZSA) that no upgrade activates |
| 227 | Issuance of Zcash Shielded Assets | Draft | consensus draft (ZSA) that no upgrade activates |
| 228 | Asset Swaps for Zcash Shielded Assets | Draft | consensus draft (ZSA) that no upgrade activates |
| 230 | Withdrawn Version 6 Transaction Format | Withdrawn | withdrawn |
| 231 | Memo Bundles | Draft | consensus draft (memo bundles) that no upgrade activates |
| 233 | Network Sustainability Mechanism: Removing Funds From Circulation | Draft | consensus draft (NSM burn) that NU7 does not deploy (ZIP 259) |
| 234 | Network Sustainability Mechanism: Issuance Smoothing | Draft | consensus draft (NSM smoothing) that NU7 does not deploy (ZIP 259) |
| 240 | Standard Transaction Rules | Reserved | reserved; no text |
| 245 | Transaction Identifier Digests & Signature Validation for Transparent Zcash Extensions | Draft | consensus draft (TZE digests) that no upgrade activates |
| 246 | Digests for the Withdrawn Version 6 Transaction Format | Withdrawn | withdrawn |
| 248 | Extensible Transaction Format | Draft | consensus draft (extensible format) that no upgrade activates |
| 254 | Deployment of the NU7 Network Upgrade (Withdrawn) | Withdrawn | withdrawn (NU7 deployment, replaced by ZIP 259) |
| 260 | Extending Block Messages with Additional Authentication Data | Reserved | network ZIP, reserved; no text |
| 270 | Key Rotation for Tracked Signing Keys | Reserved | reserved; no text |
| 300 | Cross-chain Atomic Transactions | Proposed | informational (atomic swaps) |
| 302 | Standardized Memo Field Format | Draft | wallet memo format |
| 303 | Sprout Payment Disclosure | Withdrawn | withdrawn |
| 304 | Sapling Address Signatures | Draft | wallet signatures |
| 305 | Best Practices for Hardware Wallets supporting Sapling | Reserved | wallet (reserved) |
| 306 | Security Considerations for Anchor Selection | Reserved | informational (reserved) |
| 307 | Light Client Protocol for Payment Detection | Draft | light client protocol |
| 308 | Sprout to Sapling Migration | Active | wallet migration |
| 309 | Blind Off-chain Lightweight Transactions (BOLT) | Reserved | reserved |
| 310 | Security Properties of Sapling Viewing Keys | Draft | informational |
| 311 | Zcash Payment Disclosures | Draft | wallet |
| 312 | FROST for Spend Authorization Multisignatures | Draft | wallet (FROST) |
| 313 | Reduce Conventional Transaction Fee to 1000 zatoshis | Obsolete | obsolete |
| 314 | Privacy upgrades to the Zcash light client protocol | Reserved | wallet (reserved) |
| 315 | Best Practices for Wallet Implementations | Draft | wallet |
| 316 | Unified Addresses and Unified Viewing Keys | [Revision 0] Active, [Revision 1] Withdrawn, [Revision 2] Draft | address encoding (wallet) |
| 318 | Orchard to Ironwood Migration | Draft | wallet migration |
| 319 | Options for Shielded Pool Retirement | Reserved | informational (reserved) |
| 320 | Defining an Address Type to which funds can only be sent from Transparent Addresses | Active | address encoding (wallet) |
| 321 | Payment Request URIs | Active | wallet URIs |
| 322 | Generic Signed Message Format | Reserved | wallet (reserved) |
| 324 | URI-Encapsulated Payments | Draft | wallet URIs |
| 325 | Account Metadata Keys | Draft | wallet keys |
| 326 | NU6.3 Consequences for Wallets | Draft | wallet consequences of NU6.3 |
| 332 | Wallet Recovery from zcashd HD Seeds | Reserved | wallet (reserved) |
| 339 | Wallet Recovery Words | Reserved | wallet (reserved) |
| 350 | Bech32m | Reserved | address encoding (reserved) |
| 374 | Partially Created Zcash Transaction Format | [Revision 0] Draft | wallet (PCZT) |
| 400 | Wallet.dat format | Draft | wallet file format |
| 402 | New Wallet Database Format | Reserved | wallet (reserved) |
| 403 | Verification Behaviour of zcashd | Reserved | informational (reserved) |
| 416 | Spending Key Derivation in the `zcashd` wallet | Reserved | wallet (reserved) |
| 1001 | Keep the Block Distribution as Initially Defined — 90% to Miners | Obsolete | obsolete dev fund proposal (process) |
| 1002 | Opt-in Donation Feature | Obsolete | obsolete dev fund proposal (process) |
| 1003 | 20% Split Evenly Between the ECC and the Zcash Foundation, and a Voting System Mandate | Obsolete | obsolete dev fund proposal (process) |
| 1004 | Miner-Directed Dev Fund | Obsolete | obsolete dev fund proposal (process) |
| 1005 | Zcash Community Funding System | Obsolete | obsolete dev fund proposal (process) |
| 1006 | Development Fund of 10% to a 2-of-3 Multisig with Community-Involved Third Entity | Obsolete | obsolete dev fund proposal (process) |
| 1007 | Enforce Development Fund Commitments with a Legal Charter | Obsolete | obsolete dev fund proposal (process) |
| 1008 | Fund ECC for Two More Years | Obsolete | obsolete dev fund proposal (process) |
| 1009 | Five-Entity Strategic Council | Obsolete | obsolete dev fund proposal (process) |
| 1010 | Compromise Dev Fund Proposal With Diverse Funding Streams | Obsolete | obsolete dev fund proposal (process) |
| 1011 | Decentralize the Dev Fee | Obsolete | obsolete dev fund proposal (process) |
| 1012 | Dev Fund to ECC + ZF + Major Grants | Obsolete | obsolete dev fund proposal (process) |
| 1013 | Keep It Simple, Zcashers: 10% to ECC, 10% to ZF | Obsolete | obsolete dev fund proposal (process) |
| 2002 | Explicit Fees | Draft | consensus draft (explicit fees) that no upgrade activates |
| 2004 | Remove the dependency of consensus on note encryption | Draft | consensus draft that no upgrade activates |
| 2007 | Quantum Recoverability for a Subset of Transparent Addresses | Reserved | reserved; no text |
| draft-arya-dairaemma-disable-addition-of-transparent-chain-value | Disabling Addition of New Value to the Transparent Chain Value Pool | Draft | consensus draft that no upgrade activates |
| draft-arya-jvff-p2p-quic-transport | Version 2 Zcash P2P Network Protocol | Draft | network draft that no node deploys |
| draft-ecc-authenticated-reply-addrs | Authenticated Reply Addresses | Draft | wallet draft |
| draft-ecc-onchain-accountable-voting | On-chain Accountable Voting | Draft | process draft |
| draft-mcgee-keyholders-organizations | Update to ZIP 1016 & ZIP 271: Key-Holder Organizations | Draft | process draft |
| draft-str4d-orchard-balance-proof | Air drops, Proof-of-Balance, and Stake-weighted Polling | Draft | informational draft |

## Findings

Each finding has an id `F-<package>-<k>`, a severity, the input that shows it, and a bd issue.
Severities: consensus divergence (hayai gives another verdict than the specification or Zakura
on some input), missing check (a rule is not enforced, but no known input reaches it or
another check catches it), doc only (the code is right; a doc or a comment is wrong).

### Code fixes of the trace

| Finding | Files and functions | Hot path | Benchmark effect |
|---|---|---|---|
| F-P3-1 (ZIP 244 S.2a, `SIGHASH_SINGLE` without a corresponding output) | `crates/hayai-prepared/src/prepare.rs` `SighashContext::transparent`; test `prepare::tests::sighash_single_needs_the_output_of_its_index_from_v5` | yes: script verification of block validation and mempool admission | none expected: one integer comparison per signature check of a v5 or v6 input, no read, no allocation |
| F-P6-1 (ZIP 204, ZIP 201: disconnect peers below the version of the active upgrade) | `crates/hayaid/src/node.rs` `min_peer_version_at`, `Node::start`, `Driver::finish_commit`, `Driver::disconnect_to`; `crates/hayai-net/src/relay.rs` `Relay::set_min_peer_version`; test `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected` | yes: block commit and reorg of the driver (one atomic swap per block; a sweep of the peers only at an activation) | none: no hayai-bench benchmark runs the hayaid driver |
| F-P6-2 (ZIP 204: no message except the handshake before the handshake ends) | `crates/hayai-net/src/session.rs` `PeerSession::on_message`; test `session::tests::a_ping_before_the_handshake_gets_no_pong` | no | none |
| F-P6-3 (ZIP 204: do not relay a transaction that expires within 3 blocks) | `crates/hayai-wire/src/lib.rs` trait `TxLookup::for_each_relay_id`; `crates/hayai-prepared/src/store.rs` (override; `relay_ids` removed); `crates/hayaid/src/mempool.rs` `PublicTxs`; `crates/hayai-net/src/relay.rs` `Relay::on_mempool`; test `the_answer_to_mempool_leaves_out_a_transaction_that_expires_soon` | no: one `mempool` message per legacy peer; compact block reconstruction keeps `for_each_id` | none: the relay benchmarks send no `mempool` |
| F-P6-8 (ZIP 239: no fetch of a v5 transaction before NU5) | `crates/hayai-net/src/relay.rs` `Relay::on_inv`; test `before_nu5_a_wtxid_announcement_is_not_fetched` | yes: transaction `inv` handling (one branch read for a message with a `MSG_WTX` entry) | none: the relay benchmarks (`relay/forward_latency`, `relay/reconstruct`, `relay/bytes_on_wire`) do not use `inv` |

### F-P1-1: Stale context lengths and minimum-difficulty wording in comments after ZIP 218
- Severity: doc only
- Rule: ZIP-218-5, ZIP-218-6, ZIP-218-18, ZIP-205-6, ZIP-208-11
- Facts: the code reads 113 times and 102 `nBits` from NU7 (`DIFFICULTY_CONTEXT_BLOCKS`, `DifficultyParams::POST_NU7`), and the expected value of a minimum-difficulty block is the limit (ZIP 205, ZIP 208: `nBits` MUST be ToCompact(PoWLimit)). The comments said:
  - `hayai-consensus/src/difficulty.rs` module doc: a window of 17 blocks, a context of 28 and 17 blocks, a gap of 6 spacings, at every height.
  - `hayai-consensus/src/rules.rs` `DifficultyParams::min_difficulty_gap_spacings` and `network.rs` `NetworkParams::min_difficulty_start_height`: such a block "can use" the limit.
  - `hayai-relay/src/header_check.rs` `ParentInfo`: the rules read 28 times and 17 `nBits` at most.
  - `hayaid/src/headers.rs` `NodeHeaderCheck`: the shadow seed has the start block and 27 blocks before it (the code in `hayaid/src/shadow.rs` takes 113 blocks).
  - Foreign, applied by the coordinator: `hayaid/src/shadow.rs` line 7 ("and of the 27 blocks before it"); `docs/consensus-rules.md` lines 37-38 ("the times of the 28 blocks before the header and the `bits` of the 17 blocks") and line 45 ("the start block and of the 27 blocks before it").
- Input that shows it: a header at Testnet height 4,465,026 or above with 28 ancestors in its context: the comments say that the context is complete, and `expected_bits` returns `ContextTooShort { needed_times: 113, needed_bits: 102 }`.
- bd: hayai-gs4 (closed)
- Fix: fixed in this change: the owned files, `hayaid/src/shadow.rs` (module doc), and the text now in `docs/consensus.md` (Header rule paths). The patch below is applied.
- Fix details: comments only, in `difficulty.rs` (module doc, `expected_bits`), `rules.rs` (`DifficultyParams`), `network.rs` (`NetworkParams`), `hayai-relay/src/header_check.rs` (`ParentInfo`), `hayaid/src/headers.rs` (`NodeHeaderCheck`). Hot path: none (no code change). Benchmark: no change. Test: not applicable (comments); the existing tests `the_window_and_the_spacing_change_at_nu7` and `a_short_context_is_an_error` show the code behaviour.
- Proposed patch for the foreign files:
  - `hayaid/src/shadow.rs` line 7: `//!   and of the blocks before it, `DIFFICULTY_CONTEXT_BLOCKS` (113) blocks in all, the Sapling, Orchard and Ironwood frontiers`
  - `docs/consensus-rules.md` lines 37-38: "The contextual rules read the times of the 28 blocks before the header and the `bits` of the 17 blocks before it before NU7, and 113 and 102 blocks from NU7 (ZIP 218)". Line 45: "Its seed holds the time and the `bits` of the start block and of the blocks before it, 113 blocks in all".

### F-P1-2: Specification §7.7.3 has no threshold value for heights at or below PoWAveragingWindow
- Severity: doc only (the specification text; hayai, zcashd and Zakura agree)
- Rule: SPEC-7.7.3-D5, SPEC-7.7.3-D6, SPEC-7.6-2
- Facts: protocol.tex §7.7.3 sets `MeanTarget(h) = PoWLimit` for `h ≤ PoWAveragingWindow` and `Threshold(h) = min(PoWLimit, floor(MeanTarget(h) / AveragingWindowTimespan) · ActualTimespanBounded(h))` for `h ≠ 0`. `ActualTimespan(h)` reads `MedianTime(h − PoWAveragingWindow)`, the median of an empty list for `h ≤ 17` (102 from NU7). zcashd `GetNextWorkRequired` returns `nProofOfWorkLimit` when the window has no first block. Zakura `zakura-header-chain/src/validation/contextual/adjusted_difficulty.rs:227-235` returns the limit. hayai `hayai-consensus/src/difficulty.rs` `expected_bits` (l. 168) returns the limit. A literal reading of the formula with a defined `ActualTimespan` could give a target below the limit when the first blocks are fast.
- Input that shows it: Mainnet block 5 (`hayai-bench/tests/vectors/block-main-0-000-005.hex` in the header tests): hayai, zcashd and Zakura require `nBits` = `0x1f07ffff`; the formula of the specification has no value for it.
- bd: hayai-8sc
- Fix: not fixed (the code follows zcashd and Zakura; the brief says to report a disagreement and leave the code). Heights 1 to 17 (and 1 to 102 after an NU7 at a low height) are below the mandatory checkpoint on Mainnet and Testnet, and Regtest waives the rule. The code comment at `expected_bits` now states the difference.

### F-P2-1: ZIP 2008 recipient of the Mainnet `FS_FPF_ZCG_H3` stream
- Severity: missing check
- Rule: ZIP-2008-1
- Facts: ZIP 259 deploys ZIP 2008 with NU7 and sets the Mainnet NU7 height "TBD (To be set on OCT 20)". ZIP 2008 replaces `FS_FPF_ZCG_H3.AddressList[N..35]` with `t1MkHnkxVjNpNbCrSs3AJ8J7ZSp6NTYiUcG`, `N = AddressIndex(A − 1) + 1`. Zakura implements it: `zk-main:192-221` (`nu7_fpf_addresses`), and pays the P2PKH script (`zk-check:54-56`, test `zk-check:600-618`). hayai has no code: `hayai-consensus/src/funding.rs` module doc and the guard test `funding::tests::zip_2008_has_no_code_while_mainnet_has_no_nu7_height`; `coinbase.rs` `p2sh_script` refuses a P2PKH address. The owner recorded this gap (CHANGES.md 2026-10-05 "Not done: ZIP 2008"). Specification gap: ZIP 207 (revision 1), ZIP 2001 and Spec §7.10 (M2) allow only P2SH, Sapling and `DEFERRED_POOL` recipients and define no prescribed way for a P2PKH address; ZIP 2008 adds one. Zakura uses the standard P2PKH script.
- Input that shows it: a Mainnet build with an NU7 height `A`; a block in an address period with index at or above `N` of `FS_FPF_ZCG_H3` that pays 4,166,666 zatoshis to the P2PKH script of t1MkHnkx… and nothing to t3cFfPt1…. Zakura accepts it, hayai refuses it (`MissingOutput`). Today no input: Mainnet has no NU7 height.
- bd: hayai-pzt
- Fix: not fixed. The rule needs the Mainnet NU7 height (a P1 constant), a P2PKH required-output script in `coinbase.rs`, and the address list change in `funding.rs`. The owner decided to add it when Mainnet gets a height; the guard test then fails. The date in ZIP 259 is 2026-10-20.

### F-P2-2: Order of the ZIP 271 deduction from the deferred pool
- Severity: missing check
- Rule: ZIP-271-8 (with ZIP-271-11, Spec §4.17)
- Facts: ZIP 271: "The latter deduction occurs before any other change to the Deferred Dev Fund Lockbox balance in the transaction, and MUST NOT cause the Deferred Dev Fund Lockbox balance to become negative at that point." Spec §4.17 (protocol.tex 7671) rejects a block only when the deferred pool "would become negative in the block chain created as a result of accepting a block", that is after the block. Zakura checks after the block: `zk-check:292-296` adds the deferred part and subtracts the disbursement into one change, and `zakura-chain/src/value_balance.rs:218` requires a non-negative pool after it. hayai `hayai-consensus/src/lockbox.rs` `deferred_pool_after` checks `before + deferred − disbursed >= 0`. The specification and Zakura agree with hayai; ZIP 271 text is stricter. Rule 4a applies: the code stays.
- Input that shows it: the Mainnet NU6.1 activation block (3,146,400) on a chain whose deferred pool before the block is 78,750 ZEC − 18,750,000 zatoshis. hayai and Zakura accept (pool after = 0); the ZIP 271 text refuses (pool at the deduction = −18,750,000). The test `hayai-state` `check::tests::the_deferred_pool_pays_the_disbursement_or_the_block_fails` asserts the acceptance (`with(78_750 * ZEC - 18_750_000) == Ok(0)`). The real chains cannot give this input: the pool before the block is exactly 78,750 ZEC on Mainnet and Testnet (`coinbase::tests::the_deferred_pool_of_a_chain_pays_the_disbursement`). A configured Regtest can give it; ZIP 271 does not define Regtest.
- bd: hayai-28v
- Fix: not fixed (the specification and Zakura disagree with the ZIP text). The ZIP editors decide which text is normative.

### F-P2-3: NSM sources in the code comments
- Severity: doc only
- Rule: ZIP-235-1..7, ZIP-237-1..17, ZIP-234-3
- Facts: `hayai-consensus/src/nsm.rs` module doc cited "NU7 deployment draft, `draft-valargroup-deploy-nu7`", "zips#1354" and "the halving-preserving NSM draft, `draft-judah-nsm-halving-preserving-issuance`". `subsidy.rs` `scheduled_issuance` cited "zips#1354, `ExpectedIssuedSupply`". ZIP 259 deploys ZIP 235 and ZIP 237 and states that NU7 does not deploy ZIP 233 or ZIP 234. The rules of the code are those of ZIP 235 and ZIP 237 (checked row by row above).
- Input that shows it: none (comment only).
- bd: hayai-zq4 (closed)
- Fix: fixed in this change.
- Fix details: `hayai-consensus/src/nsm.rs` module doc names ZIP 235 and ZIP 237 and states that NU7 does not deploy ZIP 233 or ZIP 234; `subsidy.rs` `scheduled_issuance` doc names `S_A(height)` of ZIP 237. Comments only. Hot path: none. Benchmark: no change (no code change). Test: none applies to a comment.

### F-P2-4: NSM sources and stale gaps in the docs
- Severity: doc only
- Rule: ZIP-235-1, ZIP-237-4, ZIP-237-9, ZIP-234-3, ZIP-207-r0-10
- Facts:
  - `docs/consensus-rules.md:158` cites "NU7 deployment draft (`draft-valargroup-deploy-nu7`)" for the fee share: the source is ZIP 235.
  - `docs/consensus-rules.md:159-161` cite "zips#1354": the source is ZIP 237.
  - `docs/consensus-rules.md:162` cites "`draft-judah-nsm-halving-preserving-issuance`": the source is ZIP 237 (ZIP 259 `NSM_REISSUANCE_HEIGHT`).
  - `docs/consensus-rules.md:163` cites "ZIP 234, the same draft" for the bonus: ZIP 259 does not deploy ZIP 234; the source is ZIP 237.
  - `docs/plan-consensus-and-sync.md:70,204` name the bonus "ZIP 234".
  - `docs/install.md:22-26` says "Funding streams: the amounts and scripts of the funding outputs of the coinbase are not checked" and "Sprout JoinSplits and ZIP 234 issuance: the validator returns `Unsupported`". hayai-state checks the funding stream outputs in each fully validated block (`hayai-state/src/check.rs:981-982,1248-1249` call `check_coinbase_value`), and the reissuance bonus of ZIP 237 is implemented (`coinbase.rs` `CoinbaseTerms::after`).
- Input that shows it: none (documentation only).
- bd: hayai-7e1 (closed)
- Fix: fixed in this change: `docs/consensus-rules.md` is folded into `docs/consensus.md` with the ZIP 235 and ZIP 237 sources; `docs/plan-consensus-and-sync.md` names ZIP 237; `docs/install.md` lists the real gaps of a shadow node. The proposal was:
  - `docs/consensus-rules.md:158`: replace "NU7 deployment draft (`draft-valargroup-deploy-nu7`), ZIP 236" with "ZIP 235, ZIP 236".
  - `docs/consensus-rules.md:159-161`: replace "zips#1354" with "ZIP 237".
  - `docs/consensus-rules.md:162`: replace "halving-preserving NSM draft (`draft-judah-nsm-halving-preserving-issuance`)" with "ZIP 237, ZIP 259 (`NSM_REISSUANCE_HEIGHT`)".
  - `docs/consensus-rules.md:163`: replace "ZIP 234, the same draft" with "ZIP 237 (NU7 does not deploy ZIP 234)".
  - `docs/plan-consensus-and-sync.md:70,204`: replace "ZIP 234" with "ZIP 237".
  - `docs/install.md`: remove the line on funding streams, and change the last item to "Sprout JoinSplits: the validator returns `Unsupported`." (P4 owns the Sprout part and the ZIP 213 line above it, which is also stale per `docs/consensus-rules.md:83`.)

### F-P3-1: SIGHASH_SINGLE without a corresponding output in v5 and v6 transactions
- Severity: consensus divergence.
- Rule: ZIP-244-S.2a-2 (ZIP 244 S.2a: "Using SIGHASH_SINGLE without a corresponding output ... cause validation failure").
- Facts: upstream `zcash_primitives` 0.30.1 `sighash_v5.rs:89-94` hashes an empty output list when the input index has no output. hayai `SighashContext::transparent` passed that digest to the interpreter. Zakura `zakura-script/src/lib.rs:214-221` gives no sighash (a random digest), so the signature fails.
- Input that shows it: a v5 transaction with 2 transparent inputs and 1 transparent output, whose input 1 has a valid signature with hash type 0x03 over the digest with the empty output list. hayai accepted it; Zakura and the specification reject it.
- bd: hayai-x8v (closed).
- Fix: fixed in this change. `hayai-prepared/src/prepare.rs` `SighashContext::transparent` returns `None` for a v5 or v6 input with `SIGHASH_SINGLE` (with or without ANYONECANPAY) and no output at its index. `None` makes `CallbackTransactionSignatureChecker::check_sig` return false, as in Zakura.
- Hot path: yes, script verification of block validation and mempool admission (one comparison per signature check). Benchmark: no measurable change expected (one integer comparison per signature, no allocation, no read).
- Test: `prepare::tests::sighash_single_needs_the_output_of_its_index_from_v5`. Before the fix: `FAILED ... assertion left == right failed: input 1 has no output 1; left: Some([85, 253, ...]) right: None`. After the fix: `test prepare::tests::sighash_single_needs_the_output_of_its_index_from_v5 ... ok`; `cargo test -p hayai-prepared --release --lib`: 71 passed; `hayai-bench` `--test prepared --test conformance_txs --test conformance_blocks`: 8, 2 and 2 passed (the script vectors and the published block vectors still pass).

### F-P3-2: No direct tests for several parser-enforced rules
- Severity: missing check (tests only; the code enforces the rules).
- Rule: ZIP-244-S.2a-1, SPEC-4.10-1, SPEC-7.1.2-34, SPEC-7.1.2-51..53.
- Facts: the rules hold in upstream code or in `draft`, but no hayai test gives the refused input.
- Input that shows it: a v5 transaction with hash type 0x04; a coinbase v4 transaction with one Sapling spend; a v4 transaction with `valueBalanceSapling` = MAX_MONEY + 1.
- bd: hayai-cvi.
- Fix: not fixed (tests to add).

### F-P3-3: NU5 branch id in specification §4.10
- Severity: doc only (an error of the specification text; hayai is right).
- Rule: SPEC-4.10-12 (the NU5 item of SPEC-4.10-7..16).
- Facts: `protocol.tex` line 6954 (commit a735caf) says "All transactions MUST use the NU5 consensus branch ID 0xF919A198 as defined in ZIP 252". ZIP 252 line 73 defines `0xc2d6d0b4`. Zakura, zcashd and hayai (`hayai-consensus/src/network.rs`, test `network::tests::the_deployment_constants_of_the_zips`) use 0xC2D6D0B4, and every NU5 block of Mainnet and Testnet carries it.
- Input that shows it: a v5 transaction of an NU5 block with `nConsensusBranchId` = 0xF919A198: the specification text accepts it, ZIP 252, Zakura and hayai refuse it.
- bd: hayai-27o
- Fix: not fixed in hayai (no change needed). Report the error to the ZIPs repository.

### F-P4-1: No direct tests for parser-level point and encoding rules
- Severity: missing check (tests only; upstream code enforces the rules).
- Rule: ZIP-215-1..3, ZIP-216-1..4, ZIP-256-1, ZIP-256-4, SPEC-4.4-2, SPEC-4.5-2, SPEC-4.6-7, SPEC-7.3-1, SPEC-7.4-1, SPEC-7.5-1, ZIP-212-5, ZIP-258-7.
- Facts: the rules hold in `orchard` 0.15.5 (`action.rs:65-73`), `sapling-crypto` 0.7.0 (`verifier.rs:49,108`, `value.rs:167`), `ed25519-zebra`, and the `zcash_primitives` 0.30.1 parser. No hayai test gives a refused input for them.
- Input that shows it: an Orchard action with `rk` = 32 zero bytes; an Orchard action with `ephemeralKey` = 32 zero bytes; a Sapling spend with `rk` of small order; `cmx` = q_P; an Orchard or Ironwood coinbase output with a wrong lead byte.
- bd: hayai-8od.
- Fix: not fixed (tests to add; no behaviour change).

### F-P5-1: History tree of a shadow node after an upgrade activation
- Severity: missing check (shadow mode only).
- Rule: ZIP-221-1, ZIP-221-19 (also SPEC-3.9-1 and the anchor rows before the seed, documented trust limits).
- Facts: a shadow seed sets no history tree (`hayaid/src/node.rs:2365-2400`; no `HistoryState::from_peaks` from upstream; the Zakura RPC has no peaks: no `history_tree` in `zakura-rpc/src/methods.rs`). `history_after(None, ..)` returns `None` for every block at or after Heartwood (`hayai-state/src/history.rs:409`), so the tree stays unknown for the life of the node, also after the next upgrade activation, where a new tree of one leaf starts and needs no peaks (ZIP 221; Zakura `history_tree.rs:313-321`). `docs/hayaid.md` (Trust limits) counts the skipped blocks in `hayai_block_commitments_unchecked_total`. Zakura always checks (`zakura-state/src/service/check.rs:265-345`).
- Input that shows it: a Testnet shadow node seeded in NU6.3, then a block at height 4,465,027 (the second NU7 block) whose `hashBlockCommitments` is wrong. hayai: valid (not checked). Zakura: `InvalidChainHistoryBlockTxAuthCommitment`. Only the upstream Zakura node feeds a shadow node, so no such block reaches it in practice.
- bd: hayai-k6y.
- Fix: proposed, not applied. In `check_history`, when `view.history()` is `None` and `network.upgrade_at(height - 1) != network.upgrade_at(height)`, give `history_after` the parent `HistoryState::empty(<branch of height - 1>)` (the commitment of the activation block itself stays unchecked: it needs the old root). `check_history` then needs `cfg.network` (three callers: `contextual_check_with_outputs`, `PrebuiltBody::commit`, `checkpoint_layer`). Not applied because `hayai-bench/tests/conformance_blocks.rs` `history_after_activation` (l. 348-367) does the same step outside hayai-state, guarded by `roots_known`: with the change in hayai-state, a vector chain whose seeded roots are unknown would compute a leaf from roots that are not real and fail the next commitment. That file is not in P5. The patch must change both files together. Hot path: block validation, one `upgrade_at` call per block only when the parent tree is unknown; no benchmark change.

### F-P5-2: Wrong section of the anchor rule in hayai-state
- Severity: doc only.
- Rule: SPEC-3.6, SPEC-3.7 (anchor items, P4).
- Facts: the module doc of `hayai-state/src/check.rs` cited "the protocol specification §4.1.?" for "anchors must refer to some earlier block's final treestate". The rule is in §3.5 (Sprout), §3.6 (Sapling), §3.7 (Orchard, Ironwood) (`nu6_3.txt` lines 991, 1032, 1069, 1073).
- Input that shows it: none (comment).
- bd: hayai-dwy (closed).
- Fix: fixed in this change: the module doc now cites §3.5 to §3.7. No code change.

### F-P5-3: Seed of the history tree in the docs
- Severity: doc only.
- Rule: ZIP-221-1.
- Facts: `docs/consensus-rules.md` line 22 says "a base at or after Heartwood starts with an unknown tree, and the node seeds it with `HistoryState::from_peaks`". The module doc of `hayai-state/src/history.rs` said the same. hayaid calls `from_peaks` only to read its own state log (`hayaid/src/persist.rs:470`); a shadow seed leaves the tree unknown (F-P5-1).
- Input that shows it: none (doc).
- bd: hayai-5j5 (closed).
- Fix: fixed in this change: `history.rs` module doc, and the text now in `docs/consensus.md` (History tree state). Original proposal: fixed in `hayai-state/src/history.rs` (module doc). Proposed for `docs/consensus-rules.md` line 22: replace "a base at or after Heartwood starts with an unknown tree, and the node seeds it with `HistoryState::from_peaks` (peaks cannot be derived from headers)" with "a base at or after Heartwood comes back from the state log with `HistoryState::from_peaks`; the start state of a shadow node has no peaks (they cannot be derived from headers), so its tree stays unknown".

### F-P5-4: No BIP 30 check
- Severity: missing check.
- Rule: BLOCK-11.
- Facts: hayai-state has no check that a new transaction does not reuse the txid of an earlier transaction with unspent outputs. Zakura has none either (no `BIP30` in `zakura-state` or `zakura-consensus`). No input reaches the rule: a coinbase txid commits to the height (scriptSig height push, Spec §7.1.2; from NU5 `nExpiryHeight` = height in the ZIP 244 header digest), and each other transaction commits in its txid to a unique outpoint or nullifier (a source of funds is required; Orchard and Ironwood actions always reveal a nullifier). A second transaction with the same txid would need a repeated spend, which BLOCK-4, BLOCK-5 and SPEC-3.9-1 refuse.
- Input that shows it: none known.
- bd: hayai-wz8.
- Fix: not fixed: a check needs a coin read per transaction on the validation path for a rule that no input reaches, and it would differ from Zakura's code path only by cost.

### F-P6-1: Peers below the protocol version of the active upgrade
- Severity: missing check
- Rule: ZIP-204-28, ZIP-204-33, ZIP-204-81, ZIP-201-4, ZIP-201-5
- Facts: ZIP 204, "Network Upgrade Epoch Enforcement": "When a network upgrade activates ... a
  node MUST disconnect any peer whose negotiated protocol version is less than the protocol
  version associated with the current epoch." Zakura takes the minimum from the upgrade of the
  best tip (`zakura-network/src/protocol/external/types.rs:32-48`,
  `peer/minimum_peer_version.rs:73-80`), refuses a lower version at the handshake
  (`peer/handshake.rs:931-953`) and drops ready peers below it (`peer_set/set.rs:801-807`). In
  hayai, `Relay::set_min_peer_version` existed, but `hayaid` never called it: the minimum stayed
  at `INITIAL_MIN_PEER_VERSION` (170,150) for the life of the process.
- Input that shows it: a Regtest chain with NU6.3 at height 3, and a peer with the version
  170,150. Before the fix the peer stays connected at the tip 3; Zakura disconnects it. The same
  on Mainnet and Testnet after the NU6.3 activation (3,428,143 and 4,134,000).
- bd: hayai-y7x (closed)
- Fix: fixed in this change
- Fix details:
  - `hayaid/src/node.rs`: new `min_peer_version_at(params, height)` (the version of the upgrade
    of the tip, as Zakura); `Node::start` sets `RelayConfig::min_peer_version` from the start
    tip; `Driver::finish_commit` and `Driver::disconnect_to` call
    `Relay::set_min_peer_version` before they publish the tip.
  - `hayai-net/src/relay.rs` `Relay::set_min_peer_version`: an unchanged value now returns at
    once (one atomic swap). Only a change sweeps the peers.
  - Hot path: block commit and reorg (one atomic swap for each block; the sweep of the peers,
    tens of entries, runs only at an activation). No benchmark of `hayai-bench` runs the
    `hayaid` driver, so no benchmark number changes.
  - Test: `node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected`
    (`hayaid/src/node.rs`). It checks the two sides of the boundary (tip 2 connected, tip 3
    disconnected).
  - Before the fix:
    ```
    test node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected ... FAILED
    panicked at crates/hayaid/src/node.rs:3263:9:
    the NU6.2 version fails from NU6.3
    ```
  - After the fix:
    ```
    test node::tests::a_peer_below_the_version_of_the_upgrade_of_the_tip_is_disconnected ... ok
    test result: ok. 1 passed; 0 failed
    ```
  - Other tests run after the fix: `cargo test -p hayaid --release --lib` with the filters
    `node::tests`, `a_chain_crosses`, `compact_relay_and_a_legacy_peer`,
    `a_reorg_of_depth_three`, `a_node_synchronizes_from_three_peers`, `mempool::tests`: 13
    passed. `cargo test -p hayai-net --release` (all targets): 98 passed.
  - Foreign patch needed for the zakura backend (not run): `hayaid/src/sync_tests.rs` (owner
    not listed) fixes the version of its scripted peer at 170,160 (`fn version`, line 136).
    `a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization` calls `fetch_chain` at the
    tip 111 (NU7). On a build with the NU7 rule set the minimum is then 170,180 (Regtest), so
    the scripted peer is now refused, as Zakura would refuse it. Proposed change:
    ```diff
    -        version: 170_160,
    +        version: hayai_net::protocol::protocol_version(),
    ```
    On the default (upstream) backend the test stops at height 108 and passes (run above).

### F-P6-2: Pong before the end of the handshake
- Severity: missing check
- Rule: ZIP-204-17, ZIP-204-18
- Facts: ZIP 204, "Handshake Sequence": "A peer MUST NOT send any message other than
  `version` before receiving the remote peer's `version` message. A peer MUST NOT send any
  message other than `verack` after sending its `version` and before receiving the remote
  peer's `verack`." `PeerSession::on_message` answered each `ping` with a `pong`, also before
  the handshake. Zakura ignores every message other than `version`, then `verack`, in the
  handshake (`zakura-network/src/peer/handshake.rs:873-887`, `995-1010`).
- Input that shows it: a peer that sends `ping` before its `version`, or between its `version`
  and its `verack`, got a `pong`.
- bd: hayai-c2q (closed)
- Fix: fixed in this change
- Fix details: `hayai-net/src/session.rs` `PeerSession::on_message`: a `ping` gets a `pong`
  only after the handshake; before it the node ignores the `ping` (as Zakura). The old line of
  `session::tests::handshake_errors` that expected a `pong` now expects nothing. Hot path: no
  (one bool test per `ping`). No benchmark change. Test:
  `session::tests::a_ping_before_the_handshake_gets_no_pong`.
  - Before the fix:
    ```
    test session::tests::a_ping_before_the_handshake_gets_no_pong ... FAILED
    assertion `left == right` failed
      left: Ok([Send(Pong(3))])
     right: Ok([])
    ```
  - After the fix:
    ```
    test session::tests::a_ping_before_the_handshake_gets_no_pong ... ok
    test result: ok. 9 passed; 0 failed (session::)
    ```

### F-P6-3: The answer to `mempool` lists transactions that expire within 3 blocks
- Severity: missing check (and a wrong statement in `docs/mempool-policy.md`)
- Rule: ZIP-204-74
- Facts: ZIP 204, "Transaction Expiry": "A node SHOULD NOT relay a transaction that will expire
  within 3 blocks of its view of the current chain tip." `docs/mempool-policy.md` (section
  Relay) states that the answer to `mempool` leaves out such transactions with
  `PreparedStore::relay_ids`. No code calls `relay_ids` (it is dead code outside its test):
  `Relay::on_mempool` (`hayai-net/src/relay.rs:1623`) lists `TxLookup::for_each_id` of
  `PublicTxs` (`hayaid/src/mempool.rs`), which is every public transaction. The admission
  refuses a transaction that expires soon (`MempoolPolicy::check_expiry`), but a stored
  transaction comes within 3 blocks of its expiry as the chain grows. Zakura has no such rule
  (`docs/mempool-policy.md`), so this is a SHOULD that hayai claims and does not follow.
- Input that shows it: a stored transaction with the expiry height `tip + 3`; after one block it
  expires within 2 blocks of the next height, and a `mempool` message still lists it.
- bd: hayai-z94 (closed)
- Fix: fixed in this change (by the coordinator, with the patch of P6).
- Fix details:
  - `hayai-wire/src/lib.rs` trait `TxLookup`: new method `for_each_relay_id(next_height, f)`; the default visits every id.
  - `hayai-prepared/src/store.rs`: `PreparedStore` overrides it with `is_relayable`; the unused `PreparedStore::relay_ids` is removed and its test calls the trait method.
  - `hayaid/src/mempool.rs` `PublicTxs`: overrides it without the private transactions.
  - `hayai-net/src/relay.rs` `Relay::on_mempool`: calls it with the tip height + 1. `ShortIdIndex::build` keeps `for_each_id`, so compact block reconstruction does not change.
  - Hot path: no (one `mempool` message for each legacy peer). Benchmark: no change (the relay benchmarks do not send `mempool`).
  - Test: `the_answer_to_mempool_leaves_out_a_transaction_that_expires_soon` (`hayai-net/tests/loopback.rs`).
  - Before the fix (trait method with its default, relay still on `for_each_id`): `FAILED ... left: [Wtx(..7859..), Wtx(..630f..)] right: [Wtx(..630f..)]`.
  - After the fix: `test the_answer_to_mempool_leaves_out_a_transaction_that_expires_soon ... ok`; `cargo test -p hayai-net --release --test loopback`: 24 passed; `hayai-prepared` `store::` tests: 10 passed; `hayaid` `mempool::` tests: 1 passed.

### F-P6-4: An addrv2 entry of a known network with a wrong length is dropped, not refused
- Severity: missing check
- Rule: ZIP-155-8, ZIP-204-51
- Facts: ZIP 155: "Clients MUST reject messages that contain addresses that have a different
  length than specified in this table for a specific network ID". `Reader::addr_v2`
  (`hayai-net/src/codec.rs:884`) refuses a wrong length for IPv4 and IPv6, and drops a TORV3,
  I2P or CJDNS entry of any length. Zakura does the same: only IPv4 and IPv6 lengths are
  checked, every other id is `AddrV2::Unsupported`
  (`zakura-network/src/protocol/external/addr/v2.rs:292-305`). The ZIP and Zakura disagree, so
  the code stays.
- Input that shows it: an `addrv2` message with one entry of network id 0x04 (TORV3) and an
  address of 31 bytes: the ZIP refuses the message; hayai and Zakura accept it and drop the
  entry.
- bd: hayai-00t (open)
- Fix: not fixed (the specification and Zakura disagree; the brief says leave the code). A fix
  is one match arm in `Reader::addr_v2` (`(4 | 5, l) if l != 32`, `(6, l) if l != 16` give
  `DecodeError::AddrV2Length`) and a case in `addrv2_rejects_invalid_lengths`.

### F-P6-5: Inventory type 0 is accepted
- Severity: missing check
- Rule: ZIP-204-15, ZIP-239-8
- Facts: ZIP 204, "Inventory Vectors": "A node MUST reject inventory vectors with unrecognized
  type codes." The table lists the types 1, 2, 3 and 5. `Reader::inv`
  (`hayai-net/src/codec.rs:982`) accepts type 0 as `InvItem::Error`. Zakura accepts it too
  (`zakura-network/src/protocol/external/inv.rs:161-164`). The ZIP and Zakura disagree, so the
  code stays.
- Input that shows it: an `inv` with one entry of type 0: the ZIP refuses the message; hayai and
  Zakura accept it and ignore the entry.
- bd: hayai-eea (open)
- Fix: not fixed (the specification and Zakura disagree).

### F-P6-6: Misbehaviour points differ from the ZIP 204 table
- Severity: missing check
- Rule: ZIP-204-30, ZIP-204-44, ZIP-204-55, ZIP-204-57, ZIP-204-65, ZIP-204-79
- Facts: ZIP 204: "A node receiving a `getdata` message with more than 50,000 entries MUST
  assign a misbehavior penalty of 20 points"; the table gives 1 point for a duplicate `version`
  or a message before the handshake, and 20 or 100 points for the other limits. hayai treats each
  decode fault as `Misbehaviour::Malformed` (`hayai-sync/src/score.rs:69`, 50 points) and closes
  the connection (`hayai-net/src/relay.rs` `Relay::add_peer`, `Incoming::Malformed`); a
  message before the handshake and a duplicate `version` also cost 50 points and a
  disconnect. Zakura refuses such a message in its parser and closes the connection
  (`zakura-network/src/protocol/external/inv.rs:190-210`, `codec.rs:425`). The reaction of hayai
  is stronger than the ZIP and close to Zakura's.
- Input that shows it: a `getdata` with 50,001 entries: the ZIP gives 20 points and keeps the
  peer; hayai gives 50 points and disconnects it; a second fault in the decay time bans it.
- bd: hayai-b63 (open)
- Fix: not fixed (the ZIP and Zakura disagree; a lower penalty weakens the protection, and the
  score table is in `hayai-sync`, P1 file).

### F-P6-7: No setting for the ZIP 401 eviction memory time
- Severity: missing check
- Rule: ZIP-401-7
- Facts: ZIP 401: "There MUST be a configuration option `mempoolevictionmemoryminutes`, which
  SHOULD default to 60." hayai has the fixed constant `EVICTION_MEMORY` (60 min,
  `hayai-prepared/src/store.rs:63`), and `hayaid/src/config.rs` lists Zakura's
  `mempool.eviction_memory_time` as a setting that hayaid does not use. Zakura has the setting
  (`zakurad/src/components/mempool/config.rs:39,72`). The ZIP places its keywords on zcashd and on
  implementations that take the specification in full; `docs/mempool-policy.md` states that the
  store applies ZIP 401.
- Input that shows it: a configuration with `[mempool] eviction_memory_time = "10m"`: Zakura
  refuses a recently evicted transaction for 10 min; hayai warns that the key is unused and
  refuses it for 60 min.
- bd: hayai-6iw (open)
- Fix: proposed. `hayai-prepared/src/store.rs` (P3 file): `PreparedStore::new` takes the time,
  `RecentlyEvicted` stores it and `prune` uses it. `hayaid/src/config.rs` (P6): a field
  `MempoolSection::eviction_memory_minutes` (default 60), with the Zakura key
  `eviction_memory_time` read into it and removed from `ZAKURA_UNUSED`. `hayaid/src/node.rs`
  (P6) passes it to `PreparedStore::new`. Test: `an_evicted_transaction_is_refused_until_the_memory_expires`
  with a time of 10 min (refused at 9 min 59 s, admitted at 10 min 1 s). The config key is a new
  user-visible setting: the owner decides.

### F-P6-8: A MSG_WTX announcement was fetched before NU5
- Severity: missing check
- Rule: ZIP-239-7
- Facts: ZIP 239, "Deployment": before NU5 activates, "the node MUST NOT advertise, fetch, or
  provide v5 transactions." `Relay::on_inv` sent `getdata` for each unknown `MSG_WTX` entry
  whatever the branch. A v5 transaction before NU5 fails the parse or the version rule, so it
  never entered the store (no advertise and no provide), but the fetch happened.
- Input that shows it: a node whose next block has the Canopy branch, and a peer that sends
  `inv` with one `MSG_WTX` entry: the node sent `getdata` for it.
- bd: hayai-kdy (closed)
- Fix: fixed in this change
- Fix details: `hayai-net/src/relay.rs` `Relay::on_inv`: when the message has a `MSG_WTX` entry,
  the relay reads `ChainSource::tx_branch` once; before NU5 (Sprout to Canopy) it does not
  request `MSG_WTX` entries. Hot path: transaction relay (`inv` handling): one scan of the
  entries and, for a message with a `MSG_WTX` entry, one branch read (the same read that each
  `tx` message already makes). The relay benchmarks (`relay/forward_latency`,
  `relay/reconstruct`, `relay/bytes_on_wire`) do not use `inv`: no change. Test:
  `before_nu5_a_wtxid_announcement_is_not_fetched` (`hayai-net/tests/loopback.rs`), with a
  `MSG_TX` announcement as the control.
  - Before the fix:
    ```
    test before_nu5_a_wtxid_announcement_is_not_fetched ... FAILED
    unexpected GetData([Wtx(WtxId { txid: TxId("fefac738..."), ... })]) before the pong
    ```
  - After the fix:
    ```
    test before_nu5_a_wtxid_announcement_is_not_fetched ... ok
    test result: ok. 1 passed; 0 failed
    ```

## Unverified items

### Package P1

- The zakura backend: no test ran with `--no-default-features --features zakura`. The rows that depend on the NU7 rule set (`the_window_and_the_spacing_change_at_nu7`, ZIP-218-18, ZIP-259-3) cite tests that return early on the upstream backend.
- ZIP-200-13, ZIP-200-14, SPEC-7.6-M2, ZIP-252-7: P6 owns the code; P1 did not look for a direct test.
- ZIP-218-13: no test reaches `ContextError::TooManySaplingIos` (hayai-fuzz maps it to a rule class only). P5 owns `add_totals`.
- SPEC-7.6-8: hayai checks the Sapling root of Sapling and Blossom blocks on the checkpoint path, and Zakura does not check it. No test covers it at a Sapling-era height. P5 owns the check.
- Zakura line ranges of `adjusted_difficulty.rs` (`258-302` mean target, `304-365` bounded timespan) come from the function boundaries, not from a read of each line.
- The working tree is shared with other packages: `git status` shows changes of P2, P3 and P6 files that this package did not make.

### Package P2

- `INITIAL_NSM_VALUE_BALANCE`: hayai copies Zakura's measured constants (Mainnet 36,858,445,520 zatoshis = 368.58 ZEC, Testnet 55,768,414,957). ZIP 237 gives only "approximately 350.8 ZEC" with a TODO for the exact values; Zakura's comment (`zk-main:36-43`) says the ZIP estimate is about 17.8 ZEC low and that the pools held 15,749,631.41554480 ZEC at 2,726,399. The values need a full node with the chain value pools at 2,726,399 (Mainnet) and 2,975,999 (Testnet). A wrong Testnet constant makes hayai and Zakura refuse the Testnet block 4,465,025.
- The NU7 branches of the tests ran only on the default (`upstream`) backend, where `RuleSet::of(Upgrade::Nu7)` is `None` and the tests check the error path. The `zakura` backend runs (`--no-default-features --features zakura,baselines`) and the `baselines` comparisons (`conformance_subsidy`, `conformance_nu7`) were not run in this package.
- The ZIP 237 Mainnet example (A = 3,543,000 gives a reissuance height of 8,940,474) was checked with a Python copy of the closed form of `nsm::reissuance_height`, not with hayai: hayai has no Mainnet NU7 height to test it.
- ZIP-271-12 (maturity of the disbursement outputs) relies on the coinbase maturity rule of hayai-state, which P5 traces; this package did not read that code.

### Package P3

- F-P3-1 against history: no Mainnet or Testnet block was replayed with the fix. The published block vectors pass. Zakura rejects the same inputs, so a historical block that the fix refuses would also be refused by Zakura.
- Zakura line numbers for the transaction serialization rows point to the file only (`zakura-chain/src/transaction/serialize.rs`).

### Package P4

- ZIP 215 edge cases in `ed25519-zebra`: not read in this pass (the crate is the one Zebra and Zakura use).
- Zakura line numbers for the serializer rows: file only.

### Package P5

- zcashd source is not on this machine. The zcashd names (`ConnectBlock`, `bad-txns-BIP30`, `MAX_BLOCK_SIGOPS`, `bad-cb-missing`) come from memory and from Zakura comments, not from a read of zcashd.
- The Zakura line of the tree append in block order (`non_finalized_state/chain.rs:1686`) names the function; the per-transaction order inside it was not read.
- Mainnet and Testnet history were not replayed for the value pool rules; the statement that no block breaks them rests on Zakura enforcing the same rules at every height.
- The coordinator draft created bd hayai-76c ("§3.8 and §3.9 not extracted"); the rows are now in this file, so that issue can be closed.

### Package P6

- The zakura backend (`--no-default-features --features zakura`) was not built or run. With the
  fix of F-P6-1, `sync_tests::a_chain_crosses_nu7_at_the_tip_and_during_the_synchronization`
  is expected to fail there until the foreign patch of `sync_tests.rs` lands (its scripted peer
  states 170,160 at the NU7 tip, where the minimum is 170,180).
- The full `hayaid` lib test target was not run; only the filtered set listed under F-P6-1.
  `hayaid/tests` (Regtest pair, Zakura pair) need external binaries and were not run.
- Zakura line references for the Zakura reaction to an oversized `getdata` (F-P6-6) rest on
  the parser limit (`inv.rs:190-210`) and the general rule that a codec error closes the
  connection; the misbehaviour score that Zakura records then was not traced.
- Zakura references given only by file (no line), for example the timeouts in
  `zakura-network/src/peer/connection.rs`, `zakurad/src/components/inbound.rs` and
  `zakura-network/src/address_book.rs`, were not opened at the line; they name where the
  behaviour lives.
- `ZIP-204-67` cites `hayai-sync/tests/download.rs` without a test name at the 1,024 bound.
- The ids `ZIP-<n>-PEER` and `ZIP-<n>-PEERS` of the deployment ZIPs need P1's numbers.
- Clippy of `hayai-prepared --all-targets` fails in `hayai-prepared/src/prepare.rs` (P3 file, lines
  844 and 872, `redundant_pattern_matching`) from another package's work in progress; my
  change to `hayai-prepared/src/policy.rs` is comments only. Clippy of `hayai-net`, `hayaid` and
  `hayai-template` with `--all-targets -D warnings` passes.
