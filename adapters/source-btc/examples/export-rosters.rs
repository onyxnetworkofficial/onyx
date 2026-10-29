// SPDX-License-Identifier: Apache-2.0
//! Per-entity daily inflow and outflow, and balances where a committed scan
//! holds the addresses, for label set v3 over the live fixture blocks
//! (ROADMAP P2a, ADR-0037 Amendment 1).
//!
//! ```text
//! cargo run --release -p onyx-source-btc --example export-rosters -- \
//!   <week dir> <tail dir> apps/web/static/history/rosters-week.json
//! ```
//!
//! * Shards follow ADR-0037 item 3: every membership change of v3 and every
//!   UTC day starts a shard, so each shard runs one window of the dated set
//!   (`DatedLabelSet::window`) as a plain label set; each window is a reviewed
//!   v3 window, level 4 (`dated_window_proof_level`).
//! * Flows: the `btc-entity-balance` recipe on the canonical engine (preflight,
//!   fuel equal), every shard recounted by `scripts/recount-entity-flow.py`.
//! * Balances: only for an entity all of whose addresses in the range the
//!   committed scan (`fixtures/reserves/scan-971199.json`, the v2 addresses)
//!   holds: the scan at 971,199 rolled back by each address's own flows, summed
//!   over the addresses valid at each day's last block. Other entities show no
//!   balance: their addresses need a UTXO scan (THREATS ROS-6, TODO D23).
//! * Cross-check: where the v3 and v2 address sets of an entity are the same,
//!   the balance equals the committed v2 reserve chain's at the same height.

use borsh::BorshDeserialize;
use onyx_btc_schema::{BtcSource, EntityFlowRow};
use onyx_coordinator::cas::{Cas, CasExt, Kind, MemCas};
use onyx_coordinator::recompute::preflight;
use onyx_core::{
    keccak256, Hash32, LabelSetRef, ProofLevel, Registry, RuntimeProfile, ShardSpec, SourceId,
    SourceRef, StaticRegistry,
};
use onyx_labels::attest::AttestCache;
use onyx_labels::LabelSet;
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::history::{plan_days, plan_shards, PLAN_FUEL_TARGET};
use onyx_source_btc::rosters::LABELS_V3;
use onyx_source_btc::{
    dated_snapshots_from_json, dated_window_proof_level, fixture_registry_v3, load_dated_label_set,
    load_label_set, node, valid_failure_codes, SOURCE_ID,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const BALANCE_SCHEMA: &[u8] = b"onyx.btc.entity-flow-row.v1";
const V2: &str = "labels/test.onyx.entities.btc.v2.json";
const SCAN: &str = "reserves/scan-971199.json";
const V2_RESERVES: &str = "apps/web/static/history/reserves-week.json";
/// First block of `fixtures-live/tail` (the week holds 969,839-970,846).
const TAIL_FROM: u64 = 970_847;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

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
    reg: &StaticRegistry,
    snapshot: onyx_core::Snapshot,
    set: &LabelSet,
) -> Res<ShardSpec> {
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
        schema: keccak256(BALANCE_SCHEMA),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    })
}

fn script_of(address: &str) -> Res<Vec<u8>> {
    let a = address.strip_prefix("btc:").ok_or("address prefix")?;
    Ok(bitcoin::Address::from_str(a)?
        .require_network(bitcoin::Network::Bitcoin)?
        .script_pubkey()
        .to_bytes())
}

/// `recount-entity-flow.py` prints `full {'entity:x': (received, sent, net)}`.
fn recount(
    root: &Path,
    dir: &Path,
    window: &LabelSet,
    from: u64,
    to: u64,
    scratch: &Path,
) -> Res<BTreeMap<String, (u128, u128)>> {
    let labels: Vec<Value> = window
        .labels
        .iter()
        .map(|l| json!({"address": l.address, "entity": l.entity}))
        .collect();
    std::fs::write(scratch, json!({"labels": labels}).to_string())?;
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("scripts/recount-entity-flow.py"))
        .arg(dir)
        .arg(scratch)
        .arg(from.to_string())
        .arg(to.to_string())
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "recount {from}-{to}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    let text = String::from_utf8(out.stdout)?;
    let full = text
        .lines()
        .find_map(|l| l.strip_prefix("full "))
        .ok_or("recount printed no full line")?;
    let v: Value =
        serde_json::from_str(&full.replace('\'', "\"").replace('(', "[").replace(')', "]"))?;
    let mut m = BTreeMap::new();
    for (e, t) in v.as_object().ok_or("recount object")? {
        let n =
            |i: usize| -> Res<u128> { Ok(t[i].as_u64().map(u128::from).ok_or("recount number")?) };
        m.insert(e.clone(), (n(0)?, n(1)?));
    }
    Ok(m)
}

fn main() -> Res<()> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [week, tail, out] = args.as_slice() else {
        return Err("usage: export-rosters <week dir> <tail dir> <out.json>".into());
    };
    let (week, tail) = (root.join(week), root.join(tail));
    let fixtures = root.join("fixtures");
    let v3 = load_dated_label_set(&fixtures, LABELS_V3)?;
    let records = dated_snapshots_from_json(&std::fs::read_to_string(fixtures.join(LABELS_V3))?)?;
    let reg = fixture_registry_v3();
    v3.validate(&reg)?;
    let scan: Value = serde_json::from_slice(&std::fs::read(fixtures.join(SCAN))?)?;
    let scan_height = scan["height"].as_u64().ok_or("scan height")?;
    let v2 = load_label_set(&fixtures, V2)?;
    if scan["label_set_hash"].as_str() != Some(hex0x(&v2.content_hash().0).as_str()) {
        return Err("the committed scan is not over label set v2".into());
    }
    let scanned: BTreeSet<&str> = v2.labels.iter().map(|l| l.address.as_str()).collect();
    let mut scan_bal: BTreeMap<String, u128> = BTreeMap::new();
    for r in scan["rows"].as_array().ok_or("scan rows")? {
        scan_bal.insert(
            r["address"].as_str().ok_or("address")?.to_string(),
            r["sats"].as_str().ok_or("sats")?.parse()?,
        );
    }

    // Blocks: week then tail, one header chain, proof of work checked.
    let dir_of = |h: u64| -> &Path {
        if h < TAIL_FROM {
            &week
        } else {
            &tail
        }
    };
    let (from, to) = (969_839u64, scan_height);
    let pin = node::NodePin {
        min_bits: node::MAINNET_MIN_BITS,
        tip: None,
    };

    // Entities with memberships in the range, and whether the scan holds them.
    let in_range: Vec<&onyx_labels::dated::Membership> = v3
        .members
        .iter()
        .filter(|m| m.from <= to && m.to.is_none_or(|t| t >= from))
        .collect();
    let mut addrs_of: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for m in &in_range {
        addrs_of
            .entry(m.label.entity.as_str())
            .or_default()
            .insert(m.label.address.as_str());
    }
    let covered: BTreeSet<&str> = addrs_of
        .iter()
        .filter(|(_, a)| a.iter().all(|x| scanned.contains(x)))
        .map(|(e, _)| *e)
        .collect();
    let mut watch: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    for e in &covered {
        for a in &addrs_of[e] {
            watch.insert(script_of(a)?, (*a).to_string());
        }
    }

    let mut blocks = Vec::new();
    let mut flows: BTreeMap<u64, BTreeMap<String, (u128, u128)>> = BTreeMap::new();
    let mut prev: Option<[u8; 32]> = None;
    for h in from..=to {
        let dir = dir_of(h);
        let raw = std::fs::read_to_string(dir.join(format!("{h}.raw.hex")))?;
        let header = hex::decode(raw.get(..160).ok_or("short block file")?)?;
        let (hash, parent) = node::check_raw_header(&header, node::MAINNET_MIN_BITS)?;
        if prev.is_some_and(|p| p != parent) {
            return Err(format!("block {h} does not extend block {}", h - 1).into());
        }
        prev = Some(hash);
        let time = u64::from(u32::from_le_bytes(header[68..72].try_into()?));
        let (src, _) = node::load_node_range(dir, h, h, &pin)?;
        let parsed = BtcSource::try_from_slice(&src)?;
        let mut io = 0u64;
        for t in parsed.blocks.iter().flat_map(|b| b.txs.iter()) {
            io += (t.spent.len() + t.created.len()) as u64;
            for o in &t.created {
                if let Some(a) = watch.get(&o.script) {
                    flows.entry(h).or_default().entry(a.clone()).or_default().0 +=
                        u128::from(o.value);
                }
            }
            for o in &t.spent {
                if let Some(a) = watch.get(&o.script) {
                    flows.entry(h).or_default().entry(a.clone()).or_default().1 +=
                        u128::from(o.value);
                }
            }
        }
        blocks.push((h, io, time));
        if h % 100 == 0 {
            eprintln!("read block {h}");
        }
    }
    let mut last_hash = prev.ok_or("no blocks")?;
    last_hash.reverse();
    if scan["block_hash"].as_str() != Some(hex::encode(last_hash).as_str()) {
        return Err("the last block is not the scan's block".into());
    }

    // Plan: UTC days, split again at every membership change inside a day.
    let changes: Vec<u64> = v3
        .boundaries()
        .into_iter()
        .filter(|b| *b > from && *b <= to)
        .collect();
    let days = plan_days(&blocks, PLAN_FUEL_TARGET);
    let io_of: BTreeMap<u64, u64> = blocks.iter().map(|b| (b.0, b.1)).collect();
    let mut plan: Vec<(String, u64, u64)> = Vec::new();
    for (date, dfrom, dto, _) in &days {
        let mut cuts: Vec<u64> = vec![*dfrom];
        // Also cut where the week's directory ends and the tail's begins.
        cuts.extend(
            changes
                .iter()
                .copied()
                .chain(std::iter::once(TAIL_FROM))
                .filter(|c| c > dfrom && c <= dto),
        );
        cuts.sort_unstable();
        cuts.dedup();
        for (i, s) in cuts.iter().enumerate() {
            let e = cuts.get(i + 1).map_or(*dto, |n| n - 1);
            let seg: Vec<(u64, u64)> = (*s..=e).map(|h| (h, io_of[&h])).collect();
            for (f, t) in plan_shards(&seg, PLAN_FUEL_TARGET) {
                plan.push((date.clone(), f, t));
            }
        }
    }

    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let id = keccak256(&wasm);
    let cas = MemCas::new();
    cas.put(Kind::Recipe, &wasm)?;
    let host = Host::new()?;
    let allowed = valid_failure_codes().into_iter().collect();
    let scratch =
        std::env::temp_dir().join(format!("onyx-rosters-window-{}.json", std::process::id()));
    let mut shards_json = Vec::new();
    let mut day_rows: BTreeMap<String, BTreeMap<String, (u128, u128, u64)>> = BTreeMap::new();
    let mut day_span: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for (date, f, t) in &plan {
        let span = day_span.entry(date.clone()).or_insert((*f, *t));
        span.0 = span.0.min(*f);
        span.1 = span.1.max(*t);
        let window = v3.window(*f, *t)?;
        if window.labels.is_empty() {
            shards_json.push(json!({"from": f, "to": t, "date": date, "labels": 0}));
            continue;
        }
        let wref = LabelSetRef {
            name: window.name.clone(),
            version: window.version,
            content_hash: window.content_hash(),
        };
        if dated_window_proof_level(&fixtures, &wref, *f, *t) != ProofLevel::BondedClaim {
            return Err(format!("{f}-{t}: not a reviewed v3 window").into());
        }
        cas.put_label_set(&window)?;
        let dir = dir_of(*f);
        if dir_of(*t) != dir {
            return Err(format!("{f}-{t} crosses the week/tail split").into());
        }
        let (source, snapshot) = node::load_node_range(dir, *f, *t, &pin)?;
        let s = spec(id, &reg, snapshot, &window)?;
        let fuel = preflight(
            &s,
            source.clone(),
            &reg,
            &cas,
            &host,
            &AttestCache::new(),
            &allowed,
        )
        .map_err(|e| format!("{f}-{t}: preflight refused the shard: {e:?}"))?;
        let input = assemble_input(&s, source, Some(&window), &[], &reg)?.bytes;
        let r = host.run(&id, &wasm, &input, &profile())?;
        if r.outcome != Outcome::Ok || r.fuel_used != fuel {
            return Err(format!(
                "{f}-{t}: run {:?}, fuel {} vs {fuel}",
                r.outcome, r.fuel_used
            )
            .into());
        }
        let rows: Vec<EntityFlowRow> = r
            .leaves
            .iter()
            .map(|l| -> Res<EntityFlowRow> {
                let row = OutputRow::try_from_slice(l)?;
                Ok(EntityFlowRow::try_from_slice(&row.value)?)
            })
            .collect::<Res<_>>()?;
        let py = recount(&root, dir, &window, *f, *t, &scratch)?;
        let mine: BTreeMap<String, (u128, u128)> = rows
            .iter()
            .filter(|r| r.received_sats > 0 || r.sent_sats > 0)
            .map(|r| (r.entity.clone(), (r.received_sats, r.sent_sats)))
            .collect();
        if mine != py {
            return Err(
                format!("{f}-{t}: the Python recount disagrees: {mine:?} vs {py:?}").into(),
            );
        }
        for r in &rows {
            let d = day_rows
                .entry(date.clone())
                .or_default()
                .entry(r.entity.clone())
                .or_default();
            d.0 += r.received_sats;
            d.1 += r.sent_sats;
            d.2 += r.tx_count;
        }
        let entities: BTreeSet<&str> = window.labels.iter().map(|l| l.entity.as_str()).collect();
        eprintln!(
            "shard {f}-{t} ({date}): {} labels, fuel {fuel}",
            window.labels.len()
        );
        shards_json.push(
            json!({"from": f, "to": t, "date": date, "labels": window.labels.len(),
            "entities": entities, "window_hash": hex0x(&wref.content_hash.0),
            "root": hex0x(&MerkleTree::from_rows(&r.leaves).root().0), "fuel": fuel}),
        );
    }
    let _ = std::fs::remove_file(&scratch);

    // Balances of the covered entities at each day's last block: the scan,
    // rolled back by every later block's flows of that address.
    let balance_at = |entity: &str, h: u64| -> Res<u128> {
        let mut total: i128 = 0;
        for m in v3.members_at(h).filter(|m| m.label.entity == entity) {
            let a = &m.label.address;
            let mut b = i128::try_from(*scan_bal.get(a).unwrap_or(&0))?;
            for (_, f) in flows
                .range(h.saturating_add(1)..)
                .take_while(|(k, _)| **k <= to)
            {
                if let Some((rcv, snt)) = f.get(a) {
                    b -= i128::try_from(*rcv)?;
                    b += i128::try_from(*snt)?;
                }
            }
            if b < 0 {
                return Err(format!("{a} below zero at {h}").into());
            }
            total += b;
        }
        Ok(u128::try_from(total)?)
    };

    // Cross-check against the v2 reserve chain where the address sets agree.
    let v2_doc: Value = serde_json::from_slice(&std::fs::read(root.join(V2_RESERVES))?)?;
    let mut v2_at: BTreeMap<(String, u64), u128> = BTreeMap::new();
    for s in v2_doc["shards"].as_array().ok_or("v2 shards")? {
        for r in s["rows"].as_array().ok_or("v2 rows")? {
            v2_at.insert(
                (
                    r["entity"].as_str().ok_or("e")?.to_string(),
                    r["height"].as_u64().ok_or("h")?,
                ),
                r["balance_sats"].as_str().ok_or("b")?.parse()?,
            );
        }
    }
    let v2_addrs = |e: &str| -> BTreeSet<&str> {
        v2.labels
            .iter()
            .filter(|l| l.entity == e)
            .map(|l| l.address.as_str())
            .collect()
    };
    let mut checks = Vec::new();
    let mut days_json = Vec::new();
    for (date, (dfrom, dto)) in &day_span {
        let mut rows = Vec::new();
        let flows_of = day_rows.get(date).cloned().unwrap_or_default();
        for e in addrs_of.keys() {
            let active = v3.members_at(*dto).any(|m| m.label.entity == *e)
                || v3.members_at(*dfrom).any(|m| m.label.entity == *e);
            let fl = flows_of.get(*e);
            if !active && fl.is_none() {
                continue;
            }
            // Only while the entity has addresses at the day's last block (a
            // balance of an entity whose memberships ended would read as zero).
            let listed_at_end = v3.members_at(*dto).any(|m| m.label.entity == *e);
            let balance = if covered.contains(e) && listed_at_end {
                Some(balance_at(e, *dto)?)
            } else {
                None
            };
            if let Some(b) = balance {
                let now: BTreeSet<&str> = v3
                    .members_at(*dto)
                    .filter(|m| m.label.entity == *e)
                    .map(|m| m.label.address.as_str())
                    .collect();
                if now == v2_addrs(e) {
                    if let Some(v2b) = v2_at.get(&((*e).to_string(), *dto)) {
                        if *v2b != b {
                            return Err(format!(
                                "{e} at {dto}: v3 balance {b} vs v2 reserve chain {v2b}"
                            )
                            .into());
                        }
                        checks.push(json!({"entity": e, "height": dto, "balance_sats": b.to_string(), "v2_reserve_chain": "equal"}));
                    }
                }
            }
            let (rcv, snt, txs) = fl.copied().unwrap_or_default();
            rows.push(json!({"entity": e, "received_sats": rcv.to_string(), "sent_sats": snt.to_string(),
                "tx_count": txs, "addresses_at_end": v3.members_at(*dto).filter(|m| m.label.entity == *e).count(),
                "balance_sats": balance.map(|b| b.to_string())}));
        }
        days_json.push(json!({"date": date, "from": dfrom, "to": dto, "rows": rows}));
    }

    // Every entity of the set: its sources and grades, from the records.
    let mut entities_json = Vec::new();
    for (e, kind) in onyx_source_btc::LABELS_V3_ENTITIES {
        let mine: Vec<&onyx_labels::dated::Membership> =
            v3.members.iter().filter(|m| m.label.entity == *e).collect();
        let recs: Vec<&onyx_labels::roster::SnapshotRecord> =
            records.iter().filter(|r| r.entity == *e).collect();
        let addresses: BTreeSet<&str> = mine.iter().map(|m| m.label.address.as_str()).collect();
        let signed = mine
            .iter()
            .filter(|m| m.label.confidence.0 == onyx_labels::roster::CONFIDENCE_SIGNED)
            .count();
        let first = recs.first().ok_or("entity without snapshots")?;
        let last = recs.last().ok_or("entity without snapshots")?;
        let bases: BTreeSet<&str> = recs.iter().map(|r| r.height_basis.as_str()).collect();
        entities_json.push(json!({"entity": e, "kind": format!("{kind:?}"), "source": first.source,
            "source_url": last.url, "snapshots": recs.len(), "first_issued": first.issued, "last_issued": last.issued,
            "last_height": last.height, "fetched": last.fetched, "height_basis": bases,
            "grade": if signed == mine.len() { "signed" } else if signed > 0 { "signed and published" } else { "published" },
            "memberships": mine.len(), "signed_memberships": signed, "addresses": addresses.len(),
            "open_at_publication": mine.iter().filter(|m| m.to.is_none()).count(),
            "in_fixture_range": addrs_of.get(e).map_or(0, |a| a.len()),
            "balance": if covered.contains(e) { "rolled back from the committed scan" }
                else if addrs_of.contains_key(e) { "not computed: the committed scan does not hold these addresses (a v3 scan is TODO D23)" }
                else { "no membership in the fixture blocks (listings older than 8,928 blocks)" }}));
    }
    let doc = json!({
        "version": 1,
        "generated_by": "cargo run --release -p onyx-source-btc --example export-rosters -- <week dir> <tail dir> <out.json>",
        "label_set": {"name": v3.name, "version": v3.version, "published": v3.published,
            "content_hash": hex0x(&v3.content_hash().0), "members_root": hex0x(&v3.members_root().0)},
        "proof_level": 4,
        "balance_recipe_id": hex0x(&id.0),
        "range": {"from": from, "to": to, "scan_height": scan_height},
        "python_recount": "scripts/recount-entity-flow.py: every shard matches",
        "entities": entities_json,
        "days": days_json,
        "v2_cross_checks": checks,
        "shards": shards_json,
    });
    std::fs::write(root.join(out), serde_json::to_string(&doc)? + "\n")?;
    eprintln!(
        "wrote {out}: {} shards, {} days, {} cross-checks",
        plan.len(),
        day_span.len(),
        doc["v2_cross_checks"].as_array().map_or(0, Vec::len)
    );
    Ok(())
}
