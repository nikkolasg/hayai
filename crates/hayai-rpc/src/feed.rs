//! The RPC's view of the live template: the current template, the recent templates kept for
//! submission, and the wake-ups that long polls wait on.
//!
//! The node feeds every [`TemplateUpdate`] that its `LiveTemplate` emits into
//! [`TemplateFeed::publish`]. The feed rebuilds nothing per call. Templates are shared `Arc`s,
//! so the store of the feed costs no copies.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use hayai_template::{StoredTemplate, TemplateStore, TemplateUpdate};

struct State {
    current: Option<Arc<StoredTemplate>>,
    store: TemplateStore,
    /// Id of the newest template that a tip event produced (`Empty` or `Full`).
    tip_template_id: u64,
}

pub struct TemplateFeed {
    state: Mutex<State>,
    changed: Condvar,
}

/// Why a long poll returned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wake {
    /// The chain tip moved: previous work is stale (`submitold: false`).
    Tip,
    /// Only the transaction set changed (`submitold: true`).
    Set,
    /// The maximum wait elapsed. The template is unchanged.
    Timeout,
}

impl TemplateFeed {
    pub fn new(retention: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                current: None,
                store: TemplateStore::new(retention),
                tip_template_id: 0,
            }),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records an update from the live template and wakes waiting long polls.
    pub fn publish(&self, update: &TemplateUpdate) {
        let mut s = self.lock();
        let template = update.template().clone();
        s.store.insert(template.clone());
        s.store.prune(Instant::now());
        // A revert moves the tip back to the parent of a rejected speculative block.
        if let TemplateUpdate::Empty(_)
        | TemplateUpdate::Full(_)
        | TemplateUpdate::Reverted { .. } = update
        {
            s.tip_template_id = template.id;
        }
        s.current = Some(template);
        drop(s);
        self.changed.notify_all();
    }

    pub fn current(&self) -> Option<Arc<StoredTemplate>> {
        self.lock().current.clone()
    }

    pub fn get(&self, id: u64) -> Option<Arc<StoredTemplate>> {
        self.lock().store.get(id).cloned()
    }

    /// Runs `f` over the submission store (for `rebuild_block`).
    pub fn with_store<T>(&self, f: impl FnOnce(&TemplateStore) -> T) -> T {
        f(&self.lock().store)
    }

    /// Blocks until a template newer than `since` is available. It returns at once after a
    /// tip event (the coinbase-only template, then the full one). It returns after `set_delay`
    /// when only the transaction set changed. It returns at `max_wait` in all cases, with the
    /// current template. Returns `None` only when no template exists yet.
    pub fn wait_for_newer(
        &self,
        since: u64,
        set_delay: Duration,
        max_wait: Duration,
    ) -> Option<(Arc<StoredTemplate>, Wake)> {
        let start = Instant::now();
        let deadline = start + max_wait;
        let mut s = self.lock();
        // The tip of the template that the caller holds. If the tip is different now, the
        // work of the caller is already stale, and the caller must not wait.
        let since_tip = s.store.get(since).map(|t| t.tip);
        loop {
            let current = s.current.clone()?;
            let tip_changed = match since_tip {
                Some(tip) => tip != current.tip,
                None => true,
            };
            // Tip events publish a coinbase-only template and then the full one. Both wake
            // the poll at once, and `submitold` says whether the tip itself moved.
            if tip_changed || since < s.tip_template_id {
                return Some((current, if tip_changed { Wake::Tip } else { Wake::Set }));
            }
            let now = Instant::now();
            if current.id != since && now >= start + set_delay {
                return Some((current, Wake::Set));
            }
            if now >= deadline {
                return Some((current, Wake::Timeout));
            }
            let next = if current.id != since {
                (start + set_delay).min(deadline)
            } else {
                deadline
            };
            let (guard, _) = self
                .changed
                .wait_timeout(s, next.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
            s = guard;
        }
    }
}
