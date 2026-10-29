//! `/v1/submit` (ROADMAP S1, ADR-0038): checks, routes, the rights-cleared
//! pull through a fixture fetcher, the journal, ids, statuses, limits, and
//! the transport's address and redirect rules (THREATS SUB-*). No network.

use super::*;
use crate::journal::test_dir;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

const RESPONSE: &[u8] =
    include_bytes!("../../../fixtures/intake/fiscaldata-debt-to-penny-2026-10-08.json");
/// Keccak-256 of the recorded body, from adapters/source-fiscaldata/fixtures/PROVENANCE.md.
const RESPONSE_KECCAK: &str = "0xf5dd7a5c490fe1584511f4629bb503fdb1a770af9c1b4c13fe07497c6d45bfc4";

fn vectors() -> Value {
    serde_json::from_slice(include_bytes!("../../../fixtures/intake/vectors.json"))
        .expect("vectors")
}

fn example() -> Value {
    vectors()["example"].clone()
}

/// A fetcher that answers from memory and records what it was asked.
struct Fixture {
    answer: Result<Vec<u8>, FetchError>,
    calls: Mutex<Vec<(String, String)>>,
}

impl Fixture {
    fn new(answer: Result<Vec<u8>, FetchError>) -> Arc<Fixture> {
        Arc::new(Fixture {
            answer,
            calls: Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("calls").clone()
    }
}

impl PageFetch for Fixture {
    fn get<'a>(
        &'a self,
        url: &'a str,
        host: &'a str,
    ) -> BoxFuture<'a, Result<Vec<u8>, FetchError>> {
        self.calls
            .lock()
            .expect("calls")
            .push((url.to_string(), host.to_string()));
        let a = self.answer.clone();
        Box::pin(async move { a })
    }
}

fn store_at(
    dir: &std::path::Path,
    f: Option<Arc<Fixture>>,
    limits: SubmitLimits,
) -> Arc<SubmitStore> {
    let f = f.map(|f| f as Arc<dyn PageFetch>);
    Arc::new(SubmitStore::open(dir.join("submissions.journal"), f, limits).expect("open"))
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    client: &str,
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request");
    req.extensions_mut().insert(ClientKey(client.to_string()));
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    // PP6: every answer these tests see conforms to docs/api/openapi.json.
    crate::openapi_check::check(method, uri, status.as_u16(), &v);
    (status, headers, v)
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).expect("bytes")
}

fn code(v: &Value) -> Option<&str> {
    v["error"]["code"].as_str()
}

/// The Treasury walk-through: valid, level 2, pulled once from the cleared
/// host through the fetcher, the value read at the pointer and the body's hash
/// equal to the recorded one in PROVENANCE.md.
#[tokio::test]
async fn the_treasury_example_is_checked_and_pulled_once() {
    let dir = test_dir("submit-check");
    let f = Fixture::new(Ok(RESPONSE.to_vec()));
    let app = routes(store_at(&dir, Some(f.clone()), SubmitLimits::default()));
    let (s, h, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.1",
        bytes(&example()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        h.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(b["valid"], true);
    assert_eq!(b["route"]["level"], 2);
    assert_eq!(b["route"]["path"], "auto_pull");
    assert_eq!(b["route"]["levelName"], "Re-observed");
    assert_eq!(b["fetch"]["pull"], true);
    assert_eq!(b["preview"]["value"], "40305316210829.72");
    assert_eq!(b["preview"]["valueStatus"], "text");
    assert_eq!(b["preview"]["bodyKeccak256"], RESPONSE_KECCAK);
    assert_eq!(b["preview"]["bytes"], RESPONSE.len());
    let calls = f.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "api.fiscaldata.treasury.gov");
    assert_eq!(calls[0].0, example()["endpointUrl"].as_str().expect("url"));
    // A check records nothing.
    assert!(
        std::fs::read_to_string(dir.join("submissions.journal"))
            .expect("journal")
            .lines()
            .count()
            <= 1
    );
}

/// SUB-1: a host that is not rights-cleared is never fetched, whatever the
/// submitter says; nor is anything on a refused, held or claim route.
#[tokio::test]
async fn only_cleared_hosts_on_the_level_2_route_are_pulled() {
    let dir = test_dir("submit-nofetch");
    let f = Fixture::new(Ok(RESPONSE.to_vec()));
    let app = routes(store_at(
        &dir,
        Some(f.clone()),
        SubmitLimits {
            check_per_minute: 100,
            ..SubmitLimits::default()
        },
    ));
    let mut cases = 0;
    for case in vectors()["submissions"].as_array().expect("subs") {
        if !case["error"].is_null() || case["fetch"] == "pull" {
            continue;
        }
        cases += 1;
        let (s, _, b) = call(
            &app,
            "POST",
            "/v1/submit/check",
            "198.51.100.2",
            bytes(&case["submission"]),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{}: {b}", case["name"]);
        assert_eq!(b["fetch"]["pull"], false, "{}", case["name"]);
        assert_eq!(b["fetch"]["reason"], case["fetch"], "{}", case["name"]);
        assert!(b["preview"].is_null());
    }
    assert!(cases >= 6);
    assert!(f.calls().is_empty(), "fetched: {:?}", f.calls());
}

/// Every refused vector is a 400 with the vector's code; an oversized body
/// is a 413; neither fetches.
#[tokio::test]
async fn bad_submissions_are_refused_with_their_code() {
    let dir = test_dir("submit-bad");
    let f = Fixture::new(Ok(RESPONSE.to_vec()));
    let app = routes(store_at(
        &dir,
        Some(f.clone()),
        SubmitLimits {
            check_per_minute: 100,
            submit_per_minute: 100,
            ..SubmitLimits::default()
        },
    ));
    for case in vectors()["submissions"].as_array().expect("subs") {
        let Some(want) = case["error"].as_str() else {
            continue;
        };
        for uri in ["/v1/submit/check", "/v1/submit"] {
            let (s, _, b) = call(
                &app,
                "POST",
                uri,
                "198.51.100.3",
                bytes(&case["submission"]),
            )
            .await;
            assert_eq!(
                (s, code(&b)),
                (StatusCode::BAD_REQUEST, Some(want)),
                "{uri} {}",
                case["name"]
            );
        }
    }
    let mut big = example();
    big["description"] = "x".repeat(MAX_BODY).into();
    let (s, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.3",
        bytes(&big),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("body_too_large"))
    );
    let (s, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.3",
        b"not json".to_vec(),
    )
    .await;
    assert_eq!((s, code(&b)), (StatusCode::BAD_REQUEST, Some("bad_json")));
    assert!(f.calls().is_empty());
    assert_eq!(
        store_at(&test_dir("submit-bad-2"), None, SubmitLimits::default()).recorded(),
        0
    );
}

/// A failed pull (redirect, status, size, timeout, refused address) is a
/// valid check with the fetch error stated, never a preview.
#[tokio::test]
async fn a_failed_pull_says_why_and_shows_no_value() {
    for (err, want) in [
        (FetchError::Redirect(302), "redirect_refused"),
        (FetchError::Status(503), "source_status"),
        (
            FetchError::TooLarge(onyx_intake::MAX_BODY_BYTES),
            "body_too_large",
        ),
        (FetchError::Timeout(10), "timeout"),
        (
            FetchError::Refused("api.fiscaldata.treasury.gov resolves to 10.0.0.1".into()),
            "address_refused",
        ),
    ] {
        let dir = test_dir("submit-fetcherr");
        let app = routes(store_at(
            &dir,
            Some(Fixture::new(Err(err))),
            SubmitLimits::default(),
        ));
        let (s, _, b) = call(
            &app,
            "POST",
            "/v1/submit/check",
            "198.51.100.4",
            bytes(&example()),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["fetchError"]["code"], want);
        assert!(b["preview"].is_null());
    }
    // No transport configured: the check says so and pulls nothing.
    let app = routes(store_at(
        &test_dir("submit-notransport"),
        None,
        SubmitLimits::default(),
    ));
    let (_, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.4",
        bytes(&example()),
    )
    .await;
    assert_eq!(b["fetch"]["reason"], "no_transport");
}

/// Accepted and held submissions are recorded with their status, survive a
/// reopen, and read back by id; the same content again is a new entry with a
/// new id and says nothing about the first (S1-R1-8); a refused one
/// is a 422 and is not recorded. The journal holds no client address.
#[tokio::test]
async fn submissions_are_recorded_pending_review_and_read_back_by_id() {
    let dir = test_dir("submit-journal");
    let limits = SubmitLimits {
        submit_per_minute: 100,
        ..SubmitLimits::default()
    };
    let client = "203.0.113.77";
    let id = {
        let store = store_at(&dir, None, limits);
        let app = routes(store.clone());
        let (s, _, b) = call(&app, "POST", "/v1/submit", client, bytes(&example())).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        let id = b["id"].as_str().expect("id").to_string();
        assert!(is_id(&id), "{id}");
        assert_eq!(b["status"], PENDING_REVIEW);
        let (s, _, b2) = call(&app, "POST", "/v1/submit", client, bytes(&example())).await;
        assert_eq!(s, StatusCode::CREATED, "{b2}");
        assert_ne!(
            b2["id"], b["id"],
            "the same content must not return the first id"
        );
        assert!(
            b2.get("duplicate").is_none(),
            "no field says the content was seen: {b2}"
        );
        let mut held = example();
        held["answers"]["rights"] = "not_sure".into();
        held["answers"]
            .as_object_mut()
            .expect("answers")
            .remove("licenseId");
        let (s, _, h) = call(&app, "POST", "/v1/submit", client, bytes(&held)).await;
        assert_eq!(
            (s, &h["status"]),
            (StatusCode::CREATED, &Value::from(HELD_RIGHTS_REVIEW))
        );
        let mut person = example();
        person["answers"]["namesPerson"] = true.into();
        let (s, _, r) = call(&app, "POST", "/v1/submit", client, bytes(&person)).await;
        assert_eq!(
            (s, code(&r)),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("refused"))
        );
        assert_eq!(r["route"]["reasons"][0], "names_person");
        assert_eq!(store.recorded(), 3);
        id
    };
    // Reopened from the journal: the same id, the same status.
    let store = store_at(&dir, None, limits);
    assert_eq!(store.recorded(), 3);
    let app = routes(store);
    let (s, h, b) = call(&app, "GET", &format!("/v1/submit/{id}"), client, Vec::new()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, json!({ "id": id, "status": PENDING_REVIEW }));
    assert_eq!(
        h.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    for bad in [
        "sub_00000000000000000000000000000000",
        "sub_XYZ",
        "1",
        "sub_",
    ] {
        let (s, _, b) = call(
            &app,
            "GET",
            &format!("/v1/submit/{bad}"),
            client,
            Vec::new(),
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::NOT_FOUND, Some("not_found")),
            "{bad}"
        );
    }
    // No listing route.
    let (s, _, _) = call(&app, "GET", "/v1/submit", client, Vec::new()).await;
    assert_ne!(s, StatusCode::OK);
    let journal = std::fs::read_to_string(dir.join("submissions.journal")).expect("journal");
    assert!(
        !journal.contains(client),
        "the journal holds the client's address"
    );
    assert!(journal.contains("U.S. total public debt outstanding"));
}

#[tokio::test]
async fn own_data_payload_stays_private_and_survives_journal_replay() {
    let dir = test_dir("submit-own-data");
    let client = "203.0.113.78";
    let content = "date,value\n2026-01-01,12\n";
    let digest = format!(
        "0x{}",
        hex::encode(onyx_core::keccak256(content.as_bytes()).0)
    );
    let mut own = example();
    own["answers"]["what"] = "own".into();
    own["answers"]["rights"] = "own_data".into();
    own["answers"]
        .as_object_mut()
        .expect("answers")
        .remove("licenseId");
    own["sourceUrl"] = "".into();
    own["dataPayload"] =
        json!({"format":"csv","content":content,"bytes":content.len(),"keccak256":digest});
    let store = store_at(&dir, None, SubmitLimits::default());
    let app = routes(store.clone());
    let (status, _, receipt) = call(&app, "POST", "/v1/submit", client, bytes(&own)).await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    assert_eq!(receipt["status"], PENDING_REVIEW);
    assert!(receipt.get("dataPayload").is_none());
    assert!(receipt.get("content").is_none());
    let id = receipt["id"].as_str().expect("receipt id").to_string();
    drop(app);
    drop(store);

    let reopened = store_at(&dir, None, SubmitLimits::default());
    let item = reopened.review_item(&id).expect("private review record");
    assert_eq!(
        item.submission
            .data_payload
            .as_ref()
            .expect("payload")
            .content,
        content
    );
    let app = routes(reopened);
    let (status, _, public) =
        call(&app, "GET", &format!("/v1/submit/{id}"), client, Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(public, json!({"id":id,"status":PENDING_REVIEW}));
    assert!(public.get("dataPayload").is_none());
}

/// SUB-2: per-client budgets (another client is unaffected), a global budget
/// on submissions and on pulls, and the recorded cap.
#[tokio::test]
async fn budgets_and_the_recorded_cap_hold() {
    let dir = test_dir("submit-limits");
    let f = Fixture::new(Ok(RESPONSE.to_vec()));
    let app = routes(store_at(
        &dir,
        Some(f.clone()),
        SubmitLimits {
            check_per_minute: 1,
            submit_per_minute: 1,
            status_per_minute: 1,
            global_submit_per_minute: 2,
            global_fetch_per_minute: 2,
            max_recorded: 2,
            ..SubmitLimits::default()
        },
    ));
    let (s, _, _) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "192.0.2.10",
        bytes(&example()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "192.0.2.10",
        bytes(&example()),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.get("retry-after").is_some());
    let (s, _, _) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "192.0.2.11",
        bytes(&example()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "another client has its own budget");
    // The third pull on the server in the minute: refused for everyone.
    let (s, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "192.0.2.12",
        bytes(&example()),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert_eq!(f.calls().len(), 2);

    let distinct = |n: usize| {
        let mut e = example();
        e["title"] = format!("Submission {n}").into();
        bytes(&e)
    };
    let (s, _, _) = call(&app, "POST", "/v1/submit", "192.0.2.20", distinct(1)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, b) = call(&app, "POST", "/v1/submit", "192.0.2.20", distinct(2)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    let (s, _, _) = call(&app, "POST", "/v1/submit", "192.0.2.21", distinct(3)).await;
    assert_eq!(s, StatusCode::CREATED);
    // The cap: two recorded, the third refused before any budget is spent.
    let (s, _, b) = call(&app, "POST", "/v1/submit", "192.0.2.22", distinct(4)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("intake_full"))
    );
    // A global budget below the clients' sum: a fourth client is refused too.
    let app = routes(store_at(
        &test_dir("submit-global"),
        None,
        SubmitLimits {
            global_submit_per_minute: 1,
            ..SubmitLimits::default()
        },
    ));
    let (s, _, _) = call(&app, "POST", "/v1/submit", "192.0.2.30", distinct(5)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, b) = call(&app, "POST", "/v1/submit", "192.0.2.31", distinct(6)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

#[test]
fn redirects_and_errors_are_never_followed() {
    assert_eq!(check_status(200), Ok(()));
    assert_eq!(check_status(204), Ok(()));
    for r in [301, 302, 303, 307, 308] {
        assert_eq!(check_status(r), Err(FetchError::Redirect(r)));
    }
    assert_eq!(check_status(404), Err(FetchError::Status(404)));
    assert_eq!(check_status(500), Err(FetchError::Status(500)));
}

#[test]
fn ids_bind_the_entry_and_the_content() {
    let a = submission_id(0, "0xaa");
    assert!(is_id(&a));
    assert_ne!(a, submission_id(1, "0xaa"));
    assert_ne!(a, submission_id(0, "0xab"));
}

/// SUB-1: the real transport refuses names that resolve to a non-public
/// address before any connection (names resolved from /etc/hosts or written
/// as literals: no network).
#[cfg(feature = "http-client")]
#[tokio::test]
async fn the_https_transport_refuses_non_public_addresses() {
    use std::net::SocketAddr;
    let sa = |s: &str| s.parse::<SocketAddr>().expect("addr");
    assert!(https::public_only("h", vec![sa("8.8.8.8:443")]).is_ok());
    for bad in [
        vec![],
        vec![sa("127.0.0.1:443")],
        vec![sa("8.8.8.8:443"), sa("169.254.169.254:443")],
        vec![sa("[fd00:ec2::254]:443")],
        vec![sa("10.0.0.1:443")],
    ] {
        assert!(https::public_only("h", bad.clone()).is_err(), "{bad:?}");
    }
    let f = HttpsFetch::new();
    for url in [
        "https://localhost:9/x",
        "https://127.0.0.1:9/x",
        "https://[::1]:9/x",
    ] {
        match f.get(url, "localhost").await {
            Err(FetchError::Refused(why)) => assert!(why.contains("not a public address"), "{why}"),
            other => panic!("{url}: {other:?}"),
        }
    }
    // https only: a plain http URL is refused by the connector.
    assert!(matches!(
        f.get("http://example.org/", "example.org").await,
        Err(FetchError::Unreachable(_))
    ));
}

/// SUB-9: the FRED walk-through. Checked: held on the keyed path, never
/// pulled. Recorded: `held_keyed_source_review`. A submission that carries
/// the key (in the template, a URL, or an extra field) is refused and never
/// recorded; the journal never holds the key.
#[tokio::test]
async fn a_keyed_source_is_held_and_its_key_never_reaches_the_journal() {
    let v = vectors();
    let key = v["syntheticKey"].as_str().expect("key").to_string();
    let fred = v["fredExample"].clone();
    let dir = test_dir("submit-keyed");
    let f = Fixture::new(Ok(RESPONSE.to_vec()));
    let app = routes(store_at(
        &dir,
        Some(f.clone()),
        SubmitLimits {
            check_per_minute: 100,
            submit_per_minute: 100,
            ..SubmitLimits::default()
        },
    ));
    let (s, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.9",
        bytes(&fred),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["route"]["status"], HELD_KEYED_SOURCE_REVIEW);
    assert_eq!(b["route"]["reasons"][0], "source_blocked");
    assert_eq!(b["fetch"]["pull"], false);
    let (s, _, b) = call(&app, "POST", "/v1/submit", "198.51.100.9", bytes(&fred)).await;
    assert_eq!(
        (s, &b["status"]),
        (StatusCode::CREATED, &Value::from(HELD_KEYED_SOURCE_REVIEW))
    );
    let mut leaks = Vec::new();
    let mut t = fred.clone();
    t["endpointTemplate"] = format!(
        "https://api.stlouisfed.org/fred/series/observations?series_id=CPIAUCSL&api_key={key}"
    )
    .into();
    leaks.push(t);
    let mut t = fred.clone();
    t["sourceUrl"] = format!("https://fred.stlouisfed.org/series/CPIAUCSL?api_key={key}").into();
    leaks.push(t);
    let mut t = fred.clone();
    t["apiKey"] = key.clone().into();
    leaks.push(t);
    let mut t = fred.clone();
    t["keyProof"]["key"] = key.clone().into();
    leaks.push(t);
    for t in leaks {
        let (s, _, b) = call(&app, "POST", "/v1/submit", "198.51.100.9", bytes(&t)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
    }
    assert!(f.calls().is_empty());
    let journal = std::fs::read_to_string(dir.join("submissions.journal")).expect("journal");
    assert!(journal.contains("CPIAUCSL"));
    assert!(!journal.contains(&key), "the journal holds the key");
}

/// S1-R1-2: an IPv6 /48 rotating its /64s shares one client budget per
/// minute (no prefix multiplier on submissions or checks), so a /48 cannot
/// outrun the global budget.
#[tokio::test]
async fn a_whole_ipv6_48_shares_one_budget() {
    let distinct = |n: usize| {
        let mut e = example();
        e["title"] = format!("Rotating {n}").into();
        bytes(&e)
    };
    let app = routes(store_at(
        &test_dir("submit-48"),
        None,
        SubmitLimits {
            submit_per_minute: 2,
            check_per_minute: 2,
            global_submit_per_minute: 1_000,
            ..SubmitLimits::default()
        },
    ));
    let mut created = 0;
    for (n, k) in (1..=6).zip([
        "2001:db8:1:1::/64",
        "2001:db8:1:2::/64",
        "2001:db8:1:3::/64",
        "2001:db8:1:104::/64",
        "2001:db8:1:205::/64",
        "2001:db8:1:306::/64",
    ]) {
        let (s, _, _) = call(&app, "POST", "/v1/submit", k, distinct(n)).await;
        created += usize::from(s == StatusCode::CREATED);
    }
    assert_eq!(
        created, 2,
        "six /64s of one /48 got {created} submissions in a minute"
    );
    let mut checked = 0;
    for k in [
        "2001:db8:2:1::/64",
        "2001:db8:2:2::/64",
        "2001:db8:2:3::/64",
        "2001:db8:2:4::/64",
    ] {
        let (s, _, _) = call(&app, "POST", "/v1/submit/check", k, bytes(&example())).await;
        checked += usize::from(s == StatusCode::OK);
    }
    assert_eq!(
        checked, 2,
        "four /64s of one /48 got {checked} checks in a minute"
    );
}

/// S1-R1-2: a per-client daily quota (the /48 shared) and a global daily
/// cap bound what slow, patient clients record.
#[tokio::test]
async fn daily_quotas_bound_each_client_and_the_server() {
    let distinct = |n: usize| {
        let mut e = example();
        e["title"] = format!("Daily {n}").into();
        bytes(&e)
    };
    let app = routes(store_at(
        &test_dir("submit-daily"),
        None,
        SubmitLimits {
            submit_per_minute: 100,
            submit_per_day: 2,
            check_per_minute: 100,
            check_per_day: 1,
            global_submit_per_minute: 1_000,
            global_submit_per_day: 3,
            ..SubmitLimits::default()
        },
    ));
    for n in 1..=2 {
        let (s, _, b) = call(&app, "POST", "/v1/submit", "198.51.100.40", distinct(n)).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    let (s, _, b) = call(&app, "POST", "/v1/submit", "198.51.100.40", distinct(3)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("daily_quota"))
    );
    // The /48 shares the client's day.
    let (s, _, _) = call(&app, "POST", "/v1/submit", "2001:db8:9:1::/64", distinct(4)).await;
    assert_eq!(s, StatusCode::CREATED);
    // Three recorded today on the server: a fresh client is refused too.
    let (s, _, b) = call(&app, "POST", "/v1/submit", "198.51.100.41", distinct(5)).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("daily_quota"))
    );
    let (s, _, _) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.42",
        bytes(&example()),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, b) = call(
        &app,
        "POST",
        "/v1/submit/check",
        "198.51.100.42",
        bytes(&example()),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("daily_quota"))
    );
}

// U2 / ADR-0039: actual journal and router, no mocked persistence.
#[tokio::test]
async fn review_is_durable_private_hash_bound_and_revocable() {
    let dir = test_dir("submit-review");
    let store = store_at(&dir, None, SubmitLimits::default());
    let app = routes(store.clone());
    let (_, _, receipt) = call(
        &app,
        "POST",
        "/v1/submit",
        "198.51.100.1",
        bytes(&example()),
    )
    .await;
    let id = receipt["id"].as_str().expect("test setup/result");
    let hash = receipt["hash"].as_str().expect("test setup/result");
    let item = store.review_item(id).expect("test setup/result");
    let at = item.received_unix + 1;
    assert!(store
        .review(id, 0, "wrong", "approved", "reviewed", at)
        .is_err());
    assert!(store
        .review(id, 1, hash, "approved", "reviewed", at)
        .is_err());
    assert!(store
        .review(id, 0, hash, "approved", "anything", at)
        .is_err());
    assert!(store
        .review(id, 0, hash, "approved", "reviewed", at - 2)
        .is_err());
    let approved = store
        .review(id, 0, hash, "approved", "reviewed", at)
        .expect("test setup/result");
    assert_eq!(approved.revision, 1);
    assert!(store
        .review(id, 0, hash, "rejected", "withdrawn", at)
        .is_err());
    let (status, headers, public) = call(
        &app,
        "GET",
        &format!("/v1/submit/{id}"),
        "198.51.100.1",
        vec![],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(public, json!({"id":id,"status":"approved"}));
    let id = id.to_owned();
    let hash = hash.to_owned();
    drop(app);
    drop(store);
    let store = store_at(&dir, None, SubmitLimits::default());
    assert_eq!(
        store.review_item(&id).expect("test setup/result").status,
        "approved"
    );
    let rejected = store
        .review(&id, 1, &hash, "rejected", "withdrawn", at + 1)
        .expect("test setup/result");
    assert_eq!(rejected.revision, 2);
    assert!(store
        .review(&id, 2, &hash, "approved", "reviewed", at + 2)
        .is_err());
    assert_eq!(store.recorded(), 1, "review events are not submissions");
    drop(store);
    let reopened = store_at(&dir, None, SubmitLimits::default());
    assert_eq!(
        reopened.review_item(&id).expect("test setup/result"),
        rejected
    );
    assert_eq!(
        reopened
            .review_list(None, 100)
            .expect("test setup/result")
            .len(),
        1
    );
    assert!(reopened.review_list(None, 101).is_err());
    assert!(reopened.review_list(None, 0).is_err());
    assert!(reopened
        .review_list(Some(&id), 100)
        .expect("test setup/result")
        .is_empty());
}

#[tokio::test]
async fn held_sources_cannot_be_approved_or_reviewed_by_http() {
    let dir = test_dir("submit-held-review");
    let store = store_at(&dir, None, SubmitLimits::default());
    let app = routes(store.clone());
    let mut s = example();
    s["answers"]["rights"] = json!("not_sure");
    s["answers"]
        .as_object_mut()
        .expect("test setup/result")
        .remove("licenseId");
    let (_, _, receipt) = call(&app, "POST", "/v1/submit", "198.51.100.1", bytes(&s)).await;
    let id = receipt["id"].as_str().expect("test setup/result");
    let hash = receipt["hash"].as_str().expect("test setup/result");
    let at = store
        .review_item(id)
        .expect("test setup/result")
        .received_unix;
    assert!(store
        .review(id, 0, hash, "approved", "reviewed", at)
        .is_err());
    assert_eq!(
        store
            .review(id, 0, hash, "rejected", "rights_unresolved", at)
            .expect("test setup/result")
            .status,
        "rejected"
    );
    for uri in ["/v1/submit/review", "/v1/submit/list"] {
        let resp = app
            .clone()
            .oneshot(
                Request::post(uri)
                    .body(Body::from("{}"))
                    .expect("test setup/result"),
            )
            .await
            .expect("test setup/result");
        assert!(matches!(
            resp.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
    }
    let second = SubmitStore::open(
        dir.join("submissions.journal"),
        None,
        SubmitLimits::default(),
    );
    assert!(
        second.is_err(),
        "a reviewer cannot bypass the running node's writer lock"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_submitters_cannot_take_the_same_last_slot() {
    let dir = test_dir("submit-last-slot");
    let store = store_at(
        &dir,
        None,
        SubmitLimits {
            max_recorded: 1,
            ..SubmitLimits::default()
        },
    );
    let app = routes(store.clone());
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let written_tx = Mutex::new(Some(written_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    store
        .ledger
        .lock()
        .expect("test setup/result")
        .before_publish = Some(Arc::new(move || {
        written_tx
            .lock()
            .expect("test setup/result")
            .take()
            .expect("test setup/result")
            .send(())
            .expect("test setup/result");
        release_rx
            .lock()
            .expect("test setup/result")
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("test setup/result");
    }));
    let sending = app.clone();
    let first = tokio::spawn(async move {
        call(
            &sending,
            "POST",
            "/v1/submit",
            "198.51.100.1",
            bytes(&example()),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), written_rx)
        .await
        .expect("test setup/result")
        .expect("test setup/result");
    let sending = app.clone();
    let second = tokio::spawn(async move {
        call(
            &sending,
            "POST",
            "/v1/submit",
            "198.51.100.2",
            bytes(&example()),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.workers.available_permits() != MAX_LEDGER_WORKERS - 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test setup/result");
    release_tx.send(()).expect("test setup/result");
    let tasks = [first, second];
    let mut codes = Vec::new();
    for t in tasks {
        let (code, _, body) = t.await.expect("test setup/result");
        if code == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(body["error"]["code"], "intake_full");
        }
        codes.push(code.as_u16());
    }
    codes.sort();
    assert_eq!(codes, [201, 503]);
    assert_eq!(store.recorded(), 1);
    drop(app);
    drop(store);
    assert_eq!(store_at(&dir, None, SubmitLimits::default()).recorded(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_after_append_still_publishes_live_receipt_and_capacity() {
    let dir = test_dir("submit-cancel");
    let store = store_at(
        &dir,
        None,
        SubmitLimits {
            max_recorded: 1,
            ..SubmitLimits::default()
        },
    );
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let written_tx = Mutex::new(Some(written_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    store
        .ledger
        .lock()
        .expect("test setup/result")
        .before_publish = Some(Arc::new(move || {
        written_tx
            .lock()
            .expect("test setup/result")
            .take()
            .expect("test setup/result")
            .send(())
            .expect("test setup/result");
        release_rx
            .lock()
            .expect("test setup/result")
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("test setup/result");
    }));
    let app = routes(store.clone());
    let sending = app.clone();
    let task = tokio::spawn(async move {
        call(
            &sending,
            "POST",
            "/v1/submit",
            "198.51.100.1",
            bytes(&example()),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), written_rx)
        .await
        .expect("test setup/result")
        .expect("test setup/result");
    task.abort();
    assert!(task.await.expect_err("cancelled task").is_cancelled());
    release_tx.send(()).expect("test setup/result");
    let st = store.clone();
    let item = tokio::task::spawn_blocking(move || {
        st.review_list(None, 1)
            .expect("test setup/result")
            .remove(0)
    })
    .await
    .expect("test setup/result");
    let (_, _, public) = call(
        &app,
        "GET",
        &format!("/v1/submit/{}", item.id),
        "198.51.100.1",
        vec![],
    )
    .await;
    assert_eq!(public, json!({"id":item.id,"status":"pending_review"}));
    let (status, _, _) = call(
        &app,
        "POST",
        "/v1/submit",
        "198.51.100.2",
        bytes(&example()),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    drop(app);
    drop(store);
    assert_eq!(
        store_at(&dir, None, SubmitLimits::default())
            .review_item(&item.id)
            .expect("test setup/result")
            .status,
        "pending_review"
    );
}

#[tokio::test]
async fn replay_revalidates_submission_and_rejects_forged_or_reordered_review() {
    for corruption in ["hash", "route", "submission", "review", "version"] {
        let dir = test_dir(&format!("submit-corrupt-{corruption}"));
        let store = store_at(&dir, None, SubmitLimits::default());
        let app = routes(store.clone());
        let (_, _, receipt) = call(
            &app,
            "POST",
            "/v1/submit",
            "198.51.100.1",
            bytes(&example()),
        )
        .await;
        drop(app);
        drop(store);
        let path = dir.join("submissions.journal");
        let (mut journal, live) =
            Journal::open(&path, JOURNAL_KIND, &BINDING).expect("test setup/result");
        let (seq, mut entry) = live[0].clone();
        if corruption == "review" {
            journal.apply(&[],&[json!({"kind":"review","version":1,"submissionId":receipt["id"],"hash":receipt["hash"],"revision":2,"from":"pending_review","to":"approved","reason":"reviewed","reviewedUnix":u64::MAX})]).expect("test setup/result");
        } else {
            match corruption {
                "hash" => entry["hash"] = json!("0x00"),
                "route" => entry["status"] = json!("approved"),
                "submission" => entry["submission"]["title"] = json!(""),
                _ => entry["version"] = json!(99),
            }
            journal.apply(&[seq], &[entry]).expect("test setup/result");
        }
        drop(journal);
        assert!(
            SubmitStore::open(&path, None, SubmitLimits::default()).is_err(),
            "{corruption}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_handlers_cannot_exhaust_unbounded_ledger_workers() {
    let dir = test_dir("submit-worker-cap");
    let store = store_at(
        &dir,
        None,
        SubmitLimits {
            status_per_minute: 100,
            ..SubmitLimits::default()
        },
    );
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let written_tx = Mutex::new(Some(written_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    store
        .ledger
        .lock()
        .expect("test setup/result")
        .before_publish = Some(Arc::new(move || {
        written_tx
            .lock()
            .expect("test setup/result")
            .take()
            .expect("test setup/result")
            .send(())
            .expect("test setup/result");
        release_rx
            .lock()
            .expect("test setup/result")
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("test setup/result");
    }));
    let app = routes(store.clone());
    let sending = app.clone();
    let sender = tokio::spawn(async move {
        call(
            &sending,
            "POST",
            "/v1/submit",
            "198.51.100.1",
            bytes(&example()),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), written_rx)
        .await
        .expect("test setup/result")
        .expect("test setup/result");
    sender.abort();
    let _ = sender.await;
    let uri = "/v1/submit/sub_00000000000000000000000000000000";
    for expected_left in (0..MAX_LEDGER_WORKERS - 1).rev() {
        let reading = app.clone();
        let reader =
            tokio::spawn(async move { call(&reading, "GET", uri, "198.51.100.2", vec![]).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store.workers.available_permits() != expected_left {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("test setup/result");
        reader.abort();
        let _ = reader.await;
    }
    let (status, _, body) = call(&app, "GET", uri, "198.51.100.3", vec![]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "intake_busy");
    assert_eq!(store.workers.available_permits(), 0);
    release_tx.send(()).expect("test setup/result");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.workers.available_permits() != MAX_LEDGER_WORKERS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test setup/result");
    let (status, _, _) = call(&app, "GET", uri, "198.51.100.3", vec![]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(store.recorded(), 1);
}

#[test]
fn legacy_receipts_replay_and_each_invalid_review_field_is_refused() {
    let submission = Submission::parse(&bytes(&example())).expect("test setup/result");
    let hash = hash_hex(&submission);
    let route = submission.route();
    for corrupt in [
        "none", "hash", "revision", "from", "to", "version", "orphan", "time", "unknown",
    ] {
        let dir = test_dir(&format!("submit-event-{corrupt}"));
        let path = dir.join("submissions.journal");
        let (mut journal, _) =
            Journal::open(&path, JOURNAL_KIND, &BINDING).expect("test setup/result");
        let seq=journal.apply(&[],&[json!({"hash":hash,"status":route.status(),"route":route.to_json(),"receivedUnix":100,"submission":submission})]).expect("test setup/result")[0];
        let id = submission_id(seq, &hash);
        if corrupt != "none" {
            let mut event = json!({"kind":"review","version":1,"submissionId":id,"hash":hash,"revision":1,"from":"pending_review","to":"approved","reason":"reviewed","reviewedUnix":101});
            match corrupt {
                "hash" => event["hash"] = json!("wrong"),
                "revision" => event["revision"] = json!(2),
                "from" => event["from"] = json!("approved"),
                "to" => event["to"] = json!("rejected"),
                "version" => event["version"] = json!(2),
                "orphan" => event["submissionId"] = json!("sub_00000000000000000000000000000000"),
                "time" => event["reviewedUnix"] = json!(99),
                _ => event["extra"] = json!(true),
            }
            journal.apply(&[], &[event]).expect("test setup/result");
        }
        drop(journal);
        let reopened = SubmitStore::open(&path, None, SubmitLimits::default());
        if corrupt == "none" {
            assert_eq!(
                reopened
                    .expect("test setup/result")
                    .review_item(&id)
                    .expect("test setup/result")
                    .hash,
                hash
            );
        } else {
            assert!(reopened.is_err(), "{corrupt}");
        }
    }
}

#[tokio::test]
async fn a_fed_receipt_shows_the_observers_evidence_and_nothing_private() {
    use feed::{ledger_path, FedEvidence, FeedLedger};
    let dir = test_dir("submit-feed");
    let journal = dir.join("submissions.journal");
    let store = store_at(&dir, None, SubmitLimits::default());
    let app = routes(store.clone());
    let mut ids = Vec::new();
    for client in ["198.51.100.1", "198.51.100.2", "198.51.100.3"] {
        let (_, _, r) = call(&app, "POST", "/v1/submit", client, bytes(&example())).await;
        ids.push((
            r["id"].as_str().expect("id").to_owned(),
            r["hash"].as_str().expect("hash").to_owned(),
        ));
    }
    let at = store.review_item(&ids[0].0).expect("item").received_unix + 1;
    for (id, hash) in &ids {
        store
            .review(id, 0, hash, "approved", "reviewed", at)
            .expect("approve");
    }
    drop(app);
    drop(store);
    // The worker, with the node stopped: ids[0] fed, ids[1] failed, ids[2] fed then withdrawn.
    let evidence = |id: &str, hash: &str| FedEvidence {
        id: id.into(),
        hash: hash.into(),
        record_date: "2026-10-08".into(),
        round_id: format!("0x{}", "1a".repeat(32)),
        result: format!("0x{}", "2b".repeat(32)),
        value: "40305316210829.72".into(),
        registry: "simulated".into(),
    };
    let mut ledger = FeedLedger::default();
    ledger.fed(&evidence(&ids[0].0, &ids[0].1));
    ledger.failed(&ids[1].0, "private operator reason");
    ledger.fed(&evidence(&ids[2].0, &ids[2].1));
    ledger.save(&ledger_path(&journal)).expect("save");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(ledger_path(&journal))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the ledger is owner-only");
    }
    let store = store_at(&dir, None, SubmitLimits::default());
    store
        .review(&ids[2].0, 1, &ids[2].1, "rejected", "withdrawn", at + 1)
        .expect("withdraw");
    let app = routes(store.clone());
    let get = |id: String| {
        let app = app.clone();
        async move {
            call(
                &app,
                "GET",
                &format!("/v1/submit/{id}"),
                "198.51.100.9",
                vec![],
            )
            .await
            .2
        }
    };
    assert_eq!(
        get(ids[0].0.clone()).await,
        json!({"id": ids[0].0, "status": "approved", "check": {
            "level": 2, "recordDate": "2026-10-08",
            "roundId": format!("0x{}", "1a".repeat(32)), "result": format!("0x{}", "2b".repeat(32)),
            "value": "40305316210829.72", "registry": "simulated"}})
    );
    let failed = get(ids[1].0.clone()).await;
    assert_eq!(failed, json!({"id": ids[1].0, "status": "approved"}));
    assert!(!failed.to_string().contains("private operator reason"));
    assert_eq!(
        get(ids[2].0.clone()).await,
        json!({"id": ids[2].0, "status": "rejected"}),
        "a withdrawn approval shows no evidence"
    );
    drop(app);
    drop(store);
    // A damaged ledger refuses to open, as a damaged journal does.
    std::fs::write(ledger_path(&journal), "{\"x\":1}").expect("write");
    assert!(SubmitStore::open(&journal, None, SubmitLimits::default()).is_err());
}
