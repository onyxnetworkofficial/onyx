//! READINESS A9, THREATS USE-5, review USE-R1 M2: the usage counts on a
//! real listener. Every request is counted alike and the stored counts name
//! no caller. No route gives the counts out: `/v1/usage` is the ordinary 404
//! whatever a caller sends, from the host itself too. The operator reads
//! the files with `onyx-node usage`, which asks the node nothing and writes
//! nothing.
//!
//! Time (ROADMAP E12): every socket wait has a cap far above any run; the
//! once-a-minute write runs on a paused clock.

use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use onyx_api::usage::Usage;
use onyx_api::Cidr;
use onyx_node::serve::{serve_with, ServeLimits};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The cap on every wait: far above any run, only a hang reaches it.
const CAP: Duration = Duration::from_secs(30);
/// 2026-11-16 12:00:00 UTC.
const NOW: u64 = 20_773 * 86_400 + 43_200;

static N: AtomicU64 = AtomicU64::new(0);

fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "onyx-node-usage-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Node {
    addr: SocketAddr,
    usage: Option<Arc<Usage>>,
    dir: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

/// A node's router as `app.rs` builds it around the usage counts: a route,
/// a 404 fallback, the counting layer outermost, served behind a trusted
/// proxy on loopback.
async fn node(on: bool) -> Node {
    let dir = state_dir();
    let usage = onyx_node::usage::open(on, &dir, Arc::new(|| NOW));
    let extra = Router::new().route("/v1/health", get(|| async { "ok" }));
    let app = extra.fallback(|| async { (StatusCode::NOT_FOUND, "no such route") });
    let app = onyx_node::usage::counted(usage.as_ref(), app);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let trusted: Vec<Cidr> = vec!["127.0.0.1/32".parse().expect("cidr")];
    let limits = ServeLimits {
        header_read_timeout: CAP,
        max_connections: 64,
        max_per_client: 64,
        write_stall_timeout: CAP,
    };
    let task = tokio::spawn(async move {
        serve_with(listener, app, limits, &trusted, async {
            let _ = rx.await;
        })
        .await
    });
    Node {
        addr,
        usage,
        dir,
        stop: Some(stop),
        task,
    }
}

impl Node {
    /// One request with `headers` (each `Name: value`); the status and body.
    async fn get(&self, path: &str, headers: &[&str]) -> (u16, String) {
        let mut head = format!("GET {path} HTTP/1.1\r\nHost: node\r\nConnection: close\r\n");
        for h in headers {
            head.push_str(h);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        let answer = tokio::time::timeout(CAP, async {
            let mut s = TcpStream::connect(self.addr).await.expect("connect");
            s.write_all(head.as_bytes()).await.expect("write");
            let mut out = Vec::new();
            s.read_to_end(&mut out).await.expect("read");
            String::from_utf8(out).expect("utf8")
        })
        .await
        .expect("an answer");
        let status = answer
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("status");
        let body = answer.split_once("\r\n\r\n").map_or("", |(_, b)| b);
        (status, body.to_string())
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(CAP, &mut self.task)
            .await
            .expect("the server stops")
            .expect("join")
            .expect("serve");
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `onyx-node usage` as the operator runs it.
fn cli(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_onyx-node"))
        .arg("usage")
        .args(args)
        .env_clear()
        .output()
        .expect("run onyx-node usage");
    (
        out.status.code().expect("exit code"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every file under `dir` with its bytes and modification time.
fn snapshot(dir: &std::path::Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).expect("dir") {
        let p = e.expect("entry").path();
        let m = std::fs::metadata(&p)
            .expect("meta")
            .modified()
            .expect("mtime");
        out.push((p.clone(), std::fs::read(&p).expect("read"), m));
    }
    out.sort();
    out
}

#[tokio::test]
async fn no_route_gives_the_counts_out_and_the_operator_reads_the_files() {
    let n = node(true).await;
    // Visitors, through the proxy (which adds X-Forwarded-For) and a local
    // service (no forwarding header): all counted.
    for _ in 0..6 {
        let (s, _) = n
            .get(
                "/v1/health?who=VISITORMARK",
                &["X-Forwarded-For: 203.0.113.50", "User-Agent: AgentMark"],
            )
            .await;
        assert_eq!(s, 200);
    }
    assert_eq!(n.get("/v1/health", &[]).await.0, 200);

    // Review USE-R1 M2: there is no read route. From the host itself, with
    // the header the removed route took, with or without forwarding
    // headers: the 404 of a path that is no route.
    for headers in [
        vec![],
        vec!["X-Onyx-Operator: usage"],
        vec!["X-Forwarded-For: 203.0.113.50"],
        vec!["X-Forwarded-For: 127.0.0.1", "X-Onyx-Operator: usage"],
    ] {
        for path in ["/v1/usage", "/v1/usage?days=1", "/v1/usage/"] {
            let (s, body) = n.get(path, &headers).await;
            assert_eq!(s, 404, "{path} {headers:?}");
            assert_eq!(body, "no such route", "{path} {headers:?}");
        }
    }

    // Every request above was counted, and the stored file names no caller.
    let usage = n.usage.clone().expect("on");
    usage.flush().expect("flush");
    let text = std::fs::read_to_string(n.dir.join("usage/2026-11-16.json")).expect("file");
    for mark in [
        "127.0.0.1",
        "203.0.113",
        "VISITORMARK",
        "AgentMark",
        "Onyx-Operator",
        "node",
    ] {
        assert!(!text.contains(mark), "{mark} in {text}");
    }
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(
        v["routes"],
        json!({
            "GET /v1/health": {"2xx": 7, "3xx": 0, "4xx": 0, "5xx": 0},
            "GET (no route)": {"2xx": 0, "3xx": 0, "4xx": 12, "5xx": 0},
        })
    );

    // The operator's read: the files, through the node's own binary.
    let state = n.dir.to_str().expect("path");
    let before = snapshot(&n.dir.join("usage"));
    let (code, out, err) = cli(&["--days", "3", "--until", "2026-11-16", "--state-dir", state]);
    assert_eq!(code, 0, "{err}");
    let r: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(r["from"], json!("2026-11-14"));
    assert_eq!(r["to"], json!("2026-11-16"));
    assert_eq!(r["totals"]["routes"]["GET /v1/health"]["2xx"], json!(7));
    assert!(r["note"].as_str().expect("note").contains("not of people"));
    // It wrote nothing and touched nothing.
    assert_eq!(snapshot(&n.dir.join("usage")), before);
    // What the node has not written yet is not in it.
    assert_eq!(n.get("/v1/health", &[]).await.0, 200);
    let (_, out, _) = cli(&["--until", "2026-11-16", "--state-dir", state]);
    let r: Value = serde_json::from_str(&out).expect("json");
    assert_eq!(r["totals"]["routes"]["GET /v1/health"]["2xx"], json!(7));
    n.stop().await;
}

#[test]
fn the_reader_refuses_bad_arguments_and_says_when_there_is_nothing() {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).expect("dir");
    let state = dir.to_str().expect("path");
    for args in [
        &["--days", "0", "--state-dir", state][..],
        &["--days", "91", "--state-dir", state],
        &["--days", "7x", "--state-dir", state],
        &["--days", "007", "--state-dir", state],
        &["--days"],
        &["--until", "2026-13-01", "--state-dir", state],
        &["--until", "yesterday", "--state-dir", state],
        &["--url", "http://127.0.0.1:8080", "--state-dir", state],
        // No state directory named, and none in the (cleared) environment.
        &["--days", "7"],
        &["--state-dir", " "],
    ] {
        let (code, out, err) = cli(args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?} printed a report: {out}");
        assert!(err.contains("usage: onyx-node usage"), "{args:?}: {err}");
    }
    // A state directory without counts (ONYX_USAGE off): exit 1, a reason,
    // nothing made.
    let (code, out, err) = cli(&["--state-dir", state]);
    assert_eq!(code, 1, "{out}{err}");
    assert!(out.is_empty());
    assert!(err.contains("ONYX_USAGE=on"), "{err}");
    assert_eq!(std::fs::read_dir(&dir).expect("dir").count(), 0);
    let (code, out, _) = cli(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("writes nothing"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn off_means_no_counts_and_no_file() {
    let n = node(false).await;
    assert!(n.usage.is_none());
    assert_eq!(n.get("/v1/health", &[]).await.0, 200);
    let (s, body) = n.get("/v1/usage", &[]).await;
    assert_eq!(s, 404);
    assert_eq!(body, "no such route");
    assert!(!n.dir.exists(), "nothing is written when off");
    n.stop().await;
}

/// The counts are written once a minute without any request or restart.
#[tokio::test(start_paused = true)]
async fn the_counts_are_written_every_minute() {
    let dir = state_dir();
    let usage = onyx_node::usage::open(true, &dir, Arc::new(|| NOW)).expect("on");
    let task = tokio::spawn(onyx_node::usage::run(
        usage.clone(),
        onyx_node::usage::FLUSH_EVERY,
    ));
    usage.record(
        &axum::http::Method::GET,
        Some("/v1/health"),
        StatusCode::OK,
        None,
    );
    let file = dir.join("usage/2026-11-16.json");
    // Not before the minute is over.
    tokio::time::sleep(onyx_node::usage::FLUSH_EVERY - Duration::from_secs(1)).await;
    assert!(!file.exists());
    // Then within a few (paused-clock) seconds: the write is a blocking
    // task, so wait for the file, not for a time.
    let mut waited = 0;
    while !file.exists() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        waited += 1;
        assert!(waited < 100_000, "the counts were never written");
    }
    let v: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("file")).expect("json");
    assert_eq!(v["routes"]["GET /v1/health"]["2xx"], json!(1));
    task.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
