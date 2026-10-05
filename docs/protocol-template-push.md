# hayai template push protocol

- Status: draft 1
- Scope: delivery of block templates from a hayai node to mining pool software. The protocol
  replaces `getblocktemplate` long polling. The consensus rules are unchanged.

## Terminology

- **Template**: everything that a pool needs to build a block header and body: the header fields
  except nonce and solution, the coinbase transaction, the transaction set in canonical
  order, and the per-template roots.
- **Template id**: a monotonically increasing `u64` per node process.
- **Tip event**: the best chain tip of the node changed. **Set event**: the prepared store gained
  or lost transactions that affect the template.
- **Speculative tip**: a block whose layer is built (contextual rules, tree appends, history
  tree append) and whose scripts and proofs are still being verified
  (`docs/architecture.md`, Speculative tip).

## Abstract

The node keeps one live template per configured coinbase target. When the template changes, the
node pushes an update to every subscriber. The updates are incremental. A tip event sends a full
template. A set event sends only the changed transaction list and the roots. Pools submit solved
headers with the template id. The node validates the submission against the exact template that
it served.

## Motivation

- `getblocktemplate` rebuilds the template on every call, per caller, and polls the mempool
  every 5 s. Fee-bearing work reaches the pool up to 5 s late. Several pool instances get
  different random selections, and this defeats the own-block caches on submission.
- A live template costs one incremental update per event instead of one rebuild per call.
- Submission against a known template id makes the own-block path a lookup plus header checks.

## Transport

- A length-prefixed binary stream (the same framing as the relay protocol) over a local TCP or
  Unix socket. An equivalent JSON-lines encoding exists for pools that prefer text. The field
  names and semantics are identical.

## Messages

| Direction | Type | Payload |
|---|---|---|
| pool → node | `Subscribe` | `coinbase_target` (address or script), `max_block_bytes`, `want_full_txs: bool` |
| node → pool | `TemplateFull` | `template_id`, `parent_hash`, `height`, `time`, `bits`, `version`, `coinbase` (wire bytes), `tx_count`, (`WtxId`, `len`, wire bytes?)[tx_count], `merkle_root`, `auth_data_root`, `block_commitments`, `expiry`, `fees_total` |
| node → pool | `TemplateDelta` | `template_id`, `base_template_id`, `removed` (indexes), `added` (position, `WtxId`, `len`, wire bytes?)[], new `coinbase` if fees changed, new roots |
| node → pool | `TemplateEmpty` | `template_id`, `parent_hash`, `height`, `time`, `bits`, `coinbase`, roots: a coinbase-only template sent on a tip event before the full one |
| node → pool | `TemplateRevert` | `rejected_hash`, then the `TemplateFull` fields: the speculative block `rejected_hash` failed verification, and this is the full template on its parent again |
| pool → node | `Submit` | `template_id`, `time`, `nonce`, `solution`, optional `coinbase` override |
| node → pool | `SubmitResult` | `template_id`, `accepted: bool`, `reason`, `block_hash` |

`fees_total` is the total of the fees of the transactions of the template. The `coinbase`
bytes hold the value that the block rules require: before NU7 the subsidy and all the fees,
from NU7 the subsidy and the miner share of the fees, and from the NSM reissuance height
the reissuance bonus too. A pool that sends a `coinbase` override must keep that value.

## Procedures

### Tip event

The tip event of a block B is the push of B's layer: a speculative push after
`build_layer`, or a commit. A header alone does not start it. The header of a template on B
commits to the ZIP 221 history tree after B, and the leaf of B in that tree holds B's final
Sapling and Orchard roots. These roots need B's body and its tree appends, so no template on
B (empty or full) can exist before B's layer.

1. The node sends `TemplateEmpty` within the same scheduling quantum as the layer push
   (target: under 1 ms). The node precomputes the coinbase for height+1 as soon as it knows
   the height. The node prepares a shielded coinbase proof for the subsidy-only output in
   advance. The node adds the fee outputs as a transparent output. Therefore the node
   generates no proof on the event path.
2. The node rebuilds the full template from the feerate-ordered view of the prepared store. It
   excludes the transactions that the new tip mined and the transactions that are now in
   conflict with it, with their descendants. The children of a mined transaction stay: they
   no longer wait for their parent. It then sends `TemplateFull`.
3. The templates for the previous parent stay valid for submission for 60 s. A late solution is
   still a block if the chain has not moved.

### Speculative tip and revert

- The node pushes B's layer as a speculative tip when `build_layer` passes, and sends
  `TemplateEmpty` then `TemplateFull` on B (steps 1 and 2 above). The scripts and the proofs
  of B's unknown transactions (`verify`) run at the same time.
- When `verify` passes, B commits. The templates on B stay. No message is sent.
- When `verify` fails, the node drops B and every speculative block on top of it, and sends
  `TemplateRevert` with the full template on B's parent. That template holds the
  transactions that B mined or conflicted with, except the ones that left the prepared
  store in the meantime. A pool stops work on B and its descendants at once.
- Work on a speculative tip lasts at most the verification time of the block (under 150 ms
  for a cold block within the NU7 limits).

### Set event

- When the prepared store adds, removes or re-prices transactions, the live template applies
  the change to its ordered candidate set. If the selected set changed, the live template sends
  `TemplateDelta` to the subscribers. The node coalesces deltas to at most one per 200 ms per
  subscriber.

### Selection

- The node selects the candidates with the ZIP 317 block production algorithm, in the
  order of the weight ratio (fee / conventional fee, capped) in place of the random pick:
  first the candidates that pay the conventional fee, then the others up to the unpaid
  action limit of the block, which is 0 (`docs/mempool-policy.md`, Template selection). The node tracks
  dependencies, so a child is selectable only after its parents. The selection is
  deterministic for a given set. Every subscriber therefore sees the same template, and the
  own-block cache always matches.
- The block order is the canonical order of the selected set: parents first, then txid
  (`docs/protocol-compact-relay.md`, Canonical order). The same set gives the same block
  bytes on every node, and a new high-ratio transaction does not move the transactions
  around it. `TemplateDelta` positions refer to this order.

### Lane publication

- A node with the compact relay publishes every template change (`TemplateFull`,
  `TemplateDelta`, `TemplateRevert`) as its lane: one `BatchAnnounce` of the added
  transactions, with `seq` equal to the template id, and one `CandidateAnnounce` that names
  the batches of the template on its parent and the positions that left it
  (`docs/protocol-compact-relay.md`, Candidates). A `TemplateEmpty` is not published: a full
  template follows it at once.
- A block that a pool solves on a published template then travels to peers with the
  candidates feature as a reference to the candidate plus its coinbase. Peers that hold the
  candidate rebuild the block without a per-transaction id.

### Own-block commit

- After each template change, the node prebuilds the layer of the template's body against
  the tip while it is idle, at most once per 200 ms (`docs/architecture.md`, Prebuilt
  bodies). A submission whose body is that template commits after the header and coinbase
  rules only: the merkle root and the auth data root come from the coinbase and the
  prebuilt branches, and the layer is the prebuilt one plus the coinbase outputs.
- ZIP 317 weighted random sampling is a recommendation, and not a consensus rule. The protocol
  uses deterministic ordering by weight ratio for cache stability and O(log n) updates.

### Submission

- `Submit` names the template id. The node rebuilds the block from the stored template plus the
  submitted header fields (and the coinbase override, if any). The node checks the header,
  verifies the merkle root, and commits from the prebuilt body when it is the template's
  (section Own-block commit), else through the normal contextual path. The context-free
  work is zero, because every transaction is already prepared.
- The node accepts a coinbase override when it changes only the coinbase scriptSig, outputs or
  extra nonce fields. The transaction set of the template is unchanged, so the own-block fast
  path still applies.

## getblocktemplate compatibility

A shim over the same live template (`hayai-rpc`) serves pool software that speaks zcashd's
`getblocktemplate`. The shim rebuilds nothing per call.

- `getblocktemplate` returns the current template in zcashd's shape: `capabilities`,
  `version`, `previousblockhash`, `blockcommitmentshash` (also as `lightclientroothash` and
  `finalsaplingroothash`), `defaultroots { merkleroot, chainhistoryroot, authdataroot,
  blockcommitmentshash }`, `transactions[] { data, hash, authdigest, depends, fee, sigops,
  required }`, `coinbasetxn`, `longpollid`, `target`, `mintime`, `mutable`, `noncerange`,
  `sigoplimit`, `sizelimit`, `curtime`, `bits`, `height`, `maxtime`, `workid`. Hashes are hex
  in display order. `depends` are 1-based indexes into `transactions`. The coinbase `fee` is
  the negated fee total.
- `longpollid` and `workid` are the template id. A request that carries `longpollid` and the
  `longpoll` capability blocks until a newer template exists. The call returns at once after a
  tip event or a revert (the coinbase-only template first, then the full one, as the
  subscribers of the push protocol see them). The call returns after a short delay (5 s)
  when only the transaction set changed. The call returns after at most 60 s in all cases.
  `submitold` is `false` when the tip moved, and `true` otherwise. Without the capability,
  or with a `longpollid` of a previous tip, the call returns at once.
- `submitblock` takes the full block hex, as zcashd does. With a `workid`, the shim
  reconstructs the block from the stored template with the submitted header fields and coinbase
  (`rebuild_block`). When the bytes match, the submission is the own-block path, and the
  validator skips the context-free work. Otherwise, the node parses and validates the block
  like a block from a peer. The results are zcashd's: `null`, `"duplicate"`, `"inconclusive"`,
  `"rejected"`. Undecodable input is error `-22`.
- `getblockcount` and `getbestblockhash` answer pool health checks. The shim does not support
  `mode: "proposal"` (error `-8`).
- Transport: HTTP/1.1 `POST` with `Content-Length`, keep-alive, JSON-RPC 1.0 or 2.0 as the
  request chose (1.0 responses carry both `result` and `error`; 2.0 responses carry one of
  them).

## Rationale

- Deterministic selection over weighted random sampling: the designers of ZIP 317 made the
  random sampling to give low-fee transactions a chance under congestion. The second pass
  takes low-fee transactions up to the unpaid action limit, which is 0 as in Zakura and
  Zebra: the mempool admits no transaction with an unpaid action.
  Determinism gives cache hits and identical templates across pool instances.
- Deltas instead of full templates on set events: a full 2 MB template every 200 ms is wasteful
  over the control channel of a pool. A delta is typically a few hundred bytes.
- JSON-lines variant: Stratum servers are often not Rust. The text encoding costs nothing on the
  event path, because the node encodes once per update.

## Security considerations

- Template ids are per connection. They are unguessable only in this sense: a pool cannot submit
  against a template that the node did not serve to it. The node validates fully in any case.
- The node disconnects a subscriber that falls behind after 64 unacknowledged updates. This
  bounds the node memory.
