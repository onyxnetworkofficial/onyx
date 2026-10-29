//! A real level-2 round on the U.S. Treasury Fiscal Data API.
//!
//! Always on: the committed recording (five real HTTPS responses of
//! 2026-10-11) is replayed through five simulated observers.
//! Live (`ONYX_LIVE_FETCH=1`): five `curl` processes fetch the pinned record
//! now, each one an observer; the round must settle on the same cents.

use onyx_coordinator::observe::SimulatedChain;
use onyx_coordinator::observe::{
    sign_commit, sign_reveal, ObservationOutcome, ObservationResults, ObservationRound,
    ObserveConfig, ObserveError, Observer, ObserverStatus,
};
use onyx_coordinator::observe::{RecordedDraw, SimulatedRegistry};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, observation_commit, observe, Answer, Claim, FeedTemplate, Fetcher, Hash32,
    ObservationSpec, ProofLevel, Snapshot, SourceRef, SubjectRule, Value, VerificationMethod,
};
use onyx_source_fiscaldata::round::{
    observer_address, observer_key, recorded_draw, run, store, Page,
};
use onyx_source_fiscaldata::{
    check_record_date, day_number, record_url, recorded, registry, rights, spec, template,
    FiscalDataFetcher, ENDPOINT, METRIC, RECORDED_BODY, RECORDED_WINDOW_START, SCALE, SUBJECT,
};

/// Total public debt outstanding on 2026-10-08, in cents, as recorded.
const RECORDED_CENTS: i128 = 4_030_531_621_082_972;

fn recorded_pages() -> Vec<Page> {
    recorded()
        .fetches
        .into_iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body),
        })
        .collect()
}

#[test]
fn the_recorded_round_settles_and_its_claim_is_accepted_at_level_two() {
    let mut chain = RecordedDraw::new(recorded_draw());
    let rec = recorded();
    let mut store = store();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &recorded_pages(),
        &mut store,
        &mut chain,
    )
    .expect("round");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .all(|(_, s)| *s == ObserverStatus::Agreed));
    for (_, o, _, _) in &r.reveals {
        assert_eq!(o.page_hash, keccak256(RECORDED_BODY));
    }
    // Pinned: the same recording always gives the same result.
    assert_eq!(
        r.settlement.result_hash(),
        Hash32(hex32(
            "0xf65720c7a4c2358f3c8d8047066846994e117bc0daa307e29009ca2da3f2cec9"
        ))
    );
    // The fact is settled once per store.
    assert!(run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &recorded_pages(),
        &mut store,
        &mut chain
    )
    .is_err());
}

/// NS3 (review NS3-R1 finding 10): after the move to the shared runner the
/// Treasury claim is still dollars and cents (`Fixed`, scale 2), never `Int`.
#[test]
fn the_shared_runner_keeps_the_treasury_value_in_cents() {
    let rec = recorded();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &recorded_pages(),
        &mut store(),
        &mut SimulatedRegistry::new(),
    )
    .expect("round");
    assert_eq!(
        r.value().expect("a value").value,
        Value::Fixed {
            mantissa: RECORDED_CENTS,
            scale: SCALE
        }
    );
    assert_eq!(SCALE, 2);
}

#[test]
fn a_lying_observer_is_outvoted_and_slashed() {
    let mut chain = SimulatedRegistry::new();
    let rec = recorded();
    let mut pages = recorded_pages();
    let forged = std::str::from_utf8(RECORDED_BODY)
        .expect("utf8")
        .replace("40305316210829.72", "30305316210829.72");
    pages[4].body = Some(forged.into_bytes());
    let mut store = store();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &pages,
        &mut store,
        &mut chain,
    )
    .expect("round");
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
    );
    let outliers = r
        .settlement
        .statuses()
        .iter()
        .filter(|(_, s)| *s == ObserverStatus::Outlier)
        .count();
    assert_eq!(outliers, 1);
}

fn hex32(s: &str) -> [u8; 32] {
    let s = s.trim_start_matches("0x");
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
    }
    out
}

/// One `curl` process per observer (no shell, https only, 20 s limit).
fn curl(url: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args(["-sS", "--fail", "--proto", "=https", "-m", "20", url])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs()
}

#[test]
fn live_observers_fetch_the_record_now_and_agree() {
    let mut chain = SimulatedRegistry::new();
    if std::env::var("ONYX_LIVE_FETCH").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_LIVE_FETCH=1 to fetch live from api.fiscaldata.treasury.gov");
        return;
    }
    let window_start = now();
    // The recorded date while it is still in the allowed lag; afterwards a
    // date three days back (its cents are not pinned).
    let recorded_ok = spec("2026-10-08", window_start).is_ok();
    let date_buf;
    let date = if recorded_ok {
        "2026-10-08"
    } else {
        // The latest record date the federal calendar allows now.
        let today = i64::try_from(window_start / 86_400).expect("fits");
        date_buf = (1..=10)
            .map(|back| civil(today - back))
            .find(|d| spec(d, window_start).is_ok())
            .expect("a record date in the lag");
        date_buf.as_str()
    };
    let url = record_url(date);
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || {
                let t = now();
                Page {
                    fetched_unix: t,
                    body: curl(&url),
                }
            })
        })
        .collect();
    let pages: Vec<Page> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    let mut store = store();
    let r = run(date, window_start, &pages, &mut store, &mut chain).expect("live round");
    if recorded_ok {
        // A published historical record does not change.
        assert_eq!(
            *r.settlement.outcome(),
            ObservationOutcome::Value(Answer::Number(RECORDED_CENTS))
        );
    }
}

/// `YYYY-MM-DD` of a day number (inverse of `day_number`, for the live test).
fn civil(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let out = format!("{y:04}-{m:02}-{d:02}");
    assert_eq!(day_number(&out), Some(z - 719_468));
    out
}

fn body_for(date: &str, field: &str, value: &str) -> Vec<u8> {
    format!(
        "{{\"data\":[{{\"record_date\":\"{date}\",\"{field}\":\"{value}\"}}],\"meta\":{{\"count\":1}}}}"
    )
    .into_bytes()
}

/// L2SRC-R1-F1: an old record date is refused for today's window (it never
/// becomes a fact here), the recorded date's claim names its own day, and the
/// refused attempt does not block the honest round.
#[test]
fn an_old_record_date_is_refused_and_the_honest_round_still_settles() {
    let mut chain = SimulatedRegistry::new();
    let w = RECORDED_WINDOW_START;
    let old = body_for("2001-01-02", "tot_pub_debt_out_amt", "5662216013697.37");
    let pages: Vec<Page> = (0..5)
        .map(|i| Page {
            fetched_unix: w + 10 + i,
            body: Some(old.clone()),
        })
        .collect();
    let mut store = store();
    assert!(run("2001-01-02", w, &pages, &mut store, &mut chain).is_err());
    let rec = recorded();
    let r = run(
        &rec.record_date,
        w,
        &recorded_pages(),
        &mut store,
        &mut chain,
    )
    .expect("honest round");
    assert_eq!(
        r.value().expect("value").subject,
        Subject::Aggregate(format!("{SUBJECT}:2026-10-08"))
    );
    // Two record dates are two facts: a date still in the lag, same window,
    // is its own fact (and here no value: its pages are the 10-08 record).
    // ATT-R1-F1: the run is returned; it settles no value.
    let other = run("2026-10-07", w, &recorded_pages(), &mut store, &mut chain).expect("runs");
    assert!(other.value().is_err());
}

fn cfg_for(spec: &ObservationSpec) -> ObserveConfig {
    ObserveConfig {
        commit_close: spec.window_end + 60,
        reveal_close: spec.window_end + 180,
        reward: 10,
        outlier_slash: 100,
        miss_slash: 20,
    }
}

fn five() -> Vec<Observer> {
    let mut obs: Vec<Observer> = (1..=5u8)
        .map(|i| Observer {
            address: observer_address(i),
            bond: 1_000,
        })
        .collect();
    obs.sort_by_key(|o| o.address);
    obs
}

/// Open a round of `spec` in `st` drawn from `five()` (the pool is the draw
/// size, so the draw is the whole pool).
fn open(
    st: &mut ObservationResults,
    spec: &ObservationSpec,
) -> Result<ObservationRound, ObserveError> {
    let snapshot = st.announce_draw(spec, &cfg_for(spec), &five(), 10, &SimulatedChain::at(0))?;
    st.open_round(
        spec.clone(),
        cfg_for(spec),
        snapshot,
        &SimulatedChain::with_beacon(Hash32([3; 32])),
    )
}

/// Play five honest observers of `spec` reading `body` with `f` in `round`.
fn play<F: Fetcher>(f: &F, round: &mut ObservationRound, spec: &ObservationSpec, body: &[u8]) {
    let cfg = cfg_for(spec);
    let rid = round.round_id();
    let mut rv = vec![];
    let attempt = round.draw().attempt;
    for i in 1..=5u8 {
        let (a, sk) = (observer_address(i), observer_key(i));
        let o = observe(f, spec, Some(body), spec.window_start + 10).expect("obs");
        let s = Hash32([i; 32]);
        let h = observation_commit(&rid, &a, &o, &s);
        let sig = sign_commit(&sk, &rid, attempt, &h).expect("sign");
        round
            .commit(spec.window_start + 10, a, h, sig)
            .expect("commit");
        rv.push((a, sk, o, s));
    }
    for (a, sk, o, s) in &rv {
        let sig = sign_reveal(sk, &rid, attempt, o, s).expect("sign");
        round
            .reveal(cfg.commit_close + 1, *a, o.clone(), *s, sig)
            .expect("reveal");
    }
}

/// The claim a settled value of `spec` backs.
fn claim_for(spec: &ObservationSpec, value: i128, result: Hash32) -> Claim {
    Claim {
        metric: spec.metric.clone(),
        subject: spec.subject.clone(),
        value: Value::Fixed {
            mantissa: value,
            scale: SCALE,
        },
        source: SourceRef {
            source: spec.rights.source.clone(),
            snapshot: Snapshot::Document {
                published_unix: spec.window_end,
                content_hash: result,
            },
        },
        method: VerificationMethod::ReObserved {
            spec: spec.id(),
            result,
        },
        label_set: None,
        rights: rights(),
        evidence: vec![],
        confidence: None,
    }
}

/// A generic JSON-pointer reader (like `onyx_source_page::JsonPointerFetcher`,
/// which this adapter does not depend on): what a forger would point at an
/// old record.
struct PointerFetcher;

impl Fetcher for PointerFetcher {
    fn id(&self) -> Hash32 {
        keccak256(b"review: a generic json-pointer fetcher")
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        serde_json::from_slice::<serde_json::Value>(page)
            .ok()
            .and_then(|d| {
                d.pointer(&spec.selector)
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .and_then(|s| onyx_source_fiscaldata::parse_cents(&s))
            .map_or(Answer::Unavailable, Answer::Number)
    }
}

/// L2SRC-R1-F2 and R2-F1: the same metric on another field, another host or
/// another fetcher is not the registered feed, so the round never opens.
#[test]
fn field_host_and_fetcher_substitutions_never_open() {
    let mut st = store();
    let canon = spec("2026-10-08", RECORDED_WINDOW_START).expect("canonical feed");
    assert!(st
        .announce_draw(
            &canon,
            &cfg_for(&canon),
            &five(),
            10,
            &SimulatedChain::at(0)
        )
        .is_ok());
    let mut field = canon.clone();
    field.url = format!(
        "{ENDPOINT}?fields=record_date,debt_held_public_amt&filter=record_date:eq:2026-10-08"
    );
    field.selector = "/data/0/debt_held_public_amt".into();
    let mut host = canon.clone();
    host.url = "https://evil.example/debt?filter=record_date:eq:2026-10-08".into();
    let mut fetcher = canon.clone();
    fetcher.fetcher = PointerFetcher.id();
    for (name, f) in [("field", field), ("host", host), ("fetcher", fetcher)] {
        assert!(
            matches!(
                st.announce_draw(&f, &cfg_for(&f), &five(), 10, &SimulatedChain::at(0)),
                Err(ObserveError::Template(_))
            ),
            "{name}"
        );
    }
}

/// L2SRC-R2-F1 (review probe `another_fetcher_restates_an_old_record_under_a_dated_subject`):
/// another fetcher id, this metric, the dated subject 2026-10-08, the URL of
/// the 2001-01-02 record, a window 400 days out of lag. It never opens here,
/// and even settled in a store whose own admin registered that forged feed
/// for the metric, its claim is refused by this store.
#[test]
fn an_old_record_under_a_dated_subject_is_refused_at_open_and_at_accept() {
    let mut chain = SimulatedRegistry::new();
    let mut forged = spec("2026-10-08", RECORDED_WINDOW_START).expect("canonical feed");
    forged.fetcher = PointerFetcher.id();
    forged.url = forged.url.replace("2026-10-08", "2001-01-02");
    forged.window_start += 400 * 86_400;
    forged.window_end += 400 * 86_400;
    // On the rogue fixed feed's grid (L2RND-R2-F2).
    let len = forged.window_end - forged.window_start;
    forged.window_start -= forged.window_start % len;
    forged.window_end = forged.window_start + len;
    let old = std::str::from_utf8(RECORDED_BODY)
        .expect("utf8")
        .replace("2026-10-08", "2001-01-02")
        .replace("40305316210829.72", "5662216013697.37");
    let mut honest = store();
    assert!(matches!(
        honest.announce_draw(
            &forged,
            &cfg_for(&forged),
            &five(),
            10,
            &SimulatedChain::at(0)
        ),
        Err(ObserveError::Template(_))
    ));
    // A rogue coordinator registers its own template for the metric (a
    // fixed subject naming the date, any fetcher, any url) in its own store.
    let (mut rogue, admin) = ObservationResults::new();
    rogue
        .register_feed(
            &admin,
            FeedTemplate {
                feed: forged.feed.clone(),
                metric: MetricId(METRIC.into()),
                fetcher: forged.fetcher,
                url_template: forged.url.clone(),
                selector: forged.selector.clone(),
                kind: forged.kind,
                window_secs: forged.window_end - forged.window_start,
                min_observers: forged.min_observers,
                rights: rights(),
                subject: SubjectRule::Fixed(forged.subject.clone()),
            },
            "rogue",
            0,
        )
        .expect("rogue registers in its own store");
    let mut round = open(&mut rogue, &forged).expect("opens in the rogue store");
    play(&PointerFetcher, &mut round, &forged, old.as_bytes());
    let s = rogue
        .settle(&round, cfg_for(&forged).reveal_close + 1)
        .expect("settles in the rogue store");
    assert_eq!(
        *s.outcome(),
        ObservationOutcome::Value(Answer::Number(566_221_601_369_737))
    );
    let c = claim_for(&forged, 566_221_601_369_737, s.result_hash());
    let reg = registry("2026-10-08");
    assert!(honest.accept_claim(&c, &reg).is_err());
    // The honest round for the date still settles and is accepted.
    let rec = recorded();
    let r = run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &recorded_pages(),
        &mut honest,
        &mut chain,
    )
    .expect("honest round");
    assert_eq!(
        honest.accept_claim(r.value().expect("value"), &reg),
        Ok(ProofLevel::ReObserved)
    );
    // The template registered here is the adapter's, recorded once.
    assert_eq!(honest.template(&MetricId(METRIC.into())), Some(&template()));
    assert_eq!(honest.feed_decisions().len(), 1);
}

/// L2SRC-R2-F2 (review probe `one_dated_fact_two_results_in_one_store`): a
/// dated subject is one fact whatever the window: a second round for
/// 2026-10-08 one second later (another value) is refused at settle.
#[test]
fn one_dated_fact_has_one_result_whatever_the_window() {
    let mut chain = SimulatedRegistry::new();
    let mut st = store();
    let rec = recorded();
    run(
        &rec.record_date,
        RECORDED_WINDOW_START,
        &recorded_pages(),
        &mut st,
        &mut chain,
    )
    .expect("first round");
    let second = spec("2026-10-08", RECORDED_WINDOW_START + 1).expect("canonical feed");
    let forged = std::str::from_utf8(RECORDED_BODY)
        .expect("utf8")
        .replace("40305316210829.72", "30305316210829.72");
    // It is the canonical feed, but this store draws no second set for a
    // settled fact; a round opened elsewhere is refused at settle.
    assert_eq!(
        st.announce_draw(
            &second,
            &cfg_for(&second),
            &five(),
            10,
            &SimulatedChain::at(0)
        ),
        Err(ObserveError::FeedAlreadySettled)
    );
    let mut round = open(&mut store(), &second).expect("opens in a fresh store");
    play(&FiscalDataFetcher, &mut round, &second, forged.as_bytes());
    assert_eq!(
        st.settle(&round, cfg_for(&second).reveal_close + 1),
        Err(ObserveError::FeedAlreadySettled)
    );
}

/// L2SRC-R2-F4 (review probe `date_edges`, on the business calendar): a
/// Friday record before Columbus Day is out on Tuesday; weekends never
/// publish; leap days and UTC midnight are exact; years outside the calendar
/// are refused.
#[test]
fn record_dates_follow_the_federal_business_calendar() {
    let at = |d: &str| u64::try_from(day_number(d).expect("date")).expect("fits") * 86_400;
    // Friday 2026-10-09: Monday 10-12 is Columbus Day, published Tuesday.
    assert!(check_record_date("2026-10-09", at("2026-10-13")).is_err());
    assert!(check_record_date("2026-10-09", at("2026-10-13") + 86_399).is_err());
    assert!(check_record_date("2026-10-09", at("2026-10-14")).is_ok());
    // Weekend record dates never publish.
    assert!(check_record_date("2026-10-10", at("2026-10-14")).is_err());
    // Leap day 2028-02-29 is a Tuesday.
    assert!(check_record_date("2028-02-28", at("2028-02-29") + 86_399).is_err());
    assert!(check_record_date("2028-02-28", at("2028-03-01")).is_ok());
    assert!(check_record_date("2028-02-29", at("2028-03-02")).is_ok());
    assert!(check_record_date("2027-02-29", at("2027-03-03")).is_err());
    assert!(check_record_date("2100-02-26", at("2100-03-03")).is_err());
    assert!(check_record_date("2025-12-30", at("2026-01-02")).is_err());
    // A window starting 23:58 UTC: the start's day counts.
    let w = at("2026-10-10") + 86_400 - 120;
    assert!(check_record_date("2026-10-08", w).is_ok());
    assert!(check_record_date("2026-09-30", w).is_ok()); // 10 days
    assert!(check_record_date("2026-09-29", w).is_err()); // 11 days
}
