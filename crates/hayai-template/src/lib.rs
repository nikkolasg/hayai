//! Live block template with incremental updates and push delivery.
//!
//! Contract: `docs/protocol-template-push.md` (messages, procedures) and
//! `docs/architecture.md`, section hayai-template.
//!
//! - [`Candidate`] and [`CandidateSource`]: what the prepared store hands over, and the
//!   [`SetEvent`] stream that keeps the template current.
//! - [`LiveTemplate`]: the ordered candidate set, the dependency graph and the deterministic
//!   greedy selection under the block limits. `apply` handles set events. `on_tip` handles tip
//!   events.
//! - [`messages`]: the protocol messages with binary-frame and JSON-lines encodings, the
//!   crate `hayai-template-messages`: a miner decodes the push without the template.
//! - [`Publisher`]: fan-out to subscribers with coalescing and the slow-subscriber rule.
//! - [`submission`]: the rebuild of a block from a `Submit` message and a stored template.
//! - [`zip317`]: fee parameters, logical actions and the fixed-point weight ratio.

#![forbid(unsafe_code)]

pub mod candidate;
pub mod coinbase;
pub mod live;
/// The protocol messages (`hayai-template-messages`).
pub use hayai_template_messages as messages;
pub mod publisher;
pub mod submission;
#[cfg(test)]
mod test_support;
pub mod zip317;

pub use candidate::{Candidate, CandidateSource, OrderKey, SetEvent};
pub use coinbase::{CoinbaseSpec, CoinbaseTx};
pub use live::{ApplyError, LiveTemplate, StoredTemplate, TemplateConfig, TemplateUpdate, Tip};
pub use publisher::{Publisher, SubscriberId};
pub use submission::{RebuiltBlock, SubmitError, TemplateStore};
pub use zip317::{WeightRatio, Zip317Params};
