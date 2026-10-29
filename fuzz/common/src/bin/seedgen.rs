//! Writes the seed corpus of every fuzz target to `<out>/<target>/` from the
//! project's own fixtures and from real values the code under test produces
//! (a signed x402 payment, the API's responses, a recorded level-2 round,
//! journals written by the journal code). Deterministic.
//!
//! Usage: `cargo run --release -p onyx-fuzz-common --bin seedgen -- <out>`

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request};
use borsh::BorshSerialize;
use http_body_util::BodyExt;
use onyx_api::x402::{
    encode_header, exact_evm, FacilitatorRequest, PaymentPayload, PaymentRequired, SettleResponse,
    VerifyResponse,
};
use onyx_fuzz_common::*;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tower::ServiceExt;

struct Out {
    root: PathBuf,
    n: usize,
}

impl Out {
    fn put(&mut self, target: &str, name: &str, bytes: &[u8]) {
        let dir = self.root.join(target);
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(dir.join(name), bytes).is_ok() {
            self.n += 1;
        } else {
            eprintln!("seedgen: cannot write {target}/{name}");
        }
    }

    fn sel(&mut self, target: &str, name: &str, sel: u8, bytes: &[u8]) {
        let mut b = vec![sel];
        b.extend_from_slice(bytes);
        self.put(target, name, &b);
    }

    fn borsh<T: BorshSerialize>(&mut self, name: &str, sel: u8, v: &T) {
        match borsh::to_vec(v) {
            Ok(b) => self.sel("borsh_decode", name, sel, &b),
            Err(e) => eprintln!("seedgen: {name}: {e}"),
        }
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn read_json(rel: &str) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(rel)).ok()?).ok()
}

fn unhex(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap_or_default()
}

/// A payment by `payer a` for `req`, valid until `valid_before`.
fn payment(req: &onyx_api::x402::PaymentRequirements, valid_before: u64, nonce: u8) -> Value {
    let mut to = [0u8; 20];
    to.copy_from_slice(&unhex(&req.pay_to));
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&secret("payer a")).unwrap_or([0; 20]),
        to,
        value: req.amount.parse().unwrap_or(0),
        valid_after: 0,
        valid_before,
        nonce: [nonce; 32],
    };
    exact_evm::sign(&secret("payer a"), &auth, req).unwrap_or(Value::Null)
}

async fn get(
    f: &ApiFixture,
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
) -> (u16, HeaderMap, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let Ok(mut req) = req.body(Body::empty()) else {
        return (0, HeaderMap::new(), Value::Null);
    };
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 9], 4000))));
    let Ok(resp) = onyx_api::router(f.state.clone()).oneshot(req).await;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn x402(o: &mut Out) {
    let req = requirements();
    let payload = PaymentPayload {
        x402_version: 2,
        resource: Some(onyx_api::x402::ResourceInfo {
            url: "https://api.onyx.test/v1/proofs/1/0x00/1".into(),
            description: None,
            mime_type: None,
        }),
        accepted: req.clone(),
        payload: payment(&req, 9_999_999_999, 1),
    };
    o.put("x402_header", "payment", encode_header(&payload).as_bytes());
    let required = PaymentRequired {
        x402_version: 2,
        error: Some("payment required".into()),
        resource: payload
            .resource
            .clone()
            .unwrap_or(onyx_api::x402::ResourceInfo {
                url: String::new(),
                description: None,
                mime_type: None,
            }),
        accepts: vec![req.clone()],
        extensions: None,
    };
    o.put(
        "x402_header",
        "required",
        encode_header(&required).as_bytes(),
    );
    let fr = FacilitatorRequest {
        x402_version: 2,
        payment_payload: payload.clone(),
        payment_requirements: req.clone(),
    };
    o.put("x402_header", "facilitator", encode_header(&fr).as_bytes());
    let vr = VerifyResponse {
        is_valid: true,
        invalid_reason: None,
        payer: Some(addr("payer a")),
    };
    o.put("x402_header", "verify", encode_header(&vr).as_bytes());
    let sr = SettleResponse {
        success: true,
        error_reason: None,
        payer: Some(addr("payer a")),
        transaction: hex0x(&[0x77; 32]),
        network: onyx_api::x402::BASE_SEPOLIA.into(),
    };
    o.put("x402_header", "settle", encode_header(&sr).as_bytes());
    for (name, vb) in [("far", 9_999_999_999u64), ("near", 2_000), ("past", 1_000)] {
        let v = json!({"payload": payment(&req, vb, 2), "accepted": req});
        o.put("x402_precheck", name, v.to_string().as_bytes());
    }
}

fn rpc(o: &mut Out) {
    let (shard, root, binding) = fixture_ids();
    let word = |n: u64| format!("0x{:064x}", n);
    let log = json!({
        "address": hex0x(&CONTRACT),
        "topics": [
            onyx_api::roots::FINALIZED_TOPIC,
            word(1),
            shard.to_string(),
        ],
        "data": format!("0x{}{}", hex::encode(root.0), hex::encode(binding.0)),
        "blockNumber": "0x96",
        "blockHash": hex0x(&[0x12; 32]),
        "transactionHash": hex0x(&[0x77; 32]),
        "logIndex": "0x0",
        "removed": false,
    });
    let block = json!({"number": "0x96", "hash": hex0x(&[0x12; 32]), "timestamp": "0x5f5e100"});
    let envelope = json!({"jsonrpc": "2.0", "id": 1, "result": [log]});
    o.sel("rpc_json", "logs", 255, envelope.to_string().as_bytes());
    o.sel("rpc_json", "block", 16, block.to_string().as_bytes());
    o.sel(
        "rpc_json",
        "nested",
        64,
        b"[[[[1,2,{\"a\":[true,null,\"x\"]}]]]]",
    );
    o.sel("rpc_answers", "log", 0, log.to_string().as_bytes());
    let receipt = onyx_api::receipts::Receipt {
        round: 1,
        shard,
        amount: 1_000,
        payer: [0x44; 20],
        nonce: [0x55; 32],
    };
    let sent = json!({
        "hash": hex0x(&[0x77; 32]), "raw": "0x02f8", "chainId": CHAIN, "nonce": 7,
        "tip": "1000", "maxFee": "2000", "gas": 21000, "to": hex0x(&[0x22; 20]),
        "value": "0", "data": "0x1234",
    });
    o.sel("rpc_answers", "sent", 1, sent.to_string().as_bytes());
    o.sel(
        "rpc_answers",
        "receipt",
        1,
        receipt.to_json().to_string().as_bytes(),
    );
    let manifest = json!({
        "chainId": CHAIN,
        "network": onyx_api::x402::BASE_SEPOLIA,
        "settlement": hex0x(&CONTRACT),
        "revenue": hex0x(&[0x22; 20]),
        "registry": hex0x(&[0x66; 20]),
        "usdc": ASSET,
        "deployBlock": 100,
        "revenueCodeHash": hex0x(&[0x01; 32]),
        "settlementCodeHash": hex0x(&[0x02; 32]),
        "registryCodeHash": hex0x(&[0x03; 32]),
    });
    o.sel(
        "rpc_answers",
        "manifest-pin",
        2,
        manifest.to_string().as_bytes(),
    );
    o.sel(
        "rpc_answers",
        "manifest-node",
        3,
        manifest.to_string().as_bytes(),
    );
    let draw = onyx_source_fiscaldata::round::recorded_draw();
    o.sel(
        "rpc_answers",
        "draw",
        4,
        draw.to_json().to_string().as_bytes(),
    );
    o.sel(
        "rpc_answers",
        "cast-commit",
        6,
        format!("{}\n1760000000 [1.76e9]\n0x", hex0x(&[9; 32])).as_bytes(),
    );
    o.sel("rpc_answers", "cast-block", 6, block.to_string().as_bytes());
    o.sel(
        "rpc_answers",
        "envelope",
        7,
        json!({"jsonrpc": "2.0", "id": 1, "result": "0x14a34"})
            .to_string()
            .as_bytes(),
    );
    o.sel(
        "rpc_answers",
        "envelope-error",
        7,
        json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "ünïcödé ".repeat(40)}})
            .to_string()
            .as_bytes(),
    );
    // Scripted nodes for the RPC-driven components.
    let ret = |b: &[u8]| hex0x(b);
    let mut fin = root.0.to_vec();
    fin.extend_from_slice(&binding.0);
    fin.extend_from_slice(&[0u8; 24]);
    fin.extend_from_slice(&400u64.to_be_bytes());
    let mut state = vec![0u8; 160];
    state[31] = 3;
    let chain = format!("0x{CHAIN:x}");
    let node = json!({
        "eth_chainId": [chain, chain, chain, chain, chain, chain, chain, chain],
        "eth_getBlockByNumber": [block, block, block, block, block, block, block, block],
        "eth_blockNumber": ["0x96", "0x96", "0x96", "0x96"],
        "eth_getLogs": [[log], [], [log], [], [], []],
        "eth_call": [ret(&state), ret(&fin), ret(&state), ret(&fin), ret(&[0u8; 32]), ret(&[1u8; 32])],
        "eth_getCode": ["0x6001", "0x", "0x6001"],
        "eth_getTransactionCount": ["0x7", "0x7"],
        "eth_estimateGas": ["0x5208"],
        "eth_gasPrice": ["0x3b9aca00"],
        "eth_maxPriorityFeePerGas": ["0x3b9aca00"],
        "eth_feeHistory": [{"baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"], "reward": [["0x1"]], "oldestBlock": "0x95"}],
        "eth_sendRawTransaction": [hex0x(&[0x77; 32])],
        "eth_getTransactionReceipt": [{"status": "0x1", "blockNumber": "0x96", "transactionHash": hex0x(&[0x77; 32]), "logs": [log], "gasUsed": "0x5208", "effectiveGasPrice": "0x3b9aca00"}],
    });
    for sel in 0u8..8 {
        o.sel(
            "rpc_node",
            &format!("node-{sel}"),
            sel,
            node.to_string().as_bytes(),
        );
    }
}

fn abi(o: &mut Out) {
    let (_, root, binding) = fixture_ids();
    let mut fin = root.0.to_vec();
    fin.extend_from_slice(&binding.0);
    fin.extend_from_slice(&[0u8; 24]);
    fin.extend_from_slice(&400u64.to_be_bytes());
    o.put("abi_returns", "finalized-root", &fin);
    o.put("abi_returns", "not-finalized", &[0u8; 96]);
    let mut state = vec![0u8; 160];
    state[31] = 2;
    o.put("abi_returns", "shard-state", &state);
    let mut w = [0u8; 32];
    w[31] = 1;
    o.put("abi_returns", "word", &w);
    let mut b = [0u8; 64];
    b[31] = 1;
    b[63] = 3;
    o.put("abi_returns", "budget", &b);
}

fn borsh_seeds(o: &mut Out) {
    if let Some(v) = read_json("claims/vectors.json") {
        o.sel(
            "borsh_decode",
            "claim",
            0,
            &unhex(v["claim"].as_str().unwrap_or("")),
        );
    }
    let (spec, rights) = held_spec();
    o.borsh("spec", 1, &spec);
    o.borsh("rights", 4, &rights);
    o.borsh("source", 5, &spec.source);
    o.borsh("value-int", 6, &onyx_core::Value::Int(-5));
    o.borsh(
        "value-fixed",
        6,
        &onyx_core::Value::Fixed {
            mantissa: 12_345,
            scale: 2,
        },
    );
    o.borsh("value-bytes", 6, &onyx_core::Value::Bytes(vec![1, 2, 3]));
    let t = level2::template();
    o.borsh("template-fiscal", 7, &t);
    for c in onyx_source_edgar::COMPANIES {
        o.borsh(
            &format!("template-{}", c.key),
            7,
            &onyx_source_edgar::template(&c),
        );
    }
    let date = level2::record_date();
    if let Ok(s) =
        onyx_source_fiscaldata::spec(&date, onyx_source_fiscaldata::RECORDED_WINDOW_START)
    {
        o.borsh("spec-fiscal", 8, &s);
        if let Ok(obs) = onyx_core::observe(
            &onyx_source_fiscaldata::FiscalDataFetcher,
            &s,
            Some(onyx_source_fiscaldata::RECORDED_BODY),
            s.window_start,
        ) {
            o.borsh("observation", 9, &obs);
            o.borsh("answer", 10, &obs.answer);
        }
        o.borsh("subject", 17, &s.subject);
    }
    o.borsh("hash", 19, &onyx_core::keccak256(b"x"));
    let tree = onyx_merkle::MerkleTree::from_rows(&fixture_rows());
    if let Some(p) = tree.proof(1) {
        o.borsh("proof", 20, &p);
    }
    for (i, r) in fixture_rows().iter().enumerate() {
        o.sel("borsh_decode", &format!("row-{i}"), 28, r);
    }
    if let Some(v) = read_json("api/settlement-vector.json") {
        let row = unhex(v["row"].as_str().unwrap_or(""));
        o.sel("borsh_decode", "settlement-row", 28, &row);
        if let Ok(r) = borsh::from_slice::<onyx_recipe_sdk::OutputRow>(&row) {
            o.sel("borsh_decode", "entity-flow-row", 32, &r.value);
        }
    }
    o.borsh(
        "metric-info",
        18,
        &onyx_core::MetricInfo {
            value_kind: onyx_core::ValueKind::UInt,
            needs_label_set: false,
        },
    );
    let draw = onyx_source_fiscaldata::round::recorded_draw();
    o.borsh("draw-record", 38, &draw);
    for (i, ob) in onyx_source_fiscaldata::round::rehearsal_pool()
        .iter()
        .enumerate()
        .take(3)
    {
        o.borsh(&format!("observer-{i}"), 38, ob);
    }
}

fn catalog(o: &mut Out) {
    use onyx_node::catalog_file::{CatalogFile, HeldEntry, MarketEntry, ShardEntry};
    // The committed fixture's end block, as onyx-node's own tests use it.
    let Some(v) = read_json("btc/blocks-921080-921082.json") else {
        return;
    };
    let Some(b) = v["blocks"].as_array().and_then(|a| a.last()) else {
        return;
    };
    let header = unhex(b["header"].as_str().unwrap_or(""));
    let mut tip = [0u8; 32];
    tip.copy_from_slice(&unhex(b["hash"].as_str().unwrap_or(&"00".repeat(32))));
    let rights = onyx_core::DataRights::public_chain(onyx_core::SourceId("chain.test".into()));
    let spec = onyx_core::ShardSpec {
        recipe: None,
        runtime: None,
        label_set: None,
        source: onyx_core::SourceRef {
            source: onyx_core::SourceId("chain.test".into()),
            snapshot: onyx_core::Snapshot::Block {
                chain: onyx_core::ChainId("btc.mainnet".into()),
                height: 921_082,
                block_hash: onyx_core::Hash32(tip),
            },
        },
        schema: onyx_core::keccak256(b"schema"),
        rights_hash: rights.hash(),
        params: vec![],
        registry_root: onyx_core::keccak256(b"r"),
    };
    let (Ok(entry), Ok(held)) = (
        ShardEntry::new(1, "test.flow", &spec, &rights, Some(3), Some(&header)),
        HeldEntry::new("test.flow", &spec, &rights, Some(3), Some(&header)),
    ) else {
        return;
    };
    let file = CatalogFile {
        version: onyx_node::catalog_file::CATALOG_VERSION,
        shards: vec![entry.clone()],
        held: vec![],
        markets: vec![MarketEntry {
            id: "m".into(),
            round: 1,
            shard: spec.shard_id().to_string(),
            subject: "entity:x".into(),
        }],
        default_pricing: None,
        pricing: Default::default(),
        default_dataset_pricing: None,
        dataset_pricing: Default::default(),
    };
    if let Ok(t) = serde_json::to_string_pretty(&file) {
        o.put("catalog_file", "shard", t.as_bytes());
    }
    let mut f2 = file.clone();
    f2.shards.clear();
    f2.held.push(held);
    if let Ok(t) = serde_json::to_string(&f2) {
        o.put("catalog_file", "held", t.as_bytes());
    }
    if let Ok(mut v) = serde_json::to_value(&file) {
        v["defaultPricing"] = json!({"basePriceMicroUsdc": 1000, "freshWindowSecs": 3600});
        v["defaultDatasetPricing"] = json!({"flatMicroUsdc": 5000, "floorRows": 10});
        o.put("catalog_file", "priced", v.to_string().as_bytes());
    }
}

fn level2_seeds(o: &mut Out) {
    use onyx_api::observations::ObservationStore;
    use onyx_source_fiscaldata::round::{results_store, run, store, Page};
    let rec = onyx_source_fiscaldata::recorded();
    let pages: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: Some(f.body.clone()),
        })
        .collect();
    let mut chain = onyx_coordinator::observe::SimulatedRegistry::new();
    let mut s = store();
    let r = match run(
        &rec.record_date,
        onyx_source_fiscaldata::RECORDED_WINDOW_START,
        &pages,
        &mut s,
        &mut chain,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("seedgen: level-2 run: {e:?}");
            return;
        }
    };
    if let Ok(book) = s.non_value_book() {
        o.sel("book_store", "book", 0, book.to_string().as_bytes());
    }
    let Ok(text) = results_store(&r).map(|v| v.to_string()) else {
        return;
    };
    o.sel("book_store", "store", 1, text.as_bytes());
    if let Ok(feed) = ObservationStore::from_json(&text, level2::NOW, &level2::pins()) {
        let subj = onyx_source_fiscaldata::subject(&rec.record_date);
        if let Some(record) = feed.record(&subj, None, None) {
            let body = feed.body(record);
            let (t, subject) = (level2::template(), level2::subject());
            let ex = onyx_api::client::ObservationExpect {
                template: &t,
                subject: &subject,
                window_end: None,
                min_window_end: onyx_source_fiscaldata::RECORDED_WINDOW_START,
                now: level2::NOW,
            };
            if let Err(e) = onyx_api::client::verify_observation(&body, &ex) {
                eprintln!("seedgen: the recorded round does not verify: {e}");
            }
            o.sel(
                "level2_verify",
                "observation",
                0,
                body.to_string().as_bytes(),
            );
            o.sel(
                "level2_verify",
                "observation-as-attempts",
                1,
                body.to_string().as_bytes(),
            );
        }
    }
    // A round no observer could fetch: an attempt without a value.
    let none: Vec<Page> = rec
        .fetches
        .iter()
        .map(|f| Page {
            fetched_unix: f.fetched_unix,
            body: None,
        })
        .collect();
    let mut chain = onyx_coordinator::observe::SimulatedRegistry::new();
    let mut s = store();
    if let Ok(r) = run(
        &rec.record_date,
        onyx_source_fiscaldata::RECORDED_WINDOW_START,
        &none,
        &mut s,
        &mut chain,
    ) {
        if let Ok(book) = s.non_value_book() {
            o.sel("book_store", "book-failed", 0, book.to_string().as_bytes());
        }
        if let Ok(text) = results_store(&r).map(|v| v.to_string()) {
            o.sel("book_store", "store-failed", 1, text.as_bytes());
            if let Ok(feed) = ObservationStore::from_json(&text, level2::NOW, &level2::pins()) {
                let subj = onyx_source_fiscaldata::subject(&rec.record_date);
                if let Some(fact) = feed.attempt_fact(&subj, None) {
                    let body =
                        feed.attempts_body(&feed.attempts_of(&fact), feed.fact_closed(&fact));
                    o.sel("level2_verify", "attempts", 1, body.to_string().as_bytes());
                }
            }
        }
    }
    o.sel(
        "page_extract",
        "fiscal",
        0,
        onyx_source_fiscaldata::RECORDED_BODY,
    );
    o.sel(
        "page_extract",
        "pointer",
        1,
        onyx_source_fiscaldata::RECORDED_BODY,
    );
    o.sel(
        "page_extract",
        "pointer-exact",
        2,
        onyx_source_fiscaldata::RECORDED_BODY,
    );
    o.sel("page_extract", "mstr", 4, onyx_source_edgar::MSTR_BODY);
    o.sel("page_extract", "riot", 4, onyx_source_edgar::RIOT_BODY);
    let mut m = onyx_source_fiscaldata::RECORDED_MANIFEST
        .as_bytes()
        .to_vec();
    m.push(0);
    m.extend_from_slice(onyx_source_fiscaldata::RECORDED_BODY);
    o.sel("text_parsers", "fiscal-recording", 4, &m);
    let mut m = onyx_source_edgar::MSTR_MANIFEST.as_bytes().to_vec();
    m.push(0);
    m.extend_from_slice(onyx_source_edgar::MSTR_BODY);
    o.sel("text_parsers", "mstr-recording", 4, &m);
}

fn indexer_state(o: &mut Out) {
    let (shard, root, binding) = fixture_ids();
    let ev = |block: u64, idx: u64| {
        json!({
            "round": 1, "shard": shard.to_string(), "root": root.to_string(),
            "binding": binding.to_string(), "block": block, "logIndex": idx,
            "tx": hex0x(&[0x77; 32]),
        })
    };
    let state = json!({
        "onyxIndexer": onyx_api::indexer::STATE_VERSION,
        "chainId": CHAIN,
        "contract": hex0x(&CONTRACT),
        "deployBlock": 100,
        "next": 200,
        "rescan": {"next": 120, "to": 150, "held": onyx_core::keccak256(b"held").to_string()},
        "events": [ev(150, 0), ev(151, 3)],
    });
    o.put("indexer_state", "state", state.to_string().as_bytes());
    let mut s2 = state.clone();
    s2["rescan"] = Value::Null;
    o.put(
        "indexer_state",
        "state-no-rescan",
        s2.to_string().as_bytes(),
    );
}

/// The body lines of a journal file (its checksums and header stripped),
/// as the `journals` target frames them again.
fn journal_body(path: &Path) -> Vec<u8> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(' ').map(|(_, b)| b))
        .collect();
    lines.join("\n").into_bytes()
}

fn journals(o: &mut Out) {
    let dir = std::env::temp_dir().join(format!("onyx-seedgen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let (shard, _, _) = fixture_ids();
    let receipt = onyx_api::receipts::Receipt {
        round: 1,
        shard,
        amount: 1_000,
        payer: [0x44; 20],
        nonce: [0x55; 32],
    };
    // Receipts, written by the receipt log.
    let p = dir.join("receipts.jsonl");
    if let Ok(log) = onyx_api::receipts::ReceiptLog::open(&p, &BIND) {
        for k in 0..3u8 {
            let mut r = receipt;
            r.nonce = [k; 32];
            let _ = log.record(r);
        }
        let first = log.peek(1);
        let _ = log.commit(&first);
        drop(log);
        o.sel("journals", "receipts", 0, &journal_body(&p));
        let raw = std::fs::read(&p).unwrap_or_default();
        o.sel("journals", "receipts-raw", 0x80, &raw);
    }
    // Settle intents, written by the intent log.
    let p = dir.join("intents.jsonl");
    if let Ok(log) = onyx_api::intents::IntentLog::open(&p, &BIND) {
        for k in 0..2u8 {
            let mut r = receipt;
            r.nonce = [k; 32];
            let _ = log.begin(onyx_api::intents::Intent {
                receipt: r,
                valid_after: 1_000,
                valid_before: 2_000,
                earliest: 1_000,
                created: 1_500,
            });
        }
        drop(log);
        o.sel("journals", "intents", 1, &journal_body(&p));
    }
    // Pending self-settles and credits in flight, in their journal format.
    let pending = json!({"ops": [{"add": 0, "v": {
        "hash": hex0x(&[0x77; 32]),
        "receipt": receipt.to_json(),
        "auth": {"from": hex0x(&[0x44; 20]), "to": hex0x(&[0x22; 20]), "value": "1000",
                 "validAfter": 1000, "validBefore": 2000, "nonce": hex0x(&[0x55; 32])},
        "since": 1500,
    }}]});
    o.sel("journals", "pending", 2, pending.to_string().as_bytes());
    let sent = json!({
        "hash": hex0x(&[0x77; 32]), "raw": "0x02f8", "chainId": CHAIN, "nonce": 7,
        "tip": "1000", "maxFee": "2000", "gas": 21000, "to": hex0x(&[0x22; 20]),
        "value": "0", "data": "0x1234",
    });
    let credits = json!({"ops": [{"add": 0, "v": {
        "batch": [receipt.to_json()], "sent": [sent], "firstUnix": 1500,
    }}]});
    o.sel("journals", "credits", 3, credits.to_string().as_bytes());
    let generic = "{\"ops\":[{\"add\":0,\"v\":1},{\"add\":1,\"v\":2}]}\n{\"ops\":[{\"del\":0}]}";
    o.sel("journals", "generic", 4, generic.as_bytes());
    // FUZZ-1's reproducer (an id past the range), kept as a seed.
    o.sel(
        "journals",
        "fuzz-1",
        4,
        br#"{"ops":[{"add":18446744073709551615,"v":1}]}"#,
    );
    let _ = std::fs::remove_dir_all(&dir);
}

async fn api(o: &mut Out) {
    let Some(free) = api_fixture(false) else {
        eprintln!("seedgen: no API fixture");
        return;
    };
    let shard = free.shard;
    let uris = expect_uris(shard);
    for (k, uri) in uris.iter().enumerate() {
        let (status, _, body) = get(&free, Method::GET, uri, &[]).await;
        if status == 200 {
            let ex = &expects(shard)[k];
            if let Err(e) = onyx_api::client::verify_response(&body, ex, Some(&chain_answer())) {
                eprintln!("seedgen: {uri}: the fixture response does not verify: {e}");
            }
            o.sel(
                "client_verify",
                &format!("response-{k}"),
                k as u8,
                body.to_string().as_bytes(),
            );
        } else {
            eprintln!("seedgen: {uri}: HTTP {status}");
        }
        o.sel(
            "api_router",
            &format!("free-{k}"),
            0,
            format!("GET {uri}").as_bytes(),
        );
    }
    for (name, req) in [
        ("health", "GET /v1/health".to_string()),
        ("head", format!("HEAD /v1/proofs/1/{shard}/0")),
        (
            "as-of",
            "GET /v1/metrics/test.flow?subject=entity:a&as_of=100".into(),
        ),
        (
            "forwarded",
            "GET /v1/health\nx-forwarded-for: 1.2.3.4, 10.0.0.2".into(),
        ),
        (
            "observations",
            "GET /v1/observations/test.flow?subject=x&outcome=all".into(),
        ),
        (
            "attempts",
            "GET /v1/observations/test.flow/attempts?subject=x".into(),
        ),
        ("post", "POST /v1/health".into()),
        (
            "upper",
            format!("GET /v1/proofs/1/{}/1", shard.to_string().to_uppercase()),
        ),
    ] {
        o.sel("api_router", name, 0, req.as_bytes());
    }
    let Some(paid) = api_fixture(true) else {
        return;
    };
    let uri = format!("/v1/proofs/1/{shard}/1");
    let (_, headers, _) = get(&paid, Method::GET, &uri, &[]).await;
    let challenge = headers
        .get(onyx_api::x402::PAYMENT_REQUIRED)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    o.put("x402_header", "challenge", challenge.as_bytes());
    if let Ok(pr) = onyx_api::x402::decode_header::<PaymentRequired>(&challenge) {
        if let Some(accepted) = pr.accepts.first() {
            let payload = PaymentPayload {
                x402_version: 2,
                resource: Some(pr.resource.clone()),
                payload: payment(accepted, 9_999_999_999, 3),
                accepted: accepted.clone(),
            };
            let h = encode_header(&payload);
            o.put("x402_header", "paid-request", h.as_bytes());
            o.sel(
                "api_router",
                "paid",
                1,
                format!("GET {uri}\n{}: {h}", onyx_api::x402::PAYMENT_SIGNATURE).as_bytes(),
            );
            let (status, _, body) = get(
                &paid,
                Method::GET,
                &uri,
                &[(onyx_api::x402::PAYMENT_SIGNATURE, h.clone())],
            )
            .await;
            if status == 200 {
                o.sel(
                    "client_verify",
                    "paid-proof",
                    0,
                    body.to_string().as_bytes(),
                );
            } else {
                eprintln!("seedgen: paid request: HTTP {status} {body}");
            }
        }
    }
    o.sel("api_router", "unpaid", 1, format!("GET {uri}").as_bytes());
}

fn text(o: &mut Out) {
    let st = onyx_labels::attest::Statement {
        domain: "onyx.test".into(),
        entity: "entity:example-exchange".into(),
        address: "btc:bc1qexampleaddress0".into(),
        valid_from: 900_000,
        expires: 1_000_000,
        nonce: [7; 16],
    };
    o.sel("text_parsers", "attestation", 0, st.render().as_bytes());
    let rv = onyx_labels::revoke::RevocationStatement {
        domain: "onyx.test".into(),
        entity: "entity:example-exchange".into(),
        address: "btc:bc1qexampleaddress0".into(),
        effective: 950_000,
        nonce: [8; 16],
    };
    o.sel("text_parsers", "revocation", 1, rv.render().as_bytes());
    o.sel("text_parsers", "address", 2, b"btc:bc1qexampleaddress0");
    o.sel("text_parsers", "decimal", 3, b"40305316210829.72");
    o.sel("text_parsers", "decimal-neg", 3, b"-0.000001");
    o.sel(
        "text_parsers",
        "proxies",
        5,
        b"10.0.0.0/8,2001:db8::/32,127.0.0.1\n10.0.0.1\n203.0.113.9, 10.0.0.2, [2001:db8::1]:443",
    );
    o.sel("text_parsers", "url", 6, b"https://x402.org/facilitator");
    o.sel("text_parsers", "url-local", 6, b"http://[::1]:8080/f");
    o.sel("text_parsers", "network", 6, b"eip155:84532");
    o.sel(
        "text_parsers",
        "addr",
        7,
        b"0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
    );
}

fn main() {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: seedgen <out-dir>");
        std::process::exit(2);
    };
    let mut o = Out { root, n: 0 };
    x402(&mut o);
    rpc(&mut o);
    abi(&mut o);
    borsh_seeds(&mut o);
    catalog(&mut o);
    level2_seeds(&mut o);
    indexer_state(&mut o);
    journals(&mut o);
    text(&mut o);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    match rt {
        Ok(rt) => rt.block_on(api(&mut o)),
        Err(e) => eprintln!("seedgen: runtime: {e}"),
    }
    println!("seedgen: wrote {} seeds to {}", o.n, o.root.display());
}
