//! A level-2 round on one station's daily maximum temperature from NOAA NCEI
//! GHCN-Daily (NS3), through the shared runner (`onyx_level2`). THREATS
//! NS3-5..NS3-7.
//!
//! Always on: the committed recording (five real HTTPS responses of
//! 2026-10-26 for the day 2026-09-01) is replayed through five simulated
//! observers. Live (`ONYX_LIVE_FETCH=1`): five `curl` processes fetch a
//! settled day now; the round must settle on one value.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{ObservationOutcome, ObserverStatus, SimulatedRegistry};
use onyx_core::{Answer, Fetcher, ObservationSpec, ProofLevel, Value};
use onyx_level2::{run, store, Page};
use onyx_source_ncei::round::Ghcnd;
use onyx_source_ncei::{
    day_number, day_url, parse_tenths, recorded, recorded_window_start, registry, spec,
    GhcndFetcher, MAX_DAYS, MIN_DAYS, RECORDED_BODY,
};

/// TMAX at USW00003017 on 2026-09-01: 30.6 °C, in tenths.
const RECORDED_TENTHS: i128 = 306;
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

fn settled_spec() -> ObservationSpec {
    spec("2026-09-01", day_start("2026-09-01") + 40 * DAY).unwrap()
}

#[test]
fn the_recorded_round_settles_the_temperature_at_level_two() {
    let rec = recorded();
    assert_eq!(rec.url, day_url("2026-09-01"));
    assert!(rec.fetches.iter().all(|f| f.body == RECORDED_BODY));
    let mut st = store(&Ghcnd);
    let r = run(
        &Ghcnd,
        &rec.day,
        recorded_window_start(),
        &pages(),
        &mut st,
        &mut SimulatedRegistry::new(),
    )
    .unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(RECORDED_TENTHS))
    );
    assert!(r
        .settlement
        .statuses()
        .iter()
        .all(|(_, s)| *s == ObserverStatus::Agreed));
    let claim = r.value().unwrap();
    assert_eq!(
        claim.value,
        Value::Fixed {
            mantissa: RECORDED_TENTHS,
            scale: 1
        }
    );
    assert_eq!(
        st.accept_claim(claim, &registry("2026-09-01")).unwrap(),
        ProofLevel::ReObserved
    );
}

/// NS3-5: a day is observable only after GHCN-Daily's late updates.
#[test]
fn a_day_is_observable_only_after_it_settles() {
    let d = "2026-09-01";
    let start = day_start(d);
    assert!(spec(d, start + u64::from(MIN_DAYS) * DAY).is_ok());
    assert!(spec(d, start + u64::from(MIN_DAYS) * DAY - 1).is_err());
    assert!(spec(d, start + (u64::from(MAX_DAYS) + 1) * DAY).is_err());
    assert_eq!(
        settled_spec().url,
        "https://www.ncei.noaa.gov/access/services/data/v1?dataset=daily-summaries&stations=USW00003017&startDate=2026-09-01&endDate=2026-09-01&dataTypes=TMAX&units=metric&format=json"
    );
}

/// NS3-6: one row of exactly this station, day and field; anything else is
/// Unavailable.
#[test]
fn the_fetcher_reads_one_exact_row_only() {
    let s = settled_spec();
    let row = |date: &str, station: &str, tmax: &str| {
        format!("[\n{{\"DATE\":\"{date}\",\"STATION\":\"{station}\",\"TMAX\":\"{tmax}\"}}\n]\n")
    };
    let read = |body: &str| GhcndFetcher.extract(&s, body.as_bytes());
    assert_eq!(
        read(&row("2026-09-01", "USW00003017", "30.6")),
        Answer::Number(306)
    );
    assert_eq!(
        read(&row("2026-09-01", "USW00003017", "-12.0")),
        Answer::Number(-120)
    );
    for bad in [
        row("2026-09-02", "USW00003017", "30.6"),
        row("2026-09-01", "USW00023062", "30.6"),
        row("2026-09-01", "USW00003017", "  306"),
        "[\n]\n".to_string(),
        format!(
            "[{},{}]",
            row("2026-09-01", "USW00003017", "30.6").trim(),
            row("2026-09-01", "USW00003017", "30.6").trim()
        ),
        "[{\"DATE\":\"2026-09-01\",\"STATION\":\"USW00003017\",\"TMAX\":\"30.6\",\"TMAX_ATTRIBUTES\":\",,W,2400\"}]".into(),
        "[{\"DATE\":\"2026-09-01\",\"STATION\":\"USW00003017\",\"TMAX\":30.6}]".into(),
        "not json".into(),
    ] {
        assert_eq!(read(&bad), Answer::Unavailable, "{bad}");
    }
    for (s, want) in [
        ("0.0", Some(0)),
        ("9.5", Some(95)),
        ("-0.5", Some(-5)),
        ("100.0", Some(1000)),
        ("-0.0", None),
        ("05.0", None),
        ("30.60", None),
        ("30", None),
        ("+30.6", None),
        ("1000.0", None),
        ("3e1", None),
    ] {
        assert_eq!(parse_tenths(s), want, "{s}");
    }
}

/// NS3-7: another station, unit or dataset in the URL is not the template.
#[test]
fn the_fetcher_refuses_a_feed_that_is_not_the_template() {
    let good = settled_spec();
    let body = RECORDED_BODY;
    for edit in [
        ("units=metric", "units=standard"),
        ("USW00003017", "USW00023062"),
        ("endDate=2026-09-01", "endDate=2026-09-02"),
        ("dataTypes=TMAX", "dataTypes=TMIN"),
    ] {
        let mut s = good.clone();
        s.url = s.url.replace(edit.0, edit.1);
        assert_eq!(
            GhcndFetcher.extract(&s, body),
            Answer::Unavailable,
            "{edit:?}"
        );
    }
}

fn curl(url: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS", "--fail", "--proto", "=https", "-m", "30", "-A", "onyx-dev", url,
        ])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

#[test]
fn live_round_on_a_settled_day() {
    if std::env::var("ONYX_LIVE_FETCH").as_deref() != Ok("1") {
        eprintln!("skipped: set ONYX_LIVE_FETCH=1 to fetch GHCN-Daily live");
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let day = civil(i64::try_from(now / DAY).unwrap() - 45);
    let url = day_url(&day);
    let p: Vec<Page> = (0..5)
        .map(|_| Page {
            fetched_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            body: curl(&url),
        })
        .collect();
    let r = run(
        &Ghcnd,
        &day,
        now - 5,
        &p,
        &mut store(&Ghcnd),
        &mut SimulatedRegistry::new(),
    )
    .unwrap();
    let ObservationOutcome::Value(Answer::Number(n)) = r.settlement.outcome().clone() else {
        panic!("no value: {:?}", r.settlement.outcome());
    };
    eprintln!("live: TMAX {n} tenths of a degree C at USW00003017 on {day}");
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

/// Review NS3-R1 finding 9 (THREATS NS3-9): a short observable range, so a
/// coordinator cannot pick a late window by its value.
#[test]
fn the_observable_range_is_short() {
    const { assert!(MAX_DAYS > MIN_DAYS && MAX_DAYS - MIN_DAYS <= 30) };
}

/// The website's summary (`apps/web/static/level2/ncei-tmax.json`) is this
/// recording's round on the recorded draw: replayed here, it gives the same
/// round id and result hash the browser pins.
#[test]
fn the_site_summary_is_the_recorded_round_on_its_recorded_draw() {
    let site: serde_json::Value = serde_json::from_str(include_str!(
        "../../../apps/web/static/level2/ncei-tmax.json"
    ))
    .unwrap();
    let rec = recorded();
    let mut chain =
        onyx_coordinator::observe::RecordedDraw::new(onyx_source_ncei::round::recorded_draw());
    let r = run(
        &Ghcnd,
        &rec.day,
        recorded_window_start(),
        &pages(),
        &mut store(&Ghcnd),
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
