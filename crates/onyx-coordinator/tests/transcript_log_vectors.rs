//! Rust <-> Solidity vectors of the level-2 transcript log (ADR-0027
//! amendment 8, SIG-R1-F1): the commit and receipt texts the registry
//! rebuilds, the log key, real signatures it must accept, and
//! `transcriptComplete` calldata with the answer Rust's simulated log gives.
//! `contracts/test/OnyxRegistryTranscriptLog.t.sol` replays the file.
//! Regenerate with `ONYX_BLESS=1` only with an ADR change.

use onyx_coordinator::observe::{
    observation_commit_message, reveal_receipt_message, sign_commit, sign_reveal_receipt,
    test_keys, transcript_log_key, SimulatedTranscriptLog, TranscriptEntry, TranscriptLogQuery,
};
use onyx_core::onchain::registry_fact_key;
use onyx_core::{keccak256, Hash32};
use serde_json::json;

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/registry/transcript-log.json"
);

fn hex(b: &[u8]) -> String {
    format!("0x{}", ::hex::encode(b))
}

#[test]
fn transcript_log_vectors_match_the_contract_file() {
    // The forge fixture's feed: `FEED_METRIC`, `FEED_TEMPLATE`, `SUBJECT`, `WINDOW`.
    let fact = registry_fact_key(
        &keccak256(b"example.fixed.feed"),
        &keccak256(b"feed template"),
        &keccak256(b"subject"),
        1_000_000,
        false,
    );
    let round = keccak256(b"vector round");
    let (open, close) = (1_000u64, 2_000u64);
    let keys: Vec<_> = (1..=3).map(test_keys::ordered).collect();
    let commits: Vec<Hash32> = (1..=3u8).map(|i| keccak256(&[b'c', i])).collect();
    let commit_sigs: Vec<[u8; 65]> = keys
        .iter()
        .zip(&commits)
        .map(|((sk, _), c)| sign_commit(sk, &round, 0, c).expect("sign"))
        .collect();
    let receipt = sign_reveal_receipt(
        &test_keys::receipt_secret(),
        &round,
        0,
        &keys[0].1,
        &commits[0],
    )
    .expect("sign");
    // The log after observers 1 and 2 post their commits inside the commit
    // phase and observer 1 posts its receipt.
    let mut log = SimulatedTranscriptLog::default();
    log.signers.insert((fact, 0), test_keys::receipt_signer());
    for i in 0..2 {
        log.post_commit(
            &fact,
            0,
            &keys[i].1,
            &round,
            &commits[i],
            &commit_sigs[i],
            1_500,
        )
        .expect("post commit");
    }
    log.post_receipt(&fact, 0, &keys[0].1, &round, &commits[0], &receipt)
        .expect("post receipt");
    let full = TranscriptLogQuery {
        registry_fact: fact,
        attempt: 0,
        round_id: round,
        commit_open: open,
        commit_close: close,
        entries: keys
            .iter()
            .zip(&commits)
            .map(|((_, a), c)| TranscriptEntry {
                observer: *a,
                commit: *c,
                revealed: true,
            })
            .collect(),
    };
    let edit = |f: &dyn Fn(&mut TranscriptLogQuery)| {
        let mut q = full.clone();
        f(&mut q);
        q
    };
    let cases = [
        ("full", full.clone()),
        (
            "observer 1's receipted reveal dropped",
            edit(&|q| q.entries[0].revealed = false),
        ),
        (
            "observer 2's posted commit dropped",
            edit(&|q| {
                q.entries[1].commit = Hash32::ZERO;
                q.entries[1].revealed = false;
            }),
        ),
        (
            "observer 2's commit swapped",
            edit(&|q| q.entries[1].commit = keccak256(b"other")),
        ),
        (
            "observer 3 (nothing posted) dropped",
            edit(&|q| {
                q.entries[2].commit = Hash32::ZERO;
                q.entries[2].revealed = false;
            }),
        ),
        (
            "commit phase before the posts",
            edit(&|q| {
                q.commit_open = 1_600;
                q.commit_close = 1_700;
                q.entries[1].commit = Hash32::ZERO;
                q.entries[1].revealed = false;
                q.entries[0].revealed = true;
            }),
        ),
        ("another attempt", edit(&|q| q.attempt = 1)),
    ];
    let doc = json!({
        "fact": hex(&fact.0),
        "round": hex(&round.0),
        "commitOpen": open,
        "commitClose": close,
        "postedAt": 1_500,
        "receiptSigner": hex(&test_keys::receipt_signer()),
        "observers": keys.iter().map(|(_, a)| hex(a)).collect::<Vec<_>>(),
        "commits": commits.iter().map(|c| hex(&c.0)).collect::<Vec<_>>(),
        "commitSigs": commit_sigs.iter().map(|s| hex(s)).collect::<Vec<_>>(),
        "receiptSig": hex(&receipt),
        "commitMessage": hex(observation_commit_message(&round, 0, &keys[0].1, &commits[0]).as_bytes()),
        "receiptMessage": hex(reveal_receipt_message(&round, 0, &keys[0].1, &commits[0]).as_bytes()),
        "logKey": hex(&transcript_log_key(&fact, 0, &keys[0].1, &round).0),
        "caseNames": cases.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "caseCalldata": cases.iter().map(|(_, q)| hex(&q.calldata())).collect::<Vec<_>>(),
        "caseExpect": cases.iter().map(|(_, q)| log.complete(q)).collect::<Vec<_>>(),
    });
    // The answers the contract must give.
    assert_eq!(
        doc["caseExpect"],
        json!([true, false, false, false, true, true, false])
    );
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(PATH, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(PATH)
        .expect("fixtures/registry/transcript-log.json missing; run with ONYX_BLESS=1");
    assert_eq!(
        on_disk, text,
        "transcript log vectors drifted from ADR-0027 amendment 8"
    );
}
