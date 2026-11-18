//! Review RETRY-R2 of the anchor's transaction record and replacement
//! (THREATS RETRY-R2-M1..L3). The reviewer's attack tests, turned into
//! tests of the fixed behaviour; the fake chain is `retry_tests`'s.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A node whose `eth_sendRawTransaction` answers 503 while `send_down`,
/// whose receipts cannot be read while `receipts_down`, and whose fee
/// answers are absurd while `absurd`.
struct Flaky {
    chain: Chain,
    send_down: AtomicBool,
    absurd: AtomicBool,
    receipts_down: AtomicBool,
}

impl Flaky {
    fn new(chain: &Chain) -> Flaky {
        Flaky {
            chain: chain.clone(),
            send_down: AtomicBool::new(false),
            absurd: AtomicBool::new(false),
            receipts_down: AtomicBool::new(false),
        }
    }
}

impl JsonRpc for &Flaky {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move {
            let max = "0xffffffffffffffffffffffffffffffff";
            match method {
                "eth_sendRawTransaction" if self.send_down.load(Ordering::Relaxed) => {
                    Err("eth_sendRawTransaction: node answered HTTP 503".to_string())
                }
                "eth_getTransactionReceipt" if self.receipts_down.load(Ordering::Relaxed) => {
                    Err("eth_getTransactionReceipt: node answered HTTP 429".to_string())
                }
                "eth_getBlockByNumber" if self.absurd.load(Ordering::Relaxed) => {
                    Ok(json!({ "baseFeePerGas": max }))
                }
                "eth_maxPriorityFeePerGas" if self.absurd.load(Ordering::Relaxed) => Ok(json!(max)),
                _ => self.chain.call(method, params).await,
            }
        })
    }
}

fn read(rec: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(rec).expect("record")).expect("json")
}

/// Write `v` as the record (as a person, or a restore, would).
fn write(rec: &Path, v: &Value) {
    std::fs::write(rec, v.to_string()).expect("w");
}

/// A sixth of the bound: the least time between two tries to replace.
const GAP: u64 = STUCK / 6;

/// RETRY-R2-M1: a replacement that never reached the node (the chain node
/// answers 503 to the send for a few minutes) is not a fee raise. The same
/// signed bytes go out again once the node is well, no sooner than the gap
/// after the last try, and all four raises are still there to be used.
#[tokio::test]
async fn m1_a_send_that_never_left_is_not_a_raise_and_is_tried_again_after_a_gap() {
    let dir = scratch("r2-m1");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    let node = Flaky::new(&chain);
    post_with(&node, &opts(Some(&rec), T0))
        .await
        .expect_err("not mined");
    node.send_down.store(true, Ordering::Relaxed);
    let e = post_with(&node, &opts(Some(&rec), T0 + STUCK))
        .await
        .expect_err("503");
    assert_eq!(e.class(), Some(classes::RPC_HTTP), "{e:?}");
    assert!(e.msg().contains("not counted as a raise"), "{e:?}");
    // The version is in the record, marked unsent, before anything else.
    let v = read(&rec);
    assert_eq!(v["txs"].as_array().expect("txs").len(), 2);
    assert_eq!(v["txs"][1]["sent"], json!(false));
    // Seconds later: no second try yet (the gap), whatever the node says.
    let calls = chain.state.lock().expect("lock").calls.len();
    for i in 1..=3 {
        let e = post_with(&node, &opts(Some(&rec), T0 + STUCK + i))
            .await
            .expect_err("waits");
        assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    }
    node.send_down.store(false, Ordering::Relaxed);
    let e = post_with(&node, &opts(Some(&rec), T0 + STUCK + GAP - 1))
        .await
        .expect_err("waits");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    assert_eq!(chain.sent().len(), 1, "no try inside the gap");
    let sends = |from: usize| {
        let s = chain.state.lock().expect("lock");
        s.calls[from..]
            .iter()
            .filter(|m| *m == "eth_sendRawTransaction")
            .count()
    };
    assert_eq!(
        sends(calls),
        0,
        "nothing was handed to the node inside the gap"
    );
    // After the gap the very same bytes go out, and are the first raise.
    let unsent = read(&rec)["txs"][1]["raw"].clone();
    let e = post_with(&node, &opts(Some(&rec), T0 + STUCK + GAP))
        .await
        .expect_err("replaced, not mined");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    assert_eq!(chain.sent().len(), 2);
    assert_eq!(json!(chain.sent()[1]), unsent);
    assert_eq!(read(&rec)["txs"][1]["sent"], json!(true));
    // Three more raises are left, then the refusal says four, truly.
    let mut now = T0 + STUCK + GAP;
    for _ in 0..3 {
        now += STUCK;
        run(&chain, &rec, now).await.expect_err("replaced");
    }
    assert_eq!(chain.sent().len(), 5);
    let e = run(&chain, &rec, now + STUCK).await.expect_err("refused");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("raised 4 times"), "{e:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-M2: a replacement is written to the record before it is sent.
/// After a stop between the send and its answer, the next run finds the
/// version marked unsent, hands the same bytes to the node, and takes its
/// "already known" as sent: the record learns the hash, no raise is lost
/// and nothing new is signed.
#[tokio::test]
async fn m2_a_replacement_sent_before_a_stop_is_learned_from_already_known() {
    let dir = scratch("r2-m2");
    let rec = dir.join("tx.json");
    // A node that neither counts its pool nor knows a pooled transaction
    // by its hash (the node of 2026-11-18): only the send's answer tells.
    let mut chain = Chain::new(Pool::Hides);
    chain.blind = true;
    run(&chain, &rec, T0).await.expect_err("not mined");
    run(&chain, &rec, T0 + STUCK).await.expect_err("replaced");
    assert_eq!(chain.sent().len(), 2);
    // The stop: the record as it was written before the send was answered.
    let mut stopped = read(&rec);
    assert_eq!(stopped["txs"][1]["sent"], json!(true));
    stopped["txs"][1]["sent"] = json!(false);
    write(&rec, &stopped);
    let e = run(&chain, &rec, T0 + STUCK + 1).await.expect_err("known");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    let v = read(&rec);
    assert_eq!(v["txs"].as_array().expect("txs").len(), 2);
    assert_eq!(v["txs"][1]["sent"], json!(true), "learned from the node");
    assert_eq!(v["txs"][1]["hash"], stopped["txs"][1]["hash"]);
    assert_eq!(chain.sent().len(), 2, "nothing new was signed or taken");
    assert!(chain
        .refused()
        .iter()
        .any(|e| e.contains(r#""message":"already known""#)));
    // On a node that knows its pool by hash, the question alone tells.
    let chain = Chain::new(Pool::Keeps);
    let rec = dir.join("tx2.json");
    run(&chain, &rec, T0).await.expect_err("not mined");
    run(&chain, &rec, T0 + STUCK).await.expect_err("replaced");
    let mut stopped = read(&rec);
    stopped["txs"][1]["sent"] = json!(false);
    write(&rec, &stopped);
    run(&chain, &rec, T0 + STUCK + 1).await.expect_err("known");
    assert_eq!(read(&rec)["txs"][1]["sent"], json!(true));
    assert_eq!(chain.sent().len(), 2);
    assert!(chain.refused().is_empty(), "not even handed over again");
    // The record put back to an older state (a restore): the pooled
    // replacement is then not in the record, so it is another's by the
    // record's own rule: waited for, then refused, never replaced.
    let mut old = read(&rec);
    old["txs"].as_array_mut().expect("txs").pop();
    write(&rec, &old);
    let e = run(&chain, &rec, T0 + STUCK + 2).await.expect_err("other");
    assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
    assert!(e.msg().contains("not this tool's"), "{e:?}");
    let e = run(&chain, &rec, T0 + 3 * STUCK)
        .await
        .expect_err("refused");
    assert_eq!(e.class(), None, "{e:?}");
    assert_eq!(chain.sent().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-M3: the recorded nonce is not proof of whose transaction is
/// pending there. The record's own transaction was dropped and one made by
/// hand (a transfer, a cancel as the runbook says) holds the nonce: the
/// tool asks for its own versions by hash, finds none, and treats the
/// pending one as another's: nothing is handed over, nothing is replaced,
/// and it ends as the refusal that names the nonce.
#[tokio::test]
async fn m3_a_transaction_made_by_hand_at_the_recorded_nonce_is_never_sent_over() {
    let dir = scratch("r2-m3");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    {
        let mut s = chain.state.lock().expect("lock");
        s.pool.clear();
        s.foreign = 1;
    }
    for later in [60, STUCK, STUCK + 59] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("another's");
        assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{later}: {e:?}");
        assert!(e.msg().contains("not this tool's: nothing sent"), "{e:?}");
    }
    let e = run(&chain, &rec, T0 + 2 * STUCK + 60)
        .await
        .expect_err("refused");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("holds nonce 0"), "{e:?}");
    {
        let s = chain.state.lock().expect("lock");
        assert!(!s.replaced_foreign);
        assert_eq!(s.sent.len(), 1, "only the first send, before the drop");
        assert!(s.refused.is_empty(), "nothing was even handed to the node");
    }
    // The same on a node that shows neither its pool nor a pooled hash:
    // the recorded version is handed back as signed (no fee raised), the
    // node's "replacement transaction underpriced" says another one holds
    // the nonce, and that is the end of it.
    let mut chain = Chain::new(Pool::Hides);
    chain.blind = true;
    let rec = dir.join("tx2.json");
    run(&chain, &rec, T0).await.expect_err("not mined");
    {
        let mut s = chain.state.lock().expect("lock");
        s.pool.clear();
        s.foreign = 1;
    }
    for later in [STUCK, 2 * STUCK - 1] {
        let e = run(&chain, &rec, T0 + later).await.expect_err("another's");
        assert_eq!(e.class(), Some(classes::EARLIER_PENDING), "{e:?}");
    }
    let e = run(&chain, &rec, T0 + 2 * STUCK)
        .await
        .expect_err("refused");
    assert_eq!(e.class(), None, "{e:?}");
    {
        let s = chain.state.lock().expect("lock");
        assert!(!s.replaced_foreign);
        assert_eq!(s.sent.len(), 1);
        assert!(s
            .refused
            .iter()
            .all(|e| e.contains("replacement transaction underpriced")));
        // Every time the same recorded bytes: no raise was ever signed.
        assert_eq!(read(&rec)["txs"].as_array().expect("txs").len(), 1);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-M3: the record's own transaction was dropped and nothing holds
/// its nonce: it goes back to the node as it was signed, once, with no fee
/// raised, and from then its time to be mined runs anew.
#[tokio::test]
async fn m3_a_dropped_transaction_is_handed_back_as_signed_and_not_raised() {
    let dir = scratch("r2-m3-dropped");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    chain.state.lock().expect("lock").pool.clear();
    // Long past the bound: still not a replacement, the same bytes.
    let e = run(&chain, &rec, T0 + 3 * STUCK)
        .await
        .expect_err("handed back");
    assert_eq!(e.class(), Some(classes::NOT_MINED), "{e:?}");
    let sent = chain.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "as signed: no new fee");
    assert_eq!(read(&rec)["txs"].as_array().expect("txs").len(), 1);
    assert_eq!(read(&rec)["txs"][0]["sent_unix"], json!(T0 + 3 * STUCK));
    // Known to the node now: not handed over again, and replaced only
    // after the bound has passed once more.
    run(&chain, &rec, T0 + 4 * STUCK - 1)
        .await
        .expect_err("young");
    assert_eq!(chain.sent().len(), 2);
    run(&chain, &rec, T0 + 4 * STUCK)
        .await
        .expect_err("replaced");
    assert_eq!(chain.sent().len(), 3);
    assert!(decode(&chain.sent()[2]).2 > decode(&sent[0]).2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-L1: a receipt that cannot be read (429) is not "no receipt".
/// The run stops as transient and the mined transaction's record stays; the
/// run after it, with the receipt readable, is done without a second send.
#[tokio::test]
async fn l1_a_receipt_that_cannot_be_read_keeps_the_record() {
    let dir = scratch("r2-l1");
    let rec = dir.join("tx.json");
    let mut chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    let before = read(&rec);
    chain.mine_pool();
    chain.lags = true;
    let node = Flaky::new(&chain);
    node.receipts_down.store(true, Ordering::Relaxed);
    let e = post_with(&node, &opts(Some(&rec), T0 + 60))
        .await
        .expect_err("unreadable");
    assert_eq!(e.class(), Some(classes::RPC_HTTP), "{e:?}");
    assert_eq!(read(&rec), before, "the record is as it was");
    node.receipts_down.store(false, Ordering::Relaxed);
    let e = post_with(&node, &opts(Some(&rec), T0 + 120))
        .await
        .expect_err("behind");
    assert_eq!(e.class(), Some(classes::NOT_VISIBLE), "{e:?}");
    assert_eq!(read(&rec), before);
    chain.lags = false;
    run(&chain, &rec, T0 + 180).await.expect("already");
    assert_eq!(chain.sent().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-L2: the record is written through a new name of the run's own
/// (never an existing file, never a link), owner-only whatever the umask,
/// and renamed over the record; a record that is a link is never read.
#[cfg(unix)]
#[tokio::test]
async fn l2_the_record_is_written_owner_only_and_never_through_a_link() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("r2-l2");
    let rec = dir.join("tx.json");
    let victim = dir.join("victim");
    std::fs::write(&victim, "mine").expect("w");
    // The name the first write used: a link there is neither followed nor
    // replaced.
    std::os::unix::fs::symlink(&victim, dir.join("tx.json.new")).expect("link");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    assert_eq!(std::fs::read_to_string(&victim).expect("r"), "mine");
    let meta = std::fs::symlink_metadata(&rec).expect("m");
    assert!(meta.file_type().is_file(), "the record is a plain file");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    // A replacement rewrites it the same way, and no work file is left.
    run(&chain, &rec, T0 + STUCK).await.expect_err("replaced");
    assert_eq!(
        std::fs::symlink_metadata(&rec)
            .expect("m")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["tx.json", "tx.json.new", "victim"]);
    // A record that is a link (to a record of this very report, even) is
    // refused: nothing is read through it and nothing is sent.
    let linked = dir.join("linked.json");
    std::os::unix::fs::symlink(&rec, &linked).expect("link");
    let before = chain.sent().len();
    let e = run(&chain, &linked, T0 + STUCK + 1)
        .await
        .expect_err("link");
    assert_eq!(e.class(), None, "{e:?}");
    assert!(e.msg().contains("link"), "{e:?}");
    assert_eq!(chain.sent().len(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// RETRY-R2-L3: the record is checked against the signed bytes it holds.
/// A changed nonce, gas, fee or hash, bytes of another transaction, or an
/// "unsent" mark on an old version is refused before anything is sent: an
/// edited record would steer what is replaced, with how much gas and at
/// what price. A time in the future is brought back to now and written, so
/// the transaction still becomes stuck one bound later.
#[tokio::test]
async fn l3_a_record_that_does_not_match_its_signed_bytes_is_refused() {
    let dir = scratch("r2-l3");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    run(&chain, &rec, T0).await.expect_err("not mined");
    run(&chain, &rec, T0 + STUCK).await.expect_err("replaced");
    let good = read(&rec);
    let other_raw = {
        // A transaction of the same key, nonce and fees with other data.
        let tx = onyx_api::evm_tx::Tx1559 {
            chain_id: 84_532,
            nonce: 0,
            max_priority_fee_per_gas: 1,
            max_fee_per_gas: 201,
            gas: good["gas"].as_u64().expect("gas"),
            to: [0xb0; 20],
            value: 0,
            data: vec![1, 2, 3],
        };
        let (raw, hash) = tx.sign(&GAS_KEY).expect("signs");
        (hx(&raw), hx(&hash))
    };
    let edits: Vec<(&str, Edit)> = vec![
        ("the nonce", Box::new(|v| v["nonce"] = json!(7))),
        ("the gas", Box::new(|v| v["gas"] = json!(14_000_000))),
        ("no gas", Box::new(|v| v["gas"] = json!(0))),
        (
            "a lower fee, so that a raise looks cheap",
            Box::new(|v| v["txs"][1]["max_fee_per_gas"] = json!("1")),
        ),
        (
            "a fee above any cap",
            Box::new(|v| v["txs"][1]["max_fee_per_gas"] = json!("9000000000000")),
        ),
        (
            "another hash",
            Box::new(|v| v["txs"][1]["hash"] = json!(format!("0x{}", "ab".repeat(32)))),
        ),
        (
            "bytes that are no transaction",
            Box::new(|v| v["txs"][0]["raw"] = json!("0x02c0")),
        ),
        (
            "another transaction's bytes",
            Box::new({
                let (raw, hash) = other_raw.clone();
                move |v| {
                    v["txs"][0]["raw"] = json!(raw.clone());
                    v["txs"][0]["hash"] = json!(hash.clone());
                }
            }),
        ),
        (
            "an old version unsent",
            Box::new(|v| v["txs"][0]["sent"] = json!(false)),
        ),
    ];
    for (what, edit) in edits {
        let mut v = good.clone();
        edit(&mut v);
        write(&rec, &v);
        let before = chain.state.lock().expect("lock").calls.len();
        let e = run(&chain, &rec, T0 + 3 * STUCK).await.expect_err(what);
        assert_eq!(e.class(), None, "{what}: {e:?}");
        assert!(
            e.msg().contains("does not match this report"),
            "{what}: {e:?}"
        );
        let s = chain.state.lock().expect("lock");
        assert!(
            !s.calls[before..]
                .iter()
                .any(|m| m == "eth_sendRawTransaction"),
            "{what}: nothing is handed to the node"
        );
        assert_eq!(read(&rec), v, "{what}: left as it is for a person");
    }
    // The gas of a replacement is the first send's: its estimate with the
    // margin, never a new and larger one.
    write(&rec, &good);
    let gas = |raw: &str| anchor_gas(raw);
    assert_eq!(gas(&chain.sent()[1]), gas(&chain.sent()[0]));
    assert_eq!(json!(gas(&chain.sent()[0])), good["gas"]);
    // A time in the future: brought back to now, and written.
    let mut ahead = good.clone();
    ahead["txs"][1]["sent_unix"] = json!(T0 + 1_000_000);
    ahead["last_try_unix"] = json!(T0 + 1_000_000);
    write(&rec, &ahead);
    run(&chain, &rec, T0 + 2 * STUCK)
        .await
        .expect_err("young again");
    assert_eq!(read(&rec)["txs"][1]["sent_unix"], json!(T0 + 2 * STUCK));
    assert_eq!(chain.sent().len(), 2);
    run(&chain, &rec, T0 + 3 * STUCK)
        .await
        .expect_err("replaced");
    assert_eq!(chain.sent().len(), 3, "stuck one bound later, not never");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A change of a record, as someone with the file could make it.
type Edit = Box<dyn Fn(&mut Value)>;

/// The gas limit in a raw transaction (the fifth field).
fn anchor_gas(raw: &str) -> u64 {
    let b = hex::decode(raw.trim_start_matches("0x")).expect("hex");
    let (list, _) = rlp_item(&b[1..]);
    let mut rest = list;
    for _ in 0..4 {
        rest = rlp_item(rest).1;
    }
    rlp_item(rest)
        .0
        .iter()
        .fold(0u64, |a, x| (a << 8) | u64::from(*x))
}

/// Money (the code was right; the test was missing): a node that answers
/// the largest base fee and tip there are gets a replacement at the cap,
/// never above it, on both fees; after that no room is left, which is a
/// `fee_cap` stop and never a transaction.
#[tokio::test]
async fn money_absurd_fee_answers_stop_at_the_cap_on_both_fees() {
    let dir = scratch("r2-money");
    let rec = dir.join("tx.json");
    let chain = Chain::new(Pool::Keeps);
    let node = Flaky::new(&chain);
    post_with(&node, &opts(Some(&rec), T0))
        .await
        .expect_err("not mined");
    node.absurd.store(true, Ordering::Relaxed);
    post_with(&node, &opts(Some(&rec), T0 + STUCK))
        .await
        .expect_err("replaced");
    let sent = chain.sent();
    assert_eq!(sent.len(), 2);
    let (nonce, tip, max_fee) = decode(&sent[1]);
    assert_eq!(nonce, 0);
    assert_eq!((tip, max_fee), (10_000_000_000, 10_000_000_000));
    for k in 2..6 {
        let e = post_with(&node, &opts(Some(&rec), T0 + k * STUCK))
            .await
            .expect_err("cap");
        assert_eq!(e.class(), Some(classes::FEE_CAP), "{e:?}");
    }
    assert_eq!(chain.sent().len(), 2);
    assert!(chain.sent().iter().all(|raw| {
        let (_, tip, fee) = decode(raw);
        tip <= 10_000_000_000 && fee <= 10_000_000_000
    }));
    let _ = std::fs::remove_dir_all(&dir);
}
