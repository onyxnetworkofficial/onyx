//! The storage network's node side (ADR-0060): the chain view decoded from
//! a fake `OnyxStorage` (views and logs ABI-encoded as the contract emits
//! them), the storage node's fetch, check and answer, and the client read's
//! fail-over (THREATS STORAGE-4, STORAGE-8). Shards are real bulk shards
//! served by a real `BulkStore` (the publisher's node).

use super::*;
use crate::bulk_serve::tests::{
    published, published_keys, sign, widened, world, world_format2, World,
};
use crate::bulk_serve::{parse_rows_query, BulkStore};
use onyx_storage::abi::{encode, uint, Tok};
use onyx_storage::ids::{deal_salt, node_id};
use onyx_storage::placement::WHOLE_DEAL;

/// Review STORAGE-R1-3: a source that comes back after a few failed passes
/// in the same epoch: nothing declined, the shards held.
#[tokio::test]
async fn a_short_outage_declines_nothing() {
    let s = setup(5).await;
    let me = s.nodes[0];
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let down = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: [("http://publisher".to_string(), Peer::Missing)]
            .into_iter()
            .collect(),
        calls: Mutex::new(Vec::new()),
    });
    let tx = Arc::new(Sent::default());
    let cfg = StorageNodeConfig {
        contract: CONTRACT,
        node: me,
        fetch: true,
        keeper: false,
        fetches_per_tick: 64,
    };
    let node = StorageNode::new(reader(&s), tx.clone(), down, own.store.clone(), cfg.clone());
    for _ in 0..4 {
        node.tick().await.expect("tick");
    }
    let up = net(&s, &[("http://publisher", Peer::Good)]);
    let node2 = StorageNode::new(reader(&s), tx.clone(), up, own.store.clone(), cfg);
    let r = node2.tick().await.expect("tick");
    assert_eq!(r.held, r.wanted);
    assert!(tx
        .0
        .lock()
        .expect("lock")
        .iter()
        .all(|d| d[..4] != abi::selector("decline(uint64,uint32,bytes32)")));
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

const CONTRACT: [u8; 20] = [0x5e; 20];

fn word_tok(h: &Hash32) -> Tok {
    Tok::Word(h.0)
}

fn topic_u64(v: u64) -> Hash32 {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    Hash32(w)
}

#[derive(Clone)]
struct Fake {
    epoch: u64,
    block: u64,
    nodes: Vec<(NodeId, String)>,
    deal: DealInfo,
    seeds: BTreeMap<u64, Hash32>,
    slashed: Vec<(NodeId, u64, u32, u64)>,
    /// `Declined` logs: (node, deal, index, raw from).
    declined: Vec<(NodeId, u64, u32, u64)>,
    /// `Answered` logs: (deal, epoch, node, slot, shard, passed).
    answered: Vec<(u64, u64, NodeId, u32, u32, bool)>,
    /// What `challengedHolders(deal, epoch, slot, nodes)` returns (none: it reverts).
    asks: BTreeMap<(u64, u64, u32), (u32, Vec<NodeId>)>,
    status: BTreeMap<(u64, u64, u32, NodeId), u8>,
    /// Node 0 registered these bytes as its URL (review STORAGE-R1-1).
    junk_url: Option<Vec<u8>>,
    /// The RPC's quirks (review STORAGE-R3-1).
    quirks: Quirks,
    /// `eth_chainId`.
    chain_id: u64,
}

/// How the fake RPC misbehaves.
#[derive(Clone, Default)]
struct Quirks {
    /// The next this many `eth_getLogs` calls answer `[]` (a lagging backend).
    empty_logs: usize,
    /// `eth_getLogs` ignores the asked range.
    ignore_range: bool,
    /// Changes every block hash (a reorg).
    fork: u8,
    /// Only this many more `eth_getLogs` calls are answered; the rest fail
    /// with HTTP 429 (a public endpoint's rate limit, THREATS STORAGE-S15-4).
    answer_logs: Option<usize>,
    /// The contract is from before ADR-0060 amendment 2 (no
    /// `RULES_VERSION()`: the call reverts).
    v0: bool,
    /// No call is ever answered (a node that hangs; review OBS-STORAGE-R1 5).
    hang: bool,
    /// Only this many more `eth_getLogs` calls are answered; the rest never
    /// return (a node killed part way through a read, THREATS
    /// STORAGE-RESTART-1).
    hang_logs: Option<usize>,
    /// A backend behind the chain (review STORAGE-RESTART-R1 M3): blocks
    /// above this one are unknown (null), as blocks above the head always are.
    known_to: Option<u64>,
    /// The backend's safe block is this many blocks lower.
    safe_lag: u64,
}

/// The safe block of the fake chain: 64 below the head.
fn safe(head: u64) -> u64 {
    head.saturating_sub(64)
}

/// The block every fake log is in.
const LOG_BLOCK: u64 = 0x10;

struct FakeChain(Mutex<Fake>, Mutex<Calls>);

/// What the node asked the fake chain.
#[derive(Default)]
struct Calls {
    /// (fromBlock, toBlock) of every `eth_getLogs`.
    log_ranges: Vec<(u64, u64)>,
    /// `dealInfo` calls.
    deal_infos: usize,
    /// (deal, epoch, slot) of every `challengedHolders` call.
    holders_asked: Vec<(u64, u64, u32)>,
}

fn log(event: &str, topics: Vec<Hash32>, data: Vec<u8>) -> Value {
    let mut t = vec![abi::topic(event).to_string()];
    t.extend(topics.iter().map(|h| h.to_string()));
    json!({"topics": t, "data": hex0x(&data), "blockNumber": format!("0x{LOG_BLOCK:x}")})
}

impl JsonRpc for FakeChain {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let f = self.0.lock().expect("lock").clone();
        if f.quirks.hang {
            return Box::pin(std::future::pending());
        }
        if method == "eth_getLogs" {
            let mut g = self.0.lock().expect("lock");
            match g.quirks.hang_logs.as_mut() {
                Some(0) => return Box::pin(std::future::pending()),
                Some(n) => *n -= 1,
                None => {}
            }
        }
        let r = (|| -> Result<Value, String> {
            Ok(match method {
                "eth_blockNumber" => json!(format!("0x{:x}", f.block)),
                "eth_chainId" => json!(format!("0x{:x}", f.chain_id)),
                "eth_getBlockByNumber" => {
                    let tag = p[0].as_str().unwrap_or_default();
                    let n = if tag == "safe" {
                        safe(f.block).saturating_sub(f.quirks.safe_lag)
                    } else {
                        qty(&p[0])?
                    };
                    if n > f.block || f.quirks.known_to.is_some_and(|k| n > k) {
                        return Ok(Value::Null);
                    }
                    let mut h = [f.quirks.fork; 32];
                    h[24..].copy_from_slice(&n.to_be_bytes());
                    json!({"number": format!("0x{n:x}"), "hash": Hash32(h).to_string()})
                }
                "eth_getLogs" => {
                    let from = qty(&p[0]["fromBlock"])?;
                    let to = qty(&p[0]["toBlock"])?;
                    let lie = {
                        let mut g = self.0.lock().map_err(|_| "lock")?;
                        match g.quirks.answer_logs.as_mut() {
                            Some(0) => return Err("eth_getLogs: node answered HTTP 429".into()),
                            Some(n) => *n -= 1,
                            None => {}
                        }
                        if g.quirks.empty_logs > 0 {
                            g.quirks.empty_logs -= 1;
                            true
                        } else {
                            false
                        }
                    };
                    if let Ok(mut c) = self.1.lock() {
                        c.log_ranges.push((from, to));
                    }
                    if lie || (!f.quirks.ignore_range && !(from..=to).contains(&LOG_BLOCK)) {
                        return Ok(json!([]));
                    }
                    // One call asks for any of several events (topic 0 is a list).
                    let asked: Vec<String> = match &p[0]["topics"][0] {
                        Value::Array(a) => a
                            .iter()
                            .map(|t| t.as_str().unwrap_or_default().to_string())
                            .collect(),
                        t => vec![t.as_str().unwrap_or_default().to_string()],
                    };
                    let ids: Vec<NodeId> = f.nodes.iter().map(|n| n.0).collect();
                    let mut out = Vec::new();
                    for topic in &asked {
                        if *topic == abi::topic(events::NODE_SET).to_string() {
                            out.push(log(
                                events::NODE_SET,
                                vec![topic_u64(1)],
                                encode(&[
                                    word_tok(&set_hash(&ids)),
                                    Tok::Words(ids.iter().map(|n| n.0 .0).collect()),
                                ]),
                            ));
                        } else if *topic == abi::topic(events::REGISTERED).to_string() {
                            for (i, (_, url)) in f.nodes.iter().enumerate() {
                                let bytes = match (&f.junk_url, i) {
                                    (Some(j), 0) => j.clone(),
                                    _ => url.as_bytes().to_vec(),
                                };
                                out.push(log(
                                    events::REGISTERED,
                                    vec![topic_u64(i as u64), topic_u64(0)],
                                    encode(&[uint(100), uint(1), Tok::Bytes(bytes)]),
                                ));
                            }
                        } else if *topic == abi::topic(events::ACTIVATED).to_string() {
                            for (i, (id, _)) in f.nodes.iter().enumerate() {
                                out.push(log(
                                    events::ACTIVATED,
                                    vec![id.0, topic_u64(i as u64), topic_u64(0)],
                                    vec![],
                                ));
                            }
                        } else if *topic == abi::topic(events::SLASHED).to_string() {
                            for (n, d, idx, from) in &f.slashed {
                                out.push(log(
                                    events::SLASHED,
                                    vec![n.0, topic_u64(*d)],
                                    encode(&[
                                        uint(3),
                                        uint(0),
                                        uint((*idx).into()),
                                        uint(5),
                                        uint((*from).into()),
                                        uint(2),
                                    ]),
                                ));
                            }
                        } else if *topic == abi::topic(events::DECLINED).to_string() {
                            for (n, d, idx, from) in &f.declined {
                                out.push(log(
                                    events::DECLINED,
                                    vec![n.0, topic_u64(*d)],
                                    encode(&[uint((*idx).into()), uint((*from).into())]),
                                ));
                            }
                        } else if *topic == abi::topic(events::ANSWERED).to_string() {
                            for (d, e, n, slot, shard, passed) in &f.answered {
                                out.push(log(
                                    events::ANSWERED,
                                    vec![topic_u64(*d), topic_u64(*e), n.0],
                                    encode(&[
                                        uint((*slot).into()),
                                        uint((*shard).into()),
                                        uint(u128::from(*passed)),
                                        uint(if *passed { 7 } else { 0 }),
                                    ]),
                                ));
                            }
                        } else if *topic == abi::topic(events::DEAL_CREATED).to_string() {
                            let d = &f.deal;
                            out.push(log(
                                events::DEAL_CREATED,
                                vec![topic_u64(0), topic_u64(1)],
                                encode(&[
                                    word_tok(&d.manifest_root),
                                    word_tok(&d.shards_root),
                                    uint(d.shard_count.into()),
                                    uint(d.byte_count.into()),
                                    uint(d.r.into()),
                                    uint(d.start_epoch.into()),
                                    uint(d.end_epoch.into()),
                                    word_tok(&d.salt),
                                    uint(d.deposit),
                                    Tok::Bytes(b"example.publisher".to_vec()),
                                    Tok::Bytes(b"synthetic.metrics".to_vec()),
                                    Tok::Bytes(b"http://publisher".to_vec()),
                                ]),
                            ));
                        }
                    }
                    json!(out)
                }
                "eth_call" => {
                    let data = hexb(&p[0]["data"])?;
                    let sel: [u8; 4] = data[..4].try_into().map_err(|_| "sel")?;
                    let arg = |i: usize| -> [u8; 32] {
                        data[4 + 32 * i..36 + 32 * i].try_into().unwrap_or([0; 32])
                    };
                    let u =
                        |i: usize| u64::from_be_bytes(arg(i)[24..].try_into().unwrap_or([0; 8]));
                    let out = if sel == abi::selector("RULES_VERSION()") {
                        if f.quirks.v0 {
                            return Err("execution reverted".into());
                        }
                        encode(&[uint(3)])
                    } else if sel
                        == abi::selector("challengedHolders(uint64,uint64,uint32,bytes32[])")
                    {
                        let k = (u(0), u(1), u(2) as u32);
                        if let Ok(mut c) = self.1.lock() {
                            c.holders_asked.push(k);
                        }
                        let Some((i, who)) = f.asks.get(&k) else {
                            return Err("execution reverted: NoSeed".into());
                        };
                        encode(&[
                            uint((*i).into()),
                            Tok::Words(who.iter().map(|n| n.0 .0).collect()),
                        ])
                    } else if sel == abi::selector("currentEpoch()") {
                        encode(&[uint(f.epoch.into())])
                    } else if sel == abi::selector("params()") {
                        encode(&[
                            uint(10),
                            uint(1_000),
                            uint(1),
                            uint(1_000_000),
                            uint(100_000),
                            uint(2),
                            uint(3),
                            uint(0),
                        ])
                    } else if sel == abi::selector("nodeInfo(bytes32)") {
                        encode(&[
                            uint(1),
                            uint(1),
                            uint(100),
                            uint(1_000_000),
                            uint(0),
                            uint(0),
                            uint(0),
                            uint(0),
                        ])
                    } else if sel == abi::selector("dealInfo(uint64)") {
                        if let Ok(mut c) = self.1.lock() {
                            c.deal_infos += 1;
                        }
                        let d = &f.deal;
                        encode(&[
                            abi::addr(&d.payer),
                            word_tok(&d.manifest_root),
                            word_tok(&d.shards_root),
                            word_tok(&d.salt),
                            uint(d.shard_count.into()),
                            uint(d.r.into()),
                            uint(d.byte_count.into()),
                            uint(d.start_epoch.into()),
                            uint(d.end_epoch.into()),
                            uint(d.deposit),
                            uint(d.paid),
                            uint(u128::from(d.closed)),
                        ])
                    } else if sel == abi::selector("dealCount()") {
                        encode(&[uint(1)])
                    } else if sel == abi::selector("registrationCount()") {
                        encode(&[uint(f.nodes.len() as u128)])
                    } else if sel == abi::selector("seedOf(uint64)") {
                        encode(&[word_tok(
                            &f.seeds.get(&u(0)).copied().unwrap_or(Hash32::ZERO),
                        )])
                    } else if sel == abi::selector("setHashAt(uint64)") {
                        let ids: Vec<NodeId> = f.nodes.iter().map(|n| n.0).collect();
                        encode(&[word_tok(&set_hash(&ids))])
                    } else if sel == abi::selector("epochStart(uint64)") {
                        encode(&[uint((u(0) * 10).into())])
                    } else if sel == abi::selector("exclusionFrom(uint64,uint32,bytes32)") {
                        // From the same records the fake's logs carry, as of
                        // the asked block (every log is at `LOG_BLOCK`).
                        let mut x = Exclusions::new();
                        let at = match &p[1] {
                            Value::String(t) if t.starts_with("0x") => qty(&p[1]).ok(),
                            Value::Object(o) => {
                                o.get("blockHash").and_then(|h| h32(h).ok()).map(|h| {
                                    u64::from_be_bytes(h.0[24..].try_into().unwrap_or([0; 8]))
                                })
                            }
                            _ => None,
                        };
                        let before = at.is_some_and(|b| b < LOG_BLOCK);
                        let (slashed, declined, answered) = if before {
                            (Vec::new(), Vec::new(), Vec::new())
                        } else {
                            (f.slashed.clone(), f.declined.clone(), f.answered.clone())
                        };
                        for (n, d, idx, from) in &slashed {
                            x.slash(*d, *idx, *n, *from);
                        }
                        for (n, d, idx, from) in &declined {
                            x.decline(*d, *idx, *n, *from);
                        }
                        for (d, e, n, _, shard, passed) in &answered {
                            if *passed {
                                x.pass(*d, *shard, *n, *e);
                            }
                        }
                        let from = x.from(u(0), u(1) as u32, &NodeId(Hash32(arg(2))));
                        encode(&[uint(from.unwrap_or(0).into())])
                    } else if sel == abi::selector("slotStatus(uint64,uint64,uint32,bytes32)") {
                        let k = (u(0), u(1), u(2) as u32, NodeId(Hash32(arg(3))));
                        encode(&[uint(f.status.get(&k).copied().unwrap_or(0).into())])
                    } else {
                        return Err("unknown call".into());
                    };
                    json!(hex0x(&out))
                }
                _ => return Err(format!("unexpected {method}")),
            })
        })();
        Box::pin(async move { r })
    }
}

/// How a URL behaves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Peer {
    Good,
    Down,
    Missing,
    Forged,
    /// Serves a real shard of the dataset, but another one.
    Other,
}

struct Net {
    publisher: Arc<BulkStore>,
    manifest_root: Hash32,
    peers: BTreeMap<String, Peer>,
    calls: Mutex<Vec<String>>,
}

impl HttpGet for Net {
    fn get<'a>(
        &'a self,
        url: &'a str,
        _max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            if let Ok(mut c) = self.calls.lock() {
                c.push(url.to_string());
            }
            let base = url.split("/v1/").next().unwrap_or_default();
            let path = &url[base.len()..];
            let behave = self.peers.get(base).copied().unwrap_or(Peer::Down);
            match behave {
                Peer::Down => return Err("connection refused".into()),
                Peer::Missing if !path.starts_with("/v1/bulk/manifests/") => {
                    return Ok((503, b"{}".to_vec()))
                }
                _ => {}
            }
            if let Some(root) = path.strip_prefix("/v1/bulk/manifests/") {
                let root = crate::parse_hash(root, "root").map_err(|e| e.message)?.0;
                let Some((m, r)) = self.publisher.manifest_record(&root) else {
                    return Ok((404, vec![]));
                };
                let v = json!({"manifest": hex0x(&m.to_bytes()), "report": hex0x(&borsh::to_vec(&r).unwrap_or_default())});
                return Ok((200, serde_json::to_vec(&v).unwrap_or_default()));
            }
            if let Some(root) = path.strip_prefix("/v1/bulk/shards/") {
                let root = crate::parse_hash(root, "root").map_err(|e| e.message)?.0;
                let root = if behave == Peer::Other {
                    let m = self
                        .publisher
                        .manifest_record(&self.manifest_root)
                        .ok_or("manifest")?
                        .0;
                    let i = m.shards.iter().position(|e| e.root == root).unwrap_or(0);
                    m.shards[(i + 1) % m.shards.len()].root
                } else {
                    root
                };
                let Some(mut b) = self.publisher.shard_file(&root).await else {
                    return Ok((503, vec![]));
                };
                if behave == Peer::Forged {
                    let n = b.len();
                    b[n - 1] ^= 1;
                }
                return Ok((200, b));
            }
            if let Some(q) = path.strip_prefix("/v1/bulk/example.publisher/synthetic.metrics/rows?")
            {
                let q = parse_rows_query(Some(q)).map_err(|e| e.message)?;
                let mut v = self
                    .publisher
                    .rows("example.publisher", "synthetic.metrics", &q)
                    .await
                    .map_err(|e| e.message)?;
                if behave == Peer::Forged {
                    let r = v["rows"][0]["row"].as_str().unwrap_or_default().to_string();
                    let flip = if r.ends_with('0') { "1" } else { "0" };
                    v["rows"][0]["row"] = json!(format!("{}{flip}", &r[..r.len() - 1]));
                }
                return Ok((200, serde_json::to_vec(&v).unwrap_or_default()));
            }
            Ok((404, vec![]))
        })
    }
}

#[derive(Default)]
struct Sent(Mutex<Vec<Vec<u8>>>);

impl TxSend for Sent {
    fn address(&self) -> [u8; 20] {
        [0xAA; 20]
    }
    fn send<'a>(
        &'a self,
        _to: &'a [u8; 20],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        if let Ok(mut s) = self.0.lock() {
            s.push(data);
        }
        Box::pin(async { Ok("0x01".to_string()) })
    }
}

struct Setup {
    publisher: World,
    manifest: Manifest,
    chain: Arc<FakeChain>,
    nodes: Vec<NodeId>,
}

async fn setup(n: usize) -> Setup {
    let publisher = world();
    let (m, _) = published(&publisher).await;
    setup_with(n, publisher, m)
}

/// `setup` for a dataset of `shards` shards (10 rows each).
async fn setup_shards(n: usize, shards: i128) -> Setup {
    let publisher = world();
    let (m, _) = published_keys(&publisher, 0..shards * 10).await;
    setup_with(n, publisher, m)
}

fn setup_with(n: usize, publisher: World, m: Manifest) -> Setup {
    let nodes: Vec<(NodeId, String)> = (0..n)
        .map(|i| {
            (
                node_id(&[0x0d; 20], i as u64, &Hash32([i as u8; 32])),
                format!("http://node{i}"),
            )
        })
        .collect();
    let deal = DealInfo {
        payer: [1; 20],
        manifest_root: m.header.root(),
        shards_root: m.header.shards_root,
        salt: deal_salt(&Hash32([9; 32]), 0),
        shard_count: m.header.shard_count,
        r: 3,
        byte_count: m.header.byte_count,
        start_epoch: 2,
        end_epoch: 10,
        deposit: 1_000_000,
        paid: 0,
        closed: false,
    };
    let ids = nodes.iter().map(|x| x.0).collect();
    let chain = Arc::new(FakeChain(
        Mutex::new(Fake {
            epoch: 3,
            block: 31,
            nodes,
            deal,
            seeds: BTreeMap::new(),
            slashed: Vec::new(),
            declined: Vec::new(),
            answered: Vec::new(),
            asks: BTreeMap::new(),
            status: BTreeMap::new(),
            junk_url: None,
            quirks: Quirks::default(),
            chain_id: 84_532,
        }),
        Mutex::new(Calls::default()),
    ));
    Setup {
        publisher,
        manifest: m,
        chain,
        nodes: ids,
    }
}

fn reader(s: &Setup) -> StorageChain {
    StorageChain::new(s.chain.clone(), CONTRACT, 0)
}

#[tokio::test]
async fn snapshot_decodes_the_contracts_logs_and_views() {
    let s = setup(5).await;
    s.chain
        .0
        .lock()
        .expect("lock")
        .slashed
        .push((s.nodes[2], 0, 1, 4));
    let snap = reader(&s).snapshot().await.expect("snapshot");
    assert_eq!(snap.epoch, 3);
    assert_eq!(snap.list_at(2), s.nodes);
    assert_eq!(snap.list_at(0), Vec::<NodeId>::new());
    assert_eq!(snap.url(&s.nodes[4]), Some("http://node4"));
    let d = snap.deal(0).expect("deal");
    assert_eq!(d.publisher, "example.publisher");
    assert_eq!(d.dataset, "synthetic.metrics");
    assert_eq!(d.source_url, "http://publisher");
    assert_eq!(d.info.manifest_root, s.manifest.header.root());
    assert_eq!(snap.excl.from(0, 1, &s.nodes[2]), Some(4));
    assert_eq!(snap.params.money.challenges_per_deal, 2);
    // Every shard has R holders; sources list holders then the publisher.
    for i in 0..3 {
        assert_eq!(snap.holders(d, i, 3).len(), 3);
        let src = sources(&snap, d, i, None);
        assert_eq!(src.last().map(String::as_str), Some("http://publisher"));
    }
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R1-1: a registration with bytes that are no URL (here not
/// even UTF-8) must not fail the chain read every node runs each pass; the
/// node's URL is just empty.
#[tokio::test]
async fn a_junk_url_never_stops_the_snapshot() {
    let s = setup(5).await;
    s.chain.0.lock().expect("lock").junk_url = Some(vec![0xff, 0xfe, 0x00]);
    let snap = reader(&s).snapshot().await.expect("still reads");
    assert_eq!(snap.url(&s.nodes[0]), None);
    assert_eq!(snap.url(&s.nodes[1]), Some("http://node1"));
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

fn net(s: &Setup, peers: &[(&str, Peer)]) -> Arc<Net> {
    Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: peers.iter().map(|(u, b)| (u.to_string(), *b)).collect(),
        calls: Mutex::new(Vec::new()),
    })
}

/// A seed is recorded only for an epoch in which a deal is checked: with the
/// only deal ended (or closed), the node sends nothing (no gas spent on a seed
/// that asks nobody).
#[tokio::test]
async fn no_seed_is_recorded_when_no_deal_is_checked() {
    let s = setup(5).await;
    let me = s.nodes[0];
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let net = net(&s, &[("http://publisher", Peer::Good)]);
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        net,
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    // Epoch 3, in the seed window; the deal ran epochs 2..3 and has ended.
    s.chain.0.lock().expect("lock").deal.end_epoch = 3;
    node.tick().await.expect("tick");
    assert!(
        tx.0.lock().expect("lock").is_empty(),
        "a seed was sent for an epoch with no deal"
    );
    // The same epoch with the deal running: the seed is recorded.
    s.chain.0.lock().expect("lock").deal.end_epoch = 10;
    node.tick().await.expect("tick");
    let sent = tx.0.lock().expect("lock").clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0][..4], abi::selector("recordSeed(uint64)"));
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

#[tokio::test]
async fn a_storage_node_fetches_checks_and_proves_only_its_shards() {
    let s = setup(5).await;
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let me = s.nodes[0];
    let mine = snap.wanted(&me);
    // The node's own store: the book there knows the publisher's anchor.
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    // Every other node is down, one peer forges: the publisher serves.
    let mut peers = vec![("http://publisher", Peer::Good)];
    peers.push(("http://node1", Peer::Forged));
    let net = net(&s, &peers);
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        net.clone(),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    let r = node.tick().await.expect("tick");
    assert_eq!(r.wanted, mine.len());
    assert_eq!(r.held, mine.len());
    assert_eq!(r.fetched, mine.len());
    for (_, i) in &mine {
        assert!(own.store.holds_shard(&s.manifest.shards[*i as usize].root));
    }
    for e in &s.manifest.shards {
        if !mine.iter().any(|(_, i)| *i == e.index) {
            assert!(
                !own.store.holds_shard(&e.root),
                "fetched a shard not placed here"
            );
        }
    }
    // The epoch had no seed: the node recorded it (one recordSeed sent).
    let sent = tx.0.lock().expect("lock").clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0][..4], abi::selector("recordSeed(uint64)"));
    // With a seed, it answers each slot it is challenged in, with a proof
    // the contract's rule accepts.
    let seed = Hash32([0x33; 32]);
    s.chain.0.lock().expect("lock").seeds.insert(3, seed);
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    let mut want = 0;
    for slot in 0..2 {
        if snap.challenged(&d, 3, &seed, slot).1.contains(&me) {
            want += 1;
        }
    }
    let r = node.tick().await.expect("tick");
    assert_eq!(r.answered, want);
    let sent = tx.0.lock().expect("lock").clone();
    let answers: Vec<&Vec<u8>> = sent
        .iter()
        .filter(|d| d[..4] == abi::selector("answer(uint64,uint64,uint32,bytes32,bytes32[],bytes,bytes32[],bytes,bytes32[])"))
        .collect();
    assert_eq!(answers.len(), want);
    for a in answers {
        let b = &a[4..];
        let slot = abi::word_u64(b, 2).expect("slot") as u32;
        let entry = challenge::parse_entry(&abi::dyn_bytes(b, 5).expect("entry")).expect("entry");
        let p = onyx_storage::StorageProof {
            entry,
            entry_siblings: abi::dyn_words(b, 6).expect("es"),
            row: abi::dyn_bytes(b, 7).expect("row"),
            row_siblings: abi::dyn_words(b, 8).expect("rs"),
        };
        // The row drawn for this node (ADR-0060 amendment 1 item 1): valid for it.
        challenge::verify(
            &d.info.shards_root,
            d.info.shard_count,
            &seed,
            0,
            slot,
            &me,
            &p,
        )
        .expect("valid");
        assert_eq!(
            abi::dyn_words(b, 4).expect("nodes"),
            s.nodes.iter().map(|n| n.0).collect::<Vec<_>>()
        );
    }
    // A second tick sends nothing new (answered slots are remembered).
    node.tick().await.expect("tick");
    assert_eq!(tx.0.lock().expect("lock").len(), sent.len());
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

#[tokio::test]
async fn a_forged_shard_from_a_peer_is_refused_and_another_source_used() {
    let s = setup(4).await;
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let me = s.nodes[0];
    let d = snap.deal(0).expect("deal").clone();
    let (_, i) = snap.wanted(&me)[0];
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    // Every other holder forges; the publisher is honest.
    let mut peers: Vec<(String, Peer)> = sources(&snap, &d, i, Some(&me))
        .into_iter()
        .map(|u| (u, Peer::Forged))
        .collect();
    peers.retain(|(u, _)| u != "http://publisher");
    peers.push(("http://publisher".into(), Peer::Good));
    let n = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: peers.into_iter().collect(),
        calls: Mutex::new(Vec::new()),
    });
    let node = StorageNode::new(
        reader(&s),
        Arc::new(Sent::default()),
        n.clone(),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    let m = node
        .ensure_manifest(&snap, &d, None)
        .await
        .expect("manifest");
    node.fetch_shard(&snap, &d, &m, i, None)
        .await
        .expect("fetched from the publisher");
    assert!(own.store.holds_shard(&m.shards[i as usize].root));
    let calls = n.calls.lock().expect("lock").clone();
    assert!(calls
        .last()
        .is_some_and(|u| u.starts_with("http://publisher")));
    // Only forgers: refused, nothing stored.
    let own2 = world();
    own2.node.anchor(&sign(&s.manifest));
    let bad = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: sources(&snap, &d, i, Some(&me))
            .into_iter()
            .map(|u| (u, Peer::Forged))
            .collect(),
        calls: Mutex::new(Vec::new()),
    });
    let node2 = StorageNode::new(
        reader(&s),
        Arc::new(Sent::default()),
        bad,
        own2.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    own2.store
        .accept_manifest(&m.to_bytes(), &borsh::to_vec(&sign(&m)).expect("ok"))
        .await
        .expect("manifest");
    let e = node2
        .fetch_shard(&snap, &d, &m, i, None)
        .await
        .expect_err("refused");
    assert!(e.contains("refused"), "{e}");
    assert!(!own2.store.holds_shard(&m.shards[i as usize].root));
    for w in [own, own2] {
        let _ = std::fs::remove_dir_all(&w.dir);
    }
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

#[tokio::test]
async fn a_client_read_fails_over_past_down_missing_and_forging_replicas() {
    let s = setup(5).await;
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    // Shard 1 holds keys 10..=19. Its first source is down, the second
    // lacks it, the third forges a row; the publisher answers.
    let src = sources(&snap, &d, 1, None);
    let mut peers: Vec<(String, Peer)> = Vec::new();
    for (k, u) in src.iter().enumerate() {
        let b = match k {
            0 => Peer::Down,
            1 => Peer::Missing,
            _ if u == "http://publisher" => Peer::Good,
            _ => Peer::Forged,
        };
        peers.push((u.clone(), b));
    }
    let n = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: peers.iter().cloned().collect(),
        calls: Mutex::new(Vec::new()),
    });
    let r = read_rows(&reader(&s), n.as_ref(), 0, 12, 15)
        .await
        .expect("read");
    assert_eq!(
        r.rows.iter().map(|x| x.2).collect::<Vec<_>>(),
        vec![12, 13, 14, 15]
    );
    assert_eq!(r.served_by, vec![(1, "http://publisher".to_string())]);
    assert_eq!(r.refused.len(), src.len() - 1);
    assert!(
        r.refused.iter().any(|(_, why)| why.contains("refused")
            || why.contains("proof")
            || why.contains("row")),
        "{:?}",
        r.refused
    );
    // Everyone down: unavailable, never wrong rows.
    let none = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: [("http://publisher".to_string(), Peer::Good)]
            .into_iter()
            .collect(),
        calls: Mutex::new(Vec::new()),
    });
    // The manifest is still found at the publisher, but rows only from forgers fail.
    let forgers = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: src.iter().map(|u| (u.clone(), Peer::Forged)).collect(),
        calls: Mutex::new(Vec::new()),
    });
    let e = read_rows(&reader(&s), forgers.as_ref(), 0, 12, 15)
        .await
        .expect_err("refused");
    assert!(matches!(e, ReadError::Unavailable { shard: 1, .. }), "{e}");
    // Review STORAGE-R1-4: a replica serving another real shard of the same
    // dataset is refused, never read as this shard's rows.
    let other = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: src.iter().map(|u| (u.clone(), Peer::Other)).collect(),
        calls: Mutex::new(Vec::new()),
    });
    let e = read_rows(&reader(&s), other.as_ref(), 0, 12, 15)
        .await
        .expect_err("refused");
    assert!(e.to_string().contains("not the deal's shard"), "{e}");
    let ok = read_rows(&reader(&s), none.as_ref(), 0, 0, 29)
        .await
        .expect("publisher alone");
    assert_eq!(ok.rows.len(), 30);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

#[tokio::test]
async fn a_node_sends_nothing_to_an_older_contract() {
    // ADR-0060 amendment 2 (review STORAGE-S16-R6 H1): an S15 contract draws
    // rows without the node id and an amendment 1 contract has no decline
    // notice, so this node's answers would fail there or its view of who is
    // asked would differ. Neither has `RULES_VERSION()`: it must send nothing.
    let s = setup(5).await;
    s.chain.0.lock().expect("lock").quirks.v0 = true;
    let own = world();
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        net(&s, &[]),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[0],
            fetch: true,
            keeper: true,
            fetches_per_tick: 64,
        },
    );
    let err = node.tick().await.expect_err("an older contract is refused");
    assert!(
        err.contains("rules v3") && err.contains("exit its node"),
        "{err}"
    );
    assert!(tx.0.lock().expect("lock").is_empty(), "nothing sent");
    // The same node runs once the contract answers as amendment 2 does.
    s.chain.0.lock().expect("lock").quirks.v0 = false;
    node.tick().await.expect("tick");
}

/// THREATS STORAGE-16: when no source serves a shard placed on the node (a
/// publisher or replicas withholding it), the node declines that shard after
/// three passes instead of waiting to be slashed for it.
#[tokio::test]
async fn a_node_declines_shards_nobody_serves() {
    let s = setup(5).await;
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let me = s.nodes[0];
    let mine = snap.wanted(&me);
    assert!(!mine.is_empty());
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    // The manifest is served, every shard file is not.
    let mut peers: Vec<(String, Peer)> = vec![("http://publisher".into(), Peer::Missing)];
    for i in 1..5 {
        peers.push((format!("http://node{i}"), Peer::Missing));
    }
    let n = Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: peers.into_iter().collect(),
        calls: Mutex::new(Vec::new()),
    });
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        n,
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 64,
        },
    );
    let declines = |tx: &Sent| {
        tx.0.lock()
            .expect("lock")
            .iter()
            .filter(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
            .count()
    };
    // Review STORAGE-R1-3: four failed passes inside one epoch decline nothing.
    for pass in 1..=4 {
        let r = node.tick().await.expect("tick");
        assert_eq!(r.fetch_failed, mine.len(), "pass {pass}");
        assert_eq!(declines(&tx), 0, "not within one epoch");
    }
    // Still nothing served in the next epoch: after three failed passes there
    // the whole deal is given up in one transaction (nothing of it is held and
    // every try failed).
    s.chain.0.lock().expect("lock").epoch = 4;
    for _ in 0..2 {
        node.tick().await.expect("tick");
        assert_eq!(declines(&tx), 0, "review STORAGE-R2-4: not after two");
    }
    let r = node.tick().await.expect("tick");
    assert_eq!(r.declined, 1);
    assert_eq!(declines(&tx), 1);
    let sent = tx.0.lock().expect("lock").clone();
    let d = sent
        .iter()
        .find(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
        .expect("decline");
    let b = &d[4..];
    assert_eq!(abi::word_u64(b, 0).expect("deal"), 0);
    assert_eq!(abi::word_u64(b, 1).expect("index") as u32, WHOLE_DEAL);
    assert_eq!(abi::word(b, 2).expect("node"), me.0 .0);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// One log of fetches and transactions in the order they happened.
struct Ordered {
    net: Arc<Net>,
    delay: Duration,
    log: Mutex<Vec<String>>,
}

impl HttpGet for Ordered {
    fn get<'a>(
        &'a self,
        url: &'a str,
        max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            if let Ok(mut l) = self.log.lock() {
                l.push(format!("get {url}"));
            }
            tokio::time::sleep(self.delay).await;
            self.net.get(url, max).await
        })
    }
}

impl TxSend for Ordered {
    fn address(&self) -> [u8; 20] {
        [0xAA; 20]
    }
    fn send<'a>(
        &'a self,
        _to: &'a [u8; 20],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        if let Ok(mut l) = self.log.lock() {
            l.push(format!("tx {}", hex::encode(&data[..4])));
        }
        Box::pin(async { Ok("0x01".to_string()) })
    }
}

/// Review STORAGE-R1-2: a slow source (every fetch here takes 100 s) never
/// delays this epoch's answers: they are sent before the first fetch starts.
#[tokio::test(start_paused = true)]
async fn answers_go_out_before_any_slow_fetch() {
    let s = setup(5).await;
    let seed = Hash32([0x33; 32]);
    s.chain.0.lock().expect("lock").seeds.insert(3, seed);
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    // A node challenged this epoch that also has a placed shard to fetch.
    let (me, asked) = s
        .nodes
        .iter()
        .find_map(|n| {
            let asked: BTreeSet<u32> = (0..2)
                .filter_map(|slot| {
                    let (i, who) = snap.challenged(&d, 3, &seed, slot);
                    who.contains(n).then_some(i)
                })
                .collect();
            let wanted: BTreeSet<u32> = snap.wanted(n).into_iter().map(|x| x.1).collect();
            (!asked.is_empty() && wanted.difference(&asked).next().is_some()).then_some((*n, asked))
        })
        .expect("a challenged node with a shard to fetch");
    // It holds the manifest and the challenged shards already.
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    own.store
        .accept_manifest(
            &s.manifest.to_bytes(),
            &borsh::to_vec(&sign(&s.manifest)).expect("report"),
        )
        .await
        .expect("manifest");
    for i in &asked {
        let e = &s.manifest.shards[*i as usize];
        let body = s.publisher.store.shard_file(&e.root).await.expect("file");
        own.store
            .put_shard("example.publisher", "synthetic.metrics", &e.root, body)
            .await
            .expect("shard");
    }
    let io = Arc::new(Ordered {
        net: net(&s, &[("http://publisher", Peer::Good)]),
        delay: Duration::from_secs(100),
        log: Mutex::new(Vec::new()),
    });
    let node = StorageNode::new(
        reader(&s),
        io.clone(),
        io.clone(),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    let r = node.tick().await.expect("tick");
    assert_eq!(r.answered, asked.len());
    assert!(r.fetched >= 1, "{r:?}");
    let log = io.log.lock().expect("lock").clone();
    let first_get = log
        .iter()
        .position(|x| x.starts_with("get"))
        .expect("a fetch");
    let last_tx = log
        .iter()
        .rposition(|x| x.starts_with("tx"))
        .expect("answers");
    assert!(last_tx < first_get, "{log:?}");
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

fn withholding(s: &Setup) -> Arc<Net> {
    // Manifests are served, no shard file is.
    let mut peers: Vec<(String, Peer)> = vec![("http://publisher".into(), Peer::Missing)];
    for i in 0..s.nodes.len() {
        peers.push((format!("http://node{i}"), Peer::Missing));
    }
    Arc::new(Net {
        publisher: s.publisher.store.clone(),
        manifest_root: s.manifest.header.root(),
        peers: peers.into_iter().collect(),
        calls: Mutex::new(Vec::new()),
    })
}

fn decline_count(tx: &Sent) -> usize {
    tx.0.lock()
        .expect("lock")
        .iter()
        .filter(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
        .count()
}

/// Review STORAGE-R2-4: one failed pass at the end of an epoch and two at the
/// start of the next give nothing up.
#[tokio::test]
async fn a_boundary_outage_declines_nothing() {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        withholding(&s),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[0],
            fetch: true,
            keeper: false,
            fetches_per_tick: 64,
        },
    );
    node.tick().await.expect("tick");
    s.chain.0.lock().expect("lock").epoch = 4;
    node.tick().await.expect("tick");
    node.tick().await.expect("tick");
    assert_eq!(decline_count(&tx), 0);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R2-1: a deal whose shards are all withheld is given up whole
/// even when a pass tries only some of its shards (here 2 of the 3 placed).
#[tokio::test]
async fn a_withheld_deal_is_declined_whole_when_only_some_shards_are_tried() {
    let s = setup(3).await;
    let me = s.nodes[0];
    let snap = reader(&s).snapshot().await.expect("snapshot");
    assert_eq!(
        snap.wanted(&me).len(),
        3,
        "R = N = 3: every shard is placed here"
    );
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        withholding(&s),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 1,
        },
    );
    let r = node.tick().await.expect("tick");
    assert_eq!(r.fetch_failed, 2, "two tries a pass");
    s.chain.0.lock().expect("lock").epoch = 4;
    for _ in 0..3 {
        node.tick().await.expect("tick");
    }
    assert_eq!(decline_count(&tx), 1);
    let sent = tx.0.lock().expect("lock").clone();
    let d = sent
        .iter()
        .find(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
        .expect("decline");
    assert_eq!(abi::word_u64(&d[4..], 1).expect("index") as u32, WHOLE_DEAL);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Partial withholding: the node holds one shard of a deal and no source
/// serves the others. Declined one by one (three failed passes each in an
/// epoch) thousands of withheld shards would take many epochs of misses; with
/// more failing shards than a pass can try, the deal is given up whole at once.
#[tokio::test]
async fn a_partly_withheld_deal_is_declined_whole() {
    let s = setup_shards(3, 8).await;
    let me = s.nodes[0];
    let snap = reader(&s).snapshot().await.expect("snapshot");
    assert_eq!(
        snap.wanted(&me).len(),
        8,
        "R = N = 3: every shard is placed here"
    );
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    own.store
        .accept_manifest(
            &s.manifest.to_bytes(),
            &borsh::to_vec(&sign(&s.manifest)).expect("report"),
        )
        .await
        .expect("manifest");
    // The node already holds shard 0; nobody serves shards 1 to 7.
    let e = &s.manifest.shards[0];
    let body = s.publisher.store.shard_file(&e.root).await.expect("file");
    own.store
        .put_shard("example.publisher", "synthetic.metrics", &e.root, body)
        .await
        .expect("shard");
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        withholding(&s),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 1,
        },
    );
    // Epoch 3: two tries a pass, so all 7 missing shards have failed once
    // after four passes; nothing is declined within one epoch.
    for _ in 0..4 {
        let r = node.tick().await.expect("tick");
        assert_eq!(r.fetch_failed, 2, "two tries a pass");
    }
    assert_eq!(decline_count(&tx), 0, "not within one epoch");
    // Epoch 4: once a shard is due (three failed passes here), 7 failing
    // shards are more than a pass can try (2): one decline of the whole deal.
    s.chain.0.lock().expect("lock").epoch = 4;
    let mut passes = 0;
    while decline_count(&tx) == 0 {
        passes += 1;
        assert!(passes <= 12, "declined within the epoch");
        node.tick().await.expect("tick");
    }
    assert_eq!(decline_count(&tx), 1);
    let sent = tx.0.lock().expect("lock").clone();
    let d = sent
        .iter()
        .find(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
        .expect("decline");
    assert_eq!(abi::word_u64(&d[4..], 1).expect("index") as u32, WHOLE_DEAL);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// A few failing shards among many held ones are still declined one by one:
/// the whole deal is given up only past what a pass can try.
#[tokio::test]
async fn a_few_withheld_shards_are_declined_one_by_one() {
    let s = setup_shards(3, 8).await;
    let me = s.nodes[0];
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    own.store
        .accept_manifest(
            &s.manifest.to_bytes(),
            &borsh::to_vec(&sign(&s.manifest)).expect("report"),
        )
        .await
        .expect("manifest");
    // The node holds shards 0 to 5; nobody serves 6 and 7.
    for e in &s.manifest.shards[..6] {
        let body = s.publisher.store.shard_file(&e.root).await.expect("file");
        own.store
            .put_shard("example.publisher", "synthetic.metrics", &e.root, body)
            .await
            .expect("shard");
    }
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        withholding(&s),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: true,
            keeper: false,
            fetches_per_tick: 1,
        },
    );
    node.tick().await.expect("tick");
    s.chain.0.lock().expect("lock").epoch = 4;
    for _ in 0..3 {
        node.tick().await.expect("tick");
    }
    let sent = tx.0.lock().expect("lock").clone();
    let mut idx: Vec<u32> = sent
        .iter()
        .filter(|d| d[..4] == abi::selector("decline(uint64,uint32,bytes32)"))
        .map(|d| abi::word_u64(&d[4..], 1).expect("index") as u32)
        .collect();
    idx.sort_unstable();
    assert_eq!(idx, vec![6, 7]);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R2-1: deals take turns, so a deal with many unservable
/// shards never starves a later one; the first deal moves each pass.
#[test]
fn deals_take_turns_in_the_fetch_order() {
    let mut q = BTreeMap::new();
    q.insert(0u64, (0..500).collect::<Vec<u32>>());
    q.insert(1u64, vec![0, 1, 2]);
    let first: Vec<(u64, u32)> = fetch_order(&q, 0).into_iter().take(6).collect();
    assert_eq!(first, vec![(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)]);
    assert_eq!(fetch_order(&q, 1)[0], (1, 0));
    assert_eq!(fetch_order(&q, 0).len(), 503);
    assert!(fetch_order(&BTreeMap::new(), 3).is_empty());
}

/// Review STORAGE-R2-3: every peer call takes 120 s and nothing is served; the
/// pass still ends within the fetch budget plus one call, and the manifest is
/// asked only from the source and the deal's holders, not every node.
#[tokio::test(start_paused = true)]
async fn a_slow_network_cannot_stretch_a_pass() {
    let s = setup(12).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let io = Arc::new(Ordered {
        net: net(&s, &[]),
        delay: Duration::from_secs(120),
        log: Mutex::new(Vec::new()),
    });
    let node = StorageNode::new(
        reader(&s),
        io.clone(),
        io.clone(),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[0],
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    let t0 = tokio::time::Instant::now();
    node.tick().await.expect("tick");
    assert!(
        t0.elapsed() <= FETCH_BUDGET + Duration::from_secs(120),
        "{:?}",
        t0.elapsed()
    );
    let gets: Vec<String> = io
        .log
        .lock()
        .expect("lock")
        .iter()
        .filter(|x| x.starts_with("get"))
        .cloned()
        .collect();
    assert!(gets.len() <= 3, "{gets:?}");
    assert!(gets[0].contains("http://publisher/"), "{gets:?}");
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R1-8: logs of safe blocks are read once and kept; a
/// later pass asks only for the newer blocks, and a closed deal is not asked
/// about again.
#[tokio::test]
async fn a_pass_reads_only_new_logs_and_skips_closed_deals() {
    let s = setup(5).await;
    s.chain.0.lock().expect("lock").block = 20_000;
    let first = reader(&s);
    let snap = first.snapshot().await.expect("snapshot");
    assert_eq!(snap.deals.len(), 1);
    assert_eq!(snap.nodes.len(), 5);
    s.chain.1.lock().expect("lock").log_ranges.clear();
    s.chain.0.lock().expect("lock").block = 20_010;
    let again = first.snapshot().await.expect("snapshot");
    assert_eq!(again.deals.len(), 1, "the kept logs are used");
    assert_eq!(again.lists, snap.lists);
    let ranges = s.chain.1.lock().expect("lock").log_ranges.clone();
    assert!(!ranges.is_empty());
    assert!(
        ranges.iter().all(|(f, _)| *f > safe(20_000)),
        "only the newer blocks: {ranges:?}"
    );
    // A closed deal is dropped and never asked about again.
    s.chain.0.lock().expect("lock").deal.closed = true;
    let closed = first.snapshot().await.expect("snapshot");
    assert!(closed.deals.is_empty());
    let n = s.chain.1.lock().expect("lock").deal_infos;
    first.snapshot().await.expect("snapshot");
    assert_eq!(s.chain.1.lock().expect("lock").deal_infos, n);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R3-1: a backend that answers `[]` for the old blocks once is
/// caught by the contract's counts and the logs are read again; the snapshot
/// is right.
#[tokio::test]
async fn a_lagging_rpc_answer_is_caught_by_the_counts() {
    let s = setup(5).await;
    // The calls one clean read makes.
    s.chain.0.lock().expect("lock").block = 20_000;
    reader(&s).snapshot().await.expect("snapshot");
    let per_read = std::mem::take(&mut s.chain.1.lock().expect("lock").log_ranges).len();
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.empty_logs = 1_000;
    }
    // Every call lies: the counts never match, so no wrong snapshot is returned.
    let r = reader(&s);
    assert!(r.snapshot().await.is_err());
    // Exactly the first read lies: the retry is right.
    s.chain.0.lock().expect("lock").quirks.empty_logs = per_read;
    let snap = r.snapshot().await.expect("the second read is right");
    assert_eq!(snap.deals.len(), 1);
    assert_eq!(snap.nodes.len(), 5);
    assert_eq!(snap.list_at(snap.epoch).len(), 5);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R3-1: an RPC that ignores the asked range never fills the
/// kept logs twice, and a reorg under the kept logs (the safe block's
/// hash changed) drops them.
#[tokio::test]
async fn ranges_are_enforced_and_a_reorg_drops_the_kept_logs() {
    let s = setup(5).await;
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.ignore_range = true;
    }
    let r = reader(&s);
    for k in 0..3u64 {
        s.chain.0.lock().expect("lock").block = 20_000 + k;
        let snap = r.snapshot().await.expect("snapshot");
        assert_eq!(snap.deals.len(), 1);
        assert_eq!(snap.registrations, 5);
    }
    let kept = r
        .cache
        .lock()
        .expect("lock")
        .logs
        .as_ref()
        .map_or(0, |(_, l, _)| l.len());
    assert_eq!(
        kept,
        1 + 5 + 5 + 1,
        "one NodeSet, five Registered, five Activated, one DealCreated"
    );
    s.chain.1.lock().expect("lock").log_ranges.clear();
    s.chain.0.lock().expect("lock").quirks.fork = 7;
    r.snapshot().await.expect("snapshot");
    let ranges = s.chain.1.lock().expect("lock").log_ranges.clone();
    assert!(
        ranges.iter().any(|(f, _)| *f == 0),
        "read again from the deploy block: {ranges:?}"
    );
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-S15-1: sepolia.base.org refuses more than 200 blocks per
/// `eth_getLogs`. A snapshot asks for all its events in one call per range of
/// at most 200 blocks (every block once, not once per event), and the next
/// pass reads only the blocks above the kept safe block: two calls.
#[tokio::test]
async fn one_call_per_range_of_at_most_200_blocks_for_every_event() {
    let s = setup(5).await;
    s.chain.0.lock().expect("lock").block = 20_000;
    let r = reader(&s);
    let snap = r.snapshot().await.expect("snapshot");
    assert_eq!((snap.deals.len(), snap.nodes.len()), (1, 5));
    let ranges = std::mem::take(&mut s.chain.1.lock().expect("lock").log_ranges);
    assert!(
        ranges.iter().all(|(f, t)| t >= f && t - f < LOG_RANGE),
        "{ranges:?}"
    );
    let blocks: u64 = ranges.iter().map(|(f, t)| t - f + 1).sum();
    assert_eq!(blocks, 20_001, "blocks 0..=20000, each asked once");
    s.chain.0.lock().expect("lock").block = 20_010;
    r.snapshot().await.expect("snapshot");
    let again = std::mem::take(&mut s.chain.1.lock().expect("lock").log_ranges);
    assert_eq!(
        again,
        vec![(safe(20_000) + 1, safe(20_010)), (safe(20_010) + 1, 20_010)]
    );
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-S15-4: five nodes on one host reading 70,000 blocks from
/// the public endpoint got HTTP 429 part way; every failed pass dropped the
/// logs kept so far and began again at the deploy block, so most nodes never
/// finished a first read. A read cut off by the node keeps every range it got
/// and the next pass goes on from there: each block is answered once.
#[tokio::test]
async fn a_rate_limited_first_read_goes_on_where_it_stopped() {
    let s = setup(5).await;
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.answer_logs = Some(40);
    }
    let r = reader(&s);
    let e = r.snapshot().await.expect_err("cut off");
    assert!(e.contains("HTTP 429"), "{e}");
    let kept = |r: &StorageChain| r.cache.lock().expect("lock").logs.as_ref().map(|l| l.0);
    assert_eq!(
        kept(&r),
        Some(40 * LOG_RANGE),
        "the 40 answered ranges are kept"
    );
    s.chain.0.lock().expect("lock").quirks.answer_logs = Some(30);
    r.snapshot().await.expect_err("cut off again");
    assert_eq!(kept(&r), Some(70 * LOG_RANGE));
    s.chain.0.lock().expect("lock").quirks.answer_logs = None;
    let snap = r.snapshot().await.expect("the third pass finishes");
    assert_eq!((snap.deals.len(), snap.nodes.len()), (1, 5));
    assert_eq!(snap.list_at(snap.epoch).len(), 5);
    let ranges = std::mem::take(&mut s.chain.1.lock().expect("lock").log_ranges);
    let blocks: u64 = ranges.iter().map(|(f, t)| t - f + 1).sum();
    assert_eq!(blocks, 20_001, "blocks 0..=20000, each answered once");
    // A 429 after the first read leaves the kept logs as they were.
    let before = kept(&r);
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_400;
        f.quirks.answer_logs = Some(0);
    }
    r.snapshot().await.expect_err("no answer");
    assert_eq!(kept(&r), before);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R2-5: the placement key changes exactly when an input of
/// the placement does, so a node can keep its shard list in between.
#[tokio::test]
async fn the_placement_key_follows_its_inputs() {
    let s = setup(5).await;
    let a = reader(&s).snapshot().await.expect("snapshot");
    let b = reader(&s).snapshot().await.expect("snapshot");
    assert_eq!(a.placement_key(), b.placement_key());
    s.chain
        .0
        .lock()
        .expect("lock")
        .slashed
        .push((s.nodes[1], 0, 0, 4));
    let c = reader(&s).snapshot().await.expect("snapshot");
    assert_ne!(a.placement_key(), c.placement_key());
    // A pass alone moves nothing (ADR-0060 amendment 2: passes come every epoch).
    s.chain
        .0
        .lock()
        .expect("lock")
        .answered
        .push((0, 3, s.nodes[2], 0, 1, true));
    let c2 = reader(&s).snapshot().await.expect("snapshot");
    assert_ne!(c.excl, c2.excl);
    assert_eq!(c.placement_key(), c2.placement_key());
    s.chain.0.lock().expect("lock").epoch = 4;
    let d = reader(&s).snapshot().await.expect("snapshot");
    assert_ne!(c.placement_key(), d.placement_key());
    // The cached list equals a fresh computation.
    let own = world();
    let node = StorageNode::new(
        reader(&s),
        Arc::new(Sent::default()),
        net(&s, &[]),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[1],
            fetch: false,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    assert_eq!(node.wanted(&d), d.wanted(&s.nodes[1]));
    assert_eq!(node.wanted(&d), d.wanted(&s.nodes[1]));
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// ADR-0060 amendment 2 item 2: the snapshot reads `Answered` with the
/// shard index (a passed one is a pass on that shard, a failed one is not)
/// and `Declined` with the raw epoch, and gives `exclusionFrom` with the
/// notice.
#[tokio::test]
async fn a_snapshot_reads_passes_and_declines_into_the_notice() {
    let s = setup(5).await;
    let n = &s.nodes;
    {
        let mut f = s.chain.0.lock().expect("lock");
        // Passed shard 1 at epoch 2 (slot 0), declined it at 3 (raw 4): notice, out from 5.
        f.answered.push((0, 2, n[1], 0, 1, true));
        f.declined.push((n[1], 0, 1, 4));
        // A failed answer is no pass: out from 4.
        f.answered.push((0, 2, n[2], 0, 2, false));
        f.declined.push((n[2], 0, 2, 4));
        // A whole-deal decline: notice only on the shard it passed (0, at slot 1).
        f.answered.push((0, 2, n[3], 1, 0, true));
        f.declined.push((n[3], 0, WHOLE_DEAL, 4));
        // A slash after a noticed decline wins.
        f.answered.push((0, 3, n[4], 1, 2, true));
        f.declined.push((n[4], 0, 2, 4));
        f.slashed.push((n[4], 0, 2, 4));
    }
    let snap = reader(&s).snapshot().await.expect("snapshot");
    assert_eq!(snap.excl.from(0, 1, &n[1]), Some(5));
    assert_eq!(snap.excl.from(0, 0, &n[1]), None);
    assert_eq!(snap.excl.from(0, 2, &n[2]), Some(4));
    assert_eq!(snap.excl.from(0, 0, &n[3]), Some(5));
    assert_eq!(snap.excl.from(0, 1, &n[3]), Some(4));
    assert_eq!(snap.excl.from(0, 2, &n[3]), Some(4));
    assert_eq!(snap.excl.from(0, 2, &n[4]), Some(4));
    let r = snap.excl.record(0, 0, &n[3]).expect("record");
    assert_eq!((r.decline_from, r.slash_from, r.last_pass), (0, 0, 2));
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-S16-R6 H1, defense in depth: the node's view says it is not
/// asked for a slot (here a slash the contract does not have), but it is in
/// the shard's top R with no exclusion. It asks the contract
/// (`challengedHolders`): when the contract lists it, it answers; when not, it
/// sends nothing and asks once for that slot and epoch.
#[tokio::test]
async fn a_node_answers_when_the_contract_asks_although_its_view_does_not() {
    let s = setup(5).await;
    let seed = Hash32([0x33; 32]);
    s.chain.0.lock().expect("lock").seeds.insert(3, seed);
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    let (index, who) = snap.challenged(&d, 3, &seed, 0);
    let me = who[0];
    // The drift: a decline sent in epoch 2 that the view sees without the pass
    // the contract saw (an `Answered` log the RPC dropped), so the view puts
    // the node out from epoch 3 and the contract, with the notice, from 4. A
    // slash has no notice, so a slash in the view is never asked about.
    s.chain
        .0
        .lock()
        .expect("lock")
        .declined
        .push((me, 0, index, 3));
    let drifted = reader(&s).snapshot().await.expect("snapshot");
    assert!(!drifted.challenged(&d, 3, &seed, 0).1.contains(&me));
    // Slots the drifted view still asks `me` for are answered as usual.
    let usual = (0..2u32)
        .filter(|slot| drifted.challenged(&d, 3, &seed, *slot).1.contains(&me))
        .count();
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    own.store
        .accept_manifest(
            &s.manifest.to_bytes(),
            &borsh::to_vec(&sign(&s.manifest)).expect("report"),
        )
        .await
        .expect("manifest");
    for e in &s.manifest.shards {
        let body = s.publisher.store.shard_file(&e.root).await.expect("file");
        own.store
            .put_shard("example.publisher", "synthetic.metrics", &e.root, body)
            .await
            .expect("shard");
    }
    let cfg = StorageNodeConfig {
        contract: CONTRACT,
        node: me,
        fetch: false,
        keeper: false,
        fetches_per_tick: 8,
    };
    let slot0 = |tx: &Sent| {
        tx.0.lock()
            .expect("lock")
            .iter()
            .filter(|x| x[..4] == abi::selector("answer(uint64,uint64,uint32,bytes32,bytes32[],bytes,bytes32[],bytes,bytes32[])"))
            .filter(|x| abi::word_u64(&x[4..], 2).expect("slot") == 0)
            .count()
    };
    let asked0 = |s: &Setup| {
        s.chain
            .1
            .lock()
            .expect("lock")
            .holders_asked
            .iter()
            .filter(|k| **k == (0, 3, 0))
            .count()
    };
    // The contract does not list it: nothing for slot 0, and it asks once.
    s.chain
        .0
        .lock()
        .expect("lock")
        .asks
        .insert((0, 3, 0), (index, who[1..].to_vec()));
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(&s),
        tx.clone(),
        net(&s, &[]),
        own.store.clone(),
        cfg.clone(),
    );
    node.tick().await.expect("tick");
    node.tick().await.expect("tick");
    assert_eq!(slot0(&tx), 0);
    assert_eq!(asked0(&s), 1, "asked once per slot and epoch");
    // The contract lists it: it answers slot 0 with a proof for its own row.
    s.chain
        .0
        .lock()
        .expect("lock")
        .asks
        .insert((0, 3, 0), (index, who.clone()));
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(reader(&s), tx.clone(), net(&s, &[]), own.store.clone(), cfg);
    let r = node.tick().await.expect("tick");
    assert_eq!(slot0(&tx), 1);
    assert_eq!(r.answered, usual + 1);
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// `books()` of an S15 contract has eight words (no `rewarded`); from
/// ADR-0060 amendment 1 it has nine. Anything else is refused.
#[test]
fn books_decode_eight_or_nine_words() {
    let words =
        |n: u64| -> Vec<u8> { encode(&(1..=n).map(|i| uint(i.into())).collect::<Vec<_>>()) };
    assert_eq!(
        decode_books(&words(9)).expect("nine"),
        [1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    assert_eq!(
        decode_books(&words(8)).expect("eight"),
        [1, 2, 3, 4, 5, 6, 7, 8, 0]
    );
    assert!(decode_books(&words(7)).is_err());
}

/// Review STORAGE-S16-R7 M1: `Answered` logs are folded into the last pass per
/// (deal, shard, node) as they are read, never kept; other logs are kept in
/// chain order.
#[test]
fn answered_logs_fold_into_the_last_pass_and_are_not_kept() {
    let t64 = |v: u64| {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&v.to_be_bytes());
        Hash32(w)
    };
    let node = NodeId(Hash32([7; 32]));
    let answered = |deal: u64, epoch: u64, index: u128, passed: u128| Log {
        topics: vec![abi::topic(events::ANSWERED), t64(deal), t64(epoch), node.0],
        data: encode(&[uint(1), uint(index), uint(passed), uint(0)]),
        block: epoch,
    };
    let declined = Log {
        topics: vec![abi::topic(events::DECLINED), node.0, t64(3)],
        data: encode(&[uint(2), uint(9)]),
        block: 9,
    };
    let logs = vec![
        answered(3, 5, 2, 1),
        declined.clone(),
        answered(3, 8, 2, 1),
        answered(3, 6, 2, 1),
        answered(3, 9, 2, 0),
        answered(4, 7, 0, 1),
    ];
    let (mut keep, mut passes) = (Vec::new(), Passes::new());
    fold_logs(logs, &mut keep, &mut passes).expect("fold");
    assert_eq!(keep.len(), 1, "only the Declined log is kept");
    assert_eq!(keep[0].topics[0], abi::topic(events::DECLINED));
    assert_eq!(passes.len(), 2);
    assert_eq!(
        passes[&(3, 2, node)],
        8,
        "the latest pass; a failed answer is not a pass"
    );
    assert_eq!(passes[&(4, 0, node)], 7);
}

/// Review STORAGE-S16-R7 L2: a node out of a shard whatever the notice (a
/// slash) never asks the contract about it.
#[tokio::test]
async fn a_slashed_node_does_not_ask_the_contract() {
    let s = setup(5).await;
    let seed = Hash32([0x33; 32]);
    s.chain.0.lock().expect("lock").seeds.insert(3, seed);
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    let (index, who) = snap.challenged(&d, 3, &seed, 0);
    let me = who[0];
    s.chain
        .0
        .lock()
        .expect("lock")
        .slashed
        .push((me, 0, index, 3));
    let own = world();
    let node = StorageNode::new(
        reader(&s),
        Arc::new(Sent::default()),
        net(&s, &[]),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: me,
            fetch: false,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    node.tick().await.expect("tick");
    node.tick().await.expect("tick");
    let asked = s
        .chain
        .1
        .lock()
        .expect("lock")
        .holders_asked
        .iter()
        .filter(|k| k.0 == 0 && k.1 == 3)
        .count();
    assert_eq!(asked, 0, "no challengedHolders call for a slashed shard");
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-S16-R10: passes are spaced at random around `every`, so
/// nodes started in the same second drift apart; the spacing stays within a
/// quarter of `every` and never waits past it on the first pass.
#[test]
fn passes_are_spaced_at_random_around_the_interval() {
    use super::Jitter;
    let every = Duration::from_secs(60);
    let a = NodeId(Hash32([1; 32]));
    let mut j = Jitter::new(&a);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..2_000 {
        let w = j.around(every);
        assert!(
            w >= Duration::from_secs(45) && w < Duration::from_secs(75),
            "{w:?}"
        );
        let p = j.part(every);
        assert!(p < every, "{p:?}");
        seen.insert(w.as_secs());
    }
    // Spread over the window, not stuck on one value.
    assert!(seen.len() > 25, "{} distinct waits", seen.len());
    assert_eq!(j.part(Duration::ZERO), Duration::ZERO);
    assert_eq!(j.around(Duration::ZERO), Duration::ZERO);
    // The waits `run` takes: the first within `every`, the rest within a quarter of it.
    let mut w = super::pass_waits(&a, every);
    assert!(w.next().expect("first") < every);
    for _ in 0..500 {
        let d = w.next().expect("next");
        assert!(
            d >= Duration::from_secs(45) && d < Duration::from_secs(75),
            "{d:?}"
        );
    }
    // Two nodes started together do not draw the same waits.
    let mut x = Jitter::new(&NodeId(Hash32([2; 32])));
    let mut y = Jitter::new(&NodeId(Hash32([3; 32])));
    let xs: Vec<Duration> = (0..8).map(|_| x.around(every)).collect();
    let ys: Vec<Duration> = (0..8).map(|_| y.around(every)).collect();
    assert_ne!(xs, ys);
}

// --- Storage nodes learn of a removal (ADR-0059 amendment 4 item 5a slice 2a; review CAT-LEARN-R1)

use crate::bulk_serve::tests::{
    leaf_answer_of, live_catalog, live_then_removed, removed_in_catalog, tombstone_catalog,
};
use crate::bulk_serve::RemovalsMode;

/// How a URL answers a leaf request.
#[derive(Clone)]
enum LeafPeer {
    Down,
    /// Through a real store's peer routes.
    Store(Arc<BulkStore>),
    /// A fixed answer.
    Says(u16, Vec<u8>),
}

struct Leaves {
    peers: BTreeMap<String, LeafPeer>,
    calls: Mutex<Vec<String>>,
}

impl Leaves {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }
}

impl HttpGet for Leaves {
    fn get<'a>(
        &'a self,
        url: &'a str,
        max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            use http_body_util::BodyExt as _;
            use tower::ServiceExt as _;
            if let Ok(mut c) = self.calls.lock() {
                c.push(url.to_string());
            }
            assert_eq!(max, LEARN_ANSWER_BYTES, "answers are capped at 64 KiB");
            let base = url.split("/v1/").next().unwrap_or_default();
            let path = &url[base.len()..];
            match self.peers.get(base).cloned().unwrap_or(LeafPeer::Down) {
                LeafPeer::Down => Err("connection refused".into()),
                LeafPeer::Says(s, b) => Ok((s, b)),
                LeafPeer::Store(st) => {
                    let req = axum::http::Request::get(path)
                        .body(axum::body::Body::empty())
                        .map_err(|e| e.to_string())?;
                    let r = crate::bulk_serve::peer_routes(st)
                        .oneshot(req)
                        .await
                        .map_err(|e| e.to_string())?;
                    let status = r.status().as_u16();
                    let b = r
                        .into_body()
                        .collect()
                        .await
                        .map_err(|e| e.to_string())?
                        .to_bytes();
                    Ok((status, b.to_vec()))
                }
            }
        })
    }
}

fn leaves(peers: &[(&str, LeafPeer)]) -> Arc<Leaves> {
    Arc::new(Leaves {
        peers: peers
            .iter()
            .map(|(u, p)| (u.to_string(), p.clone()))
            .collect(),
        calls: Mutex::new(Vec::new()),
    })
}

const LEAF_URL: &str = "/v1/bulk/catalogs/example.publisher/leaf/synthetic.metrics";

/// `setup(5)`'s deal, the publisher's node holding catalog 0 that removes
/// the dataset, and node 0's own store (`own`, in `mode`) whose book anchors
/// the deal's manifest and that catalog.
async fn removed_deal(mode: RemovalsMode) -> (Setup, World, SignedReport) {
    let s = setup(5).await;
    let own = world().with_mode(mode);
    own.node.anchor(&sign(&s.manifest));
    let (_, r0) = removed_in_catalog(&s.publisher, &s.manifest).await;
    own.node.anchor(&r0);
    (s, own, r0)
}

/// Node 0 over `own`: its ticks fetch from the publisher; it learns over
/// `lv` from `catalog_nodes` first.
fn learner(
    s: &Setup,
    own: &World,
    catalog_nodes: &[&str],
    lv: Arc<Leaves>,
) -> (StorageNode, Arc<Sent>) {
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        reader(s),
        tx.clone(),
        net(s, &[("http://publisher", Peer::Good)]),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[0],
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    )
    .with_learn(catalog_nodes.iter().map(|u| u.to_string()).collect(), lv);
    (node, tx)
}

fn clean(s: &Setup, own: &World) {
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review CAT-LEARN-R1 H1: a deal's source may be dead or hostile and its
/// peers hold no catalogs, so the configured catalog node is asked first:
/// the node learns the removal from it (one call: the proof holds), rows
/// stop with 410, and its storage paths (manifest, shards, challenges) are
/// untouched. Without a catalog node, `never_learned` counts the dataset.
#[tokio::test]
async fn cat_3_learn_a_storage_node_learns_from_a_catalog_node_when_the_deals_source_is_dead() {
    let (s, own, _) = removed_deal(RemovalsMode::On).await;
    let lv = leaves(&[
        ("http://publisher", LeafPeer::Down),
        ("http://catalog", LeafPeer::Store(s.publisher.store.clone())),
    ]);
    let (node, _) = learner(&s, &own, &["http://catalog"], lv.clone());
    let r = node.learn_pass().await.expect("pass");
    assert_eq!(
        (r.block, r.asked),
        (100, 0),
        "before a tick's snapshot: the recheck only"
    );
    let t = node.tick().await.expect("tick");
    assert!(t.wanted > 0 && t.held == t.wanted);
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.asked, r.learned, r.kept, r.never_learned), (1, 1, 1, 0));
    assert_eq!(lv.calls(), vec![format!("http://catalog{LEAF_URL}")]);
    assert!(own
        .store
        .learned_removal("example.publisher", "synthetic.metrics")
        .expect("ok")
        .is_some());
    let e = own
        .store
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=0&to=3")).expect("q"),
        )
        .await
        .expect_err("removed");
    assert_eq!(e.code, "bulk_dataset_removed");
    let v = node.status_json();
    assert_eq!(v["removals"]["kept"], json!(1));
    assert_eq!(v["removals"]["neverLearned"], json!(0));
    // The storage paths: the next tick still holds every placed shard, and
    // they serve by root.
    let t = node.tick().await.expect("tick");
    assert_eq!(t.held, t.wanted);
    for e in &s.manifest.shards {
        if own.store.holds_shard(&e.root) {
            assert!(own.store.shard_file(&e.root).await.is_some());
        }
    }
    // Another node with no catalog node and a dead source learns nothing,
    // and says so.
    let other = world();
    other.node.anchor(&sign(&s.manifest));
    let lv2 = leaves(&[("http://publisher", LeafPeer::Down)]);
    let (n2, _) = learner(&s, &other, &[], lv2.clone());
    n2.tick().await.expect("tick");
    let r = n2.learn_pass().await.expect("pass");
    assert_eq!((r.learned, r.kept, r.never_learned), (0, 0, 1));
    assert_eq!(lv2.calls()[0], format!("http://publisher{LEAF_URL}"));
    assert_eq!(n2.status_json()["removals"]["neverLearned"], json!(1));
    let _ = std::fs::remove_dir_all(&other.dir);
    clean(&s, &own);
}

/// A 404, a live leaf of an anchored catalog and an unanchored tombstone
/// from the catalog nodes do not end the search: the deal's source, asked
/// next, gives the proof that holds.
#[tokio::test]
async fn cat_3_learn_a_lying_source_does_not_stop_the_search() {
    let (s, own, _) = removed_deal(RemovalsMode::On).await;
    let (cl, rl) = live_catalog(&s.manifest, T0_LEARN);
    own.node.anchor(&rl);
    let (ct, rt) = tombstone_catalog(&s.manifest, T0_LEARN + 1);
    let unanchored = leaf_answer_of(&ct, &rt, "synthetic.metrics");
    let lv = leaves(&[
        ("http://liar-404", LeafPeer::Says(404, b"{}".to_vec())),
        (
            "http://liar-live",
            LeafPeer::Says(200, leaf_answer_of(&cl, &rl, "synthetic.metrics")),
        ),
        ("http://liar-unanchored", LeafPeer::Says(200, unanchored)),
        (
            "http://publisher",
            LeafPeer::Store(s.publisher.store.clone()),
        ),
    ]);
    let (node, _) = learner(
        &s,
        &own,
        &[
            "http://liar-404",
            "http://liar-live",
            "http://liar-unanchored",
        ],
        lv.clone(),
    );
    node.tick().await.expect("tick");
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.learned, r.kept), (1, 1));
    assert_eq!(
        lv.calls(),
        [
            "http://liar-404",
            "http://liar-live",
            "http://liar-unanchored",
            "http://publisher"
        ]
        .iter()
        .map(|u| format!("{u}{LEAF_URL}"))
        .collect::<Vec<_>>()
    );
    clean(&s, &own);
}

/// The learn task reads only (the book at one finalized block, peers' leaf
/// answers): learning, keeping and dropping send no transaction, and with
/// `ONYX_STORAGE_REMOVALS=off` it asks nobody.
#[tokio::test]
async fn cat_3_learn_the_learn_task_never_sends_a_transaction() {
    let (s, own, r0) = removed_deal(RemovalsMode::On).await;
    let lv = leaves(&[("http://catalog", LeafPeer::Store(s.publisher.store.clone()))]);
    let (node, tx) = learner(&s, &own, &["http://catalog"], lv.clone());
    node.tick().await.expect("tick");
    let sent = tx.0.lock().expect("lock").len();
    assert_eq!(node.learn_pass().await.expect("pass").learned, 1);
    assert_eq!(node.learn_pass().await.expect("pass").kept, 1);
    own.node.withdraw(&r0);
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.dropped, r.kept), (1, 0));
    assert_eq!(tx.0.lock().expect("lock").len(), sent, "no transaction");
    // Off: nobody is asked.
    let off = world().with_mode(RemovalsMode::Off);
    off.node.anchor(&sign(&s.manifest));
    let lv2 = leaves(&[("http://catalog", LeafPeer::Store(s.publisher.store.clone()))]);
    let (n2, tx2) = learner(&s, &off, &["http://catalog"], lv2.clone());
    n2.tick().await.expect("tick");
    let before = tx2.0.lock().expect("lock").len();
    assert_eq!(n2.learn_pass().await.expect("pass"), LearnReport::default());
    assert!(lv2.calls().is_empty());
    assert_eq!(tx2.0.lock().expect("lock").len(), before);
    let _ = std::fs::remove_dir_all(&off.dir);
    clean(&s, &own);
}

const T0_LEARN: u64 = 1_800_000_100;

/// A pass of `own`'s store learning the publisher's leaf of the dataset (as
/// a pass would), outside any node.
async fn learn_directly(s: &Setup, own: &World) {
    let leaf = s
        .publisher
        .store
        .leaf_answer("example.publisher", "synthetic.metrics")
        .expect("leaf");
    let body = serde_json::to_vec(&leaf).expect("json");
    let mut pass = own.store.learn_pass_start().await.expect("pass");
    let l = own
        .store
        .learn_from(
            &mut pass,
            "example.publisher",
            "synthetic.metrics",
            &body,
            "test",
        )
        .await
        .expect("read");
    assert_eq!(l, crate::bulk_serve::Learn::Kept(0));
}

async fn rows_of(own: &World) -> Result<Value, crate::ApiError> {
    own.store
        .rows(
            "example.publisher",
            "synthetic.metrics",
            &parse_rows_query(Some("from=0&to=3")).expect("q"),
        )
        .await
}

/// Review CAT-LEARN-R2 1: anyone may post an old anchored catalog of the
/// publisher (here its catalog 0, the dataset still live) to a storage
/// node's public catalog route. Learning goes on (a held catalog no longer
/// skips the publisher), rows stop with 410 although `removed()` reads the
/// old catalog as live, and the leaf route relays the newer learned proof,
/// not the old live leaf.
#[tokio::test]
async fn cat_3_learn_an_old_catalog_handed_to_a_storage_node_does_not_stop_learning() {
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;
    let s = setup(5).await;
    let ((c0, r0), (c1, r1)) = live_then_removed(&s.publisher, &s.manifest).await;
    let own = world();
    for r in [&sign(&s.manifest), &r0, &r1] {
        own.node.anchor(r);
    }
    let lv = leaves(&[("http://catalog", LeafPeer::Store(s.publisher.store.clone()))]);
    let (node, _) = learner(&s, &own, &["http://catalog"], lv.clone());
    node.tick().await.expect("tick");
    // The attack: catalog 0, through the node's public upload route.
    let body = json!({
        "catalog": hex0x(&borsh::to_vec(&c0).expect("ok")),
        "report": hex0x(&borsh::to_vec(&r0).expect("ok")),
    });
    let r = crate::bulk_serve::upload_routes(own.store.clone())
        .oneshot(
            axum::http::Request::post("/v1/bulk/catalogs")
                .body(axum::body::Body::from(body.to_string()))
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(
        r.status(),
        axum::http::StatusCode::CREATED,
        "the old catalog is taken"
    );
    assert_eq!(
        own.store
            .removed("example.publisher", "synthetic.metrics")
            .expect("ok"),
        None
    );
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.asked, r.learned, r.kept, r.never_learned), (1, 1, 1, 0));
    assert_eq!(
        rows_of(&own).await.expect_err("removed").code,
        "bulk_dataset_removed"
    );
    let leaf = crate::bulk_serve::peer_routes(own.store.clone())
        .oneshot(
            axum::http::Request::get(LEAF_URL)
                .body(axum::body::Body::empty())
                .expect("ok"),
        )
        .await
        .expect("ok");
    assert_eq!(leaf.status(), axum::http::StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&leaf.into_body().collect().await.expect("ok").to_bytes())
            .expect("json");
    crate::openapi_check::check("GET", LEAF_URL, 200, &v);
    assert_eq!(v["from"], json!("learned"));
    assert_eq!(v["number"], json!(1));
    assert_eq!(v["root"], json!(c1.header.root().to_string()));
    assert_eq!(v["removed"], json!(true));
    // A later pass still asks, and the removal stays.
    assert_eq!(node.learn_pass().await.expect("pass").asked, 1);
    assert!(own
        .store
        .learned_removal("example.publisher", "synthetic.metrics")
        .expect("ok")
        .is_some());
    clean(&s, &own);
}

/// Review CAT-LEARN-R2 5: a pass reads the kept removals again before any
/// tick has given it a snapshot (the book's withdrawal drops one, asked again
/// of the catalog nodes only, in vain).
#[tokio::test]
async fn cat_3_learn_a_pass_rechecks_before_any_tick() {
    let (s, own, r0) = removed_deal(RemovalsMode::On).await;
    own.store
        .accept_manifest(
            &s.manifest.to_bytes(),
            &borsh::to_vec(&sign(&s.manifest)).expect("ok"),
        )
        .await
        .expect("held");
    learn_directly(&s, &own).await;
    assert_eq!(
        rows_of(&own).await.expect_err("removed").code,
        "bulk_dataset_removed"
    );
    let lv = leaves(&[("http://catalog", LeafPeer::Down)]);
    let (node, _) = learner(&s, &own, &["http://catalog"], lv.clone());
    own.node.withdraw(&r0);
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.reasked, r.asked, r.dropped, r.kept), (1, 1, 1, 0));
    assert_eq!(lv.calls(), vec![format!("http://catalog{LEAF_URL}")]);
    // Served again (this node holds no shard of it: not removed, missing).
    assert_eq!(
        rows_of(&own).await.expect_err("no shard here").code,
        "bulk_shard_missing"
    );
    clean(&s, &own);
}

/// Review CAT-LEARN-R2 4: a removal whose kept proof the book withdraws is
/// asked about again in the same pass, first; a proof that holds (the
/// publisher's replacement under its next key) replaces it and rows never
/// serve in between.
#[tokio::test]
async fn cat_3_learn_a_withdrawn_removal_is_asked_again_in_the_same_pass() {
    let (s, own, r0) = removed_deal(RemovalsMode::On).await;
    let next_key = [0x43; 32];
    let (cb, _) = tombstone_catalog(&s.manifest, T0_LEARN + 5);
    let rb = onyx_bulk::catalog::sign(&next_key, &cb.header).expect("ok");
    let lv = leaves(&[("http://catalog", LeafPeer::Store(s.publisher.store.clone()))]);
    let (node, _) = learner(&s, &own, &["http://catalog"], lv.clone());
    node.tick().await.expect("tick");
    assert_eq!(node.learn_pass().await.expect("pass").learned, 1);
    // The book withdraws catalog 0 and records the replacement.
    own.node.withdraw(&r0);
    own.node.anchor_by(&rb, &next_key);
    let lv2 = leaves(&[(
        "http://catalog",
        LeafPeer::Says(200, leaf_answer_of(&cb, &rb, "synthetic.metrics")),
    )]);
    let (node2, _) = learner(&s, &own, &["http://catalog"], lv2.clone());
    node2.tick().await.expect("tick");
    let r = node2.learn_pass().await.expect("pass");
    assert_eq!((r.reasked, r.learned, r.dropped, r.kept), (1, 1, 0, 1));
    assert_eq!(lv2.calls()[0], format!("http://catalog{LEAF_URL}"));
    assert_eq!(
        rows_of(&own).await.expect_err("still removed").code,
        "bulk_dataset_removed"
    );
    clean(&s, &own);
}

/// Review CAT-LEARN-R2 6: with every source down, the URLs go in tiers: the
/// catalog nodes, the deal's source, then its peers (the holders of its first
/// shard now and last epoch).
#[tokio::test]
async fn cat_3_learn_the_peers_are_asked_after_the_catalog_nodes_and_the_source() {
    let (s, own, _) = removed_deal(RemovalsMode::On).await;
    let lv = leaves(&[]);
    let (node, _) = learner(&s, &own, &["http://catalog"], lv.clone());
    node.tick().await.expect("tick");
    let r = node.learn_pass().await.expect("pass");
    assert_eq!((r.learned, r.never_learned), (0, 1));
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal");
    let peers: Vec<String> = manifest_sources(&snap, d, Some(&s.nodes[0]))
        .into_iter()
        .filter(|u| u != "http://publisher")
        .collect();
    assert!(!peers.is_empty());
    let mut want = vec!["http://catalog".to_string(), "http://publisher".to_string()];
    want.extend(peers);
    assert_eq!(
        lv.calls(),
        want.iter()
            .map(|u| format!("{u}{LEAF_URL}"))
            .collect::<Vec<_>>()
    );
    clean(&s, &own);
}

/// Review CAT-LEARN-R2 5: the order of a pass: every dataset to ask again,
/// then those with no record (taking turns, a window a pass), keeping one
/// place for one with a record; so with more than four placed datasets each
/// is asked within a few passes, and the unlearned never wait for the learned.
#[test]
fn cat_3_learn_the_order_asks_again_then_the_unlearned_then_one_learned() {
    let un: Vec<u32> = (0..7).collect();
    let le: Vec<u32> = (100..103).collect();
    assert_eq!(learn_order(&[50], &un, &le, 0, 4), vec![50, 0, 1, 2, 100]);
    let mut seen = BTreeSet::new();
    for pass in 0..3 {
        let o = learn_order(&[], &un, &le, pass, 4);
        assert_eq!(o.len(), 4);
        assert!(o[..3].iter().all(|k| *k < 100) && o[3] >= 100, "{o:?}");
        seen.extend(o);
    }
    assert_eq!(seen.len(), 10, "every one within three passes");
    assert_eq!(learn_order(&[], &un, &[], 1, 4), vec![4, 5, 6, 0]);
    assert_eq!(learn_order(&[], &[7], &le, 0, 4), vec![7, 100, 101, 102]);
    assert_eq!(learn_order(&[100], &[], &le, 0, 4), vec![100, 101, 102]);
}

// ---- THREATS OBS-STORAGE-1, review OBS-STORAGE-R1: /v1/health shows the storage loop ----

fn health_cfg(node: NodeId) -> StorageNodeConfig {
    StorageNodeConfig {
        contract: CONTRACT,
        node,
        fetch: true,
        keeper: false,
        fetches_per_tick: 64,
    }
}

/// A storage node over `s` (node 0) whose ticks are stamped with `now`.
fn health_node(s: &Setup, own: &World, now: Arc<std::sync::atomic::AtomicU64>) -> Arc<StorageNode> {
    Arc::new(
        StorageNode::new(
            reader(s),
            Arc::new(Sent::default()),
            net(s, &[("http://publisher", Peer::Good)]),
            own.store.clone(),
            health_cfg(s.nodes[0]),
        )
        .with_clock(Arc::new(move || now.load(Ordering::SeqCst))),
    )
}

/// GET `/v1/health` of an API state with `storage` (none: not a storage
/// node), checked against docs/api/openapi.json.
async fn health_of(storage: Option<Arc<StorageNode>>) -> Value {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let state = crate::ApiState::new(
        crate::catalog::Catalog::new(),
        Arc::new(crate::roots::LedgerRoots::new()),
        Arc::new(onyx_coordinator::cas::MemCas::new()),
        None,
        Arc::new(|| 1_700_000_000),
        "http://127.0.0.1",
        crate::ApiConfig::default(),
    )
    .expect("state");
    let state = Arc::new(match storage {
        Some(n) => state.with_storage(n),
        None => state,
    });
    let r = crate::router(state)
        .oneshot(
            Request::get("/v1/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = r.status().as_u16();
    let body = r.into_body().collect().await.expect("body").to_bytes();
    let v: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(status, 200);
    crate::openapi_check::check("GET", "/v1/health", status, &v);
    v
}

/// Yield until `done` holds (the condition the test reads, never a fixed
/// sleep); a cap far above any run.
async fn until(mut done: impl FnMut() -> bool, what: &str) {
    for _ in 0..100_000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what}");
}

/// A storage node's health carries `storage`: before the first tick no tick
/// time (`startedUnix` dates the wait); after each tick that ends well its
/// end on the node's clock (it moves with the clock), no tick running, no
/// error class, and the tick's counters.
#[tokio::test]
async fn obs_storage_1_health_shows_when_the_last_tick_ended() {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let now = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let node = health_node(&s, &own, now.clone());
    let h = health_of(Some(node.clone())).await;
    let st = &h["storage"];
    assert_eq!(st["node"], json!(s.nodes[0].to_string()));
    assert_eq!(st["startedUnix"], json!(1_000));
    for k in [
        "tickStartedUnix",
        "lastTickUnix",
        "lastOkUnix",
        "lastTickOk",
        "lastErrorClass",
    ] {
        assert_eq!(st[k], Value::Null, "no tick yet: {k}");
    }
    assert_eq!(st["failedInARow"], json!(0));
    assert_eq!(st["everySecs"], json!(0), "no loop runs here");
    assert_eq!(st["removals"]["mode"], json!("on"));
    now.store(1_060, Ordering::SeqCst);
    let t = node.tick().await.expect("tick");
    let st = health_of(Some(node.clone())).await["storage"].clone();
    assert_eq!(st["lastTickUnix"], json!(1_060));
    assert_eq!(st["lastOkUnix"], json!(1_060));
    assert_eq!(st["tickStartedUnix"], Value::Null, "between ticks");
    assert_eq!(st["lastTickOk"], json!(true));
    assert_eq!(st["lastErrorClass"], Value::Null);
    assert_eq!(st["epoch"], json!(t.epoch));
    assert_eq!(st["wanted"], json!(t.wanted));
    assert_eq!(st["held"], json!(t.held));
    assert_eq!(st["fetched"], json!(t.fetched));
    assert_eq!(st["seeds"], json!(t.seeds));
    assert!(t.wanted > 0 && t.held == t.wanted);
    now.store(1_120, Ordering::SeqCst);
    node.tick().await.expect("tick");
    let st = health_of(Some(node)).await["storage"].clone();
    assert_eq!(st["lastTickUnix"], json!(1_120), "it moves with each tick");
    assert_eq!(st["lastTickOk"], json!(true));
    clean(&s, &own);
}

/// Review OBS-STORAGE-R1 1: failed ticks are counted in a row
/// (`failedInARow`), with the last one's error class and the last good
/// tick's end (`lastOkUnix`); one good tick clears the count. No error text
/// is on health (review OBS-STORAGE-R1 3).
#[tokio::test]
async fn obs_storage_r1_failed_ticks_in_a_row_and_the_last_good_one_show_on_health() {
    let s = setup(5).await;
    s.chain.0.lock().expect("lock").quirks.v0 = true;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let now = Arc::new(std::sync::atomic::AtomicU64::new(3_000));
    let node = health_node(&s, &own, now.clone());
    for (n, t) in [(1, 3_060), (2, 3_120)] {
        now.store(t, Ordering::SeqCst);
        node.tick().await.expect_err("an older contract is refused");
        let st = health_of(Some(node.clone())).await["storage"].clone();
        assert_eq!(st["failedInARow"], json!(n));
        assert_eq!(st["lastTickUnix"], json!(t));
        assert_eq!(st["lastOkUnix"], Value::Null, "no good tick yet");
        assert_eq!(st["lastTickOk"], json!(false));
        assert_eq!(st["lastErrorClass"], json!("contract_version"));
        assert_eq!(st["tickStartedUnix"], Value::Null);
        assert!(st.get("lastError").is_none(), "no error text: {st}");
        assert_eq!(
            st["wanted"],
            json!(0),
            "the counters are the last good tick's"
        );
    }
    s.chain.0.lock().expect("lock").quirks.v0 = false;
    now.store(3_180, Ordering::SeqCst);
    node.tick().await.expect("tick");
    let st = health_of(Some(node)).await["storage"].clone();
    assert_eq!(
        (
            &st["failedInARow"],
            &st["lastOkUnix"],
            &st["lastTickOk"],
            &st["lastErrorClass"]
        ),
        (&json!(0), &json!(3_180), &json!(true), &Value::Null)
    );
    clean(&s, &own);
}

/// Sends everything but answers, whose sends fail as an operator out of gas
/// does.
#[derive(Default)]
struct NoGasForAnswers(Mutex<Vec<Vec<u8>>>);

impl TxSend for NoGasForAnswers {
    fn address(&self) -> [u8; 20] {
        [0xAA; 20]
    }
    fn send<'a>(
        &'a self,
        _to: &'a [u8; 20],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        let answer = data[..4]
            == abi::selector(
                "answer(uint64,uint64,uint32,bytes32,bytes32[],bytes,bytes32[],bytes,bytes32[])",
            );
        if let Ok(mut s) = self.0.lock() {
            s.push(data);
        }
        Box::pin(async move {
            if answer {
                Err("insufficient funds for gas * price + value".to_string())
            } else {
                Ok("0x01".to_string())
            }
        })
    }
}

/// Review OBS-STORAGE-R1 2: a challenge asked before the node holds its
/// shard counts as `unanswerable`; an answer whose send fails (out of gas)
/// counts as `answerFailed` while the tick itself ends well.
#[tokio::test]
async fn obs_storage_r1_answer_send_failures_and_unanswerable_show_on_health() {
    let s = setup(5).await;
    let seed = Hash32([0x33; 32]);
    s.chain.0.lock().expect("lock").seeds.insert(3, seed);
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    let (me, asked) = s
        .nodes
        .iter()
        .find_map(|n| {
            let k = (0..2)
                .filter(|slot| snap.challenged(&d, 3, &seed, *slot).1.contains(n))
                .count();
            (k > 0).then_some((*n, k))
        })
        .expect("a challenged node");
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let tx = Arc::new(NoGasForAnswers::default());
    let node = Arc::new(StorageNode::new(
        reader(&s),
        tx.clone(),
        net(&s, &[("http://publisher", Peer::Good)]),
        own.store.clone(),
        health_cfg(me),
    ));
    // Answers come before fetches: the first tick is asked before it holds.
    let t = node.tick().await.expect("tick");
    assert_eq!((t.unanswerable, t.answer_failed), (asked, 0));
    assert_eq!(t.held, t.wanted);
    let st = health_of(Some(node.clone())).await["storage"].clone();
    assert_eq!(st["unanswerable"], json!(asked));
    assert_eq!(st["answerFailed"], json!(0));
    // Now it holds them, and every answer's send fails.
    let t = node.tick().await.expect("the tick still ends well");
    assert_eq!((t.answered, t.answer_failed, t.unanswerable), (0, asked, 0));
    let st = health_of(Some(node)).await["storage"].clone();
    assert_eq!(st["answerFailed"], json!(asked));
    assert_eq!(st["unanswerable"], json!(0));
    assert_eq!(st["lastTickOk"], json!(true));
    assert_eq!(st["failedInARow"], json!(0));
    clean(&s, &own);
}

/// A node's health after a good tick at 1,060 (`ok`), and at 2,000 while its
/// next tick, begun at 1,120, never returns (`running`: the chain never
/// answers).
async fn ok_and_running() -> (Value, Value) {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let now = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let node = health_node(&s, &own, now.clone());
    now.store(1_060, Ordering::SeqCst);
    node.tick().await.expect("tick");
    let ok = health_of(Some(node.clone())).await;
    s.chain.0.lock().expect("lock").quirks.hang = true;
    now.store(1_120, Ordering::SeqCst);
    let n = node.clone();
    let task = tokio::spawn(async move { n.tick().await });
    until(
        || node.loop_status().tick_started.is_some(),
        "the tick began",
    )
    .await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(
        !task.is_finished(),
        "a chain that never answers never ends the tick"
    );
    now.store(2_000, Ordering::SeqCst);
    let running = health_of(Some(node)).await;
    task.abort();
    clean(&s, &own);
    (ok, running)
}

/// Review OBS-STORAGE-R1 5: a tick that never returns: health still
/// answers, with `tickStartedUnix` set and `lastTickUnix` frozen at the last
/// tick that ended, however far the clock moves (the watch's stuck alert).
#[tokio::test]
async fn obs_storage_r1_a_tick_that_never_returns_shows_as_running_on_health() {
    let (ok, running) = ok_and_running().await;
    let (a, b) = (&ok["storage"], &running["storage"]);
    assert_eq!(a["tickStartedUnix"], Value::Null);
    assert_eq!(a["lastTickUnix"], json!(1_060));
    assert_eq!(b["tickStartedUnix"], json!(1_120), "the tick that hangs");
    assert_eq!(b["lastTickUnix"], json!(1_060), "frozen");
    assert_eq!(b["lastOkUnix"], json!(1_060));
    assert_eq!(
        b["lastTickOk"],
        json!(true),
        "the last tick that ended did well"
    );
    assert_eq!(b["failedInARow"], json!(0));
}

/// The shared golden health (fixtures/storage/health.json, `ok` and
/// `running`) that scripts/health-watch-tests.py feeds the watch, so the
/// watch's tests use the shape this node writes. Regenerate after an
/// intended change of the `storage` object: `ONYX_BLESS=1 cargo test -p
/// onyx-api --lib obs_storage_r1_health_matches_the_shared_golden`.
#[tokio::test]
async fn obs_storage_r1_health_matches_the_shared_golden() {
    let (ok, running) = ok_and_running().await;
    let doc = json!({
        "about": "GET /v1/health of a storage node (THREATS OBS-STORAGE-1, review OBS-STORAGE-R1): `ok` after a good tick at 1060, `running` at 2000 while the tick begun at 1120 never returns. Written by onyx-api::storage::tests::obs_storage_r1_health_matches_the_shared_golden (ONYX_BLESS=1), read by scripts/health-watch-tests.py.",
        "ok": ok,
        "running": running,
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    let path = format!(
        "{}/../../fixtures/storage/health.json",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("ONYX_BLESS").is_some() {
        std::fs::write(&path, &text).expect("write");
    }
    let on_disk = std::fs::read_to_string(&path)
        .expect("fixtures/storage/health.json missing; run with ONYX_BLESS=1");
    assert_eq!(
        on_disk, text,
        "fixtures/storage/health.json drifted from the node's health"
    );
}

/// Review OBS-STORAGE-R1 5: `run` sets the loop's period on health.
#[tokio::test]
async fn obs_storage_r1_run_sets_every_secs() {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    let now = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let node = health_node(&s, &own, now);
    let task = tokio::spawn(run(node.clone(), Duration::from_secs(60)));
    until(
        || node.status_json()["everySecs"] != json!(0),
        "run set its period",
    )
    .await;
    let st = health_of(Some(node)).await["storage"].clone();
    assert_eq!(st["everySecs"], json!(60));
    task.abort();
    clean(&s, &own);
}

/// A node outside storage mode answers `storage: null`.
#[tokio::test]
async fn obs_storage_1_a_node_without_storage_mode_has_no_storage_object() {
    let h = health_of(None).await;
    assert_eq!(h.get("storage"), Some(&Value::Null));
}

/// Review OBS-STORAGE-R1 3: health names a fixed class of a tick's error,
/// never its text (a provider URL and its key stay in the node's log).
#[test]
fn obs_storage_r1_the_error_class_is_fixed_never_text() {
    for (e, want) in [
        (
            "this node follows rules v3 (ADR-0060 amendment 2) and the contract has no \
             RULES_VERSION(), so it is older (eth_call: execution reverted)",
            "contract_version",
        ),
        ("eth_getLogs: node answered HTTP 429", "rpc_rate_limited"),
        (
            "eth_call: node error: {\"code\":-32005,\"message\":\"limit\"}",
            "rpc_rate_limited",
        ),
        ("rpc unreachable: client error (Connect)", "rpc_unreachable"),
        ("rpc call timed out", "rpc_unreachable"),
        (
            "chain read failed or disagrees with the contract: deals 2, logs 1",
            "snapshot_disagrees",
        ),
        (
            "eth_call: node error: {\"code\":-32000,\"message\":\"x\"}",
            "rpc_error",
        ),
        (
            "could not read the contract's RULES_VERSION(): eth_call: answer has no result",
            "rpc_error",
        ),
        ("No space left on device (os error 28)", "store"),
        (
            "https://user:pw@rpc.example/v2/SECRETKEY?k=x; (y) 'z', w",
            "other",
        ),
    ] {
        let c = error_class(e);
        assert_eq!(c, want, "{e}");
        assert!(ERROR_CLASSES.contains(&c));
        assert!(!c.contains("SECRET") && !c.contains("://"));
    }
}

// ---- THREATS STORAGE-RESTART-1, review STORAGE-RESTART-R1: the kept logs outlive a restart ----

/// The log cache file of `s`'s contract in a state folder of the test's own.
fn log_file(s: &Setup) -> PathBuf {
    log_cache_file(&s.publisher.dir.join("state"), &CONTRACT)
}

/// A reader of `contract` from block `from` that keeps its logs in `path`.
fn file_reader(s: &Setup, contract: [u8; 20], from: u64, path: &Path) -> StorageChain {
    StorageChain::new(s.chain.clone(), contract, from).with_log_file(path.to_path_buf())
}

/// The `eth_getLogs` ranges asked since the last call.
fn log_calls(s: &Setup) -> Vec<(u64, u64)> {
    std::mem::take(&mut s.chain.1.lock().expect("lock").log_ranges)
}

/// Blocks the ranges cover.
fn blocks_of(ranges: &[(u64, u64)]) -> u64 {
    ranges.iter().map(|(f, t)| t - f + 1).sum()
}

/// The next block the reader's kept logs reach, checked or not.
fn kept_next(r: &StorageChain) -> Option<u64> {
    r.cache.lock().expect("lock").logs.as_ref().map(|l| l.0)
}

/// The fake chain's head.
fn head(s: &Setup, block: u64) {
    s.chain.0.lock().expect("lock").block = block;
}

/// A whole read of blocks 0..=20,000 at head 20,000: 100 ranges up to the
/// safe block and one above it.
const WHOLE_READ: usize = 101;

/// Yield until `done` holds, for at most a minute (a save syncs on a
/// blocking thread, so the cap is time, not yields).
async fn until_soon(mut done: impl FnMut() -> bool, what: &str) {
    let end = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < end {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{what}");
}

/// THREATS STORAGE-RESTART-1: a reader that caught up and saved is rebuilt
/// from its file after a restart and asks only for the ranges above the
/// saved block (2 `eth_getLogs` calls, not 46), with the same snapshot;
/// passes and declines come back from the file too. A pass that moves the
/// kept logs a few blocks with no new log does not write the file; 50
/// ranges do (review STORAGE-RESTART-R1 L2).
#[tokio::test]
async fn storage_restart_1_a_rebuilt_reader_reads_only_the_new_ranges() {
    let s = setup(5).await;
    let n = s.nodes.clone();
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 9_000;
        f.answered.push((0, 2, n[1], 0, 1, true));
        f.declined.push((n[1], 0, 1, 4));
    }
    let path = log_file(&s);
    let first = file_reader(&s, CONTRACT, 0, &path);
    let note = first.log_note().expect("note");
    assert!(note.starts_with("none yet"), "{note}");
    let snap = first.snapshot().await.expect("snapshot");
    assert_eq!(
        log_calls(&s).len(),
        46,
        "45 ranges to the safe block, one above"
    );
    assert!(path.exists(), "saved");
    drop(first);
    // The restart.
    head(&s, 9_010);
    let again = file_reader(&s, CONTRACT, 0, &path);
    let note = again.log_note().expect("note");
    assert!(note.starts_with("loaded"), "{note}");
    assert_eq!(kept_next(&again), Some(safe(9_000) + 1));
    assert_eq!(again.logs_read_through(), None, "not checked yet");
    let snap2 = again.snapshot().await.expect("snapshot");
    assert_eq!(
        log_calls(&s),
        vec![(safe(9_000) + 1, safe(9_010)), (safe(9_010) + 1, 9_010)],
        "only the new ranges"
    );
    assert_eq!(again.logs_read_through(), Some(safe(9_010)));
    assert_eq!((snap2.deals.len(), snap2.nodes.len()), (1, 5));
    assert_eq!(snap2.lists, snap.lists);
    assert_eq!(snap2.registrations, snap.registrations);
    assert_eq!(snap2.placement_key(), snap.placement_key());
    assert_eq!(
        snap2.excl.from(0, 1, &n[1]),
        Some(5),
        "the pass and decline"
    );
    let on_disk = |s: &Setup| kept_next(&file_reader(s, CONTRACT, 0, &path));
    assert_eq!(
        on_disk(&s),
        Some(safe(9_000) + 1),
        "10 quiet blocks: not saved"
    );
    head(&s, 19_500);
    again.snapshot().await.expect("snapshot");
    assert_eq!(
        on_disk(&s),
        Some(safe(9_010) + 1 + LOG_SAVE_EVERY_RANGES as u64 * LOG_RANGE),
        "saved 50 ranges on; the quiet ranges after that are not"
    );
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1: a file written for another contract or
/// deploy block is ignored (logged) and every log read again from the deploy
/// block; the reader's own file replaces it. Another contract's reader uses
/// a file of its own name.
#[tokio::test]
async fn storage_restart_1_a_file_for_another_contract_or_deploy_block_is_ignored() {
    let s = setup(5).await;
    head(&s, 20_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 5, &path)
        .snapshot()
        .await
        .expect("snapshot from block 5");
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    let note = r.log_note().expect("note");
    assert!(
        note.starts_with("ignored") && note.contains("written for deploy block 5"),
        "{note}"
    );
    assert_eq!(kept_next(&r), None);
    r.snapshot().await.expect("snapshot");
    let calls = log_calls(&s);
    assert_eq!((calls.len(), calls[0].0), (WHOLE_READ, 0), "read again");
    let other = [0x77; 20];
    assert_ne!(log_cache_file(&s.publisher.dir.join("state"), &other), path);
    let r = file_reader(&s, other, 0, &path);
    let note = r.log_note().expect("note");
    assert!(
        note.starts_with("ignored") && note.contains("written for another contract"),
        "{note}"
    );
    assert_eq!(kept_next(&r), None);
    r.snapshot().await.expect("snapshot");
    assert_eq!(log_calls(&s).len(), WHOLE_READ, "read again");
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1: a damaged file (cut short, empty, not JSON,
/// one digit changed, an older version) or one that cannot be read never
/// fails the start: it is ignored (logged), every log is read again, and
/// the pass writes a good file; a save that fails leaves the pass well.
#[tokio::test]
async fn storage_restart_1_a_damaged_file_is_ignored_and_never_fails_the_start() {
    let s = setup(5).await;
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    log_calls(&s);
    let good = std::fs::read(&path).expect("file");
    let text = String::from_utf8(good.clone()).expect("utf-8");
    let at = text.find("\"data\":\"0x").expect("a log") + 10;
    let mut flipped = text.clone().into_bytes();
    flipped[at] = if flipped[at] == b'0' { b'1' } else { b'0' };
    let mut v: Value = serde_json::from_str(&text).expect("json");
    v.as_object_mut().expect("object").remove("check");
    v["onyxStorageLogs"] = json!(1);
    v["check"] = json!(log_file_check(&v));
    let version1 = serde_json::to_vec(&v).expect("json");
    for (what, bytes, why) in [
        (
            "cut short",
            good[..good.len() / 2].to_vec(),
            "does not decode",
        ),
        ("empty", Vec::new(), "does not decode"),
        ("not JSON", b"\x00\xffjunk".to_vec(), "does not decode"),
        ("one digit changed", flipped, "check does not match"),
        ("an older version", version1, "version 1"),
    ] {
        std::fs::write(&path, &bytes).expect("write");
        let r = file_reader(&s, CONTRACT, 0, &path);
        let note = r.log_note().expect("note");
        assert!(
            note.starts_with("ignored") && note.contains(why),
            "{what}: {note}"
        );
        assert_eq!(kept_next(&r), None, "{what}");
        r.snapshot().await.expect(what);
        assert_eq!(log_calls(&s).len(), 46, "{what}: read again");
        assert_eq!(
            std::fs::read(&path).expect("file"),
            good,
            "{what}: replaced"
        );
    }
    // A folder where the file should be: not read, not written, the pass ends well.
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir_all(path.join("x")).expect("folder");
    let r = file_reader(&s, CONTRACT, 0, &path);
    let note = r.log_note().expect("note");
    assert!(note.contains("it does not read"), "{note}");
    r.snapshot().await.expect("a failed save fails no pass");
    assert_eq!(r.logs_read_through(), Some(safe(9_000)), "kept in memory");
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1: dropping the kept logs removes the file, so a
/// restart never reads them back: directly, and when the chain changed under
/// them (a reorg) and the logs read again disagree with the contract's
/// counts (a lying backend), nothing is left on disk.
#[tokio::test]
async fn storage_restart_1_forget_logs_invalidates_the_file() {
    let s = setup(5).await;
    head(&s, 20_000);
    let path = log_file(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    r.snapshot().await.expect("snapshot");
    assert!(path.exists());
    r.forget_logs("a test").await;
    assert!(!path.exists(), "removed");
    let note = r.log_note().expect("note");
    assert!(note.starts_with("dropped (a test)"), "{note}");
    let fresh = file_reader(&s, CONTRACT, 0, &path);
    assert!(fresh.log_note().expect("note").starts_with("none yet"));
    r.snapshot().await.expect("snapshot");
    assert!(path.exists(), "written again");
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.quirks.fork = 7;
        f.quirks.empty_logs = 1_000;
    }
    r.snapshot()
        .await
        .expect_err("the logs read again disagree");
    assert!(!path.exists(), "the dropped logs are not on disk");
    let rebuilt = file_reader(&s, CONTRACT, 0, &path);
    assert_eq!(kept_next(&rebuilt), None);
    s.chain.0.lock().expect("lock").quirks.empty_logs = 0;
    log_calls(&s);
    rebuilt.snapshot().await.expect("a clean read");
    assert_eq!(log_calls(&s)[0].0, 0, "from the deploy block");
    assert!(path.exists());
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1: a node killed part way through its first read
/// (60 ranges in) restarts from the last save ([`LOG_SAVE_EVERY_RANGES`]:
/// range 50, anchored at its last block) and reads only the blocks from
/// there, each once.
#[tokio::test]
async fn storage_restart_1_a_restart_mid_catch_up_resumes_from_the_saved_next_block() {
    let s = setup(5).await;
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.hang_logs = Some(60);
    }
    let path = log_file(&s);
    let r = Arc::new(file_reader(&s, CONTRACT, 0, &path));
    let r2 = r.clone();
    let task = tokio::spawn(async move { r2.snapshot().await });
    until_soon(|| kept_next(&r) == Some(60 * LOG_RANGE), "60 ranges read").await;
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    assert!(!task.is_finished(), "the 61st range never returns");
    task.abort();
    let _ = task.await;
    drop(r);
    // The restart.
    s.chain.0.lock().expect("lock").quirks.hang_logs = None;
    let saved = LOG_SAVE_EVERY_RANGES as u64 * LOG_RANGE;
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert_eq!(kept_next(&r), Some(saved), "saved at range 50");
    log_calls(&s);
    let snap = r.snapshot().await.expect("snapshot");
    let calls = log_calls(&s);
    assert_eq!(calls[0].0, saved, "goes on from the saved block");
    assert_eq!(blocks_of(&calls), 20_001 - saved, "each block once");
    assert_eq!((snap.deals.len(), snap.nodes.len()), (1, 5));
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1 with STORAGE-S15-4: a first read cut off by
/// HTTP 429 after 70 ranges keeps its last save (range 50: 20 quiet ranges
/// are not saved, review STORAGE-RESTART-R1 L2), and a restart goes on from
/// there.
#[tokio::test]
async fn storage_restart_1_a_rate_limited_catch_up_resumes_from_its_last_save() {
    let s = setup(5).await;
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.answer_logs = Some(70);
    }
    let path = log_file(&s);
    let e = file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect_err("cut off");
    assert!(e.contains("HTTP 429"), "{e}");
    s.chain.0.lock().expect("lock").quirks.answer_logs = None;
    let saved = LOG_SAVE_EVERY_RANGES as u64 * LOG_RANGE;
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert_eq!(kept_next(&r), Some(saved));
    log_calls(&s);
    r.snapshot().await.expect("snapshot");
    let calls = log_calls(&s);
    assert_eq!(calls[0].0, saved);
    assert_eq!(blocks_of(&calls), 20_001 - saved);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// THREATS STORAGE-RESTART-1, review STORAGE-RESTART-R1: health's
/// `logsReadThrough` is the last block the kept logs cover: null before the
/// first tick of a restarted node (the file is not checked against the
/// chain yet), the new safe block after it, null with nothing kept.
#[tokio::test]
async fn storage_restart_1_health_shows_how_far_the_logs_are_cached() {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    let node = Arc::new(StorageNode::new(
        file_reader(&s, CONTRACT, 0, &path),
        Arc::new(Sent::default()),
        net(&s, &[("http://publisher", Peer::Good)]),
        own.store.clone(),
        health_cfg(s.nodes[0]),
    ));
    let st = health_of(Some(node.clone())).await["storage"].clone();
    assert_eq!(
        st["logsReadThrough"],
        Value::Null,
        "loaded, not checked yet"
    );
    head(&s, 9_400);
    node.tick().await.expect("tick");
    let st = health_of(Some(node)).await["storage"].clone();
    assert_eq!(st["logsReadThrough"], json!(safe(9_400)), "after the tick");
    let bare = health_node(&s, &own, Arc::new(std::sync::atomic::AtomicU64::new(1_000)));
    let st = health_of(Some(bare)).await["storage"].clone();
    assert_eq!(st["logsReadThrough"], Value::Null, "nothing kept yet");
    clean(&s, &own);
}

/// Review STORAGE-RESTART-R1 M3, L1: a file whose last block's hash changed
/// while the node was down (a reorg) is dropped on the first pass, before
/// it is used, and every log is read again.
#[tokio::test]
async fn storage_restart_r1_a_file_whose_anchor_changed_while_down_is_dropped_at_load() {
    let s = setup(5).await;
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    s.chain.0.lock().expect("lock").quirks.fork = 7;
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert!(r.log_note().expect("note").starts_with("loaded"));
    let snap = r.snapshot().await.expect("snapshot");
    let note = r.log_note().expect("note");
    assert!(
        note.starts_with(&format!(
            "dropped (the hash of the kept block {} changed)",
            safe(9_000)
        )),
        "{note}"
    );
    let calls = log_calls(&s);
    assert_eq!((calls[0].0, blocks_of(&calls)), (0, 9_001), "read again");
    assert_eq!((snap.deals.len(), snap.nodes.len()), (1, 5));
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-RESTART-R1 M3: the safe boundary. A file whose next block
/// is the chain's safe block + 1 is used on the first pass; one at safe + 2
/// is used too when the RPC knows its last block with the same hash (it was
/// safe when saved), and while the RPC does not know that block yet it is
/// neither used nor dropped: the pass ends as one with no answer and the
/// next uses it. Only a different hash drops a file (the anchor test).
#[tokio::test]
async fn storage_restart_r1_the_safe_boundary_keeps_safe_plus_one_and_waits_on_an_unknown_last_block(
) {
    let s = setup(5).await;
    let path = log_file(&s);
    // next = safe + 1.
    head(&s, 9_000);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert_eq!(kept_next(&r), Some(safe(9_000) + 1));
    r.snapshot().await.expect("snapshot");
    assert_eq!(log_calls(&s), vec![(safe(9_000) + 1, 9_000)], "used");
    assert_eq!(r.logs_read_through(), Some(safe(9_000)));
    // next = safe + 2: saved one block later, read at 9,000 again.
    std::fs::remove_file(&path).expect("remove");
    head(&s, 9_001);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    head(&s, 9_000);
    let good = std::fs::read(&path).expect("file");
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert_eq!(kept_next(&r), Some(safe(9_000) + 2));
    s.chain.0.lock().expect("lock").quirks.known_to = Some(safe(9_000));
    let e = r.snapshot().await.expect_err("its last block is unknown");
    assert!(e.contains("does not know the block yet"), "{e}");
    assert_eq!(error_class(&e), "rpc_error");
    assert_eq!(kept_next(&r), Some(safe(9_000) + 2), "kept");
    assert_eq!(r.logs_read_through(), None, "not used yet");
    assert_eq!(std::fs::read(&path).expect("file"), good, "the file stays");
    assert!(log_calls(&s).is_empty(), "nothing read meanwhile");
    s.chain.0.lock().expect("lock").quirks.known_to = None;
    r.snapshot().await.expect("the RPC knows it now");
    assert_eq!(log_calls(&s), vec![(safe(9_000) + 2, 9_000)], "used");
    assert_eq!(r.logs_read_through(), Some(safe(9_000) + 1));
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-RESTART-R1 M3: a lagging backend on the first pass after a
/// restart (its safe block 100 below the file's last block, and that block
/// unknown to it) keeps the file: the pass ends as one with no answer; once
/// the backend knows the block (its safe block still behind) the file is
/// used and only the blocks above it are read.
#[tokio::test]
async fn storage_restart_r1_a_lagging_backend_on_the_first_pass_keeps_the_file() {
    let s = setup(5).await;
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    let good = std::fs::read(&path).expect("file");
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.quirks.safe_lag = 100;
        f.quirks.known_to = Some(safe(9_000) - 100);
    }
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    let e = r.snapshot().await.expect_err("behind the file");
    assert_eq!(error_class(&e), "rpc_error", "{e}");
    assert_eq!(std::fs::read(&path).expect("file"), good, "kept");
    assert_eq!(kept_next(&r), Some(safe(9_000) + 1));
    assert!(
        r.log_note().expect("note").starts_with("loaded"),
        "not dropped"
    );
    s.chain.0.lock().expect("lock").quirks.known_to = None;
    let snap = r.snapshot().await.expect("its anchor checks");
    assert_eq!(
        log_calls(&s),
        vec![(safe(9_000) + 1, 9_000)],
        "only the new blocks"
    );
    assert_eq!((snap.deals.len(), snap.nodes.len()), (1, 5));
    assert_eq!(
        std::fs::read(&path).expect("file"),
        good,
        "nothing new to save"
    );
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-RESTART-R1 M1: a file with a good check but wrong
/// contents (a `Slashed` log it never got, as an empty `eth_getLogs` answer
/// leaves) passes the counts and the anchor; the node's first pass compares
/// the contract's `exclusionFrom` for the nodes ranked above it, finds the
/// difference, drops the logs and fails the pass (nothing sent from that
/// view); the next pass reads them again and sees the slash.
#[tokio::test]
async fn storage_restart_r1_a_file_missing_a_slashed_log_is_caught_by_exclusion_from() {
    let s = setup(5).await;
    let own = world();
    own.node.anchor(&sign(&s.manifest));
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    let salt = s.chain.0.lock().expect("lock").deal.salt;
    let order = ranked(&shard_key(&salt, 0), &s.nodes);
    let (top, me) = (order[0], order[order.len() - 1]);
    // Slashed on chain (the log is in the kept blocks), missing from the file.
    s.chain.0.lock().expect("lock").slashed.push((top, 0, 0, 4));
    let tx = Arc::new(Sent::default());
    let node = StorageNode::new(
        file_reader(&s, CONTRACT, 0, &path),
        tx.clone(),
        net(&s, &[("http://publisher", Peer::Good)]),
        own.store.clone(),
        health_cfg(me),
    );
    let e = node.tick().await.expect_err("the view is wrong");
    assert!(
        e.contains("exclusionFrom(deal 0, shard 0, node ") && e.contains("is 4 at block"),
        "{e}"
    );
    assert_eq!(error_class(&e), "snapshot_disagrees");
    assert!(!path.exists(), "dropped with the logs");
    assert!(
        tx.0.lock().expect("lock").is_empty(),
        "nothing sent from the wrong view"
    );
    node.tick().await.expect("read again");
    let snap = node.snap.lock().expect("lock").clone().expect("snapshot");
    assert_eq!(snap.excl.from(0, 0, &top), Some(4), "the slash is seen");
    assert!(path.exists(), "saved again");
    clean(&s, &own);
}

/// Review STORAGE-RESTART-R1 M2: a file of another event set or fold rules
/// is ignored at load; one of another chain (`eth_chainId`) is dropped on
/// the first pass, before it is used.
#[tokio::test]
async fn storage_restart_r1_a_changed_event_set_fold_rules_or_chain_id_ignores_the_file() {
    let s = setup(5).await;
    head(&s, 9_000);
    let path = log_file(&s);
    file_reader(&s, CONTRACT, 0, &path)
        .snapshot()
        .await
        .expect("snapshot");
    let good = std::fs::read(&path).expect("file");
    let changed = |field: &str, to: Value| {
        let mut v: Value = serde_json::from_slice(&good).expect("json");
        v.as_object_mut().expect("object").remove("check");
        v[field] = to;
        v["check"] = json!(log_file_check(&v));
        serde_json::to_vec(&v).expect("json")
    };
    for (field, to, why) in [
        (
            "events",
            json!(Hash32([1; 32]).to_string()),
            "another set of events",
        ),
        ("fold", json!(LOG_FOLD_VERSION + 1), "fold rules"),
    ] {
        std::fs::write(&path, changed(field, to)).expect("write");
        let r = file_reader(&s, CONTRACT, 0, &path);
        let note = r.log_note().expect("note");
        assert!(
            note.starts_with("ignored") && note.contains(why),
            "{field}: {note}"
        );
        assert_eq!(kept_next(&r), None, "{field}");
    }
    std::fs::write(&path, &good).expect("write");
    s.chain.0.lock().expect("lock").chain_id = 1;
    log_calls(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    assert!(r.log_note().expect("note").starts_with("loaded"));
    r.snapshot().await.expect("snapshot");
    let note = r.log_note().expect("note");
    assert!(
        note.starts_with("dropped (written for chain 84532, the RPC serves chain 1)"),
        "{note}"
    );
    assert_eq!(log_calls(&s)[0].0, 0, "read again");
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-RESTART-R1 L3: a save and a drop of the kept logs that
/// race leave no file of the dropped logs: a save copied before the drop and
/// written after its removal is not written (the generation), a removal
/// waits for a write under way, and so in either order however the
/// blocking threads interleave.
#[tokio::test]
async fn storage_restart_r1_a_save_racing_forget_logs_leaves_no_stale_file() {
    let s = setup(5).await;
    head(&s, 9_000);
    let path = log_file(&s);
    let r = file_reader(&s, CONTRACT, 0, &path);
    r.snapshot().await.expect("snapshot");
    let (logs, anchor) = {
        let c = r.cache.lock().expect("lock");
        (c.logs.clone(), c.anchor)
    };
    let restore = |r: &StorageChain| {
        if let Ok(mut c) = r.cache.lock() {
            c.logs = logs.clone();
            c.anchor = anchor;
            c.saved = None;
        }
    };
    // Copied, then dropped (the file removed), then written: not written.
    restore(&r);
    let file = r.file.clone().expect("file");
    let (mark, bytes) = LogFile::prepare(&r.cache, &CONTRACT, 0)
        .expect("prepare")
        .expect("a save is due");
    r.forget_logs("a race").await;
    assert!(!path.exists());
    file.commit(&r.cache, mark, &bytes).expect("commit");
    assert!(!path.exists(), "the dropped logs are not written");
    // Both orders, the threads as they come.
    for round in 0..64 {
        restore(&r);
        if round % 2 == 0 {
            tokio::join!(r.save_logs(), r.forget_logs("a race"));
        } else {
            tokio::join!(r.forget_logs("a race"), r.save_logs());
        }
        assert!(!path.exists(), "round {round}: no stale file");
        assert_eq!(kept_next(&r), None, "round {round}");
    }
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// ADR-0059 amendment 5 item 4: a client read of a deal on a widened
/// manifest checks every shard under the schema its index was built under;
/// item 1: a storage node refuses a deal on a format-2 manifest (the deployed
/// `OnyxStorage` reads format 1 only) rather than fail its checks.
#[tokio::test]
async fn widen_storage_read_rows_checks_each_shard_under_its_schema() {
    let publisher = world_format2();
    let (v1, _) = published(&publisher).await;
    let (v2, _) = widened(&publisher, &v1).await;
    let s = setup_with(5, publisher, v2.clone());
    let only = net(&s, &[("http://publisher", Peer::Good)]);
    let r = read_rows(&reader(&s), only.as_ref(), 0, 0, 39)
        .await
        .expect("read");
    assert_eq!(r.rows.len(), 40);
    assert_eq!(r.served_by.len(), 4);
    for (shard, _, k, bytes) in &r.rows {
        let schema = v2.header.schema_of(*shard);
        let cells = onyx_bulk::row::decode_row(schema, bytes).expect("fits its shard's schema");
        assert_eq!(onyx_bulk::row::key_of(schema, &cells), Some(*k));
    }
    // A row of shard 0 fits schema 0 only; one of shard 3 the newest only.
    let first = &r.rows[0].3;
    assert!(onyx_bulk::row::decode_row(&v2.header.schema, first).is_err());
    // The storage node: no manifest taken, no shard fetched.
    let own = world_format2();
    own.node.anchor(&sign(&v1));
    own.node.anchor(&sign(&v2));
    let node = StorageNode::new(
        reader(&s),
        Arc::new(Sent::default()),
        only.clone(),
        own.store.clone(),
        StorageNodeConfig {
            contract: CONTRACT,
            node: s.nodes[0],
            fetch: true,
            keeper: false,
            fetches_per_tick: 8,
        },
    );
    let snap = reader(&s).snapshot().await.expect("snapshot");
    let d = snap.deal(0).expect("deal").clone();
    let e = node
        .ensure_manifest(&snap, &d, None)
        .await
        .expect_err("refused");
    assert!(e.contains("format 2") && e.contains("OnyxStorage"), "{e}");
    let rep = node.tick().await.expect("tick");
    assert!(rep.fetch_failed >= 1 && rep.fetched == 0, "{rep:?}");
    assert!(own.store.manifest_record(&v2.header.root()).is_none());
    assert!(v2.shards.iter().all(|e| !own.store.holds_shard(&e.root)));
    let _ = std::fs::remove_dir_all(&own.dir);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}
