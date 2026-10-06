# hayai compact relay protocol

- Status: draft 4 (protocol version 2, candidates feature)
- Scope: block and transaction dissemination between hayai nodes and, where noted, between
  hayai nodes and legacy nodes. Consensus rules do not change.

## Terminology

- WtxId: the 64-byte identifier `txid || auth_digest` (ZIP 239). For v4 transactions, the
  auth digest is `0xff…ff`.
- Short id: a 6-byte identifier of a WtxId under a per-block key, as in BIP 152.
- Full id: a WtxId that a `CompactBlockV2` carries with its block index (version 2).
- Batch: a non-empty ordered list of WtxIds that a lane owner publishes. Its identifier is
  `BatchId = BLAKE2b-256("hayai:batch" || wtxid_0 || … || wtxid_n)`. This is a BLAKE2b hash
  with a 32-byte output and no personalization. Its input is the 11 ASCII bytes `hayai:batch`,
  followed by the 64-byte WtxIds in batch order, with no count. Every WtxId is 64 bytes, so
  the input is prefix-free.
- Lane: the append-only sequence of batches of a lane owner. Lane owners are miners or pools.
- Candidate: the transaction set of one revision of the block template of a lane owner on one
  parent. A candidate is a list of batches of the lane, plus the positions of their ids that
  left the template (section Candidates).
- Canonical order: the order of the transactions of a block after the coinbase that depends
  only on the set (section Canonical order).
- Header check: PoW target, Equihash solution, known parent, timestamp window, version. The
  header check needs no body.

## Abstract

A node announces a block as a header plus references to transactions that the receiver
already holds. 3 reference forms exist:

- Short ids over the prepared store of the receiver, as in the high-bandwidth mode of BIP 152.
- Batch ids over lanes that miners publish before they find a block.
- Full WtxIds for transactions that the receiver possibly does not hold yet.

A receiver resolves the references immediately after its header check passes. When every
position has a WtxId and the id list matches the header, the receiver forwards the block. It
forwards the block before it holds every transaction and before it validates the body.

Batch ids and full ids are content-addressed, so a node forwards them without change. Each
hop computes the short ids again with a new key. Between hayai peers, the full body never
crosses a hop, unless the resolution fails. With feature bit 2, a lane owner publishes every
revision of its template as a candidate. A block in canonical order that is close to a
candidate then travels as a reference to that candidate plus the difference.

## Motivation

- Zakura and Zebra forward a block by `inv` → `getdata` → full body. They do this only after
  they fully validate and commit the block, and only to a third of their peers. Each hop
  costs 1.5 RTT, the full body and a validation. Over 3 to 4 hops, a full block needs some
  seconds.
- Each shielded transaction is 2.5–9 kB. A 6-byte reference decreases a mostly-shielded 2 MB
  block to about 10–20 kB. A batch reference decreases it to a few hundred bytes.
- At a block spacing of 25 s (NU7), a relay delay of 1 s costs about 4 % of blocks.
- DAG-mempool systems (Narwhal, Bullshark, Autobahn, Quorum Store) get their throughput as
  follows: they disseminate transaction batches continuously and order only digests. A
  proof-of-work chain has no certificates to wait for, so the same separation is simpler.
  The header is the decision on the order, and lanes are the dissemination layer.

## Messages

Every message is a length-prefixed frame: a little-endian `u32` length, followed by the
payload. The payload starts with a 1-byte message type, followed by the fields below.
Integers are little-endian. Variable-length counts use Bitcoin `CompactSize` in canonical
form. A receiver rejects a payload longer than 8 MiB.

The encoding of differential indexes is as in BIP 152. The first index is absolute. Each next
index is the gap to its predecessor minus 1. Each index is a `CompactSize`.

| Type | Code | Payload |
|---|---|---|
| `TxAnnounce` | 1 | `count`, `WtxId[count]` |
| `TxRequest` | 2 | `count`, `WtxId[count]` |
| `Tx` | 3 | `count`, (`len u32`, wire bytes)[count] |
| `BatchAnnounce` | 4 | `lane_id [32]`, `seq u64`, `BatchId [32]`, `count`, `WtxId[count]` |
| `BatchRequest` | 5 | `count`, `BatchId[count]`; answered by one `BatchAnnounce` per known id |
| `CompactBlock` | 6 | see below |
| `BlockTxnRequest` | 7 | `block_hash [32]`, `count`, differential `index`[count] |
| `BlockTxn` | 8 | `block_hash [32]`, `count`, (`len u32`, wire bytes)[count], in request order |
| `Block` | 9 | `len u32`, wire bytes (fallback and legacy bridge) |
| `CandidateAnnounce` | 10 | see section Candidates (feature bit 2) |
| `CandidateBlock` | 11 | see section Candidates (feature bit 2) |
| `CompactBlockV2` | 12 | see below (version 2) |

The type code names the layout of the payload. Every field of a layout is always present. An
empty list is a zero count. A decoder never decides from the remaining length of a payload
whether a section is present.

### CompactBlock and CompactBlockV2

```
header            [var]   serialized block header of the network, including nonce and solution
nonce             u64     short-id key nonce
batch_count       CompactSize
batch_refs        BatchId[batch_count]           ordered; expands to its WtxIds in place
short_count       CompactSize
short_ids         6 bytes × short_count           transactions not covered by a batch
prefilled_count   CompactSize
prefilled         (differential index, len u32, wire bytes)[prefilled_count]
full_count        CompactSize                     type 12 only, never 0
full_ids          (differential index, WtxId)[full_count]   type 12 only
```

Type 6 (`CompactBlock`) ends after `prefilled`. Type 12 (`CompactBlockV2`) has the same
fields, then `full_count` and `full_ids`.

`header` is the serialized header of the network. It contains 140 bytes of fixed fields, the
CompactSize length of the Equihash solution, and the solution. Thus the header carries its
own length. The length is 1487 bytes on Mainnet and Testnet (Equihash (200, 9), 1344-byte
solution) and 177 bytes on Regtest (Equihash (48, 5), 36-byte solution). A receiver rejects a
frame whose solution length matches no known parameter set. The header check rejects a
solution length of another network.

- The full-id section is the version 2 extension. A sender uses type 12 if and only if the
  block has 1 full id or more for that peer. It uses type 6 for every other block, also on a
  version 2 connection. A type 12 payload with `full_count = 0` is malformed. A type 6
  payload with bytes after `prefilled` is also malformed. Thus each compact block has 1
  encoding, and a decoder reads both types without per-peer state.
- A sender never has full ids on a version 1 connection. Thus a version 1 peer receives type
  6 only, byte for byte the version 1 frame. A receiver that negotiated version 1
  disconnects a peer that sends type 12.
- Transaction order. The prefilled transactions and the full ids occupy their stated
  indexes. The WtxIds of each referenced batch, in order, then fill the remaining positions
  in increasing index order. The short ids, in order, fill the positions after them. The
  transaction count of the block is `prefilled_count + full_count + Σ batch sizes + short_count`.
  A message that breaks one of the rules that follow is malformed:
  - The prefilled indexes are strictly increasing.
  - The full-id indexes are strictly increasing.
  - No index appears in both lists.
  - Every index is below the transaction count.
- Thus a batch reference covers a run of consecutive positions that are not prefilled and
  not full ids. Every position that a batch covers comes before every short-id position.
  From the first such position, the sender references the longest known batch whose ids
  equal the next such transactions. The sender repeats this step. It sends the other
  transactions as short ids.
- The sender chooses the form of each transaction for each peer:
  - The sender always prefills the coinbase.
  - The sender prefills a transaction outside its prepared store (a transaction that it got
    only inside this block). The peer cannot hold it.
  - On a version 2 connection, a transaction is "fresh" for a peer in the cases that follow:
    - The sender announced it to that peer less than `fresh_window` (default 3 s) ago, and
      the peer did not announce it back.
    - The sender never announced it to that peer.

    A fresh transaction is a full id. A transaction whose bytes the sender does not hold yet
    is always a full id. Every other transaction is a short id or part of a batch reference.
  - On a version 1 connection, a transaction in the prepared store of the sender is a short
    id or part of a batch reference. The sender prefills a transaction outside the prepared
    store.
- Short id key: `k0, k1 = SHA-256(header || nonce)[0..16]` as 2 little-endian `u64`;
  `short_id = SipHash-2-4(k0, k1, WtxId)[0..6]`.

### Canonical order

Consensus requires only that a transaction comes after every transaction of the block whose
outputs it spends. The canonical order sets every other choice:

- The depth of a transaction is 0 when it spends no output of another transaction of the
  set. Otherwise, the depth is 1 plus the largest depth of those parents.
- The node sorts the transactions after the coinbase by depth, then by txid. The comparison
  of txids treats each txid as a 32-byte string in internal byte order (the order of the
  serialized transaction id).

A parent has a smaller depth than its children, so the order is a valid block order. The same
set always gives the same block bytes. A hayai template orders its block this way. It still
selects its transactions by ZIP 317 weight ratio. A receiver computes the order from the
transparent inputs of the transactions that it holds.

### Candidates

Candidates use feature bit 2. A lane owner publishes each change of its block template as
follows:

1. A `BatchAnnounce` with the transactions that the template added, with `seq` equal to the
   template revision. A change that only removed transactions has no batch.
2. A `CandidateAnnounce` with the same `seq`, with this layout:

```
lane_id           [32]
seq               u64     template revision; strictly increasing within the lane
parent            [32]    hash of the block the template extends
batch_count       CompactSize
batches           BatchId[batch_count]       batches of this lane, in lane order
removed_count     CompactSize
removed           differential index[removed_count]
```

The id list `L` of the candidate is the concatenation of the ids of `batches` in order,
without the positions in `removed`. The positions count from 0 over the concatenation. The
positions in `removed` are strictly increasing.

A lane owner starts a new list (1 batch of the whole template) on a new parent. It also
starts a new list when a candidate would name more than 64 batches. An id that leaves the
template and comes back gets its old position again. Thus `L` holds each id once.

A sender sends a block as a `CandidateBlock` when the conditions that follow are true:

- The transactions of the block after the coinbase are in canonical order.
- The set of the block is close to a candidate on its parent.

`CandidateBlock` has this layout:

```
header            [var]   serialized block header, as in CompactBlock
nonce             u64     short-id key nonce of short_ids
lane_id           [32]
seq               u64     the candidate (lane_id, seq)
flags             u8      bit 0: canonical order (must be 1); bits 1–7: zero
coinbase          len u32, wire bytes        position 0
removed_count     CompactSize
removed           differential index[removed_count]   positions in L
short_count       CompactSize
short_ids         6 bytes × short_count       additions the receiver holds
full_count        CompactSize
full_ids          WtxId[full_count]           additions the receiver may not hold
```

The block is the coinbase, followed by the canonical order of a set. The set is `L` without
the positions in `removed`, plus the additions. A block equal to its candidate costs 61 bytes
in the frame, without the header and the coinbase bytes. These 61 bytes are the length
prefix, the type, 8 + 32 + 8 + 1 bytes of fields, the coinbase length and 3 zero counts.

Sender rules:

- A sender uses the candidate form only for a block in canonical order. The difference of
  the block to the candidate (removals plus additions) must have fewer entries than the
  block has transactions after the coinbase. Among its own candidates and the received
  candidates whose batches it holds, the sender takes the candidate on the parent of the
  block with the smallest difference.
- An addition uses the forms of `CompactBlock`. It is a short id when the peer holds it. It
  is a full id when the peer possibly does not hold it, or when the sender does not hold the
  bytes. If the sender would prefill an addition, the sender uses `CompactBlock` instead.
- A sender sends `CandidateAnnounce` and `CandidateBlock` only to peers that negotiated
  feature bits 1 and 2.
- Publication is the choice of the lane owner. A lane owner can publish each template
  change, a part of each change, or nothing. A transaction that the lane owner keeps out of
  its lane is not in a batch and has no `TxAnnounce`. The lane owner does not serve it before
  its block. The block then has the transaction as a prefilled transaction in a
  `CompactBlock`. A lane owner that publishes nothing sends each of its blocks as a
  `CompactBlock`, or in the candidate form of a candidate of another lane.
- The choice changes no message, no feature bit and no version. A feature bit states what a
  node can receive and forward. It does not state that the node publishes a lane. A receiver
  never waits for a batch or a candidate before it handles a block. A node that publishes
  nothing still sets bits 1 and 2 to receive the lanes of other lane owners.

Receiver rules:

- A receiver keeps the last 16 candidates of each lane, for at most 64 lanes. It discards a
  lane after 10 minutes without an announcement. In a lane, `seq` is strictly increasing.
  The receiver rejects another announcement for a stored `(lane_id, seq)` and keeps the
  first one.
- On `CandidateAnnounce`, it requests with `BatchRequest` the batches that it does not hold.
  After it holds every batch and every transaction of these batches, it floods the
  announcement once to every peer with the feature, except the sender.
- On `CandidateBlock`, it runs the header check and then rebuilds the set. In the cases that
  follow, the receiver requests the full block from the sender and penalizes no peer:
  - The candidate is not stored.
  - The candidate extends another parent.
  - The receiver does not hold a batch.
  - A removed position is out of range.
  - A short id does not resolve.
  - An id appears twice.

  The reason is that the positions of a candidate block are known only when its set is
  known. Thus no `BlockTxnRequest` can name them. A frame whose flags are not exactly 1 is
  malformed.
- When the set is complete but some bytes are missing, the receiver requests them with
  `TxRequest` from every announcer. When it holds every transaction, it sorts the set in
  canonical order. It then continues at step 4 of Block announcement. The order needs the
  inputs of every transaction. Thus the receiver forwards a candidate block when it holds
  its bytes.
- A receiver that holds the candidate can prebuild its layer against the parent. A block
  equal to it then commits after the header and coinbase rules only
  (`docs/architecture.md`, Prebuilt bodies).

## Procedures

### Transaction dissemination

- A node announces with `TxAnnounce` every transaction that it accepts into its prepared
  store. It groups the ids and sends a group after 100 ms or at 64 ids, at the first limit
  that it gets to. A peer requests unknown ids with `TxRequest`. A node prepares a
  transaction once on receipt and serves it from wire bytes. A miner can keep a transaction
  of a local client out of this rule until its block (section Candidates, sender rules).
- Lane owners can also publish their template as a lane. Each template change is a
  `BatchAnnounce` of the added transactions and, with feature bit 2, a `CandidateAnnounce`
  (section Candidates). A batch contains only transactions that the lane owner already
  announced. A receiver that does not hold some of them requests them with `TxRequest`. It
  marks the batch complete when it has prepared all of them.
- A receiver keeps the last 256 batches per lane. It discards a lane after 10 minutes without
  an announcement. In a lane, `seq` is strictly increasing. The receiver rejects an
  announcement that does not increase `seq`, or whose `BatchId` is not the hash of its ids.
  It ignores a known `BatchId` that a peer announces again.
- After a node holds all the transactions of a batch, it floods the accepted `BatchAnnounce`
  once. It sends it to every peer with the lanes feature, except the peer that sent it.
  Before that, it forwards nothing for the batch. The lane limits above apply before the
  flood. Thus a peer cannot make a node flood more batches than it stores.

### Block announcement

1. The finder sends `CompactBlock` to every peer that negotiated this protocol. It sends it
   before it validates its own block beyond the header check.
2. A receiver runs the header check. A block that is already in its chain fails the check.
   On success, the receiver resolves every position:
   - It expands each `batch_ref` from its lane store. An unknown batch id leaves the
     transaction count and all later indexes undefined. Thus the resolution fails as a
     whole. The receiver requests the batch with `BatchRequest` and tries again when a peer
     announces the batch. As the fallback, it uses `Block`.
   - It matches each short id against its prepared store. The receiver computes the short
     ids of every stored WtxId once per block. A collision between 2 stored transactions
     makes both ambiguous and leaves the position unknown.
   - It takes full ids and batch entries as known ids, with the bytes from the prepared
     store when the store holds them.
   - It parses the prefilled transactions.

   A position is then held (id and bytes), known (id only) or unknown (an unresolved short
   id).
3. The receiver requests the unknown positions with `BlockTxnRequest` from the peer that sent
   the block. That peer answers with `BlockTxn`. If a peer cannot answer in 2 s, the
   receiver asks another peer that announced the block. With no other announcer, the
   receiver requests the full block with `getdata MSG_BLOCK` on the same connection. The
   receiver discards a block that is still incomplete after 20 s. The next announcement of
   the block starts the procedure again.
4. When no position is unknown, the receiver checks the id list against the header:
   - It always checks the merkle root of the txids.
   - When it knows the ZIP 221 history root of the parent, it also checks
     `hashBlockCommitments = BLAKE2b-256^"ZcashBlockCommit"
     (history_root || auth_data_root || 0^32)`. It computes the auth data root from the
     auth digests of the ids.

   A node that does not know the history root forwards on the merkle root only. It counts
   the block as "forwarded without auth root". A mismatch is a short-id collision or a stale
   entry of the prepared store (BIP 152), and not a fault of the sender. On a mismatch, the
   receiver forwards nothing, requests the full block from the sender and penalizes no peer.
5. On a match, the receiver immediately forwards the block to every version 2 peer, as a
   `CompactBlock` that it makes itself:
   - Short ids under its own nonce.
   - The batch references and the full ids, without change.
   - A full id for every position whose bytes it does not hold.

   Then the receiver requests the bytes of the known positions in parallel with `TxRequest`
   from every peer that announced the block. A peer that holds the bytes answers with `Tx`.
   A peer that itself still completes the block answers when its bytes arrive. Validation
   waits for the bytes. The receiver does not wait for the bytes to forward the block.
6. When the receiver holds every position, it assembles the body. It checks the merkle root
   of the body again. Then it does the actions that follow:
   - It forwards the block to version 1 peers (with a new key, after the body, as in
     version 1).
   - It announces the block to legacy peers with `inv`.
   - It gives the block to validation once.
7. The receiver validates the body (`docs/architecture.md`, hayai-validate). A node that
   detects an invalid body does not forward the block again, and it penalizes the sender.
   Nodes that already forwarded the header-valid compact block did no harm beyond what the
   proof of work already paid for.
8. Mining on the new tip starts when the first half of step 7 completes. The first half is
   the layer build: contextual rules, tree appends, history tree append. The node then
   pushes the layer as a speculative tip. It sends the coinbase-only template, then the full
   template (`docs/protocol-template-push.md`, Tip event).

   A template needs the body. Its header commits to the ZIP 221 history tree after the new
   block. The leaf of that tree holds the final note commitment roots of the block. If the
   second half of step 7 (scripts and proofs) fails, the node sends `TemplateRevert`. Mining
   then goes back to the parent.

### Legacy bridge

- A node that also speaks the legacy protocol answers `getdata` for a block from its wire
  bytes when the body is complete, before validation completes. This applies to blocks
  whose header check passed. The node sends `inv` to legacy peers at the same point.
- Blocks from legacy peers enter at step 2 as full bodies.

## Negotiation and legacy coexistence

The compact relay protocol is an extension that peers negotiate inside the legacy Zcash
peer-to-peer protocol. Toward every other node, a node that runs the extension stays a legacy
node. Blocks and transactions always travel over the legacy path also. Thus the protocol that
a miner runs cannot cause a chain split.

### Service bit and user agent

- `version.services` carries `NODE_COMPACT_RELAY = 1 << 26` when the extension is enabled.
  Bit 24 is Zakura's P2P v2, and hayai does not change it. The user agent is
  `/hayai:0.1.0/`.
- A node with the extension disabled sets neither the bit nor the user agent. It never sends
  the commands below. It treats them as unknown commands when it receives them. Thus its
  peers see the same behaviour as from a legacy node.

### `zcmpctver`

After `verack`, a node that saw the service bit in the `version` of the peer sends the
legacy-framed command `zcmpctver` with this payload:

```
max_version   u16 LE   highest extension version offered
min_version   u16 LE   lowest extension version accepted
features      u64 LE   feature bits
```

A receiver ignores trailing bytes, so that later versions can append fields. A node that
receives `zcmpctver` and did not send one answers with its own. Both sides choose
`v = min(max_a, max_b)`. If `v` is at least both minimums, the peer speaks compact relay `v`.
If not, the peer stays legacy. The feature set is the intersection of both bit sets, limited
to the bits that a node knows.

A node ignores unknown bits. A peer that never sends `zcmpctver` stays legacy for the life of
the connection. A node ignores a repeated `zcmpctver`. This document describes versions 1
and 2. A node offers `max_version = 2, min_version = 1`.

| Version | Behaviour |
|---|---|
| 1 | Short ids, batch references and prefilled transactions. A receiver forwards after it reconstructs the body. |
| 2 | Adds `CompactBlockV2` (type 12): a compact block with a full-id section. A receiver forwards once the id list matches the header, before it holds every transaction. |

| Bit | Feature |
|---|---|
| 0 | Short-id compact blocks v1: `CompactBlock`, `BlockTxnRequest`, `BlockTxn`, `TxAnnounce`, `TxRequest`, `Tx`, `Block` |
| 1 | Batch lanes v1: `BatchAnnounce`, `BatchRequest`, batch references in `CompactBlock` |
| 2 | Candidates v1: `CandidateAnnounce`, `CandidateBlock`; in use only with bit 1 |

The candidates are a feature bit and not version 3. Only lane owners publish candidates, and
a relay node can leave them out. Every version can carry them, because a `CandidateBlock`
resolves to a complete id list. A peer that does not set the bit receives neither message
and sees no change. This is also true for a version 1 or a version 2 peer. The versions stay
a total order of the rule to forward.

### `zcmpct`

After the negotiation, every message of the section Messages travels as the payload of the
legacy command `zcmpct`. The payload is 1 frame as the section Messages defines it (length
prefix included), inside 1 legacy frame. Thus the stream keeps 1 framing, 1 checksum
discipline and the legacy size bounds. Proxies and middleboxes see ordinary messages. A node
ignores a `zcmpct` from a peer that did not negotiate, as zcashd ignores unknown commands. A
node disconnects a peer when the payload of its `zcmpct` does not decode, as for any
malformed frame.

### Both paths, always

- The node announces the transactions that it accepts into the prepared store to legacy
  peers with `inv` (`MSG_WTX`, or `MSG_TX` for v4). At the same time, it announces them to
  compact-relay peers with `TxAnnounce`. The node serves `getdata` and `TxRequest` from the
  same wire bytes. A transaction from either path enters the same sink.
- All blocks take the same path, whatever their origin: found locally, a full `block` from a
  legacy peer, or a reconstructed compact block. The node does these steps:
  1. It removes duplicates by hash.
  2. It runs the header check.
  3. When the body is available, it forwards the block as `CompactBlock` to compact-relay
     peers. At the same time, it announces the block as `inv MSG_BLOCK` to legacy peers.
  4. It serves `getdata` from the retained wire bytes.

  The forward comes before full validation (the high-bandwidth mode of BIP 152). The
  validator receives every block exactly once.
- The node forwards a received compact block with a new key:
  - To version 2 peers, when its id list matches the header.
  - To version 1 peers, when its body is complete.

  At that same later point, the node announces the block to legacy peers.
- `BatchAnnounce` goes only to peers that negotiated bit 1. The node does not forward a
  compact block with batch references to a peer without bit 1.
- `CandidateAnnounce` and `CandidateBlock` go only to peers that negotiated bits 1 and 2. A
  node that receives a candidate block rebuilds it. It forwards the block as a
  `CompactBlock` to peers without bit 2, and announces it with `inv` to legacy peers. A node
  disconnects a peer that sends either message without the bits.
- The node answers `getheaders` with up to 160 full headers, each followed by a zero
  transaction count, as zcashd sends them. Thus legacy nodes synchronise past a hayai node.

### Forward compatibility

Later extension versions add new message types with a higher `max_version`. Nodes that do
not get to that version never see them. New optional behaviour comes as feature bits. Legacy
nodes see neither.

## Rationale

- The header check as the only condition to forward: to make nodes forward invalid data, an
  attacker needs a valid Equihash solution at the current target. This costs a block reward.
  The high-bandwidth mode for compact blocks of Bitcoin Core makes the same trade-off.
- Batch references in addition to short ids: short ids require that the prepared store of
  the receiver contains the transactions. This is true for well-connected nodes. It is not
  true for a node that just started or for a node behind a slow link. A batch announcement
  names exactly the transactions that the block will need. It gives the receiver time to get
  them before the block exists. Lanes also remove the `BlockTxnRequest` round trip in the
  usual case.
- No availability certificates: the order by proof of work does not wait for a quorum. A
  lane owner that announces batches and then withholds them harms only the propagation of
  its own block.
- Short ids of 6 bytes: with a prepared store of 50,000 transactions, the probability of a
  collision in a block is about 5 × 10⁻⁶. A collision costs 1 request round trip, not a
  failure.
- The id list rather than the body as the condition to forward: assume that a node forwards
  only after reconstruction. Then every hop that does not hold 1 transaction adds 1
  `BlockTxnRequest` round trip. Thus a block with a fresh transaction costs `h` round trips
  over `h` hops. A WtxId is `txid || auth_digest`, so a complete id list sets both roots that
  the header commits to. A node that checked these roots knows the body of the block, except
  the bytes that it did not receive.

  Batch ids and full ids are hashes of that content, and they travel without change. Only
  short ids depend on the nonce of a sender, and each hop computes them again. In
  `relay/forward_latency`, the measured cost of the id check and the rebuild is 0.4–2.5 ms
  per hop, compared with a round trip of 20 ms.
- Full ids rather than announcements in 2 phases or keys for each link: an announcement in 2
  phases helps only while a node waits for data, and full ids remove that wait. Keys for
  each link still need local resolution and a change of key. They add state to the
  handshake and do not remove a round trip.
- Full ids carry their own index: a fresh transaction can be at any position in the block.
  An entry with an index does not change the batch positions and the short-id positions
  around it. The cost is 58 bytes per fresh transaction.
- The type code names the layout: draft 2 put the full-id section at the end of type 6, and
  only its presence marked it. A type 6 payload cut before the section was then a valid
  payload without full ids. Thus a hop could remove the section and keep a valid message.
  Only the frame length and the legacy checksum detected a cut. With 1 type for each layout
  and no optional section, no proper prefix of a payload decodes. Each message then has 1
  encoding.

  The type follows the content and not the negotiated version. Thus the encoder and the
  decoder need no per-peer state.
- Mining on a layer that is built but not validated: an invalid block costs the miners that
  build on it at most the validation time of the block. This time is less than 150 ms for a
  cold block within the NU7 limits. After that time, `TemplateRevert` sends the miners back
  to the parent. Mining on the header alone is not possible, because the template header
  commits to the history tree after the block.
- Extension inside the legacy framing, not a second port or a stream switch: this gives 1
  framing, 1 handshake and 1 size discipline. A node with the extension disabled is
  byte-for-byte a legacy node. The legacy path is always in use, so the legacy path stays
  tested.
- Canonical order rather than weight order: a batch reference covers a run of consecutive
  positions. In a block ordered by weight, a new transaction with a high ratio goes into the
  middle of the order and breaks every batch after it. A block in canonical order is a
  function of its set. Thus a candidate names the set, and the set gives the order. The
  selection stays by weight ratio.
- A candidate rather than a batch per position: a candidate is a set. Thus additions,
  removals and changes of fee change only the difference, never the positions around them.
  The measured cost of a block equal to its candidate is 61 bytes in addition to the header
  and the coinbase. With short ids, the cost is 6 bytes per transaction
  (`relay/bytes_on_wire`).
- Set differences against the whole candidate rather than a log of template deltas: a
  receiver that did not get an announcement still resolves the next one. The reason is that
  each `CandidateAnnounce` names every batch of its candidate.

## Security considerations

- Proof of work limits how often a node forwards a block that it did not validate. The
  penalty per peer for invalid bodies prevents the repeated use of 1 solution.
- A node can detect short-id collisions, and they never make a wrong block. The node checks
  the merkle root of the id list against the header before it forwards anything. It checks
  the merkle root of the assembled body again before it uses the body.
- Content-addressed sections are safe to forward without change. A batch id is the hash of
  its WtxIds and a full id is a WtxId. Thus a hop cannot change what they name. No peer can
  make a receiver resolve them to transactions other than the transactions that the header
  commits to. With a forwarded short-id nonce, 1 sender could search for collisions against
  every prepared store on the network. For this reason, a node never forwards short ids.
- A node can forward a block whose bytes it does not hold yet. It then forwarded 1 header
  with references per peer, and proof of work limits this as in header-first relay. The node
  then answers the `TxRequest`s of its peers when its own requests get answers. It sends the
  block itself to legacy and version 1 peers when the body is complete.
- A node discards a block whose bytes do not arrive in 20 s. The next announcement starts
  the procedure again. An attacker who withholds the bytes of a valid header delays only its
  own block.
- A transaction with the same txid and different authorizing data (a malleated witness)
  passes the merkle check. It fails the commitments check when the node knows the history
  root of the parent. Without that root, the validation of the body detects the mismatch.
  The hops in between then forwarded references to a block that they cannot validate, as
  after a header-first relay.
- Lane and batch announcements have a limit per peer: 256 batches per lane and 64 lanes per
  peer. A node ignores the batches beyond the limit.
- The batches of a candidate address its content. A batch id is the hash of its WtxIds, and
  the removed positions are indexes into those ids. Whatever a lane announces, a candidate
  block resolves to an id list. The receiver checks this list against the header roots
  before it forwards anything. It checks the body against the merkle root before it uses it.
- A lane owner can announce 2 different candidates under 1 `(lane_id, seq)`. Then some
  receivers rebuild another set, their merkle check fails, and they request the full block.
  Thus a malicious lane can only waste bandwidth, within these limits: 64 lanes, 16
  candidates per lane, 64 batches per candidate and 256 batches per lane.
- A node prepares and policy-checks transactions from a batch exactly as transactions from
  gossip. A lane cannot bypass admission.

## Version history

- Draft 1: version 1. Short ids, batch references and prefilled transactions. A receiver
  reconstructs the body, checks its merkle root and forwards it with a new key.
- Draft 2: version 2. It adds the items that follow:
  - The full-id section.
  - The forward once the id list matches the header: the merkle root always, and the auth
    data root through `hashBlockCommitments` when the history root of the parent is known.
  - `TxRequest` to every announcer for the missing bytes.
  - Deferred answers for transactions of a block that a node still completes.
  - A record per peer of fresh transactions.
  - The flood of received batch announcements when they are complete.
- Draft 3: feature bit 2. Canonical block order, template candidates as lanes
  (`CandidateAnnounce`), and the candidate form of a block (`CandidateBlock`).
- Draft 4: type 12 (`CompactBlockV2`) carries the full-id section. Type 6 is the version 1
  layout only. In drafts 2 and 3, the section was an optional tail of type 6. That layout is
  not compatible with draft 4. A version 2 node of draft 2 or 3 and a node of draft 4
  disconnect when one of them sends a block with full ids. Version 1 frames are the same.

## References

- BIP 152, Compact Block Relay.
- Danezis, Kokoris-Kogias, Sonnino, Spiegelman, "Narwhal and Tusk: a DAG-based mempool and
  efficient BFT consensus", EuroSys 2022.
- Spiegelman, Giridharan, Sonnino, Kokoris-Kogias, "Bullshark: DAG BFT protocols made
  practical", CCS 2022.
- Giridharan, Suri-Payer, Abraham, Alvisi, Crooks, "Autobahn: seamless high speed BFT",
  SOSP 2024 (data lanes separated from consensus).
- Aptos Quorum Store (batch dissemination by workers, consensus over batch digests).
- Zakura Dogwood design notes (erasure-coded push of full bodies), for the alternative that
  does not assume receivers hold the transactions.
