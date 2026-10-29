//! Golden test of `btc-pool-blocks` (ADR-0032): the committed coinbase proofs
//! of 11 mainnet blocks (970,019-970,029; chosen because every coinbase there
//! carries only a pool's own tag and pays at most two outputs, POOL-R1-F2) load (proof of work, parent links, BIP-34 height,
//! coinbase Merkle branch), the recipe is byte-identical across runs and a
//! native build, matches the checked-in golden file and an independent
//! Python recount, and stays far under half its fuel. With the gitignored
//! week blocks (`ONYX_WEEK_DIR`, default `fixtures-live/week`) every day of
//! the website's pool history is rerun from raw blocks and recounted, and the
//! website's pinned commitment is recomputed; without them that test says so
//! and returns. Regenerate the golden file with
//! `ONYX_BLESS=1 cargo test -p onyx-source-btc --test golden_pools` (only
//! with a recipe or table change).

use borsh::BorshDeserialize;
use onyx_core::{keccak256, Hash32, Registry, RuntimeProfile, ShardSpec, SourceId, SourceRef};
use onyx_merkle::MerkleTree;
use onyx_pool_schema::{PoolBlocksRow, UNKNOWN_POOL};
use onyx_recipe_host::{assemble_input, Host, Outcome, HOST_ID};
use onyx_recipe_sdk::{OutputRow, RecipeInput};
use onyx_source_btc::node::{NodePin, MAINNET_MIN_BITS};
use onyx_source_btc::pools::{
    load_coinbase_proofs, load_coinbase_range, pool_table, pools_commitment, CoinbaseProofs,
    PoolShard, EXCLUDED_POOL_IDS,
};
use onyx_source_btc::{fixture_registry_pools, SCHEMA_POOL_BLOCKS, SOURCE_ID};
use std::collections::BTreeMap;
use std::path::PathBuf;

const PROOFS: &str = "btc/coinbase-970019-970029.json";
const GOLDEN: &str = "golden/btc-pool-blocks-970019-970029.json";
const LIVE_TIP: &str = "0000000000000000000200491e37ff370d8f79bd056051ddc6e9123da05ba446";
/// What the website pins (`apps/web/verify` `pinned::POOLS_*`).
const POOLS_COMMITMENT: &str = "0x8421123c0290c9a41205415489f6f4da945cdceedc2681dfb81a4ca6656d8e4c";
const POOLS_TABLE: &str = "0x9c84add53ed8ac3933948ef81455fcd24c5dad5ba8ffb5dcf72d2490945912cc";

fn root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
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

fn wasm() -> (Vec<u8>, Hash32) {
    let w = std::fs::read(root().join("recipes/out/btc_pool_blocks.wasm"))
        .expect("built recipe; run scripts/build-recipes.sh");
    let id = keccak256(&w);
    let rec = std::fs::read_to_string(root().join("recipes/out/btc_pool_blocks.wasm.keccak"))
        .expect("keccak file");
    assert_eq!(rec.trim(), id.to_string(), "id file out of date");
    (w, id)
}

fn proofs() -> CoinbaseProofs {
    onyx_source_btc::verify_checksum(&root().join("fixtures"), PROOFS).expect("checksum");
    serde_json::from_slice(&std::fs::read(root().join("fixtures").join(PROOFS)).expect("read"))
        .expect("proofs")
}

fn pin(tip: Option<(u64, &str)>) -> NodePin {
    NodePin {
        min_bits: MAINNET_MIN_BITS,
        tip: tip.map(|(h, s)| (h, s.to_string())),
    }
}

/// Run one shard on the canonical engine; returns leaves and fuel.
fn run(source: Vec<u8>, snapshot: onyx_core::Snapshot) -> (Vec<Vec<u8>>, u64, Vec<u8>) {
    let (w, id) = wasm();
    let (table, _) = pool_table(&root().join("fixtures")).expect("table");
    let params = borsh::to_vec(&table).expect("params");
    let reg = fixture_registry_pools();
    let spec = ShardSpec {
        recipe: Some(id),
        runtime: Some(profile()),
        label_set: None,
        source: SourceRef {
            source: SourceId(SOURCE_ID.into()),
            snapshot,
        },
        schema: keccak256(SCHEMA_POOL_BLOCKS),
        rights_hash: reg
            .source_rights(&SourceId(SOURCE_ID.into()))
            .expect("source")
            .hash(),
        params: params.clone(),
        registry_root: reg.root(),
    };
    assert_eq!(spec.validate(&reg), Ok(()));
    let input = assemble_input(&spec, source, None, &[], &reg)
        .expect("assemble")
        .bytes;
    let host = Host::new().expect("host");
    let r = host.run(&id, &w, &input, &profile()).expect("run");
    assert_eq!(r.outcome, Outcome::Ok);
    let again = host.run(&id, &w, &input, &profile()).expect("run");
    assert_eq!(r.leaves, again.leaves, "deterministic");
    (r.leaves, r.fuel_used, input)
}

fn rows(leaves: &[Vec<u8>]) -> BTreeMap<String, u64> {
    leaves
        .iter()
        .map(|l| {
            let row = OutputRow::try_from_slice(l).expect("row");
            let v = PoolBlocksRow::try_from_slice(&row.value).expect("pool row");
            assert_eq!(row.key, v.pool.as_bytes());
            (v.pool, v.blocks)
        })
        .collect()
}

fn python(args: &[&str]) -> BTreeMap<String, u64> {
    let out = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/recount-pool-blocks.py"))
        .arg(root().join("fixtures/pools/pool-allowlist.json"))
        .args(args)
        .output()
        .expect("python3");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("recount json")
}

#[test]
fn live_coinbases_give_the_golden_pool_counts() {
    let p = proofs();
    let (source, snapshot) =
        load_coinbase_proofs(&p, &pin(Some((970_029, LIVE_TIP)))).expect("proofs load");
    let (leaves, fuel, input) = run(source, snapshot);
    // Far under half the fuel limit (preflight rule).
    assert!(fuel < 2_000_000_000, "fuel {fuel}");
    eprintln!("11 coinbases: {fuel} fuel");
    // Native build agrees with the WASM run.
    let native = btc_pool_blocks::run(borsh::from_slice::<RecipeInput>(&input).expect("input"))
        .expect("native");
    let native_leaves: Vec<Vec<u8>> = native
        .rows
        .iter()
        .map(|r| borsh::to_vec(r).expect("row"))
        .collect();
    assert_eq!(native_leaves, leaves);
    let got = rows(&leaves);
    assert_eq!(got.values().sum::<u64>(), 11);
    // Independent recount from the same proof file.
    assert_eq!(
        got,
        python(&[
            "proofs",
            root().join("fixtures").join(PROOFS).to_str().expect("path")
        ])
    );
    let golden = serde_json::json!({
        "recipe_id": wasm().1.to_string(),
        "table": POOLS_TABLE,
        "root": MerkleTree::from_rows(&leaves).root().to_string(),
        "rows": got,
    });
    let path = root().join("fixtures").join(GOLDEN);
    if std::env::var("ONYX_BLESS").is_ok() {
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&golden).expect("json") + "\n",
        )
        .expect("bless");
    }
    let want: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("golden")).expect("golden json");
    assert_eq!(golden, want);
}

#[test]
fn a_tampered_proof_is_refused() {
    let good = proofs();
    let p = pin(None);
    assert!(load_coinbase_proofs(&good, &p).is_ok());
    // An edited coinbase (one output byte) no longer folds to the root.
    let mut bad = good.clone();
    let c = &mut bad.blocks[3].coinbase;
    let flip = if c.ends_with('0') { '1' } else { '0' };
    c.pop();
    c.push(flip);
    assert!(load_coinbase_proofs(&bad, &p).is_err());
    // A wrong branch hash.
    let mut bad = good.clone();
    bad.blocks[5].branch[0] = "00".repeat(32);
    assert!(load_coinbase_proofs(&bad, &p).is_err());
    // A dropped block breaks the parent link.
    let mut bad = good.clone();
    bad.blocks.remove(5);
    assert!(load_coinbase_proofs(&bad, &p).is_err());
    // A header with its nonce changed fails proof of work.
    let mut bad = good.clone();
    let h = &mut bad.blocks[0].header;
    let tail = if h.ends_with("00") { "01" } else { "00" };
    h.replace_range(h.len() - 2.., tail);
    assert!(load_coinbase_proofs(&bad, &p).is_err());
    // The pinned tip must be the last block.
    assert!(load_coinbase_proofs(&good, &pin(Some((970_029, &"00".repeat(32))))).is_err());
}

/// R-POOL-2 (POOL-4): a tag that sits inside another pool's longer tag must
/// come after that pool in table order, or the longer tag's blocks go to the
/// wrong pool. The pinned allowlist has two such pairs (EMCDPool before
/// EclipseMC, TATMAS Pool before ViaBTC), both in the safe order; a list
/// update that breaks one fails here. No payout script is
/// listed for two pools.
#[test]
fn a_tag_inside_another_pools_tag_comes_after_it() {
    let (t, _) = pool_table(&root().join("fixtures")).expect("table");
    let mut safe = 0;
    for (i, p) in t.pools.iter().enumerate() {
        for q in &t.pools[i + 1..] {
            for a in &p.tags {
                for b in &q.tags {
                    let inside = |s: &[u8], x: &[u8]| {
                        s.len() > x.len() && s.windows(x.len()).any(|w| w == x)
                    };
                    assert!(
                        !inside(b, a),
                        "{:?} of {} would take {:?} blocks of {}",
                        String::from_utf8_lossy(a),
                        p.name,
                        String::from_utf8_lossy(b),
                        q.name
                    );
                    safe += usize::from(inside(a, b));
                }
            }
        }
    }
    assert_eq!(safe, 2, "overlapping tag pairs in the pinned list");
    let mut scripts: Vec<&Vec<u8>> = t.pools.iter().flat_map(|p| &p.scripts).collect();
    let n = scripts.len();
    scripts.sort();
    scripts.dedup();
    assert_eq!(scripts.len(), n, "a payout script listed for two pools");
}

/// R-POOL-1 and R-POOL-3: the table names reviewed companies only; excluded
/// list entries never appear in it or in any exported row.
#[test]
fn the_table_names_reviewed_pools_only() {
    let (table, info) = pool_table(&root().join("fixtures")).expect("table");
    assert_eq!(
        keccak256(&borsh::to_vec(&table).expect("params")).to_string(),
        POOLS_TABLE
    );
    // The committed allowlist holds no excluded id (POOL-R1-F4): the upstream
    // list is not committed at all, since it names people.
    let list: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("fixtures/pools/pool-allowlist.json")).expect("list"),
    )
    .expect("json");
    let ids: Vec<u64> = list
        .as_array()
        .expect("array")
        .iter()
        .map(|p| p["id"].as_u64().expect("id"))
        .collect();
    for (id, _) in EXCLUDED_POOL_IDS {
        assert!(
            !ids.contains(&u64::from(*id)),
            "excluded id {id} in the allowlist"
        );
    }
    assert!(!root().join("fixtures/pools/pool-list.json").exists());
    assert_eq!(
        table.pools.len(),
        ids.len(),
        "every allowlisted pool has a rule"
    );
    for p in &table.pools {
        assert_ne!(p.name, UNKNOWN_POOL);
    }
    assert_eq!(info.len(), table.pools.len());
    assert!(info
        .iter()
        .all(|p| p.link.starts_with("http") && !p.link.contains("github.com")));
    // Every name in the website export is a table name or "unknown".
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/pools-week.json")).expect("export"),
    )
    .expect("json");
    for d in doc["days"].as_array().expect("days") {
        for r in d["rows"].as_array().expect("rows") {
            let n = r["pool"].as_str().expect("pool");
            assert!(
                n == UNKNOWN_POOL || table.pools.iter().any(|p| p.name == n),
                "{n} not in the table"
            );
        }
    }
}

#[test]
fn the_week_reruns_to_the_pinned_commitment() {
    // A relative ONYX_WEEK_DIR is taken from the repo root (cargo runs tests
    // in the crate directory); a set but missing directory is an error, not
    // a silent skip (POOL-R2-F5).
    let set = std::env::var("ONYX_WEEK_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(|d| {
            let d = PathBuf::from(d);
            if d.is_absolute() {
                d
            } else {
                root().join(d)
            }
        });
    let dir = set
        .clone()
        .unwrap_or_else(|| root().join("fixtures-live/week"));
    if !dir.join("969839.raw.hex").exists() {
        assert!(
            set.is_none(),
            "ONYX_WEEK_DIR is set but has no week blocks: {}",
            dir.display()
        );
        eprintln!("skipped: no week blocks at {} (gitignored)", dir.display());
        return;
    }
    let doc: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("apps/web/static/history/pools-week.json")).expect("export"),
    )
    .expect("json");
    let tip = doc["tip"].as_str().expect("tip").to_string();
    let days = doc["days"].as_array().expect("days");
    let mut shards = Vec::new();
    for (n, d) in days.iter().enumerate() {
        let from = d["from"].as_u64().expect("from");
        let to = d["to"].as_u64().expect("to");
        let last = (n + 1 == days.len()).then_some((to, tip.as_str()));
        let (source, snapshot) = load_coinbase_range(&dir, from, to, &pin(last)).expect("load");
        let (leaves, fuel, _) = run(source, snapshot);
        assert!(fuel < 2_000_000_000);
        let root_hash = MerkleTree::from_rows(&leaves).root();
        assert_eq!(d["root"].as_str(), Some(root_hash.to_string().as_str()));
        let want: BTreeMap<String, u64> = d["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| {
                (
                    r["pool"].as_str().expect("p").to_string(),
                    r["blocks"].as_u64().expect("b"),
                )
            })
            .collect();
        assert_eq!(rows(&leaves), want);
        assert_eq!(
            want,
            python(&[
                "raw",
                dir.to_str().expect("dir"),
                &from.to_string(),
                &to.to_string()
            ])
        );
        shards.push(PoolShard {
            from,
            to,
            root: root_hash,
        });
    }
    let mut t = [0u8; 32];
    hex::decode_to_slice(&tip, &mut t).expect("tip");
    let mut tb = [0u8; 32];
    hex::decode_to_slice(POOLS_TABLE.trim_start_matches("0x"), &mut tb).expect("table hash");
    let table = Hash32(tb);
    assert_eq!(
        pools_commitment(&wasm().1, &table, &t, &shards).to_string(),
        POOLS_COMMITMENT
    );
    assert_eq!(doc["commitment"].as_str(), Some(POOLS_COMMITMENT));
}
