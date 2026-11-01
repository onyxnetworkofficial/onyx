//! After an unknown settle (M7-F5 residual, M7L3, SOAK-1): the authorization is checked on the
//! token, a retry is served only for a transfer that landed, and re-sends are capped.

use super::*;

/// The token as the API's node sees it, for the authorization check after
/// an unknown settle (M7-F5 residual): block `n` at time `1_000 + 2 n`, head
/// 1,000; the authorization (if used) in block 500, moving `value` to
/// `paid_to`. EIP-3009 lets a nonce move money once: `land` refuses a
/// second use.
struct FakeToken {
    token: [u8; 20],
    from: [u8; 20],
    nonce: [u8; 32],
    value: AtomicU64,
    paid_to: std::sync::Mutex<Option<[u8; 20]>>,
    transfers: AtomicU64,
    down: std::sync::atomic::AtomicBool,
    state_reads: AtomicU64,
}

const FAKE_TOKEN: [u8; 20] = [0x36; 20];
const PAID_TX: &str = "0x7a11000000000000000000000000000000000000000000000000000000000001";

impl FakeToken {
    fn new(nonce: u8) -> Arc<Self> {
        Arc::new(FakeToken {
            token: FAKE_TOKEN,
            from: addr_bytes("payer a"),
            nonce: [nonce; 32],
            value: AtomicU64::new(0),
            paid_to: std::sync::Mutex::new(None),
            transfers: AtomicU64::new(0),
            down: std::sync::atomic::AtomicBool::new(false),
            state_reads: AtomicU64::new(0),
        })
    }

    /// The authorization moves its value to `to`, unless it was used.
    fn land(&self, to: [u8; 20]) -> bool {
        let mut p = self.paid_to.lock().expect("lock");
        if p.is_some() {
            return false;
        }
        *p = Some(to);
        self.transfers.fetch_add(1, Ordering::SeqCst);
        true
    }
}

impl roots::JsonRpc for FakeToken {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let q = |v: &Value| {
            u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
                .unwrap_or(0)
        };
        let paid = *self.paid_to.lock().expect("lock");
        let r = if self.down.load(Ordering::SeqCst) {
            Err("node: connection refused".to_string())
        } else {
            match method {
                "eth_call" => {
                    self.state_reads.fetch_add(1, Ordering::SeqCst);
                    Ok(json!(format!("0x{:064x}", u8::from(paid.is_some()))))
                }
                "eth_blockNumber" => Ok(json!("0x3e8")),
                "eth_getBlockByNumber" => {
                    let n = q(&params[0]);
                    Ok(
                        json!({ "number": format!("0x{n:x}"), "timestamp": format!("0x{:x}", 1_000 + 2 * n) }),
                    )
                }
                "eth_getLogs" => {
                    let (f, t) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                    Ok(if paid.is_some() && (f..=t).contains(&500) {
                        json!([{ "address": hex0x(&self.token), "transactionHash": PAID_TX }])
                    } else {
                        json!([])
                    })
                }
                "eth_getTransactionReceipt" => {
                    let to = paid.unwrap_or_default();
                    Ok(json!({"status": "0x1", "logs": [{
                        "address": hex0x(&self.token),
                        "topics": [hex0x(&crate::arrival::authorization_used_topic()), hex0x(&crate::arrival::word_addr(&self.from)), hex0x(&self.nonce)],
                        "data": "0x",
                    }, {
                        "address": hex0x(&self.token),
                        "topics": [hex0x(&crate::arrival::transfer_topic()), hex0x(&crate::arrival::word_addr(&self.from)), hex0x(&crate::arrival::word_addr(&to))],
                        "data": hex0x(&crate::arrival::word_u128(u128::from(self.value.load(Ordering::SeqCst)))),
                    }]}))
                }
                _ => Err(format!("unexpected {method}")),
            }
        };
        Box::pin(async move { r })
    }
}

impl roots::JsonRpc for Arc<FakeToken> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        self.as_ref().call(method, params)
    }
}

/// What one facilitator settle does, in call order (the last repeats).
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Never answers; nothing lands.
    Hang,
    /// The transfer lands in OnyxRevenue, then the answer is lost.
    LandThenHang,
    /// The authorization pays someone else, then the answer is lost.
    ElsewhereThenHang,
    /// The transfer lands and the facilitator says so.
    Settle,
    /// The earlier settle, still in a mempool, lands first; this one is
    /// refused by the token (the nonce is used).
    EarlierLandsThenRefused,
    /// Refused for a reason that is not a used nonce; nothing lands.
    RefusedOther,
}

struct Scripted {
    inner: Arc<Counting>,
    chain: Arc<FakeToken>,
    revenue: [u8; 20],
    steps: std::sync::Mutex<std::collections::VecDeque<Step>>,
    calls: AtomicU64,
}

impl Facilitator for Scripted {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.inner.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let step = {
            let mut q = self.steps.lock().expect("lock");
            if q.len() > 1 {
                q.pop_front().expect("step")
            } else {
                *q.front().expect("step")
            }
        };
        Box::pin(async move {
            let refused = |reason: &str| SettleResponse {
                success: false,
                error_reason: Some(reason.into()),
                payer: Some(addr("payer a")),
                transaction: String::new(),
                network: BASE_SEPOLIA.into(),
            };
            match step {
                Step::Hang => std::future::pending::<()>().await,
                Step::LandThenHang => {
                    self.chain.land(self.revenue);
                    std::future::pending::<()>().await
                }
                Step::ElsewhereThenHang => {
                    self.chain.land([0x99; 20]);
                    std::future::pending::<()>().await
                }
                Step::Settle => {
                    if !self.chain.land(self.revenue) {
                        return Ok(refused("invalid_transaction_state"));
                    }
                }
                Step::EarlierLandsThenRefused => {
                    self.chain.land(self.revenue);
                    return Ok(refused("invalid_transaction_state"));
                }
                Step::RefusedOther => return Ok(refused("unexpected_settle_error")),
            }
            self.inner.settle(r).await
        })
    }
}

/// A paid server whose facilitator follows `steps`, with the authorization
/// check over `chain` (or none).
fn scripted(steps: &[Step], chain: &Arc<FakeToken>, check: bool) -> (Fixture, Arc<Scripted>) {
    let revenue = addr_bytes_of(PAY_TO);
    let fac = Arc::new(Scripted {
        inner: counting(),
        chain: chain.clone(),
        revenue,
        steps: std::sync::Mutex::new(steps.iter().copied().collect()),
        calls: AtomicU64::new(0),
    });
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_millis(2_000),
            ..ApiConfig::default()
        },
        settle_check: check.then(|| {
            Arc::new(crate::settle_check::ChainSettleCheck::new(
                chain.clone(),
                FAKE_TOKEN,
                revenue,
            )) as Arc<dyn crate::settle_check::SettleCheck>
        }),
        ..Setup::default()
    });
    (f, fac)
}

fn addr_bytes_of(hex_addr: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    a.copy_from_slice(&hex::decode(hex_addr.trim_start_matches("0x")).expect("hex"));
    a
}

/// One paid request with nonce `nonce`, the fake token told its value.
async fn paid_once(f: &Fixture, chain: &FakeToken, nonce: u8) -> (StatusCode, HeaderMap, Value) {
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    chain.value.store(
        pr.accepts[0].amount.parse().expect("amount"),
        Ordering::SeqCst,
    );
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))];
    get(f, &uri, &sig).await
}

/// M7-F5 residual (R-SETTLE-CHECK-1): a settle that landed but whose answer
/// was lost (a timeout) is not left unknown: `authorizationState` says
/// used, the token's logs show the transfer into OnyxRevenue, and the
/// request is served with that transaction; the receipt is written and the
/// intent ends. Nothing is re-sent. Without the check the payer was charged
/// and got 503.
#[tokio::test(start_paused = true)]
async fn a_timed_out_settle_that_landed_is_served_from_the_token() {
    let chain = FakeToken::new(41);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 41).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert!(s.success);
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never re-sent");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "the receipt is written");
    assert_eq!(f.state.intents.open_count(), 0, "the intent ended");
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 0);
    // Without the check the same settle stays unknown (the M7-F5 residual).
    let chain = FakeToken::new(42);
    let (f, _) = scripted(&[Step::LandThenHang], &chain, false);
    let (st, _, b) = paid_once(&f, &chain, 42).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
}

/// M7-F5 residual (R-SETTLE-CHECK-2): a timed-out settle whose
/// authorization is still unused is sent once more (the same signed
/// authorization: its nonce moves money once); success serves the request.
/// The payer is charged once.
#[tokio::test(start_paused = true)]
async fn an_unused_authorization_is_re_sent_once() {
    let chain = FakeToken::new(43);
    let (f, fac) = scripted(&[Step::Hang, Step::Settle], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 43).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
    assert_eq!(f.state.intents.open_count(), 0);
}

/// M7-F5 residual (R-SETTLE-CHECK-3, never charged twice): the first settle
/// was still in a mempool when the server re-sent it. The earlier one lands,
/// the re-send is refused by the token, and the second check finds the
/// payment: served, one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn a_re_send_racing_the_first_settle_charges_once_and_serves() {
    let chain = FakeToken::new(44);
    let (f, fac) = scripted(&[Step::Hang, Step::EarlierLandsThenRefused], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 44).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send only");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert_eq!(
        chain.state_reads.load(Ordering::SeqCst),
        2,
        "checked before and after"
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7-F5 residual (R-SETTLE-CHECK-4, never served unpaid): an authorization
/// used to pay someone else is the payer's refusal (402, nothing served, no
/// receipt); one that stays unused after the one re-send, or a node that
/// cannot be read, stays unknown (503, the unit spent, the intent left to
/// the resolver).
#[tokio::test(start_paused = true)]
async fn a_used_elsewhere_or_unread_authorization_is_never_served() {
    let chain = FakeToken::new(45);
    let (f, fac) = scripted(&[Step::ElsewhereThenHang], &chain, true);
    let (st, h, b) = paid_once(&f, &chain, 45).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::PAYMENT_REQUIRED, Some("payment_failed")),
        "{b}"
    );
    assert!(b.get("row").is_none());
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    // Still unused after the re-send.
    let chain = FakeToken::new(46);
    let (f, fac) = scripted(&[Step::Hang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 46).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        ),
        "{b}"
    );
    assert!(
        b["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("still unused")),
        "{b}"
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send, not more");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 0);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.intents.open_count(), 1, "left to the resolver");
    // A node error decides nothing: no re-send, unknown.
    let chain = FakeToken::new(47);
    chain.down.store(true, Ordering::SeqCst);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 47).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        ),
        "{b}"
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.intents.open_count(), 1);
}

/// A node behind the one that mined the settle (M7L3-R1-1): its
/// `authorizationState` reads "unused" for the next `lag` reads, or for
/// `window` after its first read, even after the transfer landed (a
/// load-balanced backend a block behind). Its logs are current.
struct Lagging {
    chain: Arc<FakeToken>,
    lag: AtomicU64,
    window: Option<(Duration, std::sync::Mutex<Option<tokio::time::Instant>>)>,
}

impl roots::JsonRpc for Lagging {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        if method == "eth_call" {
            let behind = match &self.window {
                Some((d, start)) => {
                    let mut s = start.lock().expect("lock");
                    let t0 = *s.get_or_insert_with(tokio::time::Instant::now);
                    t0.elapsed() < *d
                }
                None => self
                    .lag
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok(),
            };
            if behind {
                return Box::pin(async { Ok(json!(format!("0x{:064x}", 0))) });
            }
        }
        self.chain.call(method, params)
    }
}

/// A paid server whose facilitator follows `steps`, checking through a node
/// that lags as `node` says; the wait before a re-check is `wait`.
fn lagging(
    steps: &[Step],
    chain: &Arc<FakeToken>,
    node: Lagging,
    wait: Duration,
) -> (Fixture, Arc<Scripted>) {
    lagging_until(steps, chain, node, wait, Duration::from_millis(2_000))
}

/// [`lagging`] with the request deadline `request_timeout`.
fn lagging_until(
    steps: &[Step],
    chain: &Arc<FakeToken>,
    node: Lagging,
    wait: Duration,
    request_timeout: Duration,
) -> (Fixture, Arc<Scripted>) {
    let revenue = addr_bytes_of(PAY_TO);
    let fac = Arc::new(Scripted {
        inner: counting(),
        chain: chain.clone(),
        revenue,
        steps: std::sync::Mutex::new(steps.iter().copied().collect()),
        calls: AtomicU64::new(0),
    });
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout,
            settle_recheck_wait: wait,
            ..ApiConfig::default()
        },
        settle_check: Some(Arc::new(crate::settle_check::ChainSettleCheck::new(
            node, FAKE_TOKEN, revenue,
        ))),
        ..Setup::default()
    });
    (f, fac)
}

fn behind_by(chain: &Arc<FakeToken>, reads: u64) -> Lagging {
    Lagging {
        chain: chain.clone(),
        lag: AtomicU64::new(reads),
        window: None,
    }
}

/// M7L3-R1-1 (b) (review PoC `lagging_state_read_after_landed_settle`):
/// the first settle landed, its answer was lost, and the node read
/// "unused" on both checks. The re-send was refused (nonce used), the
/// second check said "still unused", the payer was charged and got 503, and
/// the identical retry got 409 "nothing was charged". A re-send the token
/// refused as used now has the second check read the token's logs: the
/// transfer into OnyxRevenue is found and the request is served, charged
/// once, one receipt.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_a_refused_re_send_reads_the_logs_past_a_lagging_node() {
    let chain = FakeToken::new(61);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang, Step::Settle],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, h, b) = paid_once(&f, &chain, 61).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "one re-send");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
}

/// M7L3-R1-1 (c): a payment whose settle ended unknown (the node lagged
/// through both checks, the re-send's answer was lost) is served on its
/// identical retry once the token proves the transfer: the retry runs the
/// authorization check instead of getting 409, never settles again, and is
/// charged once with one receipt. Another request with the same
/// authorization still gets 409.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_the_identical_retry_of_an_unknown_settle_is_checked_and_served() {
    let chain = FakeToken::new(64);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 64).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.intents.open_count(), 1);
    // Another resource with the same authorization: still one settle.
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, ho, _) = get(&f, &other, &[]).await;
    let (st, _, b) = get(
        &f,
        &other,
        &[(x402::PAYMENT_SIGNATURE, pay(&ho, "payer a", 64))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::CONFLICT, Some("payment_in_use")),
        "{b}"
    );
    // The identical retry (same header, resource and client).
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 64))];
    let (st, h, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    assert!(h.get(x402::PAYMENT_RESPONSE).is_some());
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "never settled again");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
    // A third identical request replays the settled answer.
    let (st, _, _) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7L3-R1-1 (a): the check after a re-send waits about one block first.
/// The node reads "unused" for 40 ms after its first read; the re-send is
/// refused for another reason (so the logs are not read regardless). With
/// the wait the second check sees the landed transfer and serves; an
/// immediate second check read "still unused" (503, charged).
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_the_second_check_waits_one_block() {
    let chain = FakeToken::new(65);
    let node = Lagging {
        chain: chain.clone(),
        lag: AtomicU64::new(0),
        window: Some((Duration::from_millis(40), std::sync::Mutex::new(None))),
    };
    let (f, fac) = lagging(
        &[Step::LandThenHang, Step::RefusedOther],
        &chain,
        node,
        Duration::from_millis(90),
    );
    assert_eq!(f.state.settle_recheck_wait(), Duration::from_millis(90));
    let (st, _, b) = paid_once(&f, &chain, 65).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// M7L3-R1-1 (review PoC `wrong_value_is_not_served`): a transfer into
/// OnyxRevenue for another value is not proof of this payment: never
/// served, no receipt, also not on the identical retry.
#[tokio::test(start_paused = true)]
async fn m7l3_r1_1_a_transfer_of_another_value_is_never_served() {
    let chain = FakeToken::new(63);
    let (f, _) = scripted(&[Step::LandThenHang], &chain, true);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    let v: u64 = pr.accepts[0].amount.parse().expect("amount");
    chain.value.store(v + 1, Ordering::SeqCst);
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 63))];
    for _ in 0..2 {
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_ne!(st, StatusCode::OK, "{b}");
        assert!(b.get("row").is_none());
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.receipts.pending(), 0);
    }
}

/// M7L3-R2-1 (review PoC A): the resolver (every 30 s by default) recovered
/// the payment of a request whose settle ended unknown before the payer's
/// identical retry came. The intent is gone, so the retry used to take the
/// fresh path: a new verify and settle (with a real facilitator, 402 on the
/// used nonce) although the payer was charged. The resolver now keeps a
/// proof for that request: the retry is served from it, with the
/// transaction that paid, and no new settle; one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn m7l3_r2_1_the_identical_retry_after_the_resolver_is_served_from_its_proof() {
    let chain = FakeToken::new(68);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 68).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    let out = r.resolve_once().await;
    assert_eq!(out.recovered, 1, "{out:?}");
    assert_eq!(f.state.intents.open_count(), 0);
    assert_eq!(f.state.receipts.pending(), 1);
    // The fake chain's clock is not the fixture's: as if they agreed.
    f.state.intents.set_skew(0);
    let calls = fac.calls.load(Ordering::SeqCst);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 68))];
    let (st, h, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    let s: SettleResponse = decode_header(
        h.get(x402::PAYMENT_RESPONSE)
            .expect("payment response")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert!(s.success);
    assert_eq!(s.transaction, PAID_TX);
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
    // Further identical retries replay the stored answer; the proof is
    // used once.
    let (st, _, _) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls);
    assert_eq!(f.state.receipts.pending(), 1);
}

/// SOAK-1 (L6 soak, found with an RPC proxy that drops answers): without
/// the authorization check (a self-settling facilitator, ADR-0026), a
/// settle that landed but whose answer was lost left its intent open, and
/// the payer's identical retry took the fresh path and got 409
/// `payment_in_use` "already being settled for another request; this
/// request added no charge", though it is the same request and the payer
/// was charged: a client trusting it signs a new authorization and pays
/// twice. The identical retry now gets 503 `payment_pending` with
/// `Retry-After`, never settles again, and once the resolver proved the
/// transfer it is served from that proof: one settle, one transfer, one
/// receipt. Another request with the same authorization still gets 409.
#[tokio::test(start_paused = true)]
async fn soak_1_the_identical_retry_of_an_unknown_self_settle_is_told_to_retry() {
    let chain = FakeToken::new(44);
    let (f, fac) = scripted(&[Step::LandThenHang], &chain, false);
    let (st, _, b) = paid_once(&f, &chain, 44).await;
    assert_eq!(
        (st, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "the payer paid");
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 44))];
    let (st, hs, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("payment_pending")),
        "{b}"
    );
    assert_eq!(
        hs.get("retry-after").and_then(|v| v.to_str().ok()),
        Some(PAYMENT_PENDING_RETRY_SECS.to_string().as_str())
    );
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never settled again");
    // Another resource with the same authorization: 409, as before.
    let other = format!("/v1/proofs/1/{}/2", f.shard);
    let (_, ho, _) = get(&f, &other, &[]).await;
    let (st, _, b) = get(
        &f,
        &other,
        &[(x402::PAYMENT_SIGNATURE, pay(&ho, "payer a", 44))],
    )
    .await;
    assert_eq!(
        (st, code(&b)),
        (StatusCode::CONFLICT, Some("payment_in_use")),
        "{b}"
    );
    // The resolver proves the transfer; the identical retry is served.
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    assert_eq!(r.resolve_once().await.recovered, 1);
    f.state.intents.set_skew(0);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b.get("row").is_some(), "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.receipts.pending(), 1, "one receipt");
    assert_eq!(f.state.intents.open_count(), 0);
}

/// A scripted facilitator that credits the shard in its own settle, as
/// `self_settle::SelfSettle` does (ADR-0026: `credits_on_settle` true).
struct SelfCrediting(Arc<Scripted>);

impl Facilitator for SelfCrediting {
    fn verify<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        self.0.verify(r)
    }
    fn settle<'a>(
        &'a self,
        r: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        self.0.settle(r)
    }
    fn credits_on_settle(&self) -> bool {
        true
    }
}

/// SOAK-1, review NB1: the same with a facilitator that credits on settle
/// (self-settle). Such a server skips the authorization check even when one
/// is configured (`checks_unknown_settles`), so the identical retry of an
/// unknown settle takes the `payment_pending` path, with or without a
/// check: 503 with `Retry-After`, no second settle, then served from the
/// resolver's proof; one transfer, one receipt.
#[tokio::test(start_paused = true)]
async fn soak_1_a_self_crediting_facilitator_tells_the_identical_retry_to_wait() {
    for (nonce, check) in [(45u8, false), (46, true)] {
        let chain = FakeToken::new(nonce);
        let revenue = addr_bytes_of(PAY_TO);
        let fac = Arc::new(Scripted {
            inner: counting(),
            chain: chain.clone(),
            revenue,
            steps: std::sync::Mutex::new([Step::LandThenHang].into_iter().collect()),
            calls: AtomicU64::new(0),
        });
        let f = build(Setup {
            paid: true,
            facilitator: Some(Arc::new(SelfCrediting(fac.clone()))),
            config: ApiConfig {
                facilitator_timeout: Duration::from_millis(50),
                request_timeout: Duration::from_millis(2_000),
                ..ApiConfig::default()
            },
            settle_check: check.then(|| {
                Arc::new(crate::settle_check::ChainSettleCheck::new(
                    chain.clone(),
                    FAKE_TOKEN,
                    revenue,
                )) as Arc<dyn crate::settle_check::SettleCheck>
            }),
            ..Setup::default()
        });
        assert!(
            !f.state.checks_unknown_settles(),
            "self-settle runs no check"
        );
        let (st, _, b) = paid_once(&f, &chain, nonce).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.intents.open_count(), 1);
        assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "the payer paid");
        let uri = format!("/v1/proofs/1/{}/1", f.shard);
        let (_, h, _) = get(&f, &uri, &[]).await;
        let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", nonce))];
        let (st, hs, b) = get(&f, &uri, &sig).await;
        assert_eq!(
            (st, code(&b)),
            (StatusCode::SERVICE_UNAVAILABLE, Some("payment_pending")),
            "check {check}: {b}"
        );
        assert!(hs.get("retry-after").is_some());
        assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "never settled again");
        let r = crate::intents::IntentResolver::new(
            chain.clone(),
            FAKE_TOKEN,
            revenue,
            f.state.intents.clone(),
            f.state.receipts.clone(),
        )
        .with_clock(f.state.clock.clone());
        assert_eq!(r.resolve_once().await.recovered, 1);
        f.state.intents.set_skew(0);
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_eq!(st, StatusCode::OK, "check {check}: {b}");
        assert_eq!(fac.calls.load(Ordering::SeqCst), 1, "no new settle");
        assert_eq!(chain.transfers.load(Ordering::SeqCst), 1, "charged once");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        assert_eq!(f.state.receipts.pending(), 1, "one receipt");
        assert_eq!(f.state.intents.open_count(), 0);
    }
}

/// Raise the default price of the fixture's rows to `micro_usdc`.
fn set_price(f: &Fixture, micro_usdc: u64) {
    f.state.catalog.write().expect("lock").default_pricing = Some(PricingPolicy {
        base_price_micro_usdc: micro_usdc,
        fresh_window_secs: 3_600,
    });
}

/// M7L3-R3-5: the proof path compares the payment with the requirements
/// offered now, like a fresh payment: after a price change the identical
/// retry is not served from the proof at the old price (402 with the new
/// terms, the proof untouched); at the old price again it is.
#[tokio::test(start_paused = true)]
async fn m7l3_r3_5_the_proof_path_checks_the_current_price() {
    let chain = FakeToken::new(71);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 71).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    let r = crate::intents::IntentResolver::new(
        chain.clone(),
        FAKE_TOKEN,
        addr_bytes_of(PAY_TO),
        f.state.intents.clone(),
        f.state.receipts.clone(),
    )
    .with_clock(f.state.clock.clone());
    assert_eq!(r.resolve_once().await.recovered, 1);
    f.state.intents.set_skew(0);
    assert_eq!(f.state.intents.proven_count(), 1);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 71))];
    let calls = fac.calls.load(Ordering::SeqCst);
    set_price(&f, 2_000);
    let (st, h2, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{b}");
    assert_eq!(
        b["error"]["message"],
        "payment does not match the requirements"
    );
    assert!(b.get("row").is_none());
    let pr: PaymentRequired = decode_header(
        h2.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    assert_eq!(pr.accepts[0].amount, "2000", "the new terms");
    assert_eq!(f.state.intents.proven_count(), 1, "the proof is untouched");
    set_price(&f, 1_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R3-5: the retry path (an open intent, the authorization check)
/// also compares the payment with the requirements offered now: after a
/// price change the identical retry gets 402 and does not take the intent.
#[tokio::test(start_paused = true)]
async fn m7l3_r3_5_the_retry_path_checks_the_current_price() {
    let chain = FakeToken::new(72);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
    );
    let (st, _, b) = paid_once(&f, &chain, 72).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 72))];
    let calls = fac.calls.load(Ordering::SeqCst);
    set_price(&f, 2_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{b}");
    assert_eq!(
        b["error"]["message"],
        "payment does not match the requirements"
    );
    assert!(b.get("row").is_none());
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(
        f.state.receipts.pending(),
        0,
        "not served, nothing recorded"
    );
    assert_eq!(f.state.intents.due().len(), 1, "still the resolver's");
    set_price(&f, 1_000);
    let (st, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), calls, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R2-2 (review drop-order PoC): when a settle task ends without a
/// success, its intent goes back to the resolver BEFORE its claim is
/// released, so a waiting identical retry woken by the claim finds the
/// intent (`awaiting`) instead of one still marked as the task's. The test
/// holds the intent log's lock while the task ends: the task must block on
/// the intent's release with its claim still held. Released first, the
/// claim was gone while the intent was still the task's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m7l3_r2_2_the_intent_is_released_before_the_claim() {
    // Real time (a std thread holds the lock), so no deadline here may race
    // the machine (E12): the request deadline, which this test does not
    // check, is far above the time the test holds the lock (a 2 s one gave
    // 504 at load 67-73), and every wait is a condition with a 60 s cap.
    const CAP: Duration = Duration::from_secs(60);
    let chain = FakeToken::new(69);
    let node = behind_by(&chain, 2);
    let (f, fac) = lagging_until(
        &[Step::LandThenHang],
        &chain,
        node,
        Duration::from_millis(10),
        CAP,
    );
    let f = Arc::new(f);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let pr: PaymentRequired = decode_header(
        h.get(x402::PAYMENT_REQUIRED)
            .expect("challenge")
            .to_str()
            .expect("ascii"),
    )
    .expect("decodes");
    chain.value.store(
        pr.accepts[0].amount.parse().expect("amount"),
        Ordering::SeqCst,
    );
    let sig = pay(&h, "payer a", 69);
    let (f1, u1, s1) = (f.clone(), uri.clone(), sig.clone());
    let first = tokio::spawn(async move { get(&f1, &u1, &[(x402::PAYMENT_SIGNATURE, s1)]).await });
    // Past the re-send's intent access (`take_resend`): the re-send started.
    let deadline = std::time::Instant::now() + CAP;
    while fac.calls.load(Ordering::SeqCst) < 2 {
        assert!(std::time::Instant::now() < deadline, "no re-send");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (free_tx, free_rx) = std::sync::mpsc::channel::<()>();
    let log = f.state.intents.clone();
    let holder = std::thread::spawn(move || {
        log.hold_lock_for_test(|| {
            let _ = locked_tx.send(());
            let _ = free_rx.recv();
        })
    });
    locked_rx.recv().expect("locked");
    // The task reached its end (the outcome is unknown) ... The runtime's
    // timers may stall while a worker blocks on the lock: this thread
    // sleeps without them.
    while f.state.unknown_settles.load(Ordering::SeqCst) < 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the settle never ended"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(100));
    // ... and blocks on releasing its intent, its claim still held.
    let claimed = f.state.caches.claimed();
    free_tx.send(()).expect("free");
    holder.join().expect("holder");
    assert_eq!(claimed, 1, "the claim was released before the intent");
    let (st, _, b) = first.await.expect("join");
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(CAP).await);
    assert_eq!(f.state.caches.claimed(), 0);
    // The identical retry takes the intent over and is served.
    let (st, _, b) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(fac.calls.load(Ordering::SeqCst), 2, "no new settle");
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 1);
}

/// M7L3-R2-5: identical retries of a request whose authorization stays
/// unused take no new payer unit, so each re-sending the settle is capped:
/// at most `MAX_RESENDS` re-sends per authorization, the first request's
/// included. Past it the check still runs, nothing is sent.
#[tokio::test(start_paused = true)]
async fn m7l3_r2_5_re_sends_of_one_authorization_are_capped() {
    let chain = FakeToken::new(70);
    let (f, fac) = scripted(&[Step::Hang], &chain, true);
    let (st, _, b) = paid_once(&f, &chain, 70).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(
        fac.calls.load(Ordering::SeqCst),
        2,
        "the settle and one re-send"
    );
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 70))];
    let max = u64::from(crate::intents::MAX_RESENDS);
    for n in 1..=max + 1 {
        let (st, _, b) = get(&f, &uri, &sig).await;
        assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
        assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
        let sent = fac.calls.load(Ordering::SeqCst) - 1;
        assert_eq!(sent, (n + 1).min(max), "retry {n}: re-sends so far");
        if n + 1 > max {
            assert!(
                b["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("not re-sent")),
                "{b}"
            );
        }
    }
    assert_eq!(chain.transfers.load(Ordering::SeqCst), 0);
    assert_eq!(f.state.intents.open_count(), 1, "left to the resolver");
}
