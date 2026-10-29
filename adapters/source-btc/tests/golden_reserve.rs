//! Golden test of `btc-entity-reserve` (ADR-0034) and the committed reserve
//! history:
//! - on the committed fixture blocks: byte-identical output across runs,
//!   engines and a native build; the checked-in golden file; an independent
//!   Python recount; and R-RES-2: the rows equal `btc-entity-balance` run with
//!   the same base (its `end_balance_sats` and flows);
//! - the committed scan snapshot and base (`fixtures/reserves/`) match label
//!   set v2 and each other, and the snapshot hash is the pinned one;
//! - the website's export (`apps/web/static/history/reserves-week.json`)
//!   chains (R-RES-CHAIN-1), lands on the scan (R-RES-SCAN-1) and gives the
//!   pinned commitment;
//! - with the fetched blocks (`ONYX_RESERVE_DIRS=<week dir>:<tail dir>`), every
//!   shard reruns to the export's leaves. Without them that test says so and returns.
//!
//! Regenerate the golden file with
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden_reserve` (only with a recipe change).

use borsh::BorshDeserialize;
use onyx_btc_schema::{BalanceParams, EntityFlowRow};
use onyx_core::{
    keccak256, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, Snapshot, SourceAdapter,
    SourceId, SourceRef, StaticRegistry,
};
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, RunResult, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use onyx_source_btc::reserve::{
    check_chain, check_params_chain, reconcile, reserve_commitment, ReserveShard, ScanSnapshot,
};
use onyx_source_btc::{
    fixture_registry_reserve, fixture_registry_v2, load_label_set, node, verify_checksum,
    BtcFixtureAdapter, SCHEMA_ENTITY_RESERVE, SOURCE_ID,
};
use std::path::PathBuf;

const BLOCKS: &str = "btc/blocks-921080-921082.json";
const LABELS_V2: &str = "labels/test.onyx.entities.btc.v2.json";
const GOLDEN: &str = "golden/btc-entity-reserve-921080-921082.labels-v2.json";
const SCAN: &str = "reserves/scan-971199.json";
const BASE: &str = "reserves/base-969838.json";
/// As the website verifier pins them (`apps/web/verify` `pinned::RESERVES_*`).
const RESERVES_SNAPSHOT: &str =
    "0x01fd0e6a76c9bcdf722a498cbea2945a24ffd2227ab9991aab9523c84afad87b";
const RESERVES_COMMITMENT: &str =
    "0x03ef38cde4f543eb1ed230bff10ced0d819634e8d3c23767876ec064893ac00b";
const SCAN_HEIGHT: u64 = 971_199;
const SCAN_BLOCK: &str = "0000000000000000000040018970c37de87ac02d99e424a5f8e45064301cb536";
/// Fixture base for the golden range: 1,000 BTC per entity, a test value (no
/// scan exists for block 921,079); large enough that no balance goes negative.
const FIXTURE_BASE_SATS: u64 = 100_000_000_000;

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn fixtures() -> PathBuf {
    root().join("fixtures")
}

fn profile() -> RuntimeProfile {
    RuntimeProfile {
        abi_version: 1,
        host: HOST_ID.into(),
        fuel_limit: 4_000_000_000,
        max_memory_bytes: 256 << 20,
        max_output_bytes: 1 << 20,
        max_call_depth: 512,
    }
}

fn recipe(name: &str) -> (Vec<u8>, Hash32) {
    let wasm = std::fs::read(root().join("recipes/out").join(format!("{name}.wasm")))
        .expect("built recipe; run scripts/build-recipes.sh");
    let id = keccak256(&wasm);
    let recorded = std::fs::read_to_string(
        root()
            .join("recipes/out")
            .join(format!("{name}.wasm.keccak")),
    )
    .expect("keccak file");
    assert_eq!(
        recorded.trim(),
        id.to_string(),
        "{name}: id file out of date"
    );
    (wasm, id)
}

#[allow(clippy::too_many_arguments)]
fn spec(
    id: Hash32,
    schema: &[u8],
    reg: &StaticRegistry,
    snapshot: Snapshot,
    labels: &LabelSet,
    params: Vec<u8>,
) -> ShardSpec {
    let s = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: labels.name.clone(),
            version: labels.version,
            content_hash: labels.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(schema),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("registered source")
            .hash(),
        params,
        registry_root: reg.root(),
    };
    assert_eq!(s.validate(reg), Ok(()));
    s
}

fn reserve_rows(r: &RunResult) -> Vec<EntityReserveRow> {
    r.leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l).expect("row");
            let v = EntityReserveRow::try_from_slice(&row.value).expect("reserve row");
            assert_eq!(row.key, v.entity.as_bytes(), "key is the entity id");
            v
        })
        .collect()
}

fn rows_json(rows: &[EntityReserveRow]) -> serde_json::Value {
    serde_json::Value::Array(
        rows.iter()
            .map(|r| {
                serde_json::json!({"entity": r.entity, "base_height": r.base_height,
                "height": r.height, "base_sats": r.base_sats.to_string(),
                "received_sats": r.received_sats.to_string(), "sent_sats": r.sent_sats.to_string(),
                "tx_count": r.tx_count, "balance_sats": r.balance_sats.to_string()})
            })
            .collect(),
    )
}

fn h32(s: &str) -> Hash32 {
    let mut b = [0u8; 32];
    hex::decode_to_slice(s.trim_start_matches("0x"), &mut b).expect("hash");
    Hash32(b)
}

#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn reserve_golden_v2_labels() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let adapter = BtcFixtureAdapter::new(fixtures(), BLOCKS);
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let mut entities: Vec<String> = labels.labels.iter().map(|l| l.entity.clone()).collect();
    entities.sort();
    entities.dedup();
    let base: Vec<EntityBase> = entities
        .iter()
        .map(|e| EntityBase {
            entity: e.clone(),
            balance_sats: FIXTURE_BASE_SATS,
        })
        .collect();
    let params = ReserveParams {
        base_height: 921_079,
        base_ref: [0x11; 32],
        base: base.clone(),
    };
    let reg = fixture_registry_reserve();
    let (wasm, id) = recipe("btc_entity_reserve");
    let sp = spec(
        id,
        SCHEMA_ENTITY_RESERVE,
        &reg,
        snapshot.clone(),
        &labels,
        borsh::to_vec(&params).expect("params"),
    );
    let input = assemble_input(&sp, source.clone(), Some(&labels), &[], &reg)
        .expect("assemble")
        .bytes;
    let host = Host::new().expect("host");
    let first = host.run(&id, &wasm, &input, &profile()).expect("run");
    assert_eq!(first.outcome, Outcome::Ok);
    assert!(
        first.fuel_used <= profile().fuel_limit / 2,
        "fuel {}",
        first.fuel_used
    );

    // Repeated runs, the advisory fast path, and a native build agree.
    assert_eq!(
        host.run(&id, &wasm, &input, &profile()).expect("run"),
        first
    );
    let fast = onyx_recipe_host::fast::FastHost::with_parallel_compilation(false)
        .expect("fast host")
        .allowing(id)
        .run(&id, &wasm, &input, &profile())
        .expect("fast run");
    assert_eq!((fast.outcome, &fast.leaves), (first.outcome, &first.leaves));
    let native = btc_entity_reserve::run(RecipeInput::try_from_slice(&input).expect("input"))
        .expect("native run");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("encode"))
        .collect();
    assert_eq!(native_leaves, first.leaves);

    // R-RES-2: btc-entity-balance with the same base ends on the same balances
    // and flows.
    let rows = reserve_rows(&first);
    assert_eq!(rows.len(), entities.len(), "one row per entity");
    let breg = fixture_registry_v2();
    let (bwasm, bid) = recipe("btc_entity_balance");
    let bparams = BalanceParams {
        base: base
            .iter()
            .map(|b| onyx_btc_schema::EntityBase {
                entity: b.entity.clone(),
                balance_sats: b.balance_sats,
            })
            .collect(),
    };
    let bsp = spec(
        bid,
        b"onyx.btc.entity-flow-row.v1",
        &breg,
        snapshot,
        &labels,
        borsh::to_vec(&bparams).expect("params"),
    );
    let binput = assemble_input(&bsp, source, Some(&labels), &[], &breg)
        .expect("assemble")
        .bytes;
    let b = host.run(&bid, &bwasm, &binput, &profile()).expect("run");
    assert_eq!(b.outcome, Outcome::Ok);
    for (r, l) in rows.iter().zip(&b.leaves) {
        let row = OutputRow::try_from_slice(l).expect("row");
        let f = EntityFlowRow::try_from_slice(&row.value).expect("flow row");
        assert_eq!(f.entity, r.entity);
        assert_eq!(
            f.end_balance_sats,
            Some(u128::from(r.balance_sats)),
            "{}",
            r.entity
        );
        assert_eq!(
            (f.received_sats, f.sent_sats, f.tx_count),
            (r.received_sats, r.sent_sats, r.tx_count)
        );
    }
    assert!(
        rows.iter().any(|r| r.tx_count > 0),
        "labelled companies move coins here"
    );

    // Independent Python recount, row for row.
    let base_json = serde_json::json!({"base_height": 921_079, "base": base.iter()
        .map(|b| serde_json::json!({"entity": b.entity, "balance_sats": b.balance_sats.to_string()}))
        .collect::<Vec<_>>()});
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/recount-entity-reserve.py"))
        .arg("shard")
        .arg(fixtures().join(BLOCKS))
        .arg(fixtures().join(LABELS_V2))
        .arg(base_json.to_string())
        .args(["921080", "921082"])
        .output()
        .expect("python3 runs");
    assert!(
        out.status.success(),
        "recount: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recount: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(recount, rows_json(&rows));

    // Checked-in golden file.
    let doc = serde_json::json!({
        "recipe_id": id.to_string(),
        "input_keccak": keccak256(&input).to_string(),
        "shard_id": sp.shard_id().to_string(),
        "root": first.root().to_string(),
        "fuel": first.fuel_used,
        "rows": rows_json(&rows),
    });
    let text = serde_json::to_string_pretty(&doc).expect("json") + "\n";
    let path = fixtures().join(GOLDEN);
    if std::env::var_os("ONYX_BLESS").is_some() {
        // Never re-bless in CI: a blessed run would pass against its own output (R-GOLD-2).
        assert!(
            std::env::var_os("CI").is_none(),
            "ONYX_BLESS must not be set in CI"
        );
        std::fs::write(&path, &text).expect("write golden");
    }
    let on_disk = std::fs::read_to_string(&path).expect("golden file; run with ONYX_BLESS=1");
    assert_eq!(on_disk, text, "golden output changed");
}

/// The recipe's address decoder is a copy (so `btc-entity-balance`'s bytes
/// never change); both decode every v2 label to the same script.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_decoders_agree_on_every_label() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    for l in &labels.labels {
        assert_eq!(
            btc_entity_reserve::address_script(&l.address),
            btc_entity_balance::address_script(&l.address),
            "{}",
            l.address
        );
    }
}

fn scan() -> ScanSnapshot {
    verify_checksum(&fixtures(), SCAN).expect("scan checksum");
    serde_json::from_slice(&std::fs::read(fixtures().join(SCAN)).expect("scan")).expect("scan json")
}

fn base_doc() -> serde_json::Value {
    verify_checksum(&fixtures(), BASE).expect("base checksum");
    serde_json::from_slice(&std::fs::read(fixtures().join(BASE)).expect("base")).expect("base json")
}

fn amounts(v: &serde_json::Value) -> Vec<EntityBase> {
    v.as_array()
        .expect("list")
        .iter()
        .map(|b| EntityBase {
            entity: b["entity"].as_str().expect("entity").into(),
            balance_sats: b["balance_sats"]
                .as_str()
                .expect("sats")
                .parse()
                .expect("int"),
        })
        .collect()
}

/// RES-2 on the committed data: the scan is a scan of label set v2 at the
/// pinned block, its hash is the pinned one, and the base is the scan minus
/// the recorded net flows.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_committed_scan_and_base_match_label_set_v2() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let s = scan();
    assert_eq!(s.check_labels(&labels), Ok(()));
    assert_eq!((s.height, s.block_hash.as_str()), (SCAN_HEIGHT, SCAN_BLOCK));
    assert_eq!(s.hash().to_string(), RESERVES_SNAPSHOT);
    let totals = s.entity_totals(&labels).expect("totals");
    let d = base_doc();
    assert_eq!(d["snapshot_height"].as_u64(), Some(SCAN_HEIGHT));
    let base = amounts(&d["base"]);
    let net: std::collections::BTreeMap<String, i128> = d["net_sats"]
        .as_object()
        .expect("net")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("s").parse().expect("int")))
        .collect();
    assert_eq!(
        onyx_source_btc::reserve::derive_base(&totals, &net),
        Ok(base)
    );
}

fn export() -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/reserves-week.json")).expect("export"),
    )
    .expect("export json")
}

fn export_shards(doc: &serde_json::Value) -> Vec<ReserveShard> {
    doc["shards"]
        .as_array()
        .expect("shards")
        .iter()
        .map(|s| {
            let leaves: Vec<Vec<u8>> = s["leaves"]
                .as_array()
                .expect("leaves")
                .iter()
                .map(|l| {
                    hex::decode(l.as_str().expect("leaf").trim_start_matches("0x")).expect("hex")
                })
                .collect();
            let root = MerkleTree::from_rows(&leaves).root();
            assert_eq!(
                root,
                h32(s["root"].as_str().expect("root")),
                "leaves rebuild the root"
            );
            ReserveShard {
                from: s["from"].as_u64().expect("from"),
                to: s["to"].as_u64().expect("to"),
                root,
                rows: leaves
                    .iter()
                    .map(|l| {
                        let row = OutputRow::try_from_slice(l).expect("row");
                        EntityReserveRow::try_from_slice(&row.value).expect("reserve row")
                    })
                    .collect(),
            }
        })
        .collect()
}

/// R-RES-CHAIN-1 and R-RES-SCAN-1 on the export: the chain links from the
/// committed base through every shard and lands on the committed scan exactly;
/// the commitment is the pinned one.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_export_chains_onto_the_scan_and_gives_the_pinned_commitment() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let doc = export();
    let s = scan();
    let totals = s.entity_totals(&labels).expect("totals");
    let base = amounts(&base_doc()["base"]);
    assert_eq!(amounts(&doc["base"]["rows"]), base);
    assert_eq!(amounts(&doc["snapshot"]["totals"]), totals);
    let shards = export_shards(&doc);
    let end = check_chain(969_838, &base, &s.hash(), &shards).expect("chain");
    assert_eq!(end, totals, "the chain lands on the scan");
    assert_eq!(shards.last().map(|x| x.to), Some(SCAN_HEIGHT));
    let (_, id) = recipe("btc_entity_reserve");
    assert_eq!(doc["recipe_id"].as_str(), Some(id.to_string().as_str()));
    let mut block = [0u8; 32];
    hex::decode_to_slice(SCAN_BLOCK, &mut block).expect("hex");
    block.reverse();
    let c = reserve_commitment(
        &id,
        &labels.content_hash(),
        &s.hash(),
        SCAN_HEIGHT,
        &block,
        969_838,
        &base,
        &totals,
        &shards,
    );
    assert_eq!(c.to_string(), RESERVES_COMMITMENT);
    assert_eq!(doc["commitment"].as_str(), Some(RESERVES_COMMITMENT));
    // A base one satoshi off no longer chains onto the scan.
    let mut off = base.clone();
    off[0].balance_sats += 1;
    assert!(check_chain(969_838, &off, &s.hash(), &shards).is_err());
}

#[test]
fn every_reserve_shard_reruns_to_the_export() {
    let Some(dirs) = std::env::var("ONYX_RESERVE_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
    else {
        eprintln!(
            "skipped: set ONYX_RESERVE_DIRS=<week dir>:<tail dir> (blocks 969839-970846 and \
             970847-{SCAN_HEIGHT}, scripts/fetch-btc-node.sh) to rerun the reserve chain"
        );
        return;
    };
    let (week, tail) = dirs.split_once(':').expect("<week dir>:<tail dir>");
    let (week, tail) = (root().join(week), root().join(tail));
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let reg = fixture_registry_reserve();
    let (wasm, id) = recipe("btc_entity_reserve");
    let host = Host::new().expect("host");
    let doc = export();
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    let last = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: Some((SCAN_HEIGHT, SCAN_BLOCK.to_string())),
    };
    let s = scan();
    let (mut height, mut state, mut link) = (969_838u64, amounts(&base_doc()["base"]), s.hash());
    for sh in doc["shards"].as_array().expect("shards") {
        let (from, to) = (
            sh["from"].as_u64().expect("f"),
            sh["to"].as_u64().expect("t"),
        );
        let dir = if sh["week"].as_bool() == Some(true) {
            &week
        } else {
            &tail
        };
        let p = if to == SCAN_HEIGHT { &last } else { &pin };
        let (source, snapshot) = node::load_node_range(dir, from, to, p).expect("range");
        let params = ReserveParams {
            base_height: height,
            base_ref: link.0,
            base: state.clone(),
        };
        let sp = spec(
            id,
            SCHEMA_ENTITY_RESERVE,
            &reg,
            snapshot,
            &labels,
            borsh::to_vec(&params).expect("p"),
        );
        let input = assemble_input(&sp, source, Some(&labels), &[], &reg)
            .expect("assemble")
            .bytes;
        let r = host.run(&id, &wasm, &input, &profile()).expect("run");
        assert!(
            r.fuel_used <= profile().fuel_limit / 2,
            "{from}-{to}: fuel {}",
            r.fuel_used
        );
        let want: Vec<Vec<u8>> = sh["leaves"]
            .as_array()
            .expect("leaves")
            .iter()
            .map(|l| hex::decode(l.as_str().expect("l").trim_start_matches("0x")).expect("hex"))
            .collect();
        assert_eq!(r.leaves, want, "{from}-{to}");
        state = reserve_rows(&r)
            .iter()
            .map(|x| EntityBase {
                entity: x.entity.clone(),
                balance_sats: x.balance_sats,
            })
            .collect();
        height = to;
        link = r.root();
        eprintln!("reserve shard {from}-{to}: reruns to the export");
    }
}

/// RES-1 on real data: a second scan three blocks later reconciles with the
/// first, address by address, through the flows of the blocks between
/// (`fixtures/reserves/flows-971200-971202.jsonl`, streamed from the node by
/// `scripts/stream-reserve-flows.py`, which checked the chain links). The
/// flows link the two scans' blocks. Dropping one address's flow breaks it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn two_committed_scans_reconcile() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let t0 = scan();
    verify_checksum(&fixtures(), "reserves/scan-971202.json").expect("checksum");
    let t1: ScanSnapshot = serde_json::from_slice(
        &std::fs::read(fixtures().join("reserves/scan-971202.json")).expect("scan"),
    )
    .expect("scan json");
    assert_eq!(t1.check_labels(&labels), Ok(()));
    let rel = "reserves/flows-971200-971202.jsonl";
    verify_checksum(&fixtures(), rel).expect("checksum");
    let text = std::fs::read_to_string(fixtures().join(rel)).expect("flows");
    let mut flows: std::collections::BTreeMap<String, (u128, u128)> = Default::default();
    let mut prev = t0.block_hash.clone();
    let mut height = t0.height;
    for line in text.lines() {
        let b: serde_json::Value = serde_json::from_str(line).expect("line");
        height += 1;
        assert_eq!(b["height"].as_u64(), Some(height), "contiguous");
        assert_eq!(
            b["prev"].as_str(),
            Some(prev.as_str()),
            "{height} links to the block before"
        );
        prev = b["hash"].as_str().expect("hash").to_string();
        for (a, v) in b["flows"].as_object().expect("flows") {
            let e = flows.entry(format!("btc:{a}")).or_default();
            e.0 += u128::from(v[0].as_u64().expect("rx"));
            e.1 += u128::from(v[1].as_u64().expect("tx"));
        }
    }
    assert_eq!(
        (height, prev.as_str()),
        (t1.height, t1.block_hash.as_str()),
        "flows end at the second scan"
    );
    assert!(
        !flows.is_empty(),
        "labelled addresses moved coins in between"
    );
    assert_eq!(
        onyx_source_btc::reserve::reconcile(&t0, &t1, &flows),
        Ok(vec![])
    );
    let first = flows
        .iter()
        .find(|(_, (rx, tx))| rx != tx)
        .map(|(a, _)| a.clone())
        .expect("an address whose balance moved");
    flows.remove(&first);
    assert_eq!(
        onyx_source_btc::reserve::reconcile(&t0, &t1, &flows),
        Ok(vec![first])
    );
}

fn committed_flows(rel: &str) -> std::collections::BTreeMap<String, (u128, u128)> {
    verify_checksum(&fixtures(), rel).expect("checksum");
    let mut flows: std::collections::BTreeMap<String, (u128, u128)> = Default::default();
    for line in std::fs::read_to_string(fixtures().join(rel))
        .expect("flows")
        .lines()
    {
        let b: serde_json::Value = serde_json::from_str(line).expect("line");
        for (a, v) in b["flows"].as_object().expect("flows") {
            let e = flows.entry(format!("btc:{a}")).or_default();
            e.0 += u128::from(v[0].as_u64().expect("rx"));
            e.1 += u128::from(v[1].as_u64().expect("tx"));
        }
    }
    flows
}

/// RES-1 residual (review RES-R1 F1), pinned so the docs stay honest: a node
/// that never reports one address (here the largest Binance address that did
/// not move between the scans) passes every check that uses only that node.
/// Both scans still match the label set and reconcile, and a chain from the
/// lowered base lands on the lowered scan exactly. Only an independent
/// re-scan or an independently published figure (OKX's PoR file, Bitwise's
/// list) shows the omission. If this test fails, a same-node check got
/// stronger: update THREATS RES-1.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn res_1_residual_a_consistent_omission_passes_the_same_node_checks() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let mut s0 = scan();
    let mut s1: ScanSnapshot = serde_json::from_slice(
        &std::fs::read(fixtures().join("reserves/scan-971202.json")).expect("scan"),
    )
    .expect("scan json");
    let flows = committed_flows("reserves/flows-971200-971202.jsonl");
    let victim = s0
        .rows
        .iter()
        .filter(|r| r.entity == "entity:binance" && !flows.contains_key(&r.address))
        .max_by_key(|r| r.sats)
        .expect("an unmoved Binance address")
        .clone();
    let x = victim.sats;
    assert!(x > 100_000 * 100_000_000, "a large address: {x} sats");
    s0.rows.retain(|r| r.address != victim.address);
    s1.rows.retain(|r| r.address != victim.address);
    assert_eq!(s0.check_labels(&labels), Ok(()));
    assert_eq!(s1.check_labels(&labels), Ok(()));
    assert_eq!(reconcile(&s0, &s1, &flows), Ok(vec![]));

    // The chain from the lowered base (the recipe is linear in the base; the
    // reruns of that are in the review's PoC) lands on the lowered scan.
    let doc = export();
    let mut base = amounts(&base_doc()["base"]);
    assert_eq!(base[0].entity, "entity:binance");
    base[0].balance_sats -= x;
    let mut shards = export_shards(&doc);
    assert!(
        shards.iter().all(|s| s.rows[0].balance_sats > x),
        "never negative"
    );
    let mut link = s0.hash();
    for s in &mut shards {
        s.rows[0].base_sats -= x;
        s.rows[0].balance_sats -= x;
        for r in &mut s.rows {
            r.base_ref = link.0;
        }
        let leaves: Vec<Vec<u8>> = s
            .rows
            .iter()
            .map(|r| {
                borsh::to_vec(&OutputRow {
                    key: r.entity.as_bytes().to_vec(),
                    value: borsh::to_vec(r).expect("row"),
                })
                .expect("leaf")
            })
            .collect();
        s.root = MerkleTree::from_rows(&leaves).root();
        link = s.root;
    }
    let end = check_chain(969_838, &base, &s0.hash(), &shards).expect("chain");
    assert_eq!(end, s0.entity_totals(&labels).expect("totals"));
}

/// RES-7 (review RES-R1 F2): a shard on a forged base reruns fine, and miner
/// and validator agree (the recipe is a pure function of its params), so a
/// rerun cannot see it; `check_params_chain` from the committed start refuses
/// it. A base above 21 million BTC is refused in the recipe itself (RES-8).
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn a_forged_base_reruns_but_the_params_chain_refuses_it() {
    let labels = load_label_set(&fixtures(), LABELS_V2).expect("labels v2");
    let adapter = BtcFixtureAdapter::new(fixtures(), BLOCKS);
    let snapshot = adapter.range_snapshot().expect("snapshot");
    let source = adapter.load(&snapshot).expect("load");
    let reg = fixture_registry_reserve();
    let (wasm, id) = recipe("btc_entity_reserve");
    let host = Host::new().expect("host");
    let mut entities: Vec<String> = labels.labels.iter().map(|l| l.entity.clone()).collect();
    entities.sort();
    entities.dedup();
    let mk = |sats: u64| ReserveParams {
        base_height: 921_079,
        base_ref: [0x11; 32],
        base: entities
            .iter()
            .map(|e| EntityBase {
                entity: e.clone(),
                balance_sats: sats,
            })
            .collect(),
    };
    let run = |p: &ReserveParams| {
        let sp = spec(
            id,
            SCHEMA_ENTITY_RESERVE,
            &reg,
            snapshot.clone(),
            &labels,
            borsh::to_vec(p).expect("params"),
        );
        let input = assemble_input(&sp, source.clone(), Some(&labels), &[], &reg)
            .expect("assemble")
            .bytes;
        host.run(&id, &wasm, &input, &profile()).expect("run")
    };
    // Committed start for this test: 1,000 BTC per entity. A forged 2,000.
    let committed = mk(FIXTURE_BASE_SATS);
    let forged = mk(2 * FIXTURE_BASE_SATS);
    let a = run(&forged);
    let b = run(&forged);
    assert_eq!(a.outcome, Outcome::Ok, "a forged base runs");
    assert_eq!(a.root(), b.root(), "and miner and validator agree");
    let shard = ReserveShard {
        from: 921_080,
        to: 921_082,
        root: a.root(),
        rows: reserve_rows(&a),
    };
    let snap = Hash32([0x11; 32]);
    assert!(check_params_chain(
        921_079,
        &committed.base,
        &snap,
        &[forged],
        std::slice::from_ref(&shard)
    )
    .is_err());
    let honest = run(&committed);
    let hshard = ReserveShard {
        from: 921_080,
        to: 921_082,
        root: honest.root(),
        rows: reserve_rows(&honest),
    };
    assert!(check_params_chain(
        921_079,
        &committed.base,
        &snap,
        std::slice::from_ref(&committed),
        &[hshard]
    )
    .is_ok());
    // 21 million BTC per entity: refused in the sandbox (R-RES-CAP-1).
    assert_eq!(
        run(&mk(2_100_000_000_000_000)).outcome,
        Outcome::RecipeError(btc_entity_reserve::code::ABOVE_SUPPLY)
    );
}

/// R-RES-NEG-2 in the Python derivation (`recount-entity-reserve.py derive`):
/// an address whose scan minus its own flows is negative is refused, also when
/// its entity's total covers it.
#[test]
#[cfg_attr(
    not(feature = "label-data"),
    ignore = "needs the label data, not public yet"
)]
fn the_python_derivation_checks_each_address() {
    let mut s = scan();
    let bin: Vec<_> = s
        .rows
        .iter()
        .filter(|r| r.entity == "entity:binance")
        .take(2)
        .cloned()
        .collect();
    s.rows = bin.clone();
    let dir = std::env::temp_dir().join(format!("onyx-res-neg2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let snap = dir.join("scan.json");
    std::fs::write(&snap, serde_json::to_vec(&s).expect("json")).expect("write");
    let derive = |rx: u64| {
        // One block of flows: the first address received rx and sent nothing.
        let a = bin[0].address.trim_start_matches("btc:");
        let line = serde_json::json!({"height": s.height, "hash": "", "prev": "",
            "flows": {a: [rx, 0]}});
        let flows = dir.join("flows.jsonl");
        std::fs::write(&flows, line.to_string() + "\n").expect("write");
        std::process::Command::new("python3")
            .arg("-I")
            .arg(root().join("scripts/recount-entity-reserve.py"))
            .arg("derive")
            .arg(&snap)
            .arg(fixtures().join(LABELS_V2))
            .arg(&flows)
            .arg((s.height - 1).to_string())
            .arg(dir.join("base.json"))
            .output()
            .expect("python3 runs")
    };
    assert!(derive(bin[0].sats).status.success());
    // One satoshi more than the address holds: its entity still covers it.
    assert!(bin[1].sats > 1);
    let out = derive(bin[0].sats + 1);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("negative"));
    let _ = std::fs::remove_dir_all(&dir);
}
