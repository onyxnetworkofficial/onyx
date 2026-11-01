//! The money loop (ADR-0025): contributor reads with a one-time proof, their rate limits,
//! receipts on disk before the answer, payment intents, and settle slots.

use super::*;

fn sk(label: &str) -> secp256k1::SecretKey {
    secp256k1::SecretKey::from_slice(&secret(label)).expect("key")
}

fn eth(label: &str) -> [u8; 20] {
    exact_evm::address(&secret(label)).expect("key")
}

/// A server where "miner m" did accepted work at unix 1,400 (30-day window).
fn contributor_fixture(fac: Option<Arc<dyn Facilitator>>, per_minute: u32) -> Fixture {
    let src = Arc::new(StaticContributors::new(30 * 86_400));
    src.insert(eth("miner m"), 1_400);
    let mut policy = ContributorPolicy::new(src);
    policy.reads_per_minute = per_minute;
    build(Setup {
        paid: true,
        facilitator: fac,
        contributors: Some(policy),
        ..Setup::default()
    })
}

/// ADR-0025 item 3: a contributor reads fresh data free with a one-time proof
/// for that exact URL; the answer is private and nothing is settled.
#[tokio::test(start_paused = true)]
async fn contributors_read_fresh_data_free_with_a_one_time_proof() {
    let fac = counting();
    let f = contributor_fixture(Some(fac.clone()), 60);
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let proof = crate::contrib::sign(&sk("miner m"), &resource, 1_500, "off-chain");
    let (s, h, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, proof.clone())]).await;
    assert_eq!(s, StatusCode::OK);
    assert!(verifies(&b));
    assert!(!h.contains_key(x402::PAYMENT_RESPONSE));
    assert_eq!(cc(&h), "private, no-cache", "never cached for others");
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.state.receipts.pending(),
        0,
        "a free read earns no revenue"
    );
    // The same proof again: refused (one use).
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, proof)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, Some("invalid_contributor_proof"))
    );
    // HEAD never uses a proof (its body would be stripped).
    let fresh = crate::contrib::sign(&sk("miner m"), &resource, 1_499, "off-chain");
    let (s, _, b) = send(
        &f,
        Method::HEAD,
        &uri,
        &[(CONTRIBUTOR_HEADER, fresh.clone())],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(b, Value::Null, "HEAD has no body");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, fresh)]).await;
    assert_eq!(s, StatusCode::OK, "the proof was not used by the HEAD");
    // A proof for another row is refused here.
    let other = crate::contrib::sign(&sk("miner m"), &format!("{resource}x"), 1_500, "off-chain");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, other)]).await;
    assert_eq!(code(&b), Some("invalid_contributor_proof"));
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Someone with no accepted work: refused (they pay instead).
    let stranger = crate::contrib::sign(&sk("stranger"), &resource, 1_500, "off-chain");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, stranger)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, Some("not_a_contributor"))
    );
    // (The pass lapsing after its window: `contrib::tests`.)
}

/// ADR-0025 item 3: free readers cannot drain the service; each address has
/// its own read budget, whatever IP it comes from.
#[tokio::test(start_paused = true)]
async fn contributor_reads_are_rate_limited_per_address() {
    let f = contributor_fixture(mock(), 1);
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let p1 = crate::contrib::sign(&sk("miner m"), &resource, 1_500, "off-chain");
    let p2 = crate::contrib::sign(&sk("miner m"), &resource, 1_501, "off-chain");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p1)]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, h, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p2)]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    assert!(h.contains_key("retry-after"));
}

/// Counts how often the contributor source is asked (REV-L1-F1).
struct CountingSource {
    inner: StaticContributors,
    asked: AtomicU64,
}

impl crate::contrib::ContributorSource for CountingSource {
    fn is_contributor<'a>(
        &'a self,
        who: [u8; 20],
        now_unix: u64,
    ) -> x402::BoxFuture<'a, Result<bool, crate::roots::RootError>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.inner.is_contributor(who, now_unix)
    }
}

/// REV-L1-F1: requests signed by throwaway keys (free to make, no accepted
/// work) are charged to the sending client's own proof-check budget before the
/// source is asked; they never enter the contributor budget table, so a
/// contributor's spent budget is never evicted and refilled by churn. A
/// confirmed contributor is not looked up again on every read.
#[tokio::test(start_paused = true)]
async fn throwaway_keys_never_refill_a_contributor_budget() {
    let inner = StaticContributors::new(30 * 86_400);
    inner.insert(eth("miner m"), 1_400);
    let src = Arc::new(CountingSource {
        inner,
        asked: AtomicU64::new(0),
    });
    let mut policy = ContributorPolicy::new(src.clone());
    policy.reads_per_minute = 2;
    policy.checks_per_minute = 8;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let miner = |t: u64| crate::contrib::sign(&sk("miner m"), &resource, t, "off-chain");
    let ip: Option<IpAddr> = Some("198.51.100.7".parse().expect("ip"));
    let other: Option<IpAddr> = Some("203.0.113.9".parse().expect("ip"));
    for t in [1_500, 1_501] {
        let (s, _, _) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, miner(t))], ip).await;
        assert_eq!(s, StatusCode::OK);
    }
    assert_eq!(
        src.asked.load(Ordering::SeqCst),
        1,
        "status cached after the first read"
    );
    let (s, _, b) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, miner(1_502))],
        ip,
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    // Another client floods throwaway keys: 403 until its own check budget
    // is spent, then 429; the source is asked at most that budget.
    let mut forbidden = 0;
    for i in 0..50u64 {
        let junk = crate::contrib::sign(&sk(&format!("junk {i}")), &resource, 1_500, "off-chain");
        let (s, _, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, junk)], other).await;
        match (s, code(&b)) {
            (StatusCode::FORBIDDEN, Some("not_a_contributor")) => forbidden += 1,
            (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")) => {}
            x => panic!("junk {i}: {x:?}"),
        }
    }
    assert_eq!(forbidden, 8, "the flooder's own budget");
    assert!(src.asked.load(Ordering::SeqCst) <= 9);
    assert_eq!(
        f.state.caches.contributors_tracked(),
        1,
        "junk never enters the table"
    );
    // The miner's budget is still spent.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, miner(1_503))],
        ip,
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

/// REV-L2-F1: a used proof is refused before any budget is charged, so
/// whoever holds one (it is a bearer token) cannot replay it from other IPs to
/// spend the contributor's reads and lock them out.
#[tokio::test(start_paused = true)]
async fn replaying_a_used_proof_costs_its_signer_nothing() {
    let f = contributor_fixture(mock(), 2);
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let p = |t: u64| crate::contrib::sign(&sk("miner m"), &resource, t, "off-chain");
    let miner: Option<IpAddr> = Some("198.51.100.7".parse().expect("ip"));
    let used = p(1_500);
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, used.clone())],
        miner,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for i in 0..5u8 {
        let ip: Option<IpAddr> = Some(format!("203.0.113.{i}").parse().expect("ip"));
        let (s, _, b) = send(
            &f,
            Method::GET,
            &uri,
            &[(CONTRIBUTOR_HEADER, used.clone())],
            ip,
        )
        .await;
        assert_eq!(
            (s, code(&b)),
            (StatusCode::FORBIDDEN, Some("invalid_contributor_proof")),
            "replay {i}"
        );
    }
    // The miner's second read of the minute is still there.
    let (s, _, _) = send(
        &f,
        Method::GET,
        &uri,
        &[(CONTRIBUTOR_HEADER, p(1_501))],
        miner,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "replays spent none of the miner's reads");
}

/// REV-L2-F2: contributor-source load has a ceiling. A "not a contributor"
/// answer is reused for a minute (one junk key re-signed from many IPs asks
/// the source once), and lookups for new addresses share one server-wide
/// budget (503 `contributor_checks_busy` beyond), however many IPs send them.
#[tokio::test(start_paused = true)]
async fn non_contributors_are_remembered_and_source_lookups_are_capped() {
    let src = Arc::new(CountingSource {
        inner: StaticContributors::new(30 * 86_400),
        asked: AtomicU64::new(0),
    });
    let mut policy = ContributorPolicy::new(src.clone());
    policy.source_calls_per_minute = 5;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let resource = format!("https://api.onyx.test{uri}");
    let junk = sk("junk");
    for ip_i in 0..10u8 {
        let ip: Option<IpAddr> = Some(format!("192.0.2.{ip_i}").parse().expect("ip"));
        for t in 0..20u64 {
            let h = crate::contrib::sign(&junk, &resource, 1_400 + t, "off-chain");
            let (s, _, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, h)], ip).await;
            assert_eq!(
                (s, code(&b)),
                (StatusCode::FORBIDDEN, Some("not_a_contributor"))
            );
        }
    }
    assert_eq!(src.asked.load(Ordering::SeqCst), 1, "answer reused");
    // New keys from new IPs: 4 more lookups fit the budget, then 503.
    let (mut forbidden, mut busy) = (0, 0);
    for i in 0..20u8 {
        let ip: Option<IpAddr> = Some(format!("198.18.0.{i}").parse().expect("ip"));
        let h = crate::contrib::sign(&sk(&format!("key {i}")), &resource, 1_500, "off-chain");
        let (s, h, b) = send(&f, Method::GET, &uri, &[(CONTRIBUTOR_HEADER, h)], ip).await;
        match (s, code(&b)) {
            (StatusCode::FORBIDDEN, Some("not_a_contributor")) => forbidden += 1,
            (StatusCode::SERVICE_UNAVAILABLE, Some("contributor_checks_busy")) => {
                assert!(h.contains_key("retry-after"));
                busy += 1;
            }
            x => panic!("key {i}: {x:?}"),
        }
    }
    assert_eq!((forbidden, busy), (4, 16));
    assert_eq!(
        src.asked.load(Ordering::SeqCst),
        5,
        "the server-wide budget"
    );
}

/// REV-L1-F5: a contributor proof is a bearer token, so it never buys a whole
/// shard (the export is paid; the proof is not used), and an address's free
/// reads are capped in rows per day as well as per minute.
#[tokio::test(start_paused = true)]
async fn contributor_proofs_never_buy_an_export_and_rows_per_day_are_capped() {
    let src = Arc::new(StaticContributors::new(30 * 86_400));
    src.insert(eth("miner m"), 1_400);
    let mut policy = ContributorPolicy::new(src);
    policy.rows_per_day = 1;
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        contributors: Some(policy),
        ..Setup::default()
    });
    let export = format!("/v1/export/1/{}", f.shard);
    let res = format!("https://api.onyx.test{export}");
    let proof = crate::contrib::sign(&sk("miner m"), &res, 1_500, "off-chain");
    let (s, h, _) = get(&f, &export, &[(CONTRIBUTOR_HEADER, proof)]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED, "exports are always paid");
    assert!(h.contains_key(x402::PAYMENT_REQUIRED));
    let uri = format!("/v1/proofs/1/{}/0", f.shard);
    let res = format!("https://api.onyx.test{uri}");
    let p = |t: u64| crate::contrib::sign(&sk("miner m"), &res, t, "off-chain");
    let (s, _, _) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p(1_500))]).await;
    assert_eq!(s, StatusCode::OK, "the export did not use the day's row");
    let (s, _, b) = get(&f, &uri, &[(CONTRIBUTOR_HEADER, p(1_501))]).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
}

/// ADR-0025 item 2: each settled payment leaves one receipt naming the shard
/// served, the payer and the EIP-3009 nonce; a replayed answer leaves none.
#[tokio::test(start_paused = true)]
async fn settled_payments_leave_one_receipt_for_the_served_shard() {
    let f = fixture(true, mock());
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = pay(&h, "payer a", 9);
    let (s, _, _) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig.clone())]).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = get(&f, &uri, &[(x402::PAYMENT_SIGNATURE, sig)]).await;
    assert_eq!(s, StatusCode::OK);
    let got = f.state.receipts.drain(10);
    assert_eq!(
        got,
        vec![crate::receipts::Receipt {
            round: 1,
            shard: f.shard,
            amount: 1_000,
            payer: eth("payer a"),
            nonce: [9; 32],
        }]
    );
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["payTo"], PAY_TO);
    assert_eq!(health["receiptsPending"], 0);
    assert_eq!(health["receiptLog"], "memory");
    assert_eq!(health["receiptWriteErrors"], 0);
}

/// REV-11: with a receipt journal, the receipt of a paid answer is on disk
/// when the answer returns; a restarted server (a new state on the same file)
/// still holds it for the reporter.
#[tokio::test(start_paused = true)]
async fn a_paid_answers_receipt_is_on_disk_before_the_answer_returns() {
    let d = crate::journal::test_dir("api-receipts");
    let path = d.join("receipts.jsonl");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(path.clone()),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 9))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["receiptLog"], "file");
    assert_eq!(health["receiptsPending"], 1);
    // While this server runs, nobody else may open its journal (REV-L3-F4).
    let busy = crate::receipts::ReceiptLog::open(&path, &crate::journal::TEST_BINDING);
    assert!(busy.is_err(), "one server per journal");
    // Stopped: read the file as a restarted server would.
    drop(f);
    let reopened =
        crate::receipts::ReceiptLog::open(&path, &crate::journal::TEST_BINDING).expect("reopen");
    assert_eq!(reopened.peek(9).len(), 1);
    assert_eq!(reopened.peek(9)[0].nonce, [9; 32]);
    std::fs::remove_dir_all(d).ok();
}

/// REV-L4-F1: `/v1/health` shows the reporter's credits in flight and since
/// when one is stuck, with its last error; `null` without a reporter.
#[tokio::test(start_paused = true)]
async fn health_shows_a_stuck_reporter() {
    struct Stuck;
    impl crate::reporter::ReporterHealth for Stuck {
        fn reporter_status(&self) -> crate::reporter::ReporterStatus {
            crate::reporter::ReporterStatus {
                posts: 7,
                applied: 40,
                in_flight: 2,
                stuck_since: Some(1_700_000_000),
                write_errors: 0,
                last_error: Some("credit at nonce 9 unmined".into()),
            }
        }
    }
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        reporter: Some(Arc::new(Stuck)),
        ..Setup::default()
    });
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    let r = &health["reporter"];
    assert_eq!(r["inFlight"], 2);
    assert_eq!(r["stuckSince"], 1_700_000_000u64);
    assert_eq!(r["posts"], 7);
    assert_eq!(r["lastError"], "credit at nonce 9 unmined");
    let (_, _, health) = get(&fixture(true, mock()), "/v1/health", &[]).await;
    assert!(health["reporter"].is_null());
}

/// ROADMAP C62: `/v1/health` carries the level-3 store's `level3` object
/// as it reports it, and `null` without a level-3 store.
#[tokio::test(start_paused = true)]
async fn c62_health_shows_the_level_3_post_log() {
    struct Posts;
    impl crate::Level3Health for Posts {
        fn level3_health(&self) -> Value {
            json!({"postsPending": 2, "postsRefused": 1, "oldestPendingSecs": 299, "stopped": null})
        }
    }
    let f = build(Setup {
        level3: Some(Arc::new(Posts)),
        ..Setup::default()
    });
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(
        health["level3"],
        json!({"postsPending": 2, "postsRefused": 1, "oldestPendingSecs": 299, "stopped": null})
    );
    let (_, _, health) = get(&fixture(false, None), "/v1/health", &[]).await;
    assert!(health["level3"].is_null(), "{health}");
    assert!(health.as_object().expect("object").contains_key("level3"));
}

/// REV-L3-F5: once a receipt write failed, the server refuses new paid
/// requests (503, before anything is verified or charged) until it is
/// drained and restarted; `/v1/health` says it is not accepting payments.
#[tokio::test(start_paused = true)]
async fn a_failed_receipt_write_refuses_new_payments() {
    let d = crate::journal::test_dir("api-receipts-fail");
    let path = d.join("receipts.jsonl");
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        receipt_log: Some(path.clone()),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let (s, _, _) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 1))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["acceptingPayments"], true);
    // The disk fails on the next write.
    f.state.receipts.break_journal_for_test();
    f.state.receipts.record(crate::receipts::Receipt {
        round: 1,
        shard: f.shard,
        amount: 1,
        payer: [1; 20],
        nonce: [1; 32],
    });
    assert_eq!(f.state.receipts.write_errors(), 1);
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 2))],
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(b["error"]["code"], "receipt_log_unavailable");
    let (_, _, health) = get(&f, "/v1/health", &[]).await;
    assert_eq!(health["acceptingPayments"], false);
    assert_eq!(health["receiptWriteErrors"], 1);
    std::fs::remove_dir_all(d).ok();
}

/// ADR-0025 item 4: an agent buys a whole fresh dataset at a flat price,
/// never more than every row and never less than the floor; old data is free.
#[tokio::test(start_paused = true)]
async fn datasets_have_a_flat_price_between_the_floor_and_every_row() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        dataset: Some(DatasetPricing {
            flat_micro_usdc: 2_000,
            floor_rows: 1,
        }),
        ..Setup::default()
    });
    let uri = format!("/v1/export/1/{}", f.shard);
    let (s, h, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(
        pr.accepts[0].amount, "2000",
        "3 rows at 1,000 each, flat 2,000"
    );
    let (s, _, b) = get(
        &f,
        &uri,
        &[(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 3))],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["rows"].as_array().map(Vec::len), Some(3));
    // One row still costs one row.
    let (_, h, _) = get(&f, &format!("/v1/proofs/1/{}/0", f.shard), &[]).await;
    let pr: PaymentRequired =
        decode_header(h[x402::PAYMENT_REQUIRED].to_str().expect("ascii")).expect("challenge");
    assert_eq!(pr.accepts[0].amount, "1000");
    // Past the fresh window the dataset is free.
    f.now.store(4_600, Ordering::SeqCst);
    let (s, _, _) = get(&f, &uri, &[]).await;
    assert_eq!(s, StatusCode::OK);
}

/// OPS-L2-F1, F3: a settle whose outcome is unknown (here, a facilitator
/// slower than the whole settle patience) stays a settle intent on disk for
/// the resolver, is counted, and shows on `/v1/health`; no receipt is
/// guessed.
#[tokio::test(start_paused = true)]
async fn an_unknown_settle_stays_an_intent_and_is_counted() {
    let dir = crate::journal::test_dir("api-intent-unknown");
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(Arc::new(Slow {
            inner: fac.clone(),
            settle_ms: Some(400),
        })),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_millis(1_000),
            ..ApiConfig::default()
        },
        intent_log: Some(dir.join("settle-intents.jsonl")),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 21))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("facilitator_unavailable")
        )
    );
    assert!(f.state.settle_tasks.wait_idle(Duration::from_secs(2)).await);
    assert_eq!(f.state.intents.open_count(), 1);
    assert_eq!(f.state.intents.due().len(), 1, "released to the resolver");
    assert_eq!(f.state.receipts.pending(), 0);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 1);
    let (_, _, hb) = get(&f, "/v1/health", &[]).await;
    assert_eq!(hb["settles"]["unknownOutcomes"], 1);
    assert_eq!(hb["settles"]["intentsOpen"], 1);
    assert_eq!(hb["settles"]["intentLog"], "file");
    // The intent survives a restart (it is on disk).
    drop(f);
    let again = crate::intents::IntentLog::open(
        dir.join("settle-intents.jsonl"),
        &crate::journal::TEST_BINDING,
    )
    .expect("reopen");
    assert_eq!(again.due().len(), 1);
}

/// OPS-L2-F1: a settled payment ends its intent once its receipt is on disk.
#[tokio::test(start_paused = true)]
async fn a_settled_payment_ends_its_intent() {
    let f = build(Setup {
        paid: true,
        facilitator: mock(),
        ..Setup::default()
    });
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 22))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(f.state.receipts.pending(), 1);
    assert_eq!(f.state.intents.open_count(), 0);
    assert_eq!(f.state.unknown_settles.load(Ordering::SeqCst), 0);
}

/// OPS-L2-F1: an intent that cannot be made durable means no settle at all
/// (fail closed): nothing is charged and paid requests get 503.
#[tokio::test(start_paused = true)]
async fn a_broken_intent_log_refuses_before_settling() {
    let dir = crate::journal::test_dir("api-intent-broken");
    let fac = counting();
    let f = build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        intent_log: Some(dir.join("settle-intents.jsonl")),
        ..Setup::default()
    });
    f.state.intents.break_journal_for_test();
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 23))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("settle_intent_unavailable")
        )
    );
    assert_eq!(fac.settles.load(Ordering::SeqCst), 0, "never settled");
    // The failed write closes the log: later payments are refused before
    // verify, like a receipt log write error (REV-L3-F5).
    let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, "payer a", 24))];
    let (s, _, b) = get(&f, &uri, &sig).await;
    assert_eq!(
        (s, code(&b)),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("receipt_log_unavailable")
        )
    );
    let (_, _, hb) = get(&f, "/v1/health", &[]).await;
    assert_eq!(hb["acceptingPayments"], false);
}

/// Counts settles running at once (OPS-L2-F2).
struct Concurrent {
    inner: MockFacilitator,
    running: AtomicU64,
    max_running: AtomicU64,
}

impl Facilitator for Concurrent {
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
        Box::pin(async move {
            let n = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_running.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            self.inner.settle(r).await
        })
    }
}

/// OPS-L2-F2 (review 2's slot-queue case): the slot taken at verify is held
/// through the settle, so settle tasks never queue for a slot outside their
/// timeout: at most `max_facilitator_calls` exist at once, and after the
/// last request every one ends within the settle budget.
///
/// E10: the clock is paused (time moves only when every task waits on a
/// timer), so the 80 ms settle always fits its 100 ms timeout. On the real
/// clock a loaded machine timed every settle out (2 of 10 suite runs at load
/// ~40 on 2026-10-16 had no payment settled).
///
/// E10 review: a payer told `503` retries 10 ms later (it used to spend all
/// six nonces at once, so only two payers ever contended and a settle that
/// queued for a slot passed), and a sampler counts settle tasks every 1 ms,
/// not only when a request returns. With the slot dropped after verify and
/// taken again by the settle task, `max_live` reaches 8.
#[tokio::test(start_paused = true)]
async fn settles_never_queue_for_a_facilitator_slot() {
    let payers: Vec<String> = (0..8).map(|i| format!("slot payer {i}")).collect();
    let fac = Arc::new(Concurrent {
        inner: MockFacilitator::new(payers.iter().map(|p| addr(p))),
        running: AtomicU64::new(0),
        max_running: AtomicU64::new(0),
    });
    let slots = 2;
    let f = Arc::new(build(Setup {
        paid: true,
        facilitator: Some(fac.clone()),
        config: ApiConfig {
            facilitator_timeout: Duration::from_millis(100),
            request_timeout: Duration::from_millis(500),
            max_facilitator_calls: slots,
            payment_attempts_per_minute: 1_000_000,
            unsettled_per_hour: 1_000_000,
            ..ApiConfig::default()
        },
        ..Setup::default()
    }));
    let uri = format!("/v1/proofs/1/{}/1", f.shard);
    let (_, h, _) = get(&f, &uri, &[]).await;
    let max_live = Arc::new(AtomicU64::new(0));
    let sampler = {
        let (f, max_live) = (f.clone(), max_live.clone());
        tokio::spawn(async move {
            loop {
                max_live.fetch_max(f.state.settle_tasks.live() as u64, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    };
    let mut hs = Vec::new();
    for (i, p) in payers.iter().enumerate() {
        let (f, h, p, uri, max_live) = (
            f.clone(),
            h.clone(),
            p.clone(),
            uri.clone(),
            max_live.clone(),
        );
        hs.push(tokio::spawn(async move {
            let mut statuses = Vec::new();
            for nonce in 0..6u8 {
                // The mock's nonces are global: one per payment.
                let sig = [(x402::PAYMENT_SIGNATURE, pay(&h, &p, i as u8 * 6 + nonce))];
                let ip: IpAddr = format!("10.9.{i}.{nonce}").parse().expect("ip");
                let mut s = StatusCode::SERVICE_UNAVAILABLE;
                for _ in 0..10_000 {
                    s = send(&f, Method::GET, &uri, &sig, Some(ip)).await.0;
                    max_live.fetch_max(f.state.settle_tasks.live() as u64, Ordering::SeqCst);
                    if s != StatusCode::SERVICE_UNAVAILABLE {
                        break;
                    }
                    // Retries spread over every millisecond, so some
                    // payer is always about to ask for a slot.
                    tokio::time::sleep(Duration::from_millis(1 + i as u64)).await;
                }
                statuses.push(s);
            }
            statuses
        }));
    }
    let mut ok = 0;
    for h in hs {
        ok += h
            .await
            .expect("task")
            .iter()
            .filter(|s| **s == StatusCode::OK)
            .count();
    }
    sampler.abort();
    assert_eq!(ok, 8 * 6, "every payment settled once it had a slot");
    assert!(
        fac.max_running.load(Ordering::SeqCst) <= slots as u64,
        "settling at once: {}",
        fac.max_running.load(Ordering::SeqCst)
    );
    assert!(
        max_live.load(Ordering::SeqCst) <= slots as u64,
        "settle tasks at once: {}",
        max_live.load(Ordering::SeqCst)
    );
    let budget = f.state.settle_budget();
    assert!(f.state.settle_tasks.wait_idle(budget).await);
}
