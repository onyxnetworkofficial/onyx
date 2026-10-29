//! Rust <-> Solidity vectors of sealed reveal posts and attempt records
//! (ADR-0027 amendment 10): the texts the registry rebuilds, real posts (both
//! signatures) it must accept, `revealLogComplete` calldata with the answer
//! Rust's simulated registry gives, and an attempt record.
//! `contracts/test/OnyxRegistrySealedReveals.t.sol` replays the file.
//! Regenerate with `ONYX_BLESS=1` only with an ADR change.

use onyx_coordinator::observe::{sign_commit, test_keys};
use onyx_coordinator::sealed::{
    key_proof_challenge, public_point, seal, sealed_reveal_message, EntryStatus, RevealLogQuery,
    SealedEntry, SimulatedSealedLog,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::onchain::registry_fact_key;
use onyx_core::{
    keccak256, observation_commit, Answer, DataRights, Hash32, Observation, ObservationKind,
    ObservationSpec, Redistribution, SourceId,
};
use serde_json::json;

const PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/registry/sealed-reveals.json"
);
const REVEAL_SECRET: [u8; 32] = [0x42; 32];

fn hex(b: &[u8]) -> String {
    format!("0x{}", ::hex::encode(b))
}

fn feed() -> ObservationSpec {
    ObservationSpec {
        feed: "example.page.usd_per_unit".into(),
        metric: MetricId("example.page.usd_per_unit".into()),
        subject: Subject::Entity("entity:alpha".into()),
        fetcher: keccak256(b"fetcher"),
        url: "https://example.org/page.json".into(),
        selector: "/data/usd_per_unit".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 50,
        },
        window_start: 1_000,
        window_end: 1_050,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("example.page".into()),
            attribution: None,
            terms: String::new(),
        },
    }
}

#[test]
fn sealed_reveal_vectors_match_the_contract_file() {
    let spec = feed();
    // The forge fixture's feed: `FEED_METRIC`, `FEED_TEMPLATE`, `SUBJECT`, `WINDOW`.
    let fact = registry_fact_key(
        &keccak256(b"example.fixed.feed"),
        &keccak256(b"feed template"),
        &keccak256(b"subject"),
        1_000_000,
        false,
    );
    let round = keccak256(b"vector round");
    let (open, close, reveal_close) = (1_000u64, 2_000u64, 3_000u64);
    let key = public_point(&REVEAL_SECRET).expect("key");
    let keys: Vec<_> = (1..=3).map(test_keys::ordered).collect();
    let o = |v: i128| Observation {
        answer: Answer::Number(v),
        observed_at: 1_010,
        page_hash: keccak256(b"page"),
    };
    let salts: Vec<Hash32> = (1..=3u8).map(|i| keccak256(&[b's', i])).collect();
    let commits: Vec<Hash32> = keys
        .iter()
        .enumerate()
        .map(|(i, (_, a))| observation_commit(&round, a, &o(100), &salts[i]))
        .collect();
    let commit_sigs: Vec<[u8; 65]> = keys
        .iter()
        .zip(&commits)
        .map(|((sk, _), c)| sign_commit(sk, &round, 0, c).expect("sign"))
        .collect();
    let posts: Vec<_> = keys
        .iter()
        .enumerate()
        .map(|(i, (sk, _))| {
            seal(
                sk,
                &keccak256(&[b'r', i as u8]).0,
                &key,
                &round,
                0,
                &spec,
                &o(100),
                &salts[i],
            )
            .expect("seal")
        })
        .collect();
    // Commits land at 1,500 (commit phase); observers 1 and 2 post sealed
    // reveals at 2,500 (reveal phase), observer 3 at 3,500 (late, ignored).
    let mut log = SimulatedSealedLog::default();
    log.log
        .signers
        .insert((fact, 0), test_keys::receipt_signer());
    log.keys.insert((fact, 0), key);
    for i in 0..3 {
        log.log
            .post_commit(
                &fact,
                0,
                &keys[i].1,
                &round,
                &commits[i],
                &commit_sigs[i],
                1_500,
            )
            .expect("commit");
    }
    let posted_at = [2_500u64, 2_500, 3_500];
    for (p, at) in posts.iter().zip(posted_at) {
        log.post_sealed(&fact, 0, &round, p, at).expect("post");
    }
    let q = |st: [EntryStatus; 3]| RevealLogQuery {
        registry_fact: fact,
        attempt: 0,
        round_id: round,
        commit_open: open,
        commit_close: close,
        reveal_close,
        entries: keys
            .iter()
            .zip(&commits)
            .zip(st)
            .map(|(((_, a), c), status)| SealedEntry {
                observer: *a,
                commit: *c,
                status,
            })
            .collect(),
    };
    use EntryStatus::{Dismissed as D, NotRevealed as N, Revealed as R};
    let full = q([R, R, N]);
    let mut wrong_window = q([D, R, N]);
    wrong_window.reveal_close = 2_400;
    let mut other_attempt = full.clone();
    other_attempt.attempt = 1;
    let cases = [
        ("posts revealed, late post ignored", full.clone()),
        ("observer 1's post dropped", q([N, R, N])),
        ("observer 2's post dropped", q([R, N, N])),
        ("observer 1 dismissed", q([D, R, N])),
        ("late post dismissed", q([R, R, D])),
        ("late post revealed", q([R, R, R])),
        (
            "dismissal with the reveal window before the posts",
            wrong_window,
        ),
        ("another attempt", other_attempt),
    ];
    let result = keccak256(b"vector result");
    let mut recorded = log.clone();
    recorded
        .record_attempt(&full, &result, reveal_close + 1)
        .expect("record");
    assert!(recorded.attempt_matches(&full, &result));
    let doc = json!({
        "fact": hex(&fact.0),
        "round": hex(&round.0),
        "commitOpen": open,
        "commitClose": close,
        "revealClose": reveal_close,
        "commitPostedAt": 1_500,
        "receiptSigner": hex(&test_keys::receipt_signer()),
        "revealKeyX": hex(&key[..32]),
        "revealKeyY": hex(&key[32..]),
        "observers": keys.iter().map(|(_, a)| hex(a)).collect::<Vec<_>>(),
        "commits": commits.iter().map(|c| hex(&c.0)).collect::<Vec<_>>(),
        "commitSigs": commit_sigs.iter().map(|s| hex(s)).collect::<Vec<_>>(),
        "postedAt": posted_at,
        "postEx": posts.iter().map(|p| hex(&p.ephemeral[..32])).collect::<Vec<_>>(),
        "postEy": posts.iter().map(|p| hex(&p.ephemeral[32..])).collect::<Vec<_>>(),
        "postCiphertext": posts.iter().map(|p| hex(&p.ciphertext)).collect::<Vec<_>>(),
        "postKeyProof": posts.iter().map(|p| hex(&p.key_proof)).collect::<Vec<_>>(),
        "postSig": posts.iter().map(|p| hex(&p.sig)).collect::<Vec<_>>(),
        "keyProofChallenge": hex(&key_proof_challenge(
            &posts[0].ephemeral,
            &posts[0].key_proof[..20].try_into().expect("20 bytes"),
            &round,
            0,
            &keys[0].1,
        ).expect("challenge").to_be_bytes()),
        "sealedRevealMessage": hex(sealed_reveal_message(
            &round, 0, &keys[0].1, &posts[0].ephemeral, &keccak256(&posts[0].ciphertext)
        ).as_bytes()),
        "caseNames": cases.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
        "caseCalldata": cases.iter().map(|(_, q)| hex(&q.calldata())).collect::<Vec<_>>(),
        "caseExpect": cases.iter().map(|(_, q)| log.complete(q)).collect::<Vec<_>>(),
        "recordEntries": hex(&full.packed_entries()),
        "recordResult": hex(&result.0),
        "recordDigest": hex(&full.digest(&result).0),
        "matchesCalldata": hex(&full.matches_calldata(&result)),
        "otherMatchesCalldata": hex(&q([R, R, R]).matches_calldata(&result)),
    });
    assert_eq!(
        doc["caseExpect"],
        json!([true, false, false, true, false, true, false, false])
    );
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(PATH, &text).expect("write vectors");
    }
    let on_disk = std::fs::read_to_string(PATH)
        .expect("fixtures/registry/sealed-reveals.json missing; run with ONYX_BLESS=1");
    assert_eq!(
        on_disk, text,
        "sealed reveal vectors drifted from ADR-0027 amendment 10"
    );
}
