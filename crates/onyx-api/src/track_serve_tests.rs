//! `/v1/track` (ROADMAP C6): only what was published, a stable 404 otherwise,
//! the rights fallback, the append-only store, `since` chains, limits, and the
//! routes behind the API's guard (THREATS TR-S-*).

use super::*;
use crate::catalog::{Catalog, LabelRights};
use crate::roots::LedgerRoots;
use crate::{router_with, ApiConfig, ApiState};
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use onyx_coordinator::cas::MemCas;
use onyx_core::{
    Confidence, DataRights, MetricInfo, StaticRegistry, Subject, Value as V, ValueKind,
};
use onyx_track::example::{self, AS_OF, AS_OF_EARLY, FORECASTER, GUESSER, METRIC};
use onyx_track::publication::Publication;
use onyx_track::wire::{
    check_history_page, check_log_body, check_record_body, complete_history, Expect, LogError,
    LogExpect, WireError, MAX_HISTORY,
};
use onyx_track::{compute, daily, sort_statements, Close, Fact, MetricRule};
use tower::ServiceExt;

async fn call(app: &Router, uri: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder().uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    // PP6: every answer these tests see conforms to docs/api/openapi.json.
    crate::openapi_check::check("GET", uri, status.as_u16(), &body);
    (status, headers, body)
}

fn code(b: &Value) -> Option<&str> {
    b["error"]["code"].as_str()
}

fn example_store(rights: LabelRights, as_of: u64) -> Arc<TrackStore> {
    let store = TrackStore::new(rights).allowing_examples();
    publish_example(&store, as_of).expect("publish");
    Arc::new(store)
}

fn publish_example(store: &TrackStore, as_of: u64) -> Result<TrackCommitment, PublishError> {
    let (s, r) = example::logs(as_of);
    store.publish(example::params(), &example::registry(), s, r, as_of)
}

fn record_uri(source: &str) -> String {
    format!("/v1/track/{source}/{METRIC}")
}

fn expect(source: &str) -> Expect<'_> {
    Expect {
        source,
        metric: METRIC,
        min_as_of: 0,
        known: None,
        latest: None,
    }
}

/// Nothing published (a node today): a stable 404 with a reason, never a record.
#[tokio::test]
async fn nothing_published_is_a_stable_404_and_an_empty_index() {
    // What a node mounts (app.rs) without ONYX_TRACK_PUBLICATIONS: no handle to any record.
    let app = node_routes(LabelRights::Cleared, None, std::env::temp_dir());
    let (s, h, b) = call(
        &app,
        "/v1/track/vendor.a/us.treasury.total_public_debt_outstanding",
        &[],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("no_track_record"))
    );
    assert_eq!(b["reason"], NONE_PUBLISHED);
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    let (s, _, b) = call(&app, "/v1/track", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["published"], false);
    assert_eq!(b["records"], json!([]));
    assert_eq!(b["reason"], NOTHING_PUBLISHED);
    assert!(b["reason"]
        .as_str()
        .is_some_and(|r| r.contains("close-time rule") && r.contains("Bitcoin")));
}

/// TR-S-7: a node's store never publishes the example (or any `example.*` id) as real.
#[test]
fn a_node_store_refuses_example_ids() {
    let store = TrackStore::new(LabelRights::Cleared);
    assert_eq!(
        publish_example(&store, AS_OF),
        Err(PublishError::Example(METRIC.into()))
    );
    assert!(store.latest().is_none());
}

/// The published example is served with proofs a client checks; unknown
/// records and unruled metrics are 404s with their reasons.
#[tokio::test]
async fn a_published_record_is_served_and_checks() {
    let app = routes(example_store(LabelRights::Cleared, AS_OF));
    for src in [FORECASTER, GUESSER] {
        let (s, h, b) = call(&app, &record_uri(src), &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(h[header::CACHE_CONTROL], "public, no-cache");
        let c = check_record_body(&b, &expect(src)).expect("checks");
        assert!(c.history_checked && c.example);
        assert_eq!(c.commitment.as_of, AS_OF);
        // Revalidation: 304 with the same tag, no body.
        let tag = h[header::ETAG].to_str().expect("etag").to_string();
        let (s, h2, b2) = call(&app, &record_uri(src), &[("if-none-match", &tag)]).await;
        assert_eq!((s, b2), (StatusCode::NOT_MODIFIED, Value::Null));
        assert_eq!(h2[header::ETAG], tag.as_str());
    }
    let (s, _, b) = call(&app, &record_uri("example.nobody"), &[]).await;
    assert_eq!(
        (s, code(&b), b["reason"].as_str()),
        (
            StatusCode::NOT_FOUND,
            Some("no_track_record"),
            Some(NO_RECORD)
        )
    );
    let (s, _, b) = call(&app, &format!("/v1/track/{FORECASTER}/example.other"), &[]).await;
    assert_eq!(
        (s, b["reason"].as_str()),
        (StatusCode::NOT_FOUND, Some(NO_RULE))
    );
    let (s, _, b) = call(&app, "/v1/track", &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["published"], true);
    assert_eq!(b["records"].as_array().map(Vec::len), Some(2));
    assert_eq!(b["records"][0]["example"], true);
    assert_eq!(b["rules"][0]["metric"], METRIC);
    assert_eq!(b["reason"], Value::Null);
}

/// The served bytes are the site's example file (apps/web/static/level5):
/// regenerate with `ONYX_BLESS=1 cargo test -p onyx-api track_serve`.
#[tokio::test]
async fn the_site_example_is_what_the_server_serves() {
    // Published as the example's chain: the early commitment, then the latest.
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    publish_example(&store, AS_OF_EARLY).expect("early");
    let latest = publish_example(&store, AS_OF).expect("latest");
    let [_, (chained, _)] = example::chain().expect("chain");
    assert_eq!(latest, chained);
    let app = routes(Arc::new(store));
    let mut records = serde_json::Map::new();
    for src in [FORECASTER, GUESSER] {
        let (s, _, b) = call(&app, &record_uri(src), &[]).await;
        assert_eq!(s, StatusCode::OK);
        records.insert(src.to_string(), b);
    }
    let doc = json!({
        "example": true,
        "note": "Example only: invented sources and values on the example level-3 metric (onyx_track::example). Not a real record.",
        "records": records,
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/web/static/level5/track-example.json"
    );
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(std::path::Path::new(path).parent().expect("dir")).expect("mkdir");
        std::fs::write(path, &text).expect("write");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("site example (bless with ONYX_BLESS=1)"),
        text
    );
}

fn labelled_registry() -> StaticRegistry {
    let mut r = StaticRegistry::new();
    r.add_metric(
        "test.labelled",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: true,
        },
    )
    .and_then(|r| r.add_aggregate("a.one"))
    .and_then(|r| r.add_source(DataRights::public_chain(SourceId("vendor.a".into()))))
    .expect("registry");
    r
}

fn labelled_statement() -> Statement {
    Statement {
        source: SourceId("vendor.a".into()),
        fact: Fact {
            metric: MetricId("test.labelled".into()),
            subject: Subject::Aggregate("a.one".into()),
            as_of: 1_000,
        },
        value: V::UInt(5),
        confidence: Confidence(900_000),
        committed_at: 900,
    }
}

/// ROADMAP L14 pattern: in fallback, a label-derived metric's records are 403
/// `label_rights_withheld` before any lookup (published or not, a record or
/// not), and left out of the index with a count. Cleared serves them.
#[tokio::test]
async fn fallback_withholds_label_derived_records() {
    let params = TrackParams::new(vec![MetricRule {
        metric: MetricId("test.labelled".into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }]);
    for rights in [LabelRights::Cleared, LabelRights::Fallback] {
        let store = Arc::new(TrackStore::new(rights));
        let app = routes(store.clone());
        let entity = "/v1/track/vendor.a/crypto.btc.entity_flow";
        let (s, _, b) = call(&app, entity, &[]).await;
        let fallback = rights == LabelRights::Fallback;
        assert_eq!(
            (s, code(&b)),
            if fallback {
                (StatusCode::FORBIDDEN, Some("label_rights_withheld"))
            } else {
                (StatusCode::NOT_FOUND, Some("no_track_record"))
            }
        );
        store
            .publish(
                params.clone(),
                &labelled_registry(),
                vec![labelled_statement()],
                vec![],
                2_000,
            )
            .expect("publish");
        for uri in [
            "/v1/track/vendor.a/test.labelled",
            "/v1/track/vendor.z/test.labelled",
        ] {
            let (s, _, b) = call(&app, uri, &[]).await;
            if fallback {
                assert_eq!(
                    (s, code(&b)),
                    (StatusCode::FORBIDDEN, Some("label_rights_withheld")),
                    "{uri}"
                );
            } else {
                assert_ne!(s, StatusCode::FORBIDDEN, "{uri}");
            }
        }
        let (_, _, b) = call(&app, "/v1/track", &[]).await;
        assert_eq!(b["withheld"], u64::from(fallback));
        assert_eq!(
            b["records"].as_array().map(Vec::len),
            Some(usize::from(!fallback))
        );
    }
    // The example metric is not label-derived: served in fallback too.
    let app = routes(example_store(LabelRights::Fallback, AS_OF));
    let (s, _, _) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(label_derived(
        &labelled_registry(),
        &MetricId("test.labelled".into())
    ));
    assert!(label_derived(
        &labelled_registry(),
        &MetricId("test.unregistered".into())
    ));
    assert!(!label_derived(
        &example::registry(),
        &MetricId(METRIC.into())
    ));
}

/// TR-S-1 (TR-R2-5 on the server): the store only moves forward. Publishing
/// an older computation, the same one again, or a log that drops a statement
/// is refused, and the latest stays served.
#[test]
fn the_store_never_goes_back() {
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    assert_eq!(c2.prev, c1.hash());
    assert_eq!(
        publish_example(&store, AS_OF_EARLY),
        Err(PublishError::NotAnExtension(ExtendError::TimeNotForward))
    );
    assert!(matches!(
        publish_example(&store, AS_OF),
        Err(PublishError::NotAnExtension(ExtendError::TimeNotForward))
    ));
    let (mut s, r) = example::logs(AS_OF + 1);
    s.remove(0);
    assert!(matches!(
        store.publish(example::params(), &example::registry(), s, r, AS_OF + 1),
        Err(PublishError::NotAnExtension(ExtendError::StatementDropped))
    ));
    assert_eq!(store.latest(), Some(c2));
}

/// `since`: the commitments after a known one, so a client can check that the
/// served one descends from what it saw; an unknown one is a 409.
#[tokio::test]
async fn since_serves_the_chain_from_a_known_commitment() {
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    let app = routes(Arc::new(store));
    let uri = format!("{}?since={}", record_uri(FORECASTER), c1.hash());
    let (s, _, b) = call(&app, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["chain"].as_array().map(Vec::len), Some(1));
    let known = Expect {
        known: Some(c1.hash()),
        min_as_of: AS_OF,
        ..expect(FORECASTER)
    };
    assert_eq!(
        check_record_body(&b, &known).map(|c| c.commitment).ok(),
        Some(c2.clone())
    );
    let (_, _, b) = call(
        &app,
        &format!("{}?since={}", record_uri(FORECASTER), c2.hash()),
        &[],
    )
    .await;
    assert_eq!(b["chain"], json!([]));
    let (_, _, plain) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(plain["chain"], Value::Null);
    assert_eq!(
        check_record_body(&plain, &known).err(),
        Some(WireError::NotDescendant)
    );
    let fork = Hash32([9; 32]);
    let (s, _, b) = call(
        &app,
        &format!("{}?since={fork}", record_uri(FORECASTER)),
        &[],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, Some("unknown_commitment"))
    );
    let upper = format!(
        "{}?since=0x{}",
        record_uri(FORECASTER),
        hex::encode_upper(c1.hash().0)
    );
    for bad in [
        upper,
        format!("{}?since={}&x=1", record_uri(FORECASTER), c1.hash()),
        format!("{}?x=1", record_uri(FORECASTER)),
        "/v1/track?x=1".to_string(),
        "/v1/track/Vendor.A/example.signed.reserve".to_string(),
        "/v1/track/vendor..a/example.signed.reserve".to_string(),
    ] {
        let (s, _, b) = call(&app, &bad, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, Some("bad_request")),
            "{bad}"
        );
    }
}

/// Free answers are limited per client; a 304 costs nothing.
#[tokio::test]
async fn free_answers_are_limited_per_client() {
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .with_free_per_minute(1);
    publish_example(&store, AS_OF).expect("publish");
    let app = routes(Arc::new(store));
    let (s, h, _) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let tag = h[header::ETAG].to_str().expect("etag").to_string();
    let (s, _, _) = call(&app, &record_uri(FORECASTER), &[("if-none-match", &tag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    let (s, h, b) = call(&app, &record_uri(GUESSER), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.contains_key("retry-after"));
}

/// The routes behind the full API: the guard runs (client key, deadline), the
/// API's own fallback still answers unknown routes.
#[tokio::test]
async fn the_routes_join_the_api_behind_its_guard() {
    let state = Arc::new(
        ApiState::new(
            Catalog::new(),
            Arc::new(LedgerRoots::new()),
            Arc::new(MemCas::new()),
            None,
            Arc::new(|| AS_OF),
            "http://127.0.0.1",
            ApiConfig::default(),
        )
        .expect("state"),
    );
    let app = router_with(state, routes(example_store(LabelRights::Cleared, AS_OF)));
    let (s, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(check_record_body(&b, &expect(FORECASTER)).is_ok());
    let (s, _, b) = call(&app, "/v1/nothing", &[]).await;
    assert_eq!((s, code(&b)), (StatusCode::NOT_FOUND, Some("not_found")));
    let (s, _, _) = call(&app, "/v1/health", &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// The example's chain as a coordinator writes it.
fn example_file(steps: &[u64]) -> Publication {
    let (p, reg) = (example::params(), example::registry());
    let mut out = Publication::new();
    for &as_of in steps {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        out.push(&p, &s, &r, &book, None).expect("push");
    }
    out
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("onyx-track-serve-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    dir.join("publication.borsh")
}

/// Write like the coordinator: a whole new file renamed into place.
fn write(path: &std::path::Path, p: &Publication) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, borsh::to_vec(p).expect("borsh")).expect("write");
    std::fs::rename(&tmp, path).expect("rename");
}

/// ROADMAP C20 item 1 (TR-S-13, TR-S-14): the logs are served, a client
/// reruns `compute` on them and checks every served commitment extends the
/// one before; the served bytes are the site's example log.
#[tokio::test]
async fn the_logs_are_served_and_rerun() {
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .with_log_per_minute(100);
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    let app = routes(Arc::new(store));
    let reg = example::registry();
    let (s, h, b) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let e = LogExpect {
        registry: Some(&reg),
        ..Default::default()
    };
    let got = check_log_body(&b, &e).expect("checks");
    assert_eq!((got.commitment.clone(), got.links), (c2.clone(), 1));
    assert!(got.recomputed);
    let tag = h[header::ETAG].to_str().expect("etag").to_string();
    let (s, _, _) = call(&app, "/v1/track/log", &[("if-none-match", &tag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    let uri = format!("/v1/track/log?since={}", c1.hash());
    let (s, _, b) = call(&app, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
    let known = LogExpect {
        known: Some(c1.hash()),
        min_as_of: AS_OF,
        ..e
    };
    assert_eq!(check_log_body(&b, &known).expect("checks").links, 2);
    // The plain body names no earlier commitment: a client that knew c1 refuses it.
    let (_, _, plain) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(
        check_log_body(&plain, &known).err(),
        Some(LogError::NotDescendant)
    );
    let (s, _, b) = call(
        &app,
        &format!("/v1/track/log?since={}", Hash32([9; 32])),
        &[],
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, Some("unknown_commitment"))
    );
    for bad in ["/v1/track/log?x=1", "/v1/track/log?since=0x12"] {
        let (s, _, _) = call(&app, bad, &[]).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
    // The site's copy (apps/web/static/level5/track-example-log.json) is the
    // body with since = the first commitment.
    let (_, _, b) = call(&app, &uri, &[]).await;
    let text = serde_json::to_string_pretty(&b).expect("json") + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/web/static/level5/track-example-log.json"
    );
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &text).expect("write");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("site example log (bless with ONYX_BLESS=1)"),
        text
    );
}

/// The site's `since` file (apps/web/static/level5/track-example-since.json):
/// each example record as the server answers `?since=` each example
/// commitment, so the browser asks with the commitment it saw last (C20 item 4).
#[tokio::test]
async fn the_site_since_file_is_what_the_server_serves() {
    // (record answers: the default budget of 30 a minute covers the four)
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    let app = routes(Arc::new(store));
    let mut records = serde_json::Map::new();
    for src in [FORECASTER, GUESSER] {
        let mut by = serde_json::Map::new();
        for c in [&c1, &c2] {
            let uri = format!("{}?since={}", record_uri(src), c.hash());
            let (s, _, b) = call(&app, &uri, &[]).await;
            assert_eq!(s, StatusCode::OK);
            by.insert(c.hash().to_string(), b);
        }
        records.insert(src.to_string(), Value::Object(by));
    }
    let doc = json!({
        "example": true,
        "note": "Example only: each example record as the server answers ?since= each example commitment. Not a real record.",
        "since": records,
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/web/static/level5/track-example-since.json"
    );
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &text).expect("write");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("site since file (bless with ONYX_BLESS=1)"),
        text
    );
}

/// ADR-0036 amendment 5 item 2: logs holding a level-1 or level-2 answer (a
/// value the paid API sells) are not served free; the records still are.
#[tokio::test]
async fn logs_with_priced_answers_are_not_served_free() {
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    let (s, r) = example::logs_with_a_level1_answer(AS_OF);
    store
        .publish(example::params(), &example::registry(), s, r, AS_OF)
        .expect("publish");
    let app = routes(Arc::new(store));
    let (st, _, b) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::FORBIDDEN, Some("track_log_priced"))
    );
    let (st, _, _) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(st, StatusCode::OK);
}

/// TR-S-7 for the logs: they cover every record, so in fallback a
/// publication with any label-derived rule has no log served.
#[tokio::test]
async fn fallback_withholds_the_logs_of_a_label_derived_track() {
    let params = TrackParams::new(vec![MetricRule {
        metric: MetricId("test.labelled".into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }]);
    for rights in [LabelRights::Cleared, LabelRights::Fallback] {
        let store = TrackStore::new(rights);
        store
            .publish(
                params.clone(),
                &labelled_registry(),
                vec![labelled_statement()],
                vec![],
                1_000,
            )
            .expect("publish");
        let app = routes(Arc::new(store));
        let (s, _, b) = call(&app, "/v1/track/log", &[]).await;
        if rights == LabelRights::Fallback {
            assert_eq!(
                (s, code(&b)),
                (StatusCode::FORBIDDEN, Some("label_rights_withheld"))
            );
        } else {
            assert_eq!(s, StatusCode::OK, "{b}");
            assert!(check_log_body(&b, &LogExpect::default()).is_ok());
        }
    }
    // Nothing published: a 404 like the records.
    let (s, _, b) = call(&empty_routes(LabelRights::Cleared), "/v1/track/log", &[]).await;
    assert_eq!(
        (s, code(&b), b["reason"].as_str()),
        (
            StatusCode::NOT_FOUND,
            Some("no_track_record"),
            Some(NONE_PUBLISHED)
        )
    );
}

/// ROADMAP C20 item 2: a node serves the coordinator's publication file,
/// re-read when it changes; a missing file publishes nothing yet; a file that
/// does not check, or rewrites what was served, stops every track answer
/// (503) until it is fixed, never the node (review C20-R1-4).
#[tokio::test]
async fn a_node_serves_the_coordinators_publication_file() {
    let path = scratch("node");
    let head = path.with_file_name(SERVED_HEAD_FILE);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&head);
    let (store, first) = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .remembering(head.clone())
        .reading(path.clone());
    assert_eq!(first, Ok(()), "missing file: nothing yet");
    let app = routes(Arc::new(store));
    let (_, _, b) = call(&app, "/v1/track", &[]).await;
    assert_eq!(b["published"], false);

    let early = example_file(&[AS_OF_EARLY]);
    write(&path, &early);
    let (s, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["asOf"], AS_OF_EARLY);

    let full = example_file(&[AS_OF_EARLY, AS_OF]);
    write(&path, &full);
    let (_, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    let c = check_record_body(&b, &expect(FORECASTER)).expect("checks");
    assert_eq!(c.commitment, full.steps[1].commitment);

    // A rewritten file (the logs reordered, same length) and a truncated one
    // (back to the early file) both stop every answer.
    let mut rewritten = full.clone();
    rewritten.statements.swap(0, 1);
    for bad in [rewritten, early.clone()] {
        write(&path, &bad);
        for uri in ["/v1/track", "/v1/track/log", &record_uri(FORECASTER)] {
            let (s, _, b) = call(&app, uri, &[]).await;
            assert_eq!(
                (s, code(&b)),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some("track_store_unreadable")
                ),
                "{uri}"
            );
        }
    }
    write(&path, &full);
    let (s, _, _) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);

    // At start: a file that does not parse, or a node store given the
    // example, answers 503 on the track routes; the node itself starts.
    let dir = path.parent().expect("dir").to_path_buf();
    let _ = std::fs::remove_file(&head);
    std::fs::write(&path, b"not borsh").expect("write");
    let app = node_routes(LabelRights::Cleared, Some(path.clone()), dir.clone());
    let (s, _, _) = call(&app, "/v1/track", &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    write(&path, &full);
    let app = node_routes(LabelRights::Cleared, Some(path.clone()), dir.clone());
    let (s, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        b["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("example")),
        "{b}"
    );
    let (s, _, _) = call(
        &node_routes(LabelRights::Cleared, None, dir),
        "/v1/track",
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let _ = std::fs::remove_file(&path);
}

/// Review C20-R1-1: what a node served is kept across restarts. After a
/// restart, a fresh chain that drops what was served, a truncated file, or no
/// file at all answers 503; an honest extension is served.
#[tokio::test]
async fn a_restart_never_goes_behind_what_was_served() {
    let path = scratch("restart");
    let head = path.with_file_name(SERVED_HEAD_FILE);
    let _ = std::fs::remove_file(&head);
    let start = || {
        let (store, _) = TrackStore::new(LabelRights::Cleared)
            .allowing_examples()
            .remembering(head.clone())
            .reading(path.clone());
        routes(Arc::new(store))
    };
    let early = example_file(&[AS_OF_EARLY]);
    let full = example_file(&[AS_OF_EARLY, AS_OF]);
    write(&path, &full);
    let (s, _, _) = call(&start(), &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    // A fresh chain from zero that drops the earlier commitment's
    // statement: it checks on its own, but is not what was served.
    let (p, reg) = (example::params(), example::registry());
    let (mut st, r) = example::logs(AS_OF);
    st.remove(0);
    let book = compute(&p, &reg, &st, &r, AS_OF).expect("compute");
    let mut fork = Publication::new();
    fork.push(&p, &st, &r, &book, None).expect("push");
    for bad in [Some(fork), Some(early), None] {
        match &bad {
            Some(f) => write(&path, f),
            None => std::fs::remove_file(&path).expect("remove"),
        }
        let (s, _, b) = call(&start(), "/v1/track", &[]).await;
        assert_eq!(
            (s, code(&b)),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("track_store_unreadable")
            )
        );
    }
    write(&path, &full);
    let (s, _, _) = call(&start(), &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&head);
}

/// A node has no registry: every ruled metric counts as label-derived, so in
/// fallback a file-served record is withheld (fail closed).
#[tokio::test]
async fn a_file_served_track_is_withheld_in_fallback() {
    let reg = labelled_registry();
    let params = TrackParams::new(vec![MetricRule {
        metric: MetricId("test.labelled".into()),
        close: Close::AfterAsOf(0),
        tolerance: 0,
    }]);
    let st = vec![labelled_statement()];
    let book = compute(&params, &reg, &st, &[], 1_000).expect("compute");
    let mut file = Publication::new();
    file.push(&params, &st, &[], &book, None).expect("push");
    let path = scratch("fallback");
    write(&path, &file);
    for (rights, want) in [
        (LabelRights::Cleared, StatusCode::OK),
        (LabelRights::Fallback, StatusCode::FORBIDDEN),
    ] {
        let dir = path.parent().expect("dir").to_path_buf();
        let _ = std::fs::remove_file(dir.join(SERVED_HEAD_FILE));
        let app = node_routes(rights, Some(path.clone()), dir);
        let (s, _, _) = call(&app, "/v1/track/vendor.a/test.labelled", &[]).await;
        assert_eq!(s, want, "{rights:?}");
    }
    let _ = std::fs::remove_file(&path);
}

/// NL15 review: a node serving a file uses the registry the file carries. A
/// metric that registry marks label-derived stays withheld in fallback; one it
/// marks free (weather, say) is served; a carried registry with an address
/// rule the node cannot rebuild counts for nothing (fail closed).
#[tokio::test]
async fn a_file_served_track_follows_its_carried_registry() {
    let rule = |metric: &str| {
        TrackParams::new(vec![MetricRule {
            metric: MetricId(metric.into()),
            close: Close::AfterAsOf(0),
            tolerance: 0,
        }])
    };
    // The labelled metric, carried: withheld in fallback, served when cleared.
    let reg = labelled_registry();
    let params = rule("test.labelled");
    let st = vec![labelled_statement()];
    let book = compute(&params, &reg, &st, &[], 1_000).expect("compute");
    let mut file = Publication::with_format(onyx_track::chained::LOG_FORMAT_WHOLE);
    file.push_with(&params, &st, &[], &book, None, Some(&reg))
        .expect("push");
    for (rights, want) in [
        (LabelRights::Cleared, StatusCode::OK),
        (LabelRights::Fallback, StatusCode::FORBIDDEN),
    ] {
        let store = TrackStore::new(rights);
        store.install(file.clone(), None).expect("install");
        let (s, _, _) = call(
            &routes(Arc::new(store)),
            "/v1/track/vendor.a/test.labelled",
            &[],
        )
        .await;
        assert_eq!(s, want, "labelled {rights:?}");
    }
    // The same record under a free metric: served in fallback.
    let mut free = StaticRegistry::new();
    free.add_metric(
        "test.weather",
        MetricInfo {
            value_kind: ValueKind::UInt,
            needs_label_set: false,
        },
    )
    .and_then(|r| r.add_aggregate("a.one"))
    .and_then(|r| r.add_source(DataRights::public_chain(SourceId("vendor.a".into()))))
    .expect("registry");
    let params = rule("test.weather");
    let mut st = labelled_statement();
    st.fact.metric = MetricId("test.weather".into());
    let st = vec![st];
    let book = compute(&params, &free, &st, &[], 1_000).expect("compute");
    let mut file = Publication::with_format(onyx_track::chained::LOG_FORMAT_WHOLE);
    file.push_with(&params, &st, &[], &book, None, Some(&free))
        .expect("push");
    let store = TrackStore::new(LabelRights::Fallback);
    store.install(file.clone(), None).expect("install");
    let app = routes(Arc::new(store));
    for uri in [
        "/v1/track/vendor.a/test.weather",
        "/v1/track/log",
        "/v1/track/registry",
    ] {
        let (s, _, _) = call(&app, uri, &[]).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
    }
    // A carried registry naming an address rule the node does not have.
    let mut ruled = file;
    if let Some(r) = ruled.registry.as_mut() {
        r.canons.push("test.canon.unknown.v1".into());
    }
    let store = TrackStore::new(LabelRights::Fallback);
    store.install(ruled, None).expect("install");
    let (s, _, _) = call(
        &routes(Arc::new(store)),
        "/v1/track/vendor.a/test.weather",
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

/// ROADMAP C20 item 3: a record past 1,024 entries is served without its
/// history and paged; the client recounts it over the pages. `at` must be the
/// latest commitment.
#[tokio::test]
async fn a_large_history_is_served_in_pages() {
    let params = example::params();
    let reg = example::registry();
    let mut st = Vec::new();
    for d in 0..200u64 {
        for e in example::ENTITIES {
            let as_of = example::T0 + d * 86_400;
            st.push(Statement {
                source: SourceId(FORECASTER.into()),
                fact: Fact {
                    metric: MetricId(METRIC.into()),
                    subject: Subject::Entity(e.into()),
                    as_of,
                },
                value: V::UInt(u128::from(d)),
                confidence: Confidence(700_000),
                committed_at: as_of - 60,
            });
        }
    }
    sort_statements(&mut st);
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    let c = store
        .publish(params, &reg, st, vec![], example::T0 + 300 * 86_400)
        .expect("publish");
    let app = routes(Arc::new(store));
    let (_, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    let mut checked = check_record_body(&b, &expect(FORECASTER)).expect("record");
    assert!(!checked.history_checked);
    let mut pages = Vec::new();
    for from in [0, MAX_HISTORY] {
        let uri = format!(
            "{}/history?at={}&from={from}",
            record_uri(FORECASTER),
            c.hash()
        );
        let (s, _, b) = call(&app, &uri, &[]).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        pages.push(check_history_page(&b, &checked, from).expect("page"));
    }
    complete_history(&mut checked, &pages).expect("recount");
    assert_eq!(checked.entries.len(), 1_200);
    let base = format!("{}/history", record_uri(FORECASTER));
    let (s, _, b) = call(&app, &format!("{base}?at={}&from=0", Hash32([4; 32])), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::CONFLICT, Some("commitment_moved"))
    );
    for bad in [
        format!("{base}?at={}&from=1200", c.hash()),
        format!("{base}?at={}&from=01", c.hash()),
        format!("{base}?from=0&at={}", c.hash()),
        format!("{base}?at={}", c.hash()),
        base.clone(),
    ] {
        let (s, _, b) = call(&app, &bad, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::BAD_REQUEST, Some("bad_request")),
            "{bad}"
        );
    }
    let (s, _, _) = call(
        &app,
        &format!(
            "/v1/track/example.nobody/{METRIC}/history?at={}&from=0",
            c.hash()
        ),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Review TR-S-R2-3: the index is paged (`from`, `next`).
#[tokio::test]
async fn the_index_is_paged() {
    let app = routes(example_store(LabelRights::Cleared, AS_OF));
    let (_, _, all) = call(&app, "/v1/track", &[]).await;
    assert_eq!(
        (all["records"].as_array().map(Vec::len), all["next"].clone()),
        (Some(2), Value::Null)
    );
    let (_, _, b) = call(&app, "/v1/track?from=1", &[]).await;
    assert_eq!(b["records"].as_array().map(Vec::len), Some(1));
    assert_eq!(b["records"][0], all["records"][1]);
    for bad in [
        "/v1/track?from=01",
        "/v1/track?from=-1",
        "/v1/track?from=",
        "/v1/track?x=1",
    ] {
        let (s, _, _) = call(&app, bad, &[]).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
}

/// TR-C20-5: a log body can be tens of megabytes, so log answers have their
/// own budget (2 a minute by default); a 304 costs nothing; records keep theirs.
#[tokio::test]
async fn log_answers_have_their_own_small_budget() {
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    publish_example(&store, AS_OF).expect("publish");
    let app = routes(Arc::new(store));
    let (s, h, _) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let tag = h[header::ETAG].to_str().expect("etag").to_string();
    let (s, _, _) = call(&app, "/v1/track/log", &[("if-none-match", &tag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    let (s, _, _) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, b) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert_eq!(h["retry-after"], "30");
    let (s, _, _) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// Review C20-R2-2: across a restart the file must start with the same bytes
/// the node served (steps and log rows, the kept prefix digest), not only the
/// same latest commitment: a file with the served steps but its early rows
/// reordered (every commitment still checks) is a rewrite, as it is without a
/// restart. Only the steps after the kept head are checked again.
#[tokio::test]
async fn a_restart_keeps_the_served_bytes_as_its_floor() {
    let path = scratch("restart-bytes");
    let head = path.with_file_name(SERVED_HEAD_FILE);
    let _ = std::fs::remove_file(&head);
    let start = || {
        let (store, _) = TrackStore::new(LabelRights::Cleared)
            .allowing_examples()
            .remembering(head.clone())
            .reading(path.clone());
        routes(Arc::new(store))
    };
    let full = example_file(&[AS_OF_EARLY, AS_OF]);
    assert!(
        full.steps[0].statements >= 2,
        "the first step covers two rows"
    );
    write(&path, &full);
    let (s, _, _) = call(&start(), &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let mut reordered = full.clone();
    reordered.statements.swap(0, 1);
    assert!(reordered.check().is_ok(), "it checks on its own");
    write(&path, &reordered);
    let (s, _, b) = call(&start(), "/v1/track", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("track_store_unreadable")
        )
    );
    write(&path, &full);
    let (s, _, _) = call(&start(), &record_uri(FORECASTER), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&head);
}

/// Review C20-R2-2: a file whose check would pass `MAX_INSTALL_CHECK` (steps
/// times rows) is refused before any hashing, so a griefing coordinator cannot
/// hold a node's start or its blocking pool for hours.
#[test]
fn a_file_too_costly_to_check_is_refused_before_checking() {
    let mut big = example_file(&[AS_OF_EARLY, AS_OF]);
    let filler = big.statements[0].clone();
    let rows = 4_096usize;
    big.statements = vec![filler; rows];
    big.resolutions.clear();
    let step = big.steps[1].clone();
    let steps = (MAX_INSTALL_CHECK as usize / rows) + 1;
    big.steps = vec![step; steps];
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    // This thread's CPU time, not the wall clock (ROADMAP E12): a loaded
    // machine stretches the wall time of a refusal, never the work it does.
    let wall = std::time::Instant::now();
    let t = thread_cpu(wall);
    assert_eq!(store.install(big, None).err(), Some(PublishError::TooLarge));
    let used = thread_cpu(wall).saturating_sub(t);
    assert!(used < std::time::Duration::from_secs(2), "{used:?} of CPU");
    // Just under the bound the file is checked (and refused for what it is).
    let mut under = example_file(&[AS_OF_EARLY, AS_OF]);
    under.steps = vec![under.steps[1].clone(); MAX_INSTALL_CHECK as usize / rows];
    under.statements = vec![under.statements[0].clone(); rows];
    under.resolutions.clear();
    assert!(!matches!(
        store.install(under, None).err(),
        Some(PublishError::TooLarge) | None
    ));
}

/// Time this thread has run on a CPU (Linux: `/proc/thread-self/schedstat`,
/// as in `tests/observation_store_scale.rs`), so time spent waiting for a CPU
/// on a loaded machine is not counted; elsewhere, wall time since `wall`.
fn thread_cpu(wall: std::time::Instant) -> std::time::Duration {
    let ns = std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok());
    ns.map_or_else(|| wall.elapsed(), std::time::Duration::from_nanos)
}

/// Review C20-R2-3: a request never waits on a read of the publication file
/// (nor reads it on the runtime's workers): while a read runs, it is answered
/// from the publication the last finished read left.
#[tokio::test]
// The test holds the read lock across a request on purpose: a read in progress.
#[allow(clippy::await_holding_lock)]
async fn a_request_never_waits_on_a_running_read() {
    let path = scratch("busy");
    let head = path.with_file_name(SERVED_HEAD_FILE);
    let _ = std::fs::remove_file(&head);
    write(&path, &example_file(&[AS_OF_EARLY]));
    let (store, first) = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .remembering(head.clone())
        .reading(path.clone());
    assert_eq!(first, Ok(()));
    let store = Arc::new(store);
    let app = routes(store.clone());
    write(&path, &example_file(&[AS_OF_EARLY, AS_OF]));
    let f = store.file.as_ref().expect("file");
    {
        // A read in progress (this test holds its lock).
        let _busy = f.seen.lock().expect("lock");
        let (s, _, b) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            call(&app, &record_uri(FORECASTER), &[]),
        )
        .await
        .expect("answered without waiting on the read");
        assert_eq!((s, b["asOf"].as_u64()), (StatusCode::OK, Some(AS_OF_EARLY)));
    }
    let (s, _, b) = call(&app, &record_uri(FORECASTER), &[]).await;
    assert_eq!((s, b["asOf"].as_u64()), (StatusCode::OK, Some(AS_OF)));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&head);
}

/// Review C20-R2-4: a history page is bounded before the budget and built
/// only after it: past the end is a 400 whatever the budget, a valid page past
/// the budget a 429.
#[tokio::test]
async fn a_history_page_is_built_only_within_the_budget() {
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .with_free_per_minute(1);
    let c = publish_example(&store, AS_OF).expect("publish");
    let app = routes(Arc::new(store));
    let base = format!("{}/history?at={}", record_uri(FORECASTER), c.hash());
    let (s, _, b) = call(&app, &format!("{base}&from=0"), &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, _, _) = call(&app, &format!("{base}&from=1000000"), &[]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _, b) = call(&app, &format!("{base}&from=1"), &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

/// Review C20-R2-5: a node whose state directory holds a served head but
/// that names no publication file answers 503, never "none published" about
/// records it served.
#[tokio::test]
async fn a_node_that_served_before_and_names_no_file_answers_503() {
    let path = scratch("unset");
    let dir = path.parent().expect("dir").to_path_buf();
    let head = dir.join(SERVED_HEAD_FILE);
    let _ = std::fs::remove_file(&head);
    let app = node_routes(LabelRights::Cleared, None, dir.clone());
    let (s, _, b) = call(&app, "/v1/track", &[]).await;
    assert_eq!((s, b["published"].as_bool()), (StatusCode::OK, Some(false)));
    let full = example_file(&[AS_OF_EARLY, AS_OF]);
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .remembering(head.clone());
    store.install(full, None).expect("served");
    assert!(head.exists());
    let app = node_routes(LabelRights::Cleared, None, dir);
    for uri in ["/v1/track", "/v1/track/log", &record_uri(FORECASTER)] {
        let (s, _, b) = call(&app, uri, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Some("track_store_unreadable")
            ),
            "{uri}"
        );
    }
    let _ = std::fs::remove_file(&head);
}

/// ROADMAP C21/C23: chained logs answer a head and their rows in pages; a
/// client pages through them, rebuilds the served registry, checks it hashes
/// to `registry_root` and reruns `compute` to the served commitment.
#[tokio::test]
async fn chained_logs_are_served_in_pages_and_rerun_with_the_served_registry() {
    use onyx_track::wire::{check_registry_body, PagedLog};
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .chaining()
        .with_log_per_minute(100)
        .with_pages_per_minute(4);
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    assert_ne!(c2, example::chain().expect("chain")[1].0, "chained hashes");
    let app = routes(Arc::new(store));
    let (s, _, reg_body) = call(&app, "/v1/track/registry", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let reg = check_registry_body(&reg_body, &c2, &onyx_core::NoRules).expect("registry");
    let (s, _, head) = call(&app, &format!("/v1/track/log?since={}", c1.hash()), &[]).await;
    assert_eq!(s, StatusCode::OK);
    let e = LogExpect {
        known: Some(c1.hash()),
        registry: Some(&reg),
        ..Default::default()
    };
    let mut log = PagedLog::start(&head, &e).expect("head");
    let mut tag = None;
    while let Some(pos) = log.next_rows() {
        let uri = format!(
            "/v1/track/log?at={}&from={pos}&since={}",
            log.at(),
            log.since()
        );
        let (s, h, page) = call(&app, &uri, &[]).await;
        assert_eq!(s, StatusCode::OK, "{page}");
        tag = h
            .get(header::ETAG)
            .map(|v| v.to_str().expect("tag").to_string());
        log.take_rows(&page).expect("page");
    }
    let got = log.finish(&e).expect("checks");
    assert!(got.recomputed && got.links == 2);
    // A revalidated page is free (304); the page budget is its own.
    let first = format!("/v1/track/log?at={}&from=0&since={}", c2.hash(), c1.hash());
    // (the example's stream fits one page: `first` is the page just taken)
    let tag = tag.expect("etag");
    let (s, _, _) = call(&app, &first, &[("if-none-match", &tag)]).await;
    assert_eq!(s, StatusCode::NOT_MODIFIED);
    for _ in 0..4 {
        call(&app, &first, &[]).await;
    }
    let (s, _, b) = call(&app, &first, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

/// The page queries, in the documented order: canonical forms only; a stale
/// `at`, an unknown `since`, a position past the end, posts without an intake.
#[tokio::test]
async fn log_pages_refuse_stale_unknown_and_out_of_range_asks() {
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .chaining()
        .with_pages_per_minute(100);
    let c1 = publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    let app = routes(Arc::new(store));
    let (a, k) = (c2.hash(), c1.hash());
    for (uri, status, want) in [
        (
            format!("/v1/track/log?at={k}&from=0&since={k}"),
            StatusCode::CONFLICT,
            "commitment_moved",
        ),
        (
            format!("/v1/track/log?at={a}&from=0&since={}", Hash32([9; 32])),
            StatusCode::CONFLICT,
            "unknown_commitment",
        ),
        (
            format!("/v1/track/log?at={a}&from=100000&since={k}"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/track/log?at={a}&posts=0"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/track/log?at={a}&from=01&since={k}"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!("/v1/track/log?since={k}&at={a}&from=0"),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            format!(
                "/v1/track/log?at={}&from=0&since={k}",
                a.to_string().to_uppercase().replace("0X", "0x")
            ),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
    ] {
        let (s, _, b) = call(&app, &uri, &[]).await;
        assert_eq!((s, code(&b)), (status, Some(want)), "{uri}");
    }
    let (s, _, _) = call(&app, &format!("/v1/track/log?at={a}&from=0&since={k}"), &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// ROADMAP C23 on format-1 logs: the store's publication carries the
/// registry too, so a client reruns the whole body with the served registry;
/// pages are only for chained logs.
#[tokio::test]
async fn whole_logs_rerun_with_the_served_registry_and_have_no_pages() {
    use onyx_track::wire::check_registry_body;
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .with_log_per_minute(100);
    publish_example(&store, AS_OF_EARLY).expect("early");
    let c2 = publish_example(&store, AS_OF).expect("latest");
    assert_eq!(
        c2,
        example::chain().expect("chain")[1].0,
        "format 1 unchanged"
    );
    let app = routes(Arc::new(store));
    let (_, _, reg_body) = call(&app, "/v1/track/registry", &[]).await;
    let reg = check_registry_body(&reg_body, &c2, &onyx_core::NoRules).expect("registry");
    let (_, _, b) = call(&app, "/v1/track/log", &[]).await;
    let got = check_log_body(
        &b,
        &LogExpect {
            registry: Some(&reg),
            ..Default::default()
        },
    )
    .expect("checks");
    assert!(got.recomputed);
    let uri = format!("/v1/track/log?at={}&from=0&since={}", c2.hash(), c2.hash());
    let (s, _, b) = call(&app, &uri, &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (s, _, b) = call(&app, "/v1/track/registry?x=1", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    // The site's copy (apps/web/static/level5/track-example-registry.json) is
    // what the server serves for the example.
    let text = serde_json::to_string_pretty(&reg_body).expect("json") + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/web/static/level5/track-example-registry.json"
    );
    if std::env::var("ONYX_BLESS").as_deref() == Ok("1") {
        std::fs::write(path, &text).expect("write");
    }
    assert_eq!(
        std::fs::read_to_string(path).expect("site example registry (bless with ONYX_BLESS=1)"),
        text
    );
}

/// ROADMAP C23 rights: the registry is withheld wherever the logs are (a
/// label-derived ruled metric in fallback), and in fallback whenever it names
/// an address (a recorded revocation), even when the logs are not; a
/// version-1 file carries none (404); nothing published is a 404.
#[tokio::test]
async fn the_registry_is_withheld_where_label_data_is() {
    let app = node_routes(LabelRights::Cleared, None, std::env::temp_dir());
    let (s, _, b) = call(&app, "/v1/track/registry", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("no_track_record"))
    );

    // A version-1 file (no registry).
    let store = TrackStore::new(LabelRights::Cleared).allowing_examples();
    store
        .install(
            example_file(&[AS_OF_EARLY, AS_OF]),
            Some(&example::registry()),
        )
        .expect("install");
    let (s, _, b) = call(&routes(Arc::new(store)), "/v1/track/registry", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::NOT_FOUND, Some("no_track_registry"))
    );

    // Fallback, a node with no registry of its own: it uses the registry the
    // file carries (no address rules, the latest commitment's root), so the
    // example metric is not label-derived and the registry is served (NL15
    // review). A carried registry that does not rebuild to that root counts
    // for nothing: every ruled metric is label-derived, logs and registry
    // are withheld (fail closed).
    let mut file = Publication::with_format(onyx_track::chained::LOG_FORMAT_WHOLE);
    let (p, reg) = (example::params(), example::registry());
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        file.push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
    }
    let store = TrackStore::new(LabelRights::Fallback).allowing_examples();
    store.install(file.clone(), None).expect("install");
    let (s, _, _) = call(&routes(Arc::new(store)), "/v1/track/registry", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let mut forged = file.clone();
    if let Some(r) = forged.registry.as_mut() {
        r.metrics.clear();
    }
    let store = TrackStore::new(LabelRights::Fallback).allowing_examples();
    store.install(forged, None).expect("install");
    let (s, _, b) = call(&routes(Arc::new(store)), "/v1/track/registry", &[]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, Some("label_rights_withheld"))
    );

    // Fallback with the logs served (the example metric is not label-derived):
    // the registry is served, unless it names an address.
    let store = TrackStore::new(LabelRights::Fallback).allowing_examples();
    store.install(file.clone(), Some(&reg)).expect("install");
    let (s, _, _) = call(&routes(Arc::new(store)), "/v1/track/registry", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let mut named = file;
    if let Some(r) = named.registry.as_mut() {
        r.revocations.push((
            ("entity:example-a".into(), "btc:1example".into()),
            vec![(1, [1; 20], Hash32([1; 32]))],
        ));
    }
    // Review C23-R2-2: a revocation is a label row (an address named with
    // an entity) the root covers, so the registry is withheld in every
    // rights mode, never served free with it.
    for rights in [LabelRights::Fallback, LabelRights::Cleared] {
        let store = TrackStore::new(rights).allowing_examples();
        store.install(named.clone(), Some(&reg)).expect("install");
        let (s, _, b) = call(&routes(Arc::new(store)), "/v1/track/registry", &[]).await;
        assert_eq!(
            (s, code(&b)),
            (
                StatusCode::FORBIDDEN,
                Some("track_registry_names_addresses")
            ),
            "{rights:?}"
        );
        assert!(!b.to_string().contains("1example"), "{rights:?}");
    }
}

/// C22 kept on pages: chained logs holding a level-1 answer answer 403
/// `track_log_priced` on the head and on every page.
#[tokio::test]
async fn chained_logs_with_priced_answers_are_not_paged_free() {
    let store = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .chaining();
    let (s, r) = example::logs_with_a_level1_answer(AS_OF);
    let c = store
        .publish(example::params(), &example::registry(), s, r, AS_OF)
        .expect("publish");
    let app = routes(Arc::new(store));
    for uri in [
        "/v1/track/log".to_string(),
        format!("/v1/track/log?at={0}&from=0&since={0}", c.hash()),
        format!("/v1/track/log?at={}&posts=0", c.hash()),
    ] {
        let (s, _, b) = call(&app, &uri, &[]).await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, Some("track_log_priced")),
            "{uri}"
        );
    }
}

/// A node serves a chained publication file from `ONYX_TRACK_PUBLICATIONS`
/// (written as the coordinator writes it) and pages it.
#[tokio::test]
async fn a_node_pages_a_chained_publication_file() {
    use onyx_track::wire::PagedLog;
    let path = scratch("chained");
    let head = path.with_file_name("head.borsh");
    let _ = std::fs::remove_file(&head);
    let mut file = Publication::chained();
    let (p, reg) = (example::params(), example::registry());
    for as_of in [AS_OF_EARLY, AS_OF] {
        let (s, r) = example::logs(as_of);
        let book = compute(&p, &reg, &s, &r, as_of).expect("compute");
        file.push_with(&p, &s, &r, &book, None, Some(&reg))
            .expect("push");
    }
    write(&path, &file);
    let (store, first) = TrackStore::new(LabelRights::Cleared)
        .allowing_examples()
        .remembering(head)
        .reading(path);
    assert_eq!(first, Ok(()));
    let app = routes(Arc::new(store));
    let (s, _, h) = call(&app, "/v1/track/log", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let e = LogExpect::default();
    let mut log = PagedLog::start(&h, &e).expect("head");
    while let Some(pos) = log.next_rows() {
        let uri = format!(
            "/v1/track/log?at={}&from={pos}&since={}",
            log.at(),
            log.since()
        );
        let (_, _, page) = call(&app, &uri, &[]).await;
        log.take_rows(&page).expect("page");
    }
    assert_eq!(log.finish(&e).expect("checks").links, 1);
}

/// C66 (ADR-0036 amendment 7): the daily level-2 metrics live on Base Sepolia, under their
/// daily close rule, with a forecaster's statements resolved by recorded level-2 rounds only.
/// The served record checks, counts what agreed and what was late, and the index lists the
/// rule that says how it was computed.
#[tokio::test]
async fn a_live_publication_serves_the_daily_records() {
    const TMAX: &str = "noaa.ghcnd.usw00003017.tmax";
    const QUAKES: &str = "usgs.earthquakes.m5_count";
    const DAY: u64 = daily::DAY;
    // 2026-09-28 and 2026-09-29 00:00 UTC: the ends of 2026-09-27 and 2026-09-28.
    let ends = [1_790_553_600u64, 1_790_553_600 + DAY];
    let days = ["2026-09-27", "2026-09-28"];
    let mut reg = StaticRegistry::new();
    reg.add_metric(
        QUAKES,
        MetricInfo {
            value_kind: ValueKind::Int,
            needs_label_set: false,
        },
    )
    .and_then(|r| {
        r.add_metric(
            TMAX,
            MetricInfo {
                value_kind: ValueKind::Fixed { scale: 1 },
                needs_label_set: false,
            },
        )
    })
    .and_then(|r| r.add_source(DataRights::public_chain(SourceId("test.forecaster".into()))))
    .and_then(|r| r.add_source(DataRights::public_chain(SourceId("noaa.ncei.ghcnd".into()))))
    .expect("registry");
    for d in days {
        reg.add_aggregate(&format!("noaa.ghcnd.usw00003017:{d}"))
            .expect("subject");
    }
    let params = TrackParams::new(vec![
        daily::daily_rule(MetricId(TMAX.into()), 5),
        daily::daily_rule(MetricId(QUAKES.into()), 0),
    ])
    .with_level2_resolvers_only();
    let fact = |i: usize| {
        daily::daily_fact(
            MetricId(TMAX.into()),
            "noaa.ghcnd.usw00003017",
            &format!("noaa.ghcnd.usw00003017:{}", days[i]),
        )
        .expect("fact")
    };
    let close = |i: usize| ends[i] - DAY - daily::DAILY_LEAD_SECS;
    let st = |i: usize, mantissa: i128, at: u64| Statement {
        source: SourceId("test.forecaster".into()),
        fact: fact(i),
        value: V::Fixed { mantissa, scale: 1 },
        confidence: Confidence(500_000),
        committed_at: at,
    };
    // Day 0: forecast 28.0, observed 28.3 (within 0.5: agrees). Day 1: stated after the close.
    let mut statements = vec![st(0, 280, close(0) - 10), st(1, 250, close(1) + 1)];
    sort_statements(&mut statements);
    // Each resolution from a round the client's node shows recorded at level 2.
    let res = |i: usize, mantissa: i128| {
        daily::resolution_of(
            fact(i),
            V::Fixed { mantissa, scale: 1 },
            onyx_core::keccak256(days[i].as_bytes()),
            Some(daily::Recorded::level2(ends[i] + 30 * DAY)),
            SourceId("noaa.ncei.ghcnd".into()),
        )
        .expect("resolution")
    };
    let mut resolutions = vec![res(0, 283), res(1, 250)];
    onyx_track::sort_resolutions(&mut resolutions);
    let as_of = ends[1] + 31 * DAY;
    let store = Arc::new(TrackStore::new(LabelRights::Cleared));
    store
        .publish(params, &reg, statements, resolutions, as_of)
        .expect("publish");
    let app = routes(store);
    let uri = format!("/v1/track/test.forecaster/{TMAX}");
    let (s, _, b) = call(&app, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let c = check_record_body(
        &b,
        &Expect {
            source: "test.forecaster",
            metric: TMAX,
            min_as_of: 0,
            known: None,
            latest: None,
        },
    )
    .expect("checks");
    assert!(!c.example);
    assert_eq!((c.record.agreed, c.record.late), (1, 1));
    // The index names the rule the scores were computed under.
    let (s, _, b) = call(&app, "/v1/track", &[]).await;
    assert_eq!(s, StatusCode::OK);
    let rules = b["rules"].as_array().expect("rules");
    assert!(rules.iter().any(|r| r["metric"] == TMAX), "{b}");
    assert_eq!(b["records"].as_array().map(Vec::len), Some(1));
}
