//! A level-3 coordinator wired to the chain book (ROADMAP C10, ADR-0028
//! amendment 7, docs/spec/proof-ladder.md R-SIG-ON-7).
//!
//! [`BookIntake::take`] is the one way a coordinator takes a signed report:
//! 1. it reads the chain's latest block and refuses to act on a block time
//!    more than [`MAX_CHAIN_AHEAD_SECS`] ahead of, or [`MAX_CHAIN_BEHIND_SECS`]
//!    behind, this host's clock (a lying or stale RPC; THREATS C10-4);
//! 2. it posts the report to `OnyxSignedBook`, whatever its own store would
//!    say: the chain is the book of record (L3-F7) and a contradiction this
//!    coordinator alone saw must reach it;
//! 3. it records the post in its [`PostLog`] (recorded, refused by the book
//!    with the book's error, or pending when the transaction did not land);
//! 4. it runs its own [`SignedSources`] as a pre-check, with `now` the latest
//!    block's timestamp (never earlier than the store's clock, which only
//!    ever held earlier block times: L3C-4, L3R-F8).
//!
//! A pending post is retried with [`BookIntake::retry_pending`]. The post log
//! is what the API serves level 3 from ([`PostLog::to_json`], read by
//! the API's `/v1/signed` route): a fact is served only once every report the
//! coordinator took for it is on chain at a finalized block, so a post the
//! sequencer holds back extends that fact's window until it lands (THREATS
//! L3C-2, C10-3).

use crate::signed::{signed_fact_key, Intake, SignedError, SignedSources, MAX_CLOCK_SKEW_SECS};
use crate::signed_chain::{PostOutcome, SignedBookCast};
use onyx_core::{keccak256, Claim, Hash32, Registry, SignedReport};
use serde_json::{json, Value};

/// OP Stack `max_sequencer_drift` since Fjord: how far a block's timestamp
/// may run ahead of its L1 origin's (THREATS L3C-12).
pub const MAX_SEQUENCER_DRIFT_SECS: u64 = 1_800;

/// The most the chain's latest block time may lie ahead of this host's
/// clock before a coordinator refuses to take reports on it: the sequencer
/// drift plus the store's clock skew. Beyond it the RPC is lying (a far
/// future time would move the store's monotonic clock for good, L3C-4).
pub const MAX_CHAIN_AHEAD_SECS: u64 = MAX_SEQUENCER_DRIFT_SECS + MAX_CLOCK_SKEW_SECS;

/// The most the chain's latest block time may lag this host's clock: Base
/// makes a block every 2 s, so a latest block five minutes old is a stale
/// RPC or a stalled sequencer, and a post sent there may not land.
pub const MAX_CHAIN_BEHIND_SECS: u64 = 300;

/// The largest Borsh payload a coordinator posts (review C10-R2-1). A valid
/// payload is under 1,600 bytes (dotted ids of at most 128 bytes, a value of
/// at most `MAX_VALUE_BYTES`); a contradiction of a valid fact shares its
/// source, metric and subject, so it is under this bound too. A larger one
/// would pass the book's gas estimate and then fail to send (a node's
/// transaction size limit), stay pending for good and trip the API's
/// delayed-post breaker for every fact: it is refused before any post.
pub const MAX_POST_PAYLOAD_BYTES: usize = 4_096;

/// Version of the post log file.
pub const POST_LOG_VERSION: u64 = 1;

/// The book errors a refused `postReport` can carry (the contract's custom
/// errors on that path); anything else is a transport failure, so the post
/// stays pending and is retried.
pub const BOOK_ERRORS: [&str; 13] = [
    "BadPayload",
    "SequenceReserved",
    "UnknownKey",
    "KeySourceMismatch",
    "KeyRetiredError",
    "KeyCompromisedError",
    "KeySuspended",
    "BadSignature",
    "FromFuture",
    "Stale",
    "Replay",
    "AlreadyStated",
    "NotActive",
];

/// Book refusals that depend on time or on the key's state, so the same
/// report may land later: [`BookIntake::retry_pending`] posts them again
/// (review C10-R1-1). `FromFuture` is checked after the signature, so only a
/// key holder's report gets it.
pub const RETRYABLE_ERRORS: [&str; 3] = ["FromFuture", "UnknownKey", "KeySuspended"];

/// Whether a book refusal may clear later ([`RETRYABLE_ERRORS`]).
pub fn retryable(error: &str) -> bool {
    RETRYABLE_ERRORS.contains(&error)
}

/// The 4-byte selector of a custom error `name()` (no arguments).
pub fn error_selector(name: &str) -> [u8; 4] {
    let h = keccak256(format!("{name}()").as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

/// The book error a revert message names (its selector as `0x` hex, as
/// `cast` and JSON-RPC nodes print revert data), if any.
pub fn book_error_in(message: &str) -> Option<&'static str> {
    let m = message.to_ascii_lowercase();
    BOOK_ERRORS
        .iter()
        .find(|n| m.contains(&format!("0x{}", hex::encode(error_selector(n)))))
        .copied()
}

/// Where a post stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostStatus {
    /// Sent; its transaction has not been seen in a block (yet). `since` is
    /// this host's clock when the coordinator took the report.
    Pending {
        /// Unix seconds, this host's clock.
        since: u64,
        /// The last transport error.
        error: String,
    },
    /// The book recorded the payload: accepted, an equivocation or evidence
    /// in a dispute. `block_time` is the block's timestamp (0 when found
    /// already recorded by a retry).
    Recorded {
        /// What the book did.
        outcome: Option<PostOutcome>,
        /// The block it landed in (0: found recorded by a retry).
        block: u64,
        /// That block's timestamp (0: found recorded by a retry).
        block_time: u64,
    },
    /// The book refused it (reverted with one of [`BOOK_ERRORS`]); nothing
    /// was written on chain.
    Refused {
        /// The book's error name.
        error: String,
    },
}

/// One report the coordinator took, and what became of its post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostEntry {
    /// The report, as received.
    pub report: SignedReport,
    /// This host's clock when it was first taken.
    pub received_unix: u64,
    /// The chain's latest block time when it was first taken.
    pub received_block_time: u64,
    /// The post.
    pub status: PostStatus,
}

impl PostEntry {
    /// The payload digest (the book's key for the report).
    pub fn digest(&self) -> Hash32 {
        self.report.payload.digest()
    }

    /// The fact the report states.
    pub fn fact(&self) -> Hash32 {
        signed_fact_key(&self.report.payload)
    }

    /// True while the post has not landed and was not refused.
    pub fn pending(&self) -> bool {
        matches!(self.status, PostStatus::Pending { .. })
    }
}

/// What a landed post returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Landed {
    /// What the book did.
    pub outcome: PostOutcome,
    /// The block it landed in.
    pub block: u64,
    /// That block's timestamp.
    pub block_time: u64,
}

/// Why a post did not land.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostError {
    /// The book refused it (one of [`BOOK_ERRORS`]): final, nothing written.
    Refused(String),
    /// Transport: no answer, a timeout, a dropped transaction. Retry.
    Unavailable(String),
}

/// The chain side of a level-3 coordinator.
pub trait SignedBookChain {
    /// The latest block: (number, timestamp).
    fn latest(&self) -> Result<(u64, u64), String>;
    /// Post `report` with `postReport`.
    fn post(&self, report: &SignedReport) -> Result<Landed, PostError>;
    /// Whether the book recorded the payload with this digest (any
    /// signature), at the latest block.
    fn recorded(&self, digest: &Hash32) -> Result<bool, String>;
}

/// [`SignedBookCast`] posting from one unlocked account (local anvil and
/// fork rehearsals; no key is read or passed).
#[derive(Clone, Debug)]
pub struct CastPoster {
    /// The book.
    pub book: SignedBookCast,
    /// The coordinator's account.
    pub from: [u8; 20],
}

impl SignedBookChain for CastPoster {
    fn latest(&self) -> Result<(u64, u64), String> {
        let n = self.book.block_number("latest")?;
        let t = self.book.block_time(&n.to_string())?;
        Ok((n, t))
    }

    fn post(&self, report: &SignedReport) -> Result<Landed, PostError> {
        match self.book.post_report_landed(&self.from, report) {
            Ok((outcome, block)) => {
                let block_time = self
                    .book
                    .block_time(&block.to_string())
                    .map_err(PostError::Unavailable)?;
                Ok(Landed {
                    outcome,
                    block,
                    block_time,
                })
            }
            Err(e) => match book_error_in(&e) {
                Some(name) => Err(PostError::Refused(name.to_string())),
                None => Err(PostError::Unavailable(e)),
            },
        }
    }

    fn recorded(&self, digest: &Hash32) -> Result<bool, String> {
        self.book.recorded(digest, "latest")
    }
}

/// Why [`BookIntake::take`] took nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum TakeError {
    /// The payload is larger than any valid one (review C10-R2-1).
    #[error("the report's payload is {bytes} bytes (most {MAX_POST_PAYLOAD_BYTES}): no valid report is that large; nothing posted or logged")]
    Oversized {
        /// Its Borsh length.
        bytes: usize,
    },
    /// The latest block could not be read.
    #[error("chain read failed: {0}")]
    Chain(String),
    /// The chain's latest block time is too far ahead of this host's clock.
    #[error("the chain's latest block time {chain} is {ahead} s ahead of this host's clock {host} (most {MAX_CHAIN_AHEAD_SECS} s): a lying RPC or a broken clock; nothing taken")]
    ChainAhead {
        /// Latest block time.
        chain: u64,
        /// Host clock.
        host: u64,
        /// The gap.
        ahead: u64,
    },
    /// The chain's latest block is too old.
    #[error("the chain's latest block time {chain} is {behind} s behind this host's clock {host} (most {MAX_CHAIN_BEHIND_SECS} s): a stale RPC or a stalled sequencer; nothing taken")]
    ChainBehind {
        /// Latest block time.
        chain: u64,
        /// Host clock.
        host: u64,
        /// The gap.
        behind: u64,
    },
}

/// Refuse a latest block time `chain` too far from this host's clock `host`.
pub fn check_chain_clock(host: u64, chain: u64) -> Result<(), TakeError> {
    if chain > host.saturating_add(MAX_CHAIN_AHEAD_SECS) {
        return Err(TakeError::ChainAhead {
            chain,
            host,
            ahead: chain - host,
        });
    }
    if host > chain.saturating_add(MAX_CHAIN_BEHIND_SECS) {
        return Err(TakeError::ChainBehind {
            chain,
            host,
            behind: host - chain,
        });
    }
    Ok(())
}

/// What [`BookIntake::take`] did with a report.
#[derive(Debug, PartialEq, Eq)]
pub struct Taken {
    /// The post: recorded, refused or pending (retry with
    /// [`BookIntake::retry_pending`]). The chain decides.
    pub chain: PostStatus,
    /// The coordinator's own store (a pre-check, never what anyone settles
    /// on): its verdict at `now`.
    pub local: Result<Intake, SignedError>,
    /// The `now` the store ran at: the latest block's timestamp, or the
    /// store's clock if that was later (a lagging RPC).
    pub now: u64,
}

/// Every report a coordinator took, with its post (the file the API serves
/// level 3 from).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PostLog {
    /// Chain id of the book.
    pub chain_id: u64,
    /// The book.
    pub book: [u8; 20],
    /// One entry per payload digest, in the order taken.
    pub entries: Vec<PostEntry>,
}

fn h0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn unhex(v: &Value, what: &str) -> Result<Vec<u8>, String> {
    let s = v
        .as_str()
        .ok_or_else(|| format!("post log: {what} must be a hex string"))?;
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|e| format!("post log: {what}: {e}"))
}

fn outcome_text(o: Option<PostOutcome>) -> &'static str {
    match o {
        Some(PostOutcome::Accepted) => "accepted",
        Some(PostOutcome::Equivocation) => "equivocation",
        Some(PostOutcome::FactDisputed) => "factDisputed",
        None => "recorded",
    }
}

impl PostLog {
    /// An empty log for the book at `book` on `chain_id`.
    pub fn new(chain_id: u64, book: [u8; 20]) -> Self {
        PostLog {
            chain_id,
            book,
            entries: Vec::new(),
        }
    }

    /// The entry for a payload digest.
    pub fn entry(&self, digest: &Hash32) -> Option<&PostEntry> {
        self.entries.iter().find(|e| e.digest() == *digest)
    }

    /// The entries stating `fact`.
    pub fn for_fact(&self, fact: &Hash32) -> Vec<&PostEntry> {
        self.entries.iter().filter(|e| e.fact() == *fact).collect()
    }

    /// The oldest pending post's host time, if any.
    pub fn oldest_pending(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter_map(|e| match e.status {
                PostStatus::Pending { since, .. } => Some(since),
                _ => None,
            })
            .min()
    }

    /// The file form. Derived values (digest, fact, hashes) are not written:
    /// a reader recomputes them from the payload and signature.
    pub fn to_json(&self) -> Value {
        let entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| {
                let chain = match &e.status {
                    PostStatus::Pending { since, error } => {
                        json!({"status": "pending", "since": since, "error": error})
                    }
                    PostStatus::Recorded {
                        outcome,
                        block,
                        block_time,
                    } => json!({
                        "status": "recorded",
                        "outcome": outcome_text(*outcome),
                        "block": block,
                        "blockTime": block_time,
                    }),
                    PostStatus::Refused { error } => json!({"status": "refused", "error": error}),
                };
                json!({
                    "payload": h0x(&borsh::to_vec(&e.report.payload).unwrap_or_default()),
                    "signature": h0x(&e.report.signature),
                    "receivedUnix": e.received_unix,
                    "receivedBlockTime": e.received_block_time,
                    "chain": chain,
                })
            })
            .collect();
        json!({
            "version": POST_LOG_VERSION,
            "chainId": self.chain_id,
            "book": h0x(&self.book),
            "reports": entries,
        })
    }

    /// Parse [`Self::to_json`]'s form. Refuses a duplicate payload, an
    /// unknown status or version, and a payload that does not decode.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        if v["version"].as_u64() != Some(POST_LOG_VERSION) {
            return Err(format!("post log: version must be {POST_LOG_VERSION}"));
        }
        let chain_id = v["chainId"]
            .as_u64()
            .ok_or("post log: chainId must be a number")?;
        let book: [u8; 20] = unhex(&v["book"], "book")?
            .try_into()
            .map_err(|_| "post log: book must be 20 bytes")?;
        let mut log = PostLog::new(chain_id, book);
        for r in v["reports"]
            .as_array()
            .ok_or("post log: reports must be a list")?
        {
            let payload = borsh::from_slice(&unhex(&r["payload"], "payload")?)
                .map_err(|e| format!("post log: payload does not decode: {e}"))?;
            let signature: [u8; 65] = unhex(&r["signature"], "signature")?
                .try_into()
                .map_err(|_| "post log: signature must be 65 bytes")?;
            let num = |k: &str| {
                r[k].as_u64()
                    .ok_or_else(|| format!("post log: {k} must be a number"))
            };
            let c = &r["chain"];
            let status = match c["status"].as_str() {
                Some("pending") => PostStatus::Pending {
                    since: c["since"].as_u64().ok_or("post log: pending since")?,
                    error: c["error"].as_str().unwrap_or_default().to_string(),
                },
                Some("recorded") => PostStatus::Recorded {
                    outcome: match c["outcome"].as_str() {
                        Some("accepted") => Some(PostOutcome::Accepted),
                        Some("equivocation") => Some(PostOutcome::Equivocation),
                        Some("factDisputed") => Some(PostOutcome::FactDisputed),
                        Some("recorded") => None,
                        _ => return Err("post log: unknown outcome".into()),
                    },
                    block: c["block"].as_u64().ok_or("post log: block")?,
                    block_time: c["blockTime"].as_u64().ok_or("post log: blockTime")?,
                },
                Some("refused") => PostStatus::Refused {
                    error: c["error"]
                        .as_str()
                        .ok_or("post log: refused error")?
                        .to_string(),
                },
                _ => return Err("post log: unknown chain status".into()),
            };
            let e = PostEntry {
                report: SignedReport { payload, signature },
                received_unix: num("receivedUnix")?,
                received_block_time: num("receivedBlockTime")?,
                status,
            };
            if log.entry(&e.digest()).is_some() {
                return Err("post log: a payload appears twice".into());
            }
            log.entries.push(e);
        }
        Ok(log)
    }

    /// Write the file atomically (a temporary file in the same directory,
    /// then a rename), so a reader never sees half a log.
    pub fn write(&self, path: &std::path::Path) -> Result<(), String> {
        let tmp = path.with_extension("tmp");
        let body = serde_json::to_string_pretty(&self.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, body + "\n").map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// A level-3 coordinator: its own store as a pre-check, the chain book as
/// the book of record, and the log of its posts.
pub struct BookIntake<C: SignedBookChain> {
    /// The chain.
    pub chain: C,
    /// The pre-check store.
    pub store: SignedSources,
    /// Every report taken and its post.
    pub log: PostLog,
}

impl<C: SignedBookChain> BookIntake<C> {
    /// A coordinator posting to `chain`, logging under (`chain_id`, `book`).
    pub fn new(chain: C, store: SignedSources, chain_id: u64, book: [u8; 20]) -> Self {
        BookIntake {
            chain,
            store,
            log: PostLog::new(chain_id, book),
        }
    }

    /// The `now` for the store: the chain's latest block time, checked
    /// against this host's clock `host`, never before the store's clock.
    fn chain_now(&self, host: u64) -> Result<u64, TakeError> {
        let (_, t) = self.chain.latest().map_err(TakeError::Chain)?;
        check_chain_clock(host, t)?;
        Ok(t.max(self.store.clock()))
    }

    fn post(&self, report: &SignedReport, host: u64) -> PostStatus {
        match self.chain.post(report) {
            Ok(l) => PostStatus::Recorded {
                outcome: Some(l.outcome),
                block: l.block,
                block_time: l.block_time,
            },
            // The payload is already in the book (posted by anyone, or by an
            // earlier try whose answer was lost): it landed.
            Err(PostError::Refused(e)) if e == "Replay" => {
                match self.chain.recorded(&report.payload.digest()) {
                    Ok(true) => PostStatus::Recorded {
                        outcome: None,
                        block: 0,
                        block_time: 0,
                    },
                    Ok(false) => PostStatus::Refused { error: e },
                    Err(err) => PostStatus::Pending {
                        since: host,
                        error: err,
                    },
                }
            }
            Err(PostError::Refused(error)) => PostStatus::Refused { error },
            Err(PostError::Unavailable(error)) => PostStatus::Pending { since: host, error },
        }
    }

    /// Take a report (ROADMAP C10): check the chain clock, post it to the
    /// book whatever this store says, log the post, then run the store's
    /// pre-check at the latest block's time. `host` is this host's clock
    /// (unix seconds). Nothing changes when the chain clock is refused or
    /// the payload is larger than [`MAX_POST_PAYLOAD_BYTES`].
    pub fn take(
        &mut self,
        claim: &Claim,
        report: &SignedReport,
        reg: &dyn Registry,
        host: u64,
    ) -> Result<Taken, TakeError> {
        let bytes = borsh::to_vec(&report.payload).map_or(usize::MAX, |b| b.len());
        if bytes > MAX_POST_PAYLOAD_BYTES {
            return Err(TakeError::Oversized { bytes });
        }
        let now = self.chain_now(host)?;
        let digest = report.payload.digest();
        let chain = match self.log.entry(&digest).map(|e| &e.status) {
            // Landed before: the book has it. A refusal is posted again: most
            // depend on the time or the key's state (FromFuture, a suspension).
            Some(s @ PostStatus::Recorded { .. }) => s.clone(),
            _ => self.post(report, host),
        };
        match self.log.entries.iter_mut().find(|e| e.digest() == digest) {
            Some(e) => {
                // Keep the first pending time: the delay counts from the
                // first take.
                e.status = match (&e.status, chain.clone()) {
                    (PostStatus::Pending { since, .. }, PostStatus::Pending { error, .. }) => {
                        PostStatus::Pending {
                            since: *since,
                            error,
                        }
                    }
                    (_, s) => s,
                };
            }
            None => self.log.entries.push(PostEntry {
                report: report.clone(),
                received_unix: host,
                received_block_time: now,
                status: chain.clone(),
            }),
        }
        let local = self.store.accept_claim(claim, report, reg, now);
        Ok(Taken { chain, local, now })
    }

    /// Post every pending report again (or find it recorded), and every
    /// report the book refused for a reason that may clear
    /// ([`RETRYABLE_ERRORS`]). Returns how many are still pending.
    pub fn retry_pending(&mut self, host: u64) -> Result<usize, TakeError> {
        self.chain_now(host)?;
        let pending: Vec<(usize, SignedReport, u64)> = self
            .log
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match &e.status {
                PostStatus::Pending { since, .. } => Some((i, e.report.clone(), *since)),
                PostStatus::Refused { error } if retryable(error) => {
                    Some((i, e.report.clone(), host))
                }
                _ => None,
            })
            .collect();
        let mut left = 0;
        for (i, report, since) in pending {
            let s = match self.chain.recorded(&report.payload.digest()) {
                Ok(true) => PostStatus::Recorded {
                    outcome: None,
                    block: 0,
                    block_time: 0,
                },
                Ok(false) => self.post(&report, host),
                Err(error) => PostStatus::Pending { since, error },
            };
            let s = match s {
                PostStatus::Pending { error, .. } => {
                    left += 1;
                    PostStatus::Pending { since, error }
                }
                s => s,
            };
            self.log.entries[i].status = s;
        }
        Ok(left)
    }
}

#[cfg(test)]
#[path = "signed_intake_tests.rs"]
mod tests;
