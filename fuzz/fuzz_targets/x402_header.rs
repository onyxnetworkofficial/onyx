//! x402 headers (`payment-signature`, `payment-required`, and the
//! facilitator's JSON): bounded base64 + JSON decoding, then the local
//! EIP-3009 precheck and the mock facilitator on whatever decodes. A decoded
//! value must re-encode to a header that decodes to the same value.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::x402::{
    decode_header, encode_header, exact_evm, Facilitator, FacilitatorRequest, MockFacilitator,
    PaymentPayload, PaymentRequired, SettleResponse, VerifyResponse,
};
use onyx_fuzz_common::{addr, block_on, requirements, API_NOW};

fn roundtrip<T>(v: &T)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let again: T = decode_header(&encode_header(v)).expect("a decoded header re-decodes");
    assert_eq!(&again, v, "header round trip");
}

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(p) = decode_header::<PaymentPayload>(s) {
        if encode_header(&p).len() <= onyx_api::x402::MAX_PAYMENT_HEADER {
            roundtrip(&p);
        }
        for now in [0, API_NOW, u64::MAX] {
            let _ = exact_evm::precheck(&p.payload, &p.accepted, now);
            let _ = exact_evm::precheck(&p.payload, &requirements(), now);
        }
        let req = FacilitatorRequest {
            x402_version: p.x402_version,
            payment_requirements: p.accepted.clone(),
            payment_payload: p,
        };
        let f = MockFacilitator::new([addr("payer a")]);
        let _ = block_on(async {
            let _ = f.verify(&req).await;
            let _ = f.settle(&req).await;
        });
    }
    if let Ok(r) = decode_header::<PaymentRequired>(s) {
        if encode_header(&r).len() <= onyx_api::x402::MAX_PAYMENT_HEADER {
            roundtrip(&r);
        }
    }
    if let Ok(r) = decode_header::<FacilitatorRequest>(s) {
        let _ = exact_evm::precheck(&r.payment_payload.payload, &r.payment_requirements, API_NOW);
    }
    let _ = decode_header::<VerifyResponse>(s);
    let _ = decode_header::<SettleResponse>(s);
});
