//! EIP-1559 transactions for the self-settle path (ADR-0026): RLP encoding,
//! secp256k1 signing, and a send-and-wait over [`JsonRpc`]. The operator key
//! signs in this process and is never written, logged or put on a command
//! line; [`Sender`]'s `Debug` prints only the address.

use crate::roots::JsonRpc;
use onyx_core::keccak256;
use serde_json::{json, Value};
use std::time::Duration;

/// A type-2 (EIP-1559) transaction with an empty access list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx1559 {
    /// EIP-155 chain id.
    pub chain_id: u64,
    /// Sender nonce.
    pub nonce: u64,
    /// Tip per gas, in wei.
    pub max_priority_fee_per_gas: u128,
    /// Fee cap per gas, in wei.
    pub max_fee_per_gas: u128,
    /// Gas limit.
    pub gas: u64,
    /// Recipient (never a contract creation here).
    pub to: [u8; 20],
    /// Value in wei.
    pub value: u128,
    /// Calldata.
    pub data: Vec<u8>,
}

fn rlp_bytes(b: &[u8]) -> Vec<u8> {
    if b.len() == 1 && b[0] < 0x80 {
        return b.to_vec();
    }
    let mut out = rlp_len(b.len(), 0x80);
    out.extend_from_slice(b);
    out
}

fn rlp_len(len: usize, offset: u8) -> Vec<u8> {
    if len <= 55 {
        // `len` fits: at most 55 plus an offset of at most 0xc0.
        return vec![offset + len as u8];
    }
    let be = len.to_be_bytes();
    let first = be.iter().position(|b| *b != 0).unwrap_or(be.len() - 1);
    let mut out = vec![offset + 55 + (be.len() - first) as u8];
    out.extend_from_slice(&be[first..]);
    out
}

fn rlp_uint(v: u128) -> Vec<u8> {
    let be = v.to_be_bytes();
    let first = be.iter().position(|b| *b != 0).unwrap_or(be.len());
    rlp_bytes(&be[first..])
}

fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.concat();
    let mut out = rlp_len(payload.len(), 0xc0);
    out.extend_from_slice(&payload);
    out
}

impl Tx1559 {
    fn fields(&self) -> Vec<Vec<u8>> {
        vec![
            rlp_uint(u128::from(self.chain_id)),
            rlp_uint(u128::from(self.nonce)),
            rlp_uint(self.max_priority_fee_per_gas),
            rlp_uint(self.max_fee_per_gas),
            rlp_uint(u128::from(self.gas)),
            rlp_bytes(&self.to),
            rlp_uint(self.value),
            rlp_bytes(&self.data),
            rlp_list(&[]),
        ]
    }

    /// The hash the sender signs: `keccak256(0x02 || rlp(fields))`.
    pub fn signing_hash(&self) -> [u8; 32] {
        let mut m = vec![0x02];
        m.extend(rlp_list(&self.fields()));
        keccak256(&m).0
    }

    /// Sign with `secret`; returns the raw transaction and its hash.
    pub fn sign(&self, secret: &[u8; 32]) -> Option<(Vec<u8>, [u8; 32])> {
        let key = secp256k1::SecretKey::from_slice(secret).ok()?;
        let msg = secp256k1::Message::from_digest(self.signing_hash());
        let (id, rs) = secp256k1::Secp256k1::signing_only()
            .sign_ecdsa_recoverable(&msg, &key)
            .serialize_compact();
        let parity = u128::try_from(id.to_i32()).ok()?;
        let mut f = self.fields();
        f.push(rlp_uint(parity));
        f.push(rlp_bytes(trim(&rs[..32])));
        f.push(rlp_bytes(trim(&rs[32..])));
        let mut raw = vec![0x02];
        raw.extend(rlp_list(&f));
        let hash = keccak256(&raw).0;
        Some((raw, hash))
    }
}

/// Big-endian integer bytes without leading zeros (RLP integer form).
fn trim(b: &[u8]) -> &[u8] {
    let first = b.iter().position(|x| *x != 0).unwrap_or(b.len());
    &b[first..]
}

/// The outcome of a mined transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mined {
    /// Transaction hash.
    pub hash: String,
    /// Block number.
    pub block: u64,
    /// Gas used.
    pub gas_used: u64,
    /// Wei paid per gas.
    pub effective_gas_price: u128,
}

/// Why a send did not end in a successful receipt.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The node's gas estimate reverted: nothing was broadcast.
    #[error("would revert, not sent: {0}")]
    WouldRevert(String),
    /// Mined, but reverted.
    #[error("transaction {0} reverted")]
    Reverted(String),
    /// Broadcast, no receipt within the deadline: the outcome is unknown.
    #[error("transaction {0} not mined before the deadline")]
    Pending(String),
    /// The node could not be reached or answered garbage (also a gas
    /// estimate that failed for a reason other than a revert, such as a rate
    /// limit or the sender lacking gas money: never the payer's fault,
    /// M8-L58-F4). Nothing is known to have been broadcast.
    #[error("rpc: {0}")]
    Rpc(String),
    /// Another send held the key past half the deadline: nothing was
    /// broadcast (M8-L58-F2).
    #[error("sender busy, not sent")]
    Busy,
    /// An earlier transaction from this key has stayed unmined (the
    /// `latest` nonce behind `pending`) for longer than the sender's gap
    /// limit: a new one would only queue behind it, so nothing was sent
    /// (REV-L4-F1). Carries the lowest unmined nonce.
    #[error("an earlier transaction (nonce {0}) is stuck unmined; not sent")]
    Stuck(u64),
}

/// Whether a node error is a revert (the call itself fails), as opposed to a
/// transport, rate-limit or funding problem. Nodes answer a revert with
/// JSON-RPC code 3 or an "execution reverted" message (M8-L58-F4).
pub fn is_revert(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("revert") || l.contains("\"code\":3,") || l.contains("\"code\":3}")
}

/// A node's answer for a block it does not have yet (a load-balanced
/// backend behind the head). Nethermind says "Block 123 could not be found"
/// (review LAUNCH-R1-A2).
fn is_missing_block(e: &str) -> bool {
    let e = e.to_lowercase();
    [
        "block not found",
        "unknown block",
        "header not found",
        "could not be found",
    ]
    .iter()
    .any(|m| e.contains(m))
}

/// A node that refuses the request's shape, here the block parameter of
/// `eth_estimateGas`, which some providers do not take (JSON-RPC -32602,
/// "invalid params"; review LAUNCH-R1-A2).
fn is_invalid_params(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("-32602") || l.contains("invalid params") || l.contains("invalid argument")
}

/// Signs and sends transactions from one key, one at a time (the nonce is
/// read under a lock, so concurrent settles never share a nonce).
pub struct Sender<T: JsonRpc> {
    rpc: T,
    chain_id: u64,
    secret: [u8; 32],
    address: [u8; 20],
    lock: tokio::sync::Mutex<()>,
    poll: Duration,
    /// How long the key may have an unmined transaction ahead of a new one
    /// before new sends are refused ([`SendError::Stuck`], REV-L4-F1).
    gap_limit: Duration,
    /// Since when the `latest` nonce has been behind `pending`.
    gap_since: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Ceiling on `max_fee_per_gas` for every send (AUD-1-F3): fees come
    /// from the node, which must not be able to make the key overpay.
    max_fee_cap: u128,
    /// Ceiling on the gas limit of every send (AUD-1-F3).
    max_gas: u64,
    /// Block of this key's last mined transaction. A load-balanced node can
    /// estimate against a block before it, where a call that skips unknown
    /// entries looks cheaper than it is: the estimate then runs out of gas on
    /// chain. Estimates name a block at or after this one.
    seen_block: std::sync::atomic::AtomicU64,
}

/// Default [`Sender::with_fee_limits`] fee cap: 10 gwei, far above Base
/// Sepolia's usual fees (well under 0.1 gwei) yet bounding what a node that
/// lies about fees can make one transaction cost.
pub const DEFAULT_MAX_FEE_PER_GAS: u128 = 10_000_000_000;
/// Default [`Sender::with_fee_limits`] gas cap: no Onyx call needs more.
pub const DEFAULT_MAX_GAS: u64 = 15_000_000;

/// Default [`Sender::with_gap_limit`]: a normal transaction mines within
/// seconds on Base Sepolia; a minute behind means it is stuck.
pub const DEFAULT_GAP_LIMIT: Duration = Duration::from_secs(60);
/// Tries of `eth_blockNumber` before a pinned estimate gives up
/// (REVLOG-R13-4).
pub const HEAD_READ_TRIES: u32 = 3;

impl<T: JsonRpc> std::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("chain_id", &self.chain_id)
            .field("address", &format!("0x{}", hex::encode(self.address)))
            .finish_non_exhaustive()
    }
}

fn quantity(v: &Value) -> Result<u128, SendError> {
    let s = v
        .as_str()
        .ok_or_else(|| SendError::Rpc(format!("not a quantity: {v}")))?;
    u128::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| SendError::Rpc(format!("bad quantity {s}: {e}")))
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

impl<T: JsonRpc> Sender<T> {
    /// A sender for `secret` on `chain_id`. `None` if the key is invalid.
    pub fn new(rpc: T, chain_id: u64, secret: [u8; 32]) -> Option<Self> {
        let address = crate::x402::exact_evm::address(&secret)?;
        Some(Sender {
            rpc,
            chain_id,
            secret,
            address,
            lock: tokio::sync::Mutex::new(()),
            poll: Duration::from_millis(500),
            gap_limit: DEFAULT_GAP_LIMIT,
            gap_since: std::sync::Mutex::new(None),
            max_fee_cap: DEFAULT_MAX_FEE_PER_GAS,
            max_gas: DEFAULT_MAX_GAS,
            seen_block: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Fee and gas ceilings for every send (AUD-1-F3): the fee cap bounds
    /// `max_fee_per_gas` (the tip is clamped under it; a base fee alone above
    /// it refuses the send), and an estimate above `max_gas` refuses it.
    /// Defaults [`DEFAULT_MAX_FEE_PER_GAS`] and [`DEFAULT_MAX_GAS`].
    pub fn with_fee_limits(mut self, max_fee_cap: u128, max_gas: u64) -> Self {
        self.max_fee_cap = max_fee_cap.max(1);
        self.max_gas = max_gas.max(21_000);
        self
    }

    /// Refuse new sends ([`SendError::Stuck`]) while an earlier transaction
    /// from this key has been unmined for longer than `limit`: a settle or
    /// `credit` sent behind it would wait with it, so a payer would be
    /// charged late or never get the data (REV-L4-F1). Default
    /// [`DEFAULT_GAP_LIMIT`].
    pub fn with_gap_limit(mut self, limit: Duration) -> Self {
        self.gap_limit = limit;
        self
    }

    /// Since when an unmined transaction has held up this key (the `latest`
    /// nonce behind `pending`), as last seen by a send; `None` when no gap
    /// was seen.
    pub fn gap_since(&self) -> Option<tokio::time::Instant> {
        self.gap_since.lock().ok().and_then(|g| *g)
    }

    /// Receipt polling interval (default 500 ms).
    pub fn with_poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    /// The sending address.
    pub fn address(&self) -> [u8; 20] {
        self.address
    }

    /// Hold the sending lock (tests: a concurrent settle in progress).
    #[cfg(test)]
    pub(crate) async fn lock_for_test(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// The node this sender talks to (for read-only calls).
    pub fn rpc(&self) -> &T {
        &self.rpc
    }

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, SendError> {
        self.rpc.call(method, params).await.map_err(SendError::Rpc)
    }

    /// `eth_call` from this sender at `latest`; `Err` carries a revert.
    pub async fn call(&self, to: &[u8; 20], data: &[u8]) -> Result<Vec<u8>, String> {
        let r = self
            .rpc
            .call(
                "eth_call",
                json!([{"from": hex0x(&self.address), "to": hex0x(to), "data": hex0x(data)}, "latest"]),
            )
            .await?;
        hex::decode(r.as_str().unwrap_or_default().trim_start_matches("0x"))
            .map_err(|e| e.to_string())
    }

    /// The chain this sender signs for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Estimate, sign, send and wait for the receipt. `deadline` bounds the
    /// whole call from its start: waiting for the key (at most half of it,
    /// else [`SendError::Busy`] with nothing sent) and polling for the
    /// receipt (M8-L58-F2). The key is held only until the node accepted the
    /// transaction, never while waiting for it to be mined (REV-L3-F8): the
    /// next send reads the `pending` nonce, which counts this one.
    pub async fn send(
        &self,
        to: &[u8; 20],
        data: &[u8],
        value: u128,
        deadline: Duration,
    ) -> Result<Mined, SendError> {
        let start = tokio::time::Instant::now();
        let sent = self.broadcast(to, data, value, deadline / 2).await?;
        let left = deadline.saturating_sub(start.elapsed());
        match self
            .wait_mined(std::slice::from_ref(&sent.hash), left)
            .await?
        {
            Some((_, m)) => Ok(m),
            None => Err(SendError::Pending(sent.hash)),
        }
    }

    /// Estimate, sign and send; returns once the node accepted the
    /// transaction. Waits at most `lock_wait` for the key
    /// ([`SendError::Busy`] with nothing sent).
    pub async fn broadcast(
        &self,
        to: &[u8; 20],
        data: &[u8],
        value: u128,
        lock_wait: Duration,
    ) -> Result<Sent, SendError> {
        let _one_at_a_time = tokio::time::timeout(lock_wait, self.lock.lock())
            .await
            .map_err(|_| SendError::Busy)?;
        let from = hex0x(&self.address);
        let call = json!({
            "from": from, "to": hex0x(to), "data": hex0x(data), "value": format!("0x{value:x}")
        });
        let seen = self.seen_block.load(std::sync::atomic::Ordering::Relaxed);
        let gas = self.estimate(&call, seen).await?;
        let nonce = quantity(
            &self
                .rpc_call("eth_getTransactionCount", json!([from, "pending"]))
                .await?,
        )?;
        let mined = quantity(
            &self
                .rpc_call("eth_getTransactionCount", json!([from, "latest"]))
                .await?,
        )?;
        self.check_gap(mined, nonce)?;
        let block = self
            .rpc_call("eth_getBlockByNumber", json!(["latest", false]))
            .await?;
        let base = quantity(&block["baseFeePerGas"])?;
        let tip = match self.rpc.call("eth_maxPriorityFeePerGas", json!([])).await {
            Ok(t) => quantity(&t)?,
            Err(_) => 1_000_000,
        };
        // Fees and gas come from the node; never let it make this key
        // overpay (AUD-1-F3).
        if base >= self.max_fee_cap {
            return Err(SendError::Rpc(format!(
                "base fee {base} is at or above the fee cap {}",
                self.max_fee_cap
            )));
        }
        let max_fee = base
            .saturating_mul(2)
            .saturating_add(tip)
            .min(self.max_fee_cap);
        // 25% headroom over the estimate.
        let gas_limit = gas.saturating_mul(5) / 4;
        if gas_limit > u128::from(self.max_gas) {
            return Err(SendError::Rpc(format!(
                "gas estimate {gas} is above the gas cap {}",
                self.max_gas
            )));
        }
        let tx = Tx1559 {
            chain_id: self.chain_id,
            nonce: u64::try_from(nonce).map_err(|e| SendError::Rpc(e.to_string()))?,
            max_priority_fee_per_gas: tip.min(max_fee),
            max_fee_per_gas: max_fee,
            gas: u64::try_from(gas_limit).map_err(|e| SendError::Rpc(e.to_string()))?,
            to: *to,
            value,
            data: data.to_vec(),
        };
        self.sign_and_send(tx).await
    }

    /// `eth_estimateGas` for `call`, pinned at or after `seen` (the block of
    /// this key's last mined transaction) once there is one.
    ///
    /// - A node that does not have the named block yet answers "block not
    ///   found" (never an old estimate): it is asked again, 20 times at most.
    ///   After that the pin is lowered to the head the node reported (review
    ///   LAUNCH-R1-A1): one receipt naming a block the node never reaches
    ///   (a lying or reorged backend) must not cost every later send 20
    ///   polls. This send still fails; the next one pins at that head.
    /// - A node that refuses the block parameter itself (-32602, "invalid
    ///   params") is asked once without it (review LAUNCH-R1-A2).
    /// - `eth_blockNumber` is asked up to [`HEAD_READ_TRIES`] times, a poll
    ///   apart (review REVLOG-R13-4: self-settle does not retry a failed
    ///   send, so one flaky read failed a paid request). If every try fails,
    ///   the send fails ([`SendError::Rpc`], nothing sent; review
    ///   LAUNCH-R1-A3). Pinning at `seen` alone could name a block older
    ///   than the head, and an unpinned estimate could come from a backend
    ///   behind this key's own last transaction; both price a stale state
    ///   (the out-of-gas case the pin exists for).
    async fn estimate(&self, call: &Value, seen: u64) -> Result<u128, SendError> {
        let mut pin = seen != 0;
        let mut tries = 0;
        loop {
            tries += 1;
            let mut head = 0;
            let params = if pin {
                head = self.head_for_estimate(seen).await?;
                json!([call, format!("0x{:x}", head.max(seen))])
            } else {
                json!([call])
            };
            match self.rpc.call("eth_estimateGas", params).await {
                Ok(g) => return quantity(&g),
                Err(e) if is_revert(&e) => return Err(SendError::WouldRevert(e)),
                Err(e) if is_missing_block(&e) && tries < 20 => {
                    tokio::time::sleep(self.poll).await;
                }
                Err(e) if pin && is_missing_block(&e) => {
                    if head < seen {
                        // Only if no newer receipt moved it meanwhile.
                        let _ = self.seen_block.compare_exchange(
                            seen,
                            head,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    return Err(SendError::Rpc(format!(
                        "{e} (block {seen} still missing after {tries} tries; later estimates name the node's head {head})"
                    )));
                }
                // A missing block may also carry -32602: told apart first.
                Err(e) if pin && is_invalid_params(&e) => pin = false,
                Err(e) => return Err(SendError::Rpc(e)),
            }
        }
    }

    /// The node's head for a pinned estimate: [`HEAD_READ_TRIES`] tries, a
    /// poll apart (REVLOG-R13-4); a bad answer is not retried.
    async fn head_for_estimate(&self, seen: u64) -> Result<u64, SendError> {
        let mut last = String::new();
        for n in 0..HEAD_READ_TRIES {
            if n > 0 {
                tokio::time::sleep(self.poll).await;
            }
            match self.rpc.call("eth_blockNumber", json!([])).await {
                Ok(h) => return Ok(u64::try_from(quantity(&h)?).unwrap_or(0)),
                Err(e) => last = e,
            }
        }
        Err(SendError::Rpc(format!(
            "eth_blockNumber failed {HEAD_READ_TRIES} times (the estimate must name block {seen} or later): {last}"
        )))
    }

    /// Replace `prev` (sent, not mined) with the same transaction at the same
    /// nonce and both fees raised by `bump_pct` percent (at least 13%, above
    /// the 10% nodes require of a replacement), the fee cap never above
    /// `max_fee_cap` nor the sender's own fee cap ([`Sender::with_fee_limits`],
    /// AUD-1R-F2): unsticks a transaction priced below the market
    /// (REV-L3-F8). [`SendError::Rpc`] when the cap leaves no room for a
    /// valid bump, or the node refused (e.g. `prev` was mined meanwhile).
    pub async fn replace(
        &self,
        prev: &Sent,
        bump_pct: u32,
        max_fee_cap: u128,
        lock_wait: Duration,
    ) -> Result<Sent, SendError> {
        let _one_at_a_time = tokio::time::timeout(lock_wait, self.lock.lock())
            .await
            .map_err(|_| SendError::Busy)?;
        let max_fee_cap = max_fee_cap.min(self.max_fee_cap);
        let pct = u128::from(bump_pct.max(13));
        let bump = |x: u128| x.saturating_mul(100 + pct) / 100 + 1;
        let max_fee = bump(prev.tx.max_fee_per_gas);
        if max_fee > max_fee_cap {
            return Err(SendError::Rpc(format!(
                "fee cap {max_fee_cap} leaves no room to replace {}",
                prev.hash
            )));
        }
        let tx = Tx1559 {
            max_priority_fee_per_gas: bump(prev.tx.max_priority_fee_per_gas).min(max_fee),
            max_fee_per_gas: max_fee,
            ..prev.tx.clone()
        };
        self.sign_and_send(tx).await
    }

    /// Track how long `latest` has been behind `pending`; refuse once that
    /// gap is older than the limit.
    fn check_gap(&self, latest: u128, pending: u128) -> Result<(), SendError> {
        let Ok(mut g) = self.gap_since.lock() else {
            return Ok(());
        };
        if latest >= pending {
            *g = None;
            return Ok(());
        }
        let since = *g.get_or_insert_with(tokio::time::Instant::now);
        if since.elapsed() >= self.gap_limit {
            return Err(SendError::Stuck(u64::try_from(latest).unwrap_or(u64::MAX)));
        }
        Ok(())
    }

    async fn sign_and_send(&self, tx: Tx1559) -> Result<Sent, SendError> {
        let (raw, hash) = tx
            .sign(&self.secret)
            .ok_or_else(|| SendError::Rpc("signing failed".into()))?;
        let hash = hex0x(&hash);
        self.rpc_call("eth_sendRawTransaction", json!([hex0x(&raw)]))
            .await?;
        Ok(Sent { hash, tx, raw })
    }

    /// Hand `sent` to the node again, as signed (no new nonce, no new fee):
    /// a node that dropped it from its pool takes it back (REV-L4-F1). The
    /// node's refusal ("already known", "nonce too low") is returned for the
    /// caller to ignore.
    pub async fn rebroadcast(&self, sent: &Sent) -> Result<(), SendError> {
        self.rpc_call("eth_sendRawTransaction", json!([hex0x(&sent.raw)]))
            .await
            .map(|_| ())
    }

    /// Poll until one of `hashes` (versions of one transaction) is mined, or
    /// `deadline` passes (`Ok(None)`). A mined but reverted version is
    /// [`SendError::Reverted`]. Holds no lock.
    pub async fn wait_mined(
        &self,
        hashes: &[String],
        deadline: Duration,
    ) -> Result<Option<(String, Mined)>, SendError> {
        let start = tokio::time::Instant::now();
        loop {
            if let Some(found) = self.mined_now(hashes).await? {
                return Ok(Some(found));
            }
            if start.elapsed() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    /// One look for a mined version among `hashes`.
    pub async fn mined_now(&self, hashes: &[String]) -> Result<Option<(String, Mined)>, SendError> {
        for hash in hashes {
            match self
                .rpc
                .call("eth_getTransactionReceipt", json!([hash]))
                .await
            {
                Ok(r) if !r.is_null() => {
                    let block = u64::try_from(quantity(&r["blockNumber"])?)
                        .map_err(|e| SendError::Rpc(e.to_string()))?;
                    self.seen_block
                        .fetch_max(block, std::sync::atomic::Ordering::Relaxed);
                    if r["status"] != "0x1" {
                        return Err(SendError::Reverted(hash.clone()));
                    }
                    return Ok(Some((
                        hash.clone(),
                        Mined {
                            block,
                            gas_used: u64::try_from(quantity(&r["gasUsed"])?)
                                .map_err(|e| SendError::Rpc(e.to_string()))?,
                            effective_gas_price: quantity(&r["effectiveGasPrice"])?,
                            hash: hash.clone(),
                        },
                    )));
                }
                // Not mined yet, or a passing RPC error: keep polling.
                _ => {}
            }
        }
        Ok(None)
    }

    /// The account's nonce at `tag` (`latest`: mined transactions only).
    pub async fn nonce_at(&self, tag: &str) -> Result<u64, SendError> {
        let n = quantity(
            &self
                .rpc_call(
                    "eth_getTransactionCount",
                    json!([hex0x(&self.address), tag]),
                )
                .await?,
        )?;
        u64::try_from(n).map_err(|e| SendError::Rpc(e.to_string()))
    }
}

/// A transaction the node accepted, not yet seen mined.
#[derive(Clone, PartialEq, Eq)]
pub struct Sent {
    /// Its hash.
    pub hash: String,
    /// What was signed (for a replacement at the same nonce).
    pub tx: Tx1559,
    /// The signed raw transaction (for a re-broadcast, REV-L4-F1).
    pub raw: Vec<u8>,
}

impl std::fmt::Debug for Sent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sent")
            .field("hash", &self.hash)
            .field("nonce", &self.tx.nonce)
            .finish_non_exhaustive()
    }
}

impl Sent {
    /// JSON for a journal (a reporter's in-flight `credit`, REV-L4-F1). The
    /// raw transaction is public once broadcast; no key is in it.
    pub fn to_json(&self) -> Value {
        json!({
            "hash": self.hash,
            "raw": hex0x(&self.raw),
            "chainId": self.tx.chain_id,
            "nonce": self.tx.nonce,
            "tip": self.tx.max_priority_fee_per_gas.to_string(),
            "maxFee": self.tx.max_fee_per_gas.to_string(),
            "gas": self.tx.gas,
            "to": hex0x(&self.tx.to),
            "value": self.tx.value.to_string(),
            "data": hex0x(&self.tx.data),
        })
    }

    /// The inverse of [`Sent::to_json`]; `None` for anything else.
    pub fn from_json(v: &Value) -> Option<Sent> {
        let bytes = |k: &str| hex::decode(v.get(k)?.as_str()?.strip_prefix("0x")?).ok();
        let num = |k: &str| v.get(k)?.as_str()?.parse::<u128>().ok();
        Some(Sent {
            hash: v.get("hash")?.as_str()?.to_string(),
            raw: bytes("raw")?,
            tx: Tx1559 {
                chain_id: v.get("chainId")?.as_u64()?,
                nonce: v.get("nonce")?.as_u64()?,
                max_priority_fee_per_gas: num("tip")?,
                max_fee_per_gas: num("maxFee")?,
                gas: v.get("gas")?.as_u64()?,
                to: bytes("to")?.try_into().ok()?,
                value: num("value")?,
                data: bytes("data")?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public test key (sha256 of "onyx evm_tx test key"); it holds nothing.
    const KEY: &str = "3b29ac64edc02d09b07a4a07590beb15e5b67465bd69ed8eecddb4f8c0645df6";

    fn key() -> [u8; 32] {
        let mut k = [0u8; 32];
        hex::decode_to_slice(KEY, &mut k).expect("hex");
        k
    }

    /// Byte-identical to Foundry's `cast mktx` (1.8.5) for the same fields:
    /// `cast mktx --private-key <KEY> --nonce 7 --gas-limit 100000
    /// --gas-price 2000000000 --priority-gas-price 1000000 --chain 84532
    /// 0x…aa --value 12345 "transfer(address,uint256)" 0x…bb 99`.
    #[test]
    fn signed_transaction_matches_cast_mktx() {
        let mut to = [0u8; 20];
        to[19] = 0xaa;
        let mut data = hex::decode("a9059cbb").expect("selector");
        let mut arg = [0u8; 32];
        arg[31] = 0xbb;
        data.extend_from_slice(&arg);
        let mut n = [0u8; 32];
        n[31] = 99;
        data.extend_from_slice(&n);
        let tx = Tx1559 {
            chain_id: 84_532,
            nonce: 7,
            max_priority_fee_per_gas: 1_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas: 100_000,
            to,
            value: 12_345,
            data,
        };
        let (raw, _) = tx.sign(&key()).expect("signs");
        assert_eq!(
            hex0x(&raw),
            "0x02f8b483014a3407830f42408477359400830186a09400000000000000000000000000000000000000aa823039b844a9059cbb00000000000000000000000000000000000000000000000000000000000000bb0000000000000000000000000000000000000000000000000000000000000063c001a0a328b2b26a7b76cc5ad387edea5900a02b981a1f86e7835d66d61ee74634613aa03e3a502e7187ec372192720ed56da12527252bb4396e96d9d33d4a414084406d"
        );
    }

    #[test]
    fn rlp_lengths_cross_the_55_byte_boundary() {
        assert_eq!(rlp_bytes(&[0x7f]), vec![0x7f]);
        assert_eq!(rlp_bytes(&[0x80]), vec![0x81, 0x80]);
        assert_eq!(rlp_bytes(&[]), vec![0x80]);
        assert_eq!(rlp_bytes(&[1u8; 55])[0], 0x80 + 55);
        assert_eq!(rlp_bytes(&[1u8; 56])[..2], [0xb8, 56]);
        assert_eq!(rlp_bytes(&[1u8; 256])[..3], [0xb9, 1, 0]);
        assert_eq!(rlp_uint(0), vec![0x80]);
        assert_eq!(rlp_uint(1024), vec![0x82, 0x04, 0x00]);
        assert_eq!(rlp_list(&[]), vec![0xc0]);
    }

    #[test]
    fn debug_never_shows_the_key() {
        struct NoRpc;
        impl JsonRpc for NoRpc {
            fn call<'a>(
                &'a self,
                _: &'a str,
                _: Value,
            ) -> crate::x402::BoxFuture<'a, Result<Value, String>> {
                Box::pin(async { Err("offline".to_string()) })
            }
        }
        let s = Sender::new(NoRpc, 84_532, key()).expect("key");
        let d = format!("{s:?}");
        assert!(
            !d.contains(KEY) && !d.to_lowercase().contains(&KEY[..16]),
            "{d}"
        );
        assert!(
            d.contains("0x34bf6ba1f17501b100a707913e09084d9ce65219"),
            "{d}"
        );
    }

    /// More fields against `cast mktx` (Foundry 1.8.5): nonce 0 and 300
    /// bytes of calldata (a two-byte list length), and extreme values (a
    /// large nonce, u128::MAX fee cap, zero tip and value). From review
    /// M8-L58.
    #[test]
    fn long_calldata_and_extreme_fields_match_cast() {
        let addr = |s: &str| {
            let mut a = [0u8; 20];
            hex::decode_to_slice(s, &mut a).expect("hex");
            a
        };
        let long = Tx1559 {
            chain_id: 84_532,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000,
            max_fee_per_gas: 11_000_000,
            gas: 1_000_000,
            to: addr("036cbd53842c5426634e7929541ec2318f3dcf7e"),
            value: 0,
            data: vec![0xab; 300],
        };
        let (raw, _) = long.sign(&key()).expect("sign");
        let want = format!(
            "02f9019a83014a3480830f424083a7d8c0830f424094036cbd53842c5426634e7929541ec2318f3dcf7e80b9012c{}c001a0346050bf54121210d73c18b2486ed0a9fb706d76eff23303d73d1aa965b1e8e6a01d86543b7e9eb563f7217425dc9516244b790127281cd718d2ef4796c259511d",
            "ab".repeat(300)
        );
        assert_eq!(hex::encode(raw), want);
        let extreme = Tx1559 {
            chain_id: 84_532,
            nonce: 1_152_921_504_606_846_975,
            max_priority_fee_per_gas: 0,
            max_fee_per_gas: u128::MAX,
            gas: 21_000,
            to: addr("0000000000000000000000000000000000000001"),
            value: 0,
            data: vec![],
        };
        let (raw, _) = extreme.sign(&key()).expect("sign");
        assert_eq!(hex::encode(raw), "02f87d83014a34880fffffffffffffff8090ffffffffffffffffffffffffffffffff8252089400000000000000000000000000000000000000018080c001a0f3c1d9f3c20020c29520db54d74d67a8a8fa0f66886d179195627ce511959687a07d0b9c8c24c45a21f72bcc3986e53f92fd555989b32edb13af3dc3b94f3307c4");
    }

    /// M8-L58-F4: only a revert is a revert.
    #[test]
    fn revert_errors_are_told_from_transport_errors() {
        for e in [
            "eth_estimateGas: node error: {\"code\":3,\"message\":\"execution reverted: x\"}",
            "eth_estimateGas: node error: {\"code\":-32000,\"message\":\"execution reverted\"}",
            "execution reverted",
        ] {
            assert!(is_revert(e), "{e}");
        }
        for e in [
            "eth_estimateGas: http status 429",
            "eth_estimateGas: node error: {\"code\":-32000,\"message\":\"insufficient funds for gas * price + value\"}",
            "eth_estimateGas: node error: {\"code\":-32005,\"message\":\"rate limited\"}",
            "connection refused",
        ] {
            assert!(!is_revert(e), "{e}");
        }
    }

    /// A node that lies about fees and gas: a huge tip, a normal base fee,
    /// then a huge gas estimate (AUD-1-F3).
    struct Hostile {
        tip: &'static str,
        base: &'static str,
        gas: &'static str,
    }
    impl JsonRpc for Hostile {
        fn call<'a>(
            &'a self,
            method: &'a str,
            _: Value,
        ) -> crate::x402::BoxFuture<'a, Result<Value, String>> {
            let v = match method {
                "eth_estimateGas" => json!(self.gas),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getBlockByNumber" => json!({"baseFeePerGas": self.base}),
                "eth_maxPriorityFeePerGas" => json!(self.tip),
                "eth_sendRawTransaction" => json!(format!("0x{}", "11".repeat(32))),
                _ => return Box::pin(async { Err("unexpected".to_string()) }),
            };
            Box::pin(async move { Ok(v) })
        }
    }

    /// A load-balanced node: the head it reports lags this key's last mined
    /// block (48), and every estimate's block tag is recorded.
    struct Lagging {
        estimates: std::sync::Mutex<Vec<Value>>,
    }
    impl JsonRpc for Lagging {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> crate::x402::BoxFuture<'a, Result<Value, String>> {
            let v = match method {
                "eth_estimateGas" => {
                    let mut est = self.estimates.lock().expect("lock");
                    est.push(params.clone());
                    // The first pinned estimate hits a backend without the
                    // block: an error, never an old answer.
                    let pinned = |p: &Value| p.as_array().map(Vec::len) == Some(2);
                    if pinned(&params) && est.iter().filter(|p| pinned(p)).count() == 1 {
                        return Box::pin(async {
                            Err("eth_estimateGas: node error: {\"code\":-32001,\"message\":\"block not found: 0x30\"}".to_string())
                        });
                    }
                    json!("0x5208")
                }
                "eth_blockNumber" => json!("0x28"),
                "eth_getTransactionReceipt" => {
                    json!({"status": "0x1", "blockNumber": "0x30", "gasUsed": "0x5208",
                           "effectiveGasPrice": "0x1"})
                }
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getBlockByNumber" => json!({"baseFeePerGas": "0x1"}),
                "eth_maxPriorityFeePerGas" => json!("0x1"),
                "eth_sendRawTransaction" => json!(format!("0x{}", "11".repeat(32))),
                _ => return Box::pin(async { Err("unexpected".to_string()) }),
            };
            Box::pin(async move { Ok(v) })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_estimate_never_reads_before_the_keys_last_mined_block() {
        let s = Sender::new(
            Lagging {
                estimates: std::sync::Mutex::new(Vec::new()),
            },
            84_532,
            key(),
        )
        .expect("key");
        let to = [0xaa; 20];
        // Nothing mined yet: the node's own latest block.
        s.broadcast(&to, &[], 0, Duration::from_secs(1))
            .await
            .expect("sends");
        // A receipt in block 0x30; the node then reports head 0x28.
        s.mined_now(&[format!("0x{}", "11".repeat(32))])
            .await
            .expect("mined")
            .expect("found");
        s.broadcast(&to, &[], 0, Duration::from_secs(1))
            .await
            .expect("sends");
        let est = s.rpc.estimates.lock().expect("lock");
        assert_eq!(est[0].as_array().map(Vec::len), Some(1));
        assert_eq!(
            est[1][1], "0x30",
            "the estimate names the mined block, not the lagging head"
        );
        assert_eq!(
            est.len(),
            3,
            "a missing block is waited for, then asked again"
        );
        assert_eq!(est[2][1], "0x30");
    }

    /// A scripted node for the estimate pin (review LAUNCH-R1-A1..A3): its
    /// head, its answer to an estimate, and every call recorded.
    struct Pinned {
        head: Option<&'static str>,
        /// The next this many `eth_blockNumber` calls fail (REVLOG-R13-4).
        head_fails: std::sync::atomic::AtomicU32,
        estimate: fn(&Value) -> Result<Value, String>,
        calls: std::sync::Mutex<Vec<(String, Value)>>,
    }
    impl Pinned {
        fn new(head: Option<&'static str>, estimate: fn(&Value) -> Result<Value, String>) -> Self {
            Pinned {
                head,
                head_fails: std::sync::atomic::AtomicU32::new(0),
                estimate,
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn estimates(&self) -> Vec<Value> {
            let c = self.calls.lock().expect("lock");
            c.iter()
                .filter(|(m, _)| m == "eth_estimateGas")
                .map(|(_, p)| p.clone())
                .collect()
        }
        fn count(&self, method: &str) -> usize {
            let c = self.calls.lock().expect("lock");
            c.iter().filter(|(m, _)| m == method).count()
        }
    }
    impl JsonRpc for Pinned {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> crate::x402::BoxFuture<'a, Result<Value, String>> {
            self.calls
                .lock()
                .expect("lock")
                .push((method.to_string(), params.clone()));
            let r = match method {
                "eth_estimateGas" => (self.estimate)(&params),
                "eth_blockNumber"
                    if self
                        .head_fails
                        .fetch_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |n| n.checked_sub(1),
                        )
                        .is_ok() =>
                {
                    Err("eth_blockNumber: http status 503".to_string())
                }
                "eth_blockNumber" => self
                    .head
                    .map(|h| json!(h))
                    .ok_or_else(|| "eth_blockNumber: http status 503".to_string()),
                // This key's last transaction, mined in block 0x30.
                "eth_getTransactionReceipt" => Ok(json!({"status": "0x1", "blockNumber": "0x30",
                    "gasUsed": "0x5208", "effectiveGasPrice": "0x1"})),
                "eth_getTransactionCount" => Ok(json!("0x0")),
                "eth_getBlockByNumber" => Ok(json!({"baseFeePerGas": "0x1"})),
                "eth_maxPriorityFeePerGas" => Ok(json!("0x1")),
                "eth_sendRawTransaction" => Ok(json!(format!("0x{}", "11".repeat(32)))),
                _ => Err("unexpected".to_string()),
            };
            Box::pin(async move { r })
        }
    }

    /// The block an estimate names (0 when unpinned).
    fn pinned_at(p: &Value) -> u64 {
        p.get(1)
            .and_then(Value::as_str)
            .and_then(|b| u64::from_str_radix(b.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    }

    /// A sender whose key has a transaction mined in block 0x30.
    async fn after_a_mined_tx(rpc: Pinned) -> Sender<Pinned> {
        let s = Sender::new(rpc, 84_532, key())
            .expect("key")
            .with_poll(Duration::from_millis(1));
        s.mined_now(&[format!("0x{}", "11".repeat(32))])
            .await
            .expect("mined")
            .expect("found");
        s
    }

    /// Review LAUNCH-R1-A1 (probe-verified): a receipt naming a block the
    /// node never reaches cost every later send 20 polls (about 9.5 s on
    /// the 500 ms default). After the 20th poll the pin drops to the head
    /// the node reported, so only that one send fails.
    #[tokio::test(start_paused = true)]
    async fn a_receipt_block_the_node_never_reaches_slows_one_send_only() {
        // Head 0x28; any block above it is "header not found".
        let s = after_a_mined_tx(Pinned::new(Some("0x28"), |p| {
            if pinned_at(p) > 0x28 {
                Err("node error: {\"code\":-32000,\"message\":\"header not found\"}".into())
            } else {
                Ok(json!("0x5208"))
            }
        }))
        .await;
        let to = [0xaa; 20];
        let first = s.broadcast(&to, &[], 0, Duration::from_secs(1)).await;
        assert!(matches!(first, Err(SendError::Rpc(_))), "{first:?}");
        assert_eq!(s.rpc.estimates().len(), 20);
        assert_eq!(
            s.seen_block.load(std::sync::atomic::Ordering::Relaxed),
            0x28,
            "the pin dropped to the reported head"
        );
        s.broadcast(&to, &[], 0, Duration::from_secs(1))
            .await
            .expect("the next send goes at once");
        let est = s.rpc.estimates();
        assert_eq!(est.len(), 21, "one estimate, no polling");
        assert_eq!(est[20][1], "0x28");
    }

    /// Review LAUNCH-R1-A2: a provider that takes no block parameter on
    /// `eth_estimateGas` is asked once without it; Nethermind's wording is
    /// a missing block.
    #[tokio::test(start_paused = true)]
    async fn an_estimate_without_a_block_parameter_is_asked_once_unpinned() {
        let s = after_a_mined_tx(Pinned::new(Some("0x30"), |p| {
            if p.as_array().map(Vec::len) == Some(2) {
                Err("node error: {\"code\":-32602,\"message\":\"invalid params\"}".into())
            } else {
                Ok(json!("0x5208"))
            }
        }))
        .await;
        s.broadcast(&[0xaa; 20], &[], 0, Duration::from_secs(1))
            .await
            .expect("sends");
        let est = s.rpc.estimates();
        assert_eq!(est.len(), 2);
        assert_eq!(est[0][1], "0x30", "pinned first");
        assert_eq!(est[1].as_array().map(Vec::len), Some(1), "then unpinned");
        assert!(is_missing_block(
            "node error: {\"code\":-32001,\"message\":\"Block 48 could not be found\"}"
        ));
        assert!(!is_invalid_params("header not found"));
    }

    /// Review LAUNCH-R1-A3: a failed `eth_blockNumber` never pins the
    /// estimate at the old block (which may be behind the head): the send
    /// fails with nothing sent.
    #[tokio::test(start_paused = true)]
    async fn a_failed_head_read_sends_nothing() {
        let s = after_a_mined_tx(Pinned::new(None, |_| Ok(json!("0x5208")))).await;
        let r = s
            .broadcast(&[0xaa; 20], &[], 0, Duration::from_secs(1))
            .await;
        assert!(
            matches!(&r, Err(SendError::Rpc(e)) if e.contains("eth_blockNumber")),
            "{r:?}"
        );
        assert_eq!(s.rpc.estimates().len(), 0, "no estimate at the old block");
        assert_eq!(s.rpc.count("eth_sendRawTransaction"), 0);
        assert_eq!(
            s.rpc.count("eth_blockNumber"),
            HEAD_READ_TRIES as usize,
            "asked {HEAD_READ_TRIES} times before giving up"
        );
    }

    /// Review REVLOG-R13-4: a flaky `eth_blockNumber` (two failures, then
    /// an answer) no longer fails the send: the estimate asks again, a poll
    /// apart, and pins at the head it gets.
    #[tokio::test(start_paused = true)]
    async fn a_flaky_head_read_is_retried_inside_the_estimate() {
        let s = after_a_mined_tx(Pinned::new(Some("0x31"), |_| Ok(json!("0x5208")))).await;
        s.rpc
            .head_fails
            .store(2, std::sync::atomic::Ordering::SeqCst);
        s.broadcast(&[0xaa; 20], &[], 0, Duration::from_secs(1))
            .await
            .expect("sends after the retries");
        assert_eq!(s.rpc.count("eth_blockNumber"), 3);
        let est = s.rpc.estimates();
        assert_eq!(est.len(), 1);
        assert_eq!(est[0][1], "0x31", "pinned at the head it got");
        assert_eq!(s.rpc.count("eth_sendRawTransaction"), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_cannot_make_a_send_overpay() {
        let to = [0xaa; 20];
        // A tip of 10^30 wei per gas is clamped to the cap.
        let s = Sender::new(
            Hostile {
                tip: "0xc9f2c9cd04674edea40000000",
                base: "0x3b9aca00",
                gas: "0x5208",
            },
            84_532,
            key(),
        )
        .expect("key")
        .with_fee_limits(5_000_000_000, 1_000_000);
        let sent = s
            .broadcast(&to, &[], 0, Duration::from_secs(1))
            .await
            .expect("sends at the cap");
        assert_eq!(sent.tx.max_fee_per_gas, 5_000_000_000);
        assert!(sent.tx.max_priority_fee_per_gas <= sent.tx.max_fee_per_gas);
        // A base fee at or above the cap refuses the send.
        let s = Sender::new(
            Hostile {
                tip: "0x1",
                base: "0x2540be400",
                gas: "0x5208",
            },
            84_532,
            key(),
        )
        .expect("key")
        .with_fee_limits(5_000_000_000, 1_000_000);
        assert!(matches!(
            s.broadcast(&to, &[], 0, Duration::from_secs(1)).await,
            Err(SendError::Rpc(_))
        ));
        // A huge gas estimate refuses the send.
        let s = Sender::new(
            Hostile {
                tip: "0x1",
                base: "0x1",
                gas: "0xffffffffff",
            },
            84_532,
            key(),
        )
        .expect("key");
        assert!(matches!(
            s.broadcast(&to, &[], 0, Duration::from_secs(1)).await,
            Err(SendError::Rpc(_))
        ));
        // The defaults bound a sender that set nothing.
        let s = Sender::new(
            Hostile {
                tip: "0xc9f2c9cd04674edea40000000",
                base: "0x1",
                gas: "0x5208",
            },
            84_532,
            key(),
        )
        .expect("key");
        let sent = s
            .broadcast(&to, &[], 0, Duration::from_secs(1))
            .await
            .expect("sends");
        assert_eq!(sent.tx.max_fee_per_gas, DEFAULT_MAX_FEE_PER_GAS);
    }
}
