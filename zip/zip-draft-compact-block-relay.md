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

- "Block header" means the serialized header of a Zcash block on the network of the
  connection. It contains the fixed fields, the CompactSize length of the Equihash solution,
  and the solution. It is 1487 bytes on Mainnet and Testnet (Equihash (200, 9)) and 177 bytes
  on Regtest (Equihash (48, 5)).
- "Header check" means the checks that follow. These checks need no block body.
  - The header parses.
  - `version >= 4`.
  - The parent hash names a block that the node accepted.
  - `bits` equals the value that the difficulty adjustment sets for that parent.
  - `time` is after the median time past of the parent and at most 2 h after the clock of
    the node.
  - The block hash is at or below the target that `bits` encodes.
  - The Equihash solution has the length that the parameters of the network set, and it is
    valid.
- "WtxId" means the 64-byte transaction identifier of ZIP 239 [^zip-0239]. It is the txid
  followed by the authorizing data commitment. For transactions of version 4 and below, the
  authorizing data commitment is 32 bytes of `0xFF`.
- "Short id" means a 6-byte identifier that a node derives from a WtxId with a key for each
  block. The Specification defines it.
- "Full id" means a WtxId that a `CompactBlockV2` carries together with its index in the
  block.
- "Batch" means an ordered list of WtxIds that a batch publisher publishes before a block.
  "Batch id" means the 32-byte identifier of a batch.
- "Lane" means the append-only sequence of batches of a publisher.
- "Candidate" means the transaction set of one revision of the block template of a publisher
  on one parent block. A candidate is a list of batches of the lane of the publisher, plus the
  positions of their ids that left the template.
- "Canonical order" means the order of the transactions of a block after the coinbase, as the
  section Canonical order defines it.
- "Legacy relay" means block relay by `inv`, `getdata` and `block` messages, as zcashd, Zebra
  and Zakura implement it.

# Abstract

This ZIP defines an extension of the Zcash peer-to-peer protocol for block relay with low
latency. A node announces a block to capable peers as its header plus references to
transactions. The references are of the kinds that follow:

- 6-byte short ids over the mempool of the peer, in the manner of BIP 152 [^bip-0152].
- 32-byte batch ids over transaction batches that miners publish in lanes before they find a
  block.
- Full 64-byte WtxIds for transactions that the peer possibly does not hold yet.

A receiver resolves the references immediately after the header check passes. The node
forwards the block when every position has a WtxId and the id list matches the roots that the
header commits to. The node forwards the block before it holds every transaction and before
it validates the body. It requests only the transactions that it does not hold.

A miner can publish each revision of its template as a candidate. When the block lists its
transactions in the canonical order, the miner then sends its block as a reference to the
candidate plus the difference. The peers of a connection negotiate the extension for that
connection. The extension coexists with legacy relay on the same connection, so nodes that do
not implement it see no change.

# Motivation

Current implementations of a Zcash node forward a block only after they fully validate and
commit it. To forward a block, a node does these steps:

1. It sends `inv` to a subset of peers.
2. It waits for `getdata`.
3. It sends the full body.

Thus each hop costs:

- 1.5 round trips.
- The transfer of the full body (up to 2,000,000 bytes).
- 1 full validation.

Most miners are 3 to 4 hops apart. Thus a full block needs some seconds to get to the whole
network.

3 properties of Zcash make this cost higher than for Bitcoin:

- Shielded transactions are large. An Orchard action adds approximately 820 bytes plus proof
  data, and a transaction with 2 actions is about 9 kB. A 6-byte reference replaces
  2.5–9 kB. A 2 MB block with mostly shielded transactions becomes 10–20 kB.
- The target block spacing decreases from 75 s to 25 s at NU7 (ZIP 218 [^zip-0218]). The
  Mainnet activation of NU7 is scheduled for November 2026. The probability that another
  miner finds a competing block during a delay of `d` seconds is approximately `d / 25`. Thus
  a relay delay of 1 s costs about 4 % of blocks to the miner who found the block.
- A small number of pools do most of the mining. Each pool knows its candidate blocks long
  before it finds the proof of work. When a pool publishes those candidates as batches, every
  peer can prepare the body in advance. This removes the remaining round trip.

DAG-based mempool designs separate the dissemination of transaction data from the
announcement of the block order. This separation gives them their throughput [^narwhal]
[^autobahn]. Under proof of work, no availability certificate is necessary. The block header
is the decision on the order. A publisher who withholds a batch that it announced harms only
the propagation of its own block.

# Requirements

- A node that implements this ZIP MUST stay able to exchange blocks and transactions with
  nodes that do not implement it. Its behaviour toward those nodes MUST NOT change.
- The choice of protocol MUST NOT change which blocks a node accepts. The extension changes
  how a node gets bodies, never what is valid.
- A node MUST check a reconstructed body against the merkle root of the header before any
  use. A node MUST check an id list against the merkle root of the header before it forwards
  a block that it does not hold.
- The extension MUST have a version, so that it can change without a flag day.

# Non-requirements

- This ZIP does not change consensus rules.
- This ZIP does not specify erasure coding or UDP transport. It is compatible with such
  designs, and it adds to them. Such designs address the case where the receiver does not
  hold the transactions. This ZIP makes that case rare.
- This ZIP does not require miners to publish batches. Short ids alone give most of the
  benefit. Lanes remove the last round trip for the miners that publish batches.

# Specification

## Service bit and negotiation

A node that implements this ZIP sets the service bit `NODE_COMPACT_RELAY = (1 << 26)` in its
`version` message. After `verack`, a node MUST send a `zcmpctver` message when it set the bit
and its peer also set the bit. The message has these fields:

| Field | Type | Description |
|---|---|---|
| `max_version` | `uint16` | highest extension version supported |
| `min_version` | `uint16` | lowest extension version supported |
| `features` | `uint64` | bit set of optional features (below) |

Both peers compute `v = min(max_version_local, max_version_remote)`. If
`v >= min_version_local` and `v >= min_version_remote`, the extension is active at version `v`.
If not, the connection continues as a legacy connection. A node MUST ignore feature bits that
it does not know. A node that never receives `zcmpctver` treats the connection as legacy.

This ZIP defines versions 1 and 2 and the feature bits that follow:

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

A node that implements this ZIP SHOULD offer `max_version = 2` and `min_version = 1`.

## Message framing

A regular Zcash P2P message with the command `zcmpct` carries each extension message as its
payload. The payload is 1 frame:

```
length   uint32    byte length of what follows
type     uint8     message type (table below)
body     ...
```

In a frame, integers are little-endian and counts are `CompactSize`. A `CompactSize` MUST be
in canonical (shortest) form. A receiver MUST reject a frame larger than 8,388,608 bytes.

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

The type names the layout of the body. Every field of a layout MUST be present. An empty list
is a zero count. A receiver MUST NOT decide from the remaining length of a frame whether a
field is present. A receiver MUST treat a frame that ends before the last field of its type,
or that has bytes after it, as malformed.

The encoding of differential indexes is as in BIP 152. The sender encodes each index as the
difference from the previous index plus 1. The sender encodes the first index as it is.

## Short ids

The sender chooses a 64-bit `nonce`. With the block header `H` (all its serialized bytes) and
that `nonce`, the short id is:

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

BLAKE2b-256 is BLAKE2b with a 32-byte output and no personalization. The prefix is the 11
ASCII bytes in the formula. A batch MUST contain at least 1 WtxId.

The publisher of a lane chooses a 32-byte `lane_id` that identifies the lane. The batches in
a lane carry strictly increasing `seq` values. A receiver does the actions that follow:

- It keeps at most 64 lanes per peer and at most 256 batches per lane. It discards the oldest.
- It discards a lane after 10 min without a new batch.
- On `BatchAnnounce`, it requests with `TxRequest` the WtxIds that it does not hold. It marks
  the batch complete when it holds all the transactions of the batch.
- It answers `BatchRequest` with the related `BatchAnnounce` if it holds the batch.

A publisher SHOULD announce a batch only for transactions that it already announced. While
the batch is in its retention window, the publisher MUST serve `TxRequest` for these
transactions.

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
(`CompactBlockV2`) has the same fields. After them, it has the full-id section: `full_count`
and `full_ids`. In the rest of this ZIP, "compact block" means a message of type 6 or type 12.
A type 6 message has no full ids.

A sender MUST use type 12 if and only if the block has at least 1 full id for the peer. A
sender MUST use type 6 for every other block, also on a version 2 connection. A receiver MUST
treat a type 12 frame with `full_count = 0` as malformed. Thus each compact block has exactly
1 encoding. A sender MUST NOT send type 12 on a version 1 connection. A receiver on a version
1 connection MUST treat a type 12 frame as malformed.

A receiver gets the transactions of the block, in order, as follows:

1. The prefilled transactions and the full ids occupy their stated indexes.
2. The WtxIds of each referenced batch, in order, fill the remaining positions in increasing
   index order. Thus a batch reference covers a contiguous run of positions that are not
   prefilled and not full ids.
3. The short ids, in order, fill the positions that remain after that.

The total count of transactions is
`prefilled_count + full_count + sum(batch sizes) + short_count`. The indexes MUST obey the
rules that follow:

- The prefilled indexes MUST be strictly increasing.
- The full-id indexes MUST be strictly increasing.
- An index MUST NOT appear in both lists.
- Every index MUST be below the count of transactions.

A sender MUST prefill the coinbase transaction. A sender MUST NOT reference a batch that it
did not announce to the receiver. A sender SHOULD prefill a transaction that it got only
inside this block (a transaction outside its mempool), because the receiver cannot hold it.

On a version 2 connection, a sender SHOULD send these transactions as full ids:

- Each transaction that it announced to the receiver less than 3 s ago and that the receiver
  did not announce back.
- Each transaction that it never announced to the receiver.

A sender MUST send as a full id each transaction whose bytes it does not hold. Every other
transaction SHOULD be a short id or part of a batch reference.

On a version 1 connection, a sender SHOULD send each transaction in its mempool as a short id
or a batch reference. The sender SHOULD prefill the other transactions.

## Canonical order

The depth of a transaction in a set is 0 when it spends no output of another transaction of
the set. Otherwise, its depth is 1 plus the largest depth of those parents. The canonical
order of a set sorts it by depth, then by txid. The comparison of 2 txids treats each txid as
a 32-byte string in internal byte order.

A parent has a smaller depth than its children. Thus the canonical order is a valid order for
the transactions of a block after its coinbase. The same set always gives the same block
bytes. Consensus does not require the canonical order. The candidate form of a block
requires it.

## Candidates

A node that publishes a lane and sets bit 2 SHOULD publish each change of its block template
on a parent as the messages that follow:

- A `BatchAnnounce` of the transactions that the change added, with `seq` equal to the
  template revision. The node sends no `BatchAnnounce` when the change only removed
  transactions.
- A `CandidateAnnounce` with the same `seq`.

`CandidateAnnounce` has this layout:

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
without the positions in `removed`. The positions count from 0 over the concatenation.
`removed` MUST be strictly increasing. Every position MUST be below the length of the
concatenation. A candidate MUST NOT name more than 64 batches.

A publisher SHOULD keep each WtxId at most once in `L`. The publisher SHOULD start a new
candidate from 1 batch on a new parent, or when the candidate gets to the batch limit.

A sender MAY send a block as a `CandidateBlock` when the transactions of the block after the
coinbase are in canonical order. `CandidateBlock` has this layout:

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

The block is the coinbase, followed by the canonical order of a set. The set is `L` without
the positions in `removed`, plus the transactions that the short ids and the full ids name.
A sender MUST set `flags` to 1. A sender MUST NOT send `CandidateAnnounce` or `CandidateBlock`
to a peer on which bits 1 and 2 are not both in use. A sender SHOULD use the candidate form
only when the difference (removed plus added transactions) is smaller than the number of
transactions after the coinbase. The sender chooses the form of each addition as for
`CompactBlock`.

A receiver does the actions that follow:

- It MUST treat a `CandidateAnnounce` or a `CandidateBlock` from a peer on which bits 1 and 2
  are not both in use as a protocol violation.
- It keeps at most 16 candidates per lane and at most 64 lanes. It discards a lane after
  10 min without an announcement. It keeps the first announcement of a `(lane_id, seq)`.
- It requests with `BatchRequest` the batches of a candidate that it does not hold. After it
  holds every batch and every transaction of a new candidate, it SHOULD send the candidate
  once to every peer with the feature, other than the sender. It MUST NOT send the candidate
  before that.
- It MUST treat a `CandidateBlock` whose `flags` are not 1 as malformed.
- After the header check, it rebuilds the set of a `CandidateBlock`. In the cases that
  follow, the receiver requests the full block and penalizes no peer:
  - The receiver does not hold the candidate.
  - The candidate extends another parent.
  - The candidate names a batch that the receiver does not hold.
  - The `CandidateBlock` has a removed position out of range.
  - The `CandidateBlock` has a short id that does not resolve.
  - The `CandidateBlock` names a transaction twice.
- It requests with `TxRequest` the bytes that it does not hold. When it holds every
  transaction, it sorts the set in canonical order and continues at step 6 of the next
  section.

## Reception of a CompactBlock

1. Parse the header. Run the header check. A block that is already in the chain of the
   receiver fails the check. If the check fails, discard the message. Then penalize the
   peer.
2. If a peer already announced the block hash, record the sender as an additional source for
   steps 5, 8 and 9. Then stop.
3. Expand the batch references. If a batch id is unknown, send `BatchRequest` for it. When
   the batch arrives, try the resolution again. If the batch does not arrive in 2 s, go to
   step 9.
4. Resolve the short ids against the local transaction set. For each block, compute once the
   short id of every transaction that the node holds. If 2 held transactions have the same
   short id, treat both as unresolved. Full ids and batch entries are known ids. The local
   set supplies their bytes when it holds them. Each position is now held (id and bytes),
   known (id only) or unknown.
5. Request the unknown positions with `BlockTxnRequest` from the peer that sent the block.
   The peer answers with `BlockTxn`. If the peer does not answer in 2 s, request the
   positions from another peer that announced the block.
6. When no position is unknown, check the id list against the header. The node MUST compute
   the merkle root of the txids. The node MUST compare it with the merkle root of the header.
   If the node knows the ZIP 221 history root of the parent block, it MUST also compute the
   ZIP 244 authorizing data root from the auth digests of the ids. It MUST then check
   `hashBlockCommitments = BLAKE2b-256^"ZcashBlockCommit"(history_root || auth_data_root
   || [0u8; 32])` against the header.

   A node that does not know the history root continues with the merkle root only. A
   mismatch comes from a short-id collision or from a stale local entry. It is not a fault of
   the sender. On a mismatch, forward nothing. Go to step 9. Penalize no peer.
7. On a match, forward the block to every peer on which version 2 is active. Send it as a new
   `CompactBlock` that this node makes, with these contents:
   - Short ids under a nonce that this node chooses, never the received nonce or short ids.
   - The batch references and the full ids, without change.
   - A full id for each known position whose bytes this node does not hold.
8. Request the bytes of the known positions in parallel with `TxRequest` from every peer that
   announced the block. A peer that holds the bytes answers with `Tx`. A peer that itself
   completes the block at that time MUST answer when its bytes arrive. If no answer arrives
   in 2 s, go to step 9.
9. If the resolution cannot complete, request the full block from a peer that announced it.
   Use `getdata` (legacy) or `Block`.
10. When the node holds every position, assemble the body. Check the merkle root of the body
    against the header again. Forward the block as in step 7 to every peer on which
    version 1 is active. Send `inv` for the block to legacy peers.
11. Validate the block as usual. If the body is invalid, do not forward the block again.
    Penalize the peer that sent the block.

A node SHOULD serve `getdata` for the block to legacy peers from the assembled bytes when
step 10 succeeds, before step 11 completes. A node MUST discard a block when its resolution
does not complete in a bounded time. The suggested bound is 20 s. A later announcement of the
same block starts the procedure again.

A node can receive `BatchAnnounce` for a batch that it did not know. After it holds all the
transactions of that batch, it SHOULD send the `BatchAnnounce` once to every peer with the
lanes feature, other than the sender. It MUST NOT send it before that.

## Transmission of a new block

A node that finds a block immediately sends `CompactBlock` to each extension peer. The node
makes the message as follows:

- It uses batch references from lanes that it knows the peer holds.
- On version 2 connections, it uses full ids as the section CompactBlock and CompactBlockV2
  specifies.
- It uses short ids for the other transactions.
- It prefills the coinbase and each transaction outside its mempool.

At the same time, the node sends `inv` to legacy peers. A node that receives a block forwards
it as the section Reception of a CompactBlock specifies.

## Interaction with legacy relay

On a connection where the extension is not active, no part of this ZIP applies. On a
connection where the extension is active and the candidates feature is not in use, a node
sends a block that it received in the candidate form as a `CompactBlock`. On a connection
where the extension is active, legacy `inv`/`getdata`/`block` messages stay valid. A node MUST
handle them. A node MAY use them at any time. A node MUST use them as the fallback in step 6.

# Rationale

## The header check as the condition to forward

A header that passes the check needs a valid proof of work at the current target. This costs
a block reward. For the same reason, the high-bandwidth mode of BIP 152 forwards a block
before body validation. A node can forward an announcement whose body later fails
validation. That node then forwarded at most 1,487 bytes plus references per peer.

## The id list as the condition to forward

Assume that a node forwards a block only after it reconstructs the body. Then each hop that
does not hold 1 transaction of the block adds 1 `BlockTxnRequest` round trip. Thus a block
with a transaction that a peer announced 1 s earlier costs 1 round trip per hop. A WtxId is
the txid followed by the authorizing data commitment. Thus a complete id list sets both roots
that the header commits to.

A node that checked these roots knows the content of the block, except the bytes that it did
not receive. The node can reference that content for the next hop. In the reference
implementation, the id check and the rebuild cost less than 3 ms per hop, compared with 1
network round trip.

This design rejects announcements in 2 phases. They help only while a node waits for data,
and full ids remove that wait. This design also rejects short-id keys for each link. These
keys still need local resolution and a change of key, and they add state to the handshake.
They do not remove the round trip.

## Full ids with indexes

A new transaction can be at any position in a block that is ordered by fee. An entry with an
index does not change the batch positions and the short-id positions around it. Without an
index, every position after the first new transaction would need the long form. The cost is
58 bytes per new transaction, compared with a short id.

## Batch references in addition to short ids

Short ids work only when the receiver already holds the transactions. A node that just
started, or a node behind a slow link, does not hold them. A batch announcement names exactly
the transactions that the block will need. It gives the receiver time to get them before the
block exists. In the usual case, it removes the `BlockTxnRequest` round trip.

## No availability certificates

The order by proof of work does not wait for a quorum. A publisher who announces a batch and
then withholds it delays only its own block.

## Short ids of 6 bytes

With 50,000 held transactions, the probability of a collision in a block is about 5 × 10⁻⁶.
A collision costs 1 request round trip, never an incorrect block. The reason is that the node
checks the merkle root before any use.

## Extension messages inside the legacy framing

With 1 framing per connection, proxies, firewalls and the current code for peer management
need no change. The fallback in step 6 then uses the same connection.

## Canonical order and batch references

A batch reference covers a run of consecutive positions. In a block that is ordered by fee, a
new transaction with a high fee goes into the middle and breaks every batch after it. A block
in canonical order is a function of its transaction set. Thus a candidate names the set, and
the set gives the order. The selection by fee does not change.

## One layout for each type

An earlier draft put the full-id section at the end of type 6, and only its presence marked
it. A body cut before the section was then a valid body without full ids. Thus a hop could
remove the section and keep a valid message. Only the frame length and the checksum of the
outer message detected a cut. With 1 type for each layout and no optional field, no proper
prefix of a frame decodes. Each message then has 1 encoding.

The type follows the content of the block and not the negotiated version. Thus a codec needs
no state of the connection.

## Candidates as sets

Each `CandidateAnnounce` names every batch of its candidate. Thus a receiver that did not get
1 announcement still resolves the next. Additions, removals and changes of fee change only
the difference. A block equal to its candidate costs 61 bytes in the frame, in addition to
the header and the coinbase. With short ids, the cost is 6 bytes per transaction.

## A feature bit for candidates

Only lane publishers make candidates. Each extension version can carry them. A peer that
leaves the bit clear sees no change. A new version would make every later version carry
candidates.

## Version negotiation by range

With `min`/`max`, a node can stop the support of an old version without a flag day. 2 nodes
with disjoint ranges then use legacy relay and do not disconnect.

# Security and Privacy Considerations

- Proof of work limits how often a node forwards a block that it did not validate. The
  penalty per peer for invalid bodies prevents the repeated use of 1 solution.
- The key of the short ids is different for each block and uses a nonce that the sender
  chooses. Thus an attacker cannot precompute transactions with colliding short ids for a
  future block. A node never forwards short ids. With a forwarded nonce, 1 sender could
  search for collisions against every mempool on the network. Also, a node cannot compute
  again the short ids of transactions that it did not resolve.
- A node can safely forward batch ids and full ids without change. A batch id is the hash of
  its WtxIds and a full id is a WtxId. Thus a hop cannot change what they name. The node
  checks the id list from these ids against the header before it forwards the block. The
  node checks the merkle root of the body again before any use. Thus a forwarded reference
  never becomes a wrong block.
- A node can forward a block that it does not hold yet. The node then forwarded 1 header with
  references per peer, as in header-first relay, and proof of work limits this. The node then
  answers the `TxRequest`s of its peers when its own `TxRequest`s get answers. It sends the
  body to legacy and version 1 peers when the body is complete.
- A node discards a block whose bytes do not arrive in the bounded time. The next
  announcement starts the procedure again. A miner who withholds the bytes of a valid header
  delays only its own block.
- A transaction with the same txid and different authorizing data passes the merkle check.
  It fails the `hashBlockCommitments` check when the node knows the history root of the
  parent. When the node does not know it, the hops in between forwarded references to a block
  that fails validation. This is the same exposure as with header-first relay. The body check
  at step 10 and the validation at step 11 still reject the block.
- The retention of lanes and batches has a bound per peer: 64 lanes, 256 batches per lane,
  and expiry after 10 min. This bound limits the memory that an attacker can use.
- The same admission policy applies to transactions from batches and to transactions from
  gossip. A lane cannot bypass the mempool policy.
- The batches of a candidate address its content. A batch id is the hash of its WtxIds, and
  the removed positions are indexes into these WtxIds. Thus a candidate block resolves to an
  id list, and the node checks this list against the roots of the header before it forwards
  the block. A publisher can announce 2 candidates under 1 `(lane_id, seq)`. Then some
  receivers rebuild another set, the merkle check fails, and they request the full block. A
  malicious lane can only waste bandwidth in the limits above (64 lanes, 16 candidates per
  lane, 64 batches per candidate).
- A batch publisher reveals its candidate block before the block exists. This is a choice of
  the publisher. The information is the same as the information that a pool reveals to its
  own hashers through `getblocktemplate` or Stratum.
- Block reconstruction from the local transaction set does not change what a node reveals
  about its mempool beyond what `inv` already reveals.

# Reference Implementation

The `hayai-relay` and `hayai-net` crates of the hayai repository implement the items that
follow:

- The codec, short ids, full ids, lanes, candidates and the canonical order.
- The resolution and the id check.
- The rule to forward a block on the id list.
- The negotiation of versions 1 and 2 and of the feature bits.
- The rules of coexistence with legacy relay in this ZIP.

The crates have test vectors for every message and loopback tests of the multi-hop
behaviour.

# Version History

- Version 1: short ids, batch references and prefilled transactions. A node forwards a block
  after it reconstructs the body.
- Version 2: `CompactBlockV2` (type 12) with the full-id section. A node forwards a block when
  the id list matches the header. A node sends `TxRequest` for the missing bytes to every
  peer that announced the block, and these peers can answer later. A node floods batch
  announcements when they are complete.
- Feature bit 2: the canonical order, `CandidateAnnounce` and `CandidateBlock`.
- An earlier draft of version 2 carried the full-id section as an optional tail of type 6.
  That layout is not compatible with this ZIP.

# References

[^BCP14]: [Information on BCP 14 — "RFC 2119: Key words for use in RFCs to Indicate Requirement Levels" and "RFC 8174: Ambiguity of Uppercase vs Lowercase in RFC 2119 Key Words"](https://www.rfc-editor.org/info/bcp14)

[^bip-0152]: [BIP 152: Compact Block Relay](https://github.com/bitcoin/bips/blob/master/bip-0152.mediawiki)

[^zip-0239]: [ZIP 239: Relay of Version 5 Transactions](https://zips.z.cash/zip-0239)

[^zip-0218]: [ZIP 218: Reduce the block target spacing and bound per-block shielded actions](https://zips.z.cash/zip-0218)

[^narwhal]: [Danezis, Kokoris-Kogias, Sonnino, Spiegelman. Narwhal and Tusk: A DAG-based Mempool and Efficient BFT Consensus. EuroSys 2022](https://arxiv.org/abs/2105.11827)

[^autobahn]: [Giridharan, Suri-Payer, Abraham, Alvisi, Crooks. Autobahn: Seamless high speed BFT. SOSP 2024](https://arxiv.org/abs/2401.10369)
