//! `getblocktemplate`/`submitblock` compatibility shim over the live template
//! (`docs/protocol-template-push.md`, section getblocktemplate compatibility).
//!
//! - [`TemplateFeed`]: the node publishes every `TemplateUpdate` here. The RPC serves the
//!   current template without a rebuild. Long polls wait on the next update.
//! - [`Rpc`]: JSON-RPC 1.0/2.0 dispatch with zcashd's result shapes and error codes.
//! - [`HttpServer`]: the HTTP/1.1 front end that pool software connects to.
//! - [`cookie`]: the cookie authentication of the HTTP front end, as Zakura.
//! - [`BlockSubmitSink`], [`TipSource`], [`NodeQuery`] and, on test networks,
//!   [`BlockGenerator`]: the parts that the node supplies.
//! - [`metrics`]: the Prometheus registry and the `/metrics` endpoint ([`MetricsServer`]).

#![forbid(unsafe_code)]

pub mod cookie;
pub mod feed;
pub mod http;
pub mod info;
pub mod metrics;
pub mod rpc;
pub mod template;

pub use cookie::Cookie;
pub use feed::{TemplateFeed, Wake};
pub use http::HttpServer;
pub use metrics::{Counter, FloatCounter, Gauge, Histogram, MetricsServer, Registry};
pub use rpc::{
    BlockGenerator, BlockInfo, BlockState, BlockSubmitSink, ChainTip, NodeQuery, NodeState,
    PeerRow, Pools, Rpc, RpcConfig, SubmitOutcome, SubmittedBlock, TipSource, TipState,
};
pub use template::{block_template, BlockTemplate, TransactionTemplate};
