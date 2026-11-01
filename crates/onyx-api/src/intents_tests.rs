//! Settle intent tests (OPS-L2-F1, OPS-L3): every paid settle is written down before the call,
//! and each way an intent ends (settled, refunded, expired, recovered on a retry or a restart).

use super::*;
use crate::arrival::{
    authorization_canceled_topic, authorization_used_topic, transfer_topic, word_addr, word_u128,
};
use crate::journal::{test_dir, TEST_BINDING};
use crate::x402::BoxFuture;
use onyx_core::Hash32;

const TOKEN: [u8; 20] = [0x55; 20];
const REVENUE: [u8; 20] = [0x77; 20];
const PAYER: [u8; 20] = [0x11; 20];
const ELSEWHERE: [u8; 20] = [0xee; 20];

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn intent(nonce: u8, valid_before: u64) -> Intent {
    intent_from(PAYER, nonce, valid_before)
}

/// The longest authorization life the test server accepts (the default
/// `maxTimeoutSeconds` 60 plus `authorization_slack` 300).
const LIFE: u64 = 360;

fn intent_from(payer: [u8; 20], nonce: u8, valid_before: u64) -> Intent {
    Intent::new(
        Receipt {
            round: 7,
            shard: Hash32([nonce; 32]),
            amount: 2_500,
            payer,
            nonce: [nonce; 32],
        },
        valid_before.saturating_sub(600),
        valid_before,
        valid_before.saturating_sub(60),
        LIFE,
    )
}

/// One transaction: its token logs in order, `(nonce, Some(to))` for an
/// AuthorizationUsed followed by its Transfer of 2,500 to `to`, or
/// `(nonce, None)` for an AuthorizationUsed with no Transfer after it.
#[derive(Clone)]
struct Tx {
    block: u64,
    /// The authorizer of every use in it.
    from: [u8; 20],
    uses: Vec<([u8; 32], Option<[u8; 20]>)>,
}

/// A chain where block n has timestamp n. `head` is the head every
/// backend reports for `eth_blockNumber`; `served` the highest block a
/// (lagging) backend serves for reads by number; `fail` fails every call.
struct Chain {
    head: u64,
    served: u64,
    txs: Vec<Tx>,
    /// `(authorizer, nonce, block)`: cancelled with `cancelAuthorization`
    /// in `block`.
    cancels: Vec<([u8; 20], [u8; 32], u64)>,
    /// Nonces the token marks used whose logs no node returns (a use
    /// outside any span we read, or a backend that drops them).
    silent: Vec<[u8; 32]>,
    /// `eth_getLogs` answers as if the node's head were this (a
    /// load-balanced backend that silently clamps, OPS-L4-F2).
    logs_clamp: Option<u64>,
    fail: bool,
    log_calls: Mutex<u64>,
}

impl Chain {
    fn at(head: u64) -> Self {
        Chain {
            head,
            served: head,
            txs: Vec::new(),
            cancels: Vec::new(),
            silent: Vec::new(),
            logs_clamp: None,
            fail: false,
            log_calls: Mutex::new(0),
        }
    }
    /// A plain x402 payment: authorization `nonce` sent 2,500 to `to`
    /// in its own tx in `block`.
    fn pay(self, nonce: u8, to: [u8; 20], block: u64) -> Self {
        self.pay_from(PAYER, nonce, to, block)
    }
    fn pay_from(mut self, from: [u8; 20], nonce: u8, to: [u8; 20], block: u64) -> Self {
        self.txs.push(Tx {
            block,
            from,
            uses: vec![([nonce; 32], Some(to))],
        });
        self
    }
    fn tx(mut self, tx: Tx) -> Self {
        self.txs.push(tx);
        self
    }
    fn cancel(self, nonce: u8, block: u64) -> Self {
        self.cancel_from(PAYER, nonce, block)
    }
    fn cancel_from(mut self, from: [u8; 20], nonce: u8, block: u64) -> Self {
        self.cancels.push((from, [nonce; 32], block));
        self
    }
    fn used_by(&self, nonce: &[u8; 32], block: u64) -> bool {
        self.txs
            .iter()
            .any(|t| t.block <= block && t.uses.iter().any(|(n, _)| n == nonce))
            || self
                .cancels
                .iter()
                .any(|(_, n, b)| n == nonce && *b <= block)
            || self.silent.contains(nonce)
    }
    fn receipt(&self, i: usize) -> Value {
        let t = &self.txs[i];
        let mut logs = Vec::new();
        for (n, to) in &t.uses {
            logs.push(json!({
                "address": hx(&TOKEN),
                "topics": [hx(&authorization_used_topic()), hx(&word_addr(&t.from)), hx(n)],
                "data": "0x",
            }));
            if let Some(to) = to {
                logs.push(json!({
                    "address": hx(&TOKEN),
                    "topics": [hx(&transfer_topic()), hx(&word_addr(&t.from)), hx(&word_addr(to))],
                    "data": hx(&word_u128(2_500)),
                }));
            }
        }
        for (k, l) in logs.iter_mut().enumerate() {
            l["logIndex"] = json!(format!("0x{k:x}"));
        }
        json!({ "status": "0x1", "logs": logs })
    }
}

fn q(v: &Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
        .unwrap_or(u64::MAX)
}

impl JsonRpc for Chain {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = if self.fail {
            Err("node down".to_string())
        } else {
            match method {
                "eth_blockNumber" => Ok(json!(format!("0x{:x}", self.head))),
                "eth_getBlockByNumber" => {
                    let n = q(&params[0]);
                    if n > self.served {
                        Ok(Value::Null)
                    } else {
                        Ok(json!({ "timestamp": format!("0x{n:x}") }))
                    }
                }
                "eth_call" => {
                    // Never asked at "latest" by the resolver (OPS-L3-F3).
                    let block = q(&params[1]);
                    if block > self.served {
                        Err("header not found".to_string())
                    } else {
                        let data = params[0]["data"].as_str().unwrap_or("").to_string();
                        let nonce: [u8; 32] = hex::decode(&data[data.len() - 64..])
                            .expect("hex")
                            .try_into()
                            .expect("32");
                        Ok(json!(format!(
                            "0x{:064x}",
                            u8::from(self.used_by(&nonce, block))
                        )))
                    }
                }
                "eth_getLogs" => {
                    *self.log_calls.lock().expect("lock") += 1;
                    let (f, t) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                    assert!(t - f < crate::arrival::LOGS_WINDOW);
                    let nonce = params[0]["topics"][2].as_str().unwrap_or("").to_string();
                    let from = params[0]["topics"][1].as_str().unwrap_or("").to_string();
                    // Both events are asked for (OPS-L4-F1).
                    assert_eq!(
                        params[0]["topics"][0],
                        json!([
                            hx(&authorization_used_topic()),
                            hx(&authorization_canceled_topic())
                        ])
                    );
                    let top = t.min(self.served).min(self.logs_clamp.unwrap_or(u64::MAX));
                    let mut hits: Vec<Value> = self
                            .txs
                            .iter()
                            .enumerate()
                            .filter(|(_, tx)| {
                                (f..=top).contains(&tx.block)
                                    && hx(&word_addr(&tx.from)) == from
                                    && tx.uses.iter().any(|(n, _)| hx(n) == nonce)
                            })
                            .map(|(i, _)| json!({
                                "address": hx(&TOKEN),
                                "topics": [hx(&authorization_used_topic()), from.clone(), nonce.clone()],
                                "transactionHash": format!("0x{i:x}"),
                            }))
                            .collect();
                    hits.extend(
                            self.cancels
                                .iter()
                                .filter(|(a, n, b)| {
                                    (f..=top).contains(b)
                                        && hx(&word_addr(a)) == from
                                        && hx(n) == nonce
                                })
                                .map(|_| json!({
                                    "address": hx(&TOKEN),
                                    "topics": [hx(&authorization_canceled_topic()), from.clone(), nonce.clone()],
                                    "transactionHash": "0xcancel",
                                })),
                        );
                    Ok(json!(hits))
                }
                "eth_getTransactionReceipt" => Ok(self.receipt(q(&params[0]) as usize)),
                _ => Err(format!("unexpected {method}")),
            }
        };
        Box::pin(async move { r })
    }
}

/// Paused tokio time (ROADMAP E12): deadlines and polls follow the
/// test's order, not the machine's speed.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("runtime")
}

/// A resolver whose clock agrees with the chain's head (block n is at
/// time n).
fn resolver(
    chain: Chain,
    intents: &Arc<IntentLog>,
    receipts: &Arc<ReceiptLog>,
) -> IntentResolver<Chain> {
    let now = chain.head;
    IntentResolver::new(chain, TOKEN, REVENUE, intents.clone(), receipts.clone())
        .with_clock(Arc::new(move || now))
}

/// An intent no settle owns any more.
fn released(intents: &IntentLog, i: Intent) -> u64 {
    let id = intents.begin(i).expect("begin");
    intents.release(id);
    id
}

const VB: u64 = 1_000_000;
const GRACE: u64 = EXPIRY_GRACE.as_secs();

/// A settle that timed out after the facilitator broadcast it: the
/// payment reached OnyxRevenue, so the intent becomes a receipt.
#[test]
fn a_settle_that_timed_out_after_broadcast_is_recovered() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let i = intent(1, VB);
    released(&intents, i);
    let chain = Chain::at(VB - 10).pay(1, REVENUE, VB - 30);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    assert_eq!(receipts.peek(10), vec![i.receipt]);
    assert_eq!(intents.open_count(), 0);
    assert_eq!(intents.recovered(), 1);
}

/// A settle still owned by a running task is never judged.
#[test]
fn a_live_settle_is_left_alone() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    intents.begin(intent(2, VB)).expect("begin");
    let chain = Chain::at(VB + GRACE + 100).pay(2, REVENUE, VB - 30);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r, Resolved::default());
    assert_eq!(receipts.pending(), 0);
    assert_eq!(intents.open_count(), 1);
}

/// SIGKILL mid-settle: the intent is on disk; after the restart it is no
/// settle's, and the resolver recovers it.
#[test]
fn a_kill_mid_settle_is_recovered_after_the_restart() {
    let dir = test_dir("intents-kill");
    let path = dir.join("settle-intents.jsonl");
    let i = intent(3, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin(i).expect("begin");
        // The process dies here: no end, no release.
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    assert_eq!(intents.due().len(), 1, "no settle owns it after a restart");
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB).pay(3, REVENUE, VB - 20);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    assert_eq!(receipts.peek(10), vec![i.receipt]);
    drop(intents);
    let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
    assert_eq!(again.open_count(), 0, "the removal is durable");
}

/// M7L3-R2-1: an intent's retry key and the resolver's proof are in
/// the journal. After a restart the identical retry still finds its open
/// intent (`awaiting`) or, once recovered, its proof (`proven`), within
/// the proof's window, for its own receipt, once; a pass past the window
/// removes the proof, durably.
#[test]
fn m7l3_r2_1_retry_keys_and_proofs_survive_a_restart() {
    let dir = test_dir("intents-proof");
    let path = dir.join("settle-intents.jsonl");
    let (ka, kb) = (Hash32([0xa1; 32]), Hash32([0xb2; 32]));
    let (a, b) = (intent(21, VB), intent(22, VB));
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin_for(a, Some(ka)).expect("a");
        log.begin_for(b, Some(kb)).expect("b");
        // The process dies here.
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    assert_eq!(intents.awaiting(&ka), Some(a), "the retry key survives");
    assert_eq!(intents.awaiting(&kb), Some(b));
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
    let now = chain.head;
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    assert_eq!(receipts.peek(10), vec![a.receipt]);
    assert_eq!(intents.awaiting(&ka), None);
    let (pa, tx) = intents.proven(&ka, now).expect("a proof");
    assert_eq!(pa, a);
    assert!(tx.starts_with("0x"), "{tx}");
    assert_eq!(intents.proven(&kb, now), None, "b is not proven");
    drop(intents);
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen 2"));
    assert_eq!(
        intents.proven(&ka, now),
        Some((a, tx.clone())),
        "the proof survives"
    );
    assert_eq!(intents.open_count(), 1);
    assert_eq!(
        intents.proven(&ka, now + PROVEN_WINDOW_SECS),
        None,
        "only in its window"
    );
    assert_eq!(
        intents.take_proven(&ka, &b.receipt, now),
        None,
        "only its own receipt"
    );
    assert_eq!(intents.take_proven(&ka, &a.receipt, now), Some(tx));
    assert_eq!(
        intents.take_proven(&ka, &a.receipt, now),
        None,
        "served once"
    );
    let late = Chain::at(VB - 10 + PROVEN_WINDOW_SECS);
    rt().block_on(resolver(late, &intents, &receipts).resolve_once());
    assert_eq!(intents.proven_count(), 0, "gone after its window");
    drop(intents);
    let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 3");
    assert_eq!(again.proven_count(), 0, "durably");
    assert_eq!(again.open_count(), 1);
    assert_eq!(again.awaiting(&kb), Some(b));
}

/// M7L3-R3-2 (review PoC b): the proof serves once across a restart.
/// Its served flag was memory only, so a restart inside the window
/// served the same proof again. Serving now deletes it from the journal.
#[test]
fn m7l3_r3_2_a_served_proof_is_not_served_again_after_a_restart() {
    let dir = test_dir("intents-r3-2");
    let path = dir.join("settle-intents.jsonl");
    let ka = Hash32([0xa1; 32]);
    let a = intent(21, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin_for(a, Some(ka)).expect("a");
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
    let now = chain.head;
    rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert!(
        intents.take_proven(&ka, &a.receipt, now).is_some(),
        "first serve"
    );
    assert_eq!(intents.take_proven(&ka, &a.receipt, now), None);
    assert_eq!(intents.proven_count(), 0);
    drop(intents);
    let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
    assert_eq!(again.proven_count(), 0, "the deletion is durable");
    assert_eq!(again.take_proven(&ka, &a.receipt, now + 1), None);
    assert_eq!(again.awaiting(&ka), None);
}

/// M7L3-R3-2: a proof whose deletion cannot be made durable is not
/// served (fail closed); it stays on disk and the next start serves it
/// once.
#[test]
fn m7l3_r3_2_a_proof_whose_deletion_fails_is_not_served() {
    let dir = test_dir("intents-r3-2-fail");
    let path = dir.join("settle-intents.jsonl");
    let ka = Hash32([0xa1; 32]);
    let a = intent(21, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin_for(a, Some(ka)).expect("a");
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
    let now = chain.head;
    rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    intents.break_journal_for_test();
    assert_eq!(intents.take_proven(&ka, &a.receipt, now), None);
    assert_eq!(intents.write_errors(), 1);
    drop(intents);
    let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
    assert!(again.take_proven(&ka, &a.receipt, now).is_some());
    assert_eq!(again.take_proven(&ka, &a.receipt, now), None);
}

/// M7L3-R3-3 (review PoC c): an intent a retry adopted while a resolver
/// pass judged it (`live`) got a proof as well: the retry served from
/// its own check and the proof served once more later. No proof is
/// made for it now; the intent still goes (its receipt is written).
#[test]
fn m7l3_r3_3_no_proof_for_an_intent_a_retry_adopted() {
    let dir = test_dir("intents-r3-3");
    let path = dir.join("settle-intents.jsonl");
    let ka = Hash32([0xa1; 32]);
    let a = intent(21, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin_for(a, Some(ka)).expect("a");
    }
    let log = IntentLog::open(&path, &TEST_BINDING).expect("reopen");
    assert_eq!(log.due().len(), 1, "the pass snapshot");
    let id = log.adopt(&ka, &a.receipt).expect("retry adopts mid-pass");
    log.resolved(&[(id, "0xabc".into())], &[], &[], 100);
    assert_eq!(log.proven_count(), 0, "no proof for an adopted intent");
    assert_eq!(log.proven(&ka, 100), None);
    assert_eq!(log.open_count(), 0);
    drop(log);
    let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
    assert_eq!(again.proven_count(), 0);
}

/// M7L3-R3-4: a request served though its receipt could not be written
/// keeps its intent for the resolver, but without its request key, in
/// memory and on disk: after a restart its identical retry neither takes
/// the intent over nor gets a proof.
#[test]
fn m7l3_r3_4_a_served_request_without_receipt_loses_its_key() {
    let dir = test_dir("intents-r3-4");
    let path = dir.join("settle-intents.jsonl");
    let ka = Hash32([0xa1; 32]);
    let a = intent(21, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        let id = log.begin_for(a, Some(ka)).expect("a");
        let id = log.forget_request(id);
        log.release(id);
        assert_eq!(log.awaiting(&ka), None);
        assert_eq!(log.due().len(), 1, "left to the resolver");
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    assert_eq!(intents.open_count(), 1);
    assert_eq!(intents.awaiting(&ka), None, "no key after a restart");
    assert_eq!(intents.adopt(&ka, &a.receipt), None);
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
    let now = chain.head;
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    assert_eq!(receipts.peek(10), vec![a.receipt], "still attributed");
    assert_eq!(intents.proven(&ka, now), None, "no proof");
    assert_eq!(intents.proven_count(), 0);
}

/// An authorization never used: kept while the chain's own clock is at
/// or before validBefore + grace, dropped once a block past it says
/// "unused"; no receipt.
#[test]
fn an_unused_authorization_is_dropped_only_by_chain_time() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(4, VB));
    let r = rt().block_on(resolver(Chain::at(VB + GRACE), &intents, &receipts).resolve_once());
    assert_eq!(r.waiting, 1, "the chain has not passed the expiry: kept");
    assert_eq!(intents.open_count(), 1);
    let r = rt().block_on(resolver(Chain::at(VB + GRACE + 5), &intents, &receipts).resolve_once());
    assert_eq!(r.expired, 1);
    assert_eq!(intents.open_count(), 0);
    assert_eq!(receipts.pending(), 0);
    assert_eq!(intents.expired(), 1);
}

/// OPS-L3-F3: a lagging backend behind a load balancer. Its head is
/// reported past the expiry, but it cannot serve that block: an error,
/// never "unused"; the intent is kept. A node whose own head is before
/// the expiry keeps it too.
#[test]
fn a_lagging_node_never_drops_an_intent() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(5, VB));
    let mut lagging = Chain::at(VB + GRACE + 1_000);
    lagging.served = VB - 100;
    let r = rt().block_on(resolver(lagging, &intents, &receipts).resolve_once());
    assert_eq!((r.expired, r.errors), (0, 1));
    let behind = Chain::at(VB - 100);
    let r = rt().block_on(resolver(behind, &intents, &receipts).resolve_once());
    assert_eq!((r.expired, r.waiting), (0, 1));
    assert_eq!(intents.open_count(), 1);
    // The payment did land; a node that has the blocks recovers it.
    let good = Chain::at(VB + GRACE + 1_000).pay(5, REVENUE, VB - 50);
    let r = rt().block_on(resolver(good, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
}

/// The nonce paid someone else: never a receipt; dropped as soon as its
/// use is found (a nonce is used once), whatever the clock says.
#[test]
fn a_nonce_spent_paying_someone_else_is_never_credited() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(6, VB));
    let chain = Chain::at(VB - 10).pay(6, ELSEWHERE, VB - 40);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.expired, 1);
    assert_eq!(receipts.pending(), 0);
}

/// OPS-L3-F2: one helper transaction uses two authorizations and holds
/// ONE Transfer to OnyxRevenue. Only the authorization whose own
/// Transfer (the next log) went to us is recovered; the other is never
/// credited, so 2,500 received is attributed once.
#[test]
fn one_transfer_proves_one_authorization() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(7, VB));
    released(&intents, intent(8, VB));
    let helper = Tx {
        block: VB - 30,
        from: PAYER,
        uses: vec![([7; 32], Some(ELSEWHERE)), ([8; 32], Some(REVENUE))],
    };
    let r =
        rt().block_on(resolver(Chain::at(VB - 10).tx(helper), &intents, &receipts).resolve_once());
    assert_eq!((r.recovered, r.expired), (1, 1));
    assert_eq!(receipts.peek(10), vec![intent(8, VB).receipt]);
    // The reviewer's shape: both AuthorizationUsed first, one Transfer at
    // the end. Nonce 9's next log is not its Transfer: not proven (an
    // error, kept); nonce 10 is.
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(9, VB));
    released(&intents, intent(10, VB));
    let helper = Tx {
        block: VB - 30,
        from: PAYER,
        uses: vec![([9; 32], None), ([10; 32], Some(REVENUE))],
    };
    let r =
        rt().block_on(resolver(Chain::at(VB - 10).tx(helper), &intents, &receipts).resolve_once());
    // Nonce 9's one use is not a payment shape: dropped, never credited
    // and never kept (OPS-L4).
    assert_eq!((r.recovered, r.expired), (1, 1));
    assert_eq!(receipts.peek(10), vec![intent(10, VB).receipt]);
    assert_eq!(intents.open_count(), 0);
}

/// A node error is never a "no": the intent is kept, even long after it
/// expired.
#[test]
fn a_node_error_keeps_the_intent() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(11, VB));
    let mut chain = Chain::at(VB * 2);
    chain.fail = true;
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.errors, 1);
    assert_eq!(intents.open_count(), 1);
}

/// OPS-L3-F1: the receipt journal fails; the resolver finds the payment
/// on chain but cannot make its receipt durable, so the intent stays on
/// disk. After a restart (a healthy receipt journal) it is recovered.
#[test]
fn a_receipt_write_error_keeps_the_intent_until_a_restart_recovers_it() {
    let dir = test_dir("intents-receipt-error");
    let rpath = dir.join("receipts.jsonl");
    let ipath = dir.join("intents.jsonl");
    let i = intent(12, VB);
    let chain = || Chain::at(VB - 10).pay(12, REVENUE, VB - 30);
    {
        let receipts = Arc::new(ReceiptLog::open(&rpath, &TEST_BINDING).expect("receipts"));
        let intents = Arc::new(IntentLog::open(&ipath, &TEST_BINDING).expect("intents"));
        released(&intents, i);
        receipts.break_journal_for_test();
        // A paid request's own receipt fails first: not durable.
        assert!(!receipts.record(intent(13, VB).receipt));
        assert_eq!(receipts.write_errors(), 1);
        let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
        assert_eq!(
            r.recovered, 0,
            "nothing removed while receipts cannot write"
        );
        assert_eq!(intents.open_count(), 1);
    }
    // Restart: both journals reopen; the intent is still there.
    let receipts = Arc::new(ReceiptLog::open(&rpath, &TEST_BINDING).expect("reopen"));
    let intents = Arc::new(IntentLog::open(&ipath, &TEST_BINDING).expect("reopen"));
    assert_eq!(intents.open_count(), 1);
    let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    drop((receipts, intents));
    let receipts = ReceiptLog::open(&rpath, &TEST_BINDING).expect("reopen 2");
    assert_eq!(receipts.peek(10), vec![i.receipt], "the receipt is durable");
}

/// Mutation L4: the receipt journal breaks during a pass: the paid
/// intent is an error and stays; once the journal has failed, a pass
/// judges nothing and counts every due intent as an error.
#[test]
fn a_receipt_journal_failing_mid_pass_counts_errors() {
    let dir = test_dir("intents-receipt-midpass");
    let receipts =
        Arc::new(ReceiptLog::open(dir.join("receipts.jsonl"), &TEST_BINDING).expect("receipts"));
    let intents = Arc::new(IntentLog::new());
    released(&intents, intent(12, VB));
    released(&intents, intent(14, VB));
    receipts.break_journal_for_test();
    let chain = || Chain::at(VB - 10).pay(12, REVENUE, VB - 30);
    let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
    assert_eq!((r.recovered, r.errors, r.waiting), (0, 1, 1));
    assert_eq!(intents.open_count(), 2);
    assert_eq!(receipts.write_errors(), 1);
    let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
    assert_eq!((r.recovered, r.errors, r.waiting), (0, 2, 0));
    assert_eq!(intents.open_count(), 2);
}

/// OPS-L3-F4: a payer cannot hold more than MAX_OPEN_PER_PAYER intents;
/// another payer is unaffected.
#[test]
fn a_payer_has_a_cap_on_open_intents() {
    let intents = IntentLog::new();
    for n in 0..MAX_OPEN_PER_PAYER as u8 {
        released(&intents, intent(20 + n, VB));
    }
    assert_eq!(intents.begin(intent(40, VB)), Err(BeginError::PayerBusy));
    assert!(intents.begin(intent_from([0x22; 20], 41, VB)).is_ok());
}

/// OPS-L3-F4: a pass reads at most its budget; intents it did not reach
/// are judged on the next pass, earliest `validBefore` first.
#[test]
fn a_pass_has_a_budget_and_the_rest_wait_their_turn() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    // Each use sits at the end of its span: several log windows each,
    // more than one pass's budget in all.
    let n: u8 = 180;
    let mut chain = Chain::at(VB + GRACE + 1_000);
    for k in 0..n {
        let payer = [k.wrapping_add(1); 20];
        released(&intents, intent_from(payer, k, VB));
        chain = chain.tx(Tx {
            block: VB - 1,
            from: payer,
            uses: vec![([k; 32], None)],
        });
    }
    let res = resolver(chain, &intents, &receipts);
    let r = rt().block_on(res.resolve_once());
    assert!(*res.rpc.log_calls.lock().expect("lock") <= MAX_LOG_CALLS_PER_PASS);
    assert!(
        r.expired < usize::from(n),
        "the budget stopped the pass: {r:?}"
    );
    let r2 = rt().block_on(res.resolve_once());
    assert_eq!(
        r.expired + r2.expired,
        usize::from(n),
        "the rest end next pass"
    );
    assert_eq!(intents.open_count(), 0);
}

/// OPS-L4-F1: the payer cancels the authorization (FiatToken
/// `cancelAuthorization`: marked used, no Transfer). The intent ends
/// as cancelled; no receipt.
#[test]
fn a_cancelled_authorization_ends_its_intent() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(60, VB));
    // Cancelled after validBefore, too (the reviewer's attack).
    let chain = Chain::at(VB + GRACE + 3_600).cancel(60, VB + 100);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.cancelled, 1, "{r:?}");
    assert_eq!(intents.open_count(), 0);
    assert_eq!(intents.cancelled(), 1);
    assert_eq!(receipts.pending(), 0);
    // Cancelled before expiry: found by the early read already.
    released(&intents, intent(61, VB));
    let chain = Chain::at(VB - 10).cancel(61, VB - 50);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.cancelled, 1);
}

/// OPS-L4-F1, the class: an authorization the token marks used whose
/// use no log shows (outside the span, or never returned) ends after its
/// full reads, never later than a bounded number of passes past expiry.
#[test]
fn a_used_authorization_with_no_payment_of_ours_ends() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    released(&intents, intent(62, VB));
    let mut chain = Chain::at(VB + GRACE + 1_000);
    chain.silent.push([62; 32]);
    let res = resolver(chain, &intents, &receipts);
    let mut passes = 0;
    while intents.open_count() > 0 {
        passes += 1;
        assert!(
            passes <= 2 * usize::from(crate::arrival::FINAL_SCANS),
            "never ended"
        );
        rt().block_on(res.resolve_once());
    }
    assert_eq!(intents.expired(), 1);
    assert_eq!(receipts.pending(), 0);
}

/// OPS-L4-F2: one backend clamps `eth_getLogs` below the payment's
/// block; one full read finds nothing, but a single read never drops a
/// used authorization: the next read, on a healthy node, recovers it.
#[test]
fn a_clamping_backend_cannot_drop_a_payment_alone() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let i = intent(63, VB);
    released(&intents, i);
    let mut clamped = Chain::at(VB + GRACE + 1_000).pay(63, REVENUE, VB - 30);
    clamped.logs_clamp = Some(VB - 200);
    let r = rt().block_on(resolver(clamped, &intents, &receipts).resolve_once());
    assert_eq!((r.expired, r.waiting), (0, 1));
    let good = Chain::at(VB + GRACE + 1_000).pay(63, REVENUE, VB - 30);
    let r = rt().block_on(resolver(good, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
    assert_eq!(receipts.peek(10), vec![i.receipt]);
}

/// OPS-L4-F3: the span is chain time from `validAfter`/`validBefore`,
/// not the local clock: an intent written by a clock running an hour
/// fast still finds a payment mined right after verify.
#[test]
fn a_fast_local_clock_does_not_hide_the_payment() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let mut i = intent(64, VB);
    i.created = VB + 3_600;
    released(&intents, i);
    let chain = Chain::at(VB + GRACE + 1_000).pay(64, REVENUE, VB - LIFE + 10);
    let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(r.recovered, 1);
}

/// OPS-L4-F3: while the server's clock is more than MAX_CLOCK_SKEW_SECS
/// off the chain's, no new settle starts (503); it starts again once
/// they agree.
#[test]
fn a_skewed_clock_refuses_new_settles() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let fast = IntentResolver::new(
        Chain::at(VB),
        TOKEN,
        REVENUE,
        intents.clone(),
        receipts.clone(),
    )
    .with_clock(Arc::new(|| VB + MAX_CLOCK_SKEW_SECS + 1));
    rt().block_on(fast.resolve_once());
    assert_eq!(
        intents.skew(),
        Some(i64::try_from(MAX_CLOCK_SKEW_SECS).expect("fits") + 1)
    );
    assert_eq!(intents.begin(intent(65, VB)), Err(BeginError::ClockSkew));
    rt().block_on(resolver(Chain::at(VB), &intents, &receipts).resolve_once());
    assert!(intents.begin(intent(65, VB)).is_ok());
    // The bound itself is allowed, either way (mutation L4).
    let max = i64::try_from(MAX_CLOCK_SKEW_SECS).expect("fits");
    intents.set_skew(-max);
    assert!(intents.begin(intent(66, VB)).is_ok());
    intents.set_skew(max);
    assert!(intents.begin(intent(67, VB)).is_ok());
    intents.set_skew(-max - 1);
    assert_eq!(intents.begin(intent(68, VB)), Err(BeginError::ClockSkew));
}

/// Mutation L4: a proof is served only before its window ends, and the
/// pass at the end of its window (not one second before) drops it.
#[test]
fn a_proof_lives_exactly_its_window() {
    let dir = test_dir("intents-proof-window");
    let path = dir.join("settle-intents.jsonl");
    let ka = Hash32([0xa1; 32]);
    let a = intent(21, VB);
    {
        let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
        log.begin_for(a, Some(ka)).expect("a");
        // The process dies here: the resolver of the next start recovers it.
    }
    let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
    let receipts = Arc::new(ReceiptLog::new());
    let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
    let now = chain.head;
    rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
    assert_eq!(intents.proven_count(), 1);
    let until = now + PROVEN_WINDOW_SECS;
    assert_eq!(intents.take_proven(&ka, &a.receipt, until), None);
    rt().block_on(resolver(Chain::at(until - 1), &intents, &receipts).resolve_once());
    assert_eq!(intents.proven_count(), 1, "still in its window");
    rt().block_on(resolver(Chain::at(until), &intents, &receipts).resolve_once());
    assert_eq!(intents.proven_count(), 0, "its window ended");
}

/// OPS-L4-F4: one authorization, two requests: only one intent.
#[test]
fn one_authorization_has_one_intent() {
    let intents = IntentLog::new();
    intents.begin(intent(66, VB)).expect("first");
    let mut other_shard = intent(66, VB);
    other_shard.receipt.shard = Hash32([0xab; 32]);
    assert_eq!(intents.begin(other_shard), Err(BeginError::InUse));
}

/// OPS-L4-F5: settles in flight do not count toward the payer's cap;
/// unsettled (released) ones do.
#[test]
fn only_unsettled_intents_count_toward_the_payer_cap() {
    let intents = IntentLog::new();
    for n in 0..=MAX_OPEN_PER_PAYER as u8 {
        intents.begin(intent(70 + n, VB)).expect("in flight");
    }
    assert_eq!(intents.open_count(), MAX_OPEN_PER_PAYER + 1);
}

/// The class invariant, with every payer action the token allows: each
/// released intent ends within a bounded number of passes once the
/// chain is past its expiry, and only payments to us become receipts.
#[test]
fn every_intent_ends_whatever_the_payer_does() {
    let intents = Arc::new(IntentLog::new());
    let receipts = Arc::new(ReceiptLog::new());
    let mut chain = Chain::at(VB + GRACE + 2_000);
    for k in 0..20u8 {
        let payer = [k + 1; 20];
        released(&intents, intent_from(payer, 100 + k, VB));
        let nonce = 100 + k;
        chain = match k % 5 {
            0 => chain,                                            // never used
            1 => chain.cancel_from(payer, nonce, VB + 50),         // cancelled after expiry
            2 => chain.pay_from(payer, nonce, ELSEWHERE, VB - 20), // spent elsewhere
            3 => chain.pay_from(payer, nonce, REVENUE, VB - 20),   // paid us
            _ => {
                chain.silent.push([nonce; 32]); // used, no log
                chain
            }
        };
    }
    let res = resolver(chain, &intents, &receipts);
    let mut passes = 0;
    while intents.open_count() > 0 {
        passes += 1;
        assert!(
            passes <= 2 * usize::from(crate::arrival::FINAL_SCANS),
            "stuck: {}",
            intents.open_count()
        );
        rt().block_on(res.resolve_once());
    }
    assert_eq!(receipts.pending(), 4, "only the four payments to us");
}

/// A guard dropped without `ended` (an error or a panic in the settle
/// task) hands the intent to the resolver; `ended` removes it.
#[test]
fn the_guard_releases_or_ends() {
    let intents = Arc::new(IntentLog::new());
    let id = intents.begin(intent(50, VB)).expect("begin");
    drop(IntentGuard::new(intents.clone(), id));
    assert_eq!(intents.due().len(), 1);
    let id2 = intents.begin(intent(51, VB)).expect("begin");
    rt().block_on(IntentGuard::new(intents.clone(), id2).ended());
    assert_eq!(intents.open_count(), 1);
}

/// A write that cannot be made durable refuses the settle (fail closed)
/// and is counted.
#[test]
fn a_failed_write_refuses_the_settle() {
    let dir = test_dir("intents-broken");
    let log = IntentLog::open(dir.join("i.jsonl"), &TEST_BINDING).expect("open");
    log.break_journal_for_test();
    assert_eq!(log.begin(intent(52, VB)), Err(BeginError::NotDurable));
    assert_eq!(log.write_errors(), 1);
    assert_eq!(
        log.begin(intent(53, VB)),
        Err(BeginError::NotDurable),
        "stays closed"
    );
}

/// The span runs from the bound the server accepted (life + skew before
/// `validBefore`, never before `validAfter`) to `validBefore`.
#[test]
fn the_span_is_the_authorizations_bounded_life() {
    let r = intent(54, 100_000).receipt;
    let i = Intent::new(r, 0, 100_000, 99_950, LIFE);
    assert_eq!(i.earliest, 100_000 - LIFE - MAX_CLOCK_SKEW_SECS);
    let late_after = Intent::new(r, 99_900, 100_000, 99_950, LIFE);
    assert_eq!(late_after.earliest, 99_900);
}
