//! Level-2 source (ADR-0027, NS3): the daily maximum temperature (TMAX, °C to
//! a tenth) at Denver International Airport, station `USW00003017`, from
//! NOAA NCEI's Global Historical Climatology Network - Daily (GHCN-Daily),
//! read through the Access Data Service.
//!
//! Rights: a work of the U.S. Government ("Copyright protection under this
//! title is not available for any work of the United States Government",
//! 17 U.S.C. 105(a)); the station's values come from the National Weather
//! Service, whose pages "are in the public domain, unless specifically noted
//! otherwise" (<https://www.weather.gov/disclaimer>), on three conditions:
//! not claiming it as one's own, not implying endorsement, not presenting
//! modified content as official. NCEI asks a citation ("Cite as", GHCN-Daily
//! version 3, doi:10.7289/V5D21VHZ) and states no licence. No account, no
//! key. See `fixtures/PROVENANCE.md`.
//!
//! This crate never touches the network. An observer does its own HTTPS GET
//! of the feed's `url` and hands the bytes to [`GhcndFetcher`].
//!
//! The metric's one canonical feed is [`template`], a daily subject
//! ([`onyx_core::SubjectRule::Daily`]): `noaa.ghcnd.usw00003017:YYYY-MM-DD`,
//! the URL asking for that day as both start and end date, in metric units
//! (without `units=metric` the service answers tenths with leading spaces; with
//! `units=standard`, °F). GHCN-Daily appends updates and changed values 5 to 25
//! days back in a sampled diff file (NS3 research), so a day is observable
//! only [`MIN_DAYS`] to [`MAX_DAYS`] days after it.
//!
//! Fetcher contract (pinned by [`FETCHER_TAG`]): the feed must equal the
//! template for its own day; the body must be a JSON array of exactly one
//! object with exactly the keys `DATE` (the feed's day), `STATION`
//! ([`STATION`]) and `TMAX`, a JSON string of an optional `-`, an integer part
//! without leading zeros, a point and one digit (no `-0.0`). Anything else
//! (an empty array for a missing day, a second row, an extra field such as a
//! quality flag) is [`Answer::Unavailable`], never a guess.

pub mod nws;
#[cfg(feature = "round")]
pub mod round;
pub mod series;

use onyx_core::claim::{MetricId, Subject};
pub use onyx_core::day_number;
use onyx_core::{
    keccak256, Answer, DataRights, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind,
    ObservationSpec, Redistribution, SourceId, StaticRegistry, SubjectRule, ValueKind,
};
use serde_json::Value as Json;

/// Version tag the fetcher id is derived from.
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:ncei-ghcnd-daily-value:v1";
/// The Access Data Service endpoint.
pub const ENDPOINT: &str = "https://www.ncei.noaa.gov/access/services/data/v1";
/// The station: Denver International Airport.
pub const STATION: &str = "USW00003017";
/// Feed and metric id.
pub const METRIC: &str = "noaa.ghcnd.usw00003017.tmax";
/// The subject prefix: the station; a feed's subject is `SUBJECT:YYYY-MM-DD`.
pub const SUBJECT: &str = "noaa.ghcnd.usw00003017";
/// Fewest days from the day to the window (GHCN-Daily's late updates).
pub const MIN_DAYS: u16 = 30;
/// Most days from the day to the window. Short on purpose (review NS3-R1
/// finding 9): a coordinator must not be able to read the archive first and
/// pick a late window whose value it prefers.
pub const MAX_DAYS: u16 = 60;
/// Where the source's name, terms and quote shown come from (L2SRC-R2-F3).
pub const SOURCE_NAME: &str =
    "NOAA NCEI GHCN-Daily: daily maximum temperature, Denver International Airport";
/// The terms page.
pub const TERMS_URL: &str = "https://www.weather.gov/disclaimer";
/// The terms, quoted.
pub const TERMS_QUOTE: &str =
    "are in the public domain, unless specifically noted otherwise, and may be used without charge for any lawful purpose";
/// Source id in the registry.
pub const SOURCE: &str = "noaa.ncei.ghcnd";
/// The selector: the one row's `TMAX`.
pub const SELECTOR: &str = "/0/TMAX";
/// Degrees Celsius to a tenth.
pub const SCALE: u8 = 1;
/// Numeric windows are at most 300 s (ADR-0027).
pub const WINDOW_SECS: u64 = 300;
/// Observers per round.
pub const OBSERVERS: u16 = 5;

/// Reviewed rights for this source.
pub fn rights() -> DataRights {
    rights_with(STATION)
}

/// The reviewed rights with the attribution naming `station` (every series of this service,
/// [`series`]).
pub fn rights_with(station: &str) -> DataRights {
    DataRights {
        license: "onyx:us-government-work-noaa".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: Some(format!(
            "Source: NOAA National Centers for Environmental Information, GHCN-Daily version 3 (doi:10.7289/V5D21VHZ), station {station}; not an official NOAA product as shown here"
        )),
        terms: String::new(),
    }
}

/// The URL template: `{date}` as the start and the end date.
pub fn url_template() -> String {
    format!(
        "{ENDPOINT}?dataset=daily-summaries&stations={STATION}&startDate={{date}}&endDate={{date}}&dataTypes=TMAX&units=metric&format=json"
    )
}

/// The pinned URL for one day (`YYYY-MM-DD`).
pub fn day_url(day: &str) -> String {
    url_template().replace(onyx_core::feed::DATE_PLACEHOLDER, day)
}

/// The daily subject for one day: `noaa.ghcnd.usw00003017:YYYY-MM-DD`.
pub fn subject(day: &str) -> String {
    format!("{SUBJECT}:{day}")
}

/// The metric's one canonical feed (L2SRC-R2-F1).
pub fn template() -> FeedTemplate {
    FeedTemplate {
        feed: METRIC.into(),
        metric: MetricId(METRIC.into()),
        fetcher: GhcndFetcher.id(),
        url_template: url_template(),
        selector: SELECTOR.into(),
        // Read after the late updates: every honest observer reads the same
        // tenths.
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
/// `[window_start, window_start + 300]`. Refuses a day the rule does not
/// allow.
pub fn spec(day: &str, window_start: u64) -> Result<ObservationSpec, String> {
    template()
        .instantiate(&Subject::Aggregate(subject(day)), window_start)
        .map_err(|e| format!("day {day}: {e}"))
}

/// Whether `feed` is exactly the canonical feed for its own subject and
/// window.
pub fn is_canonical(feed: &ObservationSpec) -> bool {
    template().matches(feed).is_ok()
}

/// A registry that knows this metric, the subject of `day`, and the source.
pub fn registry(day: &str) -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        METRIC,
        MetricInfo {
            value_kind: ValueKind::Fixed { scale: SCALE },
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate(&subject(day)))
    .and_then(|r| r.add_source(rights()))
    .expect("registry entries are valid for a strict day");
    r
}

/// Tenths of a degree: `-?` then an integer part of 1 to 3 digits without a
/// leading zero (a lone `0` allowed), a point, one digit; never `-0.0`.
pub fn parse_tenths(s: &str) -> Option<i128> {
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = rest.split_once('.')?;
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
    if !digits(int) || int.len() > 3 || (int.len() > 1 && int.starts_with('0')) {
        return None;
    }
    if frac.len() != 1 || !digits(frac) {
        return None;
    }
    let v: i128 = int.parse::<i128>().ok()? * 10 + frac.parse::<i128>().ok()?;
    if neg && v == 0 {
        return None;
    }
    Some(if neg { -v } else { v })
}

/// The GHCN-Daily one-value fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct GhcndFetcher;

impl Fetcher for GhcndFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        if !is_canonical(spec) || spec.selector != SELECTOR {
            return Answer::Unavailable;
        }
        let Some(day) = template().date_of(&spec.subject).map(str::to_owned) else {
            return Answer::Unavailable;
        };
        let Ok(doc) = serde_json::from_slice::<Json>(page) else {
            return Answer::Unavailable;
        };
        let Some([row]) = doc.as_array().map(Vec::as_slice) else {
            return Answer::Unavailable;
        };
        let Some(obj) = row.as_object() else {
            return Answer::Unavailable;
        };
        let exact_keys = obj.len() == 3
            && obj.get("DATE").and_then(Json::as_str) == Some(day.as_str())
            && obj.get("STATION").and_then(Json::as_str) == Some(STATION);
        if !exact_keys {
            return Answer::Unavailable;
        }
        match obj.get("TMAX") {
            Some(Json::String(s)) => parse_tenths(s).map_or(Answer::Unavailable, Answer::Number),
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

/// Recording manifest of 2026-10-26 (day 2026-09-01).
pub const RECORDED_MANIFEST: &str =
    include_str!("../fixtures/tmax-usw00003017-2026-09-01/fetches.json");
/// The recorded response body.
pub const RECORDED_BODY: &[u8] =
    include_bytes!("../fixtures/tmax-usw00003017-2026-09-01/response.json");

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
