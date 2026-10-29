//! Export a history for the website's one-week history: consecutive shards of
//! `btc-entity-flow-series` and `btc-entity-balance` over blocks fetched from
//! a node, each shard preflighted (<= half its fuel), recounted in Python and
//! checked with R-SERIES-2 (series sums equal the range rows). Shards never
//! cross a UTC day ([`plan_days`]).
//!
//! ```text
//! scripts/fetch-btc-node.sh indexer 969839 970846 fixtures-live/week
//! ONYX_HISTORY=fixtures-live/week:969839:970846 \
//!   cargo run --release -p onyx-source-btc --example export-history -- \
//!   apps/web/static/history/week
//! ```
//!
//! Writes `index.json` (headers, every shard's range and roots, the pinned
//! commitment, and per-day six-block buckets for the 7-day chart) and one
//! `<YYYY-MM-DD>.json` per day with that day's shard leaves. The website loads
//! the index first and a day's leaves when it checks that day. Prints the
//! commitment the website pins (`apps/web/verify` `pinned::HISTORY_*`).

use borsh::BorshDeserialize;
use onyx_btc_schema::{BtcSource, EntityBlockFlowRow, EntityFlowRow, SERIES_NO_ACTIVITY};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{
    keccak256, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef,
    StaticRegistry,
};
use onyx_labels::attest::AttestCache;
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, failure_leaf, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::history::{
    estimated_fuel, history_commitment, plan_days, HistoryShard, BUCKET_BLOCKS, PLAN_FUEL_TARGET,
};
use onyx_source_btc::{
    fixture_registry_series, fixture_registry_v2, load_label_set, node, valid_failure_codes,
    SCHEMA_ENTITY_FLOW_SERIES, SOURCE_ID,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

const LABELS: &str = "labels/test.onyx.entities.btc.v2.json";
const BALANCE_SCHEMA: &[u8] = b"onyx.btc.entity-flow-row.v1";

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
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

fn spec(
    id: Hash32,
    schema: &[u8],
    reg: &StaticRegistry,
    snapshot: onyx_core::Snapshot,
    set: &LabelSet,
) -> Result<ShardSpec, Box<dyn std::error::Error>> {
    Ok(ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: Some(LabelSetRef {
            name: set.name.clone(),
            version: set.version,
            content_hash: set.content_hash(),
        }),
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(schema),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    })
}

struct ShardRun {
    leaves: Vec<Vec<u8>>,
    fuel: u64,
    secs: f64,
    took: std::time::Duration,
}

/// Preflight a shard, then run it on the canonical engine.
#[allow(clippy::too_many_arguments)]
fn run_shard(
    host: &Host,
    cas: &MemCas,
    reg: &StaticRegistry,
    wasm: &[u8],
    schema: &[u8],
    source: &[u8],
    snapshot: onyx_core::Snapshot,
    set: &LabelSet,
) -> Result<ShardRun, Box<dyn std::error::Error>> {
    let id = keccak256(wasm);
    let s = spec(id, schema, reg, snapshot, set)?;
    let allowed = valid_failure_codes().into_iter().collect();
    let fuel = preflight(
        &s,
        source.to_vec(),
        reg,
        cas,
        host,
        &AttestCache::new(),
        &allowed,
    )
    .map_err(|e| format!("preflight refused the shard: {e:?}"))?;
    let input = assemble_input(&s, source.to_vec(), Some(set), &[], reg)?.bytes;
    let t = Instant::now();
    let r = host.run(&id, wasm, &input, &profile())?;
    if r.fuel_used != fuel {
        return Err(format!("fuel differs from the preflight: {} vs {fuel}", r.fuel_used).into());
    }
    Ok(ShardRun {
        leaves: r.leaves,
        fuel,
        secs: t.elapsed().as_secs_f64(),
        took: t.elapsed(),
    })
}

fn series_rows(leaves: &[Vec<u8>]) -> Result<Vec<EntityBlockFlowRow>, Box<dyn std::error::Error>> {
    if leaves == [failure_leaf(Outcome::RecipeError(SERIES_NO_ACTIVITY))] {
        return Ok(Vec::new());
    }
    leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l)?;
            Ok(EntityBlockFlowRow::try_from_slice(&row.value)?)
        })
        .collect()
}

fn range_rows(leaves: &[Vec<u8>]) -> Result<Vec<EntityFlowRow>, Box<dyn std::error::Error>> {
    leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l)?;
            Ok(EntityFlowRow::try_from_slice(&row.value)?)
        })
        .collect()
}

fn row_json(r: &EntityBlockFlowRow) -> Value {
    json!({"entity": r.entity, "height": r.height,
        "received_sats": r.received_sats.to_string(), "sent_sats": r.sent_sats.to_string(),
        "net_sats": r.net_sats.to_string(), "tx_count": r.tx_count})
}

/// Per-entity sums of a day's rows in [`BUCKET_BLOCKS`]-block buckets that
/// start at the day's first block (the 7-day chart; the website checks a
/// day's buckets against that day's leaves).
fn buckets(day_from: u64, day_to: u64, rows: &[EntityBlockFlowRow]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut b = day_from;
    while b <= day_to {
        let e = (b + BUCKET_BLOCKS - 1).min(day_to);
        let mut sums: BTreeMap<&str, (u128, u128, i128, u64)> = BTreeMap::new();
        for r in rows.iter().filter(|r| (b..=e).contains(&r.height)) {
            let s = sums.entry(r.entity.as_str()).or_default();
            s.0 += r.received_sats;
            s.1 += r.sent_sats;
            s.2 += r.net_sats;
            s.3 += r.tx_count;
        }
        let entities: Vec<Value> = sums
            .iter()
            .map(|(k, v)| {
                json!({"entity": k, "received_sats": v.0.to_string(),
                "sent_sats": v.1.to_string(), "net_sats": v.2.to_string(), "tx_count": v.3})
            })
            .collect();
        out.push(json!({"from": b, "to": e, "entities": entities}));
        b = e + 1;
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    let spec_text =
        std::env::var("ONYX_HISTORY").map_err(|_| "set ONYX_HISTORY=<dir>:<from>:<to>")?;
    let parts: Vec<&str> = spec_text.split(':').collect();
    let [dir, from, to] = parts[..] else {
        return Err("ONYX_HISTORY must be <dir>:<from>:<to>".into());
    };
    let (from, to): (u64, u64) = (from.parse()?, to.parse()?);
    let dir = root.join(dir);
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("apps/web/static/history/week"));
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    let set = load_label_set(&fixtures, LABELS)?;
    let host = Host::new()?;
    let swasm = std::fs::read(root.join("recipes/out/btc_entity_flow_series.wasm"))?;
    let bwasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let (sid, bid) = (keccak256(&swasm), keccak256(&bwasm));
    let sreg = fixture_registry_series();
    let breg = fixture_registry_v2();
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &swasm)?;
    cas.put(Kind::Recipe, &bwasm)?;
    cas.put_label_set(&set)?;
    cas.put(Kind::Blob, &[])?;

    // 1. Headers (proof of work, chain links) and inputs + outputs per block.
    let mut headers = Vec::new();
    let mut io = Vec::new();
    let mut times = Vec::new();
    let mut prev: Option<[u8; 32]> = None;
    let mut tip = [0u8; 32];
    for h in from..=to {
        let raw = std::fs::read_to_string(dir.join(format!("{h}.raw.hex")))?;
        let header = raw.get(..160).ok_or("short block file")?.to_string();
        let (hash, parent) =
            node::check_raw_header(&hex::decode(&header)?, node::MAINNET_MIN_BITS)?;
        if prev.is_some_and(|p| p != parent) {
            return Err(format!("block {h} does not extend block {}", h - 1).into());
        }
        prev = Some(hash);
        tip = hash;
        let raw = hex::decode(&header)?;
        let time = u32::from_le_bytes(raw[68..72].try_into()?);
        times.push(u64::from(time));
        headers.push(header);
        let (src, _) = node::load_node_range(&dir, h, h, &pin)?;
        let parsed = BtcSource::try_from_slice(&src)?;
        let n: usize = parsed
            .blocks
            .iter()
            .flat_map(|b| b.txs.iter())
            .map(|t| t.spent.len() + t.created.len())
            .sum();
        io.push((h, n as u64));
    }

    // 2. Plan by UTC day and inputs + outputs, confirm with the preflight, run
    //    both recipes.
    let blocks: Vec<(u64, u64, u64)> = io
        .iter()
        .zip(&times)
        .map(|(&(h, n), &t)| (h, n, t))
        .collect();
    let days = plan_days(&blocks, PLAN_FUEL_TARGET);
    let plan: Vec<(u64, u64)> = days.iter().flat_map(|d| d.3.iter().copied()).collect();
    let mut day_leaves: BTreeMap<u64, Vec<Value>> = BTreeMap::new();
    let mut day_rows: BTreeMap<u64, Vec<EntityBlockFlowRow>> = BTreeMap::new();
    let day_of = |h: u64| -> u64 {
        days.iter()
            .find(|d| (d.1..=d.2).contains(&h))
            .map_or(0, |d| d.1)
    };
    let started = Instant::now();
    let (mut max_fuel, mut series_time) = (0u64, std::time::Duration::ZERO);
    let mut shards_json = Vec::new();
    let mut shards = Vec::new();
    let mut all_rows = 0usize;
    let mut entities: BTreeMap<String, ()> = BTreeMap::new();
    for (sfrom, sto) in plan {
        let (source, snapshot) = node::load_node_range(&dir, sfrom, sto, &pin)?;
        let shard_io: u64 = io
            .iter()
            .filter(|(h, _)| (sfrom..=sto).contains(h))
            .map(|b| b.1)
            .sum();
        let s = run_shard(
            &host,
            &cas,
            &sreg,
            &swasm,
            SCHEMA_ENTITY_FLOW_SERIES,
            &source,
            snapshot.clone(),
            &set,
        )?;
        let b = run_shard(
            &host,
            &cas,
            &breg,
            &bwasm,
            BALANCE_SCHEMA,
            &source,
            snapshot,
            &set,
        )?;
        let rows = series_rows(&s.leaves)?;
        let range = range_rows(&b.leaves)?;
        // R-SERIES-2: each entity's rows add up to its range row.
        let mut sums: BTreeMap<&str, (u128, u128, i128, u64)> = BTreeMap::new();
        for r in &rows {
            let e = sums.entry(r.entity.as_str()).or_default();
            e.0 += r.received_sats;
            e.1 += r.sent_sats;
            e.2 += r.net_sats;
            e.3 += r.tx_count;
        }
        for r in &range {
            let got = sums.remove(r.entity.as_str()).unwrap_or_default();
            if got != (r.received_sats, r.sent_sats, r.net_sats, r.tx_count) {
                return Err(format!("{sfrom}-{sto} {}: series does not add up", r.entity).into());
            }
            entities.insert(r.entity.clone(), ());
        }
        if !sums.is_empty() {
            return Err(format!("{sfrom}-{sto}: series names entities the range lacks").into());
        }
        // Independent Python recount of every row of the shard.
        let recount = std::process::Command::new("python3")
            .arg("-I")
            .arg(root.join("scripts/recount-entity-flow-series.py"))
            .arg(&dir)
            .arg(fixtures.join(LABELS))
            .arg(sfrom.to_string())
            .arg(sto.to_string())
            .output()?;
        if !recount.status.success() {
            return Err(format!(
                "recount failed: {}",
                String::from_utf8_lossy(&recount.stderr)
            )
            .into());
        }
        let recounted: Value = serde_json::from_slice(&recount.stdout)?;
        let mine = Value::Array(rows.iter().map(row_json).collect());
        if recounted != mine {
            return Err(format!("{sfrom}-{sto}: the Python recount disagrees").into());
        }
        let shard = HistoryShard {
            from: sfrom,
            to: sto,
            series_root: MerkleTree::from_rows(&s.leaves).root(),
            balance_root: MerkleTree::from_rows(&b.leaves).root(),
        };
        eprintln!(
            "shard {sfrom}-{sto}: {} blocks, io {shard_io} (est {}), series fuel {} ({:.2} s), \
             balance fuel {}, rows {}",
            sto - sfrom + 1,
            estimated_fuel(shard_io),
            s.fuel,
            s.secs,
            b.fuel,
            rows.len()
        );
        shards_json.push(json!({"from": sfrom, "to": sto, "io": shard_io,
            "series_root": hex0x(&shard.series_root.0), "balance_root": hex0x(&shard.balance_root.0),
            "series_fuel": s.fuel, "balance_fuel": b.fuel,
            "no_activity": rows.is_empty()}));
        day_leaves
            .entry(day_of(sfrom))
            .or_default()
            .push(json!({"from": sfrom, "to": sto,
            "series_leaves": s.leaves.iter().map(hex::encode).collect::<Vec<_>>(),
            "balance_leaves": b.leaves.iter().map(hex::encode).collect::<Vec<_>>()}));
        max_fuel = max_fuel.max(s.fuel).max(b.fuel);
        series_time += s.took;
        all_rows += rows.len();
        day_rows.entry(day_of(sfrom)).or_default().extend(rows);
        shards.push(shard);
    }

    let commitment = history_commitment(&sid, &bid, &set.content_hash(), &tip, &shards);
    let mut tip_display = tip;
    tip_display.reverse();
    std::fs::create_dir_all(&out)?;
    let mut days_json = Vec::new();
    for (date, dfrom, dto, dshards) in &days {
        let file = format!("{date}.json");
        let rows = day_rows.remove(dfrom).unwrap_or_default();
        let leaves = day_leaves.remove(dfrom).unwrap_or_default();
        let doc = json!({"version": 2, "date": date, "from": dfrom, "to": dto, "shards": leaves});
        std::fs::write(out.join(&file), serde_json::to_string(&doc)? + "\n")?;
        days_json.push(json!({"date": date, "from": dfrom, "to": dto, "file": file,
            "shards": dshards.len(), "rows": rows.len(),
            "buckets": buckets(*dfrom, *dto, &rows)}));
    }
    let index = json!({
        "version": 2,
        "generated_by": "ONYX_HISTORY=<dir>:<from>:<to> cargo run --release -p onyx-source-btc --example export-history -- <out dir>",
        "from": from, "to": to,
        "tip": hex::encode(tip_display),
        "series_recipe_id": hex0x(&sid.0),
        "balance_recipe_id": hex0x(&bid.0),
        "label_set": {"name": set.name, "version": set.version, "content_hash": hex0x(&set.content_hash().0)},
        "commitment": hex0x(&commitment.0),
        "python_recount": "scripts/recount-entity-flow-series.py: every row of every shard matches",
        "entities": entities.keys().collect::<Vec<_>>(),
        "bucket_blocks": BUCKET_BLOCKS,
        "headers": headers,
        "shards": shards_json,
        "days": days_json,
    });
    std::fs::write(
        out.join("index.json"),
        serde_json::to_string(&index)? + "\n",
    )?;
    eprintln!(
        "wrote {} ({} days, {} shards, {} rows; max fuel {max_fuel}; series run {:.1} s; \
         total {:.1} s); commitment {}; tip {}",
        out.display(),
        days.len(),
        shards.len(),
        all_rows,
        series_time.as_secs_f64(),
        started.elapsed().as_secs_f64(),
        hex0x(&commitment.0),
        hex::encode(tip_display)
    );
    Ok(())
}
