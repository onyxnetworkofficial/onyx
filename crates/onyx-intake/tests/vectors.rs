//! The shared intake vectors (fixtures/intake/vectors.json): the website's
//! classifier (apps/web/tests/s1.mjs) runs the same file, so the two cannot
//! drift (THREATS SUB-7).

use onyx_core::ProofLevel;
use onyx_intake::{
    classify, fetch_policy, level_name, Answers, FetchPolicy, NoFetch, Reason, Submission,
    CLEARED_HOSTS,
};
use serde_json::Value;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/intake/vectors.json"
    );
    let text = std::fs::read_to_string(path).expect("vectors file");
    serde_json::from_str(&text).expect("vectors json")
}

#[test]
fn every_route_vector_matches_the_classifier() {
    let v = vectors();
    let routes = v["routes"].as_array().expect("routes");
    assert!(routes.len() >= 15);
    for case in routes {
        let name = case["name"].as_str().expect("name");
        let a: Answers = serde_json::from_value(case["answers"].clone()).expect(name);
        a.validate().expect(name);
        assert_eq!(classify(&a).to_json(), case["route"], "{name}");
    }
}

#[test]
fn every_submission_vector_matches_the_checks_and_the_fetch_policy() {
    let v = vectors();
    let subs = v["submissions"].as_array().expect("submissions");
    for case in subs {
        let name = case["name"].as_str().expect("name");
        let bytes = serde_json::to_vec(&case["submission"]).expect("bytes");
        match Submission::parse(&bytes) {
            Ok(s) => {
                assert!(
                    case["error"].is_null(),
                    "{name}: accepted, want {}",
                    case["error"]
                );
                let route = s.route();
                assert_eq!(Some(route.status()), case["status"].as_str(), "{name}");
                if let Some(want) = case.get("reasons") {
                    assert_eq!(&route.to_json()["reasons"], want, "{name}");
                }
                let got = match fetch_policy(&s, &route, CLEARED_HOSTS) {
                    FetchPolicy::Pull { .. } => "pull".to_string(),
                    FetchPolicy::NoFetch(r) => r.code().to_string(),
                };
                assert_eq!(Some(got.as_str()), case["fetch"].as_str(), "{name}");
            }
            Err(e) => assert_eq!(Some(e.code()), case["error"].as_str(), "{name}: {e}"),
        }
    }
}

/// S1-R1-4: raw JSON text (duplicate keys, number spellings, trailing text)
/// gives the same verdict as the website's strict parser.
#[test]
fn every_raw_submission_vector_matches_the_parser() {
    let v = vectors();
    let raw = v["rawSubmissions"].as_array().expect("raw");
    assert!(raw.len() >= 10);
    for case in raw {
        let name = case["name"].as_str().expect("name");
        let text = case["raw"].as_str().expect("raw");
        let got = Submission::parse(text.as_bytes()).err().map(|e| e.code());
        assert_eq!(got, case["error"].as_str(), "{name}");
    }
}

/// The texts, level names, caps and hosts in the file are the crate's: the
/// website reads its copy against the same file.
#[test]
fn texts_caps_and_hosts_match_the_crate() {
    let v = vectors();
    for r in Reason::ALL {
        assert_eq!(
            v["reasons"][r.code()].as_str(),
            Some(r.text()),
            "{}",
            r.code()
        );
    }
    assert_eq!(
        v["reasons"].as_object().map(|o| o.len()),
        Some(Reason::ALL.len())
    );
    for r in NoFetch::ALL {
        assert_eq!(
            v["noFetch"][r.code()].as_str(),
            Some(r.text()),
            "{}",
            r.code()
        );
    }
    assert_eq!(
        v["noFetch"].as_object().map(|o| o.len()),
        Some(NoFetch::ALL.len())
    );
    for l in ProofLevel::ALL {
        assert_eq!(
            v["levelNames"][l.number().to_string()].as_str(),
            Some(level_name(l))
        );
    }
    let hosts: Vec<&str> = v["clearedHosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(hosts, CLEARED_HOSTS);
    use onyx_intake::submission::*;
    let caps = &v["caps"];
    for (k, n) in [
        ("title", MAX_TITLE),
        ("description", MAX_DESCRIPTION),
        ("sourceName", MAX_SOURCE_NAME),
        ("url", MAX_URL),
        ("jsonPointer", MAX_POINTER),
        ("unit", MAX_UNIT),
        ("licenseId", onyx_intake::answers::MAX_LICENSE_ID),
        ("seriesId", MAX_SERIES_ID),
    ] {
        assert_eq!(caps[k].as_u64(), u64::try_from(n).ok(), "{k}");
    }
    assert_eq!(caps["responseBytes"].as_u64(), Some(MAX_RESPONSE_BYTES));
}

/// Every outcome, path, reason and no-fetch reason appears in at least one
/// vector, so the website's copy is exercised on each.
#[test]
fn the_vectors_cover_every_outcome_path_and_reason() {
    let v = vectors();
    let routes: Vec<&Value> = v["routes"]
        .as_array()
        .expect("routes")
        .iter()
        .map(|c| &c["route"])
        .collect();
    for o in ["accept", "held", "refused"] {
        assert!(routes.iter().any(|r| r["outcome"] == o), "{o}");
    }
    for p in [
        "recipe_request",
        "auto_pull",
        "signed_source",
        "claim",
        "track_record",
    ] {
        assert!(routes.iter().any(|r| r["path"] == p), "{p}");
    }
    let sub_reasons: Vec<&Value> = v["submissions"]
        .as_array()
        .expect("subs")
        .iter()
        .filter_map(|c| c.get("reasons"))
        .collect();
    for r in Reason::ALL {
        assert!(
            sub_reasons.iter().any(|x| x
                .as_array()
                .is_some_and(|a| a.iter().any(|c| c == r.code())))
                || routes.iter().any(|x| x["reasons"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|c| c == r.code()))),
            "{}",
            r.code()
        );
    }
    let fetches: Vec<&str> = v["submissions"]
        .as_array()
        .expect("subs")
        .iter()
        .filter_map(|c| c["fetch"].as_str())
        .collect();
    assert!(fetches.contains(&"pull"));
    for r in NoFetch::ALL {
        assert!(fetches.contains(&r.code()), "{}", r.code());
    }
}

/// The example the website's "Try an example" fills is a valid submission on
/// the level-2 route that the server may pull.
#[test]
fn the_treasury_example_is_pulled_on_the_level_2_route() {
    let v = vectors();
    let s = Submission::parse(&serde_json::to_vec(&v["example"]).expect("bytes")).expect("valid");
    let r = classify(&s.answers);
    assert_eq!(r.level, Some(ProofLevel::ReObserved));
    assert!(matches!(
        fetch_policy(&s, &r, CLEARED_HOSTS),
        FetchPolicy::Pull { host, .. } if host == "api.fiscaldata.treasury.gov"
    ));
    // The hash is the Borsh bytes under a domain tag: any field change moves it.
    let mut t = s.clone();
    t.unit = Some("USD ".into());
    assert_ne!(s.hash(), t.hash());
}

/// A parsed submission written back is the JSON it came from (the journal
/// keeps that form).
#[test]
fn submissions_round_trip_to_the_same_json() {
    let v = vectors();
    for case in v["submissions"].as_array().expect("subs") {
        let bytes = serde_json::to_vec(&case["submission"]).expect("bytes");
        if let Ok(s) = Submission::parse(&bytes) {
            assert_eq!(
                serde_json::to_value(&s).expect("json"),
                case["submission"],
                "{}",
                case["name"]
            );
        }
    }
}

/// The FRED walk-through (THREATS SUB-9): held on the keyed path, the blocked
/// source named first, never pulled, and the synthetic key appears nowhere in
/// the submission as written back (the journal's form).
#[test]
fn the_fred_example_is_held_and_carries_no_key() {
    let v = vectors();
    let key = v["syntheticKey"].as_str().expect("key");
    let s =
        Submission::parse(&serde_json::to_vec(&v["fredExample"]).expect("bytes")).expect("valid");
    let r = s.route();
    assert_eq!(r.status(), "held_keyed_source_review");
    assert_eq!(r.reasons.first(), Some(&Reason::SourceBlocked));
    assert!(matches!(
        fetch_policy(&s, &r, CLEARED_HOSTS),
        FetchPolicy::NoFetch(_)
    ));
    let written = serde_json::to_string(&s).expect("json");
    assert!(!written.contains(key));
    let hosts: Vec<&str> = v["blockedHosts"]
        .as_array()
        .expect("hosts")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(hosts, onyx_intake::BLOCKED_HOSTS);
    for h in [
        "api.stlouisfed.org",
        "fred.stlouisfed.org",
        "alfred.stlouisfed.org",
        "research.stlouisfed.org",
        "stlouisfed.org",
        "x.fred.stlouisfed.org",
    ] {
        assert!(onyx_intake::is_blocked_host(h), "{h}");
    }
    for h in [
        "notstlouisfed.org",
        "stlouisfed.org.example",
        "stlouisfed.com",
    ] {
        assert!(!onyx_intake::is_blocked_host(h), "{h}");
    }
}
