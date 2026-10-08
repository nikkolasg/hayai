//! Candidate sets and bodies of the template benchmarks (`benches/template.rs`) and of the
//! `template_build_8000` sysbench scenario: one template from N synthetic candidates, hayai's
//! live template (coinbase and roots included) against the port of Zakura's ZIP 317
//! selection.

use bytes::Bytes;
use hayai_consensus::BlockLimits;
use hayai_crypto::{zcash_primitives, zcash_transparent};
use hayai_template::live::MAX_BLOCK_BYTES;
use hayai_template::zip317::BLOCK_UNPAID_ACTION_LIMIT;
use hayai_template::{
    Candidate, CoinbaseSpec, LiveTemplate, TemplateConfig, TemplateUpdate, Tip, Zip317Params,
};
use hayai_wire::header::{BlockHash, PowParams};
use hayai_wire::WtxId;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use zcash_primitives::transaction::TxId;
use zcash_transparent::bundle::OutPoint;

use super::{Built, Impl};
use crate::zakura_zip317::{select_mempool_transactions, Limits};

/// The fee values of both sides of the comparison: the values that the node uses.
pub const PARAMS: Zip317Params = Zip317Params::ZAKURA;

pub fn coinbase_spec() -> CoinbaseSpec {
    CoinbaseSpec {
        script_pubkey: vec![0x76, 0xa9, 0x14, 0x11, 0x22, 0x33, 0x88, 0xac],
        miner_data: b"hayai-bench".to_vec(),
        // The Mainnet terms of the template height: the miner output and the founders'
        // reward or the funding stream outputs.
        network: hayai_consensus::Network::Mainnet,
    }
}

pub fn tip(height: u32) -> Tip {
    Tip {
        parent_hash: BlockHash([height as u8; 32]),
        height,
        time: 1_700_000_000 + height,
        median_time_past: 1_700_000_000 + height - 1,
        bits: 0x1c00_ffff,
        history_root: [0x22; 32],
        issued_supply: None,
    }
}

/// `n` candidates: 60 % transparent (250–600 bytes), 40 % shielded (1.5–9 kB with Orchard
/// actions and Sapling I/O), fees spanning the low-fee pass to the weight cap, and 15 % with
/// one earlier candidate as unmined parent.
pub fn candidates(n: usize, seed: u64) -> Vec<Candidate> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut out: Vec<Candidate> = Vec::with_capacity(n);
    for i in 0..n {
        let shielded = rng.gen_bool(0.4);
        let (size, orchard_actions, sapling_ios, logical) = if shielded {
            let actions = rng.gen_range(2..7u32);
            let sapling = if rng.gen_bool(0.3) {
                rng.gen_range(1..5u32)
            } else {
                0
            };
            let size = 300 + actions as usize * 1400 + sapling as usize * 700;
            (size, actions, sapling, actions + sapling)
        } else {
            let outputs = rng.gen_range(1..5u32);
            (
                250 + outputs as usize * 60 + rng.gen_range(0..120),
                0,
                0,
                outputs.max(2),
            )
        };
        let conventional_fee = PARAMS.conventional_fee(logical);
        // Ratio in [0.3, 1.5 times the weight ratio cap): about a tenth fall below the
        // conventional fee, and about a third are above the cap.
        let ratio: f64 = if rng.gen_bool(0.1) {
            rng.gen_range(0.3..1.0)
        } else {
            rng.gen_range(1.0..1.5 * f64::from(PARAMS.weight_ratio_cap))
        };
        let fee = (conventional_fee as f64 * ratio) as u64;
        let mut wtxid = [0u8; 32];
        rng.fill(&mut wtxid);
        let mut auth = [0u8; 32];
        rng.fill(&mut auth);
        let depends_on = if i > 0 && rng.gen_bool(0.15) {
            vec![out[rng.gen_range(0..i)].wtxid]
        } else {
            Vec::new()
        };
        out.push(Candidate {
            wtxid: WtxId {
                txid: TxId::from_bytes(wtxid),
                auth_digest: auth,
            },
            bytes: Bytes::from(vec![0u8; size]),
            fee,
            conventional_fee,
            weight_ratio: PARAMS.weight_ratio(fee, conventional_fee),
            unpaid_actions: PARAMS.unpaid_actions(fee, conventional_fee),
            sigops: rng.gen_range(0..4),
            orchard_actions,
            ironwood_actions: 0,
            sapling_ios,
            // The block order reads the dependencies from the inputs, as the store does.
            spends: depends_on
                .iter()
                .map(|p| OutPoint::new(*p.txid.as_ref(), 0))
                .collect(),
            depends_on,
        });
    }
    out
}

/// The limits that the live template applies at the heights of the benchmarks: the rule
/// set before NU7 and the unpaid action limit of hayai.
pub fn zakura_limits(coinbase_reserved: usize) -> Limits {
    let limits = BlockLimits::PRE_NU7;
    Limits {
        remaining_bytes: MAX_BLOCK_BYTES - PowParams::MAINNET.header_len() - 5 - coinbase_reserved,
        remaining_sigops: limits.sigops,
        remaining_unpaid_actions: BLOCK_UNPAID_ACTION_LIMIT,
        remaining_orchard_actions: limits.orchard_actions,
        remaining_sapling_ios: limits.sapling_ios,
    }
}

pub fn coinbase_reserved() -> usize {
    let cb = coinbase_spec().build(1, 0).unwrap();
    cb.bytes.len() + cb.script_slack
}

pub fn live_with(cands: Vec<Candidate>, height: u32) -> LiveTemplate {
    let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
    live.load(cands).unwrap();
    live.on_tip(tip(height), &[], &[], |_| {}).unwrap();
    live
}

/// The hayai body: a live template over `cands`, number of selected transactions.
pub fn hayai_build(cands: Vec<Candidate>) -> usize {
    live_with(cands, 1).current().unwrap().txs.len()
}

/// The Zakura body: ZIP 317 selection over `cands`, number of selected transactions.
pub fn zakura_build(cands: Vec<Candidate>, reserved: usize, rng: &mut StdRng) -> usize {
    select_mempool_transactions(cands, &PARAMS, zakura_limits(reserved), rng).len()
}

/// `template_build_<n>`: the candidate clone is outside the timed region, as in the
/// criterion bench's `iter_batched`.
pub fn build_from_scratch(n: usize, imp: Impl) -> Built {
    let cands = candidates(n, 7);
    match imp {
        Impl::Hayai => Built::new(move |m| {
            let input = cands.clone();
            let selected = m.timed(|| hayai_build(input));
            assert!(selected > 0);
        }),
        Impl::Zebra => unreachable!("{}", super::NO_ZEBRA),
        Impl::Zakura => {
            let reserved = coinbase_reserved();
            let mut rng = StdRng::seed_from_u64(1);
            Built::new(move |m| {
                let input = cands.clone();
                let selected = m.timed(|| zakura_build(input, reserved, &mut rng));
                assert!(selected > 0);
            })
        }
    }
}

/// Candidates in the template of the `switch_after_block` scenarios.
pub const SWITCH_CANDIDATES: usize = 8_000;

/// `template/switch_after_block`: a node holds a live template of [`SWITCH_CANDIDATES`]
/// synthetic candidates on the parent of a fixture block, and the fixture block arrives. The
/// measured path ends when the full template on the new block exists. The chain has a
/// known history tree, so the layer build runs the commitment rule and the history append.
pub struct Switch {
    harness: crate::chain_fixture::Harness,
    live: LiveTemplate,
    parent: Tip,
}

/// The template tip on top of `view`'s tip, with the history root after that tip.
pub fn tip_on(view: &hayai_state::ChainView, time: u32) -> Tip {
    let tip = view.tip();
    Tip {
        parent_hash: tip.hash,
        height: tip.height + 1,
        time,
        median_time_past: time - 1,
        bits: 0x1c01_0000,
        history_root: view.history().expect("the history tree is known").root(),
        issued_supply: Some(view.value_pools().total()),
    }
}

impl Switch {
    /// `warm`: the prepared store holds every transaction of the block.
    pub fn new(fixture: &hayai_fixtures::Fixture, warm: bool) -> Switch {
        let harness = crate::chain_fixture::harness_with_history(fixture);
        if warm {
            harness.fill_store();
        }
        let parent = tip_on(&harness.chain.view(), harness.block.header.time + 1);
        let mut live = LiveTemplate::new(TemplateConfig::new(coinbase_spec()));
        live.load(candidates(SWITCH_CANDIDATES, 7)).unwrap();
        live.on_tip(parent, &[], &[], |_| {}).unwrap();
        // The first validation builds the Orchard verifying key and fills the coins cache.
        hayai_validate::validate_block(
            harness.block.clone(),
            &harness.store,
            &harness.chain.view(),
            &harness.cfg,
        )
        .expect("valid");
        Switch {
            harness,
            live,
            parent,
        }
    }

    /// Today's path: `validate_block`, the layer push, then `on_tip`. Returns the time to
    /// the full template; the chain and the template return to the parent afterwards.
    pub fn serial(&mut self) -> std::time::Duration {
        let h = &self.harness;
        let started = std::time::Instant::now();
        let (layer, _) =
            hayai_validate::validate_block(h.block.clone(), &h.store, &h.chain.view(), &h.cfg)
                .expect("valid");
        self.push_then_switch(layer, started)
    }

    /// The own-block path of a warm store: the block's body was prebuilt before the block
    /// (outside the measured path); `commit_prebuilt`, the layer push, then `on_tip`.
    pub fn swap(&mut self) -> std::time::Duration {
        let h = &self.harness;
        let ids: Vec<WtxId> = h.block.txs[1..].iter().map(|t| t.wtxid()).collect();
        let body = hayai_validate::prebuild(&ids, &h.store, &h.chain.view(), &h.cfg)
            .expect("the store holds the body");
        let started = std::time::Instant::now();
        let (layer, _) = hayai_validate::commit_prebuilt(&h.block, body, &h.chain.view(), &h.cfg)
            .expect("valid");
        self.push_then_switch(layer, started)
    }

    /// Pushes `layer`, moves the template to it, and returns the time since `started`; the
    /// chain and the template then return to the parent.
    fn push_then_switch(
        &mut self,
        layer: hayai_state::Layer,
        started: std::time::Instant,
    ) -> std::time::Duration {
        let h = &mut self.harness;
        let mined: Vec<WtxId> = layer.wtxids[1..].to_vec();
        h.chain.push(layer).expect("on the tip");
        let tip = tip_on(&h.chain.view(), self.parent.time + 1);
        let mut full = false;
        self.live
            .on_tip(tip, &mined, &[], |u| {
                if let TemplateUpdate::Full(_) = u {
                    full = true;
                }
            })
            .unwrap();
        let took = started.elapsed();
        assert!(full, "on_tip emits the full template");
        h.chain.pop().expect("the block is the tip");
        self.live.on_tip(self.parent, &[], &[], |_| {}).unwrap();
        took
    }

    /// The speculative path: `build_layer`, the speculative push, then
    /// `on_speculative_tip`, while `verify` runs on another thread. Returns the time to the
    /// full template; the verification completes outside the measured path, then the chain
    /// and the template return to the parent.
    pub fn speculative(&mut self) -> std::time::Duration {
        let h = &mut self.harness;
        let parent = self.parent;
        let live = &mut self.live;
        std::thread::scope(|s| {
            let started = std::time::Instant::now();
            let (layer, verification, _) =
                hayai_validate::build_layer(h.block.clone(), &h.store, &h.chain.view(), &h.cfg)
                    .expect("valid");
            let verifying = s.spawn(move || hayai_validate::verify(verification));
            let mined: Vec<WtxId> = layer.wtxids[1..].to_vec();
            let id = h.chain.push_speculative(layer).expect("on the tip");
            let tip = tip_on(&h.chain.view_speculative(), parent.time + 1);
            let mut full = false;
            live.on_speculative_tip(tip, &mined, &[], |u| {
                if let TemplateUpdate::Full(_) = u {
                    full = true;
                }
            })
            .unwrap();
            let took = started.elapsed();
            assert!(full, "on_speculative_tip emits the full template");
            verifying.join().expect("verify thread").expect("valid");
            h.chain.reject(id).expect("speculative");
            live.on_revert(parent, |_| {}).unwrap();
            took
        })
    }
}
