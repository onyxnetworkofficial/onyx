//! Level-2 source (ADR-0027): one record of the U.S. Treasury "Debt to the
//! Penny" dataset from the Fiscal Data API, read by a pinned fetcher.
//!
//! Rights: the Fiscal Data site states its data "is available to copy, adapt,
//! redistribute, or otherwise use for non-commercial and commercial purposes"
//! (<https://fiscaldata.treasury.gov/about-us/>). No account, no API key.
//! See `fixtures/PROVENANCE.md`.
//!
//! This crate never touches the network. An observer does its own HTTPS GET
//! of the feed's `url` and hands the bytes to [`FiscalDataFetcher`]. The
//! recorded responses in `fixtures/` let tests and the website rerun the round
//! offline.
//!
//! The metric's one canonical feed is [`template`], an
//! [`onyx_core::FeedTemplate`] (L2SRC-R2-F1): the coordinator registers it
//! and opens and accepts rounds of this metric only as that template
//! instantiated for a dated subject, so no other fetcher, host, field or
//! record can be passed off as this metric. The record date must be a
//! business day on the U.S. federal calendar, with at least one business day
//! between it and the window's UTC day (the Treasury publishes a record on
//! the next business day, so it is out before the window opens), and at most
//! [`MAX_LAG_DAYS`] calendar days before it (L2SRC-R1-F1, L2SRC-R2-F4).
//!
//! Fetcher contract (pinned by [`FETCHER_TAG`]): the feed must equal the
//! template for the record date its `url` pins (L2SRC-R1-F2). The response must
//! hold exactly one record (`data` of length 1, `meta.count` 1) whose
//! `record_date` is that date; the value at the selector must be a JSON string
//! holding dollars and cents (digits, a point, exactly two decimals; no sign,
//! no leading zeros, L2SRC-R1-F6), read exactly (no floats, ADR-0009).
//! Anything else is [`Answer::Unavailable`], never a guess.
//!
//! The record date is part of the claim: the subject is
//! `us.treasury.federal_debt:YYYY-MM-DD`, so a level-2 fact always says which
//! day's debt it is, and two record dates are two facts (L2SRC-R1-F1).
//!
//! Other Fiscal Data records under the same terms and calendar are [`series`]
//! (north-star loop NL25: the Daily Treasury Statement's TGA closing balance).

#[cfg(feature = "round")]
pub mod intake_feed;
#[cfg(feature = "round")]
pub mod round;
pub mod series;

use onyx_core::claim::{MetricId, Subject};
pub use onyx_core::day_number;
use onyx_core::{
    keccak256, Answer, DataRights, DateLag, FeedTemplate, Fetcher, Hash32, MetricInfo,
    ObservationKind, ObservationSpec, Redistribution, SourceId, StaticRegistry, SubjectRule,
    ValueKind,
};
use serde_json::Value as Json;

/// Version tag the fetcher id is derived from.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:fiscaldata-record:v1";
/// The dataset endpoint (Debt to the Penny).
pub const ENDPOINT: &str =
    "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v2/accounting/od/debt_to_penny";
/// Feed and metric id.
pub const METRIC: &str = "us.treasury.total_public_debt_outstanding";
/// The subject prefix: the U.S. federal debt as a whole; a feed's subject is
/// `SUBJECT:YYYY-MM-DD` ([`subject`]).
pub const SUBJECT: &str = "us.treasury.federal_debt";
/// Business days that must lie strictly between a record date and the
/// window's UTC day (Debt to the Penny publishes a record on the next
/// business day, so with one in between it is out before the window opens;
/// L2SRC-R2-F4).
pub const MIN_BUSINESS_DAYS: u16 = 1;
/// At most this many calendar days from the record date to the window's day:
/// a feed reports a recent record, never an old one passed off under a new
/// window (L2SRC-R1-F1). Ten days covers a long holiday weekend.
pub const MAX_LAG_DAYS: u16 = 10;
/// First and last year of the federal holiday table ([`federal_holidays`]).
/// Outside it every record date is refused (fail closed).
pub const CALENDAR_YEARS: (i64, i64) = (2026, 2035);
/// Where the source's name, terms and quote shown on the website come from:
/// constants here, never the summary JSON (L2SRC-R2-F3).
pub const SOURCE_NAME: &str = "U.S. Treasury Fiscal Data: Debt to the Penny";
/// The terms page.
pub const TERMS_URL: &str = "https://fiscaldata.treasury.gov/about-us/";
/// The terms, quoted.
pub const TERMS_QUOTE: &str =
    "available to copy, adapt, redistribute, or otherwise use for non-commercial and commercial purposes";
/// Source id in the registry.
pub const SOURCE: &str = "us.treasury.fiscaldata";
/// JSON pointer into the one record.
pub const SELECTOR: &str = "/data/0/tot_pub_debt_out_amt";
/// Dollars and cents.
pub const SCALE: u8 = 2;
/// Numeric windows are at most 300 s (ADR-0027).
pub const WINDOW_SECS: u64 = 300;
/// Observers per round.
pub const OBSERVERS: u16 = 5;

/// Reviewed rights for this source.
pub fn rights() -> DataRights {
    DataRights {
        license: "onyx:us-treasury-fiscal-data-open".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: Some(
            "Source: U.S. Department of the Treasury, Bureau of the Fiscal Service (fiscaldata.treasury.gov)"
                .into(),
        ),
        terms: String::new(),
    }
}

/// The URL template: one `{date}`.
pub fn url_template() -> String {
    format!("{ENDPOINT}?fields=record_date,tot_pub_debt_out_amt&filter=record_date:eq:{{date}}")
}

/// The pinned URL for one record date (`YYYY-MM-DD`).
pub fn record_url(record_date: &str) -> String {
    url_template().replacen(onyx_core::feed::DATE_PLACEHOLDER, record_date, 1)
}

/// The dated subject for one record date: `us.treasury.federal_debt:YYYY-MM-DD`.
pub fn subject(record_date: &str) -> String {
    format!("{SUBJECT}:{record_date}")
}

fn weekday(day: i64) -> i64 {
    // 1970-01-01 was a Thursday (Mon = 0).
    (day + 3).rem_euclid(7)
}

/// The `n`-th (1-based) `wd` weekday (Mon = 0) of month `m` of year `y`.
fn nth_weekday(y: i64, m: u32, wd: i64, n: i64) -> i64 {
    let first = day_number(&format!("{y:04}-{m:02}-01")).unwrap_or(0);
    first + (wd - weekday(first)).rem_euclid(7) + 7 * (n - 1)
}

/// The last `wd` weekday of month `m` of year `y`.
fn last_weekday(y: i64, m: u32, wd: i64) -> i64 {
    let next = if m == 12 {
        day_number(&format!("{:04}-01-01", y + 1))
    } else {
        day_number(&format!("{y:04}-{:02}-01", m + 1))
    }
    .unwrap_or(0);
    let last = next - 1;
    last - (weekday(last) - wd).rem_euclid(7)
}

/// A fixed-date holiday moved to its observed day: Saturday to Friday,
/// Sunday to Monday (5 U.S.C. 6103(b); OPM, "Federal Holidays",
/// <https://www.opm.gov/policy-data-oversight/pay-leave/federal-holidays/>).
fn observed(day: i64) -> i64 {
    match weekday(day) {
        5 => day - 1,
        6 => day + 1,
        _ => day,
    }
}

/// U.S. federal holidays (5 U.S.C. 6103(a), observed per 6103(b)) for the
/// years [`CALENDAR_YEARS`], as day numbers, ascending: the days the Treasury
/// publishes no Debt to the Penny record. Observed days that fall in the
/// neighbouring year (New Year's Day on a Saturday) are kept only inside the
/// range.
pub fn federal_holidays() -> Vec<i64> {
    let (from, to) = CALENDAR_YEARS;
    let fixed = |y: i64, md: &str| observed(day_number(&format!("{y:04}-{md}")).unwrap_or(0));
    let mut out = Vec::new();
    for y in from..=to + 1 {
        out.push(fixed(y, "01-01")); // New Year's Day
        if y > to {
            break;
        }
        out.push(nth_weekday(y, 1, 0, 3)); // Birthday of Martin Luther King, Jr.
        out.push(nth_weekday(y, 2, 0, 3)); // Washington's Birthday
        out.push(last_weekday(y, 5, 0)); // Memorial Day
        out.push(fixed(y, "06-19")); // Juneteenth
        out.push(fixed(y, "07-04")); // Independence Day
        out.push(nth_weekday(y, 9, 0, 1)); // Labor Day
        out.push(nth_weekday(y, 10, 0, 2)); // Columbus Day
        out.push(fixed(y, "11-11")); // Veterans Day
        out.push(nth_weekday(y, 11, 3, 4)); // Thanksgiving Day
        out.push(fixed(y, "12-25")); // Christmas Day
    }
    let first = day_number(&format!("{from:04}-01-01")).unwrap_or(0);
    let last = day_number(&format!("{to:04}-12-31")).unwrap_or(0);
    out.retain(|d| (first..=last).contains(d));
    out.sort_unstable();
    out.dedup();
    out
}

/// The U.S. federal business calendar and this feed's lag rule.
pub fn date_lag() -> DateLag {
    let (from, to) = CALENDAR_YEARS;
    DateLag {
        min_business_days: MIN_BUSINESS_DAYS,
        max_calendar_days: MAX_LAG_DAYS,
        first_day: day_number(&format!("{from:04}-01-01")).unwrap_or(0),
        last_day: day_number(&format!("{to:04}-12-31")).unwrap_or(0),
        holidays: federal_holidays(),
    }
}

/// The metric's one canonical feed (L2SRC-R2-F1): what the coordinator
/// registers for [`METRIC`]. A round or claim of this metric exists only as
/// this template instantiated for `us.treasury.federal_debt:YYYY-MM-DD`.
pub fn template() -> FeedTemplate {
    FeedTemplate {
        feed: METRIC.into(),
        metric: MetricId(METRIC.into()),
        fetcher: FiscalDataFetcher.id(),
        url_template: url_template(),
        selector: SELECTOR.into(),
        // A published historical record does not move: every honest observer
        // reads the same cents.
        kind: ObservationKind::Numeric {
            scale: SCALE,
            tolerance: 0,
        },
        window_secs: WINDOW_SECS,
        min_observers: OBSERVERS,
        rights: rights(),
        subject: SubjectRule::Dated {
            prefix: SUBJECT.into(),
            lag: date_lag(),
        },
    }
}

/// Whether `record_date` may be observed in a window starting at
/// `window_start` on the federal business calendar ([`date_lag`]).
pub fn check_record_date(record_date: &str, window_start: u64) -> Result<(), String> {
    let record =
        day_number(record_date).ok_or_else(|| format!("bad record date {record_date:?}"))?;
    date_lag()
        .check(record, window_start)
        .map_err(|e| format!("record date {record_date}: {e}"))
}

/// The canonical feed for one record date, observed in
/// `[window_start, window_start + 300]`: [`template`] instantiated. Refuses a
/// record date the calendar does not allow ([`check_record_date`]).
pub fn spec(record_date: &str, window_start: u64) -> Result<ObservationSpec, String> {
    template()
        .instantiate(&Subject::Aggregate(subject(record_date)), window_start)
        .map_err(|e| format!("record date {record_date}: {e}"))
}

/// Whether `feed` is exactly the canonical feed for its own subject and
/// window ([`FeedTemplate::matches`], L2SRC-R1-F2).
pub fn is_canonical(feed: &ObservationSpec) -> bool {
    template().matches(feed).is_ok()
}

/// A registry that knows this metric, the dated subject for `record_date`, and
/// the source.
pub fn registry(record_date: &str) -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: SCALE },
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate(&subject(record_date)))
    .and_then(|r| r.add_source(rights()))
    .expect("registry entries are valid for a strict record date");
    r
}

/// Dollars and cents as a mantissa at scale 2: `digits "." two digits`, no
/// sign, no leading zero (except a lone `0`), no exponent (L2SRC-R1-F6).
pub fn parse_cents(s: &str) -> Option<i128> {
    let (int, frac) = s.split_once('.')?;
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
    if !digits(int) || frac.len() != 2 || !digits(frac) || (int.len() > 1 && int.starts_with('0')) {
        return None;
    }
    let whole: i128 = int.parse().ok()?;
    let cents: i128 = frac.parse().ok()?;
    whole.checked_mul(100)?.checked_add(cents)
}

/// The record date a feed URL filters on, if it filters on exactly one.
pub fn record_date_of(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("filter=record_date:eq:")?;
    let date = rest.get(..10)?;
    let tail = &rest[10..];
    let ok = date.bytes().enumerate().all(|(i, b)| {
        if i == 4 || i == 7 {
            b == b'-'
        } else {
            b.is_ascii_digit()
        }
    });
    (ok && (tail.is_empty() || tail.starts_with('&'))).then_some(date)
}

/// The Fiscal Data one-record fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct FiscalDataFetcher;

impl Fetcher for FiscalDataFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        if !is_canonical(spec) {
            return Answer::Unavailable;
        }
        let Some(date) = record_date_of(&spec.url) else {
            return Answer::Unavailable;
        };
        let Ok(doc) = serde_json::from_slice::<Json>(page) else {
            return Answer::Unavailable;
        };
        let one_record = doc
            .pointer("/data")
            .and_then(Json::as_array)
            .is_some_and(|d| d.len() == 1)
            && doc.pointer("/meta/count").and_then(Json::as_u64) == Some(1);
        if !one_record || doc.pointer("/data/0/record_date").and_then(Json::as_str) != Some(date) {
            return Answer::Unavailable;
        }
        match doc.pointer(&spec.selector) {
            Some(Json::String(s)) => parse_cents(s).map_or(Answer::Unavailable, Answer::Number),
            _ => Answer::Unavailable,
        }
    }
}

/// One recorded observer fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedFetch {
    /// Observer number (1-based).
    pub observer: u8,
    /// Local clock just before the request.
    pub fetched_unix: u64,
    /// The server's `Date` header.
    pub http_date: String,
    /// The response body.
    pub body: Vec<u8>,
}

/// The committed recording: record date and the observers' fetches.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The record date the feed pins.
    pub record_date: String,
    /// The pinned URL that was fetched.
    pub url: String,
    /// Fetches in observer order.
    pub fetches: Vec<RecordedFetch>,
}

/// Recording manifest of 2026-10-11 (record date 2026-10-08).
pub const RECORDED_MANIFEST: &str =
    include_str!("../fixtures/debt-to-penny-2026-10-08/fetches.json");
/// The recorded response body.
pub const RECORDED_BODY: &[u8] =
    include_bytes!("../fixtures/debt-to-penny-2026-10-08/response.json");
/// The window the recording was made in (05:26:00 UTC, 2026-10-11).
pub const RECORDED_WINDOW_START: u64 = 1_791_696_360;

/// Parse a manifest with its body.
pub fn parse_recording(manifest: &str, body: &[u8]) -> Result<Recording, String> {
    let m: Json = serde_json::from_str(manifest).map_err(|e| e.to_string())?;
    let s = |p: &str| {
        m.pointer(p)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("manifest {p} missing"))
    };
    let record_date = s("/record_date")?;
    let url = s("/url")?;
    if url != record_url(&record_date) {
        return Err("manifest url is not the pinned url for its date".into());
    }
    let fetches = m
        .pointer("/fetches")
        .and_then(Json::as_array)
        .ok_or("manifest fetches missing")?
        .iter()
        .map(|f| {
            Ok(RecordedFetch {
                observer: f
                    .get("observer")
                    .and_then(Json::as_u64)
                    .and_then(|o| u8::try_from(o).ok())
                    .ok_or("observer")?,
                fetched_unix: f
                    .get("fetched_unix")
                    .and_then(Json::as_u64)
                    .ok_or("fetched_unix")?,
                http_date: f
                    .get("http_date")
                    .and_then(Json::as_str)
                    .ok_or("http_date")?
                    .to_owned(),
                body: body.to_vec(),
            })
        })
        .collect::<Result<Vec<_>, &str>>()?;
    Ok(Recording {
        record_date,
        url,
        fetches,
    })
}

/// The committed recording.
pub fn recorded() -> Recording {
    parse_recording(RECORDED_MANIFEST, RECORDED_BODY).expect("committed recording parses")
}

/// The paid window a rehearsal prices the recorded round with: 30 days after
/// the round settles, stretched so that at `now` the value is still fresh for
/// another 30 days. The recorded round keeps its template-fixed (past) decay
/// start (`FeedTemplate::decay_start`), so the price clock is honest, and a demo run months later still shows a paid
/// sale (L2API-R1-F4).
pub fn rehearsal_fresh_window_secs(settled_unix: u64, now: u64) -> u64 {
    const THIRTY_DAYS: u64 = 30 * 86_400;
    now.saturating_sub(settled_unix)
        .saturating_add(THIRTY_DAYS)
        .max(THIRTY_DAYS)
}

/// `YYYY-MM-DD` of `day` (days since 1970-01-01), the inverse of
/// [`day_number`].
pub fn civil_date(day: i64) -> String {
    onyx_core::feed::civil_date(day)
}

/// The newest record date a window starting at `window_start` may observe
/// on the federal business calendar ([`check_record_date`]): what a live
/// round (ROADMAP B2's independent observers) pins. `None` when no date in
/// the allowed lag qualifies.
pub fn latest_record_date(window_start: u64) -> Option<String> {
    let today = i64::try_from(window_start / 86_400).ok()?;
    (1..=i64::from(MAX_LAG_DAYS))
        .map(|back| civil_date(today - back))
        .find(|d| check_record_date(d, window_start).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ROADMAP B2: a live round pins the newest record date its window may
    /// observe (a dated feed's window may start at any second); the
    /// recorded window gives the recorded date.
    #[test]
    fn b2_a_live_window_pins_the_newest_allowed_record_date() {
        assert_eq!(
            latest_record_date(RECORDED_WINDOW_START).as_deref(),
            Some("2026-10-08")
        );
        for day in 20_000..20_400 {
            let d = civil_date(day);
            assert_eq!(day_number(&d), Some(day), "{d}");
        }
        for w in (0..14).map(|d| RECORDED_WINDOW_START + d * 86_400 + 17) {
            let date = latest_record_date(w).expect("a date in the lag");
            assert!(spec(&date, w).is_ok(), "{date} at {w}");
        }
    }
    use onyx_core::observe;

    #[test]
    fn the_rehearsal_price_window_stays_fresh_far_in_the_future() {
        let settled = RECORDED_WINDOW_START + 600;
        // Right after the round, and years later: still 30 days of freshness.
        for now in [settled, settled + 40 * 86_400, 4_102_444_800] {
            let w = rehearsal_fresh_window_secs(settled, now);
            assert_eq!(
                w,
                (now - settled) + 30 * 86_400,
                "fresh for 30 more days at {now}"
            );
        }
        // A clock behind the round never shortens the window.
        assert_eq!(rehearsal_fresh_window_secs(settled, 0), 30 * 86_400);
    }

    #[test]
    fn the_recorded_record_reads_to_the_cent() {
        let rec = recorded();
        let s = spec(&rec.record_date, RECORDED_WINDOW_START).expect("canonical feed");
        s.validate().expect("feed is valid");
        assert_eq!(
            FiscalDataFetcher.extract(&s, RECORDED_BODY),
            Answer::Number(4_030_531_621_082_972)
        );
        for f in &rec.fetches {
            let o =
                observe(&FiscalDataFetcher, &s, Some(&f.body), f.fetched_unix).expect("in window");
            assert_eq!(o.page_hash, keccak256(RECORDED_BODY));
        }
    }

    #[test]
    fn anything_but_one_matching_record_is_unavailable() {
        let s = spec("2026-10-08", RECORDED_WINDOW_START).expect("canonical feed");
        let body = std::str::from_utf8(RECORDED_BODY).expect("utf8");
        let cases = [
            body.replace("2026-10-08", "2026-10-07"), // another day's record
            body.replace("\"count\":1", "\"count\":2"), // more than one record
            body.replace("\"40305316210829.72\"", "40305316210829.72"), // a JSON float
            body.replace("\"40305316210829.72\"", "\"4.03e13\""), // exponent
            body.replace("\"40305316210829.72\"", "\"40305316210829.7\""), // one decimal
            body.replace("\"40305316210829.72\"", "\"40305316210829.723\""), // three
            body.replace("\"40305316210829.72\"", "\"-40305316210829.72\""), // sign
            body.replace("\"40305316210829.72\"", "\"040305316210829.72\""), // leading 0
            body.replace("tot_pub_debt_out_amt\":\"", "other\":\""), // field missing
            "<html>maintenance</html>".to_owned(),
        ];
        for c in &cases {
            assert_eq!(
                FiscalDataFetcher.extract(&s, c.as_bytes()),
                Answer::Unavailable,
                "{c}"
            );
        }
        let mut no_filter = s.clone();
        no_filter.url = ENDPOINT.into();
        assert_eq!(
            FiscalDataFetcher.extract(&no_filter, RECORDED_BODY),
            Answer::Unavailable
        );
    }

    #[test]
    fn record_dates_parse_strictly() {
        assert_eq!(
            record_date_of(&record_url("2026-10-08")),
            Some("2026-10-08")
        );
        assert_eq!(
            record_date_of("x?filter=record_date:eq:2026-10-08&a=b"),
            Some("2026-10-08")
        );
        for bad in [
            "x?filter=record_date:gte:2026-10-08",
            "x?filter=record_date:eq:2026-10-0",
            "x?filter=record_date:eq:2026-10-08,2026-10-09",
            "x?filter=record_date:eq:2026/10/08",
        ] {
            assert_eq!(record_date_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn cents_parse_strictly() {
        assert_eq!(
            parse_cents("40305316210829.72"),
            Some(4_030_531_621_082_972)
        );
        assert_eq!(parse_cents("0.05"), Some(5));
        for bad in [
            "", ".72", "1.", "1.7", "1.723", "-1.00", "+1.00", "01.00", "1e3", "1,000.00", " 1.00",
        ] {
            assert_eq!(parse_cents(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn record_dates_must_be_recent_and_published() {
        let w = RECORDED_WINDOW_START; // Sunday 2026-10-11 UTC
        assert!(check_record_date("2026-10-08", w).is_ok()); // Thu, out Fri
        assert!(check_record_date("2026-10-01", w).is_ok()); // 10 days
        for bad in [
            "2026-10-09", // Fri: published Tue 10-13 (Mon is Columbus Day)
            "2026-10-10", // Sat: never published
            "2026-10-11",
            "2026-10-12",
            "2026-09-30", // 11 days
            "2001-01-02", // outside the calendar
            "2026-02-30",
            "2026-13-01",
        ] {
            assert!(check_record_date(bad, w).is_err(), "{bad}");
            assert!(spec(bad, w).is_err(), "{bad}");
        }
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
        assert_eq!(
            day_number("2026-10-11"),
            Some(i64::try_from(w / 86_400).expect("fits"))
        );
    }

    /// L2SRC-R2-F4: the federal calendar (5 U.S.C. 6103, observed days).
    #[test]
    fn federal_holidays_2026_and_observed_days() {
        let d = |s: &str| day_number(s).expect("date");
        let h = federal_holidays();
        let y2026: Vec<i64> = h
            .iter()
            .copied()
            .filter(|x| (d("2026-01-01")..=d("2026-12-31")).contains(x))
            .collect();
        let want: Vec<i64> = [
            "2026-01-01",
            "2026-01-19",
            "2026-02-16",
            "2026-05-25",
            "2026-06-19",
            "2026-07-03", // Jul 4 is a Saturday
            "2026-09-07",
            "2026-10-12",
            "2026-11-11",
            "2026-11-26",
            "2026-12-25",
        ]
        .iter()
        .map(|s| d(s))
        .collect();
        assert_eq!(y2026, want);
        // New Year's Day 2028 is a Saturday: observed Friday 2027-12-31.
        assert!(h.contains(&d("2027-12-31")));
        // Christmas 2033 is a Sunday: observed Monday 2033-12-26.
        assert!(h.contains(&d("2033-12-26")));
        assert!(h.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(h.len(), 11 * 10);
    }

    #[test]
    fn only_the_canonical_feed_is_read() {
        let canon = spec("2026-10-08", RECORDED_WINDOW_START).expect("canonical feed");
        assert!(is_canonical(&canon));
        let mut field = canon.clone();
        field.url = format!(
            "{ENDPOINT}?fields=record_date,debt_held_public_amt&filter=record_date:eq:2026-10-08"
        );
        field.selector = "/data/0/debt_held_public_amt".into();
        let mut host = canon.clone();
        host.url = "https://evil.example/debt?filter=record_date:eq:2026-10-08".into();
        let mut http = canon.clone();
        http.url = http.url.replacen("https://", "http://", 1);
        let mut selector = canon.clone();
        selector.selector = "/data/0/debt_held_public_amt".into();
        let mut subj = canon.clone();
        subj.subject = Subject::Aggregate(SUBJECT.into());
        let mut few = canon.clone();
        few.min_observers = 3;
        for (name, f) in [
            ("field", field),
            ("host", host),
            ("http", http),
            ("selector", selector),
            ("subject", subj),
            ("observers", few),
        ] {
            assert!(!is_canonical(&f), "{name}");
            assert_eq!(
                FiscalDataFetcher.extract(&f, RECORDED_BODY),
                Answer::Unavailable,
                "{name}"
            );
        }
    }
}
