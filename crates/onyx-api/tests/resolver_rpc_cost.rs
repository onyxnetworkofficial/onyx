//! OPS-L5-F2 (review 5): the intent resolver's RPC cost per pass is bounded,
//! and a rate-limiting operator node slows drops but never stops them or turns
//! an unused authorization into a receipt (fail-safe: an intent is kept on any
//! node error, run-api.md).
use onyx_api::intents::{Intent, IntentLog, IntentResolver, MAX_INTENTS_PER_PASS};
use onyx_api::receipts::{Receipt, ReceiptLog};
use onyx_api::roots::JsonRpc;
use onyx_api::x402::BoxFuture;
use onyx_core::Hash32;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const T0: u64 = 1_700_000_000;
/// Base Sepolia: 2 s blocks.
fn ts(n: u64) -> u64 {
    T0 + 2 * n
}

struct Chain {
    head: u64,
    calls: Mutex<BTreeMap<String, u64>>,
    /// Fail every k-th call (a rate-limiting public node); 0 = never.
    fail_every: u64,
    n: AtomicU64,
}

fn q(v: &Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16).unwrap_or(0)
}

impl JsonRpc for Chain {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        *self
            .calls
            .lock()
            .expect("test setup")
            .entry(method.to_string())
            .or_default() += 1;
        let k = self.n.fetch_add(1, Ordering::Relaxed) + 1;
        // LCG hash of the call index -> pseudo-random, so failures do not
        // align to a fixed stride.
        let h = k
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
            >> 33;
        let r = if self.fail_every > 0 && h.is_multiple_of(self.fail_every) {
            Err("429 Too Many Requests".to_string())
        } else {
            match method {
                "eth_blockNumber" => Ok(json!(format!("0x{:x}", self.head))),
                "eth_getBlockByNumber" => {
                    let n = q(&params[0]);
                    Ok(json!({ "timestamp": format!("0x{:x}", ts(n)) }))
                }
                // Every authorization unused (payer drained its balance
                // before our settle: the settle reverted).
                "eth_call" => Ok(json!(format!("0x{:064x}", 0))),
                "eth_getLogs" => Ok(json!([])),
                _ => Err(format!("unexpected {method}")),
            }
        };
        Box::pin(async move { r })
    }
}

fn intent(i: u32, valid_before: u64) -> Intent {
    let mut payer = [0u8; 20];
    payer[..4].copy_from_slice(&i.to_be_bytes());
    let mut nonce = [0u8; 32];
    nonce[..4].copy_from_slice(&i.to_be_bytes());
    Intent::new(
        Receipt {
            round: 1,
            shard: Hash32([1; 32]),
            amount: 1_000,
            payer,
            nonce,
        },
        valid_before - 600,
        valid_before,
        valid_before - 60,
        360,
    )
}

/// Open intents after each of six passes over `n` expired-unused intents.
fn run(n: u32, head_after_expiry_secs: u64, fail_every: u64) -> (Arc<ReceiptLog>, Vec<usize>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test setup");
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let vb = ts(1_000_000);
    for i in 0..n {
        let id = intents.begin(intent(i, vb)).expect("begin");
        intents.release(id);
    }
    let head = 1_000_000 + (600 + head_after_expiry_secs) / 2;
    let chain = Chain {
        head,
        calls: Mutex::default(),
        fail_every,
        n: AtomicU64::new(0),
    };
    let now = ts(head);
    let r = IntentResolver::new(
        chain,
        [0x55; 20],
        [0x77; 20],
        intents.clone(),
        receipts.clone(),
    )
    .with_clock(Arc::new(move || now));
    let mut left = Vec::new();
    for _ in 0..6 {
        rt.block_on(r.resolve_once());
        left.push(intents.open_count());
    }
    (receipts, left)
}

/// Same as `run` but returns the call counts of ONE pass.
fn one_pass_calls(n: u32, head_after_expiry_secs: u64) -> BTreeMap<String, u64> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test setup");
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let vb = ts(1_000_000);
    for i in 0..n {
        let id = intents.begin(intent(i, vb)).expect("begin");
        intents.release(id);
    }
    let head = 1_000_000 + (600 + head_after_expiry_secs) / 2;
    let chain = Arc::new(Chain {
        head,
        calls: Mutex::default(),
        fail_every: 0,
        n: AtomicU64::new(0),
    });
    struct Shared(Arc<Chain>);
    impl JsonRpc for Shared {
        fn call<'a>(&'a self, m: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
            self.0.call(m, p)
        }
    }
    let now = ts(head);
    let r = IntentResolver::new(
        Shared(chain.clone()),
        [0x55; 20],
        [0x77; 20],
        intents.clone(),
        receipts,
    )
    .with_clock(Arc::new(move || now));
    let out = rt.block_on(r.resolve_once());
    assert_eq!(
        out.expired, MAX_INTENTS_PER_PASS,
        "one pass judges a bounded batch: {out:?}"
    );
    let c = chain.calls.lock().expect("lock").clone();
    c
}

#[test]
fn rpc_cost_per_expired_unused_intent_is_bounded() {
    // Measured (review 5): 11, 15, 19 and 29 calls per intent as the node's
    // head moves further past the expiry; the span search is logarithmic.
    for lag in [10u64, 600, 3600, 86_400] {
        let c = one_pass_calls(MAX_INTENTS_PER_PASS as u32, lag);
        let total: u64 = c.values().sum();
        let per = total / MAX_INTENTS_PER_PASS as u64;
        assert!(
            per <= 30,
            "{lag} s after expiry: {per} calls per intent {c:?}"
        );
    }
}

#[test]
fn a_rate_limiting_node_slows_drops_but_never_stops_them() {
    // A healthy node drains 1,000 expired-unused intents at the per-pass cap.
    let (receipts, left) = run(1_000, 60, 0);
    assert_eq!(left, vec![800, 600, 400, 200, 0, 0]);
    assert_eq!(
        receipts.pending(),
        0,
        "an unused authorization is never a receipt"
    );
    // Every 10th call failing: slower, still progress every pass, still no
    // receipt from an unused authorization.
    for fail_every in [20u64, 10] {
        let (receipts, left) = run(1_000, 60, fail_every);
        assert!(
            left.windows(2).all(|w| w[1] < w[0]),
            "every pass drops some: {left:?}"
        );
        assert!(left[5] < 1_000, "{left:?}");
        assert_eq!(receipts.pending(), 0);
    }
}

/// Live, read-only: one resolver pass over N expired-unused intents against
/// the public Base Sepolia node, timed.
#[cfg(feature = "http-client")]
#[test]
#[ignore]
fn live_pass_time_on_public_node() {
    use onyx_api::roots::HttpJsonRpc;
    use onyx_api::x402::HyperPost;
    let n: u32 = std::env::var("N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test setup");
    let rpc = HttpJsonRpc::new(
        "https://sepolia.base.org",
        HyperPost::for_rpc(std::time::Duration::from_secs(10)),
    );
    let (_, now) = rt
        .block_on(onyx_api::arrival::head_time(&rpc))
        .expect("head");
    let intents = Arc::new(IntentLog::new());
    // Expired 15 min ago (validBefore + 600 grace passed 5 min ago).
    let vb = now - 900;
    for i in 0..n {
        let mut payer = [0xabu8; 20];
        payer[..4].copy_from_slice(&i.to_be_bytes());
        let mut nonce = [0x5au8; 32];
        nonce[..4].copy_from_slice(&i.to_be_bytes());
        let it = Intent::new(
            Receipt {
                round: 1,
                shard: Hash32([1; 32]),
                amount: 1_000,
                payer,
                nonce,
            },
            vb - 600,
            vb,
            vb - 60,
            360,
        );
        let id = intents.begin(it).expect("begin");
        intents.release(id);
    }
    let usdc: [u8; 20] = hex::decode("036CbD53842c5426634e7929541eC2318f3dCF7e")
        .expect("test setup")
        .try_into()
        .expect("test setup");
    let r = IntentResolver::new(
        rpc,
        usdc,
        [0x77; 20],
        intents.clone(),
        Arc::new(ReceiptLog::new()),
    );
    let t = std::time::Instant::now();
    let out = rt.block_on(r.resolve_once());
    eprintln!(
        "live pass over {n} expired-unused intents: {:?} in {:?}; open after {}",
        out,
        t.elapsed(),
        intents.open_count()
    );
}
