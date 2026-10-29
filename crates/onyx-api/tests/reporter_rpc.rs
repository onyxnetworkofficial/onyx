//! Reporter finality and fee bumps against a node that lags, lies or
//! refuses (from review REV-L4): nothing commits early, nothing is sent
//! twice, and a refused replacement of an already mined `credit` is
//! harmless.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use onyx_api::evm_tx::Sender;
use onyx_api::receipts::{Receipt, ReceiptLog};
use onyx_api::reporter::{credited_topic, Confirm, Reporter};
use onyx_api::roots::JsonRpc;
use onyx_api::x402::BoxFuture;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const REVENUE: [u8; 20] = [0x5e; 20];

fn receipt(i: u32) -> Receipt {
    let mut nonce = [0u8; 32];
    nonce[28..].copy_from_slice(&i.to_be_bytes());
    Receipt {
        round: 1,
        shard: Hash32([1; 32]),
        amount: 2_500,
        payer: [0xaa; 20],
        nonce,
    }
}

#[derive(Default)]
struct Chain {
    pending_nonce: u64,
    latest_nonce: u64,
    finalized: Option<u64>,
    finalized_err: bool,
    canon: BTreeMap<u64, String>,
    sent: Vec<String>,
    reject_sends: Option<String>,
    mined: BTreeMap<String, Value>,
}

#[derive(Clone)]
struct Node(Arc<Mutex<Chain>>);

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let mut c = self.0.lock().unwrap();
        let r = match method {
            "eth_estimateGas" => Ok(json!("0x30000")),
            // The head (review LAUNCH-R1-A3: a pinned estimate reads it).
            "eth_blockNumber" => Ok(json!("0x10")),
            "eth_getCode" => Ok(json!("0x6080")),
            "eth_getTransactionCount" => Ok(json!(format!(
                "0x{:x}",
                if params[1] == "latest" {
                    c.latest_nonce
                } else {
                    c.pending_nonce
                }
            ))),
            "eth_getBlockByNumber" => match params[0].as_str().unwrap() {
                "latest" => Ok(json!({"number": "0x10", "baseFeePerGas": "0x3b9aca00"})),
                "finalized" if c.finalized_err => Err("finalized: unknown block".into()),
                "finalized" | "safe" => Ok(match c.finalized {
                    Some(n) => json!({"number": format!("0x{n:x}")}),
                    None => Value::Null,
                }),
                n => {
                    let n = u64::from_str_radix(n.trim_start_matches("0x"), 16).unwrap();
                    Ok(c.canon.get(&n).map_or(Value::Null, |h| json!({"hash": h})))
                }
            },
            "eth_maxPriorityFeePerGas" => Ok(json!("0xf4240")),
            "eth_sendRawTransaction" => {
                let raw =
                    hex::decode(params[0].as_str().unwrap().trim_start_matches("0x")).unwrap();
                let h = format!("0x{}", hex::encode(keccak256(&raw).0));
                c.sent.push(h.clone());
                match &c.reject_sends {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!(h)),
                }
            }
            "eth_getTransactionReceipt" => Ok(c
                .mined
                .get(params[0].as_str().unwrap())
                .cloned()
                .unwrap_or(Value::Null)),
            m => Err(format!("unexpected {m}")),
        };
        Box::pin(async move { r })
    }
}

fn credited_receipt(batch: &[Receipt], block: u64, block_hash: &str) -> Value {
    let logs: Vec<Value> = batch
        .iter()
        .map(|r| {
            let mut payer = [0u8; 32];
            payer[12..].copy_from_slice(&r.payer);
            let mut amount = [0u8; 32];
            amount[24..].copy_from_slice(&r.amount.to_be_bytes());
            json!({
                "address": format!("0x{}", hex::encode(REVENUE)),
                "topics": [format!("0x{}", hex::encode(credited_topic())),
                           format!("0x{:064x}", r.round),
                           format!("0x{}", hex::encode(r.shard.0)),
                           format!("0x{}", hex::encode(payer))],
                "data": format!("0x{}{}", hex::encode(amount), hex::encode(r.nonce)),
            })
        })
        .collect();
    json!({"status": "0x1", "blockNumber": format!("0x{block:x}"), "blockHash": block_hash,
           "gasUsed": "0x5208", "effectiveGasPrice": "0x3b9aca00", "logs": logs})
}

/// Paused tokio time (ROADMAP E12): the reporter's 20 ms deadline and 2 ms
/// polls follow the test's order, not the machine's speed.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap()
}

fn setup(log: Arc<ReceiptLog>) -> (Arc<Mutex<Chain>>, Reporter<Node>) {
    let chain = Arc::new(Mutex::new(Chain {
        pending_nonce: 3,
        latest_nonce: 3,
        ..Chain::default()
    }));
    let sender = Sender::new(Node(chain.clone()), 84_532, [0x11; 32])
        .unwrap()
        .with_poll(Duration::from_millis(2));
    let rep = Reporter::new(Arc::new(sender), REVENUE, log)
        .with_deadline(Duration::from_millis(20))
        .with_confirm(Confirm::Finalized);
    (chain, rep)
}

/// Fee bump racing a mined original: the replacement is refused ("nonce too
/// low"), nothing breaks, and the next post commits the original once.
#[test]
fn l4_bump_of_an_already_mined_credit_is_harmless() {
    rt().block_on(async {
        let log = Arc::new(ReceiptLog::new());
        log.record(receipt(1));
        let (chain, rep) = setup(log.clone());
        let rep = rep.with_fee_bumps(Duration::ZERO, 25, 4, 4);
        // The original mines right after the first broadcast; this node's
        // receipts lag, so the reporter tries to bump it.
        let p = rep.run_once().await.unwrap();
        assert!(p.waiting);
        let h0 = chain.lock().unwrap().sent[0].clone();
        {
            let mut c = chain.lock().unwrap();
            c.reject_sends = Some("nonce too low".into());
        }
        let p = rep.run_once().await.unwrap();
        assert!(p.waiting && !p.bumped, "refused replacement ignored");
        {
            let mut c = chain.lock().unwrap();
            c.mined
                .insert(h0.clone(), credited_receipt(&[receipt(1)], 0x10, "0xaa"));
            c.canon.insert(0x10, "0xaa".into());
            c.finalized = Some(0x20);
            c.latest_nonce = 4;
        }
        let p = rep.run_once().await.unwrap();
        assert_eq!((p.applied, p.waiting), (1, false));
        assert_eq!(p.tx.as_deref(), Some(h0.as_str()));
        assert_eq!(log.pending(), 0);
    });
}

/// Finality when the RPC lags or lies: a finalized head below the block,
/// a canonical hash that differs (reorg; stale receipt), a null or failing
/// `finalized` tag: nothing is committed and nothing is re-sent.
#[test]
fn l4_finality_with_a_lagging_or_lying_rpc_never_commits_early() {
    rt().block_on(async {
        let log = Arc::new(ReceiptLog::new());
        log.record(receipt(1));
        let (chain, rep) = setup(log.clone());
        rep.run_once().await.unwrap();
        let h0 = chain.lock().unwrap().sent[0].clone();
        {
            let mut c = chain.lock().unwrap();
            c.mined
                .insert(h0.clone(), credited_receipt(&[receipt(1)], 0x10, "0xaa"));
            c.canon.insert(0x10, "0xaa".into());
            c.finalized = Some(0x0f);
        }
        assert!(rep.run_once().await.unwrap().waiting, "finalized below");
        chain.lock().unwrap().finalized = None;
        assert!(rep.run_once().await.unwrap().waiting, "tag returns null");
        {
            let mut c = chain.lock().unwrap();
            c.finalized = Some(0x20);
            c.canon.insert(0x10, "0xbb".into());
        }
        assert!(
            rep.run_once().await.unwrap().waiting,
            "stale receipt, reorged block"
        );
        chain.lock().unwrap().finalized_err = true;
        assert!(rep.run_once().await.is_err(), "tag errors");
        assert_eq!(log.pending(), 1);
        assert_eq!(chain.lock().unwrap().sent.len(), 1, "no re-send");
        {
            let mut c = chain.lock().unwrap();
            c.finalized_err = false;
            c.canon.insert(0x10, "0xaa".into());
        }
        assert_eq!(rep.run_once().await.unwrap().applied, 1);
        assert_eq!(log.pending(), 0);
    });
}
