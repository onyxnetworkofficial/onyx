//! A publication: the commitment chain with its logs, in one Borsh value
//! (ROADMAP C20; ADR-0036 amendment 5; docs/spec/delivery.md "Track records").
//!
//! The coordinator builds it from its computations ([`Publication::push`])
//! and writes it to a file; a node reads that file into its store and serves
//! the logs from it (`onyx_api::track_serve`, `GET /v1/track/log`); every
//! client checks the served logs with the same [`check_steps`].
//!
//! Every later commitment of a chain keeps every earlier statement and
//! resolution ([`check_extends`]), so the logs are kept once, in publication
//! order (each step appends what is new to it), and a step names only how many
//! rows of each log it covers: the log of step `i` is the first
//! `steps[i].statements` statements (and `.resolutions` resolutions), sorted
//! into the canonical order. So one publication carries the log of every
//! commitment of the chain in linear size, and a client that knew an earlier
//! commitment runs [`check_extends`] from it to the latest (THREATS TR-S-14).
//!
//! The book rows of the latest step travel with it, so a node needs no
//! registry to serve them ([`TrackBook::from_committed`]); a client with the
//! registry reruns [`compute`] on the logs (THREATS TR-S-13).
//!
//! Pure: no I/O, no clock. Nothing here returns a proof level or touches
//! money (ADR-0036 item 12); [`Publication::priced_answers`] reads each
//! resolution's level number only to count the priced ones.

use crate::chained::{
    chained_resolutions_hash, chained_statements_hash, check_chained, LOG_FORMAT_CHAINED,
    LOG_FORMAT_WHOLE,
};
use crate::intake::PublishedIntake;
use crate::{
    check_extends, resolutions_hash, sort_resolutions, sort_statements, statements_hash, Entry,
    ExtendError, MetricSummary, Record, Resolution, Statement, TrackBook, TrackCommitment,
    TrackLog, TrackParams, MAX_ITEMS,
};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{Hash32, Registry, RegistryData, StaticRegistry};
use std::collections::BTreeSet;
use std::io::{Error as IoError, ErrorKind, Read, Write};

/// Version byte of a [`Publication`] in its first layout (format-1 logs, no
/// registry): read and written unchanged (ADR-0036 amendment 5).
pub const PUBLICATION_VERSION: u8 = 1;
/// Version byte of the second layout (ROADMAP C21, C23; ADR-0036 amendment
/// 6): a log format byte after the version, and the registry of the latest
/// commitment at the end.
pub const PUBLICATION_VERSION_2: u8 = 2;

/// One commitment of the chain and how much of the logs it covers.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Step {
    /// The commitment.
    pub commitment: TrackCommitment,
    /// Its params (`commitment.params_hash`).
    pub params: TrackParams,
    /// Its statement log is the first this many statements, sorted.
    pub statements: u64,
    /// Its resolution list is the first this many resolutions, sorted.
    pub resolutions: u64,
}

/// The commitment chain of one track with its logs and the latest book's rows.
/// Borsh: the version byte, then (version 2 only) the log format byte, the
/// fields below in order, then (version 2 only) the registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Publication {
    /// [`PUBLICATION_VERSION`] or [`PUBLICATION_VERSION_2`].
    pub version: u8,
    /// The log format of every step ([`LOG_FORMAT_WHOLE`] or
    /// [`LOG_FORMAT_CHAINED`]; version 1 is always format 1). A chain never
    /// changes format.
    pub format: u8,
    /// Every commitment, oldest first (the first has `prev` zero).
    pub steps: Vec<Step>,
    /// Every statement, in publication order.
    pub statements: Vec<Statement>,
    /// Every resolution, in publication order.
    pub resolutions: Vec<Resolution>,
    /// The latest book's history rows.
    pub entries: Vec<Entry>,
    /// The latest book's record rows.
    pub records: Vec<Record>,
    /// The latest book's metric summaries.
    pub metrics: Vec<MetricSummary>,
    /// The intake at the latest commitment's head (`None` without an intake).
    pub intake: Option<PublishedIntake>,
    /// The registry the latest commitment names (`registry_root`), as data, so
    /// a client rebuilds it and reruns `compute` (ROADMAP C23; version 2 only;
    /// clients check its root, the node cannot: it has no address rules).
    pub registry: Option<RegistryData>,
}

fn invalid(what: &str) -> IoError {
    IoError::new(ErrorKind::InvalidData, what.to_string())
}

impl BorshSerialize for Publication {
    fn serialize<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        self.version.serialize(w)?;
        match self.version {
            PUBLICATION_VERSION if self.format == LOG_FORMAT_WHOLE && self.registry.is_none() => {}
            PUBLICATION_VERSION_2 => self.format.serialize(w)?,
            _ => return Err(invalid("publication version, format or registry")),
        }
        self.steps.serialize(w)?;
        self.statements.serialize(w)?;
        self.resolutions.serialize(w)?;
        self.entries.serialize(w)?;
        self.records.serialize(w)?;
        self.metrics.serialize(w)?;
        self.intake.serialize(w)?;
        if self.version == PUBLICATION_VERSION_2 {
            self.registry.serialize(w)?;
        }
        Ok(())
    }
}

impl BorshDeserialize for Publication {
    fn deserialize_reader<R: Read>(r: &mut R) -> std::io::Result<Self> {
        let version = u8::deserialize_reader(r)?;
        let format = match version {
            PUBLICATION_VERSION => LOG_FORMAT_WHOLE,
            PUBLICATION_VERSION_2 => u8::deserialize_reader(r)?,
            _ => return Err(invalid("publication version")),
        };
        let steps = BorshDeserialize::deserialize_reader(r)?;
        let statements = BorshDeserialize::deserialize_reader(r)?;
        let resolutions = BorshDeserialize::deserialize_reader(r)?;
        let entries = BorshDeserialize::deserialize_reader(r)?;
        let records = BorshDeserialize::deserialize_reader(r)?;
        let metrics = BorshDeserialize::deserialize_reader(r)?;
        let intake = BorshDeserialize::deserialize_reader(r)?;
        let registry = if version == PUBLICATION_VERSION_2 {
            BorshDeserialize::deserialize_reader(r)?
        } else {
            None
        };
        Ok(Publication {
            version,
            format,
            steps,
            statements,
            resolutions,
            entries,
            records,
            metrics,
            intake,
            registry,
        })
    }
}

/// Why a publication, or served logs, do not check.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublicationError {
    /// Another version.
    #[error("publication version {0} is not supported")]
    Version(u8),
    /// No step.
    #[error("no commitment")]
    Empty,
    /// The first commitment of a whole chain has a non-zero `prev`.
    #[error("the chain does not start at its first commitment")]
    NotFirst,
    /// Counts falling, past the logs, or the latest not covering every row.
    #[error("step {0}: its row counts do not fit the logs")]
    Counts(usize),
    /// A row twice in a log, or a log not hashing to its commitment.
    #[error("step {0}: its log does not match its commitment")]
    Log(usize),
    /// The params do not hash to the commitment's `params_hash`.
    #[error("step {0}: params do not match the commitment")]
    Params(usize),
    /// A step does not extend the one before.
    #[error("step {0}: does not extend the one before: {1}")]
    Extend(usize, ExtendError),
    /// The rows do not give the latest commitment.
    #[error("the book rows do not match the latest commitment")]
    Rows,
    /// The intake is missing, present without one in the params, for another
    /// intake, or not at the latest commitment's head.
    #[error("the intake does not match the latest commitment")]
    Intake,
    /// Inputs a computation would refuse (canonical order, hashes).
    #[error("the logs are not the book's inputs")]
    Inputs,
    /// More than [`MAX_ITEMS`] rows.
    #[error("too many rows")]
    TooMany,
    /// An unknown log format, or format 2 in a version-1 publication.
    #[error("log format {0} is not supported here")]
    Format(u8),
    /// The registry is not the one the latest commitment names, or is
    /// carried by a version-1 publication.
    #[error("the registry is not the latest commitment's")]
    Registry,
}

/// The canonical log covered by `step` (the first `step.statements` and
/// `step.resolutions` rows, sorted), or `None` when the counts do not fit or
/// a row repeats.
pub fn step_log(
    step: &Step,
    statements: &[Statement],
    resolutions: &[Resolution],
) -> Option<(Vec<Statement>, Vec<Resolution>)> {
    let n = usize::try_from(step.statements).ok()?;
    let m = usize::try_from(step.resolutions).ok()?;
    let mut s = statements.get(..n)?.to_vec();
    let mut r = resolutions.get(..m)?.to_vec();
    sort_statements(&mut s);
    sort_resolutions(&mut r);
    (s.len() == n && r.len() == m).then_some((s, r))
}

/// Check a run of steps over the logs (oldest first, possibly starting
/// mid-chain): counts never fall and the last covers every row; each step's
/// log hashes to its commitment, its params to `params_hash`; each extends the
/// one before ([`check_extends`]). Returns the latest step's log. The first
/// step's own link to its `prev` is the caller's (a whole chain starts at zero,
/// [`Publication::check`]; a served run starts at a commitment the client
/// knew, or is taken on the floor and the pin, `wire::check_log_body`).
pub fn check_steps(
    steps: &[Step],
    statements: &[Statement],
    resolutions: &[Resolution],
) -> Result<(Vec<Statement>, Vec<Resolution>), PublicationError> {
    if statements.len() > MAX_ITEMS || resolutions.len() > MAX_ITEMS {
        return Err(PublicationError::TooMany);
    }
    if steps.len() > MAX_STEPS {
        return Err(PublicationError::TooMany);
    }
    // Each step's log grows from the one before by the rows new to it (an
    // already sorted run plus a sorted chunk: the sort merges the two), so no
    // prefix is rebuilt from scratch (review C20-R1-3).
    let (mut s, mut r): (Vec<Statement>, Vec<Resolution>) = (Vec::new(), Vec::new());
    let mut prev: Option<(&Step, Vec<Statement>, Vec<Resolution>)> = None;
    for (i, step) in steps.iter().enumerate() {
        let (n, m) = (
            usize::try_from(step.statements).map_err(|_| PublicationError::Counts(i))?,
            usize::try_from(step.resolutions).map_err(|_| PublicationError::Counts(i))?,
        );
        if n < s.len() || m < r.len() || n > statements.len() || m > resolutions.len() {
            return Err(PublicationError::Counts(i));
        }
        if prev.is_none() {
            // The first step covers its whole prefix.
            s = statements[..n].to_vec();
            r = resolutions[..m].to_vec();
            sort_statements(&mut s);
            sort_resolutions(&mut r);
        } else {
            let (from_s, from_r) = (
                prev.as_ref().map_or(0, |p| p.0.statements as usize),
                prev.as_ref().map_or(0, |p| p.0.resolutions as usize),
            );
            s.extend_from_slice(&statements[from_s..n]);
            r.extend_from_slice(&resolutions[from_r..m]);
            sort_statements(&mut s);
            sort_resolutions(&mut r);
        }
        // A row twice (the sort drops exact duplicates) is not canonical.
        if s.len() != n || r.len() != m {
            return Err(PublicationError::Log(i));
        }
        let c = &step.commitment;
        if statements_hash(&s) != c.statements_hash || resolutions_hash(&r) != c.resolutions_hash {
            return Err(PublicationError::Log(i));
        }
        if step.params.hash() != c.params_hash {
            return Err(PublicationError::Params(i));
        }
        if let Some((p, ps, pr)) = &prev {
            check_extends(
                &p.commitment,
                TrackLog {
                    params: &p.params,
                    statements: ps,
                    resolutions: pr,
                },
                c,
                TrackLog {
                    params: &step.params,
                    statements: &s,
                    resolutions: &r,
                },
            )
            .map_err(|e| PublicationError::Extend(i, e))?;
        }
        prev = Some((step, s.clone(), r.clone()));
    }
    let (last, s, r) = prev.ok_or(PublicationError::Empty)?;
    if last.statements != statements.len() as u64 || last.resolutions != resolutions.len() as u64 {
        return Err(PublicationError::Counts(steps.len() - 1));
    }
    Ok((s, r))
}

/// The work [`check_steps`] does on `links` steps over `rows` log rows (each
/// step hashes its whole log): a client refuses a body past
/// [`MAX_LOG_CHECK`] before checking it, and a server does not serve one
/// (review C20-R1-3).
pub fn log_check_cost(links: usize, rows: u64) -> u64 {
    (links as u64).saturating_mul(rows)
}

/// Most [`log_check_cost`] a client spends on one log body: 1,048,576 (64
/// commitments over 16,384 rows). Each unit is about 5 microseconds of native
/// work (a link sorts, hashes and runs [`check_extends`] over its whole log,
/// several Keccak hashes a row): measured 4.6 s for 64 links over 16,384 rows,
/// and 21 s at the former 4,194,304 (review C20-R2-1).
pub const MAX_LOG_CHECK: u64 = 1 << 20;

/// The work of checking `links` chained (format-2) steps over `rows` rows:
/// one pass over the rows ([`crate::chained::ChainedCheck`]) plus the links.
/// Counted in the same units as [`log_check_cost`] (a row taken once is
/// cheaper than a row hashed and sorted in a format-1 link), against the same
/// caps.
pub fn chained_check_cost(links: usize, rows: u64) -> u64 {
    rows.saturating_add(links as u64)
}

/// Most steps in one publication.
pub const MAX_STEPS: usize = 1 << 16;

impl Publication {
    /// An empty publication in the first layout (format-1 logs, no registry;
    /// ADR-0036 amendment 5).
    pub fn new() -> Self {
        Publication {
            version: PUBLICATION_VERSION,
            format: LOG_FORMAT_WHOLE,
            ..Default::default()
        }
    }

    /// An empty publication in the second layout with log format `format`
    /// (it can carry the registry; ADR-0036 amendment 6).
    pub fn with_format(format: u8) -> Self {
        Publication {
            version: PUBLICATION_VERSION_2,
            format,
            ..Default::default()
        }
    }

    /// An empty publication with chained logs (format 2): paged when served,
    /// checked in linear time (ROADMAP C21). What a new chain starts with.
    pub fn chained() -> Self {
        Publication::with_format(LOG_FORMAT_CHAINED)
    }

    fn check_format(&self) -> Result<(), PublicationError> {
        match (self.version, self.format) {
            (PUBLICATION_VERSION, LOG_FORMAT_WHOLE) if self.registry.is_none() => Ok(()),
            (PUBLICATION_VERSION, _) => Err(PublicationError::Format(self.format)),
            (PUBLICATION_VERSION_2, LOG_FORMAT_WHOLE | LOG_FORMAT_CHAINED) => Ok(()),
            (PUBLICATION_VERSION_2, f) => Err(PublicationError::Format(f)),
            (v, _) => Err(PublicationError::Version(v)),
        }
    }

    /// True for chained (format-2) logs.
    pub fn is_chained(&self) -> bool {
        self.format == LOG_FORMAT_CHAINED
    }

    /// The latest step.
    pub fn latest(&self) -> Option<&Step> {
        self.steps.last()
    }

    /// Append the computation `book` of `params` over `statements` and
    /// `resolutions` (canonical, as [`crate::compute`] took them), with the
    /// intake at its head. It must extend the latest step ([`check_extends`]);
    /// returns its commitment, chained to the latest (zero for the first).
    pub fn push(
        &mut self,
        params: &TrackParams,
        statements: &[Statement],
        resolutions: &[Resolution],
        book: &TrackBook,
        intake: Option<PublishedIntake>,
    ) -> Result<TrackCommitment, PublicationError> {
        self.push_with(params, statements, resolutions, book, intake, None)
    }

    /// [`Publication::push`] carrying `registry`, the registry `book` was
    /// computed with (its root must be the book's `registry_root`; version 2
    /// only), so clients can rerun `compute` (ROADMAP C23). `None` drops a
    /// registry carried before. With chained logs (format 2) the commitment's
    /// two log hashes are the chained ones over the publication order.
    pub fn push_with(
        &mut self,
        params: &TrackParams,
        statements: &[Statement],
        resolutions: &[Resolution],
        book: &TrackBook,
        intake: Option<PublishedIntake>,
        registry: Option<&StaticRegistry>,
    ) -> Result<TrackCommitment, PublicationError> {
        self.check_format()?;
        let prev = self.latest().map_or(Hash32::ZERO, |s| s.commitment.hash());
        let c = book.commitment(prev);
        if statements_hash(statements) != c.statements_hash
            || resolutions_hash(resolutions) != c.resolutions_hash
            || params.hash() != c.params_hash
        {
            return Err(PublicationError::Inputs);
        }
        let registry = match registry {
            None => None,
            Some(r) if self.version == PUBLICATION_VERSION_2 && r.root() == c.registry_root => {
                Some(r.data())
            }
            Some(_) => return Err(PublicationError::Registry),
        };
        if self.is_chained() {
            return self.push_chained(params, statements, resolutions, book, intake, registry, c);
        }
        check_intake(params, &c, intake.as_ref())?;
        if let Some(last) = self.latest() {
            let (ps, pr) = step_log(last, &self.statements, &self.resolutions)
                .ok_or(PublicationError::Counts(self.steps.len() - 1))?;
            check_extends(
                &last.commitment,
                TrackLog {
                    params: &last.params,
                    statements: &ps,
                    resolutions: &pr,
                },
                &c,
                TrackLog {
                    params,
                    statements,
                    resolutions,
                },
            )
            .map_err(|e| PublicationError::Extend(self.steps.len(), e))?;
        }
        let known: BTreeSet<Hash32> = self.statements.iter().map(Statement::hash).collect();
        self.statements.extend(
            statements
                .iter()
                .filter(|s| !known.contains(&s.hash()))
                .cloned(),
        );
        let known: BTreeSet<Hash32> = self.resolutions.iter().map(Resolution::hash).collect();
        self.resolutions.extend(
            resolutions
                .iter()
                .filter(|r| !known.contains(&r.hash()))
                .cloned(),
        );
        self.steps.push(Step {
            commitment: c.clone(),
            params: params.clone(),
            statements: self.statements.len() as u64,
            resolutions: self.resolutions.len() as u64,
        });
        self.entries = book.entries().to_vec();
        self.records = book.records().to_vec();
        self.metrics = book.metrics().to_vec();
        self.intake = intake;
        self.registry = registry;
        Ok(c)
    }

    /// [`Publication::push_with`] for chained logs: every earlier row must be
    /// in the new log (none dropped), the new rows follow in canonical order,
    /// the commitment takes the chained hashes, and the last two steps must
    /// pass the same check a node and a client run ([`check_chained`]).
    #[allow(clippy::too_many_arguments)]
    fn push_chained(
        &mut self,
        params: &TrackParams,
        statements: &[Statement],
        resolutions: &[Resolution],
        book: &TrackBook,
        intake: Option<PublishedIntake>,
        registry: Option<RegistryData>,
        mut c: TrackCommitment,
    ) -> Result<TrackCommitment, PublicationError> {
        let given: BTreeSet<Hash32> = statements.iter().map(Statement::hash).collect();
        if self.statements.iter().any(|s| !given.contains(&s.hash())) {
            return Err(PublicationError::Extend(
                self.steps.len(),
                crate::ExtendError::StatementDropped,
            ));
        }
        let given: BTreeSet<Hash32> = resolutions.iter().map(Resolution::hash).collect();
        if self.resolutions.iter().any(|r| !given.contains(&r.hash())) {
            return Err(PublicationError::Extend(
                self.steps.len(),
                crate::ExtendError::ResolutionDropped,
            ));
        }
        let mut next = self.clone();
        let known: BTreeSet<Hash32> = next.statements.iter().map(Statement::hash).collect();
        next.statements.extend(
            statements
                .iter()
                .filter(|s| !known.contains(&s.hash()))
                .cloned(),
        );
        let known: BTreeSet<Hash32> = next.resolutions.iter().map(Resolution::hash).collect();
        next.resolutions.extend(
            resolutions
                .iter()
                .filter(|r| !known.contains(&r.hash()))
                .cloned(),
        );
        c.statements_hash = chained_statements_hash(&next.statements);
        c.resolutions_hash = chained_resolutions_hash(&next.resolutions);
        check_intake(params, &c, intake.as_ref())?;
        next.steps.push(Step {
            commitment: c.clone(),
            params: params.clone(),
            statements: next.statements.len() as u64,
            resolutions: next.resolutions.len() as u64,
        });
        let from = next.steps.len().saturating_sub(2);
        check_chained(
            &next.steps[from..],
            &next.statements,
            &next.resolutions,
            false,
        )
        .map_err(|e| match e {
            PublicationError::Extend(i, x) => PublicationError::Extend(from + i, x),
            e => e,
        })?;
        next.entries = book.entries().to_vec();
        next.records = book.records().to_vec();
        next.metrics = book.metrics().to_vec();
        next.intake = intake;
        next.registry = registry;
        *self = next;
        Ok(c)
    }

    /// Check the whole publication (what a node does before serving it): the
    /// version, a chain from its first commitment ([`check_steps`]), the
    /// latest book's rows, and the intake. Returns the latest book.
    pub fn check(&self) -> Result<TrackBook, PublicationError> {
        self.check_format()?;
        let first = self.steps.first().ok_or(PublicationError::Empty)?;
        if first.commitment.prev != Hash32::ZERO {
            return Err(PublicationError::NotFirst);
        }
        for (i, w) in self.steps.windows(2).enumerate() {
            if w[1].commitment.prev != w[0].commitment.hash() {
                return Err(PublicationError::Extend(i + 1, ExtendError::NotChained));
            }
        }
        if self.is_chained() {
            check_chained(&self.steps, &self.statements, &self.resolutions, false)?;
        } else {
            check_steps(&self.steps, &self.statements, &self.resolutions)?;
        }
        self.latest_book()
    }

    /// What checking this publication costs when its first `trusted` steps
    /// were checked before ([`Publication::check_after`]): format 1 hashes
    /// every checked step's whole log ([`log_check_cost`]); format 2 is one
    /// pass over the rows and the steps ([`chained_check_cost`]).
    pub fn check_cost(&self, trusted: usize) -> u64 {
        let rows = (self.statements.len() as u64).saturating_add(self.resolutions.len() as u64);
        if self.is_chained() {
            chained_check_cost(self.steps.len(), rows)
        } else {
            let checked = self.steps.len().saturating_sub(trusted.saturating_sub(1));
            log_check_cost(checked, rows)
        }
    }

    /// [`Publication::check`] for a publication whose first `trusted` steps
    /// were checked before (a node that served them; the caller has checked
    /// that this one [`Publication::starts_with`] them): the run from the last
    /// trusted step on and the latest rows only (review C20-R1-2).
    pub fn check_after(&self, trusted: usize) -> Result<TrackBook, PublicationError> {
        // Chained logs are checked whole: one linear pass, which also builds
        // the state the new steps are checked against.
        if trusted == 0 || trusted > self.steps.len() || self.is_chained() {
            return self.check();
        }
        self.check_format()?;
        let run = &self.steps[trusted - 1..];
        for (i, w) in run.windows(2).enumerate() {
            if w[1].commitment.prev != w[0].commitment.hash() {
                return Err(PublicationError::Extend(
                    trusted + i,
                    ExtendError::NotChained,
                ));
            }
        }
        check_steps(run, &self.statements, &self.resolutions)?;
        self.latest_book()
    }

    /// The latest book from its rows, and the intake checked against it,
    /// without rechecking the logs (a node that has checked the steps before).
    pub fn latest_book(&self) -> Result<TrackBook, PublicationError> {
        let last = self.latest().ok_or(PublicationError::Empty)?;
        check_intake(&last.params, &last.commitment, self.intake.as_ref())?;
        TrackBook::from_committed(
            &last.commitment,
            self.entries.clone(),
            self.records.clone(),
            self.metrics.clone(),
        )
        .ok_or(PublicationError::Rows)
    }

    /// True when `earlier` is a prefix of this publication: the same first
    /// steps and the same first rows of each log (so a node never serves a
    /// file that rewrote what it served before).
    pub fn starts_with(&self, earlier: &Publication) -> bool {
        self.steps.get(..earlier.steps.len()) == Some(&earlier.steps[..])
            && self.statements.get(..earlier.statements.len()) == Some(&earlier.statements[..])
            && self.resolutions.get(..earlier.resolutions.len()) == Some(&earlier.resolutions[..])
    }

    /// Resolutions at level 1 or 2 (a recomputed row or a level-2 result the
    /// paid API sells): a server serves the logs free only when there are
    /// none (ADR-0036 amendment 5 item 2).
    pub fn priced_answers(&self) -> usize {
        priced_answers(&self.resolutions)
    }

    /// Rows a log body carries: statements, resolutions and revealed intake rows.
    pub fn log_rows(&self) -> u64 {
        (self.statements.len() as u64)
            .saturating_add(self.resolutions.len() as u64)
            .saturating_add(
                self.intake
                    .as_ref()
                    .map_or(0, PublishedIntake::revealed_rows),
            )
    }
}

/// Resolutions at level 1 or 2 in `resolutions`.
pub fn priced_answers(resolutions: &[Resolution]) -> usize {
    resolutions
        .iter()
        .filter(|r| matches!(r.level.number(), 1 | 2))
        .count()
}

/// The intake must be present exactly when the params name one, for that
/// intake, at the commitment's head.
pub fn check_intake(
    params: &TrackParams,
    c: &TrackCommitment,
    intake: Option<&PublishedIntake>,
) -> Result<(), PublicationError> {
    match intake {
        None if params.intake == Hash32::ZERO => Ok(()),
        Some(i)
            if params.intake != Hash32::ZERO
                && i.intake == params.intake
                && i.head.timestamp == c.as_of
                && i.posts.len() == i.batches.len() =>
        {
            Ok(())
        }
        _ => Err(PublicationError::Intake),
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
