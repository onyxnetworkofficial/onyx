//! Label set v2 (fixtures/labels/PROVENANCE.md): 9,850 company labels from the
//! The entity indexer. Loads under its checksum, validates against the reviewed v2
//! registry, names only allowlisted companies, and keeps every v1 label.

use onyx_core::{Confidence, EntityKind, Registry};
use onyx_source_btc::{fixture_registry, fixture_registry_v2, load_label_set, LABELS_V2_ENTITIES};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const V1: &str = "labels/test.onyx.entities.btc.v1.json";
const V2: &str = "labels/test.onyx.entities.btc.v2.json";

fn fixtures() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures"))
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_loads_and_validates_against_the_v2_registry() {
    let set = load_label_set(&fixtures(), V2).expect("v2 loads under its checksum");
    assert_eq!(set.name, "test.onyx.entities.btc");
    assert_eq!(set.version, 2);
    assert_eq!(set.labels.len(), 9_850);
    // Strictly ascending addresses (no duplicates), canonical spelling,
    // registered entity with the registered kind, confidence in range.
    assert_eq!(set.validate(&fixture_registry_v2()), Ok(()));
    // Test data: never served (R-LBL-FIX-1) until the rights review closes.
    assert!(set.is_test_only());
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_names_only_allowlisted_companies() {
    let set = load_label_set(&fixtures(), V2).expect("v2");
    let allow: BTreeMap<&str, EntityKind> = LABELS_V2_ENTITIES.iter().copied().collect();
    assert_eq!(
        allow.len(),
        LABELS_V2_ENTITIES.len(),
        "allowlist has no duplicates"
    );
    let mut per: BTreeMap<&str, usize> = BTreeMap::new();
    for l in &set.labels {
        assert_eq!(
            allow.get(l.entity.as_str()),
            Some(&l.kind),
            "{} is not an allowlisted company of that kind",
            l.entity
        );
        assert!(
            matches!(l.kind, EntityKind::Exchange | EntityKind::Etf),
            "{}",
            l.address
        );
        // No v2 label claims certainty: none carries a first-party attestation.
        assert!(l.confidence.0 < Confidence::ONE, "{}", l.address);
        *per.entry(l.entity.as_str()).or_default() += 1;
    }
    // Every allowlisted entity is used, so the allowlist is not wider than the data.
    assert_eq!(per.len(), allow.len());
    assert_eq!(per["entity:okx"], 9_443);
    assert_eq!(per["entity:bitmex"], 209);
    assert_eq!(per["entity:binance"], 81);
    assert_eq!(per["entity:bitwise-bitb"], 117);
    // LBL-V2-F1: no mining-pool labels (payout addresses are not proven pool-owned).
    assert!(set.labels.iter().all(|l| l.kind != EntityKind::MiningPool));
    // LBL-V2-F5: the re-checked grade only where a re-check happened.
    let grade = |e: &str, ppm: u32| {
        set.labels
            .iter()
            .filter(|l| l.entity == e && l.confidence.0 == ppm)
            .count()
    };
    assert_eq!(grade("entity:okx", 990_000), 9_443);
    assert_eq!(grade("entity:binance", 950_000), 55);
    assert_eq!(grade("entity:binance", 900_000), 26);
    assert_eq!(grade("entity:bitwise-bitb", 950_000), 65);
    assert_eq!(grade("entity:bitwise-bitb", 900_000), 52);
    assert_eq!(grade("entity:bitmex", 950_000), 106);
    assert_eq!(grade("entity:bitmex", 900_000), 103);
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_keeps_every_v1_label() {
    let v1 = load_label_set(&fixtures(), V1).expect("v1");
    let v2 = load_label_set(&fixtures(), V2).expect("v2");
    let have: BTreeSet<(&str, &str)> = v2
        .labels
        .iter()
        .map(|l| (l.address.as_str(), l.entity.as_str()))
        .collect();
    for l in &v1.labels {
        assert!(
            have.contains(&(l.address.as_str(), l.entity.as_str())),
            "{}",
            l.address
        );
    }
    // Same kind and confidence too: v1 is a subset with identical values.
    let by: BTreeMap<&str, (EntityKind, u32)> = v2
        .labels
        .iter()
        .map(|l| (l.address.as_str(), (l.kind, l.confidence.0)))
        .collect();
    for l in &v1.labels {
        assert_eq!(
            by[l.address.as_str()],
            (l.kind, l.confidence.0),
            "{}",
            l.address
        );
    }
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_registry_extends_v1_without_changing_it() {
    let v1 = fixture_registry();
    let v2 = fixture_registry_v2();
    // The v1 registry root is pinned by goldens, specs and settlement vectors.
    assert_ne!(v1.root(), v2.root());
    for id in ["entity:okx", "entity:bitwise-bitb"] {
        assert_eq!(v1.entity_kind(id), v2.entity_kind(id));
    }
    // v1 labels still validate against the v2 registry, and v2 labels do not
    // validate against v1 (the new companies are not in it).
    let set_v1 = load_label_set(&fixtures(), V1).expect("v1");
    assert_eq!(set_v1.validate(&v2), Ok(()));
    let set_v2 = load_label_set(&fixtures(), V2).expect("v2");
    assert!(set_v2.validate(&v1).is_err());
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_is_reproducible_from_the_converter_shape() {
    // The file is the converter's output: sorted keys, one-space indent, a
    // trailing newline. A hand edit that breaks this shape fails here as well
    // as on the checksum.
    let text = std::fs::read_to_string(fixtures().join(V2)).expect("read");
    assert!(text.starts_with("{\n \"labels\": [\n  {\n   \"address\": \"btc:"));
    assert!(text.ends_with("],\n \"name\": \"test.onyx.entities.btc\",\n \"version\": 2\n}\n"));
}

/// LBL-V2R-F1: an address a government filing names (here OKX-signed but also on
/// the OFAC SDN list, uid 58287) is not labelled, whatever its source.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn v2_drops_an_address_a_government_filing_names() {
    let set = load_label_set(&fixtures(), V2).expect("v2");
    let filed = "btc:bc1qjzk63wwatq2f638psymq3qapn00aqgksyx56l3t8eg7zuuccwasq498khf";
    assert!(set.labels.iter().all(|l| l.address != filed));
}

/// LBL-V2R-F5: the converter grades rows only from note formats it knows; any
/// other format stops the build instead of grading the row silently.
#[test]
fn v2_converter_refuses_unknown_note_formats() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/build-label-set-v2.py"
    );
    let out = std::process::Command::new("python3")
        .args(["-I", script, "--self-test"])
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "self-test failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
