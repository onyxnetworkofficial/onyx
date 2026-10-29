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
mod tests {
    use super::*;
    use crate::arrival::{
        authorization_canceled_topic, authorization_used_topic, transfer_topic, word_addr,
        word_u128,
    };
    use crate::journal::{test_dir, TEST_BINDING};
    use crate::x402::BoxFuture;
    use onyx_core::Hash32;

    const TOKEN: [u8; 20] = [0x55; 20];
    const REVENUE: [u8; 20] = [0x77; 20];
    const PAYER: [u8; 20] = [0x11; 20];
    const ELSEWHERE: [u8; 20] = [0xee; 20];

    fn hx(b: &[u8]) -> String {
        format!("0x{}", hex::encode(b))
    }

    fn intent(nonce: u8, valid_before: u64) -> Intent {
        intent_from(PAYER, nonce, valid_before)
    }

    /// The longest authorization life the test server accepts (the default
    /// `maxTimeoutSeconds` 60 plus `authorization_slack` 300).
    const LIFE: u64 = 360;

    fn intent_from(payer: [u8; 20], nonce: u8, valid_before: u64) -> Intent {
        Intent::new(
            Receipt {
                round: 7,
                shard: Hash32([nonce; 32]),
                amount: 2_500,
                payer,
                nonce: [nonce; 32],
            },
            valid_before.saturating_sub(600),
            valid_before,
            valid_before.saturating_sub(60),
            LIFE,
        )
    }

    /// One transaction: its token logs in order, `(nonce, Some(to))` for an
    /// AuthorizationUsed followed by its Transfer of 2,500 to `to`, or
    /// `(nonce, None)` for an AuthorizationUsed with no Transfer after it.
    #[derive(Clone)]
    struct Tx {
        block: u64,
        /// The authorizer of every use in it.
        from: [u8; 20],
        uses: Vec<([u8; 32], Option<[u8; 20]>)>,
    }

    /// A chain where block n has timestamp n. `head` is the head every
    /// backend reports for `eth_blockNumber`; `served` the highest block a
    /// (lagging) backend serves for reads by number; `fail` fails every call.
    struct Chain {
        head: u64,
        served: u64,
        txs: Vec<Tx>,
        /// `(authorizer, nonce, block)`: cancelled with `cancelAuthorization`
        /// in `block`.
        cancels: Vec<([u8; 20], [u8; 32], u64)>,
        /// Nonces the token marks used whose logs no node returns (a use
        /// outside any span we read, or a backend that drops them).
        silent: Vec<[u8; 32]>,
        /// `eth_getLogs` answers as if the node's head were this (a
        /// load-balanced backend that silently clamps, OPS-L4-F2).
        logs_clamp: Option<u64>,
        fail: bool,
        log_calls: Mutex<u64>,
    }

    impl Chain {
        fn at(head: u64) -> Self {
            Chain {
                head,
                served: head,
                txs: Vec::new(),
                cancels: Vec::new(),
                silent: Vec::new(),
                logs_clamp: None,
                fail: false,
                log_calls: Mutex::new(0),
            }
        }
        /// A plain x402 payment: authorization `nonce` sent 2,500 to `to`
        /// in its own tx in `block`.
        fn pay(self, nonce: u8, to: [u8; 20], block: u64) -> Self {
            self.pay_from(PAYER, nonce, to, block)
        }
        fn pay_from(mut self, from: [u8; 20], nonce: u8, to: [u8; 20], block: u64) -> Self {
            self.txs.push(Tx {
                block,
                from,
                uses: vec![([nonce; 32], Some(to))],
            });
            self
        }
        fn tx(mut self, tx: Tx) -> Self {
            self.txs.push(tx);
            self
        }
        fn cancel(self, nonce: u8, block: u64) -> Self {
            self.cancel_from(PAYER, nonce, block)
        }
        fn cancel_from(mut self, from: [u8; 20], nonce: u8, block: u64) -> Self {
            self.cancels.push((from, [nonce; 32], block));
            self
        }
        fn used_by(&self, nonce: &[u8; 32], block: u64) -> bool {
            self.txs
                .iter()
                .any(|t| t.block <= block && t.uses.iter().any(|(n, _)| n == nonce))
                || self
                    .cancels
                    .iter()
                    .any(|(_, n, b)| n == nonce && *b <= block)
                || self.silent.contains(nonce)
        }
        fn receipt(&self, i: usize) -> Value {
            let t = &self.txs[i];
            let mut logs = Vec::new();
            for (n, to) in &t.uses {
                logs.push(json!({
                    "address": hx(&TOKEN),
                    "topics": [hx(&authorization_used_topic()), hx(&word_addr(&t.from)), hx(n)],
                    "data": "0x",
                }));
                if let Some(to) = to {
                    logs.push(json!({
                        "address": hx(&TOKEN),
                        "topics": [hx(&transfer_topic()), hx(&word_addr(&t.from)), hx(&word_addr(to))],
                        "data": hx(&word_u128(2_500)),
                    }));
                }
            }
            for (k, l) in logs.iter_mut().enumerate() {
                l["logIndex"] = json!(format!("0x{k:x}"));
            }
            json!({ "status": "0x1", "logs": logs })
        }
    }

    fn q(v: &Value) -> u64 {
        u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
            .unwrap_or(u64::MAX)
    }

    impl JsonRpc for Chain {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            let r = if self.fail {
                Err("node down".to_string())
            } else {
                match method {
                    "eth_blockNumber" => Ok(json!(format!("0x{:x}", self.head))),
                    "eth_getBlockByNumber" => {
                        let n = q(&params[0]);
                        if n > self.served {
                            Ok(Value::Null)
                        } else {
                            Ok(json!({ "timestamp": format!("0x{n:x}") }))
                        }
                    }
                    "eth_call" => {
                        // Never asked at "latest" by the resolver (OPS-L3-F3).
                        let block = q(&params[1]);
                        if block > self.served {
                            Err("header not found".to_string())
                        } else {
                            let data = params[0]["data"].as_str().unwrap_or("").to_string();
                            let nonce: [u8; 32] = hex::decode(&data[data.len() - 64..])
                                .expect("hex")
                                .try_into()
                                .expect("32");
                            Ok(json!(format!(
                                "0x{:064x}",
                                u8::from(self.used_by(&nonce, block))
                            )))
                        }
                    }
                    "eth_getLogs" => {
                        *self.log_calls.lock().expect("lock") += 1;
                        let (f, t) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                        assert!(t - f < crate::arrival::LOGS_WINDOW);
                        let nonce = params[0]["topics"][2].as_str().unwrap_or("").to_string();
                        let from = params[0]["topics"][1].as_str().unwrap_or("").to_string();
                        // Both events are asked for (OPS-L4-F1).
                        assert_eq!(
                            params[0]["topics"][0],
                            json!([
                                hx(&authorization_used_topic()),
                                hx(&authorization_canceled_topic())
                            ])
                        );
                        let top = t.min(self.served).min(self.logs_clamp.unwrap_or(u64::MAX));
                        let mut hits: Vec<Value> = self
                            .txs
                            .iter()
                            .enumerate()
                            .filter(|(_, tx)| {
                                (f..=top).contains(&tx.block)
                                    && hx(&word_addr(&tx.from)) == from
                                    && tx.uses.iter().any(|(n, _)| hx(n) == nonce)
                            })
                            .map(|(i, _)| json!({
                                "address": hx(&TOKEN),
                                "topics": [hx(&authorization_used_topic()), from.clone(), nonce.clone()],
                                "transactionHash": format!("0x{i:x}"),
                            }))
                            .collect();
                        hits.extend(
                            self.cancels
                                .iter()
                                .filter(|(a, n, b)| {
                                    (f..=top).contains(b)
                                        && hx(&word_addr(a)) == from
                                        && hx(n) == nonce
                                })
                                .map(|_| json!({
                                    "address": hx(&TOKEN),
                                    "topics": [hx(&authorization_canceled_topic()), from.clone(), nonce.clone()],
                                    "transactionHash": "0xcancel",
                                })),
                        );
                        Ok(json!(hits))
                    }
                    "eth_getTransactionReceipt" => Ok(self.receipt(q(&params[0]) as usize)),
                    _ => Err(format!("unexpected {method}")),
                }
            };
            Box::pin(async move { r })
        }
    }

    /// Paused tokio time (ROADMAP E12): deadlines and polls follow the
    /// test's order, not the machine's speed.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("runtime")
    }

    /// A resolver whose clock agrees with the chain's head (block n is at
    /// time n).
    fn resolver(
        chain: Chain,
        intents: &Arc<IntentLog>,
        receipts: &Arc<ReceiptLog>,
    ) -> IntentResolver<Chain> {
        let now = chain.head;
        IntentResolver::new(chain, TOKEN, REVENUE, intents.clone(), receipts.clone())
            .with_clock(Arc::new(move || now))
    }

    /// An intent no settle owns any more.
    fn released(intents: &IntentLog, i: Intent) -> u64 {
        let id = intents.begin(i).expect("begin");
        intents.release(id);
        id
    }

    const VB: u64 = 1_000_000;
    const GRACE: u64 = EXPIRY_GRACE.as_secs();

    /// A settle that timed out after the facilitator broadcast it: the
    /// payment reached OnyxRevenue, so the intent becomes a receipt.
    #[test]
    fn a_settle_that_timed_out_after_broadcast_is_recovered() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        let i = intent(1, VB);
        released(&intents, i);
        let chain = Chain::at(VB - 10).pay(1, REVENUE, VB - 30);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        assert_eq!(receipts.peek(10), vec![i.receipt]);
        assert_eq!(intents.open_count(), 0);
        assert_eq!(intents.recovered(), 1);
    }

    /// A settle still owned by a running task is never judged.
    #[test]
    fn a_live_settle_is_left_alone() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        intents.begin(intent(2, VB)).expect("begin");
        let chain = Chain::at(VB + GRACE + 100).pay(2, REVENUE, VB - 30);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r, Resolved::default());
        assert_eq!(receipts.pending(), 0);
        assert_eq!(intents.open_count(), 1);
    }

    /// SIGKILL mid-settle: the intent is on disk; after the restart it is no
    /// settle's, and the resolver recovers it.
    #[test]
    fn a_kill_mid_settle_is_recovered_after_the_restart() {
        let dir = test_dir("intents-kill");
        let path = dir.join("settle-intents.jsonl");
        let i = intent(3, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin(i).expect("begin");
            // The process dies here: no end, no release.
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        assert_eq!(intents.due().len(), 1, "no settle owns it after a restart");
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB).pay(3, REVENUE, VB - 20);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        assert_eq!(receipts.peek(10), vec![i.receipt]);
        drop(intents);
        let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
        assert_eq!(again.open_count(), 0, "the removal is durable");
    }

    /// M7L3-R2-1: an intent's retry key and the resolver's proof are in
    /// the journal. After a restart the identical retry still finds its open
    /// intent (`awaiting`) or, once recovered, its proof (`proven`), within
    /// the proof's window, for its own receipt, once; a pass past the window
    /// removes the proof, durably.
    #[test]
    fn m7l3_r2_1_retry_keys_and_proofs_survive_a_restart() {
        let dir = test_dir("intents-proof");
        let path = dir.join("settle-intents.jsonl");
        let (ka, kb) = (Hash32([0xa1; 32]), Hash32([0xb2; 32]));
        let (a, b) = (intent(21, VB), intent(22, VB));
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin_for(a, Some(ka)).expect("a");
            log.begin_for(b, Some(kb)).expect("b");
            // The process dies here.
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        assert_eq!(intents.awaiting(&ka), Some(a), "the retry key survives");
        assert_eq!(intents.awaiting(&kb), Some(b));
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
        let now = chain.head;
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        assert_eq!(receipts.peek(10), vec![a.receipt]);
        assert_eq!(intents.awaiting(&ka), None);
        let (pa, tx) = intents.proven(&ka, now).expect("a proof");
        assert_eq!(pa, a);
        assert!(tx.starts_with("0x"), "{tx}");
        assert_eq!(intents.proven(&kb, now), None, "b is not proven");
        drop(intents);
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen 2"));
        assert_eq!(
            intents.proven(&ka, now),
            Some((a, tx.clone())),
            "the proof survives"
        );
        assert_eq!(intents.open_count(), 1);
        assert_eq!(
            intents.proven(&ka, now + PROVEN_WINDOW_SECS),
            None,
            "only in its window"
        );
        assert_eq!(
            intents.take_proven(&ka, &b.receipt, now),
            None,
            "only its own receipt"
        );
        assert_eq!(intents.take_proven(&ka, &a.receipt, now), Some(tx));
        assert_eq!(
            intents.take_proven(&ka, &a.receipt, now),
            None,
            "served once"
        );
        let late = Chain::at(VB - 10 + PROVEN_WINDOW_SECS);
        rt().block_on(resolver(late, &intents, &receipts).resolve_once());
        assert_eq!(intents.proven_count(), 0, "gone after its window");
        drop(intents);
        let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 3");
        assert_eq!(again.proven_count(), 0, "durably");
        assert_eq!(again.open_count(), 1);
        assert_eq!(again.awaiting(&kb), Some(b));
    }

    /// M7L3-R3-2 (review PoC b): the proof serves once across a restart.
    /// Its served flag was memory only, so a restart inside the window
    /// served the same proof again. Serving now deletes it from the journal.
    #[test]
    fn m7l3_r3_2_a_served_proof_is_not_served_again_after_a_restart() {
        let dir = test_dir("intents-r3-2");
        let path = dir.join("settle-intents.jsonl");
        let ka = Hash32([0xa1; 32]);
        let a = intent(21, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin_for(a, Some(ka)).expect("a");
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
        let now = chain.head;
        rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert!(
            intents.take_proven(&ka, &a.receipt, now).is_some(),
            "first serve"
        );
        assert_eq!(intents.take_proven(&ka, &a.receipt, now), None);
        assert_eq!(intents.proven_count(), 0);
        drop(intents);
        let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
        assert_eq!(again.proven_count(), 0, "the deletion is durable");
        assert_eq!(again.take_proven(&ka, &a.receipt, now + 1), None);
        assert_eq!(again.awaiting(&ka), None);
    }

    /// M7L3-R3-2: a proof whose deletion cannot be made durable is not
    /// served (fail closed); it stays on disk and the next start serves it
    /// once.
    #[test]
    fn m7l3_r3_2_a_proof_whose_deletion_fails_is_not_served() {
        let dir = test_dir("intents-r3-2-fail");
        let path = dir.join("settle-intents.jsonl");
        let ka = Hash32([0xa1; 32]);
        let a = intent(21, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin_for(a, Some(ka)).expect("a");
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
        let now = chain.head;
        rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        intents.break_journal_for_test();
        assert_eq!(intents.take_proven(&ka, &a.receipt, now), None);
        assert_eq!(intents.write_errors(), 1);
        drop(intents);
        let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
        assert!(again.take_proven(&ka, &a.receipt, now).is_some());
        assert_eq!(again.take_proven(&ka, &a.receipt, now), None);
    }

    /// M7L3-R3-3 (review PoC c): an intent a retry adopted while a resolver
    /// pass judged it (`live`) got a proof as well: the retry served from
    /// its own check and the proof served once more later. No proof is
    /// made for it now; the intent still goes (its receipt is written).
    #[test]
    fn m7l3_r3_3_no_proof_for_an_intent_a_retry_adopted() {
        let dir = test_dir("intents-r3-3");
        let path = dir.join("settle-intents.jsonl");
        let ka = Hash32([0xa1; 32]);
        let a = intent(21, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin_for(a, Some(ka)).expect("a");
        }
        let log = IntentLog::open(&path, &TEST_BINDING).expect("reopen");
        assert_eq!(log.due().len(), 1, "the pass snapshot");
        let id = log.adopt(&ka, &a.receipt).expect("retry adopts mid-pass");
        log.resolved(&[(id, "0xabc".into())], &[], &[], 100);
        assert_eq!(log.proven_count(), 0, "no proof for an adopted intent");
        assert_eq!(log.proven(&ka, 100), None);
        assert_eq!(log.open_count(), 0);
        drop(log);
        let again = IntentLog::open(&path, &TEST_BINDING).expect("reopen 2");
        assert_eq!(again.proven_count(), 0);
    }

    /// M7L3-R3-4: a request served though its receipt could not be written
    /// keeps its intent for the resolver, but without its request key, in
    /// memory and on disk: after a restart its identical retry neither takes
    /// the intent over nor gets a proof.
    #[test]
    fn m7l3_r3_4_a_served_request_without_receipt_loses_its_key() {
        let dir = test_dir("intents-r3-4");
        let path = dir.join("settle-intents.jsonl");
        let ka = Hash32([0xa1; 32]);
        let a = intent(21, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            let id = log.begin_for(a, Some(ka)).expect("a");
            let id = log.forget_request(id);
            log.release(id);
            assert_eq!(log.awaiting(&ka), None);
            assert_eq!(log.due().len(), 1, "left to the resolver");
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        assert_eq!(intents.open_count(), 1);
        assert_eq!(intents.awaiting(&ka), None, "no key after a restart");
        assert_eq!(intents.adopt(&ka, &a.receipt), None);
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
        let now = chain.head;
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        assert_eq!(receipts.peek(10), vec![a.receipt], "still attributed");
        assert_eq!(intents.proven(&ka, now), None, "no proof");
        assert_eq!(intents.proven_count(), 0);
    }

    /// An authorization never used: kept while the chain's own clock is at
    /// or before validBefore + grace, dropped once a block past it says
    /// "unused"; no receipt.
    #[test]
    fn an_unused_authorization_is_dropped_only_by_chain_time() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(4, VB));
        let r = rt().block_on(resolver(Chain::at(VB + GRACE), &intents, &receipts).resolve_once());
        assert_eq!(r.waiting, 1, "the chain has not passed the expiry: kept");
        assert_eq!(intents.open_count(), 1);
        let r =
            rt().block_on(resolver(Chain::at(VB + GRACE + 5), &intents, &receipts).resolve_once());
        assert_eq!(r.expired, 1);
        assert_eq!(intents.open_count(), 0);
        assert_eq!(receipts.pending(), 0);
        assert_eq!(intents.expired(), 1);
    }

    /// OPS-L3-F3: a lagging backend behind a load balancer. Its head is
    /// reported past the expiry, but it cannot serve that block: an error,
    /// never "unused"; the intent is kept. A node whose own head is before
    /// the expiry keeps it too.
    #[test]
    fn a_lagging_node_never_drops_an_intent() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(5, VB));
        let mut lagging = Chain::at(VB + GRACE + 1_000);
        lagging.served = VB - 100;
        let r = rt().block_on(resolver(lagging, &intents, &receipts).resolve_once());
        assert_eq!((r.expired, r.errors), (0, 1));
        let behind = Chain::at(VB - 100);
        let r = rt().block_on(resolver(behind, &intents, &receipts).resolve_once());
        assert_eq!((r.expired, r.waiting), (0, 1));
        assert_eq!(intents.open_count(), 1);
        // The payment did land; a node that has the blocks recovers it.
        let good = Chain::at(VB + GRACE + 1_000).pay(5, REVENUE, VB - 50);
        let r = rt().block_on(resolver(good, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
    }

    /// The nonce paid someone else: never a receipt; dropped as soon as its
    /// use is found (a nonce is used once), whatever the clock says.
    #[test]
    fn a_nonce_spent_paying_someone_else_is_never_credited() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(6, VB));
        let chain = Chain::at(VB - 10).pay(6, ELSEWHERE, VB - 40);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.expired, 1);
        assert_eq!(receipts.pending(), 0);
    }

    /// OPS-L3-F2: one helper transaction uses two authorizations and holds
    /// ONE Transfer to OnyxRevenue. Only the authorization whose own
    /// Transfer (the next log) went to us is recovered; the other is never
    /// credited, so 2,500 received is attributed once.
    #[test]
    fn one_transfer_proves_one_authorization() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(7, VB));
        released(&intents, intent(8, VB));
        let helper = Tx {
            block: VB - 30,
            from: PAYER,
            uses: vec![([7; 32], Some(ELSEWHERE)), ([8; 32], Some(REVENUE))],
        };
        let r = rt()
            .block_on(resolver(Chain::at(VB - 10).tx(helper), &intents, &receipts).resolve_once());
        assert_eq!((r.recovered, r.expired), (1, 1));
        assert_eq!(receipts.peek(10), vec![intent(8, VB).receipt]);
        // The reviewer's shape: both AuthorizationUsed first, one Transfer at
        // the end. Nonce 9's next log is not its Transfer: not proven (an
        // error, kept); nonce 10 is.
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(9, VB));
        released(&intents, intent(10, VB));
        let helper = Tx {
            block: VB - 30,
            from: PAYER,
            uses: vec![([9; 32], None), ([10; 32], Some(REVENUE))],
        };
        let r = rt()
            .block_on(resolver(Chain::at(VB - 10).tx(helper), &intents, &receipts).resolve_once());
        // Nonce 9's one use is not a payment shape: dropped, never credited
        // and never kept (OPS-L4).
        assert_eq!((r.recovered, r.expired), (1, 1));
        assert_eq!(receipts.peek(10), vec![intent(10, VB).receipt]);
        assert_eq!(intents.open_count(), 0);
    }

    /// A node error is never a "no": the intent is kept, even long after it
    /// expired.
    #[test]
    fn a_node_error_keeps_the_intent() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(11, VB));
        let mut chain = Chain::at(VB * 2);
        chain.fail = true;
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.errors, 1);
        assert_eq!(intents.open_count(), 1);
    }

    /// OPS-L3-F1: the receipt journal fails; the resolver finds the payment
    /// on chain but cannot make its receipt durable, so the intent stays on
    /// disk. After a restart (a healthy receipt journal) it is recovered.
    #[test]
    fn a_receipt_write_error_keeps_the_intent_until_a_restart_recovers_it() {
        let dir = test_dir("intents-receipt-error");
        let rpath = dir.join("receipts.jsonl");
        let ipath = dir.join("intents.jsonl");
        let i = intent(12, VB);
        let chain = || Chain::at(VB - 10).pay(12, REVENUE, VB - 30);
        {
            let receipts = Arc::new(ReceiptLog::open(&rpath, &TEST_BINDING).expect("receipts"));
            let intents = Arc::new(IntentLog::open(&ipath, &TEST_BINDING).expect("intents"));
            released(&intents, i);
            receipts.break_journal_for_test();
            // A paid request's own receipt fails first: not durable.
            assert!(!receipts.record(intent(13, VB).receipt));
            assert_eq!(receipts.write_errors(), 1);
            let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
            assert_eq!(
                r.recovered, 0,
                "nothing removed while receipts cannot write"
            );
            assert_eq!(intents.open_count(), 1);
        }
        // Restart: both journals reopen; the intent is still there.
        let receipts = Arc::new(ReceiptLog::open(&rpath, &TEST_BINDING).expect("reopen"));
        let intents = Arc::new(IntentLog::open(&ipath, &TEST_BINDING).expect("reopen"));
        assert_eq!(intents.open_count(), 1);
        let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        drop((receipts, intents));
        let receipts = ReceiptLog::open(&rpath, &TEST_BINDING).expect("reopen 2");
        assert_eq!(receipts.peek(10), vec![i.receipt], "the receipt is durable");
    }

    /// Mutation L4: the receipt journal breaks during a pass: the paid
    /// intent is an error and stays; once the journal has failed, a pass
    /// judges nothing and counts every due intent as an error.
    #[test]
    fn a_receipt_journal_failing_mid_pass_counts_errors() {
        let dir = test_dir("intents-receipt-midpass");
        let receipts = Arc::new(
            ReceiptLog::open(dir.join("receipts.jsonl"), &TEST_BINDING).expect("receipts"),
        );
        let intents = Arc::new(IntentLog::new());
        released(&intents, intent(12, VB));
        released(&intents, intent(14, VB));
        receipts.break_journal_for_test();
        let chain = || Chain::at(VB - 10).pay(12, REVENUE, VB - 30);
        let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
        assert_eq!((r.recovered, r.errors, r.waiting), (0, 1, 1));
        assert_eq!(intents.open_count(), 2);
        assert_eq!(receipts.write_errors(), 1);
        let r = rt().block_on(resolver(chain(), &intents, &receipts).resolve_once());
        assert_eq!((r.recovered, r.errors, r.waiting), (0, 2, 0));
        assert_eq!(intents.open_count(), 2);
    }

    /// OPS-L3-F4: a payer cannot hold more than MAX_OPEN_PER_PAYER intents;
    /// another payer is unaffected.
    #[test]
    fn a_payer_has_a_cap_on_open_intents() {
        let intents = IntentLog::new();
        for n in 0..MAX_OPEN_PER_PAYER as u8 {
            released(&intents, intent(20 + n, VB));
        }
        assert_eq!(intents.begin(intent(40, VB)), Err(BeginError::PayerBusy));
        assert!(intents.begin(intent_from([0x22; 20], 41, VB)).is_ok());
    }

    /// OPS-L3-F4: a pass reads at most its budget; intents it did not reach
    /// are judged on the next pass, earliest `validBefore` first.
    #[test]
    fn a_pass_has_a_budget_and_the_rest_wait_their_turn() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        // Each use sits at the end of its span: several log windows each,
        // more than one pass's budget in all.
        let n: u8 = 180;
        let mut chain = Chain::at(VB + GRACE + 1_000);
        for k in 0..n {
            let payer = [k.wrapping_add(1); 20];
            released(&intents, intent_from(payer, k, VB));
            chain = chain.tx(Tx {
                block: VB - 1,
                from: payer,
                uses: vec![([k; 32], None)],
            });
        }
        let res = resolver(chain, &intents, &receipts);
        let r = rt().block_on(res.resolve_once());
        assert!(*res.rpc.log_calls.lock().expect("lock") <= MAX_LOG_CALLS_PER_PASS);
        assert!(
            r.expired < usize::from(n),
            "the budget stopped the pass: {r:?}"
        );
        let r2 = rt().block_on(res.resolve_once());
        assert_eq!(
            r.expired + r2.expired,
            usize::from(n),
            "the rest end next pass"
        );
        assert_eq!(intents.open_count(), 0);
    }

    /// OPS-L4-F1: the payer cancels the authorization (FiatToken
    /// `cancelAuthorization`: marked used, no Transfer). The intent ends
    /// as cancelled; no receipt.
    #[test]
    fn a_cancelled_authorization_ends_its_intent() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(60, VB));
        // Cancelled after validBefore, too (the reviewer's attack).
        let chain = Chain::at(VB + GRACE + 3_600).cancel(60, VB + 100);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.cancelled, 1, "{r:?}");
        assert_eq!(intents.open_count(), 0);
        assert_eq!(intents.cancelled(), 1);
        assert_eq!(receipts.pending(), 0);
        // Cancelled before expiry: found by the early read already.
        released(&intents, intent(61, VB));
        let chain = Chain::at(VB - 10).cancel(61, VB - 50);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.cancelled, 1);
    }

    /// OPS-L4-F1, the class: an authorization the token marks used whose
    /// use no log shows (outside the span, or never returned) ends after its
    /// full reads, never later than a bounded number of passes past expiry.
    #[test]
    fn a_used_authorization_with_no_payment_of_ours_ends() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        released(&intents, intent(62, VB));
        let mut chain = Chain::at(VB + GRACE + 1_000);
        chain.silent.push([62; 32]);
        let res = resolver(chain, &intents, &receipts);
        let mut passes = 0;
        while intents.open_count() > 0 {
            passes += 1;
            assert!(
                passes <= 2 * usize::from(crate::arrival::FINAL_SCANS),
                "never ended"
            );
            rt().block_on(res.resolve_once());
        }
        assert_eq!(intents.expired(), 1);
        assert_eq!(receipts.pending(), 0);
    }

    /// OPS-L4-F2: one backend clamps `eth_getLogs` below the payment's
    /// block; one full read finds nothing, but a single read never drops a
    /// used authorization: the next read, on a healthy node, recovers it.
    #[test]
    fn a_clamping_backend_cannot_drop_a_payment_alone() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        let i = intent(63, VB);
        released(&intents, i);
        let mut clamped = Chain::at(VB + GRACE + 1_000).pay(63, REVENUE, VB - 30);
        clamped.logs_clamp = Some(VB - 200);
        let r = rt().block_on(resolver(clamped, &intents, &receipts).resolve_once());
        assert_eq!((r.expired, r.waiting), (0, 1));
        let good = Chain::at(VB + GRACE + 1_000).pay(63, REVENUE, VB - 30);
        let r = rt().block_on(resolver(good, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
        assert_eq!(receipts.peek(10), vec![i.receipt]);
    }

    /// OPS-L4-F3: the span is chain time from `validAfter`/`validBefore`,
    /// not the local clock: an intent written by a clock running an hour
    /// fast still finds a payment mined right after verify.
    #[test]
    fn a_fast_local_clock_does_not_hide_the_payment() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        let mut i = intent(64, VB);
        i.created = VB + 3_600;
        released(&intents, i);
        let chain = Chain::at(VB + GRACE + 1_000).pay(64, REVENUE, VB - LIFE + 10);
        let r = rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(r.recovered, 1);
    }

    /// OPS-L4-F3: while the server's clock is more than MAX_CLOCK_SKEW_SECS
    /// off the chain's, no new settle starts (503); it starts again once
    /// they agree.
    #[test]
    fn a_skewed_clock_refuses_new_settles() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        let fast = IntentResolver::new(
            Chain::at(VB),
            TOKEN,
            REVENUE,
            intents.clone(),
            receipts.clone(),
        )
        .with_clock(Arc::new(|| VB + MAX_CLOCK_SKEW_SECS + 1));
        rt().block_on(fast.resolve_once());
        assert_eq!(
            intents.skew(),
            Some(i64::try_from(MAX_CLOCK_SKEW_SECS).expect("fits") + 1)
        );
        assert_eq!(intents.begin(intent(65, VB)), Err(BeginError::ClockSkew));
        rt().block_on(resolver(Chain::at(VB), &intents, &receipts).resolve_once());
        assert!(intents.begin(intent(65, VB)).is_ok());
        // The bound itself is allowed, either way (mutation L4).
        let max = i64::try_from(MAX_CLOCK_SKEW_SECS).expect("fits");
        intents.set_skew(-max);
        assert!(intents.begin(intent(66, VB)).is_ok());
        intents.set_skew(max);
        assert!(intents.begin(intent(67, VB)).is_ok());
        intents.set_skew(-max - 1);
        assert_eq!(intents.begin(intent(68, VB)), Err(BeginError::ClockSkew));
    }

    /// Mutation L4: a proof is served only before its window ends, and the
    /// pass at the end of its window (not one second before) drops it.
    #[test]
    fn a_proof_lives_exactly_its_window() {
        let dir = test_dir("intents-proof-window");
        let path = dir.join("settle-intents.jsonl");
        let ka = Hash32([0xa1; 32]);
        let a = intent(21, VB);
        {
            let log = IntentLog::open(&path, &TEST_BINDING).expect("open");
            log.begin_for(a, Some(ka)).expect("a");
            // The process dies here: the resolver of the next start recovers it.
        }
        let intents = Arc::new(IntentLog::open(&path, &TEST_BINDING).expect("reopen"));
        let receipts = Arc::new(ReceiptLog::new());
        let chain = Chain::at(VB - 10).pay(21, REVENUE, VB - 30);
        let now = chain.head;
        rt().block_on(resolver(chain, &intents, &receipts).resolve_once());
        assert_eq!(intents.proven_count(), 1);
        let until = now + PROVEN_WINDOW_SECS;
        assert_eq!(intents.take_proven(&ka, &a.receipt, until), None);
        rt().block_on(resolver(Chain::at(until - 1), &intents, &receipts).resolve_once());
        assert_eq!(intents.proven_count(), 1, "still in its window");
        rt().block_on(resolver(Chain::at(until), &intents, &receipts).resolve_once());
        assert_eq!(intents.proven_count(), 0, "its window ended");
    }

    /// OPS-L4-F4: one authorization, two requests: only one intent.
    #[test]
    fn one_authorization_has_one_intent() {
        let intents = IntentLog::new();
        intents.begin(intent(66, VB)).expect("first");
        let mut other_shard = intent(66, VB);
        other_shard.receipt.shard = Hash32([0xab; 32]);
        assert_eq!(intents.begin(other_shard), Err(BeginError::InUse));
    }

    /// OPS-L4-F5: settles in flight do not count toward the payer's cap;
    /// unsettled (released) ones do.
    #[test]
    fn only_unsettled_intents_count_toward_the_payer_cap() {
        let intents = IntentLog::new();
        for n in 0..=MAX_OPEN_PER_PAYER as u8 {
            intents.begin(intent(70 + n, VB)).expect("in flight");
        }
        assert_eq!(intents.open_count(), MAX_OPEN_PER_PAYER + 1);
    }

    /// The class invariant, with every payer action the token allows: each
    /// released intent ends within a bounded number of passes once the
    /// chain is past its expiry, and only payments to us become receipts.
    #[test]
    fn every_intent_ends_whatever_the_payer_does() {
        let intents = Arc::new(IntentLog::new());
        let receipts = Arc::new(ReceiptLog::new());
        let mut chain = Chain::at(VB + GRACE + 2_000);
        for k in 0..20u8 {
            let payer = [k + 1; 20];
            released(&intents, intent_from(payer, 100 + k, VB));
            let nonce = 100 + k;
            chain = match k % 5 {
                0 => chain,                                            // never used
                1 => chain.cancel_from(payer, nonce, VB + 50),         // cancelled after expiry
                2 => chain.pay_from(payer, nonce, ELSEWHERE, VB - 20), // spent elsewhere
                3 => chain.pay_from(payer, nonce, REVENUE, VB - 20),   // paid us
                _ => {
                    chain.silent.push([nonce; 32]); // used, no log
                    chain
                }
            };
        }
        let res = resolver(chain, &intents, &receipts);
        let mut passes = 0;
        while intents.open_count() > 0 {
            passes += 1;
            assert!(
                passes <= 2 * usize::from(crate::arrival::FINAL_SCANS),
                "stuck: {}",
                intents.open_count()
            );
            rt().block_on(res.resolve_once());
        }
        assert_eq!(receipts.pending(), 4, "only the four payments to us");
    }

    /// A guard dropped without `ended` (an error or a panic in the settle
    /// task) hands the intent to the resolver; `ended` removes it.
    #[test]
    fn the_guard_releases_or_ends() {
        let intents = Arc::new(IntentLog::new());
        let id = intents.begin(intent(50, VB)).expect("begin");
        drop(IntentGuard::new(intents.clone(), id));
        assert_eq!(intents.due().len(), 1);
        let id2 = intents.begin(intent(51, VB)).expect("begin");
        rt().block_on(IntentGuard::new(intents.clone(), id2).ended());
        assert_eq!(intents.open_count(), 1);
    }

    /// A write that cannot be made durable refuses the settle (fail closed)
    /// and is counted.
    #[test]
    fn a_failed_write_refuses_the_settle() {
        let dir = test_dir("intents-broken");
        let log = IntentLog::open(dir.join("i.jsonl"), &TEST_BINDING).expect("open");
        log.break_journal_for_test();
        assert_eq!(log.begin(intent(52, VB)), Err(BeginError::NotDurable));
        assert_eq!(log.write_errors(), 1);
        assert_eq!(
            log.begin(intent(53, VB)),
            Err(BeginError::NotDurable),
            "stays closed"
        );
    }

    /// The span runs from the bound the server accepted (life + skew before
    /// `validBefore`, never before `validAfter`) to `validBefore`.
    #[test]
    fn the_span_is_the_authorizations_bounded_life() {
        let r = intent(54, 100_000).receipt;
        let i = Intent::new(r, 0, 100_000, 99_950, LIFE);
        assert_eq!(i.earliest, 100_000 - LIFE - MAX_CLOCK_SKEW_SECS);
        let late_after = Intent::new(r, 99_900, 100_000, 99_950, LIFE);
        assert_eq!(late_after.earliest, 99_900);
    }
}
