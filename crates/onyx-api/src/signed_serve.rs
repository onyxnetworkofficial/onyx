//! Level-3 signed-source facts, served only against the chain book (ROADMAP
//! C10, ADR-0028 amendment 7, docs/spec/proof-ladder.md R-SIG-ON-7,
//! docs/spec/delivery.md "Signed facts").
//!
//! `GET /v1/signed/{fact}` serves a signed report only when `OnyxSignedBook`
//! lets a consumer settle that fact on exactly that report, read at the
//! node's FINALIZED block (by its hash, so every read is one state):
//! 1. no report the operator's coordinator took for the fact is still
//!    pending, and every one it saw land is in the book at that block (a
//!    post the sequencer holds back extends the fact's window until it lands
//!    and is final: THREATS L3C-2, C10-3);
//! 2. `settleable(fact)` is true and names this report's hash and value hash;
//! 3. the fact was accepted at least [`SERVABLE_AFTER_SECS`] of block time
//!    before that block (the 1 h dispute window plus the sequencer's drift,
//!    THREATS L3C-12), and as long on this host's clock since the operator
//!    took the report (block time can run fast after a stall, and a node can
//!    lie about it: review C10-R1-2);
//! 4. no post of the coordinator has been pending more than
//!    [`POST_DELAY_ALERT_SECS`]: a sequencer that holds our posts back may
//!    hold others' contradictions too, so level 3 stops altogether; and no
//!    time in the post log is more than [`HOST_SKEW_SECS`] ahead of this
//!    host's clock (a clock behind the coordinator's trips that breaker
//!    late: review C10-R2-2).
//!
//! Anything else is a refusal with a stable code and a `reason`, never a
//! value. The book itself is pinned at start ([`check_book`]: code hash,
//! reviewer, chain id).
//!
//! Not money: this route is free; no market settles on it (`/v1/settlement`
//! answers for listed shards only, and this file names no money, level or
//! market code: the signed-money fence in scripts/check-boundaries.sh).
//! Level-3 money settlement waits for ROADMAP H6.

use crate::cache::Caches;
use crate::roots::JsonRpc;
use crate::{bad_request, rate_scopes, ApiError, ClientKey};
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use onyx_coordinator::signed::{DISPUTE_WINDOW_SECS, MAX_CLOCK_SKEW_SECS};
use onyx_coordinator::signed_chain::value_hash;
use onyx_coordinator::signed_intake::{PostEntry, PostLog, PostStatus, MAX_SEQUENCER_DRIFT_SECS};
use onyx_core::{keccak256, Hash32, Subject, Value as OnyxValue};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Extra block time a fact must stand past the dispute window before it is
/// served: the OP Stack sequencer drift (THREATS L3C-12).
pub const SERVE_MARGIN_SECS: u64 = MAX_SEQUENCER_DRIFT_SECS;
/// Block time from acceptance to the first serve: 5,400 s.
pub const SERVABLE_AFTER_SECS: u64 = DISPUTE_WINDOW_SECS + SERVE_MARGIN_SECS;
/// A post pending longer than this (host clock) stops level-3 serving.
pub const POST_DELAY_ALERT_SECS: u64 = 600;
/// The most the coordinator's clock (the post log's times) may run ahead of
/// this host's before level 3 stops (review C10-R2-2).
pub const HOST_SKEW_SECS: u64 = MAX_CLOCK_SKEW_SECS;
/// Answers per client per minute (each costs four or more node reads).
pub const FREE_PER_MINUTE: u32 = 30;

/// Why a fact is not served (the `reason` of a 409 `level3_not_servable`).
pub mod reason {
    /// The book has no value for the fact.
    pub const UNKNOWN: &str = "unknown";
    /// The book holds a contradiction for the fact.
    pub const DISPUTED: &str = "disputed";
    /// Accepted, but inside the dispute window at the finalized block.
    pub const IN_WINDOW: &str = "in_dispute_window";
    /// Past the window, not yet past the serving margin.
    pub const IN_MARGIN: &str = "in_serving_margin";
    /// The book settles the fact on a report this server does not hold.
    pub const OTHER_REPORT: &str = "other_report";
    /// A report the coordinator took for the fact has not landed.
    pub const POST_PENDING: &str = "post_pending";
    /// A report the coordinator saw land is not in the finalized state yet.
    pub const POST_NOT_FINAL: &str = "post_not_final";
    /// A contradiction the book refused for now (`FromFuture`) is retried.
    pub const POST_RETRYING: &str = "post_retrying";
    /// Past the block-time margin, not yet past it on this host's clock.
    pub const IN_HOST_MARGIN: &str = "in_host_margin";
}

/// The book a node serves level 3 against (manifest `signedBook`,
/// `signedBookCodeHash`, `signedBookReviewer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookPin {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// `OnyxSignedBook`.
    pub book: [u8; 20],
    /// keccak-256 of its runtime code.
    pub code_hash: Hash32,
    /// Its immutable `reviewer()`.
    pub reviewer: [u8; 20],
}

fn hx(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn hex_of(v: &Value, what: &str) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not a string"))?;
    hex::decode(
        s.strip_prefix("0x")
            .ok_or_else(|| format!("{what}: no 0x"))?,
    )
    .map_err(|e| format!("{what}: {e}"))
}

fn qty(v: &Value, what: &str) -> Result<u64, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not a string"))?;
    u64::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16).map_err(|e| format!("{what}: {e}"))
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes());
    [h.0[0], h.0[1], h.0[2], h.0[3]]
}

fn calldata(sig: &str, word: &Hash32) -> String {
    let mut d = selector(sig).to_vec();
    d.extend_from_slice(&word.0);
    hx(&d)
}

fn word(b: &[u8], i: usize) -> Option<[u8; 32]> {
    b.get(i * 32..i * 32 + 32)?.try_into().ok()
}

fn word_u64(w: &[u8; 32]) -> Option<u64> {
    if w[..24].iter().any(|x| *x != 0) {
        return None;
    }
    Some(u64::from_be_bytes(w[24..].try_into().ok()?))
}

/// Refuse to start unless the node's chain is the pin's, the code at the
/// book's address is the pinned build and its `reviewer()` is the pinned
/// reviewer (ROADMAP C10, like the registry pin L16). Errors name no URL.
pub async fn check_book(rpc: &dyn JsonRpc, pin: &BookPin) -> Result<(), String> {
    let id = rpc
        .call("eth_chainId", json!([]))
        .await
        .map_err(|e| format!("chain id read failed: {e}"))?;
    if qty(&id, "chain id")? != pin.chain_id {
        return Err(format!(
            "the node's chain is not the manifest's ({})",
            pin.chain_id
        ));
    }
    let to = hx(&pin.book);
    let code = rpc
        .call("eth_getCode", json!([to, "latest"]))
        .await
        .map_err(|e| format!("signed book code read failed: {e}"))?;
    let code = hex_of(&code, "signed book code")?;
    if code.is_empty() || keccak256(&code) != pin.code_hash {
        return Err(
            "the signed book's runtime code is not the pinned build (signedBookCodeHash)".into(),
        );
    }
    let ret = rpc
        .call(
            "eth_call",
            json!([{"to": to, "data": hx(&selector("reviewer()"))}, "latest"]),
        )
        .await
        .map_err(|e| format!("signed book reviewer() failed: {e}"))?;
    let w = word(&hex_of(&ret, "reviewer return")?, 0).ok_or("reviewer() returned no word")?;
    if w[..12].iter().any(|x| *x != 0) || w[12..] != pin.reviewer {
        return Err("the signed book's reviewer() is not the manifest's signedBookReviewer".into());
    }
    Ok(())
}

/// The `OnyxSignedBook` a forge `DeploySignedBook` broadcast created
/// (`contracts/broadcast/DeploySignedBook.s.sol/<chain>/run-latest.json`).
pub fn book_from_broadcast(v: &Value) -> Result<[u8; 20], String> {
    let txs = v["transactions"]
        .as_array()
        .ok_or("broadcast: no transactions")?;
    let mut found = txs
        .iter()
        .filter(|t| t["contractName"] == "OnyxSignedBook" && t["transactionType"] == "CREATE");
    let tx = found
        .next()
        .ok_or("broadcast: OnyxSignedBook not deployed")?;
    if found.next().is_some() {
        return Err("broadcast: more than one OnyxSignedBook".into());
    }
    let a = hex_of(&tx["contractAddress"], "broadcast contractAddress")?;
    a.try_into()
        .map_err(|_| "broadcast: contractAddress is not 20 bytes".to_string())
}

/// The code at the book's address must be this checkout's `OnyxSignedBook`
/// build (forge's `deployedBytecode`, with only its immutable `reviewer`
/// filled in) before a manifest pins its hash (as the registry pin, L16-1):
/// the hash is read from the chain, so without this a proxy or other code
/// at the broadcast's address would be pinned as it is.
pub fn code_matches_build(code: &[u8], artifact: &Value) -> Result<(), String> {
    let db = &artifact["deployedBytecode"];
    let want = hex_of(&db["object"], "artifact deployedBytecode")?;
    if want.is_empty() || code.len() != want.len() {
        return Err(format!(
            "the book's runtime code is {} bytes, the OnyxSignedBook build {}",
            code.len(),
            want.len()
        ));
    }
    let refs = db["immutableReferences"]
        .as_object()
        .ok_or("artifact: no immutableReferences")?;
    let mut masked = code.to_vec();
    for r in refs
        .values()
        .flat_map(|v| v.as_array().into_iter().flatten())
    {
        let (Some(start), Some(len)) = (r["start"].as_u64(), r["length"].as_u64()) else {
            return Err("artifact: a bad immutable reference".into());
        };
        let (start, end) = (start as usize, start.saturating_add(len) as usize);
        if end > want.len() || start > end {
            return Err("artifact: an immutable reference out of range".into());
        }
        masked[start..end].copy_from_slice(&want[start..end]);
    }
    if masked != want {
        return Err("the book's runtime code is not this checkout's OnyxSignedBook build".into());
    }
    Ok(())
}

/// The finalized block every read of one answer is made at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedBlock {
    /// Number.
    pub number: u64,
    /// Hash.
    pub hash: Hash32,
    /// Timestamp.
    pub time: u64,
}

fn unavailable(message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "level3_chain_unavailable",
        message,
    )
    .with_header("retry-after", "30".into())
}

fn not_servable(reason: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "level3_not_servable", message)
        .with_field("reason", Value::String(reason.into()))
}

/// The node's finalized block. A node without one (no `finalized` tag) is
/// refused: level 3 is never served from `latest` (THREATS L3C-3, C10-5).
pub async fn finalized_block(rpc: &dyn JsonRpc) -> Result<FinalizedBlock, ApiError> {
    let b = rpc
        .call("eth_getBlockByNumber", json!(["finalized", false]))
        .await
        .map_err(|_| unavailable("the node did not answer for its finalized block"))?;
    if b.is_null() {
        return Err(unavailable(
            "the node has no finalized block: level 3 is served only at a finalized block",
        ));
    }
    let number = qty(&b["number"], "number").map_err(|_| unavailable("bad finalized block"))?;
    let time = qty(&b["timestamp"], "timestamp").map_err(|_| unavailable("bad finalized block"))?;
    let hash: [u8; 32] = hex_of(&b["hash"], "hash")
        .ok()
        .and_then(|h| h.try_into().ok())
        .ok_or_else(|| unavailable("bad finalized block"))?;
    Ok(FinalizedBlock {
        number,
        hash: Hash32(hash),
        time,
    })
}

async fn call_at(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    data: String,
    at: &FinalizedBlock,
) -> Result<Vec<u8>, ApiError> {
    let ret = rpc
        .call(
            "eth_call",
            json!([{"to": hx(book), "data": data}, {"blockHash": at.hash.to_string()}]),
        )
        .await
        .map_err(|_| unavailable("the node did not answer a read of the signed book"))?;
    hex_of(&ret, "signed book return").map_err(|_| unavailable("bad answer from the node"))
}

/// The book's view of a fact at `at`: `settleable` and `factView`'s head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BookFact {
    /// `settleable(fact)`: (report hash, value hash) when settleable.
    pub settleable: Option<(Hash32, Hash32)>,
    /// `factView(fact).state`: 0 none, 1 accepted, 2 disputed.
    pub state: u8,
    /// `factView(fact).acceptedAt` (0 unless accepted).
    pub accepted_at: u64,
}

/// Read `fact` from the book at `at`.
pub async fn book_fact(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    fact: &Hash32,
    at: &FinalizedBlock,
) -> Result<BookFact, ApiError> {
    let bad = || unavailable("the signed book's answer does not decode");
    let s = call_at(rpc, book, calldata("settleable(bytes32)", fact), at).await?;
    if s.is_empty() {
        // No code at that block: the book was deployed after it (its code
        // at `latest` was pinned at start), so it held nothing then.
        return Ok(BookFact {
            settleable: None,
            state: 0,
            accepted_at: 0,
        });
    }
    let (ok, r, v) = (
        word(&s, 0).ok_or_else(bad)?,
        word(&s, 1).ok_or_else(bad)?,
        word(&s, 2).ok_or_else(bad)?,
    );
    let settleable = match word_u64(&ok).ok_or_else(bad)? {
        0 => None,
        1 => Some((Hash32(r), Hash32(v))),
        _ => return Err(bad()),
    };
    let f = call_at(rpc, book, calldata("factView(bytes32)", fact), at).await?;
    let state = word_u64(&word(&f, 0).ok_or_else(bad)?).ok_or_else(bad)?;
    let accepted_at = word_u64(&word(&f, 2).ok_or_else(bad)?).ok_or_else(bad)?;
    if state > 2 {
        return Err(bad());
    }
    Ok(BookFact {
        settleable,
        state: state as u8,
        accepted_at,
    })
}

/// Whether the book recorded the payload with this digest at `at`.
pub async fn recorded_at(
    rpc: &dyn JsonRpc,
    book: &[u8; 20],
    digest: &Hash32,
    at: &FinalizedBlock,
) -> Result<bool, ApiError> {
    let r = call_at(rpc, book, calldata("report(bytes32)", digest), at).await?;
    if r.is_empty() {
        // The book was deployed after that block: nothing recorded then.
        return Ok(false);
    }
    let key = word(&r, 0).ok_or_else(|| unavailable("the signed book's answer does not decode"))?;
    Ok(key != [0u8; 32])
}

/// The served form of a value (the source-signed JSON form; numbers as
/// strings).
pub fn value_json(v: &OnyxValue) -> Value {
    match v {
        OnyxValue::Int(i) => json!({"int": i.to_string()}),
        OnyxValue::UInt(u) => json!({"uint": u.to_string()}),
        OnyxValue::Fixed { mantissa, scale } => {
            json!({"fixed": {"mantissa": mantissa.to_string(), "scale": scale}})
        }
        OnyxValue::Bool(b) => json!({"bool": b}),
        OnyxValue::Bytes(b) => json!({"bytes": hx(b)}),
    }
}

fn subject_text(s: &Subject) -> &str {
    match s {
        Subject::Entity(x)
        | Subject::Address(x)
        | Subject::Instrument(x)
        | Subject::Aggregate(x) => x,
    }
}

/// The post log as last read, by file identity.
struct Loaded {
    stamp: Option<(std::time::SystemTime, u64)>,
    log: Result<Arc<PostLog>, String>,
}

/// The level-3 facts one node serves: the pinned book and the post log its
/// coordinator writes (`ONYX_SIGNED_REPORTS`), re-read when the file changes.
pub struct SignedStore {
    pin: BookPin,
    path: Option<PathBuf>,
    rpc: Arc<dyn JsonRpc>,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    free_per_minute: u32,
    limits: Caches,
    loaded: RwLock<Loaded>,
}

fn check_log(pin: &BookPin, log: PostLog) -> Result<Arc<PostLog>, String> {
    if log.chain_id != pin.chain_id || log.book != pin.book {
        return Err("the signed report log names another chain or book than the manifest's".into());
    }
    Ok(Arc::new(log))
}

impl SignedStore {
    /// A store over the post log file at `path` (`None`: nothing served),
    /// reading the chain through `rpc`, with `now` the host clock.
    pub fn new(
        pin: BookPin,
        path: Option<PathBuf>,
        rpc: Arc<dyn JsonRpc>,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        SignedStore {
            pin,
            path,
            rpc,
            now,
            free_per_minute: FREE_PER_MINUTE,
            limits: Caches::new(1, 1),
            loaded: RwLock::new(Loaded {
                stamp: None,
                log: Ok(Arc::new(PostLog::new(pin.chain_id, pin.book))),
            }),
        }
    }

    /// A store over a log held in memory (tests and rehearsals).
    pub fn with_log(self, log: PostLog) -> Self {
        let checked = check_log(&self.pin, log);
        if let Ok(mut l) = self.loaded.write() {
            l.log = checked;
            l.stamp = Some((std::time::SystemTime::UNIX_EPOCH, 0));
        }
        self
    }

    /// Answers per client per minute (tests).
    pub fn with_free_per_minute(mut self, n: u32) -> Self {
        self.free_per_minute = n.max(1);
        self
    }

    /// The post log, re-read when the file's modification time or length
    /// changed. A file that does not parse, or names another chain or book,
    /// refuses every answer until it is fixed (fail closed).
    pub fn log(&self) -> Result<Arc<PostLog>, String> {
        let Some(path) = &self.path else {
            return self
                .loaded
                .read()
                .map_err(|_| "lock".to_string())?
                .log
                .clone();
        };
        let meta = std::fs::metadata(path).map_err(|e| format!("signed report log: {e}"))?;
        let stamp = (
            meta.modified()
                .map_err(|e| format!("signed report log: {e}"))?,
            meta.len(),
        );
        if let Ok(l) = self.loaded.read() {
            if l.stamp == Some(stamp) {
                return l.log.clone();
            }
        }
        let log = std::fs::read(path)
            .map_err(|e| format!("signed report log: {e}"))
            .and_then(|b| {
                serde_json::from_slice::<Value>(&b).map_err(|e| format!("signed report log: {e}"))
            })
            .and_then(|v| PostLog::from_json(&v))
            .and_then(|l| check_log(&self.pin, l));
        let mut l = self.loaded.write().map_err(|_| "lock".to_string())?;
        l.stamp = Some(stamp);
        l.log = log;
        l.log.clone()
    }

    /// The answer for `fact`, or why there is none (ROADMAP C10 rules 1-4).
    pub async fn answer(&self, fact: &Hash32) -> Result<Value, ApiError> {
        let log = self.log().map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "level3_store_unreadable",
                format!("level 3 is not served until the operator fixes its report log ({e})"),
            )
        })?;
        let now = (self.now)();
        if let Some(stop) = stopped(&log, now) {
            return Err(stop);
        }
        let entries: Vec<&PostEntry> = log.for_fact(fact);
        if entries.is_empty() {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "no_signed_report",
                "this server holds no signed report for this fact",
            ));
        }
        // Rule 1, before any read: a pending post of this fact.
        if entries.iter().any(|e| e.pending()) {
            return Err(not_servable(
                reason::POST_PENDING,
                "a report this operator took for the fact has not landed in the signed book yet",
            ));
        }
        let rpc = self.rpc.as_ref();
        let at = finalized_block(rpc).await?;
        // Rule 1: every post seen landing is in the finalized state.
        for e in &entries {
            if matches!(e.status, PostStatus::Recorded { .. })
                && !recorded_at(rpc, &self.pin.book, &e.digest(), &at).await?
            {
                return Err(not_servable(
                    reason::POST_NOT_FINAL,
                    "a report posted for the fact is not in the finalized state yet",
                ));
            }
        }
        // Rule 2: the book's value, at that block.
        let f = book_fact(rpc, &self.pin.book, fact, &at).await?;
        let Some((report_hash, value_h)) = f.settleable else {
            return Err(match f.state {
                2 => not_servable(
                    reason::DISPUTED,
                    "the signed book holds a contradiction for this fact: it has no value",
                ),
                1 => not_servable(
                    reason::IN_WINDOW,
                    "accepted, but still inside the dispute window at the finalized block",
                )
                .with_field(
                    "servableAt",
                    json!(f.accepted_at.saturating_add(SERVABLE_AFTER_SECS)),
                ),
                _ => not_servable(
                    reason::UNKNOWN,
                    "the signed book has no value for this fact at the finalized block",
                ),
            });
        };
        let Some(e) = entries
            .iter()
            .find(|e| e.report.hash() == report_hash && value_hash(&e.report) == value_h)
        else {
            return Err(not_servable(
                reason::OTHER_REPORT,
                "the signed book settles this fact on a report this server does not hold",
            ));
        };
        // Rule 1 (review C10-R1-1): a contradiction the book refused only
        // for its time is retried by the coordinator; hold the fact.
        if entries.iter().any(|x| {
            matches!(&x.status, PostStatus::Refused { error } if error == "FromFuture")
                && value_hash(&x.report) != value_h
        }) {
            return Err(not_servable(
                reason::POST_RETRYING,
                "a contradicting report the book refused for its time is being posted again",
            ));
        }
        // Rule 3: the serving margin over the sequencer's drift.
        let servable_at = f.accepted_at.saturating_add(SERVABLE_AFTER_SECS);
        if at.time < servable_at {
            return Err(not_servable(
                reason::IN_MARGIN,
                format!("past the dispute window, not yet past the {SERVE_MARGIN_SECS} s serving margin for the sequencer's drift"),
            )
            .with_field("servableAt", json!(servable_at)));
        }
        // Rule 3 on this host's clock (review C10-R1-2, R1-3): block time can
        // run fast after a sequencer stall, and a lying node can report any
        // finalized time; the margin must also have passed since this
        // operator took the report.
        let host_at = e.received_unix.saturating_add(SERVABLE_AFTER_SECS);
        if now < host_at {
            return Err(not_servable(
                reason::IN_HOST_MARGIN,
                format!("past the margin in block time, not yet {SERVABLE_AFTER_SECS} s on this host's clock since the report was taken"),
            )
            .with_field("servableAtHostUnix", json!(host_at)));
        }
        let p = &e.report.payload;
        Ok(json!({
            "level": 3,
            "fact": fact.to_string(),
            "source": p.source.0,
            "key": p.key.to_string(),
            "metric": p.metric.0,
            "subject": subject_text(&p.subject),
            "value": value_json(&p.value),
            "observedUnix": p.observed_unix,
            "sequence": p.sequence,
            "payload": hx(&borsh::to_vec(p).unwrap_or_default()),
            "signature": hx(&e.report.signature),
            "reportHash": report_hash.to_string(),
            "valueHash": value_h.to_string(),
            "acceptedAt": f.accepted_at,
            // ADR-0048 amendment 2 (ROADMAP C64): the level-3 value's
            // one-step ladder, at the book's acceptance (a chain time).
            "ladder": crate::ladder::signed_json(
                onyx_core::ladder::value_key(
                    &onyx_core::ladder::fact_key(&p.metric, &p.subject, p.observed_unix),
                    &onyx_core::ladder::ladder_value_hash(&p.value),
                ),
                report_hash,
                f.accepted_at,
            ),
            "checkedAt": {
                "block": at.number,
                "blockHash": at.hash.to_string(),
                "blockTime": at.time,
            },
            "book": hx(&self.pin.book),
            "chainId": self.pin.chain_id,
            "verifyCall": "OnyxSignedBook.settleable(bytes32)",
            "moneySettlement": "none: no market settles on a signed fact until ROADMAP H6",
        }))
    }
}

/// Why level 3 is stopped for every fact at `now` (this host's clock), if it
/// is: the clock-skew stop (review C10-R2-2) and the delayed-post breaker
/// (rule 4, THREATS C10-3). `/v1/signed` and `/v1/health` share it, so the
/// health answer names exactly the stop a request would get.
fn stopped(log: &PostLog, now: u64) -> Option<ApiError> {
    // Review C10-R2-2: the breaker and the host margin compare this
    // host's clock with times the coordinator wrote. A clock behind the
    // coordinator's would trip the breaker late (by the skew), so a log
    // time more than HOST_SKEW_SECS ahead of this host stops level 3.
    let latest = log
        .entries
        .iter()
        .map(|e| match e.status {
            PostStatus::Pending { since, .. } => since.max(e.received_unix),
            _ => e.received_unix,
        })
        .max();
    if let Some(t) = latest.filter(|t| *t > now.saturating_add(HOST_SKEW_SECS)) {
        return Some(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "level3_clock_skew",
            format!(
                "this server's clock is {} s behind the level-3 coordinator's (most {HOST_SKEW_SECS} s): no level-3 fact is served until the clocks agree",
                t - now
            ),
        )
        .with_header("retry-after", "60".into()));
    }
    // Rule 4: a post held back too long stops level 3 altogether.
    if let Some(since) = log.oldest_pending() {
        if now > since.saturating_add(POST_DELAY_ALERT_SECS) {
            return Some(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "level3_posts_delayed",
                format!(
                    "a report this operator took has not landed in the signed book for {} s (most {POST_DELAY_ALERT_SECS} s): the sequencer may be holding posts back, so no level-3 fact is served",
                    now - since
                ),
            ));
        }
    }
    None
}

/// The `level3` field of `/v1/health` (ROADMAP C62, THREATS C10-3, C62-*):
/// the post log's counts as this node last read it and the stop a request
/// would get now, on this host's clock (the one the breaker runs on), so a
/// watch can alert before the breaker trips. Counts are `null` while the
/// log is unreadable (`stopped`: `level3_store_unreadable`).
pub fn health_of(log: Result<&PostLog, &str>, now: u64) -> Value {
    let base = json!({
        "breakerSecs": POST_DELAY_ALERT_SECS,
        "hostSkewSecs": HOST_SKEW_SECS,
        "now": now,
    });
    let mut v = base;
    let log = match log {
        Ok(l) => l,
        Err(e) => {
            for k in [
                "posts",
                "postsRecorded",
                "postsPending",
                "postsRefused",
                "postsRefusedRetrying",
                "postsRefusedFinal",
                "oldestPendingSince",
                "oldestPendingSecs",
            ] {
                v[k] = Value::Null;
            }
            v["logReadable"] = json!(false);
            v["logError"] = json!(e);
            v["stopped"] = json!("level3_store_unreadable");
            return v;
        }
    };
    let (mut recorded, mut pending, mut refused, mut retrying) = (0u64, 0u64, 0u64, 0u64);
    for e in &log.entries {
        match &e.status {
            PostStatus::Recorded { .. } => recorded += 1,
            PostStatus::Pending { .. } => pending += 1,
            PostStatus::Refused { error } => {
                refused += 1;
                if onyx_coordinator::signed_intake::retryable(error) {
                    retrying += 1;
                }
            }
        }
    }
    let oldest = log.oldest_pending();
    v["logReadable"] = json!(true);
    v["logError"] = Value::Null;
    v["posts"] = json!(log.entries.len());
    v["postsRecorded"] = json!(recorded);
    v["postsPending"] = json!(pending);
    v["postsRefused"] = json!(refused);
    v["postsRefusedRetrying"] = json!(retrying);
    // Review C61-R1-2 (C62): refusals that never clear, so never retried:
    // only grows while the log is kept, the count a watch alerts on.
    v["postsRefusedFinal"] = json!(refused - retrying);
    v["oldestPendingSince"] = json!(oldest);
    // Saturating: a pending time ahead of this clock reads 0 here and stops
    // level 3 as `level3_clock_skew` once more than HOST_SKEW_SECS ahead.
    v["oldestPendingSecs"] = json!(oldest.map(|s| now.saturating_sub(s)));
    v["stopped"] = json!(stopped(log, now).map(|e| e.code));
    v
}

impl crate::Level3Health for SignedStore {
    fn level3_health(&self) -> Value {
        let now = (self.now)();
        match self.log() {
            Ok(l) => health_of(Ok(&l), now),
            Err(e) => health_of(Err(&e), now),
        }
    }
}

/// The level-3 routes, to merge into [`crate::router_with`]'s `extra`.
pub fn routes(store: Arc<SignedStore>) -> Router {
    Router::new()
        .route("/v1/signed/{fact}", get(signed))
        .with_state(store)
}

async fn signed(
    State(store): State<Arc<SignedStore>>,
    client: Option<Extension<ClientKey>>,
    path: Result<Path<String>, PathRejection>,
    RawQuery(q): RawQuery,
) -> Response {
    let client = client.map_or_else(|| "local".to_string(), |Extension(c)| c.0);
    reply(&store, &client, path, q)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}

async fn reply(
    store: &SignedStore,
    client: &str,
    path: Result<Path<String>, PathRejection>,
    q: Option<String>,
) -> Result<Response, ApiError> {
    let Path(fact) = path.map_err(|e| bad_request(e.body_text()))?;
    if q.is_some_and(|q| !q.is_empty()) {
        return Err(bad_request(
            "GET /v1/signed/{fact} takes no query parameters",
        ));
    }
    let (fact, canonical) = crate::parse_hash(&fact, "fact")?;
    if !canonical {
        return Err(bad_request("fact must be 0x and 64 lowercase hex digits"));
    }
    let scopes: Vec<(String, u32)> = rate_scopes(client, store.free_per_minute)
        .into_iter()
        .map(|(k, n)| (format!("signed:{k}"), n))
        .collect();
    if !store.limits.take_attempts(&scopes) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many level-3 answers from this address",
        )
        .with_header(
            "retry-after",
            (60 / store.free_per_minute.max(1)).max(1).to_string(),
        ));
    }
    let body = store.answer(&fact).await?;
    let mut r = (StatusCode::OK, Json(body)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(r)
}

#[cfg(test)]
#[path = "signed_serve_tests.rs"]
mod tests;
