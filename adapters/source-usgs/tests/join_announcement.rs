//! North-star loop NL16 (docs/join.md): an outside observer joins a round
//! from the coordinator's announcement, and checks it before it commits to
//! anything: its own copy of the template, sealed reveals, no round flag
//! given twice. THREATS NL16-1..3.
#![allow(clippy::unwrap_used)]

use onyx_level2::live::{announcement, apply_announcement, ANNOUNCEMENT_V};
use onyx_source_usgs::region::by_id;
use onyx_source_usgs::round::{Usgs, UsgsRegion};
use serde_json::json;
use std::path::{Path, PathBuf};

const POOL: &str = r#"[{"address":"0x1111111111111111111111111111111111111111","bond":1000},{"address":"0x2222222222222222222222222222222222222222","bond":1000}]"#;
const REGISTRY: &str = "0x497389e04f7d58e07a79ac8d395c3927aecbdf2b";

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("join-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn args(d: &Path, file: &Path, extra: &[&str]) -> Vec<String> {
    let mut a: Vec<String> = [
        "level2-observer",
        "--key-file",
        "observer.key",
        "--state",
        d.join("state").to_str().unwrap(),
        "--announcement",
        file.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(extra.iter().map(|s| s.to_string()));
    a
}

fn write(d: &Path, v: &serde_json::Value) -> PathBuf {
    let f = d.join("round.json");
    std::fs::write(&f, serde_json::to_string_pretty(v).unwrap()).unwrap();
    f
}

fn flag<'a>(a: &'a [String], name: &str) -> Option<&'a str> {
    a.iter().position(|x| x == name).map(|i| a[i + 1].as_str())
}

#[test]
fn an_announced_round_expands_into_the_observers_flags() {
    let d = dir("ok");
    let ann = announcement(
        &Usgs,
        84_532,
        REGISTRY,
        "https://sepolia.base.org",
        "2026-07-19",
        1_800_000_000,
        POOL,
        true,
    )
    .unwrap();
    assert_eq!(ann["v"], ANNOUNCEMENT_V);
    assert_eq!(ann["reveals"], "sealed");
    let f = write(&d, &ann);
    let (a, chain) = apply_announcement(&Usgs, &args(&d, &f, &[])).unwrap();
    assert_eq!(chain, Some(84_532));
    assert_eq!(flag(&a, "--record-date"), Some("2026-07-19"));
    assert_eq!(flag(&a, "--window-start"), Some("1800000000"));
    assert_eq!(flag(&a, "--registry"), Some(REGISTRY));
    assert_eq!(flag(&a, "--rpc"), Some("https://sepolia.base.org"));
    let pool = std::fs::read_to_string(flag(&a, "--pool").unwrap()).unwrap();
    assert_eq!(onyx_level2::parse_pool(&pool).unwrap().len(), 2);
    // The observer's own RPC wins over the coordinator's suggestion.
    let (a, _) =
        apply_announcement(&Usgs, &args(&d, &f, &["--rpc", "http://127.0.0.1:8545"])).unwrap();
    assert_eq!(a.iter().filter(|x| *x == "--rpc").count(), 1);
    assert_eq!(flag(&a, "--rpc"), Some("http://127.0.0.1:8545"));
    // No announcement: the flags pass through unchanged.
    let plain: Vec<String> = vec!["level2-observer".into(), "--pool".into(), "p.json".into()];
    assert_eq!(
        apply_announcement(&Usgs, &plain).unwrap(),
        (plain.clone(), None)
    );
}

#[test]
fn a_round_of_another_source_or_series_is_refused() {
    let d = dir("other");
    let la = UsgsRegion(by_id("la300.m2_5").unwrap());
    let ann = announcement(
        &la,
        84_532,
        REGISTRY,
        "https://sepolia.base.org",
        "2026-07-20",
        1_800_000_000,
        POOL,
        true,
    )
    .unwrap();
    let f = write(&d, &ann);
    let err = apply_announcement(&Usgs, &args(&d, &f, &[])).unwrap_err();
    assert!(
        err.contains("get the program for that source and series"),
        "{err}"
    );
    // A template id changed under the same metric: refused too.
    let mut forged = announcement(
        &Usgs,
        84_532,
        REGISTRY,
        "https://x",
        "2026-07-19",
        1,
        POOL,
        true,
    )
    .unwrap();
    forged["templateId"] = json!(format!("0x{}", "00".repeat(32)));
    let f = write(&d, &forged);
    assert!(apply_announcement(&Usgs, &args(&d, &f, &[])).is_err());
}

#[test]
fn spool_reveals_a_second_round_flag_and_another_version_are_refused() {
    let d = dir("refused");
    let spool = announcement(
        &Usgs,
        84_532,
        REGISTRY,
        "https://x",
        "2026-07-19",
        1,
        POOL,
        false,
    )
    .unwrap();
    let f = write(&d, &spool);
    let err = apply_announcement(&Usgs, &args(&d, &f, &[])).unwrap_err();
    assert!(err.contains("sealed rounds only"), "{err}");
    let ok = announcement(
        &Usgs,
        84_532,
        REGISTRY,
        "https://x",
        "2026-07-19",
        1,
        POOL,
        true,
    )
    .unwrap();
    let f = write(&d, &ok);
    let err = apply_announcement(&Usgs, &args(&d, &f, &["--registry", REGISTRY])).unwrap_err();
    assert!(err.contains("give one, not both"), "{err}");
    let mut v2 = ok.clone();
    v2["v"] = json!(2);
    let f = write(&d, &v2);
    assert!(apply_announcement(&Usgs, &args(&d, &f, &[])).is_err());
    let mut bad_pool = ok;
    bad_pool["pool"] = json!([{"address": "0x12"}]);
    let f = write(&d, &bad_pool);
    assert!(apply_announcement(&Usgs, &args(&d, &f, &[])).is_err());
}
