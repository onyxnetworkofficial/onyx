//! Level-2 source (ADR-0027, NS3): how many earthquakes of magnitude 5.0 or
//! more the U.S. Geological Survey's catalog (ComCat) lists worldwide on one
//! UTC day, read from the FDSN event web service's `count` endpoint.
//!
//! Rights: "USGS-authored or produced data and information are considered to
//! be in the U.S. Public Domain"
//! (<https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits>),
//! with credit asked for ("U.S. Geological Survey"). ComCat also merges events
//! from contributing networks, which that statement does not cover: the value
//! Onyx keeps is only a count (a fact), never their event records. No
//! account, no key. See `fixtures/PROVENANCE.md`.
//!
//! This crate never touches the network. An observer does its own HTTPS GET
//! of the feed's `url` and hands the bytes to [`UsgsCountFetcher`]. The
//! recorded responses in `fixtures/` let tests rerun the round offline.
//!
//! The metric's one canonical feed is [`template`] (L2SRC-R2-F1), a daily
//! subject ([`onyx_core::SubjectRule::Daily`]): `usgs.earthquakes.m5_world:YYYY-MM-DD`,
//! any calendar day, the URL asking from the day's first millisecond to its
//! last (`endtime` is inclusive, so the next midnight would count an event
//! at exactly 00:00:00.000 on two days). The catalog keeps revising a day for
//! weeks (events added late, magnitudes changed, duplicates merged; NS3
//! research: the last update typically 70 to 80 days after the event), so a
//! day is observable only [`MIN_DAYS`] to [`MAX_DAYS`] days after it. Limits
//! (THREATS NS3-8, NS3-9): a few events still change after 100 days, so a
//! late read can differ by one; and the service sits behind a CDN that caches
//! an answer for 60 s, so observers reading within a minute of each other may
//! read one cached copy rather than the catalog twice.
//!
//! Fetcher contract (pinned by [`FETCHER_TAG`]): the feed must equal the
//! template for its own day, and the whole body must be 1 to 9 ASCII digits
//! (no sign, no leading zero unless the count is 0, no whitespace). Anything
//! else is [`Answer::Unavailable`], never a guess.

pub mod region;
#[cfg(feature = "round")]
pub mod round;

use onyx_core::claim::{MetricId, Subject};
pub use onyx_core::day_number;
use onyx_core::{
    keccak256, Answer, DataRights, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind,
    ObservationSpec, Redistribution, SourceId, StaticRegistry, SubjectRule, ValueKind,
};
use serde_json::Value as Json;

/// Version tag the fetcher id is derived from.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:usgs-fdsn-count:v1";
/// The FDSN event web service's count endpoint.
pub const ENDPOINT: &str = "https://earthquake.usgs.gov/fdsnws/event/1/count";
/// The smallest magnitude counted, exactly as the URL states it (the
/// service counts events at this magnitude too).
pub const MIN_MAGNITUDE: &str = "5.0";
/// Feed and metric id.
pub const METRIC: &str = "usgs.earthquakes.m5_count";
/// The subject prefix: the world; a feed's subject is `SUBJECT:YYYY-MM-DD`.
pub const SUBJECT: &str = "usgs.earthquakes.m5_world";
/// Fewest days from the day to the window: the catalog's revisions have
/// mostly settled (NS3 research, 2026-10-26: of 1,914 M4.5+ events of
/// April-June 2026 the last update came 70-80 days after the event for most,
/// and 2.7% were still updated after 100 days).
pub const MIN_DAYS: u16 = 100;
/// Most days from the day to the window. Short on purpose (review NS3-R1
/// finding 9): the catalog still changes a little after 100 days, so a long
/// range would let a coordinator read the catalog first and pick the window
/// whose count it wants.
pub const MAX_DAYS: u16 = 120;
/// Where the source's name, terms and quote shown come from: constants here,
/// never the summary JSON (L2SRC-R2-F3).
pub const SOURCE_NAME: &str = "U.S. Geological Survey: ANSS Comprehensive Earthquake Catalog";
/// The terms page.
pub const TERMS_URL: &str =
    "https://www.usgs.gov/information-policies-and-instructions/copyrights-and-credits";
/// The terms, quoted.
pub const TERMS_QUOTE: &str =
    "USGS-authored or produced data and information are considered to be in the U.S. Public Domain";
/// Source id in the registry.
pub const SOURCE: &str = "usgs.comcat";
/// The selector: the whole body is the count.
pub const SELECTOR: &str = "body";
/// A count has no decimals.
pub const SCALE: u8 = 0;
/// Numeric windows are at most 300 s (ADR-0027).
pub const WINDOW_SECS: u64 = 300;
/// Observers per round.
pub const OBSERVERS: u16 = 5;
/// Longest count accepted (a day has never had a billion events).
const MAX_DIGITS: usize = 9;

/// Reviewed rights for this source.
pub fn rights() -> DataRights {
    DataRights {
        // A count derived from ComCat: a fact. USGS-authored parts are public
        // domain; contributed networks' event records are not copied (review
        // NS3-R1 finding 6).
        license: "onyx:facts-from-usgs-comcat".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: Some(
            "Source: U.S. Geological Survey, ANSS Comprehensive Earthquake Catalog (earthquake.usgs.gov)"
                .into(),
        ),
        terms: String::new(),
    }
}

/// The URL template: `{date}` at the day's start and end, earthquakes only
/// (without `eventtype` the catalog also counts explosions such as nuclear
/// tests: 2 events of M5.0+ on 2017-09-03, 1 of them an earthquake; review
/// NS3-R1 finding 1).
pub fn url_template() -> String {
    format!(
        "{ENDPOINT}?starttime={{date}}T00:00:00&endtime={{date}}T23:59:59.999&minmagnitude={MIN_MAGNITUDE}&eventtype=earthquake"
    )
}

/// The pinned URL for one day (`YYYY-MM-DD`).
pub fn day_url(day: &str) -> String {
    url_template().replace(onyx_core::feed::DATE_PLACEHOLDER, day)
}

/// The daily subject for one day: `usgs.earthquakes.m5_world:YYYY-MM-DD`.
pub fn subject(day: &str) -> String {
    format!("{SUBJECT}:{day}")
}

/// The metric's one canonical feed (L2SRC-R2-F1).
pub fn template() -> FeedTemplate {
    FeedTemplate {
        feed: METRIC.into(),
        metric: MetricId(METRIC.into()),
        fetcher: UsgsCountFetcher.id(),
        url_template: url_template(),
        selector: SELECTOR.into(),
        // Read after the revisions settle: every honest observer reads the
        // same count.
        kind: ObservationKind::Numeric {
            scale: SCALE,
            tolerance: 0,
        },
        window_secs: WINDOW_SECS,
        min_observers: OBSERVERS,
        rights: rights(),
        subject: SubjectRule::Daily {
            prefix: SUBJECT.into(),
            min_days: MIN_DAYS,
            max_days: MAX_DAYS,
        },
    }
}

/// The canonical feed for one day, observed in
/// `[window_start, window_start + 300]`: [`template`] instantiated. Refuses a
/// day the rule does not allow.
pub fn spec(day: &str, window_start: u64) -> Result<ObservationSpec, String> {
    template()
        .instantiate(&Subject::Aggregate(subject(day)), window_start)
        .map_err(|e| format!("day {day}: {e}"))
}

/// Whether `feed` is exactly the canonical feed for its own subject and
/// window ([`FeedTemplate::matches`]).
pub fn is_canonical(feed: &ObservationSpec) -> bool {
    template().matches(feed).is_ok()
}

/// A registry that knows this metric, the subject of `day`, and the source.
pub fn registry(day: &str) -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::Int,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate(&subject(day)))
    .and_then(|r| r.add_source(rights()))
    .expect("registry entries are valid for a strict day");
    r
}

/// A count: 1 to 9 ASCII digits, no leading zero unless it is `0`.
pub fn parse_count(body: &[u8]) -> Option<i128> {
    let ok = !body.is_empty()
        && body.len() <= MAX_DIGITS
        && body.iter().all(u8::is_ascii_digit)
        && (body.len() == 1 || body[0] != b'0');
    ok.then(|| std::str::from_utf8(body).ok()?.parse().ok())
        .flatten()
}

/// The USGS FDSN count fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct UsgsCountFetcher;

impl Fetcher for UsgsCountFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        if !is_canonical(spec) || spec.selector != SELECTOR {
            return Answer::Unavailable;
        }
        parse_count(page).map_or(Answer::Unavailable, Answer::Number)
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

/// The committed recording: the day and the observers' fetches.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The day the feed pins.
    pub day: String,
    /// The pinned URL that was fetched.
    pub url: String,
    /// Fetches in observer order.
    pub fetches: Vec<RecordedFetch>,
}

/// Recording manifest of 2026-10-26 (day 2026-07-08, 110 days earlier).
pub const RECORDED_MANIFEST: &str = include_str!("../fixtures/quakes-m5-2026-07-08/fetches.json");
/// The recorded response body.
pub const RECORDED_BODY: &[u8] = include_bytes!("../fixtures/quakes-m5-2026-07-08/response.txt");

/// Parse a manifest with its body.
pub fn parse_recording(manifest: &str, body: &[u8]) -> Result<Recording, String> {
    let m: Json = serde_json::from_str(manifest).map_err(|e| e.to_string())?;
    let s = |p: &str| {
        m.pointer(p)
            .and_then(Json::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("manifest {p} missing"))
    };
    let day = s("/day")?;
    let url = s("/url")?;
    if url != day_url(&day) {
        return Err("manifest url is not the pinned url for its day".into());
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
    Ok(Recording { day, url, fetches })
}

/// The committed recording.
pub fn recorded() -> Recording {
    parse_recording(RECORDED_MANIFEST, RECORDED_BODY).expect("committed recording parses")
}

/// The window the recording was made in: it opens just before the first
/// fetch, and every fetch falls inside it.
pub fn recorded_window_start() -> u64 {
    recorded()
        .fetches
        .iter()
        .map(|f| f.fetched_unix)
        .min()
        .unwrap_or(0)
        .saturating_sub(5)
}
