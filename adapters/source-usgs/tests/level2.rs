//! A level-2 round on the USGS earthquake count (NS3), through the shared
//! runner (`onyx_level2`). THREATS NS3-1..NS3-4.
//!
//! Always on: the committed recording (five real HTTPS responses of
//! 2026-10-26 for the day 2026-07-08) is replayed through five simulated
//! observers. Live (`ONYX_LIVE_FETCH=1`): five `curl` processes fetch the
//! pinned day now, each one an observer; the round must settle on one count.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{ObservationOutcome, ObserverStatus, SimulatedRegistry};
use onyx_core::claim::Subject;
use onyx_core::{Answer, Fetcher, ObservationSpec, ProofLevel, Value};
use onyx_level2::{run, store, Page};
use onyx_source_usgs::round::Usgs;
use onyx_source_usgs::{
    day_number, day_url, parse_count, recorded, recorded_window_start, spec, template,
    UsgsCountFetcher, MAX_DAYS, MIN_DAYS, RECORDED_BODY, SUBJECT,
};

/// The count the recording holds: M5.0+ earthquakes worldwide on 2026-07-08.
const RECORDED_COUNT: i128 = 9;
const DAY: u64 = 86_400;

fn pages() -> Vec<Page> {
    recorded()
        .fetches
        .into_iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body),
        })
        .collect()
}

fn day_start(day: &str) -> u64 {
    u64::try_from(day_number(day).unwrap()).unwrap() * DAY
}

#[test]
fn the_recorded_round_settles_the_count_at_level_two() {
    let rec = recorded();
    assert_eq!(rec.url, day_url("2026-07-08"));
    assert!(rec.fetches.iter().all(|f| f.body == RECORDED_BODY));
    let mut chain = SimulatedRegistry::new();
    let mut st = store(&Usgs);
    let r = run(
        &Usgs,
        &rec.day,
        recorded_window_start(),
        &pages(),
        &mut st,
        &mut chain,
    )
    .unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_COUNT))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .all(|(_, s)| *s == ObserverStatus::Agreed));
    let claim = r.value().unwrap();
    assert_eq!(claim.value, Value::Int(RECORDED_COUNT));
    assert_eq!(
        claim.subject,
        Subject::Aggregate(format!("{SUBJECT}:{}", rec.day))
    );
    // Accepted at level 2 by the store's own check (`accept_claim` in the runner).
    assert_eq!(
        st.accept_claim(claim, &onyx_source_usgs::registry(&rec.day))
            .unwrap(),
        ProofLevel::ReObserved
    );
}

/// NS3-1: one observer reading another count (a revision, a cached page, a
/// lie) is outvoted, never averaged in.
#[test]
fn an_observer_with_another_count_is_outvoted() {
    let rec = recorded();
    let mut p = pages();
    p[2].body = Some(b"20".to_vec());
    let mut chain = SimulatedRegistry::new();
    let r = run(
        &Usgs,
        &rec.day,
        recorded_window_start(),
        &p,
        &mut store(&Usgs),
        &mut chain,
    )
    .unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_COUNT))
    );
    let outliers = r
        .settlement
        .statuses()
        .iter()
        .filter(|(_, s)| *s == ObserverStatus::Outlier)
        .count();
    assert_eq!(outliers, 1);
}

/// NS3-2: a day is observable only after the catalog's revisions settle.
#[test]
fn a_day_is_observable_only_after_it_settles() {
    let d = "2026-04-02";
    let start = day_start(d);
    assert!(spec(d, start + u64::from(MIN_DAYS) * DAY).is_ok());
    assert!(spec(d, start + u64::from(MIN_DAYS) * DAY - 1).is_err());
    assert!(spec(d, start + u64::from(MAX_DAYS) * DAY).is_ok());
    assert!(spec(d, start + (u64::from(MAX_DAYS) + 1) * DAY).is_err());
    // Weekends count like any day: the catalog has no business calendar.
    assert!(spec("2026-04-05", day_start("2026-04-05") + 120 * DAY).is_ok());
    let s = spec(d, start + 120 * DAY).unwrap();
    assert_eq!(
        s.url,
        "https://earthquake.usgs.gov/fdsnws/event/1/count?starttime=2026-04-02T00:00:00&endtime=2026-04-02T23:59:59.999&minmagnitude=5.0&eventtype=earthquake"
    );
    template().validate().unwrap();
}

/// NS3-3: only a bare count is read; anything else is Unavailable.
#[test]
fn the_fetcher_reads_a_bare_count_only() {
    for (body, want) in [
        (&b"21"[..], Some(21)),
        (b"0", Some(0)),
        (b"999999999", Some(999_999_999)),
        (b"021", None),
        (b"", None),
        (b"21\n", None),
        (b" 21", None),
        (b"-1", None),
        (b"2e1", None),
        (b"1000000000", None),
        (b"{\"count\":21}", None),
    ] {
        assert_eq!(parse_count(body), want, "{body:?}");
    }
    let s = spec("2026-04-02", day_start("2026-04-02") + 120 * DAY).unwrap();
    assert_eq!(UsgsCountFetcher.extract(&s, b"21"), Answer::Number(21));
    assert_eq!(UsgsCountFetcher.extract(&s, b"21 "), Answer::Unavailable);
}

/// NS3-4: a spec that is not the template for its own day reads nothing:
/// another magnitude, another day in one of the two places, another host.
#[test]
fn the_fetcher_refuses_a_feed_that_is_not_the_template() {
    let good = spec("2026-04-02", day_start("2026-04-02") + 120 * DAY).unwrap();
    let edit = |f: &dyn Fn(&mut ObservationSpec)| {
        let mut s = good.clone();
        f(&mut s);
        UsgsCountFetcher.extract(&s, b"21")
    };
    assert_eq!(
        edit(&|s| s.url = s.url.replace("minmagnitude=5.0", "minmagnitude=4.5")),
        Answer::Unavailable
    );
    assert_eq!(
        edit(&|s| s.url = s.url.replacen("2026-04-02T23", "2026-04-03T23", 1)),
        Answer::Unavailable
    );
    assert_eq!(
        edit(&|s| s.url = s.url.replace("earthquake.usgs.gov", "example.org")),
        Answer::Unavailable
    );
    assert_eq!(edit(&|s| s.selector = "count".into()), Answer::Unavailable);
}

/// One `curl` process per observer (no shell, https only, 20 s limit).
fn curl(url: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS", "--fail", "--proto", "=https", "-m", "20", "-A", "onyx-dev", url,
        ])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[test]
fn live_round_on_a_settled_day() {
    if std::env::var("ONYX_LIVE_FETCH").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_LIVE_FETCH=1 to fetch the USGS count live");
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let window_start = now - 5;
    // The day 110 days ago: inside the observable range.
    let day_n = i64::try_from(now / DAY).unwrap() - 110;
    let day = civil(day_n);
    assert_eq!(day_number(&day), Some(day_n));
    let url = day_url(&day);
    let mut p = Vec::new();
    for _ in 0..5 {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        p.push(Page {
            fetched_unix: t,
            body: curl(&url),
        });
    }
    let mut chain = SimulatedRegistry::new();
    let r = run(&Usgs, &day, window_start, &p, &mut store(&Usgs), &mut chain).unwrap();
    let ObservationOutcome::Value(Answer::Number(n)) = r.settlement.outcome().clone() else {
        panic!("no value: {:?}", r.settlement.outcome());
    };
    eprintln!("live: {n} M5.0+ earthquakes on {day}");
}

/// `YYYY-MM-DD` of a day number (days since 1970-01-01).
fn civil(z: i64) -> String {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", if m <= 2 { y + 1 } else { y })
}

/// Review NS3-R1 findings 1, 4 and 9 (THREATS NS3-8, NS3-9, NS3-10): earthquakes only, a
/// short observable range, and every recorded fetch keeps the CDN's cache headers.
#[test]
fn earthquakes_only_a_short_range_and_cache_headers_kept() {
    let t = template();
    assert!(t.url_template.ends_with("&eventtype=earthquake"));
    const { assert!(MAX_DAYS > MIN_DAYS && MAX_DAYS - MIN_DAYS <= 30) };
    // A feed without the event type (explosions counted too) reads nothing.
    let mut s = spec("2026-04-02", day_start("2026-04-02") + 110 * DAY).unwrap();
    s.url = s.url.replace("&eventtype=earthquake", "");
    assert_eq!(UsgsCountFetcher.extract(&s, b"21"), Answer::Unavailable);
    let m: serde_json::Value = serde_json::from_str(onyx_source_usgs::RECORDED_MANIFEST).unwrap();
    let fetches = m["fetches"].as_array().unwrap();
    assert_eq!(fetches.len(), 5);
    for f in fetches {
        assert!(f["age"].is_string() && f["x_cache"].is_string(), "{f}");
    }
}

/// The website's summary (`apps/web/static/level2/usgs-quakes.json`) is this
/// recording's round on the recorded draw: replayed here, it gives the same
/// round id and result hash the browser pins.
#[test]
fn the_site_summary_is_the_recorded_round_on_its_recorded_draw() {
    let site: serde_json::Value = serde_json::from_str(include_str!(
        "../../../apps/web/static/level2/usgs-quakes.json"
    ))
    .unwrap();
    let rec = recorded();
    let mut chain =
        onyx_coordinator::observe::RecordedDraw::new(onyx_source_usgs::round::recorded_draw());
    let r = run(
        &Usgs,
        &rec.day,
        recorded_window_start(),
        &pages(),
        &mut store(&Usgs),
        &mut chain,
    )
    .unwrap();
    let hx = |b: &[u8]| {
        format!(
            "0x{}",
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        )
    };
    assert_eq!(site["round_id"], hx(&r.round_id.0));
    assert_eq!(site["result_hash"], hx(&r.settlement.result_hash().0));
    assert_eq!(site["feed"]["spec_id"], hx(&r.spec.id().0));
}
