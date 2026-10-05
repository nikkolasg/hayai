//! The chain context of a case: what both implementations know before the block.

use serde::{Deserialize, Serialize};

/// The network of a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Net {
    Mainnet,
    Testnet,
}

/// A shielded pool, for a nullifier of the context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShieldedPool {
    Orchard,
    Ironwood,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutPoint {
    pub hash: [u8; 32],
    pub index: u32,
}

/// An unspent transparent output of the chain before the block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coin {
    pub value: u64,
    #[serde(with = "hex::serde")]
    pub script: Vec<u8>,
    pub height: u32,
    pub coinbase: bool,
}

/// The state of the chain at the parent of the block of a case.
#[derive(Clone, Debug)]
pub struct Context {
    pub network: Net,
    /// The height of the block of the case: the parent is at `height - 1`.
    pub height: u32,
    /// The hash of the parent block.
    pub parent: [u8; 32],
    pub coins: Vec<(OutPoint, Coin)>,
    /// Nullifiers that the chain revealed before the block.
    pub nullifiers: Vec<(ShieldedPool, [u8; 32])>,
    /// The root of the ZIP 221 history tree of the parent, when the case has one. With
    /// `None`, no implementation checks the commitments field of the header.
    pub history_root: Option<[u8; 32]>,
}

/// The largest amount of money: 21 million ZEC in zatoshis.
pub const MAX_MONEY: u64 = 2_100_000_000_000_000;

/// The deferred pool of the chain of every case, in zatoshis: more than the lockbox
/// disbursement of the NU6.1 activation block (78,750 ZEC) takes out of it.
pub const DEFERRED_POOL: u64 = 10_000_000_000_000;

/// The largest value of a coin of a context. The chain value pools of a context hold at
/// most the money limit, as on a real chain: the oracle has no rule for the total of the
/// pools, and a context above the limit gives a reject of hayai that says nothing.
pub const MAX_COIN: u64 = MAX_MONEY - 4 * DEFERRED_POOL;
