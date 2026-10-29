//! Self-settled x402 payments (ADR-0026). The API acts as its own `exact`
//! facilitator: it submits the client's EIP-3009 authorization to
//! `OnyxRevenue.settle`, which moves the USDC to itself and credits the shard
//! that was served in the same transaction. Recipient and amount are proven by
//! the token; only the choice of shard rests on the operator (REV-1, now
//! narrowed to attribution). No receipt is needed for a payment settled here.
//!
//! When the authorization was already used on chain (a third party submitted
//! it to the token first, or an earlier settle of ours landed after its
//! deadline), the funds may sit in `OnyxRevenue` unallocated. A receipt for
//! the `credit` path is queued only when the token's own logs prove it: an
//! `AuthorizationUsed(from, nonce)` and a `Transfer(from -> OnyxRevenue,
//! value)` in the same transaction (M8-L58-F3). The outcome is reported as
//! unknown (M7-F5 semantics: the unit stays spent, nothing is served); a
//! nonce spent elsewhere is the payer's refusal. A settle broadcast but not
//! mined in time is kept as pending and resolved later the same way
//! ([`SelfSettle::resolve_pending`], M8-L58-F2).
//!
//! With [`SelfSettle::persist`] both the receipt log and the pending settles
//! are kept on disk (REV-11): a settle broadcast just before a crash is still
//! resolved after the restart, and its receipt (if the token's logs prove the
//! payment arrived) still reaches the reporter.

use crate::evm_tx::{SendError, Sender};
use crate::journal::{Binding, Journal};
use crate::receipts::{record_off_thread, Receipt, ReceiptLog};
use crate::roots::JsonRpc;
use crate::x402::{
    exact_evm, BoxFuture, Facilitator, FacilitatorError, FacilitatorRequest, SettleResponse,
    VerifyResponse, X402_VERSION,
};
use onyx_core::{keccak256, Hash32};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How far back the arrival proof looks for the authorization's use (no
/// further back than its `validAfter`): 6 hours, longer than any x402
/// validity window.
const PROOF_LOOKBACK_SECS: u64 = 6 * 3600;
/// Extra time over the receipt deadline for a settle's own node calls
/// (estimate, nonce, fees, send, diagnosis).
const SETTLE_RPC_SLACK: Duration = Duration::from_secs(20);
/// A settle still unmined after this long is judged by the token's logs.
const PENDING_GIVE_UP: Duration = Duration::from_secs(600);
/// The journal kind of the pending settles.
pub const PENDING_JOURNAL_KIND: &str = "onyx-pending-settles";

/// A settle that was broadcast but not seen mined before its deadline.
struct PendingSettle {
    /// Journal id (a running count when there is no journal).
    id: u64,
    hash: String,
    receipt: Receipt,
    auth: exact_evm::Authorization,
    since: Instant,
}

/// The pending settles and, with [`SelfSettle::persist`], their journal.
#[derive(Default)]
struct PendingSet {
    list: Vec<PendingSettle>,
    journal: Option<Journal>,
    next_mem: u64,
    write_errors: u64,
}

impl PendingSet {
    /// Journal the change; a write error closes the journal (as for the
    /// receipt log) and is counted. Returns the ids for `add`.
    fn write(&mut self, del: &[u64], add: &[Value]) -> Vec<u64> {
        match self.journal.as_mut().map(|j| j.apply(del, add)) {
            Some(Ok(ids)) => ids,
            other => {
                if matches!(other, Some(Err(_))) {
                    self.write_errors += 1;
                    self.journal = None;
                }
                let start = self
                    .next_mem
                    .max(self.list.iter().map(|p| p.id + 1).max().unwrap_or(0));
                self.next_mem = start + add.len() as u64;
                (start..self.next_mem).collect()
            }
        }
    }
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn fixed<const N: usize>(v: &Value) -> Option<[u8; N]> {
    hex::decode(v.as_str()?.strip_prefix("0x")?)
        .ok()?
        .try_into()
        .ok()
}

/// A pending settle as stored in the journal (`since` as unix seconds).
fn pending_json(
    hash: &str,
    receipt: &Receipt,
    a: &exact_evm::Authorization,
    since_unix: u64,
) -> Value {
    json!({
        "hash": hash,
        "receipt": receipt.to_json(),
        "auth": {
            "from": hex0x(&a.from),
            "to": hex0x(&a.to),
            "value": a.value.to_string(),
            "validAfter": a.valid_after,
            "validBefore": a.valid_before,
            "nonce": hex0x(&a.nonce),
        },
        "since": since_unix,
    })
}

fn pending_from_json(id: u64, v: &Value) -> Option<PendingSettle> {
    let a = v.get("auth")?;
    let since_unix = v.get("since")?.as_u64()?;
    let age = Duration::from_secs(now_unix().saturating_sub(since_unix));
    Some(PendingSettle {
        id,
        hash: v.get("hash")?.as_str()?.to_string(),
        receipt: Receipt::from_json(v.get("receipt")?)?,
        auth: exact_evm::Authorization {
            from: fixed::<20>(a.get("from")?)?,
            to: fixed::<20>(a.get("to")?)?,
            value: a.get("value")?.as_str()?.parse().ok()?,
            valid_after: a.get("validAfter")?.as_u64()?,
            valid_before: a.get("validBefore")?.as_u64()?,
            nonce: fixed::<32>(a.get("nonce")?)?,
        },
        // Its age carries over a restart, so the give-up clock keeps running.
        since: Instant::now().checked_sub(age).unwrap_or_else(Instant::now),
    })
}

fn selector(sig: &str) -> [u8; 4] {
    let h = keccak256(sig.as_bytes()).0;
    [h[0], h[1], h[2], h[3]]
}

fn word_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// ABI tail of a `bytes` argument: length word, then the data padded to 32.
fn bytes_tail(b: &[u8]) -> Vec<u8> {
    let mut out = word_u128(b.len() as u128).to_vec();
    out.extend_from_slice(b);
    out.resize(32 + b.len().div_ceil(32) * 32, 0);
    out
}

/// `OnyxRevenue.settle(uint64,bytes32,(address,uint256,uint256,uint256,bytes32),bytes)`.
pub fn settle_calldata(
    round: u64,
    shard: &Hash32,
    a: &exact_evm::Authorization,
    signature: &[u8],
) -> Vec<u8> {
    let mut d =
        selector("settle(uint64,bytes32,(address,uint256,uint256,uint256,bytes32),bytes)").to_vec();
    d.extend_from_slice(&word_u128(u128::from(round)));
    d.extend_from_slice(&shard.0);
    d.extend_from_slice(&word_addr(&a.from));
    d.extend_from_slice(&word_u128(a.value));
    d.extend_from_slice(&word_u128(u128::from(a.valid_after)));
    d.extend_from_slice(&word_u128(u128::from(a.valid_before)));
    d.extend_from_slice(&a.nonce);
    // Eight head words: the bytes start right after them.
    d.extend_from_slice(&word_u128(8 * 32));
    d.extend(bytes_tail(signature));
    d
}

/// The token's own `transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,bytes)`
/// (FiatToken v2.2), used to simulate the payment at verify.
pub fn transfer_with_authorization_calldata(
    a: &exact_evm::Authorization,
    signature: &[u8],
) -> Vec<u8> {
    let mut d = selector(
        "transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,bytes)",
    )
    .to_vec();
    d.extend_from_slice(&word_addr(&a.from));
    d.extend_from_slice(&word_addr(&a.to));
    d.extend_from_slice(&word_u128(a.value));
    d.extend_from_slice(&word_u128(u128::from(a.valid_after)));
    d.extend_from_slice(&word_u128(u128::from(a.valid_before)));
    d.extend_from_slice(&a.nonce);
    d.extend_from_slice(&word_u128(7 * 32));
    d.extend(bytes_tail(signature));
    d
}

fn authorization_state_calldata(from: &[u8; 20], nonce: &[u8; 32]) -> Vec<u8> {
    let mut d = selector("authorizationState(address,bytes32)").to_vec();
    d.extend_from_slice(&word_addr(from));
    d.extend_from_slice(nonce);
    d
}

fn balance_of_calldata(who: &[u8; 20]) -> Vec<u8> {
    let mut d = selector("balanceOf(address)").to_vec();
    d.extend_from_slice(&word_addr(who));
    d
}

fn parse_addr(s: &str) -> Option<[u8; 20]> {
    let b = hex::decode(s.trim().to_lowercase().strip_prefix("0x")?).ok()?;
    b.try_into().ok()
}

/// The API's own facilitator for `exact` payments to `OnyxRevenue`.
pub struct SelfSettle<T: JsonRpc> {
    sender: Arc<Sender<T>>,
    revenue: [u8; 20],
    token: [u8; 20],
    network: String,
    receipts: Arc<ReceiptLog>,
    deadline: Duration,
    pending: Arc<Mutex<PendingSet>>,
}

impl<T: JsonRpc> std::fmt::Debug for SelfSettle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfSettle")
            .field("sender", &self.sender)
            .field("revenue", &format!("0x{}", hex::encode(self.revenue)))
            .field("token", &format!("0x{}", hex::encode(self.token)))
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

/// How a settle attempt that did not land as a success ended.
enum Failed {
    /// The authorization was already used on chain: funds may sit in the
    /// contract unallocated (receipt recorded) or were spent elsewhere.
    Used,
    /// A plain refusal (bad signature, no funds, expired).
    Refused(&'static str),
}

impl<T: JsonRpc> SelfSettle<T> {
    /// A self-settling facilitator: `sender` holds the reporter key of
    /// `revenue` (the x402 `payTo`); `token` is the `exact` asset; `network`
    /// the CAIP-2 network both live on. Settles wait up to `deadline` for a
    /// receipt.
    pub fn new(
        sender: Sender<T>,
        revenue: [u8; 20],
        token: [u8; 20],
        network: impl Into<String>,
        deadline: Duration,
    ) -> Self {
        SelfSettle {
            sender: Arc::new(sender),
            revenue,
            token,
            network: network.into(),
            receipts: Arc::new(ReceiptLog::new()),
            deadline,
            pending: Arc::new(Mutex::new(PendingSet::default())),
        }
    }

    /// Keep the receipt log (`dir/receipts.jsonl`) and the pending settles
    /// (`dir/pending-settles.jsonl`) on disk, and load what they already hold
    /// (REV-11). `dir` is created if missing. Both journals are bound to this
    /// chain and `OnyxRevenue` (REV-L3-F4). Refuses a damaged journal, one of
    /// another deployment, and one another process holds.
    pub fn persist(mut self, dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let bind = Binding {
            chain_id: self.sender.chain_id(),
            contract: self.revenue,
        };
        self.receipts = Arc::new(ReceiptLog::open(dir.join("receipts.jsonl"), &bind)?);
        let (journal, live) = Journal::open(
            dir.join("pending-settles.jsonl"),
            PENDING_JOURNAL_KIND,
            &bind,
        )?;
        let mut list = Vec::with_capacity(live.len());
        for (id, v) in live {
            list.push(pending_from_json(id, &v).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("pending settle journal entry {id} is not a pending settle"),
                )
            })?);
        }
        self.pending = Arc::new(Mutex::new(PendingSet {
            list,
            journal: Some(journal),
            ..PendingSet::default()
        }));
        Ok(self)
    }

    /// The sender (operator key) this facilitator settles with: a reporter
    /// posting `credit` must share it, so the two never race for a nonce.
    pub fn shared_sender(&self) -> Arc<Sender<T>> {
        self.sender.clone()
    }

    /// The contract this facilitator pays into (the x402 `payTo`).
    pub fn revenue(&self) -> [u8; 20] {
        self.revenue
    }

    /// Settles broadcast but not yet resolved (see [`Self::resolve_pending`]).
    pub fn pending_settles(&self) -> usize {
        self.pending.lock().map_or(0, |p| p.list.len())
    }

    /// Journal writes of pending settles that failed (kept in memory only).
    pub fn pending_write_errors(&self) -> u64 {
        self.pending.lock().map_or(0, |p| p.write_errors)
    }

    /// Proof from the token's own logs that `a` moved exactly `a.value` from
    /// the payer into `OnyxRevenue`: an `AuthorizationUsed(from, nonce)` log
    /// and a `Transfer(from, revenue, value)` log from the token in the same
    /// transaction (M8-L58-F3).
    async fn arrived(&self, a: &exact_evm::Authorization) -> Result<bool, String> {
        crate::arrival::arrived(
            self.sender.rpc(),
            &crate::arrival::Payment {
                token: self.token,
                revenue: self.revenue,
                from: a.from,
                nonce: a.nonce,
                value: a.value,
            },
            a.valid_after
                .max(now_unix().saturating_sub(PROOF_LOOKBACK_SECS)),
            a.valid_before,
        )
        .await
    }

    /// Resolve settles that were broadcast but not seen mined in time: a
    /// successful one already credited its shard; a reverted or long-unmined
    /// one becomes a receipt only if the token's logs prove the payment
    /// reached `OnyxRevenue` (else the payer spent it elsewhere and nothing
    /// is queued). Called at the start of every settle; a reporter calls it
    /// before posting receipts. Returns how many receipts it queued.
    pub async fn resolve_pending(&self) -> usize {
        let open: Vec<PendingSettle> = match self.pending.lock() {
            Ok(mut p) => std::mem::take(&mut p.list),
            Err(_) => return 0,
        };
        let mut keep = Vec::new();
        let mut done = Vec::new();
        let mut queued = 0;
        for ps in open {
            let mined = self
                .sender
                .rpc()
                .call("eth_getTransactionReceipt", json!([ps.hash]))
                .await;
            let judge = match &mined {
                Ok(r) if r["status"] == "0x1" => {
                    done.push(ps.id);
                    continue;
                }
                Ok(r) if !r.is_null() => true,
                Ok(_) => ps.since.elapsed() >= PENDING_GIVE_UP,
                Err(_) => false,
            };
            if !judge {
                keep.push(ps);
                continue;
            }
            match self.arrived(&ps.auth).await {
                Ok(true) => {
                    // The receipt is on disk before the pending entry is
                    // removed: a crash in between posts it twice, and the
                    // contract skips the second as already credited. A
                    // receipt that is not durable keeps the entry
                    // (OPS-L3-F1).
                    if record_off_thread(&self.receipts, ps.receipt).await {
                        done.push(ps.id);
                        queued += 1;
                    } else {
                        keep.push(ps);
                    }
                }
                Ok(false) => done.push(ps.id),
                Err(_) => keep.push(ps),
            }
        }
        // The journal write runs on a blocking thread (REV-L3-F7).
        let set = self.pending.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(mut p) = set.lock() {
                // Settles queued while this ran stay; the resolved leave.
                let mut list = keep;
                list.append(&mut p.list);
                p.list = list;
                p.write(&done, &[]);
            }
        })
        .await;
        queued
    }

    /// The receipt log for payments that could not be settled here but may
    /// have reached the contract; give it to `ApiState::receipts` so the
    /// reporter posts them with the others.
    pub fn receipts(&self) -> Arc<ReceiptLog> {
        self.receipts.clone()
    }

    /// Local and on-chain checks; the authorization and its signature bytes.
    async fn check(
        &self,
        req: &FacilitatorRequest,
    ) -> Result<(exact_evm::Authorization, Vec<u8>), &'static str> {
        if req.x402_version != X402_VERSION || req.payment_payload.x402_version != X402_VERSION {
            return Err("invalid_x402_version");
        }
        if req.payment_payload.accepted != req.payment_requirements {
            return Err("invalid_payment_requirements");
        }
        let r = &req.payment_requirements;
        if r.network != self.network
            || parse_addr(&r.pay_to) != Some(self.revenue)
            || parse_addr(&r.asset) != Some(self.token)
        {
            return Err("invalid_payment_requirements");
        }
        let auth = exact_evm::precheck(&req.payment_payload.payload, r, now_unix())?;
        let sig = req.payment_payload.payload["signature"]
            .as_str()
            .and_then(|s| hex::decode(s.trim_start_matches("0x")).ok())
            .ok_or("invalid_payload")?;
        Ok((auth, sig))
    }

    async fn used(&self, a: &exact_evm::Authorization) -> Result<bool, String> {
        let r = self
            .sender
            .call(
                &self.token,
                &authorization_state_calldata(&a.from, &a.nonce),
            )
            .await?;
        Ok(r.last().copied() == Some(1))
    }

    /// Whether `addr` carries code: an ERC-1271 contract or an EIP-7702
    /// delegated EOA (OPS-L5-F1). FiatToken then checks the signature with
    /// `isValidSignature`, code the payer controls, so no `eth_call` (which
    /// runs at gas price 0, a condition that code can test) says anything
    /// about the real transaction. Such payers are refused, as smart-wallet
    /// signatures are (delivery.md, M7-L39-F4).
    async fn has_code(&self, addr: &[u8; 20]) -> Result<bool, String> {
        let v = self
            .sender
            .rpc()
            .call(
                "eth_getCode",
                json!([format!("0x{}", hex::encode(addr)), "latest"]),
            )
            .await
            .map_err(|e| format!("eth_getCode: {e}"))?;
        let code = v.as_str().ok_or("eth_getCode: not a hex string")?;
        Ok(!code.trim_start_matches("0x").is_empty())
    }

    /// Why a payment that could not settle failed: used nonce, no funds, or
    /// the token's own refusal (signature, window).
    async fn diagnose(&self, a: &exact_evm::Authorization) -> Result<Failed, String> {
        if self.used(a).await? {
            return Ok(Failed::Used);
        }
        let bal = self
            .sender
            .call(&self.token, &balance_of_calldata(&a.from))
            .await?;
        let mut w = [0u8; 16];
        if bal.len() == 32 {
            w.copy_from_slice(&bal[16..]);
        }
        if u128::from_be_bytes(w) < a.value {
            return Ok(Failed::Refused("insufficient_funds"));
        }
        Ok(Failed::Refused("invalid_exact_evm_payload_signature"))
    }

    fn refused(&self, reason: &str, payer: Option<String>) -> SettleResponse {
        SettleResponse {
            success: false,
            error_reason: Some(reason.into()),
            payer,
            transaction: String::new(),
            network: self.network.clone(),
        }
    }

    async fn settle_shard(
        &self,
        req: &FacilitatorRequest,
        (round, shard): (u64, Hash32),
    ) -> Result<SettleResponse, FacilitatorError> {
        let (auth, sig) = match self.check(req).await {
            Ok(x) => x,
            Err(reason) => return Ok(self.refused(reason, None)),
        };
        let payer = Some(format!("0x{}", hex::encode(auth.from)));
        // OPS-L5-F1: never spend gas on a settle whose outcome payer code
        // decides.
        match self.has_code(&auth.from).await {
            Ok(true) => return Ok(self.refused("invalid_exact_evm_payload_signature", payer)),
            Ok(false) => {}
            Err(e) => return Err(FacilitatorError::Unavailable(e)),
        }
        self.resolve_pending().await;
        let data = settle_calldata(round, &shard, &auth, &sig);
        let receipt = Receipt {
            round,
            shard,
            amount: u64::try_from(auth.value).unwrap_or(u64::MAX),
            payer: auth.from,
            nonce: auth.nonce,
        };
        match self
            .sender
            .send(&self.revenue, &data, 0, self.deadline)
            .await
        {
            Ok(m) => Ok(SettleResponse {
                success: true,
                error_reason: None,
                payer,
                transaction: m.hash,
                network: self.network.clone(),
            }),
            Err(SendError::WouldRevert(_)) | Err(SendError::Reverted(_)) => {
                match self.diagnose(&auth).await {
                    Ok(Failed::Refused(reason)) => {
                        // Payer code (a delegation set after the check
                        // above) decides the token's answer: the payer's
                        // fault, never ours (OPS-L5-F1).
                        match self.has_code(&auth.from).await {
                            Ok(true) => return Ok(self.refused(reason, payer)),
                            Ok(false) => {}
                            Err(e) => return Err(FacilitatorError::Unavailable(e)),
                        }
                        // The settle reverts but the token would take the
                        // payment: the fault is ours (reporter key, an
                        // unregistered shard), never the payer's
                        // (M8-L58-F4). Nothing moved. With no payer code the
                        // token only runs ecrecover, so this simulation is
                        // not the payer's to steer.
                        let sim = self
                            .sender
                            .call(
                                &self.token,
                                &transfer_with_authorization_calldata(&auth, &sig),
                            )
                            .await;
                        if sim.is_ok() {
                            return Err(FacilitatorError::Unavailable(
                                "the settle would revert for a server-side reason; nothing was sent".into(),
                            ));
                        }
                        Ok(self.refused(reason, payer))
                    }
                    Ok(Failed::Used) => match self.arrived(&auth).await {
                        // In the contract, unallocated: the receipt path
                        // attributes it (total-capped), or skips it if it
                        // was credited already (M8-L58-F3).
                        Ok(true) => {
                            record_off_thread(&self.receipts, receipt).await;
                            Err(FacilitatorError::Unavailable(
                                "authorization already used and its USDC reached OnyxRevenue; queued for the receipt path; the outcome is unknown (ADR-0026)".into(),
                            ))
                        }
                        // Spent somewhere else: the payer's refusal.
                        Ok(false) => Ok(self.refused("invalid_transaction_state", payer)),
                        Err(e) => Err(FacilitatorError::Unavailable(e)),
                    },
                    Err(e) => Err(FacilitatorError::Unavailable(e)),
                }
            }
            Err(SendError::Pending(hash)) => {
                let set = self.pending.clone();
                let h = hash.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mut p) = set.lock() {
                        let v = pending_json(&h, &receipt, &auth, now_unix());
                        if let Some(&id) = p.write(&[], &[v]).first() {
                            p.list.push(PendingSettle {
                                id,
                                hash: h,
                                receipt,
                                auth,
                                since: Instant::now(),
                            });
                        }
                    }
                })
                .await;
                Err(FacilitatorError::Unavailable(format!(
                    "settle {hash} not mined before the deadline; the outcome is unknown (kept as pending)"
                )))
            }
            Err(SendError::Busy) => Err(FacilitatorError::Unavailable(
                "another settle holds the operator key; nothing was sent".into(),
            )),
            // REV-L4-F1: a settle sent behind a stuck transaction would wait
            // with it (the payer charged late or never served).
            Err(SendError::Stuck(nonce)) => Err(FacilitatorError::Unavailable(format!(
                "the operator key has a transaction stuck at nonce {nonce}; nothing was sent"
            ))),
            Err(SendError::Rpc(e)) => Err(FacilitatorError::Unavailable(e)),
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl<T: JsonRpc + 'static> Facilitator for SelfSettle<T> {
    fn verify<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<VerifyResponse, FacilitatorError>> {
        Box::pin(async move {
            let (auth, sig) = match self.check(req).await {
                Ok(x) => x,
                Err(reason) => {
                    return Ok(VerifyResponse {
                        is_valid: false,
                        invalid_reason: Some(reason.into()),
                        payer: None,
                    })
                }
            };
            let payer = Some(format!("0x{}", hex::encode(auth.from)));
            // OPS-L5-F1: a payer with code is checked by its own code.
            match self.has_code(&auth.from).await {
                Ok(true) => {
                    return Ok(VerifyResponse {
                        is_valid: false,
                        invalid_reason: Some("invalid_exact_evm_payload_signature".into()),
                        payer,
                    })
                }
                Ok(false) => {}
                Err(e) => return Err(FacilitatorError::Unavailable(e)),
            }
            // The token itself judges the authorization: anyone may submit it,
            // so an `eth_call` from the operator is the real settle minus the
            // credit (x402 exact EVM verify step 5).
            let sim = self
                .sender
                .call(
                    &self.token,
                    &transfer_with_authorization_calldata(&auth, &sig),
                )
                .await;
            let invalid_reason = match sim {
                Ok(_) => None,
                // A transport error says nothing about the payment
                // (M8-L58-F4).
                Err(e) if !crate::evm_tx::is_revert(&e) => {
                    return Err(FacilitatorError::Unavailable(e))
                }
                Err(_) => match self.diagnose(&auth).await {
                    Ok(Failed::Used) => Some("invalid_transaction_state"),
                    Ok(Failed::Refused(r)) => Some(r),
                    Err(e) => return Err(FacilitatorError::Unavailable(e)),
                },
            };
            Ok(VerifyResponse {
                is_valid: invalid_reason.is_none(),
                invalid_reason: invalid_reason.map(Into::into),
                payer,
            })
        })
    }

    /// Without the shard there is nothing to credit: refused. The server
    /// always calls [`Facilitator::settle_for`].
    fn settle<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        let r = self.refused("invalid_payment_requirements", None);
        let _ = req;
        Box::pin(async move { Ok(r) })
    }

    fn settle_for<'a>(
        &'a self,
        req: &'a FacilitatorRequest,
        bill: (u64, Hash32),
    ) -> BoxFuture<'a, Result<SettleResponse, FacilitatorError>> {
        Box::pin(self.settle_shard(req, bill))
    }

    fn credits_on_settle(&self) -> bool {
        true
    }

    fn settle_budget(&self) -> Option<Duration> {
        Some(self.deadline + SETTLE_RPC_SLACK)
    }

    /// Both of this facilitator's journals: its pending settles and its
    /// receipt log (REV-L4-F6), so a receipt write error stops new paid
    /// requests even when the server does not share this log.
    fn state_write_errors(&self) -> u64 {
        self.pending_write_errors() + self.receipts.write_errors()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arrival::{authorization_used_topic, transfer_topic};

    /// The calldata layout matches `cast calldata` (Foundry 1.8.5) for the
    /// same arguments:
    /// `cast calldata "settle(uint64,bytes32,(address,uint256,uint256,uint256,bytes32),bytes)"
    /// 5 0x…07 "(0x…aa,2500,0,1000,0x…09)" 0x0102`.
    #[test]
    fn settle_calldata_matches_cast() {
        let mut shard = [0u8; 32];
        shard[31] = 7;
        let mut from = [0u8; 20];
        from[19] = 0xaa;
        let mut nonce = [0u8; 32];
        nonce[31] = 9;
        let a = exact_evm::Authorization {
            from,
            to: [0u8; 20],
            value: 2_500,
            valid_after: 0,
            valid_before: 1_000,
            nonce,
        };
        let d = settle_calldata(5, &Hash32(shard), &a, &[1, 2]);
        assert_eq!(hex::encode(d), CAST_SETTLE);
    }

    use crate::x402::{PaymentPayload, PaymentTerms, BASE_SEPOLIA};
    use serde_json::{json, Value};
    use std::sync::Mutex;

    const USDC: [u8; 20] = [0x36; 20];
    const REVENUE: [u8; 20] = [0x5e; 20];
    /// Public test keys; they hold nothing.
    const OPERATOR: [u8; 32] = [0x11; 32];
    const PAYER: [u8; 32] = [0x22; 32];

    /// A node whose settle behaviour each test picks.
    #[derive(Default)]
    struct Node {
        /// `eth_estimateGas` of a settle fails with this error.
        estimate_err: Option<&'static str>,
        /// `authorizationState` answers true.
        used: bool,
        /// The token refuses the authorization (signature, window).
        bad_sig: bool,
        /// The token's logs show the authorization moved the USDC into
        /// OnyxRevenue (else it went elsewhere).
        arrived: bool,
        /// What `eth_getTransactionReceipt` answers: None = not mined.
        receipt_status: Mutex<Option<&'static str>>,
        sent: Mutex<Vec<String>>,
        /// An earlier transaction of the key is unmined (`latest` nonce 2,
        /// `pending` 3).
        gap: bool,
        /// The nonce topic of the last `eth_getLogs` (its use's receipt
        /// names it in AuthorizationUsed).
        nonce_topic: Mutex<String>,
        /// The payer's address carries code (an EIP-7702 delegation or an
        /// ERC-1271 wallet), from now on.
        payer_code: Mutex<bool>,
        /// `eth_getCode` calls answered.
        code_reads: Mutex<u32>,
        /// The payer's code appears after this many `eth_getCode` reads (a
        /// delegation set between our checks).
        delegate_after: Option<u32>,
    }

    fn topic(b: &[u8]) -> String {
        format!("0x{}", hex::encode(b))
    }

    impl JsonRpc for Node {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            let payer = exact_evm::address(&PAYER).expect("key");
            let r = match method {
                "eth_estimateGas" => match self.estimate_err {
                    Some(e) => Err(e.to_string()),
                    None => Ok(json!("0x30000")),
                },
                "eth_getTransactionCount" if self.gap && params[1] == "latest" => Ok(json!("0x2")),
                "eth_getTransactionCount" => Ok(json!("0x3")),
                // Block n at `now - 2 (head - n)`; head 0x100000.
                "eth_getBlockByNumber" => {
                    let head = 0x10_0000u64;
                    let n = params[0]
                        .as_str()
                        .and_then(|t| u64::from_str_radix(t.trim_start_matches("0x"), 16).ok())
                        .unwrap_or(head);
                    let ts = now_unix().saturating_sub(2 * head.saturating_sub(n));
                    Ok(json!({"baseFeePerGas": "0x3b9aca00", "timestamp": format!("0x{ts:x}")}))
                }
                "eth_maxPriorityFeePerGas" => Ok(json!("0xf4240")),
                "eth_blockNumber" => Ok(json!("0x100000")),
                "eth_getCode" => {
                    let reads = {
                        let mut c = self.code_reads.lock().expect("lock");
                        *c += 1;
                        *c
                    };
                    let delegated = (*self.payer_code.lock().expect("lock")
                        || self.delegate_after.is_some_and(|n| reads > n))
                        && params[0] == topic(&payer);
                    Ok(json!(if delegated {
                        "0xef0100aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    } else {
                        "0x"
                    }))
                }
                "eth_sendRawTransaction" => {
                    self.sent.lock().expect("lock").push(params[0].to_string());
                    Ok(json!("0xhash"))
                }
                "eth_getLogs" if self.used => {
                    *self.nonce_topic.lock().expect("lock") = params[0]["topics"][2]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    Ok(json!([{"transactionHash": "0xproof"}]))
                }
                "eth_getLogs" => Ok(json!([])),
                "eth_getTransactionReceipt" if params[0] == "0xproof" => {
                    let to = if self.arrived { REVENUE } else { [0xee; 20] };
                    let nonce = self.nonce_topic.lock().expect("lock").clone();
                    // As FiatToken emits them: AuthorizationUsed, then its
                    // Transfer (OPS-L3-F2).
                    Ok(json!({"status": "0x1", "logs": [{
                        "address": topic(&USDC),
                        "topics": [topic(&authorization_used_topic()), topic(&word_addr(&payer)), nonce],
                        "data": "0x",
                    }, {
                        "address": topic(&USDC),
                        "topics": [topic(&transfer_topic()), topic(&word_addr(&payer)), topic(&word_addr(&to))],
                        "data": topic(&word_u128(2_500)),
                    }]}))
                }
                "eth_getTransactionReceipt" => match *self.receipt_status.lock().expect("lock") {
                    Some(st) => Ok(json!({
                        "status": st, "blockNumber": "0x10", "gasUsed": "0x1d4c0",
                        "effectiveGasPrice": "0x3b9aca00"
                    })),
                    None => Ok(Value::Null),
                },
                "eth_call" => {
                    let data = params[0]["data"].as_str().unwrap_or_default().to_string();
                    let sel = |sig: &str| format!("0x{}", hex::encode(selector(sig)));
                    if data.starts_with(&sel("authorizationState(address,bytes32)")) {
                        Ok(json!(format!("0x{:064x}", u8::from(self.used))))
                    } else if data.starts_with(&sel("balanceOf(address)")) {
                        Ok(json!(format!("0x{:064x}", 1_000_000u64)))
                    } else if self.used {
                        Err("execution reverted: authorization is used or canceled".into())
                    } else if self.bad_sig {
                        Err("execution reverted: invalid signature".into())
                    } else {
                        Ok(json!("0x"))
                    }
                }
                _ => Err(format!("unexpected {method}")),
            };
            Box::pin(async move { r })
        }
    }

    fn with(n: Node) -> SelfSettle<Node> {
        let sender = Sender::new(n, 84_532, OPERATOR)
            .expect("key")
            .with_poll(Duration::from_millis(5));
        SelfSettle::new(
            sender,
            REVENUE,
            USDC,
            BASE_SEPOLIA,
            Duration::from_millis(40),
        )
    }

    /// A node that mines every settle successfully.
    fn mining() -> Node {
        Node {
            receipt_status: Mutex::new(Some("0x1")),
            ..Node::default()
        }
    }

    const REVERTS: Option<&str> =
        Some("eth_estimateGas: node error: {\"code\":3,\"message\":\"execution reverted\"}");

    fn request(pay_to: [u8; 20]) -> FacilitatorRequest {
        let terms = PaymentTerms {
            network: BASE_SEPOLIA.into(),
            asset: format!("0x{}", hex::encode(USDC)),
            pay_to: format!("0x{}", hex::encode(pay_to)),
            max_timeout_seconds: 60,
            extra: Some(json!({"name": "USDC", "version": "2"})),
        };
        let req = terms.requirements(2_500);
        let auth = exact_evm::Authorization {
            from: exact_evm::address(&PAYER).expect("key"),
            to: pay_to,
            value: 2_500,
            valid_after: 0,
            valid_before: now_unix() + 600,
            nonce: [7u8; 32],
        };
        FacilitatorRequest {
            x402_version: X402_VERSION,
            payment_payload: PaymentPayload {
                x402_version: X402_VERSION,
                resource: None,
                accepted: req.clone(),
                payload: exact_evm::sign(&PAYER, &auth, &req).expect("sign"),
            },
            payment_requirements: req,
        }
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

    #[test]
    fn a_settle_credits_on_chain_and_records_no_receipt() {
        rt().block_on(async {
            let f = with(mining());
            assert!(f.verify(&request(REVENUE)).await.expect("verify").is_valid);
            let s = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("settle");
            assert!(s.success, "{s:?}");
            assert!(f.credits_on_settle());
            assert_eq!(f.receipts().pending(), 0);
            let sent = f.sender.rpc().sent.lock().expect("lock").clone();
            assert_eq!(sent.len(), 1, "one transaction");
            // The hash is computed from the signed bytes, never taken from
            // the node's answer.
            let raw = hex::decode(sent[0].trim_matches('"').trim_start_matches("0x")).expect("hex");
            assert_eq!(
                s.transaction,
                format!("0x{}", hex::encode(keccak256(&raw).0))
            );
        });
    }

    /// Only payments to the revenue contract in the configured token count:
    /// another `payTo` is refused before any chain call.
    #[test]
    fn a_payment_to_another_address_is_refused() {
        rt().block_on(async {
            let f = with(mining());
            let v = f.verify(&request([0xee; 20])).await.expect("verify");
            assert!(!v.is_valid);
            let s = f
                .settle_for(&request([0xee; 20]), (4, Hash32([9; 32])))
                .await
                .expect("settle");
            assert!(!s.success);
            assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
        });
    }

    /// A front-run whose USDC the token's logs show reaching OnyxRevenue is
    /// an unknown outcome: nothing is broadcast, the payment goes to the
    /// receipt path, and verify reports the spent nonce.
    #[test]
    fn a_used_authorization_that_reached_revenue_goes_to_the_receipt_path() {
        rt().block_on(async {
            let f = with(Node {
                estimate_err: REVERTS,
                used: true,
                arrived: true,
                ..mining()
            });
            let v = f.verify(&request(REVENUE)).await.expect("verify");
            assert_eq!(
                v.invalid_reason.as_deref(),
                Some("invalid_transaction_state")
            );
            let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
            assert!(e.is_err(), "unknown outcome, never a success: {e:?}");
            assert_eq!(f.receipts().pending(), 1);
            assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
        });
    }

    /// M8-L58-F3: a nonce the payer spent elsewhere (no Transfer into
    /// OnyxRevenue in the token's logs) queues no receipt: it could steer
    /// unallocated funds. It is the payer's refusal.
    #[test]
    fn a_nonce_spent_elsewhere_queues_no_receipt() {
        rt().block_on(async {
            let f = with(Node {
                estimate_err: REVERTS,
                used: true,
                arrived: false,
                ..mining()
            });
            let s = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("a refusal, not an unknown outcome");
            assert!(!s.success);
            assert_eq!(s.error_reason.as_deref(), Some("invalid_transaction_state"));
            assert_eq!(f.receipts().pending(), 0);
        });
    }

    /// OPS-L5-F1: a payer whose address carries code (an EIP-7702
    /// delegation, an ERC-1271 wallet) is refused at verify and at settle
    /// before anything is sent: its own code would judge the signature, and a
    /// gas-price-0 simulation says nothing about the real transaction.
    #[test]
    fn a_payer_with_code_is_refused_before_anything_is_sent() {
        rt().block_on(async {
            let f = with(Node {
                payer_code: Mutex::new(true),
                ..mining()
            });
            let v = f.verify(&request(REVENUE)).await.expect("verify");
            assert!(!v.is_valid);
            assert_eq!(
                v.invalid_reason.as_deref(),
                Some("invalid_exact_evm_payload_signature")
            );
            let s = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("a refusal, not an unknown outcome");
            assert!(!s.success);
            assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
        });
    }

    /// OPS-L5-F1 (review 5, shown on an anvil fork): code that answers
    /// "valid" to a gas-price-0 call and "invalid" in a real transaction made
    /// the settle revert while the server-fault simulation passed, so the
    /// payer's own revert was reported as ours (503, an unknown settle). A
    /// delegation set after the first check is now the payer's refusal.
    #[test]
    fn a_settle_reverting_on_payer_code_is_the_payers_refusal() {
        rt().block_on(async {
            let f = with(Node {
                estimate_err: REVERTS,
                delegate_after: Some(1),
                ..mining()
            });
            let s = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("the payer's refusal, never a server fault");
            assert!(!s.success);
            assert_eq!(
                s.error_reason.as_deref(),
                Some("invalid_exact_evm_payload_signature")
            );
            assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
            assert_eq!(f.receipts().pending(), 0);
        });
    }

    /// Without payer code the server-fault path is unchanged: the token
    /// would take the payment, so a reverting settle is ours (M8-L58-F4).
    #[test]
    fn a_settle_reverting_for_a_server_reason_is_still_unavailable() {
        rt().block_on(async {
            let f = with(Node {
                estimate_err: REVERTS,
                ..mining()
            });
            let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
            assert!(matches!(e, Err(FacilitatorError::Unavailable(_))), "{e:?}");
        });
    }

    /// A settle that reverts because the token refuses the authorization is
    /// the payer's refusal.
    #[test]
    fn a_bad_authorization_is_refused() {
        rt().block_on(async {
            let f = with(Node {
                estimate_err: REVERTS,
                bad_sig: true,
                ..mining()
            });
            let s = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("settle");
            assert!(!s.success);
            assert_eq!(f.receipts().pending(), 0);
        });
    }

    /// M8-L58-F4: a settle that reverts while the token would take the
    /// payment (reporter rotated, shard not registered), or a gas estimate
    /// that fails for a transport reason, is never blamed on the payer.
    #[test]
    fn server_side_failures_are_never_the_payers_fault() {
        rt().block_on(async {
            for err in [
                REVERTS,
                Some("eth_estimateGas: http status 429"),
                Some("eth_estimateGas: node error: {\"code\":-32000,\"message\":\"insufficient funds for gas * price + value\"}"),
            ] {
                let f = with(Node {
                    estimate_err: err,
                    ..mining()
                });
                let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
                assert!(
                    matches!(e, Err(FacilitatorError::Unavailable(_))),
                    "{err:?}: {e:?}"
                );
                assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
                assert_eq!(f.receipts().pending(), 0);
            }
        });
    }

    /// Broadcast but not mined before the deadline: unknown, kept as
    /// pending. Once it mines, it credited its shard: no receipt.
    #[test]
    fn an_unmined_settle_is_pending_until_it_mines() {
        rt().block_on(async {
            let f = with(Node::default());
            let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
            assert!(e.is_err());
            assert_eq!(f.pending_settles(), 1);
            assert_eq!(f.receipts().pending(), 0);
            *f.sender.rpc().receipt_status.lock().expect("lock") = Some("0x1");
            assert_eq!(f.resolve_pending().await, 0);
            assert_eq!(f.pending_settles(), 0);
            assert_eq!(f.receipts().pending(), 0);
        });
    }

    /// OPS-L1-F1: a self-settle running in a tracked task (as the server's
    /// `finish` runs it) reaches its pending entry before a stop that waits
    /// on `wait_idle` lets the runtime go.
    #[test]
    fn a_tracked_self_settle_writes_its_pending_entry_before_the_stop() {
        rt().block_on(async {
            let f = Arc::new(with(Node::default()));
            let tasks = Arc::new(crate::SettleTasks::default());
            let running = tasks.enter();
            let g = f.clone();
            let task = tokio::spawn(async move {
                let _running = running;
                g.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await
            });
            assert_eq!(tasks.live(), 1);
            assert!(tasks.wait_idle(Duration::from_secs(5)).await);
            assert!(task.await.expect("task").is_err(), "not mined: unknown");
            assert_eq!(f.pending_settles(), 1, "pending entry written");
        });
    }

    /// A pending settle that later reverts (a front-run landed first) is
    /// queued as a receipt only when the token's logs prove the USDC reached
    /// OnyxRevenue (M8-L58-F2, F3).
    #[test]
    fn a_pending_settle_that_reverts_needs_proof_of_arrival() {
        rt().block_on(async {
            for (arrived, queued) in [(true, 1), (false, 0)] {
                let f = with(Node {
                    used: true,
                    arrived,
                    ..Node::default()
                });
                let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
                assert!(e.is_err());
                *f.sender.rpc().receipt_status.lock().expect("lock") = Some("0x0");
                assert_eq!(f.resolve_pending().await, queued);
                assert_eq!(f.receipts().pending(), queued);
                assert_eq!(f.pending_settles(), 0);
            }
        });
    }

    /// REV-L4-F1: while an earlier transaction of the operator key has been
    /// unmined past the gap limit, a settle sends nothing (the payer would
    /// be charged behind it) and says the server is unavailable; a gap
    /// younger than the limit does not stop it.
    #[test]
    fn a_settle_behind_a_stuck_transaction_sends_nothing() {
        rt().block_on(async {
            let stuck = || Node {
                gap: true,
                receipt_status: Mutex::new(Some("0x1")),
                ..Node::default()
            };
            let sender = Sender::new(stuck(), 84_532, OPERATOR)
                .expect("key")
                .with_poll(Duration::from_millis(5))
                .with_gap_limit(Duration::ZERO);
            let f = SelfSettle::new(
                sender,
                REVENUE,
                USDC,
                BASE_SEPOLIA,
                Duration::from_millis(40),
            );
            let e = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect_err("stuck");
            assert!(
                matches!(&e, FacilitatorError::Unavailable(m) if m.contains("stuck at nonce 2")),
                "{e:?}"
            );
            assert!(
                f.sender.rpc().sent.lock().expect("lock").is_empty(),
                "nothing sent"
            );
            // A fresh gap (younger than the default limit) is a normal
            // transaction about to mine: the settle goes out.
            let f = with(stuck());
            let r = f
                .settle_for(&request(REVENUE), (4, Hash32([9; 32])))
                .await
                .expect("settled");
            assert!(r.success);
            assert!(f.shared_sender().gap_since().is_some());
        });
    }

    /// REV-L4-F6: a write error of the facilitator's own receipt log counts
    /// in its state write errors, so new paid requests stop even when the
    /// server keeps another log.
    #[test]
    fn a_receipt_log_write_error_counts_for_the_facilitator() {
        let d = crate::journal::test_dir("settle-receipt-errors");
        let f = with(mining()).persist(&d).expect("persist");
        assert_eq!(Facilitator::state_write_errors(&f), 0);
        f.receipts().break_journal_for_test();
        f.receipts().record(Receipt {
            round: 1,
            shard: Hash32([1; 32]),
            amount: 2_500,
            payer: [0xaa; 20],
            nonce: [1; 32],
        });
        assert_eq!(f.pending_write_errors(), 0);
        assert_eq!(Facilitator::state_write_errors(&f), 1);
        drop(f);
        std::fs::remove_dir_all(d).ok();
    }

    /// REV-11: a settle broadcast just before a crash is still resolved after
    /// the restart, and its receipt reaches the log exactly once, on disk.
    #[test]
    fn a_pending_settle_survives_a_restart_and_is_resolved_once() {
        rt().block_on(async {
            let d = crate::journal::test_dir("pending-settles");
            let used = || Node {
                used: true,
                arrived: true,
                ..Node::default()
            };
            {
                let f = with(used()).persist(&d).expect("persist");
                let e = f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
                assert!(e.is_err(), "not mined before the deadline");
                assert_eq!(f.pending_settles(), 1);
                // Killed here.
            }
            {
                let f = with(used()).persist(&d).expect("restart");
                assert_eq!(f.pending_settles(), 1, "the pending settle came back");
                assert_eq!(f.receipts().pending(), 0);
                // The settle reverted (a third party used the authorization);
                // the token's logs show the USDC reached OnyxRevenue.
                *f.sender.rpc().receipt_status.lock().expect("lock") = Some("0x0");
                assert_eq!(f.resolve_pending().await, 1);
                assert_eq!(f.pending_settles(), 0);
                assert_eq!(f.pending_write_errors(), 0);
            }
            let f = with(used()).persist(&d).expect("again");
            assert_eq!(f.pending_settles(), 0, "resolved for good");
            let got = f.receipts().peek(9);
            assert_eq!(got.len(), 1, "one receipt, on disk");
            assert_eq!(
                (got[0].round, got[0].shard, got[0].amount),
                (4, Hash32([9; 32]), 2_500)
            );
            std::fs::remove_dir_all(d).ok();
        });
    }

    /// M8-L58-F2 (review case): the server waits at least the settle's own
    /// budget, so a broadcast settle always reaches its pending handling;
    /// one that cannot get the key in time sends nothing.
    #[test]
    fn the_server_waits_out_the_settle_and_a_busy_key_sends_nothing() {
        rt().block_on(async {
            let f = with(Node::default());
            let budget = f.settle_budget().expect("a self-settle has a budget");
            assert!(budget > f.deadline);
            let out = tokio::time::timeout(
                budget,
                f.settle_for(&request(REVENUE), (4, Hash32([9; 32]))),
            )
            .await;
            assert!(out.is_ok(), "the settle ended within its budget");
            assert_eq!(f.sender.rpc().sent.lock().expect("lock").len(), 1);
            assert_eq!(f.pending_settles(), 1, "the broadcast settle is kept");

            let g = with(mining());
            let _held = g.sender.lock_for_test().await;
            let e = g.settle_for(&request(REVENUE), (4, Hash32([9; 32]))).await;
            assert!(matches!(e, Err(FacilitatorError::Unavailable(_))), "{e:?}");
            assert!(g.sender.rpc().sent.lock().expect("lock").is_empty());
        });
    }

    /// The plain `settle` (no shard) never moves money.
    #[test]
    fn settle_without_a_shard_is_refused() {
        rt().block_on(async {
            let f = with(mining());
            let s = f.settle(&request(REVENUE)).await.expect("settle");
            assert!(!s.success);
            assert!(f.sender.rpc().sent.lock().expect("lock").is_empty());
        });
    }

    const CAST_SETTLE: &str = "92b4a0d40000000000000000000000000000000000000000000000000000000000000005000000000000000000000000000000000000000000000000000000000000000700000000000000000000000000000000000000000000000000000000000000aa00000000000000000000000000000000000000000000000000000000000009c4000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003e80000000000000000000000000000000000000000000000000000000000000009000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000020102000000000000000000000000000000000000000000000000000000000000";
}
