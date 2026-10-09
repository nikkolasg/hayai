//! Compact block relay: messages, short ids, batch lanes, reconstruction and the header
//! check that gates forwarding. The contract is `docs/protocol-compact-relay.md`.

#![forbid(unsafe_code)]

pub mod batch;
pub mod candidate;
pub mod compact;
pub mod header_check;
pub mod message;
pub mod short_id;

#[cfg(test)]
pub(crate) mod test_util;

pub use batch::{Batch, BatchId, BatchStatus, LaneError, LaneStore};
pub use candidate::{
    expand, CandidateError, CandidateStore, LanePublisher, Publication, ResolvedCandidate,
    MAX_CANDIDATE_BATCHES,
};
pub use compact::{
    reconstruct, resolve, resolve_candidate, CandidatePartial, CompactBuilder, Entry, IdCheck,
    IdForm, Partial, ReconstructError, MAX_BLOCK_TXS,
};
pub use header_check::{
    HeaderCheck, HeaderContext, HeaderError, ParentInfo, StandardHeaderCheck, Verified,
};
pub use message::{
    decode, decode_payload, encode, BatchAnnounce, BatchRequest, Block, BlockTxn, BlockTxnRequest,
    CandidateAnnounce, CandidateBlock, CompactBlock, DecodeError, FullId, LaneId, Message,
    PrefilledTx, Tx, TxAnnounce, TxRequest, CANONICAL_ORDER, MAX_PAYLOAD,
};
pub use short_id::{short_id, ShortId, ShortIdIndex, ShortIdKey};
