//! The buyer's check of a served Treasury value (PRODUCT-PLAN.md U5, pre-public loop PP4).
//!
//! `cargo run -p onyx-source-fiscaldata --example check-observation -- BODY.json RECORD_DATE`
//!
//! Reads a `GET /v1/observations/us.treasury.total_public_debt_outstanding?subject=...` answer and
//! checks it with `onyx_api::client::verify_observation` and THIS adapter's own template (the
//! buyer's copy, never one the server sends): the observer draw, every commit and reveal, the
//! aggregation, the result hash and every derived field. The freshness floor is the record
//! date's recorded window start; the clock is this machine's. It reads no chain: with a node, a
//! buyer also asks `OnyxRegistry.observationLevel` (`verify_observation_onchain`). Prints the
//! checked value and evidence as JSON; exits non-zero on any refusal.

use onyx_api::client::{verify_observation, ObservationExpect};
use onyx_core::Subject;
use onyx_source_fiscaldata::{subject, template, RECORDED_WINDOW_START};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [body, date] = args.as_slice() else {
        return Err("usage: check-observation BODY.json RECORD_DATE".into());
    };
    let body: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(body)?)?;
    let t = template();
    let subj = Subject::Aggregate(subject(date));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let v = verify_observation(
        &body,
        &ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: RECORDED_WINDOW_START,
            now,
        },
    )
    .map_err(|e| format!("refused: {e:?}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "checked": true,
            "level": 2,
            "value": v.value,
            "roundId": v.checked.round_id.to_string(),
            "result": v.checked.result.to_string(),
            "chainChecked": false,
            "note": "checked off chain with the buyer's own template; with a node, also ask OnyxRegistry.observationLevel"
        }))?
    );
    Ok(())
}
