//! `btc.entity-reserve` at `/v1/metrics` behind its serving check (ADR-0034
//! Amendment 1): the 89 committed reserve shards
//! (`apps/web/static/history/reserves-week.json`, leaves that rebuild each
//! root) as finalized shards, plus a forged-base shard over the last range
//! that the chain also "finalized" (the recipe is linear in its base, so its
//! rerun is the honest rows with that entity's balances raised; miner and
//! validators would agree on it). Roots come from a `LedgerRoots`; the specs'
//! source snapshots carry each range's real last block from the committed
//! header chain (`fixtures/reserves/headers-969839-971207.json`, ADR-0037:
//! every range must end on it with 6 confirmations) and a synthetic content
//! hash, which the check does not read.

// PP6: every answer these tests see conforms to docs/api/openapi.json.
#[path = "../../../crates/onyx-api/src/openapi_check.rs"]
mod openapi_check;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use borsh::BorshDeserialize;
use http_body_util::BodyExt;
use onyx_api::catalog::{Catalog, LabelRights, PricingPolicy, ShardMeta};
use onyx_api::client::{
    verify_response_unchecked, verify_row, ChainAnswer, Expect, MetricDef, ProvenRow, Window,
};
use onyx_api::gate::refresh;
use onyx_api::roots::{FinalRoot, LedgerRoots};
use onyx_api::x402::{
    decode_header, encode_header, exact_evm, MockFacilitator, PaymentPayload, PaymentRequired,
    PaymentTerms, BASE_SEPOLIA, PAYMENT_REQUIRED, PAYMENT_SIGNATURE,
};
use onyx_api::{router, ApiConfig, ApiState, Payments};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_core::{
    keccak256, ChainId, DataRights, Hash32, LabelSetRef, RuntimeProfile, ShardSpec, Snapshot,
    SourceId, SourceRef,
};
use onyx_merkle::MerkleTree;
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use onyx_reserve_serve::client::{
    check_reserve_answer, link_requests, Limits, DEFAULT_SPAN_BUDGET,
};
use onyx_reserve_serve::{GateLabels, ReserveGate, GATE, METRIC};
use onyx_source_btc::reserve::{CrossChecks, HeaderChain, ReservePin, ScanSnapshot};
use onyx_source_btc::{
    load_label_set, verify_checksum, CHAIN_ID, SCHEMA_ENTITY_RESERVE, SOURCE_ID,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const START: u64 = 969_838;
const PAYER: &str = "reserve api test payer (test key, holds nothing)";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixture(rel: &str) -> Value {
    let fx = root().join("fixtures");
    verify_checksum(&fx, rel).expect("checksum");
    serde_json::from_slice(&std::fs::read(fx.join(rel)).expect("read")).expect("json")
}

const HEADERS: &str = "reserves/headers-969839-971207.json";

/// The committed header chain (RES-4), proof of work checked.
fn headers() -> HeaderChain {
    let fx = root().join("fixtures");
    verify_checksum(&fx, HEADERS).expect("checksum");
    HeaderChain::from_json(
        &std::fs::read_to_string(fx.join(HEADERS)).expect("read"),
        onyx_source_btc::node::MAINNET_MIN_BITS,
    )
    .expect("headers")
}

fn recipe_id() -> Hash32 {
    keccak256(&std::fs::read(root().join("recipes/out/btc_entity_reserve.wasm")).expect("wasm"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rights() -> DataRights {
    DataRights::public_chain(SourceId(SOURCE_ID.into()))
}

fn spec(from: u64, to: u64, tip: Hash32, params: &ReserveParams, set: &LabelSetRef) -> ShardSpec {
    ShardSpec {
        recipe: Some(recipe_id()),
        runtime: Some(RuntimeProfile {
            abi_version: 1,
            host: "onyx-wasmi".into(),
            fuel_limit: 4_000_000_000,
            max_memory_bytes: 256 << 20,
            max_output_bytes: 1 << 20,
            max_call_depth: 512,
        }),
        label_set: Some(set.clone()),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot: Snapshot::BlockRange {
                chain: ChainId(CHAIN_ID.into()),
                from_height: from,
                to_height: to,
                tip_hash: tip,
                content_hash: keccak256(format!("synthetic {from}-{to}").as_bytes()),
            },
        },
        schema: keccak256(SCHEMA_ENTITY_RESERVE),
        rights_hash: rights().hash(),
        params: borsh::to_vec(params).expect("params"),
        registry_root: keccak256(b"test registry"),
    }
}

fn rows_of(leaves: &[Vec<u8>]) -> Vec<EntityReserveRow> {
    leaves
        .iter()
        .map(|l| {
            EntityReserveRow::try_from_slice(&OutputRow::try_from_slice(l).expect("o").value)
                .expect("row")
        })
        .collect()
}

fn leaves(rows: &[EntityReserveRow]) -> Vec<Vec<u8>> {
    rows.iter()
        .map(|r| {
            borsh::to_vec(&OutputRow {
                key: r.entity.as_bytes().to_vec(),
                value: borsh::to_vec(r).expect("row"),
            })
            .expect("leaf")
        })
        .collect()
}

struct Served {
    state: Arc<ApiState>,
    roots: Arc<LedgerRoots>,
    last: ShardSpec,
    forged: Hash32,
}

/// How [`served_with`] lists the shards.
struct Listing {
    /// Register the reserve gate.
    gate: bool,
    /// Catalog metric of every shard.
    metric: &'static str,
    /// Each catalog entry names the gate (`servingCheck`).
    names_check: bool,
    /// The tip hash of the shards ending at the scan (default: its block).
    landing_tip: Option<Hash32>,
    /// `ONYX_RIGHTS` of the server (ROADMAP L14).
    rights: LabelRights,
    /// The operator skips the serving check: a gate under the reserve
    /// gate's name that clears every row (THREATS RES-10).
    skip_check: bool,
    /// No payments (every row free), for clients fetching a whole chain.
    free: bool,
}

/// The tip hash of a range ending at `to`: the header chain's block there
/// (display bytes, as `BlockRange::tip_hash` holds it), or `landing` for a
/// range ending at the scan.
fn tip_at(h: &HeaderChain, scan: &ScanSnapshot, to: u64, landing: Option<Hash32>) -> Hash32 {
    if let (true, Some(l)) = (to == scan.height, landing) {
        return l;
    }
    let mut b = [0u8; 32];
    hex::decode_to_slice(h.hash_at(to).expect("a header there"), &mut b).expect("hash");
    Hash32(b)
}

/// The shards in a catalog (each naming the gate), their rows and params in
/// the store, their roots final; served with the gate when `with_gate`.
fn served(with_gate: bool) -> Served {
    served_with(Listing {
        gate: with_gate,
        metric: METRIC,
        names_check: true,
        landing_tip: None,
        rights: LabelRights::Cleared,
        skip_check: false,
        free: false,
    })
}

fn served_with(o: Listing) -> Served {
    let fx = root().join("fixtures");
    let set = load_label_set(&fx, "labels/test.onyx.entities.btc.v2.json").expect("labels");
    let set_ref = LabelSetRef {
        name: set.name.clone(),
        version: set.version,
        content_hash: set.content_hash(),
    };
    let scan: ScanSnapshot =
        serde_json::from_value(fixture("reserves/scan-971199.json")).expect("scan");
    let cross: CrossChecks =
        serde_json::from_value(fixture("reserves/cross-checks.json")).expect("cross");
    let base: Vec<EntityBase> = fixture("reserves/base-969838.json")["base"]
        .as_array()
        .expect("base")
        .iter()
        .map(|b| EntityBase {
            entity: b["entity"].as_str().expect("e").into(),
            balance_sats: b["balance_sats"].as_str().expect("s").parse().expect("n"),
        })
        .collect();
    let doc: Value = serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/reserves-week.json")).expect("export"),
    )
    .expect("export json");
    let hc = headers();
    let cas = Arc::new(MemCas::new());
    let roots = Arc::new(LedgerRoots::new());
    let roots_out = roots.clone();
    let mut catalog = Catalog::new();
    let (mut height, mut state, mut link) = (START, base, scan.hash());
    let mut add = |round: u64, spec: &ShardSpec, l: &[Vec<u8>]| -> Hash32 {
        let root = cas.put_rows(l).expect("rows");
        cas.put(Kind::Blob, &spec.params).expect("params");
        let mut meta = ShardMeta::from_spec(round, spec, o.metric, rights()).expect("rights");
        if o.names_check {
            meta = meta.with_serving_check(GATE);
        }
        meta.leaf_count = Some(l.len() as u64);
        roots.insert(
            round,
            spec.shard_id(),
            FinalRoot {
                root,
                finalized_at: 100 + round,
                finalized_unix: now(),
                binding: Some(meta.binding.binding(root, l.len() as u64)),
                tx: None,
            },
        );
        catalog.add_shard(meta);
        root
    };
    let mut last = None;
    for (k, s) in doc["shards"].as_array().expect("shards").iter().enumerate() {
        let l: Vec<Vec<u8>> = s["leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(|x| hex::decode(x.as_str().expect("leaf").trim_start_matches("0x")).expect("hex"))
            .collect();
        let (from, to) = (s["from"].as_u64().expect("f"), s["to"].as_u64().expect("t"));
        let p = ReserveParams {
            base_height: height,
            base_ref: link.0,
            base: state.clone(),
        };
        let sp = spec(
            from,
            to,
            tip_at(&hc, &scan, to, o.landing_tip),
            &p,
            &set_ref,
        );
        let r = add(k as u64 + 1, &sp, &l);
        assert_eq!(r, MerkleTree::from_rows(&l).root());
        state = rows_of(&l)
            .iter()
            .map(|x| EntityBase {
                entity: x.entity.clone(),
                balance_sats: x.balance_sats,
            })
            .collect();
        height = to;
        link = r;
        last = Some((sp, p, l));
    }
    // The forged shard over the last range: OKX's base 1,000 BTC higher.
    let (last, p, l) = last.expect("shards");
    let mut fp = p.clone();
    let mut rows = rows_of(&l);
    for b in fp.base.iter_mut().filter(|b| b.entity == "entity:okx") {
        b.balance_sats += 100_000_000_000;
    }
    for r in rows.iter_mut().filter(|r| r.entity == "entity:okx") {
        r.base_sats += 100_000_000_000;
        r.balance_sats += 100_000_000_000;
    }
    let Snapshot::BlockRange {
        from_height,
        to_height,
        ..
    } = last.source.snapshot
    else {
        panic!("range")
    };
    let fspec = spec(
        from_height,
        to_height,
        tip_at(&hc, &scan, to_height, o.landing_tip),
        &fp,
        &set_ref,
    );
    add(1_000, &fspec, &leaves(&rows));
    catalog.set_pricing(
        o.metric,
        PricingPolicy {
            base_price_micro_usdc: 2_500,
            fresh_window_secs: 86_400,
        },
    );
    let payer = format!(
        "0x{}",
        hex::encode(exact_evm::address(&keccak256(PAYER.as_bytes()).0).expect("key"))
    );
    let mut st = ApiState::new(
        catalog,
        roots,
        cas as Arc<dyn Cas + Send + Sync>,
        (!o.free).then(|| Payments {
            terms: PaymentTerms {
                network: BASE_SEPOLIA.into(),
                asset: "0x036CbD53842c5426634e7929541eC2318f3dCF7e".into(),
                pay_to: "0x209693Bc6afc0C5328bA36FaF03C514EF312287C".into(),
                max_timeout_seconds: 60,
                extra: Some(json!({"name": "USDC", "version": "2"})),
            },
            facilitator: Arc::new(MockFacilitator::new([payer])),
        }),
        Arc::new(now),
        "http://127.0.0.1:8402",
        ApiConfig {
            allow_test_labels: true,
            rights: o.rights,
            ..ApiConfig::default()
        },
    )
    .expect("config");
    if o.skip_check {
        st = st.with_gate(Arc::new(SkippedCheck)).expect("gate");
    } else if o.gate {
        let g =
            ReserveGate::new(scan, set, START, recipe_id(), cross, Vec::new(), hc).expect("gate");
        st = st.with_gate(Arc::new(g)).expect("gate");
    }
    Served {
        state: Arc::new(st),
        roots: roots_out,
        last,
        forged: fspec.shard_id(),
    }
}

async fn get(
    s: &Served,
    uri: &str,
    payment: Option<String>,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder().uri(uri);
    if let Some(p) = payment {
        req = req.header(PAYMENT_SIGNATURE, p);
    }
    let resp = router(s.state.clone())
        .oneshot(req.body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    let status = resp.status();
    let challenge = resp
        .headers()
        .get(PAYMENT_REQUIRED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    openapi_check::check("GET", uri, status.as_u16(), &body);
    (status, body, challenge)
}

fn pay(challenge: &str) -> String {
    let pr: PaymentRequired = decode_header(challenge).expect("challenge");
    let accepted = pr.accepts[0].clone();
    let key = keccak256(PAYER.as_bytes()).0;
    let mut to = [0u8; 20];
    to.copy_from_slice(&hex::decode(&accepted.pay_to[2..]).expect("hex"));
    let auth = exact_evm::Authorization {
        from: exact_evm::address(&key).expect("key"),
        to,
        value: accepted.amount.parse().expect("amount"),
        valid_after: 0,
        valid_before: now() + 120,
        nonce: [7; 32],
    };
    encode_header(&PaymentPayload {
        x402_version: 2,
        resource: Some(pr.resource),
        payload: exact_evm::sign(&key, &auth, &accepted).expect("sign"),
        accepted,
    })
}

/// R-RES-SERVE-4: through the API, OKX's reserve at the scan is 402 unpaid,
/// then served with its proof and its cross-check basis, from the honest
/// shard (the newer forged one over the same blocks is refused by the
/// serving check); Binance's is 403 `not_cross_checked` before any 402; the
/// forged shard is 403 `serving_check_failed` on every endpoint.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn reserve_rows_are_served_only_through_the_serving_check() {
    let s = served(true);
    let runs = refresh(&s.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (90, 89), "{runs:?}");
    let uri = format!("/v1/metrics/{METRIC}?subject=entity:okx");
    let (st, _, challenge) = get(&s, &uri, None).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    let (st, b, _) = get(&s, &uri, Some(pay(&challenge.expect("402 header")))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(
        b["shard"],
        s.last.shard_id().to_string(),
        "the honest shard"
    );
    let row = EntityReserveRow::try_from_slice(
        &OutputRow::try_from_slice(
            &hex::decode(b["row"].as_str().expect("row").trim_start_matches("0x")).expect("hex"),
        )
        .expect("o")
        .value,
    )
    .expect("row");
    assert_eq!((row.entity.as_str(), row.height), ("entity:okx", 971_199));
    // The scan's OKX total (fixtures/reserves/scan-971199.json).
    assert_eq!(row.balance_sats, 14_493_185_016_221);
    let v = verify_response_unchecked(
        &b,
        &Expect::Metric {
            metric: METRIC,
            def: MetricDef {
                source: SOURCE_ID,
                chain: Some(CHAIN_ID),
                window: Window::Blocks(8),
                recipe_id: recipe_id(),
                runtime_hash: s.last.runtime.as_ref().expect("rt").hash(),
                schema_id: keccak256(SCHEMA_ENTITY_RESERVE),
                params_hash: keccak256(&s.last.params),
                label_set: s.last.label_set.as_ref().expect("ls").hash(),
                registry_root: s.last.registry_root,
            },
            subject: "entity:okx",
            as_of: None,
            min_as_of: 0,
        },
    )
    .expect("client check");
    let claim = v.require_cross_checked().expect("cross-checked");
    assert_eq!(claim.gate, GATE);
    assert_eq!(claim.bases[0].0, "published-por-file");
    // Binance: refused before any 402.
    let (st, b, challenge) = get(
        &s,
        &format!("/v1/metrics/{METRIC}?subject=entity:binance"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(b["error"]["code"], "not_cross_checked");
    // The reason names the entity and the threat (mutation L4).
    let m = b["error"]["message"].as_str().expect("message");
    assert!(
        m.contains("entity:binance is not independently cross-checked (THREATS RES-1)"),
        "{m}"
    );
    assert!(challenge.is_none());
    // The forged shard, asked for directly.
    for uri in [
        format!("/v1/proofs/1000/{}/0", s.forged),
        format!("/v1/export/1000/{}", s.forged),
    ] {
        let (st, b, _) = get(&s, &uri, None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(b["error"]["code"], "serving_check_failed");
        let m = b["error"]["message"].as_str().expect("message");
        assert!(m.contains("not the previous finalized end"), "{m}");
    }
}

/// ROADMAP L14, THREATS L8-5: with `ONYX_RIGHTS=fallback` the OKX reserve row
/// the serving check clears is not sold: 403 `label_rights_withheld` before
/// any 402, on every endpoint and under a misnamed metric too; the same
/// server in `cleared` mode sells it (402 first).
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn fallback_rights_sell_no_reserve_row() {
    for (rights, metric) in [
        (LabelRights::Fallback, METRIC),
        (LabelRights::Fallback, "btc.reserve-misnamed"),
        (LabelRights::Cleared, METRIC),
    ] {
        let s = served_with(Listing {
            gate: true,
            metric,
            names_check: true,
            landing_tip: None,
            rights,
            skip_check: false,
            free: false,
        });
        let runs = refresh(&s.state).await;
        assert_eq!(runs[0].cleared, 89, "the gate still clears: {runs:?}");
        // The last honest shard is round 89 (one per exported range).
        let uris = [
            format!("/v1/metrics/{metric}?subject=entity:okx"),
            format!("/v1/proofs/89/{}/0", s.last.shard_id()),
            format!("/v1/export/89/{}", s.last.shard_id()),
            format!("/v1/proofs/1000/{}/0", s.forged),
        ];
        if rights == LabelRights::Cleared {
            // OKX is sold (402 first); row 0 (Binance) reaches the serving
            // check, so the shard id and round above are the served ones.
            let (st, b, challenge) = get(&s, &uris[0], None).await;
            assert_eq!(st, StatusCode::PAYMENT_REQUIRED, "{b}");
            assert!(challenge.is_some());
            let (st, b, _) = get(&s, &uris[1], None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
            assert_eq!(b["error"]["code"], "not_cross_checked");
            continue;
        }
        for uri in &uris {
            let (st, b, challenge) = get(&s, uri, None).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{uri} {b}");
            assert_eq!(b["error"]["code"], "label_rights_withheld", "{uri}");
            assert!(challenge.is_none(), "{uri}: no 402 challenge");
            assert!(b.get("row").is_none(), "{uri}");
        }
    }
}

/// Fail closed: a server without the gate refuses every shard that names it.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn a_server_without_the_gate_serves_no_reserve_row() {
    let s = served(false);
    let (st, b, challenge) = get(
        &s,
        &format!("/v1/metrics/{METRIC}?subject=entity:okx"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(b["error"]["code"], "serving_check_unavailable");
    assert!(challenge.is_none());
}

/// `ONYX_RESERVE_GATE`: the config names checksummed fixtures; a scan file
/// whose bytes differ from `SHA256SUMS` is refused.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_gate_config_reads_checksummed_fixtures() {
    let dir = std::env::temp_dir().join(format!("onyx-reserve-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let cfg = |scan: &str| {
        json!({
            "fixtures": root().join("fixtures"),
            "scan": scan,
            "labels": "labels/test.onyx.entities.btc.v2.json",
            "crossChecks": "reserves/cross-checks.json",
            "headers": HEADERS,
            "startHeight": START,
            "recipeId": recipe_id().to_string(),
        })
    };
    let p = dir.join("gate.json");
    std::fs::write(&p, cfg("reserves/scan-971199.json").to_string()).expect("write");
    let g = ReserveGate::from_config(&p).expect("gate");
    assert_eq!(onyx_api::gate::ServingGate::metric(&g), METRIC);
    std::fs::write(&p, cfg("reserves/base-969838.json").to_string()).expect("write");
    assert!(ReserveGate::from_config(&p).is_err(), "not a scan");
    std::fs::write(&p, cfg("reserves/no-such-file.json").to_string()).expect("write");
    assert!(ReserveGate::from_config(&p).is_err());
    // RES-4: no header chain, or a scan file in its place, is refused.
    let mut no_headers = cfg("reserves/scan-971199.json");
    no_headers.as_object_mut().expect("obj").remove("headers");
    std::fs::write(&p, no_headers.to_string()).expect("write");
    assert!(
        ReserveGate::from_config(&p).is_err(),
        "headers are required"
    );
    let mut wrong = cfg("reserves/scan-971199.json");
    wrong["headers"] = json!("reserves/scan-971199.json");
    std::fs::write(&p, wrong.to_string()).expect("write");
    let e = ReserveGate::from_config(&p)
        .err()
        .expect("not a header chain");
    assert!(e.starts_with("headers:"), "{e}");
    // ADR-0037: a dated set and an undated one at once is refused.
    let mut both = cfg("reserves/scan-971199.json");
    both["datedLabels"] = json!("labels/test.onyx.entities.btc.v2.json");
    std::fs::write(&p, both.to_string()).expect("write");
    let e = ReserveGate::from_config(&p).err().expect("both");
    assert!(e.contains("exactly one of labels and datedLabels"), "{e}");
    // Review D1-R2: a format-1 file named as a dated set is refused (no
    // `published`, no ranges); format is never inferred from the file.
    let mut as_dated = cfg("reserves/scan-971199.json");
    as_dated.as_object_mut().expect("obj").remove("labels");
    as_dated["datedLabels"] = json!("labels/test.onyx.entities.btc.v2.json");
    std::fs::write(&p, as_dated.to_string()).expect("write");
    let e = ReserveGate::from_config(&p)
        .err()
        .expect("format 1 as dated");
    assert!(e.starts_with("datedLabels:"), "{e}");
    // Review D1-R1 F4: the proof-of-work floor is pinned, not configurable.
    let mut floor = cfg("reserves/scan-971199.json");
    floor["minBits"] = json!(0x1d00_ffff_u32);
    std::fs::write(&p, floor.to_string()).expect("write");
    assert!(
        ReserveGate::from_config(&p).is_err(),
        "minBits is not a field"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review RESAPI-R1 F1 (its PoC, ported): every reserve shard listed under
/// another metric name with no `servingCheck` (a misnamed catalog entry) is
/// still judged by the reserve gate, which claims it by its binding:
/// Binance is 403 `not_cross_checked` and the forged shard 403
/// `serving_check_failed`, both before any 402; OKX is served with its
/// cross-check. Plain `onyx-node` (no gate) refuses every one of them,
/// because the reserve binding is in `onyx_api::gate::KNOWN_GATES`.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn a_misnamed_catalog_entry_is_still_gated() {
    const ALT: &str = "btc.entity-reserve-raw";
    let listing = |gate| Listing {
        gate,
        metric: ALT,
        names_check: false,
        landing_tip: None,
        rights: LabelRights::Cleared,
        skip_check: false,
        free: false,
    };
    let s = served_with(listing(true));
    let runs = refresh(&s.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (90, 89), "{runs:?}");
    let (st, b, challenge) = get(
        &s,
        &format!("/v1/metrics/{ALT}?subject=entity:binance"),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_cross_checked"))
    );
    assert!(challenge.is_none());
    let (st, b, challenge) = get(&s, &format!("/v1/proofs/1000/{}/0", s.forged), None).await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(challenge.is_none());
    let uri = format!("/v1/metrics/{ALT}?subject=entity:okx");
    let (st, _, challenge) = get(&s, &uri, None).await;
    assert_eq!(st, StatusCode::PAYMENT_REQUIRED);
    let (st, b, _) = get(&s, &uri, Some(pay(&challenge.expect("402 header")))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["servingCheck"]["gate"], GATE);
    // Plain onyx-node: no gate registered, the binding still needs one.
    let s = served_with(listing(false));
    for uri in [
        format!("/v1/metrics/{ALT}?subject=entity:okx"),
        format!("/v1/proofs/89/{}/0", s.last.shard_id()),
    ] {
        let (st, b, challenge) = get(&s, &uri, None).await;
        assert_eq!(
            (st, b["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some("serving_check_unavailable")),
            "{uri}"
        );
        assert!(challenge.is_none());
    }
}

/// `KNOWN_GATES` (compiled into every server) lists the reserve gate with
/// this schema and the committed reserve build, so a rebuilt recipe fails
/// here until it is listed.
#[test]
fn known_gates_list_the_committed_reserve_build() {
    let k = onyx_api::gate::KNOWN_GATES
        .iter()
        .find(|k| k.name == GATE)
        .expect("listed");
    assert!(k.schemas.contains(&SCHEMA_ENTITY_RESERVE));
    assert!(k.recipe_ids.contains(&recipe_id().to_string().as_str()));
    // RESAPI-R2-1: the served metric name is gated on every server too.
    assert!(k.metrics.contains(&METRIC));
}

/// The gate claims a shard by the reserve row schema or by the pinned
/// reserve build, and no other shard (mutation L4: `claims` was only reached
/// through `KNOWN_GATES` before).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_gate_claims_reserve_bindings_only() {
    use onyx_api::gate::ServingGate;
    let g = ReserveGate::new(
        serde_json::from_value(fixture("reserves/scan-971199.json")).expect("scan"),
        load_label_set(
            &root().join("fixtures"),
            "labels/test.onyx.entities.btc.v2.json",
        )
        .expect("labels"),
        START,
        recipe_id(),
        serde_json::from_value(fixture("reserves/cross-checks.json")).expect("cross"),
        Vec::new(),
        headers(),
    )
    .expect("gate");
    let set = LabelSetRef {
        name: "test.onyx.entities.btc".into(),
        version: 2,
        content_hash: keccak256(b"set"),
    };
    let base = spec(1, 2, keccak256(b"tip"), &ReserveParams::default(), &set);
    let meta = |s: &ShardSpec| ShardMeta::from_spec(1, s, "btc.other", rights()).expect("meta");
    assert!(g.claims(&meta(&base)), "schema and build");
    let mut by_schema = base.clone();
    by_schema.recipe = Some(keccak256(b"another recipe"));
    assert!(g.claims(&meta(&by_schema)), "schema only");
    let mut by_build = base.clone();
    by_build.schema = keccak256(b"onyx.btc.other-row.v1");
    assert!(g.claims(&meta(&by_build)), "build only");
    let mut other = by_build;
    other.recipe = Some(keccak256(b"another recipe"));
    assert!(!g.claims(&meta(&other)), "neither");
}

/// Review RESAPI-R1 F2: a chain whose landing shard ends on another block at
/// the scan's height (another branch; synthetic tip) does not anchor:
/// nothing is served.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn a_landing_on_another_block_serves_nothing() {
    let s = served_with(Listing {
        gate: true,
        metric: METRIC,
        names_check: true,
        landing_tip: Some(keccak256(b"another block at 971,199")),
        rights: LabelRights::Cleared,
        skip_check: false,
        free: false,
    });
    let runs = refresh(&s.state).await;
    assert_eq!(runs[0].cleared, 0, "{runs:?}");
    let (st, b, challenge) = get(
        &s,
        &format!("/v1/metrics/{METRIC}?subject=entity:okx"),
        None,
    )
    .await;
    assert_eq!(
        (st, b["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("serving_check_failed"))
    );
    assert!(challenge.is_none());
}

/// Review RESAPI-R1 F4: the shipped `deploy/reserve-gate.json` (the
/// `onyx-node-btc` image's and systemd drop-in's default) loads from a
/// checkout, pins the committed reserve build, and every fixture it names is
/// one the image ships (Dockerfile) and the build context admits
/// (.dockerignore).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_shipped_gate_config_loads_and_its_fixtures_are_packaged() {
    let cfg = root().join("deploy/reserve-gate.json");
    ReserveGate::from_config(&cfg).expect("shipped config");
    let c: Value = serde_json::from_slice(&std::fs::read(&cfg).expect("read")).expect("json");
    assert_eq!(c["recipeId"], recipe_id().to_string());
    let dockerfile = std::fs::read_to_string(root().join("Dockerfile")).expect("Dockerfile");
    let ignore = std::fs::read_to_string(root().join(".dockerignore")).expect(".dockerignore");
    for k in ["scan", "labels", "crossChecks", "headers"] {
        let rel = c[k].as_str().expect("path");
        assert!(dockerfile.contains(rel), "the image ships {rel}");
        assert!(
            ignore.contains(&format!("!fixtures/{rel}")),
            "the context admits {rel}"
        );
    }
    assert!(
        ignore.contains("!fixtures/SHA256SUMS") && ignore.contains("!deploy/reserve-gate.json")
    );
}

/// THREATS RES-4 (ADR-0037): the gate starts only when the committed scan's
/// block is on the header chain with 6 confirmations. The committed chain
/// reaches 971,207, so the scan at 971,199 has 9; cut at 971,203 it has 5
/// and the gate refuses to start.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_4_the_gate_starts_only_on_a_scan_with_six_confirmations() {
    let make = |h: HeaderChain| {
        ReserveGate::new(
            serde_json::from_value(fixture("reserves/scan-971199.json")).expect("scan"),
            load_label_set(
                &root().join("fixtures"),
                "labels/test.onyx.entities.btc.v2.json",
            )
            .expect("labels"),
            START,
            recipe_id(),
            serde_json::from_value(fixture("reserves/cross-checks.json")).expect("cross"),
            Vec::new(),
            h,
        )
    };
    let h = headers();
    assert_eq!(h.tip(), 971_207);
    assert!(
        make(h.truncated(971_204).expect("cut")).is_ok(),
        "6 confirmations"
    );
    let e = make(h.truncated(971_203).expect("cut"))
        .err()
        .expect("5 confirmations");
    assert!(
        e.contains("RES-4") && e.contains("has 5 confirmations") && e.contains("6 needed"),
        "{e}"
    );
    let e = make(h.truncated(971_100).expect("cut"))
        .err()
        .expect("no header");
    assert!(e.contains("no header for block 971199"), "{e}");
}

/// Review D1-R1 F2: the gate checks a dated set's whole version history, so
/// a version that back-dates a membership is refused where it would be used.
/// Version 2 alone (no history) is refused; with a version 1 it back-dates,
/// refused; with an honest version 1 the history passes and only the scan
/// (which names another set) stops it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_dated_gate_refuses_a_back_dated_history() {
    use onyx_labels::dated::{DatedLabelSet, Membership};
    let m = |a: &str, from: u64, to: Option<u64>| Membership {
        label: onyx_labels::Label {
            address: a.into(),
            entity: "entity:okx".into(),
            kind: onyx_core::EntityKind::Exchange,
            confidence: onyx_core::Confidence(900_000),
            evidence: vec![],
        },
        from,
        to,
    };
    let v1 = DatedLabelSet {
        name: "test.onyx.dated".into(),
        version: 1,
        published: 971_000,
        members: vec![m("btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", 970_000, None)],
    };
    let v2 = DatedLabelSet {
        version: 2,
        published: 971_300,
        members: vec![
            m("btc:1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa", 970_000, None),
            m("btc:3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", 971_100, None),
        ],
        ..v1.clone()
    };
    let mut back = v2.clone();
    back.members[1].from = 970_500;
    let gate = |set: DatedLabelSet, history: Vec<DatedLabelSet>| {
        ReserveGate::with_labels(
            serde_json::from_value(fixture("reserves/scan-971199.json")).expect("scan"),
            GateLabels::Dated(set, None, history),
            START,
            recipe_id(),
            serde_json::from_value(fixture("reserves/cross-checks.json")).expect("cross"),
            Vec::new(),
            headers(),
        )
        .err()
        .expect("refused")
    };
    let e = gate(v2.clone(), vec![]);
    assert!(e.contains("dated label set history"), "{e}");
    let e = gate(back, vec![v1.clone()]);
    assert!(
        e.contains("dated label set history") && e.contains("back-dated"),
        "{e}"
    );
    let e = gate(v2, vec![v1]);
    assert!(!e.contains("history"), "{e}");
    assert!(
        e.contains("dated label set name, version or content hash"),
        "{e}"
    );
}

// ------------------------------------------------------------ client check (ROADMAP D3)

/// An operator who skips the serving check (THREATS RES-10): a gate under
/// the reserve gate's name that clears every row of every shard, the forged
/// one included, and claims a cross-check for each.
struct SkippedCheck;

impl onyx_api::gate::ServingGate for SkippedCheck {
    fn name(&self) -> &str {
        GATE
    }
    fn metric(&self) -> &str {
        METRIC
    }
    fn claims(&self, meta: &ShardMeta) -> bool {
        meta.binding.schema_id == keccak256(SCHEMA_ENTITY_RESERVE)
    }
    fn judge(&self, shards: &[onyx_api::gate::GateShard]) -> Vec<onyx_api::gate::Judgement> {
        shards
            .iter()
            .map(|s| {
                Ok(s.leaves
                    .iter()
                    .map(|l| onyx_api::gate::RowClearance {
                        key: OutputRow::try_from_slice(l).expect("leaf").key,
                        cross_check: Ok(onyx_api::gate::CrossCheck {
                            basis: "published-por-file".into(),
                            source: "the operator says so".into(),
                        }),
                    })
                    .collect())
            })
            .collect()
    }
}

fn client_pin() -> ReservePin {
    let set = load_label_set(
        &root().join("fixtures"),
        "labels/test.onyx.entities.btc.v2.json",
    )
    .expect("labels");
    let scan: ScanSnapshot =
        serde_json::from_value(fixture("reserves/scan-971199.json")).expect("scan");
    // The hash the client trusts (the website's `RESERVES_SNAPSHOT`).
    let mut trusted = [0u8; 32];
    hex::decode_to_slice(
        "01fd0e6a76c9bcdf722a498cbea2945a24ffd2227ab9991aab9523c84afad87b",
        &mut trusted,
    )
    .expect("hex");
    ReservePin::from_scan(&scan, &set, recipe_id(), &Hash32(trusted)).expect("pin")
}

/// The client's own node's answer for `(round, shard)` (here: the test's
/// root source, which stands for the chain).
async fn chain_answer(s: &Served, round: u64, shard: Hash32) -> ChainAnswer {
    use onyx_api::roots::RootSource;
    let r = s
        .roots
        .finalized(round, &shard)
        .await
        .expect("read")
        .expect("final");
    ChainAnswer {
        round,
        shard,
        chain_id: 84_532,
        contract: [0; 20],
        root: r.root,
        binding: r.binding.expect("binding"),
        finalized_at: r.finalized_at,
    }
}

/// Fetch and verify every link a `/v1/served` list names that the check of
/// `answer` needs, for `entity`.
async fn fetch_links(
    s: &Served,
    served: &Value,
    pin: &ReservePin,
    entity: &str,
    answer: &Value,
) -> Vec<ProvenRow> {
    let mut out = Vec::new();
    let reqs = link_requests(served, pin, entity, answer, &headers(), &Limits::default())
        .expect("requests");
    for l in reqs {
        let (st, b, _) = get(s, &l.path(), None).await;
        assert_eq!(st, StatusCode::OK, "{} {b}", l.path());
        let c = chain_answer(s, l.round, l.shard).await;
        let p = verify_row(&b, &l.expect(), Some(&c)).expect("link verifies");
        // Review D22-R1-1: stop at the first link the list misdescribed.
        l.matches(&p).expect("the listed range and root");
        out.push(p);
    }
    out
}

/// The OKX answer of `/v1/metrics`, as the server sent it.
async fn okx_answer(s: &Served) -> Value {
    let (st, b, _) = get(s, &format!("/v1/metrics/{METRIC}?subject=entity:okx"), None).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    b
}

/// The answer's own shard, fetched as `/v1/proofs` and verified on the
/// client's node (the forged answer is in no honest chain).
async fn proven(s: &Served, answer: &Value) -> ProvenRow {
    let round = answer["round"].as_u64().expect("round");
    let shard = Hash32(
        hex::decode(
            answer["shard"]
                .as_str()
                .expect("shard")
                .trim_start_matches("0x"),
        )
        .expect("hex")
        .try_into()
        .expect("32"),
    );
    let index = answer["index"].as_u64().expect("index");
    let (st, b, _) = get(s, &format!("/v1/proofs/{round}/{shard}/{index}"), None).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let c = chain_answer(s, round, shard).await;
    verify_row(
        &b,
        &Expect::Proof {
            round,
            shard,
            index,
        },
        Some(&c),
    )
    .expect("verifies")
}

/// The scan's height: the freshness floor the tests' client asks for.
const SCAN_HEIGHT: u64 = 971_199;

/// R-RES-CLIENT-6 (ROADMAP D3): against an honest server, a client re-runs
/// the serving check on API answers alone: `/v1/served` lists the 89 cleared
/// shards (not the forged one), the client fetches OKX's row in each link it
/// needs through `/v1/proofs`, verifies each on its own node and on its own
/// header chain, and derives the committed base from its own pin; the
/// `/v1/metrics` answer is a link of that chain. ROADMAP D22: the check of
/// the landing's answer pays one row (the span from the landing), the
/// week's oldest row 89. Review D3-R1-3: an older link's row (a list cut
/// after it) is refused by the client's `min_as_of`.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn the_client_reruns_the_serving_check_on_api_answers() {
    let s = served_with(Listing {
        gate: true,
        metric: METRIC,
        names_check: true,
        landing_tip: None,
        rights: LabelRights::Cleared,
        skip_check: false,
        free: true,
    });
    refresh(&s.state).await;
    let pin = client_pin();
    let (st, list, _) = get(&s, &format!("/v1/served/{METRIC}"), None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["gate"], GATE);
    let shards = list["shards"].as_array().expect("shards");
    assert_eq!(shards.len(), 89, "the forged shard is not listed");
    assert!(shards.iter().all(|x| x["shard"] != s.forged.to_string()));
    assert!(list.get("rows").is_none() && shards.iter().all(|x| x.get("leaves").is_none()));
    let h = headers();
    let answer = okx_answer(&s).await;
    let one = fetch_links(&s, &list, &pin, "entity:okx", &answer).await;
    assert_eq!(one.len(), 1, "D22: the landing's answer costs one row");
    check_reserve_answer(&answer, &one, &pin, &h, SCAN_HEIGHT).expect("the client's check passes");
    // The week's oldest row: the span from it to the landing, 89 rows.
    let (_, oldest, _) = get(
        &s,
        &format!(
            "/v1/proofs/{}/{}/3",
            shards[0]["round"],
            shards[0]["shard"].as_str().expect("shard")
        ),
        None,
    )
    .await;
    let links = fetch_links(&s, &list, &pin, "entity:okx", &oldest).await;
    assert_eq!(links.len(), 89);
    let v = check_reserve_answer(&oldest, &links, &pin, &h, 0).expect("the oldest checks");
    let base = fixture("reserves/base-969838.json");
    let okx = base["base"]
        .as_array()
        .expect("base")
        .iter()
        .find(|b| b["entity"] == "entity:okx")
        .expect("okx");
    assert_eq!(
        v.chain.start_base_sats.to_string(),
        okx["balance_sats"].as_str().expect("sats"),
        "derived from the pinned scan"
    );
    assert_eq!(v.chain.start_height, START);
    // The answered row must be a link: dropping its shard from the links
    // (the operator lists a chain that does not hold it) fails, and so does
    // an answer whose index or row differs from the proven link's.
    let without: Vec<ProvenRow> = links[..88].to_vec();
    assert!(check_reserve_answer(&answer, &without, &pin, &h, 0).is_err());
    let mut edited = answer.clone();
    edited["index"] = json!(0);
    let e = check_reserve_answer(&edited, &links, &pin, &h, 0).expect_err("refused");
    assert!(e.contains("not a link of the chain"), "{e}");
    // A stale but true answer: link 40's row with the list cut after the
    // landing still checks, and the client's freshness floor refuses it.
    let (_, old, _) = get(
        &s,
        &format!(
            "/v1/proofs/{}/{}/3",
            shards[40]["round"],
            shards[40]["shard"].as_str().expect("shard")
        ),
        None,
    )
    .await;
    check_reserve_answer(&old, &links, &pin, &h, 0).expect("true, if old");
    let e = check_reserve_answer(&old, &links, &pin, &h, SCAN_HEIGHT).expect_err("stale");
    assert!(e.contains("before min_as_of"), "{e}");
}

/// Review D3-R1-1: the same work finalized again in a later round (same
/// spec, so the same shard id, range and root) is listed twice; the client
/// keeps one link per range and still passes, and an answer from either
/// round is the same row.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn the_same_work_finalized_twice_does_not_break_the_client_check() {
    let s = served_with(Listing {
        gate: true,
        metric: METRIC,
        names_check: true,
        landing_tip: None,
        rights: LabelRights::Cleared,
        skip_check: false,
        free: true,
    });
    {
        let id = s.last.shard_id();
        let r89 = {
            use onyx_api::roots::RootSource;
            s.roots
                .finalized(89, &id)
                .await
                .expect("read")
                .expect("final")
        };
        s.roots.insert(
            2_000,
            id,
            FinalRoot {
                finalized_at: 3_000,
                ..r89
            },
        );
        let mut meta = ShardMeta::from_spec(2_000, &s.last, METRIC, rights())
            .expect("meta")
            .with_serving_check(GATE);
        meta.leaf_count = Some(4);
        s.state.catalog.write().expect("catalog").add_shard(meta);
    }
    let runs = refresh(&s.state).await;
    assert_eq!(
        runs[0].cleared, 90,
        "both rounds of the landing cleared: {runs:?}"
    );
    let pin = client_pin();
    let (_, list, _) = get(&s, &format!("/v1/served/{METRIC}"), None).await;
    assert_eq!(list["shards"].as_array().expect("shards").len(), 90);
    let landing = okx_answer(&s).await;
    let reqs = link_requests(
        &list,
        &pin,
        "entity:okx",
        &landing,
        &headers(),
        &Limits::default(),
    )
    .expect("shape");
    assert_eq!(
        reqs.len(),
        1,
        "one link per range, the span from the landing"
    );
    let oldest = json!({
        "root": list["shards"][0]["root"].clone(),
        "index": pin.index_of("entity:okx").expect("okx"),
    });
    let reqs = link_requests(
        &list,
        &pin,
        "entity:okx",
        &oldest,
        &headers(),
        &Limits::default(),
    )
    .expect("shape");
    assert_eq!(reqs.len(), 89, "one link per range");
    let links = fetch_links(&s, &list, &pin, "entity:okx", &landing).await;
    let h = headers();
    for round in [89, 2_000] {
        let (st, a, _) = get(
            &s,
            &format!("/v1/proofs/{round}/{}/3", s.last.shard_id()),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{a}");
        check_reserve_answer(&a, &links, &pin, &h, SCAN_HEIGHT).expect("same work, same answer");
    }
}

/// Review D3-R1-2: the list is checked for shape before the first paid
/// fetch: padded, a gap, an overlap, two roots for one range, no range at
/// the scan, or another gate's list, each refused. ROADMAP D22: no cap on
/// the list's length (the `MAX_LINKS` cliff is gone); the span between the
/// answer and the landing is what is fetched, refused before any fetch when
/// longer than the client's budget.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn a_padded_or_broken_chain_list_is_refused_before_any_fetch() {
    let s = served(true);
    refresh(&s.state).await;
    let pin = client_pin();
    let (_, list, _) = get(&s, &format!("/v1/served/{METRIC}"), None).await;
    // The landing's OKX answer as far as the list check reads it (root and
    // index; the row is checked after the fetch).
    let answer = json!({
        "root": list["shards"][88]["root"].clone(),
        "index": pin.index_of("entity:okx").expect("okx"),
    });
    let hdr = headers();
    let req = |l: &Value, a: &Value, budget: usize| {
        let limits = Limits {
            budget,
            ..Limits::default()
        };
        link_requests(l, &pin, "entity:okx", a, &hdr, &limits)
    };
    assert_eq!(
        req(&list, &answer, DEFAULT_SPAN_BUDGET).expect("ok").len(),
        1
    );
    let oldest = json!({
        "root": list["shards"][0]["root"].clone(),
        "index": pin.index_of("entity:okx").expect("okx"),
    });
    assert_eq!(req(&list, &oldest, 89).expect("ok").len(), 89);
    let e = req(&list, &oldest, 88).expect_err("over budget");
    assert!(
        e.contains("more than the client's budget of 88 links"),
        "{e}"
    );
    let edit = |f: &dyn Fn(&mut Vec<Value>)| {
        let mut l = list.clone();
        f(l["shards"].as_array_mut().expect("shards"));
        req(&l, &answer, DEFAULT_SPAN_BUDGET).expect_err("refused")
    };
    let e = edit(&|v| {
        let x = v[0].clone();
        v.extend(std::iter::repeat_n(x, 20_000));
    });
    assert!(e.contains("do not start right after"), "{e}");
    let e = edit(&|v| {
        v.remove(10);
    });
    assert!(e.contains("do not start right after"), "{e}");
    let e = edit(&|v| v.swap(10, 11));
    assert!(e.contains("do not start right after"), "{e}");
    let e = edit(&|v| {
        let mut x = v[88].clone();
        x["root"] = json!(keccak256(b"another root").to_string());
        v.push(x);
    });
    assert!(e.contains("two roots listed"), "{e}");
    let e = edit(&|v| {
        v.pop();
    });
    assert!(e.contains("no range ending at the pinned scan"), "{e}");
    let mut other = list.clone();
    other["gate"] = json!("another-gate");
    assert!(req(&other, &answer, DEFAULT_SPAN_BUDGET).is_err());
    // Review D3-R2-1: the answer is placed before any fetch. A later range
    // listed after the landing is not fetched for an answer at the landing
    // (it costs a row and the check does not need it), and is for an answer
    // in it; an answer whose root no listed range has, or another entity's
    // row, is refused before the first fetch.
    let mut longer = list.clone();
    let after = json!({
        "round": 5_000,
        "shard": keccak256(b"a later shard").to_string(),
        "from": SCAN_HEIGHT + 1,
        "to": SCAN_HEIGHT + 3,
        "root": keccak256(b"a later root").to_string(),
    });
    longer["shards"]
        .as_array_mut()
        .expect("shards")
        .push(after.clone());
    assert_eq!(
        req(&longer, &answer, DEFAULT_SPAN_BUDGET)
            .expect("ok")
            .len(),
        1,
        "links after the answer and the landing are not fetched"
    );
    let mut later = answer.clone();
    later["root"] = after["root"].clone();
    let reqs = req(&longer, &later, DEFAULT_SPAN_BUDGET).expect("ok");
    assert_eq!(reqs.len(), 2, "the landing and the answer after it");
    assert_eq!(reqs[1].round, 5_000);
    let e = req(&list, &later, DEFAULT_SPAN_BUDGET).expect_err("refused");
    assert!(e.contains("not a link of the chain"), "{e}");
    let mut other_entity = answer.clone();
    other_entity["index"] = json!(0);
    let e = req(&list, &other_entity, DEFAULT_SPAN_BUDGET).expect_err("refused");
    assert!(e.contains("leaf index"), "{e}");
}

/// R-RES-CLIENT-7 (THREATS RES-10, "operator skips the check"): an operator
/// whose gate clears everything serves the newer forged-base shard's OKX row
/// with `servingCheck` saying it is cross-checked. The row verifies against
/// its finalized root on the client's node; the client's own check refuses
/// it: the operator's list names two roots for the last range (refused
/// before any fetch), a list with the forged shard in place of the honest
/// one fails the chain, and the honest chain does not hold the row.
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn a_client_refuses_a_forged_row_from_an_operator_who_skips_the_check() {
    let s = served_with(Listing {
        gate: true,
        metric: METRIC,
        names_check: true,
        landing_tip: None,
        rights: LabelRights::Cleared,
        skip_check: true,
        free: true,
    });
    let runs = refresh(&s.state).await;
    assert_eq!((runs[0].judged, runs[0].cleared), (90, 90), "{runs:?}");
    let pin = client_pin();
    let h = headers();
    let answer = okx_answer(&s).await;
    assert_eq!(
        answer["shard"],
        s.forged.to_string(),
        "the forged row is served"
    );
    assert_eq!(
        answer["servingCheck"]["crossChecked"], true,
        "and claimed cross-checked"
    );
    // It verifies against its finalized root on the client's node.
    let p = proven(&s, &answer).await;
    assert_eq!(p.verified().shard, s.forged);
    let (st, list, _) = get(&s, &format!("/v1/served/{METRIC}"), None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["shards"].as_array().expect("shards").len(), 90);
    // As listed: two roots over the last range.
    let e = link_requests(
        &list,
        &pin,
        "entity:okx",
        &answer,
        &headers(),
        &Limits::default(),
    )
    .expect_err("refused");
    assert!(e.contains("two roots listed"), "{e}");
    // The forged shard in place of the honest one.
    let forged_only = {
        let mut l = list.clone();
        l["shards"]
            .as_array_mut()
            .expect("shards")
            .retain(|x| x["shard"] != s.last.shard_id().to_string());
        l
    };
    // D22: the forged landing is its own span (one paid row); its base is
    // not the one the scan's total and its flows give.
    let swapped = fetch_links(&s, &forged_only, &pin, "entity:okx", &answer).await;
    assert_eq!(swapped.len(), 1);
    assert_eq!(swapped[0].verified().shard, s.forged);
    let e = check_reserve_answer(&answer, &swapped, &pin, &h, SCAN_HEIGHT).expect_err("refused");
    assert!(
        e.contains("base is not the one derived from the committed scan"),
        "{e}"
    );
    // The honest chain does not hold the answered row.
    let honest_only = {
        let mut l = list.clone();
        l["shards"]
            .as_array_mut()
            .expect("shards")
            .retain(|x| x["shard"] != s.forged.to_string());
        l
    };
    let (_, honest_answer, _) =
        get(&s, &format!("/v1/proofs/89/{}/3", s.last.shard_id()), None).await;
    // Review D3-R2-1: the forged answer is not in the honest list, so the
    // client refuses it before paying for any link.
    let e = link_requests(
        &honest_only,
        &pin,
        "entity:okx",
        &answer,
        &h,
        &Limits::default(),
    )
    .expect_err("refused");
    assert!(e.contains("not a link of the chain"), "{e}");
    let honest = fetch_links(&s, &honest_only, &pin, "entity:okx", &honest_answer).await;
    check_reserve_answer(&honest_answer, &honest, &pin, &h, SCAN_HEIGHT)
        .expect("the honest landing passes");
    let e = check_reserve_answer(&answer, &honest, &pin, &h, SCAN_HEIGHT).expect_err("refused");
    assert!(e.contains("not a link of the chain"), "{e}");
}

/// ROADMAP L14 with D3: the chain list is withheld like the rows it names.
/// Under `ONYX_RIGHTS=fallback` `/v1/served/btc.entity-reserve` is 403
/// `label_rights_withheld` (also with no shard listed and on a server
/// without the gate), so the client check reads nothing in fallback; a
/// server without the gate has no list (404 `no_serving_check`).
#[tokio::test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
async fn the_served_list_is_withheld_in_fallback() {
    for (rights, gate) in [
        (LabelRights::Fallback, true),
        (LabelRights::Fallback, false),
        (LabelRights::Cleared, false),
    ] {
        let s = served_with(Listing {
            gate,
            metric: METRIC,
            names_check: true,
            landing_tip: None,
            rights,
            skip_check: false,
            free: false,
        });
        refresh(&s.state).await;
        let (st, b, challenge) = get(&s, &format!("/v1/served/{METRIC}"), None).await;
        assert!(challenge.is_none());
        assert!(b.get("shards").is_none(), "{b}");
        if rights == LabelRights::Fallback {
            assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
            assert_eq!(b["error"]["code"], "label_rights_withheld");
        } else {
            assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
            assert_eq!(b["error"]["code"], "no_serving_check");
        }
    }
}
