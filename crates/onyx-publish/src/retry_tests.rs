//! Tests of what `anchor` and `catalog-anchor` do when they are run again
//! (THREATS EARLY-21 and RETRY-R1-*; scripts/early-record-daily.sh and the
//! unit behind it run them again): a recorded report is never sent twice, a
//! transaction of this report that stays unmined is replaced at its own
//! nonce with a higher fee and in the end refused, a transaction the tool
//! did not send is never replaced, a second transaction never makes a
//! second report, and every stop is told apart as transient (exit 75, a
//! `TRANSIENT class=` last line) or refused (exit 1). The chain is a fake in
//! this process: no node, no key file, no real transaction. Its pool behaves
//! as a node's does: the same bytes again are "already known", a
//! replacement under 10% more is "replacement transaction underpriced", a
//! used nonce is "nonce too low". Review RETRY-R2's tests are in
//! `retry_r2_tests.rs`.

use super::*;
use crate::anchor::{post_report, AnchorOpts};
use onyx_api::x402::BoxFuture;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// What the fake chain does with a transaction it takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pool {
    /// Mined at once, and the report recorded.
    Mines,
    /// Kept in the pool (the `pending` nonce counts it), never mined by
    /// itself (see `mines_at`).
    Keeps,
    /// Kept, but the node does not show its pool (`pending` = `latest`).
    Hides,
    /// Mined and reverted (`Replay`): an earlier transaction recorded the
    /// report first.
    RevertsAsReplay,
    /// Mined and reverted, and the book holds no record.
    Reverts,
}

/// A transaction in the fake's pool.
#[derive(Clone, Debug)]
struct Pooled {
    nonce: u64,
    tip: u128,
    max_fee: u128,
    raw: String,
    hash: String,
}

#[derive(Default)]
struct State {
    /// The book holds the report.
    recorded: bool,
    /// How many times the book recorded it (never more than once).
    records: u32,
    /// Transactions mined from the gas key.
    latest: u64,
    /// This tool's unmined transactions, one a nonce.
    pool: Vec<Pooled>,
    /// Unmined transactions of the key that this tool did not send: they
    /// hold the nonces from `latest` on.
    foreign: u64,
    /// A send at a nonce a foreign transaction holds (must never happen).
    replaced_foreign: bool,
    /// Receipts by hash: mined well, or reverted.
    receipts: BTreeMap<String, bool>,
    /// The raw transactions the node took, in order.
    sent: Vec<String>,
    /// What the node answered to the sends it refused.
    refused: Vec<String>,
    /// Every method called, in order.
    calls: Vec<String>,
}

#[derive(Clone)]
struct Chain {
    pool: Pool,
    /// The digest `sequenceOf` answers (another payload under the sequence).
    other_digest: Option<[u8; 32]>,
    /// An error every call gets (the node is down).
    down: Option<&'static str>,
    /// The error `eth_estimateGas` gets.
    estimate: Option<&'static str>,
    /// `report(bytes32)` is answered by a backend behind the head: it does
    /// not show the record.
    lags: bool,
    /// A pooled transaction offering this much per gas mines at once.
    mines_at: Option<u128>,
    /// What a replacement must add to both fees, in percent (nodes: 10).
    min_bump_pct: u128,
    /// The node does not know a pooled transaction by its hash (a public
    /// node behind a load balancer, as on 2026-11-18).
    blind: bool,
    state: Arc<Mutex<State>>,
}

/// What a transaction of the key that this tool did not send offers a gas.
const FOREIGN_FEE: u128 = 1_000;

const REVERT: &str = r#"{"code":3,"message":"execution reverted"}"#;

/// One RLP item at the start of `b`: its payload and the rest.
fn rlp_item(b: &[u8]) -> (&[u8], &[u8]) {
    let len_of = |n: usize| {
        b[1..1 + n]
            .iter()
            .fold(0usize, |a, x| (a << 8) | usize::from(*x))
    };
    match b[0] {
        0..=0x7f => (&b[..1], &b[1..]),
        h @ 0x80..=0xb7 => b[1..].split_at(usize::from(h - 0x80)),
        h @ 0xb8..=0xbf => {
            let n = usize::from(h - 0xb7);
            b[1 + n..].split_at(len_of(n))
        }
        h @ 0xc0..=0xf7 => b[1..].split_at(usize::from(h - 0xc0)),
        h => {
            let n = usize::from(h - 0xf7);
            b[1 + n..].split_at(len_of(n))
        }
    }
}

/// `0x02 || rlp([chain, nonce, tip, max fee, ...])`: the nonce and the fees.
fn decode(raw: &str) -> (u64, u128, u128) {
    let b = hex::decode(raw.trim_start_matches("0x")).expect("hex");
    assert_eq!(b[0], 0x02, "a type-2 transaction");
    let (list, _) = rlp_item(&b[1..]);
    let num = |b: &[u8]| b.iter().fold(0u128, |a, x| (a << 8) | u128::from(*x));
    let (_chain, rest) = rlp_item(list);
    let (nonce, rest) = rlp_item(rest);
    let (tip, rest) = rlp_item(rest);
    let (max_fee, _) = rlp_item(rest);
    (
        u64::try_from(num(nonce)).expect("nonce"),
        num(tip),
        num(max_fee),
    )
}

impl Chain {
    fn new(pool: Pool) -> Chain {
        Chain {
            pool,
            other_digest: None,
            down: None,
            estimate: None,
            lags: false,
            mines_at: None,
            min_bump_pct: 10,
            blind: false,
            state: Arc::default(),
        }
    }

    fn sent(&self) -> Vec<String> {
        self.state.lock().expect("lock").sent.clone()
    }

    fn refused(&self) -> Vec<String> {
        self.state.lock().expect("lock").refused.clone()
    }

    fn records(&self) -> u32 {
        self.state.lock().expect("lock").records
    }

    /// Every pooled transaction of this tool is mined; the first records
    /// the report, a later one reverts.
    fn mine_pool(&self) {
        let mut s = self.state.lock().expect("lock");
        let mut pool = std::mem::take(&mut s.pool);
        pool.sort_by_key(|p| p.nonce);
        for p in pool {
            assert_eq!(p.nonce, s.latest, "mined in nonce order");
            s.latest += 1;
            let first = !s.recorded;
            if first {
                s.recorded = true;
                s.records += 1;
            }
            s.receipts.insert(p.hash, first);
        }
    }

    /// The node's answer to a raw transaction.
    fn take(&self, s: &mut State, raw: &str) -> Result<Value, String> {
        let (nonce, tip, max_fee) = decode(raw);
        let hash = format!(
            "0x{}",
            hex::encode(keccak256(&hex::decode(&raw[2..]).expect("hex")).0)
        );
        let refuse = |s: &mut State, why: &str| {
            let e = format!(
                r#"eth_sendRawTransaction: node error: {{"code":-32000,"message":"{why}"}}"#
            );
            s.refused.push(e.clone());
            Err(e)
        };
        if nonce < s.latest {
            return refuse(s, "nonce too low");
        }
        if nonce < s.latest + s.foreign {
            // Another's transaction holds this nonce (a transfer, a cancel
            // made by hand, at `FOREIGN_FEE` a gas).
            if max_fee * 100 < FOREIGN_FEE * 110 {
                return refuse(s, "replacement transaction underpriced");
            }
            s.replaced_foreign = true;
        }
        if let Some(i) = s.pool.iter().position(|p| p.nonce == nonce) {
            let old = s.pool[i].clone();
            if old.raw == raw {
                return refuse(s, "already known");
            }
            let enough = |new: u128, old: u128| new * 100 >= old * (100 + self.min_bump_pct);
            if !enough(max_fee, old.max_fee) || !enough(tip, old.tip) {
                return refuse(s, "replacement transaction underpriced");
            }
            s.pool.remove(i);
        }
        s.sent.push(raw.to_string());
        let mined = match self.pool {
            Pool::Mines => Some(true),
            Pool::RevertsAsReplay | Pool::Reverts => Some(false),
            Pool::Keeps | Pool::Hides => self.mines_at.filter(|at| max_fee >= *at).map(|_| true),
        };
        match mined {
            Some(ok) => {
                s.latest = nonce + 1;
                if ok || self.pool == Pool::RevertsAsReplay {
                    // Reverted as a replay: an earlier one recorded it.
                    if !s.recorded {
                        s.records += 1;
                    }
                    s.recorded = true;
                }
                s.receipts.insert(hash.clone(), ok);
            }
            None => s.pool.push(Pooled {
                nonce,
                tip,
                max_fee,
                raw: raw.to_string(),
                hash: hash.clone(),
            }),
        }
        Ok(json!(hash))
    }
}

impl JsonRpc for Chain {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let mut s = self.state.lock().expect("lock");
        s.calls.push(method.to_string());
        let hex = |b: &[u8]| json!(format!("0x{}", hex::encode(b)));
        let sel = |sig: &str| hex::encode(&keccak256(sig.as_bytes()).0[..4]);
        let replay = || {
            format!(
                r#"{method}: node error: {{"code":3,"message":"execution reverted","data":"0x{}"}}"#,
                sel("Replay()")
            )
        };
        let r = if let Some(e) = self.down {
            Err(format!("{method}: {e}"))
        } else {
            match method {
                "eth_call" => {
                    let data = &params[0]["data"].as_str().unwrap_or_default()[2..];
                    if data.starts_with(&sel("report(bytes32)")) {
                        Ok(hex(&[u8::from(s.recorded && !self.lags); 64]))
                    } else if data.starts_with(&sel("postReport(bytes,bytes)")) {
                        match (s.recorded, self.estimate) {
                            (true, _) => Err(replay()),
                            (false, Some(e)) => Err(format!("{method}: node error: {e}")),
                            (false, None) => Ok(json!("0x")),
                        }
                    } else {
                        Ok(hex(&self.other_digest.unwrap_or([0; 32])))
                    }
                }
                "eth_estimateGas" => match (self.estimate, s.recorded) {
                    (Some(e), _) => Err(format!("{method}: node error: {e}")),
                    (None, true) => Err(replay()),
                    (None, false) => Ok(json!("0x30d40")),
                },
                "eth_getTransactionCount" => {
                    let pooled = if self.pool == Pool::Hides {
                        0
                    } else {
                        s.foreign + s.pool.len() as u64
                    };
                    let n = s.latest + if params[1] == "pending" { pooled } else { 0 };
                    Ok(json!(format!("0x{n:x}")))
                }
                "eth_getBlockByNumber" => Ok(json!({"baseFeePerGas": "0x64"})),
                "eth_maxPriorityFeePerGas" => Ok(json!("0x1")),
                "eth_sendRawTransaction" => {
                    let raw = params[0].as_str().unwrap_or_default().to_string();
                    self.take(&mut s, &raw)
                }
                "eth_getTransactionReceipt" => Ok(
                    match s.receipts.get(params[0].as_str().unwrap_or_default()) {
                        None => Value::Null,
                        Some(ok) => json!({
                            "blockNumber": "0x5", "status": if *ok { "0x1" } else { "0x0" },
                            "gasUsed": "0x5208", "effectiveGasPrice": "0x65"
                        }),
                    },
                ),
                "eth_getTransactionByHash" => {
                    let h = params[0].as_str().unwrap_or_default();
                    let pooled = !self.blind && s.pool.iter().any(|p| p.hash == h);
                    Ok(if s.receipts.contains_key(h) {
                        json!({"hash": h, "blockNumber": "0x5"})
                    } else if pooled {
                        json!({"hash": h, "blockNumber": null})
                    } else {
                        Value::Null
                    })
                }
                m => Err(format!("{m}: unexpected")),
            }
        };
        Box::pin(async move { r })
    }
}

/// A fresh folder of this process and test.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("onyx-publish-retry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// A signed report (of a made-up version; the fake book checks nothing in
/// it).
fn report() -> SignedReport {
    let dir = scratch("report");
    let p = |n: &str| dir.join(n).display().to_string();
    std::fs::write(
        dir.join("s.json"),
        json!({"columns":[{"name":"t","kind":"int"}],"key":"t"}).to_string(),
    )
    .expect("w");
    std::fs::write(dir.join("a.csv"), "t\n1\n").expect("w");
    let a: Vec<String> = [
        "--schema",
        &p("s.json"),
        "--input",
        &p("a.csv"),
        "--dataset",
        "synthetic.test",
        "--publisher",
        "example.publisher",
        "--license",
        "CC0-1.0",
        "--out",
        &p("v1"),
        "--created",
        "10",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    build_cmd(&parse_opts(&a).expect("opts")).expect("built");
    let m = read_manifest(&dir.join("v1")).expect("manifest");
    let r = onyx_bulk::anchor::sign(&[0x22; 32], &m.header).expect("signs");
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// The report, signed once for every test (a build writes a folder).
fn the_report() -> &'static SignedReport {
    static R: std::sync::OnceLock<SignedReport> = std::sync::OnceLock::new();
    R.get_or_init(report)
}

/// The bytes every transaction of the report must carry.
fn report_data() -> String {
    hex::encode(post_report_calldata(
        &borsh::to_vec(&the_report().payload).expect("borsh"),
        &the_report().signature,
    ))
}

const GAS_KEY: [u8; 32] = [0x33; 32];
const STUCK: u64 = 1800;
const T0: u64 = 1_800_000_000;

/// The deadline is the bound of a test that waits for a transaction the fake
/// never mines: it is the configured deadline itself, never a guess at the
/// machine's speed.
fn opts(record: Option<&Path>, now_unix: u64) -> AnchorOpts {
    AnchorOpts {
        deadline: Duration::from_millis(60),
        poll: Duration::from_millis(1),
        stuck: Duration::from_secs(STUCK),
        max_bumps: 4,
        max_fee_wei: 10_000_000_000,
        record: record.map(Path::to_path_buf),
        now_unix,
    }
}

async fn post_with<T: JsonRpc>(rpc: T, o: &AnchorOpts) -> Result<(), Stop> {
    post_report(
        rpc,
        84_532,
        &[0xb0; 20],
        GAS_KEY,
        the_report(),
        "another payload under this sequence",
        o,
    )
    .await
}

/// A run at `now` that keeps its record in `record`.
async fn run(chain: &Chain, record: &Path, now: u64) -> Result<(), Stop> {
    post_with(chain.clone(), &opts(Some(record), now)).await
}

/// A run that keeps no record (as a tool built before the record did).
async fn post(chain: &Chain) -> Result<(), Stop> {
    post_with(chain.clone(), &opts(None, T0)).await
}

/// The live stop (2026-11-18): the transaction is not mined before the
/// deadline. The run after it sends nothing new while the first is still in
/// the pool (it only hands the same bytes to the node again), and nothing
/// once it mined: one transaction in all.
#[tokio::test]
async fn a_report_not_mined_in_time_is_never_sent_twice() {
    let dir = scratch("not-mined");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    let e = run(&chain, &rec, T0).await.expect_err("not mined");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    assert!(
        e.msg().starts_with("postReport: transaction 0x")
            && e.msg().ends_with("not mined before the deadline"),
        "{e:?}"
    );
    assert_eq!(chain.sent().len(), 1);
    // Run again while the first is still unmined and young: waited for.
    for later in [28, 300, STUCK - 1] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("pending");
        assert_eq!(e.class(), Some(classes::NOT_MINED), "{later}: {e:?}");
        assert_eq!(chain.sent().len(), 1, "{later}: nothing new is sent");
    }
    // The same bytes again are what the node of 2026-11-18 answered.
    assert!(chain.refused().iter().all(|e| e.contains("already known")));
    // It mined late: the next run reads the record and sends nothing.
    chain.mine_pool();
    run(&chain, &rec, T0 + 400).await.expect("already anchored");
    assert_eq!(chain.sent().len(), 1, "a recorded report is not sent twice");
    assert_eq!(chain.records(), 1);
    let last = chain.state.lock().expect("lock").calls.last().cloned();
    assert_eq!(last.as_deref(), Some("eth_call"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R1-H1: this report's own transaction stays unmined (a fee under
/// the base fee). Once it is older than `--stuck-secs` the SAME report goes
/// out again at the SAME nonce with both fees at least 12.5% higher, at
/// most `--max-bumps` times; then the run refuses, with the line a person
/// needs, and never calls it transient again.
#[tokio::test]
async fn h1_a_stuck_transaction_is_replaced_at_its_nonce_and_in_the_end_refused() {
    let dir = scratch("stuck");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    let mut now = T0;
    for bumps in 1..=4usize {
        // One second before the bound: still only waited for.
        let e = run(&chain, &rec, now + STUCK - 1).await.expect_err("young");
        assert_eq!(e.class(), Some(classes::NOT_MINED));
        assert_eq!(chain.sent().len(), bumps);
        now += STUCK;
        let e = run(&chain, &rec, now).await.expect_err("replaced");
        assert_eq!(e.class(), Some(classes::NOT_MINED), "bump {bumps}: {e:?}");
        let sent = chain.sent();
        assert_eq!(sent.len(), bumps + 1, "one replacement a bound");
        let (old, new) = (decode(&sent[bumps - 1]), decode(&sent[bumps]));
        assert_eq!((new.0, old.0), (0, 0), "the same nonce");
        assert!(new.1 * 8 >= old.1 * 9, "tip {} -> {}", old.1, new.1);
        assert!(new.2 * 8 >= old.2 * 9, "fee {} -> {}", old.2, new.2);
        assert!(sent[bumps].contains(&report_data()), "the same report");
        // One transaction of the key in the pool, whatever was replaced.
        assert_eq!(chain.state.lock().expect("lock").pool.len(), 1);
    }
    assert!(chain.refused().iter().all(|e| e.contains("already known")));
    // The fifth bound: no fifth replacement, a refusal, run after run.
    for run_no in 0..20 {
        now += STUCK;
        let e = run(&chain, &rec, now).await.expect_err("stuck for good");
        assert_eq!(e.class(), None, "run {run_no}: a refusal, not transient");
        assert!(
            e.msg().contains("nonce 0")
                && e.msg().contains("raised 4 times")
                && e.msg().contains("A stuck transaction"),
            "{e:?}"
        );
        assert_eq!(chain.sent().len(), 5, "no sixth version");
        let out = stop_lines("catalog-anchor", &e);
        assert!(!out.contains("run the same command again"), "{out}");
        assert_eq!(stop_exit(&e), 1);
    }
    // The record: every version, and never the key.
    let text = std::fs::read_to_string(&rec).expect("record");
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["nonce"], json!(0));
    let versions = v["txs"].as_array().expect("txs");
    assert_eq!(versions.len(), 5);
    assert!(versions.iter().all(|t| t["sent"] == json!(true)));
    assert!(!text.contains(&hex::encode(GAS_KEY)), "the key is not kept");
    // The newest version mines after all: done, one record in the book.
    chain.mine_pool();
    run(&chain, &rec, now + 1).await.expect("anchored");
    assert_eq!(chain.records(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R1-H1: the replacement is what gets a stuck report anchored: it
/// offers what the chain asks, mines, and the book holds one record.
#[tokio::test]
async fn h1_a_replacement_with_a_higher_fee_mines_and_records_the_report_once() {
    let dir = scratch("replace-mines");
    let rec = dir.join("tx.json");
    let mut chain = Chain::new(Pool::Keeps);
    // The first offers 2 * 100 + 1 = 201 a gas; the chain wants 220.
    chain.mines_at = Some(220);
    run(&chain, &rec, T0).await.expect_err("under the base fee");
    run(&chain, &rec, T0 + STUCK)
        .await
        .expect("replaced and mined");
    {
        let s = chain.state.lock().expect("lock");
        assert_eq!(s.sent.len(), 2);
        assert_eq!(decode(&s.sent[1]).0, 0, "the same nonce");
        assert!(decode(&s.sent[1]).2 >= 220);
        assert_eq!((s.records, s.latest), (1, 1), "one record, one nonce");
        assert_eq!(s.receipts.values().filter(|ok| **ok).count(), 1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R1-H1: the fee cap bounds what a replacement may offer (a fee
/// spike cannot drain the gas key): above it nothing is sent, the stop is
/// `fee_cap`, and the tries are counted, so it ends as a refusal too.
#[tokio::test]
async fn h1_a_replacement_never_offers_more_than_the_fee_cap() {
    let dir = scratch("cap");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    let capped = |now| {
        let mut o = opts(Some(&rec), now);
        // The first send offers 201; one raise (227) fits, a second (256) not.
        o.max_fee_wei = 230;
        o
    };
    post_with(chain.clone(), &capped(T0))
        .await
        .expect_err("sent");
    post_with(chain.clone(), &capped(T0 + STUCK))
        .await
        .expect_err("replaced once");
    assert_eq!(chain.sent().len(), 2);
    assert!(chain.sent().iter().all(|raw| decode(raw).2 <= 230));
    // A fee spike passes: the stop stays `fee_cap` however often it is
    // met, and uses up no raise (RETRY-R2-M1).
    let mut now = T0 + STUCK;
    for _ in 0..8 {
        now += STUCK;
        let e = post_with(chain.clone(), &capped(now))
            .await
            .expect_err("cap");
        assert_eq!(e.class(), Some(classes::FEE_CAP), "{e:?}");
        assert!(e.msg().contains("above the fee cap 230"), "{e:?}");
    }
    assert_eq!(chain.sent().len(), 2, "nothing above the cap was ever sent");
    // With room again (a person raised the cap) the next raise goes out.
    run(&chain, &rec, now + STUCK).await.expect_err("replaced");
    assert_eq!(chain.sent().len(), 3);
    assert_eq!(decode(&chain.sent()[2]).2, 256);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first transaction is mined while the run that would replace it is
/// at work.
struct MinesBeforeTheReplacement(Chain);

impl JsonRpc for MinesBeforeTheReplacement {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            if method == "eth_maxPriorityFeePerGas" {
                self.0.mine_pool();
            }
            self.0.call(method, params).await
        })
    }
}

/// RETRY-R1-H1: a replacement cannot be included beside the transaction it
/// replaces. If the first mines while the replacement is made, the node
/// refuses the replacement ("nonce too low"): transient, and the next run
/// reads the one record. A node that asks more than this tool's raise
/// refuses it too: transient, counted, and in the end the refusal.
#[tokio::test]
async fn h1_a_replacement_that_races_the_first_transaction_makes_no_second_report() {
    let dir = scratch("race");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    let e = post_with(
        MinesBeforeTheReplacement(chain.clone()),
        &opts(Some(&rec), T0 + STUCK),
    )
    .await
    .expect_err("the first mined meanwhile");
    assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
    assert!(e.msg().contains("nonce too low"), "{e:?}");
    run(&chain, &rec, T0 + STUCK + 300).await.expect("already");
    assert_eq!((chain.sent().len(), chain.records()), (1, 1));

    let mut chain = Chain::new(Pool::Keeps);
    chain.min_bump_pct = 50;
    let rec = dir.join("tx2.json");
    run(&chain, &rec, T0).await.expect_err("not mined");
    let mut now = T0;
    // 12.5%, 26.6% and 43.8% more are refused, and none counts as a raise.
    for _ in 0..3 {
        now += STUCK;
        let e = run(&chain, &rec, now).await.expect_err("underpriced");
        assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
        assert!(e.msg().contains("replacement transaction underpriced"));
        assert!(e.msg().contains("not counted as a raise"), "{e:?}");
        assert_eq!(chain.sent().len(), 1);
    }
    // The fourth offers 62% more and is taken: the first raise.
    let e = run(&chain, &rec, now + STUCK).await.expect_err("replaced");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    assert_eq!(chain.sent().len(), 2);
    assert_eq!(decode(&chain.sent()[1]).2, 326);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R1-H1 (second half): an unmined transaction of the gas key that
/// this tool has no record of (a transfer made by hand, another report, a
/// run that kept no record) is never replaced. It is waited for while seen
/// for less than `--stuck-secs`, then refused, naming the nonce; once it is
/// gone the report is sent.
#[tokio::test]
async fn h1_a_transaction_the_tool_did_not_send_is_never_replaced_and_ends_as_a_refusal() {
    let dir = scratch("foreign");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    chain.state.lock().expect("lock").foreign = 1;
    for later in [0, 600, STUCK - 1] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("blocked");
        assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
        assert!(e.msg().contains("not this tool's: nothing sent"), "{e:?}");
    }
    for later in [STUCK, 2 * STUCK, 40 * STUCK] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("refused");
        assert_eq!(e.class(), None, "{later}: {e:?}");
        assert!(
            e.msg().contains("holds nonce 0")
                && e.msg().contains("this tool did not send it")
                && e.msg().contains("never replaced")
                && e.msg().contains("A stuck transaction"),
            "{e:?}"
        );
    }
    {
        let s = chain.state.lock().expect("lock");
        assert!(
            s.sent.is_empty() && s.refused.is_empty(),
            "nothing was sent"
        );
        assert!(!s.replaced_foreign);
    }
    // A person let it mine (or replaced it): the report goes out, after it.
    {
        let mut s = chain.state.lock().expect("lock");
        s.foreign = 0;
        s.latest = 1;
    }
    run(&chain, &rec, T0 + 41 * STUCK)
        .await
        .expect_err("sent, not mined");
    assert_eq!(chain.sent().len(), 1);
    assert_eq!(decode(&chain.sent()[0]).0, 1, "the next nonce");
    // A transaction at a nonce BEFORE this report's recorded one is not
    // ours either: this report's own transaction behind it is left alone,
    // however old it is.
    {
        let mut s = chain.state.lock().expect("lock");
        s.latest = 0;
        s.foreign = 1;
    }
    let e = run(&chain, &rec, T0 + 60 * STUCK)
        .await
        .expect_err("behind another");
    assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
    assert_eq!(chain.sent().len(), 1, "ours is not replaced behind another");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The reviewer's third missing test: a second send of the same report
/// meets a real pool. With a node that hides its pool and a run that keeps
/// no record, the second run builds the same transaction again: the node
/// answers "already known" and nothing new is in the pool. With the
/// record, the second run does not even build one.
#[tokio::test]
async fn a_second_send_meets_already_known_and_never_a_second_transaction() {
    let chain = Chain::new(Pool::Hides);
    post(&chain).await.expect_err("not mined");
    let e = post(&chain).await.expect_err("not mined again");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    assert_eq!(chain.sent().len(), 1, "the node took one transaction");
    let refused = chain.refused();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains(r#""message":"already known""#));
    assert_eq!(chain.state.lock().expect("lock").pool.len(), 1);
    // It mines: one record.
    chain.mine_pool();
    post(&chain).await.expect("already");
    assert_eq!(chain.records(), 1);
    // The same with the record kept.
    let dir = scratch("hides");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Hides);
    run(&chain, &rec, T0).await.expect_err("not mined");
    run(&chain, &rec, T0 + 28).await.expect_err("waited for");
    assert_eq!(chain.sent().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`Chain`], with the pooled transaction mined as soon as the estimate was
/// asked (the first transaction mined between the two).
struct Racing(Chain);

impl JsonRpc for Racing {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let r = self.0.call(method, params).await;
            if method == "eth_estimateGas" {
                self.0.mine_pool();
            }
            r
        })
    }
}

/// A run that has no record of the first transaction (an older tool sent
/// it) and a node that hides its pool: the second run gets as far as a
/// second transaction. The book refuses it (`Replay`) once the first mined,
/// and that revert, at the estimate or mined, is a success once the book
/// shows the record. Never a second report.
#[tokio::test]
async fn a_second_transaction_after_a_late_first_one_makes_no_second_report() {
    // The first mines between the second run's first read and its estimate.
    let mut chain = Chain::new(Pool::Hides);
    post(&chain).await.expect_err("not mined");
    chain.estimate = Some(REVERT);
    post_with(Racing(chain.clone()), &opts(None, T0))
        .await
        .expect("the revert is the first transaction's record");
    assert_eq!(
        chain.sent().len(),
        1,
        "the estimate reverted: no second send"
    );
    assert_eq!(chain.records(), 1);
    // The second was sent with the next nonce and mined after the first: it
    // reverts on chain, and the book holds the one record.
    let chain = Chain::new(Pool::RevertsAsReplay);
    post(&chain).await.expect("recorded by the earlier one");
    assert_eq!(chain.sent().len(), 1);
    assert!(chain.sent()[0].contains(&report_data()), "the same report");
    // A transaction that reverted while the book holds no record and gives
    // no reason is refused (with no record kept to read it a second time).
    let chain = Chain::new(Pool::Reverts);
    let e = post(&chain).await.expect_err("reverted");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("reverted"), "{e:?}");
    // With the record kept it is read once more, then refused, and never
    // sent a second time by itself (a revert that repeats would burn gas).
    let dir = scratch("reverts");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Reverts);
    let e = run(&chain, &rec, T0).await.expect_err("reverted");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    for later in [300, 600] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("refused");
        assert_eq!(e.class(), None, "{e:?}");
        assert!(e.msg().contains("gives no reason"), "{e:?}");
    }
    assert_eq!(chain.sent().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A recorded nonce that another transaction of the key used (no receipt
/// of ours): the earlier transaction can never mine, so the report is sent
/// anew at the key's next nonce.
#[tokio::test]
async fn a_nonce_taken_by_another_transaction_sends_the_report_anew() {
    let dir = scratch("taken");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    {
        // A transfer made by hand at nonce 0 replaced ours and mined.
        let mut s = chain.state.lock().expect("lock");
        s.pool.clear();
        s.latest = 1;
    }
    run(&chain, &rec, T0 + 60).await.expect_err("sent anew");
    let sent = chain.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!((decode(&sent[0]).0, decode(&sent[1]).0), (0, 1));
    chain.mine_pool();
    run(&chain, &rec, T0 + 120).await.expect("already");
    assert_eq!(chain.records(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The record file: another report's is not read (and is replaced); a file
/// that is not a record is refused before anything is sent while the key
/// has a pending transaction, and moved aside when it has none
/// (RETRY-R2-L2).
#[tokio::test]
async fn the_record_is_this_reports_or_nothing() {
    let dir = scratch("record");
    let rec = dir.join("tx.json");
    std::fs::write(&rec, "{ not json").expect("w");
    let busy = Chain::new(Pool::Mines);
    busy.state.lock().expect("lock").foreign = 1;
    let e = run(&busy, &rec, T0).await.expect_err("garbage");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("not a transaction record"), "{e:?}");
    assert!(busy.sent().is_empty());
    assert!(rec.exists(), "left where it is for a person");
    // Nothing of the key is pending: nothing of ours can be lost by
    // forgetting the file. It is kept under another name.
    let chain = Chain::new(Pool::Mines);
    run(&chain, &rec, T0).await.expect("anchored");
    let aside = dir.join(format!("tx.json.bad-{T0}"));
    assert_eq!(std::fs::read_to_string(&aside).expect("r"), "{ not json");
    assert_eq!(chain.sent().len(), 1);
    let chain = Chain::new(Pool::Mines);
    // A record of another report (a catalog built again under the name).
    let other = json!({
        "version": 1, "digest": Hash32([7; 32]).to_string(), "chain_id": 84_532,
        "book": hx(&[0xb0; 20]), "from": "0x00", "nonce": 9, "gas": 1,
        "txs": [{"hash": "0x01", "max_fee_per_gas": "1", "max_priority_fee_per_gas": "1",
                 "raw": "0x02", "sent_unix": 1}],
        "reverts": 0, "foreign": null,
    });
    std::fs::write(&rec, other.to_string()).expect("w");
    run(&chain, &rec, T0).await.expect("anchored");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&rec).expect("r")).expect("json");
    assert_eq!(
        v["digest"],
        json!(the_report().payload.digest().to_string())
    );
    assert_eq!(
        (&v["nonce"], &v["txs"][0]["sent"]),
        (&json!(0), &json!(true))
    );
    assert_eq!(
        anchor::record_path(Path::new("/x/2026-11-17.bin")),
        Path::new("/x/2026-11-17.bin.anchor-tx.json")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A node that is down or answers 5xx is transient and nothing is sent; a
/// node that refuses the caller (401) is not.
#[tokio::test]
async fn a_node_that_is_down_is_transient_and_nothing_is_sent() {
    for (down, class) in [
        (
            "node unreachable: connection refused",
            Some(classes::RPC_UNREACHABLE),
        ),
        ("node call timed out", Some(classes::RPC_UNREACHABLE)),
        ("node answered HTTP 503", Some(classes::RPC_HTTP)),
        ("node answered HTTP 429", Some(classes::RPC_HTTP)),
        ("node answered HTTP 401", None),
        (
            r#"node error: {"code":-32005,"message":"rate limit"}"#,
            Some(classes::RPC_BUSY),
        ),
    ] {
        let mut chain = Chain::new(Pool::Mines);
        chain.down = Some(down);
        let e = post(&chain).await.expect_err("down");
        assert_eq!(e.class(), class, "{down}: {e:?}");
        assert!(chain.sent().is_empty());
    }
}

/// Refusals are never transient: another payload under the sequence, a gas
/// key without money (its own line), a report the book rejects, and a stuck
/// report that went stale (RETRY-R1-L4: the book is asked before a
/// replacement, and its refusal is the answer).
#[tokio::test]
async fn refusals_are_not_transient() {
    let mut chain = Chain::new(Pool::Mines);
    chain.other_digest = Some([7; 32]);
    let e = post(&chain).await.expect_err("conflict");
    assert_eq!(
        e,
        Stop::Refused("another payload under this sequence".into())
    );
    assert!(chain.sent().is_empty());

    let mut chain = Chain::new(Pool::Mines);
    chain.estimate =
        Some(r#"{"code":-32000,"message":"insufficient funds for gas * price + value"}"#);
    let e = post(&chain).await.expect_err("no gas money");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("too little test ETH"), "{e:?}");
    assert!(chain.sent().is_empty());

    let stale = r#"{"code":3,"message":"execution reverted: Stale()"}"#;
    let mut chain = Chain::new(Pool::Mines);
    chain.estimate = Some(stale);
    let e = post(&chain).await.expect_err("rejected");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("would revert, not sent"), "{e:?}");
    assert!(chain.sent().is_empty());

    let dir = scratch("stale");
    let rec = dir.join("tx.json");
    let mut chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    chain.estimate = Some(stale);
    let e = run(&chain, &rec, T0 + 8 * 86_400).await.expect_err("stale");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("would revert, not replaced"), "{e:?}");
    assert_eq!(chain.sent().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A transaction that mined while the node's `latest` does not show the
/// record yet is transient; the plain case anchors with one transaction.
#[tokio::test]
async fn a_mined_report_is_anchored_once() {
    let chain = Chain::new(Pool::Mines);
    post(&chain).await.expect("anchored");
    post(&chain).await.expect("already");
    assert_eq!(chain.sent().len(), 1);
    let dir = scratch("lag");
    let rec = dir.join("tx.json");
    let mut chain = Chain::new(Pool::Mines);
    chain.lags = true;
    let e = run(&chain, &rec, T0).await.expect_err("not visible");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    // The next run finds its own receipt (the nonce is used up) and, once
    // the read catches up, is done: no second transaction either way.
    let e = run(&chain, &rec, T0 + 60).await.expect_err("still behind");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    chain.lags = false;
    run(&chain, &rec, T0 + 120).await.expect("visible now");
    assert_eq!(chain.sent().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R1-L1: a `Replay` revert (mined, or at the estimate) read from a
/// backend one block behind is `not_visible` (exit 75), as the same lag
/// after a good receipt is: the book says it holds the report.
#[tokio::test]
async fn l1_a_replay_revert_read_from_a_lagging_backend_is_not_visible() {
    let mut chain = Chain::new(Pool::RevertsAsReplay);
    chain.lags = true;
    let e = post(&chain).await.expect_err("lagging");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    assert!(e.msg().contains("Replay"), "{e:?}");
    // At the estimate: the record is there, the read is behind.
    let mut chain = Chain::new(Pool::Mines);
    chain.lags = true;
    chain.state.lock().expect("lock").recorded = true;
    let e = post(&chain).await.expect_err("lagging");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    assert!(chain.sent().is_empty());
    chain.lags = false;
    post(&chain).await.expect("already");
}

/// RETRY-R1-L2 (the reviewer's strict fake): `report(bytes32)` is answered
/// only for one digest, at `latest`, and the book's `Replay` only for that
/// digest's report. A record under ANOTHER digest never counts as done.
struct Strict(Chain, [u8; 32]);

impl JsonRpc for Strict {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            if method == "eth_call" {
                let data = params[0]["data"].as_str().unwrap_or_default().to_string();
                let sel = |sig: &str| hex::encode(&keccak256(sig.as_bytes()).0[..4]);
                if data[2..].starts_with(&sel("report(bytes32)")) {
                    assert_eq!(params[1], "latest", "block tag");
                    if data[10..] != hex::encode(self.1) {
                        return Ok(json!(format!("0x{}", hex::encode([0u8; 64]))));
                    }
                }
                // The book refuses a second copy of the report it holds,
                // not of another one.
                if data[2..].starts_with(&sel("postReport(bytes,bytes)"))
                    && self.1 != the_report().payload.digest().0
                {
                    return Ok(json!("0x"));
                }
            }
            self.0.call(method, params).await
        })
    }
}

#[tokio::test]
async fn l2_the_record_is_read_by_the_reports_own_digest() {
    let digest = the_report().payload.digest().0;
    // A mined-and-reverted transaction with a record under ANOTHER digest
    // is a refusal, not "done".
    let other = Strict(Chain::new(Pool::RevertsAsReplay), [9; 32]);
    let e = post_with(other, &opts(None, T0))
        .await
        .expect_err("no record under this digest");
    assert_eq!(e.class(), None, "{e:?}");
    let own = Strict(Chain::new(Pool::RevertsAsReplay), digest);
    post_with(own, &opts(None, T0))
        .await
        .expect("recorded under its own digest");
}

/// RETRY-R1-M2: a chain node's errors by their code and whole phrases. A
/// quota, an answer that is not a node's and a word inside another message
/// are refusals; a full pool, a fee that moved, the fee cap and an internal
/// error pass by themselves, each with its own class.
#[test]
fn m2_chain_node_errors_by_class() {
    let node = |method: &str, code: i64, msg: &str| {
        format!(r#"{method}: node error: {{"code":{code},"message":"{msg}"}}"#)
    };
    let send = |msg: &str| {
        format!(
            "postReport: rpc: {}",
            node("eth_sendRawTransaction", -32000, msg)
        )
    };
    let s = String::from;
    let table: Vec<(String, Option<&str>)> = vec![
        // Transport and HTTP status, in the client's own words.
        (
            s("eth_chainId: node unreachable: dns error"),
            Some(classes::RPC_UNREACHABLE),
        ),
        (
            s("eth_call: node answer unreadable or over 1048576 bytes: x"),
            Some(classes::RPC_UNREACHABLE),
        ),
        (
            s("eth_call: node call timed out"),
            Some(classes::RPC_UNREACHABLE),
        ),
        (
            s("eth_call: node answered HTTP 502"),
            Some(classes::RPC_HTTP),
        ),
        (
            s("eth_call: node answered HTTP 408"),
            Some(classes::RPC_HTTP),
        ),
        (
            s("eth_call: node answered HTTP 429"),
            Some(classes::RPC_HTTP),
        ),
        (s("eth_call: node answered HTTP 403"), None),
        (s("eth_call: node answered HTTP 404"), None),
        // Not a JSON-RPC node at all (a typo, a landing page).
        (
            s("eth_chainId: bad answer: expected value at line 1 column 1"),
            None,
        ),
        (s("eth_chainId: answer has no result"), None),
        (s("eth_chainId: answer is for another request"), None),
        // A rate limit passes; a quota does not, whatever its code.
        (
            node("eth_call", -32005, "rate limit exceeded"),
            Some(classes::RPC_BUSY),
        ),
        (
            node("eth_call", 429, "Too Many Requests"),
            Some(classes::RPC_BUSY),
        ),
        (
            node("eth_call", -32000, "request timed out"),
            Some(classes::RPC_BUSY),
        ),
        (
            node(
                "eth_call",
                429,
                "Monthly capacity limit exceeded. Visit the dashboard to upgrade",
            ),
            None,
        ),
        (
            node(
                "eth_call",
                -32005,
                "daily request count exceeded, request rate limited",
            ),
            None,
        ),
        (node("eth_call", -32005, "project quota reached"), None),
        // A word inside another message is not a class.
        (
            node(
                "eth_getTransactionCount",
                -32602,
                "invalid params: unknown field timeout",
            ),
            None,
        ),
        (
            node(
                "eth_call",
                -32001,
                "API key disabled, try again with a valid key",
            ),
            None,
        ),
        (
            node(
                "eth_call",
                -32000,
                "method temporarily disabled: overloaded keyword",
            ),
            None,
        ),
        // The nonce is taken by an earlier send of the report.
        (send("already known"), Some(classes::EARLIER_PENDING)),
        (send("nonce too low"), Some(classes::EARLIER_PENDING)),
        (
            send("replacement transaction underpriced"),
            Some(classes::EARLIER_PENDING),
        ),
        // Stops that pass, each with its own class.
        (send("txpool is full"), Some(classes::POOL_FULL)),
        (send("transaction underpriced"), Some(classes::UNDERPRICED)),
        (
            send("max fee per gas less than block base fee"),
            Some(classes::UNDERPRICED),
        ),
        (
            s("postReport: rpc: base fee 11000000000 is at or above the fee cap 10000000000"),
            Some(classes::FEE_CAP),
        ),
        (
            node("eth_call", -32603, "Internal error"),
            Some(classes::RPC_INTERNAL),
        ),
        (
            node("eth_call", -32000, "missing trie node"),
            Some(classes::RPC_BUSY),
        ),
        (
            node("eth_estimateGas", -32000, "header not found"),
            Some(classes::RPC_BUSY),
        ),
        (
            s("postReport: rpc: header not found (block 9 still missing after 20 tries; later estimates name the node's head 8)"),
            Some(classes::RPC_BUSY),
        ),
        // Money first: nothing beside it makes it transient.
        (
            node(
                "eth_estimateGas",
                -32603,
                "insufficient funds for gas * price + value",
            ),
            None,
        ),
        (
            node("eth_estimateGas", -32005, "insufficient funds, rate limit"),
            None,
        ),
        // The tool's own gas cap and a revert are refusals; so is the unknown.
        (
            s("postReport: rpc: gas estimate 99000000 is above the gas cap 15000000"),
            None,
        ),
        (node("eth_call", 3, "execution reverted"), None),
        (s("eth_call: Invalid character 'x' at position 1"), None),
        (node("eth_call", -32000, "something new"), None),
        (s("something new"), None),
    ];
    for (e, class) in table {
        assert_eq!(rpc_class(&e), class, "{e}");
    }
    // A message cut at the client's 200 characters still gives its code.
    let cut = r#"eth_call: node error: {"code":-32603,"message":"Internal err"#;
    assert_eq!(rpc_class(cut), Some(classes::RPC_INTERNAL));
}

#[test]
fn node_answers_by_class() {
    let code = |c: &str| json!({"error": {"code": c}});
    for (status, v, class) in [
        (409, code("bulk_not_anchored"), Some(classes::NOT_FINAL)),
        (409, code("bulk_not_successor"), None),
        (
            503,
            code("bulk_chain_unavailable"),
            Some(classes::NODE_BUSY),
        ),
        (429, code("rate_limited"), Some(classes::NODE_BUSY)),
        (502, json!(null), Some(classes::NODE_BUSY)),
        (503, code("bulk_node_full"), None),
        (503, json!(null), Some(classes::NODE_ERROR)),
        (500, code("internal"), Some(classes::NODE_ERROR)),
        (504, json!(null), Some(classes::NODE_ERROR)),
        (400, code("bad_request"), None),
        (403, code("forbidden"), None),
        (413, json!(null), None),
        (422, code("bulk_bad_manifest"), None),
    ] {
        assert_eq!(fail_class(status, &v), class, "{status} {v}");
    }
}

/// What a wrapper matches on: exit 75 and a last stderr line with the class
/// for a transient stop, exit 1 and no such line for a refusal; the usage
/// text says so, which is how a wrapper knows this tool has the exit code.
#[test]
fn the_exit_code_and_the_last_line_tell_transient_from_refused() {
    let t = Stop::transient(
        classes::NOT_MINED,
        "postReport: transaction 0xab not mined before the deadline",
    );
    assert_eq!(stop_exit(&t), 75);
    assert_eq!(EXIT_TRANSIENT, 75);
    let out = stop_lines("catalog-anchor", &t);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "onyx-publish catalog-anchor: postReport: transaction 0xab not mined before the deadline"
    );
    assert!(
        lines[1].starts_with("onyx-publish catalog-anchor: TRANSIENT class=not_mined "),
        "{out}"
    );
    assert_eq!(lines.len(), 2);
    assert!(out.ends_with('\n'));
    let r = Stop::from("the node at --rpc is not chain 84532");
    assert_eq!(stop_exit(&r), 1);
    let out = stop_lines("anchor", &r);
    assert_eq!(
        out,
        "onyx-publish anchor: the node at --rpc is not chain 84532\n"
    );
    assert!(!out.contains("TRANSIENT"));
    assert!(USAGE.contains("exit 75"), "the marker a wrapper probes for");
    // Every class is one lower-case word a shell pattern can take.
    for c in [
        classes::NOT_MINED,
        classes::EARLIER_PENDING,
        classes::RPC_UNREACHABLE,
        classes::RPC_HTTP,
        classes::RPC_BUSY,
        classes::RPC_INTERNAL,
        classes::POOL_FULL,
        classes::UNDERPRICED,
        classes::FEE_CAP,
        classes::NOT_VISIBLE,
        classes::NOT_FINAL,
        classes::NODE_BUSY,
        classes::NODE_ERROR,
        classes::NODE_UNREACHABLE,
    ] {
        assert!(
            c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{c}"
        );
    }
}

/// The settings of the anchor commands are whole numbers in range.
#[test]
fn anchor_settings_are_checked() {
    let parse = |a: &[&str]| {
        let a: Vec<String> = a.iter().map(|s| s.to_string()).collect();
        AnchorOpts::from_opts(&parse_opts(&a).expect("opts"), PathBuf::from("/x/tx.json"))
    };
    let d = parse(&[]).expect("defaults");
    assert_eq!(
        (
            d.deadline.as_secs(),
            d.stuck.as_secs(),
            d.max_bumps,
            d.max_fee_wei
        ),
        (180, 1800, 4, 10_000_000_000)
    );
    assert!(d.record.is_some());
    let o = parse(&[
        "--stuck-secs",
        "0",
        "--max-bumps",
        "0",
        "--max-fee-wei",
        "1",
        "--mine-secs",
        "3",
    ])
    .expect("low ends");
    assert_eq!(
        (
            o.stuck.as_secs(),
            o.max_bumps,
            o.max_fee_wei,
            o.deadline.as_secs()
        ),
        (0, 0, 1, 3)
    );
    for bad in [
        ["--stuck-secs", "-1"],
        ["--stuck-secs", "700000"],
        ["--max-bumps", "21"],
        ["--max-fee-wei", "0"],
        ["--max-fee-wei", "1000000000001"],
        ["--max-fee-wei", "10gwei"],
        ["--mine-secs", "0"],
        ["--mine-secs", "3601"],
    ] {
        assert!(parse(&bad).is_err(), "{bad:?}");
    }
}

/// Review RETRY-R2: the tests of its findings.
#[path = "retry_r2_tests.rs"]
mod r2;
