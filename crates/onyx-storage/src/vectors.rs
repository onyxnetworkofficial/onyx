//! The shared Rust/Solidity vectors (`fixtures/storage/vectors.json`, read
//! by `contracts/test/OnyxStorageVectors.t.sol`). Regenerate only with an
//! ADR-0060 change: `ONYX_BLESS=1 cargo test -p onyx-storage vectors`.

use crate::challenge::{answer, entry_bytes, tests::dataset};
use crate::deal::{check_payload, read_header};
use crate::ids::*;
use crate::ledger::{self, Params};
use crate::placement::{holders_at, Exclusions, WHOLE_DEAL};
use onyx_core::Hash32;
use serde_json::{json, Value};

fn h(b: u8) -> Hash32 {
    onyx_core::keccak256(&[b])
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn hs(v: &[Hash32]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// The parameters of the vector deployment (also the demo's).
pub(crate) fn vector_params() -> Params {
    Params {
        price_per_gib_epoch: 10u128.pow(15),
        bond_per_gib: 10u128.pow(15),
        min_bond: 10u128.pow(18),
        slash_ppm: 100_000,
        challenges_per_deal: 2,
        max_misses: 3,
    }
}

fn build() -> Value {
    let operator = [0x11u8; 20];
    let nodes: Vec<NodeId> = (0..10)
        .map(|i| node_id(&operator, i, &h(i as u8)))
        .collect();
    let node_ids: Vec<Value> = (0..3u64)
        .map(|i| {
            json!({"operator": hx(&operator), "index": i, "blockHash": h(i as u8).to_string(),
                   "id": nodes[i as usize].to_string()})
        })
        .collect();
    let salt = deal_salt(&h(40), 7);
    let keys: Vec<Value> = (0..4u32)
        .map(|i| {
            let k = shard_key(&salt, i);
            json!({"salt": salt.to_string(), "index": i, "key": k.to_string(),
                   "node": nodes[i as usize].to_string(),
                   "weight": weight(&k, &nodes[i as usize]).to_string()})
        })
        .collect();
    let seeds: Vec<Value> = (0..3u64)
        .map(|e| {
            let s = epoch_seed(&h(50 + e as u8), e + 2);
            json!({"blockHash": h(50 + e as u8).to_string(), "epoch": e + 2, "seed": s.to_string(),
                   "dealId": 7, "slot": e, "shardCount": 1000 + e,
                   "shard": slot_shard(&s, 7, e as u32, 1000 + e as u32),
                   "rows": 77_777u64 + e, "row": slot_row(&s, 7, e as u32, 77_777 + e)})
        })
        .collect();
    let mut ex = Exclusions::new();
    let first = holders_at(&salt, 7, 3, &nodes, 3, 9, &ex);
    ex.insert(7, 3, first[0], 10);
    let holder_cases: Vec<Value> = (0..6u32)
        .map(|i| json!({"index": i, "holders": hs(&holders_at(&salt, 7, i, &nodes, 3, 9, &Exclusions::new()).iter().map(|n| n.0).collect::<Vec<_>>())}))
        .collect();
    let after: Vec<Hash32> = holders_at(&salt, 7, 3, &nodes, 3, 10, &ex)
        .iter()
        .map(|n| n.0)
        .collect();
    // A whole-deal decline by the first holder of shard 4, from epoch 10.
    let mut ex2 = Exclusions::new();
    let first4 = holders_at(&salt, 7, 4, &nodes, 3, 9, &ex2)[0];
    ex2.insert(7, WHOLE_DEAL, first4, 10);
    let after4: Vec<Hash32> = holders_at(&salt, 7, 4, &nodes, 3, 10, &ex2)
        .iter()
        .map(|n| n.0)
        .collect();

    let (m, files) = dataset(5, 11);
    let header = borsh::to_vec(&m.header).unwrap_or_default();
    let tail = read_header(&header).expect("reads");
    let secret = [7u8; 32];
    let report = onyx_bulk::anchor::sign(&secret, &m.header).expect("signs");
    let payload = borsh::to_vec(&report.payload).unwrap_or_default();
    let digest = check_payload(&tail, &payload).expect("ok");
    let seed = epoch_seed(&h(60), 4);
    let idx = slot_shard(&seed, 7, 1, 5);
    let p = answer(&m, &files[idx as usize], &seed, 7, 1).expect("proves");

    let pr = vector_params();
    let amounts: Vec<Value> = [(1u64, 1u8, 1u32), (3 << 30, 3, 10), ((5 << 30) + 1, 2, 7)]
        .iter()
        .map(|(b, r, e)| {
            json!({"bytes": b, "r": r, "epochs": e,
                   "deposit": ledger::deposit(&pr, *b, *r, *e).to_string(),
                   "slotPay": ledger::slot_pay(&pr, *b, *r).to_string()})
        })
        .collect();
    json!({
        "comment": "Shared Rust/Solidity vectors of the storage network (ADR-0060). Regenerate only with an ADR change: ONYX_BLESS=1 cargo test -p onyx-storage vectors.",
        "nodeIds": node_ids,
        "dealSalt": {"prevHash": h(40).to_string(), "dealId": 7, "salt": salt.to_string()},
        "shardKeys": keys,
        "seeds": seeds,
        "nodes": hs(&nodes.iter().map(|n| n.0).collect::<Vec<_>>()),
        "setHash": set_hash(&nodes).to_string(),
        "holders": {"salt": salt.to_string(), "dealId": 7, "r": 3, "cases": holder_cases,
                    "excluded": {"index": 3, "node": first[0].to_string(), "from": 10, "after": hs(&after)},
                    "declined": {"index": 4, "node": first4.to_string(), "from": 10, "after": hs(&after4)}},
        "manifest": {"header": hx(&header), "root": tail.manifest_root.to_string(),
                     "shardCount": tail.shard_count, "rowCount": tail.row_count, "byteCount": tail.byte_count,
                     "shardsRoot": tail.shards_root.to_string(), "payload": hx(&payload), "digest": digest.to_string()},
        "proof": {"seed": seed.to_string(), "dealId": 7, "slot": 1, "shard": idx,
                  "entry": hx(&entry_bytes(&p.entry)), "entrySiblings": hs(&p.entry_siblings),
                  "row": hx(&p.row), "rowSiblings": hs(&p.row_siblings),
                  "shardsRoot": m.header.shards_root.to_string(), "shardCount": 5},
        "params": {"pricePerGiBEpoch": pr.price_per_gib_epoch.to_string(), "bondPerGiB": pr.bond_per_gib.to_string(),
                   "minBond": pr.min_bond.to_string(), "slashPpm": pr.slash_ppm,
                   "challengesPerDeal": pr.challenges_per_deal, "maxMisses": pr.max_misses},
        "amounts": amounts,
    })
}

#[test]
fn vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/storage/vectors.json"
    );
    let v = build();
    let text = format!("{}\n", serde_json::to_string_pretty(&v).expect("json"));
    if std::env::var("ONYX_BLESS").is_ok_and(|x| x == "1") {
        std::fs::write(path, &text).expect("write");
    }
    let on_disk = std::fs::read_to_string(path).expect("fixtures/storage/vectors.json");
    assert_eq!(
        on_disk, text,
        "vectors changed: bless only with an ADR-0060 change"
    );
}
