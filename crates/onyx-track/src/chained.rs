//! Log format 2: chained prefix hashes over the logs in publication order
//! (ROADMAP C21; ADR-0036 amendment 6).
//!
//! In format 1 a commitment's `statements_hash` is one Keccak over its whole
//! canonical (sorted) log, so every commitment of a chain hashes its whole log
//! again and checking a chain costs links times rows (THREATS TR-C20-5,
//! TR-C20-R2-2). In format 2 the logs are kept in publication order (each
//! commitment covers a prefix: the rows of the one before, then its own new
//! rows) and hashed row by row:
//!
//! - `head_0 = 0^32`, `head_i = keccak256(domain_row ‖ head_{i-1} ‖ borsh(row_i))`;
//! - `statements_hash = keccak256("onyx-track-statements-v2" ‖ borsh(n: u64) ‖ head_n)`,
//!   and the same for resolutions with their own domains.
//!
//! The commitment's layout and hash domain do not change (`OnyxSignedBook`
//! checks both); only what its two log hashes are over does, and the domains
//! tell the formats apart: a format-1 hash never checks as a format-2 one or
//! the other way round, so nothing old is reinterpreted. A client feeds the
//! rows in order to one [`ChainedCheck`], which checks every commitment's
//! hashes as it passes them and every extension rule of `check_extends` row by
//! row, in time linear in the rows (plus the links): each served page costs
//! only its own rows.
//!
//! Pure: no I/O, no clock, never a proof level or money (ADR-0036 item 12).

use crate::publication::{PublicationError, Step, MAX_STEPS};
use crate::{
    hash_with, sort_resolutions, sort_statements, ExtendError, Resolution, Statement, MAX_ITEMS,
};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{keccak256, Hash32, SourceId};
use std::collections::{BTreeMap, BTreeSet};

/// Log format 1: one hash over the whole canonical log (ADR-0036 item 8).
pub const LOG_FORMAT_WHOLE: u8 = 1;
/// Log format 2: chained row hashes in publication order (this module).
pub const LOG_FORMAT_CHAINED: u8 = 2;

/// Domain of one statement link of the chain.
pub const STATEMENT_ROW_DOMAIN: &[u8] = b"onyx-track-statement-row-v2";
/// Domain of one resolution link of the chain.
pub const RESOLUTION_ROW_DOMAIN: &[u8] = b"onyx-track-resolution-row-v2";
/// Domain of a format-2 `statements_hash`.
pub const STATEMENTS_V2_DOMAIN: &[u8] = b"onyx-track-statements-v2";
/// Domain of a format-2 `resolutions_hash`.
pub const RESOLUTIONS_V2_DOMAIN: &[u8] = b"onyx-track-resolutions-v2";

/// One row of a served log stream.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum LogRow {
    /// A statement.
    Statement(Statement),
    /// A resolution.
    Resolution(Resolution),
}

/// A running chain over one log: rows absorbed and the head.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Head {
    /// Rows absorbed.
    pub rows: u64,
    /// `head_rows` (zero for none).
    pub head: Hash32,
}

impl Default for Head {
    fn default() -> Self {
        Head {
            rows: 0,
            head: Hash32::ZERO,
        }
    }
}

impl Head {
    fn absorb<T: BorshSerialize>(&mut self, domain: &[u8], row: &T) {
        let mut b = domain.to_vec();
        b.extend_from_slice(&self.head.0);
        b.extend_from_slice(&borsh::to_vec(row).unwrap_or_default());
        self.head = keccak256(&b);
        self.rows += 1;
    }

    fn hash(&self, domain: &[u8]) -> Hash32 {
        hash_with(domain, &(self.rows, self.head))
    }
}

/// The format-2 `statements_hash` of a log in publication order.
pub fn chained_statements_hash(rows: &[Statement]) -> Hash32 {
    let mut h = Head::default();
    for r in rows {
        h.absorb(STATEMENT_ROW_DOMAIN, r);
    }
    h.hash(STATEMENTS_V2_DOMAIN)
}

/// The format-2 `resolutions_hash` of a list in publication order.
pub fn chained_resolutions_hash(rows: &[Resolution]) -> Hash32 {
    let mut h = Head::default();
    for r in rows {
        h.absorb(RESOLUTION_ROW_DOMAIN, r);
    }
    h.hash(RESOLUTIONS_V2_DOMAIN)
}

/// Rows of a log stream over `links` (the run a client checks, oldest
/// first): segment 0 is every row the first link covers (statements, then
/// resolutions), segment `j` the rows new to link `j` (statements, then
/// resolutions).
pub fn stream_rows(links: &[Step]) -> u64 {
    links
        .last()
        .map_or(0, |l| l.statements.saturating_add(l.resolutions))
}

/// Which kind of row comes at the next position of the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A statement.
    Statement,
    /// A resolution.
    Resolution,
}

/// The incremental check of a run of format-2 links over its row stream
/// (oldest first, possibly starting mid-chain; [`stream_rows`] gives the
/// order). Equivalent, link by link, to format 1's `check_steps`: each link's
/// log hashes to it, its params to `params_hash`, and each link extends the
/// one before as [`crate::check_extends`] requires (chained, later, params
/// kept, no operator for a held source, every earlier row kept, no backdated
/// statement, no resolution rewriting a committed outcome). Rows are kept
/// only when asked ([`ChainedCheck::new`]'s `keep`).
#[derive(Debug)]
pub struct ChainedCheck {
    links: Vec<Step>,
    seg: usize,
    statements: Head,
    resolutions: Head,
    seen_statements: BTreeSet<Hash32>,
    seen_resolutions: BTreeSet<Hash32>,
    /// Sources holding a statement or a resolution in the links before `seg`.
    held: BTreeSet<SourceId>,
    /// Facts named by a statement before `seg`.
    named: BTreeSet<Hash32>,
    /// Per fact resolved before `seg`, its weakest answer's level number.
    weakest: BTreeMap<Hash32, u8>,
    pending_held: Vec<SourceId>,
    pending_named: Vec<Hash32>,
    pending_weakest: Vec<(Hash32, u8)>,
    keep: bool,
    kept_statements: Vec<Statement>,
    kept_resolutions: Vec<Resolution>,
}

impl ChainedCheck {
    /// Start a check of `links`: counts never fall, each link's params hash
    /// to it, each link is chained to the one before, later, with its params
    /// kept. Completes leading links with no rows.
    pub fn new(links: Vec<Step>, keep: bool) -> Result<Self, PublicationError> {
        if links.is_empty() {
            return Err(PublicationError::Empty);
        }
        if links.len() > MAX_STEPS {
            return Err(PublicationError::TooMany);
        }
        for (i, l) in links.iter().enumerate() {
            if l.params.hash() != l.commitment.params_hash {
                return Err(PublicationError::Params(i));
            }
            if i == 0 {
                continue;
            }
            let p = &links[i - 1];
            if l.statements < p.statements || l.resolutions < p.resolutions {
                return Err(PublicationError::Counts(i));
            }
            let e = |e| Err(PublicationError::Extend(i, e));
            if l.commitment.prev != p.commitment.hash() {
                return e(ExtendError::NotChained);
            }
            if l.commitment.as_of <= p.commitment.as_of {
                return e(ExtendError::TimeNotForward);
            }
            if !l.params.extends(&p.params) {
                return e(ExtendError::ParamsChanged);
            }
        }
        let last = &links[links.len() - 1];
        if last.statements > MAX_ITEMS as u64 || last.resolutions > MAX_ITEMS as u64 {
            return Err(PublicationError::TooMany);
        }
        let mut c = ChainedCheck {
            links,
            seg: 0,
            statements: Head::default(),
            resolutions: Head::default(),
            seen_statements: BTreeSet::new(),
            seen_resolutions: BTreeSet::new(),
            held: BTreeSet::new(),
            named: BTreeSet::new(),
            weakest: BTreeMap::new(),
            pending_held: Vec::new(),
            pending_named: Vec::new(),
            pending_weakest: Vec::new(),
            keep,
            kept_statements: Vec::new(),
            kept_resolutions: Vec::new(),
        };
        c.settle()?;
        Ok(c)
    }

    /// The links being checked.
    pub fn links(&self) -> &[Step] {
        &self.links
    }

    /// Rows taken so far (the stream position).
    pub fn position(&self) -> u64 {
        self.statements.rows + self.resolutions.rows
    }

    /// The chain heads so far (statements, resolutions): what a page names as
    /// its prefix hash.
    pub fn heads(&self) -> (Head, Head) {
        (self.statements, self.resolutions)
    }

    /// The kind of the next row, or `None` when every link is checked.
    pub fn next_kind(&self) -> Option<Kind> {
        let l = self.links.get(self.seg)?;
        if self.statements.rows < l.statements {
            Some(Kind::Statement)
        } else {
            Some(Kind::Resolution)
        }
    }

    /// True when every link is checked.
    pub fn done(&self) -> bool {
        self.seg == self.links.len()
    }

    fn prev_as_of(&self) -> Option<u64> {
        self.seg
            .checked_sub(1)
            .map(|p| self.links[p].commitment.as_of)
    }

    /// Take the next statement of the stream.
    pub fn statement(&mut self, s: &Statement) -> Result<(), PublicationError> {
        if self.next_kind() != Some(Kind::Statement) {
            return Err(PublicationError::Counts(self.seg));
        }
        let j = self.seg;
        if !self.seen_statements.insert(s.hash()) {
            return Err(PublicationError::Log(j));
        }
        self.statements.absorb(STATEMENT_ROW_DOMAIN, s);
        if let Some(prev_as_of) = self.prev_as_of() {
            // New to link j: revealable only after the link before
            // (`check_extends`'s Backdated rule, with link j's params).
            let params = &self.links[j].params;
            let sealed = params.intake != Hash32::ZERO;
            let at = match params.reveal_of(&s.fact) {
                Some(c) if sealed => c,
                _ => s.committed_at,
            };
            if s.committed_at.max(at) <= prev_as_of {
                return Err(PublicationError::Extend(j, ExtendError::Backdated));
            }
        }
        self.pending_held.push(s.source.clone());
        self.pending_named.push(s.fact.key());
        if self.keep {
            self.kept_statements.push(s.clone());
        }
        self.settle()
    }

    /// Take the next resolution of the stream.
    pub fn resolution(&mut self, r: &Resolution) -> Result<(), PublicationError> {
        if self.next_kind() != Some(Kind::Resolution) {
            return Err(PublicationError::Counts(self.seg));
        }
        let j = self.seg;
        if !self.seen_resolutions.insert(r.hash()) {
            return Err(PublicationError::Log(j));
        }
        self.resolutions.absorb(RESOLUTION_ROW_DOMAIN, r);
        let fact = r.fact.key();
        let level = r.level.number();
        if let Some(prev_as_of) = self.prev_as_of() {
            // `check_extends`'s ResolutionRewrites rule against the links
            // before j (amendment 3 item 6).
            let weakest = self.weakest.get(&fact).copied();
            let stronger = weakest.is_some_and(|l| level < l);
            let known = weakest.is_some() || self.named.contains(&fact);
            if stronger || (known && r.public_from <= prev_as_of) {
                return Err(PublicationError::Extend(j, ExtendError::ResolutionRewrites));
            }
        }
        self.pending_held.push(r.resolver.clone());
        self.pending_weakest.push((fact, level));
        if self.keep {
            self.kept_resolutions.push(r.clone());
        }
        self.settle()
    }

    /// Take one row of the stream.
    pub fn row(&mut self, row: &LogRow) -> Result<(), PublicationError> {
        match row {
            LogRow::Statement(s) => self.statement(s),
            LogRow::Resolution(r) => self.resolution(r),
        }
    }

    /// Complete every link whose rows are all in: its hashes, then the state
    /// the next link is checked against, then the next link's operators.
    fn settle(&mut self) -> Result<(), PublicationError> {
        while let Some(l) = self.links.get(self.seg) {
            if self.statements.rows != l.statements || self.resolutions.rows != l.resolutions {
                return Ok(());
            }
            let c = &l.commitment;
            if self.statements.hash(STATEMENTS_V2_DOMAIN) != c.statements_hash
                || self.resolutions.hash(RESOLUTIONS_V2_DOMAIN) != c.resolutions_hash
            {
                return Err(PublicationError::Log(self.seg));
            }
            self.held.extend(self.pending_held.drain(..));
            self.named.extend(self.pending_named.drain(..));
            for (fact, level) in self.pending_weakest.drain(..) {
                let w = self.weakest.entry(fact).or_insert(level);
                *w = (*w).max(level);
            }
            self.seg += 1;
            if let Some(next) = self.links.get(self.seg) {
                // An operator may be declared only for a source the earlier
                // logs never held (TR-R2-4).
                let prev = &l.params;
                if next
                    .params
                    .operators
                    .iter()
                    .any(|o| prev.operator_of(&o.source).is_none() && self.held.contains(&o.source))
                {
                    return Err(PublicationError::Extend(
                        self.seg,
                        ExtendError::OperatorRegrouped,
                    ));
                }
            }
        }
        Ok(())
    }

    /// The end of the stream: every link checked. Returns the latest link's
    /// log in canonical order (empty unless rows were kept).
    pub fn finish(self) -> Result<(Vec<Statement>, Vec<Resolution>), PublicationError> {
        if !self.done() {
            return Err(PublicationError::Counts(self.seg.min(self.links.len() - 1)));
        }
        let (mut s, mut r) = (self.kept_statements, self.kept_resolutions);
        sort_statements(&mut s);
        sort_resolutions(&mut r);
        Ok((s, r))
    }
}

/// Feed the whole stream of `links` over the logs (publication order) to a
/// [`ChainedCheck`]: what a node does with a file and a client with one body.
pub fn check_chained(
    links: &[Step],
    statements: &[Statement],
    resolutions: &[Resolution],
    keep: bool,
) -> Result<(Vec<Statement>, Vec<Resolution>), PublicationError> {
    if statements.len() > MAX_ITEMS || resolutions.len() > MAX_ITEMS {
        return Err(PublicationError::TooMany);
    }
    let last = links.last().ok_or(PublicationError::Empty)?;
    if last.statements != statements.len() as u64 || last.resolutions != resolutions.len() as u64 {
        return Err(PublicationError::Counts(links.len() - 1));
    }
    let mut c = ChainedCheck::new(links.to_vec(), keep)?;
    let (mut s, mut r) = (0usize, 0usize);
    for l in links {
        let (n, m) = (l.statements as usize, l.resolutions as usize);
        if n < s || m < r {
            return Err(PublicationError::Counts(0));
        }
        for x in &statements[s..n] {
            c.statement(x)?;
        }
        for x in &resolutions[r..m] {
            c.resolution(x)?;
        }
        (s, r) = (n, m);
    }
    c.finish()
}

/// Rows `[from, from + n)` of the stream of `links` (fewer at its end).
pub fn rows_from(
    links: &[Step],
    statements: &[Statement],
    resolutions: &[Resolution],
    from: u64,
    n: u64,
) -> Vec<LogRow> {
    // Locate the segment once, then walk: a page costs its own rows plus a
    // pass over the links.
    let end = from.saturating_add(n).min(stream_rows(links));
    let mut out = Vec::new();
    let (mut s, mut r, mut at) = (0u64, 0u64, 0u64);
    for l in links {
        let (seg_s, seg_r) = (
            l.statements.saturating_sub(s),
            l.resolutions.saturating_sub(r),
        );
        for (base, count, kind) in [(s, seg_s, Kind::Statement), (r, seg_r, Kind::Resolution)] {
            let (lo, hi) = (at, at + count);
            let (a, b) = (from.max(lo), end.min(hi));
            for p in a..b {
                let i = (base + (p - lo)) as usize;
                let row = match kind {
                    Kind::Statement => statements.get(i).cloned().map(LogRow::Statement),
                    Kind::Resolution => resolutions.get(i).cloned().map(LogRow::Resolution),
                };
                if let Some(x) = row {
                    out.push(x);
                }
            }
            at = hi;
        }
        (s, r) = (l.statements, l.resolutions);
        if at >= end {
            break;
        }
    }
    out
}

#[cfg(test)]
#[path = "chained_tests.rs"]
mod tests;
