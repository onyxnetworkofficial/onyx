//! More GHCN-Daily series (north-star loop NL17, docs/spec/more-datasets.md rows 1 and 2): the same
//! Access Data Service, rights record and value format as the Denver TMAX feed
//! ([`crate::template`]), for other U.S. Weather Service stations and elements.
//!
//! Each [`Series`] is one metric with its own canonical feed (a daily subject, the station's
//! prefix), observable [`crate::MIN_DAYS`] to [`crate::MAX_DAYS`] days after the day: GHCN-Daily's
//! late updates cover every element of a station, so the same lag holds (fixtures/PROVENANCE.md).
//! Only U.S. `USW` stations: their values come from the National Weather Service, whose terms
//! the rights record quotes; GHCN stations of other networks have other terms (the spec's
//! assumption 1).
//!
//! Fetcher contract (pinned by [`SERIES_FETCHER_TAG`], one fetcher for every series): the feed
//! must equal its series' template for its own day; the body must be a JSON array of exactly one
//! object with exactly the keys `DATE` (the feed's day), `STATION` (the series' station) and the
//! series' element, a JSON string in the format [`crate::parse_tenths`] reads; a precipitation
//! value must not be negative. Anything else is [`Answer::Unavailable`], never a guess.

use crate::{
    parse_tenths, rights_with, ENDPOINT, MAX_DAYS, MIN_DAYS, OBSERVERS, SCALE, WINDOW_SECS,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, Answer, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind, ObservationSpec,
    StaticRegistry, SubjectRule, ValueKind,
};
use serde_json::Value as Json;

/// Version tag the series fetcher's id is derived from.
pub const SERIES_FETCHER_TAG: &[u8] = b"onyx-fetcher:ncei-ghcnd-series:v1";

/// One station and element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Series {
    /// The short id `ONYX_L2_SERIES` names (`usw00003017.prcp`).
    pub id: &'static str,
    /// The GHCN-Daily station id.
    pub station: &'static str,
    /// The GHCN-Daily element (`TMAX`, `PRCP`).
    pub element: &'static str,
    /// Feed and metric id.
    pub metric: &'static str,
    /// The subject prefix: the station.
    pub subject: &'static str,
    /// The source's name as the website and the API show it.
    pub name: &'static str,
    /// The value's unit.
    pub unit: &'static str,
    /// The reviewer's note when the template is registered.
    pub note: &'static str,
}

/// Daily precipitation, Denver International Airport (spec row 1).
pub const DENVER_PRCP: Series = Series {
    id: "usw00003017.prcp",
    station: "USW00003017",
    element: "PRCP",
    metric: "noaa.ghcnd.usw00003017.prcp",
    subject: "noaa.ghcnd.usw00003017",
    name: "NOAA NCEI GHCN-Daily: daily precipitation, Denver International Airport",
    unit: "millimetres",
    note: "GHCN-Daily PRCP at USW00003017: rights reviewed (fixtures/PROVENANCE.md), feed pinned",
};

/// Daily maximum temperature, New York Central Park (spec row 2).
pub const NYC_TMAX: Series = Series {
    id: "usw00094728.tmax",
    station: "USW00094728",
    element: "TMAX",
    metric: "noaa.ghcnd.usw00094728.tmax",
    subject: "noaa.ghcnd.usw00094728",
    name: "NOAA NCEI GHCN-Daily: daily maximum temperature, New York Central Park",
    unit: "degrees Celsius",
    note: "GHCN-Daily TMAX at USW00094728: rights reviewed (fixtures/PROVENANCE.md), feed pinned",
};

/// Daily precipitation, New York Central Park (spec row 2).
pub const NYC_PRCP: Series = Series {
    id: "usw00094728.prcp",
    station: "USW00094728",
    element: "PRCP",
    metric: "noaa.ghcnd.usw00094728.prcp",
    subject: "noaa.ghcnd.usw00094728",
    name: "NOAA NCEI GHCN-Daily: daily precipitation, New York Central Park",
    unit: "millimetres",
    note: "GHCN-Daily PRCP at USW00094728: rights reviewed (fixtures/PROVENANCE.md), feed pinned",
};

/// Every series, in the order the docs list them.
pub const ALL: [&Series; 3] = [&DENVER_PRCP, &NYC_TMAX, &NYC_PRCP];

/// The series of `id`.
pub fn by_id(id: &str) -> Option<&'static Series> {
    ALL.into_iter().find(|s| s.id == id)
}

/// The series whose metric is `metric`.
pub fn by_metric(metric: &str) -> Option<&'static Series> {
    ALL.into_iter().find(|s| s.metric == metric)
}

impl Series {
    /// The URL template: `{date}` as the start and the end date, metric units.
    pub fn url_template(&self) -> String {
        format!(
            "{ENDPOINT}?dataset=daily-summaries&stations={}&startDate={{date}}&endDate={{date}}&dataTypes={}&units=metric&format=json",
            self.station, self.element
        )
    }

    /// The pinned URL for one day.
    pub fn day_url(&self, day: &str) -> String {
        self.url_template()
            .replace(onyx_core::feed::DATE_PLACEHOLDER, day)
    }

    /// The daily subject for one day.
    pub fn subject(&self, day: &str) -> String {
        format!("{}:{day}", self.subject)
    }

    /// The selector: the one row's element.
    pub fn selector(&self) -> String {
        format!("/0/{}", self.element)
    }

    /// The series' one canonical feed.
    pub fn template(&self) -> FeedTemplate {
        FeedTemplate {
            feed: self.metric.into(),
            metric: MetricId(self.metric.into()),
            fetcher: SeriesFetcher.id(),
            url_template: self.url_template(),
            selector: self.selector(),
            kind: ObservationKind::Numeric {
                scale: SCALE,
                tolerance: 0,
            },
            window_secs: WINDOW_SECS,
            min_observers: OBSERVERS,
            rights: rights_with(self.station),
            subject: SubjectRule::Daily {
                prefix: self.subject.into(),
                min_days: MIN_DAYS,
                max_days: MAX_DAYS,
            },
        }
    }

    /// The canonical feed for one day from `window_start`; refused when the rule does not allow
    /// the day.
    pub fn spec(&self, day: &str, window_start: u64) -> Result<ObservationSpec, String> {
        self.template()
            .instantiate(&Subject::Aggregate(self.subject(day)), window_start)
            .map_err(|e| format!("day {day}: {e}"))
    }

    /// A registry that knows this metric, the subject of `day`, and the source.
    pub fn registry(&self, day: &str) -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.add_metric(
            self.metric,
            MetricInfo {
                value_kind: ValueKind::Fixed { scale: SCALE },
                needs_label_set: false,
            },
        )
        .and_then(|r| r.add_aggregate(&self.subject(day)))
        .and_then(|r| r.add_source(rights_with(self.station)))
        .expect("registry entries are valid for a strict day");
        r
    }
}

/// The fetcher of every [`Series`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SeriesFetcher;

impl Fetcher for SeriesFetcher {
    fn id(&self) -> Hash32 {
        keccak256(SERIES_FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        let Some(s) = by_metric(&spec.metric.0) else {
            return Answer::Unavailable;
        };
        let t = s.template();
        if t.matches(spec).is_err() {
            return Answer::Unavailable;
        }
        let Some(day) = t.date_of(&spec.subject).map(str::to_owned) else {
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
            && obj.get("STATION").and_then(Json::as_str) == Some(s.station);
        if !exact_keys {
            return Answer::Unavailable;
        }
        match obj.get(s.element) {
            Some(Json::String(v)) => match parse_tenths(v) {
                Some(n) if n < 0 && s.element == "PRCP" => Answer::Unavailable,
                Some(n) => Answer::Number(n),
                None => Answer::Unavailable,
            },
            _ => Answer::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: u64 = 1_792_022_400; // 2026-10-15T00:00:00Z: 44 days after 2026-09-01

    fn body(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/fixtures/series-2026-09-01/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture")
    }

    #[test]
    fn each_recorded_series_reads_its_value() {
        for (s, file, want) in [
            (&DENVER_PRCP, "prcp-usw00003017", 0),
            (&NYC_TMAX, "tmax-usw00094728", 278),
            (&NYC_PRCP, "prcp-usw00094728", 99),
        ] {
            let spec = s.spec("2026-09-01", START).expect("spec");
            assert_eq!(
                SeriesFetcher.extract(&spec, &body(file)),
                Answer::Number(want),
                "{}",
                s.id
            );
            assert_eq!(by_id(s.id), Some(s));
            // The same body under another series is refused (station or element differs).
            for other in ALL.into_iter().filter(|o| o.id != s.id) {
                let spec = other.spec("2026-09-01", START).expect("spec");
                assert_eq!(
                    SeriesFetcher.extract(&spec, &body(file)),
                    Answer::Unavailable,
                    "{} as {}",
                    s.id,
                    other.id
                );
            }
        }
    }

    #[test]
    fn anything_but_the_one_row_is_unavailable() {
        let spec = NYC_PRCP.spec("2026-09-01", START).expect("spec");
        for bad in [
            r#"[]"#,
            r#"[{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":"-0.1"}]"#,
            r#"[{"DATE":"2026-09-02","STATION":"USW00094728","PRCP":"9.9"}]"#,
            r#"[{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":"9.90"}]"#,
            r#"[{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":9.9}]"#,
            r#"[{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":"9.9","Q":"x"}]"#,
            r#"[{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":"9.9"},{"DATE":"2026-09-01","STATION":"USW00094728","PRCP":"9.9"}]"#,
        ] {
            assert_eq!(
                SeriesFetcher.extract(&spec, bad.as_bytes()),
                Answer::Unavailable,
                "{bad}"
            );
        }
        // A feed that is not its series' canonical one.
        let mut moved = spec.clone();
        moved.url = moved.url.replace("units=metric", "units=standard");
        assert_eq!(
            SeriesFetcher.extract(&moved, &body("prcp-usw00094728")),
            Answer::Unavailable
        );
    }

    #[test]
    fn templates_are_distinct_valid_and_keep_the_denver_tmax_feed() {
        let mut ids: Vec<Hash32> = ALL.iter().map(|s| s.template().id()).collect();
        ids.push(crate::template().id());
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n, "every series is its own template");
        for s in ALL {
            s.template().validate().expect("valid");
            assert!(s.spec("2026-10-14", START).is_err(), "{}: too new", s.id);
            assert!(s.spec("2026-08-01", START).is_err(), "{}: too old", s.id);
        }
        // The Denver TMAX feed (live on Base Sepolia, ROADMAP B6) keeps its fetcher and pin.
        assert_eq!(crate::template().fetcher, keccak256(crate::FETCHER_TAG));
        assert_eq!(
            crate::template().id().to_string(),
            "0x4399b07e0ffa68bd73eab1480828afcb66aea43503eb84e0b1cf192acc512675"
        );
    }
}
