//! The `exact` EVM precheck on a decoded payment (no base64 layer, so the
//! fuzzer works on the JSON): `{"payload": {...}, "accepted": {...}}`. An
//! accepted payment must be signed by its `from` over the EIP-712 digest of
//! exactly the requirement it was checked against.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::x402::{exact_evm, PaymentRequirements};
use onyx_fuzz_common::{requirements, API_NOW};
use serde_json::Value;

fn check(payload: &Value, req: &PaymentRequirements, now: u64) {
    if let Ok(auth) = exact_evm::precheck(payload, req, now) {
        assert!(auth.valid_after < now && now < auth.valid_before, "window");
        assert_eq!(req.amount.parse::<u128>().ok(), Some(auth.value), "amount");
        let sig = payload["signature"]
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .expect("an accepted payload has a hex signature");
        let digest = exact_evm::digest(&auth, req).expect("an accepted payload has a digest");
        assert_eq!(exact_evm::recover(&digest, &sig), Some(auth.from), "signer");
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(v) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let fixed = requirements();
    for now in [0, 1, API_NOW, 2_000_000_000, u64::MAX] {
        check(&v["payload"], &fixed, now);
    }
    if let Ok(req) = serde_json::from_value::<PaymentRequirements>(v["accepted"].clone()) {
        let _ = exact_evm::chain_id(&req.network);
        check(&v["payload"], &req, API_NOW);
        check(&v["payload"], &req, 2_000_000_000);
    }
});
