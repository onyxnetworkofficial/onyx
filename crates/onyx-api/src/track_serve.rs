//! Level-5 track records, served (ROADMAP C6; docs/spec/delivery.md "Track
//! records"; ADR-0036 items 10-12).
//!
//! - `GET /v1/track` lists what is published: the latest commitment, the
//!   close-time rules and one line per record, or why there is none.
//! - `GET /v1/track/{source}/{metric}[?since=0x<commitment>]` serves one record
//!   with its proofs and whole history in the form [`onyx_track::wire`] defines;
//!   clients check it with [`onyx_track::wire::check_record_body`].
//! - `GET /v1/track/{source}/{metric}/history?at=0x<commitment>&from=<n>` serves
//!   one page of a history too large for one body (ROADMAP C20).
//! - `GET /v1/track/log[?since=0x<commitment>]` serves the logs (params,
//!   statements, resolutions) and the intake view of the publication, so a
//!   client reruns `compute` and `verify_log` (ROADMAP C20, ADR-0036
//!   amendment 5): free only when no answer in them is priced (level 1 or 2)
//!   and, in `ONYX_RIGHTS=fallback`, no ruled metric is label-derived.
//!   Chained (format-2) logs answer a head there and their rows in pages:
//!   `?at=<latest>&from=<n>&since=<first link>` and the intake's posts with
//!   `?at=<latest>&posts=<n>` (ROADMAP C21, ADR-0036 amendment 6).
//! - `GET /v1/track/registry` serves the registry the latest commitment names,
//!   as data, so a client rebuilds it, checks it hashes to `registry_root`
//!   and reruns `compute` (ROADMAP C23); withheld with the logs in fallback,
//!   and whenever it names an address (review C23-R2-2).
//!
//! A node fills its store from the coordinator's publication file
//! (`ONYX_TRACK_PUBLICATIONS`, [`node_routes`]), re-read when it changes; a
//! file that does not check, or that does not start with what was served,
//! stops every track answer (503 `track_store_unreadable`).
//!
//! Only what was published is served: no publication, no rule for the metric or
//! no statement by the source is a 404 `no_track_record` with a `reason`, never
//! a made-up record. Today no node has a publication to read: the Bitcoin flow
//! metrics have a reviewed close-time rule (docs/spec/proof-ladder.md
//! "Close-time rules", C9) but no source states on them and their answers
//! cannot be read from the chain yet (C40), so a node serves only 404s here. Ids of the example (`example.*`) are published only by a store built
//! with [`TrackStore::allowing_examples`] (tests), never by a node.
//!
//! Fenced (scripts/check-boundaries.sh, THREATS TR-11): this file names no
//! level or market-policy code, and no other onyx-api file names it except
//! lib.rs's `pub mod track_serve;`. The routes join the API as an opaque
//! `Router` through [`crate::router_with`], so nothing on the pricing, payment,
//! settlement or catalog path can read a record. Track records are free.

use crate::cache::Caches;
use crate::catalog::LabelRights;
use crate::{bad_request, rate_scopes, ApiError, ClientKey};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use onyx_core::ids::is_dotted_id;
use onyx_core::{keccak256, Hash32, MetricId, NoRules, Registry, SourceId, StaticRegistry};
use onyx_track::chained::{stream_rows, LOG_FORMAT_CHAINED, LOG_FORMAT_WHOLE};
use onyx_track::example::is_example_id;
use onyx_track::intake::PublishedIntake;
use onyx_track::publication::{
    chained_check_cost, log_check_cost, Publication, PublicationError, MAX_LOG_CHECK,
};
use onyx_track::wire::{
    chained_log_body, history_page_body, log_body, log_posts_page, log_rows_page, record_body,
    registry_body, Served, MAX_CHAIN, MAX_LOG_PAGE_BYTES, MAX_LOG_ROWS,
};
use onyx_track::{
    compute, Close, ExtendError, Resolution, Statement, TrackBook, TrackCommitment, TrackError,
    TrackParams,
};
use serde_json::{json, Value};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

/// Free record answers per client per minute (same burst); 429 beyond. A
/// revalidation that ends in 304 costs nothing.
pub const FREE_PER_MINUTE: u32 = 30;
/// Free log answers per client per minute (same burst): a log body can be
/// tens of megabytes, so it has its own, smaller budget (THREATS TR-C20-5).
pub const LOG_FREE_PER_MINUTE: u32 = 2;
/// Free pages of chained logs per client per minute (same burst): a page is
/// at most 4,096 rows (or one batch), so a client pages a log of a million
/// rows in about four minutes (ROADMAP C21).
pub const LOG_PAGES_PER_MINUTE: u32 = 60;
/// Records listed per page of `GET /v1/track[?from=n]` (`next` names the
/// following page; ROADMAP C20, review TR-S-R2-3).
pub const MAX_LISTED: usize = 1024;
/// Largest publication file a node reads (bytes).
pub const MAX_FILE_BYTES: u64 = 256 << 20;
/// Most checking one install of a publication file does
/// ([`Publication::check_cost`]: for format 1 the steps it checks times the
/// logs' rows, for chained logs the rows plus the steps): about 20 s of
/// native work (review C20-R2-2). Past it the file is refused (503). With
/// format 1 a long chain over a large log reaches it; chained logs (ROADMAP
/// C21) are checked in one linear pass, so only past about four million rows.
pub const MAX_INSTALL_CHECK: u64 = 1 << 22;

/// Why a node has published nothing (C9): the Bitcoin flow metrics have a
/// reviewed close-time rule now, but no source states through the intake yet
/// and their level-1 answers cannot be read from the chain yet (ROADMAP C40).
pub const NOTHING_PUBLISHED: &str = "no track record has been published: no source states on a metric with a close-time rule yet, and the ruled served metrics (Bitcoin entity flows) have no on-chain answer reader yet";

/// Why `GET /v1/track/...` has no record (the `reason` of a 404 `no_track_record`).
pub const NONE_PUBLISHED: &str = "none_published";
/// The metric has no close-time rule in the published params.
pub const NO_RULE: &str = "no_rule";
/// The metric has a rule, but the source stated nothing on it.
pub const NO_RECORD: &str = "no_record";

/// Why a publication was refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// An example id (`example.*`) in a store that does not allow examples.
    #[error("example id {0} is published only by a test store")]
    Example(String),
    /// The inputs fail [`onyx_track::compute`].
    #[error(transparent)]
    Compute(#[from] TrackError),
    /// Not an extension of the published commitment (a rollback, a fork, a
    /// rewritten history; THREATS TR-R2-5, TR-S-1).
    #[error("does not extend the published commitment: {0}")]
    NotAnExtension(#[from] ExtendError),
    /// A publication file that does not start with what was served (a
    /// rewritten or truncated file; THREATS TR-S-1, TR-C20-2).
    #[error("the publication does not start with what was served")]
    Rewritten,
    /// A publication that does not check ([`Publication::check`]).
    #[error(transparent)]
    Publication(PublicationError),
    /// The served head could not be read or kept.
    #[error("{0}")]
    Head(String),
    /// Checking the file would cost more than [`MAX_INSTALL_CHECK`].
    #[error("the publication would take more than {MAX_INSTALL_CHECK} row checks to check")]
    TooLarge,
}

impl From<PublicationError> for PublishError {
    fn from(e: PublicationError) -> Self {
        match e {
            PublicationError::Extend(_, e) => PublishError::NotAnExtension(e),
            e => PublishError::Publication(e),
        }
    }
}

/// One publication: the commitment chain with its logs, and the latest book.
struct Published {
    /// The chain, the logs (publication order) and the intake view.
    file: Publication,
    /// Every commitment published, oldest first; the last is this book's.
    chain: Vec<TrackCommitment>,
    /// Their hashes, in the same order (computed once at install, so a page
    /// or a 409 never rehashes the chain; review C21-R1-3).
    hashes: Vec<Hash32>,
    /// The latest params.
    params: TrackParams,
    book: TrackBook,
    /// Rule metrics whose records are label-derived (withheld in fallback), sorted.
    label_derived: Vec<MetricId>,
    /// [`Publication::priced_answers`], counted once at install.
    priced: usize,
}

/// What identifies one version of the publication file: a rename (the
/// coordinator's write) gives a new inode, so a same-length rewrite within one
/// timestamp tick is still seen (review C20-R1-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
    inode: u64,
    changed: (i64, i64),
}

/// The file version last read, and what that read gave.
type FileRead = (Stamp, Result<(), String>);

/// Where a node's store reads its publication (`ONYX_TRACK_PUBLICATIONS`).
struct FileSource {
    path: PathBuf,
    /// Held for a whole read; a request never waits on it (review C20-R2-3).
    seen: Mutex<Option<FileRead>>,
    /// The result of the last finished read, for requests that arrive while
    /// another read is running.
    last: Mutex<Result<(), String>>,
}

/// The published track records of one server.
pub struct TrackStore {
    rights: LabelRights,
    allow_examples: bool,
    free_per_minute: u32,
    log_per_minute: u32,
    pages_per_minute: u32,
    /// The log format [`TrackStore::publish`] starts a chain with.
    format: u8,
    limits: Caches,
    published: RwLock<Option<Arc<Published>>>,
    file: Option<FileSource>,
    /// Where the head of what was served is kept (a node: `ONYX_STATE_DIR`).
    head: Option<PathBuf>,
    /// Why every track answer is a 503 whatever is published (a node whose
    /// state directory holds a served head but that names no publication
    /// file, review C20-R2-5).
    refusing: Option<String>,
}

/// The head of what a store served: its step count, latest commitment, log
/// row counts and a digest of that prefix ([`prefix_digest`]). A file read
/// after a restart must start with the same bytes ([`Publication::starts_with`]
/// across restarts), so only the steps after it are checked again (review
/// C20-R2-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, borsh::BorshSerialize, borsh::BorshDeserialize)]
struct ServedHead {
    steps: u64,
    latest: Hash32,
    statements: u64,
    resolutions: u64,
    prefix: Hash32,
}

/// Domain of [`prefix_digest`].
const PREFIX_DOMAIN: &[u8] = b"onyx-track-served-prefix-v1";

/// Keccak of the first `steps` steps and the first `statements` and
/// `resolutions` rows of `p` (what [`Publication::starts_with`] compares), or
/// `None` when `p` is shorter.
fn prefix_digest(p: &Publication, steps: u64, statements: u64, resolutions: u64) -> Option<Hash32> {
    let n = |x: u64| usize::try_from(x).ok();
    let st = p.steps.get(..n(steps)?)?;
    let ss = p.statements.get(..n(statements)?)?;
    let rs = p.resolutions.get(..n(resolutions)?)?;
    let mut bytes = PREFIX_DOMAIN.to_vec();
    for part in [borsh::to_vec(st), borsh::to_vec(ss), borsh::to_vec(rs)] {
        bytes.extend(part.ok()?);
    }
    Some(keccak256(&bytes))
}

impl ServedHead {
    fn of(p: &Publication) -> Option<ServedHead> {
        let (steps, statements, resolutions) = (
            p.steps.len() as u64,
            p.statements.len() as u64,
            p.resolutions.len() as u64,
        );
        Some(ServedHead {
            steps,
            latest: p.latest()?.commitment.hash(),
            statements,
            resolutions,
            prefix: prefix_digest(p, steps, statements, resolutions)?,
        })
    }

    /// True when `p` starts with what this head names.
    fn starts(&self, p: &Publication) -> bool {
        let at = usize::try_from(self.steps)
            .ok()
            .and_then(|n| n.checked_sub(1));
        at.and_then(|i| p.steps.get(i))
            .is_some_and(|s| s.commitment.hash() == self.latest)
            && prefix_digest(p, self.steps, self.statements, self.resolutions) == Some(self.prefix)
    }
}

/// File name of the served head under a node's state directory.
pub const SERVED_HEAD_FILE: &str = "track-served-head.borsh";

/// True if records on `metric` are label-derived: the registry says the metric
/// needs a label set, or a segment of its name starts with `entity` (the
/// catalog's name rule, `catalog::ShardMeta::label_derived`). Fail closed: an
/// unregistered metric counts as label-derived.
pub fn label_derived(reg: &dyn Registry, metric: &MetricId) -> bool {
    reg.metric(&metric.0).is_none_or(|m| m.needs_label_set) || entity_named(&metric.0)
}

fn entity_named(metric: &str) -> bool {
    metric
        .split(['.', '/', ':'])
        .any(|seg| seg.to_ascii_lowercase().starts_with("entity"))
}

impl TrackStore {
    /// An empty store serving under `rights` (`ONYX_RIGHTS`).
    pub fn new(rights: LabelRights) -> Self {
        TrackStore {
            rights,
            allow_examples: false,
            free_per_minute: FREE_PER_MINUTE,
            log_per_minute: LOG_FREE_PER_MINUTE,
            pages_per_minute: LOG_PAGES_PER_MINUTE,
            format: LOG_FORMAT_WHOLE,
            limits: Caches::new(1, 1),
            published: RwLock::new(None),
            file: None,
            head: None,
            refusing: None,
        }
    }

    /// A node's store: read from the coordinator's publication file at
    /// `path` now and whenever it changes ([`TrackStore::refresh`]), never
    /// behind the head recorded at `head` (the last commitment it served,
    /// kept across restarts). A missing file publishes nothing yet; a file
    /// that does not check stops every track answer (503), never the node
    /// (review C20-R1-4). Returns the store and the first read's result.
    pub fn from_file(
        rights: LabelRights,
        path: PathBuf,
        head: PathBuf,
    ) -> (Self, Result<(), String>) {
        TrackStore::new(rights).remembering(head).reading(path)
    }

    /// Keep the head of what this store serves in the file `head` (Borsh
    /// steps and latest commitment hash, written whole by rename) and refuse
    /// any later publication, after a restart too, that does not extend it
    /// (review C20-R1-1). Call before [`TrackStore::reading`].
    pub fn remembering(mut self, head: PathBuf) -> Self {
        self.head = Some(head);
        self
    }

    /// This store, read from the publication file at `path` (see
    /// [`TrackStore::from_file`]), with the first read's result.
    pub fn reading(mut self, path: PathBuf) -> (Self, Result<(), String>) {
        self.file = Some(FileSource {
            path,
            seen: Mutex::new(None),
            last: Mutex::new(Ok(())),
        });
        let first = self.refresh();
        (self, first)
    }

    /// The head this store served last, from its head file (`None` without
    /// one or before anything was served).
    fn served_head(&self) -> Result<Option<ServedHead>, String> {
        let Some(path) = &self.head else {
            return Ok(None);
        };
        match std::fs::read(path) {
            Ok(b) => borsh::from_slice(&b)
                .map(Some)
                .map_err(|e| format!("track served head: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("track served head: {e}")),
        }
    }

    fn keep_head(&self, h: &ServedHead) -> Result<(), String> {
        let Some(path) = &self.head else {
            return Ok(());
        };
        let tmp = path.with_extension("tmp");
        let bytes = borsh::to_vec(h).map_err(|e| format!("track served head: {e}"))?;
        // Synced before the rename, so a power cut leaves the old head or the
        // new one, never an empty file (review C20-R2-5).
        std::fs::File::create(&tmp)
            .and_then(|mut f| {
                std::io::Write::write_all(&mut f, &bytes)?;
                f.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("track served head: {e}"))
    }

    /// Accept the example's ids (tests and the website fixture only; a node
    /// never sets it).
    pub fn allowing_examples(mut self) -> Self {
        self.allow_examples = true;
        self
    }

    /// Free answers per client per minute (tests).
    pub fn with_free_per_minute(mut self, n: u32) -> Self {
        self.free_per_minute = n.max(1);
        self
    }

    /// Free log answers per client per minute (tests).
    pub fn with_log_per_minute(mut self, n: u32) -> Self {
        self.log_per_minute = n.max(1);
        self
    }

    /// Free log pages per client per minute (tests).
    pub fn with_pages_per_minute(mut self, n: u32) -> Self {
        self.pages_per_minute = n.max(1);
        self
    }

    /// [`TrackStore::publish`] starts chains with chained (format-2) logs
    /// (tests; a node serves whatever format its file has).
    pub fn chaining(mut self) -> Self {
        self.format = LOG_FORMAT_CHAINED;
        self
    }

    /// Publish the computation of `params` over the logs at `as_of`. The first
    /// publication starts the chain (`prev` zero); every later one must extend
    /// the published one ([`onyx_track::check_extends`]: chained, later, params
    /// and every earlier statement and resolution kept), so the store never
    /// goes back to an older commitment or forks. Returns the new commitment.
    pub fn publish(
        &self,
        params: TrackParams,
        reg: &StaticRegistry,
        statements: Vec<Statement>,
        resolutions: Vec<Resolution>,
        as_of: u64,
    ) -> Result<TrackCommitment, PublishError> {
        self.publish_with_intake(params, reg, statements, resolutions, as_of, None)
    }

    /// [`TrackStore::publish`] with the intake as published at the
    /// commitment's head (required when the params name an intake).
    pub fn publish_with_intake(
        &self,
        params: TrackParams,
        reg: &StaticRegistry,
        statements: Vec<Statement>,
        resolutions: Vec<Resolution>,
        as_of: u64,
        intake: Option<PublishedIntake>,
    ) -> Result<TrackCommitment, PublishError> {
        let book = compute(&params, reg, &statements, &resolutions, as_of)?;
        // The registry travels with the publication (ROADMAP C23).
        let mut file = self
            .current()
            .map_or_else(|| Publication::with_format(self.format), |p| p.file.clone());
        let next = file.push_with(&params, &statements, &resolutions, &book, intake, Some(reg))?;
        self.install(file, Some(reg))?;
        Ok(next)
    }

    /// Serve `file` (a coordinator's publication): it must check
    /// ([`Publication::check`]: a chain from its first commitment, every log
    /// matching its commitment and extending the one before, the rows giving
    /// the latest commitment, the intake at its head) and start with what was
    /// served before ([`Publication::starts_with`]). Without a registry the
    /// file's own is used when it rebuilds with no address rules to the latest
    /// commitment's root; failing that every ruled metric counts as
    /// label-derived (withheld in fallback; fail closed).
    pub fn install(
        &self,
        file: Publication,
        reg: Option<&dyn Registry>,
    ) -> Result<TrackCommitment, PublishError> {
        if !self.allow_examples {
            let ids = file
                .steps
                .iter()
                .flat_map(|s| s.params.rules.iter().map(|r| r.metric.0.as_str()))
                .chain(file.statements.iter().map(|s| s.source.0.as_str()))
                .chain(file.resolutions.iter().map(|r| r.resolver.0.as_str()));
            for id in ids {
                if is_example_id(id) {
                    return Err(PublishError::Example(id.to_string()));
                }
            }
        }
        // Checked outside the lock, so readers keep being served meanwhile.
        let cur = self.current();
        // The steps checked before (served by this process, or named by the
        // head it kept): only the run from the last of them is checked again
        // (review C20-R1-2), within a bound (review C20-R2-2).
        let trusted = match cur.as_deref() {
            Some(p) => {
                if !file.starts_with(&p.file)
                    || (file.steps.len() == p.file.steps.len() && file != p.file)
                {
                    return Err(PublishError::Rewritten);
                }
                p.file.steps.len()
            }
            // After a restart: never behind the head served before (C20-R1-1),
            // the same bytes up to it.
            None => match self.served_head().map_err(PublishError::Head)? {
                Some(h) if !h.starts(&file) => return Err(PublishError::Rewritten),
                Some(h) => usize::try_from(h.steps).map_err(|_| PublishError::Rewritten)?,
                None => 0,
            },
        };
        if file.check_cost(trusted) > MAX_INSTALL_CHECK {
            return Err(PublishError::TooLarge);
        }
        let book = file.check_after(trusted)?;
        let mut slot = self
            .published
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let same = match (slot.as_ref(), cur.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            // Another publication was installed meanwhile: check against it again.
            return Err(PublishError::Rewritten);
        }
        let last = file.latest().ok_or(PublicationError::Empty)?;
        let head = ServedHead::of(&file).ok_or(PublicationError::Empty)?;
        self.keep_head(&head).map_err(PublishError::Head)?;
        let (next, params) = (last.commitment.clone(), last.params.clone());
        // A file-served node has no registry of its own: it uses the one the
        // file carries, rebuilt without address rules and only when its root is
        // the latest commitment's (NL15 review: a weather track was withheld as
        // label data). Anything else stays fail closed.
        let carried = match (reg, file.registry.as_ref()) {
            (None, Some(d)) => d.build_checked(&NoRules, next.registry_root).ok(),
            _ => None,
        };
        let none = StaticRegistry::new();
        let reg = reg
            .or(carried.as_ref().map(|r| r as &dyn Registry))
            .unwrap_or(&none);
        let mut label = Vec::new();
        for r in &params.rules {
            if label_derived(reg, &r.metric) {
                label.push(r.metric.clone());
            }
        }
        *slot = Some(Arc::new(Published {
            chain: file.steps.iter().map(|s| s.commitment.clone()).collect(),
            hashes: file.steps.iter().map(|s| s.commitment.hash()).collect(),
            priced: file.priced_answers(),
            file,
            params,
            book,
            label_derived: label,
        }));
        Ok(next)
    }

    /// Re-read the publication file when it changed (a node's store; a no-op
    /// otherwise). A missing file before anything was served publishes
    /// nothing; any other failure is kept and returned until the file changes
    /// again, and the last good publication stays loaded but unserved.
    pub fn refresh(&self) -> Result<(), String> {
        let Some(f) = &self.file else {
            return self.refusing.clone().map_or(Ok(()), Err);
        };
        let mut seen = f
            .seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let r = self.refresh_locked(f, &mut seen);
        *f.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = r.clone();
        r
    }

    /// [`TrackStore::refresh`] unless a read is running, which then answers
    /// for this call too (review C20-R2-3).
    fn refresh_if_idle(&self) {
        let Some(f) = &self.file else {
            return;
        };
        let mut seen = match f.seen.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        let r = self.refresh_locked(f, &mut seen);
        *f.last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = r;
    }

    /// The result of the last finished read (no I/O): a request never waits
    /// on a read, nor reads the file on the runtime's workers (review
    /// C20-R2-3); while a read runs, the publication the last one left stays
    /// served.
    fn status(&self) -> Result<(), String> {
        match &self.file {
            None => self.refusing.clone().map_or(Ok(()), Err),
            Some(f) => f
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        }
    }

    fn refresh_locked(&self, f: &FileSource, seen: &mut Option<FileRead>) -> Result<(), String> {
        let meta = match std::fs::metadata(&f.path) {
            Ok(m) => m,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && self.current().is_none()
                    && self.served_head()?.is_none() =>
            {
                *seen = None;
                return Ok(());
            }
            Err(e) => return Err(format!("track publication file: {e}")),
        };
        let stamp = Stamp {
            modified: meta
                .modified()
                .map_err(|e| format!("track publication file: {e}"))?,
            len: meta.len(),
            inode: meta.ino(),
            changed: (meta.ctime(), meta.ctime_nsec()),
        };
        if let Some((s, r)) = seen.as_ref() {
            if *s == stamp {
                return r.clone();
            }
        }
        let read = if stamp.len > MAX_FILE_BYTES {
            Err(format!(
                "track publication file over {MAX_FILE_BYTES} bytes"
            ))
        } else {
            std::fs::read(&f.path)
                .map_err(|e| format!("track publication file: {e}"))
                .and_then(|b| {
                    borsh::from_slice::<Publication>(&b)
                        .map_err(|e| format!("track publication file: {e}"))
                })
                .and_then(|p| {
                    self.install(p, None)
                        .map(|_| ())
                        .map_err(|e| format!("track publication file: {e}"))
                })
        };
        *seen = Some((stamp, read.clone()));
        read
    }

    /// The latest commitment, if any.
    pub fn latest(&self) -> Option<TrackCommitment> {
        self.current().and_then(|p| p.chain.last().cloned())
    }

    fn current(&self) -> Option<Arc<Published>> {
        self.published
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn withheld(&self, p: Option<&Published>, metric: &MetricId) -> bool {
        self.rights == LabelRights::Fallback
            && (entity_named(&metric.0)
                || p.is_some_and(|p| p.label_derived.binary_search(metric).is_ok()))
    }

    /// The logs cover every record: withheld in fallback when any ruled
    /// metric is.
    fn log_withheld(&self, p: &Published) -> bool {
        p.params
            .rules
            .iter()
            .any(|r| self.withheld(Some(p), &r.metric))
    }

    /// The registry route: withheld wherever the logs are withheld for
    /// rights (403 `label_rights_withheld`), and in every rights mode whenever
    /// it names an address (403 `track_registry_names_addresses`; review
    /// C23-R2-2: a revocation names an address with an entity, a label row,
    /// and the node cannot redact it since the root covers it).
    fn registry_withheld(&self, p: &Published) -> Option<ApiError> {
        if self.log_withheld(p) {
            return Some(withheld_error());
        }
        p.file
            .registry
            .as_ref()
            .is_some_and(|r| r.names_addresses())
            .then(|| {
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "track_registry_names_addresses",
                    "the registry names an address with an entity (a recorded revocation), a label row no route serves: a client reruns compute only with a registry of its own",
                )
            })
    }

    fn take_budget(&self, client: &str) -> Result<(), ApiError> {
        self.take_from(client, "track", self.free_per_minute)
    }

    fn take_from(&self, client: &str, scope: &str, per_minute: u32) -> Result<(), ApiError> {
        let scopes: Vec<(String, u32)> = rate_scopes(client, per_minute)
            .into_iter()
            .map(|(k, n)| (format!("{scope}:{k}"), n))
            .collect();
        if self.limits.take_attempts(&scopes) {
            return Ok(());
        }
        Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many track-record answers from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string()))
    }

    /// The current publication, as the last read of the file left it (503
    /// when it failed; [`reload`] reads a changed file first).
    fn serving(&self) -> Result<Option<Arc<Published>>, ApiError> {
        self.status().map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "track_store_unreadable",
                format!("track records are not served until the operator fixes the publication file ({e})"),
            )
        })?;
        Ok(self.current())
    }
}

/// An empty store's routes (nothing published, [`NOTHING_PUBLISHED`]): every
/// record a 404 `no_track_record`.
pub fn empty_routes(rights: LabelRights) -> Router {
    routes(Arc::new(TrackStore::new(rights)))
}

/// The routes a node mounts: from the coordinator's publication file when
/// `ONYX_TRACK_PUBLICATIONS` names one ([`TrackStore::from_file`], its served
/// head kept in `state_dir`), else an empty store. Takes paths, never data,
/// and returns only a `Router`, so the node that mounts it holds no handle to
/// any record (scripts/check-boundaries.sh: the node names track code on this
/// one call only). A file that does not check is reported here and answered
/// 503 on every track route; it never stops the node (review C20-R1-4).
pub fn node_routes(
    rights: LabelRights,
    publications: Option<PathBuf>,
    state_dir: PathBuf,
) -> Router {
    let head = state_dir.join(SERVED_HEAD_FILE);
    match publications {
        // A node that served commitments before and now names no file would
        // say "none published" about records it served: 503 instead (review
        // C20-R2-5; delete the head to start a new chain on purpose).
        None if head.exists() => {
            let mut store = TrackStore::new(rights);
            store.refusing = Some(format!(
                "ONYX_TRACK_PUBLICATIONS is unset but this node served track commitments before ({} names the last)",
                head.display()
            ));
            eprintln!(
                "onyx-node: {}; track routes answer 503",
                store.refusing.as_deref().unwrap_or_default()
            );
            routes(Arc::new(store))
        }
        None => empty_routes(rights),
        Some(path) => {
            let (store, first) = TrackStore::from_file(rights, path, head);
            if let Err(e) = first {
                eprintln!("onyx-node: ONYX_TRACK_PUBLICATIONS: {e}; track routes answer 503 until it is fixed");
            }
            routes(Arc::new(store))
        }
    }
}

/// The track routes, to pass to [`crate::router_with`] (which puts them behind
/// the API's guard: one concurrency limit, the deadline, the client key).
pub fn routes(store: Arc<TrackStore>) -> Router {
    Router::new()
        .route("/v1/track", get(index))
        .route("/v1/track/log", get(log))
        .route("/v1/track/registry", get(registry))
        .route("/v1/track/{source}/{metric}", get(record))
        .route("/v1/track/{source}/{metric}/history", get(history))
        .with_state(store)
}

fn withheld_error() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "label_rights_withheld",
        "this server runs with ONYX_RIGHTS=fallback: track records on metrics derived from entity label sets are not served until the label sets' data rights are cleared",
    )
}

fn none(reason: &'static str, message: String) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "no_track_record", message)
        .with_field("reason", Value::String(reason.into()))
}

fn rules_json(p: &TrackParams) -> Vec<Value> {
    p.rules
        .iter()
        .map(|r| {
            json!({
                "metric": r.metric.0,
                "closeAfterSecs": match r.close {
                    Close::AfterAsOf(secs) => Some(secs),
                    Close::PeriodStart { .. } => None,
                },
                "periodSecs": match r.close {
                    Close::PeriodStart { period_secs, .. } => Some(period_secs),
                    Close::AfterAsOf(_) => None,
                },
                "leadSecs": match r.close {
                    Close::PeriodStart { lead_secs, .. } => Some(lead_secs),
                    Close::AfterAsOf(_) => None,
                },
                "tolerance": r.tolerance.to_string(),
            })
        })
        .collect()
}

fn ok_json(body: Value, etag: Option<&str>) -> Response {
    let mut r = (StatusCode::OK, Json(body)).into_response();
    let h = r.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, no-cache"),
    );
    if let Some(v) = etag.and_then(|e| HeaderValue::from_str(e).ok()) {
        h.insert(header::ETAG, v);
    }
    r
}

/// A canonical decimal (no sign, no leading zero), or `None`.
fn decimal(v: &str) -> Option<u64> {
    let canonical =
        v == "0" || (!v.is_empty() && !v.starts_with('0') && v.bytes().all(|b| b.is_ascii_digit()));
    canonical.then(|| v.parse().ok()).flatten()
}

/// `?from=<n>` (the only parameter of the index), a canonical decimal.
fn parse_from(q: Option<String>) -> Result<usize, ApiError> {
    match q.as_deref() {
        None | Some("") => Ok(0),
        Some(q) => q
            .strip_prefix("from=")
            .and_then(decimal)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| bad_request("GET /v1/track takes only from=<record index>")),
    }
}

/// Re-read a changed publication file on the blocking pool, so a large
/// reload never parks the runtime's workers (review C20-R1-2); when another
/// read is running this returns at once and the request is answered from the
/// last finished read (review C20-R2-3).
async fn reload(store: &Arc<TrackStore>) {
    if store.file.is_some() {
        let s = store.clone();
        let _ = tokio::task::spawn_blocking(move || s.refresh_if_idle()).await;
    }
}

/// `GET /v1/track[?from=n]`: free, unlimited (a small body), [`MAX_LISTED`]
/// records a page.
async fn index(State(store): State<Arc<TrackStore>>, RawQuery(q): RawQuery) -> Response {
    reload(&store).await;
    let from = match parse_from(q) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };
    let p = match store.serving() {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let Some(p) = p else {
        return ok_json(
            json!({
                "level": 5,
                "published": false,
                "commitmentHash": null,
                "asOf": null,
                "rules": [],
                "records": [],
                "withheld": 0,
                "truncated": false,
                "next": null,
                "reason": NOTHING_PUBLISHED,
            }),
            None,
        );
    };
    let c = p.chain.last();
    let mut withheld = 0u64;
    let mut shown = 0usize;
    let mut records = Vec::new();
    let mut next = None;
    for r in p.book.records() {
        if store.withheld(Some(&p), &r.metric) {
            withheld += 1;
            continue;
        }
        shown += 1;
        if shown <= from {
            continue;
        }
        if records.len() == MAX_LISTED {
            next = Some(from + MAX_LISTED);
            continue;
        }
        records.push(json!({
            "source": r.source.0,
            "metric": r.metric.0,
            "resolved": r.resolved(),
            "established": r.established(),
            "example": is_example_id(&r.source.0) || is_example_id(&r.metric.0),
        }));
    }
    let reason = match (p.params.rules.is_empty(), p.book.records().is_empty()) {
        (true, _) => Value::String("no metric has a close-time rule yet".into()),
        (false, true) => Value::String("no source has stated on a ruled metric yet".into()),
        _ => Value::Null,
    };
    ok_json(
        json!({
            "level": 5,
            "published": true,
            "commitmentHash": c.map(|c| c.hash().to_string()),
            "asOf": c.map(|c| c.as_of),
            "rules": rules_json(&p.params),
            "records": records,
            "withheld": withheld,
            "truncated": next.is_some(),
            "next": next,
            "reason": reason,
        }),
        None,
    )
}

/// `?since=0x<commitment>` (the only parameter), canonical form only.
fn parse_since(q: Option<String>) -> Result<Option<Hash32>, ApiError> {
    let Some(q) = q.filter(|q| !q.is_empty()) else {
        return Ok(None);
    };
    let v = q
        .strip_prefix("since=")
        .filter(|v| !v.contains('&'))
        .ok_or_else(|| bad_request("the only query parameter is since=0x<commitment hash>"))?;
    let (h, canonical) = crate::parse_hash(v, "since")?;
    if !canonical {
        return Err(bad_request("since must be 0x and 64 lowercase hex digits"));
    }
    Ok(Some(h))
}

/// The record a path names, in the documented order: ids, rights (before
/// any lookup), the publication, the rule, the record.
fn lookup(
    store: &TrackStore,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<(Arc<Published>, SourceId, MetricId), ApiError> {
    let Path((source, metric)) = path.map_err(|e| bad_request(e.body_text()))?;
    if !is_dotted_id(&source) || !is_dotted_id(&metric) {
        return Err(bad_request(
            "source and metric must be dotted ids: lowercase letters, digits, '.', ':', '_' or '-'",
        ));
    }
    let (source, metric) = (SourceId(source), MetricId(metric));
    let p = store.serving()?;
    // ROADMAP L14 pattern: refused before any lookup, so a withheld metric's
    // records cannot be probed for.
    if store.withheld(p.as_deref(), &metric) {
        return Err(withheld_error());
    }
    let Some(p) = p else {
        return Err(none(NONE_PUBLISHED, NOTHING_PUBLISHED.into()));
    };
    if p.params.rule(&metric).is_none() {
        return Err(none(
            NO_RULE,
            format!(
                "{} has no close-time rule, so it has no track record",
                metric.0
            ),
        ));
    }
    if p.book.record(&source, &metric).is_none() {
        return Err(none(
            NO_RECORD,
            format!("{} has stated nothing on {}", source.0, metric.0),
        ));
    }
    Ok((p, source, metric))
}

/// `"track-{first 16 bytes of keccak(parts)}"`.
fn etag_of(parts: &[&[u8]]) -> String {
    format!(
        "\"track-{}\"",
        hex::encode(&keccak256(&parts.concat()).0[..16])
    )
}

/// A 304 when the client holds `etag` (costs no budget).
fn not_modified(headers: &HeaderMap, etag: &str) -> Option<Response> {
    if !crate::etag_matches(headers, etag) {
        return None;
    }
    let mut r = StatusCode::NOT_MODIFIED.into_response();
    if let Ok(v) = HeaderValue::from_str(etag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, no-cache"),
    );
    Some(r)
}

fn unknown_commitment() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "unknown_commitment",
        "since names no commitment of this server's chain (a fork, or another server)",
    )
}

fn chain_too_long() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "track_chain_too_long",
        format!("more than {MAX_CHAIN} commitments after since"),
    )
}

fn reply(
    store: &TrackStore,
    client: &str,
    path: Result<Path<(String, String)>, PathRejection>,
    q: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if path.is_ok() {
        // The query is checked before any lookup (as the ids are).
        parse_since(q.clone())?;
    }
    let (p, source, metric) = lookup(store, path)?;
    let since = parse_since(q)?;
    let Some(c) = p.chain.last() else {
        return Err(crate::internal("track chain empty"));
    };
    let chain = match since {
        None => None,
        Some(h) => {
            let at = p
                .chain
                .iter()
                .position(|x| x.hash() == h)
                .ok_or_else(unknown_commitment)?;
            let after = &p.chain[at + 1..];
            if after.len() > MAX_CHAIN {
                return Err(chain_too_long());
            }
            Some(after)
        }
    };
    let since_bytes = since.map(|h| h.0.to_vec()).unwrap_or_default();
    let etag = etag_of(&[
        &c.hash().0,
        source.0.as_bytes(),
        &[0],
        metric.0.as_bytes(),
        &[0],
        &since_bytes,
    ]);
    if let Some(r) = not_modified(headers, &etag) {
        return Ok(r);
    }
    // A history body can be large (up to 1,024 entries with proofs): each
    // client has its own budget of free answers.
    store.take_budget(client)?;
    let served = Served {
        book: &p.book,
        commitment: c,
        params: &p.params,
        chain,
    };
    let body = record_body(&served, &source, &metric)
        .ok_or_else(|| crate::internal("record without proofs"))?;
    Ok(ok_json(body, Some(&etag)))
}

async fn record(
    State(store): State<Arc<TrackStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reload(&store).await;
    reply(&store, &client, path, q, &headers).unwrap_or_else(IntoResponse::into_response)
}

/// `?at=0x<commitment>&from=<n>` (both, in this order, canonical).
fn parse_page(q: Option<String>) -> Result<(Hash32, u64), ApiError> {
    let bad = || bad_request("history takes at=0x<commitment hash>&from=<entry index>");
    let q = q.ok_or_else(bad)?;
    let (at, from) = q
        .strip_prefix("at=")
        .and_then(|r| r.split_once("&from="))
        .ok_or_else(bad)?;
    let (h, canonical) = crate::parse_hash(at, "at")?;
    let from = decimal(from).filter(|_| canonical).ok_or_else(bad)?;
    Ok((h, from))
}

/// `GET /v1/track/{source}/{metric}/history?at=&from=`: one page of a record's
/// history at the latest commitment (409 `commitment_moved` when `at` is not
/// it: the client starts again from the record).
fn page_reply(
    store: &TrackStore,
    client: &str,
    path: Result<Path<(String, String)>, PathRejection>,
    q: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if path.is_ok() {
        parse_page(q.clone())?;
    }
    let (p, source, metric) = lookup(store, path)?;
    let (at, from) = parse_page(q)?;
    let Some(c) = p.chain.last() else {
        return Err(crate::internal("track chain empty"));
    };
    if c.hash() != at {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "commitment_moved",
            "at is not the latest commitment: fetch the record again and page from its commitment",
        ));
    }
    let served = Served {
        book: &p.book,
        commitment: c,
        params: &p.params,
        chain: None,
    };
    let etag = etag_of(&[
        b"history",
        &at.0,
        source.0.as_bytes(),
        &[0],
        metric.0.as_bytes(),
        &[0],
        &from.to_be_bytes(),
    ]);
    if let Some(r) = not_modified(headers, &etag) {
        return Ok(r);
    }
    // The bound first (cheap), then the budget, then the page (up to 1,024
    // entries with proofs): a client past its budget never has one built
    // (review C20-R2-4).
    if p.book
        .record(&source, &metric)
        .is_none_or(|(_, r)| from >= r.entries_count)
    {
        return Err(bad_request("from is past the record's last entry"));
    }
    store.take_budget(client)?;
    let Some(body) = history_page_body(&served, &source, &metric, from) else {
        return Err(crate::internal("history page without proofs"));
    };
    Ok(ok_json(body, Some(&etag)))
}

async fn history(
    State(store): State<Arc<TrackStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<(String, String)>, PathRejection>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reload(&store).await;
    page_reply(&store, &client, path, q, &headers).unwrap_or_else(IntoResponse::into_response)
}

/// What `GET /v1/track/log` was asked (canonical forms only, in this order).
enum LogQuery {
    /// `[?since=0x<commitment>]`: the logs (format 1) or their head (format 2).
    Body(Option<Hash32>),
    /// `?at=0x<latest>&from=<n>&since=0x<first link>`: rows of chained logs.
    Rows {
        at: Hash32,
        from: u64,
        since: Hash32,
    },
    /// `?at=0x<latest>&posts=<n>`: the intake's posts from `n` with their batches.
    Posts { at: Hash32, from: u64 },
}

fn parse_log_query(q: Option<String>) -> Result<LogQuery, ApiError> {
    let bad = || {
        bad_request(
            "GET /v1/track/log takes since=0x<commitment>, at=0x<latest>&from=<n>&since=0x<first link>, or at=0x<latest>&posts=<n>",
        )
    };
    let Some(q) = q.filter(|q| !q.is_empty()) else {
        return Ok(LogQuery::Body(None));
    };
    if q.starts_with("since=") {
        return parse_since(Some(q)).map(LogQuery::Body);
    }
    let hash = |v: &str, what: &str| -> Result<Hash32, ApiError> {
        match crate::parse_hash(v, what)? {
            (h, true) => Ok(h),
            _ => Err(bad_request(format!(
                "{what} must be 0x and 64 lowercase hex digits"
            ))),
        }
    };
    let parts: Vec<&str> = q.split('&').collect();
    match parts.as_slice() {
        [at, from, since] => {
            let at = hash(at.strip_prefix("at=").ok_or_else(bad)?, "at")?;
            let from = from
                .strip_prefix("from=")
                .and_then(decimal)
                .ok_or_else(bad)?;
            let since = hash(since.strip_prefix("since=").ok_or_else(bad)?, "since")?;
            Ok(LogQuery::Rows { at, from, since })
        }
        [at, posts] => {
            let at = hash(at.strip_prefix("at=").ok_or_else(bad)?, "at")?;
            let from = posts
                .strip_prefix("posts=")
                .and_then(decimal)
                .ok_or_else(bad)?;
            Ok(LogQuery::Posts { at, from })
        }
        _ => Err(bad()),
    }
}

/// The checks every log answer shares, in this order: the publication (404
/// `none_published`); rights (403 `label_rights_withheld` in fallback when any
/// ruled metric is label-derived: the logs cover every record); priced
/// answers (403 `track_log_priced`).
fn log_publication(store: &TrackStore) -> Result<Arc<Published>, ApiError> {
    let Some(p) = store.serving()? else {
        return Err(none(NONE_PUBLISHED, NOTHING_PUBLISHED.into()));
    };
    if store.log_withheld(&p) {
        return Err(withheld_error());
    }
    let priced = p.priced;
    if priced > 0 {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "track_log_priced",
            format!("the logs hold {priced} level-1 or level-2 answer(s), values the paid API sells: they are not served free (ADR-0036 amendment 5); the records and their proofs are"),
        ));
    }
    Ok(p)
}

/// The intake's revealed rows plus its posts (what a client counts as the
/// posts pages arrive).
fn intake_rows(p: &Publication) -> u64 {
    p.intake.as_ref().map_or(0, |i| {
        i.revealed_rows().saturating_add(i.posts.len() as u64)
    })
}

fn too_costly() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "track_log_too_large",
        "too many commitments since that one for the logs' size: ask since a later commitment",
    )
}

/// The index of `since` in the chain (409 `unknown_commitment`), at most
/// [`MAX_CHAIN`] commitments before the latest (413 `track_chain_too_long`).
fn run_start(p: &Published, since: Option<Hash32>) -> Result<usize, ApiError> {
    let latest = p.chain.len().saturating_sub(1);
    let Some(h) = since else {
        return Ok(latest);
    };
    let at = p
        .hashes
        .iter()
        .position(|x| *x == h)
        .ok_or_else(unknown_commitment)?;
    if latest - at > MAX_CHAIN {
        return Err(chain_too_long());
    }
    Ok(at)
}

/// `GET /v1/track/log[?since=]` (ROADMAP C20, C21; ADR-0036 amendments 5 and
/// 6), in this order: the query; [`log_publication`]; for format 1 the size
/// (413 `track_log_too_large`); `since` (409 `unknown_commitment`, 413
/// `track_chain_too_long`); the client's check cost (413); the ETag; the free
/// budget. Chained logs answer their head here and their rows and posts in
/// pages ([`log_page_reply`]).
fn log_reply(
    store: &TrackStore,
    client: &str,
    q: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let since = match parse_log_query(q)? {
        LogQuery::Body(since) => since,
        page => return log_page_reply(store, client, page, headers),
    };
    let p = log_publication(store)?;
    let chained = p.file.is_chained();
    if !chained && p.file.log_rows() > MAX_LOG_ROWS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "track_log_too_large",
            format!("the logs hold more than {MAX_LOG_ROWS} rows in format 1, which is served in one body only"),
        ));
    }
    let from = run_start(&p, since)?;
    let latest = p.chain.len().saturating_sub(1);
    // What a client spends checking the logs is capped (review C20-R1-3):
    // past it, the client should ask since a later commitment.
    let rows = (p.file.statements.len() as u64).saturating_add(p.file.resolutions.len() as u64);
    let cost = if chained {
        // With the intake's revealed rows and posts, as the client counts
        // them (review C21-R1-5).
        chained_check_cost(latest - from + 1, rows.saturating_add(intake_rows(&p.file)))
    } else {
        log_check_cost(latest - from + 1, rows)
    };
    if cost > MAX_LOG_CHECK {
        return Err(too_costly());
    }
    let Some(c) = p.chain.last() else {
        return Err(crate::internal("track chain empty"));
    };
    let since_bytes = since.map(|h| h.0.to_vec()).unwrap_or_default();
    let etag = etag_of(&[b"log", &c.hash().0, &since_bytes]);
    if let Some(r) = not_modified(headers, &etag) {
        return Ok(r);
    }
    store.take_from(client, "track-log", store.log_per_minute)?;
    let body = if chained {
        chained_log_body(&p.file, from)
    } else {
        log_body(&p.file, from)
    }
    .ok_or_else(|| crate::internal("log without links"))?;
    Ok(ok_json(body, Some(&etag)))
}

/// A page of chained logs, after [`log_publication`]: the logs must be
/// chained (400 otherwise), `at` the latest (409 `commitment_moved`), `since`
/// a commitment of the chain (409, 413 as for the head), the position inside
/// the stream (400), the client's check cost within the cap (413, as for the
/// head); then the ETag, the page budget and the page.
fn log_page_reply(
    store: &TrackStore,
    client: &str,
    q: LogQuery,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let p = log_publication(store)?;
    if !p.file.is_chained() {
        return Err(bad_request(
            "this server's logs are in format 1 and served whole: GET /v1/track/log[?since=]",
        ));
    }
    let Some(c) = p.chain.last() else {
        return Err(crate::internal("track chain empty"));
    };
    let moved = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "commitment_moved",
            "at is not the latest commitment: fetch the head again and page from its commitment",
        )
    };
    let (etag, body) = match q {
        LogQuery::Rows { at, from, since } => {
            if at != c.hash() {
                return Err(moved());
            }
            let start = run_start(&p, Some(since))?;
            let links = &p.file.steps[start..];
            if from >= stream_rows(links) {
                return Err(bad_request("from is past the logs' last row"));
            }
            let rows = (p.file.statements.len() as u64)
                .saturating_add(p.file.resolutions.len() as u64)
                .saturating_add(intake_rows(&p.file));
            if chained_check_cost(links.len(), rows) > MAX_LOG_CHECK {
                return Err(too_costly());
            }
            let etag = etag_of(&[b"log-rows", &at.0, &since.0, &from.to_be_bytes()]);
            if let Some(r) = not_modified(headers, &etag) {
                return Ok(r);
            }
            store.take_from(client, "track-log-page", store.pages_per_minute)?;
            (etag, log_rows_page(&p.file, start, from))
        }
        LogQuery::Posts { at, from } => {
            if at != c.hash() {
                return Err(moved());
            }
            let posts = p.file.intake.as_ref().map_or(0, |i| i.posts.len() as u64);
            if from >= posts {
                return Err(bad_request("posts is past the intake's last post"));
            }
            let etag = etag_of(&[b"log-posts", &at.0, &from.to_be_bytes()]);
            if let Some(r) = not_modified(headers, &etag) {
                return Ok(r);
            }
            store.take_from(client, "track-log-page", store.pages_per_minute)?;
            let page = log_posts_page(&p.file, from);
            // One batch too large for a page (review C21-R1-2): said so, never
            // served for the client to refuse.
            if page
                .as_ref()
                .and_then(|b| b["posts"].as_str())
                .is_some_and(|h| h.len() > MAX_LOG_PAGE_BYTES)
            {
                return Err(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "track_log_too_large",
                    format!("post {from}'s batch is larger than one page ({MAX_LOG_PAGE_BYTES} bytes); paging inside a batch is not served yet"),
                ));
            }
            (etag, page)
        }
        LogQuery::Body(_) => return Err(crate::internal("not a page")),
    };
    let body = body.ok_or_else(|| crate::internal("log page out of range"))?;
    Ok(ok_json(body, Some(&etag)))
}

async fn log(
    State(store): State<Arc<TrackStore>>,
    client: Option<Extension<ClientKey>>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reload(&store).await;
    log_reply(&store, &client, q, &headers).unwrap_or_else(IntoResponse::into_response)
}

/// `GET /v1/track/registry` (ROADMAP C23), in this order: no query (400); the
/// publication (404 `none_published`); rights (403 `label_rights_withheld`
/// where the logs are withheld); 403 `track_registry_names_addresses` when
/// the registry names an address, in every rights mode; a publication
/// without one (404 `no_track_registry`); the ETag; the records' free budget. The node cannot check the registry's root (it
/// has no address rules): every client rebuilds it and checks it against the
/// commitment's `registry_root` before using it.
fn registry_reply(
    store: &TrackStore,
    client: &str,
    q: Option<String>,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if q.is_some_and(|q| !q.is_empty()) {
        return Err(bad_request("GET /v1/track/registry takes no query"));
    }
    let Some(p) = store.serving()? else {
        return Err(none(NONE_PUBLISHED, NOTHING_PUBLISHED.into()));
    };
    if let Some(e) = store.registry_withheld(&p) {
        return Err(e);
    }
    let Some(c) = p.chain.last() else {
        return Err(crate::internal("track chain empty"));
    };
    let Some(body) = registry_body(&p.file) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no_track_registry",
            "this publication carries no registry (a version-1 file): a client reruns compute only with a registry of its own",
        ));
    };
    let etag = etag_of(&[b"registry", &c.hash().0]);
    if let Some(r) = not_modified(headers, &etag) {
        return Ok(r);
    }
    store.take_budget(client)?;
    Ok(ok_json(body, Some(&etag)))
}

async fn registry(
    State(store): State<Arc<TrackStore>>,
    client: Option<Extension<ClientKey>>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reload(&store).await;
    registry_reply(&store, &client, q, &headers).unwrap_or_else(IntoResponse::into_response)
}

#[cfg(test)]
#[path = "track_serve_tests.rs"]
mod tests;
