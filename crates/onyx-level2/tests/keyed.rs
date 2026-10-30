//! CL4 (ADR-0054): keyed contributions with agreement. Several holders of a provider key, each
//! fetching with its own (FAKE) key from a loopback fake provider through `onyx_fetch`, count
//! like an ordinary level-2 round; one holder alone stays level 4; a holder whose answer
//! differs is out-voted; no byte of any key reaches anything the round produces. The rules of
//! seats and keyed pools (THREATS CL4-1..CL4-9) are tested here too. No live call.
#![allow(clippy::unwrap_used)]

use onyx_coordinator::observe::{
    draw_observers, DrawSeed, ObservationOutcome, ObserverStatus, SimulatedRegistry,
};
use onyx_core::{Answer, Hash32, Observation, ProofLevel, Value};
use onyx_fetch::fixture::{FixtureServer, Reply};
use onyx_fetch::ApiKey;
use onyx_level2::keyed::{
    answer_holds_key, check_observer_pool, collusion_odds_ppm, keyed_commit_time, keyed_page,
    keyed_snapshot, one_holder_statement, parse_seats, pool_to_json, private_fetch_time,
    private_observation, random_fetch_time, seats_to_json, sign_seat, KeyedError, SeatEntry,
    KEYED_BOND_AGE_SECS, KEYED_EVIDENCE, KEYED_MIN_POOL, KEYED_SEAT_TTL_SECS, ONE_HOLDER,
};
use onyx_level2::manifest::{load, load_with, ClearedHosts, ManifestError, ManifestSource};
use onyx_level2::{config, observer_address, run, store, summary, Page, Source};
use std::path::{Path, PathBuf};

/// 2026-10-29 00:00 UTC: record date 2026-10-27 is observable (daily, 1..=30 days back).
const START: u64 = 1_793_232_000;
const DAY: &str = "2026-10-27";
const DIR: &str = "fixtures/keyed-demo/sources/demo.keyed.widget_index";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn demo_repo() -> PathBuf {
    repo().join("fixtures/keyed-demo")
}

fn manifest_bytes() -> Vec<u8> {
    std::fs::read(repo().join(DIR).join("manifest.json")).unwrap()
}

fn source() -> ManifestSource {
    ManifestSource::from_file(&repo().join(DIR).join("manifest.json"), &demo_repo()).unwrap()
}

/// FAKE keys, one per holder (never real; each is planted so the scans below can find it).
fn fake_keys() -> Vec<String> {
    (1..=9)
        .map(|i| format!("FAKE-cl4-holder{i:02}-k3y+/{i}"))
        .collect()
}

fn body(v: &str) -> String {
    format!(r#"{{"data":[{{"date":"{DAY}","index":"{v}"}}],"meta":{{"count":1}}}}"#)
}

/// Percent-encoding as the fetch writes a query key.
fn pct(k: &str) -> String {
    k.bytes()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
                char::from(c).to_string()
            } else {
                format!("%{c:02X}")
            }
        })
        .collect()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(T[((n >> (18 - 6 * i)) & 63) as usize]));
            }
        }
    }
    out
}

/// Every form a key could leak in: as typed, percent-encoded, hex, base64, and a 10-byte piece.
fn forms(k: &str) -> Vec<String> {
    vec![
        k.to_string(),
        pct(k),
        hex::encode(k.as_bytes()),
        base64(k.as_bytes()),
        k[..10].to_string(),
    ]
}

/// The fake provider: each key's own route (the query key is in the path and query) answers
/// `answers[i]`; any other request is a 404.
fn provider(spec_url: &str, keys: &[String], answers: &[String]) -> FixtureServer {
    let path = spec_url.strip_prefix("https://keyed.example.org").unwrap();
    FixtureServer::start(
        keys.iter()
            .zip(answers)
            .map(|(k, a)| {
                (
                    path.replace("{api_key}", &pct(k)),
                    Reply::json(body(a).into_bytes()),
                )
            })
            .collect(),
    )
    .unwrap()
}

/// Each drawn observer's page, fetched with its holder's key. `n` pages (one per drawn).
fn pages(src: &ManifestSource, server: &FixtureServer, url: &str, keys: &[String]) -> Vec<Page> {
    let f = server.fetch(src.fetch_limits());
    let place = src.key_placement().unwrap();
    keys.iter()
        .enumerate()
        .map(|(i, k)| Page {
            fetched_unix: START + 10 + 7 * i as u64,
            body: f
                .get_keyed_blocking(url, &place, &ApiKey::new(k).unwrap())
                .ok(),
        })
        .collect()
}

#[test]
fn the_keyed_fixture_manifest_loads_and_is_keyed() {
    let src = source();
    assert!(src.key_placement().is_some());
    let spec = src.spec(DAY, START).unwrap();
    assert!(spec.url.contains("api_key={api_key}"), "{}", spec.url);
    assert_eq!(spec.min_observers, 7);
    let (id, terms) = src.keyed_seat_rules().unwrap();
    assert_eq!(id, "demo.keyed.widget_index");
    assert_eq!(terms, src.terms_hash());
}

/// THREATS CL4-6: a keyed manifest loads only under an `OK keyed` terms row; a plain `OK`
/// row, the project's own table (which clears no keyed host) or a missing row refuse it.
#[test]
fn a_keyed_manifest_without_cleared_terms_does_not_load() {
    let demo = ClearedHosts::from_repo(&demo_repo()).unwrap();
    load(&manifest_bytes(), &demo).unwrap();
    // The project's real terms table clears no keyed host.
    let real = ClearedHosts::from_repo(&repo()).unwrap();
    assert!(real.keyed_hosts.is_empty());
    assert!(matches!(
        load(&manifest_bytes(), &real),
        Err(ManifestError::NotCleared(_))
    ));
    // A host whose row is a plain OK (public manifests only).
    let text = String::from_utf8(manifest_bytes()).unwrap();
    let plain = text
        .replace("keyed.example.org/v1", "plain.example.org/v1")
        .replace("\"terms_row\": \"S90\"", "\"terms_row\": \"S91\"");
    assert!(matches!(
        load_with(plain.as_bytes(), &demo, false),
        Err(ManifestError::NotCleared(_))
    ));
    // A keyed template with fewer than 3 observers, a second secret parameter, or no
    // placeholder for its query key.
    for bad in [
        text.replace("\"min_observers\": 7", "\"min_observers\": 2"),
        text.replace("&api_key={api_key}", "&api_key={api_key}&token=abc"),
        text.replace("&api_key={api_key}", ""),
        text.replace("\"place\": \"query\"", "\"place\": \"header\""),
    ] {
        assert!(load_with(bad.as_bytes(), &demo, false).is_err(), "{bad}");
    }
}

/// Several holders, each with its own FAKE key, agree: the round settles the value at level 2
/// with the keyed evidence; holders whose key the provider answered differently are out-voted
/// (`Outlier`), never moving the value. THREATS CL4-1: no form of any key is in the run, the
/// results store, the site summary or the book.
#[test]
fn a_keyed_round_writes_no_byte_of_the_key_anywhere() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let keys = fake_keys();
    // Holders 3 and 6 get other answers (the provider targets their keys).
    let answers: Vec<String> = (1..=9)
        .map(|i| match i {
            3 => "999.9".to_string(),
            6 => "1000.0".to_string(),
            _ => "1234.5".to_string(),
        })
        .collect();
    let server = provider(&spec.url, &keys, &answers);
    // 7 drawn: the first 7 holders' keys (holders 3 and 6 among them).
    let p = pages(&src, &server, &spec.url, &keys[..7]);
    assert!(p.iter().all(|p| p.body.is_some()));
    // Every request went to the template's host with exactly its own key, once.
    let seen = server.seen();
    assert_eq!(seen.requests.len(), 7);
    for (r, k) in seen.requests.iter().zip(&keys) {
        assert!(r
            .to_ascii_lowercase()
            .contains("host: keyed.example.org\r\n"));
        assert_eq!(r.matches(&pct(k)).count(), 1);
    }
    let mut chain = SimulatedRegistry::new();
    let mut st = store(&src);
    let r = run(&src, DAY, START, &p, &mut st, &mut chain).unwrap();
    assert_eq!(
        *r.settlement.outcome(),
        ObservationOutcome::Value(Answer::Number(12_345))
    );
    let statuses = r.settlement.statuses();
    assert_eq!(
        statuses
            .iter()
            .filter(|(_, s)| *s == ObserverStatus::Agreed)
            .count(),
        5
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|(_, s)| *s == ObserverStatus::Outlier)
            .count(),
        2,
        "{statuses:?}"
    );
    let claim = r.value().unwrap();
    assert_eq!(
        claim.value,
        Value::Fixed {
            mantissa: 12_345,
            scale: 1
        }
    );
    assert_eq!(
        st.accept_claim(claim, &src.registry(DAY)).unwrap(),
        ProofLevel::ReObserved
    );
    // ADR-0054 items 2 and 10: reveals carry the window start and a blinded page hash.
    for (_, o, _, _) in &r.reveals {
        assert_eq!(o.observed_at, spec.window_start);
        assert_ne!(o.page_hash, Hash32::ZERO);
        for a in &answers {
            assert_ne!(o.page_hash, onyx_core::keccak256(body(a).as_bytes()));
        }
    }
    let sum = summary(&src, &r, DAY, &vec!["local fixture".into(); 7], b"", false);
    assert!(
        sum.get("response_body").is_none(),
        "a keyed page is never published"
    );
    assert_eq!(sum["keyed"]["evidence"], KEYED_EVIDENCE);
    // Everything the round produced, as text.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cl4-keyed-round");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let book = dir.join("book.json");
    st.save_book(&book).unwrap();
    let outputs = [
        format!("{r:?}"),
        sum.to_string(),
        onyx_level2::results_store(&src, &r).unwrap().to_string(),
        format!("{:?}", onyx_level2::transcript(&r).unwrap()),
        std::fs::read_to_string(&book).unwrap(),
        format!("{:?}", onyx_level2::attempt(&r)),
    ];
    for k in &keys {
        for f in forms(k) {
            for (i, out) in outputs.iter().enumerate() {
                assert!(!out.contains(&f), "output {i} holds a form of a key: {f}");
            }
        }
    }
}

/// THREATS CL4-5: an account-scoped value (each key's own usage) never agrees, so it never
/// becomes a fact: the round settles no value.
#[test]
fn account_scoped_answers_do_not_agree() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let keys = fake_keys();
    let usage: Vec<String> = (1..=9).map(|i| format!("{}.0", 100 + i * 17)).collect();
    let server = provider(&spec.url, &keys, &usage);
    let p = pages(&src, &server, &spec.url, &keys[..7]);
    let r = run(
        &src,
        DAY,
        START,
        &p,
        &mut store(&src),
        &mut SimulatedRegistry::new(),
    )
    .unwrap();
    assert_eq!(*r.settlement.outcome(), ObservationOutcome::NoResult);
    assert!(r.claim.is_none());
}

/// THREATS CL4-4: a keyed round slashes an outlier no more than a miss.
#[test]
fn keyed_templates_slash_outliers_no_more_than_a_miss() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let k = src.observe_config(&spec);
    assert_eq!(k.outlier_slash, k.miss_slash);
    k.validate(&spec).unwrap();
    let public = config(&spec);
    assert!(public.outlier_slash > public.miss_slash);
    assert_eq!(
        (k.commit_close, k.reveal_close),
        (public.commit_close, public.reveal_close)
    );
}

fn secret(i: u8) -> [u8; 32] {
    let mut s = onyx_core::keccak256(format!("onyx cl4 FAKE seat key {i}").as_bytes()).0;
    s[0] = 1;
    s
}

fn entry(i: u8, operator: Option<&str>, terms: Hash32) -> SeatEntry {
    SeatEntry {
        seat: sign_seat(
            &secret(i),
            "demo.keyed.widget_index",
            operator,
            terms,
            START - 30 * 86_400,
        )
        .unwrap(),
        bond: 1_000,
        bonded_at: START - 30 * 86_400,
        agreed_rounds: 9,
    }
}

fn sorted(mut v: Vec<SeatEntry>) -> Vec<SeatEntry> {
    v.sort_by_key(|e| e.seat.observer);
    v
}

/// THREATS CL4-3: a seat is drawable only with a valid signature for this source and these
/// terms, a bond covering the outlier slash held 7 days before the window, and 5 agreed public
/// rounds.
#[test]
fn a_fresh_seat_is_not_drawable() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let cfg = src.observe_config(&spec);
    let terms = src.terms_hash();
    let mut seats: Vec<SeatEntry> = (1..=9).map(|i| entry(i, None, terms)).collect();
    let mut fresh = entry(10, None, terms);
    fresh.bonded_at = START - KEYED_BOND_AGE_SECS + 1;
    let mut new = entry(11, None, terms);
    new.agreed_rounds = 4;
    let mut poor = entry(12, None, terms);
    poor.bond = cfg.outlier_slash - 1;
    let other_terms = entry(13, None, onyx_core::keccak256(b"other terms"));
    let mut forged = entry(14, None, terms);
    forged.seat.joined_at += 1; // the signature no longer covers it
    let mut late = entry(15, None, terms);
    late.seat = sign_seat(
        &secret(15),
        "demo.keyed.widget_index",
        None,
        terms,
        START + 1,
    )
    .unwrap();
    // Review CL4-R1-7: a seat older than 90 days has expired.
    let mut old = entry(16, None, terms);
    old.seat = sign_seat(
        &secret(16),
        "demo.keyed.widget_index",
        None,
        terms,
        START - KEYED_SEAT_TTL_SECS - 1,
    )
    .unwrap();
    let left = [&fresh, &new, &poor, &other_terms, &forged, &late, &old]
        .iter()
        .map(|e| e.seat.observer)
        .collect::<Vec<_>>();
    seats.extend([fresh, new, poor, other_terms, forged, late, old]);
    let seats = sorted(seats);
    let s = keyed_snapshot(&seats, "demo.keyed.widget_index", &terms, &spec, &cfg).unwrap();
    assert_eq!(s.pool.len(), 9);
    for a in &left {
        assert!(!s.pool.iter().any(|o| o.address == *a));
        assert!(s.left_out.iter().any(|(x, _)| x == a));
    }
    // The seats file round-trips; an unsorted one is refused.
    let text = seats_to_json(&seats).to_string();
    assert_eq!(parse_seats(&text).unwrap(), seats);
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["seats"].as_array_mut().unwrap().reverse();
    assert!(parse_seats(&v.to_string()).is_err());
}

/// THREATS CL4-3: two seats of one declared operator are never in one round's snapshot, so
/// never drawn together; the operator is signed, so dropping it breaks the seat.
#[test]
fn two_seats_of_one_operator_are_never_drawn_together() {
    let src = source();
    let terms = src.terms_hash();
    let mut seats: Vec<SeatEntry> = (1..=9).map(|i| entry(i, None, terms)).collect();
    seats.extend((20..=23).map(|i| entry(i, Some("acme.data"), terms)));
    let seats = sorted(seats);
    let acme: Vec<[u8; 20]> = seats
        .iter()
        .filter(|e| e.seat.operator.is_some())
        .map(|e| e.seat.observer)
        .collect();
    let mut kept = vec![];
    for day in 0..12u64 {
        let spec = src.spec(DAY, START + day * 3_600).unwrap();
        let cfg = src.observe_config(&spec);
        let s = keyed_snapshot(&seats, "demo.keyed.widget_index", &terms, &spec, &cfg).unwrap();
        let in_pool: Vec<_> = s
            .pool
            .iter()
            .filter(|o| acme.contains(&o.address))
            .collect();
        assert_eq!(in_pool.len(), 1, "one acme seat per round");
        kept.push(in_pool[0].address);
        for attempt in 0..8u32 {
            let seed = DrawSeed::for_pool(&s.pool, 100, Hash32([attempt as u8; 32]), attempt);
            let drawn = draw_observers(&spec.id(), &s.pool, &seed, 7, cfg.outlier_slash);
            assert!(drawn.iter().filter(|o| acme.contains(&o.address)).count() <= 1);
        }
    }
    kept.sort();
    kept.dedup();
    assert!(
        kept.len() > 1,
        "which acme seat is kept moves with the round"
    );
    // Dropping the declared operator breaks the signature: the seat is left out.
    let mut stripped = seats.clone();
    for e in &mut stripped {
        e.seat.operator = None;
    }
    let spec = src.spec(DAY, START).unwrap();
    let s = keyed_snapshot(
        &stripped,
        "demo.keyed.widget_index",
        &terms,
        &spec,
        &src.observe_config(&spec),
    )
    .unwrap();
    assert!(!s.pool.iter().any(|o| acme.contains(&o.address)));
}

/// ADR-0054 item 4: one key holder alone is no round; its value is level 4, stated by one key
/// holder, and the statement says so.
#[test]
fn one_key_holder_stays_level_four() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let terms = src.terms_hash();
    let one = vec![entry(1, None, terms)];
    let e = keyed_snapshot(
        &one,
        "demo.keyed.widget_index",
        &terms,
        &spec,
        &src.observe_config(&spec),
    )
    .unwrap_err();
    assert_eq!(
        e,
        KeyedError::TooFewSeats {
            have: 1,
            need: KEYED_MIN_POOL
        }
    );
    let s = one_holder_statement(&src, 1, &e.to_string());
    assert_eq!(s["proof_level"], 4);
    assert_eq!(s["standing"], ONE_HOLDER);
    assert_eq!(s["level2_claim"], false);
    assert!(e.to_string().contains("stated by one key holder"));
}

/// THREATS CL4-7: a keyed observer's fetch time is its own random draw, not a function of
/// public data; the reveal shows only the window start, and its page hash is blinded afresh.
#[test]
fn keyed_fetch_slots_are_not_derived_from_public_data() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let margin = onyx_core::fetch::FETCH_MARGIN_SECS;
    let times: Vec<u64> = (0..16u64)
        .map(|r| private_fetch_time(&spec, r.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
        .collect();
    assert!(times
        .iter()
        .all(|t| *t >= spec.window_start && *t < spec.window_end - margin));
    let mut d = times.clone();
    d.sort();
    d.dedup();
    assert!(d.len() > 8);
    // Two draws for the same round and observer differ (the public slot never would).
    let draws: Vec<u64> = (0..8).map(|_| random_fetch_time(&spec).unwrap()).collect();
    assert!(draws.iter().any(|t| *t != draws[0]));
    let me = observer_address(&src, 1);
    let public = onyx_core::fetch::fetch_time(&spec, &Hash32([5; 32]), &me);
    assert_eq!(
        public,
        onyx_core::fetch::fetch_time(&spec, &Hash32([5; 32]), &me)
    );
    // The reveal's time and page hash.
    let page = body("1234.5");
    let o = Observation {
        answer: Answer::Number(12_345),
        observed_at: spec.window_start + 61,
        page_hash: onyx_core::keccak256(page.as_bytes()),
    };
    let a = private_observation(&spec, o.clone()).unwrap();
    let b = private_observation(&spec, o.clone()).unwrap();
    assert_eq!(a.observed_at, spec.window_start);
    assert_eq!(a.answer, o.answer);
    assert_ne!(a.page_hash, o.page_hash);
    assert_ne!(a.page_hash, b.page_hash);
}

/// THREATS CL4-9: a seat, the seats file and the pool hold nothing derived from a key (no key,
/// no hash or fingerprint of one): exactly the ADR's fields.
#[test]
fn seats_and_pools_hold_no_key_and_no_key_fingerprint() {
    let src = source();
    let terms = src.terms_hash();
    let seats = sorted((1..=9).map(|i| entry(i, Some("op"), terms)).collect());
    let seat = seats[0].seat.to_json();
    let mut fields: Vec<&str> = seat
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "joined_at",
            "observer",
            "operator",
            "sig",
            "source",
            "terms_hash"
        ]
    );
    let spec = src.spec(DAY, START).unwrap();
    let s = keyed_snapshot(
        &sorted((1..=9).map(|i| entry(i, None, terms)).collect()),
        "demo.keyed.widget_index",
        &terms,
        &spec,
        &src.observe_config(&spec),
    )
    .unwrap();
    let pool = onyx_level2::keyed::pool_to_json(&s.pool).unwrap();
    let v: serde_json::Value = serde_json::from_str(&pool).unwrap();
    for m in v.as_array().unwrap() {
        let mut k: Vec<&str> = m.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort();
        assert_eq!(k, ["address", "bond"]);
    }
    // Nothing a holder signs or sends is computed from its key: a seat is a function of the
    // observer key, source, operator, terms and time only (sign_seat takes no API key), and
    // two holders' seats for one observer key are the same bytes whatever their API keys.
    let a = sign_seat(&secret(1), "demo.keyed.widget_index", None, terms, 7).unwrap();
    let b = sign_seat(&secret(1), "demo.keyed.widget_index", None, terms, 7).unwrap();
    assert_eq!(a, b);
    let text = format!("{}{}", seats_to_json(&seats), pool);
    for k in fake_keys() {
        for f in forms(&k) {
            assert!(!text.contains(&f));
        }
    }
}

/// ADR-0054 item 6: the odds a third of the pool colluding wins, as shown next to a value
/// (the rehearsal pool's numbers: 9 seats, 5 drawn, 3 colluders: 11.9 % and 31.6 %).
#[test]
fn keyed_collusion_odds_are_shown() {
    let (one, best3) = collusion_odds_ppm(9, 5, 3);
    assert_eq!((one / 1_000, best3 / 1_000), (119, 316));
    let (one4, _) = collusion_odds_ppm(9, 5, 4);
    assert_eq!(one4 / 1_000, 357);
    let shown = onyx_level2::keyed::keyed_shown(9, 7);
    assert_eq!(shown["colluders"], 3);
    assert_eq!(shown["evidence"], KEYED_EVIDENCE);
}

/// Review CL4-R1-1: a value that holds a piece of the key (a provider or a pointer that echoes
/// it) is never committed or revealed: the keyed observer's page becomes "no answer".
#[test]
fn a_keyed_reveal_never_holds_the_key() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let key = ApiKey::new("FAKE-1234567890123").unwrap();
    let echo = body("1234567890.5").into_bytes();
    assert!(answer_holds_key(&src.fetcher().extract(&spec, &echo), &key));
    assert!(keyed_page(&src, &spec, Some(echo), &key).is_none());
    let fine = body("1234.5").into_bytes();
    assert_eq!(
        keyed_page(&src, &spec, Some(fine.clone()), &key),
        Some(fine)
    );
    assert!(!answer_holds_key(&Answer::Unavailable, &key));
}

/// Review CL4-R1-2: a keyed commit is posted at the window's end whatever the private fetch
/// second, so its time on chain says nothing about when the key fetched.
#[test]
fn keyed_commits_never_tell_the_fetch_second() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let cfg = src.observe_config(&spec);
    let t = keyed_commit_time(&spec);
    assert_eq!(t, spec.window_end);
    assert!(t <= cfg.commit_close);
    for r in 0..16u64 {
        assert!(private_fetch_time(&spec, r * 7919) <= t);
    }
}

/// Review CL4-R1-5: a keyed observer needs the seats and refuses any pool but the round's
/// keyed snapshot; a keyed round's announcement carries the seats to it.
#[test]
fn a_keyed_observer_refuses_a_pool_that_is_not_the_snapshot() {
    let src = source();
    let spec = src.spec(DAY, START).unwrap();
    let terms = src.terms_hash();
    let seats = sorted((1..=9).map(|i| entry(i, None, terms)).collect());
    let text = seats_to_json(&seats).to_string();
    let snap = keyed_snapshot(
        &seats,
        "demo.keyed.widget_index",
        &terms,
        &spec,
        &src.observe_config(&spec),
    )
    .unwrap();
    check_observer_pool(&src, Some(&text), &spec, &snap.pool).unwrap();
    assert!(check_observer_pool(&src, None, &spec, &snap.pool).is_err());
    let mut other = snap.pool.clone();
    other[0].address = [0xee; 20];
    other.sort_by_key(|o| o.address);
    assert!(check_observer_pool(&src, Some(&text), &spec, &other).is_err());
    // The announcement carries the seats; applying it hands the observer `--seats`.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cl4-announcement");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pool_text = pool_to_json(&snap.pool).unwrap();
    let mut a = onyx_level2::live::announcement(
        &src,
        31_337,
        "0x00000000000000000000000000000000000000aa",
        "http://127.0.0.1:8545",
        DAY,
        START,
        &pool_text,
        true,
    )
    .unwrap();
    a["seats"] = serde_json::from_str(&text).unwrap();
    let file = dir.join("round.json");
    std::fs::write(&file, a.to_string()).unwrap();
    let state = dir.join("state");
    let args: Vec<String> = [
        "observer",
        "--announcement",
        file.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let (out, chain) = onyx_level2::live::apply_announcement(&src, &args).unwrap();
    assert_eq!(chain, Some(31_337));
    let at = out.iter().position(|a| a == "--seats").unwrap();
    let written = std::fs::read_to_string(&out[at + 1]).unwrap();
    assert_eq!(parse_seats(&written).unwrap(), seats);
    let mut twice = args.clone();
    twice.extend(["--seats".into(), "x.json".into()]);
    assert!(onyx_level2::live::apply_announcement(&src, &twice).is_err());
}
