//! Level-2 source (ADR-0027, indexer Phase 2 item 4): the crypto-asset
//! holdings a company states in its SEC filings, as tagged in XBRL
//! (`us-gaap:CryptoAssetNumberOfUnits`) and served by EDGAR's
//! `companyconcept` API, read by a pinned fetcher.
//!
//! What a fact says: "company C's filings tag `CryptoAssetNumberOfUnits` = N
//! (unit U) for the period ending D". It is a statement about a filing, never
//! about addresses or coins on chain, and never independently verified:
//! the company reported it to a regulator (SEC-2). Companies only.
//!
//! Rights: SEC.gov states "Information presented on sec.gov is considered
//! public information and may be copied or further distributed by users of
//! the web site without the SEC's permission"
//! (<https://www.sec.gov/privacy>). EDGAR's access page
//! (<https://www.sec.gov/os/accessing-edgar-data>) asks automated clients to
//! declare a User-Agent and stay at or under 10 requests per second. No
//! account, no API key. See `fixtures/PROVENANCE.md`.
//!
//! This crate never touches the network. An observer does its own HTTPS GET
//! of the feed's `url` and hands the bytes to [`EdgarFetcher`]; the recorded
//! responses in `fixtures/` let tests and the website rerun the rounds
//! offline.
//!
//! Each company's metric has one canonical feed, [`template`], whose subject
//! rule is [`SubjectRule::Period`] (SEC-1): the subject names the period end
//! (any calendar day), the selector carries that date, and the period is
//! observable only [`MIN_DAYS`]..=[`MAX_DAYS`] calendar days after it, by
//! which time every filer's 10-Q and 10-K deadline has passed, including a
//! Rule 12b-25 extension (SEC-R1-F4).
//!
//! Fetcher contract (pinned by [`FETCHER_TAG`]): the feed must equal its
//! company's template for its own subject and window; the response must be
//! that company's `companyconcept` document (its `cik`, taxonomy `us-gaap`,
//! tag `CryptoAssetNumberOfUnits`); every row of the template's unit whose
//! `end` is the period end must hold the same JSON integer `val` (no
//! fraction, no exponent, no sign: ADR-0009), and there must be at least one;
//! no other unit may hold a row for that period (an ambiguous filing). The
//! answer is that integer. Anything else is [`Answer::Unavailable`].

pub mod etf;
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
pub const FETCHER_TAG: &[u8] = b"onyx-fetcher:edgar-companyconcept:v1";
/// The XBRL concept.
pub const TAG: &str = "CryptoAssetNumberOfUnits";
/// The taxonomy the concept belongs to.
pub const TAXONOMY: &str = "us-gaap";
/// Fewest calendar days from a period end to the window's UTC day: the
/// longest filing deadline (a non-accelerated filer's 10-K, 90 days), plus
/// the 15-day Rule 12b-25 extension (Form NT 10-K), plus a weekend or holiday
/// roll (SEC-3, SEC-R1-F4).
pub const MIN_DAYS: u16 = 110;
/// Most calendar days from a period end to the window's day.
pub const MAX_DAYS: u16 = 730;
/// Numeric windows are at most 300 s (ADR-0027).
pub const WINDOW_SECS: u64 = 300;
/// Observers per round.
pub const OBSERVERS: u16 = 3;
/// Source id in the registry.
pub const SOURCE: &str = "sec.edgar";
/// The source's name shown on the website (constants here, never the summary
/// JSON; L2SRC-R2-F3).
pub const SOURCE_NAME: &str = "SEC EDGAR: XBRL company concept (data.sec.gov)";
/// The terms page.
pub const TERMS_URL: &str = "https://www.sec.gov/privacy";
/// The terms, quoted.
pub const TERMS_QUOTE: &str = "Information presented on sec.gov is considered public information and may be copied or further distributed by users of the web site without the SEC's permission.";
/// The User-Agent observers declare (EDGAR's access policy). It names the
/// project, not a person: no personal address is sent (SEC-4).
pub const USER_AGENT: &str = "Onyx research project (onyx-network data verification prototype)";

/// A company whose filings the feed reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Company {
    /// Short key used in metric ids and file names.
    pub key: &'static str,
    /// The company's name as EDGAR lists it.
    pub name: &'static str,
    /// The ten-digit CIK.
    pub cik: &'static str,
    /// The XBRL unit the company tags the concept with.
    pub unit: &'static str,
    /// What the unit means, in plain words, as shown on the website.
    pub unit_meaning: &'static str,
}

/// The companies with a registered feed. The `companyconcept` API serves only
/// facts without dimensions, so a value is the company-wide crypto-asset
/// unit total the filing tags, not a per-asset bitcoin figure (SEC-R1-F2).
/// These two are registered because, for the periods cross-checked in
/// `fixtures/PROVENANCE.md`, that total matches each company's own public
/// bitcoin holdings. CleanSpark is left out: its tagged values (e.g. 2,583 at
/// 2025-09-30) equal the bitcoin it posted as collateral or as a receivable,
/// not its 13,011 BTC treasury, so the concept tags a subset there.
pub const COMPANIES: [Company; 2] = [
    Company {
        key: "mstr",
        name: "Strategy Inc",
        cik: "0001050446",
        unit: "Bitcoin",
        unit_meaning: "crypto-asset units, unit tagged Bitcoin (company-wide total)",
    },
    Company {
        key: "riot",
        name: "Riot Platforms, Inc.",
        cik: "0001167419",
        unit: "item",
        unit_meaning: "crypto-asset units, unit tagged item (company-wide total)",
    },
];

/// The company with key `key`.
pub fn company(key: &str) -> Option<Company> {
    COMPANIES.iter().copied().find(|c| c.key == key)
}

/// The metric id of a company's feed.
pub fn metric(c: &Company) -> String {
    format!("sec.edgar.crypto_asset_units.{}", c.key)
}

/// The subject prefix: the company by CIK; a feed's subject is
/// `PREFIX:YYYY-MM-DD` ([`subject`]).
pub fn subject_prefix(c: &Company) -> String {
    format!("sec.cik{}.crypto_asset_units", c.cik)
}

/// The period subject for one period end.
pub fn subject(c: &Company, period_end: &str) -> String {
    format!("{}:{period_end}", subject_prefix(c))
}

/// The `companyconcept` URL for a company.
pub fn url(c: &Company) -> String {
    format!(
        "https://data.sec.gov/api/xbrl/companyconcept/CIK{}/{TAXONOMY}/{TAG}.json",
        c.cik
    )
}

/// The selector template: the unit's rows ending on `{date}`.
pub fn selector_template(c: &Company) -> String {
    format!("units/{}/end={{date}}", c.unit)
}

/// Reviewed rights for this source.
pub fn rights() -> DataRights {
    DataRights {
        license: "onyx:sec-gov-public-information".into(),
        redistribution: Redistribution::Allowed,
        source: SourceId(SOURCE.into()),
        attribution: Some(
            "Source: U.S. Securities and Exchange Commission, EDGAR XBRL data (data.sec.gov)"
                .into(),
        ),
        terms: String::new(),
    }
}

/// A company metric's one canonical feed (L2SRC-R2-F1).
pub fn template(c: &Company) -> FeedTemplate {
    FeedTemplate {
        feed: metric(c),
        metric: MetricId(metric(c)),
        fetcher: EdgarFetcher.id(),
        url_template: url(c),
        selector: selector_template(c),
        // A filed value does not move: every honest observer reads the same
        // integer.
        kind: ObservationKind::Numeric {
            scale: 0,
            tolerance: 0,
        },
        window_secs: WINDOW_SECS,
        min_observers: OBSERVERS,
        rights: rights(),
        subject: SubjectRule::Period {
            prefix: subject_prefix(c),
            min_days: MIN_DAYS,
            max_days: MAX_DAYS,
        },
    }
}

/// The company whose metric `m` is.
pub fn company_of(m: &MetricId) -> Option<Company> {
    COMPANIES.iter().copied().find(|c| metric(c) == m.0)
}

/// The canonical feed for a company's period, observed from `window_start`.
pub fn spec(c: &Company, period_end: &str, window_start: u64) -> Result<ObservationSpec, String> {
    template(c)
        .instantiate(&Subject::Aggregate(subject(c, period_end)), window_start)
        .map_err(|e| format!("{} period {period_end}: {e}", c.key))
}

/// Whether `feed` is exactly its company's canonical feed.
pub fn is_canonical(feed: &ObservationSpec) -> bool {
    company_of(&feed.metric).is_some_and(|c| template(&c).matches(feed).is_ok())
}

/// A registry that knows the company's metric, the period subject and the
/// source.
pub fn registry(c: &Company, period_end: &str) -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        &metric(c),
        MetricInfo {
            value_kind: ValueKind::Int,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate(&subject(c, period_end)))
    .and_then(|r| r.add_source(rights()))
    .expect("registry entries are valid for a strict period end");
    r
}

/// The non-negative JSON integer `v` holds, exactly: `None` for a fraction,
/// an exponent, a sign, or anything that is not a number token.
pub fn json_units(v: &Json) -> Option<u64> {
    match v {
        // serde_json keeps integer tokens as integers and anything with a
        // fraction or exponent as f64, so `as_u64` is exact or `None`.
        Json::Number(n) if n.is_u64() => n.as_u64(),
        _ => None,
    }
}

/// The EDGAR `companyconcept` fetcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct EdgarFetcher;

impl Fetcher for EdgarFetcher {
    fn id(&self) -> Hash32 {
        keccak256(FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        let Some(c) = company_of(&spec.metric) else {
            return Answer::Unavailable;
        };
        let t = template(&c);
        if t.matches(spec).is_err() {
            return Answer::Unavailable;
        }
        let Some(date) = t.date_of(&spec.subject) else {
            return Answer::Unavailable;
        };
        let Ok(doc) = serde_json::from_slice::<Json>(page) else {
            return Answer::Unavailable;
        };
        let cik_ok = c.cik.parse::<u64>().ok() == doc.get("cik").and_then(Json::as_u64);
        if !cik_ok
            || doc.get("taxonomy").and_then(Json::as_str) != Some(TAXONOMY)
            || doc.get("tag").and_then(Json::as_str) != Some(TAG)
        {
            return Answer::Unavailable;
        }
        let Some(units) = doc.get("units").and_then(Json::as_object) else {
            return Answer::Unavailable;
        };
        let rows_for = |rows: &Json| -> Option<Vec<Option<u64>>> {
            Some(
                rows.as_array()?
                    .iter()
                    .filter(|r| r.get("end").and_then(Json::as_str) == Some(date))
                    .map(|r| r.get("val").and_then(json_units))
                    .collect(),
            )
        };
        // Another unit tagging the same period: the filing is ambiguous.
        for (unit, rows) in units {
            if unit != c.unit && rows_for(rows).is_some_and(|v| !v.is_empty()) {
                return Answer::Unavailable;
            }
        }
        let Some(vals) = units.get(c.unit).and_then(rows_for) else {
            return Answer::Unavailable;
        };
        match vals.split_first() {
            Some((Some(first), rest)) if rest.iter().all(|v| *v == Some(*first)) => {
                Answer::Number(i128::from(*first))
            }
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

/// A committed recording of one company's feed.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The company.
    pub company: Company,
    /// The period end the feed pins.
    pub period_end: String,
    /// The window the fetches were made in.
    pub window_start: u64,
    /// Fetches in observer order.
    pub fetches: Vec<RecordedFetch>,
    /// The (identical) response body.
    pub body: Vec<u8>,
}

/// Strategy Inc's recording manifest (period 2026-03-31, fetched 2026-10-11).
pub const MSTR_MANIFEST: &str = include_str!("../fixtures/mstr-2026-03-31/fetches.json");
/// Strategy Inc's recorded response.
pub const MSTR_BODY: &[u8] = include_bytes!("../fixtures/mstr-2026-03-31/response.json");
/// Riot Platforms' recording manifest.
pub const RIOT_MANIFEST: &str = include_str!("../fixtures/riot-2026-03-31/fetches.json");
/// Riot Platforms' recorded response.
pub const RIOT_BODY: &[u8] = include_bytes!("../fixtures/riot-2026-03-31/response.json");

/// Parse a manifest with its body for `c`.
pub fn parse_recording(c: Company, manifest: &str, body: &[u8]) -> Result<Recording, String> {
    let m: Json = serde_json::from_str(manifest).map_err(|e| e.to_string())?;
    let period_end = m
        .pointer("/record_date")
        .and_then(Json::as_str)
        .ok_or("manifest record_date missing")?
        .to_owned();
    if m.pointer("/url").and_then(Json::as_str) != Some(url(&c).as_str()) {
        return Err("manifest url is not the company's pinned url".into());
    }
    let window_start = m
        .pointer("/window_start")
        .and_then(Json::as_u64)
        .ok_or("manifest window_start missing")?;
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
        company: c,
        period_end,
        window_start,
        fetches,
        body: body.to_vec(),
    })
}

/// The committed recordings, one per company in [`COMPANIES`] order.
pub fn recorded() -> Vec<Recording> {
    COMPANIES
        .iter()
        .map(|c| {
            let (m, b) = match c.key {
                "mstr" => (MSTR_MANIFEST, MSTR_BODY),
                _ => (RIOT_MANIFEST, RIOT_BODY),
            };
            parse_recording(*c, m, b).expect("committed recording parses")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use onyx_core::observe;

    fn mstr() -> Company {
        company("mstr").expect("mstr")
    }

    #[test]
    fn the_recorded_filings_read_exactly() {
        for (rec, want) in recorded().iter().zip([762_099i128, 15_679]) {
            let s = spec(&rec.company, &rec.period_end, rec.window_start).expect("canonical");
            s.validate().expect("feed is valid");
            assert_eq!(EdgarFetcher.extract(&s, &rec.body), Answer::Number(want));
            for f in &rec.fetches {
                let o =
                    observe(&EdgarFetcher, &s, Some(&f.body), f.fetched_unix).expect("in window");
                assert_eq!(o.page_hash, keccak256(&rec.body));
            }
        }
    }

    #[test]
    fn values_parse_as_exact_integers_only() {
        let rec = &recorded()[0];
        let s = spec(&rec.company, &rec.period_end, rec.window_start).expect("canonical");
        let body = std::str::from_utf8(&rec.body).expect("utf8");
        let cases = [
            body.replace("\"val\":762099,", "\"val\":762099.0,"), // a JSON float
            body.replace("\"val\":762099,", "\"val\":7.62099e5,"), // exponent
            body.replace("\"val\":762099,", "\"val\":-762099,"),  // sign
            body.replace("\"val\":762099,", "\"val\":\"762099\","), // a string
            body.replace("\"val\":762099,", "\"value\":762099,"), // field missing
            body.replace("\"end\":\"2026-03-31\"", "\"end\":\"2026-03-30\""), // no row
            body.replace("\"cik\":1050446", "\"cik\":1167419"),   // another company
            body.replace("\"tag\":\"CryptoAssetNumberOfUnits\"", "\"tag\":\"Other\""),
            body.replace("\"units\":{\"Bitcoin\"", "\"units\":{\"item\""), // another unit
            "<html>rate limit</html>".to_owned(),
        ];
        for c in &cases {
            assert_eq!(
                EdgarFetcher.extract(&s, c.as_bytes()),
                Answer::Unavailable,
                "{c}"
            );
        }
        // Two rows for the period that disagree (an amendment) are ambiguous.
        let doubled = body.replace(
            "\"end\":\"2026-03-31\",\"val\":762099,",
            "\"end\":\"2026-03-31\",\"val\":762099,\"x\":0},{\"end\":\"2026-03-31\",\"val\":762100,",
        );
        assert_ne!(doubled, body, "the fixture holds the period row");
        assert_eq!(
            EdgarFetcher.extract(&s, doubled.as_bytes()),
            Answer::Unavailable
        );
        // Two rows that agree (a comparative period repeated) are fine.
        let agree = body.replace(
            "\"end\":\"2026-03-31\",\"val\":762099,",
            "\"end\":\"2026-03-31\",\"val\":762099,\"x\":0},{\"end\":\"2026-03-31\",\"val\":762099,",
        );
        assert_eq!(
            EdgarFetcher.extract(&s, agree.as_bytes()),
            Answer::Number(762_099)
        );
        assert_eq!(
            json_units(&serde_json::json!(18_446_744_073_709_551_615u64)),
            Some(u64::MAX)
        );
        assert_eq!(json_units(&serde_json::json!(1.5)), None);
    }

    /// SEC-1: only the company's canonical feed is read.
    #[test]
    fn only_the_canonical_feed_is_read() {
        let rec = &recorded()[0];
        let canon = spec(&rec.company, &rec.period_end, rec.window_start).expect("canonical");
        assert!(is_canonical(&canon));
        let riot = company("riot").expect("riot");
        let mut cik = canon.clone();
        cik.url = url(&riot);
        let mut tag = canon.clone();
        tag.url = tag.url.replace(TAG, "CryptoAssetFairValue");
        let mut host = canon.clone();
        host.url = host.url.replace("data.sec.gov", "evil.example");
        let mut period = canon.clone();
        period.selector = "units/Bitcoin/end=2025-12-31".into();
        let mut unit = canon.clone();
        unit.selector = "units/item/end=2026-03-31".into();
        let mut subj = canon.clone();
        subj.subject = Subject::Aggregate(subject(&riot, "2026-03-31"));
        let mut few = canon.clone();
        few.min_observers = 5;
        for (name, f) in [
            ("cik", cik),
            ("tag", tag),
            ("host", host),
            ("period", period),
            ("unit", unit),
            ("subject", subj),
            ("observers", few),
        ] {
            assert!(!is_canonical(&f), "{name}");
            assert_eq!(
                EdgarFetcher.extract(&f, &rec.body),
                Answer::Unavailable,
                "{name}"
            );
        }
    }

    /// SEC-3: a period is observable only once every filing deadline passed,
    /// and not after two years; weekend period ends are allowed.
    #[test]
    fn periods_are_observable_only_after_the_deadlines() {
        let c = mstr();
        let day = |s: &str| u64::try_from(day_number(s).expect("date")).expect("pos") * 86_400;
        assert!(
            spec(&c, "2026-06-30", day("2026-10-18")).is_ok(),
            "110 days"
        );
        assert!(
            spec(&c, "2026-06-30", day("2026-10-17")).is_err(),
            "109 days: a 10-K filed under Rule 12b-25 may not be in yet"
        );
        assert!(
            spec(&c, "2024-09-30", day("2026-09-30")).is_ok(),
            "730 days"
        );
        assert!(
            spec(&c, "2024-09-29", day("2026-09-30")).is_err(),
            "731 days"
        );
        assert!(
            spec(&c, "2028-09-30", day("2029-01-20")).is_ok(),
            "a Saturday"
        );
        for bad in ["2026-06-31", "2026-6-30", "2026-06-30x", ""] {
            assert!(spec(&c, bad, day("2026-12-01")).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn templates_are_valid_and_distinct() {
        let ids: std::collections::BTreeSet<_> =
            COMPANIES.iter().map(|c| template(c).id()).collect();
        assert_eq!(ids.len(), COMPANIES.len());
        for c in &COMPANIES {
            template(c).validate().expect("valid");
            assert!(template(c).is_dated());
            assert_eq!(company_of(&MetricId(metric(c))), Some(*c));
        }
    }
}
