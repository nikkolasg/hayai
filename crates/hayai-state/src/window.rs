//! One index over the layer window.
//!
//! A lookup through the window costs one probe in this index, not one probe per layer. The
//! per-layer maps stay the source of truth. A pop derives the entries of the popped layer's
//! keys again from the layers that remain.
//!
//! Contribution rule. Layer `h` contributes `Spent` for every outpoint in its `spent` set.
//! It contributes `Created(coin)` for every outpoint in its `created` map that is not in
//! its `spent` set. The coin index holds, for each outpoint, the contribution of the
//! newest layer that contributes it. The nullifier index holds, for each nullifier, the
//! height of the oldest layer that reveals it. Both rules give the same answer as a
//! newest-first walk of the layers followed by the base:
//!
//! - a coin lookup stops at the newest layer that spends or creates the outpoint, and that
//!   layer's contribution is the indexed one;
//! - a nullifier is present when any layer reveals it. With the oldest height, a pop
//!   removes an entry only when no older layer reveals the nullifier, and a finalization
//!   removes it only when the base now holds it.
//!
//! The index describes one tip: the committed tip of the chain. Speculative layers are never
//! in the index. A view that holds the index's tip walks its layers above that tip, then
//! probes the index. A view that does not hold it (a view taken before a pop and a push)
//! walks all its own layers. A view never reads entries of a tip it does not hold.

use std::sync::Arc;

use hayai_coins::{Coin, OutPoint, Pool};

use crate::{Layer, Map, Tip};

/// What the newest layer that mentions an outpoint says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CoinEntry {
    Created(Coin),
    Spent,
}

pub(crate) struct WindowIndex {
    tip: Tip,
    coins: Map<OutPoint, (u32, CoinEntry)>,
    nullifiers: [Map<[u8; 32], u32>; 4],
}

/// The outpoints a layer contributes.
fn contributed(layer: &Layer) -> impl Iterator<Item = &OutPoint> + '_ {
    layer
        .created
        .keys()
        .filter(|outpoint| !layer.spent.contains(*outpoint))
        .chain(layer.spent.iter())
}

/// The contribution of the newest layer of `layers` (oldest first) that mentions `outpoint`.
fn newest_contribution(layers: &[Arc<Layer>], outpoint: &OutPoint) -> Option<(u32, CoinEntry)> {
    for layer in layers.iter().rev() {
        if layer.spent.contains(outpoint) {
            return Some((layer.height, CoinEntry::Spent));
        }
        if let Some(coin) = layer.created.get(outpoint) {
            return Some((layer.height, CoinEntry::Created(coin.clone())));
        }
    }
    None
}

impl WindowIndex {
    /// An empty index over a window whose tip is `tip`.
    pub(crate) fn new(tip: Tip) -> Self {
        Self {
            tip,
            coins: Map::default(),
            nullifiers: Default::default(),
        }
    }

    pub(crate) fn tip(&self) -> Tip {
        self.tip
    }

    /// Adds the contributions of a layer pushed on the tip.
    pub(crate) fn push(&mut self, layer: &Layer) {
        for (outpoint, coin) in &layer.created {
            if layer.spent.contains(outpoint) {
                continue;
            }
            self.coins.insert(
                outpoint.clone(),
                (layer.height, CoinEntry::Created(coin.clone())),
            );
        }
        for outpoint in &layer.spent {
            self.coins
                .insert(outpoint.clone(), (layer.height, CoinEntry::Spent));
        }
        for pool in Pool::ALL {
            let index = &mut self.nullifiers[pool.index()];
            for nf in &layer.nullifiers[pool.index()] {
                index.entry(*nf).or_insert(layer.height);
            }
        }
        self.tip = Tip {
            height: layer.height,
            hash: layer.hash,
        };
    }

    /// Removes the contributions of the popped tip layer and re-derives the entries of its
    /// keys from `remaining` (the layers still in the window, oldest first). `tip` is the tip
    /// after the pop.
    pub(crate) fn pop(&mut self, layer: &Layer, remaining: &[Arc<Layer>], tip: Tip) {
        for outpoint in contributed(layer) {
            match newest_contribution(remaining, outpoint) {
                Some(entry) => {
                    self.coins.insert(outpoint.clone(), entry);
                }
                None => {
                    self.coins.remove(outpoint);
                }
            }
        }
        for pool in Pool::ALL {
            let index = &mut self.nullifiers[pool.index()];
            for nf in &layer.nullifiers[pool.index()] {
                if index.get(nf) == Some(&layer.height) {
                    index.remove(nf);
                }
            }
        }
        self.tip = tip;
    }

    /// Removes the contributions of the finalized oldest layer. An entry a newer layer
    /// contributed stays; a removed entry is now answered by the base.
    pub(crate) fn finalize(&mut self, layer: &Layer) {
        for outpoint in contributed(layer) {
            if let Some((height, _)) = self.coins.get(outpoint) {
                if *height == layer.height {
                    self.coins.remove(outpoint);
                }
            }
        }
        for pool in Pool::ALL {
            let index = &mut self.nullifiers[pool.index()];
            for nf in &layer.nullifiers[pool.index()] {
                if index.get(nf) == Some(&layer.height) {
                    index.remove(nf);
                }
            }
        }
    }

    /// The indexed contribution for `outpoint`: `Some(Some(coin))` created, `Some(None)`
    /// spent, `None` not mentioned by the window.
    pub(crate) fn coin(&self, outpoint: &OutPoint) -> Option<Option<Coin>> {
        self.coins.get(outpoint).map(|(_, entry)| match entry {
            CoinEntry::Created(coin) => Some(coin.clone()),
            CoinEntry::Spent => None,
        })
    }

    pub(crate) fn contains_nullifier(&self, pool: Pool, nf: &[u8; 32]) -> bool {
        self.nullifiers[pool.index()].contains_key(nf)
    }

    /// Number of indexed outpoints.
    #[cfg(test)]
    pub(crate) fn coins_len(&self) -> usize {
        self.coins.len()
    }
}
