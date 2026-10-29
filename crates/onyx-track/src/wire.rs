//! The served form of one track record (ROADMAP C6; docs/spec/delivery.md
//! "Track records"): the body of `GET /v1/track/{source}/{metric}`, built by the
//! server ([`record_body`]) and checked by every client ([`check_record_body`]).
//! The Rust client and the browser verifier run this same code (ADR-0020).
//!
//! What a check proves (ADR-0036 item 10): the record is under the served
//! commitment's `records_root`, its metric's summary under `metrics_root`, and
//! (when the history is served) the record adds up over its committed entries
//! ([`crate::verify_history`]). It does not prove each outcome is right (a rerun
//! of [`crate::compute`] on the logs does) nor that the commitment is the latest
//! one by itself: a client that reads the record's on-chain commitment log
//! (ADR-0036 amendment 3, [`crate::chain`]) passes its latest as `latest` and
//! anything else is refused (THREATS TR-R2-5); one that cannot read the chain
//! only knows that it is not older than its own floor (`min_as_of`) and, with
//! `known`, that it chains back by hash to a commitment it saw before. Chaining
//! by hash is not extending: a commitment over a rewritten log chained to the
//! known one is refused only by `check_extends` on the logs (THREATS TR-S-14,
//! C20); on chain such a rewrite is at least public and attributable.
//!
//! Rows travel as Borsh hex, never as decoded JSON fields, so there is nothing
//! for a client to read that the proofs do not cover.

use crate::example::is_example_id;
use crate::{
    verify_history, verify_record, verify_summary, Entry, MetricSummary, Record, TrackBook,
    TrackCommitment, TrackParams, VerifyError,
};
use borsh::BorshDeserialize;
use onyx_core::{Hash32, MetricId, SourceId};
use onyx_merkle::Proof;
use serde_json::{json, Map, Value as Json};

/// Most history entries in one body. A record with more is served without its
/// history (`historyOmitted: "too_large"`); its client checks the record and the
/// summary only and says so.
pub const MAX_HISTORY: u64 = 1024;
/// Most commitments in a `chain` (the commitments after a client's `since`).
pub const MAX_CHAIN: usize = 4096;
/// Largest body a client parses (review TR-S-R1-5): the largest body a server
/// builds ([`MAX_HISTORY`] entries with proofs and a full chain) is about 3.5 MB
/// (tested); anything past this is refused before parsing.
pub const MAX_BODY_BYTES: usize = 8 << 20;

/// Top-level keys of a body, in order. A client refuses any other key (fail
/// closed: a server that adds a field the client cannot check is refused).
pub const KEYS: [&str; 12] = [
    "level",
    "example",
    "source",
    "metric",
    "asOf",
    "commitmentHash",
    "commitment",
    "params",
    "record",
    "summary",
    "history",
    "chain",
];

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn borsh_hex<T: borsh::BorshSerialize>(t: &T) -> String {
    hex0x(&borsh::to_vec(t).unwrap_or_default())
}

fn proof_json(p: &Proof) -> Json {
    json!({
        "index": p.index,
        "leafCount": p.leaf_count,
        "siblings": p.siblings.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    })
}

fn proven<T: borsh::BorshSerialize>(row: &T, p: &Proof) -> Json {
    json!({ "row": borsh_hex(row), "proof": proof_json(p) })
}

/// What a server serves a record from: one computation, its commitment and
/// params, and (when the client asked with `since`) the commitments after
/// `since`, oldest first, ending with `commitment`.
pub struct Served<'a> {
    /// The computation.
    pub book: &'a TrackBook,
    /// Its commitment (the latest the server has).
    pub commitment: &'a TrackCommitment,
    /// Its params (`commitment.params_hash`).
    pub params: &'a TrackParams,
    /// With `since`: the commitments after it, ending with `commitment`.
    pub chain: Option<&'a [TrackCommitment]>,
}

/// The body for (`source`, `metric`), or `None` when the book has no such
/// record. The history is left out (`null`) past [`MAX_HISTORY`] entries.
pub fn record_body(s: &Served<'_>, source: &SourceId, metric: &MetricId) -> Option<Json> {
    let (ri, r) = s.book.record(source, metric)?;
    let rp = s.book.record_proof(ri)?;
    let (mi, m) = s.book.metric(metric)?;
    let mp = s.book.metric_proof(mi)?;
    let history = if r.entries_count <= MAX_HISTORY {
        let entry = |k: u64| -> Option<Json> {
            let e = s.book.entries().get(usize::try_from(k).ok()?)?;
            Some(proven(e, &s.book.entry_proof(k)?))
        };
        let end = r.entries_from.checked_add(r.entries_count)?;
        let entries: Option<Vec<Json>> = (r.entries_from..end).map(entry).collect();
        let before = match r.entries_from.checked_sub(1) {
            Some(k) => entry(k)?,
            None => Json::Null,
        };
        let after = if end < s.commitment.entry_count {
            entry(end)?
        } else {
            Json::Null
        };
        json!({ "entries": entries?, "before": before, "after": after })
    } else {
        Json::Null
    };
    Some(json!({
        "level": 5,
        "example": is_example_id(&source.0) || is_example_id(&metric.0),
        "source": source.0,
        "metric": metric.0,
        "asOf": s.commitment.as_of,
        "commitmentHash": s.commitment.hash().to_string(),
        "commitment": borsh_hex(s.commitment),
        "params": borsh_hex(s.params),
        "record": proven(r, &rp),
        "summary": proven(m, &mp),
        "history": history,
        "chain": s.chain.map(|c| c.iter().map(borsh_hex).collect::<Vec<_>>()),
    }))
}

/// What a client asks for and will accept.
#[derive(Clone, Debug)]
pub struct Expect<'a> {
    /// The source it asked for.
    pub source: &'a str,
    /// The metric it asked for.
    pub metric: &'a str,
    /// Its freshness floor: a commitment older than this is refused (`TooOld`).
    pub min_as_of: u64,
    /// A commitment it saw before (it asked with `?since=` this hash): the
    /// served commitment must chain back to it.
    pub known: Option<Hash32>,
    /// The latest commitment of the record's on-chain log, read by the client
    /// at a finalized block (`OnyxSignedBook.trackHead`, ADR-0036 amendment 3):
    /// the served commitment must be exactly it (THREATS TR-R2-5). `None`
    /// where the client cannot read the chain (the browser today, the example).
    pub latest: Option<Hash32>,
}

/// A record that passed [`check_record_body`].
#[derive(Clone, Debug)]
pub struct Checked {
    /// The served commitment.
    pub commitment: TrackCommitment,
    /// Its params.
    pub params: TrackParams,
    /// The record.
    pub record: Record,
    /// Its metric's summary.
    pub summary: MetricSummary,
    /// The record's proof under `records_root`.
    pub record_proof: Proof,
    /// The summary's proof under `metrics_root`.
    pub summary_proof: Proof,
    /// Its committed entries (empty when the history was not served).
    pub entries: Vec<Entry>,
    /// True when the whole history was served and recounted.
    pub history_checked: bool,
    /// True for the example (the source or the metric is an example id).
    pub example: bool,
}

/// Why a client refused a body.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// Not the documented shape (a missing or extra key, bad hex or Borsh).
    #[error("malformed body: {0}")]
    Malformed(&'static str),
    /// A record of another source or metric than asked.
    #[error("not the record asked for")]
    WrongRecord,
    /// The `example` flag does not match the ids (a real record called an
    /// example, or an example served as real).
    #[error("example flag does not match the ids")]
    ExampleFlag,
    /// The commitment does not hash to `commitmentHash`, its `asOf` differs, or
    /// the params do not hash to its `params_hash`.
    #[error("commitment or params do not match")]
    Commitment,
    /// The commitment is older than the client's floor.
    #[error("commitment as of {as_of} is older than the floor {min_as_of}")]
    TooOld {
        /// The served `as_of`.
        as_of: u64,
        /// The client's floor.
        min_as_of: u64,
    },
    /// With `known`: no chain served, or it does not lead by hash from the
    /// known commitment to the served one with `as_of` rising (a stale
    /// commitment, or one not chained to it). A fork chained to the known one
    /// over a rewritten log passes (THREATS TR-S-14).
    #[error("the served commitment does not chain back to the known one")]
    NotDescendant,
    /// With `latest`: the served commitment is not the latest of its
    /// on-chain log (an older one, or one the log does not hold).
    #[error("the served commitment is not the latest on chain")]
    NotLatest,
    /// A proof or the recount failed.
    #[error("proof: {0}")]
    Proof(#[from] VerifyError),
}

fn parse_hex(v: &Json, what: &'static str) -> Result<Vec<u8>, WireError> {
    let s = v.as_str().ok_or(WireError::Malformed(what))?;
    let d = s.strip_prefix("0x").ok_or(WireError::Malformed(what))?;
    if d.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(WireError::Malformed(what));
    }
    hex::decode(d).map_err(|_| WireError::Malformed(what))
}

fn parse_h32(v: &Json, what: &'static str) -> Result<Hash32, WireError> {
    let b: [u8; 32] = parse_hex(v, what)?
        .try_into()
        .map_err(|_| WireError::Malformed(what))?;
    Ok(Hash32(b))
}

fn parse_borsh<T: BorshDeserialize>(v: &Json, what: &'static str) -> Result<T, WireError> {
    borsh::from_slice(&parse_hex(v, what)?).map_err(|_| WireError::Malformed(what))
}

fn obj<'a>(
    v: &'a Json,
    keys: &[&str],
    what: &'static str,
) -> Result<&'a Map<String, Json>, WireError> {
    let o = v.as_object().ok_or(WireError::Malformed(what))?;
    if o.len() != keys.len() || !keys.iter().all(|k| o.contains_key(*k)) {
        return Err(WireError::Malformed(what));
    }
    Ok(o)
}

fn parse_proof(v: &Json) -> Result<Proof, WireError> {
    let o = obj(v, &["index", "leafCount", "siblings"], "proof")?;
    let n = |k: &str| o[k].as_u64().ok_or(WireError::Malformed("proof"));
    let siblings = o["siblings"]
        .as_array()
        .ok_or(WireError::Malformed("proof"))?
        .iter()
        .map(|s| parse_h32(s, "proof sibling"))
        .collect::<Result<Vec<_>, _>>()?;
    // A proof has at most one sibling per level of a 2^64-leaf tree.
    if siblings.len() > 64 {
        return Err(WireError::Malformed("proof"));
    }
    Ok(Proof {
        index: n("index")?,
        leaf_count: n("leafCount")?,
        siblings,
    })
}

fn parse_proven<T: BorshDeserialize>(
    v: &Json,
    what: &'static str,
) -> Result<(T, Proof), WireError> {
    let o = obj(v, &["row", "proof"], what)?;
    Ok((parse_borsh(&o["row"], what)?, parse_proof(&o["proof"])?))
}

/// Check a served body against what the client asked (`expect`), in this
/// order: shape, the record asked for, the example flag, the commitment and
/// params, the freshness floor, the chain back to `known`, then the proofs
/// and (with the history) the recount.
pub fn check_record_body(body: &Json, expect: &Expect<'_>) -> Result<Checked, WireError> {
    let o = obj(body, &KEYS, "body")?;
    if o["level"].as_u64() != Some(5) {
        return Err(WireError::Malformed("level"));
    }
    if o["source"].as_str() != Some(expect.source) || o["metric"].as_str() != Some(expect.metric) {
        return Err(WireError::WrongRecord);
    }
    let example = is_example_id(expect.source) || is_example_id(expect.metric);
    if o["example"].as_bool() != Some(example) {
        return Err(WireError::ExampleFlag);
    }
    let c: TrackCommitment = parse_borsh(&o["commitment"], "commitment")?;
    let params: TrackParams = parse_borsh(&o["params"], "params")?;
    if c.hash() != parse_h32(&o["commitmentHash"], "commitmentHash")?
        || o["asOf"].as_u64() != Some(c.as_of)
        || params.hash() != c.params_hash
    {
        return Err(WireError::Commitment);
    }
    if expect.latest.is_some_and(|l| l != c.hash()) {
        return Err(WireError::NotLatest);
    }
    if c.as_of < expect.min_as_of {
        return Err(WireError::TooOld {
            as_of: c.as_of,
            min_as_of: expect.min_as_of,
        });
    }
    match (expect.known, &o["chain"]) {
        (None, Json::Null) => {}
        (None, _) => return Err(WireError::Malformed("chain")),
        (Some(k), Json::Array(chain)) => {
            if chain.len() > MAX_CHAIN {
                return Err(WireError::Malformed("chain"));
            }
            let mut last = k;
            let mut last_as_of: Option<u64> = None;
            for v in chain {
                let link: TrackCommitment = parse_borsh(v, "chain")?;
                if link.prev != last || last_as_of.is_some_and(|t| link.as_of <= t) {
                    return Err(WireError::NotDescendant);
                }
                last = link.hash();
                last_as_of = Some(link.as_of);
            }
            if last != c.hash() {
                return Err(WireError::NotDescendant);
            }
        }
        (Some(_), _) => return Err(WireError::NotDescendant),
    }
    let (record, rp): (Record, Proof) = parse_proven(&o["record"], "record")?;
    let (summary, mp): (MetricSummary, Proof) = parse_proven(&o["summary"], "summary")?;
    if record.source != SourceId(expect.source.into())
        || record.metric != MetricId(expect.metric.into())
    {
        return Err(WireError::WrongRecord);
    }
    let mut entries = Vec::new();
    let history_checked = match &o["history"] {
        Json::Null => {
            // Served without its history only when it is too large to serve.
            if record.entries_count <= MAX_HISTORY {
                return Err(WireError::Malformed("history"));
            }
            if !verify_record(&c, (&record, &rp)) {
                return Err(VerifyError::Record.into());
            }
            if !verify_summary(&c, (&summary, &mp))
                || summary.metric != record.metric
                || summary.sources != record.metric_sources
                || summary.resolved_facts != record.metric_resolved_facts
            {
                return Err(VerifyError::Summary.into());
            }
            false
        }
        h => {
            let h = obj(h, &["entries", "before", "after"], "history")?;
            let list = h["entries"]
                .as_array()
                .ok_or(WireError::Malformed("history"))?;
            if list.len() as u64 > MAX_HISTORY {
                return Err(WireError::Malformed("history"));
            }
            let rows = list
                .iter()
                .map(|v| parse_proven::<Entry>(v, "entry"))
                .collect::<Result<Vec<_>, _>>()?;
            let side = |v: &Json| -> Result<Option<(Entry, Proof)>, WireError> {
                match v {
                    Json::Null => Ok(None),
                    v => parse_proven(v, "neighbour").map(Some),
                }
            };
            let before = side(&h["before"])?;
            let after = side(&h["after"])?;
            let es: Vec<(&Entry, &Proof)> = rows.iter().map(|(e, p)| (e, p)).collect();
            verify_history(
                &c,
                (&record, &rp),
                (&summary, &mp),
                &es,
                before.as_ref().map(|(e, p)| (e, p)),
                after.as_ref().map(|(e, p)| (e, p)),
            )?;
            entries = rows.into_iter().map(|(e, _)| e).collect();
            true
        }
    };
    Ok(Checked {
        commitment: c,
        params,
        record,
        summary,
        record_proof: rp,
        summary_proof: mp,
        entries,
        history_checked,
        example,
    })
}

#[path = "wire_log.rs"]
mod log;
pub use log::*;

#[path = "wire_paged.rs"]
mod paged;
pub use paged::*;

/// The params' resolver levels as numbers (for display; never a level for a value).
pub fn resolver_level_numbers(params: &TrackParams) -> Vec<u8> {
    params.resolver_levels.iter().map(|l| l.number()).collect()
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
