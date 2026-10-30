//! Observe on the first paid query (NORTH-STAR NS10, north-star loop NL8, ADR-0049,
//! docs/spec/paid-observation.md).
//!
//! A level-2 fact no round observed yet costs the network nothing until someone pays for it. A
//! request for such a fact (a subject the metric's pinned, dated template allows a round for now)
//! answers 402 `observe_payment_required` with the metric's observe offer, the same for every
//! such subject (R-OBS-11). Only a prepaid voucher takes it, because the value comes later than
//! the request: the voucher is HELD ([`crate::prepaid_gate::PrepaidState::hold`], journalled,
//! never counted and never redeemed), the request answers 202 with a ticket, and the fact joins
//! the queue. [`step`] runs queued rounds one at a time through the operator's [`RoundRunner`];
//! a round whose transcript checks against the pinned template is written to the metric's
//! results store, served, and only then is the hold charged (R-OBS-2). A failed round, a
//! transcript for another subject or one that does not check, or a passed deadline releases the
//! hold in full: the buyer's next voucher starts from the earlier total (R-OBS-6).
//!
//! One round per fact (R-OBS-3): while a fact is queued or running, every other request for it
//! answers 202 without a ticket and pays nothing. One hold per buyer (R-OBS-7, R-OBS-9): a buyer
//! with a hold pending can neither hold again nor pay another answer with a voucher until the
//! hold is charged or released, so one deposit never backs two holds and one balance cannot fill
//! the queue. A ticket is 32 random bytes the server shows once; the journal keeps only its hash
//! (R-OBS-5).

use crate::observations::{Transcript, OBSERVATION_ROUND};
use crate::prepaid_gate::{self, ChargeError, Held, HoldEnd};
use crate::x402::BoxFuture;
use crate::{ApiError, ApiState, Reply};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use onyx_core::{keccak256, FeedTemplate, Hash32, Subject};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What a buyer reads before paying for a round (R-OBS-10): the value is not private.
pub const BECOMES_PUBLIC: &str = "The round records its value on chain, so after the round \
anyone can read it; you pay for the round being run, not for secrecy.";

/// The refund rule (R-OBS-6), stated in every offer.
pub const REFUND_RULE: &str = "The held amount is charged only when the round is final and its \
value checks against the registered template; if the round fails, its transcript does not \
check, or the deadline passes, the hold is released in full and nothing is charged.";

/// Seconds a client waits between ticket polls.
pub const RETRY_AFTER: u64 = 30;

/// One metric's observe offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserveOffer {
    /// The round's price (the token's smallest unit): the round plus the first answer.
    pub price: u64,
    /// Most seconds from the hold to a final round; past it the hold is released. At most
    /// half the prepaid contract's `exitDelay` (R-OBS-8, checked by [`ObserveState::new`]).
    pub deadline_secs: u64,
    /// The metric's results store file: a final round is appended here before it is served,
    /// so it survives a restart.
    pub store: PathBuf,
}

/// Runs one level-2 round of `template` for `subject` (the operator's coordinator and observers,
/// `onyx_level2::live` on a chain). Its transcript is checked here, never trusted.
pub trait RoundRunner: Send + Sync {
    /// Run the round now and return its transcript. The round must end by `deadline` (unix
    /// seconds): the hold is released then, and the worker starts the next round, so a runner
    /// stops its work by that time (two rounds of one operator never run at once).
    fn run<'a>(
        &'a self,
        template: &'a FeedTemplate,
        subject: &'a Subject,
        now: u64,
        deadline: u64,
    ) -> BoxFuture<'a, Result<Transcript, String>>;
}

/// Where a ticket stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketState {
    /// Paid for (held), waiting for its round.
    Queued,
    /// Its round is running.
    Running,
    /// Final and served; the hold was charged.
    Observed,
    /// Released in full: the round failed, did not check, or missed the deadline.
    Released,
}

impl TicketState {
    /// The state's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            TicketState::Queued => "queued",
            TicketState::Running => "running",
            TicketState::Observed => "observed",
            TicketState::Released => "released",
        }
    }

    fn active(self) -> bool {
        matches!(self, TicketState::Queued | TicketState::Running)
    }
}

/// One ticket, by the hash of its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    /// The metric.
    pub metric: String,
    /// The subject's text.
    pub subject: String,
    /// Where it stands.
    pub state: TicketState,
    /// When the hold was taken.
    pub created: u64,
    /// When the hold is released if the round is not final.
    pub deadline: u64,
}

#[derive(Default)]
struct Inner {
    tickets: BTreeMap<Hash32, Ticket>,
    /// (metric, subject) -> the fact's active ticket.
    active: BTreeMap<(String, String), Hash32>,
    queue: VecDeque<Hash32>,
    observed: u64,
    released: u64,
    /// Rounds served whose charge could not be journalled (served free, never twice).
    unbilled: u64,
}

/// The observe offers, the runner and the tickets.
pub struct ObserveState {
    offers: BTreeMap<String, ObserveOffer>,
    runner: Arc<dyn RoundRunner>,
    inner: Mutex<Inner>,
}

/// The hash a ticket secret is journalled and looked up by.
pub fn ticket_hash(secret: &[u8; 32]) -> Hash32 {
    keccak256(&[b"onyx-observe-ticket-v1".as_slice(), secret].concat())
}

fn new_secret() -> Result<[u8; 32], String> {
    use std::io::Read;
    let mut s = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut s))
        .map_err(|e| format!("ticket entropy: {e}"))?;
    Ok(s)
}

impl ObserveState {
    /// Offers by metric, run by `runner`. Each offer's deadline must be positive and at most the
    /// prepaid hold limit (half of `exitDelay`, R-OBS-8); the pending holds the prepaid journal
    /// holds come back as queued tickets (a restart resumes them).
    pub fn new(
        offers: BTreeMap<String, ObserveOffer>,
        runner: Arc<dyn RoundRunner>,
        prepaid: &prepaid_gate::PrepaidState,
    ) -> Result<ObserveState, String> {
        for (m, o) in &offers {
            if o.price == 0 {
                return Err(format!("observe offer {m}: a round is never free"));
            }
            if o.deadline_secs == 0 || o.deadline_secs > prepaid.hold_limit {
                return Err(format!(
                    "observe offer {m}: deadline {} s must be 1..={} s (half the prepaid exitDelay)",
                    o.deadline_secs, prepaid.hold_limit
                ));
            }
        }
        let mut inner = Inner::default();
        // Holds that ended answer their tickets after a restart too.
        // Relay holds (ADR-0056, `crate::credits`) share the journal but are no rounds.
        let round = |h: &Held| h.metric != prepaid_gate::RELAY_HOLD_METRIC;
        for (h, end) in prepaid.ended_holds() {
            if !round(&h) {
                continue;
            }
            let state = match end {
                HoldEnd::Charged => TicketState::Observed,
                HoldEnd::Released => TicketState::Released,
            };
            match end {
                HoldEnd::Charged => inner.observed += 1,
                HoldEnd::Released => inner.released += 1,
            }
            inner.tickets.insert(
                h.ticket,
                Ticket {
                    metric: h.metric,
                    subject: h.subject,
                    state,
                    created: h.created,
                    deadline: h.deadline,
                },
            );
        }
        for h in prepaid.holds().into_iter().filter(round) {
            inner
                .active
                .insert((h.metric.clone(), h.subject.clone()), h.ticket);
            inner.queue.push_back(h.ticket);
            inner.tickets.insert(
                h.ticket,
                Ticket {
                    metric: h.metric,
                    subject: h.subject,
                    state: TicketState::Queued,
                    created: h.created,
                    deadline: h.deadline,
                },
            );
        }
        Ok(ObserveState {
            offers,
            runner,
            inner: Mutex::new(inner),
        })
    }

    /// How many metrics have an offer.
    pub fn metrics(&self) -> usize {
        self.offers.len()
    }

    /// The offer of `metric`.
    pub fn offer(&self, metric: &str) -> Option<&ObserveOffer> {
        self.offers.get(metric)
    }

    /// The ticket whose secret hashes to `hash`.
    pub fn ticket(&self, hash: &Hash32) -> Option<Ticket> {
        self.inner.lock().ok()?.tickets.get(hash).cloned()
    }

    /// `/v1/health`'s `observe` object.
    pub fn health(&self, now: u64) -> Value {
        let Ok(i) = self.inner.lock() else {
            return json!({ "error": "poisoned" });
        };
        let count = |s: TicketState| i.tickets.values().filter(|t| t.state == s).count();
        let oldest = i
            .tickets
            .values()
            .filter(|t| t.state.active())
            .map(|t| now.saturating_sub(t.created))
            .max();
        json!({
            "queued": count(TicketState::Queued),
            "running": count(TicketState::Running),
            "observed": i.observed,
            "released": i.released,
            "unbilled": i.unbilled,
            "oldestQueuedSecs": oldest,
        })
    }

    fn set(&self, hash: &Hash32, state: TicketState) {
        if let Ok(mut i) = self.inner.lock() {
            let fact = i.tickets.get_mut(hash).map(|t| {
                t.state = state;
                (t.metric.clone(), t.subject.clone())
            });
            if !state.active() {
                if let Some(f) = fact {
                    if i.active.get(&f) == Some(hash) {
                        i.active.remove(&f);
                    }
                }
                i.queue.retain(|q| q != hash);
                match state {
                    TicketState::Observed => i.observed += 1,
                    TicketState::Released => i.released += 1,
                    _ => {}
                }
            }
        }
    }
}

/// The observe part of `/v1/observations/{metric}?subject=...` for a subject no round settled
/// yet. `None` when the metric has no observe offer, observing is off, or the template does not
/// allow a round for the subject now: the caller answers its usual 404.
pub(crate) async fn reply(
    state: &Arc<ApiState>,
    metric: &str,
    subject_text: &str,
    headers: &HeaderMap,
) -> Option<Reply> {
    let obs = state.observe.as_ref()?;
    let pp = state.prepaid.clone()?;
    let offer = obs.offer(metric)?.clone();
    state.payments.as_ref()?;
    let now = (state.clock)();
    let template = {
        let c = state.catalog.read().ok()?;
        c.observations(metric)?.template.clone()
    };
    let subject = Subject::Aggregate(subject_text.to_owned());
    // R-OBS-4: only a subject the pinned template allows a round for now; the day is fixed
    // before any payment.
    if !template.is_dated() || template.instantiate(&subject, now).is_err() {
        return None;
    }
    let fact = (metric.to_owned(), subject_text.to_owned());
    // R-OBS-3: a fact already paid for is not paid again.
    if let Some(t) = obs
        .inner
        .lock()
        .ok()
        .and_then(|i| i.active.get(&fact).and_then(|h| i.tickets.get(h)).cloned())
    {
        return Some(Ok(accepted(json!({
            "state": t.state.as_str(),
            "retryAfter": RETRY_AFTER,
        }))));
    }
    let offer_json = json!({
        "price": offer.price.to_string(),
        "deadlineSecs": offer.deadline_secs,
        "pays": prepaid_gate::PREPAID_VOUCHER,
        "becomesPublic": BECOMES_PUBLIC,
        "refund": REFUND_RULE,
    });
    let Some(h) = headers.get(prepaid_gate::PREPAID_VOUCHER) else {
        return Some(Err(ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "observe_payment_required",
            "no round settled this subject yet; a prepaid voucher pays for one",
        )
        .with_field("observe", offer_json)
        .into()));
    };
    let Some(voucher) = h.to_str().ok().and_then(prepaid_gate::parse_voucher) else {
        return Some(Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_prepaid",
            "malformed ONYX-PREPAID-VOUCHER header",
        )
        .into()));
    };
    if pp.refresh(voucher.buyer, now).await.is_err() {
        return Some(Err(crate::prepaid_unavailable().into()));
    }
    let secret = match new_secret() {
        Ok(s) => s,
        Err(e) => return Some(Err(crate::internal(e).into())),
    };
    let hash = ticket_hash(&secret);
    let deadline = now.saturating_add(offer.deadline_secs);
    // Reserve the fact before the hold, so two buyers racing on it start one round.
    {
        let Ok(mut i) = obs.inner.lock() else {
            return Some(Err(crate::internal("observe lock").into()));
        };
        if let Some(t) = i.active.get(&fact).and_then(|h| i.tickets.get(h)) {
            return Some(Ok(accepted(json!({
                "state": t.state.as_str(),
                "retryAfter": RETRY_AFTER,
            }))));
        }
        i.active.insert(fact.clone(), hash);
        i.tickets.insert(
            hash,
            Ticket {
                metric: fact.0.clone(),
                subject: fact.1.clone(),
                state: TicketState::Queued,
                created: now,
                deadline,
            },
        );
    }
    let held = Held {
        ticket: hash,
        metric: fact.0.clone(),
        subject: fact.1.clone(),
        deadline,
        created: now,
    };
    let price = u128::from(offer.price);
    let taken = tokio::task::spawn_blocking(move || pp.hold(voucher, price, held, now)).await;
    let refused = match taken {
        Ok(Ok(())) => None,
        Ok(Err(ChargeError::Refused(e))) => Some(
            ApiError::new(
                StatusCode::PAYMENT_REQUIRED,
                "prepaid_refused",
                e.to_string(),
            )
            .with_field("observe", offer_json),
        ),
        Ok(Err(ChargeError::Unavailable)) | Err(_) => Some(crate::prepaid_unavailable()),
    };
    if let Some(e) = refused {
        if let Ok(mut i) = obs.inner.lock() {
            i.tickets.remove(&hash);
            if i.active.get(&fact) == Some(&hash) {
                i.active.remove(&fact);
            }
        }
        return Some(Err(e.into()));
    }
    if let Ok(mut i) = obs.inner.lock() {
        i.queue.push_back(hash);
    }
    Some(Ok(accepted(json!({
        "ticket": hex::encode(secret),
        "state": "queued",
        "deadline": deadline,
        "retryAfter": RETRY_AFTER,
        "status": format!("/v1/tickets/{}", hex::encode(secret)),
    }))))
}

fn accepted(body: Value) -> Response {
    let mut r = (StatusCode::ACCEPTED, Json(body)).into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    if let Ok(v) = HeaderValue::from_str(&RETRY_AFTER.to_string()) {
        r.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
    }
    r
}

/// `GET /v1/tickets/{ticket}`: the ticket's state; once observed, the paid answer itself.
pub(crate) fn ticket_reply(state: &ApiState, secret_hex: &str) -> Result<Response, ApiError> {
    let unknown = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_ticket",
            "no such ticket on this server",
        )
    };
    let obs = state.observe.as_ref().ok_or_else(unknown)?;
    let secret: [u8; 32] = (secret_hex.len() == 64)
        .then(|| hex::decode(secret_hex).ok())
        .flatten()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(unknown)?;
    let t = obs.ticket(&ticket_hash(&secret)).ok_or_else(unknown)?;
    let mut body = json!({
        "state": t.state.as_str(),
        "metric": t.metric,
        "subject": t.subject,
        "deadline": t.deadline,
    });
    match t.state {
        TicketState::Queued | TicketState::Running => {
            body["retryAfter"] = json!(RETRY_AFTER);
        }
        TicketState::Observed => {
            let c = state
                .catalog
                .read()
                .map_err(|_| crate::internal("catalog lock"))?;
            let feed = c.observations(&t.metric).ok_or_else(unknown)?;
            let r = feed.record(&t.subject, None, None).ok_or_else(unknown)?;
            body["observation"] = feed.body(r);
        }
        TicketState::Released => {
            body["refund"] = json!(REFUND_RULE);
        }
    }
    let mut r = Json(body).into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    Ok(r)
}

/// What one [`step`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Tickets released (a passed deadline, a failed or refused round).
    pub released: Vec<Hash32>,
    /// The ticket whose round ran to a served value.
    pub observed: Option<Hash32>,
}

/// One pass of the observe worker: release every hold past its deadline, then run the oldest
/// queued fact's round (at most one per pass; the node calls this in a loop). Nothing runs
/// without a hold behind it (R-OBS-1): the queue holds only held tickets.
pub async fn step(state: &ApiState) -> Step {
    let mut out = Step::default();
    let (Some(obs), Some(pp)) = (state.observe.as_ref(), state.prepaid.as_ref()) else {
        return out;
    };
    let now = (state.clock)();
    let expired: Vec<Hash32> = obs.inner.lock().map_or_else(
        |_| Vec::new(),
        |i| {
            i.tickets
                .iter()
                .filter(|(_, t)| t.state.active() && t.deadline <= now)
                .map(|(h, _)| *h)
                .collect()
        },
    );
    for h in expired {
        if release(obs, pp, &h, now) {
            out.released.push(h);
        }
    }
    let next = obs.inner.lock().ok().and_then(|mut i| {
        let h = i.queue.pop_front()?;
        let t = i.tickets.get_mut(&h)?;
        t.state = TicketState::Running;
        Some((h, t.clone()))
    });
    let Some((h, t)) = next else {
        return out;
    };
    let Some(offer) = obs.offer(&t.metric).cloned() else {
        if release(obs, pp, &h, now) {
            out.released.push(h);
        }
        return out;
    };
    let template = match state.catalog.read() {
        Ok(c) => c.observations(&t.metric).map(|f| f.template.clone()),
        Err(_) => None,
    };
    let Some(template) = template else {
        if release(obs, pp, &h, now) {
            out.released.push(h);
        }
        return out;
    };
    let subject = Subject::Aggregate(t.subject.clone());
    // A restart after the store write and before the charge: the value is served already.
    let served = state.catalog.read().ok().is_some_and(|c| {
        c.observations(&t.metric)
            .and_then(|f| f.record(&t.subject, None, None))
            .is_some()
    });
    if !served {
        let left = t.deadline.saturating_sub(now);
        // A margin past the deadline: the runner stops its own work by then.
        let ran = tokio::time::timeout(
            std::time::Duration::from_secs(left.saturating_add(10)),
            obs.runner.run(&template, &subject, now, t.deadline),
        )
        .await;
        let ran = match ran {
            Ok(r) if (state.clock)() <= t.deadline => Ok(r),
            _ => Err(()),
        };
        let after = (state.clock)();
        let added = match ran {
            Ok(Ok(tr)) if tr.subject == subject => add(state, &template, &offer, tr, after),
            Ok(Ok(_)) => Err("the round observed another subject".to_owned()),
            Ok(Err(e)) => Err(e),
            Err(()) => Err("deadline".to_owned()),
        };
        if added.is_err() {
            if release(obs, pp, &h, after) {
                out.released.push(h);
            }
            return out;
        }
    }
    let bill = state.catalog.read().ok().and_then(|c| {
        c.observations(&t.metric)
            .and_then(|f| f.record(&t.subject, None, None))
            .map(|r| (OBSERVATION_ROUND, r.checked.registry_fact))
    });
    let charged = bill.is_some_and(|b| matches!(pp.commit_hold(&h, b, (state.clock)()), Ok(true)));
    if !charged {
        if let Ok(mut i) = obs.inner.lock() {
            i.unbilled += 1;
        }
    }
    obs.set(&h, TicketState::Observed);
    out.observed = Some(h);
    out
}

fn release(obs: &ObserveState, pp: &prepaid_gate::PrepaidState, h: &Hash32, now: u64) -> bool {
    // A release that cannot be journalled keeps the hold (and the ticket active): the buyer is
    // never charged for it, and the next pass tries again.
    match pp.release_hold(h, now) {
        Ok(_) => {
            obs.set(h, TicketState::Released);
            true
        }
        Err(_) => false,
    }
}

/// Check `t` against the pinned template, append it to the metric's store, then serve it.
fn add(
    state: &ApiState,
    template: &FeedTemplate,
    offer: &ObserveOffer,
    t: Transcript,
    now: u64,
) -> Result<(), String> {
    let mut probe =
        crate::observations::ObservationFeed::new(template.clone()).map_err(|e| e.to_string())?;
    probe.add(t.clone(), now).map_err(|e| e.to_string())?;
    let prev = crate::observations::ObservationStore::read_file(&offer.store, false)?
        .ok_or("no results store")?;
    let next = crate::observations::ObservationStore::append(
        &prev,
        template,
        std::slice::from_ref(&t),
        &[],
    )
    .map_err(|e| e.to_string())?;
    crate::indexer::write_atomic(&offer.store, &next).map_err(|e| e.to_string())?;
    let mut c = state.catalog.write().map_err(|_| "catalog lock")?;
    let feed = c
        .observations_mut(&template.metric.0)
        .ok_or("the metric is not served")?;
    feed.add(t, now).map_err(|e| e.to_string())
}
