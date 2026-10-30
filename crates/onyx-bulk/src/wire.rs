//! The JSON a node serves for a bulk range query, and the client check of it
//! (everything but the chain read).
//!
//! ```text
//! { "manifest": {"root": "0x…", "header": "0x<borsh ManifestHeader>", ...},
//!   "anchor":   {"report": "0x<borsh SignedReport>", "digest": "0x…", "report_hash": "0x…",
//!                "signer": "0x…", "chain_id": n, "book": "0x…", "fact": "0x…",
//!                "checked_at": {"number": n, "hash": "0x…"}},
//!   "shards": [ {"index": i, "entry": "0x<borsh ShardEntry>", "proof": {...}, ...} ],
//!   "rows":   [ {"shard": i, "index": j, "key": "…", "row": "0x<borsh cells>",
//!                "values": {...}, "proof": {"index": j, "leaf_count": n, "siblings": [...]}} ] }
//! ```
//!
//! [`check_rows_answer`] proves each row to its shard root, each shard entry
//! to the header's `shards_root`, the header to the manifest root, and the
//! root to the publisher's signature; it checks every row fits the schema,
//! lies in the asked key range, and that the rows are one contiguous run (no
//! row skipped inside a page). The chain read (the book recorded that report
//! and accepts it) is left to the caller, who reads its own node.

use crate::anchor;
use crate::manifest::{ManifestHeader, ShardEntry};
use crate::row::{cells_json, decode_row, key_of};
use borsh::BorshDeserialize;
use onyx_core::{Hash32, SignedReport};
use onyx_merkle::Proof;
use serde_json::{json, Value};

/// A malformed or false answer.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Missing or malformed field.
    #[error("answer field {0}")]
    Field(String),
    /// A proof or signature does not hold.
    #[error("{0}")]
    False(String),
}

fn field(s: impl Into<String>) -> WireError {
    WireError::Field(s.into())
}

fn falsity(s: impl Into<String>) -> WireError {
    WireError::False(s.into())
}

/// `0x` and lowercase hex.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// Bytes of a `0x` hex string field.
pub fn hex_field(v: &Value, what: &str) -> Result<Vec<u8>, WireError> {
    v.as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .and_then(|s| hex::decode(s).ok())
        .ok_or_else(|| field(what))
}

/// A 32-byte hex field.
pub fn hash_field(v: &Value, what: &str) -> Result<Hash32, WireError> {
    let b: [u8; 32] = hex_field(v, what)?.try_into().map_err(|_| field(what))?;
    Ok(Hash32(b))
}

fn u64_field(v: &Value, what: &str) -> Result<u64, WireError> {
    v.as_u64().ok_or_else(|| field(what))
}

/// A Merkle proof as JSON.
pub fn proof_json(p: &Proof) -> Value {
    json!({
        "index": p.index,
        "leaf_count": p.leaf_count,
        "siblings": p.siblings.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
    })
}

/// A Merkle proof from JSON.
pub fn proof_from(v: &Value, what: &str) -> Result<Proof, WireError> {
    let sib = v["siblings"]
        .as_array()
        .ok_or_else(|| field(format!("{what}.siblings")))?;
    if sib.len() > 64 {
        return Err(field(format!("{what}.siblings")));
    }
    Ok(Proof {
        index: u64_field(&v["index"], &format!("{what}.index"))?,
        leaf_count: u64_field(&v["leaf_count"], &format!("{what}.leaf_count"))?,
        siblings: sib
            .iter()
            .map(|s| hash_field(s, &format!("{what}.siblings")))
            .collect::<Result<_, _>>()?,
    })
}

/// A shard entry with its proof under the header's shard tree.
pub fn shard_json(e: &ShardEntry, p: &Proof) -> Value {
    json!({
        "index": e.index,
        "root": e.root.to_string(),
        "rows": e.rows,
        "bytes": e.bytes,
        "key_min": e.key_min.to_string(),
        "key_max": e.key_max.to_string(),
        "entry": hex0x(&e.leaf()),
        "proof": proof_json(p),
    })
}

/// A served row.
pub fn row_json(header: &ManifestHeader, shard: u32, row: &[u8], p: &Proof) -> Value {
    let cells = decode_row(&header.schema, row).unwrap_or_default();
    json!({
        "shard": shard,
        "index": p.index,
        "key": key_of(&header.schema, &cells).map(|k| k.to_string()),
        "row": hex0x(row),
        "values": cells_json(&header.schema, &cells),
        "proof": proof_json(p),
    })
}

/// What an answer proved off chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    /// The verified header.
    pub header: ManifestHeader,
    /// Its root.
    pub manifest_root: Hash32,
    /// The signed report anchoring it.
    pub report: SignedReport,
    /// Its payload digest (the book's record key).
    pub digest: Hash32,
    /// `SignedReport::hash` (the book's `reportHash`).
    pub report_hash: Hash32,
    /// The recovered publisher key.
    pub signer: [u8; 20],
    /// `(shard, index, key)` of every row, in order.
    pub rows: Vec<(u32, u64, i128)>,
}

/// Check a range answer for the asked key range `[from, to]` (inclusive;
/// `None` is open). See the module docs for what is checked.
pub fn check_rows_answer(
    answer: &Value,
    from: Option<i128>,
    to: Option<i128>,
) -> Result<Checked, WireError> {
    let hb = hex_field(&answer["manifest"]["header"], "manifest.header")?;
    let header =
        ManifestHeader::try_from_slice(&hb).map_err(|_| field("manifest.header (borsh)"))?;
    header
        .validate()
        .map_err(|e| falsity(format!("manifest header: {e}")))?;
    let manifest_root = header.root();
    if hash_field(&answer["manifest"]["root"], "manifest.root")? != manifest_root {
        return Err(falsity("manifest.root is not the header's root"));
    }
    let rb = hex_field(&answer["anchor"]["report"], "anchor.report")?;
    let report = SignedReport::try_from_slice(&rb).map_err(|_| field("anchor.report (borsh)"))?;
    let signer = anchor::verify(&header, &report)
        .map_err(|e| falsity(format!("publisher signature: {e}")))?;
    let digest = report.payload.digest();
    let report_hash = report.hash();
    if let Some(d) = answer["anchor"].get("digest") {
        if hash_field(d, "anchor.digest")? != digest {
            return Err(falsity("anchor.digest is not the report's digest"));
        }
    }

    let mut shards: Vec<ShardEntry> = Vec::new();
    for (i, s) in answer["shards"]
        .as_array()
        .ok_or_else(|| field("shards"))?
        .iter()
        .enumerate()
    {
        let eb = hex_field(&s["entry"], &format!("shards[{i}].entry"))?;
        let e = ShardEntry::try_from_slice(&eb)
            .map_err(|_| field(format!("shards[{i}].entry (borsh)")))?;
        let p = proof_from(&s["proof"], &format!("shards[{i}].proof"))?;
        if !header.proves_entry(&e, &p) {
            return Err(falsity(format!("shard {} is not in the manifest", e.index)));
        }
        if shards.iter().any(|x| x.index == e.index) {
            return Err(field(format!("shard {} twice", e.index)));
        }
        shards.push(e);
    }

    let mut rows = Vec::new();
    let mut prev: Option<(u32, u64)> = None;
    for (i, r) in answer["rows"]
        .as_array()
        .ok_or_else(|| field("rows"))?
        .iter()
        .enumerate()
    {
        let shard = r["shard"]
            .as_u64()
            .and_then(|s| u32::try_from(s).ok())
            .ok_or_else(|| field(format!("rows[{i}].shard")))?;
        let e = shards
            .iter()
            .find(|e| e.index == shard)
            .ok_or_else(|| field(format!("rows[{i}]: shard {shard} not proven in the answer")))?;
        let bytes = hex_field(&r["row"], &format!("rows[{i}].row"))?;
        let p = proof_from(&r["proof"], &format!("rows[{i}].proof"))?;
        if p.leaf_count != e.rows || !onyx_merkle::verify(&e.root, &bytes, &p) {
            return Err(falsity(format!("row {i} is not in shard {shard}")));
        }
        let cells = decode_row(&header.schema, &bytes)
            .map_err(|e| falsity(format!("row {i} does not fit the schema: {e}")))?;
        if let Some(v) = r.get("values") {
            if *v != cells_json(&header.schema, &cells) {
                return Err(falsity(format!(
                    "row {i}: values differ from the proven row"
                )));
            }
        }
        let key = key_of(&header.schema, &cells).ok_or_else(|| falsity("row without a key"))?;
        if from.is_some_and(|f| key < f) || to.is_some_and(|t| key > t) {
            return Err(falsity(format!(
                "row {i} (key {key}) is outside the asked range"
            )));
        }
        if let Some((ps, pi)) = prev {
            let last_of_prev = shards
                .iter()
                .find(|x| x.index == ps)
                .is_some_and(|x| pi + 1 == x.rows);
            let next = (shard == ps && p.index == pi + 1)
                || (shard == ps + 1 && p.index == 0 && last_of_prev);
            if !next {
                return Err(falsity(format!(
                    "rows skip between row {} and row {i}",
                    i - 1
                )));
            }
        }
        prev = Some((shard, p.index));
        rows.push((shard, p.index, key));
    }
    Ok(Checked {
        header,
        manifest_root,
        report,
        digest,
        report_hash,
        signer,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::rights;
    use crate::manifest::{shards_tree, Manifest, NewVersion};
    use crate::row::{encode_row, Cell};
    use crate::schema::Schema;
    use crate::shard::{check_shard, encode_shard};
    use onyx_core::SourceId;

    const SECRET: [u8; 32] = [0x22; 32];

    /// Two shards of keys 0..5 and 5..10, an answer over keys 3..=6.
    fn fixture() -> (Value, Manifest) {
        let schema = Schema::from_json(&json!({"columns":[{"name":"t","kind":"int"},{"name":"v","kind":"decimal","scale":2}],"key":"t"})).expect("ok");
        let mk = |range: std::ops::Range<i128>| -> Vec<Vec<u8>> {
            range
                .map(|t| {
                    encode_row(&schema, &[Cell::Int(t), Cell::Decimal(format!("{t}.25"))])
                        .expect("ok")
                })
                .collect()
        };
        let files = [encode_shard(&mk(0..5)), encode_shard(&mk(5..10))];
        let checked: Vec<_> = files
            .iter()
            .map(|f| check_shard(&schema, f).expect("ok"))
            .collect();
        let entries: Vec<ShardEntry> = checked
            .iter()
            .enumerate()
            .map(|(i, c)| ShardEntry::new(i as u32, &c.summary))
            .collect();
        let m = Manifest::build(
            NewVersion {
                dataset: "synthetic.test".into(),
                publisher: SourceId("example.publisher".into()),
                created_unix: 1_700_000_000,
                schema,
                rights: rights("example.publisher"),
            },
            None,
            entries.clone(),
        )
        .expect("ok");
        let report = anchor::sign(&SECRET, &m.header).expect("ok");
        let tree = shards_tree(&entries);
        let mut rows = Vec::new();
        for (s, idx) in [(0u32, 3u64), (0, 4), (1, 0), (1, 1)] {
            let c = &checked[s as usize];
            rows.push(row_json(
                &m.header,
                s,
                c.rows[idx as usize],
                &c.tree.proof(idx).expect("ok"),
            ));
        }
        let answer = json!({
            "manifest": {"root": m.header.root().to_string(), "header": hex0x(&borsh::to_vec(&m.header).expect("ok"))},
            "anchor": {"report": hex0x(&borsh::to_vec(&report).expect("ok")), "digest": report.payload.digest().to_string()},
            "shards": [shard_json(&entries[0], &tree.proof(0).expect("ok")), shard_json(&entries[1], &tree.proof(1).expect("ok"))],
            "rows": rows,
        });
        (answer, m)
    }

    #[test]
    fn honest_answer_checks() {
        let (a, m) = fixture();
        let c = check_rows_answer(&a, Some(3), Some(6)).expect("ok");
        assert_eq!(
            c.rows.iter().map(|r| r.2).collect::<Vec<_>>(),
            vec![3, 4, 5, 6]
        );
        assert_eq!(c.manifest_root, m.header.root());
        assert_eq!(c.signer, anchor::address_of(&SECRET).expect("ok"));
    }

    #[test]
    fn forgeries_are_refused() {
        let (a, _) = fixture();
        // A changed row value (bytes), a changed decoded value, a skipped row,
        // a row outside the asked range, a forged shard entry, another
        // manifest root, a tampered signature.
        let mut forged_row = a.clone();
        let mut b = hex_field(&forged_row["rows"][0]["row"], "").expect("ok");
        let last = b.len() - 2;
        b[last] ^= 1;
        forged_row["rows"][0]["row"] = json!(hex0x(&b));
        let mut forged_values = a.clone();
        forged_values["rows"][0]["values"]["v"] = json!("999.00");
        let mut skipped = a.clone();
        skipped["rows"].as_array_mut().expect("ok").remove(1);
        let mut entry = a.clone();
        let mut e =
            ShardEntry::try_from_slice(&hex_field(&entry["shards"][1]["entry"], "").expect("ok"))
                .expect("ok");
        e.rows += 1;
        entry["shards"][1]["entry"] = json!(hex0x(&e.leaf()));
        let mut root = a.clone();
        root["manifest"]["root"] = json!(Hash32([9; 32]).to_string());
        let mut sig = a.clone();
        let mut r =
            SignedReport::try_from_slice(&hex_field(&sig["anchor"]["report"], "").expect("ok"))
                .expect("ok");
        r.signature[5] ^= 1;
        sig["anchor"]["report"] = json!(hex0x(&borsh::to_vec(&r).expect("ok")));
        for (name, bad, from, to) in [
            ("row bytes", &forged_row, Some(3), Some(6)),
            ("values", &forged_values, Some(3), Some(6)),
            ("skip", &skipped, Some(3), Some(6)),
            ("range", &a, Some(4), Some(6)),
            ("entry", &entry, Some(3), Some(6)),
            ("root", &root, Some(3), Some(6)),
            ("signature", &sig, Some(3), Some(6)),
        ] {
            assert!(check_rows_answer(bad, from, to).is_err(), "{name}");
        }
    }
}
