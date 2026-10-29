// SPDX-License-Identifier: Apache-2.0
//! Label set v3 of `test.onyx.entities.btc` from the issuers' own dated
//! rosters (ROADMAP P2a, ADR-0037 Amendment 1).
//!
//! Input: the normalized listings that `python3 -I scripts/build-rosters.py
//! listings <out.json>` writes from the committed snapshot files under
//! `fixtures/rosters/` (one entry per dated snapshot: its record and the
//! addresses it lists, each with the proof the issuer published, if any).
//! This module decides every grade itself: a row is [`Grade::Signed`] only
//! when its published signature verifies here ([`verify_row`]); a published
//! redeem script that does not hash to its address drops the row
//! ([`script_binds`]); everything else the issuer listed is
//! [`Grade::Published`]. The memberships then come from
//! [`onyx_labels::roster::derive`] with [`MAX_LISTING_AGE`].
//!
//! Output: the dated set file (format 2) with its snapshot table: every
//! member cites the records of the first and last snapshot of its range by
//! index, and the loader ([`crate::dated_label_set_from_json`]) turns those
//! into evidence hashes, so the URL, issuer date, fetch date, height and raw
//! hashes of each snapshot are committed in the Merkle leaves.

use crate::signed_roster::{script_binds, verify_row, Verdict};
use crate::{label_kind, BtcAdapterError};
use onyx_core::{EntityKind, Hash32};
use onyx_labels::dated::DatedLabelSet;
use onyx_labels::roster::{
    derive, Dropped, Grade, Roster, RosterSnapshot, SnapshotRecord, MAX_LISTING_AGE,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

/// Fixture path of label set v3 (relative to `fixtures/`).
pub const LABELS_V3: &str = "labels/test.onyx.entities.btc.v3.json";
/// Name of the set (test data: never served, R-LBL-FIX-1).
pub const V3_NAME: &str = "test.onyx.entities.btc";
/// Version 3: the first dated version of the name (ADR-0037 Amendment 1).
pub const V3_VERSION: u32 = 3;
/// Publication height: the last block of the committed header chain
/// (`fixtures/reserves/headers-969839-971207.json`). Every snapshot is older;
/// nothing after it is used (ADR-0037 review D1-R1 F3).
pub const V3_PUBLISHED: u64 = 971_207;

fn bad(e: impl std::fmt::Display) -> BtcAdapterError {
    BtcAdapterError::Fixture(e.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listings {
    format: u32,
    #[allow(dead_code)]
    generated_by: String,
    sources: Vec<ListedSource>,
    proofs: Vec<Proof>,
    deny: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListedSource {
    key: String,
    source: String,
    entity: String,
    kind: String,
    name: String,
    snapshots: Vec<ListedSnapshot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListedSnapshot {
    id: String,
    issued: String,
    fetched: String,
    url: String,
    height: u64,
    height_basis: String,
    raw_sha256: String,
    raw_keccak: String,
    listed: Vec<(String, Option<usize>)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    address: String,
    message: String,
    signatures: Vec<String>,
    script: String,
}

/// What the build did with one source's rows (summed over its snapshots).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceReport {
    /// Source key (`okx`, `binance`, ...).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Entity id.
    pub entity: String,
    /// Snapshots read.
    pub snapshots: u64,
    /// Distinct addresses listed.
    pub addresses: u64,
    /// Rows graded signed (signature verified here).
    pub signed_rows: u64,
    /// Rows graded published (no signature published, or none that verifies).
    pub published_rows: u64,
    /// Rows whose published signature failed or could not be checked (kept as published).
    pub unverified_signature_rows: u64,
    /// Rows dropped: the published redeem script does not hash to the address.
    pub script_mismatch_rows: u64,
    /// Rows whose signature verifies over a message that does not name the
    /// issuer (kept as published: such a signature is not a claim by it).
    pub message_not_naming_issuer_rows: u64,
}

/// The built set with its snapshot table and what was dropped.
#[derive(Clone, Debug)]
pub struct BuiltV3 {
    /// The dated set.
    pub set: DatedLabelSet,
    /// Every snapshot record, sorted by (entity, height, source).
    pub records: Vec<SnapshotRecord>,
    /// Addresses dropped by the derivation (deny list, two entities at once).
    pub dropped: Vec<Dropped>,
    /// Per-source counts.
    pub report: Vec<SourceReport>,
}

/// The exact messages each signing issuer signs its rows with (as published
/// in its committed snapshots). HTX signs as "huobi", its former name.
pub const ISSUER_MESSAGES: &[(&str, &[&str])] = &[
    ("okx", &["I am an OKX address", "I am a OKX address"]),
    ("htx", &["huobi"]),
    ("coinex", &["I am a CoinEx address"]),
];

/// Review P2a-R1-2: a signature counts as the issuer's only when the signed
/// message is the issuer's own (any message would let a copied third-party
/// proof pass). Review P2a-R2-3: exact match, not "contains": a message such
/// as "OKX and CoinEx" or "not an OKX address" names two issuers or denies
/// one. A new message text leaves its rows `published` until it is added here.
/// Note: a signature shows the key signed that message at some time; the
/// snapshot shows the issuer listed it at the snapshot's height.
pub fn names_issuer(source_key: &str, message: &str) -> bool {
    ISSUER_MESSAGES
        .iter()
        .find(|(k, _)| *k == source_key)
        .is_some_and(|(_, ms)| ms.contains(&message))
}

fn hex32(s: &str) -> Result<[u8; 32], BtcAdapterError> {
    let mut b = [0u8; 32];
    hex::decode_to_slice(s, &mut b).map_err(bad)?;
    Ok(b)
}

/// Build label set v3 from the normalized listings (see the module doc).
pub fn build_v3(listings_json: &str) -> Result<BuiltV3, BtcAdapterError> {
    let l: Listings = serde_json::from_str(listings_json).map_err(bad)?;
    if l.format != 1 {
        return Err(bad("listings format"));
    }
    // Each proof is judged once: does a published script bind (P2SH/P2WSH:
    // hashes to the address), and does a signature verify.
    let verdicts: Vec<(Option<Verdict>, Option<bool>)> = l
        .proofs
        .iter()
        .map(|p| {
            let binds = script_binds(&p.address, &p.script);
            if p.signatures.is_empty() {
                (None, binds)
            } else {
                let sigs: Vec<&str> = p.signatures.iter().map(String::as_str).collect();
                (
                    Some(verify_row(&p.address, &p.message, &sigs, &p.script)),
                    binds,
                )
            }
        })
        .collect();
    let mut rosters = Vec::new();
    let mut records = Vec::new();
    let mut report = Vec::new();
    for s in &l.sources {
        let kind: EntityKind =
            label_kind(&s.kind).ok_or_else(|| bad(format!("kind {}", s.kind)))?;
        let mut rep = SourceReport {
            key: s.key.clone(),
            name: s.name.clone(),
            entity: s.entity.clone(),
            ..SourceReport::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut snapshots = Vec::new();
        for snap in &s.snapshots {
            let record = SnapshotRecord {
                source: s.source.clone(),
                entity: s.entity.clone(),
                url: snap.url.clone(),
                issued: snap.issued.clone(),
                fetched: snap.fetched.clone(),
                height: snap.height,
                height_basis: snap.height_basis.clone(),
                raw_sha256: hex32(&snap.raw_sha256)?,
                raw_keccak: Hash32(hex32(&snap.raw_keccak)?),
            };
            let mut listed = Vec::with_capacity(snap.listed.len());
            for (address, proof) in &snap.listed {
                let grade = match proof {
                    None => Grade::Published,
                    Some(i) => {
                        let p = l.proofs.get(*i).ok_or_else(|| bad("proof index"))?;
                        if format!("btc:{}", p.address) != *address {
                            return Err(bad(format!(
                                "{} {}: proof for another address",
                                s.key, snap.id
                            )));
                        }
                        match &verdicts[*i] {
                            // A published script that does not hash to its address
                            // drops the row, signed or not (ADR-0037 Am. 1 item 3).
                            (_, Some(false)) => {
                                rep.script_mismatch_rows += 1;
                                continue;
                            }
                            (Some(v), _) if v.is_verified() && names_issuer(&s.key, &p.message) => {
                                Grade::Signed
                            }
                            (Some(v), _) if v.is_verified() => {
                                rep.message_not_naming_issuer_rows += 1;
                                Grade::Published
                            }
                            (Some(_), _) => {
                                rep.unverified_signature_rows += 1;
                                Grade::Published
                            }
                            (None, _) => Grade::Published,
                        }
                    }
                };
                match grade {
                    Grade::Signed => rep.signed_rows += 1,
                    Grade::Published => rep.published_rows += 1,
                }
                seen.insert(address.clone());
                listed.push((address.clone(), grade));
            }
            rep.snapshots += 1;
            records.push(record.clone());
            snapshots.push(RosterSnapshot { record, listed });
        }
        rep.addresses = seen.len() as u64;
        report.push(rep);
        rosters.push(Roster {
            entity: s.entity.clone(),
            kind,
            snapshots,
        });
    }
    let d = derive(
        V3_NAME,
        V3_VERSION,
        V3_PUBLISHED,
        MAX_LISTING_AGE,
        &rosters,
        &l.deny,
    )
    .map_err(bad)?;
    records.sort_by(|a, b| (&a.entity, a.height, &a.source).cmp(&(&b.entity, b.height, &b.source)));
    Ok(BuiltV3 {
        set: d.set,
        records,
        dropped: d.dropped,
        report,
    })
}

fn grade_of(evidence_kind: &str) -> Option<&'static str> {
    match evidence_kind {
        onyx_labels::roster::EVIDENCE_SIGNED => Some("signed"),
        onyx_labels::roster::EVIDENCE_PUBLISHED => Some("published"),
        _ => None,
    }
}

/// The set file: one snapshot record and one member per line, keys sorted,
/// byte-for-byte the same for the same input.
pub fn v3_to_json(b: &BuiltV3) -> Result<String, BtcAdapterError> {
    let index: BTreeMap<Hash32, usize> = b
        .records
        .iter()
        .enumerate()
        .map(|(i, r)| (r.hash(), i))
        .collect();
    let mut out = format!(
        "{{\"name\":{},\"version\":{},\"published\":{},\n\"snapshots\":[\n",
        serde_json::to_string(&b.set.name).map_err(bad)?,
        b.set.version,
        b.set.published
    );
    let recs: Vec<String> = b
        .records
        .iter()
        .map(|r| {
            json!({"source": r.source, "entity": r.entity, "url": r.url, "issued": r.issued,
                "fetched": r.fetched, "height": r.height, "height_basis": r.height_basis,
                "raw_sha256": hex::encode(r.raw_sha256), "raw_keccak": hex::encode(r.raw_keccak.0)})
            .to_string()
        })
        .collect();
    out.push_str(&recs.join(",\n"));
    out.push_str("\n],\n\"members\":[\n");
    let mut rows = Vec::with_capacity(b.set.members.len());
    for m in &b.set.members {
        let grade = m
            .label
            .evidence
            .first()
            .and_then(|e| grade_of(&e.kind))
            .ok_or_else(|| bad("member without roster evidence"))?;
        let ev: Vec<usize> = m
            .label
            .evidence
            .iter()
            .map(|e| {
                index
                    .get(&e.content_hash)
                    .copied()
                    .ok_or_else(|| bad("evidence not in the table"))
            })
            .collect::<Result<_, _>>()?;
        let kind = match m.label.kind {
            EntityKind::Exchange => "Exchange",
            EntityKind::Etf => "Etf",
            EntityKind::Custodian => "Custodian",
            _ => return Err(bad("kind")),
        };
        rows.push(
            json!({"address": m.label.address, "entity": m.label.entity, "kind": kind,
                "confidence_ppm": m.label.confidence.0, "from": m.from, "to": m.to,
                "grade": grade, "evidence": ev})
            .to_string(),
        );
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n]}\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::names_issuer;

    /// Review P2a-R1-2: a verified signature is the issuer's claim only over
    /// a message that names it (HTX signs as "huobi", its former name).
    #[test]
    fn a_signature_counts_only_over_a_message_naming_the_issuer() {
        assert!(names_issuer("okx", "I am an OKX address"));
        assert!(names_issuer("okx", "I am a OKX address"));
        assert!(names_issuer("htx", "huobi"));
        assert!(names_issuer("coinex", "I am a CoinEx address"));
        assert!(!names_issuer("htx", "King will be back!"));
        assert!(!names_issuer("okx", "I am a CoinEx address"));
        assert!(
            !names_issuer("binance", "I am a Binance address"),
            "no signed source"
        );
        // Review P2a-R2-3: exact messages only.
        assert!(!names_issuer(
            "okx",
            "I am a CoinEx address, not an OKX address"
        ));
        assert!(!names_issuer(
            "coinex",
            "I am a CoinEx address, not an OKX address"
        ));
        assert!(!names_issuer("htx", "htx"), "HTX signs as huobi");
        assert!(!names_issuer("okx", "i am an okx address"));
    }

    /// Review P2a-R2-3: a real proof is graded by the issuer whose roster lists
    /// the row and by the exact message: CoinEx's own verified proof listed in
    /// OKX's roster is `published`; an OKX proof with its message changed to
    /// another issuer's no longer verifies; the untouched proofs are `signed`.
    #[test]
    fn a_proof_counts_only_for_its_own_issuer_and_message() {
        const COINEX_ADDR: &str = "111WVAJSmeaJYgVY2nKhArNWkvrRcV3mT";
        const COINEX_SIG: &str = "IJsR4cZysJ/VLYxXaa6ACXgrETwS5haYzOdTIV4lp3MTY84feBG8C1JAkGKhs8lyn77PzI616lOZ+gPdA93aZ4U=";
        const OKX_ADDR: &str = "13jTtHxBPFwZkaCdm6BwJMMJkqvTpBZccw";
        const OKX_SIGS: &str = r#"["IPUTSOQjXrOLy7WXyalVeuIpTlvQENsSjdjNETlvTNo+MB6LDF22NpGR9ragpCV8GnyyxXibUTgtrFjqR4A4acU=","H5PEB4/dXOxDDpzwbk0k3xjkxAeEPtnoFlzkDMiNPyApXbdkQWbzHiH/k5qn2JOuSfBGEo1s+b72iZayd/VzPe0="]"#;
        let build = |key: &str, entity: &str, addr: &str, msg: &str, sigs: &str| {
            let doc = format!(
                r#"{{"format":1,"generated_by":"t","deny":[],
                "proofs":[{{"address":"{addr}","message":"{msg}","signatures":{sigs},"script":""}}],
                "sources":[{{"key":"{key}","source":"{key}.por","entity":"{entity}","kind":"Exchange","name":"X",
                "snapshots":[{{"id":"a","issued":"2026-01-01","fetched":"2026-01-02","url":"https://example.invalid/x",
                "height":100,"height_basis":"issuer","raw_sha256":"{z}","raw_keccak":"{z}",
                "listed":[["btc:{addr}",0]]}}]}}]}}"#,
                z = "11".repeat(32)
            );
            let b = super::build_v3(&doc).expect("builds");
            assert_eq!(b.set.members.len(), 1);
            (
                b.set.members[0].label.evidence[0].kind.clone(),
                b.report[0].clone(),
            )
        };
        let coinex_sig = format!(r#"["{COINEX_SIG}"]"#);
        // The genuine rows, each in its own issuer's roster: signed.
        let (k, _) = build(
            "coinex",
            "entity:coinex",
            COINEX_ADDR,
            "I am a CoinEx address",
            &coinex_sig,
        );
        assert_eq!(k, onyx_labels::roster::EVIDENCE_SIGNED);
        let (k, _) = build(
            "okx",
            "entity:okx",
            OKX_ADDR,
            "I am an OKX address",
            OKX_SIGS,
        );
        assert_eq!(k, onyx_labels::roster::EVIDENCE_SIGNED);
        // CoinEx's verified proof in OKX's roster: not OKX's claim.
        let (k, r) = build(
            "okx",
            "entity:okx",
            COINEX_ADDR,
            "I am a CoinEx address",
            &coinex_sig,
        );
        assert_eq!(k, onyx_labels::roster::EVIDENCE_PUBLISHED);
        assert_eq!(r.message_not_naming_issuer_rows, 1);
        // OKX's signatures replayed over CoinEx's message: they no longer verify.
        let (k, r) = build(
            "coinex",
            "entity:coinex",
            OKX_ADDR,
            "I am a CoinEx address",
            OKX_SIGS,
        );
        assert_eq!(k, onyx_labels::roster::EVIDENCE_PUBLISHED);
        assert_eq!(r.unverified_signature_rows, 1);
    }

    /// Review P2a-R1-8: a published script that does not hash to its P2SH
    /// address drops the row, whether or not it carries signatures.
    #[test]
    fn a_non_binding_script_drops_the_row_signed_or_not() {
        let listings = |sigs: &str| {
            format!(
                r#"{{"format":1,"generated_by":"t","deny":[],
                "proofs":[{{"address":"3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy","message":"I am an OKX address","signatures":{sigs},"script":"51"}}],
                "sources":[{{"key":"okx","source":"okx.por","entity":"entity:okx","kind":"Exchange","name":"OKX",
                "snapshots":[{{"id":"a","issued":"2026-01-01","fetched":"2026-01-02","url":"https://static.okx.com/x",
                "height":100,"height_basis":"issuer","raw_sha256":"{z}","raw_keccak":"{z}",
                "listed":[["btc:3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy",0]]}}]}}]}}"#,
                z = "11".repeat(32)
            )
        };
        for sigs in [r#"[]"#, r#"["AAAA"]"#] {
            let b = super::build_v3(&listings(sigs)).expect("builds");
            assert!(b.set.members.is_empty(), "{sigs}: row dropped");
            assert_eq!(b.report[0].script_mismatch_rows, 1, "{sigs}");
        }
    }
}
