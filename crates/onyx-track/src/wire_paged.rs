//! Chained (format-2) logs served in pages, and the registry served for
//! reruns (ROADMAP C21, C23; ADR-0036 amendment 6; docs/spec/delivery.md
//! "Track records").
//!
//! - `GET /v1/track/log[?since=]` on chained logs answers the head
//!   ([`chained_log_body`]): the links from `since` (or the latest), the
//!   number of rows of their stream and the intake's head without its
//!   batches. A client starts a [`PagedLog`] on it.
//! - `GET /v1/track/log?at=<latest>&since=<first link>&from=<n>` answers rows
//!   `n..n + LOG_PAGE_ROWS` of the stream ([`log_rows_page`]); the client
//!   takes them in order ([`PagedLog::take_rows`]): each row extends the chain
//!   and is checked against the extension rules as it arrives, and each
//!   link's hashes are checked when its last row is in.
//! - `GET /v1/track/log?at=<latest>&posts=<n>` answers the intake's posts
//!   from `n` with their revealed batches ([`log_posts_page`],
//!   [`PagedLog::take_posts`]).
//! - `GET /v1/track/registry` answers the registry the latest commitment
//!   names, as data ([`registry_body`]); a client rebuilds it with its own
//!   address rules and uses it only when it hashes to `registry_root`
//!   ([`check_registry_body`]).

use super::log::rerun;
use super::{borsh_hex, obj, parse_borsh, parse_h32, Json, WireError, MAX_CHAIN};
use crate::chained::{rows_from, stream_rows, ChainedCheck, LogRow, LOG_FORMAT_CHAINED};
use crate::intake::{Anchor, KeyBook, Post, PublishedIntake, RevealedBatch};
use crate::publication::{chained_check_cost, Publication, Step, MAX_LOG_CHECK};
use crate::wire::{log_is_example, CheckedLog, LogError, LogExpect};
use crate::TrackCommitment;
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{Hash32, RegistryData, RuleBuilder, StaticRegistry};
use serde_json::json;

/// Rows of the stream in one page (every page but the last has exactly this many).
pub const LOG_PAGE_ROWS: u64 = 4_096;
/// Largest page a client parses (its hex): [`LOG_PAGE_ROWS`] rows, or posts
/// within [`LOG_PAGE_ROWS`] revealed statements. One batch of more than about
/// 27,000 revealed statements with their proofs does not fit, and a server
/// answers 413 for it (review C21-R1-2; TODO ROADMAP C25: page inside a batch).
pub const MAX_LOG_PAGE_BYTES: usize = 48 << 20;
/// Most hex a client takes over all pages of one chained log (review
/// C21-R1-1): an honest log of a million rows is about 220 MiB of hex.
pub const MAX_PAGED_LOG_BYTES: u64 = 512 << 20;
/// Largest registry body a client parses.
pub const MAX_REGISTRY_BODY_BYTES: usize = 8 << 20;
/// Top-level keys of a chained log head (exact, fail closed).
pub const CHAINED_LOG_KEYS: [&str; 9] = [
    "level",
    "example",
    "asOf",
    "commitmentHash",
    "format",
    "links",
    "rows",
    "pageRows",
    "intake",
];
/// Top-level keys of a rows page (exact).
pub const ROWS_PAGE_KEYS: [&str; 5] = ["level", "commitmentHash", "since", "from", "rows"];
/// Top-level keys of a posts page (exact).
pub const POSTS_PAGE_KEYS: [&str; 4] = ["level", "commitmentHash", "from", "posts"];
/// Top-level keys of a registry body (exact).
pub const REGISTRY_KEYS: [&str; 4] = ["level", "commitmentHash", "registryRoot", "registry"];

/// The intake at the latest commitment's head without its batches (they come
/// in pages): its id, head, key books and how many posts it has.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct IntakeHead {
    /// [`crate::intake::IntakeId::id`].
    pub intake: Hash32,
    /// The finalized head.
    pub head: Anchor,
    /// Every key book a header names.
    pub books: Vec<KeyBook>,
    /// Posts at or below the head (each with one revealed batch).
    pub posts: u64,
}

/// The head of chained logs from step `from` to the latest. `None` for
/// format-1 logs or without a step there.
pub fn chained_log_body(p: &Publication, from: usize) -> Option<Json> {
    if !p.is_chained() {
        return None;
    }
    let last = p.latest()?;
    let links = p.steps.get(from..).filter(|l| !l.is_empty())?;
    let intake = p.intake.as_ref().map(|i| IntakeHead {
        intake: i.intake,
        head: i.head,
        books: i.books.clone(),
        posts: i.posts.len() as u64,
    });
    Some(json!({
        "level": 5,
        "example": log_is_example(&last.params),
        "asOf": last.commitment.as_of,
        "commitmentHash": last.commitment.hash().to_string(),
        "format": LOG_FORMAT_CHAINED,
        "links": links.iter().map(borsh_hex).collect::<Vec<_>>(),
        "rows": stream_rows(links),
        "pageRows": LOG_PAGE_ROWS,
        "intake": intake.as_ref().map(borsh_hex),
    }))
}

/// Rows `pos..pos + LOG_PAGE_ROWS` of the stream from step `from` (the run a
/// client asked for with `since`). `None` past the end.
pub fn log_rows_page(p: &Publication, from: usize, pos: u64) -> Option<Json> {
    let last = p.latest()?;
    let links = p.steps.get(from..).filter(|l| !l.is_empty())?;
    if !p.is_chained() || pos >= stream_rows(links) {
        return None;
    }
    let rows = rows_from(links, &p.statements, &p.resolutions, pos, LOG_PAGE_ROWS);
    Some(json!({
        "level": 5,
        "commitmentHash": last.commitment.hash().to_string(),
        "since": links[0].commitment.hash().to_string(),
        "from": pos,
        "rows": borsh_hex(&rows),
    }))
}

/// The posts from `from` with their batches: as many whole posts as fit in
/// [`LOG_PAGE_ROWS`] revealed statements, at least one. `None` without an
/// intake or past its last post.
pub fn log_posts_page(p: &Publication, from: u64) -> Option<Json> {
    let last = p.latest()?;
    let i = p.intake.as_ref()?;
    let start = usize::try_from(from).ok()?;
    if !p.is_chained() || start >= i.posts.len() {
        return None;
    }
    let mut page: Vec<(Post, RevealedBatch)> = Vec::new();
    let mut rows = 0u64;
    for (post, batch) in i.posts[start..].iter().zip(&i.batches[start..]) {
        let n = batch.statements.len() as u64;
        // At most LOG_PAGE_ROWS posts too, so empty batches do not pile up
        // on one page (review C21-R1-4).
        if !page.is_empty() && (rows + n > LOG_PAGE_ROWS || page.len() as u64 >= LOG_PAGE_ROWS) {
            break;
        }
        rows += n;
        page.push((*post, batch.clone()));
    }
    Some(json!({
        "level": 5,
        "commitmentHash": last.commitment.hash().to_string(),
        "from": from,
        "posts": borsh_hex(&page),
    }))
}

/// The registry of the latest commitment, as data. `None` without one.
pub fn registry_body(p: &Publication) -> Option<Json> {
    let last = p.latest()?;
    let r = p.registry.as_ref()?;
    Some(json!({
        "level": 5,
        "commitmentHash": last.commitment.hash().to_string(),
        "registryRoot": last.commitment.registry_root.to_string(),
        "registry": borsh_hex(r),
    }))
}

/// Check a served registry for the commitment `c` the client checked: shape,
/// the same commitment and `registry_root`, then a rebuild with the client's
/// own `rules` that must hash to `registry_root`. Only then is it used.
pub fn check_registry_body(
    body: &Json,
    c: &TrackCommitment,
    rules: &dyn RuleBuilder,
) -> Result<StaticRegistry, LogError> {
    let malformed = |what| move |_: WireError| LogError::Malformed(what);
    let o = obj(body, &REGISTRY_KEYS, "registry body").map_err(malformed("registry body"))?;
    if o["level"].as_u64() != Some(5) {
        return Err(LogError::Malformed("level"));
    }
    let at =
        parse_h32(&o["commitmentHash"], "commitmentHash").map_err(malformed("commitmentHash"))?;
    let root = parse_h32(&o["registryRoot"], "registryRoot").map_err(malformed("registryRoot"))?;
    if at != c.hash() {
        return Err(LogError::Commitment);
    }
    if root != c.registry_root {
        return Err(LogError::RegistryDiffers);
    }
    let data: RegistryData =
        parse_borsh(&o["registry"], "registry").map_err(malformed("registry"))?;
    Ok(data.build_checked(rules, c.registry_root)?)
}

/// A client's check of chained logs, page by page (ROADMAP C21). Each page
/// costs its own rows: a row extends its log's chain and is checked against
/// the extension rules when it arrives ([`ChainedCheck`]), a link's hashes
/// when its last row is in; the rerun of `compute` and `verify_log` run once
/// at [`PagedLog::finish`] over the whole log, within [`MAX_LOG_CHECK`].
#[derive(Debug)]
pub struct PagedLog {
    check: ChainedCheck,
    first: Hash32,
    latest: Hash32,
    rows: u64,
    example: bool,
    intake: Option<IntakeHead>,
    posts: Vec<Post>,
    batches: Vec<RevealedBatch>,
    revealed: u64,
    /// Revealed statements taken so far, and posts pages taken (review
    /// C21-R2-4: [`posts_pages_within`]).
    revealed_rows: u64,
    post_pages: u64,
    bytes: u64,
    max_bytes: u64,
}

/// True when `pages` posts pages holding `rows` revealed statements over
/// `posts` posts in all are no more than a server filling each page needs
/// (review C21-R2-4). A server ends a page (but the last) only at
/// [`LOG_PAGE_ROWS`] posts or when the next post would take it past
/// [`LOG_PAGE_ROWS`] revealed statements, so two pages in a row hold more than
/// that many statements unless the first is full of posts: `(pages - 1) ×
/// LOG_PAGE_ROWS ≤ 2 × rows + posts`. Rows pages hold exactly
/// [`LOG_PAGE_ROWS`] rows for the same reason.
pub fn posts_pages_within(pages: u64, rows: u64, posts: u64) -> bool {
    pages.saturating_sub(1).saturating_mul(LOG_PAGE_ROWS)
        <= rows.saturating_mul(2).saturating_add(posts)
}

/// The hex of a page's payload, counted against the page cap and the
/// running total before it is decoded.
fn take_bytes(v: &Json, bytes: &mut u64, max: u64) -> Result<(), LogError> {
    let n = v.as_str().map_or(0, str::len);
    *bytes = bytes.saturating_add(n as u64);
    if n > MAX_LOG_PAGE_BYTES || *bytes > max {
        return Err(LogError::TooLarge);
    }
    Ok(())
}

impl PagedLog {
    /// Check the head (shape, the latest commitment and the example flag, the
    /// pin, the floor, `known` among the links, the stream's size and the
    /// client's budget, the links themselves) and start the stream check.
    pub fn start(body: &Json, expect: &LogExpect<'_>) -> Result<PagedLog, LogError> {
        let malformed = |what| move |_: WireError| LogError::Malformed(what);
        let o = obj(body, &CHAINED_LOG_KEYS, "log head").map_err(malformed("log head"))?;
        if o["level"].as_u64() != Some(5) {
            return Err(LogError::Malformed("level"));
        }
        if o["format"].as_u64() != Some(u64::from(LOG_FORMAT_CHAINED)) {
            return Err(LogError::Malformed("format"));
        }
        if o["pageRows"].as_u64() != Some(LOG_PAGE_ROWS) {
            return Err(LogError::Malformed("pageRows"));
        }
        let links = o["links"]
            .as_array()
            .filter(|l| !l.is_empty() && l.len() <= MAX_CHAIN + 1)
            .ok_or(LogError::Malformed("links"))?
            .iter()
            .map(|v| parse_borsh::<Step>(v, "link"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(malformed("link"))?;
        let intake: Option<IntakeHead> = match &o["intake"] {
            Json::Null => None,
            v => Some(parse_borsh(v, "intake").map_err(malformed("intake"))?),
        };
        let last = links.last().ok_or(LogError::Malformed("links"))?;
        let c = &last.commitment;
        let hash = parse_h32(&o["commitmentHash"], "commitmentHash")
            .map_err(malformed("commitmentHash"))?;
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
        let rows = stream_rows(&links);
        if o["rows"].as_u64() != Some(rows) {
            return Err(LogError::Malformed("rows"));
        }
        if chained_check_cost(links.len(), rows) > MAX_LOG_CHECK {
            return Err(LogError::TooLarge);
        }
        if intake.as_ref().is_some_and(|i| i.posts > MAX_LOG_CHECK) {
            return Err(LogError::TooLarge);
        }
        if intake.is_some() != (last.params.intake != Hash32::ZERO) {
            return Err(LogError::Steps(
                crate::publication::PublicationError::Intake,
            ));
        }
        let first = links[0].commitment.hash();
        Ok(PagedLog {
            check: ChainedCheck::new(links, true)?,
            first,
            latest: hash,
            rows,
            example,
            intake,
            posts: Vec::new(),
            batches: Vec::new(),
            revealed: 0,
            revealed_rows: 0,
            post_pages: 0,
            bytes: 0,
            max_bytes: MAX_PAGED_LOG_BYTES,
        })
    }

    /// This check with another running byte cap (tests; clients keep
    /// [`MAX_PAGED_LOG_BYTES`]).
    pub fn with_byte_cap(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    /// The first link of the run (what the pages name as `since`).
    pub fn since(&self) -> Hash32 {
        self.first
    }

    /// The latest commitment (what the pages name as `at`).
    pub fn at(&self) -> Hash32 {
        self.latest
    }

    /// The next stream position to ask for, or `None` when every row is in.
    pub fn next_rows(&self) -> Option<u64> {
        let p = self.check.position();
        (p < self.rows).then_some(p)
    }

    /// The next post to ask for, or `None` when every post is in (or there
    /// is no intake).
    pub fn next_posts(&self) -> Option<u64> {
        let n = self.posts.len() as u64;
        self.intake.as_ref().filter(|i| n < i.posts).map(|_| n)
    }

    /// Take the next rows page: the same commitment and run, the position
    /// asked, exactly [`LOG_PAGE_ROWS`] rows (fewer only at the end), each
    /// checked as it is taken.
    pub fn take_rows(&mut self, page: &Json) -> Result<(), LogError> {
        let malformed = |what| move |_: WireError| LogError::Malformed(what);
        let o = obj(page, &ROWS_PAGE_KEYS, "rows page").map_err(malformed("rows page"))?;
        let pos = self.next_rows().ok_or(LogError::Malformed("rows page"))?;
        if o["level"].as_u64() != Some(5) {
            return Err(LogError::Malformed("level"));
        }
        let at = parse_h32(&o["commitmentHash"], "commitmentHash")
            .map_err(malformed("commitmentHash"))?;
        let since = parse_h32(&o["since"], "since").map_err(malformed("since"))?;
        if at != self.latest || since != self.first {
            return Err(LogError::Commitment);
        }
        if o["from"].as_u64() != Some(pos) {
            return Err(LogError::Malformed("from"));
        }
        take_bytes(&o["rows"], &mut self.bytes, self.max_bytes)?;
        let rows: Vec<LogRow> = parse_borsh(&o["rows"], "rows").map_err(malformed("rows"))?;
        let want = LOG_PAGE_ROWS.min(self.rows - pos);
        if rows.len() as u64 != want {
            return Err(LogError::Malformed("rows"));
        }
        for r in &rows {
            self.check.row(r)?;
        }
        Ok(())
    }

    /// Take the next posts page: the same commitment, the post asked, at
    /// least one post, and more than one only within [`LOG_PAGE_ROWS`]
    /// revealed statements; the revealed rows stay within [`MAX_LOG_CHECK`].
    pub fn take_posts(&mut self, page: &Json) -> Result<(), LogError> {
        let malformed = |what| move |_: WireError| LogError::Malformed(what);
        let o = obj(page, &POSTS_PAGE_KEYS, "posts page").map_err(malformed("posts page"))?;
        let from = self.next_posts().ok_or(LogError::Malformed("posts page"))?;
        let total = self.intake.as_ref().map_or(0, |i| i.posts);
        if o["level"].as_u64() != Some(5) {
            return Err(LogError::Malformed("level"));
        }
        let at = parse_h32(&o["commitmentHash"], "commitmentHash")
            .map_err(malformed("commitmentHash"))?;
        if at != self.latest {
            return Err(LogError::Commitment);
        }
        if o["from"].as_u64() != Some(from) {
            return Err(LogError::Malformed("from"));
        }
        take_bytes(&o["posts"], &mut self.bytes, self.max_bytes)?;
        let page: Vec<(Post, RevealedBatch)> =
            parse_borsh(&o["posts"], "posts").map_err(malformed("posts"))?;
        let rows: u64 = page.iter().map(|(_, b)| b.statements.len() as u64).sum();
        if page.is_empty()
            || from + page.len() as u64 > total
            || page.len() as u64 > LOG_PAGE_ROWS
            || (page.len() > 1 && rows > LOG_PAGE_ROWS)
        {
            return Err(LogError::Malformed("posts"));
        }
        // As many posts as fit (review C21-R2-4): a server that cuts pages
        // short makes its client ask once per post.
        self.post_pages += 1;
        self.revealed_rows = self.revealed_rows.saturating_add(rows);
        let posts = self.posts.len() as u64 + page.len() as u64;
        if !posts_pages_within(self.post_pages, self.revealed_rows, posts) {
            return Err(LogError::Malformed("posts pages cut short"));
        }
        // Revealed rows and posts both count (review C21-R1-4).
        self.revealed = self
            .revealed
            .saturating_add(rows)
            .saturating_add(page.len() as u64);
        if chained_check_cost(1, self.revealed.saturating_add(self.rows)) > MAX_LOG_CHECK {
            return Err(LogError::TooLarge);
        }
        for (p, b) in page {
            self.posts.push(p);
            self.batches.push(b);
        }
        Ok(())
    }

    /// Every row and post in: every link's log checked, then the rerun as for
    /// a format-1 body (with the registry: `compute` gives the commitment; with
    /// the reviewed keys too: `verify_log` on the intake).
    pub fn finish(self, expect: &LogExpect<'_>) -> Result<CheckedLog, LogError> {
        if self.next_posts().is_some() {
            return Err(LogError::Malformed("posts"));
        }
        let links = self.check.links().len();
        let last = self.check.links()[links - 1].clone();
        let (s, r) = self.check.finish()?;
        let intake = self.intake.map(|h| PublishedIntake {
            intake: h.intake,
            head: h.head,
            posts: self.posts,
            batches: self.batches,
            books: h.books,
        });
        rerun(
            &last,
            intake.as_ref(),
            s,
            r,
            links,
            self.example,
            true,
            expect,
        )
    }
}

#[cfg(test)]
#[path = "wire_paged_tests.rs"]
mod tests;
