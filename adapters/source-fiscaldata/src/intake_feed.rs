//! One approved submission through the real level-2 pipeline (PRODUCT-PLAN.md U4, ADR-0039).
//!
//! A reviewer's approval of a submission does not verify anything (ADR-0039). This module is the
//! next step for the one source the intake may pull, U.S. Treasury Fiscal Data "Debt to the
//! Penny": it turns an approved submission into this adapter's pinned feed and runs the existing
//! observer round ([`crate::round::run`]) on it, so the checked value comes from bonded observers,
//! not from the submitter.
//!
//! Refused before any round runs: a submission that is not `approved`; an endpoint other than the
//! adapter's pinned endpoint with exactly one `filter=record_date:eq:YYYY-MM-DD` (and, at most,
//! the pinned `fields` list); a different JSON pointer; and a rights answer other than public
//! domain or open licence (the adapter's own reviewed rights govern the fetch, not the answer). The submitter's URL can
//! never widen what the round fetches: the round fetches [`crate::record_url`] of the record date.
//! A rerun with the same round inputs gives the same round id and the same result, so a retry
//! cannot record a second, different value.

use crate::round::{run, Page, RoundRun, RunError};
use crate::{record_url, ENDPOINT, SELECTOR};
use onyx_api::submit::review::ReviewItem;
use onyx_coordinator::observe::{DrawChain, ObservationOutcome, ObservationResults};
use onyx_core::Hash32;

/// Why an approved submission does not become a round.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    /// Only an approved submission is fed.
    #[error("the submission is {0}, not approved")]
    NotApproved(String),
    /// The submission names no endpoint.
    #[error("the submission has no endpoint")]
    NoEndpoint,
    /// The endpoint is not the adapter's pinned endpoint for one record date.
    #[error("the endpoint is not the pinned Debt to the Penny endpoint for one record date")]
    NotThePinnedFeed,
    /// The JSON pointer is not the adapter's field.
    #[error("the JSON pointer is not {SELECTOR}")]
    OtherField,
    /// The rights answer is neither public domain nor an open licence.
    #[error("the rights answer is neither public domain nor an open licence")]
    Rights,
    /// The round itself failed.
    #[error("round: {0}")]
    Round(String),
}

/// What an approved submission became: the round's checked result, tied to the receipt.
#[derive(Debug)]
pub struct FedRound {
    /// The submission receipt id.
    pub submission_id: String,
    /// The submission content hash the reviewer approved.
    pub submission_hash: String,
    /// The record date the round observed.
    pub record_date: String,
    /// The round id the observers committed to.
    pub round_id: Hash32,
    /// The settled result hash (the evidence a client checks, ADR-0027).
    pub result: Hash32,
    /// The value the observers agreed on, as the feed's decimal text.
    pub value: String,
    /// The full round, for the results store.
    pub run: RoundRun,
}

/// The record date an approved Treasury submission asks for, or why it is refused.
pub fn approved_record_date(item: &ReviewItem) -> Result<String, FeedError> {
    if item.status != "approved" {
        return Err(FeedError::NotApproved(item.status.clone()));
    }
    let s = &item.submission;
    let endpoint = s.endpoint_url.as_deref().ok_or(FeedError::NoEndpoint)?;
    if s.json_pointer.as_deref() != Some(SELECTOR) {
        return Err(FeedError::OtherField);
    }
    if !matches!(
        s.answers.rights,
        onyx_intake::Rights::PublicDomain | onyx_intake::Rights::OpenLicense
    ) {
        return Err(FeedError::Rights);
    }
    // The record date is the one thing the submitter chooses; everything else must be the pin.
    let query = endpoint
        .strip_prefix(ENDPOINT)
        .and_then(|q| q.strip_prefix('?'))
        .ok_or(FeedError::NotThePinnedFeed)?;
    let mut date = None;
    for part in query.split('&') {
        match part.split_once('=') {
            Some(("filter", f)) if date.is_none() => {
                date = f.strip_prefix("record_date:eq:").filter(|d| is_date(d));
                if date.is_none() {
                    return Err(FeedError::NotThePinnedFeed);
                }
            }
            Some(("fields", PINNED_FIELDS)) => {}
            _ => return Err(FeedError::NotThePinnedFeed),
        }
    }
    let date = date.ok_or(FeedError::NotThePinnedFeed)?;
    debug_assert!(record_url(date).starts_with(ENDPOINT));
    Ok(date.to_string())
}

/// The `fields` list of the pinned URL ([`crate::url_template`]).
const PINNED_FIELDS: &str = "record_date,tot_pub_debt_out_amt";

fn is_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Run the pinned observer round for an approved submission. `pages` are the observers' own
/// fetches of [`crate::record_url`] (recorded fetches in tests and rehearsals).
pub fn feed_approved(
    item: &ReviewItem,
    window_start: u64,
    pages: &[Page],
    store: &mut ObservationResults,
    chain: &mut dyn DrawChain,
) -> Result<FedRound, FeedError> {
    let record_date = approved_record_date(item)?;
    let r: RoundRun = run(&record_date, window_start, pages, store, chain)
        .map_err(|e: RunError| FeedError::Round(e.to_string()))?;
    let value = match r.settlement.outcome() {
        ObservationOutcome::Value(a) => {
            onyx_api::observations::answer_text(crate::template().kind, a)
        }
        other => {
            return Err(FeedError::Round(format!(
                "the round settled no value: {other:?}"
            )))
        }
    };
    Ok(FedRound {
        submission_id: item.id.clone(),
        submission_hash: item.hash.clone(),
        record_date,
        round_id: r.round_id,
        result: r.settlement.result_hash(),
        value,
        run: r,
    })
}

/// The evidence of a LIVE observer round for an approved submission (pre-public loop PP9): the
/// results store the round's coordinator wrote (`scripts/level2-observers.sh`), loaded and
/// re-checked with this adapter's pinned template at `now`, holding a round for the approved
/// record date. The receipt's registry (`eip155:<id>`) is the chain the store itself names for
/// the draw, never the operator's word. The worker runs nothing: the observers did.
pub fn fed_from_store(item: &ReviewItem, store: &str, now: u64) -> Result<FedEvidence, FeedError> {
    let record_date = approved_record_date(item)?;
    let t = crate::template();
    let pins = onyx_api::observations::TemplatePins::from([(t.metric.0.clone(), t.id())]);
    let feed = onyx_api::observations::ObservationStore::from_json(store, now, &pins)
        .map_err(|e| FeedError::Round(format!("store: {e}")))?;
    let chain = feed.onchain.ok_or_else(|| {
        FeedError::Round("a live round's store names the chain its draw was announced on".into())
    })?;
    let rec = feed
        .record(&crate::subject(&record_date), None, None)
        .ok_or_else(|| FeedError::Round(format!("the store has no round for {record_date}")))?;
    Ok(FedEvidence {
        id: item.id.clone(),
        hash: item.hash.clone(),
        record_date,
        round_id: rec.checked.round_id.to_string(),
        result: rec.checked.result.to_string(),
        value: onyx_api::observations::answer_text(t.kind, &rec.transcript.answer),
        registry: format!("eip155:{}", chain.chain_id),
    })
}

// The ledger is shared with the node, which shows a fed receipt's evidence (ADR-0039 amendment 1).
pub use onyx_api::submit::feed::{
    ledger_path, FedEvidence, FeedEntry, FeedLedger, FeedStep, MAX_FEED_ATTEMPTS,
};

impl FedRound {
    /// The evidence the feed ledger records for this round, registered on `registry`
    /// (`simulated` or `eip155:<chain id>`).
    pub fn evidence(&self, registry: &str) -> FedEvidence {
        FedEvidence {
            id: self.submission_id.clone(),
            hash: self.submission_hash.clone(),
            record_date: self.record_date.clone(),
            round_id: self.round_id.to_string(),
            result: self.result.to_string(),
            value: self.value.clone(),
            registry: registry.to_string(),
        }
    }
}
