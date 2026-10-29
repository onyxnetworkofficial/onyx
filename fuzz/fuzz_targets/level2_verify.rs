//! The client's check of a level-2 answer (`verify_observation`,
//! `verify_attempts`): the server's whole transcript (draw, commits,
//! reveals, signatures, outcome) is untrusted and rebuilt with the client's
//! own template. The first byte picks the route. Never panics; an accepted
//! value is for the subject asked.
#![no_main]

use libfuzzer_sys::fuzz_target;
use onyx_api::client::{verify_attempts, verify_observation, ObservationExpect};
use onyx_api::roots::{parse_bounded, MAX_RPC_VALUES};
use onyx_fuzz_common::level2;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(body) = parse_bounded(rest, MAX_RPC_VALUES) else {
        return;
    };
    let t = level2::template();
    let subject = level2::subject();
    let expect = ObservationExpect {
        template: &t,
        subject: &subject,
        window_end: None,
        min_window_end: onyx_source_fiscaldata::RECORDED_WINDOW_START,
        now: level2::NOW,
    };
    if sel % 2 == 0 {
        if let Ok(v) = verify_observation(&body, &expect) {
            assert_eq!(v.checked.spec.subject, subject, "accepted another subject");
            let calls = v.onchain_calls();
            let sealed = v.sealed_calls();
            let _ = onyx_api::client::verify_observation_onchain(
                &v, &calls[0], &calls[1], &calls[2], &calls[3], &sealed,
            );
        }
    } else if let Ok(a) = verify_attempts(&body, &expect) {
        let _ = a.draw_count_calldata();
        let _ = a.transcript_complete_calldatas();
        let _ = onyx_api::client::verify_attempts_onchain(&a, &[0; 32], &[], &[0; 64], &[], &[]);
    }
});
