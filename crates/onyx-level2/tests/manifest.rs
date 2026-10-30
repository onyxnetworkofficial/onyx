//! CL3 (ADR-0053): source manifests. THREATS CL3-5..CL3-8 and the template-swap and
//! rights-bypass rows, against the first reviewed manifest
//! (`sources/us.treasury.tga_total_deposits`) and the repository's own terms table.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::origin_budget::OriginLoad;
use onyx_core::onchain::MetricKind;
use onyx_level2::manifest::{load, ClearedHosts, ManifestError, ManifestSource};
use onyx_level2::Source;
use serde_json::Value as Json;
use std::path::{Path, PathBuf};

/// 2026-10-29 00:00 UTC.
const START: u64 = 1_793_232_000;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn manifest_path() -> PathBuf {
    repo().join("sources/us.treasury.tga_total_deposits/manifest.json")
}

fn raw() -> Json {
    serde_json::from_slice(&std::fs::read(manifest_path()).unwrap()).unwrap()
}

fn terms_md() -> String {
    std::fs::read_to_string(repo().join("docs/ops/data-sources.md")).unwrap()
}

fn cleared() -> ClearedHosts {
    ClearedHosts::from_data_sources(&terms_md()).unwrap()
}

type Edit = Box<dyn Fn(&mut Json)>;

fn bytes(v: &Json) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

/// Recompute and write the stated ids of an edited manifest, as a reviewer would.
fn restate(v: &mut Json) {
    let m = onyx_level2::manifest::load_with(&bytes(v), &cleared(), false).unwrap();
    v["fetcher_id"] = Json::String(m.descriptor.fetcher_id().to_string());
    v["template_id"] = Json::String(m.template.id().to_string());
}

#[test]
fn the_reviewed_manifest_loads_and_reads_its_recording() {
    let src = ManifestSource::from_file(&manifest_path(), &repo()).unwrap();
    assert_eq!(cleared().host_names(), vec!["api.fiscaldata.treasury.gov"]);
    let spec = src.spec("2026-10-27", START).unwrap();
    let body = std::fs::read(
        repo().join("sources/us.treasury.tga_total_deposits/recording/2026-10-27.json"),
    )
    .unwrap();
    assert_eq!(
        src.fetcher().extract(&spec, &body),
        onyx_core::Answer::Number(321_642)
    );
    assert_eq!(src.shown().unit, "million USD");
    assert_eq!(src.fetch_limits().max_bytes, 16_384);
    assert_eq!(src.fetch_limits().accept, "application/json");
    assert_eq!(src.latest(START).as_deref(), Some("2026-10-27"));
}

/// THREATS CL3-7: a submitter's rights answer never clears a host. A manifest loads only when
/// the terms table clears its host under the row it names, and the row itself says OK.
#[test]
fn a_manifest_without_a_cleared_terms_row_does_not_load() {
    let ok = bytes(&raw());
    load(&ok, &cleared()).unwrap();
    let md = terms_md();
    // The host's section 6 row removed, or not OK.
    let row = "| api.fiscaldata.treasury.gov | S10 | OK |";
    assert!(md.contains(row));
    for edited in [
        md.replace(row, "| api.example.org | S10 | OK |"),
        md.replace(row, "| api.fiscaldata.treasury.gov | S10 | held |"),
        // The section 1 row the host points at is no longer OK.
        md.replacen(
            "| OK | Level-2 feed (`adapters/source-fiscaldata`)",
            "| **blocked** | Level-2 feed (`adapters/source-fiscaldata`)",
            1,
        ),
    ] {
        assert_ne!(edited, md);
        let c = ClearedHosts::from_data_sources(&edited).unwrap();
        assert!(
            matches!(load(&ok, &c), Err(ManifestError::NotCleared(_))),
            "{:?}",
            c.hosts
        );
    }
    // No section 6 at all.
    let cut = md.split("## 6. Hosts cleared").next().unwrap().to_string();
    assert!(ClearedHosts::from_data_sources(&cut).is_err());
    // The manifest names another row than the one that clears the host.
    let mut v = raw();
    v["terms_row"] = Json::String("S11".into());
    assert!(matches!(
        load(&bytes(&v), &cleared()),
        Err(ManifestError::NotCleared(_))
    ));
    // Another host, even with its ids restated, is not cleared.
    let mut v = raw();
    let url = v["template"]["url_template"]
        .as_str()
        .unwrap()
        .replace("api.fiscaldata.treasury.gov", "data.example.org");
    v["template"]["url_template"] = Json::String(url);
    assert!(matches!(
        load(&bytes(&v), &cleared()),
        Err(ManifestError::NotCleared(_))
    ));
    // A blocked host is refused whatever the table says.
    let mut v = raw();
    let url = v["template"]["url_template"]
        .as_str()
        .unwrap()
        .replace("api.fiscaldata.treasury.gov", "api.stlouisfed.org");
    v["template"]["url_template"] = Json::String(url);
    let md2 = md.replace(row, "| api.stlouisfed.org | S10 | OK |");
    assert!(matches!(
        load(&bytes(&v), &ClearedHosts::from_data_sources(&md2).unwrap()),
        Err(ManifestError::Url(_))
    ));
    // Rights that forbid redistribution never load.
    let mut v = raw();
    v["template"]["rights"]["redistribution"] = Json::String("forbidden".into());
    assert!(load(&bytes(&v), &cleared()).is_err());
}

/// Template swap: any edit to what observers fetch or read changes the recomputed ids, so a
/// manifest whose stated ids were not updated is refused, and one whose ids were restated has a
/// new template id the registry does not hold (CL3-8 below).
#[test]
fn a_manifest_with_a_wrong_stated_id_does_not_load() {
    let edits: Vec<Edit> = vec![
        Box::new(|v| {
            let u = v["template"]["url_template"]
                .as_str()
                .unwrap()
                .replace("Total%20TGA%20Deposits", "Total%20TGA%20Withdrawals");
            v["template"]["url_template"] = Json::String(u);
        }),
        Box::new(|v| {
            v["descriptor"]["asserts"][2]["expect"]["str"] =
                Json::String("Total TGA Withdrawals (Table II) (-)".into())
        }),
        Box::new(|v| v["descriptor"]["unit"] = Json::String("USD".into())),
        Box::new(|v| v["template"]["kind"]["numeric"]["scale"] = Json::from(2)),
        Box::new(|v| v["template"]["window_secs"] = Json::from(600)),
        Box::new(|v| v["template"]["min_observers"] = Json::from(3)),
        Box::new(|v| v["descriptor"]["max_bytes"] = Json::from(16_385)),
    ];
    let base = load(&bytes(&raw()), &cleared()).unwrap().template.id();
    for (i, edit) in edits.iter().enumerate() {
        let mut v = raw();
        edit(&mut v);
        assert!(
            matches!(
                load(&bytes(&v), &cleared()),
                Err(ManifestError::StatedId { .. })
            ),
            "edit {i}"
        );
        restate(&mut v);
        let m = load(&bytes(&v), &cleared()).unwrap();
        assert_ne!(m.template.id(), base, "edit {i}");
    }
    // The selector must be the descriptor's pointer.
    let mut v = raw();
    v["template"]["selector"] = Json::String("/data/0/open_month_bal".into());
    assert!(matches!(
        onyx_level2::manifest::load_with(&bytes(&v), &cleared(), false),
        Err(ManifestError::Descriptor(_))
    ));
}

/// THREATS CL3-5: the unit (and the scale) are in the fetcher id, so in the template id the
/// registry pins: thousands can never be served as units under the same pin.
#[test]
fn the_unit_is_part_of_the_pin() {
    let m = load(&bytes(&raw()), &cleared()).unwrap();
    let mut d = m.descriptor.clone();
    d.unit = "thousand USD".into();
    assert_ne!(d.fetcher_id(), m.descriptor.fetcher_id());
    let mut t = m.template.clone();
    t.fetcher = d.fetcher_id();
    assert_ne!(t.id(), m.template.id());
    // The old template with the new descriptor is refused.
    assert!(d.check_template(&m.template).is_err());
}

fn registry_answer(kind: u8, template: onyx_core::Hash32, at: u64) -> Vec<u8> {
    let mut r = vec![0u8; 96];
    r[31] = kind;
    r[32..64].copy_from_slice(&template.0);
    r[88..96].copy_from_slice(&at.to_be_bytes());
    r
}

/// THREATS CL3-8: a forged manifest (on a website or an API) is refused by any client that
/// reads `OnyxRegistry.metrics` on its own node: the template id must be the registered one.
#[test]
fn a_manifest_whose_template_id_is_not_registered_is_refused() {
    let src = ManifestSource::from_file(&manifest_path(), &repo()).unwrap();
    let t = src.template();
    let kind = MetricKind::of(&t) as u8;
    src.check_registered(&registry_answer(kind, t.id(), 1_793_000_000))
        .unwrap();
    let mut forged = raw();
    forged["descriptor"]["unit"] = Json::String("thousand USD".into());
    restate(&mut forged);
    let forged = ManifestSource::new(load(&bytes(&forged), &cleared()).unwrap()).unwrap();
    for (what, ret) in [
        ("another template", registry_answer(kind, t.id(), 1)),
        (
            "not registered",
            registry_answer(kind, forged.template().id(), 0),
        ),
        (
            "another kind",
            registry_answer(kind + 1, forged.template().id(), 1),
        ),
    ] {
        assert!(forged.check_registered(&ret).is_err(), "{what}");
    }
    assert!(src
        .check_registered(&registry_answer(kind, t.id(), 0))
        .is_err());
    assert!(src.check_registered(&[0u8; 64]).is_err());
}

/// THREATS CL3-6 (library part): a round whose observers would push the source's origin over
/// its manifest budget is refused before any draw; a second source on the same host shares
/// the origin's count.
#[test]
fn the_runner_refuses_a_round_over_the_origin_budget() {
    let mut v = raw();
    v["limits"]["origin_budget"] = Json::from(8);
    restate(&mut v);
    let src = ManifestSource::new(load(&bytes(&v), &cleared()).unwrap()).unwrap();
    assert_eq!(src.origin_budget(), 8);
    let mut load_ = OriginLoad::default();
    let a = src.spec("2026-10-27", START).unwrap();
    src.admit(&mut load_, &a).unwrap();
    // Another date in an overlapping window: 5 + 5 > 8.
    let b = src.spec("2026-10-26", START + 60).unwrap();
    let err = src.admit(&mut load_, &b).unwrap_err();
    assert!(err.contains("budget is 8"), "{err}");
    // After the first window ends its fetches no longer count.
    load_.prune(a.window_end + 1);
    let c = src.spec("2026-10-26", a.window_end + 1).unwrap();
    src.admit(&mut load_, &c).unwrap();
    // The budget is capped by the source's own gap rule, and a budget that admits no round of
    // its observer count does not load.
    let mut v = raw();
    v["limits"]["min_fetch_gap_secs"] = Json::from(100); // 300 s / 100 s = 3 < 5 observers
    assert!(matches!(
        onyx_level2::manifest::load_with(&bytes(&v), &cleared(), false),
        Err(ManifestError::Field(_))
    ));
}

/// The manifest is reviewed data: strict JSON, no unknown field, and only what v1 serves.
#[test]
fn a_manifest_is_strict() {
    let ok = String::from_utf8(bytes(&raw())).unwrap();
    // A duplicate key (serde_json alone would keep the last copy).
    let dup = ok.replacen("{\"descriptor\"", "{\"metric\":\"x.y\",\"descriptor\"", 1);
    assert!(matches!(
        load(dup.as_bytes(), &cleared()),
        Err(ManifestError::Json(_))
    ));
    let edits: Vec<Edit> = vec![
        Box::new(|v| v["extra"] = Json::from(1)),
        Box::new(|v| v["template"]["fetcher"] = Json::from("0x00")),
        Box::new(|v| v["v"] = Json::from(2)),
        Box::new(|v| v["id"] = Json::from("../etc")),
        Box::new(|v| v["recording"] = Json::from("../../.env")),
        Box::new(|v| {
            v["template"]["subject"] =
                serde_json::json!({"period": {"prefix": "a.b", "min_days": 1, "max_days": 2}})
        }),
        Box::new(|v| {
            let u = v["template"]["url_template"]
                .as_str()
                .unwrap()
                .replace("?fields", "?api_key=1&fields");
            v["template"]["url_template"] = Json::String(u);
        }),
        Box::new(|v| {
            let u = v["template"]["url_template"]
                .as_str()
                .unwrap()
                .replace(".gov/", ".gov:443/");
            v["template"]["url_template"] = Json::String(u);
        }),
        Box::new(|v| {
            let u = v["template"]["url_template"]
                .as_str()
                .unwrap()
                .replace("https://api.fiscaldata.treasury.gov", "https://127.0.0.1");
            v["template"]["url_template"] = Json::String(u);
        }),
        Box::new(|v| v["descriptor"]["access"] = Json::from("keyed")),
        // Review CL3-R1-7: a scale the extraction never reads.
        Box::new(|v| v["template"]["kind"]["numeric"]["scale"] = Json::from(39)),
        Box::new(|v| v["template"]["kind"] = serde_json::json!({"exact": {"max_len": 8}})),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut v = raw();
        edit(&mut v);
        assert!(
            onyx_level2::manifest::load_with(&bytes(&v), &cleared(), false).is_err(),
            "edit {i}"
        );
    }
}
