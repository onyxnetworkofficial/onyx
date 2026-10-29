//! The client's check of a server's answer (`client::verify_response`):
//! the whole response body is untrusted. The first byte picks what the
//! client asked (proof, export, metric, settlement) and whether it holds
//! its own node's answer. Never panics; an accepted answer's rows are the
//! ones the root commits to.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::client::{verify_response, verify_response_unchecked};
use onyx_api::roots::{parse_bounded, MAX_RPC_VALUES};
use onyx_fuzz_common::{chain_answer, expects, fixture_ids, fixture_rows};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(body) = parse_bounded(rest, MAX_RPC_VALUES) else {
        return;
    };
    let (shard, root, _) = fixture_ids();
    let ex = &expects(shard)[usize::from(sel % 4)];
    let _ = verify_response_unchecked(&body, ex);
    let answer = chain_answer();
    if let Ok(v) = verify_response(&body, ex, Some(&answer)) {
        // The client's own node vouches for this root: every row the body
        // carries must be one of the shard's rows.
        assert_eq!(
            (v.round, v.shard, v.root),
            (1, shard, root),
            "accepted another shard"
        );
        assert_eq!(v.binding, answer.binding, "accepted another binding");
        let rows = fixture_rows();
        let hex_row = |x: &serde_json::Value| {
            x.as_str()
                .and_then(|h| hex::decode(h.trim_start_matches("0x")).ok())
        };
        let mut served: Vec<Vec<u8>> = body.get("row").and_then(hex_row).into_iter().collect();
        if let Some(a) = body.get("rows").and_then(|r| r.as_array()) {
            served.extend(a.iter().filter_map(hex_row));
        }
        for r in &served {
            assert!(
                rows.contains(r),
                "accepted a row the root does not commit to"
            );
        }
        let _ = v.require_cross_checked();
        let _ = v.chain_call();
    }
    let _ = verify_response(&body, ex, None);
});
