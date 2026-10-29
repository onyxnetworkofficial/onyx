//! AUD-1R-F2: `Sender::replace` never signs above the sender's own fee cap
//! (`with_fee_limits`, AUD-1-F3), whatever cap its caller passes. The
//! reporter passes the first fee times `fee_cap_factor` (default 4).

use onyx_api::evm_tx::{SendError, Sender, DEFAULT_MAX_FEE_PER_GAS};
use onyx_api::roots::JsonRpc;
use onyx_api::x402::BoxFuture;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A node whose base fee is `base`; counts raw transactions sent.
struct Node {
    base: u128,
    sent: Arc<AtomicU32>,
}

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, _: Value) -> BoxFuture<'a, Result<Value, String>> {
        let v = match method {
            "eth_estimateGas" => json!("0x30d40"),
            "eth_getTransactionCount" => json!("0x0"),
            "eth_getBlockByNumber" => json!({"baseFeePerGas": format!("0x{:x}", self.base)}),
            "eth_maxPriorityFeePerGas" => json!("0x1"),
            "eth_sendRawTransaction" => {
                self.sent.fetch_add(1, Ordering::SeqCst);
                json!(format!("0x{}", "11".repeat(32)))
            }
            _ => return Box::pin(async { Err("unexpected".to_string()) }),
        };
        Box::pin(async move { Ok(v) })
    }
}

fn sender(base: u128) -> (Sender<Node>, Arc<AtomicU32>) {
    let mut k = [0u8; 32];
    k[31] = 7;
    let sent = Arc::new(AtomicU32::new(0));
    let s = Sender::new(
        Node {
            base,
            sent: sent.clone(),
        },
        84_532,
        k,
    )
    .expect("key");
    (s, sent)
}

// Paused tokio time (E12): the 1 s send deadline never races the machine.
#[tokio::test(start_paused = true)]
async fn a_replacement_at_the_sender_cap_is_refused_not_signed_above_it() {
    // A base fee just under the cap: the first send is priced at the cap.
    let (s, sent) = sender(DEFAULT_MAX_FEE_PER_GAS - 1);
    let first = s
        .broadcast(&[0xaa; 20], &[], 0, Duration::from_secs(1))
        .await
        .expect("first send");
    assert_eq!(first.tx.max_fee_per_gas, DEFAULT_MAX_FEE_PER_GAS);
    let caller_cap = first.tx.max_fee_per_gas * 4;
    let r = s
        .replace(&first, 13, caller_cap, Duration::from_secs(1))
        .await;
    assert!(matches!(r, Err(SendError::Rpc(_))), "no room under the cap");
    assert_eq!(sent.load(Ordering::SeqCst), 1, "nothing above the cap sent");
}

// Paused tokio time (E12): the 1 s send deadline never races the machine.
#[tokio::test(start_paused = true)]
async fn replacements_climb_to_the_sender_cap_and_stop_there() {
    // Priced well under the cap; the caller allows 4x the first fee, which
    // is above the sender cap. Each bump stays under the sender cap until
    // no valid bump fits, and then the replacement is refused.
    let (s, sent) = sender(DEFAULT_MAX_FEE_PER_GAS / 4);
    let first = s
        .broadcast(&[0xaa; 20], &[], 0, Duration::from_secs(1))
        .await
        .expect("first send");
    let caller_cap = first.tx.max_fee_per_gas * 4;
    assert!(caller_cap > DEFAULT_MAX_FEE_PER_GAS);
    let mut prev = first;
    let mut bumps = 0;
    loop {
        match s
            .replace(&prev, 13, caller_cap, Duration::from_secs(1))
            .await
        {
            Ok(next) => {
                assert!(next.tx.max_fee_per_gas > prev.tx.max_fee_per_gas);
                assert!(next.tx.max_fee_per_gas <= DEFAULT_MAX_FEE_PER_GAS);
                prev = next;
                bumps += 1;
                assert!(bumps < 20, "bumps stop at the cap");
            }
            Err(e) => {
                assert!(matches!(e, SendError::Rpc(_)), "{e:?}");
                break;
            }
        }
    }
    assert!(bumps >= 1, "at least one bump fits under the cap");
    assert_eq!(sent.load(Ordering::SeqCst), 1 + bumps);
}
