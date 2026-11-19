//! The record's chain reader against a scripted fake node (no network):
//! complete days only (design test 4), the day hash check (5), no rebuild of
//! a built folder (6), the day's binary search, refused ranges split, a
//! rate-limited run resumed, role getters and observed times by `eth_call`,
//! Empty shards dropped, and fake chain days built into bulk versions that
//! follow each other.

use super::*;
use crate::early::tests::{
    activated, answered, bet, committed, deal, decided, full_day, key_event, key_registered,
    list_2_json, list_2_json_of, opened, report, resolved, submitted_on, test_contracts_json, v4,
    BETTOR_NO, BOOK, DAY, FROM, MINER_A, OPENER, PAYER, REVIEWER, SETTLE, SIGNER, STORAGE,
    STORAGE4, SUBMITTER, T0,
};
use crate::early::{addr_text, parse_day, sig};
use crate::x402::BoxFuture;
use onyx_bulk::manifest::{Manifest, NewVersion, ShardEntry};
use onyx_core::{DataRights, Redistribution, SourceId};
use std::sync::atomic::{AtomicU64, Ordering};

/// Two-second blocks: block FROM is 00:00:00 of DAY, so DAY + 1 starts at
/// block FROM + PER_DAY.
const PER_DAY: u64 = 43_200;

fn time_of(n: u64) -> u64 {
    T0 + 2 * n - 2 * FROM
}

fn hx32(h: &Hash32) -> String {
    format!("0x{}", h.to_hex())
}

fn sel(s: &str) -> String {
    format!("0x{}", hex::encode(abi::selector(s)))
}

/// A node as the reader sees it, with scripted faults.
struct Fake {
    head: AtomicU64,
    /// Blocks at or after this one have other hashes (a reorg); u64::MAX: none.
    reorg_from: AtomicU64,
    logs: Vec<ChainLog>,
    /// Widest `eth_getLogs` range the node answers.
    cap: u64,
    /// `eth_getLogs` calls answered before every later one is rate-limited.
    fail_after: AtomicU64,
    /// The next calls (any method) answer 429.
    flaky: AtomicU64,
    log_calls: Mutex<Vec<(u64, u64)>>,
    header_calls: AtomicU64,
    eth_calls: Mutex<Vec<String>>,
    /// Contracts have code from this block on.
    code_from: AtomicU64,
    /// Contracts deployed later: address -> first block with code.
    late: Mutex<BTreeMap<String, u64>>,
    /// Refusal text for a range over `cap` (empty: a span-cap message).
    refusal: String,
    /// Drop the log at this block from the next answer that holds it.
    drop_once: Mutex<Option<u64>>,
    /// Logs of this block name another fork's block hash.
    stale_hash_at: AtomicU64,
    /// Deploy receipts: tx -> (block, created address).
    receipts: Mutex<BTreeMap<String, (u64, String)>>,
    /// From this block on, blocks are `gap.1` seconds later (a halted chain).
    gap: (u64, u64),
    /// (to, selector) -> role answer; others get an address of their own.
    roles: Mutex<BTreeMap<(String, String), [u8; 20]>>,
    observed: Mutex<BTreeMap<String, u64>>,
    status: Mutex<BTreeMap<(u64, String), u8>>,
}

impl Fake {
    fn new(head: u64, logs: Vec<ChainLog>) -> Fake {
        Fake {
            head: AtomicU64::new(head),
            reorg_from: AtomicU64::new(u64::MAX),
            logs,
            cap: 10_000,
            fail_after: AtomicU64::new(u64::MAX),
            flaky: AtomicU64::new(0),
            log_calls: Mutex::new(Vec::new()),
            header_calls: AtomicU64::new(0),
            eth_calls: Mutex::new(Vec::new()),
            code_from: AtomicU64::new(FROM),
            late: Mutex::new(BTreeMap::new()),
            refusal: String::new(),
            drop_once: Mutex::new(None),
            stale_hash_at: AtomicU64::new(u64::MAX),
            receipts: Mutex::new(BTreeMap::new()),
            gap: (u64::MAX, 0),
            roles: Mutex::new(BTreeMap::new()),
            observed: Mutex::new(BTreeMap::new()),
            status: Mutex::new(BTreeMap::new()),
        }
    }

    fn time(&self, n: u64) -> u64 {
        time_of(n) + if n >= self.gap.0 { self.gap.1 } else { 0 }
    }

    /// Whether `addr` has code at block `n`.
    fn has_code(&self, addr: &str, n: u64) -> Result<bool, String> {
        let late = self
            .late
            .lock()
            .map_err(|_| "lock")?
            .get(addr)
            .copied()
            .unwrap_or(0);
        Ok(n >= self.code_from.load(Ordering::SeqCst).max(late))
    }

    fn hash(&self, n: u64) -> Hash32 {
        let fork = u64::from(n >= self.reorg_from.load(Ordering::SeqCst));
        keccak256(&[n.to_be_bytes(), fork.to_be_bytes()].concat())
    }

    fn role(&self, k: (String, String)) -> Result<[u8; 20], String> {
        Ok(match self.roles.lock().map_err(|_| "lock")?.get(&k) {
            Some(a) => *a,
            None if k.1 == sel("receiptSigner()") => [0; 20],
            None => {
                let h = keccak256(format!("{}{}", k.0, k.1).as_bytes());
                let mut a = [0u8; 20];
                a.copy_from_slice(&h.0[..20]);
                a
            }
        })
    }

    fn answer(&self, method: &str, p: &Value) -> Result<Value, String> {
        if self.flaky.load(Ordering::SeqCst) > 0 {
            self.flaky.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("{method}: node answered HTTP 429"));
        }
        let q = |v: &Value| hex_u64(v, "quantity");
        let word = |w: &[u8]| json!(format!("0x{}", hex::encode(w)));
        match method {
            "eth_getBlockByNumber" => {
                self.header_calls.fetch_add(1, Ordering::SeqCst);
                let n = match p[0].as_str() {
                    Some("finalized") => self.head.load(Ordering::SeqCst),
                    _ => q(&p[0])?,
                };
                Ok(json!({
                    "number": format!("0x{n:x}"),
                    "timestamp": format!("0x{:x}", self.time(n)),
                    "hash": hx32(&self.hash(n)),
                }))
            }
            "eth_getLogs" => {
                let f = &p[0];
                let (a, b) = (q(&f["fromBlock"])?, q(&f["toBlock"])?);
                let calls = {
                    let mut c = self.log_calls.lock().map_err(|_| "lock")?;
                    c.push((a, b));
                    c.len() as u64
                };
                if calls > self.fail_after.load(Ordering::SeqCst) {
                    return Err("eth_getLogs: node answered HTTP 429".into());
                }
                if b - a + 1 > self.cap {
                    return Err(if self.refusal.is_empty() {
                        format!("query limited to a {} range", self.cap)
                    } else {
                        self.refusal.clone()
                    });
                }
                let list = |v: &Value| -> Vec<String> {
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let (addrs, topics) = (list(&f["address"]), list(&f["topics"][0]));
                let dropped = {
                    let mut d = self.drop_once.lock().map_err(|_| "lock")?;
                    match *d {
                        Some(x) if x >= a && x <= b => d.take(),
                        _ => None,
                    }
                };
                let stale = self.stale_hash_at.load(Ordering::SeqCst);
                let out: Vec<Value> = self
                    .logs
                    .iter()
                    .filter(|l| l.block >= a && l.block <= b)
                    .filter(|l| Some(l.block) != dropped)
                    .filter(|l| addrs.contains(&addr_text(&l.address)))
                    .filter(|l| l.topics.first().is_some_and(|t| topics.contains(&hx32(t))))
                    .map(|l| {
                        json!({
                            "address": addr_text(&l.address),
                            "topics": l.topics.iter().map(hx32).collect::<Vec<_>>(),
                            "data": format!("0x{}", hex::encode(&l.data)),
                            "blockNumber": format!("0x{:x}", l.block),
                            "blockHash": hx32(&if l.block == stale {
                                keccak256(b"another fork")
                            } else {
                                self.hash(l.block)
                            }),
                            "transactionHash": hx32(&l.tx),
                            "logIndex": format!("0x{:x}", l.log_index),
                            "removed": false,
                        })
                    })
                    .collect();
                Ok(Value::Array(out))
            }
            "eth_getCode" => Ok(json!(
                if self.has_code(p[0].as_str().ok_or("addr")?, q(&p[1])?)? {
                    "0x6080"
                } else {
                    "0x"
                }
            )),
            "eth_getTransactionReceipt" => {
                let tx = p[0].as_str().ok_or("tx")?;
                Ok(match self.receipts.lock().map_err(|_| "lock")?.get(tx) {
                    Some((b, a)) => {
                        json!({"blockNumber": format!("0x{b:x}"), "contractAddress": a})
                    }
                    None => Value::Null,
                })
            }
            "eth_call" => {
                let to = p[0]["to"].as_str().ok_or("to")?.to_string();
                let data = p[0]["data"].as_str().ok_or("data")?.to_string();
                self.eth_calls
                    .lock()
                    .map_err(|_| "lock")?
                    .push(data[..10].to_string());
                if !self.has_code(&to, q(&p[1])?)? {
                    return Ok(json!("0x"));
                }
                let bytes = hex::decode(&data[2..]).map_err(|e| e.to_string())?;
                if data.starts_with(&sel("report(bytes32)")) {
                    let d = format!("0x{}", hex::encode(&bytes[4..36]));
                    let o = *self
                        .observed
                        .lock()
                        .map_err(|_| "lock")?
                        .get(&d)
                        .ok_or("no report")?;
                    let o = abi::word_u128(u128::from(o));
                    return Ok(word(
                        &[
                            [7u8; 32],
                            [1; 32],
                            [2; 32],
                            [3; 32],
                            o,
                            abi::word_u128(1),
                            o,
                        ]
                        .concat(),
                    ));
                }
                if data.starts_with(&sel("shardState(uint64,bytes32)")) {
                    let round = abi::word_u64(&bytes[4..], 0).map_err(|e| e.to_string())?;
                    let shard = format!("0x{}", hex::encode(&bytes[36..68]));
                    let st = self
                        .status
                        .lock()
                        .map_err(|_| "lock")?
                        .get(&(round, shard))
                        .copied()
                        .unwrap_or(1);
                    return Ok(word(
                        &[
                            abi::word_u128(u128::from(st)),
                            [0; 32],
                            [0; 32],
                            [0; 32],
                            [0; 32],
                        ]
                        .concat(),
                    ));
                }
                let a = self.role((to, data[..10].to_string()))?;
                let mut w = [0u8; 32];
                w[12..].copy_from_slice(&a);
                Ok(word(&w))
            }
            m => Err(format!("fake node: unexpected {m}")),
        }
    }
}

impl JsonRpc for Fake {
    fn call<'a>(&'a self, method: &'a str, params: Value) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(async move { self.answer(method, &params) })
    }
}

fn quick(f: &Fake) -> Reader<'_, Fake> {
    Reader::new(f, &[Duration::from_millis(100); 3], SPAN)
}

static N: AtomicU64 = AtomicU64::new(0);

/// A fresh folder with the test contracts and a reviewed list.
fn world() -> (PathBuf, BuildOpts) {
    let root = std::env::temp_dir().join(format!(
        "onyx-early-chain-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::write(root.join("contracts.json"), test_contracts_json(FROM)).expect("contracts");
    let reviewed = json!({
        "format": 1,
        "operator_keys": [{"address": addr_text(&[0x5c; 20]), "source": "test: the operator's demo key", "doc": "test", "doc_row": "test", "from_day": crate::early::day_text(DAY)}],
        "record_publisher": null
    });
    std::fs::write(root.join("exclusions.json"), reviewed.to_string()).expect("reviewed");
    let o = BuildOpts {
        contracts: root.join("contracts.json"),
        exclusions: root.join("exclusions.json"),
        state: root.join("state"),
        out: root.join("v1"),
        prev: None,
        day: None,
    };
    (root, o)
}

fn out(o: &BuildOpts, name: &str) -> BuildOpts {
    BuildOpts {
        out: o.out.with_file_name(name),
        ..o.clone()
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn tool_of(o: &BuildOpts) -> ToolState {
    serde_json::from_str(&read(&o.state.join("state.json"))).expect("state")
}

/// Day 2's logs: a claim decided by Vote, a storage proof, a correction.
fn day_two() -> Vec<ChainLog> {
    let b = FROM + PER_DAY;
    let node = Hash32([0x81; 32]);
    vec![
        submitted_on(b, 0, 9, Hash32([0xd5; 32]), SUBMITTER, 5),
        decided(b + 1, 0, 9, 3, 2),
        answered(b + 2, 0, 1, 3, 0, node, true, 6),
        opened(b + 3, 0, 2, OPENER),
        bet(b + 4, 0, 2, BETTOR_NO, 2, 10),
        resolved(b + 5, 0, 2, 2),
    ]
}

fn two_days() -> Vec<ChainLog> {
    let mut v = full_day();
    v.extend(day_two());
    v
}

#[tokio::test]
async fn the_last_block_of_a_day_is_found_by_binary_search() {
    let f = Fake::new(FROM + 10 * PER_DAY, vec![]);
    let r = quick(&f);
    let head = r.finalized().await.expect("head");
    assert_eq!(
        time_of(FROM + PER_DAY),
        T0 + 86_400,
        "this block is 00:00:00 of the next day"
    );
    assert_eq!(
        r.day_of(FROM, head).await.expect("day"),
        Some((DAY, FROM + PER_DAY - 1))
    );
    let reads = f.header_calls.load(Ordering::SeqCst);
    assert!(reads <= 25, "{reads} header reads for ten days of blocks");
    // The next day starts exactly at 00:00:00.
    assert_eq!(
        r.day_of(FROM + PER_DAY, head).await.expect("day"),
        Some((DAY + 1, FROM + 2 * PER_DAY - 1))
    );
    // A day that ends past the head is not complete.
    assert_eq!(r.day_of(FROM, FROM + PER_DAY - 1).await.expect("day"), None);
    assert_eq!(r.day_of(head + 1, head).await.expect("day"), None);
}

#[tokio::test]
async fn a_day_past_finalized_is_not_written() {
    let (root, o) = world();
    // Finalized is the day's last block: the day is not complete.
    let f = Fake::new(FROM + PER_DAY - 1, full_day());
    assert_eq!(build(&quick(&f), &o).await.expect("run"), Built::NothingNew);
    assert!(!o.out.exists() && !o.state.join("state.json").exists());
    // Finalized reaches 00:00:00 of the next day: the day is written.
    f.head.store(FROM + PER_DAY, Ordering::SeqCst);
    let b = build(&quick(&f), &o).await.expect("run");
    assert!(
        matches!(b, Built::Day { day, rows: 11, .. } if day == DAY),
        "{b:?}"
    );
    let meta: Value = serde_json::from_str(&read(&o.out.join("day.json"))).expect("day.json");
    assert_eq!(meta["day"], json!("2026-11-14"));
    assert_eq!(meta["to_block"], json!(FROM + PER_DAY - 1));
    assert_eq!(
        read(&o.out.join("schema.json")),
        schema_file(&record_schema())
    );
    let csv = read(&o.out.join("data.csv"));
    assert_eq!(meta["csv_keccak"], json!(csv_hash(&csv)));
    let close = csv.lines().last().expect("close").to_string();
    assert!(
        close.contains(&format!(",day_close,,{FROM}-{},", FROM + PER_DAY - 1)),
        "{close}"
    );
    assert!(close.contains(&format!("block={}", hx32(&f.hash(FROM + PER_DAY - 1)))));
    // Day 2 is not complete yet: nothing.
    assert_eq!(
        build(&quick(&f), &out(&o, "v2")).await.expect("run"),
        Built::NothingNew
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_changed_day_hash_stops_the_run() {
    let (root, o) = world();
    let f = Fake::new(FROM + 3 * PER_DAY, two_days());
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day 1"),
        Built::Day { .. }
    ));
    let state = read(&o.state.join("state.json"));
    // A reorg replaces the day's last block.
    f.reorg_from.store(FROM + PER_DAY - 10, Ordering::SeqCst);
    let o2 = out(&o, "v2");
    assert!(matches!(
        build(&quick(&f), &o2).await,
        Err(ReadError::Canonical(_))
    ));
    assert!(!o2.out.exists(), "nothing written");
    assert_eq!(
        read(&o.state.join("state.json")),
        state,
        "the state is unchanged"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_second_run_writes_nothing_and_a_built_folder_is_never_rebuilt() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY + 5, two_days());
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day 1"),
        Built::Day { .. }
    ));
    let csv = read(&o.out.join("data.csv"));
    let state = read(&o.state.join("state.json"));
    // The same folder again: nothing done, nothing changed.
    assert_eq!(
        build(&quick(&f), &o).await.expect("again"),
        Built::AlreadyBuilt(DAY)
    );
    assert_eq!(read(&o.out.join("data.csv")), csv);
    assert_eq!(read(&o.state.join("state.json")), state);
    // A changed built folder is refused, never rebuilt.
    std::fs::write(o.out.join("data.csv"), csv.replacen("winner", "also", 1)).expect("tamper");
    assert!(matches!(
        build(&quick(&f), &o).await,
        Err(ReadError::Folder(_))
    ));
    std::fs::write(o.out.join("data.csv"), &csv).expect("restore");
    // A run that wrote day 2's folder but stopped before saving its state:
    // the next run reads the day again and takes the folder only when it is
    // the same, byte for byte.
    f.head.store(FROM + 2 * PER_DAY, Ordering::SeqCst);
    let o2 = out(&o, "v2");
    assert!(matches!(
        build(&quick(&f), &o2).await.expect("day 2"),
        Built::Day { .. }
    ));
    let after = read(&o.state.join("state.json"));
    std::fs::write(o.state.join("state.json"), &state).expect("as if not saved");
    assert!(matches!(
        build(&quick(&f), &o2).await.expect("recovered"),
        Built::Day { .. }
    ));
    assert_eq!(read(&o.state.join("state.json")), after);
    std::fs::write(o.state.join("state.json"), &state).expect("as if not saved");
    let c2 = read(&o2.out.join("data.csv"));
    std::fs::write(
        o2.out.join("data.csv"),
        c2.replacen("by=Vote", "by=Fallback", 1),
    )
    .expect("tamper");
    assert!(matches!(
        build(&quick(&f), &o2).await,
        Err(ReadError::Folder(_))
    ));
    // A folder with other files is not taken.
    let o3 = out(&o, "v3");
    std::fs::create_dir_all(&o3.out).expect("dir");
    std::fs::write(o3.out.join("notes.txt"), "x").expect("file");
    assert!(matches!(
        build(&quick(&f), &o3).await,
        Err(ReadError::Folder(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_refused_range_is_split() {
    let (root, o) = world();
    let mut f = Fake::new(FROM + PER_DAY, full_day());
    f.cap = 64;
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day"),
        Built::Day { .. }
    ));
    let calls = f.log_calls.lock().expect("lock").clone();
    assert_eq!(
        calls[0],
        (FROM, FROM + SPAN - 1),
        "the first range is the full span"
    );
    assert!(
        calls.iter().any(|(a, b)| b - a + 1 == 50),
        "split in halves to 50 blocks"
    );
    let read_blocks: u64 = calls
        .iter()
        .filter(|(a, b)| b - a < 64)
        .map(|(a, b)| b - a + 1)
        .sum();
    assert_eq!(
        read_blocks,
        2 * PER_DAY,
        "every block read once in each of the two passes"
    );
    // The same day from a node without a cap gives the same bytes.
    let (root2, o2) = world();
    let g = Fake::new(FROM + PER_DAY, full_day());
    build(&quick(&g), &o2).await.expect("day");
    assert_eq!(
        read(&o.out.join("data.csv")),
        read(&o2.out.join("data.csv"))
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[tokio::test(start_paused = true)]
async fn a_rate_limited_run_resumes_from_its_cursor() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    // A few rate-limited answers are retried and change nothing.
    f.flaky.store(2, Ordering::SeqCst);
    // After 120 ranges the node turns every call away.
    f.fail_after.store(120, Ordering::SeqCst);
    let r = Reader::standard(&f);
    assert!(matches!(build(&r, &o).await, Err(ReadError::Node(_))));
    let p: Pending = serde_json::from_str(&read(&o.state.join("pending.json"))).expect("pending");
    assert_eq!((p.from, p.to, p.pass), (FROM, FROM + PER_DAY - 1, 1));
    assert_eq!(
        p.next,
        FROM + 120 * SPAN,
        "kept up to the last range answered"
    );
    assert!(!o.out.exists() && !o.state.join("state.json").exists());
    // The node is back: the next run goes on from the kept cursor.
    f.fail_after.store(u64::MAX, Ordering::SeqCst);
    let before = f.log_calls.lock().expect("lock").len();
    assert!(matches!(
        build(&r, &o).await.expect("resumed"),
        Built::Day { .. }
    ));
    let calls = f.log_calls.lock().expect("lock").clone();
    assert_eq!(
        calls[before].0, p.next,
        "the resumed run starts at the kept cursor"
    );
    assert!(!o.state.join("pending.json").exists());
    // The same bytes as an uninterrupted run.
    let (root2, o2) = world();
    let g = Fake::new(FROM + PER_DAY, full_day());
    build(&quick(&g), &o2).await.expect("day");
    assert_eq!(
        read(&o.out.join("data.csv")),
        read(&o2.out.join("data.csv"))
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[tokio::test]
async fn role_getters_feed_the_exclusions() {
    let (root, o) = world();
    let f = Fake::new(FROM + 2 * PER_DAY, two_days());
    {
        let mut roles = f.roles.lock().expect("lock");
        // The settlement's coordinator answers a book signer: skipped, not refused.
        roles.insert((addr_text(&SETTLE), sel("coordinator()")), SIGNER);
        roles.insert((addr_text(&BOOK), sel("reviewer()")), REVIEWER);
    }
    let b = build(&quick(&f), &o).await.expect("day 1");
    let Built::Day { skipped, .. } = b else {
        panic!("{b:?}")
    };
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].address, addr_text(&SIGNER));
    let xdir = o.out.join("exclusions");
    let x = read(&xdir.join("data.csv"));
    assert_eq!(
        read(&xdir.join("schema.json")),
        schema_file(&exclusions_schema())
    );
    assert!(x.contains(&format!(
        ",{},add,role,OnyxSignedBook.reviewer()\n",
        addr_text(&REVIEWER)
    )));
    assert!(x.contains(",add,operator_key,test: the operator's demo key\n"));
    assert!(x.contains(",add,fixture,anvil default account 0\n"));
    assert!(
        !x.contains(&addr_text(&SIGNER)),
        "a publisher key is not excluded"
    );
    assert!(!x.contains("receiptSigner()"), "an unset role adds nothing");
    // The day_close names the hash of the exclusions in force.
    let tool = tool_of(&o);
    let h = exclusions_hash(&exclusions_in_force(&tool.exclusions).expect("in force"));
    assert!(read(&o.out.join("data.csv")).contains(&format!("exclusions={}", hx32(&h))));
    // Day 2: the reviewer moves. One remove and one add, nothing else.
    f.roles
        .lock()
        .expect("lock")
        .insert((addr_text(&BOOK), sel("reviewer()")), [0x6d; 20]);
    let o2 = out(&o, "v2");
    build(&quick(&f), &o2).await.expect("day 2");
    let x2 = read(&o2.out.join("exclusions").join("data.csv"));
    assert_eq!(x2.lines().count(), 3, "{x2}");
    assert!(x2.contains(&format!(
        ",{},remove,role,OnyxSignedBook.reviewer()\n",
        addr_text(&REVIEWER)
    )));
    assert!(x2.contains(&format!(
        ",{},add,role,OnyxSignedBook.reviewer()\n",
        addr_text(&[0x6d; 20])
    )));
    // Day 3: nothing changed, so no exclusions version.
    f.head.store(FROM + 3 * PER_DAY, Ordering::SeqCst);
    let o3 = out(&o, "v3");
    build(&quick(&f), &o3).await.expect("day 3");
    assert!(!o3.out.join("exclusions").exists());
    // Before a contract has code, an eth_call answers nothing: no role.
    let calls = role_calls(&Contracts::load(&o.contracts).expect("c"), FROM - 1);
    let none = quick(&f)
        .eth_call(&calls[0].to, &abi::selector(calls[0].getter), FROM - 1)
        .await
        .expect("call");
    assert!(none.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn observed_times_come_from_the_book() {
    let (root, o) = world();
    let key = Hash32([0x61; 32]);
    let d1 = Hash32([0x71; 32]);
    let b1 = FROM + 500;
    let since = time_of(b1) + 100;
    let mut logs = vec![
        key_registered(FROM + 1, 0, key, SIGNER, REVIEWER),
        report(b1, 0, d1, key, 1),
        key_event(sig::KEY_COMPROMISED, FROM + 900, 0, key, since),
    ];
    let f = Fake::new(FROM + PER_DAY, logs.clone());
    // Observed 50 s before the report was recorded, so before `since`.
    f.observed
        .lock()
        .expect("lock")
        .insert(hx32(&d1), time_of(b1) - 50);
    build(&quick(&f), &o).await.expect("day");
    let csv = read(&o.out.join("data.csv"));
    assert!(
        csv.contains(",signed_report,") && !csv.contains(",revoke,"),
        "{csv}"
    );
    assert!(f
        .eth_calls
        .lock()
        .expect("lock")
        .contains(&sel("report(bytes32)")));
    // Without the book's answer the 300 s rule revokes it.
    let c = Contracts::load(&o.contracts).expect("c");
    for l in &mut logs {
        l.block_time = time_of(l.block);
    }
    let rows = State::new(&c)
        .apply(&c, FROM, FROM + 1000, &logs, &Views::default())
        .expect("apply");
    assert!(rows.iter().any(|r| r.revokes.is_some()));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_empty_shard_leaves_the_cache_at_its_status() {
    let (root, o) = world();
    let (x, y) = (Hash32([0x51; 32]), Hash32([0x54; 32]));
    let f = Fake::new(
        FROM + PER_DAY,
        vec![
            committed(FROM, 0, 7, x, MINER_A),
            committed(FROM, 1, 7, y, MINER_A),
        ],
    );
    // x ended Empty (no reveal, no event); y is still open.
    f.status.lock().expect("lock").insert((7, hx32(&x)), 4);
    build(&quick(&f), &o).await.expect("day");
    let left: Vec<String> = tool_of(&o)
        .record
        .unfinalized_shards()
        .into_iter()
        .map(|(_, _, rs)| rs)
        .collect();
    assert_eq!(left, vec![format!("7:{}", hx32(&y))]);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn the_contracts_are_checked_on_the_first_run() {
    for code_from in [FROM - 5, FROM + 1] {
        let (root, o) = world();
        let f = Fake::new(FROM + PER_DAY, full_day());
        f.code_from.store(code_from, Ordering::SeqCst);
        assert!(
            matches!(build(&quick(&f), &o).await, Err(ReadError::Contracts(_))),
            "{code_from}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
    // Checked once: the deploy blocks found are kept in the state.
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    build(&quick(&f), &o).await.expect("day");
    assert_eq!(
        tool_of(&o).deploy_blocks.expect("checked")["OnyxSettlement"],
        FROM
    );
    let _ = std::fs::remove_dir_all(root);
}

fn manifest(csv: &str, prev: Option<&Manifest>, created: u64) -> Manifest {
    manifest_of(&record_schema(), crate::early::DATASET, csv, prev, created)
}

fn manifest_of(
    schema: &onyx_bulk::Schema,
    dataset: &str,
    csv: &str,
    prev: Option<&Manifest>,
    created: u64,
) -> Manifest {
    let mut entries: Vec<ShardEntry> = prev.map(|p| p.shards.clone()).unwrap_or_default();
    for f in
        shard_files_of(schema, csv, onyx_bulk::shard::DEFAULT_TARGET_SHARD_BYTES).expect("shards")
    {
        let c = onyx_bulk::shard::check_shard(schema, &f).expect("shard");
        entries.push(ShardEntry::new(entries.len() as u32, &c.summary));
    }
    let source = SourceId("test.early-record".into());
    Manifest::build(
        NewVersion {
            dataset: dataset.into(),
            publisher: source.clone(),
            created_unix: created,
            schema: schema.clone(),
            rights: DataRights {
                license: crate::early::LICENSE.into(),
                redistribution: Redistribution::Allowed,
                source,
                attribution: None,
                terms: String::new(),
            },
        },
        prev,
        entries,
    )
    .expect("manifest")
}

#[tokio::test]
async fn a_fake_chain_day_builds_into_a_bulk_version_and_its_successor() {
    let (root, o) = world();
    let f = Fake::new(FROM + 2 * PER_DAY, two_days());
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day 1"),
        Built::Day { .. }
    ));
    let o2 = BuildOpts {
        prev: Some(o.out.clone()),
        day: Some(DAY + 1),
        ..out(&o, "v2")
    };
    // --day must name the next day, --prev the state's last one.
    let wrong_day = BuildOpts {
        day: Some(DAY + 2),
        ..o2.clone()
    };
    assert!(build(&quick(&f), &wrong_day).await.is_err());
    let wrong_prev = BuildOpts {
        prev: Some(o2.out.clone()),
        ..o2.clone()
    };
    assert!(build(&quick(&f), &wrong_prev).await.is_err());
    let b = build(&quick(&f), &o2).await.expect("day 2");
    assert!(
        matches!(b, Built::Day { day, .. } if day == DAY + 1),
        "{b:?}"
    );
    let (c1, c2) = (
        read(&o.out.join("data.csv")),
        read(&o2.out.join("data.csv")),
    );
    assert!(c2.contains(",2026-11-15,stake_correct,") && c2.contains(",by=Vote,"));
    assert!(c2.contains(",2026-11-15,storage_proof,") && c2.contains(",correction_found,"));
    // Into bulk versions: D + 1 follows D.
    let m1 = manifest(&c1, None, T0 + 86_400);
    let m2 = manifest(&c2, Some(&m1), T0 + 2 * 86_400);
    m2.check_successor(&m1).expect("D + 1 follows D");
    // verify rebuilds day 2 from the deploy blocks, and from the saved state.
    std::fs::write(o2.out.join("manifest.bin"), m2.to_bytes()).expect("manifest");
    let v = verify(&quick(&f), &o.contracts, &o.exclusions, &o2.out, None)
        .await
        .expect("verify");
    assert_eq!((v.day, v.shards), (DAY + 1, 1));
    let snap = o.state.join("before").join("2026-11-15.json");
    let v2 = verify(
        &quick(&f),
        &o.contracts,
        &o.exclusions,
        &o2.out,
        Some(&snap),
    )
    .await
    .expect("verify");
    assert_eq!(v2, v);
    // A changed folder fails verify.
    std::fs::write(
        o2.out.join("data.csv"),
        c2.replacen("by=Vote", "by=Fallback", 1),
    )
    .expect("tamper");
    assert!(
        verify(&quick(&f), &o.contracts, &o.exclusions, &o2.out, None)
            .await
            .is_err()
    );
    std::fs::write(o2.out.join("data.csv"), &c2).expect("restore");
    // A manifest whose last shard holds other rows fails too.
    let other = c2.replacen("by=Vote", "by=Fallback", 1);
    let m_bad = manifest(&other, Some(&m1), T0 + 2 * 86_400);
    std::fs::write(o2.out.join("manifest.bin"), m_bad.to_bytes()).expect("manifest");
    assert!(
        verify(&quick(&f), &o.contracts, &o.exclusions, &o2.out, None)
            .await
            .is_err()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn days_parse_back() {
    for d in [0, 11_016, 19_783, DAY, DAY + 1] {
        assert_eq!(parse_day(&crate::early::day_text(d)), Some(d));
    }
    for bad in ["2026-02-30", "2026-13-01", "26-11-14", "2026-11-1", "x"] {
        assert_eq!(parse_day(bad), None, "{bad}");
    }
}

// ------------------------------------------------------------------ review of slice 2

#[tokio::test]
async fn a_refused_range_is_split_whatever_its_numbers_or_words() {
    let c = Contracts::parse(test_contracts_json(FROM).as_bytes()).expect("contracts");
    // Block numbers holding "429" and "32005", a span cap, and Infura's
    // JSON-RPC -32005 result limit: every one is a refusal, so it splits.
    for (from, text) in [
        (4290, ""),
        (32005, ""),
        (
            42_900,
            r#"{"code":-32005,"message":"query returned more than 10000 results"}"#,
        ),
        (4290, "block range 4290..4489 is too large"),
    ] {
        let to = from + 9_999;
        let logs = vec![
            committed(from, 0, 7, Hash32([0x51; 32]), MINER_A),
            committed(from + 1, 0, 7, Hash32([0x52; 32]), MINER_A),
            committed(from + 5_000, 0, 7, Hash32([0x53; 32]), MINER_A),
            committed(to, 0, 7, Hash32([0x54; 32]), MINER_A),
        ];
        let mut f = Fake::new(to + 10, logs);
        f.cap = 64;
        f.refusal = text.to_string();
        let mut p = Pending::default();
        let got = quick(&f)
            .logs(&c, from, to, &mut p, &mut |_| Ok(()))
            .await
            .unwrap_or_else(|e| panic!("{from} {text:?}: {e}"));
        assert_eq!(got.len(), 4, "{from} {text:?}");
        let calls = f.log_calls.lock().expect("lock").clone();
        let answered: Vec<&(u64, u64)> = calls.iter().filter(|(a, b)| b - a < 64).collect();
        // Refusals across the whole span: every range answered is under the
        // cap, and they cover it twice (two passes).
        let blocks: u64 = answered.iter().map(|(a, b)| b - a + 1).sum();
        assert_eq!(blocks, 2 * 10_000, "{from} {text:?}");
        assert!(answered.iter().any(|(a, _)| *a > from + 9_000));
    }
    // A rate limit (the node's own words) is retried on the same range, never split.
    let f = Fake::new(FROM + 300, vec![]);
    f.flaky.store(1, Ordering::SeqCst);
    let mut p = Pending::default();
    quick(&f)
        .logs(&c, FROM, FROM + 199, &mut p, &mut |_| Ok(()))
        .await
        .expect("retried");
    assert_eq!(f.log_calls.lock().expect("lock")[0], (FROM, FROM + 199));
    assert!(rate_limited("eth_getLogs: node answered HTTP 429"));
    assert!(rate_limited("Too Many Requests"));
    assert!(!rate_limited(
        r#"{"code":-32005,"message":"query returned more than 10000 results"}"#
    ));
    assert!(!rate_limited("query limited to a 429 range"));
}

#[tokio::test]
async fn a_backend_that_drops_a_log_on_one_pass_stops_the_run() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    // One answer of one pass leaves out a log (a lagging backend).
    *f.drop_once.lock().expect("lock") = Some(106);
    let e = build(&quick(&f), &o).await.expect_err("refused");
    assert!(e.to_string().contains("two reads"), "{e}");
    assert!(!o.out.exists() && !o.state.join("state.json").exists());
    // Run again: both passes agree now, and the day is written.
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day"),
        Built::Day { .. }
    ));
    // The lower of two finalized reads is the head.
    let g = Fake::new(FROM + PER_DAY, vec![]);
    assert_eq!(quick(&g).finalized().await.expect("head"), FROM + PER_DAY);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_later_reviewed_entry_leaves_earlier_days_identical() {
    let (root, o) = world();
    let f = Fake::new(FROM + 3 * PER_DAY, two_days());
    build(&quick(&f), &o).await.expect("day 1");
    let o2 = out(&o, "v2");
    build(&quick(&f), &o2).await.expect("day 2");
    // The reviewer adds a key from day 3 on, and ends the demo key on day 3.
    let mut r: Value = serde_json::from_str(&read(&o.exclusions)).expect("reviewed");
    r["operator_keys"][0]["until_day"] = json!(crate::early::day_text(DAY + 2));
    r["operator_keys"]
        .as_array_mut()
        .expect("list")
        .push(json!({
            "address": addr_text(&[0x5d; 20]), "source": "test: a later key", "doc": "test",
            "doc_row": "test", "from_day": crate::early::day_text(DAY + 2)
        }));
    std::fs::write(&o.exclusions, r.to_string()).expect("reviewed");
    // Days 1 and 2 rebuild from the deploy blocks to the same bytes.
    for dir in [&o.out, &o2.out] {
        verify(&quick(&f), &o.contracts, &o.exclusions, dir, None)
            .await
            .expect("earlier days unchanged");
    }
    // Day 3 adds the new key and removes the ended one.
    let o3 = out(&o, "v3");
    build(&quick(&f), &o3).await.expect("day 3");
    let x = read(&o3.out.join("exclusions").join("data.csv"));
    assert!(x.contains(&format!(
        ",{},add,operator_key,test: a later key\n",
        addr_text(&[0x5d; 20])
    )));
    assert!(x.contains(&format!(",{},remove,operator_key,", addr_text(&[0x5c; 20]))));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_contract_without_code_yet_has_no_role_that_day() {
    let (root, o) = world();
    // OnyxStorage is deployed in day 2: no code (and no role) on day 1.
    let storage_from = FROM + PER_DAY + 100;
    std::fs::write(&o.contracts, test_contracts_json(storage_from)).expect("contracts");
    let logs: Vec<ChainLog> = full_day()
        .into_iter()
        .filter(|l| l.address != STORAGE)
        .collect();
    let f = Fake::new(FROM + 2 * PER_DAY, logs);
    f.late
        .lock()
        .expect("lock")
        .insert(addr_text(&STORAGE), storage_from);
    build(&quick(&f), &o).await.expect("day 1");
    let x1 = read(&o.out.join("exclusions").join("data.csv"));
    assert!(!x1.contains("OnyxStorage.treasury()"), "{x1}");
    let o2 = out(&o, "v2");
    build(&quick(&f), &o2).await.expect("day 2");
    let x2 = read(&o2.out.join("exclusions").join("data.csv"));
    assert!(x2.contains(",add,role,OnyxStorage.treasury()\n"), "{x2}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_log_from_another_fork_is_refused() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    f.stale_hash_at.store(106, Ordering::SeqCst);
    let e = build(&quick(&f), &o).await.expect_err("refused");
    assert!(e.to_string().contains("mixes forks"), "{e}");
    assert!(!o.out.exists());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_deploy_transaction_must_name_the_contract_at_its_block() {
    let (tx1, tx2) = (
        format!("0x{}", "a1".repeat(32)),
        format!("0x{}", "a2".repeat(32)),
    );
    let mut v: Value = serde_json::from_str(&test_contracts_json(FROM)).expect("json");
    v["contracts"][0]["deploy_tx"] = json!(tx1);
    let d = v["contracts"][1].as_object_mut().expect("disputes");
    d.remove("block_doc");
    d.remove("block_quote");
    d.insert("scan_from_is".into(), json!("bound"));
    d.insert("bound_by".into(), json!("OnyxSettlement"));
    d.insert("deploy_tx".into(), json!(tx2));
    let c = Contracts::parse(v.to_string().as_bytes()).expect("contracts");
    let (settle, disputes) = (
        addr_text(&SETTLE),
        addr_text(&crate::early::tests::DISPUTES),
    );
    async fn check(
        c: &Contracts,
        receipts: &[(&str, u64, &str)],
    ) -> Result<BTreeMap<String, u64>, ReadError> {
        let f = Fake::new(FROM + PER_DAY, vec![]);
        for (t, b, a) in receipts {
            f.receipts
                .lock()
                .expect("lock")
                .insert(t.to_string(), (*b, a.to_string()));
        }
        quick(&f).check_contracts(c, FROM + PER_DAY).await
    }
    let (t1, t2, s, d) = (
        tx1.as_str(),
        tx2.as_str(),
        settle.as_str(),
        disputes.as_str(),
    );
    let found = check(&c, &[(t1, FROM, s), (t2, FROM + 7, d)])
        .await
        .expect("ok");
    assert_eq!(
        (found["OnyxSettlement"], found["OnyxDisputes"]),
        (FROM, FROM + 7)
    );
    // A deploy block that is not the receipt's.
    assert!(check(&c, &[(t1, FROM + 1, s), (t2, FROM + 7, d)])
        .await
        .is_err());
    // A bound above the real deploy block (events would be missed).
    assert!(check(&c, &[(t1, FROM, s), (t2, FROM - 3, d)])
        .await
        .is_err());
    // A receipt that created another address.
    assert!(check(&c, &[(t1, FROM, d), (t2, FROM + 7, d)])
        .await
        .is_err());
}

#[tokio::test]
async fn a_second_run_on_the_state_is_refused_while_one_holds_it() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    let held = lock_state(&o.state).expect("lock");
    let e = build(&quick(&f), &o).await.expect_err("locked");
    assert!(e.to_string().contains("another early-record run"), "{e}");
    drop(held);
    assert!(matches!(
        build(&quick(&f), &o).await.expect("day"),
        Built::Day { .. }
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_pending_read_for_other_contracts_is_dropped() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    std::fs::create_dir_all(&o.state).expect("dir");
    let stale = Pending {
        contracts: format!("0x{}", "00".repeat(32)),
        from: FROM,
        to: FROM + PER_DAY - 1,
        pass: 1,
        next: FROM + 20_000,
        ..Pending::default()
    };
    std::fs::write(
        o.state.join("pending.json"),
        serde_json::to_vec(&stale).expect("json"),
    )
    .expect("pending");
    build(&quick(&f), &o).await.expect("day");
    assert_eq!(
        f.log_calls.lock().expect("lock")[0].0,
        FROM,
        "read from the day's start"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_changed_day_json_or_schema_is_refused() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    build(&quick(&f), &o).await.expect("day");
    for file in ["day.json", "schema.json", "exclusions/schema.json"] {
        let p = o.out.join(file);
        let keep = read(&p);
        std::fs::write(&p, keep.replacen('"', "\"x", 1)).expect("tamper");
        assert!(
            matches!(build(&quick(&f), &o).await, Err(ReadError::Folder(_))),
            "{file}"
        );
        assert!(
            verify(&quick(&f), &o.contracts, &o.exclusions, &o.out, None)
                .await
                .is_err(),
            "{file}"
        );
        std::fs::write(&p, keep).expect("restore");
    }
    assert_eq!(
        build(&quick(&f), &o).await.expect("again"),
        Built::AlreadyBuilt(DAY)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_day_without_blocks_is_reported() {
    let (root, o) = world();
    // The chain halts for a whole day: block FROM + PER_DAY is on DAY + 2.
    let mut f = Fake::new(FROM + 2 * PER_DAY, full_day());
    f.gap = (FROM + PER_DAY, 86_400);
    build(&quick(&f), &o).await.expect("day 1");
    let b = build(&quick(&f), &out(&o, "v2")).await.expect("day 3");
    assert!(
        matches!(&b, Built::Day { day, empty_days, .. } if *day == DAY + 2 && empty_days == &vec![DAY + 1]),
        "{b:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// THREATS EARLY-4 (review of slice 3): a node that leaves out a log whose
/// row comes only on a later day (a No bet of day 1 that wins on day 2)
/// writes day 1's CSV byte for byte as an honest node does. The state hash
/// in day.json still differs, so `verify` of day 1 on an honest node fails;
/// and a reader rebuilding day 2 from the deploy blocks finds the lost row.
#[tokio::test]
async fn a_dropped_log_whose_row_comes_later_fails_verify_on_its_own_day() {
    let b2 = FROM + PER_DAY;
    let mut logs = full_day();
    logs.extend([
        opened(120, 0, 3, OPENER),
        bet(121, 0, 3, BETTOR_NO, 2, 7),
        resolved(b2 + 10, 0, 3, 2),
    ]);
    let honest = Fake::new(FROM + 2 * PER_DAY, logs.clone());
    let lossy = Fake::new(
        FROM + 2 * PER_DAY,
        logs.into_iter().filter(|l| l.block != 121).collect(),
    );
    let (root, o) = world();
    let (hroot, ho) = world();
    build(&quick(&lossy), &o).await.expect("lossy day 1");
    build(&quick(&honest), &ho).await.expect("honest day 1");
    assert_eq!(
        read(&o.out.join("data.csv")),
        read(&ho.out.join("data.csv"))
    );
    assert_ne!(
        read(&o.out.join("day.json")),
        read(&ho.out.join("day.json"))
    );
    let e = verify(&quick(&honest), &o.contracts, &o.exclusions, &o.out, None).await;
    assert!(
        matches!(&e, Err(ReadError::Folder(m)) if m.contains("day.json differs")),
        "{e:?}"
    );
    let ok = verify(
        &quick(&honest),
        &ho.contracts,
        &ho.exclusions,
        &ho.out,
        None,
    )
    .await
    .expect("honest day 1");
    assert_eq!(ok.day, DAY);
    // Day 2 of the lossy node lacks the correction_found row; rebuilt from the
    // deploy blocks on an honest node, it differs.
    let o2 = BuildOpts {
        prev: Some(o.out.clone()),
        ..out(&o, "v2")
    };
    build(&quick(&lossy), &o2).await.expect("lossy day 2");
    assert!(!read(&o2.out.join("data.csv")).contains(",correction_found,"));
    assert!(verify(
        &quick(&honest),
        &o2.contracts,
        &o2.exclusions,
        &o2.out,
        None
    )
    .await
    .is_err());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(hroot);
}

/// The exclusions' built version is checked too: `verify` compares each
/// shard root of `exclusions/manifest.bin` with the exclusions rows.
#[tokio::test]
async fn verify_compares_the_exclusions_shard_roots() {
    let (root, o) = world();
    let f = Fake::new(FROM + PER_DAY, full_day());
    build(&quick(&f), &o).await.expect("day");
    let x = o.out.join("exclusions");
    let csv = read(&x.join("data.csv"));
    let schema = crate::early::exclusions_schema();
    let m = manifest_of(
        &schema,
        crate::early::EXCLUSIONS_DATASET,
        &csv,
        None,
        T0 + 86_400,
    );
    std::fs::write(x.join("manifest.bin"), m.to_bytes()).expect("manifest");
    let v = verify(&quick(&f), &o.contracts, &o.exclusions, &o.out, None)
        .await
        .expect("verify");
    assert_eq!((v.shards, v.exclusion_shards), (0, 1));
    let other = csv.replacen(",add,", ",remove,", 1);
    let bad = manifest_of(
        &schema,
        crate::early::EXCLUSIONS_DATASET,
        &other,
        None,
        T0 + 86_400,
    );
    std::fs::write(x.join("manifest.bin"), bad.to_bytes()).expect("manifest");
    let e = verify(&quick(&f), &o.contracts, &o.exclusions, &o.out, None).await;
    assert!(
        matches!(&e, Err(ReadError::Folder(m)) if m.contains("root differs")),
        "{e:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

// ------------------------------------------------------------------ a newer list

/// The hashes [`two_built_days`] gave before a list could name an earlier
/// one (recorded 2026-11-19 at commit d97dd5cb): for each of the two days
/// `data.csv`, `day.json`, `exclusions/data.csv` and the join's state.
const BEFORE: [&str; 8] = [
    "0xf4c8a92a3021b81953c644943824a90e15aaef0ed884157b0318ba0c558db7c2",
    "0x09165ffbe2f67048448f9fe121f5c42b8b5d7012459e22d60015c25c69b9588d",
    "0x46646bb542c6add5adb7277f35db8026c684e2770d39deb14494e6b72a7d1343",
    "0x4c4b5a3d4c734b3a32fa8b8430550eaf8b9800087b341a07767bb2548e9e9ee5",
    "0x84c2bc0d3894c3b7dd718982c9f6d7c650cf27fc4d655c0ee0ec577929575399",
    "0x14027305995cabea4dd7424dc58f581d2a2fa821aef233d24802deb3d6d4dee9",
    "none",
    "0xdd458eff9fa41a09d5552c84f36d9644fe4fd20c00ee28aa5746e2cd5524fa66",
];

/// Put a newer list in `o`'s folder: the list so far becomes
/// `contracts-1.json`, and `contracts.json` names it and adds the
/// `storage-v4` contract from block `v4_from`, in force from `from_day`.
fn install_list_2(o: &BuildOpts, from_day: u32, v4_from: u64) {
    let dir = o.contracts.parent().expect("dir");
    std::fs::write(dir.join("contracts-1.json"), test_contracts_json(FROM)).expect("list 1");
    std::fs::write(&o.contracts, list_2_json(from_day, v4_from)).expect("list 2");
}

/// keccak256 (`0x`) of each file of two built days and of the join's state
/// after each (`newer`: with a newer list installed that starts later).
async fn two_built_days(newer: bool) -> Vec<String> {
    let (root, o) = world();
    if newer {
        install_list_2(&o, DAY + 2, FROM + PER_DAY + 100);
    }
    let f = Fake::new(FROM + 2 * PER_DAY, two_days());
    let mut got = Vec::new();
    let mut prev: Option<PathBuf> = None;
    for name in ["v1", "v2"] {
        let o = BuildOpts {
            prev: prev.clone(),
            ..out(&o, name)
        };
        assert!(matches!(
            build(&quick(&f), &o).await.expect("day"),
            Built::Day { .. }
        ));
        for file in ["data.csv", "day.json", "exclusions/data.csv"] {
            got.push(match std::fs::read(o.out.join(file)) {
                Ok(b) => hash_bytes(&b),
                Err(_) => "none".to_string(),
            });
        }
        got.push(state_hash(&tool_of(&o).record));
        prev = Some(o.out.clone());
    }
    let _ = std::fs::remove_dir_all(root);
    got
}

/// Days built before this change keep every byte: with the list alone, and
/// with a newer list installed whose first day is later.
#[tokio::test]
async fn days_built_before_the_list_could_change_keep_their_bytes() {
    assert_eq!(
        hash_bytes(test_contracts_json(FROM).as_bytes()),
        "0xb1f91817af7a4289167321990fcceda8f2e8c8a85b094a0f1db53c2fe8a839c9",
        "the test list itself is the one the hashes were recorded with"
    );
    assert_eq!(two_built_days(false).await, BEFORE);
    assert_eq!(two_built_days(true).await, BEFORE);
}

/// Three days: the second storage contract is deployed on day 2 and has
/// work on days 2 and 3; the first one has work on every day.
fn three_days(v4_from: u64) -> Vec<ChainLog> {
    let (node, node4) = (Hash32([0x81; 32]), Hash32([0x84; 32]));
    let b3 = FROM + 2 * PER_DAY;
    let mut v = two_days();
    v.extend([
        v4(activated(v4_from, 0, node4, MINER_A)),
        v4(deal(v4_from, 1, 0, PAYER)),
        v4(answered(v4_from + 5, 0, 0, 2, 0, node4, true, 7)),
        answered(v4_from + 6, 0, 1, 4, 0, node, true, 6),
        v4(answered(b3 + 1, 0, 0, 3, 0, node4, true, 8)),
        answered(b3 + 2, 0, 1, 5, 0, node, true, 6),
    ]);
    v
}

fn folder(o: &BuildOpts) -> Vec<String> {
    ["data.csv", "day.json", "schema.json"]
        .iter()
        .map(|f| read(&o.out.join(f)))
        .collect()
}

/// The roll-out as it happens: days 1 and 2 are built with the first list
/// (the new contract is simply not read), then the newer list is installed
/// with a `from_day` of day 3. Day 3's version holds the new contract's
/// rows from its deploy block on; every day verifies from the deploy blocks
/// with the new files; and a state that had the newer list from the start
/// writes the same three folders.
#[tokio::test]
async fn a_newer_list_counts_the_new_contract_from_its_deploy_block() {
    let v4_from = FROM + PER_DAY + 100;
    let four = addr_text(&STORAGE4);
    let f = Fake::new(FROM + 3 * PER_DAY, three_days(v4_from));
    f.late.lock().expect("lock").insert(four.clone(), v4_from);
    let (root, o1) = world();
    let o2 = BuildOpts {
        prev: Some(o1.out.clone()),
        ..out(&o1, "v2")
    };
    let o3 = BuildOpts {
        prev: Some(o2.out.clone()),
        ..out(&o1, "v3")
    };
    build(&quick(&f), &o1).await.expect("day 1");
    build(&quick(&f), &o2).await.expect("day 2");
    // The first list does not name the new contract: nothing of it is read.
    assert!(!read(&o2.out.join("data.csv")).contains(&four));
    assert_eq!(
        read(&o2.out.join("data.csv"))
            .matches(",storage_proof,")
            .count(),
        2
    );
    let old_hash = tool_of(&o1).record.contracts.clone();

    install_list_2(&o1, DAY + 2, v4_from);
    let list = Contracts::load(&o1.contracts).expect("list 2");
    assert!(matches!(
        build(&quick(&f), &o3).await.expect("day 3"),
        Built::Day { day, .. } if day == DAY + 2
    ));
    let csv = read(&o3.out.join("data.csv"));
    let proofs: Vec<&str> = csv
        .lines()
        .filter(|l| l.contains(",storage_proof,"))
        .collect();
    // The new contract's proof of day 2 (late), then day 3's two proofs.
    assert_eq!(proofs.len(), 3, "{csv}");
    assert!(proofs[0].contains(&four) && proofs[0].contains(&format!(",{},0,", v4_from + 5)));
    assert!(proofs[1].contains(&four) && proofs[2].contains(&addr_text(&STORAGE)));
    assert!(proofs.iter().all(|l| l.contains(",2026-11-16,")));
    // The first storage contract's proof of day 2 is in day 2's version only.
    assert!(!csv.contains(&format!(",{},0,", v4_from + 6)));
    let day: Value = serde_json::from_str(&read(&o3.out.join("day.json"))).expect("day.json");
    assert_eq!(day["contracts_hash"], json!(hx32(&list.hash)));
    assert_eq!(day["from_block"], json!(FROM + 2 * PER_DAY));
    let tool = tool_of(&o1);
    assert_eq!(tool.record.contracts, hx32(&list.hash));
    assert_ne!(tool.record.contracts, old_hash);
    assert_eq!(
        tool.deploy_blocks
            .as_ref()
            .and_then(|d| d.get("OnyxStorage (v4)")),
        Some(&v4_from)
    );

    // Every day verifies with the new files: from the deploy blocks, and day
    // 3 also from the state saved before it.
    for o in [&o1, &o2, &o3] {
        verify(&quick(&f), &o.contracts, &o.exclusions, &o.out, None)
            .await
            .expect("from the deploy blocks");
    }
    let before = o1.state.join("before").join("2026-11-16.json");
    let v = verify(
        &quick(&f),
        &o3.contracts,
        &o3.exclusions,
        &o3.out,
        Some(&before),
    )
    .await
    .expect("from the state before");
    assert_eq!((v.day, v.rows), (DAY + 2, csv.lines().count() - 1));

    // The newer list installed from the start: the same folders.
    let (root2, p1) = world();
    install_list_2(&p1, DAY + 2, v4_from);
    let p2 = BuildOpts {
        prev: Some(p1.out.clone()),
        ..out(&p1, "v2")
    };
    let p3 = BuildOpts {
        prev: Some(p2.out.clone()),
        ..out(&p1, "v3")
    };
    for (p, o) in [(&p1, &o1), (&p2, &o2), (&p3, &o3)] {
        build(&quick(&f), p).await.expect("day");
        assert_eq!(folder(p), folder(o));
    }
    assert_eq!(
        read(&p1.state.join("state.json")),
        read(&o1.state.join("state.json"))
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

/// A newer list whose first day is already built with the list before it
/// is refused: a replay would give that day other bytes. So is a new
/// contract with no code at its deploy block.
#[tokio::test]
async fn a_newer_list_must_start_after_the_last_built_day() {
    let v4_from = FROM + PER_DAY + 100;
    let f = Fake::new(FROM + 3 * PER_DAY, three_days(v4_from));
    f.late
        .lock()
        .expect("lock")
        .insert(addr_text(&STORAGE4), v4_from);
    let (root, o1) = world();
    let o2 = BuildOpts {
        prev: Some(o1.out.clone()),
        ..out(&o1, "v2")
    };
    let o3 = BuildOpts {
        prev: Some(o2.out.clone()),
        ..out(&o1, "v3")
    };
    build(&quick(&f), &o1).await.expect("day 1");
    build(&quick(&f), &o2).await.expect("day 2");
    let state = read(&o1.state.join("state.json"));
    install_list_2(&o1, DAY + 1, v4_from);
    let e = build(&quick(&f), &o3).await;
    assert!(
        matches!(&e, Err(ReadError::Contracts(m)) if m.contains("from_day after the last built day")),
        "{e:?}"
    );
    assert!(!o3.out.exists());
    assert_eq!(read(&o1.state.join("state.json")), state);
    // With a first day after the last built one, but a wrong deploy block.
    install_list_2(&o1, DAY + 2, v4_from - 1);
    let e = build(&quick(&f), &o3).await;
    assert!(
        matches!(&e, Err(ReadError::Contracts(m)) if m.contains("no code at its deploy block")),
        "{e:?}"
    );
    assert_eq!(read(&o1.state.join("state.json")), state);
    // An earlier list whose bytes changed: the file is refused.
    install_list_2(&o1, DAY + 2, v4_from);
    let other = test_contracts_json(FROM + 1);
    let first = o1.contracts.with_file_name("contracts-1.json");
    std::fs::write(&first, &other).expect("another list 1");
    let e = build(&quick(&f), &o3).await;
    assert!(
        matches!(&e, Err(ReadError::Record(EarlyError::Contracts(m))) if m.contains("never changes")),
        "{e:?}"
    );
    // A state of a list that is not in the file's history.
    std::fs::write(&o1.contracts, list_2_json_of(&other, DAY + 2, v4_from)).expect("list 2");
    let e = build(&quick(&f), &o3).await;
    assert!(
        matches!(&e, Err(ReadError::Contracts(m)) if m.contains("neither")),
        "{e:?}"
    );
    assert_eq!(read(&o1.state.join("state.json")), state);
    let _ = std::fs::remove_dir_all(root);
}
