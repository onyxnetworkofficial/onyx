//! Usage counts (docs/spec/usage-counts.md; THREATS USE-1..USE-7 and review
//! USE-R1). The clock is injected: no test waits for a real day or minute.

use super::*;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Extension;
use http_body_util::BodyExt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use tower::ServiceExt;

/// 2026-11-16 00:00:00 UTC.
const DAY0: u64 = 20_773 * 86_400;

static N: AtomicU64 = AtomicU64::new(0);

struct Rig {
    dir: PathBuf,
    now: Arc<AtomicU64>,
    usage: Arc<Usage>,
}

impl Rig {
    fn new() -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "onyx-usage-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let now = Arc::new(AtomicU64::new(DAY0 + 3_600));
        let usage = Rig::open(&dir, &now);
        Rig { dir, now, usage }
    }

    fn open(dir: &Path, now: &Arc<AtomicU64>) -> Arc<Usage> {
        let c = now.clone();
        Arc::new(Usage::new(
            dir.join("usage"),
            Arc::new(move || c.load(Ordering::Relaxed)),
        ))
    }

    /// A restart: fresh counters over the same directory.
    fn restart(&mut self) {
        self.usage = Rig::open(&self.dir, &self.now);
    }

    fn file(&self, day: &str) -> PathBuf {
        self.dir.join("usage").join(format!("{day}.json"))
    }

    fn stored(&self, day: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.file(day)).expect("day file"))
            .expect("json")
    }

    /// What the operator's reader shows for the `days` ending today.
    fn report(&self, days: u64) -> Value {
        read_report(
            &self.dir.join("usage"),
            self.now.load(Ordering::Relaxed) / 86_400,
            days,
        )
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.join("usage"))
            .expect("dir")
            .map(|e| e.expect("entry").file_name().into_string().expect("name"))
            .collect();
        names.sort();
        names
    }

    fn hit(&self, dataset: Option<(&str, &str)>, status: StatusCode) {
        self.usage.record(
            &Method::GET,
            Some("/v1/bulk/{publisher}/{dataset}/rows"),
            status,
            dataset.map(|(p, d)| DatasetUse {
                publisher: p.into(),
                dataset: d.into(),
            }),
        );
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ds(d: &str) -> Option<(&'static str, &str)> {
    Some(("pub.one", d))
}

async fn send(app: &Router, req: axum::http::Request<Body>) -> (StatusCode, Value) {
    let r = app.clone().oneshot(req).await.expect("answer");
    let status = r.status();
    assert!(
        r.extensions().get::<DatasetUse>().is_none(),
        "the dataset mark must not leave the counting layer"
    );
    let bytes = r.into_body().collect().await.expect("body").to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn req(method: &str, uri: &str) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request")
}

/// A small router with the shapes the node has: a plain route, one with
/// path parameters, one that fails, and a fallback.
fn app(usage: Arc<Usage>) -> Router {
    counted(usage, plain())
}

fn plain() -> Router {
    Router::new()
        .route("/v1/health", get(|| async { "ok" }))
        .route(
            "/v1/things/{a}/{b}",
            get(|| async { StatusCode::NOT_FOUND }).post(|| async { StatusCode::CREATED }),
        )
        .route(
            "/v1/broken",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/v1/moved",
            get(|| async { StatusCode::PERMANENT_REDIRECT }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
}

#[tokio::test]
async fn requests_are_counted_by_route_template_and_status_class() {
    let rig = Rig::new();
    let app = app(rig.usage.clone());
    for _ in 0..3 {
        send(&app, req("GET", "/v1/health?probe=secret-query")).await;
    }
    send(&app, req("HEAD", "/v1/health")).await;
    // Two different paths, one template.
    send(&app, req("GET", "/v1/things/alpha/beta")).await;
    send(&app, req("GET", "/v1/things/gamma/delta?x=1")).await;
    send(&app, req("POST", "/v1/things/alpha/beta")).await;
    send(&app, req("GET", "/v1/broken")).await;
    send(&app, req("GET", "/v1/moved")).await;
    // No route: the path the client chose is not a key.
    send(&app, req("GET", "/v1/attacker-chosen-path-1")).await;
    send(&app, req("GET", "/wp-login.php")).await;
    send(&app, req("PROPFIND", "/x")).await;
    // Review USE-R1 L8: every 405 is one key, whatever the method and route.
    for (method, uri) in [
        ("POST", "/v1/health"),
        ("PUT", "/v1/health"),
        ("DELETE", "/v1/broken"),
        ("PATCH", "/v1/things/a/b"),
        ("OPTIONS", "/v1/moved"),
    ] {
        let (s, _) = send(&app, req(method, uri)).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
    }
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(
        v["routes"],
        json!({
            "GET /v1/health": {"2xx": 3, "3xx": 0, "4xx": 0, "5xx": 0},
            "HEAD /v1/health": {"2xx": 1, "3xx": 0, "4xx": 0, "5xx": 0},
            "GET /v1/things/{a}/{b}": {"2xx": 0, "3xx": 0, "4xx": 2, "5xx": 0},
            "POST /v1/things/{a}/{b}": {"2xx": 1, "3xx": 0, "4xx": 0, "5xx": 0},
            "GET /v1/broken": {"2xx": 0, "3xx": 0, "4xx": 0, "5xx": 1},
            "GET /v1/moved": {"2xx": 0, "3xx": 1, "4xx": 0, "5xx": 0},
            "GET (no route)": {"2xx": 0, "3xx": 0, "4xx": 2, "5xx": 0},
            "OTHER (no route)": {"2xx": 0, "3xx": 0, "4xx": 1, "5xx": 0},
            "(method not allowed)": {"2xx": 0, "3xx": 0, "4xx": 5, "5xx": 0},
        })
    );
    assert_eq!(v["datasets"], json!({}));
}

/// THREATS USE-1: the stored file has these keys and no others, and nothing
/// a client sent (address, headers, query, path values) is in its bytes.
#[tokio::test]
async fn the_stored_file_holds_counts_and_nothing_of_a_request() {
    let rig = Rig::new();
    let named = Router::new().route(
        "/v1/bulk/{publisher}/{dataset}/rows",
        get(|| async {
            let mut r = "rows".into_response();
            r.extensions_mut().insert(DatasetUse {
                publisher: "pub.one".into(),
                dataset: "set.a".into(),
            });
            r
        }),
    );
    let app = counted(rig.usage.clone(), plain().merge(named));
    let peer: SocketAddr = "203.0.113.77:54321".parse().expect("addr");
    let request = axum::http::Request::get(
        "/v1/bulk/pub.one/set.a/rows?from=424242&to=434343&marker=QUERYMARK",
    )
    .header("user-agent", "AgentMark/9.9")
    .header("referer", "https://referrer-mark.example/page")
    .header("cookie", "session=COOKIEMARK")
    .header("x-forwarded-for", "198.51.100.23")
    .header("x-request-id", "REQUESTIDMARK")
    .extension(ConnectInfo(peer))
    .body(Body::empty())
    .expect("request");
    assert_eq!(send(&app, request).await.0, StatusCode::OK);
    send(&app, req("GET", "/v1/PATHMARK/secret")).await;
    rig.usage.flush().expect("flush");

    let text = std::fs::read_to_string(rig.file("2026-11-16")).expect("file");
    for mark in [
        "203.0.113",
        "54321",
        "198.51.100",
        "AgentMark",
        "referrer-mark",
        "COOKIEMARK",
        "REQUESTIDMARK",
        "QUERYMARK",
        "424242",
        "434343",
        "PATHMARK",
        "secret",
        "from",
    ] {
        assert!(!text.contains(mark), "{mark} is in the stored file: {text}");
    }
    let v: Value = serde_json::from_str(&text).expect("json");
    let keys = |v: &Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().expect("object").keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&v),
        [
            "datasets",
            "datasets_other",
            "day",
            "routes",
            "small_folded",
            "version"
        ]
    );
    assert_eq!(v["version"], json!(1));
    assert_eq!(v["day"], json!("2026-11-16"));
    assert_eq!(v["small_folded"], json!(false));
    assert_eq!(v["datasets"], json!({"pub.one/set.a": 1}));
    assert_eq!(v["datasets_other"], json!(0));
    for (route, classes) in v["routes"].as_object().expect("routes") {
        assert_eq!(keys(classes), ["2xx", "3xx", "4xx", "5xx"], "{route}");
        assert!(classes.as_object().expect("o").values().all(Value::is_u64));
    }
    // The only other file is nothing: one file per day, no log beside it.
    assert_eq!(rig.names(), ["2026-11-16.json"]);
    // For the node's user only (review USE-R1 M2: the files are the one
    // place the counts are read from).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&rig.file("2026-11-16")), 0o600);
        assert_eq!(mode(&rig.dir.join("usage")), 0o700);
    }
    // A field this code does not know is refused when read back: the
    // schema cannot grow in the file without growing in `DayFile`.
    let mut extra = v.clone();
    extra["client"] = json!("203.0.113.77");
    assert!(serde_json::from_value::<DayFile>(extra).is_err());
    assert!(serde_json::from_value::<DayFile>(v).is_ok());
}

/// THREATS USE-3: at most 2,000 dataset keys a day; the rest are "other".
#[test]
fn the_dataset_cap_sends_the_rest_to_other() {
    let rig = Rig::new();
    for i in 0..MAX_DATASETS_PER_DAY + 50 {
        rig.hit(ds(&format!("set{i}")), StatusCode::OK);
    }
    // A dataset already counted keeps counting past the cap.
    rig.hit(ds("set0"), StatusCode::OK);
    // A refused request names no dataset use.
    rig.hit(ds("set1"), StatusCode::TOO_MANY_REQUESTS);
    rig.hit(ds("set-new"), StatusCode::NOT_FOUND);
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    let sets = v["datasets"].as_object().expect("datasets");
    assert_eq!(sets.len(), MAX_DATASETS_PER_DAY);
    assert_eq!(sets["pub.one/set0"], json!(2));
    assert_eq!(sets["pub.one/set1"], json!(1));
    assert!(!sets.contains_key("pub.one/set2000"));
    assert_eq!(v["datasets_other"], json!(50));
    let all = &v["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"];
    assert_eq!(all["2xx"], json!(MAX_DATASETS_PER_DAY + 51));
    assert_eq!(all["4xx"], json!(2));
    // The file stays small at the cap.
    let len = std::fs::metadata(rig.file("2026-11-16"))
        .expect("meta")
        .len();
    assert!(len < MAX_FILE_BYTES / 2, "{len} bytes");
}

/// The route keys are bounded too (a mistake in the router must not grow
/// the map without end).
#[test]
fn the_route_cap_sends_the_rest_to_other() {
    let rig = Rig::new();
    for i in 0..MAX_ROUTES_PER_DAY + 7 {
        rig.usage.record(
            &Method::GET,
            Some(&format!("/v1/t{i}")),
            StatusCode::OK,
            None,
        );
    }
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(
        v["routes"].as_object().expect("routes").len(),
        MAX_ROUTES_PER_DAY + 1
    );
    assert_eq!(v["routes"][OTHER_ROUTE]["2xx"], json!(7));
}

/// Review USE-R1 L8: 8 methods on each template would be more keys than
/// the cap; 405 answers use one key, so real routes keep their own.
#[test]
fn method_not_allowed_answers_cannot_use_up_the_route_keys() {
    let rig = Rig::new();
    let methods = [
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
        Method::TRACE,
    ];
    for i in 0..60 {
        for m in &methods {
            rig.usage.record(
                m,
                Some(&format!("/v1/t{i}")),
                StatusCode::METHOD_NOT_ALLOWED,
                None,
            );
        }
    }
    rig.usage
        .record(&Method::GET, Some("/v1/real"), StatusCode::OK, None);
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(
        v["routes"],
        json!({
            "(method not allowed)": {"2xx": 0, "3xx": 0, "4xx": 360, "5xx": 0},
            "GET /v1/real": {"2xx": 1, "3xx": 0, "4xx": 0, "5xx": 0},
        })
    );
}

/// THREATS USE-3: a dataset becomes a key only when the bulk store holds
/// it. A name the client made up is counted on the route, never by name.
#[tokio::test]
async fn only_datasets_the_node_holds_become_keys() {
    let w = crate::bulk_serve::tests::world();
    crate::bulk_serve::tests::published(&w).await;
    let rig = Rig::new();
    let app = counted(
        rig.usage.clone(),
        crate::bulk_serve::routes(w.store.clone()),
    );
    let held = "/v1/bulk/example.publisher/synthetic.metrics";
    assert_eq!(send(&app, req("GET", held)).await.0, StatusCode::OK);
    let (s, _) = send(&app, req("GET", &format!("{held}/rows?from=3&to=4"))).await;
    assert_eq!(s, StatusCode::OK);
    // A bad query on a held dataset: counted on the route, not as an open.
    let (s, _) = send(&app, req("GET", &format!("{held}/rows?limit=0"))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    for unknown in [
        "/v1/bulk/example.publisher/made.up.by.a.client",
        "/v1/bulk/nobody.here/synthetic.metrics/rows",
        "/v1/bulk/Example.Publisher/Synthetic.Metrics",
        "/v1/bulk/example.publisher/synthetic.metrics%2Fx/rows",
    ] {
        let (s, _) = send(&app, req("GET", unknown)).await;
        assert!(s.is_client_error(), "{unknown}: {s}");
    }
    for i in 0..300 {
        let uri = format!("/v1/bulk/example.publisher/flood{i}/rows");
        assert_eq!(send(&app, req("GET", &uri)).await.0, StatusCode::NOT_FOUND);
    }
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(
        v["datasets"],
        json!({"example.publisher/synthetic.metrics": 2})
    );
    assert_eq!(v["datasets_other"], json!(0));
    assert_eq!(
        v["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"],
        json!({"2xx": 1, "3xx": 0, "4xx": 303, "5xx": 0})
    );
    assert_eq!(v["routes"].as_object().expect("routes").len(), 2);
    assert!(w.store.usage_of("example.publisher", "flood1").is_none());
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// Review USE-R1 L2: the dataset number counts opens (the info route, the
/// first page of rows), not requests: one reader paging through a dataset,
/// or asking for its successor, does not pass the small-count line alone.
#[tokio::test]
async fn a_dataset_number_counts_opens_not_pages() {
    let w = crate::bulk_serve::tests::world();
    crate::bulk_serve::tests::published(&w).await;
    let rig = Rig::new();
    let app = counted(
        rig.usage.clone(),
        crate::bulk_serve::routes(w.store.clone()),
    );
    let held = "/v1/bulk/example.publisher/synthetic.metrics";
    // One reader: the dataset page, then all 8 pages of 4 rows, newest
    // first too, then the successor link.
    assert_eq!(send(&app, req("GET", held)).await.0, StatusCode::OK);
    for order in ["asc", "desc"] {
        for page in 0..8 {
            let uri = format!("{held}/rows?limit=4&page={page}&order={order}");
            assert_eq!(
                send(&app, req("GET", &uri)).await.0,
                StatusCode::OK,
                "{uri}"
            );
        }
    }
    send(&app, req("GET", &format!("{held}/successor"))).await;
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    // 1 info + 2 first pages; the 14 later pages are requests, not opens.
    assert_eq!(
        v["datasets"],
        json!({"example.publisher/synthetic.metrics": 3})
    );
    assert_eq!(
        v["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(16)
    );
    assert_eq!(
        rig.report(1)["totals"]["datasets"],
        json!({}),
        "one reader's visit stays under the small count"
    );
    let _ = std::fs::remove_dir_all(&w.dir);
}

/// The day is the injected clock's UTC day. A finished day is written once
/// more with its small counts folded, then leaves the memory.
#[test]
fn a_new_utc_day_starts_new_counts_and_folds_the_finished_day() {
    let rig = Rig::new();
    for _ in 0..SMALL_COUNT {
        rig.hit(ds("busy"), StatusCode::OK);
    }
    for quiet in ["quiet1", "quiet2", "quiet3"] {
        for _ in 0..SMALL_COUNT - 1 {
            rig.hit(ds(quiet), StatusCode::OK);
        }
    }
    // The last second of the day still counts for it.
    rig.now.store(DAY0 + 86_399, Ordering::Relaxed);
    rig.hit(ds("busy"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(
        v["datasets"],
        json!({"pub.one/busy": 6, "pub.one/quiet1": 4, "pub.one/quiet2": 4, "pub.one/quiet3": 4})
    );
    assert_eq!(v["small_folded"], json!(false));

    rig.now.store(DAY0 + 86_400, Ordering::Relaxed);
    rig.hit(ds("busy"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    let today = rig.stored("2026-11-17");
    assert_eq!(today["datasets"], json!({"pub.one/busy": 1}));
    assert_eq!(today["small_folded"], json!(false));
    assert_eq!(
        today["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(1)
    );
    let done = rig.stored("2026-11-16");
    assert_eq!(done["datasets"], json!({"pub.one/busy": 6}));
    assert_eq!(done["datasets_other"], json!(12));
    assert_eq!(done["small_folded"], json!(true));
    assert_eq!(
        done["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(18)
    );
    // Only today is still in memory.
    let days: Vec<u64> = lock(&rig.usage.inner).days.keys().copied().collect();
    assert_eq!(days, [DAY0 / 86_400 + 1]);
    // A day that ended with nothing new since its last write is folded
    // too, at the first write of the next day (no request needed).
    rig.usage.flush().expect("flush");
    assert_eq!(rig.stored("2026-11-17")["small_folded"], json!(false));
    rig.now.store(DAY0 + 2 * 86_400 + 5, Ordering::Relaxed);
    rig.usage.flush().expect("flush");
    let done = rig.stored("2026-11-17");
    assert_eq!(done["small_folded"], json!(true));
    assert_eq!(done["datasets"], json!({}));
    assert!(lock(&rig.usage.inner).days.is_empty());
    assert!(!rig.file("2026-11-18").exists());
}

/// Review USE-R1 L1: when only one or two datasets are under the small
/// count, "other" would be their own count under another name (the held
/// datasets are public, the named ones are in the report). Their opens are
/// left out instead; with three or more they are summed into "other".
#[test]
fn one_or_two_small_datasets_are_left_out_not_folded_into_other() {
    for (small, other) in [(1u64, 0u64), (2, 0), (3, 9), (4, 12)] {
        let rig = Rig::new();
        for _ in 0..9 {
            rig.hit(ds("busy"), StatusCode::OK);
        }
        for i in 0..small {
            for _ in 0..3 {
                rig.hit(ds(&format!("quiet{i}")), StatusCode::OK);
            }
        }
        rig.usage.flush().expect("flush");
        // The read of a day that is not over.
        let r = rig.report(1);
        assert_eq!(
            r["totals"]["datasets"],
            json!({"pub.one/busy": 9}),
            "{small}"
        );
        assert_eq!(r["totals"]["datasets_other"], json!(other), "{small}");
        assert_eq!(r["days"][0]["datasets_other"], json!(other), "{small}");
        // The finished day's file.
        rig.now.store(DAY0 + 86_400 + 1, Ordering::Relaxed);
        rig.usage.flush().expect("flush");
        let v = rig.stored("2026-11-16");
        assert_eq!(v["datasets"], json!({"pub.one/busy": 9}), "{small}");
        assert_eq!(v["datasets_other"], json!(other), "{small}");
        assert_eq!(v["small_folded"], json!(true));
    }
    // Opens past the daily cap of names stay in "other" whatever folds.
    let rig = Rig::new();
    for i in 0..MAX_DATASETS_PER_DAY + 4 {
        for _ in 0..SMALL_COUNT {
            rig.hit(ds(&format!("set{i}")), StatusCode::OK);
        }
    }
    rig.hit(ds("set0"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    assert_eq!(
        rig.report(1)["totals"]["datasets_other"],
        json!(4 * SMALL_COUNT)
    );
}

/// Review USE-R1 L3: a day the node did not finish (a deploy across
/// midnight UTC, a restart before the first write of the new day) is
/// folded by the daily sweep; it does not keep exact small counts for 90
/// days.
#[test]
fn an_unfinished_past_day_is_folded_by_the_daily_sweep() {
    let mut rig = Rig::new();
    for _ in 0..7 {
        rig.hit(ds("busy"), StatusCode::OK);
    }
    for quiet in ["q1", "q2", "q3"] {
        rig.hit(ds(quiet), StatusCode::OK);
    }
    rig.usage.flush().expect("flush");
    assert_eq!(rig.stored("2026-11-16")["small_folded"], json!(false));
    // The node stops on the 16th and starts on the 19th.
    rig.now.store(DAY0 + 3 * 86_400 + 30, Ordering::Relaxed);
    rig.restart();
    // The first write, with no request at all.
    rig.usage.flush().expect("flush");
    let v = rig.stored("2026-11-16");
    assert_eq!(v["small_folded"], json!(true));
    assert_eq!(v["datasets"], json!({"pub.one/busy": 7}));
    assert_eq!(v["datasets_other"], json!(3));
    assert_eq!(
        v["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(10)
    );
    assert_eq!(rig.names(), ["2026-11-16.json"]);
    // A folded file is left as it is by later sweeps (same bytes).
    let before = std::fs::read(rig.file("2026-11-16")).expect("file");
    rig.now.store(DAY0 + 4 * 86_400 + 30, Ordering::Relaxed);
    rig.usage.flush().expect("flush");
    assert_eq!(std::fs::read(rig.file("2026-11-16")).expect("file"), before);
    // A corrupt past file is set aside by the sweep and reported once.
    std::fs::write(rig.file("2026-11-17"), b"{").expect("write");
    rig.now.store(DAY0 + 5 * 86_400 + 30, Ordering::Relaxed);
    let err = rig.usage.flush().expect_err("reported");
    assert!(
        err.contains("set aside as 2026-11-17.json.corrupt"),
        "{err}"
    );
    rig.usage.flush().expect("nothing left to report");
    // Today's file is never folded by the sweep.
    rig.hit(ds("q1"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    rig.restart();
    rig.usage.flush().expect("flush");
    assert_eq!(rig.stored("2026-11-21")["small_folded"], json!(false));
    assert_eq!(
        rig.stored("2026-11-21")["datasets"],
        json!({"pub.one/q1": 1})
    );
}

/// Review USE-R1 L4: a clock that steps back into a finished day adds
/// counts to a file marked folded. The mark is never taken over for a live
/// day, and the day is folded again when it is over again.
#[test]
fn a_clock_stepping_back_never_leaves_a_live_day_marked_folded() {
    let mut rig = Rig::new();
    for _ in 0..6 {
        rig.hit(ds("busy"), StatusCode::OK);
    }
    rig.now.store(DAY0 + 86_400 + 10, Ordering::Relaxed);
    rig.usage.flush().expect("flush");
    assert_eq!(rig.stored("2026-11-16")["small_folded"], json!(true));
    // The clock steps back into the 16th (also across a restart) and three
    // datasets get one open each.
    for restart in [false, true] {
        rig.now.store(DAY0 + 80_000, Ordering::Relaxed);
        if restart {
            rig.restart();
        }
        for quiet in ["q1", "q2", "q3"] {
            rig.hit(ds(quiet), StatusCode::OK);
        }
        rig.usage.flush().expect("flush");
        let live = rig.stored("2026-11-16");
        assert_eq!(
            live["small_folded"],
            json!(false),
            "a live day with exact small counts must not say it is folded"
        );
        assert_eq!(live["datasets"]["pub.one/q1"], json!(1));
        // Over again: folded again, the small counts gone from the names.
        rig.now.store(DAY0 + 86_400 + 20, Ordering::Relaxed);
        rig.usage.flush().expect("flush");
        let done = rig.stored("2026-11-16");
        assert_eq!(done["small_folded"], json!(true));
        assert_eq!(done["datasets"], json!({"pub.one/busy": 6}));
    }
    assert_eq!(rig.stored("2026-11-16")["datasets_other"], json!(6));
}

/// Counts survive a restart: the day's file is added to the new counts at
/// the first write, also when requests arrived before it. The operator's
/// report reads the files only (review USE-R1 L5): what the node has not
/// written yet is not in it, so nothing can be counted twice.
#[test]
fn counts_survive_a_restart() {
    let mut rig = Rig::new();
    for _ in 0..7 {
        rig.hit(ds("a"), StatusCode::OK);
    }
    rig.hit(None, StatusCode::BAD_GATEWAY);
    rig.usage.flush().expect("flush");
    // No temporary file is left behind.
    assert_eq!(rig.names(), ["2026-11-16.json"]);
    rig.restart();
    assert_eq!(rig.report(1)["totals"]["datasets"], json!({"pub.one/a": 7}));
    for _ in 0..2 {
        rig.hit(ds("a"), StatusCode::OK);
    }
    for _ in 0..5 {
        rig.hit(ds("b"), StatusCode::OK);
    }
    // Not written yet: not in the report, and the report wrote nothing.
    let before = std::fs::read(rig.file("2026-11-16")).expect("file");
    let r = rig.report(1);
    assert_eq!(r["totals"]["datasets"], json!({"pub.one/a": 7}));
    assert_eq!(
        r["totals"]["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(7)
    );
    assert_eq!(std::fs::read(rig.file("2026-11-16")).expect("file"), before);
    assert_eq!(rig.names(), ["2026-11-16.json"]);
    rig.usage.flush().expect("flush");
    rig.usage.flush().expect("a second flush changes nothing");
    let v = rig.stored("2026-11-16");
    assert_eq!(v["datasets"], json!({"pub.one/a": 9, "pub.one/b": 5}));
    assert_eq!(
        v["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"],
        json!({"2xx": 14, "3xx": 0, "4xx": 0, "5xx": 1})
    );
    assert_eq!(
        rig.report(1)["totals"]["datasets"],
        json!({"pub.one/a": 9, "pub.one/b": 5})
    );
    // A restart with nothing new writes nothing and loses nothing.
    rig.restart();
    rig.usage.flush().expect("flush");
    assert_eq!(rig.stored("2026-11-16"), v);
}

/// A corrupt file is set aside and reported; counting goes on and the next
/// write is a clean file. Never a panic.
#[test]
fn a_corrupt_file_is_set_aside() {
    let wrong_day = serde_json::to_string(&DayFile::empty(1)).expect("json");
    let unknown_field = r#"{"version":1,"day":"2026-11-16","routes":{},"datasets":{},"datasets_other":0,"small_folded":false,"ip":"203.0.113.1"}"#;
    let bad_key = r#"{"version":1,"day":"2026-11-16","routes":{},"datasets":{"../../etc/passwd":1},"datasets_other":0,"small_folded":false}"#;
    let version = r#"{"version":2,"day":"2026-11-16","routes":{},"datasets":{},"datasets_other":0,"small_folded":false}"#;
    let huge = "x".repeat(usize::try_from(MAX_FILE_BYTES).expect("size") + 10);
    for bad in [
        "",
        "{",
        "not json at all \u{0}",
        "[1,2,3]",
        r#"{"version":1}"#,
        wrong_day.as_str(),
        unknown_field,
        bad_key,
        version,
        huge.as_str(),
    ] {
        let mut rig = Rig::new();
        for _ in 0..5 {
            rig.hit(ds("a"), StatusCode::OK);
        }
        rig.usage.flush().expect("flush");
        std::fs::write(rig.file("2026-11-16"), bad).expect("write");
        rig.restart();
        // The read skips the day, says so, and moves nothing.
        let r = rig.report(2);
        assert_eq!(r["unreadable_days"], json!(["2026-11-16"]));
        assert_eq!(r["totals"]["routes"], json!({}));
        assert_eq!(rig.names(), ["2026-11-16.json"]);
        for _ in 0..5 {
            rig.hit(ds("b"), StatusCode::OK);
        }
        let err = rig.usage.flush().expect_err("the corrupt file is reported");
        assert!(
            err.contains("set aside as 2026-11-16.json.corrupt"),
            "{err}"
        );
        let aside = rig.file("2026-11-16").with_extension("json.corrupt");
        let kept = std::fs::read(&aside).expect("set aside");
        assert_eq!(kept.len(), bad.len());
        assert_eq!(
            rig.stored("2026-11-16")["datasets"],
            json!({"pub.one/b": 5})
        );
        // Clean from here on.
        rig.hit(ds("b"), StatusCode::OK);
        rig.usage.flush().expect("flush");
        assert_eq!(
            rig.stored("2026-11-16")["datasets"],
            json!({"pub.one/b": 6})
        );
        assert_eq!(rig.report(1)["unreadable_days"], json!([]));
    }
}

/// A state directory that cannot be written: an error to log, the counters
/// keep working, and the counts are written once it can.
#[test]
fn a_failed_write_keeps_the_counts_for_the_next_one() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.dir).expect("dir");
    // A file where the directory should be.
    std::fs::write(rig.dir.join("usage"), b"in the way").expect("write");
    rig.hit(ds("a"), StatusCode::OK);
    assert!(rig.usage.flush().is_err());
    rig.hit(ds("a"), StatusCode::OK);
    std::fs::remove_file(rig.dir.join("usage")).expect("remove");
    rig.usage.flush().expect("flush");
    assert_eq!(
        rig.stored("2026-11-16")["datasets"],
        json!({"pub.one/a": 2})
    );
    // Days cannot pile up in memory while writes fail.
    std::fs::remove_dir_all(rig.dir.join("usage")).expect("remove");
    std::fs::write(rig.dir.join("usage"), b"in the way").expect("write");
    for d in 1..=10 {
        rig.now.store(DAY0 + d * 86_400, Ordering::Relaxed);
        rig.hit(ds("a"), StatusCode::OK);
        assert!(rig.usage.flush().is_err());
    }
    assert_eq!(lock(&rig.usage.inner).days.len(), MAX_DAYS_IN_MEMORY);
}

/// Files older than the retention are deleted; other files are left alone.
#[test]
fn days_older_than_the_retention_are_deleted() {
    let rig = Rig::new();
    rig.hit(ds("a"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    let other = rig.dir.join("usage").join("notes.txt");
    std::fs::write(&other, b"not ours").expect("write");
    std::fs::write(rig.dir.join("usage").join("2026-11-16.json.corrupt"), b"x").expect("write");
    // Day 89 after: still kept (90 days: today and the 89 before).
    rig.now
        .store(DAY0 + (RETENTION_DAYS - 1) * 86_400, Ordering::Relaxed);
    rig.hit(ds("a"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    assert!(rig.file("2026-11-16").exists());
    let r = rig.report(RETENTION_DAYS);
    assert_eq!(r["from"], json!("2026-11-16"));
    assert_eq!(r["days"].as_array().expect("days").len(), 2);
    // One day later it is gone, with its set-aside copy.
    rig.now
        .store(DAY0 + RETENTION_DAYS * 86_400, Ordering::Relaxed);
    rig.hit(ds("a"), StatusCode::OK);
    rig.usage.flush().expect("flush");
    assert_eq!(
        rig.names(),
        ["2027-02-13.json", "2027-02-14.json", "notes.txt"]
    );
}

/// THREATS USE-2: the report never names a dataset under 5 opens for a
/// day, also for today, whose file still holds the exact number.
#[test]
fn a_read_folds_small_dataset_counts_into_other() {
    let rig = Rig::new();
    for _ in 0..12 {
        rig.hit(ds("busy"), StatusCode::OK);
    }
    for (quiet, n) in [("quiet", 4), ("twice", 2), ("once", 1)] {
        for _ in 0..n {
            rig.hit(ds(quiet), StatusCode::OK);
        }
    }
    rig.usage.flush().expect("flush");
    rig.now.store(DAY0 + 86_400 + 60, Ordering::Relaxed);
    for (quiet, n) in [("quiet", 4), ("twice", 4), ("once", 4), ("busy", 5)] {
        for _ in 0..n {
            rig.hit(ds(quiet), StatusCode::OK);
        }
    }
    rig.usage.flush().expect("flush");
    // Today's file holds the exact numbers; the report does not show them.
    assert_eq!(
        rig.stored("2026-11-17")["datasets"]["pub.one/quiet"],
        json!(4)
    );
    let r = rig.report(7);
    assert_eq!(r["small_count"], json!(5));
    assert_eq!(r["from"], json!("2026-11-11"));
    assert_eq!(r["to"], json!("2026-11-17"));
    // "quiet" had 4 on each day: 8 in the week, still never named.
    assert_eq!(r["totals"]["datasets"], json!({"pub.one/busy": 17}));
    assert_eq!(r["totals"]["datasets_other"], json!(19));
    assert_eq!(
        r["totals"]["routes"]["GET /v1/bulk/{publisher}/{dataset}/rows"]["2xx"],
        json!(36)
    );
    let days = r["days"].as_array().expect("days");
    assert_eq!(days.len(), 2);
    assert_eq!(days[0]["day"], json!("2026-11-16"));
    assert_eq!(days[0]["datasets"], json!({"pub.one/busy": 12}));
    assert_eq!(days[0]["datasets_other"], json!(7));
    assert_eq!(days[1]["datasets"], json!({"pub.one/busy": 5}));
    assert_eq!(days[1]["datasets_other"], json!(12));
    // The report has these fields and no others.
    let mut keys: Vec<&String> = r.as_object().expect("object").keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "days",
            "from",
            "note",
            "small_count",
            "to",
            "totals",
            "unreadable_days",
            "version"
        ]
    );
    assert_eq!(rig.report(1)["days"].as_array().expect("d").len(), 1);
    // A folder that is not there is an empty report, not an error.
    let none = read_report(&rig.dir.join("nowhere"), DAY0 / 86_400, 7);
    assert_eq!(none["days"], json!([]));
    assert_eq!(none["unreadable_days"], json!([]));
}

/// Counting applies to every caller alike: a request from the host with no
/// forwarding header (the agent connector, the health watch) is counted
/// like one that came through the proxy, and neither is told apart in what
/// is kept.
#[tokio::test]
async fn local_and_proxied_requests_are_counted_alike() {
    let rig = Rig::new();
    let app = app(rig.usage.clone());
    let local: SocketAddr = "127.0.0.1:40000".parse().expect("addr");
    let direct = axum::http::Request::get("/v1/health")
        .extension(ConnectInfo(local))
        .body(Body::empty())
        .expect("request");
    let proxied = axum::http::Request::get("/v1/health")
        .header("x-forwarded-for", "203.0.113.5")
        .extension(ConnectInfo(local))
        .body(Body::empty())
        .expect("request");
    send(&app, direct).await;
    send(&app, proxied).await;
    let app2 = app.clone().layer(Extension(ConnectInfo(local)));
    send(&app2, req("GET", "/v1/health")).await;
    rig.usage.flush().expect("flush");
    assert_eq!(
        rig.stored("2026-11-16")["routes"],
        json!({"GET /v1/health": {"2xx": 3, "3xx": 0, "4xx": 0, "5xx": 0}})
    );
}
