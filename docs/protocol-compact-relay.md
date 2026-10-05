# hayai compact relay protocol

- Status: draft 4 (protocol version 2, candidates feature)
- Scope: block and transaction dissemination between hayai nodes and, where noted, between
  hayai nodes and legacy nodes. Consensus rules are unchanged.

## Terminology

- **WtxId**: the 64-byte identifier `txid || auth_digest` (ZIP 239). For v4 transactions the
  auth digest is `0xff…ff`.
- **Short id**: a 6-byte identifier of a WtxId under a per-block key, as in BIP 152.
- **Full id**: a WtxId carried in a `CompactBlockV2` with its block index (version 2).
- **Batch**: a non-empty ordered list of WtxIds published by a lane owner, identified by its
  `BatchId = BLAKE2b-256("hayai:batch" || wtxid_0 || … || wtxid_n)`: a BLAKE2b hash with a
  32-byte output and no personalization, over the 11 ASCII bytes `hayai:batch` followed by
  the 64-byte WtxIds in batch order, with no count (every WtxId is 64 bytes, so the input is
  prefix-free).
- **Lane**: a lane owner's append-only sequence of batches. Lane owners are miners or pools.
- **Candidate**: the transaction set of one revision of a lane owner's block template on one
  parent: a list of batches of the lane and the positions of their ids that left the
  template (section Candidates).
- **Canonical order**: the order of a block's transactions after the coinbase that depends
  on the set only (section Canonical order).
- **Header check**: PoW target, Equihash solution, parent known, timestamp window, version.
  No body is needed.

## Abstract

Blocks are announced as a header plus references to transactions the receiver already holds.
Three reference forms exist: short ids over the receiver's prepared-transaction store (as in
BIP 152 high-bandwidth mode), batch ids over lanes that miners publish ahead of finding a
block, and full WtxIds for transactions the receiver may not hold yet. A receiver resolves
the references as soon as its header check passes. When every position has a WtxId and the
id list matches the header, the receiver forwards the block, before it holds every
transaction and before it validates the body. Batch ids and full ids are content-addressed,
so they travel unchanged; short ids are re-keyed at each hop. The body never crosses a hop in
full between hayai peers unless resolution fails. With feature bit 2, a lane owner publishes
every revision of its template as a candidate, and a block in canonical order that is close
to a candidate travels as a reference to it plus the difference.

## Motivation

- Zakura and Zebra relay a block by `inv` → `getdata` → full body, after the receiving node has
  fully validated and committed it, and only to a third of their peers. Each hop costs 1.5 RTT,
  the full body and a validation. Over three to four hops a full block takes seconds.
- Shielded transactions are 2.5–9 kB each; a 6-byte reference shrinks a mostly-shielded 2 MB
  block to about 10–20 kB, and a batch reference shrinks it to a few hundred bytes.
- At 25 s block spacing (NU7) one second of relay delay costs about 4 % of blocks.
- DAG-mempool systems (Narwhal, Bullshark, Autobahn, Quorum Store) reach their throughput by
  disseminating transaction batches continuously and ordering only digests. A proof-of-work
  chain has no certificates to wait for, which makes the same separation simpler: the header is
  the ordering decision, and lanes are the dissemination layer.

## Messages

All messages are length-prefixed frames of a little-endian `u32` length followed by the payload.
The payload starts with a one-byte message type followed by the fields below. Integers are
little-endian. Variable-length counts use Bitcoin `CompactSize` in canonical form. A payload
longer than 8 MiB is rejected. Differential indexes are encoded as in BIP 152: the first index
is absolute, each following one is the gap to its predecessor minus one, each as a
`CompactSize`.

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

The type code names the layout of the payload. Every field of a layout is always present,
and an empty list is a zero count. A decoder never decides from the remaining length of a
payload whether a section is present.

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

`header` is the serialized header of the network: 140 bytes of fixed fields, the
CompactSize length of the Equihash solution, and the solution. The header therefore carries
its own length: 1487 bytes on Mainnet and Testnet (Equihash (200, 9), 1344-byte solution),
177 bytes on Regtest (Equihash (48, 5), 36-byte solution). A receiver rejects a frame whose
solution length matches no known parameter set; the header check rejects a solution length
of another network.

- The full-id section is the version 2 extension. A sender uses type 12 if and only if the
  block has one full id or more for that peer. It uses type 6 for every other block, on a
  version 2 connection also. A type 12 payload with `full_count = 0` is malformed, and so
  is a type 6 payload with bytes after `prefilled`. Each compact block therefore has one
  encoding, and a decoder reads both types without per-peer state. A sender never has full
  ids on a version 1 connection, so a version 1 peer receives type 6 only, byte for byte
  the version 1 frame. A receiver that negotiated version 1 disconnects a peer that sends
  type 12.
- Transaction order. Prefilled transactions and full ids occupy their stated indexes. The
  remaining positions, in increasing index order, are filled first by the WtxIds of each
  referenced batch in order, then by the short ids in order. The transaction count of the
  block is `prefilled_count + full_count + Σ batch sizes + short_count`. The prefilled
  indexes are strictly increasing, the full-id indexes are strictly increasing, no index
  appears in both lists, and every index is below the transaction count; a message that
  breaks one of these rules is malformed.
- A batch reference therefore covers a run of consecutive positions that are neither
  prefilled nor full ids, and every batch-covered position precedes every short-id position.
  A sender references, from the first such position onwards, the longest known batch whose
  ids equal the next such transactions, repeatedly, and short-ids the rest.
- Form of each transaction, chosen by the sender per peer:
  - The coinbase is always prefilled.
  - A transaction outside the sender's prepared store (one that reached the sender only
    inside this block) is prefilled: the peer cannot have it.
  - On a version 2 connection, a transaction is "fresh" for a peer when the sender announced
    it to that peer less than `fresh_window` (default 3 s) ago and the peer has not
    announced it back, or when the sender never announced it to that peer. A fresh
    transaction is a full id. A transaction whose bytes the sender does not hold yet is
    always a full id. Every other transaction is a short id or part of a batch reference.
  - On a version 1 connection, a transaction in the sender's store is a short id or part of
    a batch reference; one outside the store is prefilled.
- Short id key: `k0, k1 = SHA-256(header || nonce)[0..16]` as two little-endian `u64`;
  `short_id = SipHash-2-4(k0, k1, WtxId)[0..6]`.

### Canonical order

Consensus requires only that a transaction follows every transaction of the block whose
outputs it spends. The canonical order fixes every other choice:

- The depth of a transaction is 0 when it spends no output of another transaction of the
  set, and 1 plus the largest depth of those parents otherwise.
- The transactions after the coinbase are sorted by depth, then by txid. A txid compares as
  a 32-byte string in internal byte order (the order of the serialized transaction id).

A parent has a smaller depth than its children, so the order is a valid block order. The
same set always gives the same block bytes. A hayai template orders its block this way and
still selects its transactions by ZIP 317 weight ratio. A receiver computes the order from
the transparent inputs of the transactions it holds.

### Candidates

Feature bit 2. A lane owner publishes each change of its block template:

1. One `BatchAnnounce` with the transactions that the template added, with `seq` equal to
   the template revision. A change that only removed transactions has no batch.
2. One `CandidateAnnounce` with the same `seq`:

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
without the positions in `removed`. Positions count from 0 over the concatenation and are
strictly increasing. A publisher starts a new list (one batch of the whole template) on a
new parent, and when a candidate would name more than 64 batches. An id that leaves the
template and comes back takes its old position again, so `L` holds each id once.

A block whose transactions after the coinbase are in canonical order, and whose set is
close to a candidate on its parent, travels as a `CandidateBlock`:

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

The block is the coinbase, then the canonical order of `L` without the positions in
`removed`, with the additions. Without the header and the coinbase bytes, a block equal to
its candidate costs 61 bytes in the frame (length prefix, type, 8 + 32 + 8 + 1 bytes of
fields, the coinbase length and three zero counts).

Sender rules:

- A sender uses the candidate form only for a block in canonical order whose difference to
  the candidate (removals plus additions) has fewer entries than the block has
  transactions after the coinbase. It takes the candidate on the block's parent with the
  smallest difference, among its own and the received ones whose batches it holds.
- An addition follows the forms of `CompactBlock`: a short id when the peer holds it, a full
  id when the peer may not, or when the sender lacks the bytes. An addition the sender
  would prefill makes it use `CompactBlock` instead.
- A sender sends `CandidateAnnounce` and `CandidateBlock` only to peers that negotiated
  feature bits 1 and 2.

Receiver rules:

- A receiver keeps the last 16 candidates of each of at most 64 lanes and drops a lane after
  10 minutes of silence. Within a lane `seq` is strictly increasing; another announcement
  for a stored `(lane_id, seq)` is rejected, and the first one stays.
- On `CandidateAnnounce` it requests the batches it lacks with `BatchRequest`. It floods
  the announcement once to every peer with the feature except the sender, after it holds
  every batch and every transaction of them.
- On `CandidateBlock` it runs the header check, then rebuilds the set. When the candidate is
  not stored, it extends another parent, a batch is not held, a removed position is out of
  range, a short id does not resolve, or an id appears twice, the receiver requests the
  full block from the sender and penalizes nobody: the positions of a candidate block are
  known only once its set is, so no `BlockTxnRequest` can name them. A frame whose flags
  are not exactly 1 is malformed.
- When the set is complete but some bytes are missing, the receiver requests them with
  `TxRequest` from every announcer. Once every transaction is held, it sorts the set in
  canonical order and continues at step 4 of Block announcement. The order needs the inputs
  of every transaction, so a candidate block is forwarded once its bytes are held.
- A receiver that holds the candidate can prebuild its layer against the parent. A block
  equal to it then commits after the header and coinbase rules only
  (`docs/architecture.md`, Prebuilt bodies).

## Procedures

### Transaction dissemination

- A node announces every transaction it accepts into its prepared store with `TxAnnounce`
  (batched per 100 ms or 64 ids, whichever first). A peer requests unknown ids with
  `TxRequest`. A node prepares a transaction once on receipt and serves it from wire bytes.
- Lane owners additionally publish their template as a lane: each template change is one
  `BatchAnnounce` of the added transactions and, on feature bit 2, one `CandidateAnnounce`
  (section Candidates). A batch contains only transactions already announced; a receiver
  missing some of them requests them with `TxRequest` and marks the batch complete once all
  are prepared.
- A receiver keeps the last 256 batches per lane and drops a lane after 10 minutes of silence.
  Within a lane `seq` is strictly increasing; an announcement that does not advance it, or
  whose `BatchId` is not the hash of its ids, is rejected. A known `BatchId` announced again
  is ignored.
- A node floods each accepted `BatchAnnounce` once, to every peer with the lanes feature
  except the peer it came from, after it holds all of the batch's transactions. Before that
  it forwards nothing for the batch. The lane limits above apply before flooding, so a peer
  cannot make a node flood more batches than it stores.

### Block announcement

1. The finder sends `CompactBlock` to every peer that negotiated this protocol, before
   validating its own block beyond the header check.
2. A receiver runs the header check (a block already in its chain fails it). On success it
   resolves every position:
   - expand each `batch_ref` from its lane store; an unknown batch id leaves the transaction
     count and all later indexes undefined, so resolution fails as a whole: the receiver
     requests the batch with `BatchRequest` and retries once it is announced, or falls back
     to `Block`;
   - match each short id against its prepared store (short ids are computed for every stored
     WtxId once per block; a collision between two stored transactions makes both ambiguous
     and leaves the position unknown);
   - take full ids and batch entries as known ids, with the bytes from the store when it
     holds them;
   - parse prefilled transactions.
   A position is then held (id and bytes), known (id only) or unknown (an unresolved short
   id).
3. Unknown positions are requested with `BlockTxnRequest` from the peer that sent the block,
   which answers with `BlockTxn`. A peer that cannot answer within 2 s is replaced by any
   other peer that announced the block; with no other announcer the full block is requested
   with `getdata MSG_BLOCK` on the same connection, and a block still incomplete after 20 s
   is dropped (its next announcement starts over).
4. Once no position is unknown, the receiver checks the id list against the header: the
   merkle root of the txids always, and `hashBlockCommitments = BLAKE2b-256^"ZcashBlockCommit"
   (history_root || auth_data_root || 0^32)` when it knows the ZIP 221 history root of the
   parent, with the auth data root computed from the auth digests of the ids. A node that
   does not know the history root forwards on the merkle root alone and counts the block as
   "forwarded without auth root". A mismatch is a short-id collision or a stale store entry
   (BIP 152), not a fault of the sender: nothing is forwarded, the full block is requested
   from the sender, and no peer is penalized.
5. On a match the receiver forwards the block at once to every version 2 peer, as a
   `CompactBlock` it builds itself: short ids under its own nonce, batch references and full
   ids unchanged, and a full id for every position whose bytes it does not hold. Then it
   requests the bytes of the known positions with `TxRequest` from every peer that announced
   the block, in parallel. A peer that holds the bytes answers with `Tx`; a peer that is
   itself still completing the block answers when its bytes arrive. Validation waits for the
   bytes; forwarding does not.
6. Once every position is held, the body is assembled, its merkle root is checked again, and
   the block is forwarded to version 1 peers (re-keyed, after the body, as in version 1),
   announced to legacy peers with `inv`, and handed to validation once.
7. The body is validated (`docs/architecture.md`, hayai-validate). A block whose body fails
   validation is not re-forwarded by nodes that detect it, and the sender is penalized; nodes
   that already forwarded the header-valid compact block have done no harm that proof of work
   did not already pay for.
8. Mining on the new tip starts when the first half of step 7 (the layer build: contextual
   rules, tree appends, history tree append) completes: the node pushes the layer as a
   speculative tip and sends the coinbase-only template, then the full template
   (`docs/protocol-template-push.md`, Tip event). A template needs the body: its header
   commits to the ZIP 221 history tree after the new block, whose leaf holds the block's
   final note commitment roots. If the second half of step 7 (scripts and proofs) fails, the
   node sends `TemplateRevert` and mining returns to the parent.

### Legacy bridge

- A node that also speaks the legacy protocol answers `getdata` for a block from its wire bytes
  as soon as the body is complete, before validation completes, for blocks whose header
  check passed. It sends `inv` to legacy peers at the same point.
- Blocks received from legacy peers enter at step 2 as full bodies.

## Negotiation and legacy coexistence

The compact relay protocol is an extension negotiated inside the legacy Zcash peer-to-peer
protocol. A node that runs it remains, towards every other node, a legacy node: blocks and
transactions always travel over the legacy path as well, so no chain split can follow from
which protocol a miner runs.

### Service bit and user agent

- `version.services` carries `NODE_COMPACT_RELAY = 1 << 26` when the extension is enabled
  (bit 24 is Zakura's P2P v2 and is left alone). The user agent is `/hayai:0.1.0/`.
- A node with the extension disabled sets neither, never sends the commands below, and
  treats them as unknown commands when received: it is indistinguishable from a legacy node.

### `zcmpctver`

After `verack`, a node that saw the service bit in the peer's `version` sends the legacy-framed
command `zcmpctver` with the payload

```
max_version   u16 LE   highest extension version offered
min_version   u16 LE   lowest extension version accepted
features      u64 LE   feature bits
```

Trailing bytes are ignored so that later versions can append fields. A node that receives
`zcmpctver` without having sent one answers with its own. Both sides choose
`v = min(max_a, max_b)`; if `v` is at least both minimums the peer speaks compact relay `v`,
otherwise it stays legacy. The feature set is the intersection of both bit sets restricted to
the bits a node knows; unknown bits are ignored. A peer that never sends `zcmpctver` stays
legacy for the life of the connection; a repeated `zcmpctver` is ignored. This document
describes versions 1 and 2; a node offers `max_version = 2, min_version = 1`.

| Version | Behaviour |
|---|---|
| 1 | Short ids, batch references and prefilled transactions. A receiver forwards after it reconstructs the body. |
| 2 | Adds `CompactBlockV2` (type 12): a compact block with a full-id section. A receiver forwards once the id list matches the header, before it holds every transaction. |

| Bit | Feature |
|---|---|
| 0 | Short-id compact blocks v1: `CompactBlock`, `BlockTxnRequest`, `BlockTxn`, `TxAnnounce`, `TxRequest`, `Tx`, `Block` |
| 1 | Batch lanes v1: `BatchAnnounce`, `BatchRequest`, batch references in `CompactBlock` |
| 2 | Candidates v1: `CandidateAnnounce`, `CandidateBlock`; in use only with bit 1 |

The candidates are a feature bit and not version 3. Only lane owners publish candidates,
and a relay node can leave them out. Every version can carry them, because a
`CandidateBlock` resolves to a complete id list. A peer that does not set the bit, a
version 1 or version 2 peer included, receives neither message and sees no change. Versions
stay a total order of the forwarding rule.

### `zcmpct`

Once negotiated, every message of the Messages section travels as the payload of the legacy
command `zcmpct`: one frame as defined there (length prefix included) inside one legacy frame.
The stream therefore keeps one framing, one checksum discipline and the legacy size bounds;
proxies and middleboxes see ordinary messages. A `zcmpct` from a peer that did not negotiate
is an unknown command and is ignored, as zcashd ignores unknown commands. A `zcmpct` whose
payload does not decode disconnects the peer, as any malformed frame does.

### Both paths, always

- Transactions accepted into the prepared store are announced to legacy peers with `inv`
  (`MSG_WTX`, or `MSG_TX` for v4) and to compact-relay peers with `TxAnnounce`, at the same
  time. `getdata` and `TxRequest` are served from the same wire bytes. A transaction received
  on either path enters one sink.
- Blocks take one path whatever their origin (found locally, full `block` from a legacy peer,
  reconstructed compact block): deduplicated by hash, header-checked, then forwarded as
  `CompactBlock` to compact-relay peers and announced as `inv MSG_BLOCK` to legacy peers at
  the moment the body is available, `getdata` being served from the retained wire bytes.
  Forwarding precedes full validation (the high-bandwidth mode of BIP 152); the validator
  receives every block exactly once.
- A received compact block is forwarded (re-keyed) to version 2 peers once its id list
  matches the header, to version 1 peers once its body is complete, and announced to legacy
  peers at that same later point.
- `BatchAnnounce` goes only to peers that negotiated bit 1. A compact block with batch
  references is not forwarded to a peer without bit 1.
- `CandidateAnnounce` and `CandidateBlock` go only to peers that negotiated bits 1 and 2.
  A node receives a candidate block, rebuilds it and forwards it as a `CompactBlock` to
  peers without bit 2, and announces it with `inv` to legacy peers. A peer that sends
  either message without the bits is disconnected.
- `getheaders` is answered with up to 160 full headers (each followed by a zero transaction
  count, as zcashd sends them), so legacy nodes synchronise past a hayai node.

### Forward compatibility

New message types in later extension versions are introduced by raising `max_version`;
nodes that do not reach that version never see them. New optional behaviour is introduced
as feature bits. Legacy nodes see neither.

## Rationale

- Forwarding after the header check only: an attacker needs a valid Equihash solution at the
  current target to make nodes forward garbage, which costs a block reward. Bitcoin Core's
  high-bandwidth compact block mode makes the same trade-off.
- Batch references rather than only short ids: short ids require the receiver's store to
  contain the transactions, which holds for well-connected nodes but not for a node that just
  started or one behind a slow link; a batch announcement names exactly what will be needed and
  gives the receiver time to fetch it before the block exists. Lanes also remove the
  `BlockTxnRequest` round trip in the common case.
- No availability certificates: proof-of-work ordering does not wait for a quorum, so a lane
  owner that announces batches it then withholds only hurts its own block's propagation.
- Six-byte short ids: with a 50,000-transaction store the per-block collision probability is
  about 5 × 10⁻⁶; a collision costs one request round trip, not a failure.
- Forwarding on the id list rather than on the body: with forwarding after reconstruction,
  every hop that lacks one transaction adds one `BlockTxnRequest` round trip, so a block with
  a fresh transaction costs `h` round trips over `h` hops. A WtxId is `txid || auth_digest`,
  so a complete id list fixes both roots the header commits to; a node that has checked
  them knows the block's body up to the bytes it has not received. Batch ids and full ids are
  hashes of that content and travel unchanged; only short ids depend on a sender's nonce,
  and those are recomputed at each hop. The measured cost of the id check and the rebuild
  is 0.4–2.5 ms per hop against a 20 ms round trip in `relay/forward_latency`.
- Full ids rather than two-phase announcements or per-link keys: a two-phase announce helps
  only while a node waits for data, and full ids remove that wait; per-link keys still need
  local resolution and re-keying and add handshake state without saving a round trip.
- Full ids carry their own index: a fresh transaction can sit anywhere in the block, and an
  indexed entry leaves the batch and short-id positions around it untouched. The cost is 58
  bytes per fresh transaction.
- The type code names the layout: draft 2 put the full-id section at the end of type 6 and
  marked it by its presence. A type 6 payload cut before the section was then a valid
  payload without full ids, and a hop could remove the section and keep a valid message.
  Only the frame length and the legacy checksum caught a cut. With one type for each layout
  and no optional section, no proper prefix of a payload decodes, and each message has one
  encoding. The type follows the content and not the negotiated version, so the encoder and
  the decoder need no per-peer state.
- Mining on a built but unverified layer: an invalid block costs the miners that build on it
  at most the verification time of the block (under 150 ms for a cold block within the NU7
  limits), after which `TemplateRevert` returns them to the parent. Mining on the header
  alone is not possible: the template header commits to the history tree after the block.
- Extension inside legacy framing rather than a second port or a stream switch: one framing,
  one handshake and one size discipline; a node with the extension disabled is byte-for-byte
  a legacy node; and the legacy path is always exercised, so it cannot rot.
- Canonical order rather than weight order: a batch reference covers a run of consecutive
  positions, so in a block ordered by weight a new high-ratio transaction lands mid-order
  and breaks every batch after it. A block in canonical order is a function of its set, so
  a candidate names the set and the order follows. The selection stays by weight ratio.
- A candidate rather than a batch per position: a candidate is a set, so additions,
  removals and reprices change only the difference, never the positions around them. The
  measured cost of a block equal to its candidate is 61 bytes besides the header and the
  coinbase, against 6 bytes per transaction for short ids (`relay/bytes_on_wire`).
- Set differences against the whole candidate rather than a log of template deltas: a
  receiver that missed an announcement still resolves the next one, because each
  `CandidateAnnounce` names every batch of its candidate.

## Security considerations

- Unvalidated forwarding is bounded by proof of work; the per-peer penalty on invalid bodies
  prevents repeated use of one solution.
- Short-id collisions are detectable and never produce a wrong block: the merkle root of the
  id list is checked against the header before anything is forwarded, and the merkle root of
  the assembled body is checked again before the body is used.
- Content-addressed sections are safe to forward unchanged. A batch id is the hash of its
  WtxIds and a full id is a WtxId, so a hop cannot change what they name, and a receiver
  cannot be made to resolve them to anything but the transactions the header commits to. A
  relayed short-id nonce would let one sender grind collisions against every store on the
  network, which is why short ids are never relayed.
- A node that forwards a block whose bytes it does not hold yet has relayed one header with
  references per peer, bounded by proof of work as in header-first relay. What it then owes
  its peers is the answers to their `TxRequest`s, which it gives once its own requests are
  answered, and the block itself to legacy and version 1 peers once the body is complete. A
  block whose bytes never arrive is dropped after 20 s, and the next announcement starts
  over. An attacker who withholds the bytes of a valid header only delays its own block.
- A transaction with the same txid and different authorizing data (a malleated witness)
  passes the merkle check and fails the commitments check when the parent's history root is
  known; without it, the mismatch is caught at validation of the body, and the hops in
  between have forwarded references to a block they cannot validate, as they would after a
  header-first relay.
- Lane and batch announcements are rate-limited per peer (256 batches per lane, 64 lanes per
  peer); batches beyond the limit are ignored.
- A candidate is content-addressed by its batches: a batch id is the hash of its WtxIds,
  and the removed positions index those ids. Whatever a lane announces, a candidate block
  resolves to an id list that the receiver checks against the header roots before it
  forwards anything, and the body against the merkle root before it uses it. A lane owner
  that announces two different candidates under one `(lane_id, seq)` makes some receivers
  rebuild another set; their merkle check fails, and they fetch the full block. A malicious
  lane can therefore only waste bandwidth, within the limits of 64 lanes, 16 candidates
  per lane, 64 batches per candidate and 256 batches per lane.
- Transactions from a batch are prepared and policy-checked exactly as gossiped transactions;
  a lane cannot bypass admission.

## Version history

- Draft 1: version 1. Short ids, batch references and prefilled transactions; a receiver
  reconstructs the body, checks its merkle root and forwards it re-keyed.
- Draft 2: version 2. The full-id section, forwarding once the id list matches the header
  (merkle root always, auth data root through `hashBlockCommitments` when the parent's
  history root is known), `TxRequest` to every announcer for the missing bytes, deferred
  answers for transactions of a block a node is still completing, per-peer fresh-transaction
  tracking, and flooding of received batch announcements once complete.
- Draft 3: feature bit 2. Canonical block order, template candidates as lanes
  (`CandidateAnnounce`), and the candidate form of a block (`CandidateBlock`).
- Draft 4: type 12 (`CompactBlockV2`) carries the full-id section. Type 6 is the version 1
  layout only. In drafts 2 and 3 the section was an optional tail of type 6. That layout is
  not compatible with draft 4: a version 2 node of draft 2 or 3 and a node of draft 4
  disconnect when one sends a block with full ids. Version 1 frames are the same.

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
