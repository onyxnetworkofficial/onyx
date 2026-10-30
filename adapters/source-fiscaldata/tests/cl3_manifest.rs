//! CL3 (ADR-0053): the generic JSON fetcher covers what this adapter's newest fetcher checks by
//! hand, and the first manifest source (`sources/us.treasury.tga_total_deposits`) uses this
//! adapter's reviewed calendar and rights. The live templates and their fetchers do not move.
#![allow(clippy::unwrap_used)]

use onyx_core::feed_json::{Access, Assert, Expect, JsonDescriptor, ValueRule};
use onyx_core::{keccak256, Answer, FeedTemplate, Fetcher, SubjectRule};
use onyx_json::GenericJsonFetcher;
use onyx_level2::manifest::{load, template_to_json, ClearedHosts, ManifestSource};
use onyx_level2::Source;
use onyx_source_fiscaldata::series::{SeriesFetcher, SERIES_FETCHER_TAG, TGA_CLOSE};
use onyx_source_fiscaldata::{date_lag, rights, OBSERVERS, WINDOW_SECS};
use std::path::{Path, PathBuf};

/// 2026-10-29 00:00 UTC: record date 2026-10-27 has a business day (10-28) in between.
const START: u64 = 1_793_232_000;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn row_descriptor(row: &str) -> JsonDescriptor {
    let a = |pointer: &str, expect| Assert {
        pointer: pointer.into(),
        expect,
    };
    JsonDescriptor {
        version: 1,
        pointer: "/data/0/open_today_bal".into(),
        value: ValueRule::DecimalString,
        unit: "million USD".into(),
        asserts: vec![
            a("/data", Expect::ArrayLen(1)),
            a(
                "/data/0",
                Expect::KeysExactly(vec![
                    "account_type".into(),
                    "open_today_bal".into(),
                    "record_date".into(),
                ]),
            ),
            a("/data/0/account_type", Expect::Str(row.into())),
            a("/data/0/record_date", Expect::SubjectDate),
            a("/meta/count", Expect::Int(1)),
        ],
        max_bytes: 16_384,
        access: Access::Public,
    }
}

fn tga_body() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/dts-tga-2026-10-27/response.json"),
    )
    .unwrap()
}

/// ADR-0053 "tests to write first": the TGA series rebuilt as a descriptor extracts the same
/// value as `SeriesFetcher` from the same recording, and refuses what it refuses; its fetcher id
/// differs, so the live series keeps its own fetcher and pin.
#[test]
fn the_tga_series_as_a_manifest_reads_what_series_fetcher_reads() {
    let d = row_descriptor(TGA_CLOSE.row);
    let generic = GenericJsonFetcher::new(d.clone()).unwrap();
    let live = TGA_CLOSE.template();
    let mut t: FeedTemplate = live.clone();
    t.fetcher = generic.id();
    d.check_template(&t).unwrap();
    let spec = t.instantiate(&live_subject("2026-10-27"), START).unwrap();
    let live_spec = TGA_CLOSE.spec("2026-10-27", START).unwrap();
    assert_eq!(
        generic.extract(&spec, &tga_body()),
        Answer::Number(1_052_530)
    );
    assert_eq!(
        SeriesFetcher.extract(&live_spec, &tga_body()),
        Answer::Number(1_052_530)
    );
    // The same refusals as `series::tests::anything_but_the_one_row_is_unavailable`.
    let row = |date: &str, kind: &str, v: &str| {
        format!(
            r#"{{"data":[{{"record_date":"{date}","account_type":"{kind}","open_today_bal":{v}}}],"meta":{{"count":1}}}}"#
        )
    };
    let tga = TGA_CLOSE.row;
    for bad in [
        r#"{"data":[],"meta":{"count":0}}"#.to_string(),
        row("2026-10-26", tga, r#""1052530""#),
        row(
            "2026-10-27",
            "Treasury General Account (TGA) Opening Balance",
            r#""1020700""#,
        ),
        row("2026-10-27", tga, "1052530"),
        row("2026-10-27", tga, r#""01052530""#),
        row("2026-10-27", tga, r#""1052530.5""#),
        row("2026-10-27", tga, r#""null""#),
        row("2026-10-27", tga, r#""1052530""#).replace(r#""count":1"#, r#""count":2"#),
        row("2026-10-27", tga, r#""1052530""#)
            .replace(r#""open_today_bal""#, r#""x":1,"open_today_bal""#),
    ] {
        assert_eq!(
            generic.extract(&spec, bad.as_bytes()),
            Answer::Unavailable,
            "{bad}"
        );
        assert_eq!(
            SeriesFetcher.extract(&live_spec, bad.as_bytes()),
            Answer::Unavailable,
            "{bad}"
        );
    }
    // One documented difference: the DecimalString grammar takes a sign, the series fetcher
    // reads whole units only. A descriptor that must refuse negatives says so with its own
    // rule; the series fetcher stays as it is.
    let negative = row("2026-10-27", tga, r#""-1052530""#);
    assert_eq!(
        generic.extract(&spec, negative.as_bytes()),
        Answer::Number(-1_052_530)
    );
    assert_eq!(
        SeriesFetcher.extract(&live_spec, negative.as_bytes()),
        Answer::Unavailable
    );
    // The live series keeps its fetcher and its template.
    assert_ne!(generic.id(), SeriesFetcher.id());
    assert_eq!(SeriesFetcher.id(), keccak256(SERIES_FETCHER_TAG));
    assert_eq!(live.fetcher, keccak256(SERIES_FETCHER_TAG));
    assert_ne!(t.id(), live.id());
}

fn live_subject(date: &str) -> onyx_core::claim::Subject {
    onyx_core::claim::Subject::Aggregate(format!("{}:{date}", TGA_CLOSE.subject))
}

const MANIFEST: &str = "sources/us.treasury.tga_total_deposits/manifest.json";

/// The manifest as a reviewer writes it, built from this adapter's reviewed calendar and rights.
fn manifest_json() -> serde_json::Value {
    let d = row_descriptor("Total TGA Deposits (Table II)");
    let metric = "us.treasury.tga_total_deposits";
    let mut t = TGA_CLOSE.template();
    t.feed = metric.into();
    t.metric = onyx_core::claim::MetricId(metric.into());
    t.url_template = t.url_template.replace(
        "Treasury%20General%20Account%20(TGA)%20Closing%20Balance",
        "Total%20TGA%20Deposits%20(Table%20II)",
    );
    t.fetcher = d.fetcher_id();
    t.subject = SubjectRule::Dated {
        prefix: "us.treasury.tga_deposits".into(),
        lag: date_lag(),
    };
    let descriptor: serde_json::Value =
        serde_json::from_str(&onyx_json::descriptor_to_json(&d)).unwrap();
    serde_json::json!({
        "v": 1,
        "id": "us.treasury.tga_total_deposits",
        "metric": metric,
        "name": "U.S. Treasury Fiscal Data: Daily Treasury Statement, total deposits into the Treasury General Account",
        "terms_row": "S10",
        "terms_url": onyx_source_fiscaldata::TERMS_URL,
        "terms_quote": onyx_source_fiscaldata::TERMS_QUOTE,
        "note": "Daily Treasury Statement total TGA deposits: rights reviewed (sources/us.treasury.tga_total_deposits/PROVENANCE.md), generic fetcher, manifest pinned",
        "template": template_to_json(&t),
        "descriptor": descriptor,
        "limits": { "max_rounds_per_day": 4, "origin_budget": 16, "min_fetch_gap_secs": 1 },
        "earliest_public_offset_secs": 0,
        "recording": "recording/2026-10-27.json",
        "fetcher_id": d.fetcher_id().to_string(),
        "template_id": t.id().to_string(),
    })
}

/// The first manifest source: the file equals what this adapter's reviewed calendar and rights
/// give (regenerate with `ONYX_BLESS=1` only after a review), it loads under the repository's
/// terms table, and its recording reads the value the draft tool printed.
#[test]
fn the_deposits_manifest_uses_the_fiscal_data_calendar_and_rights() {
    let path = repo().join(MANIFEST);
    let want = format!(
        "{}\n",
        serde_json::to_string_pretty(&manifest_json()).unwrap()
    );
    if std::env::var("ONYX_BLESS").is_ok() {
        std::fs::write(&path, &want).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(String::from_utf8_lossy(&bytes), want);
    let cleared = ClearedHosts::from_repo(&repo()).unwrap();
    let m = load(&bytes, &cleared).unwrap();
    let t = &m.template;
    assert_eq!(t.rights, rights());
    assert_eq!(t.window_secs, WINDOW_SECS);
    assert_eq!(t.min_observers, OBSERVERS);
    assert!(matches!(&t.subject, SubjectRule::Dated { lag, .. } if *lag == date_lag()));
    let src = ManifestSource::new(m.clone()).unwrap();
    let spec = src.spec("2026-10-27", START).unwrap();
    assert!(spec.url.contains(
        "filter=record_date:eq:2026-10-27,account_type:eq:Total%20TGA%20Deposits%20(Table%20II)"
    ));
    let body = std::fs::read(path.parent().unwrap().join(&m.recording)).unwrap();
    assert_eq!(src.fetcher().extract(&spec, &body), Answer::Number(321_642));
    // The closing-balance recording is another row: refused under this manifest.
    assert_eq!(
        src.fetcher().extract(&spec, &tga_body()),
        Answer::Unavailable
    );
    // Its template is its own, never a live adapter's.
    assert_ne!(t.id(), TGA_CLOSE.template().id());
    assert_ne!(t.id(), onyx_source_fiscaldata::template().id());
}
