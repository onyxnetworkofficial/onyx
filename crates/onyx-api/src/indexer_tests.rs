//! Chain indexer tests (ADR-0033) over a fake JSON-RPC node: span caps and
//! splits, cursor resume, a lagging load-balanced backend, repeated and
//! out-of-order logs, and every way an event can fail to make a shard
//! servable (the contract view, the held rows, the binding, the label set's
//! evidence).

use super::*;
use crate::roots::GET_FINALIZED_ROOT_SELECTOR;
use crate::x402::BoxFuture;
use onyx_coordinator::cas::MemCas;
use onyx_core::{keccak256, ChainId, Confidence, EntityKind, Snapshot, SourceId, SourceRef};
use onyx_labels::{Label, LabelSet};
use onyx_recipe_sdk::OutputRow;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const CHAIN: u64 = 84_532;
const CONTRACT: [u8; 20] = [0x5e; 20];
const DEPLOY: u64 = 1_000;

/// (root, binding, finalizedAt, first block at which the view answers).
type View = (Hash32, Hash32, u64, u64);

/// A chain as a load-balanced public node shows it.
struct Fake {
    chain_id: u64,
    /// `finalized` answers in order; the last one repeats.
    heads: Mutex<VecDeque<u64>>,
    /// Every log on chain.
    logs: Mutex<Vec<Value>>,
    /// Larger `eth_getLogs` ranges fail like sepolia.base.org (HTTP 413).
    max_span: u64,
    /// Ranges that fail once.
    fail_once: Mutex<Vec<(u64, u64)>>,
    /// Every `eth_getLogs` fails.
    down: AtomicBool,
    /// Answer each range twice over, newest first.
    repeat: bool,
    /// The contract view: (round, shard) -> (root, binding, finalizedAt,
    /// first block at which `getFinalizedRoot` returns it).
    views: Mutex<BTreeMap<(u64, Hash32), View>>,
    calls: Mutex<Vec<(String, Value)>>,
    view_calls: AtomicU64,
    /// `shardState` status per (round, shard); unset: Finalized (3) for a
    /// key with a view, None (0) otherwise.
    states: Mutex<BTreeMap<(u64, Hash32), u8>>,
    /// Answers with more logs fail like a transport body cap.
    max_logs: usize,
    /// The contract's code appears at this block (`eth_getCode`).
    code_from: u64,
    /// `eth_getCode` below this block fails (a node without old state).
    state_from: u64,
    /// The next this many `eth_getCode` calls fail like a rate limit.
    code_fails: AtomicU64,
    /// `eth_blockNumber` (the node's latest head).
    latest: u64,
    /// `eth_call`s about these (round, shard)s fail (a backend erroring the
    /// judging of its forged keys, IDX-R3-1).
    call_fails: Mutex<Vec<(u64, Hash32)>>,
    /// The next this many `eth_call`s about a (round, shard) fail.
    call_fails_next: Mutex<BTreeMap<(u64, Hash32), u32>>,
}

impl Fake {
    fn new(head: u64) -> Self {
        Fake {
            chain_id: CHAIN,
            heads: Mutex::new(VecDeque::from([head])),
            logs: Mutex::new(Vec::new()),
            max_span: 200,
            fail_once: Mutex::new(Vec::new()),
            down: AtomicBool::new(false),
            repeat: false,
            views: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
            view_calls: AtomicU64::new(0),
            states: Mutex::new(BTreeMap::new()),
            max_logs: usize::MAX,
            code_from: DEPLOY,
            state_from: 0,
            code_fails: AtomicU64::new(0),
            latest: u64::MAX,
            call_fails: Mutex::new(Vec::new()),
            call_fails_next: Mutex::new(BTreeMap::new()),
        }
    }

    fn end(&self, round: u64, shard: Hash32, status: u8) {
        self.states
            .lock()
            .expect("lock")
            .insert((round, shard), status);
    }

    /// `eth_call`s of `shardState`.
    fn state_calls(&self) -> usize {
        self.calls
            .lock()
            .expect("lock")
            .iter()
            .filter(|(m, p)| {
                m == "eth_call"
                    && p[0]["data"]
                        .as_str()
                        .is_some_and(|d| d.starts_with(&hx(&SHARD_STATE_SELECTOR)))
            })
            .count()
    }

    fn set_heads(&self, hs: &[u64]) {
        *self.heads.lock().expect("lock") = hs.iter().copied().collect();
    }

    fn emit(&self, round: u64, shard: Hash32, root: Hash32, binding: Hash32, block: u64, idx: u64) {
        self.logs
            .lock()
            .expect("lock")
            .push(log(&CONTRACT, round, shard, root, binding, block, idx));
    }

    /// The root is final: `shardState` says Finalized from now on (a status
    /// set before, by `end`, no longer holds).
    fn finalize(&self, round: u64, shard: Hash32, root: Hash32, binding: Hash32, from_block: u64) {
        self.states.lock().expect("lock").remove(&(round, shard));
        self.views
            .lock()
            .expect("lock")
            .insert((round, shard), (root, binding, from_block, from_block));
    }

    /// The (from, to) of every `eth_getLogs` call so far.
    fn ranges(&self) -> Vec<(u64, u64)> {
        let q = |v: &Value| {
            u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
                .unwrap_or(0)
        };
        self.calls
            .lock()
            .expect("lock")
            .iter()
            .filter(|(m, _)| m == "eth_getLogs")
            .map(|(_, p)| (q(&p[0]["fromBlock"]), q(&p[0]["toBlock"])))
            .collect()
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "eth_chainId" => Ok(json!(format!("0x{:x}", self.chain_id))),
            "eth_getBlockByNumber" if params[0] != "finalized" => {
                // A block's header, read before a block is read for the held
                // shards only (never above the finalized head).
                Ok(json!({"number": params[0].clone()}))
            }
            "eth_blockNumber" => {
                let head = self
                    .heads
                    .lock()
                    .expect("lock")
                    .back()
                    .copied()
                    .unwrap_or(0);
                Ok(json!(format!("0x{:x}", self.latest.min(head + 10))))
            }
            "eth_getCode" => {
                if self
                    .code_fails
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
                {
                    return Err("eth_getCode: node answered HTTP 429".into());
                }
                let b = hex_u64(&params[1], "block")?;
                if b < self.state_from {
                    return Err("eth_getCode: node error: missing trie node".into());
                }
                Ok(json!(if b >= self.code_from { "0x6080" } else { "0x" }))
            }
            "eth_getBlockByNumber" => {
                let mut h = self.heads.lock().expect("lock");
                let head = if h.len() > 1 {
                    h.pop_front()
                } else {
                    h.front().copied()
                };
                Ok(json!({"number": format!("0x{:x}", head.unwrap_or(0))}))
            }
            "eth_getLogs" => {
                let f = &params[0];
                assert_eq!(f["address"], hx(&CONTRACT));
                assert_eq!(f["topics"][0], json!(FINALIZED_TOPIC));
                // Only the held shards: [topic, null, [shard ids]].
                let only: Option<Vec<Value>> = match f["topics"].as_array().map(Vec::len) {
                    Some(1) => None,
                    Some(3) => {
                        assert!(f["topics"][1].is_null());
                        Some(f["topics"][2].as_array().cloned().expect("shard list"))
                    }
                    n => panic!("unexpected topics {n:?}"),
                };
                let q = |v: &Value| {
                    u64::from_str_radix(v.as_str().unwrap_or("").trim_start_matches("0x"), 16)
                        .map_err(|e| e.to_string())
                };
                let (a, b) = (q(&f["fromBlock"])?, q(&f["toBlock"])?);
                if self.down.load(Ordering::SeqCst) {
                    return Err("eth_getLogs: node answered HTTP 503".into());
                }
                if b - a + 1 > self.max_span {
                    return Err("eth_getLogs: node answered HTTP 413".into());
                }
                {
                    let mut fails = self.fail_once.lock().expect("lock");
                    if let Some(i) = fails.iter().position(|r| *r == (a, b)) {
                        fails.remove(i);
                        return Err("eth_getLogs: backend timeout".into());
                    }
                }
                let mut out: Vec<Value> = self
                    .logs
                    .lock()
                    .expect("lock")
                    .iter()
                    .filter(|l| {
                        let n = q(&l["blockNumber"]).unwrap_or(0);
                        a <= n
                            && n <= b
                            && only.as_ref().is_none_or(|o| o.contains(&l["topics"][2]))
                    })
                    .cloned()
                    .collect();
                if out.len() > self.max_logs {
                    return Err("eth_getLogs: node answer over 1048576 bytes".into());
                }
                if self.repeat {
                    let again = out.clone();
                    out.extend(again);
                    out.reverse();
                }
                Ok(json!(out))
            }
            "eth_call" => {
                let data = hex_bytes(&params[0]["data"], "data")?;
                assert_eq!(params[0]["to"], hx(&CONTRACT));
                let pin = hex_u64(&params[1], "pin")?;
                let mut rb = [0u8; 8];
                rb.copy_from_slice(&data[28..36]);
                let mut s = [0u8; 32];
                s.copy_from_slice(&data[36..68]);
                let k = (u64::from_be_bytes(rb), Hash32(s));
                if self.call_fails.lock().expect("lock").contains(&k) {
                    return Err("eth_call: backend timeout".into());
                }
                if let Some(n) = self.call_fails_next.lock().expect("lock").get_mut(&k) {
                    if *n > 0 {
                        *n -= 1;
                        return Err("eth_call: backend timeout".into());
                    }
                }
                if data[..4] == SHARD_STATE_SELECTOR {
                    let st = self.states.lock().expect("lock").get(&k).copied();
                    let viewed = self.views.lock().expect("lock").contains_key(&k);
                    let mut ret = vec![0u8; 160];
                    ret[31] = st.unwrap_or(if viewed { 3 } else { 0 });
                    return Ok(json!(hx(&ret)));
                }
                assert_eq!(data[..4], GET_FINALIZED_ROOT_SELECTOR);
                self.view_calls.fetch_add(1, Ordering::SeqCst);
                let mut ret = vec![0u8; 96];
                if let Some((root, binding, at, from)) = self.views.lock().expect("lock").get(&k) {
                    if pin >= *from {
                        ret[..32].copy_from_slice(&root.0);
                        ret[32..64].copy_from_slice(&binding.0);
                        ret[88..].copy_from_slice(&at.to_be_bytes());
                    }
                }
                Ok(json!(hx(&ret)))
            }
            m => Err(format!("unexpected {m}")),
        }
    }
}

impl JsonRpc for Fake {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        self.calls
            .lock()
            .expect("lock")
            .push((method.to_string(), params));
        Box::pin(async move { r })
    }
}

impl JsonRpc for Arc<Fake> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        self.as_ref().call(method, params)
    }
}

fn log(
    contract: &[u8; 20],
    round: u64,
    shard: Hash32,
    root: Hash32,
    binding: Hash32,
    block: u64,
    idx: u64,
) -> Value {
    let mut r = [0u8; 32];
    r[24..].copy_from_slice(&round.to_be_bytes());
    json!({
        "address": hx(contract),
        "topics": [FINALIZED_TOPIC, hx(&r), shard.to_string()],
        "data": format!("0x{}{}", hex::encode(root.0), hex::encode(binding.0)),
        "blockNumber": format!("0x{block:x}"),
        "logIndex": format!("0x{idx:x}"),
        "transactionHash": hx(&keccak256(&block.to_be_bytes()).0),
        "removed": false,
    })
}

fn row(subject: &str, value: &[u8]) -> Vec<u8> {
    borsh::to_vec(&OutputRow {
        key: subject.as_bytes().to_vec(),
        value: value.to_vec(),
    })
    .expect("row")
}

fn rights() -> DataRights {
    DataRights::public_chain(SourceId("btc.node".into()))
}

fn spec(tag: &str, label_set: Option<LabelSetRef>) -> ShardSpec {
    ShardSpec {
        recipe: None,
        runtime: None,
        label_set,
        source: SourceRef {
            source: SourceId("btc.node".into()),
            snapshot: Snapshot::BlockRange {
                chain: ChainId("btc.mainnet".into()),
                from_height: 1,
                to_height: 3,
                tip_hash: keccak256(tag.as_bytes()),
                content_hash: keccak256(b"content"),
            },
        },
        schema: keccak256(b"schema"),
        rights_hash: rights().hash(),
        params: tag.as_bytes().to_vec(),
        registry_root: keccak256(b"registry"),
    }
}

/// A held shard whose rows are in `cas`: (shard id, root, binding).
fn hold(
    cas: &MemCas,
    held: &mut Holdings,
    tag: &str,
    label_set: Option<LabelSetRef>,
) -> (Hash32, Hash32, Hash32) {
    let rows = vec![
        row(&format!("entity:{tag}-a"), b"1"),
        row(&format!("entity:{tag}-b"), b"2"),
    ];
    let root = cas.put_rows(&rows).expect("rows");
    let h = HeldShard::from_spec(&spec(tag, label_set), "test.flow", rights()).expect("held");
    let ids = (h.meta.shard, root, h.meta.binding.binding(root, 2));
    held.insert(h).expect("insert");
    ids
}

/// The defaults, except that a fresh chain is read in one poll: the jump
/// past the head two polls agree on (IDX-R5-5) is unbounded here and tested
/// with its default in `idx_r5_5_*`.
fn cfg() -> IndexerConfig {
    IndexerConfig {
        max_jump: MAX_MAX_JUMP,
        ..IndexerConfig::new(CHAIN, CONTRACT, DEPLOY)
    }
}

fn indexer(
    fake: &Arc<Fake>,
    cas: &Arc<MemCas>,
    held: Holdings,
    cfg: IndexerConfig,
    path: Option<PathBuf>,
) -> Indexer<Arc<Fake>> {
    Indexer::open(
        cfg,
        fake.clone(),
        held,
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        path,
    )
    .expect("open")
}

fn status<T: JsonRpc>(idx: &Indexer<T>, round: u64, shard: &Hash32) -> ShardStatus {
    idx.known()
        .into_iter()
        .find(|k| k.round == round && k.shard == *shard)
        .map(|k| k.status)
        .expect("known")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "onyx-indexer-{}-{name}-{}",
        std::process::id(),
        keccak256(name.as_bytes()).to_hex().get(..8).unwrap_or("x")
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

#[tokio::test]
async fn a_finalized_shard_is_found_in_capped_ranges_and_served() {
    let fake = Arc::new(Fake::new(DEPLOY + 1_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 777, 3);
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    // Configured above the node's cap: the first range fails and is split.
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig { span: 500, ..cfg() },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, shard)]);
    assert_eq!(r.scanned, Some((DEPLOY, DEPLOY + 1_000)));
    assert!(r.splits >= 1, "the 500-block range was split");
    let ranges = fake.ranges();
    assert!(
        ranges
            .iter()
            .all(|(a, b)| *b <= DEPLOY + 1_000 && *a >= DEPLOY),
        "never past finalized, never before the deploy"
    );
    // Every block of the span was read by a call that answered.
    let mut covered = DEPLOY;
    for (a, b) in ranges.iter().filter(|(a, b)| b - a < 200) {
        assert_eq!(*a, covered, "contiguous");
        covered = b + 1;
    }
    assert_eq!(covered, DEPLOY + 1_001);
    let meta = catalog
        .read()
        .expect("lock")
        .shard(1, &shard)
        .cloned()
        .expect("in the catalog");
    assert_eq!(meta.round, 1);
    assert_eq!(meta.leaf_count, Some(2));
    assert_eq!(
        status(&idx, 1, &shard),
        ShardStatus::Servable {
            root,
            finalized_at: DEPLOY + 800
        }
    );
    assert_eq!(idx.next_block(), DEPLOY + 1_001);
    let h = idx.indexer_health();
    assert_eq!(h["servable"], 1);
    assert_eq!(h["shards"][0]["status"], "servable");
}

#[tokio::test]
async fn a_failing_range_is_split_and_a_dead_node_ends_the_poll_where_it_stood() {
    let fake = Arc::new(Fake::new(DEPLOY + 399));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 50, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 60);
    fake.fail_once
        .lock()
        .expect("lock")
        .push((DEPLOY, DEPLOY + 199));
    let idx = indexer(&fake, &cas, held.clone(), cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.splits, 1);
    assert_eq!(r.admitted, vec![(1, shard)]);

    // A node that refuses every range: the poll fails after splitting down
    // to one block, nothing is marked read, and the next poll resumes.
    let fake = Arc::new(Fake::new(DEPLOY + 399));
    fake.emit(1, shard, root, binding, DEPLOY + 350, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 360);
    fake.down.store(true, Ordering::SeqCst);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    assert!(idx.poll(&catalog).await.is_err());
    assert_eq!(idx.next_block(), DEPLOY);
    assert!(idx.indexer_health()["lastError"].is_string());
    let ranges = fake.ranges();
    assert_eq!(
        ranges.last().map(|(a, b)| b - a),
        Some(0),
        "split down to one block"
    );
    fake.down.store(false, Ordering::SeqCst);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, shard)]);
    assert!(idx.indexer_health()["lastError"].is_null());
}

#[tokio::test]
async fn the_cursor_and_events_survive_a_restart() {
    let dir = scratch("resume");
    let path = dir.join("indexer.json");
    let fake = Arc::new(Fake::new(DEPLOY + 450));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 20);
    let idx = indexer(&fake, &cas, held.clone(), cfg(), Some(path.clone()));
    idx.poll(&RwLock::new(Catalog::new())).await.expect("poll");
    assert_eq!(idx.next_block(), DEPLOY + 451);
    drop(idx);

    // Restart: the cursor is where it was, the event is known before any
    // scan, and the shard is served again after one poll that reads only
    // the overlap and the new blocks.
    let fake2 = Arc::new(Fake::new(DEPLOY + 500));
    fake2.finalize(1, shard, root, binding, DEPLOY + 20);
    let idx = indexer(&fake2, &cas, held.clone(), cfg(), Some(path.clone()));
    assert_eq!(idx.next_block(), DEPLOY + 451);
    assert_eq!(idx.events().len(), 1);
    assert_eq!(status(&idx, 1, &shard), ShardStatus::Unchecked);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, shard)]);
    assert_eq!(
        fake2.ranges(),
        vec![(DEPLOY + 451 - DEFAULT_OVERLAP, DEPLOY + 500)]
    );
    assert_eq!(idx.next_block(), DEPLOY + 501);

    // A file written for another deployment is refused.
    for other in [
        IndexerConfig {
            contract: [0x77; 20],
            ..cfg()
        },
        IndexerConfig {
            chain_id: 31_337,
            ..cfg()
        },
        IndexerConfig {
            deploy_block: DEPLOY + 1,
            ..cfg()
        },
    ] {
        let e = Indexer::open(
            other,
            fake2.clone(),
            held.clone(),
            cas.clone() as Arc<dyn Cas + Send + Sync>,
            Some(path.clone()),
        )
        .err()
        .expect("refused");
        assert!(e.contains("another chain"), "{e}");
    }
    std::fs::write(&path, "{not json").expect("write");
    assert!(Indexer::open(
        cfg(),
        fake2.clone(),
        held,
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(path)
    )
    .is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_lagging_backend_never_moves_the_scan_backwards_or_past_its_head() {
    let fake = Arc::new(Fake::new(0));
    // Heads: 1500, then a lagging backend's 1300, then 1600.
    fake.set_heads(&[DEPLOY + 500, DEPLOY + 300, DEPLOY + 600]);
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 550, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 560);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll 1");
    assert_eq!(idx.next_block(), DEPLOY + 501);
    let before = fake.ranges().len();
    let r = idx.poll(&catalog).await.expect("poll 2 (lagging)");
    assert_eq!(r.head, DEPLOY + 300);
    assert_eq!(idx.next_block(), DEPLOY + 501, "never backwards");
    assert!(
        fake.ranges()[before..]
            .iter()
            .all(|(_, b)| *b <= DEPLOY + 300),
        "never past the head this answer gave"
    );
    assert!(r.admitted.is_empty());
    let r = idx.poll(&catalog).await.expect("poll 3");
    assert_eq!(r.admitted, vec![(1, shard)]);
    assert_eq!(idx.indexer_health()["finalizedHead"], DEPLOY + 600);
}

#[tokio::test]
async fn a_log_left_out_by_a_lagging_backend_is_found_by_the_overlap() {
    let fake = Arc::new(Fake::new(DEPLOY + 300));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.finalize(1, shard, root, binding, DEPLOY + 290);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    // The backend that answered had not seen this log yet.
    fake.emit(1, shard, root, binding, DEPLOY + 280, 1);
    fake.set_heads(&[DEPLOY + 320]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.new_events, 1);
    assert_eq!(r.admitted, vec![(1, shard)]);
}

#[tokio::test]
async fn repeated_and_out_of_order_logs_count_once_and_a_changed_one_is_a_candidate() {
    let mut f = Fake::new(DEPLOY + 150);
    f.repeat = true;
    let fake = Arc::new(f);
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, r2, b2) = hold(&cas, &mut held, "b", None);
    fake.emit(1, s1, r1, b1, DEPLOY + 40, 2);
    fake.emit(2, s2, r2, b2, DEPLOY + 40, 1);
    fake.emit(1, s1, r1, b1, DEPLOY + 41, 0);
    fake.finalize(1, s1, r1, b1, DEPLOY + 41);
    fake.finalize(2, s2, r2, b2, DEPLOY + 41);
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            overlap: 200,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.new_events, 3);
    let ev: Vec<(u64, u64)> = idx
        .events()
        .iter()
        .map(|e| (e.block, e.log_index))
        .collect();
    assert_eq!(
        ev,
        vec![(DEPLOY + 40, 1), (DEPLOY + 40, 2), (DEPLOY + 41, 0)],
        "ordered by position"
    );
    assert_eq!(r.admitted, vec![(1, s1), (2, s2)]);
    let known = idx.known();
    assert_eq!(
        known.iter().map(|k| k.events).collect::<Vec<_>>(),
        vec![2, 1]
    );
    // The overlap reads the same blocks again: nothing new.
    fake.set_heads(&[DEPLOY + 160]);
    assert_eq!(idx.poll(&catalog).await.expect("poll").new_events, 0);
    // A different log at a known position: kept as another candidate and
    // counted, never a stop (IDX-R1-1); the served shard stays served.
    {
        let mut logs = fake.logs.lock().expect("lock");
        logs[0] = log(&CONTRACT, 1, s1, keccak256(b"other"), b1, DEPLOY + 40, 2);
    }
    fake.set_heads(&[DEPLOY + 170]);
    let r = idx
        .poll(&catalog)
        .await
        .expect("a conflict is not an error");
    assert_eq!((r.conflicts, r.new_events), (1, 1));
    assert_eq!(idx.events().len(), 4);
    let h = idx.indexer_health();
    assert_eq!(
        (h["conflicts"].clone(), h["conflictingPositions"].clone()),
        (json!(1), json!(1))
    );
    assert!(h["lastConflict"]
        .as_str()
        .is_some_and(|c| c.contains("log 2")));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
}

#[tokio::test]
async fn an_event_whose_root_the_contract_does_not_hold_is_not_served() {
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    let fake_root = keccak256(b"made-up root");
    // The event names held rows' root, but the contract's final root is another.
    fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
    fake.finalize(1, shard, fake_root, binding, DEPLOY + 20);
    let idx = indexer(&fake, &cas, held.clone(), cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert!(r.admitted.is_empty());
    assert!(catalog.read().expect("lock").shard(1, &shard).is_none());
    // The contract's root decides (IDX-R2-1): the operator holds no rows
    // for it, so nothing is served.
    assert_eq!(
        status(&idx, 1, &shard),
        ShardStatus::Unservable(Refusal::RowsMissing)
    );

    // A dispute replaced a root: the first event's root was a cheater's, the
    // case finalized the true one. Only the contract's root is served, and
    // the old event is listed as superseded.
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    fake.emit(1, shard, fake_root, binding, DEPLOY + 10, 0);
    fake.emit(1, shard, root, binding, DEPLOY + 30, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 30);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, shard)]);
    let k = &idx.known()[0];
    assert_eq!((k.events, k.superseded), (2, 1));
}

#[tokio::test]
async fn held_data_that_does_not_hash_to_the_root_is_not_served() {
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    let other_root = keccak256(b"rows we do not hold");
    let other_binding = keccak256(b"another binding");
    for (event_root, event_binding, want) in [
        // Contract and event agree on a root whose rows the operator lacks.
        (other_root, binding, Refusal::RowsMissing),
        // The rows hash to the root, but spec and rows give another binding.
        (root, other_binding, Refusal::BindingMismatch),
    ] {
        let fake = Arc::new(Fake::new(DEPLOY + 100));
        fake.emit(1, shard, event_root, event_binding, DEPLOY + 10, 0);
        fake.finalize(1, shard, event_root, event_binding, DEPLOY + 20);
        let idx = indexer(&fake, &cas, held.clone(), cfg(), None);
        let catalog = RwLock::new(Catalog::new());
        assert!(idx.poll(&catalog).await.expect("poll").admitted.is_empty());
        assert!(catalog.read().expect("lock").shard(1, &shard).is_none());
        assert_eq!(status(&idx, 1, &shard), ShardStatus::Unservable(want));
    }
    // A stated row count that the held rows do not have.
    let mut wrong = held.get(&shard).cloned().expect("held");
    wrong.meta.leaf_count = Some(3);
    assert_eq!(
        check_held(&wrong, 1, &root, &binding, cas.as_ref(), false),
        Err(Refusal::LeafCountMismatch)
    );
    // Missing rows are found later: a refused shard is judged again each
    // poll, from the contract view read twice (one read is not kept,
    // IDX-R3-2), then cached.
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas2 = Arc::new(MemCas::new());
    let mut held2 = Holdings::new();
    let h = HeldShard::from_spec(&spec("late", None), "test.flow", rights()).expect("held");
    let late_rows = vec![row("entity:late-a", b"1"), row("entity:late-b", b"2")];
    let root2 = MerkleTree::from_rows(&late_rows).root();
    let (shard2, binding2) = (h.meta.shard, h.meta.binding.binding(root2, 2));
    held2.insert(h).expect("insert");
    fake.emit(4, shard2, root2, binding2, DEPLOY + 10, 0);
    fake.finalize(4, shard2, root2, binding2, DEPLOY + 20);
    let idx = indexer(&fake, &cas2, held2, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(
        status(&idx, 4, &shard2),
        ShardStatus::Unservable(Refusal::RowsMissing)
    );
    idx.poll(&catalog).await.expect("poll");
    cas2.put_rows(&late_rows).expect("rows");
    assert_eq!(
        idx.poll(&catalog).await.expect("poll").admitted,
        vec![(4, shard2)]
    );
    assert_eq!(
        fake.view_calls.load(Ordering::SeqCst),
        2,
        "a final view is read until a second read agrees, then cached"
    );
}

#[tokio::test]
async fn a_root_still_in_its_challenge_window_waits() {
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 90, 0);
    // getFinalizedRoot answers zero until block 1150 (window open).
    fake.finalize(1, shard, root, binding, DEPLOY + 150);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &shard), ShardStatus::Pending);
    assert!(catalog.read().expect("lock").shard(1, &shard).is_none());
    fake.set_heads(&[DEPLOY + 160]);
    assert_eq!(
        idx.poll(&catalog).await.expect("poll").admitted,
        vec![(1, shard)]
    );
}

#[tokio::test]
async fn a_shard_the_operator_does_not_hold_is_listed_never_served() {
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let unknown = keccak256(b"someone else's shard");
    fake.emit(9, unknown, keccak256(b"r"), keccak256(b"b"), DEPLOY + 5, 0);
    fake.finalize(9, unknown, keccak256(b"r"), keccak256(b"b"), DEPLOY + 6);
    let idx = indexer(&fake, &cas, Holdings::new(), cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    assert!(idx.poll(&catalog).await.expect("poll").admitted.is_empty());
    assert_eq!(status(&idx, 9, &unknown), ShardStatus::NotHeld);
    assert!(catalog.read().expect("lock").shard_keys().is_empty());
    assert_eq!(fake.view_calls.load(Ordering::SeqCst), 0);
    // Mutation L4: later polls do not read it again.
    let states = fake.state_calls();
    fake.set_heads(&[DEPLOY + 110]);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(
        fake.state_calls(),
        states,
        "a shard not held is not read again"
    );
    assert_eq!(fake.view_calls.load(Ordering::SeqCst), 0);
    let h = idx.indexer_health();
    assert_eq!(
        (h["notHeld"].clone(), h["servable"].clone()),
        (json!(1), json!(0))
    );
    assert_eq!(h["shards"][0]["status"], "not_held");
}

#[tokio::test]
async fn the_wrong_chain_fails_closed() {
    let mut f = Fake::new(DEPLOY + 100);
    f.chain_id = 8_453;
    let fake = Arc::new(f);
    let cas = Arc::new(MemCas::new());
    let idx = indexer(&fake, &cas, Holdings::new(), cfg(), None);
    let e = idx
        .poll(&RwLock::new(Catalog::new()))
        .await
        .expect_err("refused");
    assert!(e.contains("chain 8453"), "{e}");
    assert!(fake.ranges().is_empty());
    assert!(Indexer::open(
        IndexerConfig { span: 0, ..cfg() },
        fake.clone(),
        Holdings::new(),
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        None
    )
    .is_err());
}

#[test]
fn malformed_logs_are_refused() {
    let good = log(
        &CONTRACT,
        3,
        Hash32([7; 32]),
        Hash32([8; 32]),
        Hash32([9; 32]),
        50,
        4,
    );
    let l = parse_log(&good, &CONTRACT, 40, 60).expect("parses");
    assert_eq!(
        (l.round, l.shard, l.root, l.binding, l.block, l.log_index),
        (3, Hash32([7; 32]), Hash32([8; 32]), Hash32([9; 32]), 50, 4)
    );
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        f(&mut v);
        parse_log(&v, &CONTRACT, 40, 60).is_err()
    };
    assert!(
        bad(&|v| v["address"] = json!(hx(&[0x11; 20]))),
        "another contract"
    );
    assert!(
        bad(&|v| v["topics"][0] = json!(hx(&[0; 32]))),
        "another event"
    );
    assert!(
        bad(&|v| v["topics"][1] = json!(hx(&[0xff; 32]))),
        "round above u64"
    );
    assert!(
        bad(&|v| {
            if let Some(t) = v["topics"].as_array_mut() {
                t.pop();
            }
        }),
        "missing topic"
    );
    assert!(bad(&|v| v["data"] = json!("0x00")), "short data");
    assert!(
        !bad(&|v| v["blockNumber"] = json!("0x3c")),
        "the range end is inside"
    );
    assert!(
        bad(&|v| v["blockNumber"] = json!("0x3d")),
        "after the range"
    );
    assert!(
        bad(&|v| v["blockNumber"] = json!("0x27")),
        "before the range"
    );
    assert!(bad(&|v| v["removed"] = json!(true)), "removed");
    assert!(bad(&|v| v["logIndex"] = json!("4")), "no 0x");
}

fn label(addr: &str, evidence: Vec<EvidenceRef>) -> Label {
    Label {
        address: addr.into(),
        entity: "entity:example".into(),
        kind: EntityKind::Exchange,
        confidence: Confidence(900_000),
        evidence,
    }
}

/// The evidence half of `check_servable` (R-LBL-FIX-1): a pinned label set
/// must be held, every label must cite evidence, and every cited hash must
/// resolve in the operator's store; a `test.*` set only in dev mode.
#[test]
fn a_pinned_label_sets_evidence_must_resolve() {
    let cas = MemCas::new();
    let doc = cas
        .put(Kind::Blob, b"published reserve list")
        .expect("blob");
    let ev = |h: Hash32| {
        vec![EvidenceRef {
            kind: "por-list".into(),
            content_hash: h,
        }]
    };
    let set = |name: &str, labels: Vec<Label>| LabelSet {
        name: name.into(),
        version: 1,
        labels,
    };
    let reference = |s: &LabelSet| LabelSetRef {
        name: s.name.clone(),
        version: s.version,
        content_hash: s.content_hash(),
    };
    let check = |s: &LabelSet, store: bool, dev: bool| {
        if store {
            cas.put_label_set(s).expect("set");
        }
        let mut held = Holdings::new();
        let (_, root, binding) = hold(
            &cas,
            &mut held,
            &format!("{}{store}", s.content_hash()),
            Some(reference(s)),
        );
        let h = held
            .ids()
            .next()
            .and_then(|id| held.get(id))
            .cloned()
            .expect("held");
        check_held(&h, 1, &root, &binding, &cas, dev)
    };
    let good = set("onyx.entities.example", vec![label("t:a", ev(doc))]);
    assert!(check(&good, true, false).is_ok());
    assert_eq!(
        check(
            &set("onyx.entities.example2", vec![label("t:a", ev(doc))]),
            false,
            false
        ),
        Err(Refusal::LabelSetMissing)
    );
    let placeholder = set(
        "onyx.entities.example",
        vec![label("t:a", ev(keccak256(b"placeholder")))],
    );
    assert_eq!(
        check(&placeholder, true, false).map_err(|e| e.code()),
        Err("evidence_missing")
    );
    let bare = set("onyx.entities.example", vec![label("t:a", vec![])]);
    assert_eq!(
        check(&bare, true, false).map_err(|e| e.code()),
        Err("evidence_missing")
    );
    let att = set(
        "onyx.entities.example",
        vec![label(
            "t:a",
            vec![EvidenceRef {
                kind: onyx_labels::ATTESTATION_EVIDENCE.into(),
                content_hash: doc,
            }],
        )],
    );
    assert_eq!(
        check(&att, true, false).map_err(|e| e.code()),
        Err("evidence_missing"),
        "an attestation must be stored as one"
    );
    let test_set = set("test.onyx.entities.example", vec![label("t:a", vec![])]);
    assert_eq!(check(&test_set, false, false), Err(Refusal::TestLabels));
    assert!(
        check(&test_set, false, true).is_ok(),
        "dev mode serves a test set, as before"
    );
}

#[test]
fn holdings_refuse_a_shard_twice() {
    let mut held = Holdings::new();
    let h = HeldShard::from_spec(&spec("a", None), "test.flow", rights()).expect("held");
    held.insert(h.clone()).expect("first");
    assert!(held.insert(h).is_err());
    let mut other = rights();
    other.source = SourceId("other.source".into());
    assert!(
        HeldShard::from_spec(&spec("a", None), "test.flow", other).is_none(),
        "rights not the spec's"
    );
}

/// The API serves a shard only once the indexer has admitted it: before the
/// poll the proof route answers 404 (the shard is not in the catalog), after
/// it 200 with a proof, and `/v1/health` lists it.
#[tokio::test]
async fn the_api_serves_a_shard_only_after_the_indexer_admits_it() {
    use crate::roots::{FinalRoot, LedgerRoots};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "api", None);
    fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 20);
    let roots = LedgerRoots::new();
    roots.insert(
        1,
        shard,
        FinalRoot {
            root,
            finalized_at: DEPLOY + 20,
            finalized_unix: 1_700_000_000,
            binding: Some(binding),
            tx: None,
        },
    );
    let idx = Arc::new(indexer(&fake, &cas, held, cfg(), None));
    let state = Arc::new(
        crate::ApiState::new(
            Catalog::new(),
            Arc::new(roots),
            cas.clone() as Arc<dyn Cas + Send + Sync>,
            None,
            Arc::new(|| 1_700_000_100),
            "http://127.0.0.1",
            crate::ApiConfig::default(),
        )
        .expect("state")
        .with_indexer(idx.clone()),
    );
    let get = |uri: String| {
        let app = crate::router(state.clone());
        async move {
            let r = app
                .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
                .await
                .expect("response");
            let status = r.status().as_u16();
            let body = r.into_body().collect().await.expect("body").to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
            )
        }
    };
    let uri = format!("/v1/proofs/1/{shard}/0");
    assert_eq!(get(uri.clone()).await.0, 404, "not discovered yet");
    idx.poll(&state.catalog).await.expect("poll");
    let (status, body) = get(uri).await;
    assert_eq!(status, 200, "{body}");
    let (_, health) = get("/v1/health".into()).await;
    assert_eq!(health["indexer"]["servable"], 1);
    assert_eq!(health["indexer"]["shards"][0]["shard"], shard.to_string());
}

// ---- Review IDX-R1 (2026-10-12): regression tests from its PoCs ----

/// IDX-R1-1: one bad answer at a position the real chain also uses (a lying
/// or buggy backend) used to wedge the indexer for good, across restarts.
/// Now the honest log is kept beside the forged one, the conflict is
/// counted, the cursor moves, both shards are served by the contract's view,
/// and a restart (with both candidates in the state file) is clean.
#[tokio::test]
async fn idx_r1_1_a_conflicting_log_is_a_candidate_and_the_contract_decides() {
    let fake = Arc::new(Fake::new(DEPLOY + 150));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, r2, b2) = hold(&cas, &mut held, "b", None);
    let forged = keccak256(b"forged");
    fake.emit(1, s1, forged, b1, DEPLOY + 100, 0);
    let dir = scratch("idx-r1-1");
    let path = dir.join("idx.json");
    let idx = indexer(&fake, &cas, held.clone(), cfg(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("first poll");
    assert_eq!(idx.next_block(), DEPLOY + 151);
    assert_eq!(
        status(&idx, 1, &s1),
        ShardStatus::Ended(Ended::NotOnChain),
        "the contract never opened what the forged log names"
    );
    // Honest from now on.
    {
        let mut logs = fake.logs.lock().expect("lock");
        logs[0] = log(&CONTRACT, 1, s1, r1, b1, DEPLOY + 100, 0);
    }
    fake.finalize(1, s1, r1, b1, DEPLOY + 100);
    fake.emit(2, s2, r2, b2, DEPLOY + 400, 0);
    fake.finalize(2, s2, r2, b2, DEPLOY + 400);
    fake.set_heads(&[DEPLOY + 200]);
    let r = idx.poll(&catalog).await.expect("not wedged");
    assert_eq!(r.conflicts, 1);
    assert_eq!(
        r.admitted,
        vec![(1, s1)],
        "the contract names the honest root"
    );
    fake.set_heads(&[DEPLOY + 600]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(2, s2)]);
    assert_eq!(idx.next_block(), DEPLOY + 601, "the cursor moves");
    let k = idx
        .known()
        .into_iter()
        .find(|k| k.shard == s1)
        .expect("known");
    assert_eq!(
        (k.events, k.superseded),
        (2, 1),
        "the forged one is superseded"
    );
    assert_eq!(idx.indexer_health()["conflicts"], 1);
    drop(idx);
    // Restart: both candidates load, nothing to delete, both shards served.
    let idx = indexer(&fake, &cas, held, cfg(), Some(path));
    assert_eq!(idx.events().len(), 3);
    let catalog = RwLock::new(Catalog::new());
    fake.set_heads(&[DEPLOY + 650]);
    let r = idx.poll(&catalog).await.expect("clean after restart");
    assert_eq!(r.admitted, vec![(1, s1), (2, s2)]);
    assert_eq!(idx.indexer_health()["conflictingPositions"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R1-1: a position holds at most MAX_CANDIDATES_PER_LOG logs; more are
/// counted and dropped, and a state file with more is refused.
#[tokio::test]
async fn idx_r1_1_candidates_per_position_are_bounded() {
    let fake = Arc::new(Fake::new(DEPLOY + 10));
    let cas = Arc::new(MemCas::new());
    let dir = scratch("idx-r1-1-bound");
    let path = dir.join("idx.json");
    let idx = indexer(&fake, &cas, Holdings::new(), cfg(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    for n in 0..(MAX_CANDIDATES_PER_LOG as u64 + 2) {
        *fake.logs.lock().expect("lock") = vec![log(
            &CONTRACT,
            1,
            keccak256(b"s"),
            keccak256(&n.to_be_bytes()),
            keccak256(b"b"),
            DEPLOY + 5,
            0,
        )];
        fake.set_heads(&[DEPLOY + 10 + n]);
        idx.poll(&catalog).await.expect("poll");
    }
    assert_eq!(idx.events().len(), MAX_CANDIDATES_PER_LOG);
    assert_eq!(
        idx.indexer_health()["conflicts"],
        MAX_CANDIDATES_PER_LOG as u64 + 1
    );
    drop(idx);
    let text = std::fs::read_to_string(&path).expect("state");
    let mut v: Value = serde_json::from_str(&text).expect("json");
    let extra = {
        let mut e = v["events"][0].clone();
        e["root"] = json!(keccak256(b"one more").to_string());
        e
    };
    v["events"].as_array_mut().expect("events").push(extra);
    std::fs::write(&path, v.to_string()).expect("write");
    let e = Indexer::open(
        cfg(),
        fake.clone(),
        Holdings::new(),
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(path),
    )
    .err()
    .expect("refused");
    assert!(e.contains("more than"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R1-1: judging runs even when the scan fails, so a known shard whose
/// window closes while the node refuses every `eth_getLogs` is still served
/// (the poll still reports the error).
#[tokio::test]
async fn idx_r1_1_judging_runs_when_the_scan_fails() {
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 90, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 300);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &shard), ShardStatus::Pending);
    fake.down.store(true, Ordering::SeqCst);
    fake.set_heads(&[DEPLOY + 400]);
    let e = idx.poll(&catalog).await.expect_err("the scan failed");
    assert!(e.contains("eth_getLogs"), "{e}");
    assert!(catalog.read().expect("lock").shard(1, &shard).is_some());
    assert_eq!(
        idx.next_block(),
        DEPLOY + 101,
        "nothing read, nothing skipped"
    );
}

/// IDX-R1-2: one block with more logs than an answer may carry (here more
/// than MAX_LOGS_PER_RANGE) used to stop the scan for good. It is now read
/// for the held shards only, counted, and the scan goes on.
#[tokio::test]
async fn idx_r1_2_one_oversized_block_is_read_for_the_held_shards() {
    let fake = Arc::new(Fake::new(DEPLOY + 1_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, r2, b2) = hold(&cas, &mut held, "b", None);
    for i in 0..(MAX_LOGS_PER_RANGE as u64 + 1) {
        fake.emit(
            1,
            keccak256(&i.to_be_bytes()),
            keccak256(b"r"),
            keccak256(b"b"),
            DEPLOY + 50,
            i,
        );
    }
    // A held shard finalized in the crowded block.
    fake.emit(1, s1, r1, b1, DEPLOY + 50, MAX_LOGS_PER_RANGE as u64 + 1);
    fake.finalize(1, s1, r1, b1, DEPLOY + 50);
    fake.emit(2, s2, r2, b2, DEPLOY + 400, 0);
    fake.finalize(2, s2, r2, b2, DEPLOY + 400);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("not wedged");
    assert_eq!(r.partial_blocks, vec![DEPLOY + 50]);
    assert_eq!(r.admitted, vec![(1, s1), (2, s2)]);
    assert_eq!(idx.next_block(), DEPLOY + 1_001);
    assert_eq!(
        idx.events().len(),
        2,
        "only the held shards' logs of that block"
    );
    assert_eq!(idx.indexer_health()["partialBlocks"], 1);
}

/// IDX-R1-2: the same over a transport body cap (the node's answer for the
/// block is too large), and a node that is down is never taken for an
/// oversized block: its header read fails or its held-shard read fails, and
/// the cursor stays.
#[tokio::test]
async fn idx_r1_2_a_body_cap_is_survived_and_a_dead_node_moves_nothing() {
    let mut f = Fake::new(DEPLOY + 300);
    f.max_logs = 3;
    let fake = Arc::new(f);
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    for i in 0..4 {
        fake.emit(
            7,
            keccak256(&[i]),
            keccak256(b"r"),
            keccak256(b"b"),
            DEPLOY + 20,
            i.into(),
        );
    }
    fake.emit(1, shard, root, binding, DEPLOY + 20, 9);
    fake.finalize(1, shard, root, binding, DEPLOY + 20);
    let idx = indexer(&fake, &cas, held.clone(), cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.partial_blocks, vec![DEPLOY + 20]);
    assert_eq!(r.admitted, vec![(1, shard)]);
    // Down: the poll fails where it stood.
    let fake = Arc::new(Fake::new(DEPLOY + 300));
    fake.down.store(true, Ordering::SeqCst);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let e = idx.poll(&catalog).await.expect_err("down");
    assert!(e.contains("held shards only"), "{e}");
    assert_eq!(idx.next_block(), DEPLOY);
}

/// IDX-R1-3: a dispute that ended NoValidRoot (no new Finalized event) used
/// to leave the shard `pending` and cost an `eth_call` every poll. The
/// contract's `shardState` now marks it ended, and it is not polled again;
/// so are Unresolved, Voided and Empty.
#[tokio::test]
async fn idx_r1_3_a_case_ending_without_a_root_is_ended_and_not_polled() {
    for (status, want) in [
        (7u8, Ended::NoValidRoot),
        (8, Ended::Unresolved),
        (5, Ended::Voided),
        (4, Ended::Empty),
    ] {
        let fake = Arc::new(Fake::new(DEPLOY + 100));
        let cas = Arc::new(MemCas::new());
        let mut held = Holdings::new();
        let (shard, root, binding) = hold(&cas, &mut held, "a", None);
        // Finalized, then challenged inside its window: the case ended
        // without a root, so getFinalizedRoot answers zero for good.
        fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
        fake.end(1, shard, status);
        let idx = indexer(&fake, &cas, held, cfg(), None);
        let catalog = RwLock::new(Catalog::new());
        idx.poll(&catalog).await.expect("poll");
        assert_eq!(status_of(&idx, &shard), ShardStatus::Ended(want));
        assert_eq!(
            (fake.view_calls.load(Ordering::SeqCst), fake.state_calls()),
            (1, 1)
        );
        // One read is not final (IDX-R3-2): a second read on a later poll
        // confirms the end, and then it is not polled again.
        fake.set_heads(&[DEPLOY + 150]);
        idx.poll(&catalog).await.expect("poll");
        let (views, states) = (fake.view_calls.load(Ordering::SeqCst), fake.state_calls());
        assert_eq!((views, states), (2, 2));
        for h in [DEPLOY + 200, DEPLOY + 300] {
            fake.set_heads(&[h]);
            idx.poll(&catalog).await.expect("poll");
        }
        assert_eq!(
            (fake.view_calls.load(Ordering::SeqCst), fake.state_calls()),
            (views, states),
            "an ended shard costs no more calls"
        );
        let h = idx.indexer_health();
        assert_eq!(
            (h["ended"].clone(), h["pending"].clone()),
            (json!(1), json!(0))
        );
        assert_eq!(h["shards"][0]["status"], want.code());
        assert!(catalog.read().expect("lock").shard(1, &shard).is_none());
    }
    // Still in its window or in a case: pending, polled again.
    for status in [3u8, 6, 2, 1] {
        assert_eq!(Ended::from_status(status), Ok(None));
    }
    assert!(Ended::from_status(9).is_err());
    assert!(decode_shard_state(&[0u8; 96]).is_err());
    let mut big = [0u8; 160];
    big[30] = 1;
    assert!(decode_shard_state(&big).is_err(), "status above u8");
    // The pinned selector is the signature's.
    assert_eq!(
        keccak256(b"shardState(uint64,bytes32)").0[..4],
        SHARD_STATE_SELECTOR
    );
}

fn status_of(idx: &Indexer<Arc<Fake>>, shard: &Hash32) -> ShardStatus {
    status(idx, 1, shard)
}

/// IDX-R1-4: the deploy block must be where the contract's code appeared:
/// none at deployBlock - 1, some at deployBlock. A later block (events
/// missed), an earlier one, and a node without old state are refused.
#[tokio::test]
async fn idx_r1_4_the_deploy_block_is_checked_against_the_code() {
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let cas = Arc::new(MemCas::new());
    let open = |c: IndexerConfig, f: &Arc<Fake>| indexer(f, &cas, Holdings::new(), c, None);
    open(cfg(), &fake)
        .check_deploy_block_with(&FAST)
        .await
        .expect("the right block");
    let late = IndexerConfig {
        deploy_block: DEPLOY + 1,
        ..cfg()
    };
    let e = open(late, &fake)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("late");
    assert!(e.contains("after the deployment"), "{e}");
    let early = IndexerConfig {
        deploy_block: DEPLOY - 1,
        ..cfg()
    };
    let e = open(early, &fake)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("early");
    assert!(e.contains("before the deployment"), "{e}");
    let mut f = Fake::new(DEPLOY + 100);
    f.state_from = DEPLOY + 50;
    let pruned = Arc::new(f);
    let e = open(cfg(), &pruned)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("no old state");
    assert!(e.contains("archive node"), "{e}");
}

/// Attempts without waiting, for tests.
const FAST: [std::time::Duration; 3] = [std::time::Duration::ZERO; 3];

/// SOAK-4 (L6 soak): the deploy-block check needs state from `deployBlock`;
/// a node keeping only recent state (anvil after a few thousand blocks; a
/// pruned full node after 128) could not restart onyx-node once the
/// deployment was older than that (17 failed starts in a row in the soak).
/// A passed check is recorded in the bound state file, and a restart with
/// that file skips it, so it reads no old state; a file without the record
/// (written before) or for another deploy block is not trusted.
#[tokio::test]
async fn soak_4_a_restart_does_not_need_the_deploy_blocks_state_again() {
    let cas = Arc::new(MemCas::new());
    let path = scratch("soak4").join("indexer.json");
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    let first = indexer(&fake, &cas, Holdings::new(), cfg(), Some(path.clone()));
    assert!(!first.deploy_checked());
    first
        .check_deploy_block_with(&FAST)
        .await
        .expect("the right block");
    assert!(first.deploy_checked());
    drop(first);
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("json");
    assert_eq!(saved["deployChecked"], json!(true));
    // The node has since dropped the old state.
    let mut f = Fake::new(DEPLOY + 100);
    f.state_from = DEPLOY + 50;
    let pruned = Arc::new(f);
    indexer(&pruned, &cas, Holdings::new(), cfg(), Some(path.clone()))
        .check_deploy_block_with(&FAST)
        .await
        .expect("a restart with the recorded check");
    assert!(
        pruned
            .calls
            .lock()
            .expect("lock")
            .iter()
            .all(|(m, _)| m != "eth_getCode"),
        "no old state read"
    );
    // Review NB3: the head check still runs with a recorded check, so a
    // node on another network or still syncing is refused.
    let mut f = Fake::new(DEPLOY + 100);
    f.latest = DEPLOY - 1;
    let behind = Arc::new(f);
    let e = indexer(&behind, &cas, Holdings::new(), cfg(), Some(path.clone()))
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("head below deployBlock");
    assert!(e.contains("above the node's head"), "{e}");
    // A state file without the record is checked again (and refused here).
    let mut old = saved.clone();
    old.as_object_mut().expect("object").remove("deployChecked");
    std::fs::write(&path, old.to_string()).expect("write");
    let e = indexer(&pruned, &cas, Holdings::new(), cfg(), Some(path.clone()))
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("checked again");
    assert!(e.contains("archive node"), "{e}");
    // A file for another deploy block is refused at open, as before.
    std::fs::write(&path, saved.to_string()).expect("write");
    let other = IndexerConfig {
        deploy_block: DEPLOY - 1,
        ..cfg()
    };
    assert!(Indexer::open(
        other,
        pruned.clone(),
        Holdings::new(),
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(path)
    )
    .is_err());
}

/// IDX-R2-2: a rate limit or timeout at start is retried; a node failing
/// every attempt, a head below deployBlock, a node without old state and a
/// real mismatch each say so, and only the state case names an archive node.
#[tokio::test]
async fn idx_r2_2_the_deploy_check_retries_and_names_the_cause() {
    let cas = Arc::new(MemCas::new());
    let open = |f: &Arc<Fake>| indexer(f, &cas, Holdings::new(), cfg(), None);
    // Two 429s, then answers: passes.
    let f = Fake::new(DEPLOY + 100);
    f.code_fails.store(2, Ordering::SeqCst);
    let flaky = Arc::new(f);
    open(&flaky)
        .check_deploy_block_with(&FAST)
        .await
        .expect("retried past two rate limits");
    // Failing more times than it retries: a node error, not "archive".
    let f = Fake::new(DEPLOY + 100);
    f.code_fails.store(10, Ordering::SeqCst);
    let down = Arc::new(f);
    let e = open(&down)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("down");
    assert!(
        e.contains("failed 4 times") && e.contains("HTTP 429"),
        "{e}"
    );
    assert!(!e.contains("archive"), "{e}");
    // The node's head is below deployBlock: another network or syncing.
    let mut f = Fake::new(DEPLOY + 100);
    f.latest = DEPLOY - 1;
    let behind = Arc::new(f);
    let e = open(&behind)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("behind");
    assert!(e.contains("above the node's head"), "{e}");
    assert!(!e.contains("archive"), "{e}");
    // A pruned node is not retried and is named as such.
    let mut f = Fake::new(DEPLOY + 100);
    f.state_from = DEPLOY + 50;
    let pruned = Arc::new(f);
    let e = open(&pruned)
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("pruned");
    assert!(
        e.contains("does not keep state") && e.contains("archive node"),
        "{e}"
    );
    assert_eq!(
        pruned
            .calls
            .lock()
            .expect("lock")
            .iter()
            .filter(|(m, _)| m == "eth_getCode")
            .count(),
        1,
        "not retried"
    );
    // The real mismatch keeps its own message.
    let mut f = Fake::new(DEPLOY + 100);
    f.code_from = DEPLOY - 5;
    let e = open(&Arc::new(f))
        .check_deploy_block_with(&FAST)
        .await
        .expect_err("mismatch");
    assert!(e.contains("after the deployment"), "{e}");
    // The default waits are bounded (about six seconds in all).
    assert!(DEPLOY_CHECK_BACKOFF_MS.iter().sum::<u64>() <= 10_000);
}

/// IDX-R1-5: `/v1/health` is a cached snapshot (a request does not walk the
/// events), the state file is written once per poll however many ranges it
/// read, and `health_detail: false` shows counts only.
#[tokio::test]
async fn idx_r1_5_health_is_a_snapshot_saves_are_bounded_and_detail_can_be_hidden() {
    let fake = Arc::new(Fake::new(DEPLOY + 2_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 20);
    let dir = scratch("idx-r1-5");
    let idx = indexer(&fake, &cas, held.clone(), cfg(), Some(dir.join("idx.json")));
    assert_eq!(idx.indexer_health()["events"], 0, "a snapshot at open");
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert!(r.ranges > 10);
    let h = idx.indexer_health();
    assert_eq!(h["stateWrites"], 1, "one write for {} ranges", r.ranges);
    assert_eq!(h["shards"][0]["shard"], shard.to_string());
    // A poll that reads nothing new writes nothing.
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(idx.indexer_health()["stateWrites"], 1);
    // Summary: counts, no shard ids, no error text.
    let fake = Arc::new(Fake::new(DEPLOY + 100));
    fake.emit(1, shard, root, binding, DEPLOY + 10, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 20);
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            health_detail: false,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    fake.down.store(true, Ordering::SeqCst);
    fake.set_heads(&[DEPLOY + 300]);
    assert!(idx.poll(&catalog).await.is_err());
    let h = idx.indexer_health();
    assert_eq!(h["servable"], 1);
    assert_eq!(h["failing"], true);
    assert_eq!(h["detail"], false);
    for hidden in ["shards", "lastError", "lastConflict"] {
        assert!(h.get(hidden).is_none(), "{hidden} hidden");
    }
    assert!(!h.to_string().contains(&shard.to_string()[2..]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The kept logs at one (block, log index).
fn at<T: JsonRpc>(idx: &Indexer<T>, block: u64, log_index: u64) -> Vec<FinalizedLog> {
    idx.events()
        .into_iter()
        .filter(|e| (e.block, e.log_index) == (block, log_index))
        .collect()
}

/// IDX-R2-1 (the review's PoC, ported): a lying backend answers four forged
/// logs at the honest log's (block, log index), naming the held shard with
/// wrong roots, before any honest answer. The honest re-read used to be
/// dropped by the cap and the shard stayed `chain_disagrees` for good, also
/// after a restart. Now the forged logs name the (round, shard), the
/// contract's view decides its root, and the shard is served at once, after
/// the honest re-read and after a restart.
#[tokio::test]
async fn idx_r2_1_forged_logs_filling_a_position_cannot_keep_a_final_shard_out() {
    let fake = Arc::new(Fake::new(DEPLOY + 150));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    for n in 0..4u64 {
        fake.emit(1, s1, keccak256(&n.to_be_bytes()), b1, DEPLOY + 100, 0);
    }
    fake.finalize(1, s1, r1, b1, DEPLOY + 100);
    let dir = scratch("idx-r2-1-poc");
    let path = dir.join("idx.json");
    let idx = indexer(&fake, &cas, held.clone(), cfg(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, s1)], "the contract's root decides");
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    assert!(matches!(status(&idx, 1, &s1), ShardStatus::Servable { root, .. } if root == r1));
    // Honest from now on: the re-read adds nothing the judging needs.
    *fake.logs.lock().expect("lock") = vec![log(&CONTRACT, 1, s1, r1, b1, DEPLOY + 100, 0)];
    fake.set_heads(&[DEPLOY + 200]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.conflicts, r.dropped), (1, 1));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    let k = idx
        .known()
        .into_iter()
        .find(|k| k.shard == s1)
        .expect("known");
    assert_eq!(
        (k.events, k.superseded),
        (4, 4),
        "every forged root listed as superseded"
    );
    assert_eq!(idx.indexer_health()["candidatesDropped"], 1);
    drop(idx);
    let idx = indexer(&fake, &cas, held, cfg(), Some(path));
    let catalog = RwLock::new(Catalog::new());
    fake.set_heads(&[DEPLOY + 210]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, s1)], "served after a restart");
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    assert!(matches!(status(&idx, 1, &s1), ShardStatus::Servable { root, .. } if root == r1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R2-1: a sustained stream of forgeries at the honest log's position,
/// while the honest shard is still in its challenge window (not final, so
/// nothing about its root can be refuted yet). Each lying answer carries
/// fresh logs naming the held shard in rounds the contract never opened,
/// shards nobody holds, and the honest (round, shard) with wrong roots; the
/// lying and honest backends alternate, a lying one answers first and last,
/// and the node restarts in the middle. The position never holds more than
/// four, the honest (round, shard) is never forgotten once read, and the
/// shard is served when its root becomes final, long after the position left
/// the overlap.
#[tokio::test]
async fn idx_r2_1_a_sustained_forgery_stream_cannot_evict_the_honest_shard() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let honest = log(&CONTRACT, 1, s1, r1, b1, P, 0);
    fake.end(1, s1, 2); // open on chain: in its challenge window
    let dir = scratch("idx-r2-1-stream");
    let path = dir.join("idx.json");
    let mut idx = indexer(&fake, &cas, held.clone(), cfg(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    let mut fresh = 0u64;
    let mut forged = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|j| {
                fresh += 1;
                let x = keccak256(&fresh.to_be_bytes());
                match j % 3 {
                    // The held shard in a round the contract never opened.
                    0 => log(&CONTRACT, 1_000 + fresh, s1, x, b1, P, 0),
                    // A shard nobody holds.
                    1 => log(&CONTRACT, 1, x, x, x, P, 0),
                    // The honest (round, shard) with a wrong root.
                    _ => log(&CONTRACT, 1, s1, x, b1, P, 0),
                }
            })
            .collect()
    };
    let mut head = DEPLOY + 101;
    let mut honest_seen = false;
    for poll in 0..16u64 {
        let lying = poll % 2 == 0;
        *fake.logs.lock().expect("lock") = if lying {
            forged(9)
        } else {
            vec![honest.clone()]
        };
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
        honest_seen |= !lying;
        let kept = at(&idx, P, 0);
        assert!(
            kept.len() <= MAX_CANDIDATES_PER_LOG,
            "poll {poll}: {}",
            kept.len()
        );
        if honest_seen {
            assert!(
                kept.iter().any(|e| (e.round, e.shard) == (1, s1)),
                "poll {poll}: the honest (round, shard) is kept"
            );
            assert_eq!(status(&idx, 1, &s1), ShardStatus::Pending, "poll {poll}");
        }
        if poll == 7 {
            drop(idx);
            idx = indexer(&fake, &cas, held.clone(), cfg(), Some(path.clone()));
        }
        head += 10;
    }
    // A last lying answer, then the position leaves the overlap.
    *fake.logs.lock().expect("lock") = forged(9);
    head += 10;
    fake.set_heads(&[head]);
    idx.poll(&catalog).await.expect("poll");
    *fake.logs.lock().expect("lock") = Vec::new();
    head += 200;
    fake.set_heads(&[head]);
    idx.poll(&catalog).await.expect("poll");
    assert!(
        idx.next_block().saturating_sub(DEFAULT_OVERLAP) > P,
        "out of the overlap"
    );
    assert!(
        catalog.read().expect("lock").shard(1, &s1).is_none(),
        "not final yet"
    );
    // The root becomes final: served, from the contract's view.
    fake.finalize(1, s1, r1, b1, head);
    head += 10;
    fake.set_heads(&[head]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, s1)]);
    assert!(matches!(status(&idx, 1, &s1), ShardStatus::Servable { root, .. } if root == r1));
    let h = idx.indexer_health();
    assert!(
        h["candidatesEvicted"].as_u64().is_some_and(|n| n > 0),
        "{h}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R2-1, IDX-R4-1: what a full position refuses and evicts. A newcomer
/// for a (round, shard) already named or not held is refused; the last log
/// naming a held (round, shard) is never evicted on a scan's read, also once
/// two reads on two polls say the contract ended it (ADR-0033 Amendment 4,
/// guarantee 1): so four logs for held (round, shard)s named nowhere else
/// refuse a newcomer until a held-shard rescan (`idx_r4_*`).
#[tokio::test]
async fn idx_r2_1_a_full_position_evicts_only_unheld_or_repeated_keys() {
    const P: u64 = DEPLOY + 50;
    let fake = Arc::new(Fake::new(DEPLOY + 60));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let shards: Vec<(Hash32, Hash32, Hash32)> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|t| hold(&cas, &mut held, t, None))
        .collect();
    // Four genuine shards, open on chain (pending), all named at P.
    for (n, (s, r, b)) in shards[..4].iter().enumerate() {
        fake.emit(n as u64 + 1, *s, *r, *b, P, 0);
        fake.end(n as u64 + 1, *s, 2);
    }
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(at(&idx, P, 0).len(), 4);
    // A fifth held shard at P: every slot is the only log naming a live,
    // held (round, shard), so the newcomer is refused.
    let (s5, r5, b5) = shards[4];
    fake.emit(5, s5, r5, b5, P, 0);
    fake.end(5, s5, 2);
    fake.set_heads(&[DEPLOY + 70]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.dropped, r.evicted), (1, 0));
    // Shard 4's case ends without a root, read on two polls: confirmed, and
    // still not evicted (an end read through the node never costs a held
    // key its last log).
    fake.end(4, shards[3].0, 7);
    for head in [DEPLOY + 80, DEPLOY + 85, DEPLOY + 90] {
        fake.set_heads(&[head]);
        let r = idx.poll(&catalog).await.expect("poll");
        assert_eq!((r.dropped, r.evicted), (1, 0));
    }
    assert_eq!(
        status(&idx, 4, &shards[3].0),
        ShardStatus::Ended(Ended::NoValidRoot)
    );
    let kept: Vec<u64> = at(&idx, P, 0).iter().map(|e| e.round).collect();
    assert_eq!(kept, vec![1, 2, 3, 4]);
    // A newcomer naming an already-named (round, shard), or a shard not
    // held, is refused at a full position (and so is round 5's log, read
    // again). A log repeating a named key, or of a shard not held, is what
    // makes room: here a second log naming round 1 at another position.
    let mut logs = fake.logs.lock().expect("lock").clone();
    logs.push(log(
        &CONTRACT,
        1,
        shards[0].0,
        keccak256(b"x"),
        shards[0].2,
        P,
        0,
    ));
    logs.push(log(&CONTRACT, 9, keccak256(b"nobody"), r5, b5, P, 0));
    *fake.logs.lock().expect("lock") = logs.clone();
    fake.set_heads(&[DEPLOY + 100]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.dropped, r.evicted), (3, 0));
    assert_eq!(at(&idx, P, 0).len(), 4);
    logs.insert(
        0,
        log(
            &CONTRACT,
            1,
            shards[0].0,
            shards[0].1,
            shards[0].2,
            P + 1,
            0,
        ),
    );
    *fake.logs.lock().expect("lock") = logs;
    fake.set_heads(&[DEPLOY + 110]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.evicted, 1, "round 1 is named twice: one of its logs goes");
    let kept: Vec<u64> = at(&idx, P, 0).iter().map(|e| e.round).collect();
    assert_eq!(kept, vec![2, 3, 4, 5]);
}

/// IDX-R3-1 (the review's PoC B, ported): a newcomer refused at a full
/// position used to reset its (round, shard) from `not_on_chain` to
/// unchecked before the refusal. A lying backend filled P with four logs
/// naming the held shard in rounds never opened (judged `not_on_chain`), and
/// in every later poll named those four keys again at another full position
/// before the honest backend answered P. A refused newcomer now changes
/// nothing (the forged keys stay ended). Since IDX-R4-1 a confirmed end no
/// longer makes their slots evictable on a scan's read, so the honest log
/// waits while the backend lies, and the held-shard rescan after a restart
/// with honest answers puts it back (ADR-0033 Amendment 4).
#[tokio::test]
async fn idx_r3_1_a_refused_newcomer_cannot_reopen_refuted_slots() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    fake.finalize(1, s1, r1, b1, DEPLOY + 100);
    let honest = log(&CONTRACT, 1, s1, r1, b1, P, 0);
    let mut c = cfg();
    c.span = 1;
    let dir = scratch("idx-r3-1-poc-b");
    let path = dir.join("idx.json");
    let idx = indexer(&fake, &cas, held.clone(), c.clone(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    // Poll 0: a lying answer for P's range (the first read of P).
    let fake_rounds: Vec<u64> = (0..4).map(|n| 1_000 + n).collect();
    *fake.logs.lock().expect("lock") = fake_rounds
        .iter()
        .map(|r| log(&CONTRACT, *r, s1, keccak256(&r.to_be_bytes()), b1, P, 0))
        .collect();
    idx.poll(&catalog).await.expect("poll");
    for r in &fake_rounds {
        assert_eq!(status(&idx, *r, &s1), ShardStatus::Ended(Ended::NotOnChain));
    }
    // Later polls: a lying answer for an earlier range names the four keys
    // again at a full position, then the honest answer for P's range.
    let mut head = DEPLOY + 101;
    let mut fresh = 5_000u64;
    let mut honest_reads = 0;
    while idx.next_block().saturating_sub(DEFAULT_OVERLAP) <= P {
        head += 10;
        let from = idx.next_block().saturating_sub(DEFAULT_OVERLAP);
        assert!(from < P);
        let mut logs = Vec::new();
        for _ in 0..4 {
            fresh += 1;
            logs.push(log(
                &CONTRACT,
                fresh,
                s1,
                keccak256(&fresh.to_be_bytes()),
                b1,
                from,
                0,
            ));
        }
        for r in &fake_rounds {
            logs.push(log(&CONTRACT, *r, s1, keccak256(b"variant"), b1, from, 0));
        }
        logs.push(honest.clone());
        *fake.logs.lock().expect("lock") = logs;
        fake.set_heads(&[head]);
        let rep = idx.poll(&catalog).await.expect("poll");
        honest_reads += 1;
        assert_eq!(rep.evicted, 0, "an end evicts nothing on a scan");
        for r in &fake_rounds {
            assert_eq!(status(&idx, *r, &s1), ShardStatus::Ended(Ended::NotOnChain));
        }
    }
    assert!(
        honest_reads >= 9,
        "the honest backend read P {honest_reads} times"
    );
    assert!(at(&idx, P, 0).iter().all(|e| e.round != 1));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_none());
    // Honest from now on, and a restart: the held-shard rescan (third poll
    // after the start, once the forged keys' ends are confirmed again)
    // finds the honest log, evicts a forged log it did not return, and the
    // shard is served.
    *fake.logs.lock().expect("lock") = vec![honest.clone()];
    drop(idx);
    let idx = indexer(&fake, &cas, held, c, Some(path));
    let catalog = RwLock::new(Catalog::new());
    // Span 1: the rescan takes a few polls at its own budget (IDX-R5-4).
    let mut done = false;
    for _ in 0..=STARTUP_RESCAN_POLL + 4 {
        head += 10;
        fake.set_heads(&[head]);
        let r = idx.poll(&catalog).await.expect("poll");
        done |= r.rescan_done;
        if done {
            break;
        }
    }
    assert!(done, "the startup rescan ran");
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    assert!(
        catalog.read().expect("lock").shard(1, &s1).is_some(),
        "served after a restart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R3-1, IDX-R4-1: a backend that fails every `eth_call` about its
/// forged keys keeps them unchecked; four of them at P keep the honest log
/// out while it does (a failed judging no longer makes a slot evictable:
/// ADR-0033 Amendment 4). Once the node answers honestly, the forged keys
/// end on two polls and the periodic held-shard rescan puts the honest log
/// back, without a restart.
#[tokio::test]
async fn idx_r3_1_forged_keys_whose_judging_fails_wait_for_the_rescan() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    fake.finalize(1, s1, r1, b1, DEPLOY + 100);
    let fake_rounds: Vec<u64> = (0..4).map(|n| 2_000 + n).collect();
    for r in &fake_rounds {
        fake.call_fails.lock().expect("lock").push((*r, s1));
    }
    *fake.logs.lock().expect("lock") = fake_rounds
        .iter()
        .map(|r| log(&CONTRACT, *r, s1, keccak256(&r.to_be_bytes()), b1, P, 0))
        .collect();
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            rescan_every: 5,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    let e = idx.poll(&catalog).await.expect_err("judging fails");
    assert!(e.contains("timeout"), "{e}");
    for r in &fake_rounds {
        assert_eq!(status(&idx, *r, &s1), ShardStatus::Unchecked);
    }
    // The honest backend answers P; the forged keys' calls still fail: the
    // honest log is refused.
    *fake.logs.lock().expect("lock") = vec![log(&CONTRACT, 1, s1, r1, b1, P, 0)];
    fake.set_heads(&[DEPLOY + 111]);
    let e = idx.poll(&catalog).await.expect_err("judging fails");
    assert!(e.contains("timeout"), "{e}");
    assert!(at(&idx, P, 0).iter().all(|e| e.round != 1));
    // The node stops failing: the forged keys end (two polls), and the
    // periodic rescan (every 5 polls) evicts one for the honest log: served.
    fake.call_fails.lock().expect("lock").clear();
    let mut head = DEPLOY + 111;
    for _ in 0..8 {
        head += 10;
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
    }
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    assert_eq!(at(&idx, P, 0).len(), MAX_CANDIDATES_PER_LOG);
    assert!(idx.indexer_health()["rescans"]
        .as_u64()
        .is_some_and(|n| n >= 1));
}

/// IDX-R3-2 (the review's PoC A, ported): one lying `eth_call` answer was
/// terminal until a restart. (a) `shardState` answered Voided once for a
/// shard in its challenge window: marked ended, never judged again, though
/// the contract later finalized it. (b) `getFinalizedRoot` answered a wrong
/// final root once: cached, refused until a restart. Now an end and a final
/// view are kept only when a read on a later poll agrees: both are served.
#[tokio::test]
async fn idx_r3_2_one_lying_view_answer_is_never_terminal() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, r2, b2) = hold(&cas, &mut held, "b", None);
    fake.emit(1, s1, r1, b1, P, 0);
    fake.emit(1, s2, r2, b2, P, 1);
    fake.end(1, s1, 5); // the lie: Voided (truth: open, 2)
    fake.finalize(1, s2, keccak256(b"lie"), b2, DEPLOY); // the lie: a wrong root
    let idx = indexer(&fake, &cas, held.clone(), cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Ended(Ended::Voided));
    assert!(matches!(status(&idx, 1, &s2), ShardStatus::Unservable(_)));
    // Honest from now on: s1 finalizes, s2's true root is final.
    fake.states.lock().expect("lock").remove(&(1, s1));
    fake.finalize(1, s1, r1, b1, DEPLOY + 110);
    fake.finalize(1, s2, r2, b2, DEPLOY);
    let mut head = DEPLOY + 101;
    for _ in 0..3 {
        head += 10;
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
    }
    assert!(matches!(status(&idx, 1, &s1), ShardStatus::Servable { root, .. } if root == r1));
    assert!(matches!(status(&idx, 1, &s2), ShardStatus::Servable { root, .. } if root == r2));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    assert!(catalog.read().expect("lock").shard(1, &s2).is_some());
    // Mutation L4: a served shard is not read again on an ordinary poll.
    let (views, states) = (fake.view_calls.load(Ordering::SeqCst), fake.state_calls());
    head += 10;
    fake.set_heads(&[head]);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(
        (fake.view_calls.load(Ordering::SeqCst), fake.state_calls()),
        (views, states),
        "a served shard is not polled again"
    );
    // An end read twice on different polls is kept and not polled again.
    let (s3, _, _) = hold(&cas, &mut held, "c", None);
    fake.emit(1, s3, r1, b1, P + 1, 0);
    fake.end(1, s3, 7);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    for _ in 0..2 {
        head += 10;
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
    }
    let states = fake.state_calls();
    head += 10;
    fake.set_heads(&[head]);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &s3), ShardStatus::Ended(Ended::NoValidRoot));
    assert_eq!(fake.state_calls(), states, "a confirmed end is not polled");
}

/// IDX-R3 (found by the property test, seed 704 of a first draft): one
/// lying `shardState` answer (an end) for the kept honest key, then one
/// lying log answer with forged newcomers at its position. If one read
/// refuted a key for eviction, the newcomers evicted the honest log and the
/// shard was lost once the block left the overlap. One read now protects the
/// slot until a read on a later poll agrees.
#[tokio::test]
async fn idx_r3_one_lying_end_never_costs_a_held_key_its_slot() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, _, b2) = hold(&cas, &mut held, "b", None);
    let honest = log(&CONTRACT, 1, s1, r1, b1, P, 0);
    *fake.logs.lock().expect("lock") = vec![honest.clone()];
    fake.end(1, s1, 2); // open: in its challenge window
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Pending);
    // The lie: Voided, once.
    fake.end(1, s1, 5);
    fake.set_heads(&[DEPLOY + 111]);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Ended(Ended::Voided));
    // Honest state again; a lying log answer leaves the honest log out and
    // brings four forged held keys (rounds never opened) to P.
    fake.end(1, s1, 2);
    *fake.logs.lock().expect("lock") = (0..4u64)
        .map(|n| {
            log(
                &CONTRACT,
                3_000 + n,
                s2,
                keccak256(&n.to_be_bytes()),
                b2,
                P,
                0,
            )
        })
        .collect();
    fake.set_heads(&[DEPLOY + 121]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.evicted, r.dropped), (0, 1));
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Pending);
    // P leaves the overlap; the root becomes final: served.
    *fake.logs.lock().expect("lock") = Vec::new();
    fake.set_heads(&[DEPLOY + 400]);
    idx.poll(&catalog).await.expect("poll");
    fake.finalize(1, s1, r1, b1, DEPLOY + 400);
    fake.set_heads(&[DEPLOY + 410]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, s1)]);
}

/// IDX-R3: one failing `eth_call` per poll about the kept honest key, then
/// forged newcomers at its position. Without a retry the key stayed
/// unchecked on two polls, which makes a slot evictable, and the newcomers
/// evicted the honest log. A failed judging call is now sent once more, so
/// one failing answer never leaves a key unjudged.
#[tokio::test]
async fn idx_r3_1_one_failing_call_never_costs_a_held_key_its_slot() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let (s2, _, b2) = hold(&cas, &mut held, "b", None);
    *fake.logs.lock().expect("lock") = vec![log(&CONTRACT, 1, s1, r1, b1, P, 0)];
    fake.end(1, s1, 2); // open: in its challenge window
    fake.call_fails_next
        .lock()
        .expect("lock")
        .insert((1, s1), 1);
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    idx.poll(&catalog)
        .await
        .expect("a failed call is sent again");
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Pending);
    fake.call_fails_next
        .lock()
        .expect("lock")
        .insert((1, s1), 1);
    *fake.logs.lock().expect("lock") = (0..4u64)
        .map(|n| {
            log(
                &CONTRACT,
                4_000 + n,
                s2,
                keccak256(&n.to_be_bytes()),
                b2,
                P,
                0,
            )
        })
        .collect();
    fake.set_heads(&[DEPLOY + 111]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.evicted, r.dropped), (0, 1));
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Pending);
}

/// splitmix64, for the seeded property test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A load-balanced node with a lying backend (IDX-R3 property test). Each
/// answer comes from the liar with probability `lie_pct`%, except that no
/// question gets two lying answers in a row (a question: one `eth_call` of
/// one (round, shard), or the log reads): the guarantee is that no single
/// answer keeps a held shard out. A lying log answer leaves out the honest
/// log and carries up to six forged logs at P (the held shard in rounds never
/// opened, another held shard, a shard nobody holds, the honest key with a
/// wrong root) and names earlier forged keys again; a lying `eth_call` says
/// another status, not final, a wrong root, or fails.
struct Liar {
    honest: Arc<Fake>,
    rng: Mutex<Rng>,
    lie_pct: u64,
    lied_last: Mutex<BTreeMap<String, bool>>,
    p: u64,
    honest_log: Value,
    held: Vec<(Hash32, Hash32)>,
    forged: Mutex<Vec<Value>>,
    lies: AtomicU64,
}

impl Liar {
    fn lie(&self, question: String) -> bool {
        let mut last = self.lied_last.lock().expect("lock");
        let before = last.get(&question).copied().unwrap_or(false);
        let lie = !before && self.rng.lock().expect("lock").below(100) < self.lie_pct;
        last.insert(question, lie);
        if lie {
            self.lies.fetch_add(1, Ordering::SeqCst);
        }
        lie
    }

    fn roll(&self, n: u64) -> u64 {
        self.rng.lock().expect("lock").below(n)
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "eth_getLogs" if self.lie("logs".into()) => {
                if self.roll(4) == 0 {
                    return Err("eth_getLogs: backend timeout".into());
                }
                let q = |v: &Value| hex_u64(v, "block").unwrap_or(0);
                let (a, b) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                let honest = self.honest.answer(method, params)?;
                let mut out: Vec<Value> = honest
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|l| *l != self.honest_log)
                    .collect();
                let mut forged = self.forged.lock().expect("lock");
                if (a..=b).contains(&self.p) {
                    for _ in 0..=self.roll(6) {
                        let n = self.roll(1 << 40);
                        let x = keccak256(&n.to_be_bytes());
                        let (s, bd) = self.held[self.roll(self.held.len() as u64) as usize];
                        let l = match self.roll(4) {
                            0 | 1 => log(&CONTRACT, 1_000 + n, s, x, bd, self.p, 0),
                            2 => log(&CONTRACT, 1, x, x, x, self.p, 0),
                            _ => log(&CONTRACT, 1, self.held[0].0, x, bd, self.p, 0),
                        };
                        forged.push(l.clone());
                        out.push(l);
                    }
                }
                // Name earlier forged keys again, each at a block of its own
                // inside the range (and at P's position when it is in range).
                for (j, f) in forged.iter().rev().take(8).enumerate() {
                    let mut f = f.clone();
                    let blk = if (a..=b).contains(&self.p) && j % 2 == 0 {
                        self.p
                    } else {
                        a
                    };
                    f["blockNumber"] = json!(format!("0x{blk:x}"));
                    f["logIndex"] = json!(format!("0x{:x}", if blk == self.p { 0 } else { 1 + j }));
                    out.push(f);
                }
                Ok(json!(out))
            }
            "eth_call" => {
                let data = hex_bytes(&params[0]["data"], "data")?;
                let question = hx(&data);
                if !self.lie(question) {
                    return self.honest.answer(method, params);
                }
                match self.roll(4) {
                    0 => Err("eth_call: backend timeout".into()),
                    _ if data[..4] == SHARD_STATE_SELECTOR => {
                        let mut ret = vec![0u8; 160];
                        ret[31] = [0u8, 2, 4, 5, 7, 8][self.roll(6) as usize];
                        Ok(json!(hx(&ret)))
                    }
                    1 => Ok(json!(hx(&[0u8; 96]))),
                    _ => {
                        let mut ret = vec![0u8; 96];
                        let n = self.roll(u64::MAX);
                        ret[..32].copy_from_slice(&keccak256(&n.to_be_bytes()).0);
                        ret[32..64].copy_from_slice(&self.held[0].1 .0);
                        ret[88..].copy_from_slice(&(DEPLOY + 1).to_be_bytes());
                        Ok(json!(hx(&ret)))
                    }
                }
            }
            m => self.honest.answer(m, params),
        }
    }
}

impl JsonRpc for Arc<Liar> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        Box::pin(async move { r })
    }
}

/// IDX-R3 property: over many seeded runs, a lying backend answers up to a
/// third of the calls (never twice in a row to one question) with forged
/// logs at the honest log's position, other statuses, wrong or missing
/// roots and errors, from the first read of that block on, across a
/// restart; the held shard, final at a random block, is always served.
#[tokio::test]
async fn idx_r3_property_no_single_answer_keeps_a_held_final_shard_out() {
    const P: u64 = DEPLOY + 100;
    const SEEDS: u64 = 400;
    let mut lies = 0;
    for seed in 0..SEEDS {
        let fake = Arc::new(Fake::new(P));
        let cas = Arc::new(MemCas::new());
        let mut held = Holdings::new();
        let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
        let (s2, _, b2) = hold(&cas, &mut held, "b", None);
        let honest_log = log(&CONTRACT, 1, s1, r1, b1, P, 0);
        fake.logs.lock().expect("lock").push(honest_log.clone());
        fake.end(1, s1, 2); // open: in its challenge window
        let mut rng = Rng(seed);
        let finalized_at = P + rng.below(300);
        let liar = Arc::new(Liar {
            honest: fake.clone(),
            rng: Mutex::new(Rng(seed ^ 0x5eed)),
            lie_pct: 20 + rng.below(14),
            lied_last: Mutex::new(BTreeMap::new()),
            p: P,
            honest_log,
            held: vec![(s1, b1), (s2, b2)],
            forged: Mutex::new(Vec::new()),
            lies: AtomicU64::new(0),
        });
        let dir = scratch(&format!("idx-r3-prop-{seed}"));
        let path = dir.join("idx.json");
        let open = |liar: &Arc<Liar>| {
            Indexer::open(
                cfg(),
                liar.clone(),
                held.clone(),
                cas.clone() as Arc<dyn Cas + Send + Sync>,
                Some(path.clone()),
            )
            .expect("open")
        };
        let mut idx = open(&liar);
        let mut catalog = RwLock::new(Catalog::new());
        let mut head = P;
        let restart_at = 3 + rng.below(20);
        for poll in 0..40u64 {
            if head >= finalized_at {
                fake.states.lock().expect("lock").remove(&(1, s1));
                fake.finalize(1, s1, r1, b1, finalized_at);
            }
            fake.set_heads(&[head]);
            let _ = idx.poll(&catalog).await;
            if poll == restart_at {
                drop(idx);
                idx = open(&liar);
                catalog = RwLock::new(Catalog::new());
            }
            head += 5 + rng.below(36);
        }
        assert!(
            catalog.read().expect("lock").shard(1, &s1).is_some(),
            "seed {seed}: the held final shard is served; known {:?}",
            idx.known()
                .iter()
                .filter(|k| k.shard == s1)
                .collect::<Vec<_>>()
        );
        lies += liar.lies.load(Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(lies > SEEDS * 20, "the liar lied {lies} times");
}

/// A node whose lies are scripted (IDX-R4 PoC, ported).
struct R4Script {
    honest: Arc<Fake>,
    /// `shardState` about the honest key answers Voided.
    lie_state: AtomicBool,
    /// `eth_getLogs` answers leave the honest log out.
    withhold: AtomicBool,
    /// `eth_getLogs` answers covering P also carry these.
    forge: Mutex<Vec<Value>>,
    p: u64,
    honest_log: Value,
    key_state_call: String,
}

impl R4Script {
    fn new(honest: &Arc<Fake>, p: u64, honest_log: Value, key: Key) -> Arc<Self> {
        Arc::new(R4Script {
            honest: honest.clone(),
            lie_state: AtomicBool::new(false),
            withhold: AtomicBool::new(false),
            forge: Mutex::new(Vec::new()),
            p,
            honest_log,
            key_state_call: hx(&shard_state_calldata(key.0, &key.1)),
        })
    }

    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        if method == "eth_call"
            && self.lie_state.load(Ordering::SeqCst)
            && params[0]["data"] == json!(self.key_state_call)
        {
            let mut ret = vec![0u8; 160];
            ret[31] = 5; // Voided
            return Ok(json!(hx(&ret)));
        }
        let mut v = self.honest.answer(method, params)?;
        if method == "eth_getLogs" {
            let arr = v.as_array_mut().ok_or("not a list")?;
            if self.withhold.load(Ordering::SeqCst) {
                arr.retain(|l| *l != self.honest_log);
            }
            let q = |x: &Value| hex_u64(x, "block").unwrap_or(0);
            let (a, b) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
            if (a..=b).contains(&self.p) {
                arr.extend(self.forge.lock().expect("lock").iter().cloned());
            }
        }
        Ok(v)
    }
}

impl JsonRpc for Arc<R4Script> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        Box::pin(async move { r })
    }
}

/// Four logs at `p` naming held shard `s` in rounds never opened.
fn forged_rounds(s: Hash32, binding: Hash32, p: u64) -> Vec<Value> {
    (0..4u64)
        .map(|n| {
            log(
                &CONTRACT,
                1_001 + n,
                s,
                keccak256(&n.to_be_bytes()),
                binding,
                p,
                0,
            )
        })
        .collect()
}

/// IDX-R4-1 (the review's PoC, ported). Two lying `shardState` answers on
/// two polls (one backend of three that always lies: 1 in 9 per pair of
/// polls) confirmed a false end for the honest held key in its challenge
/// window; that made its log evictable, and four forged logs naming the held
/// shard in rounds never opened evicted it. Once P left the overlap the
/// saved events no longer held it: the shard, final later, was never served,
/// also after a restart. Now (ADR-0033 Amendment 4) an end never makes a
/// held key's last log evictable (guarantee 1), and the periodic held-shard
/// rescan reads every confirmed end again (guarantee 2): served without a
/// restart once the node answers honestly, and after one.
#[tokio::test]
async fn idx_r4_1_two_lying_ends_cost_no_log_and_the_rescan_reads_them_again() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(P + 3));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let honest = log(&CONTRACT, 1, s1, r1, b1, P, 0);
    fake.logs.lock().expect("lock").push(honest.clone());
    fake.end(1, s1, 2); // in its challenge window
    let rpc = R4Script::new(&fake, P, honest, (1, s1));
    let dir = scratch("idx-r4-1-poc");
    let path = dir.join("idx.json");
    let c = IndexerConfig {
        rescan_every: 30,
        ..cfg()
    };
    let open = || {
        Indexer::open(
            c.clone(),
            rpc.clone(),
            held.clone(),
            cas.clone() as Arc<dyn Cas + Send + Sync>,
            Some(path.clone()),
        )
        .expect("open")
    };
    let idx = open();
    let catalog = RwLock::new(Catalog::new());
    // Polls 0 and 1: the liar answers the shardState call each time.
    rpc.lie_state.store(true, Ordering::SeqCst);
    let mut head = P + 3;
    for _ in 0..2 {
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
        head += 7;
    }
    assert_eq!(status(&idx, 1, &s1), ShardStatus::Ended(Ended::Voided));
    rpc.lie_state.store(false, Ordering::SeqCst);
    // The liar's answer to a scan covering P carries four forged logs naming
    // the held shard in rounds never opened: the confirmed end evicts
    // nothing, the fourth is refused.
    *rpc.forge.lock().expect("lock") = forged_rounds(s1, b1, P);
    fake.set_heads(&[head]);
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!((r.evicted, r.dropped), (0, 1));
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    rpc.forge.lock().expect("lock").clear();
    // Honest from now on; the root is final. The confirmed end is not
    // polled, until the periodic rescan reads it again: served.
    fake.finalize(1, s1, r1, b1, head);
    let mut served_at = None;
    for n in 0..40 {
        head += 7;
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
        if served_at.is_none() && catalog.read().expect("lock").shard(1, &s1).is_some() {
            served_at = Some(n);
        }
    }
    assert!(served_at.is_some(), "served after the periodic rescan");
    assert!(idx.indexer_health()["rescans"]
        .as_u64()
        .is_some_and(|n| n >= 1));
    // A restart: still served.
    drop(idx);
    let idx = open();
    let catalog = RwLock::new(Catalog::new());
    head += 7;
    fake.set_heads(&[head]);
    idx.poll(&catalog).await.expect("poll");
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R4-1: a node that leaves the honest log out of every read while its
/// block is in the overlap, and fills its position with four forged logs
/// naming the held shard in rounds never opened (the completeness residual
/// of IDX-R2-1). Nothing a scan reads brings it back; a restart with an
/// honest node does: the held-shard rescan finds the log, evicts a forged
/// log it did not return whose end is confirmed, and the shard is served.
#[tokio::test]
async fn idx_r4_1_a_log_withheld_in_the_overlap_is_found_by_the_rescan_after_a_restart() {
    const P: u64 = DEPLOY + 100;
    let fake = Arc::new(Fake::new(P + 3));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    let honest = log(&CONTRACT, 1, s1, r1, b1, P, 0);
    fake.logs.lock().expect("lock").push(honest.clone());
    fake.finalize(1, s1, r1, b1, P + 50);
    let rpc = R4Script::new(&fake, P, honest, (1, s1));
    rpc.withhold.store(true, Ordering::SeqCst);
    *rpc.forge.lock().expect("lock") = forged_rounds(s1, b1, P);
    let dir = scratch("idx-r4-1-withheld");
    let path = dir.join("idx.json");
    let open = || {
        Indexer::open(
            cfg(),
            rpc.clone(),
            held.clone(),
            cas.clone() as Arc<dyn Cas + Send + Sync>,
            Some(path.clone()),
        )
        .expect("open")
    };
    let idx = open();
    let catalog = RwLock::new(Catalog::new());
    let mut head = P + 3;
    while idx.next_block().saturating_sub(DEFAULT_OVERLAP) <= P + 10 {
        fake.set_heads(&[head]);
        idx.poll(&catalog).await.expect("poll");
        head += 10;
    }
    assert!(idx.known().iter().all(|k| k.round != 1));
    assert_eq!(at(&idx, P, 0).len(), 4);
    assert!(catalog.read().expect("lock").shard(1, &s1).is_none());
    // An honest node and a restart.
    rpc.withhold.store(false, Ordering::SeqCst);
    rpc.forge.lock().expect("lock").clear();
    drop(idx);
    let idx = open();
    let catalog = RwLock::new(Catalog::new());
    let mut evicted = 0;
    for _ in 0..=STARTUP_RESCAN_POLL {
        fake.set_heads(&[head]);
        evicted += idx.poll(&catalog).await.expect("poll").evicted;
        head += 10;
    }
    assert_eq!(evicted, 1);
    assert!(at(&idx, P, 0).iter().any(|e| (e.round, e.shard) == (1, s1)));
    assert!(catalog.read().expect("lock").shard(1, &s1).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The IDX-R3 liar, relaxed (review IDX-R4): `twice_pct`% of the time a
/// question may get a second lie in a row; with `one_of_three`, one backend
/// of three always lies (each call routed at random, lies in a row allowed).
/// `honest_now` turns every answer honest.
struct R4Liar {
    inner: Liar,
    one_of_three: bool,
    twice_pct: u64,
    honest_now: AtomicBool,
}

impl R4Liar {
    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        if self.honest_now.load(Ordering::SeqCst) {
            return self.inner.honest.answer(method, params);
        }
        let question = match method {
            "eth_getLogs" => Some("logs".to_string()),
            "eth_call" => hex_bytes(&params[0]["data"], "data").ok().map(|d| hx(&d)),
            _ => None,
        };
        if let Some(q) = question {
            if self.one_of_three || self.inner.roll(100) < self.twice_pct {
                self.inner.lied_last.lock().expect("lock").insert(q, false);
            }
        }
        self.inner.answer(method, params)
    }
}

impl JsonRpc for Arc<R4Liar> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        Box::pin(async move { r })
    }
}

/// Fixed liar-phase shape: (blocks per poll, finalized at P + this, polls,
/// restart after this poll).
type Shape = (u64, u64, u64, u64);

/// IDX-R4 recovery property: a liar phase (a restart in it), then honest
/// answers. Returns the seeds whose held final shard was not served (a) after
/// the periodic rescan, without a restart, or (b) after a restart from the
/// state file the liar phase left.
async fn r4_recovery(
    tag: &str,
    seeds: u64,
    one_of_three: bool,
    twice_pct: u64,
    shape: Option<Shape>,
) -> Vec<String> {
    const P: u64 = DEPLOY + 100;
    const EVERY: u64 = 20;
    let mut fails = Vec::new();
    for seed in 0..seeds {
        let fake = Arc::new(Fake::new(P));
        let cas = Arc::new(MemCas::new());
        let mut held = Holdings::new();
        let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
        let (s2, _, b2) = hold(&cas, &mut held, "b", None);
        let honest_log = log(&CONTRACT, 1, s1, r1, b1, P, 0);
        fake.logs.lock().expect("lock").push(honest_log.clone());
        fake.end(1, s1, 2);
        let mut rng = Rng(seed);
        let (step, fin, polls, restart_at) = shape.unwrap_or_else(|| {
            let fin = rng.below(300);
            (0, fin, 40, 3 + rng.below(20))
        });
        let liar = Arc::new(R4Liar {
            inner: Liar {
                honest: fake.clone(),
                rng: Mutex::new(Rng(seed ^ 0x5eed)),
                lie_pct: if one_of_three { 33 } else { 20 + rng.below(14) },
                lied_last: Mutex::new(BTreeMap::new()),
                p: P,
                honest_log,
                held: vec![(s1, b1), (s2, b2)],
                forged: Mutex::new(Vec::new()),
                lies: AtomicU64::new(0),
            },
            one_of_three,
            twice_pct,
            honest_now: AtomicBool::new(false),
        });
        let dir = scratch(&format!("idx-r4-{tag}-{seed}-{one_of_three}-{twice_pct}"));
        let path = dir.join("idx.json");
        let c = IndexerConfig {
            rescan_every: EVERY,
            ..cfg()
        };
        let open = |p: &PathBuf| {
            Indexer::open(
                c.clone(),
                liar.clone(),
                held.clone(),
                cas.clone() as Arc<dyn Cas + Send + Sync>,
                Some(p.clone()),
            )
            .expect("open")
        };
        let mut idx = open(&path);
        let mut catalog = RwLock::new(Catalog::new());
        let mut head = P;
        let finalize = |head: u64| {
            if head >= P + fin {
                fake.finalize(1, s1, r1, b1, P + fin);
            }
        };
        for poll in 0..polls {
            finalize(head);
            fake.set_heads(&[head]);
            let _ = idx.poll(&catalog).await;
            if poll == restart_at {
                drop(idx);
                idx = open(&path);
                catalog = RwLock::new(Catalog::new());
            }
            head += if step > 0 { step } else { 5 + rng.below(36) };
        }
        // The node answers honestly from now on; the root is final.
        liar.honest_now.store(true, Ordering::SeqCst);
        head = head.max(P + fin);
        finalize(head);
        let snapshot = dir.join("snapshot.json");
        std::fs::copy(&path, &snapshot).expect("snapshot");
        // (a) No restart: the periodic rescan (and the judging) recovers.
        for _ in 0..(2 * EVERY + 5) {
            head += 7;
            fake.set_heads(&[head]);
            let _ = idx.poll(&catalog).await;
        }
        if catalog.read().expect("lock").shard(1, &s1).is_none() {
            fails.push(format!("seed {seed} periodic: {:?}", idx.known()));
        }
        drop(idx);
        // (b) A restart from the state file the liar phase left.
        let idx = open(&snapshot);
        let catalog = RwLock::new(Catalog::new());
        for _ in 0..(STARTUP_RESCAN_POLL + 3) {
            head += 7;
            fake.set_heads(&[head]);
            let _ = idx.poll(&catalog).await;
        }
        if catalog.read().expect("lock").shard(1, &s1).is_none() {
            fails.push(format!("seed {seed} restart: {:?}", idx.known()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    fails
}

/// Seeds per run: `default`, or `ONYX_IDX_R4_SEEDS` for a one-off longer run.
fn r4_seeds(default: u64) -> u64 {
    std::env::var("ONYX_IDX_R4_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// IDX-R4-1 (the review's property PoC, its run shape: 7 blocks per poll,
/// final at P + 600, 120 polls, a restart after poll 60): one backend of
/// three always lies (lies in a row allowed). Liveness while it lies is not
/// promised (ADR-0033 Amendment 4: the operator's node is trusted for the
/// operator's liveness); once the node answers honestly, the held final
/// shard is served after the periodic rescan, and after a restart, for
/// every seed: never lost for good.
#[tokio::test]
async fn idx_r4_1_one_of_three_lying_recovers_after_a_rescan() {
    let fails = r4_recovery("poc", r4_seeds(60), true, 0, Some((7, 600, 120, 60))).await;
    assert!(
        fails.is_empty(),
        "{} failed: {:?}",
        fails.len(),
        fails.first()
    );
}

/// IDX-R4: the review's relaxed liar models (a question may get two lies in
/// a row 0%, 10% or 30% of the time, or one backend of three always lies),
/// with the IDX-R3 property test's random shapes: recovery after a rescan
/// once the node answers honestly, never liveness during the lies.
#[tokio::test]
async fn idx_r4_relaxed_liars_recover_after_a_rescan() {
    for (one, twice) in [(false, 0u64), (false, 10), (false, 30), (true, 0)] {
        let fails = r4_recovery("relaxed", r4_seeds(40), one, twice, None).await;
        assert!(
            fails.is_empty(),
            "model one_of_three={one} twice_pct={twice}: {} failed: {:?}",
            fails.len(),
            fails.first()
        );
    }
}

/// IDX-R4-2 (review IDX-R4 note, pre-existing): one lying `getFinalizedRoot`
/// answer naming the held rows' true root for a round the contract never
/// opened listed that round in the catalog, so forged rounds could crowd a
/// metric's [`crate::MAX_METRIC_CANDIDATES`]. A (round, shard) is now listed
/// only when `shardState` says Finalized, or two reads on two polls agree.
#[tokio::test]
async fn idx_r4_2_one_lying_view_never_lists_a_forged_round() {
    let fake = Arc::new(Fake::new(DEPLOY + 101));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (s1, r1, b1) = hold(&cas, &mut held, "a", None);
    fake.emit(1, s1, r1, b1, DEPLOY + 10, 0);
    fake.finalize(1, s1, r1, b1, DEPLOY + 10);
    // The lie: round 7 of the held shard is "final" with the held root.
    fake.emit(7, s1, r1, b1, DEPLOY + 20, 0);
    fake.views
        .lock()
        .expect("lock")
        .insert((7, s1), (r1, b1, DEPLOY + 20, DEPLOY));
    fake.end(7, s1, 0); // the contract never opened it
    let idx = indexer(&fake, &cas, held, cfg(), None);
    let catalog = RwLock::new(Catalog::new());
    let r = idx.poll(&catalog).await.expect("poll");
    assert_eq!(
        r.admitted,
        vec![(1, s1)],
        "a Finalized shardState admits at once"
    );
    assert!(catalog.read().expect("lock").shard(7, &s1).is_none());
    assert_eq!(status(&idx, 7, &s1), ShardStatus::Pending);
}

/// An honest node (like geth) with no state above its head, wrapping
/// [`Fake`]: the `lie_on`th `finalized` answer (0-based) lies 1,000,000
/// blocks ahead; every other answer is honest (review IDX-R5, PoC 1).
struct R5Pin {
    inner: Arc<Fake>,
    real: u64,
    lie_on: u64,
    asked: AtomicU64,
}

impl R5Pin {
    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "eth_getBlockByNumber" if params[0] == "finalized" => {
                if self.asked.fetch_add(1, Ordering::SeqCst) == self.lie_on {
                    return Ok(json!({"number": format!("0x{:x}", self.real + 1_000_000)}));
                }
                Ok(json!({"number": format!("0x{:x}", self.real)}))
            }
            "eth_getBlockByNumber" => {
                if hex_u64(&params[0], "n")? > self.real {
                    return Ok(Value::Null);
                }
                self.inner.answer(method, params)
            }
            "eth_getLogs" => {
                if hex_u64(&params[0]["toBlock"], "to")? > self.real {
                    return Err("eth_getLogs: block range extends beyond current head".into());
                }
                self.inner.answer(method, params)
            }
            "eth_call" => {
                if hex_u64(&params[1], "pin")? > self.real {
                    return Err("eth_call: header not found".into());
                }
                self.inner.answer(method, params)
            }
            _ => self.inner.answer(method, params),
        }
    }
}

impl JsonRpc for Arc<R5Pin> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        Box::pin(async move { r })
    }
}

fn r5_pin_indexer(
    rpc: &Arc<R5Pin>,
    cas: &Arc<MemCas>,
    held: Holdings,
    path: Option<PathBuf>,
) -> Indexer<Arc<R5Pin>> {
    Indexer::open(
        IndexerConfig {
            rescan_every: 5,
            ..IndexerConfig::new(CHAIN, CONTRACT, DEPLOY)
        },
        rpc.clone(),
        held,
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        path,
    )
    .expect("open")
}

/// IDX-R5-1 (review IDX-R5 PoC 1): the judging was pinned to the highest
/// `finalized` head ever read, so one lying answer far ahead made every
/// later judging call fail on an honest node ("header not found") until a
/// restart. It is pinned to this poll's answer now: the lie fails one poll.
#[tokio::test]
async fn idx_r5_1_one_lying_finalized_head_fails_one_poll_only() {
    let real = DEPLOY + 1_000;
    let fake = Arc::new(Fake::new(real));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 777, 3);
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    let rpc = Arc::new(R5Pin {
        inner: fake.clone(),
        real,
        lie_on: 0,
        asked: AtomicU64::new(0),
    });
    let dir = scratch("idx-r5-1-pin");
    let path = dir.join("state.json");
    let idx = r5_pin_indexer(&rpc, &cas, held.clone(), Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    let _ = idx.poll(&catalog).await; // the lie
    for _ in 0..3 {
        idx.poll(&catalog).await.expect("an honest poll succeeds");
    }
    let h = idx.indexer_health();
    assert!(
        catalog.read().expect("lock").shard(1, &shard).is_some(),
        "served while the node is honest, without a restart"
    );
    assert_eq!(h["finalizedHead"], json!(real), "the lie is not remembered");
    assert!(h["lastError"].is_null(), "{}", h["lastError"]);
    drop(idx);
    // And after a restart, as before.
    let idx2 = r5_pin_indexer(&rpc, &cas, held, Some(path));
    let catalog2 = RwLock::new(Catalog::new());
    let r = idx2.poll(&catalog2).await.expect("poll");
    assert_eq!(r.admitted, vec![(1, shard)], "served after a restart");
    let _ = std::fs::remove_dir_all(&dir);
}

/// IDX-R5-1: the lie comes while a known held shard is still in its
/// challenge window; it finalizes later and is served once the node is
/// honest (the remembered head kept it out for good).
#[tokio::test]
async fn idx_r5_1_a_lie_while_pending_never_pins_later_judging() {
    let real = DEPLOY + 1_000;
    let fake = Arc::new(Fake::new(real));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 777, 3);
    fake.end(1, shard, 2); // Pending: in its challenge window
    let rpc = Arc::new(R5Pin {
        inner: fake.clone(),
        real,
        lie_on: 2,
        asked: AtomicU64::new(0),
    });
    let idx = r5_pin_indexer(&rpc, &cas, held, None);
    let catalog = RwLock::new(Catalog::new());
    for _ in 0..2 {
        idx.poll(&catalog).await.expect("poll");
    }
    assert_eq!(status(&idx, 1, &shard), ShardStatus::Pending);
    assert!(idx.poll(&catalog).await.is_err(), "the lying poll fails");
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    let r = idx.poll(&catalog).await.expect("honest poll");
    assert_eq!(
        r.admitted,
        vec![(1, shard)],
        "served on the next honest poll"
    );
}

/// A node that answers `forged` logs and a final view for every round from
/// 1,000 (`shardState` Finalized) while `lying`, honestly otherwise (review
/// IDX-R5, PoC 2).
struct R5Crowd {
    inner: Arc<Fake>,
    lying: AtomicBool,
    forged: Vec<Value>,
    root: Hash32,
    binding: Hash32,
}

impl R5Crowd {
    fn answer(&self, method: &str, params: &Value) -> Result<Value, String> {
        if !self.lying.load(Ordering::SeqCst) {
            return self.inner.answer(method, params);
        }
        match method {
            "eth_getLogs" => {
                let mut out = self.inner.answer(method, params)?;
                let a = hex_u64(&params[0]["fromBlock"], "a")?;
                let b = hex_u64(&params[0]["toBlock"], "b")?;
                if let Some(v) = out.as_array_mut() {
                    for l in &self.forged {
                        let n = hex_u64(&l["blockNumber"], "n")?;
                        if a <= n && n <= b {
                            v.push(l.clone());
                        }
                    }
                }
                Ok(out)
            }
            "eth_call" => {
                let data = hex_bytes(&params[0]["data"], "data")?;
                let mut rb = [0u8; 8];
                rb.copy_from_slice(&data[28..36]);
                if u64::from_be_bytes(rb) < 1_000 {
                    return self.inner.answer(method, params);
                }
                if data[..4] == SHARD_STATE_SELECTOR {
                    let mut ret = vec![0u8; 160];
                    ret[31] = 3;
                    return Ok(json!(hx(&ret)));
                }
                let mut ret = vec![0u8; 96];
                ret[..32].copy_from_slice(&self.root.0);
                ret[32..64].copy_from_slice(&self.binding.0);
                ret[88..].copy_from_slice(&(DEPLOY + 900).to_be_bytes());
                Ok(json!(hx(&ret)))
            }
            _ => self.inner.answer(method, params),
        }
    }
}

impl JsonRpc for Arc<R5Crowd> {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        let r = self.answer(method, &params);
        Box::pin(async move { r })
    }
}

/// IDX-R5-2 (review IDX-R5 PoC 2): one lying poll admitted 16 forged rounds
/// of a held shard, which were never judged again and filled the metric's
/// 16 candidates (`MAX_METRIC_CANDIDATES`) ahead of the honest round. The
/// periodic rescan now reads every served shard's view again, and two
/// agreeing honest reads take a refuted one out of the catalog.
#[tokio::test]
async fn idx_r5_2_lying_admissions_are_removed_after_a_rescan() {
    let fake = Arc::new(Fake::new(DEPLOY + 1_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 777, 3);
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    let forged: Vec<Value> = (0..16u64)
        .map(|i| {
            log(
                &CONTRACT,
                1_000 + i,
                shard,
                root,
                binding,
                DEPLOY + 100 + i,
                0,
            )
        })
        .collect();
    let rpc = Arc::new(R5Crowd {
        inner: fake.clone(),
        lying: AtomicBool::new(true),
        forged,
        root,
        binding,
    });
    let idx = Indexer::open(
        IndexerConfig {
            rescan_every: 5,
            ..cfg()
        },
        rpc.clone(),
        held,
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(scratch("idx-r5-2-crowd").join("state.json")),
    )
    .expect("open");
    let catalog = RwLock::new(Catalog::new());
    let _ = idx.poll(&catalog).await; // one lying poll
    assert!(
        catalog.read().expect("lock").shard(1_000, &shard).is_some(),
        "the lie listed forged rounds"
    );
    rpc.lying.store(false, Ordering::SeqCst);
    let mut removed = Vec::new();
    for _ in 0..40 {
        removed.extend(idx.poll(&catalog).await.expect("poll").removed);
    }
    let h = idx.indexer_health();
    let c = catalog.read().expect("lock");
    let metric = c
        .shard(1, &shard)
        .expect("honest round listed")
        .metric
        .clone();
    let rounds: Vec<u64> = c
        .for_metric(&metric, None)
        .iter()
        .take(crate::MAX_METRIC_CANDIDATES)
        .map(|m| m.round)
        .collect();
    assert_eq!(rounds, vec![1], "only the honest round is listed");
    assert_eq!(removed.len(), 16);
    assert_eq!(h["removed"], json!(16));
    for r in 1_000..1_016 {
        assert_eq!(
            status(&idx, r, &shard),
            ShardStatus::Ended(Ended::NotOnChain)
        );
    }
}

/// IDX-R5-2: one read refuting a served shard's view (a lying answer at a
/// rescan) never takes it out: removal needs a second read on a later poll
/// saying the same.
#[tokio::test]
async fn idx_r5_2_one_refuting_read_never_removes_a_served_shard() {
    let fake = Arc::new(Fake::new(DEPLOY + 1_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 777, 3);
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            rescan_every: 3,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    let view = *fake
        .views
        .lock()
        .expect("lock")
        .get(&(1, shard))
        .expect("v");
    let mut rescans = 0;
    for n in 0..30 {
        // Every other poll answers "not final" for the served shard.
        if n % 2 == 0 {
            fake.views.lock().expect("lock").remove(&(1, shard));
        } else {
            fake.views.lock().expect("lock").insert((1, shard), view);
        }
        let r = idx.poll(&catalog).await.expect("poll");
        assert!(r.removed.is_empty(), "poll {n}");
        rescans += u64::from(r.rescan_done);
        if n > 0 {
            assert!(catalog.read().expect("lock").shard(1, &shard).is_some());
        }
    }
    assert!(
        rescans >= 5,
        "served views were read again ({rescans} rescans)"
    );
    // Two agreeing refutations on two polls take it out.
    fake.views.lock().expect("lock").remove(&(1, shard));
    let mut gone = false;
    for _ in 0..8 {
        gone |= !idx.poll(&catalog).await.expect("poll").removed.is_empty();
    }
    assert!(gone && catalog.read().expect("lock").shard(1, &shard).is_none());
}

/// IDX-R5-4: the held-shard rescan has its own per-poll budget and resumes
/// across polls; the scan of new blocks does not wait for it; the next
/// periodic rescan is counted from the end of the previous one.
#[tokio::test]
async fn idx_r5_4_the_rescan_has_its_own_budget_and_resumes() {
    let head = DEPLOY + 2_000;
    let fake = Arc::new(Fake::new(head));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 1_500, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 1_500);
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            rescan_every: 2,
            rescan_budget: 3,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    let held_calls = |f: &Fake| {
        f.calls
            .lock()
            .expect("lock")
            .iter()
            .filter(|(m, p)| {
                m == "eth_getLogs" && p[0]["topics"].as_array().map(Vec::len) == Some(3)
            })
            .count()
    };
    let mut h = head;
    let (mut per_poll, mut done_at, mut started_at) = (Vec::new(), Vec::new(), Vec::new());
    for n in 0..20u64 {
        h += 10;
        fake.set_heads(&[h]);
        let before = held_calls(&fake);
        let r = idx.poll(&catalog).await.expect("poll");
        per_poll.push(held_calls(&fake) - before);
        if r.rescanned.is_some_and(|(a, _)| a == DEPLOY) {
            started_at.push(n);
        }
        if r.rescan_done {
            done_at.push(n);
        }
        assert!(r.scanned.is_some_and(|(_, b)| b == h), "the scan keeps up");
    }
    assert!(per_poll.iter().all(|c| *c <= 3), "{per_poll:?}");
    // 2,000 blocks at span 200: 10 calls, so 4 polls at 3 per poll.
    let first = *started_at.first().expect("a rescan ran");
    assert_eq!(
        done_at.first(),
        Some(&(first + 3)),
        "{started_at:?} {done_at:?}"
    );
    assert_eq!(
        started_at.get(1),
        Some(&(first + 3 + 2)),
        "the next one starts 2 polls after the end: {started_at:?} {done_at:?}"
    );
}

/// IDX-R5-5: one lying `finalized` answer far ahead moved the cursor by up
/// to the scan's budget (400,000 blocks), past blocks the node had not
/// finalized; logs in them were missed until a restart (the held shards'
/// until a rescan). The scan now reads at most `max_jump` blocks past the
/// head this poll and the previous one both reported, and the default is
/// the overlap, which reads those blocks again.
#[tokio::test]
async fn idx_r5_5_one_lying_head_moves_the_cursor_one_overlap_at_most() {
    let real = DEPLOY + 1_000;
    let fake = Arc::new(Fake::new(real));
    let cas = Arc::new(MemCas::new());
    let idx = indexer(
        &fake,
        &cas,
        Holdings::new(),
        IndexerConfig::new(CHAIN, CONTRACT, DEPLOY),
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    // A fresh start reads one jump, then up to the head two polls agree on.
    fake.set_heads(&[real, real, real + 1_000_000, real + 300]);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(idx.next_block(), DEPLOY + DEFAULT_MAX_JUMP);
    idx.poll(&catalog).await.expect("poll");
    assert_eq!(idx.next_block(), real + 1);
    // The lie: this node answers no logs above its head.
    idx.poll(&catalog).await.expect("poll");
    assert!(
        idx.next_block() <= real + DEFAULT_MAX_JUMP + 1,
        "cursor {} after one lie",
        idx.next_block()
    );
    // A log the chain later finalizes in the blocks the lie let pass.
    let shard = Hash32([7; 32]);
    fake.emit(9, shard, Hash32([1; 32]), Hash32([2; 32]), real + 50, 0);
    idx.poll(&catalog).await.expect("poll");
    assert!(
        idx.events()
            .iter()
            .any(|e| e.round == 9 && e.shard == shard),
        "found by the overlap"
    );
}

/// IDX-R6-1 (review IDX-R6): a lagging backend of a load-balanced node,
/// whose `finalized` head is past the event but before the block the shard
/// became final at, answered "not final" on two polls and took a served
/// shard out of the catalog. Each served shard now keeps the pin it was
/// admitted at, and a read below it refutes nothing; a refutation at or
/// above it still removes the shard.
#[tokio::test]
async fn idx_r6_1_a_read_below_the_admission_pin_never_removes_a_served_shard() {
    let fake = Arc::new(Fake::new(DEPLOY + 1_000));
    let cas = Arc::new(MemCas::new());
    let mut held = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut held, "a", None);
    fake.emit(1, shard, root, binding, DEPLOY + 700, 3);
    // `getFinalizedRoot` answers from block DEPLOY + 800 on.
    fake.finalize(1, shard, root, binding, DEPLOY + 800);
    let idx = indexer(
        &fake,
        &cas,
        held,
        IndexerConfig {
            rescan_every: 2,
            ..cfg()
        },
        None,
    );
    let catalog = RwLock::new(Catalog::new());
    for _ in 0..3 {
        idx.poll(&catalog).await.expect("poll");
    }
    assert!(catalog.read().expect("lock").shard(1, &shard).is_some());
    // The lagging backend: past the event (DEPLOY + 700), before the root
    // was final (DEPLOY + 800).
    fake.set_heads(&[DEPLOY + 750]);
    let mut removed = Vec::new();
    for _ in 0..12 {
        removed.extend(idx.poll(&catalog).await.expect("poll").removed);
    }
    assert!(removed.is_empty(), "{removed:?}");
    assert!(
        catalog.read().expect("lock").shard(1, &shard).is_some(),
        "still served"
    );
    // At or above the admission pin, two agreeing refutations still remove it.
    fake.set_heads(&[DEPLOY + 1_000]);
    fake.views.lock().expect("lock").remove(&(1, shard));
    for _ in 0..8 {
        removed.extend(idx.poll(&catalog).await.expect("poll").removed);
    }
    assert_eq!(removed, vec![(1, shard)]);
    assert!(catalog.read().expect("lock").shard(1, &shard).is_none());
}

/// IDX-R6-2 (review IDX-R6): rescan progress lived in memory only, so a
/// restart started the held-shard rescan again from `deployBlock` (with
/// 1,000 held shards about 47 h at the default budget: frequent restarts
/// never finished it). The state file now keeps a rescan in progress, and
/// the startup rescan resumes where it stopped.
#[tokio::test]
async fn idx_r6_2_a_restart_resumes_the_rescan_where_it_stopped() {
    let head = DEPLOY + 2_000;
    let fake = Arc::new(Fake::new(head));
    let cas = Arc::new(MemCas::new());
    let held = || {
        let mut h = Holdings::new();
        let ids = hold(&cas, &mut h, "a", None);
        (h, ids)
    };
    let (h1, (shard, root, binding)) = held();
    fake.emit(1, shard, root, binding, DEPLOY + 1_500, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 1_500);
    let path = scratch("idx-r6-2-resume").join("state.json");
    let config = IndexerConfig {
        rescan_every: 2,
        rescan_budget: 1,
        ..cfg()
    };
    let catalog = RwLock::new(Catalog::new());
    let mut read_to = None;
    {
        let idx = indexer(&fake, &cas, h1, config.clone(), Some(path.clone()));
        for _ in 0..6 {
            if let Some((_, b)) = idx.poll(&catalog).await.expect("poll").rescanned {
                read_to = Some(b + 1);
            }
        }
    }
    let read_to = read_to.expect("a rescan ran");
    assert!(read_to > DEPLOY && read_to < head, "cut short at {read_to}");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("json");
    assert_eq!(
        saved["rescan"]["next"],
        json!(read_to),
        "{}",
        saved["rescan"]
    );
    // Restart: the startup rescan resumes where the last one stopped.
    let (h2, _) = held();
    let idx = indexer(&fake, &cas, h2, config, Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    let mut first = None;
    for _ in 0..(STARTUP_RESCAN_POLL + 2) {
        let r = idx.poll(&catalog).await.expect("poll");
        if first.is_none() {
            first = r.rescanned.map(|(a, _)| a);
        }
    }
    assert_eq!(
        first,
        Some(read_to),
        "resumed, not from deployBlock {DEPLOY}"
    );
    // A state file naming a rescan outside the scanned range is refused.
    let mut bad = saved.clone();
    bad["rescan"] = json!({ "next": DEPLOY - 1, "to": DEPLOY + 10 });
    let p = scratch("idx-r6-2-bad").join("state.json");
    std::fs::write(&p, serde_json::to_vec(&bad).expect("json")).expect("write");
    let (h3, _) = held();
    assert!(Indexer::open(
        cfg(),
        fake.clone(),
        h3,
        cas.clone() as Arc<dyn Cas + Send + Sync>,
        Some(p)
    )
    .is_err());
}

/// M7L3-R1-2 (review M7L3-R1): a resumed rescan skipped `deployBlock..next`
/// for a shard newly held after the restart, so a log of it a lying node
/// withheld below the cursor was never read again. The saved cursor now
/// names the held set (a hash of the held ids); a restart holding another
/// set starts the rescan again from `deployBlock`, and one with the same
/// set still resumes.
#[tokio::test]
async fn m7l3_r1_2_a_new_held_set_rescans_from_the_deploy_block() {
    let head = DEPLOY + 2_000;
    let fake = Arc::new(Fake::new(head));
    let cas = Arc::new(MemCas::new());
    let mut h1 = Holdings::new();
    let (shard, root, binding) = hold(&cas, &mut h1, "a", None);
    let one = h1.ids_hash();
    fake.emit(1, shard, root, binding, DEPLOY + 1_500, 0);
    fake.finalize(1, shard, root, binding, DEPLOY + 1_500);
    let path = scratch("m7l3-r1-2").join("state.json");
    let config = IndexerConfig {
        rescan_every: 2,
        rescan_budget: 1,
        ..cfg()
    };
    let catalog = RwLock::new(Catalog::new());
    let mut read_to = None;
    {
        let idx = indexer(&fake, &cas, h1, config.clone(), Some(path.clone()));
        for _ in 0..6 {
            if let Some((_, b)) = idx.poll(&catalog).await.expect("poll").rescanned {
                read_to = Some(b + 1);
            }
        }
    }
    let read_to = read_to.expect("a rescan ran");
    assert!(read_to > DEPLOY, "cut short at {read_to}");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("state")).expect("json");
    assert_eq!(saved["rescan"]["held"], json!(one.to_string()));
    // Restart holding one more shard: its logs below the cursor were never
    // read for it, so the rescan starts again from deployBlock.
    let mut h2 = Holdings::new();
    hold(&cas, &mut h2, "a", None);
    hold(&cas, &mut h2, "b", None);
    assert_ne!(h2.ids_hash(), one);
    let idx = indexer(&fake, &cas, h2, config, Some(path.clone()));
    let catalog = RwLock::new(Catalog::new());
    let mut first = None;
    for _ in 0..(STARTUP_RESCAN_POLL + 2) {
        let r = idx.poll(&catalog).await.expect("poll");
        if first.is_none() {
            first = r.rescanned.map(|(a, _)| a);
        }
    }
    assert_eq!(first, Some(DEPLOY), "not resumed at {read_to}");
    // An older state file (no held hash) does not resume either.
    let mut old = saved.clone();
    old["rescan"]
        .as_object_mut()
        .expect("rescan")
        .remove("held");
    let p = scratch("m7l3-r1-2-old").join("state.json");
    std::fs::write(&p, serde_json::to_vec(&old).expect("json")).expect("write");
    let mut h3 = Holdings::new();
    hold(&cas, &mut h3, "a", None);
    let idx = indexer(&fake, &cas, h3, cfg_rescan(), Some(p));
    let catalog = RwLock::new(Catalog::new());
    let mut first = None;
    for _ in 0..(STARTUP_RESCAN_POLL + 2) {
        let r = idx.poll(&catalog).await.expect("poll");
        if first.is_none() {
            first = r.rescanned.map(|(a, _)| a);
        }
    }
    assert_eq!(first, Some(DEPLOY));
}

fn cfg_rescan() -> IndexerConfig {
    IndexerConfig {
        rescan_every: 2,
        rescan_budget: 1,
        ..cfg()
    }
}
