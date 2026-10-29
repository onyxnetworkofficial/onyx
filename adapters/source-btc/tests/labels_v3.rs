//! Label set v3 (ROADMAP P2a, ADR-0037 Amendment 1): the dated set built
//! from the issuers' own rosters under `fixtures/rosters/`, reproducible from
//! the committed snapshot files, graded signed only where a signature
//! verifies here, naming only reviewed companies and never wBTC or cbBTC
//! (IDX-6), and dropping an address a government filing names.

use onyx_core::{LabelSetRef, ProofLevel};
use onyx_labels::dated::{check_history, DatedLabelSet};
use onyx_labels::roster::{EVIDENCE_PUBLISHED, EVIDENCE_SIGNED};
use onyx_source_btc::rosters::{build_v3, v3_to_json, LABELS_V3, V3_PUBLISHED};
use onyx_source_btc::{
    dated_label_set_from_json, dated_snapshots_from_json, dated_window_proof_level,
    fixture_registry_v3, load_dated_label_set, load_label_set, verify_checksum, LABELS_V3_ENTITIES,
};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixtures() -> PathBuf {
    root().join("fixtures")
}

fn v3() -> DatedLabelSet {
    load_dated_label_set(&fixtures(), LABELS_V3).expect("v3 loads (checksum, structure, evidence)")
}

fn python(args: &[&str]) -> std::process::Output {
    std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/build-rosters.py"))
        .args(args)
        .output()
        .expect("python3 runs")
}

/// The hosts the issuers serve their own rosters from (no explorer, archive
/// or aggregator: the hard limit on third-party attribution).
const ISSUER_HOSTS: &[&str] = &[
    "https://static.okx.com/",
    "https://public.bnbstatic.com/",
    "https://s3-eu-west-1.amazonaws.com/public.bitmex.com/",
    "https://www.bybit.com/",
    "https://raw.githubusercontent.com/bitfinexcom/",
    "https://raw.githubusercontent.com/huobiapi/",
    "https://file.coinexstatic.com/",
    "https://por.mocortech.com/",
    "https://crypto.com/",
    "https://support.deribit.com/",
    "https://bitbetf.com/",
];

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_loads_and_validates_against_the_v3_registry() {
    let set = v3();
    set.validate(&fixture_registry_v3()).expect("valid");
    assert_eq!(
        (set.name.as_str(), set.version, set.published),
        ("test.onyx.entities.btc", 3, V3_PUBLISHED)
    );
    assert!(set.name.starts_with("test."), "test data, never served");
    for m in &set.members {
        let kinds: Vec<&str> = m.label.evidence.iter().map(|e| e.kind.as_str()).collect();
        assert!(
            !kinds.is_empty()
                && kinds.len() <= 2
                && kinds
                    .iter()
                    .all(|k| *k == EVIDENCE_SIGNED || *k == EVIDENCE_PUBLISHED),
            "{}: every membership cites its first and last roster snapshot",
            m.label.address
        );
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_rebuilds_byte_for_byte_from_the_committed_rosters() {
    let dir = std::env::temp_dir().join(format!("onyx-v3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp");
    let listings = dir.join("listings.json");
    let out = python(&["listings", listings.to_str().expect("utf-8")]);
    assert!(
        out.status.success(),
        "listings: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let built = build_v3(&std::fs::read_to_string(&listings).expect("listings")).expect("builds");
    let _ = std::fs::remove_dir_all(&dir);
    let text = v3_to_json(&built).expect("json");
    let committed = std::fs::read_to_string(fixtures().join(LABELS_V3)).expect("v3 file");
    assert!(
        text == committed,
        "the committed v3 differs from a rebuild: run build-label-set-v3"
    );
    assert_eq!(built.set.content_hash(), v3().content_hash());
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_names_only_reviewed_issuers_from_their_own_hosts_never_wbtc_or_cbbtc() {
    let set = v3();
    let allowed: Vec<&str> = LABELS_V3_ENTITIES.iter().map(|(e, _)| *e).collect();
    for m in &set.members {
        assert!(
            allowed.contains(&m.label.entity.as_str()),
            "{}",
            m.label.entity
        );
        assert!(!m.label.entity.contains("wbtc") && !m.label.entity.contains("cbbtc"));
    }
    let text = std::fs::read_to_string(fixtures().join(LABELS_V3)).expect("v3");
    let records = dated_snapshots_from_json(&text).expect("records");
    assert!(!records.is_empty());
    for r in &records {
        assert!(
            ISSUER_HOSTS.iter().any(|h| r.url.starts_with(h)),
            "{}: not an issuer host",
            r.url
        );
        assert!(allowed.contains(&r.entity.as_str()));
        assert!(r.height <= V3_PUBLISHED);
    }
    // Every reviewed entity has at least one membership.
    for e in &allowed {
        assert!(set.members.iter().any(|m| m.label.entity == *e), "{e}");
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_grades_signed_only_for_the_issuers_that_sign() {
    let set = v3();
    let signed: std::collections::BTreeSet<&str> = set
        .members
        .iter()
        .filter(|m| m.label.evidence.iter().any(|e| e.kind == EVIDENCE_SIGNED))
        .map(|m| m.label.entity.as_str())
        .collect();
    assert!(signed.contains("entity:okx"), "OKX signs every row");
    assert!(signed
        .iter()
        .all(|e| ["entity:okx", "entity:htx", "entity:coinex"].contains(e)));
    for m in &set.members {
        let signed = m.label.evidence.iter().any(|e| e.kind == EVIDENCE_SIGNED);
        assert_eq!(m.label.confidence.0, if signed { 990_000 } else { 900_000 });
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_drops_the_address_a_government_filing_names() {
    let deny = std::fs::read_to_string(fixtures().join("rosters/deny.txt")).expect("deny");
    let set = v3();
    for a in deny.lines() {
        let a = format!("btc:{a}");
        assert!(set.members.iter().all(|m| m.label.address != a), "{a}");
    }
    assert!(
        !deny.trim().is_empty(),
        "the OKX address on the OFAC list is denied"
    );
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_is_the_first_dated_version_after_the_undated_v1_and_v2() {
    let u = |rel: &str| DatedLabelSet::from_undated(&load_label_set(&fixtures(), rel).expect("u"));
    let (v1, v2) = (
        u("labels/test.onyx.entities.btc.v1.json"),
        u("labels/test.onyx.entities.btc.v2.json"),
    );
    assert_eq!(check_history(&[v1.clone(), v2.clone(), v3()]), Ok(()));
    assert!(
        check_history(&[v3()]).is_err(),
        "not alone: it is version 3"
    );
    assert!(check_history(&[v1, v3()]).is_err(), "no skipped version");
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v3_windows_are_level_4_anything_else_level_5() {
    let set = v3();
    let w = set.window(969_839, 969_839).expect("window");
    let r = LabelSetRef {
        name: w.name.clone(),
        version: w.version,
        content_hash: w.content_hash(),
    };
    assert_eq!(
        dated_window_proof_level(&fixtures(), &r, 969_839, 969_839),
        ProofLevel::BondedClaim
    );
    // The heights are checked: the same set on a range whose window differs is level 5.
    assert_eq!(
        dated_window_proof_level(&fixtures(), &r, 963_000, 963_000),
        ProofLevel::TrackRecord
    );
    // The empty window before any membership is level 5.
    let empty = set.window(1, 1).expect("empty window");
    let re = LabelSetRef {
        name: empty.name.clone(),
        version: empty.version,
        content_hash: empty.content_hash(),
    };
    assert_eq!(
        dated_window_proof_level(&fixtures(), &re, 1, 1),
        ProofLevel::TrackRecord
    );
    let mut other = w.clone();
    other.labels.pop();
    let r2 = LabelSetRef {
        content_hash: other.content_hash(),
        ..r
    };
    assert_eq!(
        dated_window_proof_level(&fixtures(), &r2, 969_839, 969_839),
        ProofLevel::TrackRecord
    );
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_roster_builder_self_test_passes_and_every_file_is_checksummed() {
    let out = python(&["self-test"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    verify_checksum(&fixtures(), LABELS_V3).expect("v3 in SHA256SUMS");
    verify_checksum(&fixtures(), "rosters/deny.txt").expect("deny list in SHA256SUMS");
    // Review P2a-R2-2: the manifests that pin every committed roster file, and
    // the deny list's basis, are pinned in turn (THREATS ROS-5).
    verify_checksum(&fixtures(), "rosters/deny.basis.json").expect("deny basis in SHA256SUMS");
    for key in [
        "binance",
        "bitb",
        "bitfinex",
        "bitmex",
        "bybit",
        "coinex",
        "cryptocom",
        "deribit",
        "htx",
        "mexc",
        "okx",
    ] {
        verify_checksum(&fixtures(), &format!("rosters/{key}/snapshots.json"))
            .unwrap_or_else(|e| panic!("{key}/snapshots.json: {e}"));
    }
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("read_dir") {
        let e = e.expect("entry");
        let dst = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).expect("copy");
        }
    }
}

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(b))
}

/// Review P2a-R2-1/2: the listings run the deny list's freshness check and the
/// tree check on every build. A roster file that lists a new address (its
/// manifest hash updated, as an import would) is refused until `deny` is re-run
/// against the filings; an excluded source's directory, a stray file and a
/// manifest file name that leaves its directory are refused too.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn listings_refuse_a_stale_deny_list_an_excluded_source_and_stray_files() {
    let base = std::env::temp_dir().join(format!("onyx-rosters-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let run = |name: &str, edit: &dyn Fn(&std::path::Path)| -> (bool, String) {
        let dir = base.join(name);
        copy_dir(&fixtures().join("rosters"), &dir);
        edit(&dir);
        let out = python(&[
            "listings",
            base.join(format!("{name}.json")).to_str().expect("utf-8"),
            "--rosters",
            dir.to_str().expect("utf-8"),
        ]);
        let msg = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), msg)
    };
    let (ok, msg) = run("same", &|_| {});
    assert!(ok, "the committed copy builds: {msg}");
    // A new Bybit snapshot address (a valid one no roster lists), manifest updated.
    let (ok, msg) = run("new-address", &|d| {
        let f = d.join("bybit/2026-07-22.addresses");
        let mut lines: Vec<String> = std::fs::read_to_string(&f)
            .expect("read")
            .lines()
            .map(str::to_owned)
            .collect();
        lines.push("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".into());
        lines.sort();
        let body = lines.join("\n") + "\n";
        std::fs::write(&f, &body).expect("write");
        let m = d.join("bybit/snapshots.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&m).expect("m")).expect("json");
        v["files"]["2026-07-22.addresses"] = sha256_hex(body.as_bytes()).into();
        std::fs::write(&m, serde_json::to_string(&v).expect("json")).expect("write");
    });
    assert!(
        !ok && msg.contains("run build-rosters.py deny"),
        "stale deny list: {msg}"
    );
    // deny.txt edited by hand (the denied address removed).
    let (ok, msg) = run("deny-edited", &|d| {
        std::fs::write(d.join("deny.txt"), b"").expect("write");
    });
    assert!(
        !ok && msg.contains("deny.txt differs"),
        "edited deny list: {msg}"
    );
    // An excluded source committed beside the others.
    let (ok, msg) = run("wbtc", &|d| {
        std::fs::create_dir_all(d.join("wbtc")).expect("mkdir");
        std::fs::write(d.join("wbtc/snapshots.json"), b"{}").expect("write");
    });
    assert!(!ok && msg.contains("excluded"), "wbtc directory: {msg}");
    // A stray file in a source directory, and an unknown top-level entry.
    let (ok, msg) = run("stray", &|d| {
        std::fs::write(d.join("okx/extra.csv"), b"x").expect("write");
    });
    assert!(
        !ok && msg.contains("not named by its snapshots.json"),
        "stray file: {msg}"
    );
    let (ok, msg) = run("unknown", &|d| {
        std::fs::write(d.join("notes.txt"), b"x").expect("write");
    });
    assert!(!ok && msg.contains("unknown entry"), "unknown entry: {msg}");
    // A manifest naming another issuer's file.
    let (ok, msg) = run("traversal", &|d| {
        let m = d.join("deribit/snapshots.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&m).expect("m")).expect("json");
        let bitb = std::fs::read(d.join("bitb/2026-08-24.addresses")).expect("bitb");
        v["files"] = serde_json::json!({"../bitb/2026-08-24.addresses": sha256_hex(&bitb),
            "2026-08-22.addresses": v["files"]["2026-08-22.addresses"].clone()});
        v["snapshots"][0]["file"] = "../bitb/2026-08-24.addresses".into();
        std::fs::write(&m, serde_json::to_string(&v).expect("json")).expect("write");
    });
    assert!(
        !ok && msg.contains("not a plain file"),
        "path out of its directory: {msg}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// Review P2a-R1-6/7: the loader binds each member's range to its cited
/// snapshots, so a forged file is refused without the rebuild.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_forged_v3_file_is_refused_by_the_loader() {
    let text = std::fs::read_to_string(fixtures().join(LABELS_V3)).expect("v3");
    assert!(dated_label_set_from_json(&text).is_ok());
    let lines: Vec<&str> = text.lines().collect();
    // A closed membership (one cited listing) with its end stretched past its expiry, or opened.
    let i = lines
        .iter()
        .position(|l| l.contains("\"to\":8") || l.contains("\"to\":9"))
        .expect("a closed member");
    let row: serde_json::Value =
        serde_json::from_str(lines[i].trim_end_matches(',')).expect("member row");
    let to = row["to"].as_u64().expect("closed");
    let forge = |from: &str, to_: &str| {
        let mut l = lines.clone();
        let edited = lines[i].replacen(from, to_, 1);
        l[i] = &edited;
        dated_label_set_from_json(&l.join("\n")).is_err()
    };
    assert!(
        forge(&format!("\"to\":{to}"), "\"to\":null"),
        "opened past its expiry"
    );
    assert!(
        forge(&format!("\"to\":{to}"), &format!("\"to\":{}", to + 9_000)),
        "stretched past its expiry"
    );
    // Evidence pointing at another entity's snapshot (record 0 is Binance's first).
    let ev = row["evidence"][0].as_u64().expect("evidence");
    if row["entity"] != "entity:binance" {
        assert!(
            forge(&format!("\"evidence\":[{ev}"), "\"evidence\":[0"),
            "another entity"
        );
    }
    // A member without evidence in a file with a table.
    let mut bare = row.clone();
    bare.as_object_mut().expect("object").remove("grade");
    bare["evidence"] = serde_json::json!([]);
    let mut l = lines.clone();
    let edited = format!("{bare},");
    l[i] = &edited;
    assert!(
        dated_label_set_from_json(&l.join("\n")).is_err(),
        "no evidence"
    );
    // The undated marker height.
    let bad = text.replacen(
        "\"published\":971207",
        &format!("\"published\":{}", u64::MAX),
        1,
    );
    assert!(dated_label_set_from_json(&bad).is_err());
}
