//! The served logs and history pages (ROADMAP C20; ADR-0036 amendment 5;
//! docs/spec/delivery.md "Track records").
//!
//! - `GET /v1/track/log[?since=]` serves a [`Publication`]'s logs with the
//!   commitments that cover them ([`log_body`]); a client checks them with
//!   [`check_log_body`]: every served commitment's log hashes to it and
//!   extends the one before (`check_extends`, so a rewrite chained to a
//!   commitment the client knew is refused, THREATS TR-S-14), and, with the
//!   registry, a rerun of [`compute`] gives the served commitment and the
//!   intake's batches give its statement log ([`verify_log`], TR-S-13).
//! - `GET /v1/track/{source}/{metric}/history?at=&from=` serves one page of a
//!   history too large for one body ([`history_page_body`]); a client checks
//!   each page ([`check_history_page`]) and recounts the record over all of
//!   them ([`complete_history`]).

use super::{
    borsh_hex, obj, parse_borsh, parse_h32, parse_proven, proven, Checked, Served, WireError,
    MAX_CHAIN, MAX_HISTORY,
};
use crate::example::is_example_id;
use crate::intake::{verify_log, IntakeError, KeyLives, PublishedIntake};
use crate::publication::{
    check_intake, check_steps, log_check_cost, Publication, PublicationError, Step, MAX_LOG_CHECK,
};
use crate::{
    compute, verify_history, Entry, Resolution, Statement, TrackCommitment, TrackError, TrackParams,
};
use onyx_core::{Hash32, MetricId, Registry, SourceId, SourceKey};
use onyx_merkle::Proof;
use serde_json::{json, Value as Json};

/// Most rows (statements, resolutions and revealed intake rows) one format-1
/// log body carries; a server answers 413 `track_log_too_large` beyond.
/// Chained (format-2) logs are served in pages instead (ROADMAP C21,
/// `wire_paged.rs`).
pub const MAX_LOG_ROWS: u64 = 16_384;
/// Largest log body a client parses: [`MAX_LOG_ROWS`] intake rows with
/// 16-sibling proofs as hex are about 30 MB.
pub const MAX_LOG_BODY_BYTES: usize = 48 << 20;
/// Top-level keys of a log body (exact, fail closed).
pub const LOG_KEYS: [&str; 8] = [
    "level",
    "example",
    "asOf",
    "commitmentHash",
    "links",
    "statements",
    "resolutions",
    "intake",
];
/// Top-level keys of a history page (exact, fail closed).
pub const PAGE_KEYS: [&str; 8] = [
    "level",
    "source",
    "metric",
    "commitmentHash",
    "from",
    "entries",
    "before",
    "after",
];

/// True for the example's publication: every rule names an example metric.
pub fn log_is_example(params: &TrackParams) -> bool {
    !params.rules.is_empty() && params.rules.iter().all(|r| is_example_id(&r.metric.0))
}

/// The log body of `p` from step `from` (the step a client named with
/// `since`, or the latest) to the latest. `None` without a step there.
pub fn log_body(p: &Publication, from: usize) -> Option<Json> {
    if p.is_chained() {
        return None;
    }
    let last = p.latest()?;
    let links = p.steps.get(from..).filter(|l| !l.is_empty())?;
    Some(json!({
        "level": 5,
        "example": log_is_example(&last.params),
        "asOf": last.commitment.as_of,
        "commitmentHash": last.commitment.hash().to_string(),
        "links": links.iter().map(borsh_hex).collect::<Vec<_>>(),
        "statements": borsh_hex(&p.statements),
        "resolutions": borsh_hex(&p.resolutions),
        "intake": p.intake.as_ref().map(borsh_hex),
    }))
}

/// The latest commitment a log body or chained head names (its last link),
/// unchecked: a client fetches and checks the registry it names
/// (`check_registry_body`) before checking the logs with it, and the log check
/// then requires that commitment again (`Commitment`, `RegistryDiffers`).
pub fn peek_commitment(body: &Json) -> Option<TrackCommitment> {
    let last = body.get("links")?.as_array()?.last()?;
    parse_borsh::<Step>(last, "link").ok().map(|s| s.commitment)
}

/// What a client asks of a log body and brings to check it.
#[derive(Clone, Copy, Default)]
pub struct LogExpect<'a> {
    /// Its freshness floor (`TooOld` below it).
    pub min_as_of: u64,
    /// A commitment it saw before (it asked with `?since=`): the served links
    /// must include it, so the logs are checked to extend from it.
    pub known: Option<Hash32>,
    /// The latest commitment the client trusts: the record's on-chain log
    /// head when it reads the chain, or the commitment of the record body it
    /// checked (`NotLatest` otherwise). A client that checks a node's logs
    /// sets it (review C21-R2-1): unset, logs a node made up check.
    pub latest: Option<Hash32>,
    /// The registry at the commitment's `registry_root`: with it the client
    /// reruns [`compute`] (TR-S-13); without it only the hashes and the
    /// extension are checked.
    pub registry: Option<&'a dyn Registry>,
    /// The level-3 registry's reviewed keys: with them (and the registry) the
    /// client runs [`verify_log`] on the served intake.
    pub reviewed: Option<&'a [SourceKey]>,
    /// One key-status list per post, read by the client from the signed-source
    /// book (required for [`verify_log`] in chain mode).
    pub lives: Option<&'a [KeyLives]>,
}

/// Logs that passed [`check_log_body`].
#[derive(Clone, Debug)]
pub struct CheckedLog {
    /// The latest served commitment.
    pub commitment: TrackCommitment,
    /// Its params.
    pub params: TrackParams,
    /// Its statement log, canonical.
    pub statements: Vec<Statement>,
    /// Its resolution list, canonical.
    pub resolutions: Vec<Resolution>,
    /// Commitments checked as a chain of extensions (1: the latest only).
    pub links: usize,
    /// A rerun of [`compute`] on the logs gave the served commitment.
    pub recomputed: bool,
    /// [`verify_log`] passed on the served intake.
    pub intake_checked: bool,
    /// The params name an intake (so `intake_checked` false means not checked).
    pub has_intake: bool,
    /// The params pin a chain: the resolutions are the server's word until
    /// the client reruns `chain::verify_resolutions` against its own node.
    pub resolutions_from_chain: bool,
    /// The example's publication.
    pub example: bool,
}

/// Why a client refused a log body.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LogError {
    /// Not the documented shape.
    #[error("malformed log body: {0}")]
    Malformed(&'static str),
    /// The `example` flag does not match the params.
    #[error("example flag does not match the params")]
    ExampleFlag,
    /// The latest link does not hash to `commitmentHash` or its `asOf` differs.
    #[error("commitment does not match")]
    Commitment,
    /// Older than the client's floor.
    #[error("commitment as of {as_of} is older than the floor {min_as_of}")]
    TooOld {
        /// The served `as_of`.
        as_of: u64,
        /// The client's floor.
        min_as_of: u64,
    },
    /// Not the on-chain latest.
    #[error("the served commitment is not the latest the client trusts")]
    NotLatest,
    /// The links do not include the commitment the client knew.
    #[error("the served logs do not start at or before the known commitment")]
    NotDescendant,
    /// A link's log does not hash to it, or does not extend the one before.
    #[error(transparent)]
    Steps(#[from] PublicationError),
    /// The client's registry is not the one the commitment names.
    #[error("the commitment was computed with another registry")]
    RegistryDiffers,
    /// The inputs are refused by [`compute`].
    #[error("the logs are not valid inputs: {0}")]
    Compute(#[from] TrackError),
    /// A rerun of [`compute`] gives another book than the commitment's.
    #[error("a rerun on the served logs gives another commitment")]
    Recompute,
    /// The intake does not give the statement log.
    #[error("intake: {0}")]
    Intake(#[from] IntakeError),
    /// More rows than [`MAX_LOG_ROWS`], or more checking than [`MAX_LOG_CHECK`]
    /// (links times rows): refused before any hashing (review C20-R1-3).
    #[error("the log body is past the client's limits")]
    TooLarge,
    /// A served registry that does not rebuild to the commitment's
    /// `registry_root` with the client's own rules (ROADMAP C23).
    #[error("registry: {0}")]
    Registry(#[from] onyx_core::RegistryDataError),
}

fn malformed(what: &'static str) -> impl Fn(WireError) -> LogError {
    move |_| LogError::Malformed(what)
}

/// Check a log body, in this order: shape, the latest commitment and the
/// example flag, the pin (`latest`), the floor, `known` among the links, every
/// link's log and the extension between links ([`check_steps`]), the intake's
/// presence; then, with the registry, a rerun of [`compute`]; with the reviewed
/// keys too, [`verify_log`] on the intake.
pub fn check_log_body(body: &Json, expect: &LogExpect<'_>) -> Result<CheckedLog, LogError> {
    let o = obj(body, &LOG_KEYS, "log body").map_err(malformed("log body"))?;
    if o["level"].as_u64() != Some(5) {
        return Err(LogError::Malformed("level"));
    }
    let links = o["links"]
        .as_array()
        .filter(|l| !l.is_empty() && l.len() <= MAX_CHAIN + 1)
        .ok_or(LogError::Malformed("links"))?
        .iter()
        .map(|v| parse_borsh::<Step>(v, "link"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(malformed("link"))?;
    let statements: Vec<Statement> =
        parse_borsh(&o["statements"], "statements").map_err(malformed("statements"))?;
    let resolutions: Vec<Resolution> =
        parse_borsh(&o["resolutions"], "resolutions").map_err(malformed("resolutions"))?;
    let intake: Option<PublishedIntake> = match &o["intake"] {
        Json::Null => None,
        v => Some(parse_borsh(v, "intake").map_err(malformed("intake"))?),
    };
    let rows = (statements.len() as u64).saturating_add(resolutions.len() as u64);
    let all = rows.saturating_add(intake.as_ref().map_or(0, PublishedIntake::revealed_rows));
    if all > MAX_LOG_ROWS || log_check_cost(links.len(), rows) > MAX_LOG_CHECK {
        return Err(LogError::TooLarge);
    }
    let last = links.last().ok_or(LogError::Malformed("links"))?;
    let c = &last.commitment;
    let hash =
        parse_h32(&o["commitmentHash"], "commitmentHash").map_err(malformed("commitmentHash"))?;
    if c.hash() != hash || o["asOf"].as_u64() != Some(c.as_of) {
        return Err(LogError::Commitment);
    }
    let example = log_is_example(&last.params);
    if o["example"].as_bool() != Some(example) {
        return Err(LogError::ExampleFlag);
    }
    if expect.latest.is_some_and(|l| l != hash) {
        return Err(LogError::NotLatest);
    }
    if c.as_of < expect.min_as_of {
        return Err(LogError::TooOld {
            as_of: c.as_of,
            min_as_of: expect.min_as_of,
        });
    }
    if let Some(k) = expect.known {
        if !links.iter().any(|l| l.commitment.hash() == k) {
            return Err(LogError::NotDescendant);
        }
    }
    let (s, r) = check_steps(&links, &statements, &resolutions)?;
    rerun(
        last,
        intake.as_ref(),
        s,
        r,
        links.len(),
        example,
        false,
        expect,
    )
}

/// The end of a log check, once every link's log is checked (format 1 or 2):
/// the intake's presence; with the registry, a rerun of [`compute`] (with
/// chained logs the two log hashes are the chained ones the stream check
/// already matched, so the rerun compares everything else); with the reviewed
/// keys too, [`verify_log`] on the intake.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rerun(
    last: &Step,
    intake: Option<&PublishedIntake>,
    s: Vec<Statement>,
    r: Vec<Resolution>,
    links: usize,
    example: bool,
    chained: bool,
    expect: &LogExpect<'_>,
) -> Result<CheckedLog, LogError> {
    let c = &last.commitment;
    check_intake(&last.params, c, intake)?;
    let mut recomputed = false;
    let mut intake_checked = false;
    if let Some(reg) = expect.registry {
        let book = compute(&last.params, reg, &s, &r, c.as_of)?;
        let mut again = book.commitment(c.prev);
        if again.registry_root != c.registry_root {
            return Err(LogError::RegistryDiffers);
        }
        if chained {
            again.statements_hash = c.statements_hash;
            again.resolutions_hash = c.resolutions_hash;
        }
        if again != *c {
            return Err(LogError::Recompute);
        }
        recomputed = true;
        let chain_mode = last.params.chain != Hash32::ZERO;
        if let (Some(i), Some(reviewed)) = (intake, expect.reviewed) {
            if !chain_mode || expect.lives.is_some() {
                verify_log(
                    &i.view(reviewed, expect.lives),
                    &last.params,
                    reg,
                    c.as_of,
                    &s,
                )?;
                intake_checked = true;
            }
        }
    }
    Ok(CheckedLog {
        commitment: c.clone(),
        has_intake: intake.is_some(),
        resolutions_from_chain: last.params.chain != Hash32::ZERO,
        params: last.params.clone(),
        statements: s,
        resolutions: r,
        links,
        recomputed,
        intake_checked,
        example,
    })
}

/// One page of a record's history: up to [`MAX_HISTORY`] entries from the
/// record's `from`-th entry, with the entries just outside the page (`null` at
/// the tree's ends). `None` without the record or past its last entry.
pub fn history_page_body(
    s: &Served<'_>,
    source: &SourceId,
    metric: &MetricId,
    from: u64,
) -> Option<Json> {
    let (_, r) = s.book.record(source, metric)?;
    if from >= r.entries_count {
        return None;
    }
    let start = r.entries_from.checked_add(from)?;
    let n = (r.entries_count - from).min(MAX_HISTORY);
    let end = start.checked_add(n)?;
    let entry = |k: u64| -> Option<Json> {
        let e = s.book.entries().get(usize::try_from(k).ok()?)?;
        Some(proven(e, &s.book.entry_proof(k)?))
    };
    let entries: Option<Vec<Json>> = (start..end).map(entry).collect();
    let before = match start.checked_sub(1) {
        Some(k) => entry(k)?,
        None => Json::Null,
    };
    let after = if end < s.commitment.entry_count {
        entry(end)?
    } else {
        Json::Null
    };
    Some(json!({
        "level": 5,
        "source": source.0,
        "metric": metric.0,
        "commitmentHash": s.commitment.hash().to_string(),
        "from": from,
        "entries": entries?,
        "before": before,
        "after": after,
    }))
}

/// A history page that passed [`check_history_page`].
#[derive(Clone, Debug)]
pub struct HistoryPage {
    /// The record's entry it starts at.
    pub from: u64,
    /// Its entries with their proofs.
    pub entries: Vec<(Entry, Proof)>,
    /// The entry just before it.
    pub before: Option<(Entry, Proof)>,
    /// The entry just after it.
    pub after: Option<(Entry, Proof)>,
}

/// Check one page against a record the client checked (`checked`, served
/// without its history) and the `from` it asked: shape, the same record and
/// commitment, entries inside the record's range at the asked indices. The
/// proofs and the recount run in [`complete_history`].
pub fn check_history_page(
    body: &Json,
    checked: &Checked,
    from: u64,
) -> Result<HistoryPage, WireError> {
    let o = obj(body, &PAGE_KEYS, "page")?;
    let r = &checked.record;
    if o["level"].as_u64() != Some(5) {
        return Err(WireError::Malformed("level"));
    }
    if o["source"].as_str() != Some(r.source.0.as_str())
        || o["metric"].as_str() != Some(r.metric.0.as_str())
    {
        return Err(WireError::WrongRecord);
    }
    if parse_h32(&o["commitmentHash"], "commitmentHash")? != checked.commitment.hash() {
        return Err(WireError::Commitment);
    }
    if o["from"].as_u64() != Some(from) {
        return Err(WireError::Malformed("from"));
    }
    let list = o["entries"]
        .as_array()
        .filter(|l| !l.is_empty() && l.len() as u64 <= MAX_HISTORY)
        .ok_or(WireError::Malformed("entries"))?;
    if from
        .checked_add(list.len() as u64)
        .is_none_or(|e| e > r.entries_count)
    {
        return Err(WireError::Malformed("entries"));
    }
    let entries = list
        .iter()
        .map(|v| parse_proven::<Entry>(v, "entry"))
        .collect::<Result<Vec<_>, _>>()?;
    let start = r.entries_from + from;
    for (i, (_, p)) in entries.iter().enumerate() {
        if p.index != start + i as u64 {
            return Err(crate::VerifyError::Entry(start + i as u64).into());
        }
    }
    let side = |v: &Json| -> Result<Option<(Entry, Proof)>, WireError> {
        match v {
            Json::Null => Ok(None),
            v => parse_proven(v, "neighbour").map(Some),
        }
    };
    Ok(HistoryPage {
        from,
        before: side(&o["before"])?,
        after: side(&o["after"])?,
        entries,
    })
}

/// Recount a record over its pages ([`verify_history`] on all of them): the
/// pages must tile the record's range in order from its first entry; the
/// first page's `before` and the last page's `after` close the range. On
/// success `checked` holds every entry and `history_checked` is true.
pub fn complete_history(checked: &mut Checked, pages: &[HistoryPage]) -> Result<(), WireError> {
    let mut next = 0u64;
    for p in pages {
        if p.from != next {
            return Err(WireError::Malformed("pages"));
        }
        next = next
            .checked_add(p.entries.len() as u64)
            .ok_or(WireError::Malformed("pages"))?;
    }
    if next != checked.record.entries_count {
        return Err(WireError::Malformed("pages"));
    }
    let (first, last) = match (pages.first(), pages.last()) {
        (Some(f), Some(l)) => (f, l),
        _ => return Err(WireError::Malformed("pages")),
    };
    let es: Vec<(&Entry, &Proof)> = pages
        .iter()
        .flat_map(|p| p.entries.iter().map(|(e, pr)| (e, pr)))
        .collect();
    verify_history(
        &checked.commitment,
        (&checked.record, &checked.record_proof),
        (&checked.summary, &checked.summary_proof),
        &es,
        first.before.as_ref().map(|(e, p)| (e, p)),
        last.after.as_ref().map(|(e, p)| (e, p)),
    )?;
    checked.entries = es.into_iter().map(|(e, _)| e.clone()).collect();
    checked.history_checked = true;
    Ok(())
}

#[cfg(test)]
#[path = "wire_log_tests.rs"]
mod tests;
