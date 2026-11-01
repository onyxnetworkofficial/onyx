//! The reporter loop (REV-11, ADR-0025 item 2): posts the receipt log to
//! `OnyxRevenue.credit` in bounded batches with the operator (reporter) key.
//!
//! One post: resolve the self-settles still pending (their proven receipts
//! join the log); advance every `credit` already in flight, lowest nonce
//! first; then, while fewer than [`DEFAULT_MAX_IN_FLIGHT`] are in flight,
//! take the oldest due receipts not already in one ([`ReceiptLog::peek`]),
//! check once that the revenue address holds code, and send a new `credit`
//! (the key is held only while sending). A batch whose `credit` is mined but
//! not yet as final as [`Confirm`] asks (`finalized` by default, REV-L3-F8)
//! no longer holds up the next batch (REV-L4-F3).
//!
//! A `credit` still unmined is handed to the node again as signed on every
//! post (a node that dropped it takes it back), and the lowest unmined one is
//! replaced at the same nonce with higher fees after a while, up to a cap.
//! Past the last allowed bump and [`DEFAULT_GIVE_UP`] it is reported stuck
//! (`last_error`, `stuck_since` in [`ReporterStatus`], on `/v1/health`), new
//! batches wait, and the [`Sender`] refuses new sends behind it (REV-L4-F1).
//! With [`Reporter::with_state_file`] the `credit`s in flight (nonce, fees,
//! signed transactions) are kept on disk, so a restart resumes them, bumps
//! the stuck one, and never sends a second batch at a later nonce behind it.
//!
//! Once mined and final, the transaction's own `Credited` and
//! `ReceiptSkipped` events are read, the batch is committed, and the
//! receipts skipped for a reason that can clear (why 2, why 4) or not
//! mentioned by any event go back in the log to wait ([`Retry`], REV-L3-F2,
//! REV-L3-F3, REV-L4-F5). A shard skipped as unregistered is registered when
//! a [`Registrar`] can build the call. A send that failed or reverted
//! commits nothing, and a later post sends the same batch again.
//!
//! Idempotent across restarts and lost answers: the contract keys each
//! receipt by (payer, nonce) in `receiptUsed`, so a batch that landed on chain
//! but was not committed here (a crash, an unmined-looking transaction) is
//! skipped there with why 1 on the next post and then removed. The chain is
//! the source of truth; the log only says what is still worth posting.

use crate::evm_tx::{SendError, Sender, Sent};
use crate::journal::{Binding, Journal};
use crate::receipts::{now_unix, Receipt, ReceiptLog, Retry};
use crate::roots::JsonRpc;
use crate::self_settle::SelfSettle;
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `OnyxRevenue.MAX_RECEIPTS`: the most receipts one `credit` call takes.
pub const MAX_CREDIT_BATCH: usize = 64;
/// The longest wait between posts after repeated failures.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// `credit` transactions in flight at once (REV-L4-F3): a batch waiting to
/// be final no longer holds up the next.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 4;
/// How long an unmined `credit` may stay past its last fee bump before the
/// reporter reports it stuck (REV-L4-F1).
pub const DEFAULT_GIVE_UP: Duration = Duration::from_secs(30 * 60);
/// The journal kind of a reporter's in-flight `credit`s.
pub const CREDITS_JOURNAL_KIND: &str = "onyx-credits-in-flight";

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes()).0;
    [h[0], h[1], h[2], h[3]]
}

fn word(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// `OnyxRevenue.credit((uint64,bytes32,uint128,address,bytes32)[])`.
pub fn credit_calldata(batch: &[Receipt]) -> Vec<u8> {
    let mut d = selector("credit((uint64,bytes32,uint128,address,bytes32)[])").to_vec();
    // One dynamic argument: its offset, then the array (length, static tuples).
    d.extend_from_slice(&word(32));
    d.extend_from_slice(&word(batch.len() as u128));
    for r in batch {
        d.extend_from_slice(&word(u128::from(r.round)));
        d.extend_from_slice(&r.shard.0);
        d.extend_from_slice(&word(u128::from(r.amount)));
        d.extend_from_slice(&word_addr(&r.payer));
        d.extend_from_slice(&r.nonce);
    }
    d
}

/// `keccak256("Credited(uint64,bytes32,uint128,address,bytes32)")`.
pub fn credited_topic() -> [u8; 32] {
    keccak256(b"Credited(uint64,bytes32,uint128,address,bytes32)").0
}

/// `keccak256("ReceiptSkipped(uint64,bytes32,address,bytes32,uint8)")`.
pub fn skipped_topic() -> [u8; 32] {
    keccak256(b"ReceiptSkipped(uint64,bytes32,address,bytes32,uint8)").0
}

/// How sure the reporter must be that a `credit` transaction stays on chain
/// before it removes its batch from the log (REV-L3-F8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Mined once (local chains and tests: nothing reorgs there).
    Latest,
    /// At or below the node's `safe` block.
    Safe,
    /// At or below the node's `finalized` block (the default; on Base
    /// Sepolia, once the L2 block is final on L1).
    Finalized,
}

impl Confirm {
    fn tag(self) -> Option<&'static str> {
        match self {
            Confirm::Latest => None,
            Confirm::Safe => Some("safe"),
            Confirm::Finalized => Some("finalized"),
        }
    }
}

/// Calldata for `OnyxRevenue.register(round, shard, fields)` for a shard,
/// if the operator can build it (it knows the shard's commitment fields):
/// the reporter registers a shard whose receipts were skipped with why 4,
/// once it is final, before posting them again (REV-L3-F3).
pub type Registrar = Arc<dyn Fn(u64, Hash32) -> Option<Vec<u8>> + Send + Sync>;

/// What one post did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Posted {
    /// Pending self-settles resolved into receipts before the post.
    pub resolved: usize,
    /// Receipts in the `credit` calls this post sent or looked at (0: none).
    pub posted: usize,
    /// Receipts the contract credited.
    pub applied: usize,
    /// Receipts it skipped, with `why` (1 already credited, 2 payment not
    /// seen as used, 3 more than unallocated, 4 shard not registered).
    pub skipped: Vec<(Receipt, u8)>,
    /// Skipped or unaccounted receipts put back in the log for a later post.
    pub requeued: usize,
    /// Receipts of a batch the transaction's events did not mention: put
    /// back with the long [`Retry::Unaccounted`] wait (REV-L3-F2, REV-L4-F5).
    pub unaccounted: usize,
    /// Shards this post registered (why-4 receipts, with a [`Registrar`]).
    pub registered: usize,
    /// A `credit` is sent but not mined, or mined but not yet as final as
    /// [`Confirm`] asks: its batch stays in the log.
    pub waiting: bool,
    /// A `credit` was replaced with higher fees this post.
    pub bumped: bool,
    /// Unmined `credit`s handed to the node again this post (REV-L4-F1).
    pub rebroadcast: usize,
    /// `credit`s whose batch was committed this post.
    pub committed: usize,
    /// `credit`s still in flight after this post.
    pub in_flight: usize,
    /// Unix second of the first send of the oldest `credit` that has stayed
    /// unmined past the bump delay (REV-L4-F1).
    pub stuck_since: Option<u64>,
    /// The nonce of a `credit` past its last bump and the give-up time: the
    /// reporter reports it and sends nothing new until it clears.
    pub gave_up: Option<u64>,
    /// The `credit` transaction last looked at (the version mined, else the
    /// latest sent).
    pub tx: Option<String>,
}

/// Why a post committed nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// The `credit` transaction was not sent or reverted.
    #[error("credit: {0}")]
    Send(#[from] SendError),
    /// Mined, but its events could not be read: read again next post.
    #[error("credit {tx} mined; reading its events failed: {why}")]
    Events {
        /// The mined transaction.
        tx: String,
        /// The node's error.
        why: String,
    },
    /// The configured `OnyxRevenue` address holds no code: a `credit` to it
    /// would "succeed" with no events and nothing credited (REV-L3-F2).
    #[error("no contract at the revenue address {0}")]
    NoContract(String),
}

/// The reporter's running totals, for an operator's status page and
/// `/v1/health`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReporterStatus {
    /// `credit`s final and committed.
    pub posts: u64,
    /// Receipts credited in all.
    pub applied: u64,
    /// `credit`s in flight now.
    pub in_flight: usize,
    /// Unix second since which the oldest unmined `credit` has been stuck
    /// (REV-L4-F1).
    pub stuck_since: Option<u64>,
    /// Failed writes of the in-flight journal (it then runs in memory).
    pub write_errors: u64,
    /// The last error, if the last post failed or a `credit` is stuck past
    /// its last bump.
    pub last_error: Option<String>,
}

/// What `/v1/health` reads from a reporter (REV-L4-F1).
pub trait ReporterHealth: Send + Sync {
    /// The reporter's status now.
    fn reporter_status(&self) -> ReporterStatus;
}

/// A `credit` sent and not yet committed: every version sent (fee bumps
/// replace it at the same nonce) and the batch it carries.
struct InFlight {
    /// Its record in the in-flight journal, if the reporter keeps one.
    id: Option<u64>,
    batch: Vec<Receipt>,
    sent: Vec<Sent>,
    /// When the last version was sent (the bump timer).
    since: tokio::time::Instant,
    /// Unix second of the first send (stuck since, give up).
    first_unix: u64,
}

impl InFlight {
    fn nonce(&self) -> u64 {
        self.sent[0].tx.nonce
    }

    fn last(&self) -> &Sent {
        &self.sent[self.sent.len() - 1]
    }

    fn to_json(&self) -> Value {
        json!({
            "batch": self.batch.iter().map(Receipt::to_json).collect::<Vec<_>>(),
            "sent": self.sent.iter().map(Sent::to_json).collect::<Vec<_>>(),
            "firstUnix": self.first_unix,
        })
    }

    fn from_json(id: u64, v: &Value) -> Option<InFlight> {
        let batch = v["batch"]
            .as_array()?
            .iter()
            .map(Receipt::from_json)
            .collect::<Option<Vec<_>>>()?;
        let sent = v["sent"]
            .as_array()?
            .iter()
            .map(Sent::from_json)
            .collect::<Option<Vec<_>>>()?;
        if batch.is_empty() || sent.is_empty() {
            return None;
        }
        Some(InFlight {
            id: Some(id),
            batch,
            sent,
            // The bump timer restarts with the process.
            since: tokio::time::Instant::now(),
            first_unix: v["firstUnix"].as_u64()?,
        })
    }
}

/// How one in-flight `credit` stands after a look.
enum Step {
    /// Still in flight; `true` if it is unmined.
    Keep(InFlight, bool),
    /// Committed, or gone (its batch is posted again).
    Done,
}

/// Posts the receipt log to `OnyxRevenue.credit`.
pub struct Reporter<T: JsonRpc> {
    sender: Arc<Sender<T>>,
    revenue: [u8; 20],
    log: Arc<ReceiptLog>,
    settle: Option<Arc<SelfSettle<T>>>,
    registrar: Option<Registrar>,
    batch: usize,
    deadline: Duration,
    confirm: Confirm,
    stuck_after: Duration,
    bump_pct: u32,
    max_bumps: usize,
    fee_cap_factor: u128,
    max_in_flight: usize,
    give_up: Duration,
    in_flight: tokio::sync::Mutex<Vec<InFlight>>,
    state: Arc<Mutex<Option<Journal>>>,
    has_code: std::sync::atomic::AtomicBool,
    status: Mutex<ReporterStatus>,
}

impl<T: JsonRpc> std::fmt::Debug for Reporter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reporter")
            .field("sender", &self.sender)
            .field("revenue", &format!("0x{}", hex::encode(self.revenue)))
            .field("batch", &self.batch)
            .field("confirm", &self.confirm)
            .finish_non_exhaustive()
    }
}

fn topic_eq(v: &Value, want: &[u8; 32]) -> bool {
    v.as_str()
        .map(|s| s.to_ascii_lowercase())
        .is_some_and(|s| s == format!("0x{}", hex::encode(want)))
}

fn hex32(v: &Value) -> Option<[u8; 32]> {
    hex::decode(v.as_str()?.strip_prefix("0x")?)
        .ok()?
        .try_into()
        .ok()
}

fn quantity(v: &Value) -> Option<u64> {
    u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok()
}

impl<T: JsonRpc> Reporter<T> {
    /// A reporter sending from `sender` (OnyxRevenue's reporter key) to
    /// `revenue`, posting `log`. Batches of [`MAX_CREDIT_BATCH`], up to
    /// [`DEFAULT_MAX_IN_FLIGHT`] in flight; each new `credit` is waited for up
    /// to 60 s (without holding the key); a batch is committed only at
    /// [`Confirm::Finalized`]; a `credit` still unmined after 120 s is
    /// replaced with 25% higher fees, at most 4 times and never above 4
    /// times the first fee cap, and reported stuck [`DEFAULT_GIVE_UP`] after
    /// its first send once no bump is left.
    pub fn new(sender: Arc<Sender<T>>, revenue: [u8; 20], log: Arc<ReceiptLog>) -> Self {
        Reporter {
            sender,
            revenue,
            log,
            settle: None,
            registrar: None,
            batch: MAX_CREDIT_BATCH,
            deadline: Duration::from_secs(60),
            confirm: Confirm::Finalized,
            stuck_after: Duration::from_secs(120),
            bump_pct: 25,
            max_bumps: 4,
            fee_cap_factor: 4,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            give_up: DEFAULT_GIVE_UP,
            in_flight: tokio::sync::Mutex::new(Vec::new()),
            state: Arc::new(Mutex::new(None)),
            has_code: std::sync::atomic::AtomicBool::new(false),
            status: Mutex::new(ReporterStatus::default()),
        }
    }

    /// The reporter for a self-settling API: the same key, contract and log
    /// as `settle`, which it also asks to resolve its pending settles before
    /// each post.
    pub fn for_self_settle(settle: Arc<SelfSettle<T>>) -> Self {
        let mut r = Reporter::new(settle.shared_sender(), settle.revenue(), settle.receipts());
        r.settle = Some(settle);
        r
    }

    /// Keep the `credit`s in flight in the journal at `path` (created if
    /// missing), bound to this chain and `OnyxRevenue`, and resume those it
    /// holds (REV-L4-F1): after a restart a stuck `credit` is re-broadcast
    /// and bumped at its own nonce, never left behind a new batch. Refuses a
    /// damaged journal, one of another deployment, and one another process
    /// holds.
    pub fn with_state_file(self, path: impl AsRef<Path>) -> std::io::Result<Self> {
        let bind = Binding {
            chain_id: self.sender.chain_id(),
            contract: self.revenue,
        };
        let (journal, live) = Journal::open(path, CREDITS_JOURNAL_KIND, &bind)?;
        let mut flights = Vec::with_capacity(live.len());
        for (id, v) in live {
            flights.push(InFlight::from_json(id, &v).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("credit journal entry {id} is not an in-flight credit"),
                )
            })?);
        }
        flights.sort_by_key(InFlight::nonce);
        if let Ok(mut st) = self.status.lock() {
            st.in_flight = flights.len();
        }
        *self
            .in_flight
            .try_lock()
            .map_err(|_| std::io::Error::other("reporter in use while loading its state"))? =
            flights;
        if let Ok(mut g) = self.state.lock() {
            *g = Some(journal);
        }
        Ok(self)
    }

    /// Receipts per `credit` call, 1 to [`MAX_CREDIT_BATCH`].
    pub fn with_batch(mut self, n: usize) -> Self {
        self.batch = n.clamp(1, MAX_CREDIT_BATCH);
        self
    }

    /// `credit` transactions in flight at once (at least 1).
    pub fn with_max_in_flight(mut self, n: usize) -> Self {
        self.max_in_flight = n.max(1);
        self
    }

    /// How long one post waits for a new transaction to be mined.
    pub fn with_deadline(mut self, d: Duration) -> Self {
        self.deadline = d;
        self
    }

    /// How final a `credit` must be before its batch leaves the log.
    pub fn with_confirm(mut self, c: Confirm) -> Self {
        self.confirm = c;
        self
    }

    /// Replace a `credit` still unmined after `after` with fees raised by
    /// `pct` percent, at most `max` times, the fee cap never above `factor`
    /// times the first one.
    pub fn with_fee_bumps(mut self, after: Duration, pct: u32, max: usize, factor: u128) -> Self {
        self.stuck_after = after;
        self.bump_pct = pct;
        self.max_bumps = max;
        self.fee_cap_factor = factor.max(1);
        self
    }

    /// Report a `credit` stuck once no bump is left and `d` passed since its
    /// first send.
    pub fn with_give_up(mut self, d: Duration) -> Self {
        self.give_up = d;
        self
    }

    /// Register shards whose receipts were skipped as unregistered (why 4).
    pub fn with_registrar(mut self, r: Registrar) -> Self {
        self.registrar = Some(r);
        self
    }

    /// Running totals, what is in flight, and the last error.
    pub fn status(&self) -> ReporterStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// One post (see the module docs). `Ok` with `posted == 0` when there was
    /// nothing to post; `waiting` while a sent `credit` is not final yet.
    pub async fn run_once(&self) -> Result<Posted, ReportError> {
        let out = self.post().await;
        if let Ok(mut st) = self.status.lock() {
            match &out {
                Ok(p) => {
                    st.posts += p.committed as u64;
                    st.applied += p.applied as u64;
                    st.in_flight = p.in_flight;
                    st.stuck_since = p.stuck_since;
                    st.last_error = p.gave_up.map(|n| {
                        format!(
                            "credit at nonce {n} unmined since unix {} after every fee bump; \
                             re-broadcasting it, new batches wait",
                            p.stuck_since.unwrap_or_default()
                        )
                    });
                }
                Err(e) => st.last_error = Some(e.to_string()),
            }
        }
        out
    }

    async fn ensure_code(&self) -> Result<(), ReportError> {
        use std::sync::atomic::Ordering;
        if self.has_code.load(Ordering::Relaxed) {
            return Ok(());
        }
        let addr = format!("0x{}", hex::encode(self.revenue));
        let code = self
            .sender
            .rpc()
            .call("eth_getCode", json!([addr, "latest"]))
            .await
            .map_err(|e| ReportError::Send(SendError::Rpc(e)))?;
        let empty = code
            .as_str()
            .map(|c| c.trim_start_matches("0x").is_empty())
            .unwrap_or(true);
        if empty {
            return Err(ReportError::NoContract(addr));
        }
        self.has_code.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Whether the mined `credit` (its receipt `r`) is as final as
    /// [`Confirm`] asks, and still in the canonical chain.
    async fn is_final(&self, r: &Value) -> Result<bool, String> {
        let Some(tag) = self.confirm.tag() else {
            return Ok(true);
        };
        let Some(block) = quantity(&r["blockNumber"]) else {
            return Ok(false);
        };
        let rpc = self.sender.rpc();
        let head = rpc
            .call("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        if quantity(&head["number"]).is_none_or(|h| h < block) {
            return Ok(false);
        }
        // Reorged out meanwhile: the receipt's block is no longer canonical.
        let at = rpc
            .call(
                "eth_getBlockByNumber",
                json!([format!("0x{block:x}"), false]),
            )
            .await?;
        Ok(at["hash"].is_string() && at["hash"] == r["blockHash"])
    }

    /// Write a change of the in-flight journal (if any) off the async
    /// workers; the new record's id. A failed write closes the journal (its
    /// state on disk is unknown) and is counted; the reporter runs on in
    /// memory.
    async fn save(&self, del: Option<u64>, add: Option<Value>) -> Option<u64> {
        let state = self.state.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut g = state.lock().ok()?;
            let j = g.as_mut()?;
            let del: Vec<u64> = del.into_iter().collect();
            let add: Vec<Value> = add.into_iter().collect();
            match j.apply(&del, &add) {
                Ok(ids) => Some(Ok(ids.first().copied())),
                Err(e) => {
                    *g = None;
                    Some(Err(e))
                }
            }
        })
        .await
        .ok()
        .flatten();
        match res {
            Some(Ok(id)) => id,
            Some(Err(_)) => {
                if let Ok(mut st) = self.status.lock() {
                    st.write_errors += 1;
                }
                None
            }
            None => None,
        }
    }

    async fn post(&self) -> Result<Posted, ReportError> {
        let mut out = Posted::default();
        if let Some(s) = &self.settle {
            out.resolved = s.resolve_pending().await;
        }
        let mut flights = self.in_flight.lock().await;
        let mut unregistered = Vec::new();
        let result = self
            .advance_all(&mut flights, &mut out, &mut unregistered)
            .await;
        out.in_flight = flights.len();
        drop(flights);
        out.registered = self.register_all(unregistered).await;
        result.map(|()| out)
    }

    /// Advance the `credit`s in flight, lowest nonce first, then send a new
    /// batch if there is room.
    async fn advance_all(
        &self,
        flights: &mut Vec<InFlight>,
        out: &mut Posted,
        unregistered: &mut Vec<(u64, Hash32)>,
    ) -> Result<(), ReportError> {
        flights.sort_by_key(InFlight::nonce);
        let mut keep = Vec::with_capacity(flights.len() + 1);
        let mut lowest_unmined = true;
        let mut pending = std::mem::take(flights).into_iter();
        while let Some(f) = pending.next() {
            match self
                .advance(f, out, unregistered, lowest_unmined, true, Duration::ZERO)
                .await
            {
                Ok(Step::Keep(f, unmined)) => {
                    lowest_unmined &= !unmined;
                    keep.push(f);
                }
                Ok(Step::Done) => {}
                Err(e) => {
                    keep.extend(pending);
                    *flights = keep;
                    return Err(e);
                }
            }
        }
        if keep.len() >= self.max_in_flight || out.gave_up.is_some() {
            *flights = keep;
            return Ok(());
        }
        let busy: Vec<Receipt> = keep.iter().flat_map(|f| f.batch.iter().copied()).collect();
        let batch: Vec<Receipt> = self
            .log
            .peek(self.batch + busy.len())
            .into_iter()
            .filter(|r| !busy.contains(r))
            .take(self.batch)
            .collect();
        if batch.is_empty() {
            *flights = keep;
            return Ok(());
        }
        let sent = match self.ensure_code().await {
            Ok(()) => {
                self.sender
                    .broadcast(
                        &self.revenue,
                        &credit_calldata(&batch),
                        0,
                        self.deadline / 2,
                    )
                    .await
            }
            Err(e) => {
                *flights = keep;
                return Err(e);
            }
        };
        let sent = match sent {
            Ok(s) => s,
            Err(e) => {
                *flights = keep;
                return Err(e.into());
            }
        };
        let mut f = InFlight {
            id: None,
            batch,
            sent: vec![sent],
            since: tokio::time::Instant::now(),
            first_unix: now_unix(),
        };
        f.id = self.save(None, Some(f.to_json())).await;
        let res = self
            .advance(f, out, unregistered, lowest_unmined, false, self.deadline)
            .await;
        match res {
            Ok(Step::Keep(f, _)) => keep.push(f),
            Ok(Step::Done) => {}
            Err(e) => {
                *flights = keep;
                return Err(e);
            }
        }
        *flights = keep;
        Ok(())
    }

    /// Drop a `credit` from the in-flight journal.
    async fn forget(&self, f: &InFlight) {
        if let Some(id) = f.id {
            self.save(Some(id), None).await;
        }
    }

    /// One look at one `credit`: commit it once final; re-broadcast and (the
    /// lowest unmined only) bump it while unmined.
    async fn advance(
        &self,
        mut f: InFlight,
        out: &mut Posted,
        unregistered: &mut Vec<(u64, Hash32)>,
        may_bump: bool,
        rebroadcast: bool,
        wait: Duration,
    ) -> Result<Step, ReportError> {
        out.posted += f.batch.len();
        let hashes: Vec<String> = f.sent.iter().map(|s| s.hash.clone()).collect();
        out.tx = hashes.last().cloned();
        let mined = if wait.is_zero() {
            self.sender.mined_now(&hashes).await
        } else {
            self.sender.wait_mined(&hashes, wait).await
        };
        let mined = match mined {
            Ok(m) => m,
            Err(e) => {
                // Reverted: nothing committed; the batch is posted again.
                self.forget(&f).await;
                return Err(e.into());
            }
        };
        let Some((hash, _)) = mined else {
            return Ok(self.unmined(f, out, may_bump, rebroadcast).await);
        };
        out.tx = Some(hash.clone());
        let receipt = self
            .sender
            .rpc()
            .call("eth_getTransactionReceipt", json!([hash]))
            .await
            .map_err(|why| ReportError::Events {
                tx: hash.clone(),
                why,
            })?;
        if receipt.is_null() {
            // Reorged out between the two reads: look again next post.
            out.waiting = true;
            return Ok(Step::Keep(f, false));
        }
        match self.is_final(&receipt).await {
            Ok(true) => {}
            Ok(false) => {
                out.waiting = true;
                return Ok(Step::Keep(f, false));
            }
            Err(why) => return Err(ReportError::Events { tx: hash, why }),
        }
        let batch = std::mem::take(&mut f.batch);
        let revenue = format!("0x{}", hex::encode(self.revenue));
        let mut seen = vec![false; batch.len()];
        let mut retry = Vec::new();
        for l in receipt["logs"].as_array().into_iter().flatten() {
            if l["address"]
                .as_str()
                .map(str::to_ascii_lowercase)
                .as_deref()
                != Some(&revenue)
            {
                continue;
            }
            let topics = l["topics"].as_array().cloned().unwrap_or_default();
            let data = hex::decode(
                l["data"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            let (Some(t0), Some(payer)) = (topics.first(), topics.get(3).and_then(hex32)) else {
                continue;
            };
            let mut p20 = [0u8; 20];
            p20.copy_from_slice(&payer[12..]);
            // Credited: data = amount, nonce. Skipped: data = nonce, why.
            let (nonce, why) = if topic_eq(t0, &credited_topic()) && data.len() == 64 {
                (&data[32..64], 0u8)
            } else if topic_eq(t0, &skipped_topic()) && data.len() == 64 {
                (&data[..32], data[63])
            } else {
                continue;
            };
            let Some(i) = batch
                .iter()
                .enumerate()
                .position(|(i, r)| !seen[i] && r.payer == p20 && r.nonce[..] == *nonce)
            else {
                continue;
            };
            seen[i] = true;
            if why == 0 {
                out.applied += 1;
            } else {
                out.skipped.push((batch[i], why));
                if let Some(kind) = Retry::for_why(why) {
                    retry.push((batch[i], kind));
                    if kind == Retry::Unregistered {
                        unregistered.push((batch[i].round, batch[i].shard));
                    }
                }
            }
        }
        // A receipt the events never mention was not handled by the
        // contract: it goes back with the long wait (REV-L3-F2, REV-L4-F5),
        // and is dropped and counted only after its last post.
        for (i, r) in batch.iter().enumerate() {
            if !seen[i] {
                out.unaccounted += 1;
                retry.push((*r, Retry::Unaccounted));
            }
        }
        let log = self.log.clone();
        let committed =
            tokio::task::spawn_blocking(move || log.commit_with_retry(&batch, &retry)).await;
        out.requeued += committed.map(|c| c.1).unwrap_or(0);
        out.committed += 1;
        self.forget(&f).await;
        Ok(Step::Done)
    }

    /// An unmined `credit`: gone if a later nonce was mined instead (its
    /// batch is posted again, and the contract skips anything it already
    /// credited); else handed to the node again, bumped if it is the lowest
    /// unmined one and stuck, and reported once no bump is left.
    async fn unmined(
        &self,
        mut f: InFlight,
        out: &mut Posted,
        may_bump: bool,
        rebroadcast: bool,
    ) -> Step {
        let nonce = f.nonce();
        if self
            .sender
            .nonce_at("latest")
            .await
            .is_ok_and(|n| n > nonce)
        {
            self.forget(&f).await;
            return Step::Done;
        }
        out.waiting = true;
        if rebroadcast && self.sender.rebroadcast(f.last()).await.is_ok() {
            out.rebroadcast += 1;
        }
        let cap = f.sent[0]
            .tx
            .max_fee_per_gas
            .saturating_mul(self.fee_cap_factor);
        // As [`Sender::replace`] prices a bump: at least 13% more, plus 1.
        let pct = u128::from(self.bump_pct.max(13));
        let bump_left = |f: &InFlight| {
            let next = f.last().tx.max_fee_per_gas.saturating_mul(100 + pct) / 100;
            f.sent.len() <= self.max_bumps && next < cap
        };
        if may_bump && bump_left(&f) && f.since.elapsed() >= self.stuck_after {
            let last = f.last().clone();
            if let Ok(s) = self
                .sender
                .replace(&last, self.bump_pct, cap, self.deadline / 2)
                .await
            {
                out.tx = Some(s.hash.clone());
                f.sent.push(s);
                f.since = tokio::time::Instant::now();
                out.bumped = true;
                let old = f.id.take();
                f.id = self.save(old, Some(f.to_json())).await;
            }
        }
        let age = now_unix().saturating_sub(f.first_unix);
        if age >= self.stuck_after.as_secs() {
            out.stuck_since = Some(
                out.stuck_since
                    .map_or(f.first_unix, |s| s.min(f.first_unix)),
            );
        }
        if !bump_left(&f) && age >= self.give_up.as_secs() {
            out.gave_up = Some(out.gave_up.map_or(nonce, |n| n.min(nonce)));
            out.stuck_since = Some(
                out.stuck_since
                    .map_or(f.first_unix, |s| s.min(f.first_unix)),
            );
        }
        Step::Keep(f, true)
    }

    /// Register each shard once (skipped receipts said why 4), when the
    /// operator can build its `register` call; a shard not final yet (or
    /// already registered) reverts in the estimate and is left for later.
    async fn register_all(&self, mut shards: Vec<(u64, Hash32)>) -> usize {
        let Some(reg) = &self.registrar else {
            return 0;
        };
        shards.sort_by_key(|a| (a.0, a.1 .0));
        shards.dedup();
        let mut n = 0;
        for (round, shard) in shards {
            let Some(data) = reg(round, shard) else {
                continue;
            };
            if self
                .sender
                .send(&self.revenue, &data, 0, self.deadline)
                .await
                .is_ok()
            {
                n += 1;
            }
        }
        n
    }
}

impl<T: JsonRpc> ReporterHealth for Reporter<T> {
    fn reporter_status(&self) -> ReporterStatus {
        self.status()
    }
}

impl<T: JsonRpc + 'static> Reporter<T> {
    /// Post forever: right away again while full batches keep coming, every
    /// `every` when the log is drained or a `credit` is waiting to be final,
    /// and with doubling backoff (up to [`MAX_BACKOFF`]) after failures.
    /// Abort the handle to stop it; a post cut short commits nothing and is
    /// safely repeated.
    pub fn spawn(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut wait = every;
            loop {
                match self.run_once().await {
                    Ok(p) if p.posted >= self.batch && !p.waiting => {
                        wait = every;
                        continue;
                    }
                    Ok(_) => wait = every,
                    Err(_) => wait = wait.saturating_mul(2).clamp(every, MAX_BACKOFF.max(every)),
                }
                tokio::time::sleep(wait).await;
            }
        })
    }
}

#[cfg(test)]
#[path = "reporter_tests.rs"]
mod tests;
