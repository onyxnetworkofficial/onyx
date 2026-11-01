//! `/v1/observations` (ADR-0027 amendment 4): a synthetic fixed feed,
//! settled by the coordinator's own round code, served with its
//! transcript; the client rebuilds the value from its own template.
use super::*;
use crate::client::{
    draw_budget_from_return, draw_matches_from_return, honest_sealed_returns,
    observation_level_from_return, verify_observation, verify_observation_onchain,
    ObservationExpect, SealedRets, VerifiedObservation, VerifyError,
};

/// ROADMAP C11: what the registry answers for a round of a draw without a
/// reveal key (no sealed post): `revealLogComplete` true.
fn unsealed(v: &VerifiedObservation) -> SealedRets {
    honest_sealed_returns(&v.checked.reveal_log, None, true, false)
}
use crate::observations;
use crate::observations::{ObservationFeed, ObservationStore, Transcript, OBSERVATION_ROUND};
use onyx_coordinator::observe::{
    sign_commit, sign_reveal, test_keys, ObservationResults, ObserveConfig, Observer,
};
use onyx_core::claim::MetricId;
use onyx_core::{
    observation_commit, Answer, FeedTemplate, Observation, ObservationKind, Redistribution,
    Subject, SubjectRule,
};

const START: u64 = 1_500;
const METRIC: &str = "test.index";
/// The checker's clock: one minute after the test round's reveal close
/// (1,500 + 300 + 180).
const LOAD_NOW: u64 = 2_040;

fn rights(r: Redistribution) -> DataRights {
    DataRights {
        license: "CC0-1.0".into(),
        redistribution: r,
        source: SourceId("test.page".into()),
        attribution: Some("Source: test page".into()),
        terms: if r == Redistribution::Restricted {
            "test.terms".into()
        } else {
            String::new()
        },
    }
}

fn subject() -> Subject {
    Subject::Aggregate("test.index".into())
}

fn template(r: Redistribution) -> FeedTemplate {
    FeedTemplate {
        feed: "test.index.v1".into(),
        metric: MetricId(METRIC.into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/index.json".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: rights(r),
        subject: SubjectRule::Fixed(subject()),
    }
}

fn obs_addr(i: u8) -> [u8; 20] {
    test_keys::ordered(i).1
}

/// Settle one round of `template` from `START` where observers answer
/// `answers` (mantissas), and return its transcript.
fn settle(template: &FeedTemplate, answers: &[i128]) -> Transcript {
    settle_from(template, START, answers)
}

/// Settle one round of `template` from `start`.
fn settle_from(template: &FeedTemplate, start: u64, answers: &[i128]) -> Transcript {
    let (mut store, admin) = ObservationResults::new();
    store
        .register_feed(&admin, template.clone(), "test", 0)
        .expect("registers");
    let spec = template.instantiate(&subject(), start).expect("spec");
    let cfg = ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    };
    let observers: Vec<Observer> = (1..=answers.len() as u8)
        .map(|i| Observer {
            address: obs_addr(i),
            bond: 1_000,
        })
        .collect();
    let snapshot = store
        .announce_draw(&spec, &cfg, &observers, 10, &SimulatedChain::at(0))
        .expect("announces");
    let mut round = store
        .open_round(
            spec.clone(),
            cfg,
            snapshot,
            &SimulatedChain::with_beacon(Hash32([3; 32])),
        )
        .expect("opens");
    let observers = round.observers().to_vec();
    let rid = round.round_id();
    let attempt = round.draw().attempt;
    let mut reveals = Vec::new();
    for (i, n) in (1..=answers.len() as u8).zip(answers) {
        let (sk, a) = test_keys::ordered(i);
        let o = Observation {
            answer: Answer::Number(*n),
            observed_at: start + 10,
            page_hash: keccak256(&n.to_be_bytes()),
        };
        let salt = keccak256(&[i]);
        let c = observation_commit(&rid, &a, &o, &salt);
        let sig = sign_commit(&sk, &rid, attempt, &c).expect("sign");
        round.commit(start + 10, a, c, sig).expect("commit");
        reveals.push((sk, a, o, salt));
    }
    for (sk, a, o, s) in &reveals {
        let sig = sign_reveal(sk, &rid, attempt, o, s).expect("sign");
        round
            .reveal(cfg.commit_close + 1, *a, o.clone(), *s, sig)
            .expect("reveal");
    }
    let commits = round.signed_commits();
    let reveals = round.signed_reveals();
    let settled = store.settle(&round, cfg.reveal_close + 1).expect("settles");
    let onyx_coordinator::observe::ObservationOutcome::Value(answer) = settled.outcome().clone()
    else {
        panic!("no value")
    };
    Transcript {
        subject: subject(),
        window_start: start,
        cfg,
        pool: round.pool().to_vec(),
        draw: round.draw(),
        observers,
        commits,
        reveals,
        sealed: Default::default(),
        answer,
    }
}

/// The operator's pin for `t` (L2API-R4-F1).
fn pins(t: &FeedTemplate) -> observations::TemplatePins {
    observations::TemplatePins::from([(t.metric.0.clone(), t.id())])
}

fn feed(r: Redistribution) -> ObservationFeed {
    let t = template(r);
    let text =
        ObservationStore::to_json(&t, &[settle(&t, &[12_345, 12_345, 12_345])], None).to_string();
    ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("store loads")
}

/// The shard fixture plus the level-2 feed.
fn served(paid: bool, facilitator: Option<Arc<dyn Facilitator>>, r: Redistribution) -> Fixture {
    let f = build(Setup {
        paid,
        facilitator,
        ..Setup::default()
    });
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_observations(feed(r))
        .expect("adds");
    f
}

const URI: &str = "/v1/observations/test.index?subject=test.index";

/// A round of `t` from `start` at draw 0 whose three observers disagree
/// (no result), built directly: the pool is the draw size.
fn no_result(t: &FeedTemplate, start: u64) -> observations::Attempt {
    let spec = t.instantiate(&subject(), start).expect("spec");
    let cfg = ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    };
    let observers: Vec<Observer> = (1..=3u8)
        .map(|i| Observer {
            address: obs_addr(i),
            bond: 1_000,
        })
        .collect();
    let draw = onyx_coordinator::observe::DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
    let rid = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &draw);
    let mut commits = Vec::new();
    let mut reveals = Vec::new();
    for (i, o) in (1i128..).zip(&observers) {
        let ob = Observation {
            answer: Answer::Number(i),
            observed_at: start + 10,
            page_hash: keccak256(&i.to_be_bytes()),
        };
        let salt = keccak256(&o.address);
        let (c, r) = test_keys::signed(
            &test_keys::ordered_secret(&o.address).expect("test key"),
            &rid,
            draw.attempt,
            &ob,
            &salt,
        );
        commits.push(c);
        reveals.push(r);
    }
    observations::Attempt {
        subject: subject(),
        window_start: start,
        cfg,
        pool: observers.clone(),
        draw,
        observers,
        commits,
        reveals,
        sealed: Default::default(),
    }
}

/// SEC-R2-F1: the value fact at `START` and, for the window before it, a
/// fact whose only round settled no value.
fn served_with_attempt() -> (Fixture, observations::Attempt) {
    served_with_attempt_under(ApiConfig::default())
}

fn served_with_attempt_under(config: ApiConfig) -> (Fixture, observations::Attempt) {
    let t = template(Redistribution::Allowed);
    let a = no_result(&t, START - 600);
    let text = ObservationStore::to_json_with_attempts(
        &t,
        &[settle(&t, &[12_345, 12_345, 12_345])],
        std::slice::from_ref(&a),
        None,
    )
    .to_string();
    let store = ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("loads");
    let f = build(Setup {
        paid: false,
        config,
        ..Setup::default()
    });
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_observations(store)
        .expect("adds");
    (f, a)
}

/// ATT-R1-F5: free `/attempts` answers have their own per-client budget.
#[tokio::test]
async fn free_attempts_are_rate_limited_per_client() {
    let (f, a) = served_with_attempt_under(ApiConfig {
        free_attempts_per_minute: 2,
        ..ApiConfig::default()
    });
    let (t, subj) = expect();
    let window_end = t
        .instantiate(&subj, a.window_start)
        .expect("spec")
        .window_end;
    let uri =
        format!("/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}");
    let x: IpAddr = "203.0.113.7".parse().expect("ip");
    let y: IpAddr = "203.0.113.8".parse().expect("ip");
    for _ in 0..2 {
        let (s, _, b) = send(&f, Method::GET, &uri, &[], Some(x)).await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let (s, h, body) = send(&f, Method::GET, &uri, &[], Some(x)).await;
    assert_eq!(
        (s, code(&body)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.get("retry-after").is_some());
    let (s, _, _) = send(&f, Method::GET, &uri, &[], Some(y)).await;
    assert_eq!(s, StatusCode::OK, "another client has its own budget");
}

#[tokio::test]
async fn a_fact_without_a_value_serves_its_rounds_free_and_a_valued_fact_refers_to_outcome_all() {
    let (f, a) = served_with_attempt();
    let (t, subj) = expect();
    let window_end = t
        .instantiate(&subj, a.window_start)
        .expect("spec")
        .window_end;
    let ex = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: Some(window_end),
        min_window_end: 0,
        now: LOAD_NOW,
    };
    let uri =
        format!("/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}");
    let (s, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["attempts"][0]["outcome"], "noResult");
    let v = crate::client::verify_attempts(&b, &ex).expect("verifies");
    assert_eq!((v.attempts.len(), v.value_attempt), (1, None));
    // The valued fact's rounds come only with its value.
    let valued = t.instantiate(&subj, START).expect("spec").window_end;
    let (s, _, b) = get(
        &f,
        &format!("/v1/observations/test.index/attempts?subject=test.index&window_end={valued}"),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let (s, _, _) = get(&f, "/v1/observations/test.index/attempts?subject=nope", &[]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = get(
        &f,
        "/v1/observations/test.index/attempts?subject=test.index&as_of=1",
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn outcome_all_adds_the_facts_rounds_without_a_value() {
    let (f, _) = served_with_attempt();
    let (t, subj) = expect();
    let ex = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: LOAD_NOW,
    };
    let (s, h, b) = get(&f, &format!("{URI}&outcome=all"), &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    // The valued fact had no round without a value: an empty list, which
    // the client checks against the chain's drawCount (1).
    assert_eq!(b["attempts"], json!([]));
    let v = verify_observation(&b, &ex).expect("verifies");
    let all = v.attempts.expect("asked for all");
    assert_eq!(all.value_attempt, Some(0));
    let mut one = [0u8; 32];
    one[31] = 1;
    let mut budget = [0u8; 64];
    budget[63] = 3;
    let tally = crate::client::verify_attempts_onchain(&all, &one, &[], &budget, &[], &[])
        .expect("one draw, the value's");
    assert_eq!((tally.draws, tally.without_value), (1, 0));
    // Its ETag differs from the plain answer's.
    let (_, h2, b2) = get(&f, URI, &[]).await;
    assert!(b2.get("attempts").is_none());
    assert_ne!(h.get("etag"), h2.get("etag"));
    // `value` is the default: one spelling, without the parameter.
    let (s, _, _) = get(&f, &format!("{URI}&outcome=value"), &[]).await;
    assert!(s.is_redirection(), "{s}");
    let (s, _, _) = get(&f, &format!("{URI}&outcome=some"), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

fn expect() -> (FeedTemplate, Subject) {
    (template(Redistribution::Allowed), subject())
}

fn limit_ids(v: &Value) -> Vec<&str> {
    v.as_array()
        .expect("limits")
        .iter()
        .map(|l| l["id"].as_str().expect("id"))
        .collect()
}

/// ROADMAP B4 (THREATS REVLOG-1, REVLOG-5): the 402 offer (its body and
/// the PAYMENT-REQUIRED `extensions`) and every answer state both known
/// limits, so a buyer sees them before paying. A value's fact is closed,
/// so REVLOG-5 does not apply to the rounds of a value answer.
#[tokio::test]
async fn every_level2_answer_and_offer_states_revlog_1_and_revlog_5() {
    let want = observations::limits_json(false);
    assert_eq!(limit_ids(&want), ["REVLOG-1", "REVLOG-5"]);
    assert_eq!(want[0]["applies"], true);
    assert_eq!(want[1]["applies"], false);
    for l in want.as_array().expect("limits") {
        assert_eq!(l["docs"], observations::LIMITS_DOCS);
        assert!(!l["text"].as_str().expect("text").is_empty());
    }
    let (t, subj) = expect();
    let ex = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: LOAD_NOW,
    };
    let f = served(true, mock(), Redistribution::Allowed);
    for (uri, nonce) in [(URI.to_string(), 61), (format!("{URI}&outcome=all"), 62)] {
        for method in [Method::GET, Method::HEAD] {
            let (s, h, b) = send(&f, method.clone(), &uri, &[], None).await;
            assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "{method} {uri}");
            if method == Method::GET {
                assert_eq!(b["limits"], want, "402 body of {uri}");
            }
            let pr: PaymentRequired =
                decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("offer");
            // x402 v2 §5.1.2: each extension is exactly `{info, schema}`.
            let ext = pr.extensions.expect("extensions");
            let keys = |v: &Value| -> Vec<String> {
                v.as_object().expect("object").keys().cloned().collect()
            };
            assert_eq!(keys(&ext), ["limits"], "402 offer of {method} {uri}");
            let l = &ext["limits"];
            assert_eq!(keys(l), ["info", "schema"], "{method} {uri}");
            assert_eq!(l["info"], json!({ "limits": want }), "{method} {uri}");
            assert_eq!(l["schema"]["required"], json!(["limits"]));
            let item = &l["schema"]["properties"]["limits"]["items"];
            assert_eq!(item["required"], json!(["id", "applies", "text", "docs"]));
            for x in want.as_array().expect("limits") {
                assert_eq!(keys(x), keys(&item["properties"]), "schema fits info");
            }
        }
        let (_, h, _) = get(&f, &uri, &[]).await;
        let (s, _, b) = get(
            &f,
            &uri,
            &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))],
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["limits"], want, "paid answer of {uri}");
        verify_observation(&b, &ex).expect("the client accepts the stated limits");
    }
    // A free (decayed or unpriced) answer states them too.
    let free = served(false, None, Redistribution::Allowed);
    let (s, _, b) = get(&free, URI, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["limits"], want);
}

/// B4: the free `/attempts` answer of a closed (fixed) fact states both
/// limits, REVLOG-5 not applying: its rounds come with their reveals.
#[tokio::test]
async fn a_closed_facts_attempts_state_revlog_5_as_not_applying() {
    let (f, a) = served_with_attempt();
    let (t, subj) = expect();
    let window_end = t
        .instantiate(&subj, a.window_start)
        .expect("spec")
        .window_end;
    let uri =
        format!("/v1/observations/test.index/attempts?subject=test.index&window_end={window_end}");
    let (s, _, b) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(b["attempts"][0].get("reveals").is_some(), "closed: open");
    assert_eq!(b["limits"], observations::limits_json(false));
}

/// x402 v2 §5.1.2: clients echo the offer's `extensions` in their
/// `PaymentPayload` (the official SDKs do). A level-2 payment that echoes
/// `limits` is accepted and served like one that does not.
#[tokio::test]
async fn a_payment_echoing_the_limits_extension_is_accepted() {
    let f = served(true, mock(), Redistribution::Allowed);
    let (s, h, _) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let pr: Value =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("offer");
    let mut p: Value = decode_header(&pay(&h, "payer a", 63)).expect("payload");
    p["extensions"] = pr["extensions"].clone();
    assert!(p["extensions"]["limits"]["info"]["limits"].is_array());
    let (s, _, b) = get(&f, URI, &[(x402::PAYMENT_SIGNATURE, encode_header(&p))]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["limits"], observations::limits_json(false));
}

/// B4, backward compatibility: an answer without `limits` (a server
/// from before B4) still verifies; a misstated one is refused.
#[tokio::test]
async fn the_client_takes_no_limits_from_an_older_server_and_refuses_misstated_ones() {
    let f = served(false, None, Redistribution::Allowed);
    let (_, _, b) = get(&f, URI, &[]).await;
    let (t, subj) = expect();
    let ex = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: LOAD_NOW,
    };
    let mut old = b.clone();
    old.as_object_mut().expect("obj").remove("limits");
    verify_observation(&old, &ex).expect("an older server's answer verifies");
    type Edit = (&'static str, fn(&mut Value));
    let edits: [Edit; 5] = [
        ("REVLOG-5 left out", |b| {
            b["limits"].as_array_mut().expect("arr").pop();
        }),
        ("REVLOG-1 said not to apply", |b| {
            b["limits"][0]["applies"] = json!(false)
        }),
        ("REVLOG-5 said to apply to an open round", |b| {
            b["limits"][1]["applies"] = json!(true)
        }),
        ("a softened text", |b| {
            b["limits"][0]["text"] = json!("none")
        }),
        ("not a list", |b| b["limits"] = json!({})),
    ];
    for (what, edit) in edits {
        let mut x = b.clone();
        edit(&mut x);
        assert!(verify_observation(&x, &ex).is_err(), "{what}");
    }
}

/// L2RND-R3-F3, REVLOG LOW-1: the served `onchain` hint names all four
/// registry views a client must ask, and the client accepts it.
#[tokio::test]
async fn the_onchain_hint_names_all_four_registry_calls() {
    let t = template(Redistribution::Allowed);
    let pointer = observations::OnchainPointer {
        chain_id: 84_532,
        registry: [7; 20],
    };
    let text =
        ObservationStore::to_json(&t, &[settle(&t, &[12_345, 12_345, 12_345])], Some(pointer))
            .to_string();
    let store = ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).expect("store loads");
    let f = build(Setup {
        paid: false,
        ..Setup::default()
    });
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_observations(store)
        .expect("adds");
    let (s, _, b) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let o = &b["onchain"];
    assert_eq!(o["call"], observations::OBSERVATION_LEVEL_CALL);
    assert_eq!(o["drawMatches"], observations::DRAW_MATCHES_CALL);
    assert_eq!(o["drawBudget"], observations::DRAW_BUDGET_CALL);
    // Review REVLOG LOW-1: the transcript check is named too.
    assert_eq!(
        o["transcriptComplete"],
        observations::TRANSCRIPT_COMPLETE_CALL
    );
    let (t, subj) = expect();
    let e = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: LOAD_NOW,
    };
    verify_observation(&b, &e).expect("the client accepts the hint");
    // An older server's hint without the key still verifies (the client
    // asks transcriptComplete anyway); a hint naming another call does not.
    let mut old = b.clone();
    old["onchain"]
        .as_object_mut()
        .expect("hint")
        .remove("transcriptComplete");
    verify_observation(&old, &e).expect("a hint without the key verifies");
    let mut wrong = b.clone();
    wrong["onchain"]["transcriptComplete"] = "OnyxRegistry.somethingElse()".into();
    assert!(matches!(
        verify_observation(&wrong, &e),
        Err(VerifyError::Malformed("onchain.transcriptComplete"))
    ));
}

#[tokio::test]
async fn a_free_observation_verifies_from_the_clients_own_template() {
    let f = served(false, None, Redistribution::Allowed);
    let (s, h, b) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["value"], "123.45");
    assert_eq!(b["proofLevel"], 2);
    assert_eq!(cc(&h), "public, no-cache");
    let (t, subj) = expect();
    let v = verify_observation(
        &b,
        &ObservationExpect {
            template: &t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        },
    )
    .expect("verifies");
    assert_eq!(v.answer, Answer::Number(12_345));
    assert_eq!(v.value, "123.45");
    // The on-chain check the client can make with its own node.
    let call = v.observation_level_calldata();
    assert_eq!(call.len(), 4 + 5 * 32);
    let mut two = [0u8; 32];
    two[31] = 2;
    assert_eq!(observation_level_from_return(&two), Ok(2));
    assert!(observation_level_from_return(&[1u8; 32]).is_err());
    // L2RND-R1: the draw check against the client's own node.
    let call = v.draw_matches_calldata();
    assert_eq!(call.len(), 4 + 7 * 32);
    assert_eq!(
        call[..4],
        keccak256(b"drawMatches(bytes32,bytes32,uint64,uint32,bytes32,uint64,bytes32)").0[..4]
    );
    assert_eq!(
        call[4 + 5 * 32..4 + 6 * 32][24..],
        v.checked.draw.beacon_height.to_be_bytes()
    );
    assert_eq!(call[4 + 6 * 32..], v.checked.draw.beacon.0);
    // L2RND-R3: the commitment word is the client's own recomputation.
    assert_eq!(
        call[4 + 4 * 32..4 + 5 * 32],
        v.checked.onchain.draw_commit().0
    );
    let mut yes = [0u8; 32];
    yes[31] = 1;
    assert_eq!(draw_matches_from_return(&yes), Ok(true));
    assert_eq!(draw_matches_from_return(&[0u8; 32]), Ok(false));
    yes[31] = 2;
    assert!(draw_matches_from_return(&yes).is_err());
    // L2RND-R2-F6: level 2, drawMatches and the draw bound, together.
    let [_, _, budget, complete] = v.onchain_calls();
    // ADR-0027 amendment 8: the transcript log query of the served round.
    assert_eq!(complete, v.checked.log.calldata());
    assert_eq!(v.checked.log.round_id, v.checked.round_id);
    assert_eq!(v.checked.log.registry_fact, v.checked.registry_fact);
    assert!(v.checked.log.entries.iter().all(|e| e.revealed));
    assert_eq!(
        budget[..4],
        keccak256(b"drawBudget(bytes32,bytes32)").0[..4]
    );
    assert_eq!(budget[4..36], v.checked.onchain.metric_key.0);
    let words = |a: u32, b: u32| {
        let mut w = [0u8; 64];
        w[28..32].copy_from_slice(&a.to_be_bytes());
        w[60..64].copy_from_slice(&b.to_be_bytes());
        w
    };
    assert_eq!(draw_budget_from_return(&words(1, 3)), Ok((1, 3)));
    assert!(draw_budget_from_return(&[0u8; 32]).is_err());
    yes[31] = 1;
    let ok = verify_observation_onchain(&v, &two, &yes, &words(0, 3), &yes, &unsealed(&v));
    assert_eq!(ok, Ok(()));
    let chain = |e: &'static str| Err(VerifyError::ChainMismatch(e));
    let mut one = [0u8; 32];
    one[31] = 1;
    assert_eq!(
        verify_observation_onchain(&v, &one, &yes, &words(0, 3), &yes, &unsealed(&v)),
        chain("observationLevel")
    );
    assert_eq!(
        verify_observation_onchain(&v, &two, &[0u8; 32], &words(0, 3), &yes, &unsealed(&v)),
        chain("drawMatches")
    );
    // More draws open than the reopens allow, or an attempt past the bound.
    assert_eq!(
        verify_observation_onchain(&v, &two, &yes, &words(4, 3), &yes, &unsealed(&v)),
        chain("drawBudget")
    );
    let mut late = v.clone();
    late.checked.draw.attempt = 3;
    assert_eq!(
        verify_observation_onchain(&late, &two, &yes, &words(0, 3), &yes, &unsealed(&late)),
        chain("drawBudget")
    );
    assert_eq!(
        verify_observation_onchain(&late, &two, &yes, &words(0, 6), &yes, &unsealed(&late)),
        Ok(())
    );
    // SIG-R1-F1: the chain logged a commit or receipt the round leaves out.
    assert_eq!(
        verify_observation_onchain(&v, &two, &yes, &words(0, 3), &[0u8; 32], &unsealed(&v)),
        chain("transcriptComplete: a logged commit or receipted reveal is left out")
    );
    assert!(verify_observation_onchain(&v, &two, &yes, &words(0, 3), &two, &unsealed(&v)).is_err());
    // Conditional request on the result hash.
    let etag = h[header::ETAG].to_str().expect("etag").to_string();
    let (s304, _, _) = get(&f, URI, &[("if-none-match", etag)]).await;
    assert_eq!(s304, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_fresh_observation_is_paid_billed_to_its_fact_and_free_after_decay() {
    let f = served(true, mock(), Redistribution::Allowed);
    let (s, h, b) = get(&f, URI, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("payment_required"))
    );
    let (s, h2, b) = get(
        &f,
        URI,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 41))],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(h2.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(cc(&h2), "private, no-cache");
    // Billed to the observation revenue book of its on-chain fact key.
    let r = f.state.receipts.peek(10);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].round, OBSERVATION_ROUND);
    assert_eq!(
        r[0].shard.to_string(),
        b["registryFactKey"].as_str().expect("key")
    );
    // The settled time is the window end the template fixes (1,500 +
    // 300, L2API-R3-F1); the test policy keeps a value paid for an hour.
    f.now.store(1_800 + 3_599, Ordering::SeqCst);
    let (s, _, _) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "still fresh");
    f.now.store(1_800 + 3_600, Ordering::SeqCst);
    let (s, h, _) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::OK, "decayed to free");
    assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
}

/// L2API-R3-F3: a fixed feed's older, now-free windows are reachable
/// with `as_of` (newest window ending at or before it), like a shard.
#[tokio::test]
async fn as_of_picks_the_newest_window_ending_at_or_before_it() {
    let f = build(Setup::default());
    let t = template(Redistribution::Allowed);
    let rounds = [
        settle_from(&t, START, &[12_345, 12_345, 12_345]),
        settle_from(&t, 2_400, &[22_222, 22_222, 22_222]),
    ];
    let text = ObservationStore::to_json(&t, &rounds, None).to_string();
    let feed = ObservationStore::from_json(&text, 2_940, &pins(&t)).expect("store loads");
    f.state
        .catalog
        .write()
        .expect("lock")
        .add_observations(feed)
        .expect("adds");
    let (s, _, b) = get(&f, URI, &[]).await;
    assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(2_700)));
    let (s, _, b) = get(&f, &format!("{URI}&as_of=2699"), &[]).await;
    assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(1_800)));
    assert_eq!(b["value"], "123.45");
    let (s, _, b) = get(&f, &format!("{URI}&as_of=2700"), &[]).await;
    assert_eq!((s, b["windowEnd"].as_u64()), (StatusCode::OK, Some(2_700)));
    let (s, _, b) = get(&f, &format!("{URI}&as_of=1799"), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_subject"))
    );
    let (s, _, _) = get(&f, &format!("{URI}&window_end=1800&as_of=2700"), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "window_end and as_of together");
}

/// L2API-R4-F2: a round older than the client's `min_window_end` is a
/// 404 before any 402, so a client is never charged for a round its own
/// freshness floor refuses (as a shard's `min_as_of`, M7-L47-F5).
#[tokio::test]
async fn min_window_end_refuses_an_old_round_before_payment() {
    let f = served(true, mock(), Redistribution::Allowed);
    f.now.store(1_800 + 10, Ordering::SeqCst);
    let (s, h, b) = get(&f, &format!("{URI}&min_window_end=1801"), &[]).await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, Some("too_old")));
    assert!(
        !h.contains_key(x402::PAYMENT_REQUIRED),
        "no payment terms for a refused round"
    );
    let (s, _, _) = get(&f, &format!("{URI}&min_window_end=1800"), &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "the round meets the floor");
    // The canonical order is subject, window_end, as_of, min_window_end.
    let (s, _, _) = get(
        &f,
        "/v1/observations/test.index?min_window_end=1800&subject=test.index",
        &[],
    )
    .await;
    assert!(s.is_redirection(), "{s}");
}

#[tokio::test]
async fn restricted_rights_are_not_served_and_unknown_names_are_404() {
    let f = served(false, None, Redistribution::Restricted);
    let (s, _, b) = get(&f, URI, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            Some("restricted_data")
        )
    );
    let f = served(false, None, Redistribution::Allowed);
    let (s, _, b) = get(&f, "/v1/observations/test.other?subject=test.index", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_metric"))
    );
    let (s, _, b) = get(&f, "/v1/observations/test.index?subject=test.nope", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("unknown_subject"))
    );
    // One canonical URL per resource; unknown parameters are refused.
    let (s, _, _) = get(
        &f,
        "/v1/observations/test.index?subject=test.index&x=1",
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[test]
fn a_forbidden_source_never_loads_and_a_bad_transcript_never_loads() {
    // Forbidden rights: refused at load (a forbidden feed cannot even be
    // registered, so build the store text by hand from an allowed one).
    let t = template(Redistribution::Allowed);
    let mut store = ObservationStore::to_json(&t, &[settle(&t, &[7, 7, 7])], None);
    let mut forbidden = template(Redistribution::Forbidden);
    forbidden.rights.terms = String::new();
    store["template"] = json!(hex0x(&borsh::to_vec(&forbidden).expect("borsh")));
    let e = ObservationStore::from_json(&store.to_string(), LOAD_NOW, &pins(&forbidden))
        .expect_err("forbidden");
    assert!(e.to_string().contains("forbid"), "{e}");
    // A served answer that is not the value the reveals settle.
    let mut tr = settle(&t, &[7, 7, 7]);
    tr.answer = Answer::Number(8);
    let text = ObservationStore::to_json(&t, &[tr], None).to_string();
    assert!(ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).is_err());
    // Unknown store fields are refused.
    let mut v = ObservationStore::to_json(&t, &[settle(&t, &[7, 7, 7])], None);
    v["surprise"] = json!(1);
    assert!(ObservationStore::from_json(&v.to_string(), LOAD_NOW, &pins(&t)).is_err());
}

#[tokio::test]
async fn the_client_refuses_forged_evidence() {
    let f = served(false, None, Redistribution::Allowed);
    let (_, _, b) = get(&f, URI, &[]).await;
    let (t, subj) = expect();
    let ex = ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: 0,
        now: LOAD_NOW,
    };
    assert!(verify_observation(&b, &ex).is_ok());
    let forged = |edit: &dyn Fn(&mut Value)| {
        let mut x = b.clone();
        edit(&mut x);
        verify_observation(&x, &ex)
    };
    // Another value, with or without its text.
    let other = hex0x(&borsh::to_vec(&Answer::Number(99_999)).expect("borsh"));
    assert!(forged(&|x| x["answer"] = json!(other)).is_err());
    assert!(forged(&|x| x["value"] = json!("999.99")).is_err());
    // A reveal that does not open its commit.
    assert!(forged(&|x| x["reveals"][0]["salt"] = json!(Hash32([9; 32]).to_string())).is_err());
    // A changed configuration (another round id).
    assert!(forged(&|x| x["config"]["reward"] = json!("11")).is_err());
    // Derived fields must be what the transcript yields.
    assert!(forged(&|x| x["resultHash"] = json!(Hash32([1; 32]).to_string())).is_err());
    assert!(forged(&|x| x["registryFactKey"] = json!(Hash32([1; 32]).to_string())).is_err());
    assert!(forged(&|x| x["settledUnix"] = json!(1)).is_err());
    assert!(forged(&|x| x["proofLevel"] = json!(1)).is_err());
    // ADR-0048: the ladder is the round's own level-2 step; a lifted
    // level, a step on other evidence, or a rise nothing checks is refused.
    let ok = verify_observation(&b, &ex).expect("verifies");
    assert_eq!(ok.ladder.level(), Some(onyx_core::ProofLevel::ReObserved));
    assert_eq!(ok.ladder.steps[0].evidence, ok.checked.result);
    assert_eq!(b["ladder"]["level"], 2);
    assert!(forged(&|x| x["ladder"]["level"] = json!(1)).is_err());
    assert!(forged(&|x| {
        x["ladder"]["level"] = json!(1);
        x["ladder"]["steps"][0]["level"] = json!(1);
        x["ladder"]["steps"][0]["name"] = json!("recomputed");
    })
    .is_err());
    assert!(
        forged(&|x| x["ladder"]["steps"][0]["evidence"] = json!(Hash32([1; 32]).to_string()))
            .is_err()
    );
    assert!(forged(&|x| x["ladder"]["value"] = json!(Hash32([1; 32]).to_string())).is_err());
    assert!(forged(&|x| {
        let mut s = x["ladder"]["steps"][0].clone();
        s["level"] = json!(1);
        s["name"] = json!("recomputed");
        s["evidence"] = json!(Hash32([7; 32]).to_string());
        x["ladder"]["steps"] = json!([x["ladder"]["steps"][0].clone(), s]);
        x["ladder"]["level"] = json!(1);
    })
    .is_err());
    // An older node without the field: the client's own step.
    assert_eq!(
        forged(&|x| {
            x.as_object_mut().expect("object").remove("ladder");
        })
        .expect("verifies")
        .ladder,
        ok.ladder
    );
    assert!(forged(&|x| x["license"] = json!("MIT")).is_err());
    // Unknown fields, and a shard answer, are refused.
    assert!(forged(&|x| x["surprise"] = json!(1)).is_err());
    assert!(forged(&|x| x["verify"] = json!({})).is_err());
    // Another client template (another fetcher) yields another spec: the
    // served transcript does not verify under it.
    let mut other_t = template(Redistribution::Allowed);
    other_t.fetcher = keccak256(b"another fetcher");
    assert!(verify_observation(
        &b,
        &ObservationExpect {
            template: &other_t,
            subject: &subj,
            window_end: None,
            min_window_end: 0,
            now: LOAD_NOW,
        }
    )
    .is_err());
    // Another subject than the one asked for.
    let elsewhere = Subject::Aggregate("test.elsewhere".into());
    assert_eq!(
        verify_observation(
            &b,
            &ObservationExpect {
                template: &t,
                subject: &elsewhere,
                window_end: None,
                min_window_end: 0,
                now: LOAD_NOW,
            }
        ),
        Err(VerifyError::NotRequested("another subject"))
    );
}

#[test]
fn observation_metrics_are_never_markets_and_never_share_a_shard_name() {
    let f = build(Setup::default());
    let mut c = f.state.catalog.write().expect("lock");
    // A level-2 metric cannot take the name of a shard metric.
    let mut t = template(Redistribution::Allowed);
    t.metric = MetricId("test.flow".into());
    assert!(c
        .add_observations(ObservationFeed::new(t).expect("feed"))
        .is_err());
    assert!(c.add_observations(feed(Redistribution::Allowed)).is_ok());
    assert!(
        c.add_observations(feed(Redistribution::Allowed)).is_err(),
        "one feed per metric"
    );
    // Markets bind shard rows only: there is no market on an observation.
    assert!(c.market(METRIC).is_none());
}

/// R-EST-2 (ADR-0050): a level-2 feed in the estimate namespace is refused.
#[test]
fn a_level2_feed_under_an_estimate_metric_is_refused() {
    let f = build(Setup::default());
    let mut c = f.state.catalog.write().expect("lock");
    let mut t = template(Redistribution::Allowed);
    t.metric = MetricId("estimate.test.daily".into());
    let err = c
        .add_observations(ObservationFeed::new(t).expect("feed"))
        .expect_err("estimate metric");
    assert!(err.contains("estimate"), "{err}");
}
/// A self-consistent transcript for `observers` under `cfg` (every commit
/// opens, every reveal agrees on `value`): what an operator could write
/// into a store by hand, without any coordinator opening the round.
fn hand_made(
    window_start: u64,
    cfg: ObserveConfig,
    observers: Vec<Observer>,
    value: i128,
) -> Transcript {
    let t = template(Redistribution::Allowed);
    let spec = t.instantiate(&subject(), window_start).expect("spec");
    // The pool is exactly the draw size, so the draw is the whole pool.
    let draw = onyx_coordinator::observe::DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
    let rid = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &draw);
    let mut commits = Vec::new();
    let mut reveals = Vec::new();
    for (i, o) in observers.iter().enumerate() {
        let ob = Observation {
            answer: Answer::Number(value),
            observed_at: window_start + 10,
            page_hash: keccak256(b"page"),
        };
        let salt = keccak256(&[i as u8, 7]);
        let (c, r) = test_keys::signed(
            &test_keys::ordered_secret(&o.address).expect("test key"),
            &rid,
            draw.attempt,
            &ob,
            &salt,
        );
        commits.push(c);
        reveals.push(r);
    }
    Transcript {
        subject: subject(),
        window_start,
        cfg,
        pool: observers.clone(),
        draw,
        observers,
        commits,
        reveals,
        sealed: Default::default(),
        answer: Answer::Number(value),
    }
}

fn good_cfg(window_start: u64) -> ObserveConfig {
    let end = window_start + 300;
    ObserveConfig {
        commit_close: end + 60,
        reveal_close: end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

fn observers(n: u8, bond: u128) -> Vec<Observer> {
    (1..=n)
        .map(|i| Observer {
            address: obs_addr(i),
            bond,
        })
        .collect()
}

/// L2API-R1-F1: a round no coordinator could open (one observer under
/// `min_observers` 3, no bond, no slashes, commits closing at 0) neither
/// loads on the server nor verifies in a client, even though every commit
/// opens and the answer is the value the reveals settle.
#[test]
fn a_round_no_coordinator_could_open_never_loads_and_never_verifies() {
    let t = template(Redistribution::Allowed);
    let loads = |tr: Transcript| {
        ObservationStore::from_json(
            &ObservationStore::to_json(&t, &[tr], None).to_string(),
            LOAD_NOW,
            &pins(&t),
        )
    };
    // The reviewer's round: one observer, zero bond, zero slashes.
    let one = hand_made(
        START,
        ObserveConfig {
            commit_close: 0,
            reveal_close: 0,
            reward: 0,
            outlier_slash: 0,
            miss_slash: 0,
        },
        vec![Observer {
            address: obs_addr(0x42),
            bond: 0,
        }],
        99_999_999,
    );
    assert!(observations::check(&t, &one, LOAD_NOW).is_err());
    let e = loads(one).expect_err("refused at load");
    assert!(e.to_string().contains("coordinator could open"), "{e}");
    // Good config, but too few observers.
    assert!(loads(hand_made(START, good_cfg(START), observers(2, 1_000), 7)).is_err());
    // Bonds that do not cover the outlier slash.
    assert!(loads(hand_made(START, good_cfg(START), observers(3, 99), 7)).is_err());
    // Observers out of order (or repeated).
    let mut swapped = observers(3, 1_000);
    swapped.swap(0, 1);
    assert!(loads(hand_made(START, good_cfg(START), swapped, 7)).is_err());
    let mut twice = observers(3, 1_000);
    twice[1] = twice[0];
    assert!(loads(hand_made(START, good_cfg(START), twice, 7)).is_err());
    // The same rules, honestly met, load.
    assert!(loads(hand_made(START, good_cfg(START), observers(3, 1_000), 7)).is_ok());
}

/// L2API-R1-F2: a round's reveal close is bounded: never after the
/// checker's clock (plus skew), never more than a day after the commit
/// close, so a round "settling in the future" is never served. The reveal
/// close is not the price clock (L2API-R2-F2, R4-F3): the price decays
/// from the template-fixed decay start, here the round's window end.
#[test]
fn a_round_settling_in_the_future_is_refused_and_the_price_decays() {
    let t = template(Redistribution::Allowed);
    // A valid config whose reveal close is a day after LOAD_NOW.
    let late_start = (LOAD_NOW + 86_400) / 300 * 300;
    let tr = hand_made(late_start, good_cfg(late_start), observers(3, 1_000), 7);
    assert!(matches!(
        observations::check(&t, &tr, LOAD_NOW),
        Err(observations::ObservationError::NotSettledYet { .. })
    ));
    let text = ObservationStore::to_json(&t, std::slice::from_ref(&tr), None).to_string();
    assert!(ObservationStore::from_json(&text, LOAD_NOW, &pins(&t)).is_err());
    // Once the clock passes the reveal close, the same round loads.
    let after = tr.cfg.reveal_close;
    assert!(ObservationStore::from_json(&text, after, &pins(&t)).is_ok());
    // A reveal phase longer than MAX_REVEAL_SECS never loads.
    let mut long = good_cfg(START);
    long.reveal_close = long.commit_close + onyx_coordinator::observe::MAX_REVEAL_SECS + 1;
    let tr = hand_made(START, long, observers(3, 1_000), 7);
    assert!(observations::check(&t, &tr, u64::MAX).is_err());
    // And the honest round's price decays from its template-fixed decay
    // start (a fixed feed: its own window end), not from its reveal close.
    let mut c = Catalog::default();
    c.set_pricing(
        METRIC,
        PricingPolicy {
            base_price_micro_usdc: 2_500,
            fresh_window_secs: 3_600,
        },
    );
    let f = feed(Redistribution::Allowed);
    let settled = f
        .record("test.index", None, None)
        .expect("record")
        .checked
        .settled_unix;
    assert_eq!(c.observation_price(METRIC, settled, settled + 3_599), 2_500);
    assert_eq!(c.observation_price(METRIC, settled, settled + 3_600), 0);
}

/// L2API-R1-F3: a client asking for "newest" sets a freshness floor; an
/// older round is refused even though it verifies.
#[tokio::test]
async fn a_round_older_than_the_clients_floor_is_refused() {
    let f = served(false, None, Redistribution::Allowed);
    let (_, _, b) = get(&f, URI, &[]).await;
    let (t, subj) = expect();
    let ask = |floor: u64| ObservationExpect {
        template: &t,
        subject: &subj,
        window_end: None,
        min_window_end: floor,
        now: LOAD_NOW,
    };
    // The served round's window ends at 1,300.
    assert!(verify_observation(&b, &ask(1_800)).is_ok());
    assert_eq!(
        verify_observation(&b, &ask(1_801)),
        Err(VerifyError::NotRequested(
            "windowEnd older than min_window_end"
        ))
    );
    // A client clock before the round's reveal close (minus skew) refuses it.
    assert!(verify_observation(
        &b,
        &ObservationExpect {
            now: 1_980 - observations::MAX_CLOCK_SKEW_SECS - 1,
            ..ask(0)
        }
    )
    .is_err());
}

/// L2API-R1-F5: a paid observation answers no conditional request before
/// payment, and no ETag names the result hash (a guessed value could be
/// confirmed against it).
#[tokio::test]
async fn a_paid_observation_answers_no_304_before_payment() {
    let f = served(true, mock(), Redistribution::Allowed);
    let (s, h, _) = get(&f, URI, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    // Even the right ETag (and `*`) gets a 402, not a 304.
    let free = served(false, None, Redistribution::Allowed);
    let (s, fh, b) = get(&free, URI, &[]).await;
    assert_eq!(s, StatusCode::OK);
    let etag = fh[header::ETAG].to_str().expect("etag").to_string();
    assert!(!etag.contains(
        b["resultHash"]
            .as_str()
            .expect("hash")
            .trim_start_matches("0x")
    ));
    for tag in [etag.clone(), "*".to_string()] {
        let (s, _, _) = get(&f, URI, &[("if-none-match", tag)]).await;
        assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    }
    assert!(!h.contains_key(header::ETAG) || h[header::ETAG] != etag.as_str());
    // Free data still revalidates.
    let (s, _, _) = get(&free, URI, &[("if-none-match", etag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
}

mod cl7_recheck {
    //! CL7 (ADR-0057 item 6): a value answer gains the paired `.recheck`
    //! metric's note. A recheck that disagrees is a note next to the
    //! value, never a fall; the client recomputes the note from the
    //! recheck's own verified answer.
    use super::*;
    use crate::client::{verify_recheck, verify_recheck_pool};
    use onyx_core::recheck_of;

    const DAY: u64 = 86_400;
    const SUBJECT: &str = "test.daily:1970-01-01";
    /// Day 1 (the original's first allowed day) and day 3 (the recheck's).
    const START: u64 = DAY + 1_500;
    const RSTART: u64 = 3 * DAY + 1_500;
    const NOW: u64 = 4 * DAY;

    fn daily() -> FeedTemplate {
        FeedTemplate {
            feed: "test.daily.v1".into(),
            metric: MetricId("test.daily".into()),
            fetcher: keccak256(b"test fetcher"),
            url_template: "https://example.test/{date}.json".into(),
            selector: "/value".into(),
            kind: ObservationKind::Numeric {
                scale: 2,
                tolerance: 5,
            },
            window_secs: 300,
            min_observers: 3,
            rights: rights(Redistribution::Allowed),
            subject: SubjectRule::Daily {
                prefix: "test.daily".into(),
                min_days: 1,
                max_days: 1,
            },
        }
    }

    fn recheck() -> FeedTemplate {
        recheck_of(&daily(), 2, 1).expect("recheck")
    }

    fn subj() -> Subject {
        Subject::Aggregate(SUBJECT.into())
    }

    /// One honest round of `t` from `start` where every observer answers `v`.
    fn round(t: &FeedTemplate, start: u64, v: i128) -> Transcript {
        round_by(t, start, v, 1)
    }

    /// The same with observers `first..first + 3` (the test keys, ordered by first byte).
    fn round_by(t: &FeedTemplate, start: u64, v: i128, first: u8) -> Transcript {
        let spec = t.instantiate(&subj(), start).expect("allowed window");
        let cfg = ObserveConfig {
            commit_close: spec.window_end + 60,
            reveal_close: spec.window_end + 180,
            reward: 10,
            outlier_slash: 100,
            miss_slash: 20,
        };
        let observers: Vec<Observer> = (first..first + 3)
            .map(|i| Observer {
                address: obs_addr(i),
                bond: 1_000,
            })
            .collect();
        let draw =
            onyx_coordinator::observe::DrawSeed::for_pool(&observers, 10, Hash32([3; 32]), 0);
        let rid = onyx_coordinator::observe::round_id(&spec.id(), &cfg, &observers, &draw);
        let ob = Observation {
            answer: Answer::Number(v),
            observed_at: start + 10,
            page_hash: keccak256(&v.to_be_bytes()),
        };
        let mut commits = Vec::new();
        let mut reveals = Vec::new();
        for o in &observers {
            let salt = keccak256(&o.address);
            let (c, r) = test_keys::signed(
                &test_keys::ordered_secret(&o.address).expect("test key"),
                &rid,
                draw.attempt,
                &ob,
                &salt,
            );
            commits.push(c);
            reveals.push(r);
        }
        Transcript {
            subject: subj(),
            window_start: start,
            cfg,
            pool: observers.clone(),
            draw,
            observers,
            commits,
            reveals,
            sealed: Default::default(),
            answer: Answer::Number(v),
        }
    }

    fn store(t: &FeedTemplate, rounds: &[Transcript]) -> ObservationFeed {
        let text = ObservationStore::to_json(t, rounds, None).to_string();
        ObservationStore::from_json(&text, NOW, &pins(t)).expect("store loads")
    }

    /// The original value 42.00 and, when given, the recheck's value.
    fn fixture(recheck_value: Option<Option<i128>>) -> Fixture {
        let f = build(Setup::default());
        let mut c = f.state.catalog.write().expect("lock");
        c.add_observations(store(&daily(), &[round(&daily(), START, 4_200)]))
            .expect("adds");
        if let Some(v) = recheck_value {
            let r = recheck();
            let rounds: Vec<Transcript> = v.map(|v| round(&r, RSTART, v)).into_iter().collect();
            c.add_observations(store(&r, &rounds))
                .expect("adds the recheck");
        }
        drop(c);
        f
    }

    fn uri(metric: &str) -> String {
        format!("/v1/observations/{metric}?subject={SUBJECT}")
    }

    fn verify(t: &FeedTemplate, b: &Value) -> VerifiedObservation {
        let s = subj();
        verify_observation(
            b,
            &ObservationExpect {
                template: t,
                subject: &s,
                window_end: None,
                min_window_end: 0,
                now: NOW,
            },
        )
        .expect("verifies")
    }

    /// THREATS CL7-3: a client checks the recheck round's pool against the bettors (read from
    /// the contract's lists) and the original round's observers. The shared fixture's recheck
    /// reuses the original's observers, which the check refuses.
    #[tokio::test]
    async fn a_recheck_pool_with_a_bettor_or_an_original_observer_is_refused() {
        // An independent recheck: observers 4..=6; the original's are 1..=3.
        let f = build(Setup::default());
        {
            let mut c = f.state.catalog.write().expect("lock");
            c.add_observations(store(&daily(), &[round(&daily(), START, 4_200)]))
                .expect("adds");
            c.add_observations(store(&recheck(), &[round_by(&recheck(), RSTART, 4_200, 4)]))
                .expect("adds the recheck");
        }
        let (_, _, b) = get(&f, &uri("test.daily"), &[]).await;
        let (_, _, rb) = get(&f, &uri("test.daily.recheck"), &[]).await;
        let value = verify(&daily(), &b);
        let rv = verify(&recheck(), &rb);
        verify_recheck_pool(&rv, &value, &[obs_addr(9)]).expect("an independent recheck");
        let e = verify_recheck_pool(&rv, &value, &[obs_addr(9), obs_addr(5)])
            .expect_err("a bettor in the pool");
        assert!(e.to_string().contains("recheck pool"), "{e}");
        // The original round in the recheck's place: its own observers are in its pool.
        assert!(verify_recheck_pool(&value, &value, &[]).is_err());
        let f = fixture(Some(Some(4_206)));
        let (_, _, b) = get(&f, &uri("test.daily"), &[]).await;
        let (_, _, rb) = get(&f, &uri("test.daily.recheck"), &[]).await;
        assert!(
            verify_recheck_pool(&verify(&recheck(), &rb), &verify(&daily(), &b), &[]).is_err(),
            "a recheck by the original observers"
        );
    }

    #[tokio::test]
    async fn a_recheck_that_disagrees_is_a_note_never_a_fall() {
        let f = fixture(Some(Some(4_206)));
        let (s, h, b) = get(&f, &uri("test.daily"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["recheck"]["matches"], json!(false));
        assert_eq!(b["proofLevel"], json!(2), "the value keeps its level");
        assert_eq!(b["ladder"]["level"], json!(2));
        assert_eq!(b["value"], json!("42.00"));
        assert!(
            cc(&h).contains("immutable"),
            "the note is final: {}",
            cc(&h)
        );
        let value = verify(&daily(), &b);
        let (s, _, rb) = get(&f, &uri("test.daily.recheck"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{rb}");
        assert!(rb.get("recheck").is_none(), "no recheck of a recheck");
        let rv = verify(&recheck(), &rb);
        let note = verify_recheck(&b, &value, &recheck(), &rv).expect("the note holds");
        assert!(!note.matches);
        assert_eq!(note.result, rv.checked.result);
        assert_eq!(note.recorded_at, rv.checked.settled_unix);
        assert_eq!(b["recheck"]["recordedAt"], json!(rv.checked.settled_unix));
        // A server that flips the note, or names another result, is caught.
        for (k, bad) in [
            ("matches", json!(true)),
            ("result", json!(b["resultHash"].clone())),
            ("recordedAt", json!(1)),
        ] {
            let mut forged = b.clone();
            forged["recheck"][k] = bad;
            verify_observation(
                &forged,
                &ObservationExpect {
                    template: &daily(),
                    subject: &subj(),
                    window_end: None,
                    min_window_end: 0,
                    now: NOW,
                },
            )
            .expect("the shape is fine");
            assert!(
                verify_recheck(&forged, &value, &recheck(), &rv).is_err(),
                "{k}"
            );
        }
        let mut extra = b.clone();
        extra["recheck"]["level"] = json!(4);
        assert!(matches!(
            verify_observation(
                &extra,
                &ObservationExpect {
                    template: &daily(),
                    subject: &subj(),
                    window_end: None,
                    min_window_end: 0,
                    now: NOW,
                },
            ),
            Err(VerifyError::UnexpectedField(_))
        ));
        // The note cannot be recomputed from the value's own answer.
        assert!(verify_recheck(&b, &value, &recheck(), &value).is_err());
    }

    #[tokio::test]
    async fn a_recheck_within_the_tolerance_matches() {
        let f = fixture(Some(Some(4_195)));
        let (_, _, b) = get(&f, &uri("test.daily"), &[]).await;
        assert_eq!(b["recheck"]["matches"], json!(true));
    }

    /// A paired recheck feed with no round yet: no note, and the answer is
    /// not cached as immutable (it may still gain one). No paired feed:
    /// the answer is as before CL7.
    #[tokio::test]
    async fn a_pending_recheck_keeps_the_answer_revalidating() {
        let f = fixture(Some(None));
        let (s, h, b) = get(&f, &uri("test.daily"), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert!(b.get("recheck").is_none());
        assert!(!cc(&h).contains("immutable"), "{}", cc(&h));
        let f = fixture(None);
        let (_, h, b) = get(&f, &uri("test.daily"), &[]).await;
        assert!(b.get("recheck").is_none());
        assert!(cc(&h).contains("immutable"), "{}", cc(&h));
    }

    /// The ETag changes once the recheck's note is served, so a cached
    /// answer without it is not revalidated as current.
    #[tokio::test]
    async fn the_etag_names_the_recheck_once_served() {
        let (_, h1, _) = get(&fixture(None), &uri("test.daily"), &[]).await;
        let (_, h2, _) = get(&fixture(Some(Some(4_300))), &uri("test.daily"), &[]).await;
        assert_ne!(h1[header::ETAG], h2[header::ETAG]);
    }
}
