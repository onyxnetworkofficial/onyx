//! The browser's check of the daily sources' recorded rounds (NS3): the
//! committed summaries check out with the same rules as the Treasury round,
//! and a forged value, page, terms link, unit or source does not.

use onyx_web_verify::check_daily_round;
use serde_json::Value;

const USGS: &str = include_str!("../../static/level2/usgs-quakes.json");
const NCEI: &str = include_str!("../../static/level2/ncei-tmax.json");

fn check(source: &str, r: &Value) -> Value {
    let out = check_daily_round(source, &r.to_string()).unwrap_or_else(|_| panic!("check errored"));
    serde_json::from_str(&out).expect("json")
}

fn problems(v: &Value) -> String {
    v["problems"].to_string()
}

#[test]
fn the_committed_rounds_check_out() {
    for (source, text, value, unit_word) in [
        ("usgs", USGS, "9", "earthquakes"),
        ("ncei", NCEI, "30.6", "degrees Celsius"),
    ] {
        let r: Value = serde_json::from_str(text).expect("summary");
        let v = check(source, &r);
        assert_eq!(v["ok"], true, "{source}: {}", problems(&v));
        assert_eq!(v["value"], value);
        assert_eq!(v["agreed"], 5);
        assert_eq!(r["unit"], unit_word);
    }
}

#[test]
fn a_forged_summary_fails() {
    let base: Value = serde_json::from_str(USGS).expect("summary");
    let forge = |f: &dyn Fn(&mut Value)| {
        let mut r = base.clone();
        f(&mut r);
        check("usgs", &r)
    };
    for (what, v) in [
        ("value", forge(&|r| r["value"] = "10".into())),
        ("page", forge(&|r| r["response_body"] = "10".into())),
        (
            "terms link",
            forge(&|r| r["source"]["terms_url"] = "https://example.org".into()),
        ),
        ("unit", forge(&|r| r["unit"] = "events".into())),
        (
            "day",
            forge(&|r| r["feed"]["record_date"] = "2026-07-09".into()),
        ),
    ] {
        assert_eq!(v["ok"], false, "{what} was accepted");
    }
    // The other source's summary is not this source's round.
    let ncei: Value = serde_json::from_str(NCEI).expect("summary");
    assert_eq!(check("usgs", &ncei)["ok"], false);
}
