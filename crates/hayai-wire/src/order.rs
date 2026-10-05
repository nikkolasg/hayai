//! The canonical block order (`docs/protocol-compact-relay.md`, section Canonical order).
//!
//! Consensus requires only that a parent precedes its children in a block. The canonical
//! order fixes every other choice, so that the same transaction set always gives the same
//! block bytes. A transaction's depth is 0 when it spends no output of another transaction
//! of the set, and 1 plus the largest depth of those parents otherwise. The order sorts by
//! depth, then by txid as a 32-byte string in internal byte order. A parent always has a
//! smaller depth than its children, so the order is a topological order.

use std::collections::HashMap;

use hayai_crypto::zcash_primitives;
use zcash_primitives::transaction::TxId;

use crate::RawTx;

/// The parent relation of a transaction set contains a cycle. Real transactions cannot form
/// one (a txid commits to its inputs); a set that does is malformed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the parent relation of the transaction set has a cycle")]
pub struct OrderCycle;

/// The canonical order of a set given by its txids and, for each member, the txids its
/// transparent inputs spend. Input txids outside the set are ignored. Returns the member
/// indexes in canonical order. Duplicate txids are not a set and are a caller error.
pub fn canonical_order(txids: &[TxId], spends: &[Vec<TxId>]) -> Result<Vec<usize>, OrderCycle> {
    assert_eq!(txids.len(), spends.len(), "one spend list per transaction");
    let position: HashMap<TxId, usize> = txids.iter().enumerate().map(|(i, t)| (*t, i)).collect();
    // Parents and children by index, without repeats of one parent.
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); txids.len()];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); txids.len()];
    for (child, inputs) in spends.iter().enumerate() {
        for txid in inputs {
            let Some(&parent) = position.get(txid) else {
                continue;
            };
            if !parents[child].contains(&parent) {
                parents[child].push(parent);
                children[parent].push(child);
            }
        }
    }
    // Kahn's algorithm assigns each depth once every parent has one.
    let mut waiting: Vec<usize> = parents.iter().map(Vec::len).collect();
    let mut depth = vec![0u32; txids.len()];
    let mut ready: Vec<usize> = (0..txids.len()).filter(|&i| waiting[i] == 0).collect();
    let mut done = 0;
    while let Some(i) = ready.pop() {
        done += 1;
        for &child in &children[i] {
            depth[child] = depth[child].max(depth[i] + 1);
            waiting[child] -= 1;
            if waiting[child] == 0 {
                ready.push(child);
            }
        }
    }
    if done != txids.len() {
        return Err(OrderCycle);
    }
    let mut order: Vec<usize> = (0..txids.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        (depth[a], txids[a].as_ref()).cmp(&(depth[b], txids[b].as_ref()))
    });
    Ok(order)
}

/// The txids that the transparent inputs of `tx` spend.
pub fn spent_txids(tx: &RawTx) -> Vec<TxId> {
    tx.tx.transparent_bundle().map_or_else(Vec::new, |bundle| {
        if bundle.is_coinbase() {
            return Vec::new();
        }
        bundle
            .vin
            .iter()
            .map(|txin| TxId::from_bytes(*txin.prevout().hash()))
            .collect()
    })
}

/// [`canonical_order`] of parsed transactions.
pub fn canonical_order_of(txs: &[&RawTx]) -> Result<Vec<usize>, OrderCycle> {
    let txids: Vec<TxId> = txs.iter().map(|t| t.txid).collect();
    let spends: Vec<Vec<TxId>> = txs.iter().map(|t| spent_txids(t)).collect();
    canonical_order(&txids, &spends)
}

/// Whether `txs` is in canonical order.
pub fn is_canonical(txs: &[&RawTx]) -> bool {
    match canonical_order_of(txs) {
        Ok(order) => order.iter().enumerate().all(|(i, &j)| i == j),
        Err(OrderCycle) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> TxId {
        TxId::from_bytes([n; 32])
    }

    #[test]
    fn depth_then_txid() {
        // 9 spends 1; 5 spends 9 and 3; 2 and 7 stand alone.
        let txids = [id(9), id(5), id(2), id(1), id(7), id(3)];
        let spends = vec![
            vec![id(1)],
            vec![id(9), id(3), id(0xee)],
            vec![],
            vec![],
            vec![],
            vec![],
        ];
        let order = canonical_order(&txids, &spends).unwrap();
        let sorted: Vec<TxId> = order.iter().map(|&i| txids[i]).collect();
        assert_eq!(sorted, vec![id(1), id(2), id(3), id(7), id(9), id(5)]);
    }

    #[test]
    fn same_set_in_any_input_order_gives_one_order() {
        let txids = [id(4), id(8), id(6), id(1)];
        let spends = vec![vec![id(8)], vec![id(6)], vec![], vec![id(4)]];
        let first: Vec<TxId> = canonical_order(&txids, &spends)
            .unwrap()
            .iter()
            .map(|&i| txids[i])
            .collect();
        let reversed_ids: Vec<TxId> = txids.iter().rev().copied().collect();
        let reversed_spends: Vec<Vec<TxId>> = spends.iter().rev().cloned().collect();
        let second: Vec<TxId> = canonical_order(&reversed_ids, &reversed_spends)
            .unwrap()
            .iter()
            .map(|&i| reversed_ids[i])
            .collect();
        assert_eq!(first, second);
        assert_eq!(first, vec![id(6), id(8), id(4), id(1)]);
    }

    #[test]
    fn a_cycle_is_an_error() {
        let txids = [id(1), id(2)];
        let spends = vec![vec![id(2)], vec![id(1)]];
        assert_eq!(canonical_order(&txids, &spends), Err(OrderCycle));
    }
}
