//! U2 / ADR-0039: local review ledger; never mounted as an HTTP admin API.
use super::{hash_hex, submission_id, SubmitStore, BINDING, JOURNAL_KIND, PENDING_REVIEW};
use crate::journal::Journal;
use onyx_intake::{Outcome, Submission};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// Maximum entries in one private queue page.
pub const MAX_REVIEW_PAGE: usize = 100;
const MAX_UNIX: u64 = (1 << 53) - 1;

/// Local operator-visible receipt. Never serialize this into a public response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    /// Stable locator.
    pub id: String,
    /// Immutable submission content hash.
    pub hash: String,
    /// Current review state, not a verification level.
    pub status: String,
    /// Number of decisions applied (0..=2).
    pub revision: u32,
    /// Original receipt time.
    pub received_unix: u64,
    /// Last decision time, if any.
    pub reviewed_unix: Option<u64>,
    /// Last private reason code.
    pub reason: Option<String>,
    /// Original validated content; local access only.
    pub submission: Submission,
}

/// Private paged queue entry; content is obtained with an explicit show operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSummary {
    /// Locator used by show/decide.
    pub id: String,
    /// Hash for grouping identical contributions privately.
    pub hash: String,
    /// Current review state.
    pub status: String,
    /// Expected revision for a decision.
    pub revision: u32,
}

/// Bounded review failures; persistence details stay out of public responses.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// No more submissions may be recorded.
    #[error("the submission ledger is full")]
    Full,
    /// Storage is unavailable; operator must recover/reopen it.
    #[error("submission storage is unavailable")]
    Unavailable,
    /// No such receipt.
    #[error("submission not found")]
    NotFound,
    /// Reviewer acted on stale/different content.
    #[error("expected revision or content hash does not match")]
    Stale,
    /// Invalid transition, reason, timestamp or page arguments.
    #[error("review transition or arguments are not allowed")]
    Invalid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Initial {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    version: Option<u8>,
    hash: String,
    status: String,
    received_unix: u64,
    route: Value,
    submission: Submission,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Decision {
    kind: String,
    version: u8,
    submission_id: String,
    hash: String,
    revision: u32,
    from: String,
    to: String,
    reason: String,
    reviewed_unix: u64,
}

pub(super) struct Ledger {
    journal: Option<Journal>,
    records: BTreeMap<String, ReviewItem>,
    capacity: usize,
    #[cfg(test)]
    pub(super) before_publish: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

fn bad() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "invalid submission journal record",
    )
}

fn transition(item: &ReviewItem, event: &Decision) -> Result<(), ReviewError> {
    if event.kind != "review"
        || event.version != 1
        || event.submission_id != item.id
        || event.hash != item.hash
        || event.from != item.status
        || event.revision != item.revision + 1
        || event.revision > 2
        || event.reviewed_unix < item.reviewed_unix.unwrap_or(item.received_unix)
        || event.reviewed_unix > MAX_UNIX
    {
        return Err(ReviewError::Invalid);
    }
    let initial = matches!(
        item.status.as_str(),
        "pending_review" | "held_rights_review" | "held_keyed_source_review"
    );
    let valid = match event.to.as_str() {
        "approved" => {
            item.status == PENDING_REVIEW
                && item.submission.route().outcome == Outcome::Accept
                && event.reason == "reviewed"
        }
        "rejected" if initial => matches!(
            event.reason.as_str(),
            "rights_unresolved" | "unsupported_source" | "invalid_content" | "duplicate"
        ),
        "rejected" if item.status == "approved" => event.reason == "withdrawn",
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ReviewError::Invalid)
    }
}

fn apply_decision(item: &mut ReviewItem, event: Decision) {
    item.status = event.to;
    item.revision = event.revision;
    item.reviewed_unix = Some(event.reviewed_unix);
    item.reason = Some(event.reason);
}

impl Ledger {
    pub(super) fn open(path: impl AsRef<Path>, capacity: usize) -> std::io::Result<Self> {
        let (journal, entries) = Journal::open(path, JOURNAL_KIND, &BINDING)?;
        if entries.len() > capacity.saturating_mul(3) {
            return Err(bad());
        }
        let mut records: BTreeMap<String, ReviewItem> = BTreeMap::new();
        for (seq, value) in entries {
            if value["kind"] == "review" {
                let event: Decision = serde_json::from_value(value).map_err(|_| bad())?;
                let item = records.get_mut(&event.submission_id).ok_or_else(bad)?;
                transition(item, &event).map_err(|_| bad())?;
                apply_decision(item, event);
            } else {
                let raw: Initial = serde_json::from_value(value).map_err(|_| bad())?;
                let version_ok = (raw.kind.is_none() && raw.version.is_none())
                    || (raw.kind.as_deref() == Some("submission") && raw.version == Some(1));
                raw.submission.validate().map_err(|_| bad())?;
                let route = raw.submission.route();
                if !version_ok
                    || raw.hash != hash_hex(&raw.submission)
                    || raw.status != route.status()
                    || raw.route != route.to_json()
                    || route.outcome == Outcome::Refused
                    || raw.received_unix > MAX_UNIX
                    || records.len() >= capacity
                {
                    return Err(bad());
                }
                let id = submission_id(seq, &raw.hash);
                let item = ReviewItem {
                    id: id.clone(),
                    hash: raw.hash,
                    status: raw.status,
                    revision: 0,
                    received_unix: raw.received_unix,
                    reviewed_unix: None,
                    reason: None,
                    submission: raw.submission,
                };
                if records.insert(id, item).is_some() {
                    return Err(bad());
                }
            }
        }
        Ok(Self {
            journal: Some(journal),
            records,
            capacity,
            #[cfg(test)]
            before_publish: None,
        })
    }

    fn append(&mut self, value: Value) -> Result<u64, ReviewError> {
        let journal = self.journal.as_mut().ok_or(ReviewError::Unavailable)?;
        match journal.apply(&[], &[value]) {
            Ok(ids) if ids.len() == 1 => Ok(ids[0]),
            _ => {
                self.journal = None;
                Err(ReviewError::Unavailable)
            }
        }
    }

    pub(super) fn recorded(&self) -> usize {
        self.records.len()
    }

    pub(super) fn record(
        &mut self,
        submission: Submission,
        received: u64,
    ) -> Result<String, ReviewError> {
        if self.records.len() >= self.capacity {
            return Err(ReviewError::Full);
        }
        let route = submission.route();
        if route.outcome == Outcome::Refused || received > MAX_UNIX {
            return Err(ReviewError::Invalid);
        }
        let hash = hash_hex(&submission);
        let status = route.status().to_string();
        let entry = json!({"kind":"submission", "version":1, "hash":hash, "status":status,
            "receivedUnix":received, "route":route.to_json(), "submission":submission});
        let seq = self.append(entry)?;
        #[cfg(test)]
        if let Some(hook) = self.before_publish.take() {
            hook();
        }
        let id = submission_id(seq, &hash);
        self.records.insert(
            id.clone(),
            ReviewItem {
                id: id.clone(),
                hash,
                status,
                revision: 0,
                received_unix: received,
                reviewed_unix: None,
                reason: None,
                submission,
            },
        );
        Ok(id)
    }
}

impl SubmitStore {
    /// Read one complete receipt locally. Do not expose this through HTTP.
    pub fn review_item(&self, id: &str) -> Result<ReviewItem, ReviewError> {
        self.ledger
            .lock()
            .map_err(|_| ReviewError::Unavailable)?
            .records
            .get(id)
            .cloned()
            .ok_or(ReviewError::NotFound)
    }

    /// List a bounded private page in ID order, strictly after `after` when supplied.
    pub fn review_list(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ReviewSummary>, ReviewError> {
        if limit == 0 || limit > MAX_REVIEW_PAGE || after.is_some_and(|a| !super::is_id(a)) {
            return Err(ReviewError::Invalid);
        }
        let ledger = self.ledger.lock().map_err(|_| ReviewError::Unavailable)?;
        Ok(ledger
            .records
            .values()
            .filter(|r| after.is_none_or(|a| r.id.as_str() > a))
            .take(limit)
            .map(|r| ReviewSummary {
                id: r.id.clone(),
                hash: r.hash.clone(),
                status: r.status.clone(),
                revision: r.revision,
            })
            .collect())
    }

    /// Append a local operator decision. The node must be stopped before a separate CLI opens
    /// its journal. This is not authentication and must not be mounted on a public route.
    pub fn review(
        &self,
        id: &str,
        expected_revision: u32,
        expected_hash: &str,
        next: &str,
        reason: &str,
        reviewed_unix: u64,
    ) -> Result<ReviewItem, ReviewError> {
        let mut ledger = self.ledger.lock().map_err(|_| ReviewError::Unavailable)?;
        let mut item = ledger
            .records
            .get(id)
            .cloned()
            .ok_or(ReviewError::NotFound)?;
        if item.revision != expected_revision || item.hash != expected_hash {
            return Err(ReviewError::Stale);
        }
        let event = Decision {
            kind: "review".into(),
            version: 1,
            submission_id: id.into(),
            hash: item.hash.clone(),
            revision: item.revision + 1,
            from: item.status.clone(),
            to: next.into(),
            reason: reason.into(),
            reviewed_unix,
        };
        transition(&item, &event)?;
        ledger.append(serde_json::to_value(&event).map_err(|_| ReviewError::Invalid)?)?;
        apply_decision(&mut item, event);
        ledger.records.insert(id.into(), item.clone());
        Ok(item)
    }
}
