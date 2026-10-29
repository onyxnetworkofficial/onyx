//! Export the one-week reserve history for the website (ADR-0034): a chain of
//! `btc-entity-reserve` shards over label set v2, from a base derived from a
//! committed UTXO scan, through the week and on to the scan's block, where
//! the chain must land on the scan exactly.
//!
//! ```text
//! # label set hash for the snapshot document
//! cargo run --release -p onyx-source-btc --example export-reserves -- label-hash
//! # the week (fixtures-live/week) + the blocks after it up to the scan (fixtures-live/tail)
//! cargo run --release -p onyx-source-btc --example export-reserves -- export \
//!   fixtures-live/week fixtures-live/tail apps/web/static/history/week/index.json \
//!   fixtures/reserves/scan-971199.json fixtures/reserves/base-969838.json \
//!   apps/web/static/history/reserves-week.json
//! ```
//!
//! Every shard is preflighted (<= half its fuel), run on the canonical engine,
//! recounted in Python (`scripts/recount-entity-reserve.py shard`), and, for
//! the week, checked against the history's `btc-entity-balance` rows for the
//! same range (R-RES-2). Prints the pins for `apps/web/verify` (`pinned::RESERVES_*`).

use borsh::BorshDeserialize;
use onyx_btc_schema::{BtcSource, EntityFlowRow};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{
    keccak256, Hash32, LabelSetRef, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef,
};
use onyx_labels::attest::AttestCache;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_reserve_schema::{EntityBase, EntityReserveRow, ReserveParams};
use onyx_source_btc::history::{plan_shards, PLAN_FUEL_TARGET};
use onyx_source_btc::reserve::{
    check_params_chain, reserve_commitment, ReserveShard, ScanSnapshot,
};
use onyx_source_btc::{
    fixture_registry_reserve, load_label_set, node, SCHEMA_ENTITY_RESERVE, SOURCE_ID,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const LABELS: &str = "labels/test.onyx.entities.btc.v2.json";

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

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> Res<()> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("label-hash") => {
            let set = load_label_set(&root.join("fixtures"), LABELS)?;
            println!("{}", hex0x(&set.content_hash().0));
            Ok(())
        }
        Some("export") if args.len() == 7 => export(&root, &args[1..]),
        _ => Err("usage: export-reserves label-hash | export <week dir> <tail dir> <week index.json> <scan.json> <base.json> <out.json>".into()),
    }
}

/// Header chain of `dir` over `from..=to`, linked to `parent` (internal order);
/// returns each block's (height, display hash, inputs + outputs).
fn tail_blocks(dir: &Path, from: u64, to: u64, parent: [u8; 32]) -> Res<Vec<(u64, String, u64)>> {
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    let mut prev = parent;
    let mut out = Vec::new();
    for h in from..=to {
        let raw = std::fs::read_to_string(dir.join(format!("{h}.raw.hex")))?;
        let header = hex::decode(raw.get(..160).ok_or("short block file")?)?;
        let (hash, up) = node::check_raw_header(&header, node::MAINNET_MIN_BITS)?;
        if up != prev {
            return Err(format!("block {h} does not extend block {}", h - 1).into());
        }
        prev = hash;
        let (src, _) = node::load_node_range(dir, h, h, &pin)?;
        let parsed = BtcSource::try_from_slice(&src)?;
        let io: usize = parsed
            .blocks
            .iter()
            .flat_map(|b| b.txs.iter())
            .map(|t| t.spent.len() + t.created.len())
            .sum();
        let mut display = hash;
        display.reverse();
        out.push((h, hex::encode(display), io as u64));
    }
    Ok(out)
}

fn base_of(v: &Value) -> Res<Vec<EntityBase>> {
    v.as_array()
        .ok_or("base")?
        .iter()
        .map(|b| {
            Ok(EntityBase {
                entity: b["entity"].as_str().ok_or("entity")?.to_string(),
                balance_sats: b["balance_sats"].as_str().ok_or("balance")?.parse()?,
            })
        })
        .collect()
}

fn row_json(r: &EntityReserveRow) -> Value {
    json!({"entity": r.entity, "base_height": r.base_height, "height": r.height,
        "base_sats": r.base_sats.to_string(), "received_sats": r.received_sats.to_string(),
        "sent_sats": r.sent_sats.to_string(), "tx_count": r.tx_count,
        "balance_sats": r.balance_sats.to_string()})
}

fn export(root: &Path, a: &[String]) -> Res<()> {
    let (week_dir, tail_dir) = (root.join(&a[0]), root.join(&a[1]));
    let fixtures = root.join("fixtures");
    let index: Value = serde_json::from_slice(&std::fs::read(root.join(&a[2]))?)?;
    let scan: ScanSnapshot = serde_json::from_slice(&std::fs::read(root.join(&a[3]))?)?;
    let base_doc: Value = serde_json::from_slice(&std::fs::read(root.join(&a[4]))?)?;
    let out = root.join(&a[5]);
    let set = load_label_set(&fixtures, LABELS)?;
    scan.check_labels(&set)?;
    let snapshot = scan.hash();
    let totals = scan.entity_totals(&set)?;
    let base_height = base_doc["base_height"].as_u64().ok_or("base_height")?;
    let base = base_of(&base_doc["base"])?;
    let (wfrom, wto) = (
        index["from"].as_u64().ok_or("from")?,
        index["to"].as_u64().ok_or("to")?,
    );
    if base_height + 1 != wfrom || base_doc["snapshot_height"].as_u64() != Some(scan.height) {
        return Err("base is not for the block before the week, or not from this scan".into());
    }
    if base
        .iter()
        .map(|b| &b.entity)
        .ne(totals.iter().map(|t| &t.entity))
    {
        return Err("base does not name the label set's entities".into());
    }

    // Week shards from the history; tail shards planned like the history's.
    let mut tip = [0u8; 32];
    hex::decode_to_slice(index["tip"].as_str().ok_or("tip")?, &mut tip)?;
    tip.reverse();
    let tail = tail_blocks(&tail_dir, wto + 1, scan.height, tip)?;
    let last = tail.last().ok_or("no tail blocks")?;
    scan.check_block(&last.1)?;
    let mut plan: Vec<(u64, u64, bool)> = index["shards"]
        .as_array()
        .ok_or("shards")?
        .iter()
        .map(|s| {
            Ok((
                s["from"].as_u64().ok_or("f")?,
                s["to"].as_u64().ok_or("t")?,
                true,
            ))
        })
        .collect::<Res<_>>()?;
    let io: Vec<(u64, u64)> = tail.iter().map(|b| (b.0, b.2)).collect();
    plan.extend(
        plan_shards(&io, PLAN_FUEL_TARGET)
            .into_iter()
            .map(|(f, t)| (f, t, false)),
    );

    // History balance rows per week shard (R-RES-2).
    let mut history: BTreeMap<(u64, u64), Vec<EntityFlowRow>> = BTreeMap::new();
    for d in index["days"].as_array().ok_or("days")? {
        let file = root
            .join(&a[2])
            .with_file_name(d["file"].as_str().ok_or("file")?);
        let day: Value = serde_json::from_slice(&std::fs::read(file)?)?;
        for s in day["shards"].as_array().ok_or("day shards")? {
            let rows = s["balance_leaves"]
                .as_array()
                .ok_or("leaves")?
                .iter()
                .map(|l| {
                    let row = OutputRow::try_from_slice(&hex::decode(l.as_str().ok_or("leaf")?)?)?;
                    Ok(EntityFlowRow::try_from_slice(&row.value)?)
                })
                .collect::<Res<Vec<_>>>()?;
            history.insert(
                (s["from"].as_u64().ok_or("f")?, s["to"].as_u64().ok_or("t")?),
                rows,
            );
        }
    }

    let wasm = std::fs::read(root.join("recipes/out/btc_entity_reserve.wasm"))?;
    let id = keccak256(&wasm);
    let reg = fixture_registry_reserve();
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm)?;
    cas.put_label_set(&set)?;
    let host = Host::new()?;
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };
    let labels_path = fixtures.join(LABELS);
    let scratch =
        std::env::temp_dir().join(format!("onyx-reserve-base-{}.json", std::process::id()));

    let (mut height, mut state, mut link) = (base_height, base.clone(), snapshot);
    let mut shards: Vec<ReserveShard> = Vec::new();
    let mut all_params: Vec<ReserveParams> = Vec::new();
    let mut shards_json = Vec::new();
    let mut max_fuel = 0u64;
    for (from, to, in_week) in plan {
        let dir = if in_week { &week_dir } else { &tail_dir };
        let params = ReserveParams {
            base_height: height,
            base_ref: link.0,
            base: state.clone(),
        };
        let (source, snap) = node::load_node_range(dir, from, to, &pin)?;
        let spec = ShardSpec {
            recipe: Some(id),
            runtime: Some(profile()),
            label_set: Some(LabelSetRef {
                name: set.name.clone(),
                version: set.version,
                content_hash: set.content_hash(),
            }),
            source: SourceRef {
                source: SourceId(SOURCE_ID.into()),
                snapshot: snap,
            },
            schema: keccak256(SCHEMA_ENTITY_RESERVE),
            rights_hash: reg
                .source_rights(&SourceId(SOURCE_ID.into()))
                .ok_or("source not registered")?
                .hash(),
            params: borsh::to_vec(&params)?,
            registry_root: reg.root(),
        };
        spec.validate(&reg)?;
        all_params.push(params);
        let fuel = preflight(
            &spec,
            source.clone(),
            &reg,
            &cas,
            &host,
            &AttestCache::new(),
            &Default::default(),
        )
        .map_err(|e| format!("{from}-{to}: preflight refused the shard: {e:?}"))?;
        let input = assemble_input(&spec, source, Some(&set), &[], &reg)?.bytes;
        let r = host.run(&id, &wasm, &input, &profile())?;
        if r.outcome != Outcome::Ok || r.fuel_used != fuel {
            return Err(format!(
                "{from}-{to}: run {:?}, fuel {} vs {fuel}",
                r.outcome, r.fuel_used
            )
            .into());
        }
        let rows = r
            .leaves
            .iter()
            .map(|l| {
                let row = OutputRow::try_from_slice(l)?;
                Ok(EntityReserveRow::try_from_slice(&row.value)?)
            })
            .collect::<Res<Vec<_>>>()?;
        // Independent Python recount of the shard.
        let base_json = json!({"base_height": height, "base": state.iter().map(|b| json!({"entity": b.entity, "balance_sats": b.balance_sats.to_string()})).collect::<Vec<_>>()});
        std::fs::write(&scratch, base_json.to_string())?;
        let recount = std::process::Command::new("python3")
            .arg("-I")
            .arg(root.join("scripts/recount-entity-reserve.py"))
            .args(["shard"])
            .arg(dir)
            .arg(&labels_path)
            .arg(&scratch)
            .arg(from.to_string())
            .arg(to.to_string())
            .output()?;
        if !recount.status.success() {
            return Err(format!(
                "{from}-{to}: recount failed: {}",
                String::from_utf8_lossy(&recount.stderr)
            )
            .into());
        }
        let recounted: Value = serde_json::from_slice(&recount.stdout)?;
        if recounted != Value::Array(rows.iter().map(row_json).collect()) {
            return Err(format!("{from}-{to}: the Python recount disagrees").into());
        }
        // R-RES-2: the week's flows are the history's balance rows.
        if in_week {
            let h = history
                .get(&(from, to))
                .ok_or("week shard not in the history")?;
            for r in &rows {
                let got = h.iter().find(|x| x.entity == r.entity);
                let want = got.map_or((0, 0, 0), |x| (x.received_sats, x.sent_sats, x.tx_count));
                if want != (r.received_sats, r.sent_sats, r.tx_count) {
                    return Err(
                        format!("{from}-{to} {}: flows differ from the history", r.entity).into(),
                    );
                }
            }
        }
        let root_hash: Hash32 = MerkleTree::from_rows(&r.leaves).root();
        eprintln!(
            "shard {from}-{to}{}: fuel {fuel}",
            if in_week { "" } else { " (tail)" }
        );
        max_fuel = max_fuel.max(fuel);
        shards_json.push(
            json!({"from": from, "to": to, "week": in_week, "root": hex0x(&root_hash.0),
            "fuel": fuel, "leaves": r.leaves.iter().map(|l| hex0x(l)).collect::<Vec<_>>(),
            "rows": rows.iter().map(row_json).collect::<Vec<_>>()}),
        );
        state = rows
            .iter()
            .map(|r| EntityBase {
                entity: r.entity.clone(),
                balance_sats: r.balance_sats,
            })
            .collect();
        height = to;
        link = root_hash;
        shards.push(ReserveShard {
            from,
            to,
            root: root_hash,
            rows,
        });
    }
    let _ = std::fs::remove_file(&scratch);
    // R-RES-PARAMS-1 and R-RES-SCAN-1: every spec's params are the committed
    // start or the previous shard's end, and the chain lands on the scan, exactly.
    let end = check_params_chain(base_height, &base, &snapshot, &all_params, &shards)?;
    if end != totals {
        return Err(format!("the chain does not land on the scan: {end:?} vs {totals:?}").into());
    }
    let mut scan_block = [0u8; 32];
    hex::decode_to_slice(&scan.block_hash, &mut scan_block)?;
    scan_block.reverse();
    let commitment = reserve_commitment(
        &id,
        &set.content_hash(),
        &snapshot,
        scan.height,
        &scan_block,
        base_height,
        &base,
        &totals,
        &shards,
    );
    let amounts = |v: &[EntityBase]| -> Vec<Value> {
        v.iter()
            .map(|b| json!({"entity": b.entity, "balance_sats": b.balance_sats.to_string()}))
            .collect()
    };
    let doc = json!({
        "version": 1,
        "generated_by": "cargo run --release -p onyx-source-btc --example export-reserves -- export <week dir> <tail dir> <week index.json> <scan.json> <base.json> <out.json>",
        "recipe_id": hex0x(&id.0),
        "label_set": {"name": set.name, "version": set.version, "content_hash": hex0x(&set.content_hash().0)},
        "snapshot": {"hash": hex0x(&snapshot.0), "height": scan.height, "block_hash": scan.block_hash,
            "method": scan.method, "addresses_scanned": scan.addresses_scanned,
            "addresses_holding": scan.rows.len(), "totals": amounts(&totals)},
        "base": {"height": base_height, "derivation": base_doc["derivation"], "rows": amounts(&base)},
        "week": {"from": wfrom, "to": wto, "tip": index["tip"]},
        "shards": shards_json,
        "commitment": hex0x(&commitment.0),
        "python_recount": "scripts/recount-entity-reserve.py shard: every shard matches",
    });
    std::fs::write(&out, serde_json::to_string(&doc)? + "\n")?;
    eprintln!(
        "wrote {} ({} shards, max fuel {max_fuel})",
        out.display(),
        shards.len()
    );
    println!("RESERVES_COMMITMENT={}", hex0x(&commitment.0));
    println!("RESERVES_RECIPE={}", hex0x(&id.0));
    println!("RESERVES_SNAPSHOT={}", hex0x(&snapshot.0));
    Ok(())
}
