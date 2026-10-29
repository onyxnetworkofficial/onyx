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
mod tests {
    use super::*;
    use crate::journal::TEST_BINDING;
    use crate::receipts::{RetryPolicy, MAX_RECEIPT_TRIES};
    use crate::x402::BoxFuture;
    use std::collections::{BTreeMap, BTreeSet};

    /// Public test key; it holds nothing.
    const OPERATOR: [u8; 32] = [0x11; 32];
    const REVENUE: [u8; 20] = [0x5e; 20];

    /// `cast calldata "credit((uint64,bytes32,uint128,address,bytes32)[])"
    /// "[(5,0x…07,2500,0x…aa,0x…09),(6,0x…08,1,0x…bb,0x…0a)]"` (Foundry 1.8.5).
    const CAST_CREDIT: &str = "b0105f63000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000005000000000000000000000000000000000000000000000000000000000000000700000000000000000000000000000000000000000000000000000000000009c400000000000000000000000000000000000000000000000000000000000000aa000000000000000000000000000000000000000000000000000000000000000900000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000bb000000000000000000000000000000000000000000000000000000000000000a";

    fn small(b: u8) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[31] = b;
        w
    }

    #[test]
    fn credit_calldata_matches_cast() {
        let mut p1 = [0u8; 20];
        p1[19] = 0xaa;
        let mut p2 = [0u8; 20];
        p2[19] = 0xbb;
        let batch = [
            Receipt {
                round: 5,
                shard: Hash32(small(7)),
                amount: 2_500,
                payer: p1,
                nonce: small(9),
            },
            Receipt {
                round: 6,
                shard: Hash32(small(8)),
                amount: 1,
                payer: p2,
                nonce: small(10),
            },
        ];
        assert_eq!(hex::encode(credit_calldata(&batch)), CAST_CREDIT);
    }

    /// The contract's `credit`, as far as the reporter can see it.
    struct Chain {
        /// `receiptUsed`: (payer, nonce).
        used: BTreeSet<([u8; 20], [u8; 32])>,
        /// Every credit applied, in order.
        credited: Vec<Receipt>,
        /// Nonces the token reports used (payments that happened).
        paid: BTreeSet<[u8; 32]>,
        /// Shards registered.
        registered: BTreeSet<Hash32>,
        /// `register` transactions sent.
        registers: usize,
        /// The calldata of the last estimate (what the next send carries).
        data: Vec<u8>,
        /// A fresh estimate was made: the next raw send is a new
        /// transaction (a replacement reuses the last one's effect).
        fresh: bool,
        /// Raw transactions the node took.
        raws: Vec<String>,
        /// The last mined receipt.
        receipt: Value,
        /// The revenue address holds code.
        code: bool,
        /// The `credit` emits no events (an EOA or a wrong contract).
        silent: bool,
        /// The node's `finalized` block number; credits mine in block 16.
        finalized: u64,
        /// The canonical hash of block 16 (a reorg changes it).
        block16: String,
        /// The node refuses every call (down).
        down: bool,
        /// Receipts are visible (false: the transaction landed, but the
        /// sender cannot see it before its deadline).
        mine: bool,
        /// The key's `latest` nonce (mined); `pending` is 3.
        latest: u64,
        /// The receipt of each transaction that applied, by hash (the last
        /// one is `receipt`, which tests may edit).
        by_hash: BTreeMap<String, Value>,
        last_hash: String,
    }

    impl Default for Chain {
        fn default() -> Self {
            Chain {
                used: BTreeSet::new(),
                credited: Vec::new(),
                paid: BTreeSet::new(),
                registered: BTreeSet::new(),
                registers: 0,
                data: Vec::new(),
                fresh: false,
                raws: Vec::new(),
                receipt: Value::Null,
                code: true,
                silent: false,
                finalized: 0x100,
                block16: "0xb16".into(),
                down: false,
                mine: true,
                latest: 3,
                by_hash: BTreeMap::new(),
                last_hash: String::new(),
            }
        }
    }

    /// A node over a shared [`Chain`].
    struct Node {
        chain: Arc<Mutex<Chain>>,
    }

    fn word_hex(b: &[u8]) -> String {
        format!("0x{}", hex::encode(b))
    }

    fn register_selector() -> [u8; 4] {
        selector("register(uint64,bytes32)")
    }

    fn apply_credit(c: &mut Chain) -> Value {
        let d = c.data.clone();
        if d[..4] == register_selector() {
            c.registers += 1;
            c.registered
                .insert(Hash32(d[36..68].try_into().expect("32")));
            return json!({"status": "0x1", "blockNumber": "0x10", "blockHash": c.block16,
                          "gasUsed": "0x5208", "effectiveGasPrice": "0x1", "logs": []});
        }
        let n =
            usize::try_from(u128::from_be_bytes(d[52..68].try_into().expect("len"))).expect("n");
        let mut logs = Vec::new();
        for i in 0..n {
            let w = |k: usize| &d[68 + i * 160 + k * 32..68 + i * 160 + (k + 1) * 32];
            let mut payer = [0u8; 20];
            payer.copy_from_slice(&w(3)[12..]);
            let r = Receipt {
                round: u64::from_be_bytes(w(0)[24..].try_into().expect("u64")),
                shard: Hash32(w(1).try_into().expect("32")),
                amount: u64::from_be_bytes(w(2)[24..].try_into().expect("u64")),
                payer,
                nonce: w(4).try_into().expect("32"),
            };
            let why: u8 = if c.used.contains(&(r.payer, r.nonce)) {
                1
            } else if !c.paid.contains(&r.nonce) {
                2
            } else if !c.registered.contains(&r.shard) {
                4
            } else {
                0
            };
            let topics = json!([
                word_hex(&if why == 0 {
                    credited_topic()
                } else {
                    skipped_topic()
                }),
                word_hex(w(0)),
                word_hex(w(1)),
                word_hex(w(3)),
            ]);
            let data = if why == 0 {
                c.used.insert((r.payer, r.nonce));
                c.credited.push(r);
                [w(2).to_vec(), w(4).to_vec()].concat()
            } else {
                let mut why_w = [0u8; 32];
                why_w[31] = why;
                [w(4).to_vec(), why_w.to_vec()].concat()
            };
            logs.push(json!({
                "address": word_hex(&REVENUE),
                "topics": topics,
                "data": word_hex(&data),
            }));
        }
        if c.silent {
            logs.clear();
        }
        json!({"status": "0x1", "blockNumber": "0x10", "blockHash": c.block16,
               "gasUsed": "0x5208", "effectiveGasPrice": "0x3b9aca00", "logs": logs})
    }

    impl JsonRpc for Node {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            let mut c = self.chain.lock().expect("lock");
            let r = if c.down {
                Err("connection refused".to_string())
            } else {
                match method {
                    "eth_estimateGas" => {
                        c.data = hex::decode(
                            params[0]["data"]
                                .as_str()
                                .unwrap_or_default()
                                .trim_start_matches("0x"),
                        )
                        .expect("hex");
                        c.fresh = true;
                        Ok(json!("0x30000"))
                    }
                    // The head (block 16); the sender's estimate names it
                    // once a receipt was seen (review LAUNCH-R1-A3).
                    "eth_blockNumber" => Ok(json!("0x10")),
                    "eth_getCode" => Ok(json!(if c.code { "0x6080" } else { "0x" })),
                    "eth_getTransactionCount" if params[1] == "latest" => {
                        Ok(json!(format!("0x{:x}", c.latest)))
                    }
                    "eth_getTransactionCount" => Ok(json!("0x3")),
                    "eth_getBlockByNumber" => match params[0].as_str() {
                        Some("latest") => Ok(json!({"number": "0x10", "hash": c.block16,
                                                    "baseFeePerGas": "0x3b9aca00"})),
                        Some("finalized") | Some("safe") => Ok(json!({
                            "number": format!("0x{:x}", c.finalized), "hash": "0xf"
                        })),
                        Some("0x10") => Ok(json!({"number": "0x10", "hash": c.block16})),
                        _ => Ok(Value::Null),
                    },
                    "eth_maxPriorityFeePerGas" => Ok(json!("0xf4240")),
                    "eth_sendRawTransaction" => {
                        let raw = params[0].as_str().unwrap_or_default().to_string();
                        let hash = format!(
                            "0x{}",
                            hex::encode(
                                keccak256(&hex::decode(raw.trim_start_matches("0x")).expect("hex"))
                                    .0
                            )
                        );
                        c.raws.push(raw);
                        if c.fresh {
                            c.fresh = false;
                            c.receipt = apply_credit(&mut c);
                            let r = c.receipt.clone();
                            c.by_hash.insert(hash.clone(), r);
                            c.last_hash = hash.clone();
                        }
                        Ok(json!(hash))
                    }
                    "eth_getTransactionReceipt" if c.mine => {
                        let h = params[0].as_str().unwrap_or_default();
                        Ok(if h == c.last_hash {
                            c.receipt.clone()
                        } else {
                            c.by_hash.get(h).cloned().unwrap_or(Value::Null)
                        })
                    }
                    "eth_getTransactionReceipt" => Ok(Value::Null),
                    _ => Err(format!("unexpected {method}")),
                }
            };
            Box::pin(async move { r })
        }
    }

    /// Retries due right away (the time-based waits have their own test in
    /// `receipts`).
    fn quick_log() -> Arc<ReceiptLog> {
        Arc::new(ReceiptLog::new().with_policy(RetryPolicy {
            payment_base: Duration::ZERO,
            unregistered_base: Duration::ZERO,
            ..RetryPolicy::default()
        }))
    }

    fn reporter(chain: &Arc<Mutex<Chain>>, mine: bool, log: Arc<ReceiptLog>) -> Reporter<Node> {
        chain.lock().expect("lock").mine = mine;
        let sender = Sender::new(
            Node {
                chain: chain.clone(),
            },
            84_532,
            OPERATOR,
        )
        .expect("key")
        .with_poll(Duration::from_millis(2));
        Reporter::new(Arc::new(sender), REVENUE, log).with_deadline(Duration::from_millis(30))
    }

    /// Paused tokio time (ROADMAP E12): deadlines and polls follow the
    /// test's order, not the machine's speed.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("runtime")
    }

    fn receipt(i: u8) -> Receipt {
        Receipt {
            round: 1,
            shard: Hash32([1; 32]),
            amount: 2_500,
            payer: [0xaa; 20],
            nonce: [i; 32],
        }
    }

    /// One post: credited receipts and those that can never apply leave the
    /// log; one skipped for a reason that can clear goes back.
    #[test]
    fn a_post_commits_the_batch_and_requeues_only_what_can_clear() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain::default()));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.extend([[1u8; 32], [3u8; 32]]);
                c.used.insert(([0xaa; 20], [3u8; 32]));
            }
            let log = quick_log();
            for i in 1..=3 {
                log.record(receipt(i));
            }
            let rep = reporter(&chain, true, log.clone());
            let p = rep.run_once().await.expect("post");
            assert!(!p.waiting);
            assert_eq!(
                (p.posted, p.applied, p.requeued, p.unaccounted),
                (3, 1, 1, 0)
            );
            assert_eq!(p.skipped, vec![(receipt(2), 2), (receipt(3), 1)]);
            assert_eq!(log.peek(9), vec![receipt(2)], "why 2 waits for the payment");
            chain.lock().expect("lock").paid.insert([2u8; 32]);
            let p = rep.run_once().await.expect("post");
            assert_eq!(p.applied, 1);
            assert_eq!(log.pending(), 0);
            // Nothing new, nothing sent.
            assert_eq!(rep.run_once().await.expect("idle"), Posted::default());
            assert_eq!(chain.lock().expect("lock").credited.len(), 2);
            assert_eq!(rep.status().applied, 2);
        });
    }

    /// REV-11 crash recovery: three receipts are on disk; the `credit` lands
    /// on chain but the reporter never sees it mined and is killed before
    /// committing. After a restart the log still holds them; the next post
    /// sends them again, the contract skips all three as already credited
    /// (why 1), and they leave the log. Each was credited exactly once.
    #[test]
    fn a_crash_between_posting_and_committing_credits_each_receipt_once() {
        rt().block_on(async {
            let d = crate::journal::test_dir("reporter");
            let path = d.join("receipts.jsonl");
            let chain = Arc::new(Mutex::new(Chain::default()));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.extend((1..=3).map(|i| [i; 32]));
            }
            {
                let log = Arc::new(ReceiptLog::open(&path, &TEST_BINDING).expect("open"));
                for i in 1..=3 {
                    log.record(receipt(i));
                }
                let rep = reporter(&chain, false, log.clone());
                let p = rep.run_once().await.expect("sent, not seen mined");
                assert!(p.waiting && p.tx.is_some());
                assert_eq!(chain.lock().expect("lock").credited.len(), 3, "it landed");
                assert_eq!(log.pending(), 3, "nothing committed");
                // Killed here.
            }
            let log = Arc::new(ReceiptLog::open(&path, &TEST_BINDING).expect("restart"));
            assert_eq!(log.pending(), 3);
            let rep = reporter(&chain, true, log.clone());
            let p = rep.run_once().await.expect("post again");
            assert_eq!((p.posted, p.applied, p.requeued), (3, 0, 0));
            assert!(p.skipped.iter().all(|(_, why)| *why == 1));
            assert_eq!(log.pending(), 0);
            drop((rep, log));
            assert_eq!(
                ReceiptLog::open(&path, &TEST_BINDING)
                    .expect("again")
                    .pending(),
                0
            );
            let credited = chain.lock().expect("lock").credited.clone();
            assert_eq!(
                credited,
                (1..=3).map(receipt).collect::<Vec<_>>(),
                "once each"
            );
            std::fs::remove_dir_all(d).ok();
        });
    }

    /// A post that could not send commits nothing and is reported in the
    /// status; the loop backs off and retries.
    #[test]
    fn the_loop_retries_after_a_failed_post() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain::default()));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.insert([1; 32]);
                c.down = true;
            }
            let log = quick_log();
            log.record(receipt(1));
            let rep = Arc::new(reporter(&chain, true, log.clone()));
            assert!(rep.run_once().await.is_err());
            assert!(rep.status().last_error.is_some());
            chain.lock().expect("lock").down = false;
            let h = rep.clone().spawn(Duration::from_millis(5));
            for _ in 0..200 {
                if log.pending() == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            h.abort();
            assert_eq!(log.pending(), 0);
            assert_eq!(chain.lock().expect("lock").credited.len(), 1);
        });
    }

    /// REV-L3-F8: a mined `credit` leaves its batch in the log until it is
    /// final, and a reorg (its block no longer canonical) keeps it there.
    #[test]
    fn a_batch_is_committed_only_when_its_credit_is_final() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain {
                finalized: 0x0f,
                ..Chain::default()
            }));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.insert([1; 32]);
            }
            let log = quick_log();
            log.record(receipt(1));
            let rep = reporter(&chain, true, log.clone());
            let p = rep.run_once().await.expect("mined, not final");
            assert!(p.waiting);
            assert_eq!(log.pending(), 1, "kept until final");
            // Final by number, but the block was reorged away.
            {
                let mut c = chain.lock().expect("lock");
                c.finalized = 0x20;
                c.block16 = "0xother".into();
                c.receipt["blockHash"] = json!("0xb16");
            }
            assert!(rep.run_once().await.expect("reorged").waiting);
            assert_eq!(log.pending(), 1);
            // Canonical and final: committed, without sending again.
            chain.lock().expect("lock").receipt["blockHash"] = json!("0xother");
            let p = rep.run_once().await.expect("final");
            assert!(!p.waiting);
            assert_eq!(p.applied, 1);
            assert_eq!(log.pending(), 0);
            assert_eq!(chain.lock().expect("lock").raws.len(), 1, "one transaction");
            // `latest` commits at once.
            let chain2 = Arc::new(Mutex::new(Chain {
                finalized: 0,
                ..Chain::default()
            }));
            chain2.lock().expect("lock").paid.insert([2; 32]);
            let log2 = quick_log();
            log2.record(receipt(2));
            let p = reporter(&chain2, true, log2.clone())
                .with_confirm(Confirm::Latest)
                .run_once()
                .await
                .expect("latest");
            assert!(!p.waiting);
            assert_eq!(log2.peek(9), vec![receipt(2)], "why 4 went back");
        });
    }

    /// REV-L3-F8: a `credit` stuck unmined is replaced at the same nonce
    /// with higher fees, never above the cap.
    #[test]
    fn a_stuck_credit_is_replaced_with_higher_fees_up_to_a_cap() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain::default()));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.insert([1; 32]);
            }
            let log = quick_log();
            log.record(receipt(1));
            // Stuck at once: the first post sends and replaces.
            let rep = reporter(&chain, false, log.clone()).with_fee_bumps(Duration::ZERO, 25, 1, 4);
            let p1 = rep.run_once().await.expect("sent and bumped");
            assert!(p1.waiting && p1.bumped);
            // At most one bump.
            let p2 = rep.run_once().await.expect("no more bumps");
            assert!(p2.waiting && !p2.bumped);
            assert_eq!(p2.rebroadcast, 1, "handed to the node again as signed");
            assert_eq!(p1.tx, p2.tx);
            let raws = chain.lock().expect("lock").raws.clone();
            assert_eq!(
                raws.len(),
                3,
                "the first version, one replacement, its re-broadcast"
            );
            assert_ne!(raws[0], raws[1]);
            assert_eq!(raws[1], raws[2], "a re-broadcast, not a new transaction");
            // A cap of the first fee leaves no room for a valid bump.
            let chain2 = Arc::new(Mutex::new(Chain::default()));
            let log2 = quick_log();
            log2.record(receipt(9));
            let capped = reporter(&chain2, false, log2).with_fee_bumps(Duration::ZERO, 25, 4, 1);
            let p = capped.run_once().await.expect("capped");
            assert!(p.waiting && !p.bumped);
            assert_eq!(chain2.lock().expect("lock").raws.len(), 1);
            chain.lock().expect("lock").mine = true;
            let p = rep.run_once().await.expect("mined");
            assert_eq!(p.applied, 1);
            assert_eq!(log.pending(), 0);
            assert_eq!(
                chain.lock().expect("lock").credited.len(),
                1,
                "credited once"
            );
        });
    }

    /// REV-L3-F2: a revenue address without code is refused before any
    /// send; a mined `credit` whose events name none of the batch removes
    /// nothing: every receipt goes back.
    #[test]
    fn unaccounted_receipts_go_back_and_an_address_without_code_is_refused() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain {
                code: false,
                ..Chain::default()
            }));
            let log = quick_log();
            for i in 0..3 {
                log.record(receipt(i));
            }
            let rep = reporter(&chain, true, log.clone());
            let e = rep.run_once().await.expect_err("no code");
            assert!(matches!(e, ReportError::NoContract(_)), "{e:?}");
            assert!(chain.lock().expect("lock").raws.is_empty(), "nothing sent");
            {
                let mut c = chain.lock().expect("lock");
                c.code = true;
                c.silent = true;
            }
            let p = rep.run_once().await.expect("mined, no events");
            assert_eq!((p.applied, p.unaccounted, p.requeued), (0, 3, 3));
            assert_eq!(log.pending(), 3, "nothing deleted");
            assert_eq!(log.dropped(), 0);
        });
    }

    /// REV-L3-F3: why-4 receipts wait (hours, not posts) and the reporter
    /// registers their shard once; quick posts never drop them.
    #[test]
    fn unregistered_shards_are_registered_and_their_receipts_wait() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain::default()));
            chain.lock().expect("lock").paid.insert([1; 32]);
            let log = Arc::new(ReceiptLog::new());
            log.record(receipt(1));
            let calls = Arc::new(Mutex::new(0usize));
            let seen = calls.clone();
            let registrar: Registrar = Arc::new(move |round, shard| {
                *seen.lock().expect("lock") += 1;
                let mut d = selector("register(uint64,bytes32)").to_vec();
                d.extend_from_slice(&word(u128::from(round)));
                d.extend_from_slice(&shard.0);
                Some(d)
            });
            let rep = reporter(&chain, true, log.clone()).with_registrar(registrar);
            let p = rep.run_once().await.expect("post");
            assert_eq!(p.skipped, vec![(receipt(1), 4)]);
            assert_eq!((p.requeued, p.registered), (1, 1));
            assert_eq!(chain.lock().expect("lock").registers, 1);
            // Quick posts: the receipt is not due for a while, nothing drops.
            for _ in 0..MAX_RECEIPT_TRIES {
                assert_eq!(rep.run_once().await.expect("idle").posted, 0);
            }
            assert_eq!(log.pending(), 1);
            assert_eq!(log.dropped(), 0);
            assert_eq!(*calls.lock().expect("lock"), 1);
        });
    }

    /// REV-L4-F3: a batch whose `credit` is mined but not final yet no
    /// longer holds up the next: a second `credit` goes out, both are
    /// committed once final, and no receipt is in two at once.
    #[test]
    fn a_batch_waiting_to_be_final_does_not_hold_up_the_next() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain {
                finalized: 0x0f,
                ..Chain::default()
            }));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.extend([[1u8; 32], [2u8; 32], [3u8; 32]]);
            }
            let log = quick_log();
            for i in 1..=3 {
                log.record(receipt(i));
            }
            let rep = reporter(&chain, true, log.clone())
                .with_batch(1)
                .with_max_in_flight(2);
            assert!(rep.run_once().await.expect("first").waiting);
            let p = rep.run_once().await.expect("second");
            assert_eq!((p.in_flight, p.committed), (2, 0));
            assert_eq!(rep.status().in_flight, 2);
            // The cap: a third batch waits for room.
            assert_eq!(rep.run_once().await.expect("full").in_flight, 2);
            assert_eq!(chain.lock().expect("lock").raws.len(), 2);
            assert_eq!(log.pending(), 3, "nothing committed before final");
            chain.lock().expect("lock").finalized = 0x100;
            // Both commit; with room again the last receipt goes out and,
            // final at once, commits in the same post.
            let p = rep.run_once().await.expect("final");
            assert_eq!((p.committed, p.applied), (3, 3));
            assert_eq!(p.in_flight, 0);
            assert_eq!(log.pending(), 0);
            let credited = chain.lock().expect("lock").credited.clone();
            assert_eq!(credited, (1..=3).map(receipt).collect::<Vec<_>>());
        });
    }

    /// REV-L4-F1: a `credit` in flight is kept on disk; after a restart it
    /// is re-broadcast and bumped at its own nonce (no second batch is sent
    /// behind it), and committed once mined and final.
    #[test]
    fn a_credit_in_flight_survives_a_restart_and_is_bumped_not_resent() {
        rt().block_on(async {
            let d = crate::journal::test_dir("reporter-state");
            let state = d.join("credits.jsonl");
            let chain = Arc::new(Mutex::new(Chain::default()));
            {
                let mut c = chain.lock().expect("lock");
                c.registered.insert(Hash32([1; 32]));
                c.paid.extend([[1u8; 32], [2u8; 32]]);
            }
            let log = quick_log();
            log.record(receipt(1));
            {
                let rep = reporter(&chain, false, log.clone())
                    .with_state_file(&state)
                    .expect("state");
                let p = rep.run_once().await.expect("sent");
                assert!(p.waiting && p.in_flight == 1);
                // Killed here.
            }
            assert_eq!(chain.lock().expect("lock").raws.len(), 1);
            log.record(receipt(2));
            let rep = reporter(&chain, false, log.clone())
                .with_max_in_flight(1)
                .with_fee_bumps(Duration::ZERO, 25, 4, 4)
                .with_state_file(&state)
                .expect("restart");
            assert_eq!(rep.status().in_flight, 1, "resumed from disk");
            let p = rep.run_once().await.expect("resumed");
            assert!(p.waiting && p.bumped);
            assert_eq!(p.rebroadcast, 1);
            assert_eq!(p.in_flight, 1, "no second batch behind it");
            let raws = chain.lock().expect("lock").raws.clone();
            assert_eq!(raws.len(), 3, "first, its re-broadcast, one replacement");
            assert_eq!(raws[0], raws[1]);
            drop(rep);
            // The bump is on disk too: a second restart sees it mined.
            let rep = reporter(&chain, true, log.clone())
                .with_state_file(&state)
                .expect("again");
            let p = rep.run_once().await.expect("mined");
            assert!(p.committed >= 1);
            assert_eq!(p.applied, 2, "receipt 1 by the resumed credit, then 2");
            assert_eq!(log.pending(), 0);
            drop(rep);
            let rep = reporter(&chain, true, log.clone())
                .with_state_file(&state)
                .expect("empty");
            assert_eq!(rep.status().in_flight, 0);
            std::fs::remove_dir_all(d).ok();
        });
    }

    /// REV-L4-F1 (the review's wedge): after the last bump a `credit` the
    /// node dropped is handed to it again on every post, and once the give-up
    /// time passed it is reported stuck (`last_error`, `stuck_since`) while
    /// new batches wait. A later nonce mined meanwhile clears it.
    #[test]
    fn a_credit_stuck_past_its_last_bump_is_resent_and_reported() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain::default()));
            let log = quick_log();
            for i in 0..3 {
                log.record(receipt(i));
            }
            let rep = reporter(&chain, false, log.clone())
                .with_batch(1)
                .with_fee_bumps(Duration::ZERO, 25, 2, 4)
                .with_give_up(Duration::ZERO);
            for _ in 0..10 {
                assert!(rep.run_once().await.expect("post").waiting);
            }
            let raws = chain.lock().expect("lock").raws.len();
            assert_eq!(
                raws,
                1 + 2 + 9,
                "1 send, 2 bumps, then a re-broadcast per post"
            );
            let st = rep.status();
            assert!(st.stuck_since.is_some());
            assert!(
                st.last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("nonce 3")),
                "{st:?}"
            );
            assert_eq!(st.in_flight, 1, "new batches wait behind it");
            // Mined under a later nonce (another sender of the key): gone;
            // its batch is posted again.
            chain.lock().expect("lock").latest = 4;
            let p = rep.run_once().await.expect("cleared");
            assert_eq!(p.gave_up, None);
            assert_eq!(rep.status().last_error, None);
        });
    }

    /// REV-L4-F1: the sender refuses a new `credit` behind a transaction of
    /// the key stuck past the gap limit; the post fails and says so.
    #[test]
    fn no_new_credit_is_sent_behind_a_stuck_transaction() {
        rt().block_on(async {
            let chain = Arc::new(Mutex::new(Chain {
                latest: 2,
                ..Chain::default()
            }));
            let log = quick_log();
            log.record(receipt(1));
            let sender = Sender::new(
                Node {
                    chain: chain.clone(),
                },
                84_532,
                OPERATOR,
            )
            .expect("key")
            .with_poll(Duration::from_millis(2))
            .with_gap_limit(Duration::ZERO);
            let rep = Reporter::new(Arc::new(sender), REVENUE, log.clone())
                .with_deadline(Duration::from_millis(30));
            let e = rep.run_once().await.expect_err("stuck");
            assert_eq!(e, ReportError::Send(SendError::Stuck(2)));
            assert!(chain.lock().expect("lock").raws.is_empty());
            assert!(rep.status().last_error.is_some());
            assert_eq!(log.pending(), 1);
        });
    }
}
