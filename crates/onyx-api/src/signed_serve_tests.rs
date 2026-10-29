//! `/v1/signed/{fact}` (ROADMAP C10): level 3 only against `settleable` at a
//! finalized block, every known post landed and final, the serving margin
//! over the sequencer drift, the delayed-post breaker, and the book pin
//! (THREATS C10-*). The chain is a fake node that answers reads only at its
//! finalized block's hash.

use super::*;
use crate::x402::BoxFuture;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use onyx_coordinator::signed::signed_fact_key;
use onyx_coordinator::signed_intake::PostEntry;
use onyx_core::{MetricId, SignedPayload, SignedReport, SourceId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use tower::ServiceExt;

const BOOK: [u8; 20] = [0xB0; 20];
const REVIEWER: [u8; 20] = [0xAE; 20];
const CODE: &[u8] = b"\x60\x80signed-book";
const T0: u64 = 1_800_000_000;
const HOST: u64 = T0 + 20_000;

fn pin() -> BookPin {
    BookPin {
        chain_id: 84_532,
        book: BOOK,
        code_hash: keccak256(CODE),
        reviewer: REVIEWER,
    }
}

fn report(value: u128, sequence: u64, sig: u8) -> SignedReport {
    SignedReport {
        payload: SignedPayload {
            source: SourceId("example.signed".into()),
            key: Hash32([7; 32]),
            metric: MetricId("example.signed.reserve".into()),
            subject: Subject::Entity("entity:example".into()),
            value: OnyxValue::UInt(value),
            observed_unix: T0,
            sequence,
        },
        signature: [sig; 65],
    }
}

fn entry(r: &SignedReport, status: PostStatus) -> PostEntry {
    PostEntry {
        report: r.clone(),
        received_unix: HOST - SERVABLE_AFTER_SECS,
        received_block_time: T0,
        status,
    }
}

fn landed() -> PostStatus {
    PostStatus::Recorded {
        outcome: None,
        block: 10,
        block_time: T0,
    }
}

#[derive(Default)]
struct Chain {
    /// (number, hash, time) of the finalized block; None: no finalized tag.
    finalized: Option<(u64, [u8; 32], u64)>,
    /// fact -> (state, acceptedAt, accepted report hash, value hash).
    facts: BTreeMap<Hash32, (u8, u64, Hash32, Hash32)>,
    recorded: BTreeSet<Hash32>,
    chain_id: u64,
    code: Vec<u8>,
    reviewer: [u8; 20],
    down: bool,
    /// Reads made at anything but the finalized hash.
    stray_reads: u32,
}

struct Node(Mutex<Chain>);

fn w(b: &[u8]) -> Vec<u8> {
    let mut x = vec![0u8; 32 - b.len()];
    x.extend_from_slice(b);
    x
}

impl JsonRpc for Node {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = (|| {
            let mut c = self.0.lock().map_err(|_| "lock")?;
            if c.down {
                return Err("connection refused".to_string());
            }
            Ok(match method {
                "eth_chainId" => json!(format!("0x{:x}", c.chain_id)),
                "eth_getCode" => json!(hx(&c.code)),
                "eth_getBlockByNumber" => {
                    assert_eq!(p[0], "finalized", "only the finalized block is read");
                    match c.finalized {
                        None => Value::Null,
                        Some((n, h, t)) => json!({
                            "number": format!("0x{n:x}"),
                            "hash": hx(&h),
                            "timestamp": format!("0x{t:x}"),
                        }),
                    }
                }
                "eth_call" => {
                    let data = hex_of(&p[0]["data"], "data")?;
                    if data[..4] == selector("reviewer()") {
                        return Ok(json!(hx(&w(&c.reviewer))));
                    }
                    let fin = c.finalized.map(|f| hx(&f.1));
                    if p[1]["blockHash"].as_str().map(str::to_string) != fin {
                        c.stray_reads += 1;
                        return Err("read at a block other than the finalized one".into());
                    }
                    let arg = Hash32(data[4..36].try_into().map_err(|_| "arg")?);
                    let sel = &data[..4];
                    let (_, _, t) = c.finalized.ok_or("no finalized")?;
                    if sel == selector("report(bytes32)") {
                        let mut out = vec![0u8; 224];
                        if c.recorded.contains(&arg) {
                            out[31] = 1;
                        }
                        json!(hx(&out))
                    } else if sel == selector("settleable(bytes32)") {
                        let mut out = vec![0u8; 96];
                        if let Some((1, at, r, v)) = c.facts.get(&arg).copied() {
                            if t >= at + DISPUTE_WINDOW_SECS {
                                out[31] = 1;
                                out[32..64].copy_from_slice(&r.0);
                                out[64..96].copy_from_slice(&v.0);
                            }
                        }
                        json!(hx(&out))
                    } else if sel == selector("factView(bytes32)") {
                        let (state, at) = c.facts.get(&arg).map_or((0, 0), |f| (f.0, f.1));
                        let mut out = w(&[state]);
                        out.extend(vec![0u8; 32]);
                        out.extend(w(&at.to_be_bytes()));
                        out.extend(w(&[0x80]));
                        out.extend(vec![0u8; 32]);
                        json!(hx(&out))
                    } else {
                        return Err("unknown call".into());
                    }
                }
                m => return Err(format!("unexpected {m}")),
            })
        })();
        Box::pin(async move { r })
    }
}

struct Setup {
    node: Arc<Node>,
    store: Arc<SignedStore>,
    x: SignedReport,
    fact: Hash32,
}

/// One fact: report X accepted at T0 and posted, the finalized block at
/// `fin_time`.
fn setup(fin_time: u64, log: Vec<PostEntry>) -> Setup {
    let x = report(1, 1, 1);
    let fact = signed_fact_key(&x.payload);
    let mut c = Chain {
        finalized: Some((500, [0xF1; 32], fin_time)),
        chain_id: 84_532,
        code: CODE.to_vec(),
        reviewer: REVIEWER,
        ..Chain::default()
    };
    c.facts.insert(fact, (1, T0, x.hash(), value_hash(&x)));
    c.recorded.insert(x.payload.digest());
    let node = Arc::new(Node(Mutex::new(c)));
    let mut pl = PostLog::new(84_532, BOOK);
    pl.entries = if log.is_empty() {
        vec![entry(&x, landed())]
    } else {
        log
    };
    let store =
        Arc::new(SignedStore::new(pin(), None, node.clone(), Arc::new(|| HOST)).with_log(pl));
    Setup {
        node,
        store,
        x,
        fact,
    }
}

async fn get_fact(store: &Arc<SignedStore>, fact: &Hash32) -> (StatusCode, Value) {
    let app = routes(store.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/signed/{fact}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).expect("json");
    // PP6: every answer these tests see conforms to docs/api/openapi.json.
    crate::openapi_check::check("GET", &format!("/v1/signed/{fact}"), status.as_u16(), &body);
    (status, body)
}

fn why(b: &Value) -> (Option<&str>, Option<&str>) {
    (b["error"]["code"].as_str(), b["reason"].as_str())
}

#[tokio::test]
async fn a_settleable_fact_past_the_margin_is_served_from_the_finalized_block() {
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["level"], 3);
    assert_eq!(b["reportHash"], s.x.hash().to_string());
    assert_eq!(b["value"], json!({"uint": "1"}));
    assert_eq!(b["checkedAt"]["block"], 500);
    assert_eq!(b["checkedAt"]["blockHash"], hx(&[0xF1; 32]));
    assert_eq!(s.node.0.lock().expect("lock").stray_reads, 0);
    // Not money: the body says so and the route is free (no 402 path).
    assert!(b["moneySettlement"].as_str().expect("text").contains("H6"));
}

#[tokio::test]
async fn c10_5_level_3_is_never_served_without_a_finalized_block() {
    // A node with no `finalized` tag (or a dead one): refused, never `latest`.
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    s.node.0.lock().expect("lock").finalized = None;
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(why(&b).0, Some("level3_chain_unavailable"));
    s.node.0.lock().expect("lock").down = true;
    let (st, _) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn c10_5_settleable_at_latest_is_not_enough() {
    // The window passed at the head, but the finalized block is older: at
    // the finalized block the fact is still in its window.
    let s = setup(T0 + DISPUTE_WINDOW_SECS - 1, vec![]);
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(
        why(&b),
        (Some("level3_not_servable"), Some(reason::IN_WINDOW))
    );
    assert_eq!(b["servableAt"], T0 + SERVABLE_AFTER_SECS);
}

#[tokio::test]
async fn l3c_12_the_serving_margin_covers_the_sequencer_drift() {
    // settleable is true from T0 + 3600 of block time; a sequencer running
    // block time 1800 s ahead of L1 makes that 30 minutes of real time. The
    // API waits the drift on top.
    for (t, ok) in [
        (T0 + DISPUTE_WINDOW_SECS, false),
        (T0 + SERVABLE_AFTER_SECS - 1, false),
        (T0 + SERVABLE_AFTER_SECS, true),
    ] {
        let s = setup(t, vec![]);
        let (st, b) = get_fact(&s.store, &s.fact).await;
        if ok {
            assert_eq!(st, StatusCode::OK, "{b}");
        } else {
            assert_eq!(why(&b).1, Some(reason::IN_MARGIN), "at {t}: {b}");
        }
    }
    assert_eq!(SERVABLE_AFTER_SECS, 5_400);
}

#[tokio::test]
async fn disputed_unknown_and_other_reports_are_refused() {
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    s.node
        .0
        .lock()
        .expect("lock")
        .facts
        .insert(s.fact, (2, 0, Hash32::ZERO, Hash32::ZERO));
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::DISPUTED));
    s.node.0.lock().expect("lock").facts.remove(&s.fact);
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::UNKNOWN));
    // The book settles on another signature over the same payload.
    let other = report(1, 1, 2);
    s.node
        .0
        .lock()
        .expect("lock")
        .facts
        .insert(s.fact, (1, T0, other.hash(), value_hash(&other)));
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::OTHER_REPORT));
    // A fact this server holds nothing for.
    let (st, b) = get_fact(&s.store, &Hash32([3; 32])).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(why(&b).0, Some("no_signed_report"));
}

#[tokio::test]
async fn c10_3_a_delayed_contradiction_holds_the_fact() {
    // The coordinator took Y (a contradiction) and its post has not landed:
    // the chain still says X is settleable, but the fact is not served
    // (L3C-2: a held-back post extends the window until it lands).
    let x = report(1, 1, 1);
    let y = SignedReport {
        payload: SignedPayload {
            key: Hash32([8; 32]),
            value: OnyxValue::UInt(2),
            ..x.payload.clone()
        },
        signature: [9; 65],
    };
    assert_eq!(signed_fact_key(&x.payload), signed_fact_key(&y.payload));
    let pending = PostStatus::Pending {
        since: HOST - 30,
        error: "not mined".into(),
    };
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![entry(&x, landed()), entry(&y, pending)],
    );
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::POST_PENDING));
    // It landed (recorded in the log) but not yet in the finalized state.
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![entry(&x, landed()), entry(&y, landed())],
    );
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::POST_NOT_FINAL));
    // Final, and the book disputed the fact: refused for good.
    {
        let mut c = s.node.0.lock().expect("lock");
        c.recorded.insert(y.payload.digest());
        c.facts.insert(s.fact, (2, 0, Hash32::ZERO, Hash32::ZERO));
    }
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::DISPUTED));
    // A refused post (the book never recorded it) holds nothing.
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![
            entry(&x, landed()),
            entry(
                &y,
                PostStatus::Refused {
                    error: "KeySuspended".into(),
                },
            ),
        ],
    );
    let (st, _) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn c10_3_a_post_pending_too_long_stops_level_3() {
    let x = report(1, 1, 1);
    let other = report(5, 2, 1);
    let late = PostStatus::Pending {
        since: HOST - POST_DELAY_ALERT_SECS - 1,
        error: "not mined".into(),
    };
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![entry(&x, landed()), entry(&other, late)],
    );
    // Another fact's post is stuck: every level-3 answer stops.
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(why(&b).0, Some("level3_posts_delayed"));
}

#[tokio::test]
async fn c10_2_a_report_never_posted_is_never_served() {
    // A coordinator that skips posts: its log says recorded, the book never
    // saw the payload. The finalized state decides.
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    {
        let mut c = s.node.0.lock().expect("lock");
        c.recorded.clear();
        c.facts.clear();
    }
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::POST_NOT_FINAL));
}

#[tokio::test]
async fn the_log_is_bound_to_the_pinned_book_and_fails_closed() {
    let node = Arc::new(Node(Mutex::new(Chain::default())));
    let other = PostLog::new(84_532, [0xCC; 20]);
    let store =
        Arc::new(SignedStore::new(pin(), None, node.clone(), Arc::new(|| HOST)).with_log(other));
    let (st, b) = get_fact(&store, &Hash32([1; 32])).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(why(&b).0, Some("level3_store_unreadable"));
    // A file: re-read when it changes; garbage refuses everything.
    let dir = std::env::temp_dir().join(format!("onyx-signed-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("posts.json");
    let mut log = PostLog::new(84_532, BOOK);
    log.entries.push(entry(&report(1, 1, 1), landed()));
    log.write(&path).expect("write");
    let store = SignedStore::new(pin(), Some(path.clone()), node, Arc::new(|| HOST));
    assert_eq!(store.log().expect("log").entries.len(), 1);
    std::fs::write(&path, b"{not json").expect("write");
    assert!(store.log().is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn only_canonical_facts_and_no_query() {
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    let app = routes(s.store.clone());
    for uri in [
        format!(
            "/v1/signed/{}",
            s.fact.to_string().to_uppercase().replace("0X", "0x")
        ),
        format!("/v1/signed/{}?x=1", s.fact),
        "/v1/signed/0x12".to_string(),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .body(Body::empty())
                    .expect("req"),
            )
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uri}");
    }
    // The free budget per client.
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![]);
    let store = Arc::new(
        SignedStore::new(pin(), None, s.node.clone(), Arc::new(|| HOST))
            .with_free_per_minute(1)
            .with_log({
                let mut l = PostLog::new(84_532, BOOK);
                l.entries.push(entry(&s.x, landed()));
                l
            }),
    );
    assert_eq!(get_fact(&store, &s.fact).await.0, StatusCode::OK);
    assert_eq!(
        get_fact(&store, &s.fact).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_book_pin_checks_chain_code_and_reviewer() {
    let mk = |f: &dyn Fn(&mut Chain)| {
        let mut c = Chain {
            chain_id: 84_532,
            code: CODE.to_vec(),
            reviewer: REVIEWER,
            ..Chain::default()
        };
        f(&mut c);
        Node(Mutex::new(c))
    };
    assert_eq!(check_book(&mk(&|_| {}), &pin()).await, Ok(()));
    let e = check_book(&mk(&|c| c.chain_id = 1), &pin()).await;
    assert!(e.expect_err("chain").contains("chain"));
    let e = check_book(&mk(&|c| c.code = b"other".to_vec()), &pin()).await;
    assert!(e.expect_err("code").contains("signedBookCodeHash"));
    let e = check_book(&mk(&|c| c.code = Vec::new()), &pin()).await;
    assert!(e.is_err(), "no code at the address");
    let e = check_book(&mk(&|c| c.reviewer = [1; 20]), &pin()).await;
    assert!(e.expect_err("reviewer").contains("signedBookReviewer"));
}

#[test]
fn only_the_book_build_is_pinned() {
    let artifact = json!({"deployedBytecode": {
        "object": "0x6080aaaaaaaa6040",
        "immutableReferences": {"9": [{"start": 2, "length": 4}]},
    }});
    // The reviewer immutable filled in: the same build.
    assert_eq!(
        code_matches_build(&hex::decode("6080beefbeef6040").expect("hex"), &artifact),
        Ok(())
    );
    // A byte outside the immutable, another length, or no code: refused.
    assert!(code_matches_build(&hex::decode("6081beefbeef6040").expect("hex"), &artifact).is_err());
    assert!(code_matches_build(&hex::decode("6080beefbeef60").expect("hex"), &artifact).is_err());
    assert!(code_matches_build(&[], &artifact).is_err());
    let bad = json!({"deployedBytecode": {
        "object": "0x6080aaaaaaaa6040",
        "immutableReferences": {"9": [{"start": 6, "length": 4}]},
    }});
    assert!(code_matches_build(&hex::decode("6080aaaaaaaa6040").expect("hex"), &bad).is_err());
    let b = json!({"transactions": [
        {"contractName": "OnyxSignedBook", "transactionType": "CREATE",
         "contractAddress": format!("0x{}", "ab".repeat(20))},
        {"contractName": "OnyxSignedBook", "transactionType": "CALL",
         "contractAddress": null},
    ]});
    assert_eq!(book_from_broadcast(&b), Ok([0xab; 20]));
    let two = json!({"transactions": [b["transactions"][0], b["transactions"][0]]});
    assert!(book_from_broadcast(&two).is_err());
    assert!(book_from_broadcast(&json!({"transactions": []})).is_err());
}

#[tokio::test]
async fn c10_r1_2_the_margin_also_runs_on_the_host_clock() {
    // Block time 5,400 s past acceptance, but this operator took the report
    // only 30 minutes ago by its own clock (a sequencer catching up after a
    // stall, or a node lying about the finalized time).
    let x = report(1, 1, 1);
    let mut e = entry(&x, landed());
    e.received_unix = HOST - 1_800;
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![e]);
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::IN_HOST_MARGIN), "{b}");
    assert_eq!(b["servableAtHostUnix"], HOST - 1_800 + SERVABLE_AFTER_SECS);
}

#[tokio::test]
async fn c10_r1_1_a_contradiction_refused_for_its_time_holds_the_fact() {
    let x = report(1, 1, 1);
    let y = SignedReport {
        payload: SignedPayload {
            key: Hash32([8; 32]),
            value: OnyxValue::UInt(2),
            ..x.payload.clone()
        },
        signature: [9; 65],
    };
    let refused = |error: &str| PostStatus::Refused {
        error: error.into(),
    };
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![entry(&x, landed()), entry(&y, refused("FromFuture"))],
    );
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).1, Some(reason::POST_RETRYING));
    // A forged or final refusal holds nothing (no griefing by junk).
    for e in ["BadSignature", "Stale", "UnknownKey"] {
        let s = setup(
            T0 + SERVABLE_AFTER_SECS,
            vec![entry(&x, landed()), entry(&y, refused(e))],
        );
        assert_eq!(get_fact(&s.store, &s.fact).await.0, StatusCode::OK, "{e}");
    }
}

#[tokio::test]
async fn c10_r2_2_a_host_clock_behind_the_coordinators_stops_level_3() {
    // The coordinator took a report "now" by its clock; this host's clock
    // reads 301 s earlier. With the clock behind, a post pending 600 s by
    // the coordinator's clock would not trip the breaker yet: refused.
    let x = report(1, 1, 1);
    // Another fact (another observation time), recorded and not pending.
    let mut other = report(5, 2, 1);
    other.payload.observed_unix = T0 + 1;
    let mut late = entry(&other, landed());
    late.received_unix = HOST + HOST_SKEW_SECS + 1;
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![entry(&x, landed()), late]);
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{b}");
    assert_eq!(why(&b).0, Some("level3_clock_skew"));
    // A pending post's time counts too.
    let mut pend = entry(
        &other,
        PostStatus::Pending {
            since: HOST + HOST_SKEW_SECS + 1,
            error: "not mined".into(),
        },
    );
    pend.received_unix = HOST - SERVABLE_AFTER_SECS;
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![entry(&x, landed()), pend]);
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).0, Some("level3_clock_skew"));
    // Within the skew: served.
    let mut near = entry(&other, landed());
    near.received_unix = HOST + HOST_SKEW_SECS;
    let s = setup(T0 + SERVABLE_AFTER_SECS, vec![entry(&x, landed()), near]);
    let (st, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(st, StatusCode::OK, "{b}");
}

fn pending_since(r: &SignedReport, since: u64) -> PostEntry {
    let mut e = entry(
        r,
        PostStatus::Pending {
            since,
            error: "not mined".into(),
        },
    );
    e.received_unix = since;
    e
}

#[tokio::test]
async fn c62_health_counts_pending_and_refused_posts() {
    // One landed, two pending (the oldest 120 s), one refused for good, two
    // refused for a reason that may clear (retried, C10-R1-1).
    let x = report(1, 1, 1);
    let mk = |v: u128, seq: u64| {
        let mut r = report(v, seq, 1);
        r.payload.observed_unix = T0 + seq;
        r
    };
    let refused = |r: &SignedReport, e: &str| {
        entry(
            r,
            PostStatus::Refused {
                error: e.to_string(),
            },
        )
    };
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![
            entry(&x, landed()),
            pending_since(&mk(2, 2), HOST - 120),
            pending_since(&mk(3, 3), HOST - 30),
            refused(&mk(4, 4), "Replay"),
            refused(&mk(5, 5), "FromFuture"),
            refused(&mk(6, 6), "KeySuspended"),
        ],
    );
    let h = crate::Level3Health::level3_health(s.store.as_ref());
    assert_eq!(h["logReadable"], true);
    assert_eq!(h["posts"], 6);
    assert_eq!(h["postsRecorded"], 1);
    assert_eq!(h["postsPending"], 2);
    assert_eq!(h["postsRefused"], 3);
    assert_eq!(h["postsRefusedRetrying"], 2);
    assert_eq!(h["postsRefusedFinal"], 1);
    assert_eq!(h["oldestPendingSince"], HOST - 120);
    assert_eq!(h["oldestPendingSecs"], 120);
    assert_eq!(h["breakerSecs"], POST_DELAY_ALERT_SECS);
    assert_eq!(h["now"], HOST);
    assert!(h["stopped"].is_null(), "{h}");
    // An empty log: zero counts, nothing pending.
    let empty = SignedStore::new(pin(), None, s.node.clone(), Arc::new(|| HOST));
    let h = crate::Level3Health::level3_health(&empty);
    assert_eq!(
        (h["posts"].clone(), h["postsPending"].clone()),
        (json!(0), json!(0))
    );
    assert!(
        h["oldestPendingSecs"].is_null() && h["stopped"].is_null(),
        "{h}"
    );
}

#[tokio::test]
async fn c62_health_names_the_breaker_stop_at_its_edge() {
    // THREATS C10-3: the breaker trips when a post is pending more than
    // 600 s. At 600 s health says not stopped and the route still answers;
    // at 601 s both say `level3_posts_delayed`.
    let x = report(1, 1, 1);
    let mut y = report(2, 2, 1);
    y.payload.observed_unix = T0 + 2;
    for (age, stop) in [
        (POST_DELAY_ALERT_SECS, None),
        (POST_DELAY_ALERT_SECS + 1, Some("level3_posts_delayed")),
    ] {
        let s = setup(
            T0 + SERVABLE_AFTER_SECS,
            vec![entry(&x, landed()), pending_since(&y, HOST - age)],
        );
        let h = crate::Level3Health::level3_health(s.store.as_ref());
        assert_eq!(h["oldestPendingSecs"], age);
        assert_eq!(h["stopped"].as_str(), stop, "{h}");
        let (st, b) = get_fact(&s.store, &s.fact).await;
        match stop {
            None => assert_eq!(st, StatusCode::OK, "{b}"),
            Some(code) => {
                assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(why(&b).0, Some(code));
            }
        }
    }
}

#[tokio::test]
async fn c62_health_names_the_clock_skew_stop_and_an_unreadable_log() {
    // A pending time ahead of this clock reads 0 s, and past the skew the
    // stop is `level3_clock_skew`, as the route says.
    let x = report(1, 1, 1);
    let mut y = report(2, 2, 1);
    y.payload.observed_unix = T0 + 2;
    let s = setup(
        T0 + SERVABLE_AFTER_SECS,
        vec![
            entry(&x, landed()),
            pending_since(&y, HOST + HOST_SKEW_SECS + 1),
        ],
    );
    let h = crate::Level3Health::level3_health(s.store.as_ref());
    assert_eq!(h["oldestPendingSecs"], 0);
    assert_eq!(h["stopped"], "level3_clock_skew");
    let (_, b) = get_fact(&s.store, &s.fact).await;
    assert_eq!(why(&b).0, Some("level3_clock_skew"));
    // A log file that does not parse: no counts, stopped, readable false.
    let dir = std::env::temp_dir().join(format!("onyx-c62-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("posts.json");
    std::fs::write(&path, b"{not json").expect("write");
    let bad = SignedStore::new(pin(), Some(path), s.node.clone(), Arc::new(|| HOST));
    let h = crate::Level3Health::level3_health(&bad);
    assert_eq!(h["logReadable"], false);
    assert_eq!(h["stopped"], "level3_store_unreadable");
    assert!(
        h["postsPending"].is_null() && h["postsRefused"].is_null(),
        "{h}"
    );
    assert!(h["postsRefusedFinal"].is_null(), "{h}");
    assert!(h["logError"].as_str().is_some_and(|e| !e.is_empty()));
    std::fs::remove_dir_all(&dir).ok();
}
