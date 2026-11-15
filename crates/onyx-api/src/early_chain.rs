//! The early-contributor record's chain side (ADR-0065 slice 2): the reader
//! that turns finalized blocks into one version a day, and the folder rules
//! of the `early-record` tool (`examples/early-record.rs`).
//!
//! - **Reads only final blocks.** The head is the node's `finalized` block.
//!   A UTC day is written only when it is complete: the first block of the
//!   next day (a block at 00:00:00 starts it) is at or below `finalized`.
//!   Its last block is found by a binary search on block times.
//! - **One chain, checked.** Before a day is read, the previous `day_close`
//!   block hash must still be the canonical one ([`ReadError::Canonical`]
//!   stops the run). On the first run each counted contract's `scan_from`
//!   block is checked (a `deploy` block: no code one block before, code at
//!   it, as the indexer does; every contract: code at the head; a deploy
//!   transaction's receipt gives the deploy block, kept in the state).
//! - **Logs.** One `eth_getLogs` filter for every counted contract (an
//!   address array and topic 0 as a list), in ranges of at most [`SPAN`]
//!   blocks, split in halves when the node refuses one ([`crate::indexer`]'s
//!   `Spans`, shared with the indexer). Only the node's own error text is
//!   classified, before any context is added: HTTP 429 or rate-limit wording
//!   is retried with [`BACKOFF_MS`]; anything else (a span cap, a result
//!   limit such as JSON-RPC -32005 "more than 10000 results") splits. When a
//!   rate limit lasts, the run stops and the logs read so far are kept
//!   ([`Pending`], bound to the hash of `contracts.json`), so the next run
//!   goes on from there. Each log's block time comes from its block header
//!   (one read per block, cached), and a log whose block hash is not that
//!   header's is refused.
//! - **Two reads.** A load-balanced node can answer from a lagging backend
//!   (the indexer's IDX-R1). The head is the lower of two `finalized` reads,
//!   and each day's logs are read twice, the second pass independent of the
//!   first: any difference stops the run and nothing is written. `verify`
//!   against a second node is the gate before a version is published.
//! - **Views at the day's last block.** The role getters (exclusions), the
//!   book's `report(digest)` for an exact observed time where a compromise
//!   or a retirement needs it, and `shardState` for shards that ended Empty
//!   (EARLY-15: no event says so).
//! - **Folders.** [`build`] writes one new version folder (`data.csv`,
//!   `schema.json`, `day.json`, and `exclusions/` when the exclusions
//!   change) and never builds a built folder again: a second run compares
//!   and stops on any difference. `day.json` holds the hash of the join's
//!   state after the day, so a log left out whose row comes only later
//!   still shows on its own day. [`verify`] rebuilds a day from the chain
//!   and compares the bytes (and the shard roots of a built manifest).
//!
//! `l2_agreed` is not here: its evidence is the round transcript in the
//! coordinator's results store, which the free route withholds while a
//! value is sold, so a public rebuild cannot reproduce those rows yet
//! (ADR-0065 "Slices").

use crate::early::{
    exclusion_changes, exclusions_csv, exclusions_hash, exclusions_in_force, exclusions_schema,
    fixed_exclusions, record_schema, role_calls, schema_file, shard_files_of, utc_day, write_day,
    ChainLog, Contracts, DayClose, EarlyError, Exclusion, ExclusionRow, ReviewedFile, ScanFrom,
    State, Views, FORMAT,
};
use crate::indexer::{call_retrying, hex_bytes, hex_u64, RpcFailure, Spans, MAX_LOGS_PER_RANGE};
use crate::roots::JsonRpc;
use onyx_core::{keccak256, Hash32};
use onyx_storage::abi;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Blocks per `eth_getLogs` call (the public Base Sepolia node's cap).
pub const SPAN: u64 = 200;
/// Waits between tries of a failing or rate-limited call (about 31 s).
pub const BACKOFF_MS: [u64; 5] = [1_000, 2_000, 4_000, 8_000, 16_000];
/// Ranges read between two saves of [`Pending`].
pub const SAVE_EVERY_RANGES: u64 = 50;
/// `OnyxLedger.ShardStatus` values after which a shard never changes and was
/// never released: Empty, Voided, NoValidRoot, Unresolved.
const ENDED: [u8; 4] = [4, 5, 7, 8];
const SECS_PER_DAY: u64 = 86_400;

/// Why a run stopped.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The node failed, rate-limited or timed out past the retries; the logs
    /// read so far are kept and the next run goes on from there.
    #[error("node: {0}")]
    Node(String),
    /// The previous `day_close` block is no longer canonical (a reorg, or
    /// another chain): nothing is written.
    #[error("the previous day's last block changed: {0}")]
    Canonical(String),
    /// A contract's block in `contracts.json` does not hold on chain.
    #[error("contracts: {0}")]
    Contracts(String),
    /// The record refused the day.
    #[error(transparent)]
    Record(#[from] EarlyError),
    /// A version folder or the state directory.
    #[error("folder: {0}")]
    Folder(String),
}

fn node(e: impl std::fmt::Display) -> ReadError {
    ReadError::Node(e.to_string())
}

/// True for a node's own error text (never one with block numbers or other
/// context added) that says it turned the call away for load: HTTP 429, or
/// clear rate-limit wording. Retried, never split. A span cap or a result
/// limit (JSON-RPC -32005 "query returned more than 10000 results") is not
/// a rate limit: that range is split.
pub fn rate_limited(raw: &str) -> bool {
    let e = raw.to_ascii_lowercase();
    [
        "http 429",
        "status 429",
        "429 too many",
        "too many requests",
        "rate limit",
        "rate-limit",
        "ratelimit",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// One `eth_getLogs` answer, classified on the node's raw text.
enum Got {
    Logs(Vec<Value>),
    /// The node refused the range (split it).
    Refused(String),
    /// Rate-limited past the retries (stop; the next run goes on).
    Failed(ReadError),
}

/// The logs of one day read so far: kept in the state directory, so a run
/// cut off by the node goes on where it stopped. Bound to the hash of the
/// `contracts.json` it was read for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    /// `0x` hash of `contracts.json`.
    pub contracts: String,
    /// First block of the day.
    pub from: u64,
    /// Last block of the day.
    pub to: u64,
    /// 1: the first read; 2: the second, independent read.
    pub pass: u8,
    /// The next block to read in this pass.
    pub next: u64,
    /// The logs this pass read, as the node gave them.
    pub logs: Vec<Value>,
    /// The first pass's logs (during the second).
    pub first: Vec<Value>,
}

/// The logs as a set: each log's JSON text, sorted, repeats dropped.
fn log_set(v: &[Value]) -> Vec<String> {
    let mut s: Vec<String> = v.iter().map(Value::to_string).collect();
    s.sort();
    s.dedup();
    s
}

/// The reader: one node, retries, and the block headers it has read.
pub struct Reader<'a, R: JsonRpc + ?Sized> {
    rpc: &'a R,
    backoff: Vec<Duration>,
    span: u64,
    blocks: Mutex<BTreeMap<u64, (u64, Hash32)>>,
}

impl<'a, R: JsonRpc + ?Sized> Reader<'a, R> {
    /// A reader of `rpc` with `backoff` between tries and `span` blocks per
    /// log call.
    pub fn new(rpc: &'a R, backoff: &[Duration], span: u64) -> Self {
        Reader {
            rpc,
            backoff: backoff.to_vec(),
            span: span.max(1),
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    /// A reader with [`BACKOFF_MS`] and [`SPAN`].
    pub fn standard(rpc: &'a R) -> Self {
        Reader::new(rpc, &BACKOFF_MS.map(Duration::from_millis), SPAN)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, ReadError> {
        call_retrying(self.rpc, &self.backoff, method, params)
            .await
            .map_err(|f| match f {
                RpcFailure::StateMissing(e) => ReadError::Node(format!(
                    "{method}: the node does not keep that old state ({e}); use one that does (an archive node for old blocks)"
                )),
                f => ReadError::Node(format!("{method}: {f}")),
            })
    }

    fn header(v: &Value, want: Option<u64>) -> Result<(u64, u64, Hash32), ReadError> {
        if v.is_null() {
            return Err(node("the node has no such block"));
        }
        let n = hex_u64(&v["number"], "block number").map_err(node)?;
        if want.is_some_and(|w| w != n) {
            return Err(node(format!("asked for block {want:?}, got {n}")));
        }
        let t = hex_u64(&v["timestamp"], "block timestamp").map_err(node)?;
        let h: [u8; 32] = hex_bytes(&v["hash"], "block hash")
            .map_err(node)?
            .try_into()
            .map_err(|_| node("block hash: not 32 bytes"))?;
        Ok((n, t, Hash32(h)))
    }

    async fn finalized_once(&self) -> Result<u64, ReadError> {
        let v = self
            .call("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        let (n, t, h) = Self::header(&v, None)?;
        if let Ok(mut b) = self.blocks.lock() {
            b.insert(n, (t, h));
        }
        Ok(n)
    }

    /// The node's `finalized` block: the lower of two reads, so one answer
    /// from a backend ahead of the others cannot move the head.
    pub async fn finalized(&self) -> Result<u64, ReadError> {
        let a = self.finalized_once().await?;
        let b = self.finalized_once().await?;
        Ok(a.min(b))
    }

    /// Block `n`'s time and hash (read once per reader).
    pub async fn block(&self, n: u64) -> Result<(u64, Hash32), ReadError> {
        if let Some(x) = self.blocks.lock().ok().and_then(|b| b.get(&n).copied()) {
            return Ok(x);
        }
        let v = self
            .call("eth_getBlockByNumber", json!([format!("0x{n:x}"), false]))
            .await?;
        let (_, t, h) = Self::header(&v, Some(n))?;
        if let Ok(mut b) = self.blocks.lock() {
            b.insert(n, (t, h));
        }
        Ok((t, h))
    }

    /// The UTC day of block `from` and its last block, when that day is
    /// complete at or below `head`: the block before the first one whose time
    /// is at or after the next 00:00:00 (binary search). `None` while the day
    /// is not complete: a day past `finalized` is never written.
    pub async fn day_of(&self, from: u64, head: u64) -> Result<Option<(u32, u64)>, ReadError> {
        if from > head {
            return Ok(None);
        }
        let (t, _) = self.block(from).await?;
        let day = utc_day(t);
        let next = (u64::from(day) + 1) * SECS_PER_DAY;
        if self.block(head).await?.0 < next {
            return Ok(None);
        }
        let (mut lo, mut hi) = (from + 1, head);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.block(mid).await?.0 >= next {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Ok(Some((day, lo - 1)))
    }

    /// `eth_call` of `data` to `to` at `block`; an empty answer is a contract
    /// with no code there.
    pub async fn eth_call(&self, to: &str, data: &[u8], block: u64) -> Result<Vec<u8>, ReadError> {
        let v = self
            .call(
                "eth_call",
                json!([{"to": to, "data": format!("0x{}", hex::encode(data))}, format!("0x{block:x}")]),
            )
            .await?;
        hex_bytes(&v, "eth_call").map_err(node)
    }

    async fn code(&self, a: &str, block: u64) -> Result<Vec<u8>, ReadError> {
        let v = self
            .call("eth_getCode", json!([a, format!("0x{block:x}")]))
            .await?;
        hex_bytes(&v, "eth_getCode").map_err(node)
    }

    /// Check each counted contract once (EARLY-3): code at `head`; at a
    /// `deploy` block, code there and none one block before; a deploy
    /// transaction's receipt names this address, and a `bound` block is at or
    /// below its block. Returns the deploy blocks found (name -> block).
    pub async fn check_contracts(
        &self,
        c: &Contracts,
        head: u64,
    ) -> Result<BTreeMap<String, u64>, ReadError> {
        let mut found = BTreeMap::new();
        for e in c.file.contracts.iter().filter(|e| e.abi.counted()) {
            let what = |s: String| ReadError::Contracts(format!("{} ({}): {s}", e.name, e.address));
            let from = e.scan_from.unwrap_or(0);
            if self.code(&e.address, head).await?.is_empty() {
                return Err(what(format!("no code at the head {head}: another chain?")));
            }
            if e.scan_from_is == Some(ScanFrom::Deploy) {
                if self.code(&e.address, from).await?.is_empty() {
                    return Err(what(format!("no code at its deploy block {from}")));
                }
                if from > 0 && !self.code(&e.address, from - 1).await?.is_empty() {
                    return Err(what(format!(
                        "code already at block {} before its deploy block {from}: events would be missed",
                        from - 1
                    )));
                }
                found.insert(e.name.clone(), from);
            }
            if let Some(tx) = &e.deploy_tx {
                let r = self.call("eth_getTransactionReceipt", json!([tx])).await?;
                let b = hex_u64(&r["blockNumber"], "receipt blockNumber").map_err(node)?;
                if r["contractAddress"]
                    .as_str()
                    .map(str::to_ascii_lowercase)
                    .as_deref()
                    != Some(e.address.as_str())
                {
                    return Err(what(format!(
                        "its deploy transaction {tx} created another address"
                    )));
                }
                if b < from {
                    return Err(what(format!(
                        "deployed at block {b}, before its scan_from {from}: events would be missed"
                    )));
                }
                if e.scan_from_is == Some(ScanFrom::Deploy) && b != from {
                    return Err(what(format!(
                        "deployed at block {b}, not at its deploy block {from}"
                    )));
                }
                found.insert(e.name.clone(), b);
            }
        }
        Ok(found)
    }

    /// One `eth_getLogs` call, retried while the node's own text says it is
    /// rate-limited.
    async fn get_logs(&self, params: &Value, a: u64, b: u64) -> Got {
        let mut waits = self.backoff.iter();
        loop {
            match self.rpc.call("eth_getLogs", params.clone()).await {
                Ok(Value::Array(arr)) if arr.len() <= MAX_LOGS_PER_RANGE => return Got::Logs(arr),
                Ok(v) => {
                    return Got::Refused(format!(
                        "not a list of at most {MAX_LOGS_PER_RANGE} logs ({} entries)",
                        v.as_array().map_or(0, Vec::len)
                    ))
                }
                Err(e) if rate_limited(&e) => match waits.next() {
                    Some(d) => tokio::time::sleep(*d).await,
                    None => {
                        return Got::Failed(node(format!(
                            "eth_getLogs {a}..={b}: rate-limited past the retries ({e}); run again to go on"
                        )))
                    }
                },
                Err(e) => return Got::Refused(e),
            }
        }
    }

    /// Read `pending.next..=to` into `pending.logs`.
    async fn read_pass(
        &self,
        c: &Contracts,
        to: u64,
        pending: &mut Pending,
        save: &mut dyn FnMut(&Pending) -> Result<(), ReadError>,
    ) -> Result<(), ReadError> {
        let (addrs, topics) = c.filter();
        let mut spans = Spans::new(pending.next, to, self.span);
        let mut ranges = 0u64;
        while let Some((a, b)) = spans.range() {
            let params = json!([{
                "address": addrs,
                "fromBlock": format!("0x{a:x}"),
                "toBlock": format!("0x{b:x}"),
                "topics": [topics],
            }]);
            match self.get_logs(&params, a, b).await {
                Got::Logs(arr) => pending.logs.extend(arr),
                Got::Refused(_) if spans.split() => continue,
                Got::Refused(e) => {
                    save(pending)?;
                    return Err(node(format!(
                        "eth_getLogs {a}..={b}: the node refuses even one block: {e}"
                    )));
                }
                Got::Failed(e) => {
                    save(pending)?;
                    return Err(e);
                }
            }
            spans.done();
            pending.next = spans.next_block();
            ranges += 1;
            if ranges.is_multiple_of(SAVE_EVERY_RANGES) {
                save(pending)?;
            }
        }
        Ok(())
    }

    /// The logs of `from..=to` for every counted contract, read twice (the
    /// second pass independent of the first; any difference stops the run),
    /// going on from `pending` when it holds this range of this
    /// `contracts.json`; `save` keeps `pending` every [`SAVE_EVERY_RANGES`]
    /// ranges and when the node stops the run.
    pub async fn logs(
        &self,
        c: &Contracts,
        from: u64,
        to: u64,
        pending: &mut Pending,
        save: &mut dyn FnMut(&Pending) -> Result<(), ReadError>,
    ) -> Result<Vec<ChainLog>, ReadError> {
        let bound = format!("0x{}", c.hash.to_hex());
        if (pending.contracts.as_str(), pending.from, pending.to) != (bound.as_str(), from, to)
            || !(1..=2).contains(&pending.pass)
            || pending.next < from
        {
            *pending = Pending {
                contracts: bound.clone(),
                from,
                to,
                pass: 1,
                next: from,
                ..Pending::default()
            };
        }
        self.read_pass(c, to, pending, save).await?;
        if pending.pass == 1 {
            pending.first = std::mem::take(&mut pending.logs);
            pending.pass = 2;
            pending.next = from;
            save(pending)?;
            self.read_pass(c, to, pending, save).await?;
        }
        let (one, two) = (log_set(&pending.first), log_set(&pending.logs));
        if one != two {
            let e = node(format!(
                "two reads of blocks {from}..={to} differ ({} and {} logs): a lagging or lying backend; run again, or use another node",
                one.len(),
                two.len()
            ));
            *pending = Pending::default();
            save(pending)?;
            return Err(e);
        }
        let mut out = Vec::with_capacity(pending.logs.len());
        for v in &pending.logs {
            let n = hex_u64(&v["blockNumber"], "log blockNumber").map_err(node)?;
            let (t, h) = self.block(n).await?;
            if let Some(bh) = v["blockHash"].as_str() {
                if !bh.eq_ignore_ascii_case(&format!("0x{}", h.to_hex())) {
                    return Err(node(format!(
                        "a log of block {n} names block hash {bh}, not the header's: the node mixes forks"
                    )));
                }
            }
            out.push(ChainLog::from_rpc(v, Some(t))?);
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------ the tool's state

/// The previous version's `day_close`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastClose {
    /// Its day (days since 1970-01-01).
    pub day: u32,
    /// Its last block.
    pub to_block: u64,
    /// That block's hash (`0x`).
    pub hash: String,
}

/// `state.json` of the tool's state directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolState {
    /// [`FORMAT`].
    pub format: u32,
    /// The join's cache.
    pub record: State,
    /// The last day written.
    pub last: Option<LastClose>,
    /// Every exclusions row written, in key order.
    pub exclusions: Vec<ExclusionRow>,
    /// What each built day's folder holds (keccak256 of its files).
    pub built: BTreeMap<u32, BuiltFiles>,
    /// The deploy blocks found when the contracts were checked (`None`:
    /// not checked yet).
    pub deploy_blocks: Option<BTreeMap<String, u64>>,
}

/// keccak256 (`0x`) of a built folder's files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltFiles {
    /// `data.csv`.
    pub data: String,
    /// `day.json`.
    pub day: String,
    /// `exclusions/data.csv`, when the day changed the exclusions.
    pub exclusions: Option<String>,
}

impl ToolState {
    /// A fresh state for `c`.
    pub fn new(c: &Contracts) -> ToolState {
        ToolState {
            format: FORMAT,
            record: State::new(c),
            last: None,
            exclusions: Vec::new(),
            built: BTreeMap::new(),
            deploy_blocks: None,
        }
    }
}

/// One day as the chain gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Day {
    /// Days since 1970-01-01.
    pub day: u32,
    /// The `day_close` numbers.
    pub close: DayClose,
    /// The record's CSV.
    pub csv: String,
    /// The exclusions rows this day appends (none: no exclusions version).
    pub exclusion_rows: Vec<ExclusionRow>,
    /// Wanted exclusions left out because they name a publisher key.
    pub skipped: Vec<Exclusion>,
    /// Days between the previous version and this one that have no block
    /// at all (a halted chain): no version is written for them.
    pub empty_days: Vec<u32>,
    /// The tool state after the day.
    pub after: ToolState,
}

/// Read the next complete day after `tool`'s cursor (`want`: only that
/// day). `None` when no new day is complete at or below `finalized`.
pub async fn next_day<R: JsonRpc + ?Sized>(
    reader: &Reader<'_, R>,
    c: &Contracts,
    reviewed: &ReviewedFile,
    tool: &ToolState,
    want: Option<u32>,
    pending: &mut Pending,
    save: &mut dyn FnMut(&Pending) -> Result<(), ReadError>,
) -> Result<Option<Day>, ReadError> {
    let head = reader.finalized().await?;
    if let Some(l) = &tool.last {
        let (_, h) = reader.block(l.to_block).await?;
        let now = format!("0x{}", h.to_hex());
        if now != l.hash {
            return Err(ReadError::Canonical(format!(
                "block {} was {} and is now {now}",
                l.to_block, l.hash
            )));
        }
    }
    let mut after = tool.clone();
    if after.deploy_blocks.is_none() {
        after.deploy_blocks = Some(reader.check_contracts(c, head).await?);
    }
    let from = tool.record.next_block;
    let Some((day, to)) = reader.day_of(from, head).await? else {
        return Ok(None);
    };
    if want.is_some_and(|w| w != day) {
        return Err(ReadError::Folder(format!(
            "the next day to write is {}, not the one asked for",
            crate::early::day_text(day)
        )));
    }
    let logs = reader.logs(c, from, to, pending, save).await?;
    // Exact observed times where a compromise or a retirement needs them.
    let mut views = Views::default();
    if let Some(book) = c.of(crate::early::Abi::SignedBookV1) {
        for d in tool.record.digests_needing_observed(c, &logs)? {
            let w = Hash32(
                hex::decode(d.trim_start_matches("0x"))
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| node("digest"))?,
            );
            let data = abi::call("report(bytes32)", &[abi::b32(&w)]);
            let r = reader.eth_call(&book.address, &data, to).await?;
            if abi::word(&r, 0).map_err(node)? == [0u8; 32] {
                return Err(node(format!("report({d}) is not recorded at block {to}")));
            }
            views
                .observed
                .insert(d, abi::word_u64(&r, 4).map_err(node)?);
        }
    }
    let rows = after.record.apply(c, from, to, &logs, &views)?;
    // EARLY-15: shards that ended without an event (Empty) leave the cache.
    if let Some(s) = c.of(crate::early::Abi::SettlementV1) {
        for (round, shard, rs) in after.record.unfinalized_shards() {
            let data = abi::call(
                "shardState(uint64,bytes32)",
                &[abi::uint(u128::from(round)), abi::b32(&shard)],
            );
            let r = reader.eth_call(&s.address, &data, to).await?;
            let status = abi::word_uint(&r, 0).map_err(node)?;
            if ENDED.iter().any(|e| u128::from(*e) == status) {
                after.record.drop_shard(&rs);
            }
        }
    }
    // Exclusions as of the day's last block.
    let mut wanted = fixed_exclusions();
    // Each reviewed entry only on its own days, so a replay from the deploy
    // blocks gives every earlier day the same bytes after the list grows.
    wanted.extend(reviewed.exclusions_on(day));
    for call in role_calls(c, to) {
        let data = hex::decode(
            call.params[0]["data"]
                .as_str()
                .unwrap_or("0x")
                .trim_start_matches("0x"),
        )
        .map_err(node)?;
        let r = reader.eth_call(&call.to, &data, to).await?;
        if r.is_empty() {
            // No code yet at this block: the role does not exist that day.
            continue;
        }
        wanted.extend(call.exclusion(&format!("0x{}", hex::encode(&r)))?);
    }
    let in_force = exclusions_in_force(&after.exclusions)?;
    let changes = exclusion_changes(&in_force, &wanted, &after.record.publisher_signers(), day)?;
    after.exclusions.extend(changes.rows.clone());
    let now_in_force = exclusions_in_force(&after.exclusions)?;
    let (_, last_hash) = reader.block(to).await?;
    let close = DayClose {
        from_block: from,
        to_block: to,
        last_block_hash: last_hash,
        contracts_hash: c.hash,
        exclusions_hash: exclusions_hash(&now_in_force),
    };
    let v = write_day(day, &rows, &close)?;
    after.record.record_keys(&v.keys)?;
    after.record.prune();
    after.last = Some(LastClose {
        day,
        to_block: to,
        hash: format!("0x{}", last_hash.to_hex()),
    });
    let empty_days = tool
        .last
        .as_ref()
        .map(|l| (l.day + 1..day).collect())
        .unwrap_or_default();
    let mut d = Day {
        day,
        close,
        csv: v.csv,
        exclusion_rows: changes.rows,
        skipped: changes.skipped,
        empty_days,
        after,
    };
    let files = BuiltFiles {
        data: csv_hash(&d.csv),
        day: hash_bytes(&day_json_bytes(&d)?),
        exclusions: (!d.exclusion_rows.is_empty())
            .then(|| exclusions_csv(&d.exclusion_rows).map(|x| csv_hash(&x)))
            .transpose()?,
    };
    d.after.built.insert(day, files);
    Ok(Some(d))
}

fn hash_bytes(b: &[u8]) -> String {
    format!("0x{}", keccak256(b).to_hex())
}

/// keccak256 of the join's state as saved (`0x` hex): `day.json`'s
/// `state_keccak`.
pub fn state_hash(s: &State) -> String {
    hash_bytes(&s.to_json())
}

/// keccak256 of a CSV, `0x` hex.
pub fn csv_hash(csv: &str) -> String {
    format!("0x{}", keccak256(csv.as_bytes()).to_hex())
}

// ------------------------------------------------------------------ folders

/// What [`build`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Built {
    /// A new version folder for this day.
    Day {
        /// The day.
        day: u32,
        /// Rows, `day_close` included.
        rows: usize,
        /// Wanted exclusions left out (publisher keys), for the reviewer.
        skipped: Vec<Exclusion>,
        /// Days before it with no block at all (no version for them).
        empty_days: Vec<u32>,
    },
    /// The folder already holds this built day, byte for byte: nothing done.
    AlreadyBuilt(u32),
    /// No new day is complete at or below `finalized`: nothing written.
    NothingNew,
}

/// Where `build` reads and writes.
#[derive(Clone, Debug)]
pub struct BuildOpts {
    /// `contracts.json`.
    pub contracts: PathBuf,
    /// The reviewed `exclusions.json`.
    pub exclusions: PathBuf,
    /// The state directory (`state.json`, `pending.json`, `before/`).
    pub state: PathBuf,
    /// The new version folder.
    pub out: PathBuf,
    /// The previous version folder (checked against the state).
    pub prev: Option<PathBuf>,
    /// Only this day.
    pub day: Option<u32>,
}

fn io(p: &Path, e: impl std::fmt::Display) -> ReadError {
    ReadError::Folder(format!("{}: {e}", p.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<Option<T>, ReadError> {
    match std::fs::read(p) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| io(p, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(p, e)),
    }
}

/// Write `x` as JSON to `p` atomically: a temporary file, fsync, rename,
/// fsync of the directory (the indexer's `write_atomic`).
fn save_json<T: Serialize>(p: &Path, x: &T) -> Result<(), ReadError> {
    let v = serde_json::to_value(x).map_err(|e| io(p, e))?;
    crate::indexer::write_atomic(p, &v).map_err(|e| io(p, e))
}

/// Write `bytes` to `p` and fsync it.
fn write_synced(p: &Path, bytes: &[u8]) -> Result<(), ReadError> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(p).map_err(|e| io(p, e))?;
    f.write_all(bytes).map_err(|e| io(p, e))?;
    f.sync_all().map_err(|e| io(p, e))
}

fn sync_dir(p: &Path) -> Result<(), ReadError> {
    std::fs::File::open(p)
        .and_then(|d| d.sync_all())
        .map_err(|e| io(p, e))
}

/// Hold the state directory for this run: a second concurrent run is
/// refused. The `flock` dies with the process, so a crash leaves no lock.
pub fn lock_state(dir: &Path) -> Result<std::fs::File, ReadError> {
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let p = dir.join("lock");
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&p)
        .map_err(|e| io(&p, e))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(io(
            dir,
            "another early-record run holds this state directory",
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(io(&p, e)),
    }
}

fn load_inputs(o: &BuildOpts) -> Result<(Contracts, ReviewedFile), ReadError> {
    let c = Contracts::load(&o.contracts)?;
    let b = std::fs::read(&o.exclusions).map_err(|e| io(&o.exclusions, e))?;
    Ok((c, ReviewedFile::parse(&b)?))
}

/// `day.json` as written: pretty JSON and a line feed.
fn day_json_bytes(d: &Day) -> Result<Vec<u8>, ReadError> {
    let mut b =
        serde_json::to_vec_pretty(&day_json(d)).map_err(|e| ReadError::Folder(e.to_string()))?;
    b.push(b'\n');
    Ok(b)
}

fn day_json(d: &Day) -> Value {
    json!({
        "format": FORMAT,
        "dataset": crate::early::DATASET,
        "day": crate::early::day_text(d.day),
        "from_block": d.close.from_block,
        "to_block": d.close.to_block,
        "last_block_hash": format!("0x{}", d.close.last_block_hash.to_hex()),
        "contracts_hash": format!("0x{}", d.close.contracts_hash.to_hex()),
        "exclusions_hash": format!("0x{}", d.close.exclusions_hash.to_hex()),
        "csv_keccak": csv_hash(&d.csv),
        // The join's state after the day (THREATS EARLY-4, review of slice 3): a log a node
        // left out whose row comes only on a later day (a bet, a report not accepted yet)
        // changes no byte of this day's CSV, but it changes this state, so `verify` of this
        // day on another node fails on day.json.
        "state_keccak": state_hash(&d.after.record),
    })
}

/// The day a version folder holds (from its `day.json`).
fn folder_day(dir: &Path) -> Result<Option<(u32, Value)>, ReadError> {
    let Some(v) = read_json::<Value>(&dir.join("day.json"))? else {
        return Ok(None);
    };
    let text = v["day"].as_str().unwrap_or_default().to_string();
    let day = crate::early::parse_day(&text)
        .ok_or_else(|| io(dir, format!("day.json: bad day {text:?}")))?;
    Ok(Some((day, v)))
}

fn write_folder(out: &Path, d: &Day) -> Result<(), ReadError> {
    let tmp = out.with_extension("building");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    }
    std::fs::create_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    let w = |p: PathBuf, b: &[u8]| write_synced(&p, b);
    w(tmp.join("data.csv"), d.csv.as_bytes())?;
    w(
        tmp.join("schema.json"),
        schema_file(&record_schema()).as_bytes(),
    )?;
    if !d.exclusion_rows.is_empty() {
        let x = tmp.join("exclusions");
        std::fs::create_dir_all(&x).map_err(|e| io(&x, e))?;
        w(
            x.join("data.csv"),
            exclusions_csv(&d.exclusion_rows)?.as_bytes(),
        )?;
        w(
            x.join("schema.json"),
            schema_file(&exclusions_schema()).as_bytes(),
        )?;
        sync_dir(&x)?;
    }
    w(tmp.join("day.json"), &day_json_bytes(d)?)?;
    sync_dir(&tmp)?;
    if out.exists() {
        std::fs::remove_dir(out).map_err(|e| io(out, e))?;
    }
    std::fs::rename(&tmp, out).map_err(|e| io(out, e))?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent)?;
    }
    Ok(())
}

fn read_opt(p: &Path) -> Result<Option<Vec<u8>>, ReadError> {
    match std::fs::read(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(p, e)),
    }
}

/// The schema files of a built folder must be the code's.
fn same_schemas(out: &Path) -> Result<(), ReadError> {
    let s = read_opt(&out.join("schema.json"))?;
    if s.as_deref() != Some(schema_file(&record_schema()).as_bytes()) {
        return Err(io(out, "schema.json is not the record's schema"));
    }
    let x = out.join("exclusions");
    if x.exists() {
        let s = read_opt(&x.join("schema.json"))?;
        if s.as_deref() != Some(schema_file(&exclusions_schema()).as_bytes()) {
            return Err(io(
                out,
                "exclusions/schema.json is not the exclusions' schema",
            ));
        }
    }
    Ok(())
}

/// Compare a built folder with `d`, byte for byte: `data.csv`, `day.json`,
/// the schema files and, when either has them, the exclusions rows.
fn same_folder(out: &Path, d: &Day) -> Result<(), ReadError> {
    same_schemas(out)?;
    if read_opt(&out.join("day.json"))? != Some(day_json_bytes(d)?) {
        return Err(io(out, "day.json differs from the chain's"));
    }
    let got = std::fs::read_to_string(out.join("data.csv")).map_err(|e| io(out, e))?;
    if got != d.csv {
        return Err(io(
            out,
            "data.csv differs from the chain's: the folder is never built again",
        ));
    }
    let x = out.join("exclusions").join("data.csv");
    let want = (!d.exclusion_rows.is_empty())
        .then(|| exclusions_csv(&d.exclusion_rows))
        .transpose()?;
    let have = match std::fs::read_to_string(&x) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io(&x, e)),
    };
    if have != want {
        return Err(io(out, "exclusions/data.csv differs from the chain's"));
    }
    Ok(())
}

/// One run of `early-record build`: write the next complete day into the new
/// folder `o.out`, then save the state. A folder that already holds a day is
/// never built again: when the state recorded that day, its `data.csv` must
/// hash to what was recorded (a second run writes nothing); when the run that
/// built it stopped before saving the state, the day is read again and must
/// match byte for byte.
pub async fn build<R: JsonRpc + ?Sized>(
    reader: &Reader<'_, R>,
    o: &BuildOpts,
) -> Result<Built, ReadError> {
    let (c, reviewed) = load_inputs(o)?;
    let _lock = lock_state(&o.state)?;
    let state_path = o.state.join("state.json");
    let pending_path = o.state.join("pending.json");
    let tool: ToolState = read_json(&state_path)?.unwrap_or_else(|| ToolState::new(&c));
    if tool.format != FORMAT {
        return Err(io(&state_path, format!("format {}", tool.format)));
    }
    if let Some(p) = &o.prev {
        let (pday, pv) =
            folder_day(p)?.ok_or_else(|| io(p, "--prev is not a version folder (no day.json)"))?;
        let l = tool
            .last
            .as_ref()
            .ok_or_else(|| io(p, "--prev given, but the state has no day yet"))?;
        if pday != l.day
            || pv["to_block"].as_u64() != Some(l.to_block)
            || pv["last_block_hash"].as_str() != Some(l.hash.as_str())
        {
            return Err(io(p, "--prev is not the state's last day"));
        }
    }
    let existing = if o.out.exists() {
        match folder_day(&o.out)? {
            Some((d, _)) => Some(d),
            None => {
                let empty = std::fs::read_dir(&o.out)
                    .map_err(|e| io(&o.out, e))?
                    .next()
                    .is_none();
                if !empty {
                    return Err(io(&o.out, "not empty and not a version folder"));
                }
                None
            }
        }
    } else {
        None
    };
    if let Some(d) = existing {
        if let Some(h) = tool.built.get(&d) {
            same_schemas(&o.out)?;
            let hash_of = |p: PathBuf| read_opt(&p).map(|b| b.map(|b| hash_bytes(&b)));
            let have = (
                hash_of(o.out.join("data.csv"))?,
                hash_of(o.out.join("day.json"))?,
                hash_of(o.out.join("exclusions").join("data.csv"))?,
            );
            if have
                != (
                    Some(h.data.clone()),
                    Some(h.day.clone()),
                    h.exclusions.clone(),
                )
            {
                return Err(io(
                    &o.out,
                    "the folder differs from the day this state built",
                ));
            }
            return Ok(Built::AlreadyBuilt(d));
        }
    }
    let mut pending: Pending = read_json(&pending_path)?.unwrap_or_default();
    let mut save = |p: &Pending| -> Result<(), ReadError> { save_json(&pending_path, p) };
    let Some(d) = next_day(reader, &c, &reviewed, &tool, o.day, &mut pending, &mut save).await?
    else {
        return Ok(Built::NothingNew);
    };
    match existing {
        Some(e) if e == d.day => same_folder(&o.out, &d)?,
        Some(e) => {
            return Err(io(
                &o.out,
                format!(
                    "holds {}, but the next day is {}",
                    crate::early::day_text(e),
                    crate::early::day_text(d.day)
                ),
            ))
        }
        None => write_folder(&o.out, &d)?,
    }
    let before = o.state.join("before");
    std::fs::create_dir_all(&before).map_err(|e| io(&before, e))?;
    let snap = before.join(format!("{}.json", crate::early::day_text(d.day)));
    save_json(&snap, &tool)?;
    save_json(&state_path, &d.after)?;
    if pending_path.exists() {
        std::fs::remove_file(&pending_path).map_err(|e| io(&pending_path, e))?;
    }
    Ok(Built::Day {
        day: d.day,
        rows: d.csv.lines().count().saturating_sub(1),
        skipped: d.skipped,
        empty_days: d.empty_days,
    })
}

/// What [`verify`] checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The day.
    pub day: u32,
    /// Rows, `day_close` included.
    pub rows: usize,
    /// Shard roots compared with the folder's `manifest.bin` (0: none built).
    pub shards: usize,
    /// Shard roots compared with `exclusions/manifest.bin` (0: none built).
    pub exclusion_shards: usize,
}

/// Compare the last shard entries of the manifest at `m` (when built) with
/// the shards `csv` cuts into under `schema`; the number compared.
fn same_roots(m: &Path, schema: &onyx_bulk::Schema, csv: &str) -> Result<usize, ReadError> {
    if !m.exists() {
        return Ok(0);
    }
    let bytes = std::fs::read(m).map_err(|e| io(m, e))?;
    let man = onyx_bulk::Manifest::from_bytes(&bytes).map_err(|e| io(m, e))?;
    let files = shard_files_of(schema, csv, onyx_bulk::shard::DEFAULT_TARGET_SHARD_BYTES)?;
    if files.len() > man.shards.len() {
        return Err(io(m, "fewer shards than the day's rows make"));
    }
    let tail = &man.shards[man.shards.len() - files.len()..];
    for (f, e) in files.iter().zip(tail) {
        let ch = onyx_bulk::shard::check_shard(schema, f).map_err(|e| io(m, e))?;
        if ch.summary.root != e.root {
            return Err(io(
                m,
                format!("shard {} root differs from the chain's rows", e.index),
            ));
        }
    }
    Ok(files.len())
}

/// Rebuild the day of version folder `dir` from the chain (from the deploy
/// blocks, or from `state_before`, the state the build saved before that
/// day) and compare: `data.csv`, `day.json` (with the hash of the join's
/// state after the day) and the exclusions rows byte for byte, and, when the
/// folder holds a built `manifest.bin` (or `exclusions/manifest.bin`), the
/// root of each shard the CSV cuts into (8 MiB shards, `onyx-publish
/// build`'s default) with the manifest's last shard entries.
pub async fn verify<R: JsonRpc + ?Sized>(
    reader: &Reader<'_, R>,
    contracts: &Path,
    exclusions: &Path,
    dir: &Path,
    state_before: Option<&Path>,
) -> Result<Verified, ReadError> {
    let c = Contracts::load(contracts)?;
    let b = std::fs::read(exclusions).map_err(|e| io(exclusions, e))?;
    let reviewed = ReviewedFile::parse(&b)?;
    let (want, _) =
        folder_day(dir)?.ok_or_else(|| io(dir, "not a version folder (no day.json)"))?;
    let mut tool = match state_before {
        Some(p) => read_json(p)?.ok_or_else(|| io(p, "no such state"))?,
        None => ToolState::new(&c),
    };
    loop {
        let mut pending = Pending::default();
        let mut keep = |_: &Pending| Ok(());
        let d = next_day(reader, &c, &reviewed, &tool, None, &mut pending, &mut keep)
            .await?
            .ok_or_else(|| io(dir, "the chain has no such complete day yet"))?;
        if d.day < want {
            tool = d.after;
            continue;
        }
        if d.day > want {
            return Err(io(dir, "the state starts after this folder's day"));
        }
        same_folder(dir, &d)?;
        let shards = same_roots(&dir.join("manifest.bin"), &record_schema(), &d.csv)?;
        let exclusion_shards = if d.exclusion_rows.is_empty() {
            0
        } else {
            same_roots(
                &dir.join("exclusions").join("manifest.bin"),
                &exclusions_schema(),
                &exclusions_csv(&d.exclusion_rows)?,
            )?
        };
        return Ok(Verified {
            day: d.day,
            rows: d.csv.lines().count().saturating_sub(1),
            shards,
            exclusion_shards,
        });
    }
}

#[cfg(test)]
#[path = "early_chain_tests.rs"]
mod tests;
