//! The indexer bridge (ADR-0031, fixtures/labels/PROVENANCE.md): the v3
//! converter refuses to clear a source whose redistribution terms are not
//! cleared (wBTC, cbBTC: IDX-R1-F1/F2), and the only v3 fixture is the
//! dated roster set from recorded issuer sources (IDX-6, ROADMAP P2a).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn bridge_converter_refuses_uncleared_sources() {
    let script = root().join("scripts/build-label-set-v3.py");
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg("--self-test")
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "self-test failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// IDX-6 after ROADMAP P2a: the only v3 fixture is the dated roster set
/// (ADR-0037 Amendment 1). It is built from the issuers' own snapshots by
/// `scripts/build-rosters.py`, whose source list holds no excluded source
/// (wBTC, cbBTC) and whose every source has its own PROVENANCE section and
/// DATA-LICENSE row; the bridge converter's v3 (undated, from indexer label
/// files) is never written as a fixture. Stronger than the check it
/// replaces (`no_v3_fixture_ships_while_no_source_beyond_v2_is_cleared`): a
/// v3 file now ships only in that form, from those sources.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_v3_fixture_is_only_the_dated_roster_set_from_recorded_sources() {
    let fixtures = root().join("fixtures");
    let text = std::fs::read_to_string(fixtures.join("labels/test.onyx.entities.btc.v3.json"))
        .expect("v3");
    let v: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert!(
        v.get("labels").is_none(),
        "v3 is dated (members), never the undated bridge output"
    );
    assert!(v["members"].as_array().is_some_and(|m| !m.is_empty()));
    assert!(!text.contains("wbtc") && !text.contains("cbbtc") && !text.contains("coinbase"));
    let script = std::fs::read_to_string(root().join("scripts/build-rosters.py")).expect("script");
    let provenance =
        std::fs::read_to_string(fixtures.join("labels/PROVENANCE.md")).expect("provenance");
    let license = std::fs::read_to_string(root().join("DATA-LICENSE.md")).expect("license");
    for r in v["snapshots"].as_array().expect("snapshots") {
        let source = r["source"].as_str().expect("source");
        assert!(
            script.contains(&format!("(\"{source}\"")),
            "{source} not a roster source"
        );
        assert!(
            provenance.contains(&format!("`{source}`")),
            "{source}: no PROVENANCE section"
        );
        assert!(
            license.contains(&format!("`{source}`")),
            "{source}: no DATA-LICENSE row"
        );
    }
    // The excluded sources stay refused by the roster builder too.
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/build-rosters.py"))
        .args(["import", "wbtc", "/nonexistent"])
        .output()
        .expect("python3 runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("excluded"));
}
