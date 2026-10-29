//! OPS-L1-F1: a clean stop waits for settle tasks, not only for connections.
//!
//! The handler copies onyx-api's `finish`: the settle runs in its own task
//! (it outlives the request deadline, M7-L39-F1) and holds a `SettleGuard`.
//! The stop runs onyx-node's real `shutdown::drain`; only then is the
//! runtime dropped, as `main` does. Before the fix the stop reported
//! "drained" when the connection closed, and dropping the runtime cancelled
//! the settle before its receipt step ran.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use axum::routing::get;
use axum::Router;
use onyx_api::SettleTasks;
use onyx_node::shutdown;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_millis(300);
const SETTLE_MS: u64 = 1_500;
/// The drain's budgets: caps far above the 0.3 s request and the 1.5 s
/// settle (the drain returns as soon as both end), so a loaded machine
/// cannot run them out (ROADMAP E12).
const SETTLE_CAP: Duration = Duration::from_secs(60);

/// The stop is sent once the request is in its handler (a signal, not a
/// guessed sleep, E12). A tracked settle finishes `SETTLE_MS` after that; an
/// untracked one never does before the runtime drops, so the untracked case
/// cannot record by being fast or slow.
fn run(track: bool) -> (shutdown::Stopped, bool) {
    let recorded = Arc::new(AtomicBool::new(false));
    let tasks = Arc::new(SettleTasks::default());
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_rx = entered.clone();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (rec, t, ent) = (recorded.clone(), tasks.clone(), entered.clone());
    let stopped = rt.block_on(async move {
        let app = Router::new().route(
            "/paid",
            get(move || {
                let (rec, t, ent) = (rec.clone(), t.clone(), ent.clone());
                async move {
                    // finish(): the guard moves into the settle task.
                    let running = track.then(|| t.enter());
                    let task = tokio::spawn(async move {
                        let _running = running;
                        // A slow facilitator settle (or a self-settle);
                        // untracked, it outlasts the runtime.
                        if track {
                            tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                        // record_off_thread(&receipts, receipt)
                        rec.store(true, Ordering::SeqCst);
                    });
                    ent.notify_one();
                    // The request deadline fires first.
                    match tokio::time::timeout(REQUEST_TIMEOUT, task).await {
                        Ok(_) => "ok",
                        Err(_) => "timeout",
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
        });
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.write_all(b"GET /paid HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut b = String::new();
            let _ = s.read_to_string(&mut b);
            b
        });
        entered_rx.notified().await;
        // SIGTERM while the settle runs.
        let _ = stop_tx.send(());
        let stopped = shutdown::drain(server, &tasks, SETTLE_CAP, SETTLE_CAP).await;
        let body = tokio::task::spawn_blocking(move || client.join().unwrap())
            .await
            .unwrap();
        assert!(body.contains("timeout"), "{body}");
        stopped
    });
    // main() returns: the runtime is dropped, cancelling anything left.
    drop(rt);
    std::thread::sleep(Duration::from_millis(SETTLE_MS + 500));
    (stopped, recorded.load(Ordering::SeqCst))
}

#[test]
fn a_stop_waits_for_the_settle_and_its_receipt() {
    let (stopped, recorded) = run(true);
    assert!(stopped.clean(), "{stopped:?}");
    assert!(
        recorded,
        "the receipt step ran before the runtime was dropped"
    );
}

/// The reviewer's case: an untracked settle is cancelled when the runtime
/// drops, though the connections drained. This is why every settle holds a
/// guard.
#[test]
fn an_untracked_settle_is_cut_short() {
    let (stopped, recorded) = run(false);
    assert!(stopped.requests_drained);
    assert!(!recorded, "untracked: the receipt step never ran");
}

/// The shipped stop timeout covers the longest stop of the default config:
/// self-settle, whose budget is its deadline plus node slack, after the
/// request timeout.
#[test]
fn the_supervisor_stop_timeout_covers_the_default_stop() {
    use onyx_api::evm_tx::Sender;
    use onyx_api::roots::HttpJsonRpc;
    use onyx_api::self_settle::SelfSettle;
    use onyx_api::x402::{Facilitator, HyperPost, BASE_SEPOLIA};
    // Never called: building the facilitator needs no network.
    let rpc = HttpJsonRpc::new(
        "https://sepolia.base.org".to_string(),
        HyperPost::for_rpc(Duration::from_secs(1)),
    );
    let sender = Sender::new(rpc, 84_532, [0x11; 32]).expect("test key");
    let f = SelfSettle::new(
        sender,
        [0x5e; 20],
        [0x36; 20],
        BASE_SEPOLIA.to_string(),
        shutdown::SELF_SETTLE_DEADLINE,
    );
    let config = onyx_api::ApiConfig::default();
    let patience = config.facilitator_timeout * onyx_api::SETTLE_PATIENCE;
    let settle = f
        .settle_budget()
        .expect("self-settle has a budget")
        .max(patience);
    let total = shutdown::total(config.request_timeout + settle);
    assert!(
        total < shutdown::SUPERVISOR_STOP,
        "{total:?} vs {:?}",
        shutdown::SUPERVISOR_STOP
    );
}

/// M7-F5 residual: in facilitator mode an unknown settle is followed by up
/// to two authorization checks and one re-send; the stop budget counts
/// them, and the default still fits the shipped supervisor stop timeout.
#[test]
fn the_supervisor_stop_timeout_covers_the_checked_facilitator_settle() {
    use onyx_api::catalog::Catalog;
    use onyx_api::roots::LedgerRoots;
    use onyx_api::settle_check::ChainSettleCheck;
    use onyx_api::x402::{Facilitator, HttpFacilitator, HyperPost, PaymentTerms, BASE_SEPOLIA};
    use onyx_api::{ApiConfig, ApiState, Payments};
    let config = ApiConfig::default();
    // Never called: nothing here touches the network.
    let rpc = onyx_api::roots::HttpJsonRpc::new(
        "https://sepolia.base.org".to_string(),
        HyperPost::for_rpc(Duration::from_secs(1)),
    );
    let f = HttpFacilitator::new(
        "https://x402.org/facilitator".to_string(),
        HyperPost::for_facilitator(&config),
    );
    let state = ApiState::new(
        Catalog::new(),
        Arc::new(LedgerRoots::new()),
        Arc::new(onyx_coordinator::cas::MemCas::new()),
        Some(Payments {
            terms: PaymentTerms {
                network: BASE_SEPOLIA.into(),
                asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
                pay_to: "0x5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e".into(),
                max_timeout_seconds: 60,
                extra: None,
            },
            facilitator: Arc::new(f) as Arc<dyn Facilitator>,
        }),
        Arc::new(|| 0),
        "https://api.example.test",
        config.clone(),
    )
    .expect("state");
    let unchecked = state.stop_budget();
    let state =
        state.with_settle_check(Arc::new(ChainSettleCheck::new(rpc, [0x36; 20], [0x5e; 20])));
    let patience = config.facilitator_timeout * onyx_api::SETTLE_PATIENCE;
    // Two settles, two checks and the wait before the second check
    // (M7L3-R1-1): 50 s with the defaults.
    assert_eq!(
        state.settle_task_budget(),
        patience * 2 + state.settle_check_budget() * 2 + state.settle_recheck_wait()
    );
    assert_eq!(state.settle_recheck_wait(), Duration::from_secs(2));
    assert_eq!(state.settle_task_budget(), Duration::from_secs(50));
    assert!(state.stop_budget() > unchecked);
    let total = shutdown::total(state.stop_budget());
    assert!(
        total < shutdown::SUPERVISOR_STOP,
        "{total:?} vs {:?}",
        shutdown::SUPERVISOR_STOP
    );
}
