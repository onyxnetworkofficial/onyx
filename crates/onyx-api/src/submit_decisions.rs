//! Proposal review beside a running node (ROADMAP S5, READINESS B2, ADR-0039 amendment 3;
//! docs/ops/proposal-review.md).
//!
//! The node holds the intake journal's lock for as long as it runs, so a reviewer cannot append
//! to that file. The reviewer's decisions therefore live in a second journal beside it
//! ([`decisions_path`]: `submissions.decisions.journal`), in the same format (checksummed lines,
//! one `flock`ed writer, append only). The two files have one writer each:
//!
//! - the node writes the intake journal and only reads the decisions;
//! - the reviewer tool ([`decide`]) writes the decisions and only reads the intake journal
//!   ([`crate::journal::Journal::read_only`]: no lock, no write, a torn last line left out).
//!   It runs as the user that owns the intake journal, never as root, and no name it opens
//!   may be a symbolic link (review B2-R1 H1).
//!
//! Neither waits for the other, and nothing already written is changed or removed. A decision is
//! `cleared` (accepted for the next step, with every checklist line answered yes), `on_hold` or
//! `dropped` (each with a reason code from [`REASONS`]). The node re-reads the decisions when the
//! file changes ([`Watch`]) and `GET /v1/submit/{id}` then shows the state and the reason code:
//! fixed words only, never a reviewer's text. Clearing publishes nothing.
//!
//! Who may decide: whoever can write the node's state directory (the same right that can replace
//! the journal or the binary). There is no reviewer account and no HTTP route.

use super::review::{read_records, ReviewItem};
use super::{BINDING, HELD_KEYED_SOURCE_REVIEW, HELD_RIGHTS_REVIEW, PENDING_REVIEW};
use crate::journal::Journal;
use onyx_intake::submission::{has_email, has_secret_param};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The decisions journal's kind (its header; another kind refuses to open).
pub const DECISIONS_KIND: &str = "onyx-submission-decisions";
/// Accepted for the next step. Not verification and not publication.
pub const CLEARED: &str = "cleared";
/// Waiting on something outside the proposal; a later `clear` or `drop` ends it.
pub const ON_HOLD: &str = "on_hold";
/// Will not proceed. Final.
pub const DROPPED: &str = "dropped";
/// The reason codes of `hold` and `drop`, and of the public status answer. `other` is written
/// `other:CODE` by the reviewer ([`is_other_code`]); the public answer shows `other` only.
pub const REASONS: &[&str] = &[
    "terms_forbid",
    "personal_data",
    "not_entities",
    "duplicate",
    "unreachable_source",
    "out_of_scope",
    "spam",
    "other",
];
/// The rights levels a reviewer may clear with (docs/spec/rights.md: `Allowed`, or `Restricted`
/// with the source's terms; `Forbidden` data is never ingested, so it is dropped, not cleared).
pub const RIGHTS_LEVELS: &[&str] = &["allowed", "restricted"];
/// Most decisions on one proposal: hold, clear, then drop.
pub const MAX_DECISIONS: u32 = 3;
/// The log tag of the warning before the recorded cap ([`cap_alert`]).
pub const CAP_ALERT_TAG: &str = "onyx-intake-cap-alert";
/// The share of the recorded cap, in percent, from which the node warns.
pub const CAP_ALERT_PERCENT: usize = 80;
/// The log tag of a decisions journal the running node cannot read.
pub const DECISIONS_ALERT_TAG: &str = "onyx-intake-decisions-alert";
/// Largest decisions journal that is read, in bytes. At the recorded cap, three decisions on
/// each of 10,000 proposals are about 14 MB.
pub const MAX_DECISIONS_BYTES: u64 = 32 * 1024 * 1024;
/// Longest line of the decisions journal that is read, in bytes (a decision is under 1 KiB; a
/// journal written afresh holds 64 on a line).
pub const MAX_DECISION_LINE: usize = 256 * 1024;
const MAX_UNIX: u64 = (1 << 53) - 1;

/// The reviewer checklist's yes/no lines: the `clear` flag, the journal field, the question
/// (THREATS SUB-3, SUB-4, SUB-5; docs/spec/rights.md). The fifth line is the rights level.
pub const CHECKLIST_LINES: &[(&str, &str, &str)] = &[
    (
        "--terms-reread",
        "termsRereadToday",
        "I re-read the source's terms today and they allow redistribution",
    ),
    (
        "--no-personal-data",
        "noPersonalData",
        "No field, sample or source page names or describes a person",
    ),
    (
        "--entities-only",
        "entitiesOnly",
        "The data is about entities (not one person under a company or project name)",
    ),
    (
        "--source-public",
        "sourcePublicReachable",
        "The source is public (no login, key or payment) and I reached it today",
    ),
];

/// The decisions journal of the intake journal at `journal`: a trailing `.journal` is replaced
/// by `.decisions.journal` (`submissions.journal` gives `submissions.decisions.journal`), so the
/// backup's journal reader finds it by its ending.
pub fn decisions_path(journal: &Path) -> PathBuf {
    let name = journal
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".journal").unwrap_or(&name);
    journal.with_file_name(format!("{stem}.decisions.journal"))
}

/// The checklist a `clear` carries: every line `yes` and a rights level.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Checklist {
    /// The source's terms were re-read on the day of the decision.
    pub terms_reread_today: String,
    /// Nothing in the proposal is personal data.
    pub no_personal_data: String,
    /// The subject is an entity.
    pub entities_only: String,
    /// The source is public and was reached.
    pub source_public_reachable: String,
    /// One of [`RIGHTS_LEVELS`].
    pub rights_level: String,
}

impl Checklist {
    /// The first line that is missing or not `yes`, as its flag.
    pub fn complete(&self) -> Result<(), &'static str> {
        let answers = [
            &self.terms_reread_today,
            &self.no_personal_data,
            &self.entities_only,
            &self.source_public_reachable,
        ];
        for (answer, line) in answers.iter().zip(CHECKLIST_LINES) {
            if answer.as_str() != "yes" {
                return Err(line.0);
            }
        }
        if !RIGHTS_LEVELS.contains(&self.rights_level.as_str()) {
            return Err("--rights-level");
        }
        Ok(())
    }
}

/// One line of the decisions journal.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Event {
    kind: String,
    version: u8,
    submission_id: String,
    hash: String,
    revision: u32,
    from: String,
    to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checklist: Option<Checklist>,
    decided_unix: u64,
}

/// A proposal's latest decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided {
    /// [`CLEARED`], [`ON_HOLD`] or [`DROPPED`].
    pub state: String,
    /// The reason as the reviewer gave it (`duplicate`, `other:CODE`); none for `cleared`.
    pub reason: Option<String>,
    /// How many decisions were taken (1 to [`MAX_DECISIONS`]).
    pub revision: u32,
    /// When, Unix seconds.
    pub decided_unix: u64,
    /// The checklist of a `clear`.
    pub checklist: Option<Checklist>,
}

impl Decided {
    /// The reason a proposer may see: the code from [`REASONS`], without the reviewer's own
    /// code after `other:`.
    pub fn public_reason(&self) -> Option<&str> {
        let reason = self.reason.as_deref()?;
        let code = reason.split(':').next().unwrap_or(reason);
        REASONS.iter().copied().find(|r| *r == code)
    }
}

/// The latest decision per receipt id.
pub type Overlay = BTreeMap<String, Decided>;

/// Why a decision is refused.
#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    /// A journal cannot be read, is damaged, or another reviewer holds the decisions lock.
    #[error("{0}")]
    Storage(#[from] std::io::Error),
    /// No proposal has this id.
    #[error("no proposal with this id")]
    NotFound,
    /// The proposal's state takes no such decision any more.
    #[error("already decided: the proposal is {0}")]
    AlreadyDecided(String),
    /// A checklist line is missing or not `yes`.
    #[error("the checklist is not complete: {0} must be answered (yes on every line, and a rights level: allowed or restricted)")]
    Checklist(&'static str),
    /// Not a reason code.
    #[error("not a reason code (one of terms_forbid, personal_data, not_entities, duplicate, unreachable_source, out_of_scope, spam, or other:CODE with CODE 1 to 32 of a-z, 0-9 and _)")]
    Reason,
    /// The intake held this proposal's source; a hold is not lifted here.
    #[error("the intake holds this proposal ({0}); it can be put on hold or dropped, not cleared (ADR-0039 item 4)")]
    NotClearable(String),
    /// The decision's time is before the proposal's last event.
    #[error("the clock is behind the proposal's last event; check the time on this machine")]
    Clock,
}

/// Whether `s` is `other:CODE` with a short machine code.
pub fn is_other_code(s: &str) -> bool {
    s.strip_prefix("other:").is_some_and(|c| {
        (1..=32).contains(&c.len())
            && c.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

fn reason_ok(reason: &str) -> bool {
    is_other_code(reason) || (reason != "other" && REASONS.contains(&reason))
}

fn initial_status(item: &ReviewItem) -> &'static str {
    item.submission.route().status()
}

fn decided_offline(item: &ReviewItem) -> bool {
    !matches!(
        item.status.as_str(),
        PENDING_REVIEW | HELD_RIGHTS_REVIEW | HELD_KEYED_SOURCE_REVIEW
    )
}

/// Whether `event` may follow `prev` on `item`. The same rule when the reviewer decides and when
/// a journal is replayed (so a line that a replay refuses was never written by [`decide`]).
fn transition(item: &ReviewItem, prev: Option<&Decided>, event: &Event) -> Result<(), DecideError> {
    let from = prev.map_or(initial_status(item), |d| d.state.as_str());
    let revision = prev.map_or(0, |d| d.revision);
    if prev.is_some_and(|d| d.state == DROPPED) {
        return Err(DecideError::AlreadyDecided(DROPPED.into()));
    }
    if event.kind != "decision"
        || event.version != 1
        || event.submission_id != item.id
        || event.hash != item.hash
        || event.from != from
        || event.revision != revision + 1
        || event.revision > MAX_DECISIONS
    {
        return Err(DecideError::AlreadyDecided(from.into()));
    }
    if event.decided_unix < prev.map_or(item.received_unix, |d| d.decided_unix)
        || event.decided_unix > MAX_UNIX
    {
        return Err(DecideError::Clock);
    }
    match event.to.as_str() {
        CLEARED => {
            if event.reason.is_some() {
                return Err(DecideError::Reason);
            }
            if from == CLEARED {
                return Err(DecideError::AlreadyDecided(from.into()));
            }
            if initial_status(item) != PENDING_REVIEW {
                return Err(DecideError::NotClearable(initial_status(item).into()));
            }
            event
                .checklist
                .as_ref()
                .ok_or(DecideError::Checklist(CHECKLIST_LINES[0].0))?
                .complete()
                .map_err(DecideError::Checklist)
        }
        ON_HOLD | DROPPED => {
            if event.to == ON_HOLD && prev.is_some() {
                return Err(DecideError::AlreadyDecided(from.into()));
            }
            if event.checklist.is_some() || !event.reason.as_deref().is_some_and(reason_ok) {
                return Err(DecideError::Reason);
            }
            Ok(())
        }
        _ => Err(DecideError::AlreadyDecided(from.into())),
    }
}

/// What a replay of the decisions found. A line never stops the replay (review B2-R1 M1, M3):
/// one that breaks the rules is counted and ignored, and one about a receipt the intake
/// journal does not hold is set apart as orphaned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replayed {
    /// The latest decision per receipt the intake journal holds.
    pub overlay: Overlay,
    /// Decisions on receipts the intake journal does not hold (it was archived or restored
    /// from an older copy): receipt id and the state last decided. Shown to nobody.
    pub orphans: BTreeMap<String, String>,
    /// Lines that are not a decision or break the rules (a clear without its checklist,
    /// another content hash, a skipped revision, free text). Ignored.
    pub refused: usize,
}

/// Replay the decisions journal's entries over the receipts.
fn replay(records: &BTreeMap<String, ReviewItem>, entries: Vec<Value>) -> Replayed {
    let mut out = Replayed::default();
    for value in entries {
        let Ok(event) = serde_json::from_value::<Event>(value) else {
            out.refused += 1;
            continue;
        };
        let Some(item) = records.get(&event.submission_id) else {
            out.orphans.insert(event.submission_id, event.to);
            continue;
        };
        if transition(item, out.overlay.get(&event.submission_id), &event).is_err() {
            out.refused += 1;
            continue;
        }
        out.overlay.insert(
            event.submission_id,
            Decided {
                state: event.to,
                reason: event.reason,
                revision: event.revision,
                decided_unix: event.decided_unix,
                checklist: event.checklist,
            },
        );
    }
    out
}

fn bad(why: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("invalid decisions journal record: {why}"),
    )
}

/// What a reviewer decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Accept for the next step, with the checklist.
    Clear(Checklist),
    /// Put on hold, with a reason code.
    Hold(String),
    /// Drop, with a reason code.
    Drop(String),
}

/// One proposal as the reviewer sees it. Private: never serialize into a public answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    /// The receipt as the intake journal holds it.
    pub item: ReviewItem,
    /// The reviewer's latest decision, if any.
    pub decided: Option<Decided>,
}

impl Proposal {
    /// The state a proposer sees ([`public_state`]).
    pub fn state(&self) -> &str {
        public_state(&self.item, self.decided.as_ref()).0
    }

    /// Whether a reviewer still has to decide: no decision yet, or on hold.
    pub fn waiting(&self) -> bool {
        !decided_offline(&self.item)
            && self
                .decided
                .as_ref()
                .is_none_or(|d| d.state.as_str() == ON_HOLD)
    }
}

/// The state and reason code `GET /v1/submit/{id}` shows: the offline review's `approved` or
/// `rejected` (ADR-0039) when there is one, else the reviewer's decision, else the intake's
/// own state.
pub fn public_state<'a>(
    item: &'a ReviewItem,
    decided: Option<&'a Decided>,
) -> (&'a str, Option<&'a str>) {
    match decided {
        Some(d) if !decided_offline(item) => (d.state.as_str(), d.public_reason()),
        _ => (item.status.as_str(), None),
    }
}

/// The review queue: every proposal, and how full the journal is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queue {
    /// Waiting ones first (oldest first), then the decided ones.
    pub proposals: Vec<Proposal>,
    /// The recorded cap ([`super::SubmitLimits::max_recorded`]).
    pub capacity: usize,
    /// Decisions on receipts the intake journal does not hold ([`Replayed::orphans`]).
    pub orphans: Vec<(String, String)>,
    /// Lines of the decisions journal that are damaged (the next decision sets the file aside).
    pub damaged_lines: usize,
    /// The file under the decisions journal's name does not start with its header: nothing
    /// of it is read, and no decision can be written until the operator moves it away.
    pub foreign: bool,
    /// Well-formed lines that break the rules and are ignored.
    pub refused_lines: usize,
}

fn order(p: &Proposal) -> (u8, u64, String) {
    let rank = match p.state() {
        PENDING_REVIEW => 0,
        HELD_RIGHTS_REVIEW | HELD_KEYED_SOURCE_REVIEW => 1,
        ON_HOLD => 2,
        CLEARED => 3,
        _ => 4,
    };
    (rank, p.item.received_unix, p.item.id.clone())
}

fn proposals_of(records: BTreeMap<String, ReviewItem>, overlay: &mut Overlay) -> Vec<Proposal> {
    let mut proposals: Vec<Proposal> = records
        .into_values()
        .map(|item| {
            let decided = overlay.remove(&item.id);
            Proposal { item, decided }
        })
        .collect();
    proposals.sort_by_key(order);
    proposals
}

/// Refuse a name in the state directory that is a symbolic link or not a regular file (review
/// B2-R1 H1). `must_exist`: the intake journal; the decisions journal and its lock may be absent.
fn plain_file(path: &Path, must_exist: bool) -> std::io::Result<()> {
    let refuse = |what: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is {what}: the review works on regular files in the node's state directory only",
                path.display()
            ),
        ))
    };
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => refuse("a symbolic link"),
        Ok(m) if !m.is_file() => refuse("not a regular file"),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !must_exist => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "{}: no intake journal here (the node writes it when ONYX_SUBMISSIONS is set)",
                path.display()
            ),
        )),
        Err(e) => Err(e),
    }
}

fn lock_of(decisions: &Path) -> PathBuf {
    let mut lock = decisions.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// The three names the review touches, each a regular file or (the decisions journal and its
/// lock) absent. The journal's own open refuses a link again, without a gap ([`Journal::open`]).
fn plain_files(journal: &Path) -> std::io::Result<PathBuf> {
    plain_file(journal, true)?;
    let decisions = decisions_path(journal);
    plain_file(&decisions, false)?;
    plain_file(&lock_of(&decisions), false)?;
    Ok(decisions)
}

/// Read the queue of the intake journal at `journal`: no lock is taken and nothing is written,
/// so it is safe beside a running node and beside a reviewer who is deciding. Damage in the
/// decisions journal does not stop the list: the queue says what was ignored.
pub fn read_queue(journal: &Path, capacity: usize) -> std::io::Result<Queue> {
    let decisions = plain_files(journal)?;
    // The decisions first: a decision is always about a receipt recorded before it, so the
    // receipts read after them hold every receipt the decisions name.
    let found = Journal::read_lenient(
        decisions,
        DECISIONS_KIND,
        &BINDING,
        MAX_DECISIONS_BYTES,
        MAX_DECISION_LINE,
    )?;
    let foreign = found.foreign;
    let records = read_records(journal, capacity)?;
    let mut replayed = replay(&records, found.entries);
    Ok(Queue {
        proposals: proposals_of(records, &mut replayed.overlay),
        capacity,
        orphans: replayed.orphans.into_iter().collect(),
        damaged_lines: found.bad_lines,
        foreign,
        refused_lines: replayed.refused,
    })
}

/// A decision that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    /// The proposal with its new decision.
    pub proposal: Proposal,
    /// Where a damaged decisions journal was copied before it was written afresh, if it was
    /// ([`crate::journal::Held::open`]).
    pub set_aside: Option<PathBuf>,
}

/// Append one decision on the proposal `id`, at `now` (Unix seconds). Takes the decisions
/// journal's lock for the call (a second reviewer waits up to a second, then is refused), reads
/// the intake journal without its lock, and appends one checksummed, synced line. Nothing
/// already written is changed, except that a damaged decisions journal is set aside whole and
/// written afresh from its good lines, and only once the decision is known to be allowed: a
/// refused decision changes nothing. A file that does not start with the decisions journal's
/// header is never rewritten: the decision fails and the file is left for the operator.
///
/// Run it as the user that owns the intake journal, never as root (review B2-R1 H1): that
/// user owns the state directory and could plant a link under either name. The files this
/// creates then belong to that user without any change of owner. `onyx-node proposals` drops
/// to that user itself when it is started as root.
pub fn decide(
    journal: &Path,
    capacity: usize,
    id: &str,
    action: Action,
    now: u64,
) -> Result<Done, DecideError> {
    match &action {
        Action::Clear(list) => list.complete().map_err(DecideError::Checklist)?,
        Action::Hold(reason) | Action::Drop(reason) => {
            if !reason_ok(reason) {
                return Err(DecideError::Reason);
            }
        }
    }
    let decisions = plain_files(journal)?;
    // The lock, and a read past damage; nothing is written or moved until the decision itself
    // is known to be allowed (review B2-R2 M1). A file that is not a decisions journal stops
    // here, untouched.
    let held = Journal::hold(
        decisions,
        DECISIONS_KIND,
        &BINDING,
        (MAX_DECISIONS_BYTES, MAX_DECISION_LINE),
    )?;
    let records = read_records(journal, capacity)?;
    let overlay = replay(&records, held.entries().to_vec()).overlay;
    let item = records.get(id).ok_or(DecideError::NotFound)?;
    if decided_offline(item) {
        return Err(DecideError::AlreadyDecided(item.status.clone()));
    }
    let prev = overlay.get(id);
    let (to, reason, checklist) = match action {
        Action::Clear(list) => (CLEARED, None, Some(list)),
        Action::Hold(reason) => (ON_HOLD, Some(reason), None),
        Action::Drop(reason) => (DROPPED, Some(reason), None),
    };
    let event = Event {
        kind: "decision".into(),
        version: 1,
        submission_id: id.into(),
        hash: item.hash.clone(),
        revision: prev.map_or(0, |d| d.revision) + 1,
        from: prev
            .map_or(initial_status(item), |d| d.state.as_str())
            .into(),
        to: to.into(),
        reason,
        checklist,
        decided_unix: now,
    };
    transition(item, prev, &event)?;
    // Allowed: only now is a damaged journal set aside, and the line appended.
    let (mut book, _, set_aside) =
        held.open(now, |v| serde_json::from_value::<Event>(v.clone()).is_ok())?;
    book.apply(&[], &[serde_json::to_value(&event).map_err(bad)?])?;
    Ok(Done {
        proposal: Proposal {
            item: item.clone(),
            decided: Some(Decided {
                state: event.to,
                reason: event.reason,
                revision: event.revision,
                decided_unix: event.decided_unix,
                checklist: event.checklist,
            }),
        },
        set_aside,
    })
}

/// How full the journal is, in whole percent (rounded down).
pub fn fill_percent(recorded: usize, capacity: usize) -> usize {
    if capacity == 0 {
        return 100;
    }
    recorded.saturating_mul(100) / capacity
}

/// The warning line, with its fixed tag, once the journal holds [`CAP_ALERT_PERCENT`] percent
/// of the recorded cap or more. Counts only: nothing of a proposal.
pub fn cap_alert(recorded: usize, capacity: usize) -> Option<String> {
    let percent = fill_percent(recorded, capacity);
    (percent >= CAP_ALERT_PERCENT).then(|| {
        format!(
            "{CAP_ALERT_TAG}: the intake journal holds {recorded} of {capacity} proposals ({percent}% of the recorded cap); at the cap new proposals are refused (503 intake_full). Review the queue (docs/ops/proposal-review.md)"
        )
    })
}

/// Whether recording the `recorded`-th proposal is a moment to warn: the first at or over the
/// line, then once per further whole percent (at most 21 lines up to the cap).
pub fn cap_alert_due(recorded: usize, capacity: usize) -> bool {
    let percent = fill_percent(recorded, capacity);
    percent >= CAP_ALERT_PERCENT
        && (recorded == 0 || fill_percent(recorded - 1, capacity) < percent)
}

/// What the decisions journal looked like when it was last read: absent, or its size and time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    Absent,
    /// Size, modification time and (on Unix) change time, which a change of mode moves too.
    File(u64, Option<std::time::SystemTime>, Option<(i64, i64)>),
    /// Its name is there but cannot be looked at.
    Unreadable,
}

struct Seen {
    stamp: Stamp,
    /// A look that did not read: tried once, not again until the file changes.
    failed: Option<Stamp>,
    overlay: Overlay,
    /// How many times the file was read and replayed.
    reads: usize,
    /// The last alert lines (most [`MAX_ALERTS`]).
    alerts: Vec<String>,
}

/// Most alert lines the node keeps in memory.
const MAX_ALERTS: usize = 32;

/// The node's view of the decisions journal: read at start, read again when the file's size or
/// time changes. The node never writes it and never takes its lock. Nothing in that file stops
/// the node or takes a decision away while it runs (review B2-R1 M1, M3, L2): damage, a file
/// that vanished or cannot be read, and decisions on receipts the node does not hold are each
/// one log line ([`DECISIONS_ALERT_TAG`]), and the last known decisions stay.
pub(super) struct Watch {
    path: PathBuf,
    seen: Mutex<Seen>,
}

fn stamp_of(path: &Path) -> Stamp {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            #[cfg(unix)]
            let changed = {
                use std::os::unix::fs::MetadataExt;
                Some((m.ctime(), m.ctime_nsec()))
            };
            #[cfg(not(unix))]
            let changed = None;
            Stamp::File(m.len(), m.modified().ok(), changed)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stamp::Absent,
        Err(_) => Stamp::Unreadable,
    }
}

impl Seen {
    fn alert(&mut self, what: &str) {
        let line = format!("{DECISIONS_ALERT_TAG}: {what}");
        eprintln!("onyx-node: {line}");
        if self.alerts.len() == MAX_ALERTS {
            self.alerts.remove(0);
        }
        self.alerts.push(line);
    }

    /// Read the file as it is at `stamp` and take what it holds. Counts only in the alerts.
    fn read(&mut self, path: &Path, stamp: Stamp, records: &BTreeMap<String, ReviewItem>) {
        self.reads += 1;
        let found = match Journal::read_lenient(
            path,
            DECISIONS_KIND,
            &BINDING,
            MAX_DECISIONS_BYTES,
            MAX_DECISION_LINE,
        ) {
            Ok(found) => found,
            Err(e) => {
                self.failed = Some(stamp);
                self.alert(&format!(
                    "the reviewer's decisions cannot be read, the last known ones are shown: {e}"
                ));
                return;
            }
        };
        let (damaged, foreign) = (found.bad_lines, found.foreign);
        let replayed = replay(records, found.entries);
        let mut overlay = replayed.overlay;
        // Decisions are only ever added, so a decision this node knows is never taken away
        // or set back while it runs, whatever the file holds now (damage, a file set aside,
        // a restored older copy).
        for (id, known) in &self.overlay {
            if overlay
                .get(id)
                .is_none_or(|new| new.revision < known.revision)
            {
                overlay.insert(id.clone(), known.clone());
            }
        }
        if damaged > 0 {
            self.alert(&if foreign {
                "the decisions journal does not start with its header, nothing of it is read and the last known decisions are shown; it is never rewritten: move it away by hand and restore the last good copy (docs/ops/proposal-review.md \"Recovery\")".to_string()
            } else {
                format!("{damaged} damaged line(s) in the decisions journal are ignored and the last known decisions are kept; the reviewer's next decision sets the file aside (docs/ops/proposal-review.md \"Recovery\")")
            });
        }
        if replayed.refused > 0 {
            self.alert(&format!(
                "{} line(s) of the decisions journal break the decision rules and are ignored",
                replayed.refused
            ));
        }
        if !replayed.orphans.is_empty() {
            self.alert(&format!(
                "{} decision(s) are about receipts the intake journal does not hold and are skipped (the intake journal was archived or restored without its decisions journal)",
                replayed.orphans.len()
            ));
        }
        self.overlay = overlay;
        self.stamp = stamp;
        self.failed = None;
    }
}

impl Watch {
    /// Read the decisions beside `journal`. Never refuses the start.
    pub(super) fn open(journal: &Path, records: &BTreeMap<String, ReviewItem>) -> Watch {
        let path = decisions_path(journal);
        let mut seen = Seen {
            stamp: Stamp::Absent,
            failed: None,
            overlay: Overlay::new(),
            reads: 0,
            alerts: Vec::new(),
        };
        let stamp = stamp_of(&path);
        if stamp != Stamp::Absent {
            seen.read(&path, stamp, records);
        }
        Watch {
            path,
            seen: Mutex::new(seen),
        }
    }

    /// The latest decision on `id`, after re-reading the file when it changed. The caller
    /// holds the receipts' lock, so the receipts are never behind the decisions.
    pub(super) fn decided(
        &self,
        records: &BTreeMap<String, ReviewItem>,
        id: &str,
    ) -> Option<Decided> {
        let mut seen = self.seen.lock().ok()?;
        let stamp = stamp_of(&self.path);
        if stamp != seen.stamp && seen.failed != Some(stamp) {
            if stamp == Stamp::Absent {
                // It was there and is gone: not "no decisions".
                seen.failed = Some(stamp);
                seen.alert("the decisions journal is gone; the last known decisions are shown until the node restarts");
            } else {
                seen.read(&self.path, stamp, records);
            }
        }
        seen.overlay.get(id).cloned()
    }

    /// The alert lines so far (the newest last).
    pub(super) fn alerts(&self) -> Vec<String> {
        self.seen
            .lock()
            .map(|s| s.alerts.clone())
            .unwrap_or_default()
    }

    /// How many times the file was read.
    #[cfg(test)]
    pub(super) fn reads(&self) -> usize {
        self.seen.lock().map(|s| s.reads).unwrap_or(0)
    }
}

/// Text for the reviewer's terminal (review B2-R1 M2). A proposal's text is a stranger's: it is
/// printed with every control character and every invisible or direction-changing character
/// (Unicode format characters, line and paragraph separators) written out as `\u{..}`, and
/// with any word shaped like an e-mail address or a URL that gives a secret parameter a value
/// replaced by [`WITHHELD`]. Only that word is withheld and the command does not fail: the
/// intake checks URLs in the URL fields, not inside a description.
#[derive(Debug, Default)]
pub struct Printer {
    out: String,
    /// How many words were withheld.
    pub withheld: usize,
}

/// What a withheld word prints instead.
pub const WITHHELD: &str = "[withheld]";

fn withheld_word(word: &str) -> bool {
    has_email(word)
        || (word.contains("://")
            // The placeholder of a keyed source's template is not a key.
            && has_secret_param(&word.replace(onyx_intake::KEY_PLACEHOLDER, "")))
}

/// Whether `text` holds a word that may not be printed: an e-mail shape, or a URL that gives
/// a secret parameter a value (other than a template's `{api_key}` placeholder).
pub fn unprintable(text: &str) -> bool {
    text.split_whitespace().any(withheld_word)
}

/// Whether `c` could hide, reorder or move text on a terminal.
fn invisible(c: char) -> bool {
    c.is_control()
        || onyx_intake::submission::is_format_char(c)
        || matches!(c, '\u{2028}' | '\u{2029}')
}

impl Printer {
    /// `text` as it may be printed.
    pub fn safe(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let escape = |out: &mut String, word: &str| {
            for c in word.chars() {
                if invisible(c) {
                    let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
                } else {
                    out.push(c);
                }
            }
        };
        for piece in text.split_inclusive(char::is_whitespace) {
            let word = piece.trim_end_matches(char::is_whitespace);
            if withheld_word(word) {
                self.withheld += 1;
                out.push_str(WITHHELD);
            } else {
                escape(&mut out, word);
            }
            escape(&mut out, &piece[word.len()..]);
        }
        out
    }

    fn line(&mut self, label: &str, text: &str) {
        let text = self.safe(text);
        let _ = writeln!(self.out, "{label:<22}{text}");
    }

    /// The text so far.
    pub fn text(&self) -> &str {
        &self.out
    }
}

fn age(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..=3_599 => format!("{}m", s / 60),
        3_600..=86_399 => format!("{}h", s / 3_600),
        _ => format!("{}d {}h", s / 86_400, s % 86_400 / 3_600),
    }
}

fn code_of<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".into())
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn source_host(item: &ReviewItem) -> String {
    onyx_intake::HttpsUrl::parse(&item.submission.source_url)
        .map_or_else(|_| "(no source URL)".into(), |u| u.host)
}

fn rights_answers(p: &mut Printer, item: &ReviewItem) -> String {
    let a = &item.submission.answers;
    let license = a
        .license_id
        .as_deref()
        .map(|l| format!(" ({})", p.safe(l)))
        .unwrap_or_default();
    format!(
        "rights={}{license} what={} no_login={} names_person={}",
        code_of(&a.rights),
        code_of(&a.what),
        yes_no(a.no_login),
        yes_no(a.names_person)
    )
}

fn state_words(q: &Proposal) -> String {
    let reason = q
        .decided
        .as_ref()
        .filter(|_| !decided_offline(&q.item))
        .and_then(|d| d.reason.as_deref())
        .map(|r| format!(" ({r})"))
        .unwrap_or_default();
    format!("{}{reason}", q.state())
}

/// The fill level line `list` prints first.
pub fn fill_line(recorded: usize, capacity: usize) -> String {
    let warn = if cap_alert(recorded, capacity).is_some() {
        format!(
            " WARNING: over {CAP_ALERT_PERCENT}% of the cap; at the cap new proposals are refused"
        )
    } else {
        String::new()
    };
    format!(
        "Journal: {recorded} of {capacity} proposals recorded ({}% of the cap).{warn}",
        fill_percent(recorded, capacity)
    )
}

/// The `list` output: the fill level, then one line per proposal, waiting ones first. Without
/// `all`, only the waiting ones. No title, description or sample: `show` prints those.
pub fn render_list(q: &Queue, now: u64, all: bool) -> Printer {
    let mut p = Printer::default();
    let waiting = q.proposals.iter().filter(|x| x.waiting()).count();
    let _ = writeln!(p.out, "{}", fill_line(q.proposals.len(), q.capacity));
    let _ = writeln!(
        p.out,
        "{waiting} waiting for a decision, {} decided.{}",
        q.proposals.len() - waiting,
        if all {
            ""
        } else {
            " (--all lists the decided ones too)"
        }
    );
    if q.foreign {
        let _ = writeln!(
            p.out,
            "WARNING: the file under the decisions journal's name is not a decisions journal; nothing of it is read and no decision can be written. Move it away by hand and restore the last good copy (docs/ops/proposal-review.md \"Recovery\")."
        );
    } else if q.damaged_lines + q.refused_lines > 0 {
        let _ = writeln!(
            p.out,
            "WARNING: the decisions journal has {} damaged and {} rule-breaking line(s); they are ignored. The next decision sets a damaged file aside (docs/ops/proposal-review.md \"Recovery\").",
            q.damaged_lines, q.refused_lines
        );
    }
    if !q.orphans.is_empty() {
        let _ = writeln!(
            p.out,
            "WARNING: {} orphaned decision(s): the intake journal does not hold their receipts (archived or restored without the decisions journal). Nobody is shown them:",
            q.orphans.len()
        );
        for (id, state) in &q.orphans {
            let (id, state) = (p.safe(id), p.safe(state));
            let _ = writeln!(p.out, "  orphaned  {id}  {state}");
        }
    }
    for x in q.proposals.iter().filter(|x| all || x.waiting()) {
        let rights = rights_answers(&mut p, &x.item);
        let host = source_host(&x.item);
        let host = p.safe(&host);
        let _ = writeln!(
            p.out,
            "{}  {:<26} age {:<8} host {host}  {rights}",
            x.item.id,
            state_words(x),
            age(now, x.item.received_unix),
        );
    }
    p
}

/// The `show` output: every field of one proposal, private to the reviewer.
pub fn render_show(x: &Proposal, now: u64) -> Printer {
    let mut p = Printer::default();
    let s = &x.item.submission;
    let route = s.route();
    p.line("id", &x.item.id);
    p.line("state", &state_words(x));
    p.line("age", &age(now, x.item.received_unix));
    p.line("received (unix)", &x.item.received_unix.to_string());
    p.line("content hash", &x.item.hash);
    let level = route.level.map_or_else(
        || "none".to_string(),
        |l| format!("{} ({})", l.number(), onyx_intake::level_name(l)),
    );
    p.line("route (advice)", &format!("level {level}"));
    let rights = rights_answers(&mut p, &x.item);
    p.line("answers", &rights);
    p.line("changes", &code_of(&s.answers.changes));
    p.line("source host", &source_host(&x.item));
    p.line("title", &s.title);
    p.line("description", &s.description);
    p.line("source name", &s.source_name);
    p.line("source URL", &s.source_url);
    let optional = [
        ("endpoint URL", &s.endpoint_url),
        ("JSON pointer", &s.json_pointer),
        ("unit", &s.unit),
        ("endpoint template", &s.endpoint_template),
        ("series id", &s.series_id),
    ];
    for (label, value) in optional {
        if let Some(v) = value {
            p.line(label, v);
        }
    }
    if let Some(k) = &s.key_proof {
        p.line(
            "key proof",
            &format!("prover {} (real: {})", k.prover, k.real),
        );
    }
    if let Some(d) = &s.data_payload {
        p.line("sample", &format!("{} bytes of {}", d.bytes, d.format));
        for line in d.content.lines() {
            p.line("  |", line);
        }
    }
    if let Some(d) = &x.decided {
        p.line(
            "decision",
            &format!(
                "{} at {} (decision {} of at most {MAX_DECISIONS})",
                d.state, d.decided_unix, d.revision
            ),
        );
        if let Some(r) = &d.reason {
            p.line("reason", r);
        }
        if let Some(c) = &d.checklist {
            p.line(
                "checklist",
                &format!("every line yes; rights level {}", c.rights_level),
            );
        }
    }
    if decided_offline(&x.item) {
        p.line(
            "offline review",
            &format!(
                "{} ({})",
                x.item.status,
                x.item.reason.as_deref().unwrap_or("no reason")
            ),
        );
    }
    p
}

#[cfg(test)]
#[path = "submit_decisions_tests.rs"]
mod tests;
