//! Regional earthquake counts (north-star loop NL17, docs/spec/more-datasets.md row 3): the same
//! FDSN `count` endpoint, rights record and count format as the world M5+ feed
//! ([`crate::template`]), for the earthquakes within a circle around a place.
//!
//! Each [`Region`] is one metric with its own canonical feed (a daily subject), observable
//! [`crate::MIN_DAYS`] to [`crate::MAX_DAYS`] days after the day: the same catalog revisions apply
//! (fixtures/PROVENANCE.md). The circle and the magnitude floor are written into the URL exactly as
//! the constants state them, so an observer cannot widen the area or lower the floor.
//!
//! Fetcher contract (pinned by [`REGION_FETCHER_TAG`], one fetcher for every region): the feed
//! must equal its region's template for its own day, and the whole body must be a count as
//! [`crate::parse_count`] reads it. Anything else is [`Answer::Unavailable`].

use crate::{
    parse_count, rights, ENDPOINT, MAX_DAYS, MIN_DAYS, OBSERVERS, SCALE, SELECTOR, WINDOW_SECS,
};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, Answer, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind, ObservationSpec,
    StaticRegistry, SubjectRule, ValueKind,
};

/// Version tag the region fetcher's id is derived from.
pub const REGION_FETCHER_TAG: &[u8] = b"onyx-fetcher:usgs-fdsn-region-count:v1";

/// A circle and a magnitude floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// The short id `ONYX_L2_SERIES` names (`la300.m2_5`).
    pub id: &'static str,
    /// Feed and metric id.
    pub metric: &'static str,
    /// The subject prefix.
    pub subject: &'static str,
    /// The circle's centre latitude, as the URL states it.
    pub latitude: &'static str,
    /// The circle's centre longitude, as the URL states it.
    pub longitude: &'static str,
    /// The circle's radius in kilometres, as the URL states it.
    pub radius_km: &'static str,
    /// The smallest magnitude counted, as the URL states it.
    pub min_magnitude: &'static str,
    /// The source's name as the website and the API show it.
    pub name: &'static str,
    /// The reviewer's note when the template is registered.
    pub note: &'static str,
}

/// Earthquakes of magnitude 2.5 or more within 300 km of Los Angeles (34.05, -118.25).
pub const LA_300_M2_5: Region = Region {
    id: "la300.m2_5",
    metric: "usgs.earthquakes.la300km_m2_5_count",
    subject: "usgs.earthquakes.la300km_m2_5",
    latitude: "34.05",
    longitude: "-118.25",
    radius_km: "300",
    min_magnitude: "2.5",
    name: "U.S. Geological Survey: ANSS Comprehensive Earthquake Catalog, M2.5+ within 300 km of Los Angeles",
    note: "USGS regional earthquake count (300 km around Los Angeles, M2.5+): rights reviewed (fixtures/PROVENANCE.md), feed pinned",
};

/// Every region.
pub const ALL: [&Region; 1] = [&LA_300_M2_5];

/// The region of `id`.
pub fn by_id(id: &str) -> Option<&'static Region> {
    ALL.into_iter().find(|r| r.id == id)
}

/// The region whose metric is `metric`.
pub fn by_metric(metric: &str) -> Option<&'static Region> {
    ALL.into_iter().find(|r| r.metric == metric)
}

impl Region {
    /// The URL template: `{date}` at the day's start and end, earthquakes only, the circle.
    pub fn url_template(&self) -> String {
        format!(
            "{ENDPOINT}?starttime={{date}}T00:00:00&endtime={{date}}T23:59:59.999&minmagnitude={}&eventtype=earthquake&latitude={}&longitude={}&maxradiuskm={}",
            self.min_magnitude, self.latitude, self.longitude, self.radius_km
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

    /// The region's one canonical feed.
    pub fn template(&self) -> FeedTemplate {
        FeedTemplate {
            feed: self.metric.into(),
            metric: MetricId(self.metric.into()),
            fetcher: RegionCountFetcher.id(),
            url_template: self.url_template(),
            selector: SELECTOR.into(),
            kind: ObservationKind::Numeric {
                scale: SCALE,
                tolerance: 0,
            },
            window_secs: WINDOW_SECS,
            min_observers: OBSERVERS,
            rights: rights(),
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
                value_kind: ValueKind::Int,
                needs_label_set: false,
            },
        )
        .and_then(|r| r.add_aggregate(&self.subject(day)))
        .and_then(|r| r.add_source(rights()))
        .expect("registry entries are valid for a strict day");
        r
    }
}

/// The fetcher of every [`Region`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionCountFetcher;

impl Fetcher for RegionCountFetcher {
    fn id(&self) -> Hash32 {
        keccak256(REGION_FETCHER_TAG)
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        let Some(r) = by_metric(&spec.metric.0) else {
            return Answer::Unavailable;
        };
        if r.template().matches(spec).is_err() {
            return Answer::Unavailable;
        }
        parse_count(page).map_or(Answer::Unavailable, Answer::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-26T00:00:00Z: 110 days after 2026-07-08.
    const START: u64 = 1_792_972_800;

    #[test]
    fn the_recorded_day_reads_its_count_and_nothing_else_passes() {
        let body = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/region-la300-2026-07-08/response.txt"
        ))
        .expect("fixture");
        let spec = LA_300_M2_5.spec("2026-07-08", START).expect("spec");
        assert_eq!(RegionCountFetcher.extract(&spec, &body), Answer::Number(0));
        for bad in [&b""[..], b"01", b"-1", b"3 ", b"1234567890"] {
            assert_eq!(RegionCountFetcher.extract(&spec, bad), Answer::Unavailable);
        }
        // A wider circle or a lower floor is not the canonical feed.
        for (from, to) in [
            ("maxradiuskm=300", "maxradiuskm=3000"),
            ("minmagnitude=2.5", "minmagnitude=0"),
        ] {
            let mut moved = spec.clone();
            moved.url = moved.url.replace(from, to);
            assert_eq!(
                RegionCountFetcher.extract(&moved, b"7"),
                Answer::Unavailable,
                "{to}"
            );
        }
        // The world feed's fetcher does not read a regional feed, nor this one the world's.
        assert_eq!(
            crate::UsgsCountFetcher.extract(&spec, b"7"),
            Answer::Unavailable
        );
        let world = crate::spec("2026-07-08", START).expect("world spec");
        assert_eq!(
            RegionCountFetcher.extract(&world, b"7"),
            Answer::Unavailable
        );
        assert_eq!(by_id("la300.m2_5"), Some(&LA_300_M2_5));
    }

    #[test]
    fn the_template_is_valid_and_keeps_the_world_feed() {
        let t = LA_300_M2_5.template();
        t.validate().expect("valid");
        assert_ne!(t.id(), crate::template().id());
        assert!(LA_300_M2_5.spec("2026-10-20", START).is_err(), "too new");
        assert!(LA_300_M2_5.spec("2026-05-01", START).is_err(), "too old");
        // The world M5+ feed (live on Base Sepolia, ROADMAP B6) keeps its pin.
        assert_eq!(
            crate::template().id().to_string(),
            "0x89057ffd9f32c5dc84ed1f544d2a63e732d86184f5e845aa5edfe1843b09f741"
        );
    }
}
