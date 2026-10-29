//! SOAK-2 (L6 soak, docs/audit/2026-10-14-soak.md): the node reads the
//! start makes are retried. Behind a node that failed about a quarter of its
//! calls (the soak's fault proxy), one failed read stopped the start
//! (exit 1), and 59 starts in a row failed.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use onyx_api::roots::{ChainRoots, JsonRpc};
use onyx_api::x402::BoxFuture;
use onyx_node::app::{startup_step, STARTUP_TRIES};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const CODE: &str = "0x6080604052";
const CHAIN: u64 = 84_532;

/// A node whose first `fail` calls fail (as a 503 would), then answers
/// `eth_chainId` and `eth_getCode` honestly.
struct Flaky {
    fail: u32,
    calls: AtomicU32,
}

impl JsonRpc for Flaky {
    fn call<'a>(&'a self, method: &'a str, _params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail;
        Box::pin(async move {
            if n < fail {
                return Err(format!("{method}: node answered HTTP 503"));
            }
            match method {
                "eth_chainId" => Ok(json!(format!("0x{CHAIN:x}"))),
                "eth_getCode" => Ok(json!(CODE)),
                other => Err(format!("unexpected {other}")),
            }
        })
    }
}

impl JsonRpc for &Flaky {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        (**self).call(method, params)
    }
}

fn code_hash() -> onyx_core::Hash32 {
    onyx_core::keccak256(&hex::decode(CODE.trim_start_matches("0x")).unwrap())
}

async fn connect(rpc: &Flaky, tries: u32) -> Result<(), String> {
    startup_step("settlement", tries, Duration::from_millis(1), || async {
        ChainRoots::connect([7; 20], CHAIN, Some(code_hash()), rpc)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
}

/// The regression: two failed node calls at start. One try (the old
/// behaviour) stops the start; the retried step connects.
#[tokio::test]
async fn soak_2_a_failed_node_read_at_start_is_tried_again() {
    let once = Flaky {
        fail: 2,
        calls: AtomicU32::new(0),
    };
    assert!(connect(&once, 1).await.is_err(), "one try fails");
    let rpc = Flaky {
        fail: 2,
        calls: AtomicU32::new(0),
    };
    connect(&rpc, STARTUP_TRIES)
        .await
        .expect("connects after retries");
    // Two failed tries (one call each), then eth_chainId and eth_getCode.
    assert_eq!(rpc.calls.load(Ordering::SeqCst), 4);
}

/// A failure that stays still stops the start, after `STARTUP_TRIES` tries.
#[tokio::test]
async fn a_lasting_failure_still_stops_the_start() {
    let rpc = Flaky {
        fail: u32::MAX,
        calls: AtomicU32::new(0),
    };
    let e = connect(&rpc, STARTUP_TRIES)
        .await
        .expect_err("never connects");
    assert!(e.contains("503"), "{e}");
    assert_eq!(rpc.calls.load(Ordering::SeqCst), STARTUP_TRIES);
}

/// A wrong build (code hash) is not a node error, but it too is refused,
/// whatever the tries.
#[tokio::test]
async fn a_wrong_code_hash_is_refused_after_the_tries() {
    let rpc = Flaky {
        fail: 0,
        calls: AtomicU32::new(0),
    };
    let r = startup_step("settlement", 3, Duration::from_millis(1), || async {
        ChainRoots::connect([7; 20], CHAIN, Some(onyx_core::Hash32::ZERO), &rpc)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await;
    assert!(r.expect_err("refused").contains("not the configured build"));
}
