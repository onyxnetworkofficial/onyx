//! Publisher pointers (ADR-0066 item 7, ROADMAP S27, READINESS B5): the way to apply to
//! publish without an account anywhere.
//!
//! - `POST /v1/publishers/pointer`: body `{"origin": "...", "application": "0x<32 bytes>"}`,
//!   where the application file is and its Keccak-256. The node checks the two strings and
//!   keeps them with a receipt id and the receipt time. It fetches nothing: no code in this
//!   module makes a request, resolves a name or opens anything but its own journal. The
//!   reviewer's tool pulls the application from the origin later, from a throwaway machine
//!   (docs/ops/publisher-review.md).
//! - `GET /v1/publishers/pointer/{id}`: that receipt's state: `received`, `expired`, or
//!   `dropped` once the reviewer's tool found that the file at the origin is not the one the
//!   pointer names ([`drop_pointer`]). The
//!   node knows nothing of a decision (the record is a folder in the repository, which the
//!   node cannot read), so the answer always says where the decision is: the public status
//!   list, looked up by the application's hash.
//!
//! What the node checks: the body's shape (two known fields, each once, at most
//! [`MAX_BODY`] bytes), the origin's grammar ([`onyx_bulk::ids::Origin`]: a lowercase host,
//! at most four path parts, no scheme, port, address or punycode label), the list of shared
//! hosts compiled into the binary, and the hash's shape. What it leaves to the reviewer's
//! tool, because it needs the book or the origin: the overlap with a registered origin,
//! whether the id, the origin and the key are free, and everything about the file itself.
//!
//! Limits: per-client budgets per minute and per UTC day and budgets for the whole server
//! (the proposal intake's limiter, [`crate::submit`], with this route's own counts), at most
//! [`MAX_PER_ORIGIN`] waiting pointers with different hashes for one host (whatever the path,
//! review PTR-R1 L1), and a cap on the
//! entries the journal holds ([`PointerLimits::max_recorded`]). The same origin and hash
//! again is the same receipt and writes nothing. A pointer waits [`LIFETIME_SECS`] (the
//! application's own life), is shown as expired for [`KEPT_EXPIRED_SECS`] more and is then
//! removed from the journal, by the node's hourly task ([`run_prune`]), at the next pointer or
//! at the next start. The POST is taken only as JSON and never from a page of another site
//! ([`crate::json_from_this_site`], review PTR-R1 M1). The journal holds the origin, the hash, the receipt id and the
//! receipt time: never the client's address, and no line about a pointer goes to the log.

use crate::journal::{Binding, Journal};
use crate::submit::{client_of, no_store, Budgets};
use crate::{ApiError, ClientKey};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use onyx_bulk::ids::{Origin, OriginError};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex};

/// Largest request body, in bytes: an origin is at most 253 bytes and a hash 66.
pub const MAX_BODY: usize = 1024;
/// The journal's kind.
pub const JOURNAL_KIND: &str = "onyx-publisher-pointers";
/// The journal's binding: a pointer belongs to no chain or contract (the application it
/// names carries its own).
pub const BINDING: Binding = Binding {
    chain_id: 0,
    contract: [0; 20],
};
/// How long a pointer waits for the reviewer: the life of an application (ADR-0066 item 6).
pub const LIFETIME_SECS: u64 = onyx_bulk::application::LIFETIME_SECS;
/// How long an expired pointer's receipt still answers `expired` before the entry is removed.
pub const KEPT_EXPIRED_SECS: u64 = 6 * 86_400;
/// Most waiting pointers with different hashes for one host: every origin on a host shares
/// the count, so path variants of one host do not each get their own (review PTR-R1 L1).
pub const MAX_PER_ORIGIN: usize = 3;
/// Largest pointer journal the reviewer's tool reads, in bytes (the cap of 1,000 entries is
/// under 1 MiB with every removal line a compaction leaves behind).
pub const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
/// The tag of the log line that warns before the recorded cap.
pub const CAP_ALERT_TAG: &str = "onyx-pointer-cap-alert";
/// The state of a pointer the reviewer has not been late for.
pub const RECEIVED: &str = "received";
/// The state of a pointer older than [`LIFETIME_SECS`].
pub const EXPIRED: &str = "expired";
/// The reason code of [`EXPIRED`].
pub const EXPIRED_REASON: &str = "pointer_lifetime_over";
/// The state of a pointer the reviewer dropped: it no longer waits and holds no place.
pub const DROPPED: &str = "dropped";
/// The reason code of [`DROPPED`], the one reason a pointer is dropped for: the file at the
/// origin does not hash to the pointer's hash.
pub const DROPPED_REASON: &str = "application_hash_mismatch";
/// The kind of the reviewer's drops journal beside the pointer journal.
pub const DROPS_KIND: &str = "onyx-publisher-pointer-drops";
/// Largest drops journal the node and the tool read, in bytes.
pub const MAX_DROPS_BYTES: u64 = 1024 * 1024;
/// The tag of the log line that says the drops journal could not be read.
pub const DROPS_ALERT_TAG: &str = "onyx-pointer-drops-alert";
/// How often the node removes entries past their kept time ([`run_prune`]).
pub const PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(3_600);
/// Where the decision is, in every answer: the public status list, by the application's hash.
pub const DECISION_AT: &str = "see_status_list";
const MAX_UNIX: u64 = (1 << 53) - 1;
/// Ledger workers, including work whose HTTP handler was cancelled.
const MAX_WORKERS: usize = 8;
/// Status lookups in flight. Their own pool (review PTR-R3 L2): a burst of status reads must
/// not answer `pointer_busy` to someone who sends a pointer.
const MAX_READERS: usize = 16;

/// Budgets and caps. With the defaults one IPv4 address or one IPv6 /48 records at most 5
/// pointers a day and the server at most 50; an entry lives 12 days, so the budgets alone
/// keep the journal at 600 entries, under the cap of 1,000.
#[derive(Clone, Copy, Debug)]
pub struct PointerLimits {
    /// `POST /v1/publishers/pointer` per client per minute.
    pub post_per_minute: u32,
    /// `POST /v1/publishers/pointer` per client per UTC day.
    pub post_per_day: u32,
    /// `GET /v1/publishers/pointer/{id}` per client per minute.
    pub status_per_minute: u32,
    /// Pointers recorded per minute, all clients together.
    pub global_post_per_minute: u32,
    /// Pointers recorded per UTC day, all clients together.
    pub global_post_per_day: u32,
    /// Most entries the journal holds (waiting and expired); then 503 `pointer_full`.
    pub max_recorded: usize,
}

impl Default for PointerLimits {
    fn default() -> Self {
        PointerLimits {
            post_per_minute: 3,
            post_per_day: 5,
            status_per_minute: 30,
            global_post_per_minute: 6,
            global_post_per_day: 50,
            max_recorded: 1_000,
        }
    }
}

/// One recorded pointer: everything the node keeps of an applicant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointer {
    /// The receipt id: `ptr_` and 16 random bytes in hex.
    pub id: String,
    /// The origin, in the registered form.
    pub origin: String,
    /// The application file's Keccak-256, `0x` and 64 lowercase hex digits.
    pub application: String,
    /// When the node took it.
    pub received_unix: u64,
}

impl Pointer {
    /// When it stops waiting.
    pub fn expires_unix(&self) -> u64 {
        self.received_unix.saturating_add(LIFETIME_SECS)
    }

    /// [`RECEIVED`] or [`EXPIRED`] at `now`.
    pub fn state(&self, now: u64) -> &'static str {
        if now < self.expires_unix() {
            RECEIVED
        } else {
            EXPIRED
        }
    }

    /// Whether the entry is past its kept time at `now`: the node removes it, and until it
    /// has, nothing shows it any more.
    pub fn gone(&self, now: u64) -> bool {
        now >= self.expires_unix().saturating_add(KEPT_EXPIRED_SECS)
    }

    /// The host of its origin.
    pub fn host(&self) -> &str {
        self.origin.split('/').next().unwrap_or(&self.origin)
    }

    /// [`DROPPED`] when the reviewer dropped it, else [`Pointer::state`].
    pub fn state_with(&self, drops: &Drops, now: u64) -> &'static str {
        if drops.contains(&self.id) {
            DROPPED
        } else {
            self.state(now)
        }
    }

    /// The pointer as the two lines `onyx-publish verify-application --pointer FILE` reads
    /// (the lines of a pointer issue, ADR-0066 item 7).
    pub fn text(&self) -> String {
        format!(
            "origin: {}\napplication keccak: {}\n",
            self.origin, self.application
        )
    }

    fn entry(&self) -> Value {
        json!({"kind": "pointer", "version": 1, "id": self.id, "origin": self.origin,
            "application": self.application, "receivedUnix": self.received_unix})
    }
}

/// Why a pointer is not taken.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PointerError {
    /// Not the body's shape.
    #[error("a pointer is a JSON object with exactly the two text fields origin and application, each once ({0})")]
    Shape(&'static str),
    /// The origin is not in its grammar.
    #[error("{0}")]
    Origin(OriginError),
    /// The origin is on a shared host.
    #[error("{0}")]
    SharedHost(OriginError),
    /// The hash is not `0x` and 64 lowercase hex digits.
    #[error(
        "application is the Keccak-256 of the application file: 0x and 64 lowercase hex digits"
    )]
    Hash,
    /// The origin already has [`MAX_PER_ORIGIN`] waiting pointers with other hashes.
    #[error("this origin's host already has {MAX_PER_ORIGIN} pointers waiting with other hashes; a place is free again when the reviewer drops a pointer whose hash is not the file's, or when one expires. File the application as a public issue meanwhile")]
    OriginBusy,
    /// The journal is at its cap.
    #[error(
        "the list of pointers is full; try again later, or file the application as a public issue"
    )]
    Full,
    /// Storage failed; the operator must look.
    #[error("pointer storage is unavailable; try again later, or file the application as a public issue")]
    Unavailable,
}

impl PointerError {
    fn api(self) -> ApiError {
        let message = self.to_string();
        match self {
            PointerError::Shape(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "bad_pointer", message)
            }
            PointerError::Origin(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "bad_origin", message)
            }
            PointerError::Hash => {
                ApiError::new(StatusCode::BAD_REQUEST, "bad_application_hash", message)
            }
            PointerError::SharedHost(_) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "origin_shared_host",
                message,
            ),
            PointerError::OriginBusy => {
                ApiError::new(StatusCode::CONFLICT, "pointer_origin_busy", message)
            }
            PointerError::Full => {
                ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "pointer_full", message)
                    .with_header("retry-after", "3600".into())
            }
            PointerError::Unavailable => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pointer_unavailable",
                message,
            )
            .with_header("retry-after", "600".into()),
        }
    }
}

fn is_hash(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// Whether `s` has the shape of a receipt id.
pub fn is_id(s: &str) -> bool {
    s.strip_prefix("ptr_")
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

/// The two strings of a pointer, checked offline: the origin in its grammar and not on a
/// shared host, the hash in its shape. Returns the origin's one spelling.
pub fn check(origin: &str, application: &str) -> Result<String, PointerError> {
    let o = Origin::parse(origin).map_err(PointerError::Origin)?;
    if let Some(host) = o.shared_host() {
        return Err(PointerError::SharedHost(OriginError::SharedHost(host)));
    }
    if !is_hash(application) {
        return Err(PointerError::Hash);
    }
    Ok(o.text())
}

/// Parse a request body: at most [`MAX_BODY`] bytes of JSON, an object with exactly `origin`
/// and `application`, both text, neither twice; then [`check`]. Returns the two strings.
pub fn parse(bytes: &[u8]) -> Result<(String, String), PointerError> {
    if bytes.len() > MAX_BODY {
        return Err(PointerError::Shape("over the body cap"));
    }
    let v = onyx_bulk::application::strict_json(bytes)
        .map_err(|_| PointerError::Shape("not JSON, or a field stands twice"))?;
    let o = v.as_object().ok_or(PointerError::Shape("not an object"))?;
    if o.keys().any(|k| k != "origin" && k != "application") {
        return Err(PointerError::Shape("an unknown field"));
    }
    let text = |k: &str| o.get(k).and_then(Value::as_str);
    let (Some(origin), Some(application)) = (text("origin"), text("application")) else {
        return Err(PointerError::Shape("a field is missing or not text"));
    };
    Ok((check(origin, application)?, application.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    kind: String,
    version: u8,
    id: String,
    origin: String,
    application: String,
    received_unix: u64,
}

fn bad(why: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("invalid pointer journal record: {why}"),
    )
}

/// Replay the journal's live entries: each one checked whole again, as if it had just
/// arrived, with its id and its origin and hash each standing once. Anything else refuses
/// the journal: a line this code did not write is never shown to a reviewer as a pointer.
fn replay(entries: Vec<(u64, Value)>, capacity: usize) -> std::io::Result<BTreeMap<u64, Pointer>> {
    if entries.len() > capacity {
        return Err(bad("more entries than the cap"));
    }
    let mut out = BTreeMap::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut pairs = std::collections::BTreeSet::new();
    for (seq, value) in entries {
        let e: Entry = serde_json::from_value(value).map_err(|_| bad("not a pointer entry"))?;
        let spelled = check(&e.origin, &e.application).map_err(|_| bad("origin or hash"))?;
        if e.kind != "pointer"
            || e.version != 1
            || !is_id(&e.id)
            || spelled != e.origin
            || e.received_unix > MAX_UNIX
            || !ids.insert(e.id.clone())
            || !pairs.insert((e.origin.clone(), e.application.clone()))
        {
            return Err(bad("kind, id, time, or an entry that stands twice"));
        }
        out.insert(
            seq,
            Pointer {
                id: e.id,
                origin: e.origin,
                application: e.application,
                received_unix: e.received_unix,
            },
        );
    }
    Ok(out)
}

/// Read the pointers of the journal at `path` without its lock and without writing, oldest
/// first (the reviewer's tool beside a running node: `onyx-node pointers`). A missing file
/// reads as empty.
pub fn read_pointers(path: &FsPath, capacity: usize) -> std::io::Result<Vec<Pointer>> {
    let entries = Journal::read_only_capped(path, JOURNAL_KIND, &BINDING, MAX_JOURNAL_BYTES)?;
    Ok(replay(entries, capacity)?.into_values().collect())
}

/// The warning line, with its fixed tag, once the journal holds 80 percent of the recorded
/// cap or more ([`crate::submit::decisions::CAP_ALERT_PERCENT`]). Counts only.
pub fn cap_alert(recorded: usize, capacity: usize) -> Option<String> {
    use crate::submit::decisions::{fill_percent, CAP_ALERT_PERCENT};
    let percent = fill_percent(recorded, capacity);
    (percent >= CAP_ALERT_PERCENT).then(|| {
        format!(
            "{CAP_ALERT_TAG}: the pointer journal holds {recorded} of {capacity} entries ({percent}% of the recorded cap); at the cap new pointers are refused (503 pointer_full). Entries leave by themselves 12 days after they came (docs/ops/publisher-review.md \"Pointers sent to the node\")"
        )
    })
}

fn left(secs: u64) -> String {
    let (d, h) = (secs / 86_400, secs % 86_400 / 3_600);
    if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h")
    } else {
        format!("{} min", secs / 60)
    }
}

/// The receipt ids the reviewer dropped ([`drop_pointer`]).
pub type Drops = std::collections::BTreeSet<String>;

/// Where the drops journal of the pointer journal `journal` is: beside it, as
/// `<name>.drops.journal` for a name ending in `.journal`.
pub fn drops_path(journal: &FsPath) -> std::path::PathBuf {
    let name = journal
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.strip_suffix(".journal").unwrap_or(&name);
    journal.with_file_name(format!("{stem}.drops.journal"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DropEntry {
    kind: String,
    version: u8,
    id: String,
    reason: String,
    dropped_unix: u64,
}

fn drops_of(entries: Vec<(u64, Value)>) -> std::io::Result<BTreeMap<u64, String>> {
    let mut out = BTreeMap::new();
    for (seq, value) in entries {
        let e: DropEntry = serde_json::from_value(value).map_err(|_| bad("not a drop entry"))?;
        if e.kind != "drop"
            || e.version != 1
            || !is_id(&e.id)
            || e.reason != DROPPED_REASON
            || e.dropped_unix > MAX_UNIX
        {
            return Err(bad("a drop entry out of shape"));
        }
        out.insert(seq, e.id);
    }
    Ok(out)
}

/// Read the drops beside the pointer journal at `journal` without a lock and without writing.
/// A missing file is no drop; a damaged one is an error.
pub fn read_drops(journal: &FsPath) -> std::io::Result<Drops> {
    let entries =
        Journal::read_only_capped(drops_path(journal), DROPS_KIND, &BINDING, MAX_DROPS_BYTES)?;
    Ok(drops_of(entries)?.into_values().collect())
}

/// The reviewer's list: how full the journal is, then one line per waiting pointer, oldest
/// first (`all`: the expired and the dropped ones too). An entry past its kept time is never
/// shown, whether or not the node has removed it yet (review PTR-R1 L2). Every printed value
/// passed [`check`] on replay, so a line holds only lowercase letters, digits and `. / - _ :`.
pub fn render_list(
    pointers: &[Pointer],
    drops: &Drops,
    capacity: usize,
    now: u64,
    all: bool,
) -> String {
    let kept: Vec<&Pointer> = pointers.iter().filter(|p| !p.gone(now)).collect();
    let count = |state: &str| {
        kept.iter()
            .filter(|p| p.state_with(drops, now) == state)
            .count()
    };
    let mut out = format!(
        "{} pointer(s) waiting, {} expired and kept, {} dropped; the journal holds {} of {capacity} entries\n",
        count(RECEIVED),
        count(EXPIRED),
        count(DROPPED),
        pointers.len()
    );
    if let Some(line) = cap_alert(pointers.len(), capacity) {
        out.push_str(&format!("WARNING {line}\n"));
    }
    for p in kept {
        let state = p.state_with(drops, now);
        if state != RECEIVED && !all {
            continue;
        }
        let when = if state == RECEIVED {
            format!("{} left", left(p.expires_unix().saturating_sub(now)))
        } else {
            state.to_string()
        };
        out.push_str(&format!(
            "{}  {when}  {}  {}\n",
            p.id, p.origin, p.application
        ));
    }
    out
}

/// What [`undrop_pointer`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Undropped {
    /// The drop was taken back.
    Now,
    /// No drop of this id is held: nothing was written.
    NotDropped,
}

/// Take a drop back (review PTR-R2 L3): for a drop made by mistake (a wrong id typed, or a
/// file the applicant was still uploading when the tool fetched). The drop's line leaves the
/// drops journal; the node reads that at its next look, the receipt answers by its age again
/// (`received` or `expired`) and a waiting pointer holds a place of its host again. Works
/// beside the running node and never writes the pointer journal.
///
/// Refused when it would put the host over its places (review PTR-R3 N3): the place the drop
/// freed may have been taken since. A still-waiting pointer is taken back only while its
/// host holds fewer than [`MAX_PER_ORIGIN`] other waiting pointers; the reviewer drops one
/// of those first (if it is wrong) or leaves the drop. The check and the node's count are two
/// reads of one file, so a pointer that arrives in between can still make it one over; that
/// lasts until a pointer of the host expires or is dropped, and no later pointer is taken
/// for the host meanwhile.
pub fn undrop_pointer(
    journal: &FsPath,
    capacity: usize,
    id: &str,
    now: u64,
) -> Result<Undropped, DropError> {
    let path = drops_path(journal);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Undropped::NotDropped),
        Ok(m) if m.len() > MAX_DROPS_BYTES => {
            return Err(DropError::Storage(bad("the drops journal is over its cap")))
        }
        _ => {}
    }
    let (mut drops, entries) = Journal::open(&path, DROPS_KIND, &BINDING)?;
    let all = drops_of(entries)?;
    let held: Vec<u64> = all
        .iter()
        .filter(|(_, d)| *d == id)
        .map(|(seq, _)| *seq)
        .collect();
    if held.is_empty() {
        if drops.dead() > 0 {
            drops.compact()?;
        }
        return Ok(Undropped::NotDropped);
    }
    let pointers = read_pointers(journal, capacity)?;
    if let Some(p) = pointers
        .iter()
        .find(|p| p.id == id && p.state(now) == RECEIVED)
    {
        let waiting = pointers
            .iter()
            .filter(|o| {
                o.id != id
                    && o.host() == p.host()
                    && o.state(now) == RECEIVED
                    && !all.values().any(|d| *d == o.id)
            })
            .count();
        if waiting >= MAX_PER_ORIGIN {
            return Err(DropError::HostFull);
        }
    }
    drops.apply(&held, &[])?;
    // As for a removed pointer: the taken-back line does not stay in the file.
    drops.compact()?;
    Ok(Undropped::Now)
}

/// What [`drop_pointer`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The drop was written.
    Now,
    /// The pointer was dropped before: nothing was written.
    Already,
}

/// Why [`drop_pointer`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum DropError {
    /// No waiting pointer has the id (unknown, expired, or past its kept time).
    #[error("no waiting pointer has this id")]
    NotWaiting,
    /// An undrop would give the host more waiting pointers than it may hold.
    #[error("the host of this pointer already holds {MAX_PER_ORIGIN} waiting pointers; drop one whose hash is not the file's first")]
    HostFull,
    /// A journal could not be read or written.
    #[error("{0}")]
    Storage(#[from] std::io::Error),
}

/// The reviewer's release of a place (review PTR-R1 M2): mark the waiting pointer `id` as
/// dropped because the file at its origin does not hash to the pointer's hash (the tool's
/// first check said so, on a throwaway machine). Works beside the running node: the pointer
/// journal is only read; the drop is one line in the drops journal beside it, which the node
/// reads again when it changes. A dropped pointer holds no place of its host, its receipt
/// answers [`DROPPED`], and the same origin and hash sent again is that receipt, not a new
/// pointer. Drops of pointers the node has removed are taken out of the drops journal here.
pub fn drop_pointer(
    journal: &FsPath,
    capacity: usize,
    id: &str,
    now: u64,
) -> Result<Dropped, DropError> {
    let pointers = read_pointers(journal, capacity)?;
    let known: Drops = pointers.iter().map(|p| p.id.clone()).collect();
    let waiting = pointers
        .iter()
        .any(|p| p.id == id && p.state(now) == RECEIVED);
    if !waiting {
        return Err(DropError::NotWaiting);
    }
    let path = drops_path(journal);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.len() > MAX_DROPS_BYTES) {
        return Err(DropError::Storage(bad("the drops journal is over its cap")));
    }
    let (mut drops, entries) = Journal::open(&path, DROPS_KIND, &BINDING)?;
    let held = drops_of(entries)?;
    if held.values().any(|d| d == id) {
        return Ok(Dropped::Already);
    }
    let stale: Vec<u64> = held
        .iter()
        .filter(|(_, d)| !known.contains(*d))
        .map(|(seq, _)| *seq)
        .collect();
    let entry = json!({"kind": "drop", "version": 1, "id": id, "reason": DROPPED_REASON,
        "droppedUnix": now.min(MAX_UNIX)});
    drops.apply(&stale, &[entry])?;
    // Nothing of a taken-out drop stays in the file (also one a crash left there before).
    if drops.dead() > 0 {
        drops.compact()?;
    }
    Ok(Dropped::Now)
}

/// What the drops journal looked like when the node last read it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stamp {
    Absent,
    File(u64, Option<std::time::SystemTime>),
}

fn stamp(path: &FsPath) -> Stamp {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Stamp::File(m.len(), m.modified().ok()),
        Err(_) => Stamp::Absent,
    }
}

struct Ledger {
    journal: Option<Journal>,
    pointers: BTreeMap<u64, Pointer>,
    drops_file: std::path::PathBuf,
    /// The drops as last read, and the stamp of that read (`None`: never looked).
    drops: Drops,
    drops_seen: Option<Stamp>,
}

impl Ledger {
    /// Read the reviewer's drops again when their file changed. A file that cannot be read
    /// keeps the last good drops; the line for the log comes back once per change.
    fn refresh_drops(&mut self) -> Option<String> {
        let now = stamp(&self.drops_file);
        if self.drops_seen == Some(now) {
            return None;
        }
        self.drops_seen = Some(now);
        let read =
            Journal::read_only_capped(&self.drops_file, DROPS_KIND, &BINDING, MAX_DROPS_BYTES)
                .and_then(drops_of);
        match read {
            Ok(d) => {
                self.drops = d.into_values().collect();
                None
            }
            Err(_) => Some(format!(
                "{DROPS_ALERT_TAG}: the drops journal beside the pointer journal cannot be read; the node keeps the {} drop(s) it read last (docs/ops/publisher-review.md \"Pointers sent to the node\")",
                self.drops.len()
            )),
        }
    }

    /// Remove the entries whose kept time is over. A failed write closes the journal.
    fn prune(&mut self, now: u64) -> Result<usize, PointerError> {
        let old: Vec<u64> = self
            .pointers
            .iter()
            .filter(|(_, p)| p.gone(now))
            .map(|(seq, _)| *seq)
            .collect();
        if old.is_empty() {
            return Ok(0);
        }
        let journal = self.journal.as_mut().ok_or(PointerError::Unavailable)?;
        if journal.apply(&old, &[]).is_err() {
            self.journal = None;
            return Err(PointerError::Unavailable);
        }
        for seq in &old {
            self.pointers.remove(seq);
        }
        // Review PTR-R2 L1: a removal is a `del` line; the line that added the entry, with
        // its origin and hash, would stay in the file until 1,024 removals had piled up. So
        // the file is written anew from what is left, now: "removed" means gone from disk.
        if journal.compact().is_err() {
            self.journal = None;
            return Err(PointerError::Unavailable);
        }
        Ok(old.len())
    }
}

/// The pointer store: the journal, its entries and the budgets.
pub struct PointerStore {
    ledger: Mutex<Ledger>,
    budgets: Budgets,
    workers: Arc<tokio::sync::Semaphore>,
    readers: Arc<tokio::sync::Semaphore>,
    limits: PointerLimits,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// Where the warning before the recorded cap goes (the node's log).
    cap_alert: Arc<dyn Fn(&str) + Send + Sync>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A new receipt id from the system's random source: it is not derived from the origin or
/// the hash, which are public, so nobody can compute the receipt of someone else's pointer.
fn new_id() -> std::io::Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))?;
    Ok(format!("ptr_{}", hex::encode(b)))
}

impl PointerStore {
    /// Open (or create) the journal at `path`, replay it and remove what is past its kept
    /// time. A damaged journal, one held by another process or one over the cap refuses.
    pub fn open(path: impl AsRef<FsPath>, limits: PointerLimits) -> std::io::Result<PointerStore> {
        Self::open_at(path, limits, Arc::new(now_secs))
    }

    /// [`PointerStore::open`] with the clock that says what time it is (tests).
    pub fn open_at(
        path: impl AsRef<FsPath>,
        limits: PointerLimits,
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> std::io::Result<PointerStore> {
        let drops_file = drops_path(path.as_ref());
        let (journal, entries) = Journal::open(path, JOURNAL_KIND, &BINDING)?;
        let mut ledger = Ledger {
            journal: Some(journal),
            pointers: replay(entries, limits.max_recorded)?,
            drops_file,
            drops: Drops::new(),
            drops_seen: None,
        };
        ledger
            .prune(clock())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        // Review PTR-R3 L1: a removal is two steps, the `del` line and then the rewrite. A
        // crash, or a failed rewrite, between them leaves the removed origin and hash in the
        // file with nothing left to expire, and a journal written before the rewrite existed
        // is in the same state. So a start rewrites any journal that still holds a removed
        // entry, whether or not this start removed one.
        if let Some(journal) = ledger.journal.as_mut() {
            if journal.dead() > 0 {
                journal.compact()?;
            }
        }
        Ok(PointerStore {
            ledger: Mutex::new(ledger),
            budgets: Budgets::new(),
            workers: Arc::new(tokio::sync::Semaphore::new(MAX_WORKERS)),
            readers: Arc::new(tokio::sync::Semaphore::new(MAX_READERS)),
            limits,
            clock,
            cap_alert: Arc::new(|line| eprintln!("onyx-node: {line}")),
        })
    }

    /// Send the warning before the recorded cap to `sink` instead of the log (tests).
    pub fn with_cap_alert(mut self, sink: Arc<dyn Fn(&str) + Send + Sync>) -> Self {
        self.cap_alert = sink;
        self
    }

    /// Warn when the journal is at or over 80 percent of the recorded cap: the node calls
    /// this once at start; recording calls it at each further percent.
    pub fn warn_if_near_cap(&self) -> Option<String> {
        let line = cap_alert(self.recorded(), self.limits.max_recorded)?;
        (self.cap_alert)(&line);
        Some(line)
    }

    /// Number of entries the journal holds.
    pub fn recorded(&self) -> usize {
        self.ledger
            .lock()
            .map(|l| l.pointers.len())
            .unwrap_or(self.limits.max_recorded)
    }

    /// Remove every entry past its kept time now, whether or not a pointer arrives (review
    /// PTR-R1 L2). Returns how many left. Blocking (a journal write).
    pub fn prune(&self) -> Result<usize, PointerError> {
        let now = (self.clock)();
        self.ledger
            .lock()
            .map_err(|_| PointerError::Unavailable)?
            .prune(now)
    }

    fn drops_refreshed(&self, ledger: &mut Ledger) {
        if let Some(line) = ledger.refresh_drops() {
            (self.cap_alert)(&line);
        }
    }

    /// Take a checked pointer from `client`. The same origin and hash again is the entry
    /// that is there (`false`: nothing was written, whatever its state).
    ///
    /// Budgets (review PTR-R1 L4): a repeat costs the client one of its day. A new pointer
    /// takes the client's day and the node's day as one step, all or none, and then the
    /// node's minute; when the minute refuses, the day's units are given back. So a pointer
    /// the node's budgets refuse costs the client nothing of its day.
    fn record(
        &self,
        origin: String,
        application: String,
        client: &str,
    ) -> Result<(Pointer, bool), ApiError> {
        let now = (self.clock)();
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| PointerError::Unavailable.api())?;
        ledger.prune(now).map_err(PointerError::api)?;
        self.drops_refreshed(&mut ledger);
        let (limits, what) = (&self.limits, "pointers");
        if let Some(p) = ledger
            .pointers
            .values()
            .find(|p| p.origin == origin && p.application == application)
        {
            self.budgets
                .take_day("pointer-day", client, limits.post_per_day, None, what)?;
            return Ok((p.clone(), false));
        }
        let host = origin.split('/').next().unwrap_or(&origin);
        let waiting = ledger
            .pointers
            .values()
            .filter(|p| p.host() == host && p.state_with(&ledger.drops, now) == RECEIVED)
            .count();
        if waiting >= MAX_PER_ORIGIN {
            return Err(PointerError::OriginBusy.api());
        }
        if ledger.pointers.len() >= limits.max_recorded {
            return Err(PointerError::Full.api());
        }
        if now > MAX_UNIX {
            return Err(PointerError::Unavailable.api());
        }
        self.budgets.take_day(
            "pointer-day",
            client,
            limits.post_per_day,
            Some(limits.global_post_per_day),
            what,
        )?;
        if let Err(e) = self.budgets.take_global(
            "pointer-all",
            limits.global_post_per_minute,
            "pointers on this server",
        ) {
            self.budgets.refund_day("pointer-day", client, true);
            return Err(e);
        }
        let pointer = Pointer {
            id: new_id().map_err(|_| PointerError::Unavailable.api())?,
            origin,
            application,
            received_unix: now,
        };
        let journal = ledger
            .journal
            .as_mut()
            .ok_or_else(|| PointerError::Unavailable.api())?;
        let seq = match journal.apply(&[], &[pointer.entry()]) {
            Ok(ids) if ids.len() == 1 => ids[0],
            _ => {
                ledger.journal = None;
                return Err(PointerError::Unavailable.api());
            }
        };
        ledger.pointers.insert(seq, pointer.clone());
        let (recorded, cap) = (ledger.pointers.len(), limits.max_recorded);
        if crate::submit::decisions::cap_alert_due(recorded, cap) {
            if let Some(line) = cap_alert(recorded, cap) {
                (self.cap_alert)(&line);
            }
        }
        Ok((pointer, true))
    }

    /// The answer for the receipt `id`, if the node still shows it.
    fn find(&self, id: &str) -> Result<Option<Value>, ApiError> {
        let now = (self.clock)();
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| PointerError::Unavailable.api())?;
        self.drops_refreshed(&mut ledger);
        Ok(ledger
            .pointers
            .values()
            .find(|p| p.id == id && !p.gone(now))
            .map(|p| answer(p, &ledger.drops, now)))
    }

    fn answer(&self, p: &Pointer) -> Result<Value, ApiError> {
        let ledger = self
            .ledger
            .lock()
            .map_err(|_| PointerError::Unavailable.api())?;
        Ok(answer(p, &ledger.drops, (self.clock)()))
    }

    fn permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
        Self::permit_of(&self.workers)
    }

    /// A place among the status lookups, which never takes one from the writes.
    fn read_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
        Self::permit_of(&self.readers)
    }

    fn permit_of(
        pool: &Arc<tokio::sync::Semaphore>,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
        pool.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "pointer_busy",
                "pointer storage is busy; try again later",
            )
            .with_header("retry-after", "5".into())
        })
    }
}

fn answer(p: &Pointer, drops: &Drops, now: u64) -> Value {
    let state = p.state_with(drops, now);
    let mut body = json!({"id": p.id, "status": state, "expiresUnix": p.expires_unix(),
        "decision": DECISION_AT});
    if state == EXPIRED {
        body["reason"] = EXPIRED_REASON.into();
    } else if state == DROPPED {
        body["reason"] = DROPPED_REASON.into();
    }
    body
}

/// The node's task that removes entries past their kept time every `every`, so a node that
/// takes no pointer for weeks still keeps an origin and a hash no longer than it says (review
/// PTR-R1 L2). It holds the store and runs for the node's whole life (the node ends it by
/// ending its runtime; a test aborts it). A failed write closes the journal, and the next
/// pointer is then answered 503.
pub async fn run_prune(store: Arc<PointerStore>, every: std::time::Duration) {
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let st = store.clone();
        let _ = tokio::task::spawn_blocking(move || st.prune()).await;
    }
}

/// The pointer routes, to pass to [`crate::router_with`] (behind the API's guard: one
/// concurrency limit, the deadline, the client key from the trusted proxies only).
pub fn routes(store: Arc<PointerStore>) -> Router {
    Router::new()
        .route("/v1/publishers/pointer", post(send))
        .route("/v1/publishers/pointer/{id}", get(status))
        .with_state(store)
}

async fn send(
    State(store): State<Arc<PointerStore>>,
    client: Option<Extension<ClientKey>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // Review PTR-R1 M1: JSON only and never from a page of another site, before any budget
    // is counted: a visitor's browser must not spend the visitor's budget for someone else.
    if let Err(e) = crate::json_from_this_site(&headers) {
        return e.into_response();
    }
    send_inner(store, client_of(client), body)
        .await
        .map_or_else(IntoResponse::into_response, no_store)
}

async fn send_inner(
    store: Arc<PointerStore>,
    client: String,
    body: Body,
) -> Result<Response, ApiError> {
    store.budgets.take(
        "pointer",
        &client,
        store.limits.post_per_minute,
        "pointers from this address",
    )?;
    let bytes = axum::body::to_bytes(body, MAX_BODY).await.map_err(|_| {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
            format!("a pointer is at most {MAX_BODY} bytes"),
        )
    })?;
    let (origin, application) = parse(&bytes).map_err(PointerError::api)?;
    let permit = store.permit()?;
    let st = store.clone();
    let (body, new) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let (pointer, new) = st.record(origin, application, &client)?;
        Ok::<_, ApiError>((st.answer(&pointer)?, new))
    })
    .await
    .map_err(|_| crate::internal("pointer worker failed"))??;
    let status = if new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(body)).into_response())
}

async fn status(
    State(store): State<Arc<PointerStore>>,
    client: Option<Extension<ClientKey>>,
    Path(id): Path<String>,
) -> Response {
    let r = (|| {
        store.budgets.take(
            "pointer-status",
            &client_of(client),
            store.limits.status_per_minute,
            "status reads from this address",
        )?;
        let not_found = || {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no pointer with this id (a receipt is kept 12 days)",
            )
        };
        if !is_id(&id) {
            return Err(not_found());
        }
        Ok(not_found)
    })();
    let not_found = match r {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    // Review PTR-R2 N1: the lookup may read the drops journal again (a file read), so it
    // runs on a blocking thread like a write, never on the thread that serves requests.
    let permit = match store.read_permit() {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let st = store.clone();
    let found = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        st.find(&id)
    })
    .await;
    let r = match found {
        Ok(Ok(Some(body))) => Ok((StatusCode::OK, Json(body)).into_response()),
        Ok(Ok(None)) => Err(not_found()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(crate::internal("pointer lookup failed")),
    };
    r.map_or_else(IntoResponse::into_response, no_store)
}

#[cfg(test)]
#[path = "publishers_tests.rs"]
mod tests;
