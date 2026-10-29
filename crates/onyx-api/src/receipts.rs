//! Usage receipts (ADR-0025 item 2): one per settled x402 payment, naming the
//! shard whose row was served. The reporter posts them in batches to
//! `OnyxRevenue.credit`, which splits each fee among that shard's miners,
//! validators, label payee and the treasury. The reporter takes a batch with
//! [`ReceiptLog::peek`] and removes it with [`ReceiptLog::commit`] only after
//! the transaction succeeded (REV-L1-F6): a failed or dropped transaction
//! loses no receipt. `credit` skips a bad receipt with an event instead of
//! reverting, so one bad receipt never blocks the batch. Receipts skipped for
//! a reason that can clear (why 2: settle not seen yet or reorged; why 4:
//! shard not registered yet) go back in the log with
//! [`ReceiptLog::commit_with_retry`] and wait before their next post: the
//! wait doubles per post and is long for an unregistered shard ([`RetryPolicy`],
//! REV-L3-F3); after [`MAX_RECEIPT_TRIES`] posts a receipt is dropped and
//! counted (REV-L2-F5). A stored wait never lies further ahead than
//! [`RetryPolicy::max_wait`] from now: a wait written while the clock ran
//! ahead is pulled back on open and on every peek (REV-L4-F2).
//!
//! A log opened with [`ReceiptLog::open`] is kept in a [`Journal`] on disk
//! (REV-11): every change is written and synced before the call returns, so
//! a receipt recorded before the server answered a paid request survives a
//! crash or a restart. Posting is idempotent across restarts: the contract's
//! `receiptUsed` is the source of truth, so a batch that landed on chain but
//! was not committed here before a crash is posted again and skipped there
//! with why 1, then removed.

use crate::journal::{Binding, Journal};
use onyx_core::Hash32;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Receipts kept before the reporter drains them; beyond this the oldest is
/// dropped and counted (its payment stays unallocated in the contract, where
/// a later receipt with the same nonce can still attribute it).
pub const MAX_RECEIPTS_KEPT: usize = 100_000;

/// One settled payment, as `OnyxRevenue.Receipt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// Round of the shard served.
    pub round: u64,
    /// Shard served (the row's shard, not only the quoted one).
    pub shard: Hash32,
    /// Amount settled, in atomic units (micro-USDC).
    pub amount: u64,
    /// EIP-3009 `from`.
    pub payer: [u8; 20],
    /// EIP-3009 nonce.
    pub nonce: [u8; 32],
}

fn fixed<const N: usize>(v: &Value) -> Option<[u8; N]> {
    let s = v.as_str()?.strip_prefix("0x")?;
    hex::decode(s).ok()?.try_into().ok()
}

impl Receipt {
    /// JSON as the reporter posts it (`OnyxRevenue.Receipt` fields).
    pub fn to_json(&self) -> Value {
        json!({
            "round": self.round,
            "shard": self.shard.to_string(),
            "amount": self.amount.to_string(),
            "payer": format!("0x{}", hex::encode(self.payer)),
            "nonce": format!("0x{}", hex::encode(self.nonce)),
        })
    }

    /// The inverse of [`Receipt::to_json`]; `None` for anything else.
    pub fn from_json(v: &Value) -> Option<Receipt> {
        Some(Receipt {
            round: v.get("round")?.as_u64()?,
            shard: Hash32(fixed::<32>(v.get("shard")?)?),
            amount: v.get("amount")?.as_str()?.parse().ok()?,
            payer: fixed::<20>(v.get("payer")?)?,
            nonce: fixed::<32>(v.get("nonce")?)?,
        })
    }
}

/// Posts a receipt may take before it is dropped as never applicable.
pub const MAX_RECEIPT_TRIES: u8 = 8;

/// Whether `credit` skipped a receipt for a reason that can clear later
/// (`ReceiptSkipped.why`): 2 (the payment is not seen as used yet) or 4 (the
/// shard is not registered yet). 1 (already credited) and 3 (more than the
/// contract holds unattributed) never clear by waiting.
pub fn retry_after_skip(why: u8) -> bool {
    Retry::for_why(why).is_some()
}

/// Why a posted receipt goes back in the log, which sets how long it waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// The payment is not seen as used yet (why 2: the settle is not mined
    /// or was reorged out): a short wait.
    Payment,
    /// The shard is not registered in `OnyxRevenue` yet (why 4): registering
    /// can take hours (the shard must be final first), so a long wait
    /// (REV-L3-F3).
    Unregistered,
    /// The mined `credit`'s events did not mention the receipt at all
    /// (REV-L3-F2): something is wrong with the contract or the node, not
    /// with the receipt, so it waits as long as an unregistered shard
    /// (about 20 hours over its posts) before it is dropped and counted
    /// (REV-L4-F5).
    Unaccounted,
}

impl Retry {
    /// The retry for a `ReceiptSkipped.why`, if that reason can clear.
    pub fn for_why(why: u8) -> Option<Retry> {
        match why {
            2 => Some(Retry::Payment),
            4 => Some(Retry::Unregistered),
            _ => None,
        }
    }
}

/// How long a receipt that went back waits before its next post: `base`
/// for its kind, doubled per post already taken, capped at `max_wait`; after
/// `max_tries` posts in all it is dropped and counted (REV-L3-F3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Posts a receipt may take in all.
    pub max_tries: u8,
    /// First wait for [`Retry::Payment`].
    pub payment_base: Duration,
    /// First wait for [`Retry::Unregistered`].
    pub unregistered_base: Duration,
    /// The longest single wait.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    /// 8 posts; 30 s doubling for a payment not seen yet (3,810 s, about
    /// 63.5 minutes, of waits in all), 15 minutes doubling up to 6 hours for
    /// an unregistered shard or an unaccounted receipt (about 20 hours in
    /// all, REV-L4-F5).
    fn default() -> Self {
        RetryPolicy {
            max_tries: MAX_RECEIPT_TRIES,
            payment_base: Duration::from_secs(30),
            unregistered_base: Duration::from_secs(15 * 60),
            max_wait: Duration::from_secs(6 * 3600),
        }
    }
}

impl RetryPolicy {
    /// The wait before the next post of a receipt that took `tries` posts.
    pub fn wait(&self, why: Retry, tries: u8) -> Duration {
        let base = match why {
            Retry::Payment => self.payment_base,
            Retry::Unregistered | Retry::Unaccounted => self.unregistered_base,
        };
        let doubled = base.saturating_mul(1u32 << u32::from(tries.saturating_sub(1).min(20)));
        doubled.min(self.max_wait)
    }
}

/// Unix seconds now.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One waiting receipt: its journal id, the receipt, the posts it already
/// took and the unix second before which it is not posted again.
#[derive(Clone, Copy, Debug)]
struct Entry {
    id: u64,
    r: Receipt,
    tries: u8,
    after: u64,
}

fn stored(r: &Receipt, tries: u8, after: u64) -> Value {
    json!({ "receipt": r.to_json(), "tries": tries, "after": after })
}

/// The journal kind of a receipt log.
pub const JOURNAL_KIND: &str = "onyx-receipts";

#[derive(Default)]
struct Inner {
    q: VecDeque<Entry>,
    dropped: u64,
    write_errors: u64,
    journal: Option<Journal>,
    /// Ids for a log with no journal.
    next_mem: u64,
    policy: RetryPolicy,
}

impl Inner {
    /// Write `del` and `add` to the journal (if any) and return the ids for
    /// `add`. A failed write keeps the change in memory and is counted: the
    /// API then refuses new paid requests until the log is drained and the
    /// server restarted (REV-L3-F5). The journal is closed for good (its
    /// state on disk is unknown).
    fn write(&mut self, del: &[u64], add: &[(Receipt, u8, u64)]) -> Vec<u64> {
        let values: Vec<Value> = add.iter().map(|(r, t, a)| stored(r, *t, *a)).collect();
        let written = self.journal.as_mut().map(|j| j.apply(del, &values));
        match written {
            Some(Ok(ids)) => ids,
            other => {
                if matches!(other, Some(Err(_))) {
                    self.write_errors += 1;
                    self.journal = None;
                }
                let start = self
                    .next_mem
                    .max(self.q.iter().map(|e| e.id + 1).max().unwrap_or(0));
                self.next_mem = start + add.len() as u64;
                (start..self.next_mem).collect()
            }
        }
    }
}

/// Receipts waiting for the reporter, each with the posts it already took.
#[derive(Default)]
pub struct ReceiptLog {
    inner: Mutex<Inner>,
}

impl ReceiptLog {
    /// Empty, in memory only (tests, and a server with no state directory).
    pub fn new() -> Self {
        Self::default()
    }

    /// The log kept in the journal at `path` (created if missing), bound to
    /// one chain and `OnyxRevenue` (REV-L3-F4), with the receipts it already
    /// holds (REV-11). Refuses a damaged journal, one bound elsewhere, and
    /// one another process holds.
    pub fn open(path: impl AsRef<Path>, bind: &Binding) -> std::io::Result<Self> {
        let (journal, live) = Journal::open(path, JOURNAL_KIND, bind)?;
        let mut q = VecDeque::with_capacity(live.len());
        for (id, v) in live {
            let r = v.get("receipt").and_then(Receipt::from_json);
            let tries = v.get("tries").and_then(Value::as_u64);
            let after = v.get("after").and_then(Value::as_u64).unwrap_or(0);
            match (r, tries) {
                (Some(r), Some(t)) => q.push_back(Entry {
                    id,
                    r,
                    tries: u8::try_from(t).unwrap_or(u8::MAX),
                    after,
                }),
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("receipt journal entry {id} is not a receipt"),
                    ))
                }
            }
        }
        let log = ReceiptLog {
            inner: Mutex::new(Inner {
                q,
                journal: Some(journal),
                ..Inner::default()
            }),
        };
        log.clamp_waits(now_unix());
        Ok(log)
    }

    /// Pull every stored wait back to at most `now + max_wait` (REV-L4-F2):
    /// a wait written while the clock ran ahead (a bad RTC, an NTP step
    /// back) would otherwise strand its receipt, never due and never
    /// dropped. In memory; a restart clamps again from the file.
    fn clamp_waits(&self, now: u64) {
        if let Ok(mut g) = self.inner.lock() {
            let latest = now.saturating_add(g.policy.max_wait.as_secs());
            for e in g.q.iter_mut() {
                e.after = e.after.min(latest);
            }
        }
    }

    /// Use `policy` for receipts that go back in the log.
    pub fn with_policy(self, policy: RetryPolicy) -> Self {
        if let Ok(mut g) = self.inner.lock() {
            g.policy = policy;
        }
        self.clamp_waits(now_unix());
        self
    }

    /// Whether the log is kept on disk.
    pub fn is_persistent(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.journal.is_some())
            .unwrap_or(false)
    }

    /// Record a settled payment. Returns whether the receipt is as durable as
    /// this log is kept (OPS-L3-F1): with a journal, synced to disk; for a
    /// log that never had a journal (tests, a server with no state
    /// directory), held in memory. `false` when it is not: the journal
    /// failed (now or earlier: the error is counted in
    /// [`ReceiptLog::write_errors`] and the receipt is kept in memory only),
    /// or the log is full ([`MAX_RECEIPTS_KEPT`]; nothing is evicted). A
    /// caller holding a settle intent must then keep the intent: it is the
    /// only durable record of the payment.
    ///
    /// One receipt per payment: a receipt for a `(payer, nonce)` already
    /// waiting (the same payment recorded again by a recovery, OPS-L2-F1;
    /// the contract credits a `(payer, nonce)` once) is not added again, and
    /// the answer is the log's durability. Blocks on the disk: from async
    /// code use [`record_off_thread`].
    pub fn record(&self, r: Receipt) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        if g.q
            .iter()
            .any(|e| e.r.payer == r.payer && e.r.nonce == r.nonce)
        {
            return g.write_errors == 0;
        }
        if g.write_errors > 0 || g.q.len() >= MAX_RECEIPTS_KEPT {
            // Not durable (a closed journal) or no room: keep what can be
            // kept in memory for the reporter, but say so.
            if g.write_errors > 0 && g.q.len() < MAX_RECEIPTS_KEPT {
                let ids = g.write(&[], &[(r, 0, 0)]);
                if let Some(&id) = ids.first() {
                    g.q.push_back(Entry {
                        id,
                        r,
                        tries: 0,
                        after: 0,
                    });
                }
            }
            return false;
        }
        let before = g.write_errors;
        let ids = g.write(&[], &[(r, 0, 0)]);
        if let Some(&id) = ids.first() {
            g.q.push_back(Entry {
                id,
                r,
                tries: 0,
                after: 0,
            });
        }
        g.write_errors == before
    }

    /// Up to `max` receipts due for a post now, oldest first, left in the
    /// log.
    pub fn peek(&self, max: usize) -> Vec<Receipt> {
        self.peek_at(max, now_unix())
    }

    /// [`ReceiptLog::peek`] at unix second `now`: receipts waiting for a
    /// retry ([`RetryPolicy`]) are not due yet. A wait further ahead than
    /// `now + max_wait` is pulled back first (REV-L4-F2).
    pub fn peek_at(&self, max: usize, now: u64) -> Vec<Receipt> {
        self.clamp_waits(now);
        self.inner
            .lock()
            .map(|g| {
                g.q.iter()
                    .filter(|e| e.after <= now)
                    .take(max)
                    .map(|e| e.r)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Remove a posted batch: the first waiting receipt equal to each one of
    /// `batch`. Returns how many were removed.
    pub fn commit(&self, batch: &[Receipt]) -> usize {
        self.commit_with_retry(batch, &[]).0
    }

    /// [`ReceiptLog::commit`], then put back the receipts of `batch` named
    /// in `retry`, each to wait as its [`Retry`] and the [`RetryPolicy`]
    /// say; a receipt past `max_tries` posts is dropped and counted in
    /// [`ReceiptLog::dropped`]. Returns (removed from the batch, put back).
    /// With a journal, the removals and the re-adds are one durable line.
    pub fn commit_with_retry(
        &self,
        batch: &[Receipt],
        retry: &[(Receipt, Retry)],
    ) -> (usize, usize) {
        self.commit_with_retry_at(batch, retry, now_unix())
    }

    /// [`ReceiptLog::commit_with_retry`] at unix second `now`.
    pub fn commit_with_retry_at(
        &self,
        batch: &[Receipt],
        retry: &[(Receipt, Retry)],
        now: u64,
    ) -> (usize, usize) {
        let Ok(mut g) = self.inner.lock() else {
            return (0, 0);
        };
        let mut taken = Vec::new();
        for r in batch {
            if let Some(i) = g.q.iter().position(|e| e.r == *r) {
                if let Some(e) = g.q.remove(i) {
                    taken.push(e);
                }
            }
        }
        let mut retry: Vec<(Receipt, Retry)> = retry.to_vec();
        let mut again = Vec::new();
        let policy = g.policy;
        for e in &taken {
            let Some(i) = retry.iter().position(|(x, _)| *x == e.r) else {
                continue;
            };
            let (_, why) = retry.swap_remove(i);
            let tries = e.tries.saturating_add(1);
            if tries >= policy.max_tries || g.q.len() + again.len() >= MAX_RECEIPTS_KEPT {
                g.dropped += 1;
            } else {
                let after = now.saturating_add(policy.wait(why, tries).as_secs());
                again.push((e.r, tries, after));
            }
        }
        let del: Vec<u64> = taken.iter().map(|e| e.id).collect();
        let ids = g.write(&del, &again);
        for (id, (r, tries, after)) in ids.into_iter().zip(again.iter()) {
            g.q.push_back(Entry {
                id,
                r: *r,
                tries: *tries,
                after: *after,
            });
        }
        (taken.len(), again.len())
    }

    /// Take up to `max` due receipts for good, oldest first (tests; a
    /// reporter uses [`ReceiptLog::peek`] then [`ReceiptLog::commit`]).
    pub fn drain(&self, max: usize) -> Vec<Receipt> {
        let batch = self.peek(max);
        self.commit(&batch);
        batch
    }

    /// Receipts waiting (due now or later).
    pub fn pending(&self) -> usize {
        self.inner.lock().map(|g| g.q.len()).unwrap_or(0)
    }

    /// Receipts dropped because the log was full or a receipt kept being
    /// skipped.
    pub fn dropped(&self) -> u64 {
        self.inner.lock().map(|g| g.dropped).unwrap_or(0)
    }

    /// Whether the log holds [`MAX_RECEIPTS_KEPT`] receipts: a new one would
    /// not be recorded, so the API refuses new paid requests (OPS-L3-F1).
    pub fn is_full(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.q.len() >= MAX_RECEIPTS_KEPT)
            .unwrap_or(true)
    }

    /// Journal writes that failed (the change was kept in memory only).
    pub fn write_errors(&self) -> u64 {
        self.inner.lock().map(|g| g.write_errors).unwrap_or(0)
    }
}

#[cfg(test)]
impl ReceiptLog {
    /// Make the journal's next write fail (tests of the error path).
    pub(crate) fn break_journal_for_test(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(j) = g.journal.as_mut() {
                j.break_for_test();
            }
        }
    }
}

/// [`ReceiptLog::record`] on a blocking thread, so the disk write and its
/// `fsync` never stall an async worker while holding the log's lock
/// (REV-L3-F7). Returns once the receipt is recorded, with
/// [`ReceiptLog::record`]'s answer: whether it is durable.
pub async fn record_off_thread(log: &Arc<ReceiptLog>, r: Receipt) -> bool {
    let l = log.clone();
    match tokio::task::spawn_blocking(move || l.record(r)).await {
        Ok(durable) => durable,
        // The blocking thread panicked before or while recording: record
        // here rather than lose the receipt.
        Err(_) => log.record(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{test_dir, TEST_BINDING};

    fn open(p: &Path) -> std::io::Result<ReceiptLog> {
        ReceiptLog::open(p, &TEST_BINDING)
    }

    #[test]
    fn drains_oldest_first() {
        let log = ReceiptLog::new();
        for i in 0..3u64 {
            log.record(Receipt {
                round: i,
                shard: Hash32::ZERO,
                amount: 1,
                payer: [0; 20],
                nonce: [i as u8; 32],
            });
        }
        assert_eq!(log.pending(), 3);
        let got: Vec<u64> = log.drain(2).iter().map(|r| r.round).collect();
        assert_eq!(got, vec![0, 1]);
        assert_eq!(log.pending(), 1);
        assert_eq!(log.dropped(), 0);
    }

    /// OPS-L3-F1: `record` says whether the receipt is durable; one
    /// receipt per `(payer, nonce)`; a full log refuses instead of evicting.
    #[test]
    fn record_reports_durability_and_keeps_one_per_payment() {
        let dir = test_dir("receipts-durable");
        let log = open(&dir.join("r.jsonl")).expect("open");
        assert!(log.record(r(1)));
        let same_payment = Receipt { round: 9, ..r(1) };
        assert!(log.record(same_payment), "already durable");
        assert_eq!(log.pending(), 1, "one receipt per (payer, nonce)");
        log.break_journal_for_test();
        assert!(!log.record(r(2)), "the write failed");
        assert!(!log.record(r(3)), "the journal stays closed");
        assert!(!log.record(r(1)), "and nothing is durable any more");
        // A memory-only log (no state directory) is as durable as it is kept.
        let mem = ReceiptLog::new();
        assert!(mem.record(r(4)));
        assert!(!mem.is_full());
    }

    fn r(round: u64) -> Receipt {
        Receipt {
            round,
            shard: Hash32::ZERO,
            amount: 1,
            payer: [0; 20],
            nonce: [round as u8; 32],
        }
    }

    /// REV-L2-F5 and REV-L3-F3: receipts skipped with why 2 or 4 go back in
    /// the log and wait before their next post (doubling, long for an
    /// unregistered shard); after `MAX_RECEIPT_TRIES` posts they are
    /// dropped. Back-to-back posts never burn the tries.
    #[test]
    fn skipped_receipts_wait_and_are_posted_a_bounded_number_of_times() {
        assert!(retry_after_skip(2) && retry_after_skip(4));
        assert!(!retry_after_skip(1) && !retry_after_skip(3) && !retry_after_skip(0));
        let policy = RetryPolicy::default();
        let log = ReceiptLog::new();
        for i in 0..3 {
            log.record(r(i));
        }
        let t0 = 1_000_000;
        let batch = log.peek_at(3, t0);
        // `credit` applied 0, skipped 1 with why 4 (unregistered), 2 with why 1.
        assert_eq!(
            log.commit_with_retry_at(&batch, &[(r(1), Retry::Unregistered)], t0),
            (3, 1)
        );
        // Not due yet: a quick post right after takes nothing.
        assert!(log.peek_at(9, t0 + 1).is_empty());
        let first = policy.wait(Retry::Unregistered, 1).as_secs();
        assert_eq!(first, 15 * 60);
        assert_eq!(log.peek_at(9, t0 + first), vec![r(1)]);
        // Still unregistered every time: the waits double up to the cap, and
        // the receipt is dropped after MAX_RECEIPT_TRIES posts in all.
        let mut now = t0 + first;
        let mut total = first;
        for post in 2..=MAX_RECEIPT_TRIES {
            let b = log.peek_at(9, now);
            assert_eq!(b, vec![r(1)], "post {post}");
            let back = log
                .commit_with_retry_at(&b, &[(r(1), Retry::Unregistered)], now)
                .1;
            assert_eq!(back, usize::from(post < MAX_RECEIPT_TRIES), "post {post}");
            let w = policy.wait(Retry::Unregistered, post).as_secs();
            now += w;
            total += w;
        }
        assert!(
            total > 12 * 3600,
            "an unregistered shard gets hours: {total} s"
        );
        assert_eq!(log.pending(), 0);
        assert_eq!(log.dropped(), 1);
        // A retry list naming a receipt outside the batch puts nothing back.
        log.record(r(5));
        assert_eq!(
            log.commit_with_retry_at(&log.peek_at(1, now), &[(r(6), Retry::Payment)], now),
            (1, 0)
        );
        // A payment not seen yet waits less.
        assert!(policy.wait(Retry::Payment, 1) < policy.wait(Retry::Unregistered, 1));
    }

    /// REV-L1-F6: a batch stays in the log until it is committed, so a failed
    /// `credit` transaction loses nothing.
    #[test]
    fn a_batch_is_removed_only_after_commit() {
        let log = ReceiptLog::new();
        for i in 0..3 {
            log.record(r(i));
        }
        let batch = log.peek(2);
        assert_eq!(batch, vec![r(0), r(1)]);
        // The transaction failed: nothing committed, the same batch again.
        assert_eq!(log.peek(2), batch);
        assert_eq!(log.pending(), 3);
        assert_eq!(log.commit(&batch), 2);
        assert_eq!(log.peek(5), vec![r(2)]);
        // Committing a stale batch removes nothing else.
        assert_eq!(log.commit(&batch), 0);
        assert_eq!(log.pending(), 1);
    }

    fn rr(i: u64) -> Receipt {
        Receipt {
            round: i,
            shard: Hash32([i as u8; 32]),
            amount: 2_500 + i,
            payer: [0xaa; 20],
            nonce: [i as u8; 32],
        }
    }

    #[test]
    fn json_round_trips() {
        let r = rr(7);
        assert_eq!(Receipt::from_json(&r.to_json()), Some(r));
        assert_eq!(Receipt::from_json(&json!({"round": 1})), None);
    }

    /// REV-11: a recorded receipt is on disk when `record` returns; the
    /// commits, the re-added retries and their waits survive a restart.
    #[test]
    fn a_persistent_log_survives_a_restart() {
        let d = test_dir("receipts");
        let p = d.join("receipts.jsonl");
        let t0 = 2_000_000;
        {
            let log = open(&p).expect("open");
            assert!(log.is_persistent());
            for i in 0..4 {
                log.record(rr(i));
            }
            // A post: 0 applied, 1 skipped with why 2 (retry), 2 not in the batch.
            let batch = log.peek_at(2, t0);
            assert_eq!(
                log.commit_with_retry_at(&batch, &[(rr(1), Retry::Payment)], t0),
                (2, 1)
            );
            // Killed here: nothing else runs.
        }
        let log = open(&p).expect("restart");
        assert_eq!(log.peek_at(9, t0), vec![rr(2), rr(3)], "rr(1) still waits");
        assert_eq!(log.peek_at(9, t0 + 30), vec![rr(2), rr(3), rr(1)]);
        assert_eq!(log.write_errors(), 0);
        drop(log);
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L3-F1: compaction runs from the journal's own live entries, so a
    /// receipt put back in the same write (or recorded right after) is never
    /// left out of the compacted file, and a later commit of it never
    /// writes a removal the file does not know. Restart works after both.
    #[test]
    fn compaction_never_loses_a_requeued_receipt() {
        let d = test_dir("receipts-compact");
        let p = d.join("receipts.jsonl");
        let t0 = 3_000_000;
        let big = |i: u32| {
            let mut nonce = [0u8; 32];
            nonce[..4].copy_from_slice(&i.to_be_bytes());
            Receipt {
                round: 1,
                shard: Hash32([1; 32]),
                amount: 2_500,
                payer: [0xaa; 20],
                nonce,
            }
        };
        {
            let log = open(&p).expect("open");
            for i in 0..1_100 {
                log.record(big(i));
            }
            let batch = log.peek_at(1_030, t0);
            // `credit` skipped receipt 5 with why 4: it goes back.
            let (removed, back) =
                log.commit_with_retry_at(&batch, &[(big(5), Retry::Unregistered)], t0);
            assert_eq!((removed, back), (1_030, 1));
            assert_eq!(log.pending(), 71);
            assert_eq!(log.write_errors(), 0);
        }
        {
            let log = open(&p).expect("restart after compaction");
            assert_eq!(log.pending(), 71, "the requeued receipt is on disk");
            assert!(log.peek_at(100, t0 + 86_400).contains(&big(5)));
            // The reporter commits everything left, the requeued one too.
            let rest = log.peek_at(100, t0 + 86_400);
            assert_eq!(log.commit_with_retry_at(&rest, &[], t0 + 86_400).0, 71);
            assert_eq!(log.write_errors(), 0);
        }
        assert_eq!(open(&p).expect("restart again").pending(), 0);
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-L4-F2: a wait stored while the clock ran ten years ahead is
    /// pulled back to at most `max_wait` after a restart with the right
    /// clock: the receipt is due again within hours, never stranded.
    #[test]
    fn a_wait_written_while_the_clock_ran_ahead_is_pulled_back() {
        let d = test_dir("receipts-clock");
        let p = d.join("receipts.jsonl");
        let ahead = now_unix() + 10 * 365 * 86_400;
        {
            let log = open(&p).expect("open");
            log.record(rr(1));
            assert_eq!(
                log.commit_with_retry_at(&[rr(1)], &[(rr(1), Retry::Payment)], ahead),
                (1, 1)
            );
        }
        let max = RetryPolicy::default().max_wait.as_secs();
        let log = open(&p).expect("restart");
        assert_eq!(log.pending(), 1);
        let now = now_unix();
        assert!(log.peek_at(9, now).is_empty(), "still waits a little");
        assert_eq!(
            log.peek_at(9, now + max + 1),
            vec![rr(1)],
            "due within max_wait"
        );
        // A shorter policy pulls it back further.
        drop(log);
        let quick = open(&p).expect("reopen").with_policy(RetryPolicy {
            max_wait: Duration::ZERO,
            ..RetryPolicy::default()
        });
        assert_eq!(quick.peek_at(9, now_unix() + 1), vec![rr(1)]);
        std::fs::remove_dir_all(d).ok();
    }

    /// From review REV-L4: random records, commits and requeues across many
    /// compactions and reopens; every reopen holds exactly what a model of
    /// the log says (REV-L3-F1, F6).
    #[test]
    fn random_ops_across_compactions_match_a_model() {
        let d = test_dir("receipts-fuzz");
        let p = d.join("receipts.jsonl");
        let policy = RetryPolicy {
            max_tries: 250,
            payment_base: Duration::ZERO,
            unregistered_base: Duration::ZERO,
            max_wait: Duration::ZERO,
        };
        let receipt = |i: u32| {
            let mut nonce = [0u8; 32];
            nonce[28..].copy_from_slice(&i.to_be_bytes());
            Receipt {
                round: 1,
                shard: Hash32([1; 32]),
                amount: 2_500,
                payer: [0xaa; 20],
                nonce,
            }
        };
        let id_of = |r: &Receipt| u32::from_be_bytes(r.nonce[28..].try_into().expect("4"));
        let mut log = open(&p).expect("open").with_policy(policy);
        let mut model: std::collections::BTreeMap<u32, u8> = Default::default();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut next = 0u32;
        for step in 0..20_000 {
            match rnd(10) {
                0..=4 => {
                    for _ in 0..1 + rnd(40) {
                        log.record(receipt(next));
                        model.insert(next, 0);
                        next += 1;
                    }
                }
                5..=8 => {
                    let batch = log.peek_at(64, u64::MAX / 2);
                    let retry: Vec<(Receipt, Retry)> = batch
                        .iter()
                        .filter(|_| rnd(3) == 0)
                        .map(|r| (*r, Retry::Payment))
                        .collect();
                    for r in &batch {
                        let i = id_of(r);
                        match retry.iter().find(|(x, _)| x == r) {
                            Some(_) if model[&i] + 1 < 250 => {
                                let t = model[&i] + 1;
                                model.insert(i, t);
                            }
                            _ => {
                                model.remove(&i);
                            }
                        }
                    }
                    log.commit_with_retry_at(&batch, &retry, 0);
                }
                _ => {
                    drop(log);
                    log = open(&p).expect("reopen").with_policy(policy);
                }
            }
            assert_eq!(log.write_errors(), 0, "step {step}");
            if step % 997 == 0 || step == 19_999 {
                drop(log);
                log = open(&p).expect("reopen").with_policy(policy);
                let mut got: Vec<u32> = log
                    .peek_at(usize::MAX, u64::MAX / 2)
                    .iter()
                    .map(id_of)
                    .collect();
                got.sort_unstable();
                let want: Vec<u32> = model.keys().copied().collect();
                assert_eq!(got, want, "step {step}");
            }
        }
        std::fs::remove_dir_all(d).ok();
    }

    /// A journal write that fails keeps the receipt in memory (the payment
    /// is settled), counts the error, and stops writing.
    /// OPS-L2-F1: a recovery that records a payment already waiting (a
    /// crash between the receipt and its intent's removal) keeps it once.
    #[test]
    fn a_receipt_recorded_twice_is_kept_once() {
        let log = ReceiptLog::new();
        log.record(rr(1));
        log.record(rr(1));
        log.record(rr(2));
        assert_eq!(log.pending(), 2);
    }

    #[test]
    fn a_write_error_keeps_the_receipt_in_memory_and_is_counted() {
        let d = test_dir("receipts-ro");
        let p = d.join("receipts.jsonl");
        let log = open(&p).expect("open");
        log.record(rr(0));
        log.inner
            .lock()
            .expect("lock")
            .journal
            .as_mut()
            .expect("journal")
            .break_for_test();
        log.record(rr(1));
        log.record(rr(2));
        assert_eq!(log.pending(), 3);
        assert_eq!(log.write_errors(), 1, "one failure, then no more writes");
        assert!(!log.is_persistent());
        assert_eq!(log.drain(9), vec![rr(0), rr(1), rr(2)]);
        drop(log);
        // The file holds what was synced before the failure: rr(0).
        assert_eq!(open(&p).expect("reopen").peek(9), vec![rr(0)]);
        std::fs::remove_dir_all(d).ok();
    }
}
