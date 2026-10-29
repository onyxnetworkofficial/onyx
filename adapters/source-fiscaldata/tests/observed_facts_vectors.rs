//! Rust <-> Solidity vectors for `OnyxObservedFacts.resultHash` (NS11,
//! ADR-0042 amendment 2): the level-2 result hash a market settles from,
//! rebuilt on chain from (spec id, round id, number). The first case is the
//! recorded Treasury round's own settlement; the others pin the i128
//! little-endian encoding (zero, negative, the extremes).
//! `contracts/test/OnyxObservedFacts.t.sol` (`OnyxObservedFactsVectorsTest`)
//! reads the same file. Regenerate only with an ADR change:
//! `ONYX_BLESS=1 cargo test -p onyx-source-fiscaldata --test observed_facts_vectors`.

use onyx_coordinator::observe::{result_hash_of, ObservationOutcome, RecordedDraw};
use onyx_core::{keccak256, Answer};
use onyx_source_fiscaldata::round::{recorded_draw, run, store, Page};
use onyx_source_fiscaldata::{recorded, RECORDED_WINDOW_START};
use serde_json::json;

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/markets/observed-facts.json"
);

fn hex(h: &[u8]) -> String {
    let mut s = String::from("0x");
    for b in h {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[test]
fn observed_fact_vectors_match_the_file() {
    let rec = recorded();
    let pages: Vec<Page> = rec
        .fetches
        .into_iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body),
        })
        .collect();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store(),
        &mut RecordedDraw::new(recorded_draw()),
    )
    .expect("recorded round");
    let ObservationOutcome::Value(Answer::Number(v)) = r.settlement.outcome().clone() else {
        panic!("the recorded round settles a number");
    };
    let mut cases = vec![(r.spec.id(), r.round_id, v, r.settlement.result_hash())];
    for (i, n) in [0i128, -1, 283, -4_567_890_123, i128::MAX, i128::MIN]
        .into_iter()
        .enumerate()
    {
        let spec = keccak256(format!("spec {i}").as_bytes());
        let round = keccak256(format!("round {i}").as_bytes());
        let out = ObservationOutcome::Value(Answer::Number(n));
        cases.push((spec, round, n, result_hash_of(&spec, &round, &out)));
    }
    let doc = json!({
        "specs": cases.iter().map(|c| hex(&c.0 .0)).collect::<Vec<_>>(),
        "rounds": cases.iter().map(|c| hex(&c.1 .0)).collect::<Vec<_>>(),
        "values": cases.iter().map(|c| c.2.to_string()).collect::<Vec<_>>(),
        "results": cases.iter().map(|c| hex(&c.3 .0)).collect::<Vec<_>>(),
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(PATH, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(PATH)
        .expect("fixtures/markets/observed-facts.json missing; run with ONYX_BLESS=1");
    assert_eq!(
        on_disk, text,
        "observed-fact vectors drifted from ADR-0042 amendment 2"
    );
}
