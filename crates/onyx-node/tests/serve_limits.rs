//! AUD-1-F5 / AUD-1R-F1: the server loop closes a connection that does not
//! send its request headers in time (slowloris), shares its connection cap
//! fairly (a per-client cap, and a full cap evicts the oldest connection that
//! has not sent a request), passes the peer address to handlers, and stops
//! gracefully.
//!
//! Time (ROADMAP E12): these run real sockets on real time, so no check
//! guesses how fast this machine is. A slow handler holds its answer until
//! the test releases it, the test waits for handlers to start instead of
//! sleeping, order on one listener comes from its accept queue (first in,
//! first out), and every wait is a condition with a cap (`CAP`) far above
//! any run; a timeout's lower bound is checked, its upper bound is the cap.

use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::Router;
use onyx_api::Cidr;
use onyx_node::serve::{serve_with, ServeLimits};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

/// The cap on every wait: far above any run, only a hang reaches it.
const CAP: Duration = Duration::from_secs(30);

/// `/slow`'s handlers: how many have started, and the gate that ends them.
#[derive(Clone)]
struct Slow {
    started: Arc<AtomicUsize>,
    done: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Slow {
    /// Wait until `n` slow requests are in their handler (mid-request).
    async fn started(&self, n: usize) {
        tokio::time::timeout(CAP, async {
            while self.started.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the slow requests reached their handler");
    }

    /// Let every slow handler answer.
    fn finish(&self) {
        self.done.send_replace(true);
    }
}

async fn start(
    limits: ServeLimits,
) -> (
    SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
    Slow,
) {
    start_with(limits, Vec::new()).await
}

async fn start_with(
    limits: ServeLimits,
    trusted: Vec<Cidr>,
) -> (
    SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
    Slow,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let slow = Slow {
        started: Arc::new(AtomicUsize::new(0)),
        done: Arc::new(tokio::sync::watch::channel(false).0),
    };
    let gate = slow.clone();
    let app = Router::new()
        .route(
            "/",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        )
        .route(
            "/slow",
            get(move || {
                let gate = gate.clone();
                async move {
                    let mut done = gate.done.subscribe();
                    gate.started.fetch_add(1, Ordering::SeqCst);
                    let _ = done.wait_for(|d| *d).await;
                    "slow"
                }
            }),
        );
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let h = tokio::spawn(async move {
        serve_with(listener, app, limits, &trusted, async {
            let _ = rx.await;
        })
        .await
    });
    (addr, tx, h, slow)
}

/// Connect from the loopback address `from` (any 127.x works on Linux).
async fn connect_from(from: &str, to: SocketAddr) -> TcpStream {
    let sock = TcpSocket::new_v4().expect("socket");
    sock.bind(format!("{from}:0").parse().expect("from"))
        .expect("bind from");
    sock.connect(to).await.expect("connect")
}

/// True when the server closed the connection within `wait`.
async fn closed_within(s: &mut TcpStream, wait: Duration) -> bool {
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, s.read(&mut buf)).await {
            Err(_) => return false,
            Ok(Ok(0)) | Ok(Err(_)) => return true,
            Ok(Ok(_)) => continue,
        }
    }
}

/// One complete request; the response, or `None` if refused or cut.
async fn get_once(mut s: TcpStream, path: &str) -> Option<String> {
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .ok()?;
    let body = read_all(&mut s, CAP).await?;
    let text = String::from_utf8_lossy(&body).into_owned();
    text.starts_with("HTTP/1.1 200").then_some(text)
}

const fn limits(timeout_ms: u64, max: usize, per_client: usize) -> ServeLimits {
    ServeLimits {
        header_read_timeout: Duration::from_millis(timeout_ms),
        max_connections: max,
        max_per_client: per_client,
    }
}

async fn read_all(s: &mut TcpStream, wait: Duration) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    tokio::time::timeout(wait, s.read_to_end(&mut buf))
        .await
        .ok()?
        .ok()?;
    Some(buf)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_is_served_with_its_peer_address() {
    let (addr, stop, h, _) = start(limits(5_000, 8, 8)).await;
    let mut s = TcpStream::connect(addr).await.expect("connect");
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let body = read_all(&mut s, CAP).await.expect("response");
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("127.0.0.1"), "{text}");
    let _ = stop.send(());
    tokio::time::timeout(CAP, h)
        .await
        .expect("stops")
        .expect("task")
        .expect("serve");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_header_is_cut_off() {
    let timeout = Duration::from_millis(300);
    let (addr, stop, _h, _) = start(limits(300, 8, 8)).await;
    // Before the connect: the server's timer starts at its accept, later.
    let started = std::time::Instant::now();
    let mut s = TcpStream::connect(addr).await.expect("connect");
    // Half a request, then nothing: the server closes it after the timeout.
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n")
        .await
        .expect("write");
    let closed = read_all(&mut s, CAP).await;
    assert!(closed.is_some(), "connection closed by the server");
    let took = started.elapsed();
    // Cut by the header timeout, not at once and not left open (CAP).
    assert!(took >= timeout, "closed by the header timeout: {took:?}");
    let _ = stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_cap_evicts_idle_connections_for_a_real_request() {
    // The header timeout is far above CAP: a close within CAP is the
    // eviction, never the timeout.
    let (addr, stop, _h, _) = start(limits(120_000, 2, 8)).await;
    // Two idle connections hold both slots; a client that sends its request
    // at once is still served (the oldest idle one is evicted). The listener
    // accepts a, b and c in that order (its queue), however late it runs.
    let mut a = TcpStream::connect(addr).await.expect("a");
    let _b = TcpStream::connect(addr).await.expect("b");
    let c = TcpStream::connect(addr).await.expect("c");
    assert!(
        get_once(c, "/").await.is_some(),
        "served despite the idle pair"
    );
    assert!(
        closed_within(&mut a, CAP).await,
        "the oldest idle connection was evicted"
    );
    let _ = stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_cap_of_busy_connections_refuses_new_ones() {
    let (addr, stop, _h, slow) = start(limits(10_000, 2, 8)).await;
    // Both slots are mid-request (a slow handler): nothing is evictable, so
    // a third connection is closed without an answer, and both busy
    // requests still complete.
    let a = tokio::spawn(get_once(
        TcpStream::connect(addr).await.expect("a"),
        "/slow",
    ));
    let b = tokio::spawn(get_once(
        TcpStream::connect(addr).await.expect("b"),
        "/slow",
    ));
    slow.started(2).await;
    let c = TcpStream::connect(addr).await.expect("c");
    assert!(
        get_once(c, "/").await.is_none(),
        "over the cap: no response"
    );
    slow.finish();
    assert!(a.await.expect("a").is_some(), "busy request a completes");
    assert!(b.await.expect("b").is_some(), "busy request b completes");
    let _ = stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_peer_cannot_starve_honest_clients() {
    // The AUD-1R-F1 probe: one peer re-opens idle sockets to hold every
    // slot; an honest client is served every time.
    // The header timeout is far above any run: only eviction frees a slot
    // here, and the honest client's request is never cut by a slow machine.
    let (addr, stop, _h, _) = start(limits(120_000, 2, 2)).await;
    let mut refused = 0;
    for _ in 0..5 {
        // Accepted in order (the listener's queue): the pair, then c.
        let _a = connect_from("127.0.0.2", addr).await;
        let _b = connect_from("127.0.0.2", addr).await;
        let c = connect_from("127.0.0.3", addr).await;
        if get_once(c, "/").await.is_none() {
            refused += 1;
        }
    }
    let _ = stop.send(());
    assert_eq!(refused, 0, "an honest client was starved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_holds_at_most_its_share_but_a_trusted_proxy_is_exempt() {
    let (addr, stop, _h, slow) = start(limits(10_000, 16, 2)).await;
    // Two busy connections from one client fill its share: a third from it
    // is closed, another client is still served.
    let a = tokio::spawn(get_once(connect_from("127.0.0.2", addr).await, "/slow"));
    let b = tokio::spawn(get_once(connect_from("127.0.0.2", addr).await, "/slow"));
    slow.started(2).await;
    assert!(
        get_once(connect_from("127.0.0.2", addr).await, "/")
            .await
            .is_none(),
        "a third connection from one client is closed"
    );
    assert!(get_once(connect_from("127.0.0.3", addr).await, "/")
        .await
        .is_some());
    slow.finish();
    assert!(a.await.expect("a").is_some() && b.await.expect("b").is_some());
    let _ = stop.send(());

    // Behind a trusted proxy every client shares its address: no per-client cap.
    let proxy: Cidr = "127.0.0.2".parse().expect("cidr");
    let (addr, stop, _h, gate) = start_with(limits(10_000, 16, 2), vec![proxy]).await;
    let slow: Vec<_> = futures_join(addr, &gate).await;
    assert!(
        get_once(connect_from("127.0.0.2", addr).await, "/")
            .await
            .is_some(),
        "a trusted proxy is exempt"
    );
    gate.finish();
    for h in slow {
        assert!(h.await.expect("slow").is_some());
    }
    let _ = stop.send(());
}

/// Three busy connections from the trusted proxy address.
async fn futures_join(
    addr: SocketAddr,
    slow: &Slow,
) -> Vec<tokio::task::JoinHandle<Option<String>>> {
    let mut out = Vec::new();
    for _ in 0..3 {
        let s = connect_from("127.0.0.2", addr).await;
        out.push(tokio::spawn(get_once(s, "/slow")));
    }
    slow.started(3).await;
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_keep_alive_and_a_missing_body_are_closed() {
    let (addr, stop, _h, _) = start(limits(300, 2, 2)).await;
    // Idle after a request (keep-alive): closed by the header timeout.
    let mut s = TcpStream::connect(addr).await.expect("connect");
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .expect("write");
    assert!(closed_within(&mut s, CAP).await, "keep-alive");
    // Headers complete, then a declared body that never arrives.
    let mut s = TcpStream::connect(addr).await.expect("connect");
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000\r\n\r\n")
        .await
        .expect("write");
    assert!(closed_within(&mut s, CAP).await, "missing body");
    let _ = stop.send(());
}
