//! SEC-R2-F1 / L2-F2: a fact's rounds that settled no value are published
//! with its value round (`?outcome=all`), or alone when the fact never settled
//! one (`/v1/observations/{metric}/attempts`). A client rebuilds every round,
//! then checks each draw on chain (`drawCount`, `drawOf`, `drawBudget`), so
//! the served value round is the only value round and no round that ran is
//! hidden.
use onyx_api::client::{
    draw_count_from_return, verify_attempts, verify_attempts_onchain, verify_observation,
    DrawTally, ObservationExpect, VerifiedAttempts, VerifyError,
};
use onyx_api::observations::{
    check_attempt, Attempt, ObservationError, ObservationFeed, ObservationStore, TemplatePins,
    Transcript,
};
use onyx_coordinator::observe::{
    round_id, test_keys, DrawSeed, ObservationOutcome, ObserveConfig, Observer,
};
use onyx_core::claim::MetricId;
use onyx_core::{
    day_number, keccak256, Answer, DataRights, DateLag, FeedTemplate, Hash32, Observation,
    ObservationKind, Redistribution, SourceId, Subject, SubjectRule,
};
use serde_json::Value;

const DAY: u64 = 86_400;

fn day(d: &str) -> u64 {
    u64::try_from(day_number(d).expect("date")).expect("positive") * DAY
}

fn template() -> FeedTemplate {
    FeedTemplate {
        feed: "test.rec.v1".into(),
        metric: MetricId("test.rec".into()),
        fetcher: keccak256(b"test fetcher"),
        url_template: "https://example.test/{date}.json".into(),
        selector: "/value".into(),
        kind: ObservationKind::Numeric {
            scale: 2,
            tolerance: 0,
        },
        window_secs: 300,
        min_observers: 3,
        rights: DataRights {
            license: "CC0-1.0".into(),
            redistribution: Redistribution::Allowed,
            source: SourceId("test.page".into()),
            attribution: None,
            terms: String::new(),
        },
        subject: SubjectRule::Dated {
            prefix: "test.rec".into(),
            lag: DateLag {
                min_business_days: 1,
                max_calendar_days: 10,
                first_day: day_number("2026-01-01").expect("d"),
                last_day: day_number("2026-12-31").expect("d"),
                holidays: vec![],
            },
        },
    }
}

fn subject() -> Subject {
    Subject::Aggregate("test.rec:2026-10-08".into())
}

fn start() -> u64 {
    day("2026-10-12")
}

/// A round of `t` at draw `attempt` where the observers answer `answers`.
fn round(t: &FeedTemplate, attempt: u32, answers: [Answer; 3]) -> (Attempt, Answer) {
    let s = subject();
    let start = start() + u64::from(attempt) * 600;
    let spec = t.instantiate(&s, start).expect("lag allows it");
    let cfg = ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    };
    let observers: Vec<Observer> = (1..=3u8)
        .map(|i| Observer {
            address: test_keys::ordered(i).1,
            bond: 1_000,
        })
        .collect();
    // The pool is exactly the draw size, so the draw is the whole pool.
    let draw = DrawSeed::for_pool(
        &observers,
        10 + u64::from(attempt),
        Hash32([3; 32]),
        attempt,
    );
    let rid = round_id(&spec.id(), &cfg, &observers, &draw);
    let mut commits = vec![];
    let mut reveals = vec![];
    for (o, answer) in observers.iter().zip(answers.iter()) {
        let obs = Observation {
            answer: answer.clone(),
            observed_at: start + 10,
            // An Unavailable answer carries no page (zero hash).
            page_hash: if *answer == Answer::Unavailable {
                Hash32::ZERO
            } else {
                keccak256(b"page")
            },
        };
        let salt = keccak256(&o.address);
        let (c, r) = test_keys::signed(
            &test_keys::ordered_secret(&o.address).expect("test key"),
            &rid,
            draw.attempt,
            &obs,
            &salt,
        );
        commits.push(c);
        reveals.push(r);
    }
    let a = Attempt {
        subject: s,
        window_start: start,
        cfg,
        pool: observers.clone(),
        draw,
        observers,
        commits,
        reveals,
        sealed: Default::default(),
    };
    (a, answers[0].clone())
}

fn n(v: i128) -> Answer {
    Answer::Number(v)
}

/// Attempt 0 disagrees (no result), attempt 1 the source was down, attempt 2
/// settles 42.00.
fn history(t: &FeedTemplate) -> (Vec<Attempt>, Transcript) {
    let (a0, _) = round(t, 0, [n(4100), n(4200), n(4300)]);
    let (a1, _) = round(t, 1, [Answer::Unavailable, Answer::Unavailable, n(4200)]);
    let (v, answer) = round(t, 2, [n(4200), n(4200), n(4200)]);
    let value = Transcript {
        subject: v.subject,
        window_start: v.window_start,
        cfg: v.cfg,
        pool: v.pool,
        draw: v.draw,
        observers: v.observers,
        commits: v.commits,
        reveals: v.reveals,
        sealed: Default::default(),
        answer,
    };
    (vec![a0, a1], value)
}

fn now(t: &Transcript) -> u64 {
    t.cfg.reveal_close + 1
}

fn pins(t: &FeedTemplate) -> TemplatePins {
    TemplatePins::from([(t.metric.0.clone(), t.id())])
}

fn load(
    t: &FeedTemplate,
    rounds: &[Transcript],
    attempts: &[Attempt],
    now: u64,
) -> Result<ObservationFeed, ObservationError> {
    let text = ObservationStore::to_json_with_attempts(t, rounds, attempts, None).to_string();
    ObservationStore::from_json(&text, now, &pins(t))
}

fn expect<'a>(t: &'a FeedTemplate, s: &'a Subject, now: u64) -> ObservationExpect<'a> {
    ObservationExpect {
        template: t,
        subject: s,
        window_end: None,
        min_window_end: 0,
        now,
    }
}

/// The served `?outcome=all` body of the fact.
fn body_all(feed: &ObservationFeed) -> Value {
    let r = feed
        .record("test.rec:2026-10-08", None, None)
        .expect("record");
    feed.body_all(r, &feed.attempts_of(&r.checked.fact))
}

fn word(v: u64) -> Vec<u8> {
    let mut w = vec![0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// What `drawOf` returns for a draw.
fn draw_of(commit: Hash32, beacon_height: u64, announced_at: u64, beacon: Hash32) -> Vec<u8> {
    let mut r = commit.0.to_vec();
    r.extend(word(beacon_height));
    r.extend(word(announced_at));
    r.extend_from_slice(&beacon.0);
    r
}

fn budget(strikes: u64, allowance: u64) -> Vec<u8> {
    let mut r = word(strikes);
    r.extend(word(allowance));
    r
}

/// The chain as an honest coordinator left it: one `drawOf` per attempt
/// without a value, matching the published round.
fn honest_draws(a: &VerifiedAttempts) -> Vec<Vec<u8>> {
    a.attempts
        .iter()
        .map(|c| {
            draw_of(
                c.onchain.draw_commit(),
                c.draw.beacon_height,
                1,
                c.draw.beacon,
            )
        })
        .collect()
}

fn verified(t: &FeedTemplate) -> (VerifiedAttempts, u64) {
    let (attempts, value) = history(t);
    let at = now(&value);
    let feed = load(t, std::slice::from_ref(&value), &attempts, at).expect("loads");
    let s = subject();
    let v = verify_observation(&body_all(&feed), &expect(t, &s, at)).expect("verifies");
    (v.attempts.expect("asked for all"), at)
}

#[test]
fn every_round_of_a_fact_is_served_and_checks_on_chain() {
    let t = template();
    let (a, _) = verified(&t);
    assert_eq!(a.value_attempt, Some(2));
    assert_eq!(a.attempts.len(), 2);
    assert_eq!(a.attempts[0].outcome, ObservationOutcome::NoResult);
    assert_eq!(a.attempts[1].outcome, ObservationOutcome::SourceUnavailable);
    assert_eq!(a.draws_without_value(3), 2);
    assert_eq!(draw_count_from_return(&word(3)), Ok(3));
    let tally = verify_attempts_onchain(
        &a,
        &word(3),
        &honest_draws(&a),
        &budget(2, 3),
        &complete(&a),
        &unsealed_all(&a),
    )
    .expect("the chain agrees");
    assert_eq!(
        tally,
        DrawTally {
            draws: 3,
            without_value: 2,
            published: 2
        }
    );
    // Calldata selectors.
    assert_eq!(a.draw_count_calldata().len(), 4 + 32);
    assert_eq!(a.draw_of_calldata(1).len(), 4 + 64);
    assert_eq!(a.draw_budget_calldata().len(), 4 + 64);
}

#[test]
fn a_hidden_round_or_a_wrong_chain_answer_is_refused() {
    let t = template();
    let (a, _) = verified(&t);
    let draws = honest_draws(&a);
    let chain = |a: &VerifiedAttempts, count: u64, d: &[Vec<u8>], strikes: u64| {
        verify_attempts_onchain(
            a,
            &word(count),
            d,
            &budget(strikes, 3),
            &complete(a),
            &unsealed_all(a),
        )
    };
    // The value round is not the latest draw.
    assert!(matches!(
        chain(&a, 4, &draws, 2),
        Err(VerifyError::Malformed(_) | VerifyError::ChainMismatch(_))
    ));
    // A dated subject's strikes are exactly its draws without a value.
    assert_eq!(
        chain(&a, 3, &draws, 1),
        Err(VerifyError::ChainMismatch("drawBudget"))
    );
    assert_eq!(
        chain(&a, 3, &draws, 3),
        Err(VerifyError::ChainMismatch("drawBudget"))
    );
    // Another draw on chain than the published round's.
    let mut other = draws.clone();
    other[1] = draw_of(
        Hash32([9; 32]),
        a.attempts[1].draw.beacon_height,
        1,
        a.attempts[1].draw.beacon,
    );
    assert_eq!(
        chain(&a, 3, &other, 2),
        Err(VerifyError::ChainMismatch("drawOf"))
    );
    // Announced at or after its beacon height.
    let mut late = draws.clone();
    let c = &a.attempts[0];
    late[0] = draw_of(
        c.onchain.draw_commit(),
        c.draw.beacon_height,
        c.draw.beacon_height,
        c.draw.beacon,
    );
    assert_eq!(
        chain(&a, 3, &late, 2),
        Err(VerifyError::ChainMismatch("drawOf"))
    );
    // The server hides attempt 1, whose beacon the chain fixed: a round ran.
    let mut hidden = a.clone();
    hidden.attempts.remove(1);
    assert_eq!(
        chain(&hidden, 3, &draws, 2),
        Err(VerifyError::ChainMismatch(
            "a draw with a fixed beacon and no published round"
        ))
    );
    // ... but a draw whose beacon was never fixed had no committee: fine.
    let mut unfixed = draws.clone();
    unfixed[1] = draw_of(Hash32([9; 32]), 11, 1, Hash32::ZERO);
    let tally = chain(&hidden, 3, &unfixed, 2).expect("an unfixed draw has no round");
    assert_eq!((tally.without_value, tally.published), (2, 1));
    // Too few drawOf returns.
    assert!(matches!(
        chain(&a, 3, &draws[..1], 2),
        Err(VerifyError::Malformed(_))
    ));
}

#[test]
fn the_store_refuses_attempts_that_break_the_draw_order() {
    let t = template();
    let (attempts, value) = history(&t);
    let at = now(&value);
    // An attempt at the value round's draw, or after it.
    let (late, _) = round(&t, 3, [n(1), n(2), n(3)]);
    let (same, _) = round(&t, 2, [n(1), n(2), n(3)]);
    for bad in [late, same] {
        let e = load(
            &t,
            std::slice::from_ref(&value),
            &[attempts[0].clone(), bad],
            at + 1_200,
        );
        assert!(matches!(e, Err(ObservationError::Malformed(_))), "{e:?}");
    }
    // The same attempt twice.
    let e = load(
        &t,
        std::slice::from_ref(&value),
        &[attempts[0].clone(), attempts[0].clone()],
        at,
    );
    assert!(matches!(e, Err(ObservationError::Malformed(_))), "{e:?}");
    // A round whose reveals settle a value is not an attempt.
    let (valued, _) = round(&t, 0, [n(4200), n(4200), n(4200)]);
    assert_eq!(
        check_attempt(&t, &valued, at),
        Err(ObservationError::Malformed(
            "an attempt whose reveals settle a value".into()
        ))
    );
    // A store without attempts keeps its old shape.
    let plain = ObservationStore::to_json(&t, std::slice::from_ref(&value), None);
    assert!(plain.get("attempts").is_none());
}

/// An edit of a served body.
type Edit = fn(&mut Value);

#[test]
fn the_client_refuses_edited_attempts() {
    let t = template();
    let (attempts, value) = history(&t);
    let at = now(&value);
    let feed = load(&t, std::slice::from_ref(&value), &attempts, at).expect("loads");
    let s = subject();
    let good = body_all(&feed);
    let edits: [(&str, Edit); 4] = [
        ("reordered", |b| {
            b["attempts"].as_array_mut().expect("arr").reverse();
        }),
        ("outcome renamed", |b| {
            b["attempts"][0]["outcome"] = "sourceUnavailable".into();
        }),
        ("result hash", |b| {
            b["attempts"][1]["resultHash"] = Hash32([1; 32]).to_string().into();
        }),
        ("unknown field", |b| {
            b["attempts"][0]["note"] = "x".into();
        }),
    ];
    for (what, edit) in edits {
        let mut b = good.clone();
        edit(&mut b);
        assert!(
            verify_observation(&b, &expect(&t, &s, at)).is_err(),
            "{what}"
        );
    }
    // Without `attempts` the answer is the plain value answer.
    let mut plain = good.clone();
    plain.as_object_mut().expect("obj").remove("attempts");
    let v = verify_observation(&plain, &expect(&t, &s, at)).expect("verifies");
    assert!(v.attempts.is_none());
}

#[test]
fn a_fact_without_a_value_is_served_alone_and_checks() {
    let t = template();
    let (attempts, value) = history(&t);
    let at = now(&value);
    let feed = load(&t, &[], &attempts, at).expect("loads");
    let fact = feed
        .attempt_fact("test.rec:2026-10-08", None)
        .expect("indexed");
    assert!(!feed.has_value(&fact));
    let body = feed.attempts_body(&feed.attempts_of(&fact), feed.fact_closed(&fact));
    let s = subject();
    let a = verify_attempts(&body, &expect(&t, &s, at)).expect("verifies");
    assert_eq!(a.value_attempt, None);
    // Two draws, both without a value, both published.
    let tally = verify_attempts_onchain(
        &a,
        &word(2),
        &honest_draws(&a),
        &budget(2, 3),
        &complete(&a),
        &unsealed_all(&a),
    )
    .expect("the chain agrees");
    assert_eq!(
        (tally.draws, tally.without_value, tally.published),
        (2, 2, 2)
    );
    // A third draw announced and fixed but not published is caught.
    let mut more = honest_draws(&a);
    more.push(draw_of(Hash32([5; 32]), 20, 1, Hash32([6; 32])));
    assert!(verify_attempts_onchain(
        &a,
        &word(3),
        &more,
        &budget(3, 3),
        &complete(&a),
        &unsealed_all(&a)
    )
    .is_err());
    // Another subject's client refuses it.
    let other = Subject::Aggregate("test.rec:2026-10-07".into());
    assert!(matches!(
        verify_attempts(&body, &expect(&t, &other, at)),
        Err(VerifyError::NotRequested(_))
    ));
}

/// `transcriptComplete` returns of an untampered log: true for every
/// published round (ADR-0027 amendment 8).
fn complete(a: &onyx_api::client::VerifiedAttempts) -> Vec<Vec<u8>> {
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    vec![yes; a.attempts.len()]
}

/// ROADMAP C11: what the registry answers to each published round's sealed
/// calls when its draw has no reveal key (no sealed post) and the round is
/// recorded.
fn unsealed_all(a: &onyx_api::client::VerifiedAttempts) -> Vec<onyx_api::client::SealedRets> {
    a.attempts
        .iter()
        .map(|c| onyx_api::client::honest_sealed_returns(&c.reveal_log, None, true, true))
        .collect()
}
