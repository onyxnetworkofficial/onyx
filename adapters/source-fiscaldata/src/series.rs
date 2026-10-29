//! More Fiscal Data series (north-star loop NL25, ROADMAP F5): records of other Fiscal Data
//! datasets under the same terms, rights record and federal business calendar as Debt to the
//! Penny ([`crate::template`]).
//!
//! Each [`Series`] is one metric with its own canonical feed: one record per record date, picked
//! by a fixed filter, with the same lag rule as Debt to the Penny ([`crate::date_lag`]; the Daily
//! Treasury Statement is also published on the next business day). The terms page and the API
//! terms cover every Fiscal Data dataset (fixtures/PROVENANCE.md).
//!
//! Fetcher contract (pinned by [`SERIES_FETCHER_TAG`], one fetcher for every series): the feed
//! must equal its series' template for its own record date; the body must hold exactly one record
//! (`data` of length 1, `meta.count` 1) whose `record_date` is that date and whose filter field
//! is the series' row, with exactly the keys the URL asks for; the value is a JSON string of
//! whole units (digits only, no sign, no leading zero), read exactly. Anything else is
//! [`Answer::Unavailable`], never a guess.

use crate::{date_lag, rights, OBSERVERS, WINDOW_SECS};
use onyx_core::claim::{MetricId, Subject};
use onyx_core::{
    keccak256, Answer, FeedTemplate, Fetcher, Hash32, MetricInfo, ObservationKind, ObservationSpec,
    StaticRegistry, SubjectRule, ValueKind,
};
use serde_json::Value as Json;

/// Version tag the series fetcher's id is derived from.
pub const SERIES_FETCHER_TAG: &[u8] = b"onyx-fetcher:fiscaldata-series:v1";

/// One Fiscal Data row per record date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Series {
    /// The short id `ONYX_L2_SERIES` names (`dts.tga_close`).
    pub id: &'static str,
    /// The dataset endpoint.
    pub endpoint: &'static str,
    /// The field that picks the row, and the row's exact text.
    pub row_field: &'static str,
    /// The row's exact text in [`Series::row_field`].
    pub row: &'static str,
    /// The field holding the value.
    pub field: &'static str,
    /// Feed and metric id.
    pub metric: &'static str,
    /// The subject prefix; a feed's subject is `subject:YYYY-MM-DD`.
    pub subject: &'static str,
    /// The source's name as the website and the API show it.
    pub name: &'static str,
    /// The value's unit.
    pub unit: &'static str,
    /// The reviewer's note when the template is registered.
    pub note: &'static str,
}

/// The Treasury General Account's closing balance, Daily Treasury Statement table I (Operating
/// Cash Balance), in millions of dollars. The dataset files the closing balance's amount under
/// `open_today_bal` on the row named "Treasury General Account (TGA) Closing Balance".
pub const TGA_CLOSE: Series = Series {
    id: "dts.tga_close",
    endpoint: "https://api.fiscaldata.treasury.gov/services/api/fiscal_service/v1/accounting/dts/operating_cash_balance",
    row_field: "account_type",
    row: "Treasury General Account (TGA) Closing Balance",
    field: "open_today_bal",
    metric: "us.treasury.tga_closing_balance",
    subject: "us.treasury.operating_cash",
    name: "U.S. Treasury Fiscal Data: Daily Treasury Statement, Treasury General Account closing balance",
    unit: "million USD",
    note: "Daily Treasury Statement TGA closing balance: rights reviewed (fixtures/PROVENANCE.md), feed pinned",
};

/// Every series, in the order the docs list them.
pub const ALL: [&Series; 1] = [&TGA_CLOSE];

/// The series of `id`.
pub fn by_id(id: &str) -> Option<&'static Series> {
    ALL.into_iter().find(|s| s.id == id)
}

/// The series whose metric is `metric`.
pub fn by_metric(metric: &str) -> Option<&'static Series> {
    ALL.into_iter().find(|s| s.metric == metric)
}

/// Whole units: digits only, no sign, no leading zero (except a lone `0`).
pub fn parse_whole(s: &str) -> Option<i128> {
    let ok = !s.is_empty()
        && s.bytes().all(|c| c.is_ascii_digit())
        && (s.len() == 1 || !s.starts_with('0'));
    ok.then(|| s.parse().ok()).flatten()
}

impl Series {
    /// The URL template: the record date and the row, fixed fields.
    pub fn url_template(&self) -> String {
        format!(
            "{}?fields=record_date,{},{}&filter=record_date:eq:{{date}},{}:eq:{}",
            self.endpoint,
            self.row_field,
            self.field,
            self.row_field,
            self.row.replace(' ', "%20")
        )
    }

    /// The pinned URL for one record date.
    pub fn record_url(&self, record_date: &str) -> String {
        self.url_template()
            .replacen(onyx_core::feed::DATE_PLACEHOLDER, record_date, 1)
    }

    /// The dated subject for one record date.
    pub fn subject(&self, record_date: &str) -> String {
        format!("{}:{record_date}", self.subject)
    }

    /// The series' one canonical feed.
    pub fn template(&self) -> FeedTemplate {
        FeedTemplate {
            feed: self.metric.into(),
            metric: MetricId(self.metric.into()),
            fetcher: SeriesFetcher.id(),
            url_template: self.url_template(),
            selector: format!("/data/0/{}", self.field),
            // A published record does not move: every honest observer reads the same number.
            kind: ObservationKind::Numeric {
                scale: 0,
                tolerance: 0,
            },
            window_secs: WINDOW_SECS,
            min_observers: OBSERVERS,
            rights: rights(),
            subject: SubjectRule::Dated {
                prefix: self.subject.into(),
                lag: date_lag(),
            },
        }
    }

    /// The canonical feed for one record date from `window_start`; refused when the calendar does
    /// not allow the date.
    pub fn spec(&self, record_date: &str, window_start: u64) -> Result<ObservationSpec, String> {
        self.template()
            .instantiate(&Subject::Aggregate(self.subject(record_date)), window_start)
            .map_err(|e| format!("record date {record_date}: {e}"))
    }

    /// A registry that knows this metric, the subject of `record_date`, and the source.
    pub fn registry(&self, record_date: &str) -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.add_metric(
            self.metric,
            // Whole units: a scale-0 value is an `Int` claim, as the USGS counts are.
            MetricInfo {
                value_kind: ValueKind::Int,
                needs_label_set: false,
            },
        )
        .and_then(|r| r.add_aggregate(&self.subject(record_date)))
        .and_then(|r| r.add_source(rights()))
        .expect("registry entries are valid for a strict record date");
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
        let Some(date) = t.date_of(&spec.subject).map(str::to_owned) else {
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
        let Some(row) = doc.pointer("/data/0").and_then(Json::as_object) else {
            return Answer::Unavailable;
        };
        let exact = one_record
            && row.len() == 3
            && row.get("record_date").and_then(Json::as_str) == Some(date.as_str())
            && row.get(s.row_field).and_then(Json::as_str) == Some(s.row);
        if !exact {
            return Answer::Unavailable;
        }
        match row.get(s.field) {
            Some(Json::String(v)) => parse_whole(v).map_or(Answer::Unavailable, Answer::Number),
            _ => Answer::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-29 00:00 UTC: record date 2026-10-27 has a business day (10-28) in between.
    const START: u64 = 1_793_232_000;

    fn body() -> Vec<u8> {
        std::fs::read(format!(
            "{}/fixtures/dts-tga-2026-10-27/response.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture")
    }

    #[test]
    fn the_recorded_tga_close_reads_its_value() {
        let spec = TGA_CLOSE.spec("2026-10-27", START).expect("spec");
        assert_eq!(spec.url, TGA_CLOSE.record_url("2026-10-27"));
        assert_eq!(
            SeriesFetcher.extract(&spec, &body()),
            Answer::Number(1_052_530)
        );
        assert_eq!(by_id("dts.tga_close"), Some(&TGA_CLOSE));
        // The same body under Debt to the Penny's fetcher and feed is refused.
        let debt = crate::spec("2026-10-27", START).expect("debt spec");
        assert_eq!(
            crate::FiscalDataFetcher.extract(&debt, &body()),
            Answer::Unavailable
        );
        assert_eq!(SeriesFetcher.extract(&debt, &body()), Answer::Unavailable);
    }

    #[test]
    fn anything_but_the_one_row_is_unavailable() {
        let spec = TGA_CLOSE.spec("2026-10-27", START).expect("spec");
        let row = |date: &str, kind: &str, v: &str| {
            format!(
                r#"{{"data":[{{"record_date":"{date}","account_type":"{kind}","open_today_bal":{v}}}],"meta":{{"count":1}}}}"#
            )
        };
        let tga = "Treasury General Account (TGA) Closing Balance";
        for bad in [
            r#"{"data":[],"meta":{"count":0}}"#.to_string(),
            row("2026-10-26", tga, r#""1052530""#),
            row(
                "2026-10-27",
                "Treasury General Account (TGA) Opening Balance",
                r#""1020700""#,
            ),
            row("2026-10-27", tga, "1052530"),
            row("2026-10-27", tga, r#""-1052530""#),
            row("2026-10-27", tga, r#""01052530""#),
            row("2026-10-27", tga, r#""1052530.5""#),
            row("2026-10-27", tga, r#""null""#),
            row("2026-10-27", tga, r#""1052530""#).replace(r#""count":1"#, r#""count":2"#),
            row("2026-10-27", tga, r#""1052530""#)
                .replace(r#""open_today_bal""#, r#""x":1,"open_today_bal""#),
        ] {
            assert_eq!(
                SeriesFetcher.extract(&spec, bad.as_bytes()),
                Answer::Unavailable,
                "{bad}"
            );
        }
        assert_eq!(
            SeriesFetcher.extract(&spec, row("2026-10-27", tga, r#""0""#).as_bytes()),
            Answer::Number(0)
        );
        // A feed that is not its series' canonical one.
        let mut moved = spec.clone();
        moved.url = moved.url.replace("open_today_bal", "close_today_bal");
        assert_eq!(SeriesFetcher.extract(&moved, &body()), Answer::Unavailable);
    }

    /// A scale-0 round backs a `Value::Int` claim, so the registry must say `Int`: `Fixed { scale:
    /// 0 }` made the first live NL25 round's claim fail `accept_claim`.
    #[test]
    fn the_registry_kind_is_the_claim_kind() {
        use onyx_core::Registry;
        for s in ALL {
            let info = s.registry("2026-10-27").metric(s.metric).expect("metric");
            assert_eq!(info.value_kind, ValueKind::Int, "{}", s.id);
            assert!(matches!(
                s.template().kind,
                ObservationKind::Numeric { scale: 0, .. }
            ));
        }
    }

    #[test]
    fn templates_are_distinct_valid_and_keep_the_debt_feed() {
        let ids: Vec<Hash32> = ALL.iter().map(|s| s.template().id()).collect();
        assert!(
            !ids.contains(&crate::template().id()),
            "a series is its own template"
        );
        for s in ALL {
            s.template().validate().expect("valid");
            assert!(
                s.spec("2026-10-28", START).is_err(),
                "{}: published too late for this window",
                s.id
            );
            assert!(
                s.spec("2026-10-25", START).is_err(),
                "{}: a Sunday has no record",
                s.id
            );
            assert!(
                s.spec("2026-10-01", START).is_err(),
                "{}: older than the lag allows",
                s.id
            );
        }
        // The Debt to the Penny feed (live on Base Sepolia, market 1) keeps its fetcher and pin.
        assert_eq!(crate::template().fetcher, keccak256(crate::FETCHER_TAG));
        assert_eq!(
            crate::template().id().to_string(),
            "0xd5e2995fbcf2d9e5e29166cf11979b8c7d90f932072126e757c2875923486eed"
        );
    }
}
