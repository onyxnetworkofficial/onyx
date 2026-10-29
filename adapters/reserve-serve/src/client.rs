//! The client side of the reserve serving check (ROADMAP D3 and D22,
//! THREATS RES-10, ADR-0034 Amendments 2 and 3).
//!
//! A served reserve row's `servingCheck` is the operator's claim: nothing on
//! chain binds a shard's params to the scan, so an operator who skips the
//! check can serve a forged-base row that verifies against its finalized
//! root. A client that does not trust the operator re-runs the check:
//!
//! 1. It pins a committed scan itself ([`ReservePin::from_scan`], or
//!    [`ReservePin::from_dated_scan`] with the dated label set and the flows
//!    file membership changes need; their hashes must be ones the client
//!    trusts, never the server's word). Any later scan it trusts will do: the
//!    check pays only the links between that scan and the answer (D22).
//! 2. It asks `/v1/served/btc.entity-reserve` which shards form the chain
//!    (an untrusted list: [`link_requests`] picks the span between the answer
//!    and its anchor, and refuses a span over the client's budget before any
//!    fetch) and fetches the entity's row in each through
//!    `/v1/proofs/{round}/{shard}/{index}` (paid like any row; the index is
//!    the entity's in that shard's window, from the pin).
//! 3. It verifies every answer on its own node
//!    (`onyx_api::client::ClientNode::verify_row`: root final, shard id and
//!    binding recomputed, row proven under the root).
//! 4. [`check_reserve_answer`] turns each proven row into a [`ReserveLink`]
//!    from the binding alone (recipe, label set, schema, source snapshot) and
//!    runs `reserve::client_check` against the client's own header chain:
//!    every link on its window's label set and on the header chain with six
//!    confirmations, the bases derived from the pinned scan, the links' flows
//!    and the membership changes, and any row on another base refused; the
//!    answer it wanted checked must be one of the links (by root, index and
//!    row bytes) and no older than the client's own `min_as_of`.
//!
//! What it cannot check (a full node's job): that the scan saw every output
//! at the listed addresses (RES-1), and the flows file's per-address split
//! at a membership change (RES-13, the file's word under a hash the client
//! trusts).

use crate::GATE;
use onyx_api::client::{Expect, ProvenRow};
use onyx_core::{Hash32, Snapshot};
use onyx_source_btc::reserve::{
    client_check, ClientVerdict, HeaderChain, ReserveLink, ReservePin, MIN_CONFIRMATIONS,
};
use onyx_source_btc::{CHAIN_ID, SOURCE_ID};
use serde_json::Value;

/// One link to fetch: `/v1/proofs/{round}/{shard}/{index}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    /// Round.
    pub round: u64,
    /// Shard id.
    pub shard: Hash32,
    /// The entity's leaf index (from the pin, not the server).
    pub index: u64,
    /// The range and root the list gave it (checked against the proof:
    /// [`LinkRequest::matches`]).
    pub from: u64,
    /// Last height listed.
    pub to: u64,
    /// Root listed.
    pub root: Hash32,
}

impl LinkRequest {
    /// The path to ask.
    pub fn path(&self) -> String {
        format!("/v1/proofs/{}/{}/{}", self.round, self.shard, self.index)
    }

    /// What the answer must be.
    pub fn expect(&self) -> Expect<'static> {
        Expect::Proof {
            round: self.round,
            shard: self.shard,
            index: self.index,
        }
    }

    /// The proven link is the one the list named: same root and range
    /// (review D22-R1-1: a client fetches in walk order and stops at the
    /// first link the list misdescribed, instead of paying for the rest).
    pub fn matches(&self, p: &ProvenRow) -> Result<(), String> {
        let l = link_of(p)?;
        if (l.root, l.from, l.to) != (self.root, self.from, self.to) {
            return Err(format!(
                "round {} is not the listed blocks {}-{} and root: the list misdescribed it",
                self.round, self.from, self.to
            ));
        }
        Ok(())
    }
}

/// What a client spends and accepts on one check (ROADMAP D22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Most paid rows (links) one check fetches ([`DEFAULT_SPAN_BUDGET`]).
    pub budget: usize,
    /// Oldest last height of an answer the client accepts (its own
    /// headers' clock, never the server's); refused before any fetch.
    pub min_as_of: u64,
    /// Most entries of the free list it parses ([`DEFAULT_MAX_LISTED`]).
    pub max_listed: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            budget: DEFAULT_SPAN_BUDGET,
            min_as_of: 0,
            max_listed: DEFAULT_MAX_LISTED,
        }
    }
}

/// Most entries a client parses from the free `/v1/served` list (review
/// D22-R1-2: a memory bound, not a cost one): 1,048,576, about 225 years of
/// the committed chain's 89 shards a week. Not the old `MAX_LINKS` cliff:
/// the list is unpaid, and a client may raise it. It bounds the body before
/// it is parsed only through [`parse_served`] (review D22-R2-3: a list
/// already parsed into a `Value` has cost its memory).
pub const DEFAULT_MAX_LISTED: usize = 1 << 20;

/// Most bytes one `/v1/served` entry may take in [`parse_served`]: an entry
/// is about 190 bytes (two 0x-prefixed 32-byte hashes, a round and a range).
pub const MAX_LISTED_ENTRY_BYTES: usize = 256;

/// Parse a `/v1/served` body, refusing one longer than `limits.max_listed`
/// entries could be ([`MAX_LISTED_ENTRY_BYTES`] each, plus 1 KiB) before any
/// parsing (review D22-R2-3): the list is free and the operator's, so its
/// size is bounded before it costs the client memory, not after.
pub fn parse_served(body: &[u8], limits: &Limits) -> Result<Value, String> {
    let most = limits
        .max_listed
        .saturating_mul(MAX_LISTED_ENTRY_BYTES)
        .saturating_add(1024);
    if body.len() > most {
        return Err(format!(
            "the served list is {} bytes, more than {} entries take ({most})",
            body.len(),
            limits.max_listed
        ));
    }
    serde_json::from_slice(body).map_err(|e| format!("the served list is not JSON: {e}"))
}

/// A client's default budget for one check, in paid rows (ROADMAP D22):
/// 4,096 rows, 10.24 USDC at 2,500 micro-USDC a row. At the committed
/// chain's pace (89 shards a week) a span that long is an answer about 46
/// weeks from the pinned scan; a client pins a newer scan (or raises its own
/// budget) instead of hitting a cliff: nothing in the check bounds the
/// chain's length.
pub const DEFAULT_SPAN_BUDGET: usize = 4_096;

/// The links to fetch for `entity`, from a `/v1/served/{metric}` answer,
/// to check `answer` (the one-row reserve answer the client wants checked,
/// as the server sent it), at most `budget` of them.
/// The list is the operator's and is not trusted for any value; it is
/// checked for shape before anything is fetched (review D3-R1-2), because
/// each link costs the client a paid row: ranges consecutive from the first,
/// each starting right after the previous one ends; exactly one ending at
/// the pinned scan's height. Shards listed twice over the same range with the
/// same root (the same work finalized in two rounds, review D3-R1-1) are one
/// link: the first is kept; the same range with two roots is refused.
///
/// The answer is placed in the list before the first fetch (review D3-R2-1):
/// its root must be a listed range's and its index `entity`'s in that
/// range's window. ROADMAP D22: only the span from the answer's link to its
/// anchor is fetched: walking toward the landing (the range ending at the
/// pinned scan's height), up to and including it, or up to the last link
/// before one in whose window `entity` has no address (its exit or entry,
/// anchored by the flows file). Every range on the walk must be one window
/// of the pinned set (none across a membership change, none past its
/// publication), and the leaf index of each link is `entity`'s in its
/// window. A span longer than `budget` is refused before any fetch: the cost
/// is the links between the pinned scan and the answer, whatever the
/// chain's length (no list cap; [`DEFAULT_SPAN_BUDGET`]).
/// A list cut short after an old link still passes this and the chain check
/// (the operator chooses how far it lists): the answer's freshness is bound
/// by `min_as_of` in [`check_reserve_answer`].
pub fn link_requests(
    served: &Value,
    pin: &ReservePin,
    entity: &str,
    answer: &Value,
    headers: &HeaderChain,
    limits: &Limits,
) -> Result<Vec<LinkRequest>, String> {
    let budget = limits.budget;
    if served["gate"].as_str() != Some(GATE) {
        return Err(format!("not the {GATE} serving check's list"));
    }
    if !pin.names(entity) {
        return Err(format!("{entity}: not an entity of the pinned label set"));
    }
    // Review D22-R1-1: what the client can refuse for free, before any
    // paid fetch: a stale pin, a list too long to parse.
    pin.check_on(headers)
        .map_err(|e| format!("the pin is not on the client's headers: {e}"))?;
    let list = served["shards"].as_array().ok_or("no shard list")?;
    if list.len() > limits.max_listed {
        return Err(format!(
            "{} entries listed, more than the client parses ({})",
            list.len(),
            limits.max_listed
        ));
    }
    let h32 = |v: &Value| -> Option<Hash32> {
        let mut b = [0u8; 32];
        hex::decode_to_slice(v.as_str()?.strip_prefix("0x")?, &mut b).ok()?;
        Some(Hash32(b))
    };
    let answer_root = h32(&answer["root"]).ok_or("the answer has no root")?;
    // (round, shard, from, to, root) per range, deduplicated.
    let mut ranges: Vec<(u64, Hash32, u64, u64, Hash32)> = Vec::new();
    let mut last: Option<(u64, u64, Hash32)> = None;
    let mut landing: Option<usize> = None;
    let mut answered: Option<usize> = None;
    for s in list {
        let bad = |m: &str| format!("malformed shard list: {m}");
        let round = s["round"].as_u64().ok_or_else(|| bad("round"))?;
        let shard = h32(&s["shard"]).ok_or_else(|| bad("shard"))?;
        let root = h32(&s["root"]).ok_or_else(|| bad("root"))?;
        let from = s["from"].as_u64().ok_or_else(|| bad("from"))?;
        let to = s["to"].as_u64().ok_or_else(|| bad("to"))?;
        if to < from {
            return Err(bad("a range ends before it starts"));
        }
        match last {
            Some((f, t, r)) if (f, t) == (from, to) => {
                if r != root {
                    return Err(format!(
                        "two roots listed for blocks {from}-{to}: not one chain"
                    ));
                }
                continue;
            }
            Some((_, t, _)) if t.checked_add(1) != Some(from) => {
                return Err(format!(
                    "blocks {from}-{to} do not start right after the previous range"
                ));
            }
            _ => {}
        }
        if to == pin.scan_height {
            if landing.is_some() {
                return Err(format!(
                    "two ranges listed ending at the pinned scan's height {}",
                    pin.scan_height
                ));
            }
            landing = Some(ranges.len());
        }
        if root == answer_root {
            answered = Some(ranges.len());
        }
        last = Some((from, to, root));
        ranges.push((round, shard, from, to, root));
    }
    let Some(landing) = landing else {
        return Err(format!(
            "the list has no range ending at the pinned scan's height {}",
            pin.scan_height
        ));
    };
    let Some(answered) = answered else {
        return Err("the answer's root is not a listed range's: not a link of the chain".into());
    };
    // Review D22-R1-1: an answer older than `min_as_of`, or one the
    // client's headers cannot confirm six deep yet, is refused before any
    // fetch (the check would refuse it after paying for the span).
    let answer_to = ranges[answered].3;
    if answer_to < limits.min_as_of {
        return Err(format!(
            "the answer ends at block {answer_to}, before min_as_of {}",
            limits.min_as_of
        ));
    }
    if answer_to.saturating_add(MIN_CONFIRMATIONS - 1) > headers.tip() {
        return Err(format!(
            "the answer ends at block {answer_to}: not {MIN_CONFIRMATIONS} confirmations deep on the client's headers (tip {})",
            headers.tip()
        ));
    }
    let mut windows = pin.windows();
    let mut index_at = |k: usize| -> Result<Option<u64>, String> {
        let (_, _, from, to, _) = ranges[k];
        if to > pin.labels.published {
            return Err(format!(
                "blocks {from}-{to} reach past the label set's publication at {}",
                pin.labels.published
            ));
        }
        let w = windows
            .window(from, to)
            .map_err(|m| format!("blocks {from}-{to}: {m}"))?;
        Ok(w.index_of(entity))
    };
    let index = index_at(answered)?;
    if index.is_none() || answer["index"].as_u64() != index {
        return Err(format!("the answer is not {entity}'s row (leaf index)"));
    }
    // Walk from the answer toward the landing; stop at it, or before a
    // range where the entity has no address (its exit or entry anchors the
    // span instead).
    let mut span: Vec<(usize, u64)> = Vec::new();
    let step: isize = if answered <= landing { 1 } else { -1 };
    let mut k = answered;
    while let Some(i) = index_at(k)? {
        span.push((k, i));
        if k == landing || span.len() > budget {
            break;
        }
        k = k.wrapping_add_signed(step);
    }
    if span.len() > budget {
        return Err(format!(
            "the span between the answer and its anchor is more than the client's budget of {budget} links"
        ));
    }
    span.sort_unstable();
    Ok(span
        .into_iter()
        .map(|(k, index)| LinkRequest {
            round: ranges[k].0,
            shard: ranges[k].1,
            index,
            from: ranges[k].2,
            to: ranges[k].3,
            root: ranges[k].4,
        })
        .collect())
}

/// A proven row as a [`ReserveLink`], from what the binding commits only:
/// the recipe, label set and schema ids it recomputed, and the source
/// reference whose hash is in the binding (a Bitcoin block range on the
/// Bitcoin source).
pub fn link_of(p: &ProvenRow) -> Result<ReserveLink, String> {
    let v = p.verified();
    if !v.chain_checked {
        return Err("the root was not checked on the client's own node".into());
    }
    let Snapshot::BlockRange {
        chain,
        from_height,
        to_height,
        tip_hash,
        ..
    } = &p.source().snapshot
    else {
        return Err("not a block-range shard".into());
    };
    if p.source().source.0 != SOURCE_ID || chain.0 != CHAIN_ID {
        return Err("not a Bitcoin block range".into());
    }
    Ok(ReserveLink {
        round: v.round,
        shard: v.shard,
        root: v.root,
        recipe: p.inputs().recipe_id,
        label_set_hash: p.inputs().label_set_hash,
        schema: p.inputs().schema_id,
        from: *from_height,
        to: *to_height,
        tip: *tip_hash,
        index: p.index(),
        leaf: p.row().to_vec(),
    })
}

/// The client's reserve serving check of `answer`, a one-row reserve
/// answer body as the server sent it (`/v1/metrics` or `/v1/proofs`).
/// `links` are the entity's rows in the span [`link_requests`] picked,
/// oldest first, each verified on the client's own node
/// (`ClientNode::verify_row` on the [`link_requests`] answers); `pin` and
/// `headers` (proof-of-work-checked, ROADMAP D22) are the client's own;
/// `min_as_of` is the oldest last height the client accepts (as
/// `Expect::Metric`'s, PLAN q14 a: one answer cannot prove it is the
/// newest, review D3-R1-3).
///
/// Refused unless the span is bound to the pin and the header chain, is
/// anchored (the landing on the scan, or the entity's exit or entry) and
/// derives every base from it, and `answer` holds a link's row: the same
/// root, leaf index and row bytes as a proven link (the same root is the
/// same rows, so a duplicate shard of that work in another round is the
/// same answer, review D3-R1-1), ending at or after `min_as_of`. A
/// `/v1/metrics` reserve answer cannot be checked with `Expect::Metric`
/// itself (each shard's params hash carries its base); equality with a
/// proven link binds it instead.
pub fn check_reserve_answer(
    answer: &Value,
    links: &[ProvenRow],
    pin: &ReservePin,
    headers: &HeaderChain,
    min_as_of: u64,
) -> Result<ClientVerdict, String> {
    let chain = links.iter().map(link_of).collect::<Result<Vec<_>, _>>()?;
    let hex_of =
        |v: &Value| -> Option<Vec<u8>> { hex::decode(v.as_str()?.strip_prefix("0x")?).ok() };
    let (Some(root), Some(index), Some(row)) = (
        hex_of(&answer["root"]),
        answer["index"].as_u64(),
        hex_of(&answer["row"]),
    ) else {
        return Err("not a one-row answer (root, index, row)".into());
    };
    let Some(a) = chain
        .iter()
        .find(|l| l.root.0[..] == root[..] && l.index == index && l.leaf == row)
    else {
        return Err(
            "the answered row is not a link of the chain that lands on the pinned scan".into(),
        );
    };
    if a.to < min_as_of {
        return Err(format!(
            "the answer ends at block {}, before min_as_of {min_as_of}",
            a.to
        ));
    }
    let mut windows = pin.windows();
    let entity = windows
        .window(a.from, a.to)
        .map_err(|m| format!("the answer's blocks {}-{}: {m}", a.from, a.to))?
        .entities
        .get(usize::try_from(a.index).map_err(|_| "index")?)
        .cloned()
        .ok_or("the answer's index is not an entity of the pinned label set there")?;
    client_check(pin, &entity, &chain, headers).map_err(|e| e.to_string())
}
