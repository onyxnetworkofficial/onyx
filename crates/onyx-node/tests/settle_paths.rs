//! Settle paths of the service (review 2 of the packaging, OPS-L2): the
//! facilitator HTTP client waits the whole settle patience, the node URL
//! file is never echoed, and settle guards survive panics and churn.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use axum::routing::post;
use axum::Router;
use onyx_api::x402::{
    Facilitator, FacilitatorRequest, HttpFacilitator, HyperPost, PaymentPayload, PaymentTerms,
};
use onyx_api::{ApiConfig, SettleTasks, SETTLE_PATIENCE};
use onyx_node::config::NodeConfig;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn req() -> FacilitatorRequest {
    let terms = PaymentTerms {
        network: "eip155:84532".into(),
        asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
        pay_to: "0x1111111111111111111111111111111111111111".into(),
        max_timeout_seconds: 60,
        extra: None,
    };
    let r = terms.requirements(1000);
    FacilitatorRequest {
        x402_version: 2,
        payment_payload: PaymentPayload {
            x402_version: 2,
            resource: None,
            accepted: r.clone(),
            payload: serde_json::json!({}),
        },
        payment_requirements: r,
    }
}

/// A local facilitator that holds every settle's answer until `release`
/// turns true (it "broadcasts" first: `landed` counts the settles received).
async fn held_facilitator(
    landed: Arc<AtomicUsize>,
    release: tokio::sync::watch::Receiver<bool>,
) -> String {
    let app = Router::new().route(
        "/settle",
        post(move || {
            let (l, mut go) = (landed.clone(), release.clone());
            async move {
                l.fetch_add(1, Ordering::SeqCst);
                let _ = go.wait_for(|go| *go).await;
                axum::Json(serde_json::json!({
                    "success": true, "transaction": "0xabc", "network": "eip155:84532"
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

/// OPS-L2-F1 (a): a settle slower than the verify timeout but within the
/// settle patience reaches its answer through the client the server uses
/// (`HyperPost::for_facilitator`); a client built with the verify timeout
/// (the old wiring) cut it short after the transfer had landed.
///
/// No race with the machine (ROADMAP E12): the facilitator answers only
/// after an old-wiring client, started after the fixed one, has given up, so
/// the fixed client has waited past the verify timeout by construction; its
/// own limit, the patience (4 x 2 s), leaves 6 s for the old client's request
/// and the answer. (The 150 ms version raced a 400 ms answer against 600 ms.)
#[tokio::test(flavor = "multi_thread")]
async fn the_facilitator_client_waits_the_whole_settle_patience() {
    let cfg = ApiConfig {
        facilitator_timeout: Duration::from_secs(2),
        ..ApiConfig::default()
    };
    let patience = cfg.facilitator_timeout * SETTLE_PATIENCE;
    let landed = Arc::new(AtomicUsize::new(0));
    let (go, release) = tokio::sync::watch::channel(false);
    let url = held_facilitator(landed.clone(), release).await;
    let fixed = HttpFacilitator::new(url.clone(), HyperPost::for_facilitator(&cfg));
    let t = Instant::now();
    let waiting = tokio::spawn(async move { fixed.settle(&req()).await });
    // The fixed client's settle has reached the facilitator (cap: 60 s).
    tokio::time::timeout(Duration::from_secs(60), async {
        while landed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the settle reached the facilitator");
    let old = HttpFacilitator::new(url, HyperPost::new(cfg.facilitator_timeout));
    let r = tokio::time::timeout(patience, old.settle(&req()))
        .await
        .expect("within the patience");
    assert!(r.is_err(), "the verify timeout cut the settle");
    assert!(
        landed.load(Ordering::SeqCst) >= 1,
        "the transfer had landed"
    );
    go.send(true).expect("release");
    let r = tokio::time::timeout(patience, waiting)
        .await
        .expect("within the patience")
        .expect("task");
    assert!(
        r.expect("answered after the verify timeout").success,
        "took {:?}",
        t.elapsed()
    );
}

fn base(rpc_file: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("ONYX_ENV", "testnet".to_string()),
        ("ONYX_RIGHTS", "cleared".into()),
        ("ONYX_PUBLIC_URL", "https://api.example.test/".into()),
        ("ONYX_RPC_URL_FILE", rpc_file.into()),
        ("ONYX_DEPLOYMENT", "/etc/onyx/deployment.json".into()),
        ("ONYX_CATALOG", "/etc/onyx/catalog.json".into()),
        ("ONYX_CAS_DIR", "/var/lib/onyx/cas".into()),
        ("ONYX_STATE_DIR", "/var/lib/onyx/state".into()),
    ])
}

fn load(m: &BTreeMap<&str, String>) -> Result<NodeConfig, String> {
    NodeConfig::from_env(|k| m.get(k).cloned(), false)
}

fn file(dir: &std::path::Path, name: &str, body: &str, mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p.display().to_string()
}

/// OPS-L2-F4 (review 2): ONYX_RPC_URL_FILE edge cases; a bad value is never
/// echoed.
#[test]
fn rpc_url_file_edge_cases() {
    let dir = std::env::temp_dir().join(format!("onyx-l2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Trailing LF and CRLF are trimmed.
    for (n, body) in [
        ("lf", "https://n.example/KEYLF\n"),
        ("crlf", "https://n.example/KEYCR\r\n"),
    ] {
        let c = load(&base(&file(&dir, n, body, 0o600))).expect(n);
        assert!(!c.rpc_url.ends_with(['\n', '\r']));
    }
    // A systemd credential is 0400: accepted. 0640 refused.
    assert!(load(&base(&file(&dir, "ro", "https://n.example/K", 0o400))).is_ok());
    let e = load(&base(&file(&dir, "grp", "https://n.example/K", 0o640))).unwrap_err();
    assert!(e.contains("chmod 600"), "{e}");
    // Two lines refused; empty refused.
    assert!(load(&base(&file(
        &dir,
        "two",
        "https://a.example/K\nhttps://b/K",
        0o600
    )))
    .is_err());
    assert!(load(&base(&file(&dir, "empty", "\n", 0o600))).is_err());
    // Missing file and a directory: errors name the path only.
    assert!(load(&base(&dir.join("nope").display().to_string())).is_err());
    assert!(load(&base(&dir.display().to_string())).is_err());
    // Both set: refused, whichever is valid. Empty ONYX_RPC_URL + file: the file.
    let f = file(&dir, "both", "https://n.example/K", 0o600);
    let mut m = base(&f);
    m.insert("ONYX_RPC_URL", "https://other.example".into());
    assert!(load(&m).unwrap_err().contains("not both"));
    m.insert("ONYX_RPC_URL", "  ".into());
    assert_eq!(load(&m).unwrap().rpc_url, "https://n.example/K");
    // Wrong content: does the error echo the secret?
    let e = load(&base(&file(&dir, "bare", "SECRETAPIKEY123", 0o600))).unwrap_err();
    eprintln!("bare key error: {e}");
    let e2 = load(&base(&file(
        &dir,
        "nosch",
        "n.example/v2/SECRETPATH",
        0o600,
    )))
    .unwrap_err();
    eprintln!("no-scheme error: {e2}");
    let e3 = load(&base(&file(
        &dir,
        "q",
        "https://n.example/?apikey=SECRETQ",
        0o600,
    )))
    .unwrap_err();
    eprintln!("query error: {e3}");
    std::fs::remove_dir_all(&dir).unwrap();
    // OPS-L2-F4: no secret is echoed; the error names the file setting.
    for err in [&e, &e2, &e3] {
        assert!(
            !err.contains("SECRET"),
            "the error echoes the file content: {err}"
        );
        assert!(err.contains("ONYX_RPC_URL_FILE"), "{err}");
    }
}

/// Review 2: settle guards are dropped by a panic and an abort; no lost
/// wake-up under churn; a wait for a task that never ends is bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settle_guards_survive_panic_abort_and_churn() {
    let t = Arc::new(SettleTasks::default());
    let g = t.enter();
    let h = tokio::spawn(async move {
        let _g = g;
        panic!("settle panicked");
    });
    assert!(h.await.is_err());
    let g = t.enter();
    let h = tokio::spawn(async move {
        let _g = g;
        std::future::pending::<()>().await;
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(t.live(), 1);
    h.abort();
    let _ = h.await;
    assert!(t.wait_idle(Duration::from_secs(1)).await);
    // Churn: 2000 short tasks; the wait ends promptly after the last.
    let mut hs = Vec::new();
    for i in 0..2000u64 {
        let g = t.enter();
        hs.push(tokio::spawn(async move {
            let _g = g;
            tokio::time::sleep(Duration::from_micros(i % 500)).await;
        }));
    }
    // Woken by the last task's end, not by the wait's own limit: a lost
    // wake-up returns only at the limit (E12: the limit, not a guess at this
    // machine's speed, is the bound; 60 s is far above any run).
    let limit = Duration::from_secs(60);
    let s = Instant::now();
    assert!(t.wait_idle(limit).await);
    assert!(s.elapsed() < limit, "woken by the last task, not the limit");
    assert_eq!(t.live(), 0);
    for h in hs {
        h.await.unwrap();
    }
}

/// Review 2: a wait for a task that never ends is bounded (no hang): it
/// returns false at its limit. On paused time (E12) the limit is exact.
#[tokio::test(start_paused = true)]
async fn a_wait_for_a_task_that_never_ends_is_bounded() {
    let t = Arc::new(SettleTasks::default());
    let g = t.enter();
    let _h = tokio::spawn(async move {
        let _g = g;
        std::future::pending::<()>().await;
    });
    let limit = Duration::from_millis(200);
    let s = tokio::time::Instant::now();
    assert!(!t.wait_idle(limit).await);
    let took = s.elapsed();
    assert!(
        took >= limit && took <= limit + Duration::from_millis(1),
        "{took:?}"
    );
}
