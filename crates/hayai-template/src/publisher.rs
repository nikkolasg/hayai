//! Fan-out of template updates to subscribers.
//!
//! Each subscriber owns a bounded channel of [`MAX_UNACKED_UPDATES`] messages. A message
//! counts as acknowledged when the transport thread of the subscriber takes it off the
//! channel. A full channel therefore means that the subscriber has fallen that far behind, and
//! the publisher disconnects it. Tip events (`TemplateEmpty`, `TemplateFull`) and reverts
//! (`TemplateRevert`) go out at once.
//! The publisher coalesces set events per subscriber to at most one `TemplateDelta` per
//! [`COALESCE_INTERVAL`]. It computes a delta from the last template that the subscriber
//! received to the current one. Skipped intermediate templates therefore cost nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashSet;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use hayai_wire::WtxId;

use crate::live::{StoredTemplate, TemplateUpdate, BLOCK_VERSION};
use crate::messages::{
    AddedTx, Hash32, HexBytes, Message, TemplateDelta, TemplateEmpty, TemplateFull, TemplateRevert,
    TemplateTx, WtxIdHex,
};

pub const COALESCE_INTERVAL: Duration = Duration::from_millis(200);
pub const MAX_UNACKED_UPDATES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SubscriberId(pub u64);

struct Subscriber {
    id: SubscriberId,
    tx: Sender<Arc<Message>>,
    want_full_txs: bool,
    /// Last template that the publisher sent to this subscriber: the base of its next delta.
    last_sent: Option<Arc<StoredTemplate>>,
    last_sent_at: Option<Instant>,
    /// A set event arrived inside the coalescing window. A delta is due at the deadline.
    pending: bool,
}

#[derive(Default)]
pub struct Publisher {
    subscribers: Vec<Subscriber>,
    current: Option<Arc<StoredTemplate>>,
    next_id: u64,
}

impl Publisher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a subscriber and sends it the current template at once, if a template exists.
    pub fn subscribe(
        &mut self,
        want_full_txs: bool,
        now: Instant,
    ) -> (SubscriberId, Receiver<Arc<Message>>) {
        let (tx, rx) = bounded(MAX_UNACKED_UPDATES);
        let id = SubscriberId(self.next_id);
        self.next_id += 1;
        let mut sub = Subscriber {
            id,
            tx,
            want_full_txs,
            last_sent: None,
            last_sent_at: None,
            pending: false,
        };
        if let Some(current) = self.current.clone() {
            // A fresh subscriber cannot be behind yet. The channel is empty.
            let sent = send(
                &mut sub,
                Arc::new(full_message(&current, want_full_txs)),
                now,
            );
            assert!(sent, "an empty bounded channel accepts one message");
            sub.last_sent = Some(current);
        }
        self.subscribers.push(sub);
        (id, rx)
    }

    pub fn unsubscribe(&mut self, id: SubscriberId) {
        self.subscribers.retain(|s| s.id != id);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.len()
    }

    /// Distributes an update. Returns the subscribers that the publisher disconnected because
    /// they fell behind.
    pub fn publish(&mut self, update: &TemplateUpdate, now: Instant) -> Vec<SubscriberId> {
        self.current = Some(update.template().clone());
        let mut dropped = Vec::new();
        match update {
            TemplateUpdate::Empty(t) => {
                let msg = Arc::new(Message::TemplateEmpty(empty_message(t)));
                for sub in &mut self.subscribers {
                    sub.pending = false;
                    if !send(sub, msg.clone(), now) {
                        dropped.push(sub.id);
                    }
                    sub.last_sent = Some(t.clone());
                }
            }
            TemplateUpdate::Full(t) => {
                let with = Arc::new(full_message(t, true));
                let without = Arc::new(full_message(t, false));
                for sub in &mut self.subscribers {
                    sub.pending = false;
                    let msg = if sub.want_full_txs {
                        with.clone()
                    } else {
                        without.clone()
                    };
                    if !send(sub, msg, now) {
                        dropped.push(sub.id);
                    }
                    sub.last_sent = Some(t.clone());
                }
            }
            TemplateUpdate::Changed(_) => {
                for sub in &mut self.subscribers {
                    sub.pending = true;
                }
                dropped.extend(self.flush(now));
            }
            TemplateUpdate::Reverted { rejected, template } => {
                let message = |want_full_txs: bool| {
                    Arc::new(Message::TemplateRevert(TemplateRevert {
                        rejected_hash: Hash32(rejected.0),
                        template: full_body(template, want_full_txs),
                    }))
                };
                let with = message(true);
                let without = message(false);
                for sub in &mut self.subscribers {
                    sub.pending = false;
                    let msg = if sub.want_full_txs {
                        with.clone()
                    } else {
                        without.clone()
                    };
                    if !send(sub, msg, now) {
                        dropped.push(sub.id);
                    }
                    sub.last_sent = Some(template.clone());
                }
            }
        }
        self.subscribers.retain(|s| !dropped.contains(&s.id));
        dropped
    }

    /// Sends the due deltas. Returns the subscribers that the publisher disconnected because
    /// they fell behind. The caller calls it again at [`Publisher::next_deadline`].
    pub fn flush(&mut self, now: Instant) -> Vec<SubscriberId> {
        let Some(current) = self.current.clone() else {
            return Vec::new();
        };
        let mut dropped = Vec::new();
        for sub in &mut self.subscribers {
            if !sub.pending {
                continue;
            }
            let due = match sub.last_sent_at {
                Some(at) => now.duration_since(at) >= COALESCE_INTERVAL,
                None => true,
            };
            if !due {
                continue;
            }
            sub.pending = false;
            let msg = match &sub.last_sent {
                Some(base) if !Arc::ptr_eq(base, &current) => {
                    match delta_message(base, &current, sub.want_full_txs) {
                        Some(delta) => Message::TemplateDelta(delta),
                        None => full_message(&current, sub.want_full_txs),
                    }
                }
                Some(_) => continue,
                None => full_message(&current, sub.want_full_txs),
            };
            if !send(sub, Arc::new(msg), now) {
                dropped.push(sub.id);
            }
            sub.last_sent = Some(current.clone());
        }
        self.subscribers.retain(|s| !dropped.contains(&s.id));
        dropped
    }

    /// When the earliest coalesced delta becomes due.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.subscribers
            .iter()
            .filter(|s| s.pending)
            .map(|s| match s.last_sent_at {
                Some(at) => at + COALESCE_INTERVAL,
                None => Instant::now(),
            })
            .min()
    }
}

/// Returns false when the publisher must disconnect the subscriber.
fn send(sub: &mut Subscriber, msg: Arc<Message>, now: Instant) -> bool {
    match sub.tx.try_send(msg) {
        Ok(()) => {
            sub.last_sent_at = Some(now);
            true
        }
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    }
}

fn template_tx(c: &crate::candidate::Candidate, want_full_txs: bool) -> TemplateTx {
    TemplateTx {
        wtxid: WtxIdHex(c.wtxid),
        len: u32::try_from(c.bytes.len()).expect("transaction size fits in u32"),
        bytes: want_full_txs.then(|| HexBytes(c.bytes.clone())),
    }
}

pub fn full_message(t: &StoredTemplate, want_full_txs: bool) -> Message {
    Message::TemplateFull(full_body(t, want_full_txs))
}

fn full_body(t: &StoredTemplate, want_full_txs: bool) -> TemplateFull {
    TemplateFull {
        template_id: t.id,
        parent_hash: Hash32(t.tip.parent_hash.0),
        height: t.tip.height,
        time: t.tip.time,
        bits: t.tip.bits,
        version: BLOCK_VERSION,
        coinbase: HexBytes(t.coinbase.bytes.clone()),
        txs: t
            .txs
            .iter()
            .map(|c| template_tx(c, want_full_txs))
            .collect(),
        merkle_root: Hash32(t.merkle_root),
        auth_data_root: Hash32(t.auth_data_root),
        block_commitments: Hash32(t.block_commitments),
        expiry: t.tip.height,
        fees_total: t.fees_total,
    }
}

pub fn empty_message(t: &StoredTemplate) -> TemplateEmpty {
    TemplateEmpty {
        template_id: t.id,
        parent_hash: Hash32(t.tip.parent_hash.0),
        height: t.tip.height,
        time: t.tip.time,
        bits: t.tip.bits,
        version: BLOCK_VERSION,
        coinbase: HexBytes(t.coinbase.bytes.clone()),
        merkle_root: Hash32(t.merkle_root),
        auth_data_root: Hash32(t.auth_data_root),
        block_commitments: Hash32(t.block_commitments),
        expiry: t.tip.height,
    }
}

/// The delta from `base` to `new`. Returns `None` when the transactions common to both
/// changed their relative order. A delta cannot express that change, so a full template
/// follows.
pub fn delta_message(
    base: &StoredTemplate,
    new: &StoredTemplate,
    want_full_txs: bool,
) -> Option<TemplateDelta> {
    if base.tip != new.tip {
        return None;
    }
    let base_ids: AHashSet<WtxId> = base.txs.iter().map(|c| c.wtxid).collect();
    let new_ids: AHashSet<WtxId> = new.txs.iter().map(|c| c.wtxid).collect();
    let kept_in_base = base.txs.iter().filter(|c| new_ids.contains(&c.wtxid));
    let kept_in_new = new.txs.iter().filter(|c| base_ids.contains(&c.wtxid));
    if !kept_in_base
        .zip(kept_in_new)
        .all(|(a, b)| a.wtxid == b.wtxid)
    {
        return None;
    }
    let removed = base
        .txs
        .iter()
        .enumerate()
        .filter(|(_, c)| !new_ids.contains(&c.wtxid))
        .map(|(i, _)| u32::try_from(i).expect("index fits in u32"))
        .collect();
    let added = new
        .txs
        .iter()
        .enumerate()
        .filter(|(_, c)| !base_ids.contains(&c.wtxid))
        .map(|(i, c)| {
            let tx = template_tx(c, want_full_txs);
            AddedTx {
                position: u32::try_from(i).expect("index fits in u32"),
                wtxid: tx.wtxid,
                len: tx.len,
                bytes: tx.bytes,
            }
        })
        .collect();
    Some(TemplateDelta {
        template_id: new.id,
        base_template_id: base.id,
        removed,
        added,
        coinbase: (base.fees_total != new.fees_total).then(|| HexBytes(new.coinbase.bytes.clone())),
        merkle_root: Hash32(new.merkle_root),
        auth_data_root: Hash32(new.auth_data_root),
        block_commitments: Hash32(new.block_commitments),
        fees_total: new.fees_total,
    })
}

/// Applies a delta to a transaction list, as a pool would. The tests use it to check deltas.
pub fn apply_delta(base: &[WtxId], delta: &TemplateDelta) -> Vec<WtxId> {
    let mut out: Vec<WtxId> = base
        .iter()
        .enumerate()
        .filter(|(i, _)| !delta.removed.contains(&(*i as u32)))
        .map(|(_, id)| *id)
        .collect();
    for added in &delta.added {
        out.insert(added.position as usize, added.wtxid.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::SetEvent;
    use crate::live::{LiveTemplate, TemplateConfig};
    use crate::test_support::{candidate, coinbase_spec, tip};

    fn live_with_tip() -> LiveTemplate {
        let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
        live.on_tip(tip(1), &[], &[], |_| {}).unwrap();
        live
    }

    fn ids(t: &StoredTemplate) -> Vec<WtxId> {
        t.txs.iter().map(|c| c.wtxid).collect()
    }

    #[test]
    fn deltas_are_coalesced_and_apply_cleanly() {
        let mut live = live_with_tip();
        let mut publisher = Publisher::new();
        let t0 = Instant::now();
        publisher.publish(&TemplateUpdate::Full(live.current().unwrap().clone()), t0);
        let (_id, rx) = publisher.subscribe(false, t0);
        let first = rx.try_recv().unwrap();
        let Message::TemplateFull(first) = &*first else {
            panic!("expected TemplateFull");
        };
        assert!(first.txs.is_empty());
        let base_ids = ids(live.current().unwrap());

        // Three set events inside the window: one pending delta. The publisher sent nothing yet.
        for i in 1..=3 {
            let update = live
                .apply(SetEvent::Added(candidate(i, 10_000 * i, 1, &[])))
                .unwrap()
                .unwrap();
            assert!(publisher
                .publish(&update, t0 + Duration::from_millis(10 * i))
                .is_empty());
        }
        let Err(_) = rx.try_recv() else {
            panic!("deltas inside the coalescing window must wait");
        };
        assert_eq!(publisher.next_deadline(), Some(t0 + COALESCE_INTERVAL));

        assert!(publisher.flush(t0 + COALESCE_INTERVAL).is_empty());
        let msg = rx.try_recv().unwrap();
        let Message::TemplateDelta(delta) = &*msg else {
            panic!("expected one coalesced TemplateDelta, got {msg:?}");
        };
        assert_eq!(delta.base_template_id, first.template_id);
        assert_eq!(delta.added.len(), 3);
        assert!(delta.removed.is_empty());
        let Some(_) = delta.coinbase else {
            panic!("fees changed, so the coinbase must be included");
        };
        assert_eq!(apply_delta(&base_ids, delta), ids(live.current().unwrap()));
        assert_eq!(publisher.next_deadline(), None);
    }

    #[test]
    fn a_revert_reaches_every_subscriber_at_once() {
        let mut live = live_with_tip();
        let a = candidate(1, 20_000, 1, &[]);
        live.apply(SetEvent::Added(a.clone())).unwrap();
        let mut publisher = Publisher::new();
        let t0 = Instant::now();
        let (_id, rx) = publisher.subscribe(true, t0);
        let mut next = tip(2);
        next.parent_hash = hayai_wire::header::BlockHash([0x22; 32]);
        let mut updates = Vec::new();
        live.on_speculative_tip(next, &[a.wtxid], &[], |u| updates.push(u))
            .unwrap();
        live.on_revert(tip(1), |u| updates.push(u)).unwrap();
        for update in &updates {
            assert!(publisher.publish(update, t0).is_empty());
        }
        let messages: Vec<Arc<Message>> = rx.try_iter().collect();
        let [empty, full, revert] = messages.as_slice() else {
            panic!("Empty, Full and Revert, got {messages:?}");
        };
        let (Message::TemplateEmpty(_), Message::TemplateFull(full)) = (&**empty, &**full) else {
            panic!("the speculative tip sends Empty then Full");
        };
        assert!(full.txs.is_empty());
        let Message::TemplateRevert(revert) = &**revert else {
            panic!("the revert sends TemplateRevert");
        };
        assert_eq!(revert.rejected_hash, Hash32([0x22; 32]));
        assert_eq!(revert.template.parent_hash, Hash32(tip(1).parent_hash.0));
        assert_eq!(revert.template.txs.len(), 1);
        let Some(bytes) = &revert.template.txs[0].bytes else {
            panic!("full transactions were requested");
        };
        assert_eq!(bytes.0, a.bytes);
    }

    #[test]
    fn removal_delta_matches_the_new_template() {
        let mut live = live_with_tip();
        let cands: Vec<_> = (1..=5).map(|i| candidate(i, 10_000 * i, 1, &[])).collect();
        for c in &cands {
            live.apply(SetEvent::Added(c.clone())).unwrap();
        }
        let base = live.current().unwrap().clone();
        live.apply(SetEvent::Removed(cands[2].wtxid)).unwrap();
        live.apply(SetEvent::Added(candidate(9, 25_000, 1, &[])))
            .unwrap();
        let new = live.current().unwrap().clone();
        let delta = delta_message(&base, &new, true).unwrap();
        assert_eq!(apply_delta(&ids(&base), &delta), ids(&new));
        assert_eq!(delta.added.len(), 1);
        let Some(bytes) = &delta.added[0].bytes else {
            panic!("full transactions were requested");
        };
        assert_eq!(bytes.0.len() as u32, delta.added[0].len);
    }

    #[test]
    fn tip_updates_bypass_coalescing_and_slow_subscribers_are_dropped() {
        let mut live = live_with_tip();
        let mut publisher = Publisher::new();
        let t0 = Instant::now();
        publisher.publish(&TemplateUpdate::Full(live.current().unwrap().clone()), t0);
        let (slow, rx) = publisher.subscribe(true, t0);
        let _first = rx.try_recv().unwrap();
        let mut updates = Vec::new();
        live.on_tip(tip(2), &[], &[], |u| updates.push(u)).unwrap();
        for u in &updates {
            assert!(publisher.publish(u, t0).is_empty());
        }
        let Message::TemplateEmpty(_) = &*rx.try_recv().unwrap() else {
            panic!("expected TemplateEmpty first");
        };
        let Message::TemplateFull(_) = &*rx.try_recv().unwrap() else {
            panic!("expected TemplateFull second");
        };

        // The subscriber never reads. After MAX_UNACKED_UPDATES queued tip updates, the next
        // update drops the subscriber.
        let mut dropped = Vec::new();
        for h in 3..3 + MAX_UNACKED_UPDATES as u32 {
            let mut updates = Vec::new();
            live.on_tip(tip(h), &[], &[], |u| updates.push(u)).unwrap();
            for u in &updates {
                dropped.extend(publisher.publish(u, t0));
            }
            if !dropped.is_empty() {
                break;
            }
        }
        assert_eq!(dropped, vec![slow]);
        assert_eq!(publisher.subscriber_count(), 0);
        assert_eq!(rx.len(), MAX_UNACKED_UPDATES);
    }
}
