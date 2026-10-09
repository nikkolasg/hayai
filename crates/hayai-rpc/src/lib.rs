//! `getblocktemplate`/`submitblock` compatibility shim over the live template
//! (`docs/protocol-template-push.md`, section getblocktemplate compatibility).
//!
//! - [`TemplateFeed`]: the node publishes every `TemplateUpdate` here. The RPC serves the
//!   current template without a rebuild. Long polls wait on the next update.
//! - [`Rpc`]: JSON-RPC 1.0/2.0 dispatch with zcashd's result shapes and error codes.
//! - [`HttpServer`]: the HTTP/1.1 front end that pool software connects to.
//! - [`index`]: the methods of the wallet index (`getrawtransaction`, `gettxout`,
//!   `getaddressbalance`, `getaddresstxids`, `getaddressutxos`, `z_getsubtreesbyindex`).
//! - The HTTP front end reads and writes with `hayai_http`, whose `cookie` module is the
//!   authentication, as Zakura.
//! - [`BlockSubmitSink`], [`TipSource`], [`NodeQuery`] and, on test networks,
//!   [`BlockGenerator`]: the parts that the node supplies.
//! - The request counters of [`Rpc`] are in a `hayai_metrics::Registry`; the `/metrics`
//!   endpoint is `hayai_metrics::MetricsServer`.

#![forbid(unsafe_code)]

pub mod feed;
pub mod http;
pub mod index;
pub mod info;
pub mod rpc;
pub mod template;

pub use feed::{TemplateFeed, Wake};
pub use http::HttpServer;
pub use rpc::{
    AddressUtxo, BlockGenerator, BlockInfo, BlockState, BlockSubmitSink, ChainTip, IndexError,
    NodeQuery, NodeState, PeerRow, Pools, Rpc, RpcConfig, SubmitOutcome, SubmittedBlock,
    SubtreePool, SubtreeRow, TipSource, TipState, TransparentAddress, TxOutInfo,
};
pub use template::{block_template, BlockTemplate, TransactionTemplate};
