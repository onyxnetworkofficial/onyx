//! The storage network on a node (ADR-0060, docs/spec/storage-network.md):
//! the chain view of `OnyxStorage` ([`StorageChain`], [`Snapshot`]), the
//! storage-node loop that fetches, checks, serves and proves the shards
//! placed on this node ([`StorageNode`]), and the client read that finds a
//! shard's replicas from the placement and fails over between them
//! ([`read_rows`]). Test network only.
//!
//! The node never trusts a peer: a manifest is taken only when its root is
//! the deal's (the contract bound that root to the publisher's anchor), and
//! every shard byte is checked against that manifest's entry by
//! [`BulkStore::put_shard`] (the same checks as a publisher's upload).

use crate::bulk_serve::{BulkStore, RemovalsMode};
use crate::roots::JsonRpc;
use crate::x402::BoxFuture;
use onyx_bulk::manifest::Manifest;
use onyx_bulk::wire::{hex0x, hex_field};
use onyx_core::{Hash32, SignedReport};
use onyx_storage::abi::{self, events, DealInfo, NodeInfo};
use onyx_storage::challenge;
use onyx_storage::ids::{set_hash, shard_key, slot_shard, NodeId};
use onyx_storage::placement::{
    challenged, holders, holders_at, ranked, Exclusions, Jailed, WHOLE_DEAL,
};
use onyx_storage::repair::DealPlace;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Most shard fetches one tick starts (repair is paced, THREATS STORAGE-9).
pub const DEFAULT_FETCHES_PER_TICK: usize = 8;
/// Largest manifest answer a peer may send (hex doubles the 8 MiB manifest).
pub const MAX_MANIFEST_ANSWER: usize = crate::bulk_serve::MAX_MANIFEST_BODY;
/// Blocks per `eth_getLogs` call: sepolia.base.org answers HTTP 413 above
/// 200 (the indexer's default span; ROADMAP S15, found on the first live
/// deploy 2026-11-01).
pub const LOG_RANGE: u64 = crate::indexer::DEFAULT_SPAN;
/// The format of a reader's log cache file ([`StorageChain::with_log_file`],
/// THREATS STORAGE-RESTART-1); a file of another version is ignored. 2 adds
/// the chain id, the events and the fold rules (review STORAGE-RESTART-R1 M2)
/// and anchors every file at its last block (R1 L1).
pub const LOG_FILE_VERSION: u64 = 2;
/// What the kept logs mean: which logs are kept and how `Answered` logs fold
/// into passes (`fold_logs`). Raise it with any change to either: a file of
/// another value is ignored (review STORAGE-RESTART-R1 M2).
pub const LOG_FOLD_VERSION: u64 = 1;
/// Ranges the kept logs move between two saves of the log cache file when
/// no kept log changed (THREATS STORAGE-RESTART-1, review STORAGE-RESTART-R1
/// L2). A save writes the whole file and syncs it twice: a catch-up of
/// 14,900 ranges saves about 300 times, a quiet node about every 10,000
/// blocks, and a restart reads at most 49 ranges again. A pass that changed
/// the kept logs (a new event) saves when it ends.
pub const LOG_SAVE_EVERY_RANGES: usize = 50;
/// Blocks of [`LOG_SAVE_EVERY_RANGES`] ranges.
const LOG_SAVE_BLOCKS: u64 = LOG_SAVE_EVERY_RANGES as u64 * LOG_RANGE;
/// Largest log cache file read or written (review STORAGE-RESTART-R1 L2):
/// 32 MiB, far above a testnet node's (a few KiB) and well inside a storage
/// unit's 512 MiB of memory once decoded. A larger file is ignored, and a
/// cache that would be larger is not saved (logged).
pub const MAX_LOG_FILE_BYTES: u64 = 32 << 20;
/// `exclusionFrom` calls one check of the kept logs makes at most (review
/// STORAGE-RESTART-R1 M1); with more (deal, shard, node) to check, the next
/// check goes on where this one stopped.
pub const EXCL_CHECK_CALLS: usize = 64;
/// Seconds between two checks of the kept logs against `exclusionFrom`
/// (about a day); the first is on the first pass after start.
pub const EXCL_CHECK_EVERY_SECS: u64 = 86_400;
/// Seconds before a check that got no answer is tried again.
pub const EXCL_CHECK_RETRY_SECS: u64 = 600;
/// The events a snapshot reads: one `eth_getLogs` call per range asks for
/// all of them (topic 0 is any of these), not one call per event. `Answered`
/// gives the last pass per shard, which the decline notice reads (ADR-0060
/// amendment 2 item 2).
const SNAPSHOT_EVENTS: [&str; 7] = [
    events::NODE_SET,
    events::REGISTERED,
    events::ACTIVATED,
    events::SLASHED,
    events::DECLINED,
    events::ANSWERED,
    events::DEAL_CREATED,
];
/// Snapshots are rebuilt from scratch at most this many times when the chain's
/// counts disagree with the logs read (review STORAGE-R3-1).
const SNAPSHOT_TRIES: usize = 2;
/// Failed passes in the current epoch, after a failure in an earlier epoch,
/// before the node declines data no source serves (THREATS STORAGE-16; a
/// short outage at an epoch boundary never gives data up, review STORAGE-R2-4).
pub const DECLINE_AFTER_FAILED_PASSES: u32 = 3;
/// Time one pass spends starting shard fetches (answers come first).
pub const FETCH_BUDGET: Duration = Duration::from_secs(300);
/// Largest leaf answer a learn pass reads (ADR-0059 amendment 4 item 5a
/// slice 2a: a leaf, two neighbours and three paths are a few KiB).
pub const LEARN_ANSWER_BYTES: usize = 64 << 10;
/// Deadline of one learn call, whatever the transport's own.
pub const LEARN_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Datasets a learn pass asks about (they take turns).
pub const LEARN_PER_PASS: usize = 4;
/// Every `storage.lastErrorClass` on `/v1/health` ([`error_class`],
/// review OBS-STORAGE-R1 3: a class, never error text).
pub const ERROR_CLASSES: [&str; 7] = [
    "rpc_rate_limited",
    "rpc_unreachable",
    "rpc_error",
    "contract_version",
    "snapshot_disagrees",
    "store",
    "other",
];

/// HTTP GET transport (shard files, manifests and rows from peers).
pub trait HttpGet: Send + Sync {
    /// GET `url`; the body at most `max` bytes. Returns (status, body).
    fn get<'a>(&'a self, url: &'a str, max: usize)
        -> BoxFuture<'a, Result<(u16, Vec<u8>), String>>;
}

/// Sends `OnyxStorage` transactions from the node's operator key.
pub trait TxSend: Send + Sync {
    /// The operator address.
    fn address(&self) -> [u8; 20];
    /// Send `data` to `to`, wait for the receipt; the transaction hash.
    fn send<'a>(&'a self, to: &'a [u8; 20], data: Vec<u8>)
        -> BoxFuture<'a, Result<String, String>>;
}

impl<T: JsonRpc + 'static> TxSend for crate::evm_tx::Sender<T> {
    fn address(&self) -> [u8; 20] {
        crate::evm_tx::Sender::address(self)
    }

    fn send<'a>(
        &'a self,
        to: &'a [u8; 20],
        data: Vec<u8>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            crate::evm_tx::Sender::send(self, to, &data, 0, Duration::from_secs(120))
                .await
                .map(|m| m.hash)
                .map_err(|e| e.to_string())
        })
    }
}

fn qty(v: &Value) -> Result<u64, String> {
    let s = v.as_str().ok_or("not a quantity")?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn hexb(v: &Value) -> Result<Vec<u8>, String> {
    let s = v.as_str().ok_or("not a hex string")?;
    hex::decode(s.trim_start_matches("0x")).map_err(|e| e.to_string())
}

fn h32(v: &Value) -> Result<Hash32, String> {
    let b: [u8; 32] = hexb(v)?
        .try_into()
        .map_err(|_| "not 32 bytes".to_string())?;
    Ok(Hash32(b))
}

/// One log of the contract.
#[derive(Clone, Debug)]
pub struct Log {
    /// Topics (the first is the event's).
    pub topics: Vec<Hash32>,
    /// ABI data.
    pub data: Vec<u8>,
    /// Block.
    pub block: u64,
}

/// The contract's parameters (`params()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainParams {
    /// Blocks per epoch.
    pub epoch_blocks: u64,
    /// The money parameters.
    pub money: onyx_storage::ledger::Params,
    /// Deploy block.
    pub genesis: u64,
}

/// `OnyxStorage` read over JSON-RPC at `latest`.
pub struct StorageChain {
    rpc: Arc<dyn JsonRpc>,
    contract: [u8; 20],
    from_block: u64,
    /// What a pass need not read again (review STORAGE-R1-8).
    cache: Arc<Mutex<ChainCache>>,
    /// Where the kept logs outlive a restart ([`StorageChain::with_log_file`],
    /// THREATS STORAGE-RESTART-1); none: in memory only.
    file: Option<Arc<LogFile>>,
    /// JSON-RPC calls that got no answer (HTTP 429, a timeout): a pass that
    /// failed only for that keeps its logs (THREATS STORAGE-S15-4).
    rpc_errors: AtomicUsize,
}

/// Logs of safe blocks, read once (review STORAGE-R1-8): the snapshot
/// events' first block not kept yet and the logs before it; the safe block
/// the kept logs end at, with its hash (a different hash there later drops
/// them, review STORAGE-R3-1); deals seen closed (they never reopen). Safe,
/// not finalized: on Base Sepolia finalized lags the head by about 4,600
/// blocks and safe by about 400, so a pass reads 2 ranges of new blocks, not
/// 23 (THREATS STORAGE-S15-1).
#[derive(Default)]
struct ChainCache {
    /// The next block to read, the kept logs (every snapshot event but
    /// `Answered`) and the passes folded from `Answered` logs.
    logs: Option<(u64, Vec<Log>, Passes)>,
    /// A kept block and its hash: the last kept block once a pass read up to
    /// the safe block, and in every save (review STORAGE-RESTART-R1 L1).
    anchor: Option<(u64, Hash32)>,
    closed: BTreeSet<u64>,
    /// The kept logs came from the log file of this chain id and are not
    /// checked against the chain yet (THREATS STORAGE-RESTART-1, review
    /// STORAGE-RESTART-R1 M2, M3).
    from_file: Option<u64>,
    /// The chain id the RPC serves (asked once, with a log file only).
    chain_id: Option<u64>,
    /// Bumped when the kept logs or passes change.
    changes: u64,
    /// Bumped when the kept logs are dropped: a save begun before is not
    /// written (review STORAGE-RESTART-R1 L3).
    generation: u64,
    /// What the log file holds.
    saved: Option<Saved>,
    /// What the log file last did (loaded, ignored or dropped, and why); the
    /// node log has it too.
    note: Option<String>,
}

/// What the log file holds: the generation, next block and changes of the
/// kept logs it was written from.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Saved {
    generation: u64,
    next: u64,
    changes: u64,
}

impl ChainCache {
    /// The block the last save of this generation reached (the deploy block
    /// when none).
    fn saved_next(&self, from_block: u64) -> u64 {
        match self.saved {
            Some(s) if s.generation == self.generation => s.next,
            _ => from_block,
        }
    }

    /// The kept logs moved [`LOG_SAVE_EVERY_RANGES`] ranges since the last
    /// save (checked against the chain, with its chain id known).
    fn moved_far(&self, from_block: u64) -> bool {
        match (&self.logs, self.from_file, self.chain_id) {
            (Some((next, ..)), None, Some(_)) => {
                *next >= self.saved_next(from_block).saturating_add(LOG_SAVE_BLOCKS)
            }
            _ => false,
        }
    }

    /// A save is due: the kept logs changed or moved far since the last one,
    /// or none of them is saved yet (review STORAGE-RESTART-R1 L2).
    fn save_due(&self, from_block: u64) -> bool {
        let changed = match self.saved {
            Some(s) if s.generation == self.generation => s.changes != self.changes,
            _ => true,
        };
        match (&self.logs, self.from_file, self.chain_id) {
            (Some((next, ..)), None, Some(_)) => {
                *next > from_block && (changed || self.moved_far(from_block))
            }
            _ => false,
        }
    }
}

/// A reader's log cache file (THREATS STORAGE-RESTART-1).
struct LogFile {
    path: PathBuf,
    /// Held while the file is written or removed: a removal waits for a
    /// write under way, and a write checks the generation under it.
    lock: Mutex<()>,
}

/// The log cache file of `contract` in a node's state folder
/// (`ONYX_STATE_DIR`, one process per folder; THREATS STORAGE-RESTART-1):
/// named after the contract, so another contract's reader never opens it.
pub fn log_cache_file(state_dir: &Path, contract: &[u8; 20]) -> PathBuf {
    state_dir.join(format!("storage-logs-{}.json", hex0x(contract)))
}

/// What a log cache file holds: the chain id, the next block to read, the
/// kept logs, the passes and the anchor (always the last kept block, review
/// STORAGE-RESTART-R1 L1).
struct FileLogs {
    chain_id: u64,
    next: u64,
    logs: Vec<Log>,
    passes: Passes,
    anchor: (u64, Hash32),
}

/// The events the kept logs are of: Keccak-256 of [`SNAPSHOT_EVENTS`]'
/// topics in order (review STORAGE-RESTART-R1 M2).
fn events_hash() -> String {
    let mut b = Vec::new();
    for e in SNAPSHOT_EVENTS {
        b.extend_from_slice(&abi::topic(e).0);
    }
    onyx_core::hash::keccak256(&b).to_string()
}

/// The check of a log cache file: Keccak-256 of the compact JSON of every
/// field but `check` (a file changed after it was written does not decode).
fn log_file_check(body: &Value) -> String {
    onyx_core::hash::keccak256(&serde_json::to_vec(body).unwrap_or_default()).to_string()
}

/// The log cache file's bytes: compact JSON with its check.
fn encode_log_file(contract: &[u8; 20], from_block: u64, k: &FileLogs) -> Vec<u8> {
    let logs: Vec<Value> = k
        .logs
        .iter()
        .map(|l| {
            json!({
                "block": l.block,
                "topics": l.topics.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
                "data": hex0x(&l.data),
            })
        })
        .collect();
    let passes: Vec<Value> = k
        .passes
        .iter()
        .map(|((deal, index, node), epoch)| json!([deal, index, node.to_string(), epoch]))
        .collect();
    let mut body = json!({
        "onyxStorageLogs": LOG_FILE_VERSION,
        "chainId": k.chain_id,
        "contract": hex0x(contract),
        "fromBlock": from_block,
        "events": events_hash(),
        "fold": LOG_FOLD_VERSION,
        "next": k.next,
        "anchor": [k.anchor.0, k.anchor.1.to_string()],
        "logs": logs,
        "passes": passes,
    });
    let check = log_file_check(&body);
    body["check"] = json!(check);
    let mut bytes = serde_json::to_vec(&body).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Decode a log cache file whole for `contract` read from `from_block`, or
/// say why it is not used: nothing in it is used unless all of it checks.
fn decode_log_file(bytes: &[u8], contract: &[u8; 20], from_block: u64) -> Result<FileLogs, String> {
    let mut v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("it does not decode: {e}"))?;
    if v["onyxStorageLogs"] != json!(LOG_FILE_VERSION) {
        return Err(format!(
            "version {}, this node reads version {LOG_FILE_VERSION}",
            v["onyxStorageLogs"]
        ));
    }
    let check = v
        .as_object_mut()
        .and_then(|o| o.remove("check"))
        .ok_or("it has no check")?;
    if check.as_str() != Some(log_file_check(&v).as_str()) {
        return Err("its check does not match: damaged".into());
    }
    if hexb(&v["contract"]).ok().as_deref() != Some(&contract[..]) {
        return Err(format!(
            "written for another contract ({}), not {}",
            v["contract"],
            hex0x(contract)
        ));
    }
    if v["fromBlock"] != json!(from_block) {
        return Err(format!(
            "written for deploy block {}, this node reads from block {from_block}",
            v["fromBlock"]
        ));
    }
    if v["events"] != json!(events_hash()) {
        return Err("written for another set of events".into());
    }
    if v["fold"] != json!(LOG_FOLD_VERSION) {
        return Err(format!(
            "written with fold rules {}, this node folds with {LOG_FOLD_VERSION}",
            v["fold"]
        ));
    }
    let num = |x: &Value, what: &str| x.as_u64().ok_or_else(|| format!("{what} is not a number"));
    let chain_id = num(&v["chainId"], "chainId")?;
    let next = num(&v["next"], "next")?;
    if next <= from_block {
        return Err(format!("next block {next} is not after the deploy block"));
    }
    let anchor = (
        num(&v["anchor"][0], "the anchor's block")?,
        h32(&v["anchor"][1])?,
    );
    if anchor.0 != next - 1 {
        return Err(format!(
            "its anchor {} is not its last block {}",
            anchor.0,
            next - 1
        ));
    }
    let mut logs = Vec::new();
    for l in v["logs"].as_array().ok_or("logs is not a list")? {
        let block = num(&l["block"], "a log's block")?;
        if block < from_block || block >= next {
            return Err(format!("a log at block {block} is outside the blocks read"));
        }
        let topics = l["topics"]
            .as_array()
            .ok_or("a log's topics is not a list")?
            .iter()
            .map(h32)
            .collect::<Result<Vec<_>, _>>()?;
        logs.push(Log {
            topics,
            data: hexb(&l["data"])?,
            block,
        });
    }
    let mut passes = Passes::new();
    for p in v["passes"].as_array().ok_or("passes is not a list")? {
        let index = u32::try_from(num(&p[1], "a pass's shard")?)
            .map_err(|_| "a pass's shard is not a uint32".to_string())?;
        let key = (num(&p[0], "a pass's deal")?, index, NodeId(h32(&p[2])?));
        passes.insert(key, num(&p[3], "a pass's epoch")?);
    }
    Ok(FileLogs {
        chain_id,
        next,
        logs,
        passes,
        anchor,
    })
}

/// A poisoned lock as an I/O error.
fn poisoned<T>(_: T) -> std::io::Error {
    std::io::Error::other("lock")
}

/// Read at most [`MAX_LOG_FILE_BYTES`] of `path` (larger: an error).
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_LOG_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOG_FILE_BYTES {
        return Err(std::io::Error::other(format!(
            "larger than the {} MiB cap",
            MAX_LOG_FILE_BYTES >> 20
        )));
    }
    Ok(bytes)
}

/// Write `path` whole: a temporary file of this process and write only
/// (review STORAGE-RESTART-R1 L3: two writers never share one), synced,
/// renamed over `path`, then the folder synced.
fn write_log_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static TEMP: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?
        .to_string_lossy();
    let tmp = dir.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    std::fs::File::open(dir)?.sync_all()
}

impl LogFile {
    /// Write the kept logs when a save is due ([`ChainCache::save_due`]) and
    /// they are anchored at their last block (review STORAGE-RESTART-R1 L1).
    /// They are copied under the cache's lock and encoded outside it (R1
    /// L2); a generation dropped meanwhile is not written (R1 L3).
    fn save(
        &self,
        cache: &Mutex<ChainCache>,
        contract: &[u8; 20],
        from_block: u64,
    ) -> std::io::Result<()> {
        match Self::prepare(cache, contract, from_block)? {
            Some((mark, bytes)) => self.commit(cache, mark, &bytes),
            None => Ok(()),
        }
    }

    /// The bytes of a due save and what they hold (none: no save due).
    fn prepare(
        cache: &Mutex<ChainCache>,
        contract: &[u8; 20],
        from_block: u64,
    ) -> std::io::Result<Option<(Saved, Vec<u8>)>> {
        let (mark, kept) = {
            let c = cache.lock().map_err(poisoned)?;
            let (Some((next, logs, passes)), Some(anchor), Some(chain_id)) =
                (&c.logs, c.anchor, c.chain_id)
            else {
                return Ok(None);
            };
            if !c.save_due(from_block) || anchor.0.saturating_add(1) != *next {
                return Ok(None);
            }
            let mark = Saved {
                generation: c.generation,
                next: *next,
                changes: c.changes,
            };
            let kept = FileLogs {
                chain_id,
                next: *next,
                logs: logs.clone(),
                passes: passes.clone(),
                anchor,
            };
            (mark, kept)
        };
        let bytes = encode_log_file(contract, from_block, &kept);
        if bytes.len() as u64 > MAX_LOG_FILE_BYTES {
            return Err(std::io::Error::other(format!(
                "{} bytes, above the {} MiB cap (a restart reads every log again)",
                bytes.len(),
                MAX_LOG_FILE_BYTES >> 20
            )));
        }
        Ok(Some((mark, bytes)))
    }

    /// Write `bytes` (holding `mark`) unless the kept logs were dropped since
    /// they were copied.
    fn commit(&self, cache: &Mutex<ChainCache>, mark: Saved, bytes: &[u8]) -> std::io::Result<()> {
        let _held = self.lock.lock().map_err(poisoned)?;
        if cache.lock().map_err(poisoned)?.generation != mark.generation {
            return Ok(());
        }
        write_log_file(&self.path, bytes)?;
        if let Ok(mut c) = cache.lock() {
            if c.generation == mark.generation {
                c.saved = Some(mark);
            }
        }
        Ok(())
    }

    /// Remove the file and sync its folder (none: done), after any write
    /// under way.
    fn remove(&self) -> std::io::Result<()> {
        let _held = self
            .lock
            .lock()
            .map_err(|_| std::io::Error::other("lock"))?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => match self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
                Some(dir) => std::fs::File::open(dir)?.sync_all(),
                None => Ok(()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The last epoch each node passed a check on each shard: (deal, shard, node)
/// -> epoch, folded from `Answered` logs as they are read. Raw `Answered` logs
/// are never kept: there are a few per deal every epoch, and keeping them made
/// the cache grow without bound and a first sync quadratic (review
/// STORAGE-S16-R7 M1).
type Passes = BTreeMap<(u64, u32, NodeId), u64>;

/// Split `logs` into the ones to keep and the passes they hold (`Answered`
/// with `passed`), in chain order; true when either changed. A change here
/// changes what a log cache file means: raise [`LOG_FOLD_VERSION`].
fn fold_logs(logs: Vec<Log>, keep: &mut Vec<Log>, passes: &mut Passes) -> Result<bool, String> {
    let answered = abi::topic(events::ANSWERED);
    let mut changed = false;
    for l in logs {
        if l.topics.first() != Some(&answered) {
            keep.push(l);
            changed = true;
            continue;
        }
        // Data: slot, shard index, passed, paid.
        if abi::word_uint(&l.data, 2).map_err(|e| e.to_string())? == 0 {
            continue;
        }
        let deal = topic_u64(&l, 1, "Answered topic")?;
        let epoch = topic_u64(&l, 2, "Answered topic")?;
        let node = NodeId(*l.topics.get(3).ok_or("Answered topic")?);
        let index = word_u32(&l, 1, "Answered index")?;
        match passes.get_mut(&(deal, index, node)) {
            Some(e) if *e >= epoch => {}
            Some(e) => {
                *e = epoch;
                changed = true;
            }
            None => {
                passes.insert((deal, index, node), epoch);
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// The raw records behind `exclusionFrom` (ADR-0060 amendment 2 item 2) in
/// `logs` and `passes`: slashes, declines and passes, in any order.
fn exclusions_of<'a>(
    logs: impl Iterator<Item = &'a Log>,
    passes: &Passes,
) -> Result<Exclusions, String> {
    let (slashed, declined) = (abi::topic(events::SLASHED), abi::topic(events::DECLINED));
    let err = |e: abi::AbiError| e.to_string();
    let mut excl = Exclusions::new();
    for l in logs {
        let t = l.topics.first();
        if t == Some(&slashed) {
            let node = NodeId(*l.topics.get(1).ok_or("Slashed topic")?);
            let deal = topic_u64(l, 2, "Slashed topic")?;
            let index = word_u32(l, 2, "Slashed index")?;
            let from = abi::word_u64(&l.data, 4).map_err(err)?;
            excl.slash(deal, index, node, from);
        } else if t == Some(&declined) {
            let node = NodeId(*l.topics.get(1).ok_or("Declined topic")?);
            let deal = topic_u64(l, 2, "Declined topic")?;
            let index = word_u32(l, 0, "Declined index")?;
            let from = abi::word_u64(&l.data, 1).map_err(err)?;
            excl.decline(deal, index, node, from);
        }
    }
    for ((deal, index, node), epoch) in passes {
        excl.pass(*deal, *index, *node, *epoch);
    }
    Ok(excl)
}

/// Topic `i` of a log as a `uint64` (the low 8 bytes).
fn topic_u64(l: &Log, i: usize, what: &'static str) -> Result<u64, String> {
    Ok(u64::from_be_bytes(
        l.topics.get(i).ok_or(what)?.0[24..]
            .try_into()
            .map_err(|_| "topic")?,
    ))
}

/// Data word `i` of a log as a `uint32`.
fn word_u32(l: &Log, i: usize, what: &'static str) -> Result<u32, String> {
    u32::try_from(abi::word_u64(&l.data, i).map_err(|e| e.to_string())?)
        .map_err(|_| what.to_string())
}

fn ok_word(r: Result<u128, abi::AbiError>) -> Result<u128, String> {
    r.map_err(|e| e.to_string())
}

/// The rules this node follows (`OnyxStorage.RULES_VERSION`): 3 is ADR-0060
/// amendment 2 item 2 (the decline notice, `Answered` with the shard) on top
/// of amendment 1 (a row for each holder, then `ROW_VERSION` 2).
pub const RULES_VERSION: u8 = 3;

/// `books()`'s nine words. A contract from before ADR-0060 amendment 1 (the
/// S15 deployment) returns eight: no rewards, so `rewarded` is 0.
fn decode_books(d: &[u8]) -> Result<[u128; 9], String> {
    let n = match d.len() {
        256 => 8,
        _ => 9,
    };
    let mut out = [0u128; 9];
    for (i, o) in out.iter_mut().enumerate().take(n) {
        *o = ok_word(abi::word_uint(d, i))?;
    }
    Ok(out)
}

impl StorageChain {
    /// A reader of `contract`, scanning logs from `from_block` (its deploy block).
    pub fn new(rpc: Arc<dyn JsonRpc>, contract: [u8; 20], from_block: u64) -> Self {
        StorageChain {
            rpc,
            contract,
            from_block,
            cache: Arc::new(Mutex::new(ChainCache::default())),
            file: None,
            rpc_errors: AtomicUsize::new(0),
        }
    }

    /// Keep the logs of safe blocks in `path` too, so a restart reads only
    /// the blocks since (THREATS STORAGE-RESTART-1; [`log_cache_file`]), and
    /// start from what the file holds when it is for this contract, deploy
    /// block, events and fold rules, of [`LOG_FILE_VERSION`], at most
    /// [`MAX_LOG_FILE_BYTES`] and decodes whole. The first pass checks it
    /// against the chain (its chain id and its last block's hash) before
    /// using it, and the node's first pass checks it against the contract's
    /// `exclusionFrom` (review STORAGE-RESTART-R1). A file that is not used
    /// is logged and the logs are read again from the deploy block: it never
    /// fails the start.
    pub fn with_log_file(mut self, path: PathBuf) -> Self {
        let from = self.from_block;
        let what = match read_capped(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                format!("none yet; every log is read from block {from}")
            }
            Err(e) => {
                format!("ignored (it does not read: {e}); every log is read from block {from}")
            }
            Ok(bytes) => match decode_log_file(&bytes, &self.contract, from) {
                Ok(k) => {
                    let what = format!(
                        "loaded ({} logs, {} passes, read through block {}); checked against the chain on the first pass",
                        k.logs.len(),
                        k.passes.len(),
                        k.anchor.0
                    );
                    if let Ok(mut c) = self.cache.lock() {
                        c.saved = Some(Saved {
                            generation: c.generation,
                            next: k.next,
                            changes: c.changes,
                        });
                        c.logs = Some((k.next, k.logs, k.passes));
                        c.anchor = Some(k.anchor);
                        c.from_file = Some(k.chain_id);
                    }
                    what
                }
                Err(why) => format!("ignored ({why}); every log is read again from block {from}"),
            },
        };
        self.file = Some(Arc::new(LogFile {
            path,
            lock: Mutex::new(()),
        }));
        self.note(what);
        self
    }

    /// Log what the log cache file did, and keep it for [`Self::log_note`].
    fn note(&self, what: String) {
        if let Some(f) = &self.file {
            eprintln!("onyx-node: storage: log cache {}: {what}", f.path.display());
        }
        if let Ok(mut c) = self.cache.lock() {
            c.note = Some(what);
        }
    }

    /// What the log cache file last did: loaded, ignored or dropped, and why
    /// (none without a file).
    pub fn log_note(&self) -> Option<String> {
        self.cache.lock().ok().and_then(|c| c.note.clone())
    }

    /// The last block the kept logs cover: `logsReadThrough` on `/v1/health`
    /// (THREATS STORAGE-RESTART-1, review STORAGE-RESTART-R1). `None` when
    /// none are kept, or while logs loaded from the file are not checked
    /// against the chain yet.
    pub fn logs_read_through(&self) -> Option<u64> {
        let c = self.cache.lock().ok()?;
        if c.from_file.is_some() {
            return None;
        }
        c.logs.as_ref().map(|l| l.0.saturating_sub(1))
    }

    /// Save the kept logs to the log cache file when a save is due, on a
    /// blocking thread so the runtime never waits on the disk. A save is
    /// anchored at its last block: when the kept anchor is older, that
    /// block's hash is asked first (no answer: no save now; review
    /// STORAGE-RESTART-R1 L1). A failed save is logged; the logs stay in
    /// memory and a later save writes them.
    async fn save_logs(&self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        let from = self.from_block;
        let unanchored = self.cache.lock().ok().and_then(|c| {
            let next = c.logs.as_ref()?.0;
            (c.save_due(from) && c.anchor.map(|a| a.0.saturating_add(1)) != Some(next))
                .then_some(next - 1)
        });
        if let Some(n) = unanchored {
            let Some(h) = self.hash_of(n).await else {
                return;
            };
            if let Ok(mut c) = self.cache.lock() {
                if c.logs.as_ref().map(|l| l.0) == Some(n + 1) {
                    c.anchor = Some((n, h));
                }
            }
        }
        let (f, cache, contract) = (file.clone(), self.cache.clone(), self.contract);
        let r = tokio::task::spawn_blocking(move || f.save(&cache, &contract, from))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r);
        if let Err(e) = r {
            eprintln!(
                "onyx-node: storage: log cache {}: not saved: {e}",
                file.path.display()
            );
        }
    }

    /// The hash of block `n`, asked once and not counted as a failed call
    /// (a save's anchor; `None`: no save now).
    async fn hash_of(&self, n: u64) -> Option<Hash32> {
        let b = self
            .rpc
            .call("eth_getBlockByNumber", json!([format!("0x{n:x}"), false]))
            .await
            .ok()?;
        if qty(&b["number"]).ok()? != n {
            return None;
        }
        h32(&b["hash"]).ok()
    }

    /// One JSON-RPC call, counted in `rpc_errors` when it fails.
    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, String> {
        let r = self.rpc.call(method, params).await;
        if r.is_err() {
            self.rpc_errors.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    /// The contract address.
    pub fn contract(&self) -> [u8; 20] {
        self.contract
    }

    /// `eth_call` at `latest`.
    pub async fn call(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let r = self
            .rpc_call(
                "eth_call",
                json!([{"to": hex0x(&self.contract), "data": hex0x(data)}, "latest"]),
            )
            .await?;
        hexb(&r)
    }

    /// `eth_call` to another contract (the token) at `latest`.
    pub async fn call_to(&self, to: &[u8; 20], data: &[u8]) -> Result<Vec<u8>, String> {
        let r = self
            .rpc_call(
                "eth_call",
                json!([{"to": hex0x(to), "data": hex0x(data)}, "latest"]),
            )
            .await?;
        hexb(&r)
    }

    /// `eth_call` to `to` (this contract or the token) at block `block`, so
    /// several reads describe the same state.
    pub async fn call_to_at(
        &self,
        to: &[u8; 20],
        data: &[u8],
        block: u64,
    ) -> Result<Vec<u8>, String> {
        let r = self
            .rpc_call(
                "eth_call",
                json!([{"to": hex0x(to), "data": hex0x(data)}, format!("0x{block:x}")]),
            )
            .await?;
        hexb(&r)
    }

    /// The head block number.
    pub async fn block_number(&self) -> Result<u64, String> {
        qty(&self.rpc_call("eth_blockNumber", json!([])).await?)
    }

    /// `currentEpoch()`.
    pub async fn current_epoch(&self) -> Result<u64, String> {
        abi::word_u64(&self.call(&abi::current_epoch()).await?, 0).map_err(|e| e.to_string())
    }

    /// `seedOf(e)` (zero: not recorded).
    pub async fn seed_of(&self, e: u64) -> Result<Hash32, String> {
        Ok(Hash32(
            abi::word(&self.call(&abi::seed_of(e)).await?, 0).map_err(|x| x.to_string())?,
        ))
    }

    /// `setHashAt(e)`.
    pub async fn set_hash_at(&self, e: u64) -> Result<Hash32, String> {
        Ok(Hash32(
            abi::word(&self.call(&abi::set_hash_at(e)).await?, 0).map_err(|x| x.to_string())?,
        ))
    }

    /// `epochStart(e)`.
    pub async fn epoch_start(&self, e: u64) -> Result<u64, String> {
        abi::word_u64(&self.call(&abi::epoch_start(e)).await?, 0).map_err(|x| x.to_string())
    }

    /// `dealInfo(id)`.
    pub async fn deal_info(&self, id: u64) -> Result<DealInfo, String> {
        abi::decode_deal(&self.call(&abi::deal_info(id)).await?).map_err(|e| e.to_string())
    }

    /// `nodeInfo(id)`.
    pub async fn node_info(&self, id: &NodeId) -> Result<NodeInfo, String> {
        abi::decode_node(&self.call(&abi::node_info(id)).await?).map_err(|e| e.to_string())
    }

    /// `slotStatus(deal, e, slot, node)`: 0 open, 1 passed, 2 failed, 3 missed.
    pub async fn slot_status(
        &self,
        deal: u64,
        e: u64,
        slot: u32,
        node: &NodeId,
    ) -> Result<u8, String> {
        let v = abi::word_u64(&self.call(&abi::slot_status(deal, e, slot, node)).await?, 0)
            .map_err(|x| x.to_string())?;
        u8::try_from(v).map_err(|_| "status".to_string())
    }

    /// `earned(operator)`.
    pub async fn earned(&self, a: &[u8; 20]) -> Result<u128, String> {
        ok_word(abi::word_uint(&self.call(&abi::earned(a)).await?, 0))
    }

    /// `conserved()`.
    pub async fn conserved(&self) -> Result<bool, String> {
        Ok(ok_word(abi::word_uint(&self.call(&abi::conserved()).await?, 0))? == 1)
    }

    /// `books()`: deposits, bonds in, paid, slashed (fees included),
    /// refunded, bonds out, earnings out, swept, rewarded (ADR-0060
    /// amendment 1), at `latest`.
    pub async fn books(&self) -> Result<[u128; 9], String> {
        decode_books(&self.call(&abi::books()).await?)
    }

    /// `books()` at block `block` (to compare with the logs up to it).
    pub async fn books_at(&self, block: u64) -> Result<[u128; 9], String> {
        let r = self
            .rpc_call(
                "eth_call",
                json!([{"to": hex0x(&self.contract), "data": hex0x(&abi::books())},
                       format!("0x{block:x}")]),
            )
            .await?;
        decode_books(&hexb(&r)?)
    }

    /// The books rebuilt from the contract's money events from its deploy
    /// block to `to` (`onyx_storage::ledger::replay`; one `eth_getLogs` call
    /// per [`LOG_RANGE`] blocks, so for a local chain or a short history).
    pub async fn replay_books(&self, to: u64) -> Result<onyx_storage::ledger::Books, String> {
        let logs = self
            .logs_range(&abi::events::MONEY, self.from_block, to)
            .await?;
        onyx_storage::ledger::replay(
            logs.iter()
                .map(|l| (l.topics.as_slice(), l.data.as_slice())),
        )
        .map_err(|e| e.to_string())
    }

    /// Refuse a contract whose rules are not this node's: an S15 contract
    /// draws the row without the node id, an amendment 1 contract has no
    /// decline notice and another `Answered` event, and neither has
    /// `RULES_VERSION()`. Against them this node's answers would fail or its
    /// view of who is asked would differ (review STORAGE-S16-R6 H1). A
    /// transport error is reported as one (the next tick asks again), not as
    /// an older contract (review STORAGE-S16-R4).
    pub async fn check_version(&self) -> Result<(), String> {
        let d = match self.call(&abi::rules_version()).await {
            Ok(d) => d,
            Err(e) if e.contains("revert") => {
                return Err(format!(
                    "this node follows rules v{RULES_VERSION} (ADR-0060 amendment 2) and the \
                     contract has no RULES_VERSION(), so it is older: exit its node, then \
                     run the node release that matches it ({e})"
                ))
            }
            Err(e) => {
                return Err(format!(
                    "could not read the contract's RULES_VERSION(): {e}"
                ))
            }
        };
        match ok_word(abi::word_uint(&d, 0))? {
            v if v == u128::from(RULES_VERSION) => Ok(()),
            v => Err(format!(
                "the contract checks rules v{v}, this node follows rules v{RULES_VERSION}: run \
                 the node release that matches it"
            )),
        }
    }

    /// `challengedHolders(deal, epoch, slot, nodes)`: the slot's shard and
    /// the holders the contract asks.
    pub async fn challenged_holders(
        &self,
        deal: u64,
        epoch: u64,
        slot: u32,
        nodes: &[NodeId],
    ) -> Result<(u32, Vec<NodeId>), String> {
        let d = self
            .call(&abi::challenged_holders(deal, epoch, slot, nodes))
            .await?;
        abi::decode_challenged_holders(&d).map_err(|e| e.to_string())
    }

    /// `params()`.
    pub async fn params(&self) -> Result<ChainParams, String> {
        let d = self.call(&abi::params()).await?;
        let u = |i| ok_word(abi::word_uint(&d, i));
        Ok(ChainParams {
            epoch_blocks: u(0)? as u64,
            money: onyx_storage::ledger::Params {
                price_per_gib_epoch: u(1)?,
                bond_per_gib: u(2)?,
                min_bond: u(3)?,
                slash_ppm: u(4)? as u32,
                challenges_per_deal: u(5)? as u32,
                max_misses: u(6)? as u32,
            },
            genesis: u(7)? as u64,
        })
    }

    /// Every log of `event` from the deploy block to `to`.
    pub async fn logs(&self, event: &str, to: u64) -> Result<Vec<Log>, String> {
        self.logs_range(&[event], self.from_block, to).await
    }

    /// Every log of `event` from block `from` (never before the deploy
    /// block) to `to`.
    pub async fn logs_from(&self, event: &str, from: u64, to: u64) -> Result<Vec<Log>, String> {
        self.logs_range(&[event], from.max(self.from_block), to)
            .await
    }

    /// The logs of [`SNAPSHOT_EVENTS`] from the deploy block to `to`, in chain
    /// order (`Answered` folded into [`Passes`]), with the blocks up to `safe`
    /// read once and kept; only the newer blocks are read on the next call.
    /// They are kept range by range: a first read cut off by the node (HTTP
    /// 429 from a public endpoint after a few hundred calls) goes on where it
    /// stopped (THREATS STORAGE-S15-4). What is kept stays small (no raw
    /// `Answered`), so keeping it after each range stays cheap; the log
    /// cache file is written each [`LOG_SAVE_EVERY_RANGES`] ranges while
    /// catching up (THREATS STORAGE-RESTART-1) and when [`Self::snapshot`]
    /// ends with a save due. Once read up to the safe block `(safe, hash)`,
    /// the kept logs are anchored there (review STORAGE-RESTART-R1 L1).
    async fn cached_logs(
        &self,
        to: u64,
        (safe, safe_hash): (u64, Hash32),
    ) -> Result<(Vec<Log>, Passes), String> {
        let (mut next, mut out, mut passes) = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.logs.clone())
            .unwrap_or((self.from_block, Vec::new(), Passes::new()));
        let last = safe.min(to);
        let mut read = false;
        while next <= last {
            let end = next.saturating_add(LOG_RANGE - 1).min(last);
            let got = self.logs_range(&SNAPSHOT_EVENTS, next, end).await?;
            let changed = fold_logs(got, &mut out, &mut passes)?;
            next = end + 1;
            read = true;
            let far = match self.cache.lock() {
                Ok(mut c) => {
                    c.logs = Some((next, out.clone(), passes.clone()));
                    if changed {
                        c.changes += 1;
                    }
                    c.moved_far(self.from_block)
                }
                Err(_) => false,
            };
            if far && next <= last {
                self.save_logs().await;
            }
        }
        if read && next == safe.saturating_add(1) {
            if let Ok(mut c) = self.cache.lock() {
                c.anchor = Some((safe, safe_hash));
            }
        }
        if next <= to {
            let got = self.logs_range(&SNAPSHOT_EVENTS, next, to).await?;
            fold_logs(got, &mut out, &mut passes)?;
        }
        Ok((out, passes))
    }

    /// The number and hash of block `tag` (`"safe"` or a `0x` number). A
    /// block the RPC does not know (a backend behind it answers null) is no
    /// answer, counted as a failed call, never a wrong one (review
    /// STORAGE-RESTART-R1 M3).
    async fn block_id(&self, tag: &str) -> Result<(u64, Hash32), String> {
        let b = self
            .rpc_call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        if b.is_null() {
            self.rpc_errors.fetch_add(1, Ordering::Relaxed);
            return Err(format!(
                "eth_getBlockByNumber {tag}: the RPC does not know the block yet"
            ));
        }
        Ok((qty(&b["number"])?, h32(&b["hash"])?))
    }

    /// Drop every kept log (the chain changed under them, or they disagree
    /// with the contract, `why`), and the log cache file with them (THREATS
    /// STORAGE-RESTART-1): a restart never reads them back. The removal runs
    /// on a blocking thread, after any write under way; a save begun before
    /// is not written (review STORAGE-RESTART-R1 L3).
    async fn forget_logs(&self, why: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.logs = None;
            c.anchor = None;
            c.saved = None;
            c.from_file = None;
            c.generation += 1;
        }
        let Some(file) = self.file.clone() else {
            return;
        };
        let f = file.clone();
        let r = tokio::task::spawn_blocking(move || f.remove())
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r);
        self.note(match r {
            Ok(()) => format!(
                "dropped ({why}); file removed, every log is read again from block {}",
                self.from_block
            ),
            Err(e) => format!(
                "dropped ({why}); the file could not be removed ({e}), the next save replaces it"
            ),
        });
    }

    /// The chain id the RPC serves, asked once (with a log file only); logs
    /// loaded from a file of another chain are dropped (review
    /// STORAGE-RESTART-R1 M2).
    async fn check_chain(&self) -> Result<(), String> {
        if self.file.is_none() {
            return Ok(());
        }
        let known = self.cache.lock().ok().and_then(|c| c.chain_id);
        let id = match known {
            Some(id) => id,
            None => {
                let id = qty(&self.rpc_call("eth_chainId", json!([])).await?)?;
                if let Ok(mut c) = self.cache.lock() {
                    c.chain_id = Some(id);
                }
                id
            }
        };
        let file = self.cache.lock().ok().and_then(|c| c.from_file);
        if let Some(f) = file.filter(|f| *f != id) {
            self.forget_logs(&format!("written for chain {f}, the RPC serves chain {id}"))
                .await;
        }
        Ok(())
    }

    /// Logs loaded from the log cache file are used once the chain vouches
    /// for them: every block they cover is at or below its safe block
    /// `safe`, or their anchor's hash matched (`vouched`, the anchor's block)
    /// and the anchor is their last block (THREATS STORAGE-RESTART-1, review
    /// STORAGE-RESTART-R1 M3). Else the RPC is behind the file: the pass ends
    /// as one that got no answer, the logs are kept and checked again next
    /// pass. Only a different hash drops them (the anchor check).
    fn check_loaded(&self, safe: u64, vouched: Option<u64>) -> Result<(), String> {
        let last = {
            let Ok(mut c) = self.cache.lock() else {
                return Ok(());
            };
            if c.from_file.is_none() {
                return Ok(());
            }
            let Some(last) = c.logs.as_ref().map(|l| l.0.saturating_sub(1)) else {
                c.from_file = None;
                return Ok(());
            };
            if last <= safe || vouched.is_some_and(|n| n >= last) {
                c.from_file = None;
                return Ok(());
            }
            last
        };
        self.rpc_errors.fetch_add(1, Ordering::Relaxed);
        Err(format!(
            "the RPC's safe block {safe} is below the kept logs' last block {last}: checked again next pass"
        ))
    }

    /// Check the kept logs against the contract's `exclusionFrom` at their
    /// last block (review STORAGE-RESTART-R1 M1: a dropped `Slashed`,
    /// `Declined` or `Answered` log passes the counts and the anchor). Asked
    /// for every node ranked at or above `me` on each shard of the live deals
    /// of `s` (the nodes that decide whether `me` holds it), at most
    /// [`EXCL_CHECK_CALLS`] calls from turn `turn`'s place in that list. On
    /// a difference the kept logs are dropped and it is returned.
    pub async fn check_exclusions(
        &self,
        s: &Snapshot,
        me: &NodeId,
        turn: usize,
    ) -> Result<Option<String>, String> {
        let Some(((next, logs, passes), anchor)) = self
            .cache
            .lock()
            .ok()
            .and_then(|c| Some((c.logs.clone()?, c.anchor)))
        else {
            return Ok(None);
        };
        let at = next.saturating_sub(1);
        // The state the kept logs end at: by its hash when anchored there
        // (EIP-1898, the same block on every backend), else by number.
        let block = match anchor {
            Some((n, h)) if n == at => json!({ "blockHash": h.to_string() }),
            _ => json!(format!("0x{at:x}")),
        };
        let kept = exclusions_of(logs.iter(), &passes)?;
        let list = s.list_at(s.epoch);
        let list = &list;
        let pairs = move || {
            s.deals.iter().flat_map(move |d| {
                (0..d.info.shard_count).flat_map(move |index| {
                    let order = ranked(&shard_key(&d.info.salt, index), list);
                    let upto = order.iter().position(|n| n == me).map_or(0, |i| i + 1);
                    order.into_iter().take(upto).map(move |n| (d.id, index, n))
                })
            })
        };
        let total = pairs().count();
        if total == 0 {
            return Ok(None);
        }
        let start = turn.wrapping_mul(EXCL_CHECK_CALLS) % total;
        let picked: Vec<_> = pairs()
            .chain(pairs())
            .skip(start)
            .take(EXCL_CHECK_CALLS.min(total))
            .collect();
        for (deal, index, node) in picked {
            let data = abi::exclusion_from(deal, index, &node);
            let r = self
                .rpc_call(
                    "eth_call",
                    json!([{"to": hex0x(&self.contract), "data": hex0x(&data)}, block]),
                )
                .await?;
            let chain = abi::word_u64(&hexb(&r)?, 0).map_err(|e| e.to_string())?;
            let view = kept.from(deal, index, &node).unwrap_or(0);
            if chain != view {
                let why = format!(
                    "exclusionFrom(deal {deal}, shard {index}, node {node}) is {chain} at block {at}, the kept logs give {view}"
                );
                self.forget_logs(&why).await;
                return Ok(Some(why));
            }
        }
        Ok(None)
    }

    /// The logs of any of `events` in blocks `from..=to`, one call per
    /// [`LOG_RANGE`] blocks.
    async fn logs_range(
        &self,
        events: &[&str],
        mut from: u64,
        to: u64,
    ) -> Result<Vec<Log>, String> {
        let wanted: Vec<Hash32> = events.iter().map(|e| abi::topic(e)).collect();
        let any: Vec<String> = wanted.iter().map(|t| t.to_string()).collect();
        let mut out = Vec::new();
        while from <= to {
            let end = (from + LOG_RANGE - 1).min(to);
            let r = self
                .rpc_call(
                    "eth_getLogs",
                    json!([{"address": hex0x(&self.contract), "topics": [any],
                            "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{end:x}")}]),
                )
                .await?;
            for l in r.as_array().ok_or("eth_getLogs: not a list")? {
                // Only logs of the asked range and contract (an RPC that ignores the
                // range never fills the kept logs twice; review STORAGE-R3-1).
                let block = qty(&l["blockNumber"])?;
                if block < from || block > end {
                    continue;
                }
                if let Some(a) = l["address"].as_str() {
                    if !a.eq_ignore_ascii_case(&hex0x(&self.contract)) {
                        continue;
                    }
                }
                let topics = l["topics"]
                    .as_array()
                    .ok_or("log topics")?
                    .iter()
                    .map(h32)
                    .collect::<Result<Vec<_>, _>>()?;
                // Only the asked events (an RPC that ignores the topics never
                // passes another event's log off as one of them).
                if !topics.first().is_some_and(|t| wanted.contains(t)) {
                    continue;
                }
                out.push(Log {
                    topics,
                    data: hexb(&l["data"])?,
                    block,
                });
            }
            from = end + 1;
        }
        Ok(out)
    }

    /// Everything placement, challenges and repair need, read at one head.
    /// Logs of finalized blocks are kept between calls; the snapshot is
    /// checked against the contract's own counts (deals, registrations) and
    /// node-list hash, and rebuilt from scratch when they disagree (an RPC
    /// backend that answered an old range wrongly, or a deep reorg; review
    /// STORAGE-R3-1).
    pub async fn snapshot(&self) -> Result<Snapshot, String> {
        let r = self.snapshot_tries().await;
        // The pass ends: the log cache file gets what is kept now, if it
        // moved (nothing after dropped logs; THREATS STORAGE-RESTART-1).
        self.save_logs().await;
        r
    }

    /// [`Self::snapshot`] without the closing save.
    async fn snapshot_tries(&self) -> Result<Snapshot, String> {
        let mut why = String::new();
        for _ in 0..SNAPSHOT_TRIES {
            let failed = self.rpc_errors.load(Ordering::Relaxed);
            // A failed read (a log naming a deal the contract does not have)
            // also drops the kept logs before the retry. A call the node did
            // not answer (HTTP 429, a timeout) says nothing about them: they
            // stay, and the next pass goes on from them (THREATS STORAGE-S15-4).
            let w = match self.snapshot_once().await {
                Ok(s) => match self.agrees(&s).await? {
                    None => return Ok(s),
                    Some(w) => w,
                },
                Err(e) if self.rpc_errors.load(Ordering::Relaxed) != failed => {
                    return Err(format!("chain read failed: {e}"))
                }
                Err(e) => e,
            };
            self.forget_logs(&w).await;
            why = w;
        }
        Err(format!(
            "chain read failed or disagrees with the contract: {why}"
        ))
    }

    /// `None` when the snapshot's deals, registrations and current node list
    /// match the contract's counts and checkpoint; else what differs.
    async fn agrees(&self, s: &Snapshot) -> Result<Option<String>, String> {
        let word = |r: Vec<u8>| abi::word_u64(&r, 0).map_err(|e| e.to_string());
        let deals = word(self.call(&abi::deal_count()).await?)?;
        if deals != s.deal_logs {
            return Ok(Some(format!(
                "{deals} deals, {} DealCreated logs",
                s.deal_logs
            )));
        }
        let regs = word(
            self.call(abi::selector("registrationCount()").as_ref())
                .await?,
        )?;
        if regs != s.registrations {
            return Ok(Some(format!(
                "{regs} registrations, {} Registered logs",
                s.registrations
            )));
        }
        if set_hash(&s.list_at(s.epoch)) != self.set_hash_at(s.epoch).await? {
            return Ok(Some(format!("the node list of epoch {} differs", s.epoch)));
        }
        Ok(None)
    }

    async fn snapshot_once(&self) -> Result<Snapshot, String> {
        // The kept logs end at a safe block: if that block's hash changed,
        // they are dropped (review STORAGE-R3-1).
        self.check_chain().await?;
        // A block the RPC does not know yet fails the pass and keeps the
        // logs (`block_id`); only another hash drops them (review
        // STORAGE-RESTART-R1 M3).
        let anchor = self.cache.lock().ok().and_then(|c| c.anchor);
        let mut vouched = None;
        if let Some((n, h)) = anchor {
            if self.block_id(&format!("0x{n:x}")).await? == (n, h) {
                vouched = Some(n);
            } else {
                self.forget_logs(&format!("the hash of the kept block {n} changed"))
                    .await;
            }
        }
        let (fin, fin_hash) = self.block_id("safe").await?;
        self.check_loaded(fin, vouched)?;
        let head = self.block_number().await?;
        let epoch = self.current_epoch().await?;
        let params = self.params().await?;
        let err = |e: abi::AbiError| e.to_string();
        let (all, passes) = self.cached_logs(head, (fin, fin_hash)).await?;
        let of = |e: &str| {
            let t = abi::topic(e);
            all.iter().filter(move |l| l.topics.first() == Some(&t))
        };
        // Node lists by first epoch (the last event of an epoch wins).
        let mut lists: Vec<(u64, Vec<NodeId>)> = Vec::new();
        for l in of(events::NODE_SET) {
            let from = u64::from_be_bytes(
                l.topics.get(1).ok_or("NodeSet topic")?.0[24..]
                    .try_into()
                    .map_err(|_| "topic")?,
            );
            let nodes: Vec<NodeId> = abi::dyn_words(&l.data, 1)
                .map_err(err)?
                .into_iter()
                .map(NodeId)
                .collect();
            match lists.last_mut() {
                Some(last) if last.0 == from => last.1 = nodes,
                _ => lists.push((from, nodes)),
            }
        }
        // URLs: Registered(index -> url), Activated(node -> index).
        let mut url_of_index = BTreeMap::new();
        let mut registrations = 0u64;
        for l in of(events::REGISTERED) {
            registrations += 1;
            let idx = l.topics.get(1).ok_or("Registered topic")?;
            let i = u64::from_be_bytes(idx.0[24..].try_into().map_err(|_| "topic")?);
            // Anyone registers any bytes as a URL: one that is not a usable
            // URL is kept empty (never fetched), and never fails the read
            // every node runs (review STORAGE-R1-1).
            let url = text_or_empty(abi::dyn_bytes(&l.data, 2).unwrap_or_default());
            url_of_index.insert(i, url);
        }
        let mut nodes = BTreeMap::new();
        let mut jailed = Jailed::new();
        for l in of(events::ACTIVATED) {
            let id = NodeId(*l.topics.get(1).ok_or("Activated topic")?);
            let i = u64::from_be_bytes(
                l.topics.get(2).ok_or("Activated topic")?.0[24..]
                    .try_into()
                    .map_err(|_| "topic")?,
            );
            let info = self.node_info(&id).await?;
            if info.jailed_from != 0 {
                jailed.insert(id, info.jailed_from);
            }
            let url = url_of_index.get(&i).cloned().unwrap_or_default();
            nodes.insert(id, NodeEntry { info, url });
        }
        let excl = exclusions_of(all.iter(), &passes)?;
        let mut deals = Vec::new();
        let mut deal_logs = 0u64;
        for l in of(events::DEAL_CREATED) {
            deal_logs += 1;
            let id = u64::from_be_bytes(
                l.topics.get(1).ok_or("DealCreated topic")?.0[24..]
                    .try_into()
                    .map_err(|_| "topic")?,
            );
            let s = |i| text_or_empty(abi::dyn_bytes(&l.data, i).unwrap_or_default());
            // A closed deal never reopens: not asked about again (review STORAGE-R1-8).
            if self
                .cache
                .lock()
                .map(|c| c.closed.contains(&id))
                .unwrap_or(false)
            {
                continue;
            }
            let info = self.deal_info(id).await?;
            if info.closed {
                if let Ok(mut c) = self.cache.lock() {
                    c.closed.insert(id);
                }
            }
            // A deal no node can serve (more shards than a manifest may
            // hold, review STORAGE-R1-7) is never placed or asked here.
            if info.shard_count > onyx_bulk::manifest::MAX_SHARDS || info.closed {
                continue;
            }
            deals.push(DealEntry {
                id,
                info,
                publisher: s(9),
                dataset: s(10),
                source_url: s(11),
            });
        }
        Ok(Snapshot {
            block: head,
            epoch,
            params,
            lists,
            nodes,
            deals,
            excl,
            jailed,
            deal_logs,
            registrations,
        })
    }
}

/// Printable ASCII text of at most 256 bytes, or empty (a URL or id from a
/// log, set by anyone).
fn text_or_empty(b: Vec<u8>) -> String {
    if b.len() <= 256 && b.iter().all(|c| (0x21..0x7f).contains(c)) {
        String::from_utf8(b).unwrap_or_default()
    } else {
        String::new()
    }
}

/// A registered node and where it serves.
#[derive(Clone, Debug)]
pub struct NodeEntry {
    /// On chain.
    pub info: NodeInfo,
    /// Its public URL (from its registration).
    pub url: String,
}

/// A deal with the ids its event carries.
#[derive(Clone, Debug)]
pub struct DealEntry {
    /// Deal id.
    pub id: u64,
    /// On chain.
    pub info: DealInfo,
    /// Publisher source id.
    pub publisher: String,
    /// Dataset id.
    pub dataset: String,
    /// Where the publisher serves the manifest and shards.
    pub source_url: String,
}

impl DealEntry {
    /// For placement.
    pub fn place(&self) -> DealPlace {
        DealPlace {
            id: self.id,
            salt: self.info.salt,
            shard_count: self.info.shard_count,
            r: self.info.r,
        }
    }
}

/// The chain at one head.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Head block.
    pub block: u64,
    /// Current epoch.
    pub epoch: u64,
    /// Parameters.
    pub params: ChainParams,
    /// Node lists by first epoch, ascending.
    pub lists: Vec<(u64, Vec<NodeId>)>,
    /// Every activated node.
    pub nodes: BTreeMap<NodeId, NodeEntry>,
    /// Every deal.
    pub deals: Vec<DealEntry>,
    /// Exclusions from `Slashed`, `Declined` and passed `Answered` events.
    pub excl: Exclusions,
    /// Jailed nodes.
    pub jailed: Jailed,
    /// `DealCreated` logs read (closed deals included).
    pub deal_logs: u64,
    /// `Registered` logs read.
    pub registrations: u64,
}

impl Snapshot {
    /// The node list in force at epoch `e`.
    pub fn list_at(&self, e: u64) -> Vec<NodeId> {
        self.lists
            .iter()
            .rev()
            .find(|(f, _)| *f <= e)
            .map(|(_, l)| l.clone())
            .unwrap_or_default()
    }

    /// The URL a node registered, if any.
    pub fn url(&self, n: &NodeId) -> Option<&str> {
        self.nodes
            .get(n)
            .map(|e| e.url.as_str())
            .filter(|u| !u.is_empty())
    }

    /// A deal by id.
    pub fn deal(&self, id: u64) -> Option<&DealEntry> {
        self.deals.iter().find(|d| d.id == id)
    }

    /// Holders of shard `index` of `d` under the placement of epoch `e`.
    pub fn holders(&self, d: &DealEntry, index: u32, e: u64) -> Vec<NodeId> {
        holders_at(
            &d.info.salt,
            d.id,
            index,
            &self.list_at(e),
            d.info.r,
            e,
            &self.excl,
        )
    }

    /// Nodes challenged for slot `slot` of `d` in epoch `e` with `seed`
    /// (R-STOR-6), and the shard the slot picked.
    pub fn challenged(
        &self,
        d: &DealEntry,
        e: u64,
        seed: &Hash32,
        slot: u32,
    ) -> (u32, Vec<NodeId>) {
        let index = slot_shard(seed, d.id, slot, d.info.shard_count);
        let list = self.list_at(e.saturating_sub(1));
        (
            index,
            challenged(
                &d.info.salt,
                d.id,
                index,
                &list,
                d.info.r,
                e,
                &self.excl,
                &self.jailed,
            ),
        )
    }

    /// The deals a node holds shards of now or next epoch, with the shard
    /// indexes (R-STOR-12: next epoch's placement is fetched ahead).
    pub fn wanted(&self, me: &NodeId) -> Vec<(u64, u32)> {
        let mut out = BTreeSet::new();
        let lists = [self.list_at(self.epoch), self.list_at(self.epoch + 1)];
        for d in self
            .deals
            .iter()
            .filter(|d| !d.info.closed && d.info.end_epoch > self.epoch)
        {
            for (k, e) in [self.epoch, self.epoch + 1].into_iter().enumerate() {
                if !lists[k].contains(me) {
                    continue;
                }
                for i in 0..d.info.shard_count {
                    let h = holders_at(&d.info.salt, d.id, i, &lists[k], d.info.r, e, &self.excl);
                    if h.contains(me) {
                        out.insert((d.id, i));
                    }
                }
            }
        }
        out.into_iter().collect()
    }

    /// Everything [`Snapshot::wanted`] depends on: equal keys give equal
    /// placements, so a node computes them again only when one changed (review
    /// STORAGE-R2-5: a deal of 65,536 shards costs hashes per shard and node).
    pub fn placement_key(&self) -> PlacementKey {
        PlacementKey {
            epoch: self.epoch,
            lists: [self.list_at(self.epoch), self.list_at(self.epoch + 1)],
            // Passes that change no exclusion are left out: they come every
            // epoch and would recompute the placement each time.
            excl: self.excl.placement_view(),
            deals: self
                .deals
                .iter()
                .map(|d| {
                    (
                        d.id,
                        d.info.salt,
                        d.info.shard_count,
                        d.info.r,
                        d.info.end_epoch,
                        d.info.closed,
                    )
                })
                .collect(),
        }
    }
}

/// The inputs of a node's placement at one head ([`Snapshot::placement_key`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementKey {
    epoch: u64,
    lists: [Vec<NodeId>; 2],
    excl: Exclusions,
    deals: Vec<(u64, Hash32, u32, u8, u64, bool)>,
}

/// Where to fetch shard `index` of `d` from: its holders under the last three
/// placements (minus `me`), then the publisher.
pub fn sources(s: &Snapshot, d: &DealEntry, index: u32, me: Option<&NodeId>) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    for e in [s.epoch, s.epoch.saturating_sub(1), s.epoch + 1] {
        for n in s.holders(d, index, e) {
            if Some(&n) == me || !seen.insert(n) {
                continue;
            }
            if let Some(u) = s.url(&n) {
                if !urls.iter().any(|x| x == u) {
                    urls.push(u.to_string());
                }
            }
        }
    }
    if !d.source_url.is_empty() && !urls.contains(&d.source_url) {
        urls.push(d.source_url.clone());
    }
    urls
}

/// Where to fetch the manifest of `d` from: the deal's source, then the
/// nodes that hold its first shard now or last epoch (every holder keeps the
/// manifest and its earlier versions). Never every node ever registered, so
/// stale or tarpit URLs cannot stretch a pass (review STORAGE-R2-3).
pub fn manifest_sources(s: &Snapshot, d: &DealEntry, me: Option<&NodeId>) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    if !d.source_url.is_empty() {
        urls.push(d.source_url.clone());
    }
    for e in [s.epoch, s.epoch.saturating_sub(1)] {
        for n in s.holders(d, 0, e) {
            if Some(&n) == me {
                continue;
            }
            if let Some(u) = s.url(&n) {
                if !urls.iter().any(|x| x == u) {
                    urls.push(u.to_string());
                }
            }
        }
    }
    urls
}

/// The order one pass tries unheld shards in: one shard of each deal in turn,
/// starting one deal later each pass, so a deal nobody serves never starves
/// the others (review STORAGE-R2-1).
fn fetch_order(queues: &BTreeMap<u64, Vec<u32>>, pass: usize) -> Vec<(u64, u32)> {
    let deals: Vec<(&u64, &Vec<u32>)> = queues.iter().collect();
    if deals.is_empty() {
        return Vec::new();
    }
    let start = pass % deals.len();
    let longest = deals.iter().map(|d| d.1.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..longest {
        for k in 0..deals.len() {
            let (d, q) = deals[(start + k) % deals.len()];
            if let Some(x) = q.get(i) {
                out.push((*d, *x));
            }
        }
    }
    out
}

fn past(deadline: Option<tokio::time::Instant>) -> bool {
    deadline.is_some_and(|d| tokio::time::Instant::now() >= d)
}

fn join(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// Fetch the manifest with `root` from the first of `urls` that serves one
/// with exactly that root. Returns the manifest and its signed report bytes.
pub async fn fetch_manifest(
    get: &dyn HttpGet,
    urls: &[String],
    root: &Hash32,
) -> Result<(Manifest, Vec<u8>), String> {
    fetch_manifest_until(get, urls, root, None).await
}

/// [`fetch_manifest`], trying no further URL after `deadline`.
async fn fetch_manifest_until(
    get: &dyn HttpGet,
    urls: &[String],
    root: &Hash32,
    deadline: Option<tokio::time::Instant>,
) -> Result<(Manifest, Vec<u8>), String> {
    let mut why = Vec::new();
    for u in urls {
        if past(deadline) {
            why.push("fetch time budget used up".into());
            break;
        }
        let r = get
            .get(
                &join(u, &format!("/v1/bulk/manifests/{root}")),
                MAX_MANIFEST_ANSWER,
            )
            .await;
        let body = match r {
            Ok((200, b)) => b,
            Ok((s, _)) => {
                why.push(format!("{u}: HTTP {s}"));
                continue;
            }
            Err(e) => {
                why.push(format!("{u}: {e}"));
                continue;
            }
        };
        let parsed = (|| -> Result<(Manifest, Vec<u8>), String> {
            let v: Value = serde_json::from_slice(&body).map_err(|_| "not JSON")?;
            let m = hex_field(&v["manifest"], "manifest").map_err(|e| e.to_string())?;
            let rep = hex_field(&v["report"], "report").map_err(|e| e.to_string())?;
            let m = Manifest::from_bytes(&m).map_err(|e| e.to_string())?;
            if m.header.root() != *root {
                return Err("another manifest root".into());
            }
            let r: SignedReport = borsh::from_slice(&rep).map_err(|_| "report")?;
            onyx_bulk::anchor::verify(&m.header, &r).map_err(|e| e.to_string())?;
            Ok((m, rep))
        })();
        match parsed {
            Ok(x) => return Ok(x),
            Err(e) => why.push(format!("{u}: {e}")),
        }
    }
    Err(format!(
        "no source served manifest {root}: {}",
        why.join("; ")
    ))
}

/// What the storage node does (`ONYX_STORAGE_*`, docs/ops/config.md).
#[derive(Clone, Debug)]
pub struct StorageNodeConfig {
    /// `OnyxStorage`.
    pub contract: [u8; 20],
    /// This node's id.
    pub node: NodeId,
    /// Fetch placed shards (off: serve and prove only what is held).
    pub fetch: bool,
    /// Settle other nodes' missed answers after their epoch.
    pub keeper: bool,
    /// Fetches started per tick.
    pub fetches_per_tick: usize,
}

/// One tick's results.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Epoch.
    pub epoch: u64,
    /// Shards placed on this node now or next epoch.
    pub wanted: usize,
    /// Of those, held.
    pub held: usize,
    /// Fetched and stored this tick.
    pub fetched: usize,
    /// Fetches that failed this tick.
    pub fetch_failed: usize,
    /// Answers sent this tick.
    pub answered: usize,
    /// Seeds recorded this tick.
    pub seeds: usize,
    /// Misses settled this tick (keeper).
    pub settled: usize,
    /// Challenges this node could not answer (shard not held).
    pub unanswerable: usize,
    /// Answers whose transaction failed to send (out of gas, a fee cap, a
    /// nonce; review OBS-STORAGE-R1 2): the challenge is missed unless a
    /// later tick in the epoch sends it.
    pub answer_failed: usize,
    /// Shards given up because no source served them.
    pub declined: usize,
}

/// The storage loop's progress for `/v1/health` (THREATS OBS-STORAGE-1,
/// review OBS-STORAGE-R1): a tick that runs, the last one that ended and
/// the last one that ended well, so a long tick is told from a stuck loop
/// and one failed tick from a failing one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopStatus {
    /// When the running tick began, on the node's clock (`None` between ticks).
    pub tick_started: Option<u64>,
    /// When the last tick ended, well or not.
    pub last_tick: Option<u64>,
    /// When the last tick that ended well ended.
    pub last_ok: Option<u64>,
    /// Ticks in a row that ended in an error (0 after one that ended well).
    pub failed_in_a_row: u64,
    /// The last tick's [`error_class`]; `None` when it ended well.
    pub last_error_class: Option<&'static str>,
}

/// The class of a tick's error, one of [`ERROR_CLASSES`] (review
/// OBS-STORAGE-R1 3): `/v1/health` shows only this, never the text, which
/// may name a provider URL and its key; the text goes to the node's log.
pub fn error_class(e: &str) -> &'static str {
    let e = e.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if has(&["follows rules v"]) {
        "contract_version"
    } else if has(&["http 429", "-32005", "rate limit", "rate-limit"]) {
        "rpc_rate_limited"
    } else if has(&["unreachable", "timed out", "connection refused"]) {
        "rpc_unreachable"
    } else if has(&["disagrees with the contract"]) {
        "snapshot_disagrees"
    } else if has(&[
        "node error",
        "node answered http",
        "bad answer",
        "answer is for another request",
        "answer has no result",
        "chain read failed",
        "rules_version()",
        "revert",
    ]) {
        "rpc_error"
    } else if has(&[
        "os error",
        "no space left",
        "read-only file system",
        "permission denied",
    ]) {
        "store"
    } else {
        "other"
    }
}

/// This host's clock, unix seconds.
fn system_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A node's placement (its (deal, shard) pairs) and the inputs it came from.
type Placed = (PlacementKey, Vec<(u64, u32)>);

/// One learn pass's results ([`StorageNode::learn_pass`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LearnReport {
    /// The pass's finalized block (0: no pass ran).
    pub block: u64,
    /// Datasets asked about this pass.
    pub asked: usize,
    /// Removals kept or moved to a newer catalog this pass.
    pub learned: usize,
    /// Datasets asked about again first: the book withdrew their newest
    /// proof this pass.
    pub reasked: usize,
    /// Removals whose newest proof the book withdrew while an older one
    /// holds (it took the newest's place).
    pub moved: usize,
    /// Removals dropped this pass (the book withdrew every kept proof and no
    /// source gave another).
    pub dropped: usize,
    /// Removals kept after the pass.
    pub kept: usize,
    /// Placed datasets no source has given a verified answer for since the
    /// node started (live or removed): a deal whose source and peers hold no
    /// catalog, with no `ONYX_STORAGE_CATALOG_NODES` that does.
    pub never_learned: usize,
}

/// Where a storage node learns of removals (slice 2a).
struct LearnSetup {
    /// `ONYX_STORAGE_CATALOG_NODES`, asked first.
    catalog_nodes: Vec<String>,
    /// Its own transport (a short timeout); the fetch transport when unset.
    get: Option<Arc<dyn HttpGet>>,
}

/// Per (deal, shard or `WHOLE_DEAL`): the epoch of the first failed pass, the
/// epoch of the last one and the failed passes in that epoch.
type Failures = (u64, u64, u32);

/// The storage-node loop's state.
pub struct StorageNode {
    chain: StorageChain,
    tx: Arc<dyn TxSend>,
    get: Arc<dyn HttpGet>,
    store: Arc<BulkStore>,
    cfg: StorageNodeConfig,
    done: Mutex<BTreeSet<(u64, u64, u32, NodeId)>>,
    /// (deal, epoch, slot) the contract said asks this node where the chain
    /// view did not (`asked_anyway`): asked once, not every pass.
    confirmed: Mutex<BTreeSet<(u64, u64, u32)>>,
    last: Mutex<TickReport>,
    failures: Mutex<BTreeMap<(u64, u32), Failures>>,
    /// Passes run (rotates the fetch order).
    passes: AtomicUsize,
    /// The last placement computed and its inputs.
    placed: Mutex<Option<Placed>>,
    /// The contract's version was checked (`StorageChain::check_version`).
    version_ok: AtomicBool,
    /// The last snapshot a tick read: the learn pass reads no chain view of
    /// its own and never waits for a tick.
    snap: Mutex<Option<Arc<Snapshot>>>,
    learn: LearnSetup,
    /// Rotates the datasets a learn pass asks about.
    learn_turn: AtomicUsize,
    /// Datasets a verified leaf answer named since start.
    heard: Mutex<BTreeSet<(String, String)>>,
    learn_last: Mutex<LearnReport>,
    /// This host's clock, unix seconds ([`StorageNode::with_clock`]).
    clock: crate::Clock,
    /// When the node was made, on `clock` (the loop's age before its first
    /// tick).
    started: u64,
    /// The loop's progress ([`StorageNode::loop_status`]).
    progress: Mutex<LoopStatus>,
    /// The loop's period in seconds ([`run`] sets it; 0 before it starts).
    every_secs: AtomicU64,
    /// When the kept logs are next checked against `exclusionFrom`, on
    /// `clock` (`None`: on the next pass; review STORAGE-RESTART-R1 M1).
    excl_due: Mutex<Option<u64>>,
    /// Rotates the (deal, shard, node) that check asks about.
    excl_turn: AtomicUsize,
}

impl StorageNode {
    /// A storage node over `store`.
    pub fn new(
        chain: StorageChain,
        tx: Arc<dyn TxSend>,
        get: Arc<dyn HttpGet>,
        store: Arc<BulkStore>,
        cfg: StorageNodeConfig,
    ) -> Self {
        StorageNode {
            chain,
            tx,
            get,
            store,
            cfg,
            done: Mutex::new(BTreeSet::new()),
            confirmed: Mutex::new(BTreeSet::new()),
            last: Mutex::new(TickReport::default()),
            failures: Mutex::new(BTreeMap::new()),
            passes: AtomicUsize::new(0),
            placed: Mutex::new(None),
            version_ok: AtomicBool::new(false),
            snap: Mutex::new(None),
            learn: LearnSetup {
                catalog_nodes: Vec::new(),
                get: None,
            },
            learn_turn: AtomicUsize::new(0),
            heard: Mutex::new(BTreeSet::new()),
            learn_last: Mutex::new(LearnReport::default()),
            clock: Arc::new(system_unix),
            started: system_unix(),
            progress: Mutex::new(LoopStatus::default()),
            every_secs: AtomicU64::new(0),
            excl_due: Mutex::new(None),
            excl_turn: AtomicUsize::new(0),
        }
    }

    /// Stamp ticks with `clock` (unix seconds; tests move it by hand); the
    /// node counts as started now on it.
    pub fn with_clock(mut self, clock: crate::Clock) -> Self {
        self.started = clock();
        self.clock = clock;
        self
    }

    /// Learn of removals (ADR-0059 amendment 4 item 5a slice 2a) from
    /// `catalog_nodes` first (`ONYX_STORAGE_CATALOG_NODES`), over `get` (a
    /// transport with a short timeout).
    pub fn with_learn(mut self, catalog_nodes: Vec<String>, get: Arc<dyn HttpGet>) -> Self {
        self.learn = LearnSetup {
            catalog_nodes,
            get: Some(get),
        };
        self
    }

    /// The shards placed here now or next epoch, computed again only when an
    /// input changed (a deal, the node list, an exclusion or the epoch).
    fn wanted(&self, s: &Snapshot) -> Vec<(u64, u32)> {
        let key = s.placement_key();
        if let Ok(p) = self.placed.lock() {
            if let Some((k, w)) = p.as_ref() {
                if *k == key {
                    return w.clone();
                }
            }
        }
        let w = s.wanted(&self.cfg.node);
        if let Ok(mut p) = self.placed.lock() {
            *p = Some((key, w.clone()));
        }
        w
    }

    /// The last tick's report.
    pub fn last(&self) -> TickReport {
        self.last.lock().map(|l| l.clone()).unwrap_or_default()
    }

    async fn ensure_manifest(
        &self,
        s: &Snapshot,
        d: &DealEntry,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Manifest, String> {
        if let Some((m, _)) = self.store.manifest_record(&d.info.manifest_root) {
            return Ok(m);
        }
        let urls = manifest_sources(s, d, Some(&self.cfg.node));
        // Earlier versions first (versions are taken in order, ADR-0059).
        let mut chain = Vec::new();
        let mut root = d.info.manifest_root;
        for _ in 0..64 {
            if self.store.manifest_record(&root).is_some() {
                break;
            }
            if past(deadline) {
                return Err("fetch time budget used up".into());
            }
            let (m, rep) = fetch_manifest_until(self.get.as_ref(), &urls, &root, deadline).await?;
            let prev = m.header.prev;
            chain.push((m, rep));
            if prev == Hash32::ZERO {
                break;
            }
            root = prev;
        }
        for (m, rep) in chain.iter().rev() {
            self.store
                .accept_manifest(&m.to_bytes(), rep)
                .await
                .map_err(|e| format!("manifest {}: {}", m.header.root(), e.message))?;
        }
        self.store
            .manifest_record(&d.info.manifest_root)
            .map(|(m, _)| m)
            .ok_or_else(|| "manifest not held after fetch".to_string())
    }

    async fn fetch_shard(
        &self,
        s: &Snapshot,
        d: &DealEntry,
        m: &Manifest,
        index: u32,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<(), String> {
        let e = m.shards.get(index as usize).ok_or("no such shard")?;
        let mut why = Vec::new();
        for u in sources(s, d, index, Some(&self.cfg.node)) {
            if past(deadline) {
                why.push("fetch time budget used up".into());
                break;
            }
            let url = join(&u, &format!("/v1/bulk/shards/{}", e.root));
            match self.get.get(&url, e.bytes as usize).await {
                Ok((200, body)) => match self
                    .store
                    .put_shard(&d.publisher, &d.dataset, &e.root, body)
                    .await
                {
                    Ok(_) => return Ok(()),
                    // A peer's bad bytes are refused here (THREATS STORAGE-4).
                    Err(err) => why.push(format!("{u}: refused: {}", err.message)),
                },
                Ok((st, _)) => why.push(format!("{u}: HTTP {st}")),
                Err(err) => why.push(format!("{u}: {err}")),
            }
        }
        Err(format!(
            "shard {index} of deal {}: {}",
            d.id,
            why.join("; ")
        ))
    }

    async fn send(&self, data: Vec<u8>) -> Result<String, String> {
        self.tx.send(&self.chain.contract(), data).await
    }

    /// One pass: record the seed and answer this epoch's challenges first
    /// (a slow or withholding source must never delay them, review
    /// STORAGE-R1-2), settle others' misses (keeper), then fetch what is
    /// placed here within [`FETCH_BUDGET`]. When it starts and when it ends,
    /// well or not, [`StorageNode::loop_status`] records it for `/v1/health`
    /// (THREATS OBS-STORAGE-1, review OBS-STORAGE-R1).
    pub async fn tick(&self) -> Result<TickReport, String> {
        let started = (self.clock)();
        if let Ok(mut l) = self.progress.lock() {
            l.tick_started = Some(started);
        }
        let r = self.pass().await;
        let now = (self.clock)();
        if let Ok(mut l) = self.progress.lock() {
            l.tick_started = None;
            l.last_tick = Some(now);
            match &r {
                Ok(_) => {
                    l.last_ok = Some(now);
                    l.failed_in_a_row = 0;
                    l.last_error_class = None;
                }
                Err(e) => {
                    l.failed_in_a_row += 1;
                    l.last_error_class = Some(error_class(e));
                }
            }
        }
        r
    }

    /// The loop's progress: the running tick, the last one and the last good
    /// one.
    pub fn loop_status(&self) -> LoopStatus {
        self.progress.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// [`StorageNode::tick`]'s pass.
    async fn pass(&self) -> Result<TickReport, String> {
        // Nothing is sent to a contract of another version (no seed, fetch,
        // answer, settle or decline): it would read our answers as wrong.
        if !self.version_ok.load(Ordering::Relaxed) {
            self.chain.check_version().await?;
            self.version_ok.store(true, Ordering::Relaxed);
        }
        let s = Arc::new(self.chain.snapshot().await?);
        self.check_kept_logs(&s).await?;
        if let Ok(mut k) = self.snap.lock() {
            *k = Some(s.clone());
        }
        let mut rep = TickReport {
            epoch: s.epoch,
            ..Default::default()
        };
        // The seed of this epoch (anyone may record it; the first wins). Only
        // when a deal is checked in this epoch: with none, a seed asks nobody
        // and its transaction only spends the node's gas (after the S16 run's
        // deals closed, every node kept recording one every epoch).
        let e = s.epoch;
        let mut seed = self.chain.seed_of(e).await?;
        let checked = s
            .deals
            .iter()
            .any(|d| d.info.start_epoch <= e && e < d.info.end_epoch);
        if seed == Hash32::ZERO && checked {
            let start = self.chain.epoch_start(e).await?;
            if s.block > start && s.block <= start + onyx_storage::SEED_WINDOW {
                if self.send(abi::record_seed(e)).await.is_ok() {
                    rep.seeds += 1;
                }
                seed = self.chain.seed_of(e).await?;
            }
        }
        if seed != Hash32::ZERO && e >= 1 {
            let prev = s.list_at(e - 1);
            if set_hash(&prev) == self.chain.set_hash_at(e - 1).await? {
                rep.answered += self.answer_epoch(&s, e, &seed, &prev, &mut rep).await?;
            }
        }
        if self.cfg.keeper {
            rep.settled += self.settle(&s).await?;
        }
        self.fetch_placed(&s, &mut rep).await;
        if let Ok(mut l) = self.last.lock() {
            *l = rep.clone();
        }
        Ok(rep)
    }

    /// On the first pass after start and about daily after
    /// ([`EXCL_CHECK_EVERY_SECS`]), check the kept logs against the
    /// contract's `exclusionFrom` (review STORAGE-RESTART-R1 M1). A
    /// difference drops them and fails the pass, so nothing is sent from a
    /// wrong view; the next pass reads them again. A check that got no answer
    /// is logged and tried again after [`EXCL_CHECK_RETRY_SECS`].
    async fn check_kept_logs(&self, s: &Snapshot) -> Result<(), String> {
        let now = (self.clock)();
        let due = match self.excl_due.lock() {
            Ok(d) => d.is_none_or(|t| now >= t),
            Err(_) => false,
        };
        if !due {
            return Ok(());
        }
        let turn = self.excl_turn.fetch_add(1, Ordering::Relaxed);
        let (next, r) = match self.chain.check_exclusions(s, &self.cfg.node, turn).await {
            Ok(None) => (Some(now.saturating_add(EXCL_CHECK_EVERY_SECS)), Ok(())),
            Ok(Some(why)) => (
                None,
                Err(format!(
                    "the snapshot disagrees with the contract ({why}); the kept logs are dropped and read again next pass"
                )),
            ),
            Err(e) => {
                eprintln!("onyx-node: storage: exclusionFrom check of the kept logs: {e}");
                (Some(now.saturating_add(EXCL_CHECK_RETRY_SECS)), Ok(()))
            }
        };
        if let Ok(mut d) = self.excl_due.lock() {
            *d = next;
        }
        r
    }

    /// Count a failed pass for `(deal, index)` (`index` = `WHOLE_DEAL` for a
    /// deal none of whose data any source served); true when it is due for a
    /// decline: at least [`DECLINE_AFTER_FAILED_PASSES`] failed passes in this
    /// epoch after a failure in an earlier one (review STORAGE-R1-3, R2-4).
    fn failed(&self, key: (u64, u32), epoch: u64) -> bool {
        self.failures
            .lock()
            .map(|mut f| {
                let c = f.entry(key).or_insert((epoch, epoch, 0));
                if c.1 != epoch {
                    c.1 = epoch;
                    c.2 = 0;
                }
                c.2 += 1;
                c.0 < epoch && c.2 >= DECLINE_AFTER_FAILED_PASSES
            })
            .unwrap_or(false)
    }

    /// Forget the counts of data no longer placed here.
    fn prune(&self, wanted: &[(u64, u32)]) {
        let deals: BTreeSet<u64> = wanted.iter().map(|x| x.0).collect();
        if let Ok(mut f) = self.failures.lock() {
            f.retain(|k, _| {
                if k.1 == WHOLE_DEAL {
                    deals.contains(&k.0)
                } else {
                    wanted.binary_search(k).is_ok()
                }
            });
        }
    }

    /// Shards of `deal` that failed in an earlier epoch and have not been
    /// fetched since: no source served them across an epoch boundary.
    fn failing(&self, deal: u64, epoch: u64) -> usize {
        self.failures
            .lock()
            .map(|f| {
                f.iter()
                    .filter(|(k, c)| k.0 == deal && k.1 != WHOLE_DEAL && c.0 < epoch)
                    .count()
            })
            .unwrap_or(0)
    }

    fn forget_deal(&self, deal: u64) {
        if let Ok(mut f) = self.failures.lock() {
            f.retain(|k, _| k.0 != deal);
        }
    }

    fn succeeded(&self, key: (u64, u32)) {
        if let Ok(mut f) = self.failures.lock() {
            f.remove(&key);
        }
    }

    async fn decline(&self, s: &Snapshot, deal: u64, index: u32, rep: &mut TickReport) {
        let me = self.cfg.node;
        if s.excl.from(deal, index, &me).is_some() {
            return;
        }
        match self.send(abi::decline(deal, index, &me)).await {
            Ok(_) => {
                rep.declined += 1;
                self.succeeded((deal, index));
            }
            Err(e) => eprintln!("onyx-node: storage: decline deal {deal} shard {index}: {e}"),
        }
    }

    /// Fetch the shards placed on this node now or next epoch, within
    /// [`FETCH_BUDGET`] (no URL is tried after it; review STORAGE-R2-3). Data
    /// this node already gave up or was excluded from is skipped; deals take
    /// turns (review STORAGE-R2-1); failures do not use up the fetch pace. Data
    /// no source served across an epoch is declined: the whole deal in one
    /// transaction when nothing of it is held and every try failed, or when
    /// more of its shards fail than one pass can try (partial withholding).
    async fn fetch_placed(&self, s: &Snapshot, rep: &mut TickReport) {
        let me = self.cfg.node;
        let deadline = Some(tokio::time::Instant::now() + FETCH_BUDGET);
        let wanted = self.wanted(s);
        rep.wanted = wanted.len();
        self.prune(&wanted);
        let pass = self.passes.fetch_add(1, Ordering::Relaxed);
        let mut placed: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        for (deal, index) in &wanted {
            if s.excl.from(*deal, *index, &me).is_none() {
                placed.entry(*deal).or_default().push(*index);
            }
        }
        let mut manifests: BTreeMap<u64, Manifest> = BTreeMap::new();
        let mut queues: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        let mut held: BTreeMap<u64, usize> = BTreeMap::new();
        for (deal, shards) in &placed {
            let Some(d) = s.deal(*deal) else { continue };
            if past(deadline) {
                break;
            }
            let m = match self.ensure_manifest(s, d, deadline).await {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("onyx-node: storage: deal {deal}: {e}");
                    rep.fetch_failed += 1;
                    if self.cfg.fetch && self.failed((*deal, WHOLE_DEAL), s.epoch) {
                        self.decline(s, *deal, WHOLE_DEAL, rep).await;
                        self.forget_deal(*deal);
                    }
                    continue;
                }
            };
            let mut have = 0;
            let mut q = Vec::new();
            for i in shards {
                match m.shards.get(*i as usize) {
                    Some(e) if self.store.holds_shard(&e.root) => have += 1,
                    Some(_) => q.push(*i),
                    None => {}
                }
            }
            rep.held += have;
            held.insert(*deal, have);
            if !q.is_empty() {
                let n = q.len();
                q.rotate_left(pass % n);
                queues.insert(*deal, q);
            }
            manifests.insert(*deal, m);
        }
        if !self.cfg.fetch {
            return;
        }
        // deal -> (shards tried, shards failed, shards due for a decline)
        let mut tried: BTreeMap<u64, (usize, usize, Vec<u32>)> = BTreeMap::new();
        let mut attempts = 0usize;
        for (deal, index) in fetch_order(&queues, pass) {
            if rep.fetched >= self.cfg.fetches_per_tick
                || attempts >= 2 * self.cfg.fetches_per_tick
                || past(deadline)
            {
                break;
            }
            let (Some(d), Some(m)) = (s.deal(deal), manifests.get(&deal)) else {
                continue;
            };
            attempts += 1;
            let t = tried.entry(deal).or_default();
            t.0 += 1;
            match self.fetch_shard(s, d, m, index, deadline).await {
                Ok(()) => {
                    rep.fetched += 1;
                    rep.held += 1;
                    *held.entry(deal).or_default() += 1;
                    self.succeeded((deal, index));
                }
                Err(err) => {
                    eprintln!("onyx-node: storage: {err}");
                    rep.fetch_failed += 1;
                    t.1 += 1;
                    // No source served it (a withholding publisher or
                    // replicas, THREATS STORAGE-16): give it up from the
                    // next epoch rather than be slashed for data nobody gave.
                    if self.failed((deal, index), s.epoch) {
                        t.2.push(index);
                    }
                }
            }
        }
        for (deal, n) in &held {
            if *n > 0 {
                self.succeeded((*deal, WHOLE_DEAL));
            }
        }
        for (deal, (n, failed, due)) in tried {
            if held.get(&deal).copied().unwrap_or(0) == 0 && n > 0 && failed == n {
                if self.failed((deal, WHOLE_DEAL), s.epoch) {
                    self.decline(s, deal, WHOLE_DEAL, rep).await;
                    self.forget_deal(deal);
                }
                continue;
            }
            // Partial withholding (a source serves a few shards of a deal and
            // withholds the rest): one decline a shard, each due only after three
            // failed passes in an epoch, cannot keep up with thousands of
            // withheld shards, and the node is jailed for misses on them first.
            // More failing shards than one pass can try: give the deal up whole.
            if !due.is_empty() && self.failing(deal, s.epoch) > 2 * self.cfg.fetches_per_tick {
                self.decline(s, deal, WHOLE_DEAL, rep).await;
                self.forget_deal(deal);
                continue;
            }
            for i in due {
                self.decline(s, deal, i, rep).await;
            }
        }
    }

    async fn answer_epoch(
        &self,
        s: &Snapshot,
        e: u64,
        seed: &Hash32,
        prev: &[NodeId],
        rep: &mut TickReport,
    ) -> Result<usize, String> {
        let me = self.cfg.node;
        let mut sent = 0;
        let k = s.params.money.challenges_per_deal;
        for d in s
            .deals
            .iter()
            .filter(|d| d.info.start_epoch <= e && e < d.info.end_epoch)
        {
            for slot in 0..k {
                let (index, who) = s.challenged(d, e, seed, slot);
                let key = (d.id, e, slot, me);
                if self.done.lock().map(|x| x.contains(&key)).unwrap_or(false) {
                    continue;
                }
                if !who.contains(&me) && !self.asked_anyway(&s.excl, d, e, slot, index, prev).await
                {
                    continue;
                }
                if self.chain.slot_status(d.id, e, slot, &me).await? != 0 {
                    if let Ok(mut x) = self.done.lock() {
                        x.insert(key);
                    }
                    continue;
                }
                let Some((m, _)) = self.store.manifest_record(&d.info.manifest_root) else {
                    rep.unanswerable += 1;
                    continue;
                };
                let Some(entry) = m.shards.get(index as usize) else {
                    continue;
                };
                let mut file = self.store.shard_file(&entry.root).await;
                if file.is_none() && self.cfg.fetch && self.store.holds_shard(&entry.root) {
                    // Held on paper but damaged on disk: fetch it again from a
                    // replica or the publisher, for at most one call's time
                    // (a shard never held is the fetch phase's work, so a
                    // withheld one never delays later answers; review STORAGE-R2-1).
                    let until = Some(tokio::time::Instant::now() + Duration::from_secs(120));
                    match self.fetch_shard(s, d, &m, index, until).await {
                        Ok(()) => file = self.store.shard_file(&entry.root).await,
                        Err(e) => eprintln!("onyx-node: storage: re-fetch: {e}"),
                    }
                }
                let Some(file) = file else {
                    rep.unanswerable += 1;
                    continue;
                };
                // The row drawn for this node (each holder proves its own row).
                let p = match challenge::answer(&m, &file, seed, d.id, slot, &me) {
                    Ok(p) => p,
                    Err(err) => {
                        eprintln!(
                            "onyx-node: storage: cannot prove deal {} slot {slot}: {err}",
                            d.id
                        );
                        rep.unanswerable += 1;
                        continue;
                    }
                };
                // Checked here as the contract checks it: never send a proof that fails.
                if challenge::verify(
                    &d.info.shards_root,
                    d.info.shard_count,
                    seed,
                    d.id,
                    slot,
                    &me,
                    &p,
                )
                .is_err()
                {
                    rep.unanswerable += 1;
                    continue;
                }
                match self.send(abi::answer(d.id, e, slot, &me, prev, &p)).await {
                    Ok(_) => {
                        sent += 1;
                        if let Ok(mut x) = self.done.lock() {
                            x.insert(key);
                        }
                    }
                    Err(err) => {
                        rep.answer_failed += 1;
                        eprintln!(
                            "onyx-node: storage: answer deal {} slot {slot}: {err}",
                            d.id
                        )
                    }
                }
            }
        }
        Ok(sent)
    }

    /// Whether the contract asks this node for `slot` of `d` in epoch `e`
    /// although the chain view says it does not (review STORAGE-S16-R6 H1:
    /// defense in depth against a view that drifts from `exclusionFrom`).
    /// Asked of the contract (`challengedHolders`) only when the node is in
    /// the shard's top R with no exclusion at all; the answer is kept for
    /// the epoch (who is asked in an epoch is fixed when it starts).
    async fn asked_anyway(
        &self,
        excl: &Exclusions,
        d: &DealEntry,
        e: u64,
        slot: u32,
        index: u32,
        prev: &[NodeId],
    ) -> bool {
        let me = self.cfg.node;
        if !holders(&shard_key(&d.info.salt, index), prev, d.info.r, |_| false).contains(&me) {
            return false;
        }
        // Out of the shard by e whatever the notice (every decline counted one
        // epoch late): the contract cannot ask, so no call (review
        // STORAGE-S16-R7 L2).
        if excl.out_by_any_notice(d.id, index, &me, e) {
            return false;
        }
        if self
            .confirmed
            .lock()
            .map(|c| c.contains(&(d.id, e, slot)))
            .unwrap_or(false)
        {
            return true;
        }
        match self.chain.challenged_holders(d.id, e, slot, prev).await {
            Ok((i, who)) if i == index && who.contains(&me) => {
                eprintln!(
                    "onyx-node: storage: the contract asks deal {} slot {slot} epoch {e} of this \
                     node where the chain view did not: answering",
                    d.id
                );
                if let Ok(mut c) = self.confirmed.lock() {
                    // Only this epoch and the last one are ever asked about.
                    c.retain(|x| x.1 + 1 >= e);
                    c.insert((d.id, e, slot));
                }
                true
            }
            Ok((i, _)) if i != index => {
                eprintln!(
                    "onyx-node: storage: WARNING: the contract draws shard {i} for deal {} slot \
                     {slot} epoch {e}, this node's chain view shard {index}: its view is wrong \
                     (seed or shard count)",
                    d.id
                );
                if let Ok(mut x) = self.done.lock() {
                    x.insert((d.id, e, slot, me));
                }
                false
            }
            Ok(_) => {
                if let Ok(mut x) = self.done.lock() {
                    x.insert((d.id, e, slot, me));
                }
                false
            }
            Err(err) => {
                eprintln!(
                    "onyx-node: storage: challengedHolders deal {} slot {slot}: {err}",
                    d.id
                );
                false
            }
        }
    }

    /// Settle the misses of the two epochs before this one (keeper duty).
    async fn settle(&self, s: &Snapshot) -> Result<usize, String> {
        let mut n = 0;
        let k = s.params.money.challenges_per_deal;
        for back in 1..=onyx_storage::SETTLE_EPOCHS {
            let Some(e) = s.epoch.checked_sub(back) else {
                break;
            };
            if e == 0 {
                continue;
            }
            let seed = self.chain.seed_of(e).await?;
            if seed == Hash32::ZERO {
                continue;
            }
            let prev = s.list_at(e - 1);
            if set_hash(&prev) != self.chain.set_hash_at(e - 1).await? {
                continue;
            }
            for d in s
                .deals
                .iter()
                .filter(|d| d.info.start_epoch <= e && e < d.info.end_epoch)
            {
                for slot in 0..k {
                    let (_, who) = s.challenged(d, e, &seed, slot);
                    for node in who {
                        let key = (d.id, e, slot, node);
                        if self.done.lock().map(|x| x.contains(&key)).unwrap_or(false) {
                            continue;
                        }
                        if self.chain.slot_status(d.id, e, slot, &node).await? != 0 {
                            if let Ok(mut x) = self.done.lock() {
                                x.insert(key);
                            }
                            continue;
                        }
                        if self
                            .send(abi::settle_missed(d.id, e, slot, &node, &prev))
                            .await
                            .is_ok()
                        {
                            n += 1;
                            if let Ok(mut x) = self.done.lock() {
                                x.insert(key);
                            }
                        }
                    }
                }
            }
        }
        Ok(n)
    }

    /// The `storage` object of `/v1/health` (THREATS OBS-STORAGE-1, review
    /// OBS-STORAGE-R1; docs/ops/storage-node.md "Health"): the counters of
    /// the last tick that ended well, the running tick's start
    /// (`tickStartedUnix`, null between ticks), the last tick's end and the
    /// last good one's (this host's clock; `startedUnix` dates the wait
    /// before the first), failed ticks in a row and the last one's error
    /// class (never its text), the loop's period, the last block the kept
    /// logs cover (`logsReadThrough`, THREATS STORAGE-RESTART-1: it climbs
    /// to the chain's safe block while the node catches up; null while logs
    /// loaded from the file are not checked against the chain yet) and the
    /// learned removals.
    pub fn status_json(&self) -> Value {
        let l = self.last();
        let r = self.learn_last();
        let p = self.loop_status();
        json!({
            "node": self.cfg.node.to_string(), "epoch": l.epoch, "wanted": l.wanted, "held": l.held,
            "fetched": l.fetched, "fetchFailed": l.fetch_failed, "answered": l.answered,
            "answerFailed": l.answer_failed, "unanswerable": l.unanswerable, "seeds": l.seeds,
            "settled": l.settled, "declined": l.declined,
            "startedUnix": self.started,
            "tickStartedUnix": p.tick_started,
            "lastTickUnix": p.last_tick,
            "lastOkUnix": p.last_ok,
            "lastTickOk": p.last_tick.map(|_| p.failed_in_a_row == 0),
            "failedInARow": p.failed_in_a_row,
            "lastErrorClass": p.last_error_class,
            "everySecs": self.every_secs.load(Ordering::Relaxed),
            "logsReadThrough": self.chain.logs_read_through(),
            "removals": {
                "mode": format!("{:?}", self.store.removals_mode()).to_lowercase(),
                "kept": self.store.learned_count(),
                "neverLearned": r.never_learned,
                "checkedAt": r.block,
            },
        })
    }

    /// The last learn pass's report.
    pub fn learn_last(&self) -> LearnReport {
        self.learn_last
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default()
    }

    /// Where to ask about a dataset the deals `ds` hold: the catalog nodes,
    /// then each deal's source, then its peers (review CAT-LEARN-R1 H1: a
    /// deal's source and peers may hold no catalog, or lie). Without a
    /// snapshot or a deal, the catalog nodes only.
    fn learn_urls(&self, s: Option<&Snapshot>, ds: &[&DealEntry]) -> Vec<String> {
        let mut urls: Vec<String> = Vec::new();
        let mut add = |u: &str| {
            if !u.is_empty() && !urls.iter().any(|x| x == u) {
                urls.push(u.to_string());
            }
        };
        for u in &self.learn.catalog_nodes {
            add(u);
        }
        for d in ds {
            add(&d.source_url);
        }
        if let Some(s) = s {
            for d in ds {
                for u in manifest_sources(s, d, Some(&self.cfg.node)) {
                    add(&u);
                }
            }
        }
        urls
    }

    /// One learn pass (ADR-0059 amendment 4 item 5a slice 2a): read the kept
    /// removals again (even before the first tick: review CAT-LEARN-R2 5),
    /// ask again about every dataset whose newest proof the book withdrew,
    /// then about a few of the datasets the placed deals' held manifests name
    /// ([`learn_order`]: those with no record first), every URL in turn until
    /// one gives a proof that holds (a 404, a 409, a live leaf or a lie never
    /// ends the search); then drop what the book withdrew and nobody gave
    /// again. A held catalog of the publisher never stops the learning:
    /// anyone may hand this node an old anchored one (R2 1). Every chain read
    /// is at one finalized block; nothing is sent. `Err` is a chain that
    /// could not be read: nothing is dropped.
    pub async fn learn_pass(&self) -> Result<LearnReport, String> {
        if self.store.removals_mode() == RemovalsMode::Off {
            return Ok(LearnReport::default());
        }
        let mut pass = self.store.learn_pass_start().await?;
        let re = self.store.learn_recheck(&mut pass).await?;
        let snap = self.snap.lock().ok().and_then(|k| k.clone());
        let mut by: BTreeMap<(String, String), Vec<&DealEntry>> = BTreeMap::new();
        if let Some(s) = &snap {
            let deals: BTreeSet<u64> = self.wanted(s).iter().map(|(d, _)| *d).collect();
            for id in deals {
                let Some(d) = s.deal(id) else { continue };
                let Some((m, _)) = self.store.manifest_record(&d.info.manifest_root) else {
                    continue;
                };
                by.entry((m.header.publisher.0.clone(), m.header.dataset.clone()))
                    .or_default()
                    .push(d);
            }
        }
        let reask = pass.reask();
        let has = |k: &(String, String)| self.store.learned_record(k).ok().flatten().is_some();
        let (learned, unlearned): (Vec<_>, Vec<_>) = by
            .keys()
            .filter(|k| !reask.contains(k))
            .cloned()
            .partition(|k| has(k));
        let turn = self.learn_turn.fetch_add(1, Ordering::Relaxed);
        let order = learn_order(&reask, &unlearned, &learned, turn, LEARN_PER_PASS);
        let get = self.learn.get.clone().unwrap_or_else(|| self.get.clone());
        let mut rep = LearnReport {
            block: pass.block(),
            reasked: reask.len(),
            ..Default::default()
        };
        for key in &order {
            let (p, d) = (&key.0, &key.1);
            rep.asked += 1;
            let ds = by.get(key).map(Vec::as_slice).unwrap_or(&[]);
            for u in self.learn_urls(snap.as_deref(), ds) {
                let url = join(&u, &format!("/v1/bulk/catalogs/{p}/leaf/{d}"));
                let body = match tokio::time::timeout(
                    LEARN_CALL_TIMEOUT,
                    get.get(&url, LEARN_ANSWER_BYTES),
                )
                .await
                {
                    Ok(Ok((200, b))) => b,
                    _ => continue,
                };
                let l = self.store.learn_from(&mut pass, p, d, &body, &u).await?;
                if matches!(l, crate::bulk_serve::Learn::Kept(_)) {
                    rep.learned += 1;
                }
                if l.holds() {
                    break;
                }
            }
        }
        rep.dropped = self.store.learn_finish(&mut pass);
        rep.moved = re.moved;
        let heard = {
            let mut h = self.heard.lock().map_err(|_| "lock")?;
            h.extend(pass.heard.iter().cloned());
            h.clone()
        };
        rep.kept = self.store.learned_count();
        rep.never_learned = by.keys().filter(|k| !heard.contains(*k) && !has(k)).count();
        if let Ok(mut l) = self.learn_last.lock() {
            *l = rep.clone();
        }
        Ok(rep)
    }
}

/// The datasets a learn pass asks about, in order (review CAT-LEARN-R2 5):
/// every one in `reask` (the book withdrew its newest proof this pass), then
/// up to `per` others: datasets with no record first, keeping one place for
/// one with a record (so a removal still moves to newer catalogs and keys)
/// when there is one; each group takes turns by `pass`, a window a pass.
pub(crate) fn learn_order<K: Clone + Ord>(
    reask: &[K],
    unlearned: &[K],
    learned: &[K],
    pass: usize,
    per: usize,
) -> Vec<K> {
    let mut out: Vec<K> = Vec::new();
    let push = |k: &K, out: &mut Vec<K>| {
        if !out.contains(k) {
            out.push(k.clone());
        }
    };
    for k in reask {
        push(k, &mut out);
    }
    let one = usize::from(!learned.is_empty() && per > 0);
    let nu = unlearned.len().min(per.saturating_sub(one));
    let nl = learned.len().min(per - nu);
    for i in 0..nu {
        push(
            &unlearned[(pass.wrapping_mul(nu).wrapping_add(i)) % unlearned.len()],
            &mut out,
        );
    }
    for i in 0..nl {
        push(
            &learned[(pass.wrapping_mul(nl).wrapping_add(i)) % learned.len()],
            &mut out,
        );
    }
    out
}

/// Spacing for [`run`]: a xorshift64* stream seeded from the node id, the
/// clock and the process id. Not for anything secret: it only keeps nodes
/// that started together from staying in step.
pub(crate) struct Jitter(u64);

impl Jitter {
    pub(crate) fn new(node: &NodeId) -> Self {
        let mut b = [0u8; 8];
        b.copy_from_slice(&node.0 .0[..8]);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let x = u64::from_be_bytes(b) ^ nanos ^ u64::from(std::process::id()).rotate_left(32);
        Jitter(if x == 0 { 0x9E37_79B9_7F4A_7C15 } else { x })
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A random time in `[0, d)` (0 when `d` is under a millisecond).
    pub(crate) fn part(&mut self, d: Duration) -> Duration {
        let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        if ms == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(self.next() % ms)
    }

    /// `d` plus or minus up to a quarter of `d`.
    pub(crate) fn around(&mut self, d: Duration) -> Duration {
        let q = d / 4;
        d - q + self.part(q * 2)
    }
}

/// The waits of [`run`] for this node: first a random part of `every`, then
/// `every` give or take a random quarter, for ever.
pub(crate) fn pass_waits(node: &NodeId, every: Duration) -> impl Iterator<Item = Duration> {
    let mut j = Jitter::new(node);
    let first = j.part(every);
    std::iter::once(first).chain(std::iter::repeat_with(move || j.around(every)))
}

/// Run the loop about every `every` until the process stops (errors are
/// logged and retried next pass). The first pass waits a random part of
/// `every` and each wait is `every` give or take a random quarter, so nodes
/// started together on one host do not stay in step: a pass cut short by
/// HTTP 429 is quick, and five nodes started in the same second kept meeting
/// the public endpoint's rate limit together for a whole epoch (THREATS
/// STORAGE-S16-R10).
pub async fn run(node: Arc<StorageNode>, every: Duration) {
    node.every_secs.store(every.as_secs(), Ordering::Relaxed);
    let mut waits = pass_waits(&node.cfg.node, every);
    tokio::time::sleep(waits.next().unwrap_or(every)).await;
    loop {
        match node.tick().await {
            Ok(r) => {
                if r.fetched > 0 || r.answered > 0 || r.settled > 0 || r.seeds > 0 {
                    eprintln!(
                        "onyx-node: storage: epoch {} fetched {} answered {} settled {} seeds {} held {}/{}",
                        r.epoch, r.fetched, r.answered, r.settled, r.seeds, r.held, r.wanted
                    );
                }
            }
            Err(e) => eprintln!("onyx-node: storage: tick: {e}"),
        }
        tokio::time::sleep(waits.next().unwrap_or(every)).await;
    }
}

/// Run the learn pass (ADR-0059 amendment 4 item 5a slice 2a) about every
/// `every` (`ONYX_STORAGE_REMOVALS_EVERY_SECS`), beside [`run`] and never
/// delaying its tick, until the process stops; errors are logged and retried
/// next pass.
pub async fn learn_run(node: Arc<StorageNode>, every: Duration) {
    let mut waits = pass_waits(&node.cfg.node, every);
    tokio::time::sleep(waits.next().unwrap_or(every)).await;
    loop {
        match node.learn_pass().await {
            Ok(r) if r.learned > 0 || r.dropped > 0 => eprintln!(
                "onyx-node: storage: removals: block {} asked {} learned {} dropped {} kept {} never learned {}",
                r.block, r.asked, r.learned, r.dropped, r.kept, r.never_learned
            ),
            Ok(_) => {}
            Err(e) => eprintln!("onyx-node: storage: removals: {e}"),
        }
        tokio::time::sleep(waits.next().unwrap_or(every)).await;
    }
}

/// The challenges of epoch `e` nobody answered yet: (deal, slot, node).
/// Empty when the epoch has no seed or the chain's node list of `e - 1` is
/// not the one the logs give.
pub async fn open_asks(
    chain: &StorageChain,
    s: &Snapshot,
    e: u64,
) -> Result<Vec<(u64, u32, NodeId)>, String> {
    let mut out = Vec::new();
    if e == 0 {
        return Ok(out);
    }
    let seed = chain.seed_of(e).await?;
    if seed == Hash32::ZERO || set_hash(&s.list_at(e - 1)) != chain.set_hash_at(e - 1).await? {
        return Ok(out);
    }
    for d in s
        .deals
        .iter()
        .filter(|d| d.info.start_epoch <= e && e < d.info.end_epoch)
    {
        for slot in 0..s.params.money.challenges_per_deal {
            for node in s.challenged(d, e, &seed, slot).1 {
                if chain.slot_status(d.id, e, slot, &node).await? == 0 {
                    out.push((d.id, slot, node));
                }
            }
        }
    }
    Ok(out)
}

/// The accounts of epoch `e` (R-STOR-10): every challenge the placement
/// implies with its status, and per node what it was asked, passed,
/// failed and missed, earned and lost (from the `Answered` and `Slashed`
/// events of that epoch).
pub async fn epoch_report(chain: &StorageChain, s: &Snapshot, e: u64) -> Result<Value, String> {
    use onyx_storage::epoch::{EpochAccounts, Outcome};
    let seed = chain.seed_of(e).await?;
    if seed == Hash32::ZERO || e == 0 {
        return Ok(json!({"epoch": e, "seed": null, "asks": [], "lines": []}));
    }
    let ask = |id: u64, info: &DealInfo| onyx_storage::epoch::DealAsk {
        id,
        salt: info.salt,
        shard_count: info.shard_count,
        r: info.r,
    };
    let active = |info: &DealInfo| info.start_epoch <= e && e < info.end_epoch;
    let mut deals: Vec<onyx_storage::epoch::DealAsk> = s
        .deals
        .iter()
        .filter(|d| active(&d.info))
        .map(|d| ask(d.id, &d.info))
        .collect();
    // The snapshot leaves closed deals out (a node never asks about them
    // again), but their past epochs had checks: read them here, so the report
    // of an epoch is the same before and after its deals close (seen on the
    // S16 run: epoch 9 read empty once both deals were closed).
    let listed: BTreeSet<u64> = s.deals.iter().map(|d| d.id).collect();
    for id in (0..s.deal_logs).filter(|id| !listed.contains(id)) {
        let info = chain.deal_info(id).await?;
        if info.closed && active(&info) && info.shard_count <= onyx_bulk::manifest::MAX_SHARDS {
            deals.push(ask(id, &info));
        }
    }
    deals.sort_by_key(|d| d.id);
    let asks = onyx_storage::epoch::expected(
        e,
        &seed,
        &deals,
        s.params.money.challenges_per_deal,
        &s.list_at(e - 1),
        &s.excl,
        &s.jailed,
    );
    let mut acc = EpochAccounts::new(&asks);
    let mut paid: BTreeMap<(u64, u32, NodeId), u128> = BTreeMap::new();
    // Answers and slashes of epoch e are sent from its first block on: the
    // report reads from there, not from the deploy block (THREATS STORAGE-S15-1).
    let start = s.params.genesis + e * s.params.epoch_blocks;
    for l in chain.logs_from(events::ANSWERED, start, s.block).await? {
        let epoch = l
            .topics
            .get(2)
            .map(|t| u64::from_be_bytes(t.0[24..].try_into().unwrap_or([0; 8])));
        if epoch != Some(e) {
            continue;
        }
        let deal = l.topics.get(1).map_or(0, |t| {
            u64::from_be_bytes(t.0[24..].try_into().unwrap_or([0; 8]))
        });
        let node = NodeId(l.topics.get(3).copied().unwrap_or(Hash32::ZERO));
        // Data: slot, shard index, passed, paid.
        let slot = abi::word_u64(&l.data, 0).map_err(|x| x.to_string())? as u32;
        let amount = abi::word_uint(&l.data, 3).map_err(|x| x.to_string())?;
        paid.insert((deal, slot, node), amount);
    }
    let mut slashed: BTreeMap<(u64, u32, NodeId), u128> = BTreeMap::new();
    for l in chain.logs_from(events::SLASHED, start, s.block).await? {
        if abi::word_u64(&l.data, 0).map_err(|x| x.to_string())? != e {
            continue;
        }
        let node = NodeId(l.topics.get(1).copied().unwrap_or(Hash32::ZERO));
        let deal = l.topics.get(2).map_or(0, |t| {
            u64::from_be_bytes(t.0[24..].try_into().unwrap_or([0; 8]))
        });
        let slot = abi::word_u64(&l.data, 1).map_err(|x| x.to_string())? as u32;
        let amount = abi::word_uint(&l.data, 3).map_err(|x| x.to_string())?;
        slashed.insert((deal, slot, node), amount);
    }
    let mut list = Vec::new();
    for a in &asks {
        let st = chain.slot_status(a.deal, e, a.slot, &a.node).await?;
        let k = (a.deal, a.slot, a.node);
        let o = match st {
            1 => Some(Outcome::Passed(paid.get(&k).copied().unwrap_or(0))),
            2 => Some(Outcome::Failed(slashed.get(&k).copied().unwrap_or(0))),
            3 => Some(Outcome::Missed(slashed.get(&k).copied().unwrap_or(0))),
            _ => None,
        };
        if let Some(o) = o {
            acc.record(a.deal, a.slot, &a.node, o);
        }
        let status = ["open", "passed", "failed", "missed"]
            .get(usize::from(st))
            .copied()
            .unwrap_or("?");
        list.push(json!({"deal": a.deal, "slot": a.slot, "shard": a.index,
                         "node": a.node.to_string(), "status": status}));
    }
    let lines: Vec<Value> = acc
        .lines()
        .iter()
        .map(|(n, l)| {
            json!({"node": n.to_string(), "asked": l.asked, "passed": l.passed, "failed": l.failed,
                   "missed": l.missed, "earned": l.earned.to_string(), "slashed": l.slashed.to_string()})
        })
        .collect();
    Ok(
        json!({"epoch": e, "seed": seed.to_string(), "asks": list, "open": acc.open().len(), "lines": lines}),
    )
}

/// Settle every unanswered challenge of epoch `e` (after it ended, within
/// `SETTLE_EPOCHS`): the number settled.
pub async fn settle_epoch(
    chain: &StorageChain,
    tx: &dyn TxSend,
    s: &Snapshot,
    e: u64,
) -> Result<usize, String> {
    let prev = s.list_at(e.saturating_sub(1));
    let mut n = 0;
    for (deal, slot, node) in open_asks(chain, s, e).await? {
        match tx
            .send(
                &chain.contract(),
                abi::settle_missed(deal, e, slot, &node, &prev),
            )
            .await
        {
            Ok(_) => n += 1,
            Err(err) => eprintln!("settle deal {deal} epoch {e} slot {slot} node {node}: {err}"),
        }
    }
    Ok(n)
}

/// A client read's result: rows checked against the deal's anchored
/// manifest, and which node served each part.
#[derive(Clone, Debug, Default)]
pub struct ReadResult {
    /// (shard, index, key, row bytes), in key order.
    pub rows: Vec<(u32, u64, i128, Vec<u8>)>,
    /// (shard, URL) that served it.
    pub served_by: Vec<(u32, String)>,
    /// (URL, why) of every node that failed or was refused.
    pub refused: Vec<(String, String)>,
}

/// Why a client read failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// No such deal.
    #[error("no deal {0}")]
    NoDeal(u64),
    /// The chain could not be read.
    #[error("chain: {0}")]
    Chain(String),
    /// Every replica (and the publisher) failed or was refused for a shard.
    #[error("no replica served shard {shard} with rows that check: {why}")]
    Unavailable {
        /// Shard index.
        shard: u32,
        /// Every refusal.
        why: String,
    },
}

/// Read every row with a key in `[from, to]` of deal `deal_id`. For each
/// shard whose key range overlaps, fetch the whole shard file from its
/// replicas in turn (current holders, then last and next epoch's, then the
/// publisher) and accept it only when it checks against the deal's own
/// anchored manifest entry: schema, every row's Merkle root, row count and
/// key range (`check_shard`). The rows in range are then cut from a file
/// that is provably the whole shard, so nothing can be left out or added
/// (review STORAGE-R1-4, R1-5: a range answer proves its rows, not that the
/// range is complete). A node that is down, lacks the shard or serves other
/// bytes is skipped (THREATS STORAGE-8); none left: unavailable.
pub async fn read_rows(
    chain: &StorageChain,
    get: &dyn HttpGet,
    deal_id: u64,
    from: i128,
    to: i128,
) -> Result<ReadResult, ReadError> {
    let s = chain.snapshot().await.map_err(ReadError::Chain)?;
    let d = s.deal(deal_id).ok_or(ReadError::NoDeal(deal_id))?.clone();
    let all = manifest_sources(&s, &d, None);
    let (m, _) = fetch_manifest(get, &all, &d.info.manifest_root)
        .await
        .map_err(|why| ReadError::Unavailable {
            shard: u32::MAX,
            why,
        })?;
    let schema = m.header.schema.clone();
    let mut out = ReadResult::default();
    for e in m
        .shards
        .iter()
        .filter(|e| e.key_max >= from && e.key_min <= to)
    {
        let mut why = Vec::new();
        let mut done = false;
        for u in sources(&s, &d, e.index, None) {
            let url = join(&u, &format!("/v1/bulk/shards/{}", e.root));
            let checked = match get.get(&url, e.bytes as usize).await {
                Ok((200, body)) => onyx_bulk::shard::check_shard(&schema, &body)
                    .map_err(|err| format!("refused: {err}"))
                    .and_then(|c| {
                        let x = c.summary;
                        if x.root != e.root
                            || x.rows != e.rows
                            || x.bytes != e.bytes
                            || x.key_min != e.key_min
                            || x.key_max != e.key_max
                        {
                            return Err("refused: not the deal's shard".to_string());
                        }
                        Ok(c.rows
                            .iter()
                            .zip(&c.keys)
                            .enumerate()
                            .filter(|(_, (_, k))| **k >= from && **k <= to)
                            .map(|(i, (r, k))| (e.index, i as u64, *k, r.to_vec()))
                            .collect::<Vec<_>>())
                    }),
                Ok((st, _)) => Err(format!("HTTP {st}")),
                Err(err) => Err(err),
            };
            match checked {
                Ok(rows) => {
                    out.rows.extend(rows);
                    out.served_by.push((e.index, u.clone()));
                    done = true;
                    break;
                }
                Err(err) => {
                    why.push(format!("{u}: {err}"));
                    out.refused.push((u.clone(), err));
                }
            }
        }
        if !done {
            return Err(ReadError::Unavailable {
                shard: e.index,
                why: why.join("; "),
            });
        }
    }
    out.rows.sort_by_key(|a| (a.2, a.0, a.1));
    Ok(out)
}

/// `HttpGet` over hyper (https, or http to loopback only, as every peer URL
/// the node calls), User-Agent `onyx-dev`, a deadline per call, no redirects.
#[cfg(feature = "http-client")]
pub struct HyperGet {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Empty<hyper::body::Bytes>,
    >,
    timeout: Duration,
}

#[cfg(feature = "http-client")]
impl HyperGet {
    /// A client whose calls give up after `timeout`.
    pub fn new(timeout: Duration) -> Self {
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        HyperGet {
            client: hyper_util::client::legacy::Client::builder(
                hyper_util::rt::TokioExecutor::new(),
            )
            .build(https),
            timeout,
        }
    }

    async fn fetch(&self, url: &str, max: usize) -> Result<(u16, Vec<u8>), String> {
        use http_body_util::BodyExt as _;
        // The scheme and host rule of every outbound URL, on the part before
        // the query (a rows read carries `?from=&to=`).
        if url.contains('#') {
            return Err("peer URL with a fragment".into());
        }
        let base = url.split('?').next().unwrap_or(url);
        crate::x402::check_facilitator_url(base)
            .map_err(|e| e.replacen("facilitator", "peer", 1))?;
        let req = hyper::Request::get(url)
            .header("user-agent", "onyx-dev")
            .body(http_body_util::Empty::new())
            .map_err(|e| e.to_string())?;
        let r = self
            .client
            .request(req)
            .await
            .map_err(|e| format!("peer unreachable: {e}"))?;
        let status = r.status().as_u16();
        let bytes = http_body_util::Limited::new(r.into_body(), max.max(1))
            .collect()
            .await
            .map_err(|e| format!("peer answer unreadable or over {max} bytes: {e}"))?
            .to_bytes();
        Ok((status, bytes.to_vec()))
    }
}

#[cfg(feature = "http-client")]
impl HttpGet for HyperGet {
    fn get<'a>(
        &'a self,
        url: &'a str,
        max: usize,
    ) -> BoxFuture<'a, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            tokio::time::timeout(self.timeout, self.fetch(url, max))
                .await
                .map_err(|_| "peer call timed out".to_string())?
        })
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
