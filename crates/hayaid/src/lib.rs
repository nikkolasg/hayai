//! The hayai node: configuration, the Regtest full node and the Testnet shadow follower.
//!
//! Contract: `docs/hayaid.md`. [`Node::start`] assembles hayai-net, the prepared store,
//! hayai-validate with hayai-state, hayai-blockstore, hayai-template, hayai-rpc and
//! hayai-trace; `hayaid start` runs it until SIGINT or SIGTERM.

#![forbid(unsafe_code)]

pub mod backing;
pub mod config;
pub mod headers;
pub mod mempool;
pub mod metrics;
pub mod mining;
pub mod node;
pub mod params;
pub mod persist;
pub mod process;
pub mod query;
pub mod shadow;
pub mod sync;
pub mod upstream;

#[cfg(test)]
mod shadow_tests;
#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod test_support;

pub use config::{default_toml, Config, Mode};
pub use node::{Node, NodeError, TipWatch, PREBUILD_INTERVAL};
pub use params::NetworkKind;
