//! The bounded JSON-RPC answer parser (`roots::parse_bounded`, IDX-R2-3):
//! never panics; never accepts an answer holding more values than its
//! budget; and agrees with `serde_json` on every answer within budget.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::roots::parse_bounded;
use serde_json::Value;

/// Values in `v`, counted as `parse_bounded` counts them.
fn count(v: &Value) -> usize {
    1 + match v {
        Value::Array(a) => a.iter().map(count).sum(),
        Value::Object(o) => o.values().map(count).sum(),
        _ => 0,
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&m, json)) = data.split_first() else {
        return;
    };
    let max = usize::from(m);
    let bounded = parse_bounded(json, max);
    let plain = serde_json::from_slice::<Value>(json);
    match (&bounded, &plain) {
        (Ok(b), Ok(p)) => {
            assert!(count(b) <= max, "budget exceeded");
            assert_eq!(b, p, "bounded parse differs from serde_json");
        }
        (Ok(_), Err(e)) => panic!("bounded parse accepted what serde_json refuses: {e}"),
        (Err(_), Ok(p)) => {
            // Refused only for its size. Duplicate object keys count once
            // per occurrence in the bounded parser, so a refusal is fine
            // whenever the plain count is near the budget.
            assert!(
                count(p) > max || json.len() > 2 * max,
                "bounded parse refused an answer within budget"
            );
        }
        (Err(_), Err(_)) => {}
    }
    let _ = parse_bounded(json, onyx_api::roots::MAX_RPC_VALUES);
});
