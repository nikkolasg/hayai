```
ZIP: XXXX
Title: Compact Block Relay with Pre-Disseminated Batches
Owners: hayai contributors <https://github.com/nikkolasg/hayai>
Status: Draft
Category: Network
Created: 2026-10-03
License: MIT
Discussions-To: <https://github.com/nikkolasg/hayai/issues>
```

# Terminology

The key words "MUST", "MUST NOT", "SHOULD", "SHOULD NOT", and "MAY" in this document are to
be interpreted as described in BCP 14 [^BCP14] when, and only when, they appear in all
capitals.

"Block header" means the serialized header of a Zcash block on the network of the
connection: the fixed fields, the CompactSize length of the Equihash solution, and the
solution. It is 1487 bytes on Mainnet and Testnet (Equihash (200, 9)) and 177 bytes on
Regtest (Equihash (48, 5)).

"Header check" means the following checks, which require no block body: the header parses;
`version >= 4`; the parent hash names a block the node has accepted; `bits` equals the value
the difficulty adjustment prescribes for that parent; `time` is after the parent's median time
past and at most 2 hours after the node's clock; the block hash is at or below the target
encoded by `bits`; the Equihash solution has the length of the network's parameters and is valid.

"WtxId" means the 64-byte transaction identifier of ZIP 239 [^zip-0239]: the txid followed by
the authorizing data commitment, with the authorizing data commitment set to 32 bytes of
`0xFF` for transactions of version 4 and below.

"Short id" means a 6-byte identifier derived from a WtxId under a per-block key, as defined
in the Specification.

"Full id" means a WtxId carried in a `CompactBlockV2` together with its index in the block.

"Batch" means an ordered list of WtxIds published by a batch publisher ahead of a block, and
"batch id" its 32-byte identifier.

"Lane" means a publisher's append-only sequence of batches.

"Candidate" means the transaction set of one revision of a publisher's block template on one
parent block, given as batches of its lane and the positions of their ids that left the
template.

"Canonical order" means the order of a block's transactions after the coinbase defined in
the section Canonical order.

"Legacy relay" means block relay by `inv`, `getdata` and `block` messages as implemented by
zcashd, Zebra and Zakura.

# Abstract

This proposal defines an extension of the Zcash peer-to-peer protocol for low-latency block
relay. A block is announced to capable peers as its header plus references to transactions:
6-byte short ids over the peer's mempool, in the manner of BIP 152 [^bip-0152], 32-byte batch
ids over transaction batches that miners publish in lanes before they find a block, and full
64-byte WtxIds for transactions the peer may not hold yet. A receiving node resolves the
references as soon as the header check passes. Once every position has a WtxId and the id
list matches the roots the header commits to, the node forwards the block, before it holds
every transaction and before it validates the body; it requests only the transactions it
lacks. A miner that publishes each revision of its template as a candidate lets its block
travel as a reference to the candidate plus the difference, when the block lists its
transactions in a canonical order. The extension is negotiated per connection and coexists
with legacy relay on the same connection, so nodes that do not implement it see no change.

# Motivation

Current Zcash node implementations relay a block only after they have fully validated and
committed it, by sending `inv` to a subset of peers, waiting for `getdata`, and sending the
full body. Each hop therefore costs one and a half round trips, the transfer of the full body
(up to 2,000,000 bytes), and one full validation. Across the three to four hops that separate
most miners, a full block takes seconds to reach the whole network.

Three properties of Zcash make this costlier than it is for Bitcoin:

- Shielded transactions are large: an Orchard action adds roughly 820 bytes plus proof data,
  and a two-action transaction is about 9 kB. A 6-byte reference replaces 2.5–9 kB; a mostly
  shielded 2 MB block becomes 10–20 kB.
- The target block spacing drops from 75 seconds to 25 seconds at NU7 (ZIP 218 [^zip-0218],
  scheduled for Mainnet activation in November 2026).
  The probability that a competing block is found during a delay of `d` seconds is roughly
  `d / 25`, so one second of relay delay costs about 4 % of blocks to the miner who found it.
- Mining is concentrated in a small number of pools whose candidate blocks are known to
  themselves well before the proof of work is found. Publishing those candidates as batches
  lets every peer prepare the body in advance, which removes the remaining round trip.

Separating the dissemination of transaction data from the announcement of block order is the
mechanism that gives DAG-based mempool designs their throughput [^narwhal] [^autobahn]. Under
proof of work no availability certificate is needed: the block header is the ordering
decision, and a publisher who withholds a batch it announced only harms the propagation of its
own block.

# Requirements

- A node implementing this ZIP MUST remain able to exchange blocks and transactions with
  nodes that do not, with no change in behaviour toward them.
- Protocol choice MUST NOT affect which blocks a node accepts. The extension changes how
  bodies are obtained, never what is valid.
- A reconstructed body MUST be verified against the header's merkle root before any use. An
  id list MUST be verified against the header's merkle root before a node forwards a block
  it does not hold.
- The extension MUST be versioned so that it can evolve without a flag day.

# Non-requirements

- This ZIP does not change consensus rules.
- This ZIP does not specify erasure coding or UDP transport. It is compatible with, and
  complementary to, such designs: they address the case where the receiver does not hold the
  transactions, which this ZIP makes rare.
- This ZIP does not require miners to publish batches. Short ids alone give most of the
  benefit; lanes remove the last round trip for participating miners.

# Specification

## Service bit and negotiation

A node implementing this ZIP sets the service bit `NODE_COMPACT_RELAY = (1 << 26)` in its
`version` message.

After `verack`, a node that set the bit and sees the bit set by its peer MUST send a
`zcmpctver` message:

| Field | Type | Description |
|---|---|---|
| `max_version` | `uint16` | highest extension version supported |
| `min_version` | `uint16` | lowest extension version supported |
| `features` | `uint64` | bit set of optional features (below) |

Both peers compute `v = min(max_version_local, max_version_remote)`. If
`v >= min_version_local` and `v >= min_version_remote`, the extension is active at version `v`;
otherwise the connection continues as a legacy connection. A node MUST ignore feature bits it
does not know. A node that never receives `zcmpctver` treats the connection as legacy.

This document defines versions 1 and 2 and the following feature bits:

| Bit | Feature |
|---|---|
| 0 | Compact blocks with short ids |
| 1 | Batch lanes |
| 2 | Candidates (`CandidateAnnounce`, `CandidateBlock`) |

A feature is in use on a connection only if both peers set its bit. The candidates feature
is in use only together with batch lanes.

| Version | Behaviour |
|---|---|
| 1 | `CompactBlock` (type 6) only. A node forwards a block after it reconstructs the body. |
| 2 | Adds `CompactBlockV2` (type 12), which has the full-id section. A node forwards a block once the id list matches the header. |

A node implementing this document SHOULD offer `max_version = 2` and `min_version = 1`.

## Message framing

Extension messages are carried as the payload of a regular Zcash P2P message with command
`zcmpct`. The payload is one frame:

```
length   uint32    byte length of what follows
type     uint8     message type (table below)
body     ...
```

Within a frame, integers are little-endian, counts are `CompactSize`, and a `CompactSize` MUST
be in canonical (shortest) form. A frame larger than 8,388,608 bytes MUST be rejected.

| Type | Name | Body |
|---|---|---|
| 1 | `TxAnnounce` | `count`, `WtxId[count]` |
| 2 | `TxRequest` | `count`, `WtxId[count]` |
| 3 | `Tx` | `count`, (`len uint32`, transaction bytes)[count] |
| 4 | `BatchAnnounce` | `lane_id[32]`, `seq uint64`, `batch_id[32]`, `count`, `WtxId[count]` |
| 5 | `BatchRequest` | `count`, `batch_id[32][count]` |
| 6 | `CompactBlock` | see below |
| 7 | `BlockTxnRequest` | `block_hash[32]`, `count`, differential index[count] |
| 8 | `BlockTxn` | `block_hash[32]`, `count`, (`len uint32`, transaction bytes)[count] |
| 9 | `Block` | `len uint32`, block bytes |
| 10 | `CandidateAnnounce` | see Candidates |
| 11 | `CandidateBlock` | see Candidates |
| 12 | `CompactBlockV2` | see below (version 2) |

The type names the layout of the body. Every field of a layout MUST be present, and an
empty list is a zero count. A receiver MUST NOT decide from the remaining length of a frame
whether a field is present. A receiver MUST treat a frame that ends before the last field
of its type, or that has bytes after it, as malformed.

Differential indexes are encoded as in BIP 152: each index is stored as the difference from
the previous index plus one, with the first stored as is.

## Short ids

Given a block header `H` (all its serialized bytes) and a 64-bit `nonce` chosen by the
sender:

```
k = SHA-256(H || nonce_le64)
k0 = k[0..8] as little-endian uint64
k1 = k[8..16] as little-endian uint64
short_id(wtxid) = SipHash-2-4(k0, k1, wtxid_64_bytes)[0..6] (little-endian low 6 bytes)
```

## Batch ids and lanes

```
batch_id = BLAKE2b-256("hayai:batch" || wtxid_0 || wtxid_1 || ... || wtxid_n-1)
```

where BLAKE2b-256 is BLAKE2b with a 32-byte output and no personalization, and the prefix is
the 11 ASCII bytes shown. A batch MUST contain at least one WtxId.

A lane is identified by a 32-byte `lane_id` chosen by its publisher. Batches in a lane carry
strictly increasing `seq` values. A receiver:

- keeps at most 64 lanes per peer and at most 256 batches per lane, discarding the oldest;
- drops a lane after 10 minutes without a new batch;
- on `BatchAnnounce`, requests any WtxIds it does not hold with `TxRequest`, and marks the batch
  complete once all its transactions are held;
- answers `BatchRequest` with the corresponding `BatchAnnounce` if it holds the batch.

A publisher SHOULD announce a batch only for transactions it has already announced, and MUST
serve `TxRequest` for them while the batch is within its retention window.

## CompactBlock and CompactBlockV2

```
header            block header (self-delimiting: the solution has a CompactSize length)
nonce             uint64
batch_count       CompactSize
batch_refs        batch_id[32][batch_count]
short_count       CompactSize
short_ids         byte[6][short_count]
prefilled_count   CompactSize
prefilled         (differential index, len uint32, transaction bytes)[prefilled_count]
full_count        CompactSize                         type 12 only
full_ids          (differential index, WtxId)[full_count]   type 12 only
```

The body of type 6 (`CompactBlock`) ends after `prefilled`. The body of type 12
(`CompactBlockV2`) has the same fields, then the full-id section: `full_count` and
`full_ids`. In the rest of this document, "compact block" means a message of either type,
and a type 6 message has no full ids.

A sender MUST use type 12 if and only if the block has at least one full id for the peer,
and MUST use type 6 for every other block, on a version 2 connection also. A receiver MUST
treat a type 12 frame with `full_count = 0` as malformed. Each compact block therefore has
exactly one encoding. A sender MUST NOT send type 12 on a version 1 connection, and a
receiver on a version 1 connection MUST treat a type 12 frame as malformed.

The transactions of the block, in order, are obtained as follows. Prefilled transactions and
full ids occupy their stated indexes. The remaining positions are filled, in increasing index
order, first by the WtxIds of each referenced batch in order (a batch reference therefore
covers a contiguous run of positions that are neither prefilled nor full ids), then by the
short ids in order. The total transaction count is
`prefilled_count + full_count + sum(batch sizes) + short_count`. The prefilled indexes MUST
be strictly increasing, the full-id indexes MUST be strictly increasing, no index MAY appear
in both lists, and every index MUST be below the transaction count.

The coinbase transaction MUST be prefilled. A sender MUST NOT reference a batch it has not
announced to the receiving peer. A sender SHOULD prefill a transaction that reached it only
inside this block (one outside its mempool), because the receiving peer cannot have it.

On a version 2 connection, a sender SHOULD send as a full id every transaction that it
announced to the receiving peer less than 3 seconds ago and that the peer has not announced
back, and every transaction it never announced to that peer. A sender MUST send as a full
id every transaction whose bytes it does not hold. Every other transaction SHOULD be a
short id or part of a batch reference.

On a version 1 connection, a sender SHOULD send as a short id or batch reference every
transaction in its mempool and prefill the rest.

## Canonical order

The depth of a transaction in a set is 0 when it spends no output of another transaction of
the set, and 1 plus the largest depth of those parents otherwise. The canonical order of a
set sorts it by depth, then by txid, where a txid compares as a 32-byte string in internal
byte order. A parent has a smaller depth than its children, so the canonical order is a
valid order for the transactions of a block after its coinbase, and the same set always
gives the same block bytes. Consensus does not require the canonical order; the candidate
form of a block requires it.

## Candidates

A node that publishes a lane and sets bit 2 SHOULD publish each change of its block template
on a parent as one `BatchAnnounce` of the transactions the change added (absent when it only
removed transactions), with `seq` equal to the template revision, and one
`CandidateAnnounce` with the same `seq`:

```
lane_id          byte[32]
seq              uint64     strictly increasing within the lane
parent           byte[32]   hash of the block the template extends
batch_count      CompactSize
batches          batch_id[32][batch_count]   batches of this lane, in lane order
removed_count    CompactSize
removed          differential index[removed_count]
```

The id list `L` of a candidate is the concatenation of the WtxIds of its batches in order,
without the positions in `removed`, counted from 0 over the concatenation. `removed` MUST be
strictly increasing and every position MUST be below the length of the concatenation. A
candidate MUST NOT name more than 64 batches. A publisher SHOULD keep each WtxId at most
once in `L`, and SHOULD start a new candidate from one batch on a new parent or when the
batch limit is reached.

A block whose transactions after the coinbase are in canonical order MAY be sent as a
`CandidateBlock`:

```
header           block header
nonce            uint64      short-id key nonce of short_ids
lane_id          byte[32]
seq              uint64
flags            uint8       bit 0: canonical order; bits 1 to 7: zero
coinbase         len uint32, transaction bytes
removed_count    CompactSize
removed          differential index[removed_count]   positions in L
short_count      CompactSize
short_ids        byte[6][short_count]
full_count       CompactSize
full_ids         WtxId[full_count]
```

The block is the coinbase followed by the canonical order of `L` without the positions in
`removed`, together with the transactions the short ids and full ids name. A sender MUST set
`flags` to 1. A sender MUST NOT send `CandidateAnnounce` or `CandidateBlock` to a peer on
which bits 1 and 2 are not both in use, and SHOULD use the candidate form only when the
difference (removed plus added transactions) is smaller than the number of transactions
after the coinbase. It chooses the form of each addition as for `CompactBlock`.

A receiver:

- MUST treat a `CandidateAnnounce` or `CandidateBlock` from a peer on which bits 1 and 2 are
  not both in use as a protocol violation;
- keeps at most 16 candidates per lane and at most 64 lanes, drops a lane after 10 minutes
  without an announcement, and keeps the first announcement of a `(lane_id, seq)`;
- requests the batches of a candidate it does not hold with `BatchRequest`, and SHOULD send
  a new candidate once to every peer with the feature other than the sender, after it holds
  every batch and every transaction of the candidate, and MUST NOT send it before;
- MUST treat a `CandidateBlock` whose `flags` are not 1 as malformed;
- after the header check, rebuilds the set of a `CandidateBlock`. When the candidate is not
  held, extends another parent, names a batch the receiver does not hold, has a removed
  position out of range, has a short id that does not resolve, or names a transaction
  twice, the receiver requests the full block and penalizes no peer;
- requests the bytes it lacks with `TxRequest`, and once it holds every transaction, sorts
  the set in canonical order and continues at step 6 of the next section.

## Receiving a CompactBlock

1. Parse the header and run the header check. A block already in the receiver's chain fails
   the check. On failure, discard the message and penalize the peer.
2. If the block hash was already announced, remember the sender as a further source for
   steps 5, 8 and 9 and stop.
3. Expand batch references. If any batch id is unknown, send `BatchRequest` for it and retry
   resolution when it arrives; after 2 seconds without it, fall back to step 9.
4. Resolve short ids against the local transaction set. The node computes the short id of every
   transaction it holds once per block. If two held transactions share a short id, both are
   treated as unresolved. Full ids and batch entries are known ids; the local set supplies
   their bytes when it holds them. Each position is now held (id and bytes), known (id
   only) or unknown.
5. Request unknown positions with `BlockTxnRequest` from the peer that sent the block; the peer
   answers with `BlockTxn`. If it does not answer within 2 seconds, request from any other peer
   that announced the block.
6. Once no position is unknown, verify the id list against the header. The node MUST compute
   the merkle root of the txids and compare it with the header's merkle root. If the node
   knows the ZIP 221 history root of the parent block, it MUST also compute the ZIP 244
   authorizing data root from the auth digests of the ids and check
   `hashBlockCommitments = BLAKE2b-256^"ZcashBlockCommit"(history_root || auth_data_root
   || [0u8; 32])` against the header. A node that does not know the history root proceeds on
   the merkle root alone. A mismatch is a short-id collision or a stale local entry, not a
   fault of the sender: forward nothing, go to step 9, and penalize no peer.
7. On a match, forward the block to every peer on which version 2 is active, as a new
   `CompactBlock` built by this node: short ids under a nonce chosen by this node (never the
   received nonce or short ids), batch references and full ids unchanged, and a full id for
   every known position whose bytes this node does not hold.
8. Request the bytes of the known positions with `TxRequest` from every peer that announced
   the block, in parallel. A peer that holds the bytes answers with `Tx`. A peer that is
   itself completing the block MUST answer once its bytes arrive. If no answer arrives within
   2 seconds, go to step 9.
9. If resolution cannot complete, request the full block with `getdata` (legacy) or
   `Block` from any announcing peer.
10. Once every position is held, assemble the body and verify its merkle root against the
    header again. Forward the block to every peer on which version 1 is active, as in step 7,
    and send `inv` for the block to legacy peers.
11. Validate the block as usual. A block whose body is invalid is not re-forwarded by the node
    that detects it, and the sending peer is penalized.

A node SHOULD serve `getdata` for the block to legacy peers from the assembled bytes as soon as
step 10 succeeds, before step 11 completes. A node MUST drop a block whose resolution has not
completed within a bounded time (20 seconds is suggested); a later announcement of the same
block starts over.

A node that receives `BatchAnnounce` for a batch it did not know SHOULD send it once to every
peer with the lanes feature other than the sender, after it holds all of the batch's
transactions, and MUST NOT send it before.

## Sending a block

A node that finds a block sends `CompactBlock` to each extension peer immediately. It chooses
batch references from lanes it knows the peer holds, full ids as the CompactBlock section
prescribes on version 2 connections, short ids for the rest, and prefills the coinbase and
any transaction outside its mempool. It sends `inv` to legacy peers at the same moment. A
node that receives a block sends it on as the Receiving section prescribes.

## Interaction with legacy relay

On a connection where the extension is not active, nothing in this document applies. On a
connection where the extension is active and the candidates feature is not, a node sends a
block it received in the candidate form as a `CompactBlock`. On a
connection where it is active, legacy `inv`/`getdata`/`block` messages remain valid and MUST be
handled; a node MAY use them at any time, and MUST use them as the fallback in step 6.

# Rationale

- **Forwarding after the header check.** Producing a header that passes the check requires a
  valid proof of work at the current target, which costs a block reward. Forwarding before body
  validation is the choice of BIP 152's high-bandwidth mode for the same reason. A node that
  forwarded an announcement whose body later fails validation has relayed at most 1,487 bytes
  plus references per peer.
- **Forwarding on the id list.** With forwarding after reconstruction, each hop that lacks
  one transaction of the block adds one `BlockTxnRequest` round trip, so a block that carries
  a transaction announced a second earlier costs one round trip per hop. A WtxId is the txid
  followed by the authorizing data commitment, so a complete id list determines both roots
  the header commits to. A node that has checked them knows the block's content up to the
  bytes it has not received, and can reference that content for the next hop. The id check
  and the rebuild cost under 3 ms per hop in the reference implementation, against one
  network round trip. Two-phase announcements were rejected because they only help while a
  node waits for data, which full ids remove; per-link short-id keys were rejected because
  they still need local resolution and re-keying and add handshake state without saving the
  round trip.
- **Full ids with indexes.** A fresh transaction can sit anywhere in a block ordered by fee.
  An indexed entry leaves the batch and short-id positions around it untouched; without an
  index, every position after the first fresh transaction would need the long form. The cost
  is 58 bytes per fresh transaction over a short id.
- **Batch references in addition to short ids.** Short ids assume the receiver already holds the
  transactions, which fails for a node that just started or sits behind a slow link. A batch
  announcement names exactly what will be needed and gives the receiver time to fetch it before
  the block exists, and removes the `BlockTxnRequest` round trip in the common case.
- **No availability certificates.** Proof-of-work ordering does not wait for a quorum. A
  publisher who announces a batch and then withholds it delays only its own block.
- **Six-byte short ids.** With 50,000 held transactions the per-block collision probability is
  about 5 × 10⁻⁶, and a collision costs one request round trip, never an incorrect block,
  because the merkle root is checked before any use.
- **Extension messages inside legacy framing.** One framing per connection keeps proxies,
  firewalls and existing peer management code unchanged, and allows the fallback in step 6 on
  the same connection.
- **Canonical order.** A batch reference covers a run of consecutive positions. In a block
  ordered by fee, a new transaction with a high fee lands in the middle and breaks every
  batch after it. A block in canonical order is a function of its transaction set, so a
  candidate names the set and the order follows. Selection by fee is unchanged.
- **The type names the layout.** An earlier draft put the full-id section at the end of type
  6 and marked it by its presence. A body cut before the section was then a valid body
  without full ids, and a hop could remove the section and keep a valid message. Only the
  frame length and the checksum of the carrying message caught a cut. With one type for
  each layout and no optional field, no proper prefix of a frame decodes, and each message
  has one encoding. The type follows the content of the block and not the negotiated
  version, so a codec needs no state of the connection.
- **Candidates as sets.** Each `CandidateAnnounce` names every batch of its candidate, so a
  receiver that missed one announcement still resolves the next. Additions, removals and fee
  changes alter only the difference. A block equal to its candidate costs 61 bytes in the
  frame besides the header and the coinbase, against 6 bytes per transaction with short
  ids.
- **A feature bit for candidates.** Only lane publishers produce candidates, any extension
  version can carry them, and a peer that leaves the bit clear sees no change. A new
  version would make every later version carry them.
- **Version negotiation by range.** `min`/`max` lets a node drop support for an old version
  without a flag day, and lets two nodes with disjoint ranges fall back to legacy relay rather
  than disconnect.

# Security and Privacy Considerations

- Unvalidated forwarding is bounded by proof of work; the per-peer penalty on invalid bodies
  prevents repeated use of one solution.
- Short ids are keyed per block with a sender-chosen nonce, so an attacker cannot precompute
  colliding transactions for a future block. Short ids are never relayed: a relayed nonce
  would let one sender grind collisions against every mempool on the network, and a node
  cannot recompute short ids for transactions it has not resolved.
- Batch ids and full ids are safe to relay unchanged. A batch id is the hash of its WtxIds
  and a full id is a WtxId, so a hop cannot change what they name, and the id list they
  produce is checked against the header before it is forwarded. The merkle root of the body
  is checked again before any use, so a forwarded reference never becomes a wrong block.
- A node that forwards a block it does not yet hold has relayed one header with references
  per peer, as in header-first relay, bounded by proof of work. It then owes its peers the
  answers to their `TxRequest`s, which it gives once its own are answered, and the body to
  legacy and version 1 peers once complete. A block whose bytes never arrive is dropped
  after the bounded time; the next announcement starts over. A miner who withholds the bytes
  of a valid header delays only its own block.
- A transaction with the same txid and different authorizing data passes the merkle check.
  It fails the `hashBlockCommitments` check when the parent's history root is known. When it
  is not, the hops in between have forwarded references to a block that fails validation,
  which is the same exposure as header-first relay, and the body check at step 10 and
  validation at step 11 still reject it.
- Lane and batch retention are bounded per peer (64 lanes, 256 batches per lane, 10-minute
  expiry), bounding memory an attacker can consume.
- Transactions learned through batches are subject to the same admission policy as gossiped
  transactions; a lane cannot bypass mempool policy.
- A candidate is content-addressed by its batches: a batch id is the hash of its WtxIds and
  the removed positions index them. A candidate block therefore resolves to an id list that
  the header roots check before any forwarding. A publisher that announces two candidates
  under one `(lane_id, seq)` makes some receivers rebuild another set; the merkle check fails
  and they fetch the full block. A malicious lane can only waste bandwidth within the
  limits above (64 lanes, 16 candidates per lane, 64 batches per candidate).
- A batch publisher reveals its candidate block ahead of time. This is a choice of the
  publisher, and the information is the same a pool reveals to its own hashers through
  `getblocktemplate` or Stratum.
- Block reconstruction from the local transaction set does not change what a node reveals
  about its mempool beyond what `inv` already reveals.

# Reference Implementation

The `hayai-relay` and `hayai-net` crates of the hayai repository implement the codec, short
ids, full ids, lanes, candidates and the canonical order, resolution and id verification,
forwarding on the id list, negotiation of versions 1 and 2 and of the feature bits, and the
legacy coexistence rules described here, with test vectors for
every message and loopback tests of the multi-hop behaviour.

# Version History

- Version 1: short ids, batch references and prefilled transactions; a node forwards after
  it reconstructs the body.
- Version 2: `CompactBlockV2` (type 12) with the full-id section, forwarding once the id
  list matches the header, `TxRequest` to every announcer for the missing bytes with
  deferred answers, and flooding of batch announcements once complete.
- Feature bit 2: the canonical order, `CandidateAnnounce` and `CandidateBlock`.
- An earlier draft of version 2 carried the full-id section as an optional tail of type 6.
  That layout is not compatible with this document.

# References

[^BCP14]: [Information on BCP 14 — "RFC 2119: Key words for use in RFCs to Indicate Requirement Levels" and "RFC 8174: Ambiguity of Uppercase vs Lowercase in RFC 2119 Key Words"](https://www.rfc-editor.org/info/bcp14)

[^bip-0152]: [BIP 152: Compact Block Relay](https://github.com/bitcoin/bips/blob/master/bip-0152.mediawiki)

[^zip-0239]: [ZIP 239: Relay of Version 5 Transactions](https://zips.z.cash/zip-0239)

[^zip-0218]: [ZIP 218: Reduce the block target spacing and bound per-block shielded actions](https://zips.z.cash/zip-0218)

[^narwhal]: [Danezis, Kokoris-Kogias, Sonnino, Spiegelman. Narwhal and Tusk: A DAG-based Mempool and Efficient BFT Consensus. EuroSys 2022](https://arxiv.org/abs/2105.11827)

[^autobahn]: [Giridharan, Suri-Payer, Abraham, Alvisi, Crooks. Autobahn: Seamless high speed BFT. SOSP 2024](https://arxiv.org/abs/2401.10369)
