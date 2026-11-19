//! THREATS NL5-3: one client holds at most `max_inflight_per_client` of the
//! guard's permits, counted until its answer is written or dropped.
//!
//! No test here waits on the clock: a held request reports that it is inside
//! the handler and then waits for the test to let it go.

use super::*;
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;

const PROXY: &str = "127.0.0.1";

/// A server with the cap, the proxy at [`PROXY`] trusted, and three routes
/// behind the guard: `/v1/hold` (waits until [`Rig::release`]), `/v1/fail`
/// (a 500) and `/v1/panic`.
struct Rig {
    f: Fixture,
    app: Router,
    entered: mpsc::UnboundedReceiver<()>,
    open: Arc<Semaphore>,
}

fn rig(config: ApiConfig) -> Rig {
    let f = build(Setup {
        config: ApiConfig {
            trusted_proxies: vec![PROXY.parse().expect("cidr")],
            ..config
        },
        ..Setup::default()
    });
    let (tx, entered) = mpsc::unbounded_channel();
    let open = Arc::new(Semaphore::new(0));
    let gate = open.clone();
    let extra = Router::new()
        .route(
            "/v1/hold",
            axum::routing::get(move || {
                let (tx, gate) = (tx.clone(), gate.clone());
                async move {
                    let _ = tx.send(());
                    if let Ok(p) = gate.acquire().await {
                        p.forget();
                    }
                    "held"
                }
            }),
        )
        .route(
            "/v1/fail",
            axum::routing::get(|| async {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "it broke")
            }),
        )
        .route("/v1/panic", axum::routing::get(|| async { broken() }));
    let app = router_with(f.state.clone(), extra);
    Rig {
        f,
        app,
        entered,
        open,
    }
}

fn broken() -> &'static str {
    panic!("a handler panic (expected in this test)")
}

fn cap(n: u32) -> ApiConfig {
    ApiConfig {
        max_inflight_per_client: n,
        ..ApiConfig::default()
    }
}

/// Who sends a request: the peer address (none: in-process, no connect
/// info) and the `X-Forwarded-For` value, if any.
#[derive(Clone, Copy)]
struct From {
    peer: Option<&'static str>,
    xff: Option<&'static str>,
    /// A second `X-Forwarded-For` header line, after `xff`.
    xff2: Option<&'static str>,
}

/// A client behind the trusted proxy.
fn via(client: &'static str) -> From {
    From {
        peer: Some(PROXY),
        xff: Some(client),
        xff2: None,
    }
}

/// A client that reaches the node itself.
fn direct(peer: &'static str) -> From {
    From {
        peer: Some(peer),
        xff: None,
        xff2: None,
    }
}

fn request(uri: &str, from: From) -> Request<Body> {
    let mut req = Request::builder().uri(uri);
    for x in [from.xff, from.xff2].into_iter().flatten() {
        req = req.header("x-forwarded-for", x);
    }
    let mut req = req.body(Body::empty()).expect("request");
    if let Some(p) = from.peer {
        let ip: IpAddr = p.parse().expect("ip");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((ip, 4000))));
    }
    req
}

impl Rig {
    /// The answer's head; its body is still unwritten.
    async fn head(&self, uri: &str, from: From) -> Response {
        self.app
            .clone()
            .oneshot(request(uri, from))
            .await
            .expect("response")
    }

    /// A whole request: status, headers and JSON body (checked against the
    /// OpenAPI description like every answer the other tests see).
    async fn call(&self, uri: &str, from: From) -> (StatusCode, HeaderMap, Value) {
        let resp = self.head(uri, from).await;
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if uri == "/v1/health" {
            crate::openapi_check::check("GET", uri, status.as_u16(), &body);
        }
        (status, headers, body)
    }

    async fn status(&self, uri: &str, from: From) -> StatusCode {
        self.call(uri, from).await.0
    }

    /// Start a `/v1/hold` request and return once it is inside the handler.
    /// A request that is answered instead (refused) fails the test at once.
    async fn hold(&mut self, from: From) -> JoinHandle<StatusCode> {
        let app = self.app.clone();
        let mut h = tokio::spawn(async move {
            let resp = app
                .oneshot(request("/v1/hold", from))
                .await
                .expect("response");
            let status = resp.status();
            let _ = resp.into_body().collect().await;
            status
        });
        tokio::select! {
            biased;
            seen = self.entered.recv() => seen.expect("the request is held"),
            early = &mut h => panic!("the request was not held: {early:?}"),
        }
        h
    }

    /// Let `held` requests finish and wait for their answers.
    async fn release(&self, held: Vec<JoinHandle<StatusCode>>) {
        self.open.add_permits(held.len());
        for h in held {
            assert_eq!(h.await.expect("joined"), StatusCode::OK);
        }
    }

    /// Let one of `held` finish (whichever the gate wakes) and wait for its
    /// answer; the others stay held.
    async fn release_one(&self, held: &mut Vec<JoinHandle<StatusCode>>) {
        self.open.add_permits(1);
        let done = loop {
            if let Some(i) = held.iter().position(JoinHandle::is_finished) {
                break held.swap_remove(i);
            }
            tokio::task::yield_now().await;
        };
        assert_eq!(done.await.expect("joined"), StatusCode::OK);
    }

    fn table(&self) -> usize {
        self.f.state.clients_in_flight()
    }
}

/// The refusal: 429 `rate_limited` in the usual error shape, with
/// `Retry-After: 1`, never cached.
fn assert_refused(r: &(StatusCode, HeaderMap, Value)) {
    assert_eq!(r.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(code(&r.2), Some("rate_limited"));
    assert_eq!(
        r.2["error"]["message"],
        "too many requests in flight from this address"
    );
    assert_eq!(r.1.get("retry-after").expect("retry-after"), "1");
    assert_eq!(cc(&r.1), "no-store");
}

#[tokio::test]
async fn nl5_3_one_client_holds_at_most_its_share_and_others_are_served() {
    let mut r = rig(cap(3));
    let a = via("198.51.100.1");
    let b = via("198.51.100.2");
    // Under the cap all pass.
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(r.hold(a).await);
    }
    assert_eq!(r.table(), 1, "one client, one entry");
    // The next one of the same client is refused, on any route, and adds
    // nothing to the table; another client is served.
    assert_refused(&r.call("/v1/health", a).await);
    assert_refused(&r.call("/v1/hold", a).await);
    assert_eq!(r.table(), 1);
    assert_eq!(r.status("/v1/health", b).await, StatusCode::OK);
    // The proxy's hops before the client do not change who it is.
    assert_refused(&r.call("/v1/health", via("6.6.6.6, 198.51.100.1")).await);
    // One finishes: one place is free again, and only one.
    r.release_one(&mut held).await;
    assert_eq!(held.len(), 2);
    let again = r.hold(a).await;
    assert_refused(&r.call("/v1/health", a).await);
    held.push(again);
    r.release(held).await;
    assert_eq!(r.table(), 0, "nothing in flight, nothing in the table");
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
    assert_eq!(r.table(), 0);
}

#[tokio::test]
async fn nl5_3_a_dropped_request_gives_its_place_back() {
    let mut r = rig(cap(1));
    let a = via("198.51.100.1");
    // The client goes away mid-flight: the server drops the request's future.
    let gone = r.hold(a).await;
    assert_refused(&r.call("/v1/health", a).await);
    gone.abort();
    assert!(gone.await.expect_err("aborted").is_cancelled());
    assert_eq!(r.table(), 0);
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
    // An answer whose body is not written yet still holds the place; a
    // dropped body gives it back.
    let unread = r.head("/v1/health", a).await;
    assert_eq!(unread.status(), StatusCode::OK);
    assert_eq!(r.table(), 1);
    assert_refused(&r.call("/v1/health", a).await);
    drop(unread);
    assert_eq!(r.table(), 0);
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
}

#[tokio::test]
async fn nl5_3_errors_and_panics_give_the_place_back() {
    let r = rig(cap(1));
    let a = via("198.51.100.1");
    for _ in 0..5 {
        let got = r.call("/v1/fail", a).await;
        assert_eq!(
            (got.0, code(&got.2)),
            (StatusCode::INTERNAL_SERVER_ERROR, Some("internal"))
        );
        assert_eq!(r.status("/v1/nowhere", a).await, StatusCode::NOT_FOUND);
    }
    assert_eq!(r.table(), 0);
    // A handler that panics unwinds through the guard.
    for _ in 0..3 {
        let app = r.app.clone();
        let crashed = tokio::spawn(async move { app.oneshot(request("/v1/panic", a)).await });
        assert!(crashed.await.expect_err("panicked").is_panic());
        assert_eq!(r.table(), 0);
    }
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
    // A refused request holds nothing either.
    let unread = r.head("/v1/health", a).await;
    for _ in 0..5 {
        assert_refused(&r.call("/v1/health", a).await);
    }
    drop(unread);
    assert_eq!(r.table(), 0);
}

/// The request deadline answers 504 and the place goes back with that
/// answer; a body nobody reads holds it until the send timeout, like the
/// server-wide permit. The clock is paused: time moves only in the sleeps.
#[tokio::test(start_paused = true)]
async fn nl5_3_deadlines_give_the_place_back() {
    let mut r = rig(ApiConfig {
        send_timeout: Duration::from_millis(100),
        ..cap(1)
    });
    let a = via("198.51.100.1");
    let timed_out = r.hold(a).await;
    assert_refused(&r.call("/v1/health", a).await);
    assert_eq!(
        timed_out.await.expect("joined"),
        StatusCode::GATEWAY_TIMEOUT
    );
    assert_eq!(r.table(), 0);
    let stalled = r.head("/v1/health", a).await;
    tokio::time::sleep(Duration::from_millis(99)).await;
    assert_refused(&r.call("/v1/health", a).await);
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert_eq!(r.table(), 0, "the send timeout gives the place back");
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
    assert!(stalled.into_body().collect().await.is_err());
}

#[tokio::test]
async fn nl5_3_zero_turns_the_share_off() {
    let mut r = rig(cap(0));
    let a = via("198.51.100.1");
    let mut held = Vec::new();
    for _ in 0..40 {
        held.push(r.hold(a).await);
    }
    assert_eq!(r.table(), 0, "nothing is counted");
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
    r.release(held).await;
}

#[tokio::test]
async fn nl5_3_a_forwarded_ipv6_64_is_one_client_with_the_wider_scopes() {
    let mut r = rig(cap(2));
    // Two addresses of one /64 behind the proxy fill that /64's share.
    let mut held = vec![
        r.hold(via("2001:db8:0:1::1")).await,
        r.hold(via("[2001:db8:0:1::2]:4711")).await,
    ];
    assert_eq!(r.table(), 3, "the /64, its /56 and its /48");
    assert_refused(&r.call("/v1/health", via("2001:db8:0:1:9::3")).await);
    // Another /64 of the same /56 has its own share; the /56 holds 4 times
    // the cap, so the /64s of one allocation cannot each take a full share.
    for i in 2..=3 {
        let ip: &'static str = format!("2001:db8:0:{i}::1").leak();
        for _ in 0..2 {
            held.push(r.hold(via(ip)).await);
        }
    }
    held.push(r.hold(via("2001:db8:0:4::1")).await);
    held.push(r.hold(via("2001:db8:0:4::1")).await);
    assert_refused(&r.call("/v1/health", via("2001:db8:0:5::1")).await);
    // Another /56 of that /48 is served, and so is IPv4.
    assert_eq!(
        r.status("/v1/health", via("2001:db8:0:100::1")).await,
        StatusCode::OK
    );
    assert_eq!(
        r.status("/v1/health", via("198.51.100.9")).await,
        StatusCode::OK
    );
    r.release(held).await;
    assert_eq!(r.table(), 0);
}

/// Who is counted follows the request limit (`client_of`): not a caller
/// without connect info, not the trusted proxy's address when no
/// `X-Forwarded-For` was sent (a local service such as the health watch);
/// a forwarded header that names no client counts under the proxy's own key,
/// and a header from a peer that is no proxy is ignored.
#[tokio::test]
async fn nl5_3_local_callers_are_not_counted_and_a_bad_header_fails_closed() {
    let mut r = rig(cap(1));
    let in_process = From {
        peer: None,
        xff: None,
        xff2: None,
    };
    let local_service = direct(PROXY);
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(r.hold(in_process).await);
        held.push(r.hold(local_service).await);
    }
    assert_eq!(r.table(), 0);
    assert_eq!(r.status("/v1/health", local_service).await, StatusCode::OK);
    // A client at its share does not lock the local health check out.
    held.push(r.hold(via("198.51.100.1")).await);
    assert_refused(&r.call("/v1/health", via("198.51.100.1")).await);
    assert_eq!(r.status("/v1/health", local_service).await, StatusCode::OK);
    // No client named: counted under the proxy's key, all such together.
    held.push(r.hold(via("not an address")).await);
    assert_refused(&r.call("/v1/health", via("127.0.0.1")).await);
    assert_eq!(r.status("/v1/health", local_service).await, StatusCode::OK);
    // A peer that is no proxy cannot pick its key with the header.
    let spoof = |x| From {
        peer: Some("203.0.113.7"),
        xff: Some(x),
        xff2: None,
    };
    held.push(r.hold(spoof("198.51.100.50")).await);
    assert_refused(&r.call("/v1/health", spoof("198.51.100.51")).await);
    assert_refused(&r.call("/v1/health", direct("203.0.113.7")).await);
    r.release(held).await;
    assert_eq!(r.table(), 0);
}

/// The table is bounded by the server-wide permits: a place is taken only
/// with a permit, so requests naming ever new addresses add no entries once
/// the permits are out.
#[tokio::test]
async fn nl5_3_the_table_is_bounded_by_the_permits() {
    let mut r = rig(ApiConfig {
        max_concurrency: 4,
        ..cap(2)
    });
    let mut held = Vec::new();
    for i in 1..=4 {
        let ip: &'static str = format!("2001:db8:{i}::1").leak();
        held.push(r.hold(via(ip)).await);
    }
    assert_eq!(r.table(), 12, "three scopes for each permit");
    for i in 5..200 {
        let ip: &'static str = format!("2001:db8:{i}::1").leak();
        let got = r.call("/v1/health", via(ip)).await;
        assert_eq!(
            (got.0, code(&got.2)),
            (StatusCode::SERVICE_UNAVAILABLE, Some("overloaded"))
        );
        assert_eq!(r.table(), 12);
    }
    r.release(held).await;
    assert_eq!(r.table(), 0);
    // Many clients one after another leave nothing behind.
    for i in 0..200u32 {
        let ip: &'static str = format!("198.51.{}.{}", i / 250, i % 250 + 1).leak();
        assert_eq!(r.status("/v1/health", via(ip)).await, StatusCode::OK);
    }
    assert_eq!(r.table(), 0);
}

/// A request through [`client_limited`] and the guard is counted once, and
/// the guard still knows whether it is counted.
#[tokio::test]
async fn nl5_3_the_outer_client_layer_does_not_change_the_count() {
    let mut r = rig(cap(1));
    r.app = client_limited(r.f.state.clone(), r.app.clone());
    let a = via("198.51.100.1");
    let held = vec![r.hold(a).await];
    assert_eq!(r.table(), 1);
    assert_refused(&r.call("/v1/health", a).await);
    assert_eq!(r.status("/v1/health", direct(PROXY)).await, StatusCode::OK);
    r.release(held).await;
    assert_eq!(r.table(), 0);
    assert_eq!(r.status("/v1/health", a).await, StatusCode::OK);
}

/// Many clients at once on worker threads: every answer is a 200 or the
/// refusal, no client is ever above its share inside the handler, and the
/// table is empty afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nl5_3_a_concurrent_load_leaves_the_table_empty() {
    const CAP: u32 = 4;
    let f = build(Setup {
        config: ApiConfig {
            trusted_proxies: vec![PROXY.parse().expect("cidr")],
            reads_per_minute: 0,
            ..cap(CAP)
        },
        ..Setup::default()
    });
    let inside: Arc<Vec<AtomicU64>> = Arc::new((0..8).map(|_| AtomicU64::new(0)).collect());
    let worst = Arc::new(AtomicU64::new(0));
    let (seen, top) = (inside.clone(), worst.clone());
    let extra = Router::new().route(
        "/v1/work/{client}",
        axum::routing::get(move |axum::extract::Path(c): axum::extract::Path<usize>| {
            let (seen, top) = (seen.clone(), top.clone());
            async move {
                let now = seen[c].fetch_add(1, Ordering::SeqCst) + 1;
                top.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                seen[c].fetch_sub(1, Ordering::SeqCst);
                "done"
            }
        }),
    );
    let app = router_with(f.state.clone(), extra);
    let mut tasks = Vec::new();
    for t in 0..64usize {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let c = t % 8;
            let ip: &'static str = format!("198.51.100.{}", c + 1).leak();
            let (mut ok, mut refused) = (0u32, 0u32);
            for _ in 0..50 {
                let resp = app
                    .clone()
                    .oneshot(request(&format!("/v1/work/{c}"), via(ip)))
                    .await
                    .expect("response");
                match resp.status() {
                    StatusCode::OK => ok += 1,
                    StatusCode::TOO_MANY_REQUESTS => refused += 1,
                    other => panic!("unexpected status {other}"),
                }
                let _ = resp.into_body().collect().await;
            }
            (ok, refused)
        }));
    }
    let mut served = 0;
    for t in tasks {
        let (ok, refused) = t.await.expect("joined");
        assert_eq!(ok + refused, 50);
        served += ok;
    }
    assert!(served > 0);
    assert!(worst.load(Ordering::SeqCst) <= u64::from(CAP));
    assert_eq!(f.state.clients_in_flight(), 0);
}

/// The refusal of a full node: 503 `overloaded`.
fn assert_overloaded(r: &(StatusCode, HeaderMap, Value)) {
    assert_eq!(
        (r.0, code(&r.2)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("overloaded"))
    );
}

/// At the defaults (32 of 256) one IPv6 allocation cannot hold the node: a
/// /56 holds at most a quarter (64) and a /48 at most a half (128), however
/// many /64s it steps through ([`inflight_scopes`]).
#[tokio::test]
async fn nl5_3_one_ipv6_48_holds_at_most_half_the_node() {
    assert_eq!(
        inflight_scopes("2001:db8:0:1::/64", 32, 256),
        vec![
            ("2001:db8:0:1::/64".to_string(), 32),
            ("2001:db8:0:0::/56".to_string(), 64),
            ("2001:db8:0::/48".to_string(), 128),
        ]
    );
    assert_eq!(
        inflight_scopes("198.51.100.1", 32, 256),
        vec![("198.51.100.1".to_string(), 32)]
    );
    // The wider scopes are never below the client's own share.
    let small: Vec<u32> = inflight_scopes("2001:db8:0:1::/64", 2, 4)
        .into_iter()
        .map(|(_, cap)| cap)
        .collect();
    assert_eq!(small, [2, 2, 2]);

    let mut r = rig(ApiConfig::default());
    let mut held = Vec::new();
    // One /56: two /64s at their share fill it.
    for ip in ["2001:db8:0:1::1", "2001:db8:0:2::1"] {
        for _ in 0..32 {
            held.push(r.hold(via(ip)).await);
        }
        assert_refused(&r.call("/v1/health", via(ip)).await);
    }
    assert_refused(&r.call("/v1/health", via("2001:db8:0:3::1")).await);
    // A second /56 of the same /48 brings the /48 to its half.
    for ip in ["2001:db8:0:101::1", "2001:db8:0:102::1"] {
        for _ in 0..32 {
            held.push(r.hold(via(ip)).await);
        }
    }
    assert_eq!(held.len(), 128);
    // A third /56 holds nothing itself; only the /48 refuses it.
    assert_refused(&r.call("/v1/health", via("2001:db8:0:201::1")).await);
    assert_refused(&r.call("/v1/hold", via("2001:db8:0:ff01::1")).await);
    // Another /48 and the local health check are served.
    assert_eq!(
        r.status("/v1/health", via("2001:db8:1:1::1")).await,
        StatusCode::OK
    );
    assert_eq!(r.status("/v1/health", direct(PROXY)).await, StatusCode::OK);
    r.release(held).await;
    assert_eq!(r.table(), 0);
}

/// A few permits are kept for callers on the host: when remote clients hold
/// every permit they can take, the local health check is still served, no
/// forwarded client reaches the kept permits, and all together are still
/// `max_concurrency`. A full node answers 503 before it looks at the
/// client's share.
#[tokio::test]
async fn nl5_3_local_callers_keep_a_reserve_remote_clients_cannot_take() {
    assert_eq!(local_reserve(256), 4);
    assert_eq!(local_reserve(32), 4);
    assert_eq!(local_reserve(8), 1);
    assert_eq!(local_reserve(4), 0);
    assert_eq!(local_reserve(1), 0);
    let mut r = rig(ApiConfig {
        max_concurrency: 32,
        ..cap(8)
    });
    let local = direct(PROXY);
    let in_process = From {
        peer: None,
        xff: None,
        xff2: None,
    };
    // Remote clients take what they can: 28 of the 32.
    let mut held = Vec::new();
    for c in 1..=3u8 {
        let ip: &'static str = format!("198.51.100.{c}").leak();
        for _ in 0..8 {
            held.push(r.hold(via(ip)).await);
        }
    }
    for _ in 0..4 {
        held.push(r.hold(via("198.51.100.4")).await);
    }
    assert_eq!(held.len(), 28);
    // Full for remote clients, whoever asks: a client below its share, a
    // new one, one at its share (503 comes before 429), a header that names
    // no client, a peer that is no proxy.
    for from in [
        via("198.51.100.4"),
        via("198.51.100.9"),
        via("198.51.100.1"),
        via("2001:db8::1"),
        via("not an address"),
        via(PROXY),
        direct("203.0.113.7"),
    ] {
        assert_overloaded(&r.call("/v1/health", from).await);
        assert_overloaded(&r.call("/v1/hold", from).await);
    }
    assert_eq!(r.f.state.local_inflight.available_permits(), 4);
    // The host's own callers are served from the reserve.
    for _ in 0..3 {
        assert_eq!(r.status("/v1/health", local).await, StatusCode::OK);
        assert_eq!(r.status("/v1/health", in_process).await, StatusCode::OK);
    }
    // The reserve is a part of the whole, not more: 28 + 4 = 32.
    for from in [local, in_process, local, in_process] {
        held.push(r.hold(from).await);
    }
    assert_overloaded(&r.call("/v1/health", local).await);
    assert_overloaded(&r.call("/v1/health", in_process).await);
    assert_eq!(held.len(), 32);
    assert_eq!(r.f.state.inflight.available_permits(), 0);
    assert_eq!(r.f.state.local_inflight.available_permits(), 0);
    r.release(held).await;
    assert_eq!(r.f.state.inflight.available_permits(), 28);
    assert_eq!(r.f.state.local_inflight.available_permits(), 4);
    assert_eq!(r.table(), 0);
    // With room for everyone a local caller takes an ordinary permit.
    let one = r.hold(local).await;
    assert_eq!(r.f.state.local_inflight.available_permits(), 4);
    r.release(vec![one]).await;
}

/// The client is who `client_ip` says: an IPv4-mapped IPv6 address is that
/// IPv4 client, and with two `X-Forwarded-For` lines the last address of
/// the last line counts.
#[tokio::test]
async fn nl5_3_mapped_addresses_and_two_header_lines_name_the_same_client() {
    let mut r = rig(cap(1));
    let a = via("198.51.100.1");
    let held = vec![r.hold(a).await];
    assert_refused(&r.call("/v1/health", via("::ffff:198.51.100.1")).await);
    assert_refused(&r.call("/v1/health", via("[::ffff:198.51.100.1]:443")).await);
    assert_eq!(r.table(), 1, "one key for both spellings");
    let lines = |first, second| From {
        peer: Some(PROXY),
        xff: Some(first),
        xff2: Some(second),
    };
    assert_refused(
        &r.call("/v1/health", lines("198.51.100.77", "198.51.100.1"))
            .await,
    );
    assert_refused(
        &r.call(
            "/v1/health",
            lines("198.51.100.77, 198.51.100.78", "6.6.6.6, 198.51.100.1"),
        )
        .await,
    );
    assert_eq!(
        r.status("/v1/health", lines("198.51.100.1", "198.51.100.77"))
            .await,
        StatusCode::OK
    );
    r.release(held).await;
    // The mapped spelling holds the IPv4 client's place too.
    let held = vec![r.hold(via("::ffff:198.51.100.1")).await];
    assert_refused(&r.call("/v1/health", a).await);
    r.release(held).await;
    assert_eq!(r.table(), 0);
}
