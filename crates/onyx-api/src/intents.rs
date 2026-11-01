//! Settle intents (OPS-L2-F1, OPS-L3; closes THREATS M7-F5 for
//! attribution): every paid settle is written down, durably, BEFORE the
//! settle call is made, as an intent naming the payer, nonce, round, shard,
//! amount and the authorization's `validBefore`. It ends in one of these
//! ways:
//!
//! * The settle succeeds and its outcome is durable: the receipt is on disk
//!   (facilitator; [`ReceiptLog::record`] said so) or the shard was credited
//!   in the settle transaction itself (self-settle). Then the intent is
//!   removed ([`IntentLog::end`]).
//! * Anything else (a timeout, a transport error, a refusal, a crash, a
//!   SIGKILL, a stop cut short, a receipt that could not be made durable):
//!   the intent stays on disk. The [`IntentResolver`] later reads the chain
//!   ([`crate::arrival::judge`]): if the token's own logs show this
//!   authorization's `Transfer` of its exact value into `OnyxRevenue` (the
//!   log right after its `AuthorizationUsed`), the intent becomes a receipt,
//!   and is removed only once that receipt is durable. If, at a block past
//!   `validBefore` + [`EXPIRY_GRACE`] by the chain's own clock, the token
//!   says the authorization is unused, or its use was found not paying us,
//!   or (used) full reads of its whole span find no payment of ours, the
//!   intent is dropped; a cancel (`AuthorizationCanceled`) drops it at any
//!   time. A node error, or a node that is behind, keeps it.
//!
//! What this guarantees: while the state directory's files survive, a paid
//! settle whose money reached `OnyxRevenue` ends as a durable receipt, or
//! stays an intent until one can be written; an intent is never dropped
//! because of the local clock, a lagging node, a single clamped log read,
//! or a receipt log that cannot write. It does not survive the loss or
//! deliberate editing of the state directory (THREATS REV-L4-F4). Writing
//! the intent fails closed: if it cannot be made durable, the log is full,
//! the clocks disagree, the authorization already has an open intent, or
//! the payer already has [`MAX_OPEN_PER_PAYER`] unsettled ones, nothing is
//! settled and the payer's unit comes back.
//!
//! The identical retry (M7L3-R1-1, M7L3-R2-1): each intent keeps the
//! settled-payment key of the paid request it was written for (header bytes,
//! resource, client; a hash, kept in the journal entry). While the intent is
//! open, that request's identical retry takes it over and runs the
//! authorization check ([`IntentLog::adopt`]). Once the resolver proves the
//! transfer, the receipt is written and the intent is replaced, in the same
//! journal line, by a proof (its transaction) that the identical retry is
//! served from, with no new settle, for [`PROVEN_WINDOW_SECS`]
//! ([`IntentLog::take_proven`]). Both survive a restart. The proof serves
//! once: it is deleted from the journal before its retry is served
//! (M7L3-R3-2; later retries get the settled-response cache), no proof is
//! made for an intent a retry adopted meanwhile (M7L3-R3-3), and an intent
//! whose request was served though its receipt could not be written loses
//! its request key ([`IntentLog::forget_request`], M7L3-R3-4).
//!
//! Recovery is idempotent: a receipt recorded twice (a crash between the
//! receipt and the intent's removal) is kept once in the log (one per
//! `(payer, nonce)`) and skipped on chain as already credited.
//!
//! Every intent ends (OPS-L4, the invariant behind the caps): whatever the
//! payer does (cancel the authorization, spend it elsewhere, let it expire,
//! make our settle fail), an intent no settle owns reaches a receipt or is
//! dropped within `validBefore` + [`EXPIRY_GRACE`] + a bounded resolver
//! delay, all measured in chain time. The pieces:
//!
//! * the authorization's life is bounded before verify (`validBefore` at
//!   most `maxTimeoutSeconds` + slack ahead, and at least the settle budget
//!   ahead), so the span its use can lie in is bounded ([`Intent::new`]);
//! * the span is computed in chain time, guarded by a clock-skew check
//!   ([`MAX_CLOCK_SKEW_SECS`]: new paid requests get 503 while the server's
//!   clock and the chain disagree by more);
//! * a cancel ends an intent at once, and a used authorization with no
//!   payment of ours in its whole span ends it after full reads
//!   ([`crate::arrival::judge`]);
//! * one open intent per `(payer, nonce)`, and at most
//!   [`MAX_OPEN_PER_PAYER`] released (unsettled) ones per payer;
//! * each pass judges the earliest-expiring intents first, at a bounded cost.
//!
//! So filling [`MAX_INTENTS`] needs that many funded payer addresses, each
//! making a payment fail, again every `validBefore` + grace (about 15 to 20
//! minutes with the defaults), within the per-client payment-attempt limits;
//! the `cancelled` and `expiredUnused` counters on `/v1/health` show it.

use crate::arrival::{head_time, judge, Cursor, Payment, Span, Verdict};
use crate::journal::{Binding, Journal};
use crate::receipts::{record_off_thread, Receipt, ReceiptLog};
use crate::roots::JsonRpc;
use onyx_core::Hash32;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The journal kind of a settle-intent log.
pub const JOURNAL_KIND: &str = "onyx-settle-intents";

/// Intents kept at most; a full log refuses new settles (fail closed: the
/// payer's unit comes back and nothing is charged).
pub const MAX_INTENTS: usize = 100_000;

/// Open intents one payer may have at once (OPS-L3-F4): a payer whose
/// authorizations keep ending unsettled cannot fill the log. Beyond it the
/// API answers 429 and nothing is charged.
pub const MAX_OPEN_PER_PAYER: usize = 4;

/// `eth_getLogs` windows one resolver pass reads in all (OPS-L3-F4); a long
/// search resumes on the next pass from its cursor.
pub const MAX_LOG_CALLS_PER_PASS: u64 = 400;

/// Intents one resolver pass judges (oldest first); the rest wait a pass.
pub const MAX_INTENTS_PER_PASS: usize = 200;

/// How long (seconds, this server's clock) the identical retry of a paid
/// request whose intent the resolver recovered is served from that proof
/// (M7L3-R2-1): the settled-response replay window
/// ([`crate::cache::SETTLED_TTL`]).
pub const PROVEN_WINDOW_SECS: u64 = 600;

/// Proofs kept at most; past it a recovered intent keeps no proof (its
/// receipt is written all the same).
pub const MAX_PROVEN: usize = MAX_INTENTS;

/// Re-sends of one authorization's settle after the authorization check
/// found it unused, by its first request and its identical retries together
/// (M7L3-R2-5; memory only). Past it the check still runs, but nothing is
/// sent to the facilitator again.
pub const MAX_RESENDS: u8 = 3;

/// Why an intent was not written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginError {
    /// It could not be made durable (the journal failed): 503.
    NotDurable,
    /// The log holds [`MAX_INTENTS`]: 503.
    Full,
    /// The payer already has [`MAX_OPEN_PER_PAYER`] unsettled intents: 429.
    PayerBusy,
    /// This authorization (payer, nonce) already has an open intent, for
    /// any request (OPS-L4-F4): 409.
    InUse,
    /// The server's clock and the chain's differ by more than
    /// [`MAX_CLOCK_SKEW_SECS`]: 503.
    ClockSkew,
}

/// How long after an authorization's `validBefore` an intent without proof
/// of arrival is dropped: by then any transaction using it is long mined.
pub const EXPIRY_GRACE: Duration = Duration::from_secs(10 * 60);

/// How long before an intent was written its authorization may have been
/// used (journals written before OPS-L4 carry no span: their search starts
/// this long before `created`).
pub const USE_SLACK_SECS: u64 = 600;

/// The most the server's clock may differ from the chain's (head block
/// timestamp) for new paid settles to be accepted (OPS-L4-F3): the span an
/// authorization's use is searched in is computed from it.
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;

/// One paid settle about to be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Intent {
    /// The receipt the settle earns (round, shard, amount, payer, nonce).
    pub receipt: Receipt,
    /// The authorization's `validAfter` (unix seconds).
    pub valid_after: u64,
    /// The authorization's `validBefore` (unix seconds).
    pub valid_before: u64,
    /// Where the search for its use starts (chain time, unix seconds); see
    /// [`Intent::new`].
    pub earliest: u64,
    /// When the intent was written (unix seconds, this server's clock).
    pub created: u64,
}

impl Intent {
    /// The intent for a settle of `receipt` with an authorization valid from
    /// `valid_after` to `valid_before`, written at `created`, by a server
    /// that accepts authorizations living at most `max_life` seconds.
    ///
    /// Our settle happens after verify, which happened at most `max_life`
    /// before `valid_before` by this server's clock, and that clock is within
    /// [`MAX_CLOCK_SKEW_SECS`] of the chain's (or no settle is made); the
    /// token refuses any use before `valid_after`. So in chain time the
    /// settle lies in `max(valid_after, valid_before - max_life - skew) ..
    /// valid_before`: a bounded span, read in full before the intent is
    /// dropped (OPS-L4-F1/F3).
    pub fn new(
        receipt: Receipt,
        valid_after: u64,
        valid_before: u64,
        created: u64,
        max_life: u64,
    ) -> Intent {
        let earliest = valid_before
            .saturating_sub(max_life.saturating_add(MAX_CLOCK_SKEW_SECS))
            .max(valid_after);
        Intent {
            receipt,
            valid_after,
            valid_before,
            earliest,
            created,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "receipt": self.receipt.to_json(),
            "validAfter": self.valid_after,
            "validBefore": self.valid_before,
            "earliest": self.earliest,
            "created": self.created,
        })
    }

    fn from_json(v: &Value) -> Option<Intent> {
        let created = v.get("created")?.as_u64()?;
        let old = created.saturating_sub(USE_SLACK_SECS);
        Some(Intent {
            receipt: Receipt::from_json(v.get("receipt")?)?,
            valid_after: v.get("validAfter").and_then(Value::as_u64).unwrap_or(old),
            valid_before: v.get("validBefore")?.as_u64()?,
            earliest: v.get("earliest").and_then(Value::as_u64).unwrap_or(old),
            created,
        })
    }

    /// The journal entry of this intent, written for the paid request
    /// `request` (M7L3-R2-1: the key survives a restart).
    fn entry(self, request: Option<&Hash32>) -> Value {
        let mut v = self.to_json();
        if let Some(r) = request {
            v["request"] = json!(r.to_string());
        }
        v
    }

    /// The span (chain time) the authorization's use is searched in.
    pub fn span(&self) -> Span {
        Span {
            valid_after: self.valid_after,
            earliest: self.earliest,
            valid_before: self.valid_before,
        }
    }
}

struct Open {
    id: u64,
    intent: Intent,
    /// A settle task of this process still owns it: the resolver leaves it.
    live: bool,
    /// The paid request it was written for (the settled-payment key: header
    /// bytes, resource, client; in the journal entry, so it survives a
    /// restart, M7L3-R2-1; `None` for entries written before): its
    /// identical retry may take the intent over ([`IntentLog::adopt`],
    /// M7L3-R1-1).
    request: Option<Hash32>,
    /// Where the resolver's search for its use stands (memory only; a
    /// restart searches again).
    cursor: Cursor,
    /// Settle re-sends made for it (memory only, M7L3-R2-5).
    resends: u8,
}

/// The proof the resolver found for an intent written for a paid request
/// (M7L3-R2-1): the identical retry of that request is served from it, once,
/// with no new settle, until `until`. The receipt is already on disk. Kept in
/// the journal (an intent entry with a `proven` object), so a restart keeps
/// it until it is served: serving deletes it from the journal first
/// (M7L3-R3-2). An older server reads the entry as an intent and judges it
/// again (its receipt is then a duplicate the log drops).
struct Proven {
    id: u64,
    request: Hash32,
    intent: Intent,
    /// The transaction that moved the payment into `OnyxRevenue`.
    tx: String,
    /// Served from until then (unix seconds, this server's clock).
    until: u64,
}

impl Proven {
    fn entry(&self) -> Value {
        let mut v = self.intent.entry(Some(&self.request));
        v["proven"] = json!({ "tx": self.tx, "until": self.until });
        v
    }
}

/// A journal entry's `request` key, if it has a well-formed one.
fn request_of(v: &Value) -> Option<Hash32> {
    let s = v.get("request")?.as_str()?;
    let b = hex::decode(s.strip_prefix("0x")?).ok()?;
    Some(Hash32(b.try_into().ok()?))
}

#[derive(Default)]
struct Inner {
    list: Vec<Open>,
    /// Proofs of recovered intents, for their requests' identical retries.
    proven: Vec<Proven>,
    journal: Option<Journal>,
    next_mem: u64,
    write_errors: u64,
    recovered: u64,
    expired: u64,
    cancelled: u64,
    /// This server's clock minus the chain's (head timestamp), seconds, as
    /// last measured by the resolver; `None` until then.
    skew: Option<i64>,
}

impl Inner {
    /// Journal the change. `Err` when it is not durable: the journal is
    /// closed for good and the error counted (the API then refuses new paid
    /// requests, as for the receipt log, REV-L3-F5).
    fn write(&mut self, del: &[u64], add: &[Value]) -> Result<Vec<u64>, ()> {
        match self.journal.as_mut().map(|j| j.apply(del, add)) {
            Some(Ok(ids)) => Ok(ids),
            Some(Err(_)) => {
                self.write_errors += 1;
                self.journal = None;
                Err(())
            }
            None if self.write_errors > 0 => Err(()),
            None => {
                let start = self.next_mem.max(
                    self.list
                        .iter()
                        .map(|o| o.id + 1)
                        .chain(self.proven.iter().map(|p| p.id + 1))
                        .max()
                        .unwrap_or(0),
                );
                self.next_mem = start + add.len() as u64;
                Ok((start..self.next_mem).collect())
            }
        }
    }
}

/// The settle intents of one server (see the module docs).
#[derive(Default)]
pub struct IntentLog {
    inner: Mutex<Inner>,
}

impl IntentLog {
    /// In memory only (tests, and a server with no state directory).
    pub fn new() -> Self {
        Self::default()
    }

    /// The log kept in the journal at `path` (created if missing), bound to
    /// one chain and `OnyxRevenue`, with the intents it already holds: none
    /// of them is owned by a running settle any more, so the resolver
    /// judges them all. Refuses a damaged journal, one bound elsewhere, and
    /// one another process holds.
    pub fn open(path: impl AsRef<Path>, bind: &Binding) -> std::io::Result<Self> {
        let (journal, live) = Journal::open(path, JOURNAL_KIND, bind)?;
        let mut list = Vec::with_capacity(live.len());
        let mut proven = Vec::new();
        for (id, v) in live {
            let bad = |what: &str| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("settle intent journal entry {id} is not {what}"),
                )
            };
            let intent = Intent::from_json(&v).ok_or_else(|| bad("an intent"))?;
            let request = request_of(&v);
            match v.get("proven") {
                Some(p) => proven.push(Proven {
                    id,
                    request: request.ok_or_else(|| bad("a proof (no request)"))?,
                    intent,
                    tx: p
                        .get("tx")
                        .and_then(Value::as_str)
                        .ok_or_else(|| bad("a proof (no tx)"))?
                        .to_string(),
                    until: p
                        .get("until")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| bad("a proof (no until)"))?,
                }),
                None => list.push(Open {
                    id,
                    intent,
                    live: false,
                    request,
                    cursor: Cursor::default(),
                    resends: 0,
                }),
            }
        }
        Ok(IntentLog {
            inner: Mutex::new(Inner {
                list,
                proven,
                journal: Some(journal),
                ..Inner::default()
            }),
        })
    }

    /// Whether the log is kept on disk.
    pub fn is_persistent(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.journal.is_some())
            .unwrap_or(false)
    }

    /// Write `i` (durable when a journal is open) and mark it owned by the
    /// caller's settle. An error when it could not be made durable, the log
    /// is full, or the payer has [`MAX_OPEN_PER_PAYER`] open intents: the
    /// caller must not settle. Blocks on the disk: from async code use
    /// [`begin_off_thread`].
    pub fn begin(&self, i: Intent) -> Result<u64, BeginError> {
        self.begin_for(i, None)
    }

    /// [`IntentLog::begin`] for the paid request `request` (its
    /// settled-payment key), which [`IntentLog::adopt`] matches.
    pub fn begin_for(&self, i: Intent, request: Option<Hash32>) -> Result<u64, BeginError> {
        let mut g = self.inner.lock().map_err(|_| BeginError::NotDurable)?;
        if g.skew
            .is_some_and(|s| s.unsigned_abs() > MAX_CLOCK_SKEW_SECS)
        {
            return Err(BeginError::ClockSkew);
        }
        if g.list.len() >= MAX_INTENTS {
            return Err(BeginError::Full);
        }
        let (payer, nonce) = (i.receipt.payer, i.receipt.nonce);
        if g.list
            .iter()
            .any(|o| o.intent.receipt.payer == payer && o.intent.receipt.nonce == nonce)
        {
            return Err(BeginError::InUse);
        }
        // Only unsettled intents count (OPS-L4-F5): settles in flight are
        // bounded by the facilitator slots, and an honest client's
        // concurrent payments are not refused.
        let mine = g
            .list
            .iter()
            .filter(|o| !o.live && o.intent.receipt.payer == payer)
            .count();
        if mine >= MAX_OPEN_PER_PAYER {
            return Err(BeginError::PayerBusy);
        }
        let id = *g
            .write(&[], &[i.entry(request.as_ref())])
            .map_err(|_| BeginError::NotDurable)?
            .first()
            .ok_or(BeginError::NotDurable)?;
        g.list.push(Open {
            id,
            intent: i,
            live: true,
            request,
            cursor: Cursor::default(),
            resends: 0,
        });
        Ok(id)
    }

    /// The intent no settle owns that was written for the paid request
    /// `request` (its identical retry, M7L3-R1-1), if any.
    pub fn awaiting(&self, request: &Hash32) -> Option<Intent> {
        self.inner.lock().ok().and_then(|g| {
            g.list
                .iter()
                .find(|o| !o.live && o.request.as_ref() == Some(request))
                .map(|o| o.intent)
        })
    }

    /// The identical retry of a paid request whose settle ended unknown or
    /// refused takes its open intent over (M7L3-R1-1): the intent no settle
    /// owns, written for exactly this request and this receipt, becomes the
    /// caller's (the resolver leaves it) and its id is returned. `None`
    /// otherwise (another request, another receipt, owned by a running
    /// settle, or resolved meanwhile).
    pub fn adopt(&self, request: &Hash32, receipt: &Receipt) -> Option<u64> {
        let mut g = self.inner.lock().ok()?;
        let o = g.list.iter_mut().find(|o| {
            !o.live && o.request.as_ref() == Some(request) && o.intent.receipt == *receipt
        })?;
        o.live = true;
        Some(o.id)
    }

    /// The proof the resolver kept for the paid request `request`
    /// (M7L3-R2-1): the recovered intent (its receipt is on disk) and the
    /// transaction that paid, while `now` is inside its window and its
    /// retry was not served from it yet.
    pub fn proven(&self, request: &Hash32, now: u64) -> Option<(Intent, String)> {
        self.inner.lock().ok().and_then(|g| {
            g.proven
                .iter()
                .find(|p| p.request == *request && now < p.until)
                .map(|p| (p.intent, p.tx.clone()))
        })
    }

    /// Serve the identical retry of `request` from its proof (M7L3-R2-1):
    /// the proof for exactly this request and this receipt, inside its
    /// window, is deleted from the journal (one durable line, M7L3-R3-2: a
    /// restart cannot serve it again) and its transaction returned. `None`
    /// otherwise, and when the deletion is not durable (fail closed: the
    /// proof stays, the journal is closed and counted, and the next start
    /// serves it once). Blocks on the disk: from async code use
    /// [`take_proven_off_thread`].
    pub fn take_proven(&self, request: &Hash32, receipt: &Receipt, now: u64) -> Option<String> {
        let mut g = self.inner.lock().ok()?;
        let i = g
            .proven
            .iter()
            .position(|p| p.request == *request && p.intent.receipt == *receipt && now < p.until)?;
        let id = g.proven[i].id;
        g.write(&[id], &[]).ok()?;
        Some(g.proven.remove(i).tx)
    }

    /// The settle of intent `id` succeeded and its request was served, but
    /// the receipt could not be made durable, so the intent stays for the
    /// resolver (OPS-L3-F1). Its request key is dropped (M7L3-R3-4): the
    /// entry is rewritten without it, in one durable line, so after a
    /// restart (which loses the settled-response cache) the identical retry
    /// neither takes it over nor gets a proof. Called while the settle still
    /// owns it; returns its new id (the same entry when the write fails:
    /// the journal is then closed and counted).
    pub fn forget_request(&self, id: u64) -> u64 {
        let Ok(mut g) = self.inner.lock() else {
            return id;
        };
        let Some(i) = g.list.iter().position(|o| o.id == id) else {
            return id;
        };
        g.list[i].request = None;
        let entry = g.list[i].intent.entry(None);
        match g
            .write(&[id], &[entry])
            .ok()
            .and_then(|ids| ids.first().copied())
        {
            Some(new) => {
                g.list[i].id = new;
                new
            }
            None => id,
        }
    }

    /// Proofs kept (not served yet, until their window ends).
    pub fn proven_count(&self) -> usize {
        self.inner.lock().map(|g| g.proven.len()).unwrap_or(0)
    }

    /// Take one settle re-send for intent `id` (M7L3-R2-5): false once
    /// [`MAX_RESENDS`] were made for it (or it is gone).
    pub fn take_resend(&self, id: u64) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        match g.list.iter_mut().find(|o| o.id == id) {
            Some(o) if o.resends < MAX_RESENDS => {
                o.resends += 1;
                true
            }
            _ => false,
        }
    }

    /// The settle succeeded and its outcome is recorded elsewhere (receipt
    /// on disk, or credited on chain): remove the intent. Blocks on the
    /// disk: from async code use [`end_off_thread`]. A failed write leaves
    /// it in memory (counted); the resolver later finds it already credited.
    pub fn end(&self, id: u64) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(i) = g.list.iter().position(|o| o.id == id) {
                g.list.remove(i);
                let _ = g.write(&[id], &[]);
            }
        }
    }

    /// The settle ended without a known success: the intent stays for the
    /// resolver. Memory only (nothing changes on disk).
    pub fn release(&self, id: u64) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(o) = g.list.iter_mut().find(|o| o.id == id) {
                o.live = false;
            }
        }
    }

    /// Intents not owned by a running settle, earliest `validBefore` first
    /// (the first to expire are judged first, OPS-L4-F1).
    pub fn due(&self) -> Vec<(u64, Intent)> {
        let mut due: Vec<(u64, Intent)> = self
            .inner
            .lock()
            .map(|g| {
                g.list
                    .iter()
                    .filter(|o| !o.live)
                    .map(|o| (o.id, o.intent))
                    .collect()
            })
            .unwrap_or_default();
        due.sort_by_key(|(id, i)| (i.valid_before, *id));
        due
    }

    /// Record this server's clock minus the chain's (OPS-L4-F3).
    pub fn set_skew(&self, skew: i64) {
        if let Ok(mut g) = self.inner.lock() {
            g.skew = Some(skew);
        }
    }

    /// The last measured clock skew (this server minus the chain), seconds.
    pub fn skew(&self) -> Option<i64> {
        self.inner.lock().ok().and_then(|g| g.skew)
    }

    /// Intents dropped because the payer cancelled the authorization.
    pub fn cancelled(&self) -> u64 {
        self.inner.lock().map(|g| g.cancelled).unwrap_or(0)
    }

    fn cursor(&self, id: u64) -> Cursor {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.list.iter().find(|o| o.id == id).map(|o| o.cursor))
            .unwrap_or_default()
    }

    fn set_cursor(&self, id: u64, c: Cursor) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(o) = g.list.iter_mut().find(|o| o.id == id) {
                o.cursor = c;
            }
        }
    }

    /// Remove resolved intents: `recovered` became receipts (with the
    /// transaction that paid), `expired` and `cancelled` were dropped. A
    /// recovered intent written for a paid request is replaced by its proof
    /// (M7L3-R2-1), kept until `now` + [`PROVEN_WINDOW_SECS`]; proofs whose
    /// window ended by `now` go. One durable line.
    fn resolved(&self, recovered: &[(u64, String)], expired: &[u64], cancelled: &[u64], now: u64) {
        let Ok(mut g) = self.inner.lock() else {
            return;
        };
        let rec: Vec<u64> = recovered.iter().map(|(id, _)| *id).collect();
        let mut del: Vec<u64> = rec
            .iter()
            .chain(expired)
            .chain(cancelled)
            .copied()
            .filter(|id| g.list.iter().any(|o| o.id == *id))
            .collect();
        let old: Vec<u64> = g
            .proven
            .iter()
            .filter(|p| p.until <= now)
            .map(|p| p.id)
            .collect();
        if del.is_empty() && old.is_empty() {
            return;
        }
        let room = MAX_PROVEN.saturating_sub(g.proven.len() - old.len());
        let until = now.saturating_add(PROVEN_WINDOW_SECS);
        let mut keep: Vec<Proven> = recovered
            .iter()
            .filter_map(|(id, tx)| {
                // An intent a retry adopted while this pass judged it
                // (`live`) gets no proof (M7L3-R3-3): that retry serves from
                // its own check.
                let o = g.list.iter().find(|o| o.id == *id && !o.live)?;
                Some(Proven {
                    id: 0,
                    request: o.request?,
                    intent: o.intent,
                    tx: tx.clone(),
                    until,
                })
            })
            .collect();
        keep.truncate(room);
        let add: Vec<Value> = keep.iter().map(Proven::entry).collect();
        g.list.retain(|o| !del.contains(&o.id));
        g.proven.retain(|p| !old.contains(&p.id));
        del.extend(old);
        if let Ok(ids) = g.write(&del, &add) {
            for (mut p, id) in keep.into_iter().zip(ids) {
                p.id = id;
                g.proven.push(p);
            }
        }
        g.recovered += recovered.len() as u64;
        g.expired += expired.len() as u64;
        g.cancelled += cancelled.len() as u64;
    }

    /// Intents open (owned by a settle or waiting for the resolver).
    pub fn open_count(&self) -> usize {
        self.inner.lock().map(|g| g.list.len()).unwrap_or(0)
    }

    /// Intents turned into receipts by the resolver.
    pub fn recovered(&self) -> u64 {
        self.inner.lock().map(|g| g.recovered).unwrap_or(0)
    }

    /// Intents dropped because their authorization expired without moving
    /// money to `OnyxRevenue`.
    pub fn expired(&self) -> u64 {
        self.inner.lock().map(|g| g.expired).unwrap_or(0)
    }

    /// Journal writes that failed.
    pub fn write_errors(&self) -> u64 {
        self.inner.lock().map(|g| g.write_errors).unwrap_or(0)
    }
}

#[cfg(test)]
impl IntentLog {
    /// Run `f` while holding the log's lock (tests of what another thread
    /// sees in between, M7L3-R2-2).
    pub(crate) fn hold_lock_for_test<R>(&self, f: impl FnOnce() -> R) -> R {
        let _g = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    }

    /// Make the journal's next write fail (tests of the error path).
    pub(crate) fn break_journal_for_test(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(j) = g.journal.as_mut() {
                j.break_for_test();
            }
        }
    }
}

/// [`IntentLog::begin`] on a blocking thread (the write and its `fsync` never
/// stall an async worker).
pub async fn begin_off_thread(
    log: &Arc<IntentLog>,
    i: Intent,
    request: Option<Hash32>,
) -> Result<u64, BeginError> {
    let l = log.clone();
    tokio::task::spawn_blocking(move || l.begin_for(i, request))
        .await
        .unwrap_or(Err(BeginError::NotDurable))
}

/// [`IntentLog::take_proven`] on a blocking thread.
pub async fn take_proven_off_thread(
    log: &Arc<IntentLog>,
    request: Hash32,
    receipt: Receipt,
    now: u64,
) -> Option<String> {
    let l = log.clone();
    tokio::task::spawn_blocking(move || l.take_proven(&request, &receipt, now))
        .await
        .unwrap_or(None)
}

/// [`IntentLog::end`] on a blocking thread.
pub async fn end_off_thread(log: &Arc<IntentLog>, id: u64) {
    let l = log.clone();
    if tokio::task::spawn_blocking(move || l.end(id))
        .await
        .is_err()
    {
        log.end(id);
    }
}

/// Owns one intent for a running settle: dropped without [`IntentGuard::ended`]
/// (an error, a panic, a cancelled task) it releases the intent to the
/// resolver.
pub struct IntentGuard {
    log: Arc<IntentLog>,
    id: u64,
    ended: bool,
}

impl IntentGuard {
    /// Guard intent `id` of `log`.
    pub fn new(log: Arc<IntentLog>, id: u64) -> Self {
        IntentGuard {
            log,
            id,
            ended: false,
        }
    }

    /// The settle succeeded and its outcome is recorded: remove the intent.
    pub async fn ended(mut self) {
        self.ended = true;
        end_off_thread(&self.log, self.id).await;
    }

    /// The settle succeeded and the request is served, but its receipt
    /// could not be made durable: the intent drops its request key
    /// ([`IntentLog::forget_request`], M7L3-R3-4) and goes to the resolver.
    pub async fn served_without_receipt(mut self) {
        let (l, id) = (self.log.clone(), self.id);
        self.id = tokio::task::spawn_blocking(move || l.forget_request(id))
            .await
            .unwrap_or(id);
    }
}

impl Drop for IntentGuard {
    fn drop(&mut self) {
        if !self.ended {
            self.log.release(self.id);
        }
    }
}

/// What one resolver pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Intents whose payment reached `OnyxRevenue`: now receipts.
    pub recovered: usize,
    /// Intents dropped: the authorization expired without paying us.
    pub expired: usize,
    /// Intents dropped: the payer cancelled the authorization.
    pub cancelled: usize,
    /// Intents still waiting (not expired, no proof yet).
    pub waiting: usize,
    /// Intents whose check failed this pass (a node error); kept.
    pub errors: usize,
}

/// Turns intents into receipts or drops them, from the chain's own record
/// (see the module docs). Needs only read access to a node: it runs whether
/// or not a reporter key is configured.
pub struct IntentResolver<T: JsonRpc> {
    rpc: T,
    token: [u8; 20],
    revenue: [u8; 20],
    intents: Arc<IntentLog>,
    receipts: Arc<ReceiptLog>,
    /// This server's clock (unix seconds), compared with the chain's.
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl<T: JsonRpc> std::fmt::Debug for IntentResolver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IntentResolver")
            .field("token", &format!("0x{}", hex::encode(self.token)))
            .field("revenue", &format!("0x{}", hex::encode(self.revenue)))
            .finish_non_exhaustive()
    }
}

impl<T: JsonRpc> IntentResolver<T> {
    /// A resolver reading `rpc`, for payments in `token` to `revenue`,
    /// turning `intents` into receipts in `receipts`.
    pub fn new(
        rpc: T,
        token: [u8; 20],
        revenue: [u8; 20],
        intents: Arc<IntentLog>,
        receipts: Arc<ReceiptLog>,
    ) -> Self {
        IntentResolver {
            rpc,
            token,
            revenue,
            intents,
            receipts,
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            }),
        }
    }

    /// Compare the chain with `clock` instead of the system clock (tests,
    /// and a server with an injected clock).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// One pass over the intents no settle owns (at most
    /// [`MAX_INTENTS_PER_PASS`], reading at most [`MAX_LOG_CALLS_PER_PASS`]
    /// log windows in all).
    ///
    /// Nothing is removed while the receipt log cannot write (OPS-L3-F1): a
    /// recovered payment's intent stays until its receipt is durable, and
    /// the pass makes no other removal either. Expiry is judged by the
    /// chain's own clock at a pinned block, never by this server's clock
    /// (OPS-L3-F3).
    pub async fn resolve_once(&self) -> Resolved {
        let mut out = Resolved::default();
        // The clock check runs every pass, intents or not (OPS-L4-F3).
        if let Ok((_, t)) = head_time(&self.rpc).await {
            let local = (self.clock)();
            self.intents.set_skew(
                i64::try_from(local).unwrap_or(i64::MAX) - i64::try_from(t).unwrap_or(i64::MAX),
            );
        }
        if self.receipts.write_errors() > 0 {
            out.errors = self.intents.due().len();
            return out;
        }
        let (mut recovered, mut expired, mut cancelled) = (Vec::new(), Vec::new(), Vec::new());
        let mut budget = MAX_LOG_CALLS_PER_PASS;
        for (id, i) in self.intents.due().into_iter().take(MAX_INTENTS_PER_PASS) {
            let p = Payment {
                token: self.token,
                revenue: self.revenue,
                from: i.receipt.payer,
                nonce: i.receipt.nonce,
                value: u128::from(i.receipt.amount),
            };
            let mut cursor = self.intents.cursor(id);
            let verdict = judge(
                &self.rpc,
                &p,
                i.span(),
                EXPIRY_GRACE.as_secs(),
                &mut cursor,
                &mut budget,
            )
            .await;
            self.intents.set_cursor(id, cursor);
            match verdict {
                Ok(Verdict::Arrived(tx)) => {
                    // The receipt is on disk before the intent leaves: a
                    // crash in between records it twice, and the log keeps
                    // one (the contract would skip a second anyway). One
                    // that is not durable keeps the intent (OPS-L3-F1).
                    if record_off_thread(&self.receipts, i.receipt).await {
                        recovered.push((id, tx));
                    } else {
                        out.errors += 1;
                    }
                }
                Ok(Verdict::Never) => expired.push(id),
                Ok(Verdict::Cancelled) => cancelled.push(id),
                Ok(Verdict::Open) => out.waiting += 1,
                Err(_) => out.errors += 1,
            }
            if budget == 0 {
                break;
            }
        }
        out.recovered = recovered.len();
        out.expired = expired.len();
        out.cancelled = cancelled.len();
        // Also run with nothing resolved: proofs whose window ended go.
        let log = self.intents.clone();
        let now = (self.clock)();
        if tokio::task::spawn_blocking(move || log.resolved(&recovered, &expired, &cancelled, now))
            .await
            .is_err()
        {
            // A panicked write leaves the intents; the next pass finds
            // them again (a receipt then is a duplicate the log drops).
        }
        out
    }
}

impl<T: JsonRpc + 'static> IntentResolver<T> {
    /// Resolve every `every` until the task is aborted.
    pub fn spawn(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let r = self.resolve_once().await;
                if r.recovered > 0 || r.expired > 0 || r.cancelled > 0 {
                    eprintln!(
                        "onyx-api: settle intents: {} recovered as receipts, {} expired unused, {} cancelled by the payer, {} waiting, {} check error(s)",
                        r.recovered, r.expired, r.cancelled, r.waiting, r.errors
                    );
                }
                if let Some(s) = self.intents.skew() {
                    if s.unsigned_abs() > MAX_CLOCK_SKEW_SECS {
                        eprintln!(
                            "onyx-api: this server's clock is {s} s off the chain's: new paid requests are refused (503) until it is fixed"
                        );
                    }
                }
                tokio::time::sleep(every).await;
            }
        })
    }
}

#[cfg(test)]
#[path = "intents_tests.rs"]
mod tests;
