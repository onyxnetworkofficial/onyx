//! Export the data for the demo website (apps/demo). Everything here is
//! computed by the real Onyx code: the recipe input bytes, the canonical
//! engine's output, Merkle roots and proofs, the commitment, a BIP-322 +
//! EIP-191 attestation and the BIP-322 policy vectors, and the spread.
//! The page then re-runs the same recipe bytes in the browser and checks that
//! it gets the same root.
//!
//! Run: `cargo run --release -p onyx-source-btc --example export-demo`
//! Output: `apps/demo/site/data/` (generated; not committed).
//! Keys are test keys derived from fixed labels; they hold nothing.

// Timings are reported in seconds; not protocol code.
#![allow(clippy::float_arithmetic)]

use bitcoin::key::{CompressedPublicKey, PrivateKey};
use bitcoin::secp256k1::{Message, Secp256k1, SecretKey};
use bitcoin::{Address, Network};
use borsh::BorshDeserialize;
use onyx_btc_schema::{BtcSource, EntityBlockFlowRow, EntityFlowRow, SERIES_NO_ACTIVITY};
use onyx_core::{
    commit_hash, keccak256, AddressProof, Commitment, EntityKind, LabelSetRef, Registry,
    RuntimeProfile, ShardSpec, SourceAdapter, SourceId, SourceRef, StaticRegistry,
};
use onyx_labels::attest::{eip191_digest, AttestContext, Attestation, Statement};
use onyx_labels::{LabelSet, ATTESTATION_EVIDENCE};
use onyx_merkle::MerkleTree;
use onyx_recipe_host::{assemble_input, failure_leaf, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::OutputRow;
use onyx_source_btc::{
    fixture_registry_series, fixture_registry_v2, load_label_set, Bip322Proof, BtcAddressCanon,
    BtcFixtureAdapter, SCHEMA_ENTITY_FLOW_SERIES, SOURCE_ID,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

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

struct Run {
    spec: ShardSpec,
    input: Vec<u8>,
    leaves: Vec<Vec<u8>>,
    fuel: u64,
    secs: f64,
}

fn run_set(
    host: &Host,
    reg: &StaticRegistry,
    wasm: &[u8],
    source: &[u8],
    snapshot: &onyx_core::Snapshot,
    set: &LabelSet,
) -> Result<Run, Box<dyn std::error::Error>> {
    run_schema(
        host,
        reg,
        wasm,
        b"onyx.btc.entity-flow-row.v1",
        source,
        snapshot,
        set,
    )
}

fn run_schema(
    host: &Host,
    reg: &StaticRegistry,
    wasm: &[u8],
    schema: &[u8],
    source: &[u8],
    snapshot: &onyx_core::Snapshot,
    set: &LabelSet,
) -> Result<Run, Box<dyn std::error::Error>> {
    let id = keccak256(wasm);
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
            snapshot: snapshot.clone(),
        },
        schema: keccak256(schema),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .ok_or("source not registered")?
            .hash(),
        params: Vec::new(),
        registry_root: reg.root(),
    };
    let input = assemble_input(&spec, source.to_vec(), Some(set), &[], reg)?.bytes;
    let t = Instant::now();
    let r = host.run(&id, wasm, &input, &profile())?;
    Ok(Run {
        spec,
        input,
        leaves: r.leaves,
        fuel: r.fuel_used,
        secs: t.elapsed().as_secs_f64(),
    })
}

/// True when a series run ended with `SERIES_NO_ACTIVITY` (SER-R1-F1).
fn series_no_activity(leaves: &[Vec<u8>]) -> bool {
    leaves == [failure_leaf(Outcome::RecipeError(SERIES_NO_ACTIVITY))]
}

/// Rows of `btc-entity-flow-series`, decoded, with the leaf bytes the page checks.
/// A range with no activity (the `SERIES_NO_ACTIVITY` failure leaf) has no rows.
fn series_json(leaves: &[Vec<u8>]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    if series_no_activity(leaves) {
        return Ok(Vec::new());
    }
    leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l)?;
            let r = EntityBlockFlowRow::try_from_slice(&row.value)?;
            Ok(json!({
                "entity": r.entity,
                "height": r.height,
                "received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(),
                "net_sats": r.net_sats.to_string(),
                "tx_count": r.tx_count,
                "leaf": hex0x(l),
            }))
        })
        .collect()
}

fn rows_json(leaves: &[Vec<u8>]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l)?;
            let r = EntityFlowRow::try_from_slice(&row.value)?;
            Ok(json!({
                "entity": r.entity,
                "from_height": r.from_height,
                "to_height": r.to_height,
                "received_sats": r.received_sats.to_string(),
                "sent_sats": r.sent_sats.to_string(),
                "net_sats": r.net_sats.to_string(),
                "tx_count": r.tx_count,
                "leaf": hex0x(l),
            }))
        })
        .collect()
}

/// Label set the demo runs (fixtures/labels/PROVENANCE.md).
const LABELS: &str = "labels/test.onyx.entities.btc.v2.json";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let fixtures = root.join("fixtures");
    let out = root.join("apps/demo/site/data");
    std::fs::create_dir_all(&out)?;

    // 1. Real blocks and the test label set (v2: 9,850 company labels,
    // fixtures/labels/PROVENANCE.md), registered in the v2 fixture registry.
    let reg = fixture_registry_v2();
    let adapter = BtcFixtureAdapter::new(&fixtures, "btc/blocks-921080-921082.json");
    let snapshot = adapter.range_snapshot()?;
    let source = adapter.load(&snapshot)?;
    let parsed = BtcSource::try_from_slice(&source)?;
    let blocks: Vec<Value> = parsed
        .blocks
        .iter()
        .map(|b| {
            let outputs: usize = b.txs.iter().map(|t| t.created.len()).sum();
            let sats: u128 = b
                .txs
                .iter()
                .flat_map(|t| t.created.iter())
                .map(|o| u128::from(o.value))
                .sum();
            json!({"height": b.height, "hash": hex::encode(b.hash), "txs": b.txs.len(),
                   "outputs": outputs, "output_sats": sats.to_string()})
        })
        .collect();
    let full = load_label_set(&fixtures, LABELS)?;
    let mut okx_seen = 0usize;
    let half = LabelSet {
        name: "test.onyx.entities.btc.half".into(),
        version: 1,
        labels: full
            .labels
            .iter()
            .filter(|l| {
                if l.entity != "entity:okx" {
                    return true;
                }
                okx_seen += 1;
                okx_seen % 2 == 1
            })
            .cloned()
            .collect(),
    };

    // 2. The canonical engine runs the recipe for both label sets.
    let wasm = std::fs::read(root.join("recipes/out/btc_entity_balance.wasm"))?;
    let host = Host::new()?;
    let full_run = run_set(&host, &reg, &wasm, &source, &snapshot, &full)?;
    let half_run = run_set(&host, &reg, &wasm, &source, &snapshot, &half)?;
    std::fs::write(out.join("recipe.wasm"), &wasm)?;
    // Artifact hosting serves text, not raw binary: inputs ship as base64.
    let b64 = |b: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b);
    std::fs::write(out.join("input-full.b64.txt"), b64(&full_run.input))?;
    std::fs::write(out.join("input-half.b64.txt"), b64(&half_run.input))?;
    let _ = std::fs::remove_file(out.join("input-full.bin"));
    let _ = std::fs::remove_file(out.join("input-half.bin"));

    let tree = MerkleTree::from_rows(&full_run.leaves);
    let proofs: Vec<Value> = (0..tree.leaf_count())
        .filter_map(|i| tree.proof(i))
        .map(|p| {
            json!({"index": p.index, "leaf_count": p.leaf_count,
                   "siblings": p.siblings.iter().map(|s| hex0x(&s.0)).collect::<Vec<_>>()})
        })
        .collect();

    // 3. A miner's commitment (commit-reveal hash with a demo salt and test miner address).
    let spec = &full_run.spec;
    let c = Commitment {
        root: tree.root(),
        leaf_count: tree.leaf_count(),
        recipe: spec.recipe,
        runtime: spec.runtime.clone(),
        label_set: spec.label_set.clone(),
        source: spec.source.clone(),
        schema: spec.schema,
        rights_hash: spec.rights_hash,
    };
    let miner = {
        let mut m = [0u8; 20];
        m.copy_from_slice(&keccak256(b"onyx demo miner").0[12..]);
        m
    };
    let salt = keccak256(b"onyx demo salt");
    let round = 1u64;
    let commit = commit_hash(round, &spec.shard_id(), &c.binding_hash(), &miner, &salt);

    // 4. A first-party attestation with test keys (entity:example, never a real entity).
    let secp = Secp256k1::new();
    let btc_key = SecretKey::from_slice(&keccak256(b"onyx test btc key 1").0)?;
    let entity_key = SecretKey::from_slice(&keccak256(b"onyx test entity key 1").0)?;
    let entity_addr = {
        let pk = entity_key.public_key(&secp).serialize_uncompressed();
        let mut a = [0u8; 20];
        a.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
        a
    };
    let addr = Address::p2wpkh(
        &CompressedPublicKey(btc_key.public_key(&secp)),
        Network::Bitcoin,
    )
    .to_string();
    let mut areg = StaticRegistry::new();
    areg.set_domain("onyx.devnet")
        .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
        .and_then(|r| r.add_canon(Box::new(BtcAddressCanon)))
        .and_then(|r| r.add_proof(Box::new(Bip322Proof)))
        .and_then(|r| r.add_entity_key("entity:example", entity_addr))?;
    let statement = Statement {
        domain: "onyx.devnet".into(),
        entity: "entity:example".into(),
        address: format!("btc:{addr}"),
        valid_from: 921_000,
        expires: 930_000,
        nonce: [9; 16],
    }
    .render();
    let wif = PrivateKey::new(btc_key, Network::Bitcoin).to_wif();
    let address_sig = bip322::sign_simple_encoded(&addr, &statement, &[wif], None)?;
    let (recid, rs) = secp
        .sign_ecdsa_recoverable(
            &Message::from_digest(eip191_digest(statement.as_bytes()).0),
            &entity_key,
        )
        .serialize_compact();
    let mut entity_sig = [0u8; 65];
    entity_sig[..64].copy_from_slice(&rs);
    entity_sig[64] = 27 + u8::try_from(recid.to_i32())?;
    let att = Attestation {
        statement: statement.clone(),
        address_sig: address_sig.clone(),
        entity_key: entity_addr,
        entity_sig,
    };
    let ranges = [("btc", 921_080, 921_082)];
    let ctx = AttestContext { ranges: &ranges };
    let verdict = format!("{:?}", att.verify(&ctx, &areg).map(|_| "valid"));
    let mut forged = att.clone();
    forged.statement = forged.statement.replace("entity:example", "entity:other");
    let forged_verdict = format!("{:?}", forged.verify(&ctx, &areg).map(|_| "valid"));
    let vectors: Vec<Value> = Bip322Proof
        .vectors()
        .iter()
        .map(|v| {
            json!({"body": v.body, "expect": format!("{:?}", v.expect),
                   "got": format!("{:?}", Bip322Proof.verify(v.body, v.message.as_bytes(), v.signature)),
                   "variant": &v.signature[..3.min(v.signature.len())]})
        })
        .collect();

    // 5. Optional live range from a node: ONYX_LIVE=<dir>:<from>:<to>
    //    (blocks fetched by scripts/fetch-btc-node.sh). Recounted by Python too.
    let live = match std::env::var("ONYX_LIVE") {
        Ok(spec_text) => {
            let parts: Vec<&str> = spec_text.split(':').collect();
            let [dir, from, to] = parts[..] else {
                return Err("ONYX_LIVE must be <dir>:<from>:<to>".into());
            };
            let (from, to): (u64, u64) = (from.parse()?, to.parse()?);
            let pin = onyx_source_btc::node::NodePin {
                min_bits: onyx_source_btc::node::MAINNET_MIN_BITS,
                tip: None,
            };
            let (lsource, lsnap) =
                onyx_source_btc::node::load_node_range(&root.join(dir), from, to, &pin)?;
            let lparsed = BtcSource::try_from_slice(&lsource)?;
            let lblocks: Vec<Value> = lparsed
                .blocks
                .iter()
                .map(|b| {
                    // The 80-byte header, so the page can recheck proof of work itself.
                    let raw = std::fs::read_to_string(
                        root.join(dir).join(format!("{}.raw.hex", b.height)),
                    )
                    .unwrap_or_default();
                    // Header time: bytes 68..72, little-endian (the page reads it too).
                    let time = raw
                        .get(136..144)
                        .and_then(|h| hex::decode(h).ok())
                        .and_then(|b| <[u8; 4]>::try_from(b.as_slice()).ok())
                        .map(u32::from_le_bytes);
                    let outputs: usize = b.txs.iter().map(|t| t.created.len()).sum();
                    let sats: u128 = b
                        .txs
                        .iter()
                        .flat_map(|t| t.created.iter())
                        .map(|o| u128::from(o.value))
                        .sum();
                    json!({"height": b.height, "hash": hex::encode(b.hash), "txs": b.txs.len(),
                           "header": raw.get(..160).unwrap_or_default(), "time": time,
                           "outputs": outputs, "output_sats": sats.to_string()})
                })
                .collect();
            let lrun = run_set(&host, &reg, &wasm, &lsource, &lsnap, &full)?;
            // The same recipe on each single-block range: per-block rows the
            // page adds up and compares with the range rows under the root.
            let mut per_block = Vec::new();
            for h in from..=to {
                let (bsource, bsnap) =
                    onyx_source_btc::node::load_node_range(&root.join(dir), h, h, &pin)?;
                let brun = run_set(&host, &reg, &wasm, &bsource, &bsnap, &full)?;
                // Only the companies active in the block: an all-zero row adds
                // nothing to the sums the page checks, and every company x 24
                // blocks of zero rows would dominate the page's payload. `root`
                // and `leaf_count` still describe the whole single-block shard.
                let active: Vec<Value> = rows_json(&brun.leaves)?
                    .into_iter()
                    .filter(|r| {
                        r["received_sats"] != "0" || r["sent_sats"] != "0" || r["tx_count"] != 0
                    })
                    .collect();
                per_block.push(json!({"height": h,
                    "root": hex0x(&MerkleTree::from_rows(&brun.leaves).root().0),
                    "leaf_count": brun.leaves.len(), "rows": active}));
            }
            // The per-block series recipe on the same input, recounted in Python.
            let swasm = std::fs::read(root.join("recipes/out/btc_entity_flow_series.wasm"))?;
            let sreg = fixture_registry_series();
            let srun = run_schema(
                &host,
                &sreg,
                &swasm,
                SCHEMA_ENTITY_FLOW_SERIES,
                &lsource,
                &lsnap,
                &full,
            )?;
            let srows = series_json(&srun.leaves)?;
            let srecount = std::process::Command::new("python3")
                .arg("-I")
                .arg(root.join("scripts/recount-entity-flow-series.py"))
                .arg(root.join(dir))
                .arg(fixtures.join(LABELS))
                .arg(from.to_string())
                .arg(to.to_string())
                .output()?;
            let recounted: Value = serde_json::from_slice(&srecount.stdout)?;
            let mine: Vec<Value> = srows
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    if let Some(o) = r.as_object_mut() {
                        o.remove("leaf");
                    }
                    r
                })
                .collect();
            if recounted != Value::Array(mine) {
                return Err("series: the Python recount disagrees with the recipe".into());
            }
            let series = json!({
                "no_activity": series_no_activity(&srun.leaves),
                // The one leaf a quiet range commits, so the page can check its root.
                "no_activity_leaf": series_no_activity(&srun.leaves).then(|| hex0x(&srun.leaves[0])),
                "recipe_id": hex0x(&keccak256(&swasm).0),
                "root": hex0x(&MerkleTree::from_rows(&srun.leaves).root().0),
                "shard_id": hex0x(&srun.spec.shard_id().0),
                "leaf_count": srun.leaves.len(),
                "fuel": srun.fuel,
                "wasmi_secs": srun.secs,
                "python_recount": "scripts/recount-entity-flow-series.py: every row matches",
                "rows": srows,
            });
            let recount = std::process::Command::new("python3")
                .arg("-I")
                .arg(root.join("scripts/recount-entity-flow.py"))
                .arg(root.join(dir))
                .arg(fixtures.join(LABELS))
                .arg(from.to_string())
                .arg(to.to_string())
                .output()?;
            let recount_line = String::from_utf8_lossy(&recount.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            json!({"from_height": from, "to_height": to, "blocks": lblocks,
                   "content_hash": hex0x(&keccak256(&lsource).0), "source_bytes": lsource.len(),
                   "root": hex0x(&MerkleTree::from_rows(&lrun.leaves).root().0),
                   "shard_id": hex0x(&lrun.spec.shard_id().0), "fuel": lrun.fuel, "wasmi_secs": lrun.secs,
                   "rows": rows_json(&lrun.leaves)?, "per_block": per_block, "series": series, "python_recount": recount_line,
                   "node": "Bitcoin Core on the indexer host, read-only getblock 0 and 3",
                   "min_bits": format!("{:#010x}", onyx_source_btc::node::MAINNET_MIN_BITS)})
        }
        Err(_) => Value::Null,
    };

    // 6. Export.
    let net = |leaves: &[Vec<u8>], entity: &str| -> Option<String> {
        leaves.iter().find_map(|l| {
            let row = OutputRow::try_from_slice(l).ok()?;
            let r = EntityFlowRow::try_from_slice(&row.value).ok()?;
            (r.entity == entity).then(|| r.net_sats.to_string())
        })
    };
    let half_tree = MerkleTree::from_rows(&half_run.leaves);
    // The independent Python recount of the spread entity, full and half set.
    let recount = std::process::Command::new("python3")
        .arg("-I")
        .arg(root.join("scripts/recount-entity-flow.py"))
        .arg(fixtures.join("btc/blocks-921080-921082.json"))
        .arg(fixtures.join(LABELS))
        .output()?;
    let recount_net = |prefix: &str| -> Option<String> {
        let text = String::from_utf8_lossy(&recount.stdout);
        let line = text.lines().find(|l| l.starts_with(prefix))?;
        let at = line.find("'entity:okx': (")? + "'entity:okx': (".len();
        let fields: Vec<&str> = line[at..].split(')').next()?.split(',').collect();
        Some(fields.get(2)?.trim().to_string())
    };
    let recount_line = format!(
        "scripts/recount-entity-flow.py: {} and {}",
        recount_net("full ").ok_or("recount: no full line")?,
        recount_net("half ").ok_or("recount: no half line")?
    );
    let data = json!({
        "generated_by": "cargo run --release -p onyx-source-btc --example export-demo",
        "host_id": HOST_ID,
        "recipe_id": hex0x(&keccak256(&wasm).0),
        "registry_root": hex0x(&reg.root().0),
        "snapshot": {"chain": "btc.mainnet", "from_height": 921_080, "to_height": 921_082,
                     "content_hash": hex0x(&keccak256(&source).0), "source_bytes": source.len()},
        "blocks": blocks,
        "label_set": {"name": full.name, "version": full.version, "content_hash": hex0x(&full.content_hash().0),
                      "count": full.labels.len()},
        "runs": [
            {"id": "full", "label_set": full.name, "input": "data/input-full.b64.txt", "input_bytes": full_run.input.len(),
             "root": hex0x(&tree.root().0), "shard_id": hex0x(&full_run.spec.shard_id().0),
             "fuel": full_run.fuel, "wasmi_secs": full_run.secs, "rows": rows_json(&full_run.leaves)?,
             "proofs": proofs},
            {"id": "half", "label_set": half.name, "input": "data/input-half.b64.txt", "input_bytes": half_run.input.len(),
             "root": hex0x(&half_tree.root().0), "shard_id": hex0x(&half_run.spec.shard_id().0),
             "fuel": half_run.fuel, "wasmi_secs": half_run.secs, "rows": rows_json(&half_run.leaves)?,
             "labels_dropped": full.labels.len() - half.labels.len()}
        ],
        "commitment": {"round": round, "miner": hex0x(&miner), "salt": hex0x(&salt.0),
                       "binding_hash": hex0x(&c.binding_hash().0), "commit_hash": hex0x(&commit.0)},
        "spread": {"entity": "entity:okx", "values": [
            {"set": full.name, "net_sats": net(&full_run.leaves, "entity:okx")},
            {"set": half.name, "net_sats": net(&half_run.leaves, "entity:okx")}],
            "independent_recount": recount_line},
        "attestation": {"statement": statement, "btc_address": addr, "address_sig": address_sig,
                        "entity_key": hex0x(&entity_addr), "entity_sig": hex0x(&entity_sig),
                        "evidence_kind": ATTESTATION_EVIDENCE, "content_hash": hex0x(&att.content_hash().0),
                        "verdict": verdict, "forged_entity_verdict": forged_verdict,
                        "registry_domain": "onyx.devnet"},
        "bip322_vectors": vectors,
        "live": live,
    });
    std::fs::write(out.join("demo.json"), serde_json::to_string_pretty(&data)?)?;
    println!(
        "wrote {} (full root {}, half root {}, {} rows, wasmi {:.3} s)",
        out.display(),
        hex0x(&tree.root().0),
        hex0x(&half_tree.root().0),
        full_run.leaves.len(),
        full_run.secs
    );
    Ok(())
}
