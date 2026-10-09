//! Header chain, block download and peer scoring.
//!
//! - [`headers`]: the fork-aware header chain. It validates each header without its body.
//! - [`index`]: the best chain as headers, the context of the relay's header check.
//! - [`locator`]: the heights of a block locator.
//! - [`store`]: the header log, the full headers on disk.
//! - [`score`]: the misbehaviour score of each peer.
//! - [`download`]: the scheduler of the block download.
//!
//! The design is in `docs/plan-consensus-and-sync.md`, work package B.

#![forbid(unsafe_code)]

pub mod download;
pub mod headers;
pub mod index;
pub mod locator;
pub mod score;
pub mod store;
