//! `/v1/publishers/pointer` (ADR-0066 item 7, THREATS PUBON-PTR-*): what is kept, what is
//! refused, repeats, budgets, the cap, expiry, the journal after a restart and after damage.
//! No network: the module has none to use.

use super::*;
use crate::journal::test_dir;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;

const ORIGIN: &str = "data.example.org/river-levels";
/// 2026-11-17 00:00:00 UTC.
const T0: u64 = 1_794_873_600;
const DAY: u64 = 86_400;

fn hash(n: u8) -> String {
    format!("0x{}", hex::encode([n; 32]))
}

fn body(origin: &str, application: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"origin": origin, "application": application})).expect("body")
}

struct Rig {
    dir: std::path::PathBuf,
    now: Arc<AtomicU64>,
    store: Arc<PointerStore>,
    app: Router,
    alerts: Arc<Mutex<Vec<String>>>,
}

fn journal(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("publisher-pointers.journal")
}

fn rig_at(dir: std::path::PathBuf, limits: PointerLimits, now: Arc<AtomicU64>) -> Rig {
    let clock = now.clone();
    let alerts = Arc::new(Mutex::new(Vec::new()));
    let sink = alerts.clone();
    let store = Arc::new(
        PointerStore::open_at(
            journal(&dir),
            limits,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .expect("open")
        .with_cap_alert(Arc::new(move |l| {
            sink.lock().expect("alerts").push(l.to_string())
        })),
    );
    Rig {
        dir,
        now,
        app: routes(store.clone()),
        store,
        alerts,
    }
}

/// Budgets wide enough that a test of something else never meets one.
fn wide() -> PointerLimits {
    PointerLimits {
        post_per_minute: 10_000,
        post_per_day: 10_000,
        status_per_minute: 10_000,
        global_post_per_minute: 10_000,
        global_post_per_day: 10_000,
        max_recorded: 1_000,
    }
}

fn rig(tag: &str, limits: PointerLimits) -> Rig {
    rig_at(test_dir(tag), limits, Arc::new(AtomicU64::new(T0)))
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
    let v: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    // Every answer these tests see conforms to docs/api/openapi.json.
    crate::openapi_check::check(method, uri, status.as_u16(), &v);
    (status, headers, v)
}

async fn post(r: &Rig, client: &str, origin: &str, application: &str) -> (StatusCode, Value) {
    let (s, _, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        client,
        body(origin, application),
    )
    .await;
    (s, v)
}

async fn status_of(r: &Rig, id: &str) -> (StatusCode, Value) {
    let (s, _, v) = call(
        &r.app,
        "GET",
        &format!("/v1/publishers/pointer/{id}"),
        "198.51.100.9",
        Vec::new(),
    )
    .await;
    (s, v)
}

fn code(v: &Value) -> Option<&str> {
    v["error"]["code"].as_str()
}

/// The journal's lines after the header, as the JSON each holds.
fn lines(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(journal(dir))
        .expect("journal")
        .lines()
        .skip(1)
        .map(|l| serde_json::from_str(l.split_once(' ').expect("framed").1).expect("json"))
        .collect()
}

/// ADR-0066 slice 4's named test (THREATS PUBON-PTR-1, PUBON-PTR-5): a pointer is two
/// strings, the node writes those with a receipt id and a time and nothing else, never the
/// client's address, and nothing in the module can make a request.
#[tokio::test]
async fn a_pointer_keeps_only_an_origin_and_fetches_nothing() {
    let r = rig("pointer-keeps", PointerLimits::default());
    // An origin under a name that never resolves: the answer cannot have waited for it.
    let origin = "data.example.invalid/river-levels";
    let (s, h, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        "198.51.100.7",
        body(origin, &hash(0xab)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(
        h.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let id = v["id"].as_str().expect("id").to_string();
    assert!(is_id(&id), "{id}");
    assert_eq!(
        v,
        json!({"id": id, "status": "received", "expiresUnix": T0 + 6 * DAY,
            "decision": "see_status_list"})
    );
    // The id is random: it is no part and no hash of what the applicant sent.
    assert!(!hash(0xab).contains(&id[4..]));
    let written = lines(&r.dir);
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0],
        json!({"ops": [{"add": 0, "v": {"kind": "pointer", "version": 1, "id": id,
            "origin": origin, "application": hash(0xab), "receivedUnix": T0}}]})
    );
    let file = std::fs::read_to_string(journal(&r.dir)).expect("journal");
    assert!(!file.contains("198.51.100"), "no client address is kept");
    // Nothing fetched: this module names no transport, resolver or socket at all.
    let src = include_str!("publishers.rs");
    for word in [
        "onyx_fetch",
        "PageFetch",
        "HttpsFetch",
        "hyper",
        "TcpStream",
        "ToSocketAddrs",
        "lookup_host",
        "reqwest",
        "UdpSocket",
    ] {
        assert!(!src.contains(word), "publishers.rs names {word}");
    }
    // The status is the same answer, by the receipt alone.
    let (s, got) = status_of(&r, &id).await;
    assert_eq!((s, &got), (StatusCode::OK, &v));
}

/// THREATS PUBON-PTR-2: everything but exactly the two strings in their shapes is refused
/// with its code, and nothing is written.
#[tokio::test]
async fn a_wrong_shape_origin_or_hash_is_refused_and_nothing_is_written() {
    let r = rig("pointer-shapes", wide());
    let good = hash(1);
    let raw = |s: &str| s.as_bytes().to_vec();
    let cases: Vec<(Vec<u8>, StatusCode, &str)> = vec![
        (raw("not json"), StatusCode::BAD_REQUEST, "bad_pointer"),
        (raw("[]"), StatusCode::BAD_REQUEST, "bad_pointer"),
        (raw("{}"), StatusCode::BAD_REQUEST, "bad_pointer"),
        (
            raw(&format!(r#"{{"origin":"{ORIGIN}"}}"#)),
            StatusCode::BAD_REQUEST,
            "bad_pointer",
        ),
        (
            raw(&format!(r#"{{"origin":7,"application":"{good}"}}"#)),
            StatusCode::BAD_REQUEST,
            "bad_pointer",
        ),
        // An unknown field: no contact detail, note or name can ride along.
        (
            raw(&format!(
                r#"{{"origin":"{ORIGIN}","application":"{good}","contact":"someone"}}"#
            )),
            StatusCode::BAD_REQUEST,
            "bad_pointer",
        ),
        // A field twice: a reader of the first and a reader of the last would differ.
        (
            raw(&format!(
                r#"{{"origin":"other.example.org","origin":"{ORIGIN}","application":"{good}"}}"#
            )),
            StatusCode::BAD_REQUEST,
            "bad_pointer",
        ),
        (
            body(ORIGIN, &good.to_uppercase()),
            StatusCode::BAD_REQUEST,
            "bad_application_hash",
        ),
        (
            body(ORIGIN, &good[..64]),
            StatusCode::BAD_REQUEST,
            "bad_application_hash",
        ),
        (
            body(ORIGIN, &good[2..]),
            StatusCode::BAD_REQUEST,
            "bad_application_hash",
        ),
        (
            body(ORIGIN, &format!("{good}00")),
            StatusCode::BAD_REQUEST,
            "bad_application_hash",
        ),
        // A shared host from the compiled list, and one that only reads like one.
        (
            body("someone.github.io/data", &good),
            StatusCode::UNPROCESSABLE_ENTITY,
            "origin_shared_host",
        ),
        (
            body("someone.github.io.example.dev", &good),
            StatusCode::UNPROCESSABLE_ENTITY,
            "origin_shared_host",
        ),
    ];
    for (b, want, c) in cases {
        let text = String::from_utf8_lossy(&b).to_string();
        let (s, _, v) = call(&r.app, "POST", "/v1/publishers/pointer", "198.51.100.1", b).await;
        assert_eq!((s, code(&v)), (want, Some(c)), "{text}");
    }
    for origin in [
        "",
        "https://data.example.org",
        "data.example.org/",
        "data.example.org:8443",
        "data.example.org/a?b=c",
        "data.example.org/a#b",
        "Data.Example.org",
        "localhost",
        "192.0.2.7",
        "192.0.2.7/data",
        "[2001:db8::1]",
        "xn--e1afmkfd.example.org",
        "data.example.org/a/b/c/d/e",
        "data.example.org/../etc",
        "data.example.org/a b",
        "data.example.org/a\nb",
        "data.example.org/%2e%2e",
        // E-mail-shaped text and a user part are outside the grammar (THREATS SUB-4).
        "someone@example.org",
        "mailto:someone@example.org",
        "user:pass@data.example.org",
        "data.example.org/$(id)",
        "data.example.org/<script>",
    ] {
        let (s, v) = post(&r, "198.51.100.1", origin, &good).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, Some("bad_origin")),
            "{origin:?}"
        );
    }
    // Over the body cap, whatever it holds.
    let big = body(ORIGIN, &format!("{good}{}", "0".repeat(MAX_BODY)));
    let (s, _, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        "198.51.100.1",
        big,
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("body_too_large"))
    );
    assert_eq!(
        parse(&vec![b' '; MAX_BODY + 1]),
        Err(PointerError::Shape("over the body cap"))
    );
    assert!(lines(&r.dir).is_empty(), "nothing was written");
    assert_eq!(r.store.recorded(), 0);
    // A receipt that is not one, or that nobody holds.
    for id in [
        "ptr_1",
        "sub_0123456789abcdef0123456789abcdef",
        "ptr_0123456789ABCDEF0123456789abcdef",
    ] {
        let (s, v) = status_of(&r, id).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::NOT_FOUND, Some("not_found")),
            "{id}"
        );
    }
    let (s, v) = status_of(&r, &format!("ptr_{}", "0".repeat(32))).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, Some("not_found")));
}

/// THREATS PUBON-PTR-3: the same origin and hash again is the same receipt and one journal
/// line, from any client; an origin holds at most three waiting hashes.
#[tokio::test]
async fn a_repeat_is_the_same_receipt_and_an_origin_holds_three() {
    let r = rig("pointer-repeat", wide());
    let (s, first) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!(s, StatusCode::CREATED);
    r.now.fetch_add(3 * DAY, Ordering::SeqCst);
    for client in ["198.51.100.1", "203.0.113.5"] {
        let (s, again) = post(&r, client, ORIGIN, &hash(1)).await;
        assert_eq!(s, StatusCode::OK, "a repeat is not a new pointer");
        assert_eq!(again, first, "the same receipt, and its time is not moved");
    }
    assert_eq!(lines(&r.dir).len(), 1);
    // Two more hashes for the origin are taken, the fourth is not.
    for n in [2, 3] {
        let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_ne!(v["id"], first["id"]);
    }
    let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(4)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, Some("pointer_origin_busy"))
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("public issue")),
        "{v}"
    );
    // Review PTR-R1 L1: the three places are the host's, whatever the path: a shorter or a
    // longer origin on the same host is as full.
    for same in [
        "data.example.org",
        "data.example.org/river-levels/v2",
        "data.example.org/b",
    ] {
        let (s, v) = post(&r, "198.51.100.1", same, &hash(4)).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::CONFLICT, Some("pointer_origin_busy")),
            "{same}"
        );
    }
    // Another host is not affected, also not a host under or above this one.
    for other in [
        "other.example.org",
        "sub.data.example.org",
        "example.org/river-levels",
    ] {
        let (s, v) = post(&r, "198.51.100.1", other, &hash(4)).await;
        assert_eq!(s, StatusCode::CREATED, "{other}: {v}");
    }
    // A repeat of a held one is still answered while the origin is full.
    let (s, again) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!((s, &again), (StatusCode::OK, &first));
    // Once the first has expired its place is free.
    r.now.store(T0 + 6 * DAY, Ordering::SeqCst);
    let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(4)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(lines(&r.dir).len(), 7);
}

/// THREATS PUBON-PTR-4: the budgets per client and for the whole server, each with
/// `Retry-After`; another client keeps its own; a refused pointer is not written.
#[tokio::test]
async fn budgets_hold_per_client_and_for_the_server() {
    // Per minute, per client.
    let r = rig(
        "pointer-minute",
        PointerLimits {
            post_per_minute: 2,
            ..wide()
        },
    );
    for n in 1..=2 {
        let (s, _) = post(&r, "198.51.100.1", ORIGIN, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let (s, h, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        "198.51.100.1",
        body(ORIGIN, &hash(3)),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.contains_key("retry-after"));
    // A body that would be refused anyway spends the budget too (it is counted first).
    let (s, _, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        "198.51.100.1",
        b"x".to_vec(),
    )
    .await;
    assert_eq!(code(&v), Some("rate_limited"), "{s}");
    let (s, _) = post(&r, "203.0.113.5", ORIGIN, &hash(3)).await;
    assert_eq!(s, StatusCode::CREATED, "another client has its own budget");
    // An IPv6 /48 shares one client's budget, however many /64s it rotates.
    for (n, client) in ["2001:db8:1:1::/64", "2001:db8:1:2::/64"]
        .iter()
        .enumerate()
    {
        let (s, _) = post(&r, client, "v6.example.org", &hash(n as u8)).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let (s, v) = post(&r, "2001:db8:1:3::/64", "v6b.example.org", &hash(9)).await;
    assert_eq!(code(&v), Some("rate_limited"), "{s}");
    assert_eq!(r.store.recorded(), 5);

    // Per day, per client: a repeat costs the day's budget like a new pointer.
    let r = rig(
        "pointer-day",
        PointerLimits {
            post_per_day: 2,
            ..wide()
        },
    );
    let (s, _) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!(s, StatusCode::OK);
    for n in [1, 2] {
        let (s, h, v) = call(
            &r.app,
            "POST",
            "/v1/publishers/pointer",
            "198.51.100.1",
            body(ORIGIN, &hash(n)),
        )
        .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::TOO_MANY_REQUESTS, Some("daily_quota")),
            "also the repeat"
        );
        assert!(h.contains_key("retry-after"));
    }
    assert_eq!(r.store.recorded(), 1);

    // The server's minute and day: every client together.
    for (limits, want) in [
        (
            PointerLimits {
                global_post_per_minute: 2,
                ..wide()
            },
            "rate_limited",
        ),
        (
            PointerLimits {
                global_post_per_day: 2,
                ..wide()
            },
            "daily_quota",
        ),
    ] {
        let r = rig("pointer-global", limits);
        for n in 1..=2u8 {
            let (s, _) = post(&r, &format!("198.51.100.{n}"), ORIGIN, &hash(n)).await;
            assert_eq!(s, StatusCode::CREATED);
        }
        let (s, v) = post(&r, "198.51.100.3", "other.example.org", &hash(3)).await;
        assert_eq!((s, code(&v)), (StatusCode::TOO_MANY_REQUESTS, Some(want)));
        // A repeat writes nothing and so does not need the server's budget.
        let (s, _) = post(&r, "198.51.100.4", ORIGIN, &hash(1)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(r.store.recorded(), 2);
    }

    // Status reads have their own budget.
    let r = rig(
        "pointer-status-budget",
        PointerLimits {
            status_per_minute: 2,
            ..wide()
        },
    );
    let id = format!("ptr_{}", "0".repeat(32));
    for _ in 0..2 {
        assert_eq!(status_of(&r, &id).await.0, StatusCode::NOT_FOUND);
    }
    let (s, v) = status_of(&r, &id).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

/// THREATS PUBON-PTR-4: the journal's cap refuses with 503 and writes nothing more; the
/// warning with its tag comes at 80 percent, once per further percent, and at start.
#[tokio::test]
async fn the_cap_refuses_and_warns_from_eighty_percent() {
    let limits = PointerLimits {
        max_recorded: 10,
        ..wide()
    };
    let r = rig("pointer-cap", limits);
    assert_eq!(r.store.warn_if_near_cap(), None);
    for n in 0..10u8 {
        let (s, v) = post(&r, "198.51.100.1", &format!("d{n}.example.org"), &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let alerts = r.alerts.lock().expect("alerts").clone();
    assert_eq!(alerts.len(), 3, "{alerts:?}");
    assert!(
        alerts[0]
            .starts_with("onyx-pointer-cap-alert: the pointer journal holds 8 of 10 entries (80%"),
        "{alerts:?}"
    );
    assert!(
        alerts.iter().all(|a| !a.contains("example.org")),
        "counts only"
    );
    let (s, h, v) = call(
        &r.app,
        "POST",
        "/v1/publishers/pointer",
        "198.51.100.1",
        body("late.example.org", &hash(77)),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("pointer_full"))
    );
    assert!(h.contains_key("retry-after"));
    assert_eq!(lines(&r.dir).len(), 10);
    // A repeat of a held pointer is still answered at the cap.
    let (s, _) = post(&r, "198.51.100.1", "d3.example.org", &hash(3)).await;
    assert_eq!(s, StatusCode::OK);
    // After a restart the start warns again; a journal over the cap is refused whole.
    let (dir, now) = (r.dir.clone(), r.now.clone());
    drop(r);
    let r = rig_at(dir.clone(), limits, now.clone());
    assert!(r
        .store
        .warn_if_near_cap()
        .is_some_and(|l| l.contains("10 of 10 entries (100%")));
    drop(r);
    let small = PointerLimits {
        max_recorded: 9,
        ..wide()
    };
    assert!(PointerStore::open(journal(&dir), small).is_err());
    assert!(read_pointers(&journal(&dir), 9).is_err());
    // Twelve days on the entries are gone and the journal takes pointers again.
    now.store(T0 + 12 * DAY, Ordering::SeqCst);
    let r = rig_at(dir, limits, now);
    assert_eq!(r.store.recorded(), 0);
    let (s, _) = post(&r, "198.51.100.1", "late.example.org", &hash(77)).await;
    assert_eq!(s, StatusCode::CREATED);
}

/// THREATS PUBON-PTR-3: a pointer waits six days, answers `expired` with its reason for six
/// more, and is then removed from the journal, at the next pointer or the next start.
#[tokio::test]
async fn a_pointer_expires_after_six_days_and_leaves_after_twelve() {
    let r = rig("pointer-expiry", wide());
    let (_, v) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    let id = v["id"].as_str().expect("id").to_string();
    r.now.store(T0 + 6 * DAY - 1, Ordering::SeqCst);
    assert_eq!(status_of(&r, &id).await.1["status"], "received");
    r.now.store(T0 + 6 * DAY, Ordering::SeqCst);
    let (s, v) = status_of(&r, &id).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"id": id, "status": "expired", "reason": "pointer_lifetime_over",
            "expiresUnix": T0 + 6 * DAY, "decision": "see_status_list"})
    );
    // The same pointer again is the expired receipt, not a new six days.
    let (s, again) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!((s, &again), (StatusCode::OK, &v));
    r.now.store(T0 + 12 * DAY - 1, Ordering::SeqCst);
    assert_eq!(status_of(&r, &id).await.0, StatusCode::OK);
    // Past the kept time the receipt is unknown at once, before anything removed the line.
    r.now.store(T0 + 12 * DAY, Ordering::SeqCst);
    assert_eq!(status_of(&r, &id).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        read_pointers(&journal(&r.dir), 1_000).expect("read").len(),
        1
    );
    // The next pointer removes it, and the same origin and hash is then a new pointer.
    let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_ne!(v["id"].as_str(), Some(id.as_str()));
    let left = read_pointers(&journal(&r.dir), 1_000).expect("read");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].received_unix, T0 + 12 * DAY);
    // A start removes what is past its time too.
    let (dir, now) = (r.dir.clone(), r.now.clone());
    drop(r);
    now.store(T0 + 30 * DAY, Ordering::SeqCst);
    let r = rig_at(dir, wide(), now);
    assert_eq!(r.store.recorded(), 0);
    assert!(read_pointers(&journal(&r.dir), 1_000)
        .expect("read")
        .is_empty());
}

/// The journal after a restart: the same receipts, states and repeats; and the reviewer's
/// read beside the running node, which takes no lock and writes nothing.
#[tokio::test]
async fn the_journal_replays_and_is_read_beside_the_node() {
    let r = rig("pointer-replay", wide());
    let mut ids = Vec::new();
    for n in 1..=3u8 {
        let (_, v) = post(&r, "198.51.100.1", &format!("d{n}.example.org/a"), &hash(n)).await;
        ids.push(v["id"].as_str().expect("id").to_string());
        r.now.fetch_add(DAY, Ordering::SeqCst);
    }
    // Beside the open store: no lock is taken, the file is not touched.
    let before = std::fs::read(journal(&r.dir)).expect("bytes");
    let read = read_pointers(&journal(&r.dir), 1_000).expect("read beside the node");
    assert_eq!(std::fs::read(journal(&r.dir)).expect("bytes"), before);
    assert_eq!(
        read.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ids,
        "oldest first"
    );
    assert_eq!(
        read[0],
        Pointer {
            id: ids[0].clone(),
            origin: "d1.example.org/a".into(),
            application: hash(1),
            received_unix: T0
        }
    );
    // The two lines `onyx-publish verify-application --pointer` reads.
    assert_eq!(
        read[0].text(),
        format!(
            "origin: d1.example.org/a\napplication keccak: {}\n",
            hash(1)
        )
    );
    let now = T0 + 6 * DAY + 3_600;
    let none = Drops::new();
    let list = render_list(&read, &none, 1_000, now, false);
    assert_eq!(
        list,
        format!(
            "2 pointer(s) waiting, 1 expired and kept, 0 dropped; the journal holds 3 of 1000 entries\n{}  23 h left  d2.example.org/a  {}\n{}  1 d 23 h left  d3.example.org/a  {}\n",
            ids[1],
            hash(2),
            ids[2],
            hash(3)
        )
    );
    let all = render_list(&read, &none, 1_000, now, true);
    assert!(all.contains(&format!(
        "{}  expired  d1.example.org/a  {}\n",
        ids[0],
        hash(1)
    )));
    assert!(render_list(&read, &none, 3, now, false).contains("WARNING onyx-pointer-cap-alert"));
    assert_eq!(
        render_list(&[], &none, 1_000, now, false),
        "0 pointer(s) waiting, 0 expired and kept, 0 dropped; the journal holds 0 of 1000 entries\n"
    );
    // A missing journal reads as empty (the route was never on).
    assert!(read_pointers(&r.dir.join("none.journal"), 1_000)
        .expect("missing")
        .is_empty());
    // One writer: a second store on the same journal is refused while the first runs.
    assert!(PointerStore::open(journal(&r.dir), wide()).is_err());
    let (dir, clock) = (r.dir.clone(), r.now.clone());
    drop(r);
    let r = rig_at(dir, wide(), clock);
    assert_eq!(r.store.recorded(), 3);
    let (s, v) = status_of(&r, &ids[1]).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("received"))
    );
    let (s, v) = post(&r, "203.0.113.5", "d2.example.org/a", &hash(2)).await;
    assert_eq!(
        (s, v["id"].as_str()),
        (StatusCode::OK, Some(ids[1].as_str()))
    );
}

/// A damaged journal, and a line this code did not write, refuse the open and the read: a
/// reviewer is never shown as a pointer what the node did not check.
#[tokio::test]
async fn a_damaged_or_foreign_journal_is_refused_whole() {
    let r = rig("pointer-damage", wide());
    for n in 1..=2u8 {
        post(&r, "198.51.100.1", &format!("d{n}.example.org"), &hash(n)).await;
    }
    let path = journal(&r.dir);
    drop(r);
    let good = std::fs::read_to_string(&path).expect("journal");
    // One flipped character inside the first entry: its checksum no longer fits.
    let flipped = good.replacen("d1.example.org", "d9.example.org", 1);
    std::fs::write(&path, &flipped).expect("write");
    assert!(PointerStore::open(&path, wide()).is_err());
    assert!(read_pointers(&path, 1_000).is_err());
    // A torn last line is a write that was never acknowledged: left out, the rest stands.
    std::fs::write(&path, &good[..good.len() - 20]).expect("write");
    assert_eq!(read_pointers(&path, 1_000).expect("torn").len(), 1);
    // Entries with a good checksum that are not pointers as this code writes them.
    let framed = |v: &Value| {
        let body = v.to_string();
        format!(
            "{} {body}\n",
            hex::encode(&onyx_core::keccak256(body.as_bytes()).0[..16])
        )
    };
    let header = good.lines().next().expect("header");
    let id = format!("ptr_{}", "ab".repeat(16));
    let entry = |origin: &str, application: &str, extra: Option<(&str, Value)>| {
        let mut v = json!({"kind": "pointer", "version": 1, "id": id, "origin": origin,
            "application": application, "receivedUnix": T0});
        if let Some((k, x)) = extra {
            v[k] = x;
        }
        v
    };
    let ok = entry(ORIGIN, &hash(1), None);
    let cases = [
        (
            "an origin with a scheme",
            entry("https://data.example.org", &hash(1), None),
        ),
        (
            "an origin not in its one spelling",
            entry("Data.example.org", &hash(1), None),
        ),
        ("a shared host", entry("someone.github.io", &hash(1), None)),
        ("a hash out of shape", entry(ORIGIN, "0x12", None)),
        (
            "an unknown field",
            entry(ORIGIN, &hash(1), Some(("client", json!("198.51.100.7")))),
        ),
        (
            "another kind",
            entry(ORIGIN, &hash(1), Some(("kind", json!("note")))),
        ),
        (
            "another version",
            entry(ORIGIN, &hash(1), Some(("version", json!(2)))),
        ),
        (
            "an id out of shape",
            entry(ORIGIN, &hash(1), Some(("id", json!("sub_1")))),
        ),
        (
            "a time past 2^53",
            entry(ORIGIN, &hash(1), Some(("receivedUnix", json!(1u64 << 60)))),
        ),
    ];
    for (why, v) in &cases {
        let file = format!(
            "{header}\n{}",
            framed(&json!({"ops": [{"add": 0, "v": v}]}))
        );
        std::fs::write(&path, file).expect("write");
        assert!(read_pointers(&path, 1_000).is_err(), "{why}");
        assert!(PointerStore::open(&path, wide()).is_err(), "{why}");
    }
    // The same id, or the same origin and hash, twice.
    let mut other = ok.clone();
    other["origin"] = "other.example.org".into();
    let mut twin = ok.clone();
    twin["id"] = format!("ptr_{}", "cd".repeat(16)).into();
    for (why, second) in [("an id twice", other), ("a pointer twice", twin)] {
        let file = format!(
            "{header}\n{}",
            framed(&json!({"ops": [{"add": 0, "v": ok}, {"add": 1, "v": second}]}))
        );
        std::fs::write(&path, file).expect("write");
        assert!(read_pointers(&path, 1_000).is_err(), "{why}");
    }
    // The good entry alone is taken; a journal of another kind is not.
    let file = format!(
        "{header}\n{}",
        framed(&json!({"ops": [{"add": 0, "v": ok}]}))
    );
    std::fs::write(&path, &file).expect("write");
    assert_eq!(read_pointers(&path, 1_000).expect("good").len(), 1);
    std::fs::write(&path, file.replace(JOURNAL_KIND, "onyx-submissions")).expect("write");
    assert!(read_pointers(&path, 1_000).is_err());
    // Never through a link under the journal's name.
    #[cfg(unix)]
    {
        let real = path.with_extension("real");
        std::fs::write(&real, &file).expect("write");
        std::fs::remove_file(&path).expect("remove");
        std::os::unix::fs::symlink(&real, &path).expect("link");
        assert!(read_pointers(&path, 1_000).is_err());
        assert!(PointerStore::open(&path, wide()).is_err());
    }
}

/// A journal that cannot be written any more answers 503 and stays closed; nothing is
/// acknowledged that is not on disk.
#[tokio::test]
async fn a_failed_write_closes_the_journal() {
    let r = rig("pointer-broken", wide());
    let (s, first) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    assert_eq!(s, StatusCode::CREATED);
    r.store
        .ledger
        .lock()
        .expect("ledger")
        .journal
        .as_mut()
        .expect("journal")
        .break_for_test();
    for _ in 0..2 {
        let (s, h, v) = call(
            &r.app,
            "POST",
            "/v1/publishers/pointer",
            "198.51.100.1",
            body("other.example.org", &hash(2)),
        )
        .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::SERVICE_UNAVAILABLE, Some("pointer_unavailable"))
        );
        assert!(h.contains_key("retry-after"));
    }
    assert_eq!(r.store.recorded(), 1);
    // What was acknowledged before is still answered.
    let (s, v) = status_of(&r, first["id"].as_str().expect("id")).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("received"))
    );
}

// ---- review PTR-R1 ----

/// A request as a browser or a tool sends it: `headers` as given, nothing added.
async fn raw(
    r: &Rig,
    client: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/publishers/pointer");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut req = req.body(Body::from(body)).expect("request");
    req.extensions_mut().insert(ClientKey(client.to_string()));
    let resp = r.app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    crate::openapi_check::check("POST", "/v1/publishers/pointer", status.as_u16(), &v);
    (status, v)
}

/// Review PTR-R1 M1 (THREATS PTR-R1-M1): a page of another site cannot make a visitor's
/// browser send a pointer. A request a browser may send to another site without a preflight
/// (a form's or `text/plain` body, or none) is refused, so is one the browser itself marks as
/// cross-site or whose `Origin` is not the host it was sent to; nothing is written and no
/// budget of the visitor's address is spent. A tool, and the site's own form, are taken.
#[tokio::test]
async fn a_cross_site_or_non_json_post_is_refused_and_costs_no_budget() {
    let r = rig(
        "pointer-cross-site",
        PointerLimits {
            post_per_minute: 2,
            post_per_day: 2,
            ..PointerLimits::default()
        },
    );
    let b = || body("victim.example.org", &hash(1));
    let json = ("content-type", "application/json");
    let visitor = "192.0.2.77";
    // The review's attack: a simple request from another site.
    let (s, v) = raw(
        &r,
        visitor,
        &[
            ("content-type", "text/plain;charset=UTF-8"),
            ("origin", "https://evil.example"),
            ("sec-fetch-site", "cross-site"),
            ("sec-fetch-mode", "no-cors"),
        ],
        b(),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, Some("cross_site_refused"))
    );
    let cross: [&[(&str, &str)]; 6] = [
        &[json, ("sec-fetch-site", "cross-site")],
        &[json, ("sec-fetch-site", "Cross-Site")],
        &[json, ("origin", "https://evil.example")],
        &[
            json,
            ("origin", "https://evil.example"),
            ("host", "node.example.org"),
        ],
        &[json, ("origin", "null"), ("host", "node.example.org")],
        &[
            json,
            ("origin", "https://node.example.org.evil.example"),
            ("host", "node.example.org"),
        ],
    ];
    for headers in cross {
        let (s, v) = raw(&r, visitor, headers, b()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, Some("cross_site_refused")),
            "{headers:?}"
        );
    }
    // Not declared as JSON: the types a page may send anywhere without a preflight, and none.
    let not_json: [&[(&str, &str)]; 6] = [
        &[],
        &[("content-type", "text/plain")],
        &[("content-type", "application/x-www-form-urlencoded")],
        &[("content-type", "multipart/form-data; boundary=x")],
        &[("content-type", "application/jsonx")],
        &[
            ("content-type", "text/plain"),
            ("sec-fetch-site", "same-origin"),
        ],
    ];
    for headers in not_json {
        let (s, v) = raw(&r, visitor, headers, b()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, Some("json_required")),
            "{headers:?}"
        );
    }
    assert_eq!(r.store.recorded(), 0, "nothing was written");
    // Thirteen refusals spent nothing of the visitor's two a minute and two a day.
    let own: [&[(&str, &str)]; 2] = [
        // A tool: JSON, no browser header.
        &[("content-type", "application/json; charset=utf-8")],
        // The site's own form: same origin, the Origin is the host.
        &[
            json,
            ("origin", "https://Node.example.org"),
            ("host", "node.example.org"),
            ("sec-fetch-site", "same-origin"),
        ],
    ];
    for (n, headers) in own.iter().enumerate() {
        let (s, v) = raw(
            &r,
            visitor,
            headers,
            body("victim.example.org", &hash(n as u8)),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{headers:?}: {v}");
    }
    assert_eq!(r.store.recorded(), 2);
}

/// Review PTR-R1 M2 (THREATS PTR-R1-M2): one sender can fill a host's three places, and
/// again when they expire. The reviewer's release: a pointer whose hash is not the file's is
/// dropped with the tool, beside the running node; the place is free at once, the receipt
/// says dropped with its reason, and the same wrong pointer sent again is that receipt and
/// takes no place.
#[tokio::test]
async fn the_reviewer_drops_a_mismatched_pointer_and_its_place_is_free() {
    let r = rig("pointer-drop", wide());
    let path = journal(&r.dir);
    let mut wrong = Vec::new();
    for n in 1..=3 {
        let (s, v) = post(&r, "203.0.113.5", ORIGIN, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED);
        wrong.push(v["id"].as_str().expect("id").to_string());
    }
    let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(9)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, Some("pointer_origin_busy"))
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("reviewer drops") && m.contains("public issue")),
        "{v}"
    );
    // The reviewer drops one, beside the running node: the pointer journal is not written.
    let before = std::fs::read(&path).expect("bytes");
    let at = T0 + 3_600;
    assert_eq!(
        drop_pointer(&path, 1_000, &wrong[0], at).expect("drop"),
        Dropped::Now
    );
    assert_eq!(
        drop_pointer(&path, 1_000, &wrong[0], at).expect("again"),
        Dropped::Already
    );
    assert_eq!(std::fs::read(&path).expect("bytes"), before);
    assert_eq!(
        read_drops(&path).expect("drops"),
        Drops::from([wrong[0].clone()])
    );
    let (s, v) = status_of(&r, &wrong[0]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"id": wrong[0], "status": "dropped", "reason": "application_hash_mismatch",
            "expiresUnix": T0 + 6 * DAY, "decision": "see_status_list"})
    );
    // The real pointer now gets the place.
    let (s, v) = post(&r, "198.51.100.1", ORIGIN, &hash(9)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // The dropped pointer sent again is its receipt, not a new pointer and not a place.
    let (s, v) = post(&r, "203.0.113.5", ORIGIN, &hash(1)).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("dropped")));
    assert_eq!(r.store.recorded(), 4);
    // The list: three wait, one is dropped and shown only with --all.
    let (pointers, drops) = (
        read_pointers(&path, 1_000).expect("read"),
        read_drops(&path).expect("drops"),
    );
    let list = render_list(&pointers, &drops, 1_000, at, false);
    assert!(
        list.starts_with("3 pointer(s) waiting, 0 expired and kept, 1 dropped;"),
        "{list}"
    );
    assert!(!list.contains(&wrong[0]), "{list}");
    let all = render_list(&pointers, &drops, 1_000, at, true);
    assert!(
        all.contains(&format!("{}  dropped  {ORIGIN}  {}\n", wrong[0], hash(1))),
        "{all}"
    );
    // The review's lockout: six days on the sender fills the host again, and again the
    // reviewer's drops free it (another address stands for the sender's new day).
    r.now.store(T0 + 6 * DAY, Ordering::SeqCst);
    let mut again = Vec::new();
    for n in 4..=6 {
        let (s, v) = post(&r, "203.0.113.6", ORIGIN, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        again.push(v["id"].as_str().expect("id").to_string());
    }
    let (s, v) = post(&r, "198.51.100.2", ORIGIN, &hash(10)).await;
    assert_eq!(code(&v), Some("pointer_origin_busy"), "{s}");
    for id in &again {
        assert_eq!(
            drop_pointer(&path, 1_000, id, T0 + 6 * DAY).expect("drop"),
            Dropped::Now
        );
    }
    let (s, v) = post(&r, "198.51.100.2", ORIGIN, &hash(10)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // Only a waiting pointer is dropped: not an expired one, not an unknown id.
    for id in [wrong[1].as_str(), "ptr_00000000000000000000000000000000"] {
        assert!(matches!(
            drop_pointer(&path, 1_000, id, T0 + 6 * DAY),
            Err(DropError::NotWaiting)
        ));
    }
    // Once the node has removed a pointer, its drop leaves the drops journal with the next one.
    r.now.store(T0 + 12 * DAY, Ordering::SeqCst);
    assert_eq!(
        r.store.prune().expect("prune"),
        4,
        "the four of the first day"
    );
    let (_, v) = post(&r, "203.0.113.7", "late.example.org", &hash(20)).await;
    let late = v["id"].as_str().expect("id").to_string();
    assert_eq!(
        drop_pointer(&path, 1_000, &late, T0 + 12 * DAY).expect("drop"),
        Dropped::Now
    );
    let held = read_drops(&path).expect("drops");
    assert!(
        !held.contains(&wrong[0]) && held.contains(&late) && held.len() == 4,
        "{held:?}"
    );
}

/// A drops journal the node cannot read keeps the drops it read last and says so once, with
/// the alert's tag; a drop of an id the node does not hold changes nothing.
#[tokio::test]
async fn a_damaged_drops_journal_keeps_the_last_good_drops() {
    let r = rig("pointer-drops-damaged", wide());
    let path = journal(&r.dir);
    let mut ids = Vec::new();
    for n in 1..=2 {
        let (_, v) = post(&r, "203.0.113.5", ORIGIN, &hash(n)).await;
        ids.push(v["id"].as_str().expect("id").to_string());
    }
    drop_pointer(&path, 1_000, &ids[0], T0).expect("drop");
    assert_eq!(status_of(&r, &ids[0]).await.1["status"], "dropped");
    // Damage: a line that is not a framed journal line, in the middle of the file.
    let drops = drops_path(&path);
    assert_eq!(drops, r.dir.join("publisher-pointers.drops.journal"));
    let good = std::fs::read_to_string(&drops).expect("drops");
    std::fs::write(&drops, format!("{good}not a journal line\nand another\n")).expect("write");
    assert_eq!(
        status_of(&r, &ids[0]).await.1["status"],
        "dropped",
        "the last good read stands"
    );
    assert_eq!(status_of(&r, &ids[1]).await.1["status"], "received");
    status_of(&r, &ids[1]).await;
    let alerts = r.alerts.lock().expect("alerts").clone();
    assert_eq!(alerts.len(), 1, "once per change of the file: {alerts:?}");
    assert!(
        alerts[0].starts_with("onyx-pointer-drops-alert: "),
        "{alerts:?}"
    );
    assert!(!alerts[0].contains("example.org"), "counts only");
    assert!(read_drops(&path).is_err());
    assert!(matches!(
        drop_pointer(&path, 1_000, &ids[1], T0),
        Err(DropError::Storage(_))
    ));
    // An entry out of shape (another reason) is refused whole; a link is never followed.
    let framed = |v: &Value| {
        let body = v.to_string();
        format!(
            "{} {body}\n",
            hex::encode(&onyx_core::keccak256(body.as_bytes()).0[..16])
        )
    };
    let header = good.lines().next().expect("header");
    let entry =
        json!({"kind": "drop", "version": 1, "id": ids[1], "reason": "spam", "droppedUnix": T0});
    std::fs::write(
        &drops,
        format!(
            "{header}\n{}",
            framed(&json!({"ops": [{"add": 0, "v": entry}]}))
        ),
    )
    .expect("write");
    assert!(read_drops(&path).is_err());
    assert_eq!(status_of(&r, &ids[1]).await.1["status"], "received");
    // Removed: the node goes back to no drop at all.
    std::fs::remove_file(&drops).expect("remove");
    assert_eq!(status_of(&r, &ids[0]).await.1["status"], "received");
    #[cfg(unix)]
    {
        let real = r.dir.join("elsewhere");
        std::fs::write(&real, &good).expect("write");
        std::os::unix::fs::symlink(&real, &drops).expect("link");
        assert!(read_drops(&path).is_err());
        assert!(matches!(
            drop_pointer(&path, 1_000, &ids[1], T0),
            Err(DropError::Storage(_))
        ));
        assert_eq!(
            status_of(&r, &ids[0]).await.1["status"],
            "received",
            "not read through a link"
        );
    }
}

/// Review PTR-R1 L1 (THREATS PTR-R1-L1): path variants of one host do not each hold three.
#[tokio::test]
async fn path_variants_of_one_host_share_its_three_places() {
    let r = rig("pointer-paths", wide());
    let mut n = 0u8;
    let mut taken = 0;
    for origin in [
        "victim.example.org",
        "victim.example.org/a",
        "victim.example.org/a/b",
        "victim.example.org/c",
    ] {
        for _ in 0..3 {
            n += 1;
            let (s, v) = post(&r, "203.0.113.5", origin, &hash(n)).await;
            if s == StatusCode::CREATED {
                taken += 1;
            } else {
                assert_eq!(code(&v), Some("pointer_origin_busy"), "{origin}");
            }
        }
    }
    assert_eq!(taken, 3);
    assert_eq!(r.store.recorded(), 3);
}

/// Review PTR-R1 L2 (THREATS PTR-R1-L2): a node that takes no pointer still removes an entry
/// past its kept time: the hourly task does, with no POST at all. And before any removal ran,
/// the reviewer's list no longer shows such an entry.
#[tokio::test(start_paused = true)]
async fn a_quiet_node_removes_an_entry_after_twelve_days_with_no_post() {
    let r = rig("pointer-quiet", wide());
    let path = journal(&r.dir);
    post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    let task = tokio::spawn(run_prune(r.store.clone(), PRUNE_EVERY));
    let settle = || async {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    };
    // Eleven days on: the hour's run removes nothing.
    r.now.store(T0 + 11 * DAY, Ordering::SeqCst);
    tokio::time::advance(PRUNE_EVERY).await;
    settle().await;
    assert_eq!(read_pointers(&path, 1_000).expect("read").len(), 1);
    // Forty days on, still no pointer and no restart. Reads and refused posts remove nothing,
    // and the list already hides the entry.
    r.now.store(T0 + 40 * DAY, Ordering::SeqCst);
    let (s, _) = post(&r, "198.51.100.1", "not an origin", &hash(1)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let kept = read_pointers(&path, 1_000).expect("read");
    assert_eq!(kept.len(), 1);
    let shown = render_list(&kept, &Drops::new(), 1_000, T0 + 40 * DAY, true);
    assert!(
        !shown.contains(ORIGIN) && !shown.contains(&hash(1)),
        "{shown}"
    );
    assert!(
        shown.starts_with("0 pointer(s) waiting, 0 expired and kept, 0 dropped;"),
        "{shown}"
    );
    // The next hour's run removes it from the journal.
    tokio::time::advance(PRUNE_EVERY).await;
    let gone = async {
        while r.store.recorded() != 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(600), gone)
        .await
        .expect("the hourly task removed the entry");
    assert!(read_pointers(&path, 1_000).expect("read").is_empty());
    // Review PTR-R2 L1: gone from the file's bytes too, not only marked as removed.
    let file = std::fs::read_to_string(&path).expect("journal");
    assert!(
        !file.contains(ORIGIN) && !file.contains(&hash(1)[2..]),
        "{file}"
    );
    task.abort();
}

/// The reviewer's read beside a node whose journal is being rewritten: a removal of many
/// entries compacts the file (a new file renamed into place). Every read during it is whole:
/// the entries before the removal or the entries after it, never an error or a part.
#[test]
fn the_reviewers_read_holds_while_a_compaction_renames_the_journal() {
    use crate::journal::COMPACT_AFTER;
    let dir = test_dir("pointer-compaction");
    let path = journal(&dir);
    let old = COMPACT_AFTER + 40;
    let (new, cap) = (5, 2_000);
    {
        let (mut j, _) = Journal::open(&path, JOURNAL_KIND, &BINDING).expect("journal");
        let entries: Vec<Value> = (0..old + new)
            .map(|n| {
                Pointer {
                    id: format!("ptr_{n:032x}"),
                    origin: format!("h{n}.example.org"),
                    application: hash((n % 250) as u8),
                    received_unix: if n < old { T0 } else { T0 + 11 * DAY },
                }
                .entry()
            })
            .collect();
        for chunk in entries.chunks(100) {
            j.apply(&[], chunk).expect("append");
        }
    }
    let now = Arc::new(AtomicU64::new(T0 + 11 * DAY));
    let clock = now.clone();
    let limits = PointerLimits {
        max_recorded: cap,
        ..wide()
    };
    let store = PointerStore::open_at(
        &path,
        limits,
        Arc::new(move || clock.load(Ordering::SeqCst)),
    )
    .expect("open");
    assert_eq!(store.recorded(), old + new);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (path, stop) = (path.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut seen = std::collections::BTreeSet::new();
            let mut reads = 0u32;
            while !stop.load(Ordering::SeqCst) || reads < 20 {
                let got = read_pointers(&path, cap).expect("a read beside the writer is whole");
                seen.insert(got.len());
                reads += 1;
            }
            (seen, reads)
        })
    };
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        || std::fs::metadata(&path).expect("meta").ino()
    };
    #[cfg(unix)]
    let before = inode();
    now.store(T0 + 12 * DAY, Ordering::SeqCst);
    assert_eq!(store.prune().expect("prune"), old);
    #[cfg(unix)]
    assert_ne!(
        inode(),
        before,
        "the removal compacted: a new file under the journal's name"
    );
    stop.store(true, Ordering::SeqCst);
    let (seen, reads) = reader.join().expect("reader");
    assert!(reads >= 20);
    assert!(
        seen.iter().all(|n| *n == old + new || *n == new),
        "only whole states were read: {seen:?}"
    );
    assert_eq!(read_pointers(&path, cap).expect("after").len(), new);
    assert_eq!(store.recorded(), new);
}

/// Review PTR-R1 L4 (THREATS PTR-R1-L4): a pointer the node's own budget refuses costs the
/// client nothing of its day, whether the node's day or the node's minute refused it.
#[tokio::test]
async fn a_pointer_the_nodes_budget_refuses_costs_the_client_no_day_unit() {
    for (limits, want) in [
        (
            PointerLimits {
                post_per_day: 1,
                global_post_per_day: 1,
                ..wide()
            },
            "daily_quota",
        ),
        (
            PointerLimits {
                post_per_day: 1,
                global_post_per_minute: 1,
                ..wide()
            },
            "rate_limited",
        ),
    ] {
        let r = rig("pointer-refund", limits);
        let (s, _) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
        assert_eq!(s, StatusCode::CREATED);
        // The node's budget is spent: another client's new pointer is refused, twice.
        for _ in 0..2 {
            let (s, v) = post(&r, "203.0.113.5", "other.example.org", &hash(2)).await;
            assert_eq!((s, code(&v)), (StatusCode::TOO_MANY_REQUESTS, Some(want)));
        }
        // That client's one unit of the day is still there: a repeat, which needs it and
        // not the node's budget, is answered. The second repeat finds it spent.
        let (s, v) = post(&r, "203.0.113.5", ORIGIN, &hash(1)).await;
        assert_eq!(s, StatusCode::OK, "{want}: {v}");
        let (s, v) = post(&r, "203.0.113.5", ORIGIN, &hash(1)).await;
        assert_eq!(code(&v), Some("daily_quota"), "{want}: {s}");
        assert_eq!(r.store.recorded(), 1);
    }
}

// ---- review PTR-R2 ----

/// Review PTR-R2 L1 (THREATS PTR-R2-L1): a removed pointer is gone from the journal's bytes.
/// A removal alone is a `del` line, and the line that added the entry (origin and hash) would
/// stay until 1,024 removals had piled up; the file is written anew after every removal, by
/// the timed removal as by a start.
#[tokio::test]
async fn a_removed_pointer_is_gone_from_the_journal_file() {
    let r = rig("r2-retention", wide());
    post(&r, "198.51.100.1", ORIGIN, &hash(7)).await;
    post(&r, "198.51.100.1", "kept.example.org", &hash(8)).await;
    let on_disk = |dir: &std::path::Path| {
        let mut all = String::new();
        for e in std::fs::read_dir(dir).expect("dir") {
            let bytes = std::fs::read(e.expect("entry").path()).expect("file");
            all.push_str(&String::from_utf8_lossy(&bytes));
        }
        all
    };
    assert!(on_disk(&r.dir).contains(ORIGIN));
    // The second pointer is younger: it stays, the first leaves.
    r.now.store(T0 + 5 * DAY, Ordering::SeqCst);
    post(&r, "198.51.100.1", "young.example.org", &hash(9)).await;
    r.now.store(T0 + 12 * DAY, Ordering::SeqCst);
    assert_eq!(r.store.prune().expect("prune"), 2);
    assert_eq!(r.store.recorded(), 1);
    let all = on_disk(&r.dir);
    for gone in [ORIGIN, "kept.example.org", &hash(7)[2..], &hash(8)[2..]] {
        assert!(
            !all.contains(gone),
            "{gone} is still in a file of the state directory: {all}"
        );
    }
    assert!(
        all.contains("young.example.org") && !all.contains("\"del\""),
        "{all}"
    );
    assert_eq!(
        read_pointers(&journal(&r.dir), 1_000).expect("read").len(),
        1
    );
    // The journal still takes pointers after the rewrite, and a start removes as cleanly.
    let (s, _) = post(&r, "198.51.100.1", "later.example.org", &hash(10)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (dir, now) = (r.dir.clone(), r.now.clone());
    drop(r);
    now.store(T0 + 40 * DAY, Ordering::SeqCst);
    let again = rig_at(dir, wide(), now);
    assert_eq!(again.store.recorded(), 0);
    let all = on_disk(&again.dir);
    assert!(
        !all.contains("example.org") && !all.contains(&hash(9)[2..]),
        "after a start: {all}"
    );
}

/// Review PTR-R2 L2 (THREATS PTR-R2-L2): the same-site rule compares the `Origin` with the
/// `Host` the node sees, without case, without a trailing dot and without the scheme's
/// default port, so a proxy that appends `:443` does not get the site's own form refused. A
/// proxy that replaces the host is refused (the config docs say to pass `Host` on);
/// `X-Forwarded-Host` is not believed, and `Origin: null` names no host.
#[tokio::test]
async fn the_sites_own_form_passes_in_every_spelling_of_its_host() {
    let r = rig("r2-host", wide());
    let json = ("content-type", "application/json");
    let same = ("sec-fetch-site", "same-origin");
    let ok: [&[(&str, &str)]; 11] = [
        &[
            json,
            same,
            ("origin", "http://127.0.0.1:5173"),
            ("host", "127.0.0.1:5173"),
        ],
        &[
            json,
            same,
            ("origin", "http://[::1]:8080"),
            ("host", "[::1]:8080"),
        ],
        &[
            json,
            same,
            ("origin", "https://[::1]"),
            ("host", "[::1]:443"),
        ],
        &[
            json,
            same,
            ("origin", "https://node.example.org."),
            ("host", "node.example.org."),
        ],
        &[
            json,
            same,
            ("origin", "https://node.example.org."),
            ("host", "node.example.org"),
        ],
        &[
            json,
            same,
            ("origin", "https://NODE.example.org"),
            ("host", "node.Example.org"),
        ],
        // A balancer that appends the scheme's own port, or a browser that names it.
        &[
            json,
            same,
            ("origin", "https://node.example.org"),
            ("host", "node.example.org:443"),
        ],
        &[
            json,
            same,
            ("origin", "https://node.example.org:443"),
            ("host", "node.example.org"),
        ],
        &[
            json,
            same,
            ("origin", "http://node.example.org"),
            ("host", "node.example.org:80"),
        ],
        &[
            json,
            same,
            ("origin", "https://node.example.org:8443"),
            ("host", "node.example.org:8443"),
        ],
        &[json, ("sec-fetch-site", "none")],
    ];
    for (n, headers) in ok.iter().enumerate() {
        let (s, v) = raw(
            &r,
            "192.0.2.1",
            headers,
            body(&format!("ok{n}.example.org"), &hash(n as u8)),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{headers:?}: {v}");
    }
    let refused: [&[(&str, &str)]; 6] = [
        // A proxy that replaced the host with its upstream's: the forwarded one is not read.
        &[
            json,
            same,
            ("origin", "https://node.example.org"),
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "node.example.org"),
        ],
        // Another scheme's default port is not this one's.
        &[
            json,
            same,
            ("origin", "https://node.example.org"),
            ("host", "node.example.org:80"),
        ],
        &[
            json,
            same,
            ("origin", "http://node.example.org"),
            ("host", "node.example.org:443"),
        ],
        &[
            json,
            same,
            ("origin", "https://node.example.org"),
            ("host", "node.example.org:8443"),
        ],
        // A sandboxed frame: `Origin: null`.
        &[json, same, ("origin", "null"), ("host", "node.example.org")],
        &[
            json,
            same,
            ("origin", "https://node.example.org"),
            ("host", "node.example.org.evil.example"),
        ],
    ];
    for headers in refused {
        let (s, v) = raw(&r, "192.0.2.1", headers, body("no.example.org", &hash(99))).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, Some("cross_site_refused")),
            "{headers:?}"
        );
    }
    assert_eq!(r.store.recorded(), 11);
}

/// Review PTR-R2: what a page of another site could try against the rule. All refused and
/// nothing written; what passes is a tool, or the same host.
#[tokio::test]
async fn a_page_of_another_site_finds_no_way_around_the_rule() {
    let r = rig("r2-bypass", wide());
    let host = ("host", "node.example.org");
    let json = ("content-type", "application/json");
    let refused: [(&[(&str, &str)], StatusCode); 11] = [
        // A sibling subdomain: same-site for the browser, but the Origin is not the host.
        (
            &[
                json,
                ("sec-fetch-site", "same-site"),
                ("origin", "https://evil.example.org"),
                host,
            ],
            StatusCode::FORBIDDEN,
        ),
        // Another port of the same host.
        (
            &[json, ("origin", "https://node.example.org:8443"), host],
            StatusCode::FORBIDDEN,
        ),
        (
            &[
                json,
                ("origin", "https://node.example.org@evil.example"),
                host,
            ],
            StatusCode::FORBIDDEN,
        ),
        (
            &[
                json,
                ("origin", "https://evil.example/node.example.org"),
                host,
            ],
            StatusCode::FORBIDDEN,
        ),
        (
            &[json, ("origin", "https://"), ("host", "")],
            StatusCode::FORBIDDEN,
        ),
        (
            &[json, ("origin", "https://."), ("host", ".")],
            StatusCode::FORBIDDEN,
        ),
        (
            &[json, ("origin", "node.example.org"), host],
            StatusCode::FORBIDDEN,
        ),
        (
            &[("content-type", "text/plain;application/json")],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            &[("content-type", "text/plain, application/json")],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            &[
                ("content-type", "text/plain"),
                ("content-type", "application/json"),
            ],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            &[("content-type", "application/json+x")],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
    ];
    for (headers, want) in refused {
        let (s, _) = raw(
            &r,
            "192.0.2.2",
            headers,
            body("victim.example.org", &hash(1)),
        )
        .await;
        assert_eq!(s, want, "{headers:?}");
    }
    assert_eq!(r.store.recorded(), 0);
    // Taken: a tool (no browser header; any case and a charset), and a same-site request
    // without an Origin, which no browser sends for a POST.
    let passes: [&[(&str, &str)]; 2] = [
        &[json, ("sec-fetch-site", "same-site")],
        &[("content-type", "APPLICATION/JSON ; charset=utf-8")],
    ];
    for (n, headers) in passes.iter().enumerate() {
        let (s, v) = raw(
            &r,
            "192.0.2.2",
            headers,
            body("victim.example.org", &hash(n as u8)),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{headers:?}: {v}");
    }
}

/// Review PTR-R2 L3 (THREATS PTR-R2-L3): a drop made by mistake is taken back. The receipt
/// then answers by its age again, the pointer holds its place again, and nothing of the
/// taken-back drop stays in the drops journal.
#[tokio::test]
async fn a_mistaken_drop_is_taken_back() {
    let r = rig("r2-undrop", wide());
    let path = journal(&r.dir);
    let (_, v) = post(&r, "198.51.100.1", ORIGIN, &hash(9)).await;
    let id = v["id"].as_str().expect("id").to_string();
    // No drop yet: nothing to take back, and no file is made for the question.
    assert_eq!(
        undrop_pointer(&path, 1_000, &id, r.now.load(Ordering::SeqCst)).expect("none"),
        Undropped::NotDropped
    );
    assert!(!drops_path(&path).exists());
    assert_eq!(
        drop_pointer(&path, 1_000, &id, T0 + 60).expect("drop"),
        Dropped::Now
    );
    let (s, v) = post(&r, "203.0.113.9", ORIGIN, &hash(9)).await;
    assert_eq!((s, v["status"].as_str()), (StatusCode::OK, Some("dropped")));
    // The place was free meanwhile: two others took places of the host.
    for n in 1..=2 {
        let (s, _) = post(&r, "203.0.113.9", ORIGIN, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let before = std::fs::read(&path).expect("bytes");
    assert_eq!(
        undrop_pointer(&path, 1_000, &id, r.now.load(Ordering::SeqCst)).expect("undrop"),
        Undropped::Now
    );
    assert_eq!(
        undrop_pointer(&path, 1_000, &id, r.now.load(Ordering::SeqCst)).expect("again"),
        Undropped::NotDropped
    );
    assert_eq!(
        std::fs::read(&path).expect("bytes"),
        before,
        "the pointer journal is not written"
    );
    assert!(read_drops(&path).expect("drops").is_empty());
    let drops = std::fs::read_to_string(drops_path(&path)).expect("drops file");
    assert!(
        !drops.contains(&id),
        "the taken-back drop is gone from the file: {drops}"
    );
    // The node sees it at its next look: the receipt is `received` again, from any client.
    let (s, v) = status_of(&r, &id).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("received"))
    );
    assert!(v.get("reason").is_none(), "{v}");
    let (s, v) = post(&r, "203.0.113.9", ORIGIN, &hash(9)).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("received"))
    );
    // It holds its place again: with the two others the host is full.
    let (s, v) = post(&r, "203.0.113.9", ORIGIN, &hash(3)).await;
    assert_eq!(code(&v), Some("pointer_origin_busy"), "{s}");
    // Taken back after its six days, it answers expired, as it would have.
    assert_eq!(
        drop_pointer(&path, 1_000, &id, T0 + 120).expect("drop"),
        Dropped::Now
    );
    r.now.store(T0 + 7 * DAY, Ordering::SeqCst);
    assert_eq!(status_of(&r, &id).await.1["status"], "dropped");
    assert_eq!(
        undrop_pointer(&path, 1_000, &id, r.now.load(Ordering::SeqCst)).expect("undrop"),
        Undropped::Now
    );
    assert_eq!(status_of(&r, &id).await.1["status"], "expired");
    // A drops journal that cannot be read is an error, never "not dropped".
    std::fs::write(drops_path(&path), "not a journal\nat all\n").expect("write");
    assert!(matches!(
        undrop_pointer(&path, 1_000, &id, r.now.load(Ordering::SeqCst)),
        Err(DropError::Storage(_))
    ));
}

/// Review PTR-R2: a refund never hands out budget. It gives back only what the same client
/// took today: none for a client that took nothing, none after a take that was refused.
#[test]
fn a_refund_gives_back_only_what_was_taken() {
    let b = Budgets::new();
    b.refund_day("pointer-day", "192.0.2.1", true);
    for n in 0..2 {
        b.take_day("pointer-day", &format!("192.0.2.{n}"), 5, Some(2), "x")
            .expect("take");
    }
    // The node's two of the day are spent; a refused take is not refunded into a third.
    assert!(b
        .take_day("pointer-day", "192.0.2.9", 5, Some(2), "x")
        .is_err());
    b.refund_day("pointer-day", "192.0.2.9", true);
    b.refund_day("pointer-day", "192.0.2.9", true);
    assert!(b
        .take_day("pointer-day", "192.0.2.9", 5, Some(2), "x")
        .is_err());
    // A real refund gives back exactly one, once.
    b.refund_day("pointer-day", "192.0.2.0", true);
    b.refund_day("pointer-day", "192.0.2.0", true);
    assert!(b
        .take_day("pointer-day", "192.0.2.9", 5, Some(2), "x")
        .is_ok());
    assert!(b
        .take_day("pointer-day", "192.0.2.8", 5, Some(2), "x")
        .is_err());
    // An IPv6 client: all three of its keys, or nothing.
    let v6 = "2001:db8:1:1::/64";
    b.refund_day("v6-day", v6, false);
    b.take_day("v6-day", v6, 1, None, "x").expect("take");
    b.refund_day("v6-day", v6, false);
    assert!(
        b.take_day("v6-day", "2001:db8:1:2::/64", 1, None, "x")
            .is_ok(),
        "the /48's unit came back"
    );
}

// ---- review PTR-R3 ----

/// Review PTR-R3 L1 (THREATS PTR-R3-L1): a journal that still holds a removed entry is
/// written anew at the start, also when nothing is left to expire: after a crash or a failed
/// rewrite between the `del` line and the rewrite, and for a journal from before the rewrite
/// existed. The drops journal is treated the same by the tool that writes it.
#[tokio::test]
async fn a_start_rewrites_a_journal_that_still_holds_a_removed_entry() {
    let dir = test_dir("r3-reopen");
    let path = journal(&dir);
    let entry = |n: u8, origin: &str| {
        Pointer {
            id: format!("ptr_{}", hex::encode([n; 16])),
            origin: origin.to_string(),
            application: hash(n),
            received_unix: T0,
        }
        .entry()
    };
    // As a crash leaves it: two entries, one removed by a `del` line, no rewrite.
    {
        let (mut j, _) = Journal::open(&path, JOURNAL_KIND, &BINDING).expect("journal");
        let ids = j
            .apply(
                &[],
                &[
                    entry(1, "removed.example.org"),
                    entry(2, "kept.example.org"),
                ],
            )
            .expect("add");
        j.apply(&ids[..1], &[]).expect("del");
        assert_eq!(j.dead(), 1);
    }
    let before = std::fs::read_to_string(&path).expect("journal");
    assert!(before.contains("removed.example.org") && before.contains("\"del\""));
    // A start at a time when nothing is expired.
    let r = rig_at(dir.clone(), wide(), Arc::new(AtomicU64::new(T0 + 60)));
    assert_eq!(r.store.recorded(), 1);
    let after = std::fs::read_to_string(&path).expect("journal");
    assert!(
        !after.contains("removed.example.org")
            && !after.contains(&hash(1)[2..])
            && !after.contains("\"del\""),
        "the removed entry is gone from the file after a start: {after}"
    );
    assert!(after.contains("kept.example.org"));
    // A journal without a removed entry is not rewritten by a start (the same file).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inode = std::fs::metadata(&path).expect("meta").ino();
        let (d, now) = (r.dir.clone(), r.now.clone());
        drop(r);
        let r = rig_at(d, wide(), now);
        assert_eq!(std::fs::metadata(&path).expect("meta").ino(), inode);
        drop(r);
    }
    // The drops journal: a drop taken out by a `del` line that was never rewritten leaves
    // with the tool's next write, and also when the tool finds nothing to take back.
    let r = rig_at(dir, wide(), Arc::new(AtomicU64::new(T0 + 60)));
    let kept = format!("ptr_{}", hex::encode([2u8; 16]));
    let old = format!("ptr_{}", hex::encode([7u8; 16]));
    let drops = drops_path(&path);
    let plant = || {
        let _ = std::fs::remove_file(&drops);
        let (mut j, _) = Journal::open(&drops, DROPS_KIND, &BINDING).expect("drops");
        let v = json!({"kind": "drop", "version": 1, "id": old, "reason": DROPPED_REASON, "droppedUnix": T0});
        let ids = j.apply(&[], &[v]).expect("add");
        j.apply(&ids, &[]).expect("del");
    };
    plant();
    assert!(std::fs::read_to_string(&drops)
        .expect("drops")
        .contains(&old));
    assert_eq!(
        undrop_pointer(&path, 1_000, &kept, T0 + 60).expect("nothing to take back"),
        Undropped::NotDropped
    );
    assert!(!std::fs::read_to_string(&drops)
        .expect("drops")
        .contains(&old));
    plant();
    assert_eq!(
        drop_pointer(&path, 1_000, &kept, T0 + 60).expect("drop"),
        Dropped::Now
    );
    let file = std::fs::read_to_string(&drops).expect("drops");
    assert!(!file.contains(&old) && file.contains(&kept), "{file}");
    assert_eq!(status_of(&r, &kept).await.1["status"], "dropped");
}

/// Review PTR-R3 L2 (THREATS PTR-R3-L2): status reads and pointers do not share one pool. A
/// burst of status reads that fills the readers answers 503 to further status reads only; a
/// pointer is still taken. Writes that fill the workers do not stop a status read.
#[tokio::test]
async fn status_reads_never_take_a_place_from_a_pointer() {
    let r = rig("r3-pools", wide());
    let (_, v) = post(&r, "198.51.100.1", ORIGIN, &hash(1)).await;
    let id = v["id"].as_str().expect("id").to_string();
    // Every reader is busy.
    let readers: Vec<_> = (0..MAX_READERS)
        .map(|_| r.store.read_permit().expect("reader"))
        .collect();
    let (s, h, v) = call(
        &r.app,
        "GET",
        &format!("/v1/publishers/pointer/{id}"),
        "198.51.100.9",
        Vec::new(),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("pointer_busy"))
    );
    assert!(h.contains_key("retry-after"));
    let (s, v) = post(&r, "198.51.100.1", "other.example.org", &hash(2)).await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "a pointer is taken while the readers are full: {v}"
    );
    drop(readers);
    // Every writer is busy: a pointer is refused, a status read is answered.
    let writers: Vec<_> = (0..MAX_WORKERS)
        .map(|_| r.store.permit().expect("writer"))
        .collect();
    let (s, v) = post(&r, "198.51.100.1", "third.example.org", &hash(3)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("pointer_busy"))
    );
    let (s, v) = status_of(&r, &id).await;
    assert_eq!(
        (s, v["status"].as_str()),
        (StatusCode::OK, Some("received"))
    );
    drop(writers);
    assert_eq!(r.store.recorded(), 2);
}

/// Review PTR-R3 N3 (THREATS PTR-R3-N3): an undrop never puts a host over its three places.
/// When the place a drop freed was taken since, the undrop is refused and nothing is
/// written; once the reviewer has dropped one of the others it goes through.
#[tokio::test]
async fn an_undrop_is_refused_while_its_host_is_full_again() {
    let r = rig("r3-undrop-full", wide());
    let path = journal(&r.dir);
    let (_, v) = post(&r, "198.51.100.1", ORIGIN, &hash(9)).await;
    let id = v["id"].as_str().expect("id").to_string();
    assert_eq!(
        drop_pointer(&path, 1_000, &id, T0).expect("drop"),
        Dropped::Now
    );
    let mut others = Vec::new();
    for (n, origin) in [
        (1, ORIGIN),
        (2, "data.example.org"),
        (3, "data.example.org/other"),
    ] {
        let (s, v) = post(&r, "203.0.113.5", origin, &hash(n)).await;
        assert_eq!(s, StatusCode::CREATED);
        others.push(v["id"].as_str().expect("id").to_string());
    }
    let drops = std::fs::read(drops_path(&path)).expect("drops");
    let e = undrop_pointer(&path, 1_000, &id, T0).expect_err("the host is full");
    assert!(matches!(e, DropError::HostFull), "{e}");
    assert!(
        e.to_string().contains("already holds 3 waiting pointers"),
        "{e}"
    );
    assert_eq!(
        std::fs::read(drops_path(&path)).expect("drops"),
        drops,
        "nothing written"
    );
    assert_eq!(status_of(&r, &id).await.1["status"], "dropped");
    // A dropped one of the others does not count: then the undrop is taken.
    assert_eq!(
        drop_pointer(&path, 1_000, &others[0], T0).expect("drop"),
        Dropped::Now
    );
    assert_eq!(
        undrop_pointer(&path, 1_000, &id, T0).expect("undrop"),
        Undropped::Now
    );
    assert_eq!(status_of(&r, &id).await.1["status"], "received");
    let (s, v) = post(&r, "203.0.113.5", ORIGIN, &hash(4)).await;
    assert_eq!(
        code(&v),
        Some("pointer_origin_busy"),
        "three wait again, not four: {s}"
    );
    // A pointer on another host, and one that has expired, are taken back whatever the host holds.
    r.now.store(T0 + 7 * DAY, Ordering::SeqCst);
    assert_eq!(
        undrop_pointer(&path, 1_000, &others[0], T0 + 7 * DAY).expect("expired"),
        Undropped::Now
    );
}

/// Review PTR-R3 N4 (THREATS PTR-R3-N4): the scheme of the `Origin` is not compared. The node
/// does not know which scheme its site is served under; a page on the http side of an https
/// site is marked cross-site by the browser itself, and that mark is refused.
#[tokio::test]
async fn the_scheme_is_not_compared_and_the_browsers_own_mark_is() {
    let r = rig("r3-scheme", wide());
    let json = ("content-type", "application/json");
    let host = ("host", "node.example.org");
    let (s, v) = raw(
        &r,
        "192.0.2.3",
        &[json, ("origin", "http://node.example.org"), host],
        body("a.example.org", &hash(1)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "accepted as documented: {v}");
    // What a browser sends from http://node.example.org to the https site: schemelessly
    // same-site is not same-site; it says cross-site, and that is refused.
    let (s, v) = raw(
        &r,
        "192.0.2.3",
        &[
            json,
            ("origin", "http://node.example.org"),
            host,
            ("sec-fetch-site", "cross-site"),
        ],
        body("b.example.org", &hash(2)),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, Some("cross_site_refused"))
    );
    assert_eq!(r.store.recorded(), 1);
}
