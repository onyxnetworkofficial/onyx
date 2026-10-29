//! ATT-R1 (post-merge review of the level-2 attempts feature): the free
//! `/attempts` answer must not give away a value a later round sells
//! (ATT-R1-F2), and that a substituted round is refused now that observers
//! sign their commits and reveals (ATT-R1-F3).
//! Helpers follow level2_attempts.rs.
use onyx_api::client::{
    verify_attempts, verify_attempts_onchain, verify_observation, ObservationExpect,
    VerifiedAttempts, VerifyError,
};
use onyx_api::observations::{
    Attempt, ObservationError, ObservationFeed, ObservationStore, TemplatePins, Transcript,
};
use onyx_coordinator::observe::{
    result_hash_of, round_id, sign_commit, sign_reveal, sign_reveal_receipt, test_keys, DrawSeed,
    ObservationOutcome, ObserveConfig, Observer, SimulatedTranscriptLog,
};
use onyx_core::claim::MetricId;
use onyx_core::{
    day_number, keccak256, observation_commit, Answer, DataRights, DateLag, FeedTemplate, Hash32,
    Observation, ObservationKind, Redistribution, SourceId, Subject, SubjectRule,
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
    let start = start() + u64::from(attempt) * 600;
    round_at(t, subject(), start, attempt, answers)
}

/// [`round`] for `s` in the window that starts at `start`.
fn round_at(
    t: &FeedTemplate,
    s: Subject,
    start: u64,
    attempt: u32,
    answers: [Answer; 3],
) -> (Attempt, Answer) {
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

/// Every observation, salt and page hash of `attempts`, as hex: what a free
/// answer must not carry before the fact is closed.
fn secrets(attempts: &[Attempt]) -> Vec<String> {
    let mut v = vec![];
    for a in attempts {
        for (_, o, salt, _) in &a.reveals {
            v.push(hex::encode(borsh::to_vec(o).expect("borsh")));
            v.push(hex::encode(salt.0));
            if o.page_hash != Hash32::ZERO {
                v.push(hex::encode(o.page_hash.0));
            }
        }
    }
    v
}

fn as_transcript(a: Attempt, answer: Answer) -> Transcript {
    Transcript {
        subject: a.subject,
        window_start: a.window_start,
        cfg: a.cfg,
        pool: a.pool,
        draw: a.draw,
        observers: a.observers,
        commits: a.commits,
        reveals: a.reveals,
        sealed: Default::default(),
        answer,
    }
}

/// ATT-R1-F2: before the value lands, the free `/attempts` answer of a dated
/// fact is sealed: commits, who revealed and the outcome, no observation,
/// salt or page hash. The client still checks draw, committee, commits and
/// round id, and the chain check still passes. Once the value is recorded,
/// the paid `?outcome=all` answer carries every reveal.
#[test]
fn the_free_answer_does_not_leak_the_value_a_later_round_sells() {
    let t = template();
    let (attempts, value) = history(&t);
    let at = now(&value);
    let before = load(&t, &[], &attempts, at).expect("loads");
    let fact = before
        .attempt_fact("test.rec:2026-10-08", None)
        .expect("indexed");
    assert!(!before.has_value(&fact));
    assert!(!before.fact_closed(&fact), "a dated fact may still settle");
    let free = before.attempts_body(&before.attempts_of(&fact), before.fact_closed(&fact));
    let text = free.to_string();
    for s in secrets(&attempts) {
        assert!(!text.contains(&s), "the free answer leaks {s}");
    }
    for a in free["attempts"].as_array().expect("attempts") {
        assert!(a.get("reveals").is_none());
        assert_eq!(a["revealed"].as_array().expect("revealed").len(), 3);
    }
    let s = subject();
    let v = verify_attempts(&free, &expect(&t, &s, at)).expect("a sealed answer verifies");
    assert!(v.attempts.iter().all(|c| c.sealed));
    assert_eq!(
        v.attempts
            .iter()
            .map(|c| c.outcome.clone())
            .collect::<Vec<_>>(),
        [
            ObservationOutcome::NoResult,
            ObservationOutcome::SourceUnavailable
        ]
    );
    verify_attempts_onchain(
        &v,
        &word(2),
        &honest_draws(&v),
        &budget(2, 3),
        &complete(&v),
        &unsealed_all(&v),
    )
    .expect("the chain agrees with a sealed answer");
    // A sealed round still binds its round id, result hash and outcome.
    type Edit = (&'static str, fn(&mut Value));
    let edits: [Edit; 4] = [
        ("an outcome named value", |b| {
            b["attempts"][0]["outcome"] = "value".into()
        }),
        ("another outcome", |b| {
            b["attempts"][0]["outcome"] = "sourceUnavailable".into()
        }),
        ("a revealer that never committed", |b| {
            b["attempts"][0]["revealed"][0] = format!("0x{}", "99".repeat(20)).into()
        }),
        ("reveals and revealed", |b| {
            b["attempts"][0]["reveals"] = serde_json::json!([])
        }),
    ];
    for (what, edit) in edits {
        let mut b = free.clone();
        edit(&mut b);
        assert!(verify_attempts(&b, &expect(&t, &s, at)).is_err(), "{what}");
    }
    // After the value: the paid answer carries the reveals, and they check.
    let after = load(&t, std::slice::from_ref(&value), &attempts, at).expect("loads");
    assert!(after.fact_closed(&fact));
    let all = body_all(&after);
    assert_eq!(all["value"], "42.00");
    let v = verify_observation(&all, &expect(&t, &s, at)).expect("verifies");
    let a = v.attempts.expect("asked for all");
    assert!(a.attempts.iter().all(|c| !c.sealed));
    // The paid answer never accepts a sealed round: the fact is closed.
    let mut sealed_all = all.clone();
    sealed_all["attempts"] = free["attempts"].clone();
    assert!(verify_observation(&sealed_all, &expect(&t, &s, at)).is_err());
}

/// ATT-R1-F2: a fixed fact closes on its first settled round, so its rounds
/// without a value are served with their reveals, a store that also holds a
/// value for it is refused, and a client refuses a sealed answer for it.
#[test]
fn a_fixed_fact_without_a_value_is_closed_and_served_open() {
    let mut t = template();
    t.url_template = "https://example.test/latest.json".into();
    let s = Subject::Aggregate("test.fixed".into());
    t.subject = SubjectRule::Fixed(s.clone());
    let begin = start();
    let (a0, _) = round_at(&t, s.clone(), begin, 0, [n(4100), n(4200), n(4300)]);
    let (v1, answer) = round_at(&t, s.clone(), begin, 1, [n(4200), n(4200), n(4200)]);
    let value = as_transcript(v1, answer);
    let at = now(&value);
    let feed = load(&t, &[], std::slice::from_ref(&a0), at).expect("loads");
    let fact = feed.attempt_fact("test.fixed", None).expect("indexed");
    assert!(feed.fact_closed(&fact));
    let free = feed.attempts_body(&feed.attempts_of(&fact), feed.fact_closed(&fact));
    assert!(free["attempts"][0].get("reveals").is_some());
    let e = ObservationExpect {
        template: &t,
        subject: &s,
        window_end: None,
        min_window_end: 0,
        now: at,
    };
    let v = verify_attempts(&free, &e).expect("verifies");
    assert!(!v.attempts[0].sealed);
    // The same rounds sealed: refused for a fixed fact.
    let dated = template();
    let (d0, _) = round(&dated, 0, [n(4100), n(4200), n(4300)]);
    let dfeed = load(&dated, &[], std::slice::from_ref(&d0), at).expect("loads");
    let dfact = dfeed
        .attempt_fact("test.rec:2026-10-08", None)
        .expect("indexed");
    let mut sealed = free.clone();
    sealed["attempts"][0]["revealed"] =
        dfeed.attempts_body(&dfeed.attempts_of(&dfact), false)["attempts"][0]["revealed"].clone();
    sealed["attempts"][0]
        .as_object_mut()
        .expect("obj")
        .remove("reveals");
    assert!(verify_attempts(&sealed, &e).is_err());
    // A value round for this fixed fact would sell what was given away.
    assert!(matches!(
        load(
            &t,
            std::slice::from_ref(&value),
            std::slice::from_ref(&a0),
            at
        ),
        Err(ObservationError::Malformed(_))
    ));
}

/// ATT-R1-F3 (FIXED): every commit and reveal is signed by its drawn
/// observer's own key, so a coordinator that hides a dissenting round for a
/// fixed-beacon draw cannot publish a made-up round for the same draw (here
/// all `Unavailable`, which once passed every client check): the store
/// refuses it at load, and a client refuses it when a server serves it anyway.
#[test]
fn a_substituted_round_for_a_fixed_draw_is_refused() {
    let t = template();
    let (real, value) = history(&t);
    let at = now(&value);
    let s = subject();
    let real_feed = load(&t, std::slice::from_ref(&value), &real, at).expect("loads");
    let good = body_all(&real_feed);
    let real_v = verify_observation(&good, &expect(&t, &s, at)).expect("honest verifies");
    let chain_draws = honest_draws(&real_v.attempts.clone().expect("all"));
    let real_a = real_v.attempts.expect("all");
    verify_attempts_onchain(
        &real_a,
        &word(3),
        &chain_draws,
        &budget(2, 3),
        &complete(&real_a),
        &unsealed_all(&real_a),
    )
    .expect("the honest history passes on chain");

    let r0 = &real[0];
    let spec = t.instantiate(&r0.subject, r0.window_start).expect("spec");
    let rid = round_id(&spec.id(), &r0.cfg, &r0.observers, &r0.draw);
    // The coordinator's own key (fake): it holds no observer's key.
    let coordinator = keccak256(b"onyx test coordinator key").0;
    let unavailable = Observation {
        answer: Answer::Unavailable,
        observed_at: r0.window_start + 10,
        page_hash: Hash32::ZERO,
    };
    // 1. A made-up round signed with the coordinator's key.
    let mut own_key = r0.clone();
    own_key.commits.clear();
    own_key.reveals.clear();
    for o in &r0.observers {
        let salt = keccak256(&o.address);
        let h = observation_commit(&rid, &o.address, &unavailable, &salt);
        let cs = sign_commit(&coordinator, &rid, r0.draw.attempt, &h).expect("sign");
        let rs =
            sign_reveal(&coordinator, &rid, r0.draw.attempt, &unavailable, &salt).expect("sign");
        own_key.commits.push((o.address, h, cs));
        own_key
            .reveals
            .push((o.address, unavailable.clone(), salt, rs));
    }
    // 2. The observers' real signatures copied onto the made-up round.
    let mut copied = own_key.clone();
    for (f, r) in copied.commits.iter_mut().zip(&r0.commits) {
        f.2 = r.2;
    }
    for (f, r) in copied.reveals.iter_mut().zip(&r0.reveals) {
        f.3 = r.3;
    }
    // 3. The real signed commits under made-up reveals (they open nothing).
    let mut real_commits = own_key.clone();
    real_commits.commits = r0.commits.clone();
    // 4. The real round re-signed for another attempt of the same fact.
    let mut other_attempt = r0.clone();
    for ((a, h, sig), o) in other_attempt.commits.iter_mut().zip(&r0.observers) {
        let sk = test_keys::ordered_secret(&o.address).expect("test key");
        *sig = sign_commit(&sk, &rid, r0.draw.attempt + 1, h).expect("sign");
        assert_eq!(*a, o.address);
    }
    let unavailable_result =
        result_hash_of(&spec.id(), &rid, &ObservationOutcome::SourceUnavailable);
    // The served body with attempt 0's commits and reveals taken from `r`, as
    // a server that skips its own load check would serve it.
    let served = |r: &Attempt, unavailable: bool| {
        let store: Value = serde_json::from_str(
            &ObservationStore::to_json_with_attempts(
                &t,
                std::slice::from_ref(&value),
                &[r.clone(), real[1].clone()],
                None,
            )
            .to_string(),
        )
        .expect("json");
        let mut b = good.clone();
        b["attempts"][0]["commits"] = store["attempts"][0]["commits"].clone();
        b["attempts"][0]["reveals"] = store["attempts"][0]["reveals"].clone();
        if unavailable {
            b["attempts"][0]["outcome"] = "sourceUnavailable".into();
            b["attempts"][0]["resultHash"] = unavailable_result.to_string().into();
        }
        b
    };
    // Control: the splice alone keeps the honest round verifiable.
    verify_observation(&served(r0, false), &expect(&t, &s, at)).expect("control verifies");
    for (what, forged, unavailable) in [
        ("own key", own_key, true),
        ("copied signatures", copied, true),
        ("real commits", real_commits, true),
        ("another attempt", other_attempt, false),
    ] {
        let published = [forged.clone(), real[1].clone()];
        assert!(
            load(&t, std::slice::from_ref(&value), &published, at).is_err(),
            "{what}: refused at load"
        );
        let e = verify_observation(&served(&forged, unavailable), &expect(&t, &s, at))
            .expect_err("refused by the client");
        assert!(
            e.to_string().contains("signed") || e.to_string().contains("reveal"),
            "{what}: {e}"
        );
    }
}

/// SIG-R1-F1 (THREATS SEC-R2-F1-P2), closed by the transcript log (ADR-0027
/// amendment 8): a server withholds two of three honest signed reveals of a
/// unanimous value round, which turns it into `NoResult` (quorum is over all
/// drawn observers), publishes it as an earlier attempt with valid
/// signatures, re-draws, and serves another value. Store and client checks
/// pass: withholding needs no signature. But the coordinator's receipt key
/// signed a receipt for each accepted reveal, and the observers posted them:
/// `transcriptComplete` is false for the published round, so the client
/// refuses the fact. Residual: an observer that did not post its receipt (or
/// never got one) can still be dropped; the re-draw then costs one of the
/// fact's 3 draws, and the dropped observers are never slashed
/// (`observe::tests::a_withheld_honest_reveal_is_never_slashed`).
#[test]
fn a_withheld_receipted_reveal_is_caught_on_chain() {
    let t = template();
    let (mut a0, _) = round(&t, 0, [n(4200), n(4200), n(4200)]);
    let full = onyx_api::observations::check_attempt(&t, &a0, a0.cfg.reveal_close + 1);
    assert!(
        full.is_err(),
        "the full round settles a value: never an attempt"
    );
    a0.reveals.truncate(1);
    let c = onyx_api::observations::check_attempt(&t, &a0, a0.cfg.reveal_close + 1)
        .expect("a round with withheld reveals checks");
    assert_eq!(c.outcome, ObservationOutcome::NoResult);
    let (v1, answer) = round(&t, 1, [n(4300), n(4300), n(4300)]);
    let value = as_transcript(v1, answer);
    let at = now(&value);
    let feed = load(
        &t,
        std::slice::from_ref(&value),
        std::slice::from_ref(&a0),
        at,
    )
    .expect("the store accepts it");
    let s = subject();
    let v =
        verify_observation(&body_all(&feed), &expect(&t, &s, at)).expect("the client accepts it");
    assert_eq!(v.value, "43.00");
    let att = v.attempts.expect("all");
    assert_eq!(att.attempts.len(), 1, "the fact used 2 of its 3 draws");
    // Nothing posted: the chain cannot tell (the residual).
    let mut log = SimulatedTranscriptLog::default();
    log.signers
        .insert((att.registry_fact, 0), test_keys::receipt_signer());
    let q = &att.attempts[0].log;
    assert!(log.complete(q));
    // Every observer got a receipt when its reveal was accepted and posted it.
    for (o, commit, _) in &a0.commits {
        let r = sign_reveal_receipt(&test_keys::receipt_secret(), &c.round_id, 0, o, commit)
            .expect("sign");
        log.post_receipt(&att.registry_fact, 0, o, &c.round_id, commit, &r)
            .expect("post");
    }
    assert!(!log.complete(q), "two receipted reveals are missing");
    let mut no = vec![0u8; 32];
    no[31] = u8::from(log.complete(q));
    assert!(matches!(
        verify_attempts_onchain(
            &att,
            &word(2),
            &honest_draws(&att),
            &budget(1, 3),
            &[no],
            &unsealed_all(&att)
        ),
        Err(VerifyError::ChainMismatch(_))
    ));
}

/// Review REVLOG LOW-3, KNOWN LIMIT (THREATS REVLOG-5): while a round is
/// served sealed (its fact not closed, ATT-R1-F2), the `revealed` list
/// stands in for the reveals and is the server's word. A server withholds
/// two of three receipted reveals of a unanimous round, serves it sealed as
/// `NoResult` and lists all three as revealed: `transcriptComplete` is
/// TRUE, so the client accepts it until the fact is closed. Unsealed, the
/// same round (with only the reveal it kept) is refused on chain, and the
/// full round never checks as an attempt (it settles a value).
#[test]
fn known_limit_a_sealed_round_hides_withheld_reveals_until_unsealed() {
    let t = template();
    let (a0, _) = round(&t, 0, [n(4200), n(4200), n(4200)]);
    let now = a0.cfg.reveal_close + 1;
    let all: Vec<_> = a0.commits.iter().map(|(o, _, _)| *o).collect();
    let mut sealed = a0.clone();
    sealed.reveals.clear();
    let c = onyx_api::observations::check_sealed_attempt(
        &t,
        &sealed,
        &all,
        ObservationOutcome::NoResult,
        now,
    )
    .expect("the sealed round's outcome is the server's word");
    let mut log = SimulatedTranscriptLog::default();
    log.signers
        .insert((c.registry_fact, 0), test_keys::receipt_signer());
    for (o, commit, _) in &a0.commits {
        let r = sign_reveal_receipt(&test_keys::receipt_secret(), &c.round_id, 0, o, commit)
            .expect("sign");
        log.post_receipt(&c.registry_fact, 0, o, &c.round_id, commit, &r)
            .expect("post");
    }
    assert!(
        log.complete(&c.log),
        "the gap: sealed, the withholding passes"
    );
    // Unsealed (the fact closed), the round must come with its reveals.
    let mut kept = a0.clone();
    kept.reveals.truncate(1);
    let open = onyx_api::observations::check_attempt(&t, &kept, now).expect("checks");
    assert!(!log.complete(&open.log), "unsealed: refused on chain");
    assert!(onyx_api::observations::check_attempt(&t, &a0, now).is_err());
}

/// The website's level-2 round summaries, as committed (unsealed).
fn site_rounds() -> Vec<Value> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/static/level2");
    let mut out = vec![];
    for f in ["debt-to-penny.json", "sec-holdings.json"] {
        let d: Value =
            serde_json::from_str(&std::fs::read_to_string(format!("{dir}/{f}")).expect("read"))
                .expect("json");
        match d.get("companies") {
            Some(c) => out.extend(c.as_array().expect("companies").iter().cloned()),
            None => out.push(d),
        }
    }
    out
}

/// Every form a round's result hash could take in a shipped file.
fn result_hash_forms(rounds: &[Value]) -> Vec<Vec<u8>> {
    let mut v = vec![];
    for r in rounds {
        let h = r["result_hash"]
            .as_str()
            .expect("an unsealed round has its result hash");
        let h = h.trim_start_matches("0x");
        v.push(h.to_lowercase().into_bytes());
        v.push(h.to_uppercase().into_bytes());
        v.push(hex::decode(h).expect("hex"));
    }
    v
}

/// The committed ETF holdings file (ROADMAP P2c). `onyx-source-edgar`'s
/// `the_site_file_is_the_summary_of_the_recordings` pins it to the summary of
/// the recorded data.sec.gov responses.
const ETF_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/web/static/level2/etf-holdings.json"
);

/// Keys a level-2 round carries; none may appear in the unsealed ETF file.
const ROUND_KEYS: [&str; 11] = [
    "round_id",
    "value",
    "sealed",
    "reveals",
    "commits",
    "statuses",
    "agreed",
    "response_body",
    "result_hash",
    "draw",
    "salt",
];

/// The shape that keeps the public-build exemption to SEC filing data: level 2,
/// the SEC source, every fund read from data.sec.gov `companyconcept` under an
/// ETF metric, every filing linked on www.sec.gov, and no round key anywhere.
fn etf_shape(d: &Value) -> Result<(), String> {
    fn keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    out.push(k.clone());
                    keys(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
            _ => {}
        }
    }
    let mut ks = vec![];
    keys(d, &mut ks);
    if let Some(k) = ks.iter().find(|k| ROUND_KEYS.contains(&k.as_str())) {
        return Err(format!("carries round field {k:?}"));
    }
    if d["level"] != 2
        || d["source"]["name"]
            .as_str()
            .is_none_or(|n| !n.starts_with("SEC EDGAR"))
    {
        return Err("not a level-2 SEC EDGAR file".into());
    }
    let funds = d["funds"]
        .as_array()
        .filter(|f| !f.is_empty())
        .ok_or("no funds")?;
    for f in funds {
        let sec = f["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("https://data.sec.gov/api/xbrl/companyconcept/CIK"))
            && f["metric"]
                .as_str()
                .is_some_and(|m| m.starts_with("sec.edgar.etf_btc_sats."))
            && f["holdings"].as_array().is_some_and(|h| {
                h.iter().all(|x| {
                    x["filing_url"]
                        .as_str()
                        .is_some_and(|u| u.starts_with("https://www.sec.gov/Archives/edgar/data/"))
                })
            });
        if !sec {
            return Err(format!("fund {} is not SEC filing data", f["key"]));
        }
    }
    Ok(())
}

/// A shipped `level2/etf-holdings.json` holds exactly the committed file's
/// JSON value (`scripts/build-web.sh` strips whitespace, W4) and has
/// [`etf_shape`].
fn etf_file_is_sec_filing_data(bytes: &[u8]) -> Result<(), String> {
    let committed: Value =
        serde_json::from_slice(&std::fs::read(ETF_FILE).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let shipped: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if shipped != committed {
        return Err("differs from the committed SEC filing summary".into());
    }
    etf_shape(&shipped)
}

/// ROADMAP P2c review (THREATS ETF-10): the one unsealed level-2 file a public
/// build may ship is the committed SEC filing summary; an edited copy, a round
/// field, another source or a fund read from elsewhere is refused.
#[test]
fn the_public_build_etf_exemption_is_limited_to_sec_filing_data() {
    let text = std::fs::read_to_string(ETF_FILE).expect("etf file");
    etf_file_is_sec_filing_data(text.as_bytes()).expect("committed file passes");
    let doc: Value = serde_json::from_str(&text).expect("json");
    let minified = serde_json::to_vec(&doc).expect("json");
    etf_file_is_sec_filing_data(&minified).expect("the build's minified copy passes");
    let edited = text.replacen("\"held_sats\": \"", "\"held_sats\": \"1", 1);
    assert_ne!(edited, text);
    assert!(
        etf_file_is_sec_filing_data(edited.as_bytes()).is_err(),
        "edited bytes"
    );
    type Edit = fn(&mut Value);
    let edits: [(&str, Edit); 8] = [
        ("value", |d| d["value"] = "1".into()),
        ("result hash", |d| {
            d["funds"][0]["result_hash"] = "0x00".into()
        }),
        ("reveals", |d| {
            d["funds"][0]["holdings"][0]["reveals"] = Value::Array(vec![])
        }),
        ("level", |d| d["level"] = 3.into()),
        ("source", |d| d["source"]["name"] = "Treasury".into()),
        ("host", |d| {
            d["funds"][1]["url"] = "https://evil.example/x.json".into()
        }),
        ("metric", |d| {
            d["funds"][2]["metric"] = "btc.entity-reserve.x".into()
        }),
        ("filing", |d| {
            d["funds"][0]["holdings"][0]["filing_url"] = "https://evil.example/".into()
        }),
    ];
    for (name, edit) in edits {
        let mut d = doc.clone();
        edit(&mut d);
        assert!(etf_shape(&d).is_err(), "{name}");
    }
    let mut empty = doc.clone();
    empty["funds"] = Value::Array(vec![]);
    assert!(etf_shape(&empty).is_err(), "no funds");
}

/// SIG-R1-F2: the result hash is an unsalted function of the value, so a
/// sealed round that ships it gives a small value away by enumeration (the
/// review recovered both SEC values in seconds). The seal transform drops it;
/// with `ONYX_PUBLIC_BUILD_DIRS` (`ONYX_PUBLIC=1 scripts/build-web.sh`) no
/// shipped file (the level-2 JSON, the WebAssembly verifier's pins, the
/// scripts) holds a sealed round's result hash or value. THREATS NS5-8
/// (ADR-0048 amendment 1): the rounds and level history of free public
/// sources (Treasury, USGS, NCEI) ship whole; only the SEC holdings are sealed.
#[test]
fn a_public_build_ships_no_level2_value_or_result_hash() {
    let rounds = site_rounds();
    for r in &rounds {
        let mut sealed = r.clone();
        onyx_api::observations::seal_site_round(&mut sealed, vec![]);
        assert!(sealed.get("result_hash").is_none(), "{}", r["round_id"]);
        assert_eq!(sealed["value"], "");
    }
    let Ok(dirs) = std::env::var("ONYX_PUBLIC_BUILD_DIRS") else {
        return;
    };
    let sec: Vec<Value> = rounds
        .iter()
        .filter(|r| r.get("company").is_some() || r.get("key").is_some())
        .cloned()
        .collect();
    assert!(!sec.is_empty(), "the SEC rounds are found");
    let forms = result_hash_forms(&sec);
    let mut files = 0;
    // Relative to the repository root, as `scripts/build-web.sh` passes them.
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut stack: Vec<std::path::PathBuf> = dirs.split(':').map(|d| root.join(d)).collect();
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).expect("dir") {
                stack.push(e.expect("entry").path());
            }
            continue;
        }
        let bytes = std::fs::read(&p).expect("read");
        files += 1;
        for f in &forms {
            assert!(
                !bytes.windows(f.len()).any(|w| w == f.as_slice()),
                "{} ships a level-2 result hash",
                p.display()
            );
        }
        if p.parent().is_some_and(|d| d.ends_with("level2")) {
            let d: Value = serde_json::from_slice(&bytes).expect("json");
            // ROADMAP P2c (loop decision 2026-10-19, docs/TRACKER.md; THREATS ETF-10): the ETF
            // holdings file is public SEC filing data from recorded fetches, not a round the paid
            // API sells, so it ships unsealed, but only with the committed file's JSON value.
            if p.ends_with("level2/etf-holdings.json") {
                etf_file_is_sec_filing_data(&bytes)
                    .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
                continue;
            }
            // NS5-8 (ADR-0048 amendment 1): the level history of the Base Sepolia values (free
            // public sources) ships whole: value key and step evidence.
            if p.ends_with("level2/ladders-base-sepolia.json") {
                let vs = d["values"].as_array().expect("values");
                assert!(!vs.is_empty(), "{}", p.display());
                for v in vs {
                    let l = &v["ladder"];
                    assert!(l.get("sealed").is_none(), "{}", p.display());
                    assert!(l.get("value").is_some(), "{}", p.display());
                    let steps = l["steps"].as_array().expect("steps");
                    assert!(
                        !steps.is_empty() && steps.iter().all(|s| s.get("evidence").is_some()),
                        "{}",
                        p.display()
                    );
                }
                continue;
            }
            // NS5-8: the rounds of free public sources ship whole, with their value.
            if ["debt-to-penny.json", "usgs-quakes.json", "ncei-tmax.json"]
                .iter()
                .any(|f| p.ends_with(format!("level2/{f}")))
            {
                assert!(d.get("sealed").is_none(), "{}", p.display());
                assert!(
                    d["value"].as_str().is_some_and(|v| !v.is_empty()),
                    "{}",
                    p.display()
                );
                continue;
            }
            let rs = d
                .get("companies")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or(vec![d]);
            for r in rs {
                assert_eq!(r["sealed"], true, "{}", p.display());
                assert_eq!(r["value"], "", "{}", p.display());
                assert!(r.get("result_hash").is_none(), "{}", p.display());
            }
        }
    }
    assert!(files > 10, "only {files} files scanned");
}

/// `transcriptComplete` returns of an untampered log: true for every
/// published round (ADR-0027 amendment 8).
fn complete(a: &onyx_api::client::VerifiedAttempts) -> Vec<Vec<u8>> {
    let mut yes = vec![0u8; 32];
    yes[31] = 1;
    vec![yes; a.attempts.len()]
}

/// ROADMAP B4 (THREATS REVLOG-5): while a dated fact is not closed its free
/// `/attempts` answer is sealed and says REVLOG-5 applies; once the value
/// closes the fact, the paid `?outcome=all` answer serves the reveals and
/// says it does not. REVLOG-1 applies to both. The client refuses a sealed
/// answer that says REVLOG-5 does not apply.
#[test]
fn sealed_and_closed_facts_state_revlog_5_as_they_are_served() {
    let t = template();
    let (attempts, value) = history(&t);
    let at = now(&value);
    let s = subject();
    let before = load(&t, &[], &attempts, at).expect("loads");
    let fact = before
        .attempt_fact("test.rec:2026-10-08", None)
        .expect("indexed");
    let sealed = before.attempts_body(&before.attempts_of(&fact), before.fact_closed(&fact));
    assert!(sealed["attempts"][0].get("revealed").is_some());
    assert_eq!(sealed["limits"], onyx_api::observations::limits_json(true));
    let ids = |v: &Value| -> Vec<(String, bool)> {
        v.as_array()
            .expect("limits")
            .iter()
            .map(|l| {
                (
                    l["id"].as_str().expect("id").to_owned(),
                    l["applies"].as_bool().expect("applies"),
                )
            })
            .collect()
    };
    assert_eq!(
        ids(&sealed["limits"]),
        [("REVLOG-1".to_owned(), true), ("REVLOG-5".to_owned(), true)]
    );
    verify_attempts(&sealed, &expect(&t, &s, at)).expect("a sealed answer verifies");
    let mut understated = sealed.clone();
    understated["limits"] = onyx_api::observations::limits_json(false);
    assert!(
        verify_attempts(&understated, &expect(&t, &s, at)).is_err(),
        "a sealed answer may not say REVLOG-5 does not apply"
    );
    // Closed by its value: the rounds come with their reveals.
    let after = load(&t, std::slice::from_ref(&value), &attempts, at).expect("loads");
    let all = body_all(&after);
    assert!(all["attempts"][0].get("reveals").is_some());
    assert_eq!(
        ids(&all["limits"]),
        [
            ("REVLOG-1".to_owned(), true),
            ("REVLOG-5".to_owned(), false)
        ]
    );
    verify_observation(&all, &expect(&t, &s, at)).expect("verifies");
    let mut overstated = all.clone();
    overstated["limits"] = onyx_api::observations::limits_json(true);
    assert!(verify_observation(&overstated, &expect(&t, &s, at)).is_err());
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
