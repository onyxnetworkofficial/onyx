//! Components that read a JSON-RPC node the operator or client does not
//! control, against a node scripted by the input (`{"method": [answers]}`,
//! see `MethodRpc`): the chain indexer's polls, the root source, the
//! client's own-node check, payment arrival and settle checks, the
//! transaction sender, contributor reads and registry levels. The first byte
//! picks the component. Nothing may panic or loop without bound.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::arrival::{Cursor, Payment, Span};
use onyx_api::roots::{parse_bounded, RootSource, MAX_RPC_VALUES};
use onyx_coordinator::cas::{CasExt, MemCas};
use onyx_fuzz_common::{block_on, fixture_rows, held_spec, secret, MethodRpc, CHAIN, CONTRACT};
use std::sync::{Arc, RwLock};
use std::time::Duration;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(v) = parse_bounded(rest, MAX_RPC_VALUES) else {
        return;
    };
    let rpc = MethodRpc::new(&v);
    let (spec, rights) = held_spec();
    let shard = spec.shard_id();
    let payment = Payment {
        token: [0x33; 20],
        revenue: [0x22; 20],
        from: [0x44; 20],
        nonce: [0x55; 32],
        value: 1_000,
    };
    let _ = block_on(async {
        match sel % 8 {
            0 => {
                let cas = Arc::new(MemCas::new());
                let _ = cas.put_rows(&fixture_rows());
                let mut held = onyx_api::indexer::Holdings::new();
                if let Some(h) = onyx_api::indexer::HeldShard::from_spec(&spec, "test.flow", rights)
                {
                    let _ = held.insert(h);
                }
                let mut cfg = onyx_api::indexer::IndexerConfig::new(CHAIN, CONTRACT, 100);
                cfg.rescan_every = 2;
                if let Ok(idx) = onyx_api::indexer::Indexer::open(cfg, rpc, held, cas, None) {
                    let catalog = RwLock::new(onyx_api::catalog::Catalog::new());
                    for _ in 0..4 {
                        let _ = idx.poll(&catalog).await;
                    }
                    let _ = idx.known();
                    let _ = idx.events();
                    let _ = idx.check_deploy_block_with(&[]).await;
                }
            }
            1 => {
                if let Ok(r) =
                    onyx_api::roots::ChainRoots::connect(CONTRACT, CHAIN, None, rpc).await
                {
                    let _ = r.finalized(1, &shard).await;
                    let _ = r.finalized(u64::MAX, &shard).await;
                }
            }
            2 => {
                let node = onyx_api::client::ClientNode::new(rpc, CHAIN, CONTRACT);
                let _ = node.answer(1, shard).await;
            }
            3 => {
                let _ = onyx_api::arrival::head_time(&rpc).await;
                let _ = onyx_api::arrival::use_of(&rpc, &payment, 0, u64::MAX).await;
                let _ = onyx_api::arrival::logged_use(&rpc, &payment, 1_000, 2_000).await;
                let mut cursor = Cursor::default();
                let mut budget = 8;
                let span = Span {
                    valid_after: 1_000,
                    earliest: 1_000,
                    valid_before: 2_000,
                };
                for _ in 0..3 {
                    let _ = onyx_api::arrival::judge(
                        &rpc,
                        &payment,
                        span,
                        60,
                        &mut cursor,
                        &mut budget,
                    )
                    .await;
                }
            }
            4 => {
                if let Some(s) = onyx_api::evm_tx::Sender::new(rpc, CHAIN, secret("operator")) {
                    let s = s.with_poll(Duration::from_millis(10));
                    let _ = s.nonce_at("pending").await;
                    let _ = s.call(&CONTRACT, &[1, 2, 3]).await;
                    let _ = s.mined_now(&["0x01".into()]).await;
                    let _ = s
                        .send(&CONTRACT, &[1, 2, 3], 0, Duration::from_secs(5))
                        .await;
                }
            }
            5 => {
                use onyx_api::contrib::ContributorSource;
                let c = onyx_api::contrib::ChainContributors::new([0x22; 20], 100, rpc);
                let _ = c.is_contributor([0x44; 20], 1_000).await;
                let _ = c.is_contributor([0x45; 20], 1_000).await;
            }
            6 => {
                use onyx_api::settle_check::SettleCheck;
                let c = onyx_api::settle_check::ChainSettleCheck::new(rpc, [0x33; 20], [0x22; 20]);
                let _ = c.use_of([0x44; 20], [0x55; 32], 1_000, 0, u64::MAX).await;
                let _ = c
                    .logged_use([0x44; 20], [0x55; 32], 1_000, 1_000, 2_000)
                    .await;
            }
            _ => {
                let _ = onyx_api::levels::check_registry_code(
                    &rpc,
                    [0x66; 20],
                    onyx_core::Hash32::ZERO,
                )
                .await;
                let z = onyx_core::Hash32::ZERO;
                let _ = onyx_api::levels::registry_level(
                    &rpc,
                    [0x66; 20],
                    1,
                    &shard,
                    "test.flow",
                    &z,
                    &z,
                )
                .await;
            }
        }
    });
});
