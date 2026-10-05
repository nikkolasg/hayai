//! The `parse_block` sysbench scenario: hayai-wire's boundary scan and parallel parse with
//! retained bytes against zakura-chain's deserialize, hash and serialize round trip
//! (`crate::zakura_wire::parse_round_trip`), the same bodies as `benches/wire.rs`.

use hayai_wire::RawBlock;

use super::{Built, Impl};
use crate::fixtures::standard_set;
use crate::zakura_wire;

pub fn build(fixture: &str, imp: Impl) -> Result<Built, String> {
    let Some(f) = standard_set().into_iter().find(|f| f.name == fixture) else {
        return Err(format!("no fixture named {fixture}"));
    };
    let txs = f.parse().txs.len();
    Ok(match imp {
        Impl::Hayai => Built::new(move |m| {
            let block = m.timed(|| RawBlock::parse(f.bytes.clone(), f.branch_id).unwrap());
            assert_eq!(block.txs.len(), txs);
        }),
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => Built::new(move |m| {
            let (txids, _, _) = m.timed(|| zakura_wire::parse_round_trip(&f.bytes));
            assert_eq!(txids.len(), txs);
        }),
    })
}
