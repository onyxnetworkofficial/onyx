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

use crate::bulk_serve::BulkStore;
use crate::roots::JsonRpc;
use crate::x402::BoxFuture;
use onyx_bulk::manifest::Manifest;
use onyx_bulk::wire::{hex0x, hex_field};
use onyx_core::{Hash32, SignedReport};
use onyx_storage::abi::{self, events, DealInfo, NodeInfo};
use onyx_storage::challenge;
use onyx_storage::ids::{set_hash, shard_key, slot_shard, NodeId};
use onyx_storage::placement::{challenged, holders, holders_at, Exclusions, Jailed, WHOLE_DEAL};
use onyx_storage::repair::DealPlace;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    cache: Mutex<ChainCache>,
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
    anchor: Option<(u64, Hash32)>,
    closed: BTreeSet<u64>,
}

/// The last epoch each node passed a check on each shard: (deal, shard, node)
/// -> epoch, folded from `Answered` logs as they are read. Raw `Answered` logs
/// are never kept: there are a few per deal every epoch, and keeping them made
/// the cache grow without bound and a first sync quadratic (review
/// STORAGE-S16-R7 M1).
type Passes = BTreeMap<(u64, u32, NodeId), u64>;

/// Split `logs` into the ones to keep and the passes they hold (`Answered`
/// with `passed`), in chain order.
fn fold_logs(logs: Vec<Log>, keep: &mut Vec<Log>, passes: &mut Passes) -> Result<(), String> {
    let answered = abi::topic(events::ANSWERED);
    for l in logs {
        if l.topics.first() != Some(&answered) {
            keep.push(l);
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
        let e = passes.entry((deal, index, node)).or_insert(epoch);
        *e = (*e).max(epoch);
    }
    Ok(())
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
            cache: Mutex::new(ChainCache::default()),
            rpc_errors: AtomicUsize::new(0),
        }
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
    /// `Answered`), so saving it after each range stays cheap.
    async fn cached_logs(&self, to: u64, safe: u64) -> Result<(Vec<Log>, Passes), String> {
        let (mut next, mut out, mut passes) = self
            .cache
            .lock()
            .ok()
            .and_then(|c| c.logs.clone())
            .unwrap_or((self.from_block, Vec::new(), Passes::new()));
        let safe = safe.min(to);
        while next <= safe {
            let end = next.saturating_add(LOG_RANGE - 1).min(safe);
            let got = self.logs_range(&SNAPSHOT_EVENTS, next, end).await?;
            fold_logs(got, &mut out, &mut passes)?;
            next = end + 1;
            if let Ok(mut c) = self.cache.lock() {
                c.logs = Some((next, out.clone(), passes.clone()));
            }
        }
        if next <= to {
            let got = self.logs_range(&SNAPSHOT_EVENTS, next, to).await?;
            fold_logs(got, &mut out, &mut passes)?;
        }
        Ok((out, passes))
    }

    /// The number and hash of block `tag` (`"safe"` or a `0x` number).
    async fn block_id(&self, tag: &str) -> Result<(u64, Hash32), String> {
        let b = self
            .rpc_call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        Ok((qty(&b["number"])?, h32(&b["hash"])?))
    }

    /// Drop every kept log (the chain changed under them, or they disagree
    /// with the contract's own counts).
    fn forget_logs(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.logs = None;
            c.anchor = None;
        }
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
            why = w;
            self.forget_logs();
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
        let anchor = self.cache.lock().ok().and_then(|c| c.anchor);
        if let Some((n, h)) = anchor {
            if self.block_id(&format!("0x{n:x}")).await?.1 != h {
                self.forget_logs();
            }
        }
        let (fin, fin_hash) = self.block_id("safe").await?;
        let head = self.block_number().await?;
        let epoch = self.current_epoch().await?;
        let params = self.params().await?;
        let err = |e: abi::AbiError| e.to_string();
        let (all, passes) = self.cached_logs(head, fin).await?;
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
        // The raw records behind `exclusionFrom` (ADR-0060 amendment 2 item
        // 2): slashes, declines and passes, in any order.
        let mut excl = Exclusions::new();
        for l in of(events::SLASHED) {
            let node = NodeId(*l.topics.get(1).ok_or("Slashed topic")?);
            let deal = topic_u64(l, 2, "Slashed topic")?;
            let index = word_u32(l, 2, "Slashed index")?;
            let from = abi::word_u64(&l.data, 4).map_err(err)?;
            excl.slash(deal, index, node, from);
        }
        for l in of(events::DECLINED) {
            let node = NodeId(*l.topics.get(1).ok_or("Declined topic")?);
            let deal = topic_u64(l, 2, "Declined topic")?;
            let index = word_u32(l, 0, "Declined index")?;
            let from = abi::word_u64(&l.data, 1).map_err(err)?;
            excl.decline(deal, index, node, from);
        }
        for ((deal, index, node), epoch) in &passes {
            excl.pass(*deal, *index, *node, *epoch);
        }
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
        if let Ok(mut c) = self.cache.lock() {
            c.anchor = Some((fin, fin_hash));
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
    /// Shards given up because no source served them.
    pub declined: usize,
}

/// A node's placement (its (deal, shard) pairs) and the inputs it came from.
type Placed = (PlacementKey, Vec<(u64, u32)>);

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
        }
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
    /// placed here within [`FETCH_BUDGET`].
    pub async fn tick(&self) -> Result<TickReport, String> {
        // Nothing is sent to a contract of another version (no seed, fetch,
        // answer, settle or decline): it would read our answers as wrong.
        if !self.version_ok.load(Ordering::Relaxed) {
            self.chain.check_version().await?;
            self.version_ok.store(true, Ordering::Relaxed);
        }
        let s = self.chain.snapshot().await?;
        let mut rep = TickReport {
            epoch: s.epoch,
            ..Default::default()
        };
        // The seed of this epoch (anyone may record it; the first wins).
        let e = s.epoch;
        let mut seed = self.chain.seed_of(e).await?;
        if seed == Hash32::ZERO {
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
                    Err(err) => eprintln!(
                        "onyx-node: storage: answer deal {} slot {slot}: {err}",
                        d.id
                    ),
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

    /// The status JSON the operator CLI and `/v1/health` show.
    pub fn status_json(&self) -> Value {
        let l = self.last();
        json!({
            "node": self.cfg.node.to_string(), "epoch": l.epoch, "wanted": l.wanted, "held": l.held,
            "fetched": l.fetched, "fetch_failed": l.fetch_failed, "answered": l.answered,
            "seeds": l.seeds, "settled": l.settled, "unanswerable": l.unanswerable,
            "declined": l.declined,
        })
    }
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

/// Run the loop about every `every` until the process stops (errors are
/// logged and retried next pass). The first pass waits a random part of
/// `every` and each wait is `every` give or take a random quarter, so nodes
/// started together on one host do not stay in step: a pass cut short by
/// HTTP 429 is quick, and five nodes started in the same second kept meeting
/// the public endpoint's rate limit together for a whole epoch (THREATS
/// STORAGE-S16-R10).
pub async fn run(node: Arc<StorageNode>, every: Duration) {
    let mut jitter = Jitter::new(&node.cfg.node);
    tokio::time::sleep(jitter.part(every)).await;
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
        tokio::time::sleep(jitter.around(every)).await;
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
    let deals: Vec<onyx_storage::epoch::DealAsk> = s
        .deals
        .iter()
        .filter(|d| d.info.start_epoch <= e && e < d.info.end_epoch)
        .map(|d| onyx_storage::epoch::DealAsk {
            id: d.id,
            salt: d.info.salt,
            shard_count: d.info.shard_count,
            r: d.info.r,
        })
        .collect();
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
