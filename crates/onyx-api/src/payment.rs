//! Payments for one request (M7, ADR-0023): the price and the contributor pass, the x402
//! check, the settle in its own task (M7-L39-F1), prepaid spends, and what an unknown settle
//! leaves for the intent resolver (OPS-L2-F3). Moved out of lib.rs; the handlers call it.

use super::*;

/// Whether a contributor proof may lower a request's price.
#[derive(Clone, Copy)]
pub(super) enum Pass {
    /// One row (a proof, a metric, a settlement value).
    Row,
    /// An export: never (REV-L1-F5).
    Never,
}

/// Whether a request is charged, and the verified payment if it is.
pub(super) enum Gate {
    Free,
    /// A contributor's free read of fresh data: private, no payment.
    Contributor,
    /// Paid with a prepaid voucher (ROADMAP S7): taken and journalled in [`finish`], before
    /// the body leaves.
    Prepaid {
        voucher: prepaid::Voucher,
        price: u64,
        /// The request key ([`payment_key`] over the header): its identical retry is a replay.
        key: Hash32,
    },
    Paid {
        req: Box<FacilitatorRequest>,
        claim: cache::ClaimGuard,
        /// EIP-3009 payer and nonce, for the usage receipt.
        payer: [u8; 20],
        nonce: [u8; 32],
        amount: u64,
        /// The authorization's `validAfter`: the token refuses any use before it.
        valid_after: u64,
        /// The authorization's `validBefore`: after it (and a grace) an
        /// unused authorization can never move money (OPS-L2-F1 intents).
        valid_before: u64,
        /// The longest authorization life this server accepted (seconds):
        /// bounds the span the intent's use is searched in (OPS-L4-F1).
        max_life: u64,
        /// The facilitator slot taken for verify, held through the settle
        /// (OPS-L2-F2); none when nothing is sent or read (a proven retry).
        slot: Option<tokio::sync::OwnedSemaphorePermit>,
        /// A new payment, or the identical retry of one.
        attempt: Attempt,
    },
}

/// What a paid request's settle does.
pub(super) enum Attempt {
    /// A new payment: a new intent, then the settle.
    Fresh,
    /// The identical retry of a paid request whose settle ended unknown or
    /// refused, its intent still open (M7L3-R1-1): no new intent and no new
    /// settle; the authorization check decides (it re-sends only an unused
    /// authorization, at most [`intents::MAX_RESENDS`] times in all).
    Retry,
    /// The identical retry of a paid request whose intent the resolver
    /// recovered (M7L3-R2-1): served from that proof (the transaction that
    /// moved the payment into `OnyxRevenue`; the receipt is on disk), with
    /// no settle and no check.
    Proven,
}

/// Settled-payment key: the exact header bytes, for the exact resource, from
/// the same client (M7-L39-F5: a leaked header is not a bearer token).
fn payment_key(sig: &[u8], resource: &str, client: &str) -> Hash32 {
    let mut b = sig.to_vec();
    b.push(0);
    b.extend_from_slice(resource.as_bytes());
    b.push(0);
    b.extend_from_slice(client.as_bytes());
    keccak256(&b)
}

pub(super) fn prepaid_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "prepaid_unavailable",
        "the server cannot check or record prepaid balances right now; nothing was charged",
    )
    .with_header("retry-after", "5".into())
}

fn facilitator_down(message: impl Into<String>) -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "facilitator_unavailable",
        message,
    )
    .with_header("retry-after", "5".into())
    .into_response()
}

pub(super) fn replay(c: cache::CachedResponse) -> Box<Response> {
    Box::new(respond(
        c.body,
        &c.etag,
        true,
        c.immutable,
        Some(&c.payment_response),
    ))
}

/// What a 402 offers besides its price: the resource's description and,
/// for level 2, the known limits of what is sold (ROADMAP B4,
/// [`observations::limits_json`]), in the body and in `extensions`.
pub(super) struct Offer<'a> {
    pub(super) description: &'a str,
    pub(super) limits: Option<Value>,
}

impl<'a> Offer<'a> {
    pub(super) fn of(description: &'a str) -> Self {
        Offer {
            description,
            limits: None,
        }
    }
}

/// Phase one of the x402 v2 `exact` flow (spec §6.1): 402 challenge, replay
/// of an already settled identical request from the same client (M7-F5,
/// M7-L39-F5), waiting for a settlement of it still in flight (M7-L39-F1),
/// rate limit (M7-F4), the payment must be for exactly this resource and
/// these requirements, local EIP-3009 checks, then facilitator `verify` under
/// a deadline and a server-wide cap (M7-L39-F4). No row has been read yet.
pub(super) async fn authorize(
    state: &ApiState,
    headers: &HeaderMap,
    client: &str,
    resource: &str,
    offer: Offer<'_>,
    price: u64,
    pass: Pass,
) -> Result<Gate, Box<Response>> {
    let Offer {
        description,
        limits,
    } = offer;
    if state.payments.is_none() || price == 0 {
        return Ok(Gate::Free);
    }
    // REV-L1-F5: a contributor proof is a bearer token its signer can hand to
    // anyone, so it never buys a whole shard; on an export the header is
    // ignored and the export is paid (free exports keep their own budget).
    let pass_price = match pass {
        Pass::Row => contributor_price(state, headers, client, resource, price).await?,
        Pass::Never => None,
    };
    let price = match pass_price {
        Some(0) => return Ok(Gate::Contributor),
        Some(p) => p,
        None => price,
    };
    let Some(pay) = state.payments.as_ref() else {
        return Ok(Gate::Free);
    };
    // ROADMAP S7: a prepaid voucher instead of an x402 payment. Its account is read here (async);
    // the voucher is taken and journalled in `finish`, once the answer's book is known.
    if let Some(h) = headers.get(prepaid_gate::PREPAID_VOUCHER) {
        let Some(pp) = state.prepaid.as_ref() else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "prepaid_not_offered",
                "this server takes x402 payments only",
            )
            .into());
        };
        let Some(voucher) = h.to_str().ok().and_then(prepaid_gate::parse_voucher) else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_prepaid",
                "malformed ONYX-PREPAID-VOUCHER header",
            )
            .into());
        };
        if pp.refresh(voucher.buyer, (state.clock)()).await.is_err() {
            return Err(prepaid_unavailable().into());
        }
        let key = payment_key(h.as_bytes(), resource, client);
        return Ok(Gate::Prepaid {
            voucher,
            price,
            key,
        });
    }
    let requirements = pay.terms.requirements(price);
    let challenge = |error: &str| -> Box<Response> {
        let pr = PaymentRequired {
            x402_version: x402::X402_VERSION,
            error: Some(error.into()),
            resource: x402::ResourceInfo {
                url: resource.into(),
                description: Some(description.into()),
                mime_type: Some("application/json".into()),
            },
            accepts: vec![requirements.clone()],
            extensions: limits.as_ref().map(
                |l| json!({ (observations::LIMITS_EXTENSION): observations::limits_extension(l) }),
            ),
        };
        let mut e = ApiError::new(StatusCode::PAYMENT_REQUIRED, "payment_required", error)
            .with_header(x402::PAYMENT_REQUIRED, x402::encode_header(&pr));
        if let Some(l) = &limits {
            e = e.with_field("limits", l.clone());
        }
        e.into()
    };
    let Some(sig) = headers.get(x402::PAYMENT_SIGNATURE) else {
        return Err(challenge("PAYMENT-SIGNATURE header is required"));
    };
    let key = payment_key(sig.as_bytes(), resource, client);
    if let Some(c) = state.caches.replay_settled(&key) {
        return Err(replay(c));
    }
    // A failed write of the receipt log or of the facilitator's state means
    // a new payment's receipt might be lost: refuse new payments before
    // anything is verified (fail closed, REV-L3-F5). Replays of payments
    // already settled are still answered above.
    if state_write_errors(state) > 0 || state.receipts.is_full() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "receipt_log_unavailable",
            "the server cannot record payments durably right now; nothing was charged",
        )
        .into());
    }
    // Claim the payment atomically (M7-L40-F4): identical concurrent
    // requests do not both verify and settle; the second waits for the
    // first and replays its response. At most two rounds: a claim whose
    // owner ended without a response (refused, error) is claimed afresh.
    let mut claim = None;
    for _ in 0..2 {
        match state.caches.claim(key) {
            cache::Claimed::Mine(g) => {
                claim = Some(g);
                break;
            }
            cache::Claimed::Wait(mut rx) => {
                let _ = rx.wait_for(|done| *done).await;
                if let Some(c) = state.caches.replay_settled(&key) {
                    return Err(replay(c));
                }
            }
            cache::Claimed::Full => {
                return Err(Box::new(facilitator_down("too many settlements in flight")))
            }
        }
    }
    let Some(mut claim) = claim else {
        return Err(Box::new(facilitator_down("this payment is busy; retry")));
    };
    let per_minute = state.config.payment_attempts_per_minute;
    if !state.caches.take_attempts(&rate_scopes(client, per_minute)) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many payment attempts from this address",
        )
        .with_header("retry-after", (60 / per_minute.max(1)).max(1).to_string())
        .into());
    }
    let payload: PaymentPayload = match sig.to_str().ok().map(x402::decode_header) {
        Some(Ok(p)) => p,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_payment",
                "malformed PAYMENT-SIGNATURE header",
            )
            .into())
        }
    };
    if payload.x402_version != x402::X402_VERSION {
        return Err(challenge("unsupported x402Version"));
    }
    // The client must have accepted exactly what we offer now (amount,
    // asset, recipient, network): a facilitator only checks the payload
    // against the requirements we hand it. Checked before the proof and
    // retry paths too (M7L3-R3-5): a price changed since the first request
    // is not served at the old price.
    if payload.accepted != requirements {
        return Err(challenge("payment does not match the requirements"));
    }
    // M7L3-R2-1: the identical retry of a paid request whose intent the
    // resolver recovered (the transfer into OnyxRevenue of the exact value
    // proven, the receipt written). Its verify would fail on a used nonce:
    // it is served from that proof instead, with no verify, settle or check,
    // within PROVEN_WINDOW_SECS of the recovery. No new unit is taken.
    if let Some((i, _)) = state.intents.proven(&key, (state.clock)()) {
        let requirements = requirements.clone();
        return Ok(Gate::Paid {
            req: Box::new(FacilitatorRequest {
                x402_version: x402::X402_VERSION,
                payment_payload: payload,
                payment_requirements: requirements,
            }),
            claim,
            payer: i.receipt.payer,
            nonce: i.receipt.nonce,
            amount: i.receipt.amount,
            valid_after: i.valid_after,
            valid_before: i.valid_before,
            max_life: 0,
            slot: None,
            attempt: Attempt::Proven,
        });
    }
    // M7L3-R1-1: the identical retry (same header bytes, resource and
    // client) of a paid request whose settle ended unknown or refused, while
    // its intent is open. Its verify would now fail on a used nonce, and a
    // new settle would be refused, though the payer may have paid: the
    // authorization check decides instead (served only on a proven transfer
    // into OnyxRevenue of the exact value). No new unit is taken: the first
    // request's stays spent.
    if state.checks_unknown_settles() {
        if let Some(i) = state.intents.awaiting(&key) {
            let Ok(slot) = state.facilitator_slots.clone().try_acquire_owned() else {
                return Err(Box::new(facilitator_down(
                    "too many payments being checked; retry shortly",
                )));
            };
            let requirements = requirements.clone();
            let max_life = requirements
                .max_timeout_seconds
                .saturating_add(state.config.authorization_slack.as_secs());
            return Ok(Gate::Paid {
                req: Box::new(FacilitatorRequest {
                    x402_version: x402::X402_VERSION,
                    payment_payload: payload,
                    payment_requirements: requirements,
                }),
                claim,
                payer: i.receipt.payer,
                nonce: i.receipt.nonce,
                amount: i.receipt.amount,
                valid_after: i.valid_after,
                valid_before: i.valid_before,
                max_life,
                slot: Some(slot),
                attempt: Attempt::Retry,
            });
        }
    } else if state.intents.awaiting(&key).is_some() {
        // SOAK-1 (L6 soak): without the authorization check (a
        // self-settling facilitator does its own, ADR-0026), the identical
        // retry of a paid request whose settle ended unknown used to take the
        // fresh path and get 409 `payment_in_use` "for another request ...
        // added no charge", though it is this very request and its payer
        // may have been charged: a client that believed it signed a new
        // authorization and paid twice. It is told to retry instead: the
        // resolver judges the intent from the chain every pass, and once the
        // transfer into OnyxRevenue is proven this retry is served from that
        // proof (M7L3-R2-1) with no new charge.
        return Err(Box::new(
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "payment_pending",
                "the settle of this identical request ended with an unknown outcome and is being resolved from the chain; retry this same request: it is served once the payment into OnyxRevenue is proven, and nothing more is charged",
            )
            .with_header("retry-after", PAYMENT_PENDING_RETRY_SECS.to_string())
            .into_response(),
        ));
    }
    // A payment is for one URL: it cannot buy a different row or shard.
    if payload.resource.as_ref().map(|r| r.url.as_str()) != Some(resource) {
        return Err(challenge("payment is not for this resource"));
    }
    let now = (state.clock)();
    let auth = match x402::exact_evm::precheck(&payload.payload, &requirements, now) {
        Ok(a) => a,
        Err(reason) => return Err(challenge(reason)),
    };
    // An authorization valid far into the future would keep its settle
    // intent open that long (OPS-L3-F4): x402 clients sign `validBefore =
    // now + maxTimeoutSeconds`, so anything later than that plus the slack
    // is refused before verify.
    let max_life = requirements
        .max_timeout_seconds
        .saturating_add(state.config.authorization_slack.as_secs());
    let latest = now.saturating_add(max_life);
    if auth.valid_before > latest {
        return Err(challenge(
            "invalid_exact_evm_payload_authorization_valid_before_too_late",
        ));
    }
    // ... and one that expires before our settle can be mined is refused
    // too (OPS-L4-F1): a settle that misses `validBefore` is refused by the
    // token. Bounded by `maxTimeoutSeconds`, so a client signing the usual
    // `now + maxTimeoutSeconds` always passes.
    let soonest = now.saturating_add(
        state
            .settle_budget()
            .as_secs()
            .saturating_add(SETTLE_MARGIN_SECS)
            .min(requirements.max_timeout_seconds),
    );
    if auth.valid_before < soonest {
        return Err(challenge(
            "invalid_exact_evm_payload_authorization_valid_before",
        ));
    }
    // A payer whose payments keep ending unsettled (probing 404s, refused
    // settles) is not sent to the facilitator again for a while
    // (M7-L40-F2/F3). The unit is taken now, under one lock, so concurrent
    // requests cannot overspend it; it comes back if the payment settles, if
    // the facilitator refuses it at verify, or if verify fails (M7-L41-F5,
    // M7-L42-F1/F2). It stays spent on a 404 after verify, a refused settle,
    // a settle that times out or fails (outcome unknown, M7-L47-F3) and an
    // abandoned request.
    let per_hour = state.config.unsettled_per_hour;
    if !state.caches.reserve_payer(&auth.from, per_hour) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "payer_limited",
            "too many verified payments from this payer ended without settling",
        )
        .with_header("retry-after", "600".into())
        .into());
    }
    claim.reserved(auth.from, per_hour);
    // One slot covers this payment's verify AND its settle (OPS-L2-F2): the
    // settle never queues for a slot outside its timeout, so a stop's settle
    // budget is a true bound.
    let Ok(slot) = state.facilitator_slots.clone().try_acquire_owned() else {
        claim.refund();
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "facilitator_busy",
            "too many payments being checked; retry shortly",
        )
        .with_header("retry-after", "1".into())
        .into());
    };
    let req = FacilitatorRequest {
        x402_version: x402::X402_VERSION,
        payment_payload: payload,
        payment_requirements: requirements.clone(),
    };
    match tokio::time::timeout(
        state.config.facilitator_timeout,
        pay.facilitator.verify(&req),
    )
    .await
    {
        Ok(Ok(v)) if v.is_valid => {
            // From here on, ending without a settle keeps the unit spent.
            Ok(Gate::Paid {
                req: Box::new(req),
                claim,
                payer: auth.from,
                nonce: auth.nonce,
                amount: price,
                valid_after: auth.valid_after,
                valid_before: auth.valid_before,
                max_life,
                slot: Some(slot),
                attempt: Attempt::Fresh,
            })
        }
        Ok(Ok(v)) => {
            // Refused by the facilitator (a replayed, already settled header;
            // an unfunded key): nothing was served or charged, so the unit
            // comes back and replays cannot drain the real payer's budget
            // (M7-L42-F2).
            claim.refund();
            Err(challenge(
                &v.invalid_reason.unwrap_or_else(|| "invalid payment".into()),
            ))
        }
        Ok(Err(e)) => {
            claim.refund();
            Err(Box::new(facilitator_down(e.to_string())))
        }
        Err(_) => {
            claim.refund();
            Err(Box::new(facilitator_down("facilitator verify timed out")))
        }
    }
}

/// How a settlement ended.
enum Settled {
    Ok(String),
    Refused(x402::SettleResponse),
    Down(String),
}

impl ApiState {
    /// The longest one settle may run: the facilitator patience
    /// ([`SETTLE_PATIENCE`] times `facilitator_timeout`), or a self-settling
    /// facilitator's own budget when that is longer (M8-L58-F2).
    pub fn settle_budget(&self) -> Duration {
        let patience = self.config.facilitator_timeout * SETTLE_PATIENCE;
        self.payments
            .as_ref()
            .and_then(|p| p.facilitator.settle_budget())
            .map_or(patience, |b| b.max(patience))
    }

    /// The longest one authorization check runs ([`settle_check`]),
    /// its re-reads included.
    pub fn settle_check_budget(&self) -> Duration {
        self.config.facilitator_timeout * SETTLE_PATIENCE / SETTLE_CHECK_SHARE
    }

    /// The wait before the check that follows a re-send, and between the
    /// re-reads of one check (M7L3-R1-1): [`ApiConfig::settle_recheck_wait`],
    /// at most [`ApiState::settle_check_budget`].
    pub fn settle_recheck_wait(&self) -> Duration {
        self.config
            .settle_recheck_wait
            .min(self.settle_check_budget())
    }

    /// True when an unknown settle is followed by the authorization check:
    /// a check is configured and the facilitator is not self-settling (which
    /// does its own, ADR-0026).
    pub(super) fn checks_unknown_settles(&self) -> bool {
        self.settle_check.is_some()
            && self
                .payments
                .as_ref()
                .is_some_and(|p| !p.facilitator.credits_on_settle())
    }

    /// The longest one settle task runs: the settle, and with the
    /// authorization check up to two checks, one re-send and the wait
    /// before the second check (M7L3-R1-1).
    pub fn settle_task_budget(&self) -> Duration {
        if self.checks_unknown_settles() {
            self.settle_budget() * 2 + self.settle_check_budget() * 2 + self.settle_recheck_wait()
        } else {
            self.settle_budget()
        }
    }

    /// The longest a clean stop takes (OPS-L1-F1): requests in flight end
    /// within `request_timeout`, and the last settle one of them started
    /// ends within [`ApiState::settle_task_budget`] after that. A
    /// supervisor's stop timeout must be longer than this plus its own slack.
    pub fn stop_budget(&self) -> Duration {
        self.config.request_timeout + self.settle_task_budget()
    }
}

/// Counts settle tasks still running, so a stop can wait for them
/// (OPS-L1-F1). Each task holds a [`SettleGuard`]; dropping it (finish,
/// error, panic, cancellation) counts the task out.
#[derive(Debug, Default)]
pub struct SettleTasks {
    live: std::sync::atomic::AtomicUsize,
    idle: tokio::sync::Notify,
}

/// One running settle task (see [`SettleTasks`]).
#[derive(Debug)]
pub struct SettleGuard(Arc<SettleTasks>);

impl SettleTasks {
    /// Count a task in until the returned guard is dropped.
    pub fn enter(self: &Arc<Self>) -> SettleGuard {
        self.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SettleGuard(self.clone())
    }

    /// Settle tasks running now.
    pub fn live(&self) -> usize {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wait until no settle task runs, at most `limit`. True when none runs.
    pub async fn wait_idle(&self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            // Registered before the check, so a task ending in between
            // still wakes this wait.
            notified.as_mut().enable();
            if self.live() == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.live() == 0;
            }
        }
    }
}

impl Drop for SettleGuard {
    fn drop(&mut self) {
        if self
            .0
            .live
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            self.0.idle.notify_waiters();
        }
    }
}

/// Phase two of a prepaid answer: take the voucher and journal it off the async threads, then
/// release the body; refused or not durable, nothing leaves.
#[allow(clippy::too_many_arguments)]
async fn finish_prepaid(
    state: &Arc<ApiState>,
    voucher: prepaid::Voucher,
    price: u64,
    key: Hash32,
    bill: (u64, Hash32),
    body: Value,
    etag: &str,
    immutable: bool,
) -> Response {
    let Some(pp) = state.prepaid.clone() else {
        return internal("prepaid is not configured").into_response();
    };
    let now = (state.clock)();
    let taken =
        tokio::task::spawn_blocking(move || pp.charge(voucher, u128::from(price), bill, key, now))
            .await;
    match taken {
        Ok(Ok(c)) => {
            let mut r = respond(body, etag, true, immutable, None);
            if let Ok(v) =
                HeaderValue::from_str(&format!("total={}; spendable={}", c.total, c.spendable))
            {
                r.headers_mut()
                    .insert(HeaderName::from_static(prepaid_gate::PREPAID_RESPONSE), v);
            }
            r
        }
        Ok(Err(prepaid_gate::ChargeError::Refused(e))) => {
            let mut err = ApiError::new(
                StatusCode::PAYMENT_REQUIRED,
                "prepaid_refused",
                e.to_string(),
            );
            if let prepaid::PrepaidError::WrongTotal { last, price } = e {
                err = err
                    .with_field("last", json!(last.to_string()))
                    .with_field("price", json!(price.to_string()));
            }
            err.into_response()
        }
        Ok(Err(prepaid_gate::ChargeError::Unavailable)) | Err(_) => {
            prepaid_unavailable().into_response()
        }
    }
}

/// Phase two: settle and release the body only on success. Settlement runs
/// in its own task (M7-L39-F1): if the request deadline fires first, the task
/// still finishes and records the response, so the identical retry from the
/// same client gets it (and a retry arriving meanwhile waits for it).
pub(super) async fn finish(
    state: &Arc<ApiState>,
    gate: Gate,
    bill: (u64, Hash32),
    body: Value,
    etag: String,
    immutable: bool,
) -> Response {
    let (req, mut claim, receipt, (valid_after, valid_before, max_life), slot, attempt) = match gate
    {
        Gate::Free => return respond(body, &etag, false, immutable, None),
        Gate::Contributor => return respond(body, &etag, true, immutable, None),
        Gate::Prepaid {
            voucher,
            price,
            key,
        } => return finish_prepaid(state, voucher, price, key, bill, body, &etag, immutable).await,
        Gate::Paid {
            req,
            claim,
            payer,
            nonce,
            amount,
            valid_after,
            valid_before,
            max_life,
            slot,
            attempt,
        } => (
            req,
            claim,
            receipts::Receipt {
                round: bill.0,
                shard: bill.1,
                amount,
                payer,
                nonce,
            },
            (valid_after, valid_before, max_life),
            slot,
            attempt,
        ),
    };
    let Some(pay) = state.payments.as_ref() else {
        return internal("payments are not configured").into_response();
    };
    // The intent is on disk BEFORE the settle call (OPS-L2-F1): whatever
    // happens next (a timeout, an error, a crash, a kill), the resolver can
    // tell from the chain whether this payment reached OnyxRevenue. If it
    // cannot be made durable, nothing is settled and the unit comes back.
    let intent = intents::Intent::new(
        receipt,
        valid_after,
        valid_before,
        (state.clock)(),
        max_life,
    );
    let in_use = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "payment_in_use",
            "this authorization is already being settled for another request; this request added no charge",
        )
        .into_response()
    };
    let begun = match attempt {
        Attempt::Fresh => {
            intents::begin_off_thread(&state.intents, intent, Some(claim.key())).await
        }
        // The intent written for this very request (M7L3-R1-1): taken over,
        // unless it was resolved or taken meanwhile.
        Attempt::Retry => match state.intents.adopt(&claim.key(), &receipt) {
            Some(id) => Ok(id),
            None => return in_use(),
        },
        // The resolver's proof for this very request and receipt
        // (M7L3-R2-1): served like a settled payment (stored for its
        // replays first), with the transaction that paid. Its receipt is on
        // disk already. Unless it was served or expired meanwhile.
        // The proof is deleted from the journal before the answer leaves
        // (M7L3-R3-2): a restart cannot serve it again.
        Attempt::Proven => {
            let key = claim.key();
            let taken =
                intents::take_proven_off_thread(&state.intents, key, receipt, (state.clock)())
                    .await;
            let Some(tx) = taken else {
                if state.intents.write_errors() > 0 {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "settle_intent_unavailable",
                        "the server cannot record this payment durably right now; retry later",
                    )
                    .into_response();
                }
                return in_use();
            };
            let header = x402::encode_header(&x402::SettleResponse {
                success: true,
                error_reason: None,
                payer: Some(hex0x(&receipt.payer)),
                transaction: tx,
                network: pay.terms.network.clone(),
            });
            state.caches.put_settled(
                key,
                cache::CachedResponse {
                    body: body.clone(),
                    etag: etag.clone(),
                    payment_response: header.clone(),
                    immutable,
                },
            );
            claim.settled();
            return respond(body, &etag, true, immutable, Some(&header));
        }
    };
    let retry = matches!(attempt, Attempt::Retry);
    let intent_id = match begun {
        Ok(id) => id,
        Err(intents::BeginError::PayerBusy) => {
            claim.refund();
            return ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "payer_busy",
                "this payer has too many payments waiting to be resolved; nothing was charged",
            )
            .with_header("retry-after", "60".into())
            .into_response();
        }
        Err(intents::BeginError::InUse) => {
            claim.refund();
            return in_use();
        }
        Err(intents::BeginError::ClockSkew) => {
            claim.refund();
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "clock_skew",
                "the server's clock disagrees with the chain's; nothing was charged",
            )
            .with_header("retry-after", "60".into())
            .into_response();
        }
        Err(_) => {
            claim.refund();
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "settle_intent_unavailable",
                "the server cannot record this payment durably right now; nothing was charged",
            )
            .into_response();
        }
    };
    let mut guard = Some(intents::IntentGuard::new(state.intents.clone(), intent_id));
    let st = state.clone();
    let cached_body = body.clone();
    let cached_etag = etag.clone();
    // The claim moves into the task: whatever happens there (success,
    // refusal, timeout, panic), dropping it releases the key and wakes
    // waiting retries (M7-L40-F5). The guard counts the task until it ends,
    // so a stop waits for it (OPS-L1-F1). The facilitator slot taken at
    // verify moves in too, so the settle never queues (OPS-L2-F2); the
    // intent guard hands the intent to the resolver unless the task ends it.
    let running = st.settle_tasks.enter();
    let task = tokio::spawn(async move {
        let _running = running;
        let _slot = slot;
        let key = claim.key();
        let outcome = match st.payments.as_ref() {
            None => Settled::Down("payments are not configured".into()),
            Some(pay) => {
                // A self-settling facilitator bounds its own settle; never
                // cut it short, or its pending handling would not run
                // (M8-L58-F2). The facilitator's HTTP client waits as long
                // (OPS-L2-F1).
                let patience = st.settle_budget();
                let mut outcome = if retry {
                    // No new settle: the check below decides (M7L3-R1-1).
                    Err(
                        "the earlier settle of this authorization ended unknown or refused"
                            .to_string(),
                    )
                } else {
                    settle_call(pay, &req, bill, patience).await
                };
                // M7-F5 residual: before giving up on (or re-sending) a
                // settle whose outcome is unknown, ask the token.
                if let (Err(why), Some(check)) = (&outcome, &st.settle_check) {
                    if st.checks_unknown_settles() {
                        outcome = after_unknown(
                            &st,
                            pay,
                            check.as_ref(),
                            (&req, bill),
                            (&receipt, intent_id),
                            (valid_after, valid_before),
                            why.clone(),
                        )
                        .await;
                    }
                }
                match outcome {
                    Ok(s) if s.success => {
                        let header = x402::encode_header(&s);
                        // The receipt is on disk before anything else sees
                        // the payment as settled (REV-L3-F7): before the
                        // cache answers a waiting retry and before the
                        // response leaves. A self-settled payment credited
                        // its shard in the same transaction (ADR-0026): no
                        // receipt. Then the intent is done.
                        // The intent ends only once the receipt is durable
                        // (OPS-L3-F1): if the receipt log cannot keep it,
                        // the intent stays for the resolver, which records
                        // it once the log is healthy (after a restart).
                        let durable = pay.facilitator.credits_on_settle()
                            || receipts::record_off_thread(&st.receipts, receipt).await;
                        // Not durable: the request is served all the same,
                        // so its intent drops the request key (M7L3-R3-4):
                        // after a restart its identical retry is not served
                        // again.
                        if let Some(g) = guard.take() {
                            if durable {
                                g.ended().await;
                            } else {
                                g.served_without_receipt().await;
                            }
                        }
                        // Stored before the claim is released, so a waiting
                        // retry finds it.
                        st.caches.put_settled(
                            key,
                            cache::CachedResponse {
                                body: cached_body,
                                etag: cached_etag,
                                payment_response: header.clone(),
                                immutable,
                            },
                        );
                        claim.settled();
                        Settled::Ok(header)
                    }
                    // A refused settle is the payer's (no funds, a spent
                    // nonce): the unit stays spent. The intent stays for
                    // the resolver: if the money moved to us anyway it is
                    // still attributed; if not, it expires.
                    Ok(s) => Settled::Refused(s),
                    // A timeout, a transport error or an error page at
                    // settle: the facilitator may have sent the transfer
                    // before failing. The unit stays spent and nothing is
                    // stored as a success (M7-F5, M7-L47-F3, superseding
                    // M7-L42-F1's refund).
                    Err(why) => Settled::Down(format!(
                        "{why}; the settle outcome is unknown and will be resolved from the chain (THREATS.md OPS-L2-F1)"
                    )),
                }
            }
        };
        if matches!(outcome, Settled::Down(_)) {
            note_unknown(&st, &receipt);
        }
        // M7L3-R2-2: the intent goes back to the resolver BEFORE the claim
        // is released: releasing the claim wakes a waiting identical retry,
        // which must find the intent no settle owns (`awaiting`), not one
        // still marked as this task's.
        drop(guard);
        drop(claim);
        outcome
    });
    match task.await {
        Ok(Settled::Ok(header)) => respond(body, &etag, true, immutable, Some(&header)),
        Ok(Settled::Refused(s)) => ApiError::new(
            StatusCode::PAYMENT_REQUIRED,
            "payment_failed",
            s.error_reason
                .clone()
                .unwrap_or_else(|| "settlement failed".into()),
        )
        .with_header(x402::PAYMENT_RESPONSE, x402::encode_header(&s))
        .into_response(),
        Ok(Settled::Down(m)) => facilitator_down(m),
        Err(_) => {
            // A panicked settle task: the intent guard released the intent
            // to the resolver; count and log it like any unknown outcome.
            note_unknown(state, &receipt);
            internal("settlement task failed").into_response()
        }
    }
}

/// One facilitator settle within `patience`. `Err`: the outcome is unknown
/// (a timeout, a transport error or an error page).
async fn settle_call(
    pay: &Payments,
    req: &x402::FacilitatorRequest,
    bill: (u64, Hash32),
    patience: Duration,
) -> Result<x402::SettleResponse, String> {
    match tokio::time::timeout(patience, pay.facilitator.settle_for(req, bill)).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("facilitator settle timed out".into()),
    }
}

/// After a settle whose outcome is unknown (`why`), ask the token what
/// became of the authorization ([`settle_check`], M7-F5 residual): its
/// payment into `OnyxRevenue` is a settled payment (served, with that
/// transaction); a use paying someone else or a cancel is the payer's
/// refusal; an unused one is sent once more (its nonce lets it move money at
/// most once) and, about one block later, checked again if that does not
/// succeed (M7L3-R1-1 (a)). A re-send the token refused as used means the
/// first settle landed: the second check then reads the token's logs even
/// when a lagging node still reads the authorization unused (M7L3-R1-1
/// (b)). Each check reads again while a use is not found or the node fails,
/// within its budget. Anything else stays unknown (`Err`).
async fn after_unknown(
    st: &ApiState,
    pay: &Payments,
    check: &dyn settle_check::SettleCheck,
    (req, bill): (&x402::FacilitatorRequest, (u64, Hash32)),
    (receipt, intent_id): (&receipts::Receipt, u64),
    (valid_after, valid_before): (u64, u64),
    mut why: String,
) -> Result<x402::SettleResponse, String> {
    let payer = Some(hex0x(&receipt.payer));
    let network = pay.terms.network.clone();
    // The settle was made after this moment; the chain's clock is within
    // the allowed skew of ours (the intent was refused otherwise).
    let earliest = (st.clock)()
        .saturating_sub(intents::MAX_CLOCK_SKEW_SECS)
        .max(valid_after);
    let decided = |r: CheckRead| match r {
        CheckRead::Ours(tx) => Some(Ok(x402::SettleResponse {
            success: true,
            error_reason: None,
            payer: payer.clone(),
            transaction: tx,
            network: network.clone(),
        })),
        CheckRead::NotOurs => Some(Ok(x402::SettleResponse {
            success: false,
            error_reason: Some("invalid_transaction_state".into()),
            payer: payer.clone(),
            transaction: String::new(),
            network: network.clone(),
        })),
        CheckRead::Unused => None,
        CheckRead::Undecided(m) => Some(Err(m)),
    };
    let span = (earliest, valid_before);
    match decided(read_use(st, check, receipt, span, false).await) {
        Some(Ok(s)) => return Ok(s),
        Some(Err(m)) => return Err(format!("{why}; {m}")),
        None => {}
    }
    // Unused at the latest block. A re-send needs time for its own settle
    // before `validBefore` (the token refuses it after).
    let patience = st.settle_budget();
    let time_left = valid_before.saturating_sub((st.clock)());
    if time_left <= patience.as_secs().saturating_add(SETTLE_MARGIN_SECS) {
        return Err(format!(
            "{why}; the authorization is still unused (not re-sent: it expires too soon)"
        ));
    }
    // At most MAX_RESENDS re-sends per authorization, its identical retries
    // included (M7L3-R2-5): a retry takes no new payer unit.
    if !st.intents.take_resend(intent_id) {
        return Err(format!(
            "{why}; the authorization is still unused (not re-sent: already re-sent {} times)",
            intents::MAX_RESENDS
        ));
    }
    let refused_as_used = match settle_call(pay, req, bill, patience).await {
        Ok(s) if s.success => return Ok(s),
        Ok(s) => {
            why = format!(
                "re-send refused ({})",
                s.error_reason.as_deref().unwrap_or("no reason")
            );
            refused_as_used(s.error_reason.as_deref())
        }
        Err(e) => {
            why = format!("re-send: {e}");
            false
        }
    };
    // About one block, so a node one block behind has caught up (a).
    tokio::time::sleep(st.settle_recheck_wait()).await;
    match decided(read_use(st, check, receipt, span, refused_as_used).await) {
        Some(Ok(s)) => Ok(s),
        Some(Err(m)) => Err(format!("{why}; {m}")),
        None => Err(format!(
            "{why}; the authorization is still unused (not re-sent again)"
        )),
    }
}

/// Whether a refused settle's reason says the authorization was already
/// used (the token's `authorization is used or canceled`, x402's
/// `invalid_transaction_state`), or gives no reason.
fn refused_as_used(reason: Option<&str>) -> bool {
    reason.is_none_or(|r| {
        let r = r.to_ascii_lowercase();
        r.contains("transaction_state") || r.contains("nonce") || r.contains("used")
    })
}

/// What one authorization check found.
enum CheckRead {
    Ours(String),
    NotOurs,
    Unused,
    Undecided(String),
}

/// One authorization check of `receipt`'s authorization, used in `span`
/// (chain time): `authorizationState`, then (used) the token's logs; with
/// `used` (the token refused a re-send as used), an unused answer is a
/// lagging node's and the logs are read anyway. A use not found yet or a
/// node error is read again after [`ApiState::settle_recheck_wait`], within
/// [`ApiState::settle_check_budget`] (M7L3-R1-1).
async fn read_use(
    st: &ApiState,
    check: &dyn settle_check::SettleCheck,
    receipt: &receipts::Receipt,
    (earliest, latest): (u64, u64),
    used: bool,
) -> CheckRead {
    let (from, nonce, value) = (receipt.payer, receipt.nonce, u128::from(receipt.amount));
    let deadline = tokio::time::Instant::now() + st.settle_check_budget();
    let wait = st.settle_recheck_wait();
    loop {
        let read = tokio::time::timeout_at(deadline, async {
            match check.use_of(from, nonce, value, earliest, latest).await {
                Ok(arrival::UseOf::Unused) if used => {
                    check.logged_use(from, nonce, value, earliest, latest).await
                }
                r => r,
            }
        })
        .await;
        let last = match read {
            Ok(Ok(arrival::UseOf::Ours(tx))) => return CheckRead::Ours(tx),
            Ok(Ok(arrival::UseOf::NotOurs)) => return CheckRead::NotOurs,
            Ok(Ok(arrival::UseOf::Unused)) => return CheckRead::Unused,
            Ok(Ok(arrival::UseOf::NotFound)) => {
                "the authorization is used but its transfer was not found yet".to_string()
            }
            Ok(Err(e)) => format!("authorizationState check failed: {e}"),
            Err(_) => return CheckRead::Undecided("authorizationState check timed out".into()),
        };
        if tokio::time::Instant::now() + wait >= deadline {
            return CheckRead::Undecided(last);
        }
        tokio::time::sleep(wait).await;
    }
}

/// Count and log a settle whose outcome is unknown (OPS-L2-F3): the payer,
/// nonce, round, shard and amount, so an operator can follow it; the intent
/// resolver settles the question from the chain.
fn note_unknown(state: &ApiState, r: &receipts::Receipt) {
    state
        .unknown_settles
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "onyx-api: settle outcome unknown: payer 0x{} nonce 0x{} round {} shard {} amount {}; kept as an intent for the resolver",
        hex::encode(r.payer),
        hex::encode(r.nonce),
        r.round,
        r.shard,
        r.amount
    );
}
