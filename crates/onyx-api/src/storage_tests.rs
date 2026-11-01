//! The storage network's node side (ADR-0060): the chain view decoded from
//! a fake `OnyxStorage` (views and logs ABI-encoded as the contract emits
//! them), the storage node's fetch, check and answer, and the client read's
//! fail-over (THREATS STORAGE-4, STORAGE-8). Shards are real bulk shards
//! served by a real `BulkStore` (the publisher's node).

use super::*;
use crate::bulk_serve::tests::{published, sign, world, World};
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
    status: BTreeMap<(u64, u64, u32, NodeId), u8>,
    /// Node 0 registered these bytes as its URL (review STORAGE-R1-1).
    junk_url: Option<Vec<u8>>,
    /// The RPC's quirks (review STORAGE-R3-1).
    quirks: Quirks,
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
}

/// The finalized block of the fake chain: 64 below the head.
fn finalized(head: u64) -> u64 {
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
}

fn log(event: &str, topics: Vec<Hash32>, data: Vec<u8>) -> Value {
    let mut t = vec![abi::topic(event).to_string()];
    t.extend(topics.iter().map(|h| h.to_string()));
    json!({"topics": t, "data": hex0x(&data), "blockNumber": format!("0x{LOG_BLOCK:x}")})
}

impl JsonRpc for FakeChain {
    fn call<'a>(&'a self, method: &'a str, p: Value) -> BoxFuture<'a, Result<Value, String>> {
        let f = self.0.lock().expect("lock").clone();
        let r = (|| -> Result<Value, String> {
            Ok(match method {
                "eth_blockNumber" => json!(format!("0x{:x}", f.block)),
                "eth_getBlockByNumber" => {
                    let tag = p[0].as_str().unwrap_or_default();
                    let n = if tag == "finalized" {
                        finalized(f.block)
                    } else {
                        qty(&p[0])?
                    };
                    let mut h = [f.quirks.fork; 32];
                    h[24..].copy_from_slice(&n.to_be_bytes());
                    json!({"number": format!("0x{n:x}"), "hash": Hash32(h).to_string()})
                }
                "eth_getLogs" => {
                    let from = qty(&p[0]["fromBlock"])?;
                    let to = qty(&p[0]["toBlock"])?;
                    let lie = {
                        let mut g = self.0.lock().map_err(|_| "lock")?;
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
                    let topic = p[0]["topics"][0].as_str().unwrap_or_default().to_string();
                    let ids: Vec<NodeId> = f.nodes.iter().map(|n| n.0).collect();
                    let mut out = Vec::new();
                    if topic == abi::topic(events::NODE_SET).to_string() {
                        out.push(log(
                            events::NODE_SET,
                            vec![topic_u64(1)],
                            encode(&[
                                word_tok(&set_hash(&ids)),
                                Tok::Words(ids.iter().map(|n| n.0 .0).collect()),
                            ]),
                        ));
                    } else if topic == abi::topic(events::REGISTERED).to_string() {
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
                    } else if topic == abi::topic(events::ACTIVATED).to_string() {
                        for (i, (id, _)) in f.nodes.iter().enumerate() {
                            out.push(log(
                                events::ACTIVATED,
                                vec![id.0, topic_u64(i as u64), topic_u64(0)],
                                vec![],
                            ));
                        }
                    } else if topic == abi::topic(events::SLASHED).to_string() {
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
                    } else if topic == abi::topic(events::DEAL_CREATED).to_string() {
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
                    let out = if sel == abi::selector("currentEpoch()") {
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
            status: BTreeMap::new(),
            junk_url: None,
            quirks: Quirks::default(),
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
        challenge::verify(&d.info.shards_root, d.info.shard_count, &seed, 0, slot, &p)
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

/// Review STORAGE-R1-8: logs of finalized blocks are read once and kept; a
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
        ranges.iter().all(|(f, _)| *f > finalized(20_000)),
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
    {
        let mut f = s.chain.0.lock().expect("lock");
        f.block = 20_000;
        f.quirks.empty_logs = 1_000;
    }
    // Every call lies: the counts never match, so no wrong snapshot is returned.
    let r = reader(&s);
    assert!(r.snapshot().await.is_err());
    s.chain.0.lock().expect("lock").quirks.empty_logs = 24;
    let snap = r.snapshot().await.expect("the second read is right");
    assert_eq!(snap.deals.len(), 1);
    assert_eq!(snap.nodes.len(), 5);
    assert_eq!(snap.list_at(snap.epoch).len(), 5);
    let _ = std::fs::remove_dir_all(&s.publisher.dir);
}

/// Review STORAGE-R3-1: an RPC that ignores the asked range never fills the
/// kept logs twice, and a reorg under the kept logs (the finalized block's
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
    let kept: usize = r
        .cache
        .lock()
        .expect("lock")
        .logs
        .values()
        .map(|(_, l)| l.len())
        .sum();
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
