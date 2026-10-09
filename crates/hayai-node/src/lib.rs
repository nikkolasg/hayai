//! The hayai node as a library: the full node and the shadow follower.
//!
//! Contract: `docs/hayaid.md`. [`Node::start`] assembles hayai-net, the prepared store,
//! hayai-validate with hayai-state, hayai-blockstore, hayai-template, hayai-rpc and
//! hayai-trace from a [`Config`], and runs until [`Node::shutdown`]. The binary `hayaid`
//! reads the configuration file, the signals and the logs around it.

#![forbid(unsafe_code)]

pub mod config;
pub mod headers;
pub mod mempool;
pub mod metrics;
pub mod mining;
pub mod node;
pub mod params;
pub mod process;
pub mod query;
pub mod sync;

#[cfg(test)]
mod shadow_tests;
#[cfg(test)]
mod sync_tests;
#[cfg(test)]
mod wallet_tests;

pub use config::{default_toml, Config, Mode};
pub use node::{Node, NodeBuilder, NodeError, TipWatch, PREBUILD_INTERVAL};
pub use params::NetworkKind;
