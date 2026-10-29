//! Parsers of single JSON-RPC answers and chain-tool output: an
//! `eth_getLogs` log, a stored sent transaction and receipt, a deployment
//! manifest and its pay-to pin, a draw record, store pointers, `cast`
//! output, and the JSON-RPC envelope itself (`HttpJsonRpc`). The first byte
//! picks the parser; the rest is the answer.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::roots::{parse_bounded, HttpJsonRpc, JsonRpc, MAX_RPC_VALUES};
use onyx_fuzz_common::{block_on, FixedPost, CONTRACT};
use serde_json::json;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    if sel % 8 == 7 {
        // The JSON-RPC envelope: id, error excerpt, result.
        let rpc = HttpJsonRpc::new("http://node.test", FixedPost(200, rest.to_vec()));
        let _ = block_on(rpc.call("eth_chainId", json!([])));
        let rpc = HttpJsonRpc::new("http://node.test", FixedPost(500, rest.to_vec()));
        let _ = block_on(rpc.call("eth_chainId", json!([])));
        return;
    }
    if sel % 8 == 6 {
        let Ok(s) = std::str::from_utf8(rest) else {
            return;
        };
        let _ = onyx_coordinator::cast_chain::parse_commit_log(s);
        let _ = onyx_coordinator::cast_chain::parse_log_block(s);
        let _ = onyx_coordinator::cast_chain::parse_addr(s);
        return;
    }
    let Ok(v) = parse_bounded(rest, MAX_RPC_VALUES) else {
        return;
    };
    match sel % 8 {
        0 => {
            for (from, to) in [(0, u64::MAX), (100, 200), (u64::MAX, 0)] {
                if let Ok(l) = onyx_api::indexer::parse_log(&v, &CONTRACT, from, to) {
                    assert!(
                        from <= l.block && l.block <= to,
                        "log outside the asked range"
                    );
                }
            }
        }
        1 => {
            if let Some(s) = onyx_api::evm_tx::Sent::from_json(&v) {
                let back = onyx_api::evm_tx::Sent::from_json(&s.to_json());
                assert!(back == Some(s), "sent transaction round trip");
            }
            if let Some(r) = onyx_api::receipts::Receipt::from_json(&v) {
                assert_eq!(
                    onyx_api::receipts::Receipt::from_json(&r.to_json()),
                    Some(r),
                    "receipt round trip"
                );
            }
        }
        2 => {
            let _ = onyx_api::client::PayToPin::from_manifest(&v);
        }
        3 => {
            use onyx_node::config::Env;
            for env in [Env::Dev, Env::Testnet] {
                for allow in [false, true] {
                    let _ = onyx_node::manifest::Deployment::from_json(&v, env, allow);
                }
            }
        }
        4 => {
            if let Ok(d) = onyx_coordinator::observe::DrawRecord::from_json(&v) {
                let _ = d.to_json();
            }
        }
        _ => {
            let _ = onyx_api::observations::ObservationStore::onchain_of(&v);
            let _ = onyx_api::observations::ObservationStore::tag_of(&v);
        }
    }
});
