# hayai template push protocol

- Status: draft 1
- Scope: delivery of block templates from a hayai node to mining pool software. The protocol
  replaces `getblocktemplate` long polling. The consensus rules do not change.

## Terminology

- Template: all the data that a pool needs to build a block header and body. This data is the
  header fields except nonce and solution, the coinbase transaction, the transaction set in
  canonical order, and the per-template roots.
- Template id: a monotonically increasing `u64` per node process.
- Tip event: a change of the best chain tip of the node.
- Set event: the prepared store gets or loses transactions that affect the template.
- Speculative tip: a block whose layer is built (contextual rules, tree appends, history
  tree append), and whose scripts and proofs the node still verifies
  (`docs/architecture.md`, Speculative tip).

## Abstract

The node keeps 1 live template per configured coinbase target. When the template changes,
the node pushes an update to every subscriber. The updates are incremental. On a tip event,
the node sends a full template. On a set event, the node sends only the changed transaction
list and the roots.

Pools submit solved headers with the template id. The node validates the submission against
the exact template that it served.

## Motivation

- `getblocktemplate` rebuilds the template on every call, for each caller, and polls the
  mempool every 5 s. Thus fee-bearing work gets to the pool up to 5 s late. Several pool
  instances get different random selections. With different selections, the own-block caches
  do not match on submission.
- A live template costs 1 incremental update per event instead of 1 rebuild per call.
- A submission against a known template id makes the own-block path a lookup plus header
  checks.

## Transport

The transport is a length-prefixed binary stream (the same framing as the relay protocol)
over a local TCP or Unix socket. An equivalent JSON-lines encoding exists for pools that
prefer text. The field names and semantics are the same in both encodings.

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
bytes hold the value that the block rules require:

- Before NU7: the subsidy and all the fees.
- From NU7: the subsidy and the miner share of the fees.
- From the NSM reissuance height: also the reissuance bonus.

A pool that sends a `coinbase` override must keep that value.

## Procedures

### Tip event

The tip event of a block B is the push of the layer of B: a speculative push after
`build_layer`, or a commit. A header alone does not start it. The header of a template on B
commits to the ZIP 221 history tree after B. The leaf of B in that tree holds the final
Sapling and Orchard roots of B. These roots need the body of B and its tree appends. Thus no
template on B (empty or full) can exist before the layer of B.

1. The node sends `TemplateEmpty` in the same scheduling quantum as the layer push (target:
   under 1 ms). The node precomputes the coinbase for height+1 when it knows the height. The
   node prepares a shielded coinbase proof for the subsidy-only output in advance. The node
   adds the fee outputs as a transparent output. Thus the node makes no proof on the event
   path.
2. The node rebuilds the full template from the feerate-ordered view of the prepared store.
   It excludes the transactions that the new tip mined and the transactions that now conflict
   with it, with their descendants. The children of a mined transaction stay, because they no
   longer wait for their parent. The node then sends `TemplateFull`.
3. The templates for the previous parent stay valid for submission for 60 s. A late solution
   is still a block if the tip did not change.

### Speculative tip and revert

- When `build_layer` passes, the node pushes the layer of B as a speculative tip. It sends
  `TemplateEmpty` then `TemplateFull` on B (steps 1 and 2 above). At the same time, the
  scripts and the proofs of the unknown transactions of B (`verify`) run.
- When `verify` passes, B commits. The templates on B stay. The node sends no message.
- When `verify` fails, the node discards B and every speculative block on top of it. It sends
  `TemplateRevert` with the full template on the parent of B. That template holds the
  transactions that B mined or conflicted with, except the transactions that left the
  prepared store in the meantime. A pool stops work on B and its descendants immediately.
- Work on a speculative tip lasts at most the verification time of the block. This time is
  less than 150 ms for a cold block within the NU7 limits.

### Set event

When the prepared store adds, removes or re-prices transactions, the live template applies
the change to its ordered set of eligible transactions. If the selected set changed, the live
template sends `TemplateDelta` to the subscribers. The node merges deltas to at most 1 per
200 ms per subscriber.

### Selection

- The node selects the eligible transactions with the ZIP 317 block production algorithm. It
  uses the order of the weight ratio (fee / conventional fee, capped) in place of the random
  pick. First it takes the eligible transactions that pay the conventional fee. Then it takes
  the others up to the unpaid action limit of the block, which is 0
  (`docs/mempool-policy.md`, Template selection). The node tracks dependencies, so a child is
  selectable only after its parents.
- The selection is deterministic for a given set. Thus every subscriber sees the same
  template, and the own-block cache always matches.
- ZIP 317 weighted random sampling is a recommendation, and not a consensus rule. The
  protocol uses a deterministic order by weight ratio for cache stability and O(log n)
  updates.
- The block order is the canonical order of the selected set: parents first, then txid
  (`docs/protocol-compact-relay.md`, Canonical order). The same set gives the same block
  bytes on every node. A new high-ratio transaction does not move the transactions around
  it. `TemplateDelta` positions refer to this order.

### Lane publication

- A node with the compact relay publishes every template change (`TemplateFull`,
  `TemplateDelta`, `TemplateRevert`) as its lane:
  - A `BatchAnnounce` of the added transactions, with `seq` equal to the template id.
  - A `CandidateAnnounce` that names the batches of the template on its parent and the
    positions that left it (`docs/protocol-compact-relay.md`, Candidates).

  The node does not publish a `TemplateEmpty`, because a full template follows it
  immediately.
- A block that a pool solves on a published template then travels to peers with the
  candidates feature. It travels as a reference to the candidate plus its coinbase. Peers
  that hold the candidate rebuild the block without a per-transaction id.

### Own-block commit

After each template change, the node prebuilds the layer of the body of the template against
the tip while it is idle. It does this at most once per 200 ms (`docs/architecture.md`,
Prebuilt bodies). A submission whose body is that template commits after the header and
coinbase rules only. The merkle root and the auth data root come from the coinbase and the
prebuilt branches. The layer is the prebuilt layer plus the coinbase outputs.

### Submission

- `Submit` names the template id. The node rebuilds the block from the stored template plus
  the submitted header fields (and the coinbase override, if any). The node checks the header
  and the merkle root. When the body is the body of the template, the node commits from the
  prebuilt body (section Own-block commit). Else the node commits through the normal
  contextual path. The context-free work is zero, because every transaction is already
  prepared.
- The node accepts a coinbase override when it changes only the coinbase scriptSig, outputs
  or extra nonce fields. The transaction set of the template does not change, so the
  own-block fast path still applies.

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
  `longpoll` capability blocks until a newer template exists. The call returns as follows:
  - Immediately after a tip event or a revert, with the coinbase-only template first and then
    the full one, as the subscribers of the push protocol see them.
  - After a short delay (5 s) when only the transaction set changed.
  - After at most 60 s in all cases.
  - Immediately without the capability, or with a `longpollid` of a previous tip.

  `submitold` is `false` when the tip changed, and `true` otherwise.
- `submitblock` takes the full block hex, as zcashd does. With a `workid`, the shim
  reconstructs the block from the stored template with the submitted header fields and
  coinbase (`rebuild_block`). When the bytes match, the submission is the own-block path, and
  the validator skips the context-free work. Otherwise, the node parses and validates the
  block like a block from a peer. The results are zcashd's: `null`, `"duplicate"`,
  `"inconclusive"`, `"rejected"`. The shim returns error `-22` for input that does not
  decode.
- `getblockcount` and `getbestblockhash` answer pool health checks. The shim does not
  support `mode: "proposal"` (error `-8`).
- Transport: HTTP/1.1 `POST` with `Content-Length`, keep-alive, JSON-RPC 1.0 or 2.0 as the
  request chose. A 1.0 response carries both `result` and `error`. A 2.0 response carries one
  of them.

## Rationale

- Deterministic selection instead of weighted random sampling: the designers of ZIP 317 made
  the random sampling to give low-fee transactions a chance under congestion. The second pass
  takes low-fee transactions up to the unpaid action limit. This limit is 0 as in Zakura and
  Zebra, because the mempool admits no transaction with an unpaid action. Determinism gives
  cache hits and the same templates across pool instances.
- Deltas instead of full templates on set events: a full 2 MB template every 200 ms is
  wasteful over the control channel of a pool. A delta is usually a few hundred bytes.
- JSON-lines variant: Stratum servers are often not Rust. The text encoding costs nothing on
  the event path, because the node encodes once per update.

## Security considerations

- Template ids are per connection. They are unguessable only in this sense: a pool cannot
  submit against a template that the node did not serve to it. In all cases, the node does a
  full validation.
- The node disconnects a subscriber that falls behind after 64 unacknowledged updates. This
  limits the memory of the node.
