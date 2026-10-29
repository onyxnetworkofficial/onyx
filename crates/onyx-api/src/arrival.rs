//! Proof from the token's own logs that an EIP-3009 authorization moved its
//! exact value into `OnyxRevenue` (M8-L58-F3): an `AuthorizationUsed(from,
//! nonce)` log and a `Transfer(from -> revenue, value)` log from the token in
//! the same successful transaction. Used by the self-settling facilitator and
//! by the settle-intent resolver ([`crate::intents`]); a nonce spent paying
//! someone else never counts.
//!
//! The pairing is exact (OPS-L3-F2): the `Transfer` must be the log right
//! after that nonce's `AuthorizationUsed`, as FiatToken emits them, so one
//! `Transfer` proves one authorization. Any other shape (no adjacent
//! `Transfer`, one from another sender) is not our payment: a nonce is used
//! once, so that use is its only one (OPS-L4). The resolver's verdicts use
//! the chain's own time and a pinned block ([`judge`], OPS-L3-F3).
//!
//! Every authorization reaches a verdict (OPS-L4-F1): an `AuthorizationCanceled`
//! log ends it; once the chain is past `validBefore` + grace, the whole span
//! its use could lie in is read in full ([`FINAL_SCANS`] times, on a node that
//! serves the pinned block), and an authorization the token marks used with
//! no payment of ours in that span did not pay us.
//!
//! The search is cheap and fits public nodes (OPS-L2): first the token's
//! `authorizationState(from, nonce)` (an unused authorization moved nothing,
//! no log search); then the blocks whose timestamps lie in the time window
//! the authorization could have been used in are found by binary search on
//! block timestamps, and only those are read, oldest first, in windows of
//! [`LOGS_WINDOW`] blocks (the public Base Sepolia node refuses
//! `eth_getLogs` over more than 200 blocks).

use crate::roots::JsonRpc;
use onyx_core::keccak256;
use serde_json::{json, Value};

/// `keccak256("AuthorizationUsed(address,bytes32)")`, FiatToken's event.
pub fn authorization_used_topic() -> [u8; 32] {
    keccak256(b"AuthorizationUsed(address,bytes32)").0
}

/// `keccak256("AuthorizationCanceled(address,bytes32)")`, FiatToken's event
/// for `cancelAuthorization` (which marks the nonce used without a
/// `Transfer`; OPS-L4-F1).
pub fn authorization_canceled_topic() -> [u8; 32] {
    keccak256(b"AuthorizationCanceled(address,bytes32)").0
}

/// `keccak256("Transfer(address,address,uint256)")`.
pub fn transfer_topic() -> [u8; 32] {
    keccak256(b"Transfer(address,address,uint256)").0
}

/// Blocks per `eth_getLogs` call: `https://sepolia.base.org` refuses wider
/// ranges ("eth_getLogs is limited to a 200 range").
pub const LOGS_WINDOW: u64 = 200;

/// The most `eth_getLogs` calls one search makes (about 4.4 days of 2 s
/// blocks); a wider window is an error (kept, retried), never a "no".
pub const MAX_LOG_CALLS: u64 = 1_000;

/// Complete reads of an expired authorization's span, in separate resolver
/// passes, that must all find no payment of ours before it is judged
/// [`Verdict::Never`] (OPS-L4-F2): one load-balanced backend that silently
/// clamps `eth_getLogs` to an older head cannot decide it alone.
pub const FINAL_SCANS: u8 = 2;

/// How far before its computed start an expired span is read once more
/// before a [`Verdict::Never`] (OPS-L4-F3), down to the authorization's
/// `validAfter` at most: a local clock that was wrong when the intent was
/// written cannot hide the payment.
pub const EXTEND_BACK_SECS: u64 = 3_600;

/// A 32-byte word holding an address.
pub fn word_addr(a: &[u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// A 32-byte word holding an unsigned integer.
pub fn word_u128(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn quantity(v: &Value, what: &str) -> Result<u64, String> {
    u64::from_str_radix(
        v.as_str()
            .ok_or_else(|| format!("{what}: not a quantity"))?
            .trim_start_matches("0x"),
        16,
    )
    .map_err(|e| format!("{what}: {e}"))
}

/// What to look for: `value` moved from `from` to `revenue` by the
/// authorization `nonce` of `token`.
#[derive(Clone, Copy, Debug)]
pub struct Payment {
    /// The ERC-20 (EIP-3009) token.
    pub token: [u8; 20],
    /// The recipient that must have received it (`OnyxRevenue`).
    pub revenue: [u8; 20],
    /// EIP-3009 `from`.
    pub from: [u8; 20],
    /// EIP-3009 nonce.
    pub nonce: [u8; 32],
    /// Exact amount, in atomic units.
    pub value: u128,
}

/// The node's head block number.
pub async fn head<T: JsonRpc + ?Sized>(rpc: &T) -> Result<u64, String> {
    quantity(
        &rpc.call("eth_blockNumber", json!([])).await?,
        "eth_blockNumber",
    )
}

/// The node's head block number and its timestamp (the chain's own clock).
pub async fn head_time<T: JsonRpc + ?Sized>(rpc: &T) -> Result<(u64, u64), String> {
    let h = head(rpc).await?;
    Ok((h, block_time(rpc, h).await?))
}

/// The timestamp of block `n`.
async fn block_time<T: JsonRpc + ?Sized>(rpc: &T, n: u64) -> Result<u64, String> {
    let b = rpc
        .call("eth_getBlockByNumber", json!([format!("0x{n:x}"), false]))
        .await?;
    quantity(&b["timestamp"], "block timestamp")
}

/// The first block in `0..=head` whose timestamp is at least `t`, or
/// `head + 1` if none is (block timestamps never decrease). It gallops back
/// from the head, so it only touches recent blocks: public nodes prune old
/// history (`https://sepolia.base.org` answers "pruned history unavailable"
/// for early blocks).
async fn first_block_at<T: JsonRpc + ?Sized>(
    rpc: &T,
    t: u64,
    head: u64,
    head_time: u64,
) -> Result<u64, String> {
    if head_time < t {
        return Ok(head.saturating_add(1));
    }
    // Invariant: block `hi` is at or after `t`; find `lo` before it.
    let (mut lo, mut hi, mut step) = (0u64, head, 64u64);
    loop {
        let probe = hi.saturating_sub(step);
        if probe == 0 {
            if block_time(rpc, 0).await? >= t {
                return Ok(0);
            }
            break;
        }
        if block_time(rpc, probe).await? < t {
            lo = probe;
            break;
        }
        hi = probe;
        step = step.saturating_mul(2);
    }
    // Block `lo` is before `t`, block `hi` at or after it.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if block_time(rpc, mid).await? >= t {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Ok(hi)
}

/// The token's `authorizationState(from, nonce)` at `block` (`"latest"` or a
/// block number): true once the authorization was used (or cancelled).
async fn used_at<T: JsonRpc + ?Sized>(
    rpc: &T,
    token: &[u8; 20],
    from: &[u8; 20],
    nonce: &[u8; 32],
    block: Value,
) -> Result<bool, String> {
    let sel = &keccak256(b"authorizationState(address,bytes32)").0[..4];
    let mut data = sel.to_vec();
    data.extend_from_slice(&word_addr(from));
    data.extend_from_slice(nonce);
    let r = rpc
        .call(
            "eth_call",
            json!([{ "to": hex0x(token), "data": hex0x(&data) }, block]),
        )
        .await?;
    let bytes = hex::decode(
        r.as_str()
            .ok_or("authorizationState: not bytes")?
            .trim_start_matches("0x"),
    )
    .map_err(|e| format!("authorizationState: {e}"))?;
    if bytes.len() != 32 {
        return Err("authorizationState: not one word".into());
    }
    Ok(bytes.last().copied() == Some(1))
}

/// The token's `authorizationState(from, nonce)` at the latest block.
pub async fn used<T: JsonRpc + ?Sized>(
    rpc: &T,
    token: &[u8; 20],
    from: &[u8; 20],
    nonce: &[u8; 32],
) -> Result<bool, String> {
    used_at(rpc, token, from, nonce, json!("latest")).await
}

fn lower(v: &Value) -> Option<String> {
    v.as_str().map(str::to_lowercase)
}

/// Where the authorization `p.nonce` of `p.from` sent its value, read from
/// the receipt of the transaction that used it (OPS-L3-F2). FiatToken's
/// `transferWithAuthorization` emits `AuthorizationUsed(from, nonce)` and
/// then, as the very next log, the `Transfer(from, to, value)` it caused; so
/// the proof is that exact pair, adjacent, from the token, in a successful
/// transaction. One `Transfer` therefore proves at most one authorization.
enum Use {
    /// The authorization moved exactly `p.value` from `p.from` to `p.revenue`.
    Ours,
    /// Its one use did not pay us: another recipient or amount, or not the
    /// `AuthorizationUsed`-then-`Transfer` pair a payment makes.
    Elsewhere,
    /// The payer cancelled it (`cancelAuthorization`): it never moves money.
    Cancelled,
}

fn pairing(receipt: &Value, p: &Payment) -> Result<Option<Use>, String> {
    if receipt["status"] != "0x1" {
        return Ok(None);
    }
    let token = hex0x(&p.token);
    let used = [
        hex0x(&authorization_used_topic()),
        hex0x(&word_addr(&p.from)),
        hex0x(&p.nonce),
    ];
    let logs = receipt["logs"].as_array().ok_or("receipt has no logs")?;
    let topics = |l: &Value| -> Vec<Option<String>> {
        l["topics"]
            .as_array()
            .map(|t| t.iter().map(lower).collect())
            .unwrap_or_default()
    };
    let Some(i) = logs.iter().position(|l| {
        lower(&l["address"]).as_deref() == Some(token.as_str())
            && topics(l) == used.iter().map(|u| Some(u.clone())).collect::<Vec<_>>()
    }) else {
        return Ok(None);
    };
    // The nonce's one use is this one. Not followed (adjacently) by its own
    // Transfer, it is not a payment to us: never credited, and never kept
    // open either (OPS-L4: a shape an attacker can choose must end).
    let Some(next) = logs.get(i + 1) else {
        return Ok(Some(Use::Elsewhere));
    };
    if let (Some(a), Some(b)) = (
        logs[i]["logIndex"]
            .as_str()
            .map(|x| quantity(&json!(x), "logIndex")),
        next["logIndex"]
            .as_str()
            .map(|x| quantity(&json!(x), "logIndex")),
    ) {
        if b? != a?.saturating_add(1) {
            return Ok(Some(Use::Elsewhere));
        }
    }
    let t = topics(next);
    let is_transfer = lower(&next["address"]).as_deref() == Some(token.as_str())
        && t.len() == 3
        && t[0].as_deref() == Some(hex0x(&transfer_topic()).as_str())
        && t[1].as_deref() == Some(hex0x(&word_addr(&p.from)).as_str());
    if !is_transfer {
        return Ok(Some(Use::Elsewhere));
    }
    let ours = t[2].as_deref() == Some(hex0x(&word_addr(&p.revenue)).as_str())
        && lower(&next["data"]).as_deref() == Some(hex0x(&word_u128(p.value)).as_str());
    Ok(Some(if ours { Use::Ours } else { Use::Elsewhere }))
}

/// Scan `from..=to` (block numbers) for the use of `p`, at most `budget`
/// `eth_getLogs` calls. `Ok((found, next))`: the use if found (with its
/// transaction hash), and the first block not read yet (`to + 1` when the
/// range was read in full).
async fn scan<T: JsonRpc + ?Sized>(
    rpc: &T,
    p: &Payment,
    from: u64,
    to: u64,
    budget: &mut u64,
) -> Result<(Option<(Use, String)>, u64), String> {
    let token = hex0x(&p.token);
    let mut at = from;
    while at <= to {
        if *budget == 0 {
            return Ok((None, at));
        }
        *budget -= 1;
        let end = at.saturating_add(LOGS_WINDOW - 1).min(to);
        // Either event of this (authorizer, nonce): its use or its cancel.
        let filter = json!([{
            "address": token,
            "fromBlock": format!("0x{at:x}"),
            "toBlock": format!("0x{end:x}"),
            "topics": [
                [hex0x(&authorization_used_topic()), hex0x(&authorization_canceled_topic())],
                hex0x(&word_addr(&p.from)),
                hex0x(&p.nonce),
            ],
        }]);
        let logs = rpc.call("eth_getLogs", filter).await?;
        let logs = logs.as_array().ok_or("eth_getLogs: not a list")?;
        let canceled = hex0x(&authorization_canceled_topic());
        for log in logs {
            if lower(&log["topics"][0]).as_deref() == Some(canceled.as_str())
                && lower(&log["address"]).as_deref() == Some(token.as_str())
            {
                let tx = log["transactionHash"].as_str().unwrap_or_default();
                return Ok((Some((Use::Cancelled, tx.into())), end.saturating_add(1)));
            }
            let Some(tx) = log["transactionHash"].as_str() else {
                continue;
            };
            let r = rpc.call("eth_getTransactionReceipt", json!([tx])).await?;
            if let Some(u) = pairing(&r, p)? {
                return Ok((Some((u, tx.into())), end.saturating_add(1)));
            }
        }
        at = end.saturating_add(1);
    }
    Ok((None, at))
}

/// Whether `p` arrived in a block whose timestamp lies in `earliest..=latest`
/// (unix seconds; `latest` past the head means up to the head), judged at
/// the node's latest block (the self-settling facilitator's immediate
/// check). `Ok(false)` only when the authorization is unused, or its use was
/// found and sent the value elsewhere, or every block of the window was read
/// without finding it: an error anywhere is an error, never a "no".
pub async fn arrived<T: JsonRpc + ?Sized>(
    rpc: &T,
    p: &Payment,
    earliest: u64,
    latest: u64,
) -> Result<bool, String> {
    Ok(matches!(
        use_of(rpc, p, earliest, latest).await?,
        UseOf::Ours(_)
    ))
}

/// What became of an authorization, read at the node's latest block
/// ([`use_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UseOf {
    /// `authorizationState` says unused.
    Unused,
    /// Its use moved exactly its value into `OnyxRevenue`, in this
    /// transaction.
    Ours(String),
    /// Its one use did not pay us, or the payer cancelled it.
    NotOurs,
    /// Used, but no use was found in the window (a backend behind another,
    /// or a use outside the window): undecided, never a "no".
    NotFound,
}

/// [`arrived`], telling every answer apart: the token's
/// `authorizationState(from, nonce)` at the latest block, then (used) the
/// token's own logs in blocks whose timestamps lie in `earliest..=latest`.
/// An error anywhere is an error.
pub async fn use_of<T: JsonRpc + ?Sized>(
    rpc: &T,
    p: &Payment,
    earliest: u64,
    latest: u64,
) -> Result<UseOf, String> {
    if !used(rpc, &p.token, &p.from, &p.nonce).await? {
        return Ok(UseOf::Unused);
    }
    logged_use(rpc, p, earliest, latest).await
}

/// The token's logs alone (no `authorizationState` read): this
/// authorization's use in blocks whose timestamps lie in
/// `earliest..=latest`. For when the token already refused a re-send of it
/// but a lagging node still reads it unused (M7L3-R1-1). Never
/// [`UseOf::Unused`]; nothing found is [`UseOf::NotFound`].
pub async fn logged_use<T: JsonRpc + ?Sized>(
    rpc: &T,
    p: &Payment,
    earliest: u64,
    latest: u64,
) -> Result<UseOf, String> {
    let head = head(rpc).await?;
    let head_time = block_time(rpc, head).await?;
    let lo = first_block_at(rpc, earliest, head, head_time).await?;
    let hi = if latest >= head_time {
        head
    } else {
        first_block_at(rpc, latest.saturating_add(1), head, head_time)
            .await?
            .saturating_sub(1)
    };
    if lo > hi {
        return Ok(UseOf::NotFound);
    }
    if (hi - lo) / LOGS_WINDOW >= MAX_LOG_CALLS {
        return Err(format!(
            "arrival search over blocks {lo}..={hi} needs more than {MAX_LOG_CALLS} log calls"
        ));
    }
    let mut budget = MAX_LOG_CALLS;
    Ok(match scan(rpc, p, lo, hi, &mut budget).await?.0 {
        Some((Use::Ours, tx)) => UseOf::Ours(tx),
        Some((Use::Elsewhere | Use::Cancelled, _)) => UseOf::NotOurs,
        None => UseOf::NotFound,
    })
}

/// A resumable search for one authorization's use (OPS-L3-F4): the block
/// range is found once, and the next block to read is kept between passes,
/// so a long window is read across passes instead of failing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// First block of the window (`None` until found).
    lo: Option<u64>,
    /// Next block to read before expiry.
    next: Option<u64>,
    /// After expiry: the first block of the span read in full (OPS-L4-F2).
    final_lo: Option<u64>,
    /// After expiry: the next block of the current full read.
    final_next: Option<u64>,
    /// After expiry: full reads that found no payment of ours.
    finals: u8,
    /// The span was extended back once (OPS-L4-F3).
    extended: bool,
}

/// The verdict on one authorization, from the chain's own time (OPS-L3-F3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It moved its exact value into `OnyxRevenue`, in this transaction.
    Arrived(String),
    /// It never will: at a block past `validBefore + grace` (pinned, so a
    /// lagging node errors instead of answering) the token says it is
    /// unused; or its one use did not pay us; or, used, the full span it
    /// could have paid us in holds no payment of ours ([`FINAL_SCANS`]
    /// complete reads).
    Never,
    /// The payer cancelled it (`AuthorizationCanceled`): it never moves money.
    Cancelled,
    /// Not decided yet (not expired on chain, or the search is not done).
    Open,
}

/// The time span (unix seconds, chain time) an authorization's use can lie
/// in: from `earliest` (the server's bound, see [`crate::intents::Intent`])
/// to `valid_before`, extendable back to `valid_after` (the token refuses
/// any use outside `valid_after..valid_before`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The authorization's `validAfter`.
    pub valid_after: u64,
    /// Where the search starts.
    pub earliest: u64,
    /// The authorization's `validBefore`.
    pub valid_before: u64,
}

/// Judge `p`, used (if at all) within `span`, with `grace` seconds of margin
/// after `valid_before`, reading at most `budget` log windows (decremented)
/// and resuming from `cursor`.
///
/// Only the chain's own time decides expiry: the head's timestamp, then the
/// first block past `valid_before + grace`, read by number. A node that is
/// behind (its head before that time) gives [`Verdict::Open`]; one that
/// cannot serve that block gives an error. "Unused" is asked at that pinned
/// block, never at "latest".
///
/// Before expiry the span is read once, opportunistically (a payment found
/// early becomes a receipt early). After expiry it is read in full,
/// [`FINAL_SCANS`] times in separate calls (so separate passes), on a node
/// that has just served the pinned block; only then does a used
/// authorization with no payment of ours become [`Verdict::Never`], after one
/// extension back toward `valid_after` ([`EXTEND_BACK_SECS`]). Every call
/// does a bounded amount of work, so every authorization reaches a verdict
/// within a bounded number of passes after its expiry (OPS-L4-F1).
pub async fn judge<T: JsonRpc + ?Sized>(
    rpc: &T,
    p: &Payment,
    span: Span,
    grace: u64,
    cursor: &mut Cursor,
    budget: &mut u64,
) -> Result<Verdict, String> {
    let head = head(rpc).await?;
    let head_time = block_time(rpc, head).await?;
    let valid_before = span.valid_before;
    let expiry = valid_before.saturating_add(grace);
    let pinned = if head_time > expiry {
        let b = first_block_at(rpc, expiry.saturating_add(1), head, head_time).await?;
        if b > head || block_time(rpc, b).await? <= expiry {
            return Err("no block past the authorization's expiry".into());
        }
        Some(b)
    } else {
        None
    };
    let at = pinned.unwrap_or(head);
    if !used_at(rpc, &p.token, &p.from, &p.nonce, json!(format!("0x{at:x}"))).await? {
        return Ok(if pinned.is_some() {
            Verdict::Never
        } else {
            Verdict::Open
        });
    }
    let hi = if valid_before >= head_time {
        head
    } else {
        first_block_at(rpc, valid_before.saturating_add(1), head, head_time)
            .await?
            .saturating_sub(1)
    };
    let found = |u: Use, tx: String| match u {
        Use::Ours => Verdict::Arrived(tx),
        Use::Elsewhere => Verdict::Never,
        Use::Cancelled => Verdict::Cancelled,
    };
    let Some(pinned) = pinned else {
        // Before expiry: one opportunistic read, resuming.
        let lo = match cursor.lo {
            Some(lo) => lo,
            None => {
                let lo = first_block_at(rpc, span.earliest, head, head_time).await?;
                cursor.lo = Some(lo);
                lo
            }
        };
        let from = cursor.next.unwrap_or(lo).max(lo);
        if from > hi {
            return Ok(Verdict::Open);
        }
        let (use_, next) = scan(rpc, p, from, hi, budget).await?;
        if let Some((u, tx)) = use_ {
            return Ok(found(u, tx));
        }
        // The window still grows (head before `valid_before`): re-read its
        // last full window, since a block just past the head may not have
        // been served yet. A miss here is caught by the full reads below.
        cursor.next = Some(if valid_before >= head_time {
            next.saturating_sub(LOGS_WINDOW).max(lo)
        } else {
            next
        });
        return Ok(Verdict::Open);
    };
    // After expiry: full reads of the span, on a node that has just served
    // block `pinned`, so every block read exists on it (OPS-L4-F2). They run
    // up to the block before `pinned`: no use can follow `validBefore` (the
    // token refuses it), but a cancel can (OPS-L4-F1, `cancelAuthorization`
    // works on an expired authorization too).
    let hi = pinned.saturating_sub(1);
    let lo = match cursor.final_lo {
        Some(lo) => lo,
        None => {
            let lo = first_block_at(rpc, span.earliest, head, head_time).await?;
            cursor.final_lo = Some(lo);
            lo
        }
    };
    let from = cursor.final_next.unwrap_or(lo).max(lo);
    let (use_, next) = if from > hi {
        (None, from)
    } else {
        scan(rpc, p, from, hi, budget).await?
    };
    if let Some((u, tx)) = use_ {
        return Ok(found(u, tx));
    }
    if next <= hi {
        cursor.final_next = Some(next);
        return Ok(Verdict::Open);
    }
    // One full read found nothing; the next must be another call.
    cursor.final_next = None;
    cursor.finals = cursor.finals.saturating_add(1);
    if cursor.finals < FINAL_SCANS {
        return Ok(Verdict::Open);
    }
    if !cursor.extended && span.earliest > span.valid_after {
        // Read once more from further back, toward `valid_after`.
        cursor.extended = true;
        cursor.finals = 0;
        let back = span
            .earliest
            .saturating_sub(EXTEND_BACK_SECS)
            .max(span.valid_after);
        cursor.final_lo = Some(first_block_at(rpc, back, head, head_time).await?);
        return Ok(Verdict::Open);
    }
    Ok(Verdict::Never)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x402::BoxFuture;
    use std::sync::Mutex;

    /// A chain of `head + 1` blocks, block n at time `1_000 + 2 n`, with the
    /// payment in `at` (if any); records every `eth_getLogs` range.
    struct Blocks {
        head: u64,
        at: Option<u64>,
        ranges: Mutex<Vec<(u64, u64)>>,
        pruned_below: u64,
    }

    const TOKEN: [u8; 20] = [0x55; 20];
    const REVENUE: [u8; 20] = [0x77; 20];
    const FROM: [u8; 20] = [0x11; 20];

    impl JsonRpc for Blocks {
        fn call<'a>(
            &'a self,
            method: &'a str,
            params: Value,
        ) -> BoxFuture<'a, Result<Value, String>> {
            let q = |v: &Value| {
                u64::from_str_radix(v.as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
                    .unwrap_or(0)
            };
            let r = match method {
                "eth_call" => Ok(json!(format!("0x{:064x}", u8::from(self.at.is_some())))),
                "eth_blockNumber" => Ok(json!(format!("0x{:x}", self.head))),
                // Like a public node: early history is pruned.
                "eth_getBlockByNumber" if q(&params[0]) < self.pruned_below => {
                    Err("pruned history unavailable".into())
                }
                "eth_getBlockByNumber" => {
                    let n = q(&params[0]);
                    Ok(
                        json!({ "number": format!("0x{n:x}"), "timestamp": format!("0x{:x}", 1_000 + 2 * n) }),
                    )
                }
                "eth_getLogs" => {
                    let (f, t) = (q(&params[0]["fromBlock"]), q(&params[0]["toBlock"]));
                    assert!(t - f < LOGS_WINDOW, "range {f}..={t}");
                    self.ranges.lock().expect("lock").push((f, t));
                    Ok(match self.at {
                        Some(b) if (f..=t).contains(&b) => json!([{ "transactionHash": "0xpaid" }]),
                        _ => json!([]),
                    })
                }
                "eth_getTransactionReceipt" => Ok(json!({"status": "0x1", "logs": [{
                    "address": hex0x(&TOKEN),
                    "topics": [hex0x(&authorization_used_topic()), hex0x(&word_addr(&FROM)), hex0x(&[1u8; 32])],
                    "data": "0x",
                }, {
                    "address": hex0x(&TOKEN),
                    "topics": [hex0x(&transfer_topic()), hex0x(&word_addr(&FROM)), hex0x(&word_addr(&REVENUE))],
                    "data": hex0x(&word_u128(2_500)),
                }]})),
                _ => Err(format!("unexpected {method}")),
            };
            Box::pin(async move { r })
        }
    }

    fn pay() -> Payment {
        Payment {
            token: TOKEN,
            revenue: REVENUE,
            from: FROM,
            nonce: [1; 32],
            value: 2_500,
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("rt")
    }

    /// Mutation L4: `first_block_at` at its edges. A time equal to the
    /// head's is the head; a time past it is `head + 1` with no block read
    /// (here every block read fails); before the first block is block 0.
    #[test]
    fn first_block_at_edges() {
        let b = |pruned_below| Blocks {
            head: 100,
            at: None,
            ranges: Mutex::new(Vec::new()),
            pruned_below,
        };
        let rt = rt();
        let at = |c: &Blocks, t| rt.block_on(first_block_at(c, t, 100, 1_200));
        assert_eq!(at(&b(u64::MAX), 1_201), Ok(101));
        assert_eq!(at(&b(u64::MAX), u64::MAX), Ok(101));
        let open = b(0);
        assert_eq!(at(&open, 1_200), Ok(100));
        assert_eq!(at(&open, 1_199), Ok(100));
        assert_eq!(at(&open, 1_198), Ok(99));
        assert_eq!(at(&open, 1_003), Ok(2));
        // The gallop's first probe (block 36) lands exactly on the time.
        assert_eq!(at(&open, 1_072), Ok(36));
        assert_eq!(at(&open, 1_000), Ok(0));
        assert_eq!(at(&open, 0), Ok(0));
    }

    /// Mutation L4: `logged_use` reads a one-block window; `judge` at its
    /// block edges (block n is at time 1,000 + 2n, head 100 at 1,200).
    #[test]
    fn one_block_windows_and_judge_edges() {
        let chain = |at| Blocks {
            head: 100,
            at,
            ranges: Mutex::new(Vec::new()),
            pruned_below: 0,
        };
        let rt = rt();
        // The window is the head block alone.
        let c = chain(Some(100));
        assert_eq!(
            rt.block_on(logged_use(&c, &pay(), 1_200, 1_200)),
            Ok(UseOf::Ours("0xpaid".into()))
        );
        let run = |c: &Blocks, span: Span, grace: u64, cursor: &mut Cursor| {
            let mut budget = MAX_LOG_CALLS;
            rt.block_on(judge(c, &pay(), span, grace, cursor, &mut budget))
        };
        let span = |earliest, valid_before| Span {
            valid_after: 0,
            earliest,
            valid_before,
        };
        let paid = Ok(Verdict::Arrived("0xpaid".into()));
        // Expired with the head itself the first block past expiry: pinned
        // at the head, read in full, not an error.
        let mut cur = Cursor::default();
        assert_eq!(run(&chain(Some(50)), span(1_000, 1_199), 0, &mut cur), paid);
        // Before expiry the read stops at the last block before
        // `validBefore` + 1 (block 50); a use logged after it is not read.
        let mut cur = Cursor::default();
        assert_eq!(
            run(&chain(Some(60)), span(1_000, 1_100), 1_000, &mut cur),
            Ok(Verdict::Open)
        );
        // Before expiry, a one-block span (block 50) is read.
        let mut cur = Cursor::default();
        assert_eq!(
            run(&chain(Some(50)), span(1_100, 1_100), 1_000, &mut cur),
            paid
        );
        // After expiry (pinned at block 51), a one-block span is read.
        let mut cur = Cursor::default();
        assert_eq!(run(&chain(Some(50)), span(1_100, 1_100), 0, &mut cur), paid);
        // While the window still grows, the next pass re-reads its last full
        // window (from block 40, the span's first block, here).
        let mut cur = Cursor::default();
        assert_eq!(
            run(&chain(Some(150)), span(1_080, 5_000), 0, &mut cur),
            Ok(Verdict::Open)
        );
        assert_eq!((cur.lo, cur.next), (Some(40), Some(40)));
        // Used, but no payment of ours anywhere (the use is past the head):
        // after expiry, FINAL_SCANS full reads, one extension back toward
        // `validAfter` and FINAL_SCANS more, then Never; without room to
        // extend (`validAfter` = earliest), Never after FINAL_SCANS reads.
        let gone = chain(Some(1_000));
        let passes = |span: Span| {
            let mut cur = Cursor::default();
            (1..=8)
                .find(|_| run(&gone, span, 0, &mut cur) == Ok(Verdict::Never))
                .expect("a verdict")
        };
        assert_eq!(passes(span(1_100, 1_100)), 2 * usize::from(FINAL_SCANS));
        let no_room = Span {
            valid_after: 1_100,
            earliest: 1_100,
            valid_before: 1_100,
        };
        assert_eq!(passes(no_room), usize::from(FINAL_SCANS));
    }

    /// Only the blocks of the time window are read, oldest first, in
    /// windows a public node accepts (OPS-L2).
    #[test]
    fn only_the_time_window_is_read_in_public_node_sized_ranges() {
        let chain = Blocks {
            head: 1_000_000,
            at: Some(500_123),
            ranges: Mutex::new(Vec::new()),
            pruned_below: 0,
        };
        // Block 500_000 is at 1_001_000; the payment at block 500_123.
        let found = rt().block_on(arrived(&chain, &pay(), 1_001_000, 1_001_000 + 600));
        assert_eq!(found, Ok(true));
        let ranges = chain.ranges.lock().expect("lock").clone();
        assert_eq!(ranges.first().map(|r| r.0), Some(500_000));
        assert!(ranges.len() <= 2, "{ranges:?}");
    }

    /// Not in the window (paid elsewhere in time) and unused: false.
    #[test]
    fn outside_the_window_or_unused_is_no() {
        let chain = Blocks {
            head: 1_000_000,
            at: Some(900_000),
            ranges: Mutex::new(Vec::new()),
            pruned_below: 0,
        };
        assert_eq!(
            rt().block_on(arrived(&chain, &pay(), 1_001_000, 1_001_600)),
            Ok(false)
        );
        let unused = Blocks {
            head: 1_000_000,
            at: None,
            ranges: Mutex::new(Vec::new()),
            pruned_below: 0,
        };
        assert_eq!(
            rt().block_on(arrived(&unused, &pay(), 1_001_000, 1_001_600)),
            Ok(false)
        );
        assert!(
            unused.ranges.lock().expect("lock").is_empty(),
            "no log search when unused"
        );
    }

    /// Only recent blocks are touched: a node that pruned early history
    /// still answers (review 2 smoke run, OPS-L2).
    #[test]
    fn pruned_early_history_is_never_read() {
        let chain = Blocks {
            head: 1_000_000,
            at: Some(999_900),
            ranges: Mutex::new(Vec::new()),
            pruned_below: 900_000,
        };
        let t = 1_000 + 2 * 999_800;
        assert_eq!(rt().block_on(arrived(&chain, &pay(), t, t + 600)), Ok(true));
    }

    /// A window wider than the call cap is an error, never a "no".
    #[test]
    fn a_too_wide_window_is_an_error() {
        let chain = Blocks {
            head: 1_000_000,
            at: Some(1),
            ranges: Mutex::new(Vec::new()),
            pruned_below: 0,
        };
        assert!(rt().block_on(arrived(&chain, &pay(), 0, u64::MAX)).is_err());
    }

    fn log(topics: Vec<String>, data: &str, index: u64) -> Value {
        json!({ "address": hex0x(&TOKEN), "topics": topics, "data": data, "logIndex": format!("0x{index:x}") })
    }

    fn auth_used(nonce: u8, index: u64) -> Value {
        log(
            vec![
                hex0x(&authorization_used_topic()),
                hex0x(&word_addr(&FROM)),
                hex0x(&[nonce; 32]),
            ],
            "0x",
            index,
        )
    }

    fn transfer(to: &[u8; 20], value: u128, index: u64) -> Value {
        log(
            vec![
                hex0x(&transfer_topic()),
                hex0x(&word_addr(&FROM)),
                hex0x(&word_addr(to)),
            ],
            &hex0x(&word_u128(value)),
            index,
        )
    }

    /// OPS-L3-F2: the Transfer must be the log right after the nonce's
    /// AuthorizationUsed; a Transfer elsewhere in the transaction proves
    /// nothing for it.
    #[test]
    fn only_the_adjacent_transfer_counts() {
        let p = Payment {
            nonce: [1; 32],
            ..pay()
        };
        let ok = json!({"status": "0x1", "logs": [auth_used(1, 0), transfer(&REVENUE, 2_500, 1)]});
        assert!(matches!(pairing(&ok, &p), Ok(Some(Use::Ours))));
        // Two authorizations, one Transfer at the end: nonce 1's next log is
        // not its Transfer.
        let shared = json!({"status": "0x1", "logs": [
            auth_used(1, 0), auth_used(2, 1), transfer(&REVENUE, 2_500, 2)
        ]});
        // FiatToken never emits that shape: nonce 1's use did not pay us.
        assert!(matches!(pairing(&shared, &p), Ok(Some(Use::Elsewhere))));
        let p2 = Payment {
            nonce: [2; 32],
            ..pay()
        };
        assert!(matches!(pairing(&shared, &p2), Ok(Some(Use::Ours))));
        // Sent elsewhere, or another amount: spent, not ours.
        let other =
            json!({"status": "0x1", "logs": [auth_used(1, 0), transfer(&[0xee; 20], 2_500, 1)]});
        assert!(matches!(pairing(&other, &p), Ok(Some(Use::Elsewhere))));
        let less =
            json!({"status": "0x1", "logs": [auth_used(1, 0), transfer(&REVENUE, 2_499, 1)]});
        assert!(matches!(pairing(&less, &p), Ok(Some(Use::Elsewhere))));
        // Log indexes that are not adjacent, or no log after it: not ours.
        let gap = json!({"status": "0x1", "logs": [auth_used(1, 0), transfer(&REVENUE, 2_500, 5)]});
        assert!(matches!(pairing(&gap, &p), Ok(Some(Use::Elsewhere))));
        let last = json!({"status": "0x1", "logs": [auth_used(1, 0)]});
        assert!(matches!(pairing(&last, &p), Ok(Some(Use::Elsewhere))));
        // Mutation L4: the next log must be the token's own Transfer from the
        // payer: another contract's Transfer-shaped log, another event of the
        // token (an Approval with the same topics and data), a Transfer from
        // someone else or one with a fourth topic proves nothing.
        let mut foreign = transfer(&REVENUE, 2_500, 1);
        foreign["address"] = json!(hex0x(&[0xcc; 20]));
        let mut approval = transfer(&REVENUE, 2_500, 1);
        approval["topics"][0] = json!(hex0x(&keccak256(b"Approval(address,address,uint256)").0));
        let mut not_payer = transfer(&REVENUE, 2_500, 1);
        not_payer["topics"][1] = json!(hex0x(&word_addr(&[0x12; 20])));
        let mut four = transfer(&REVENUE, 2_500, 1);
        four["topics"]
            .as_array_mut()
            .expect("topics")
            .push(json!(hex0x(&[0u8; 32])));
        for next in [foreign, approval, not_payer, four] {
            let r = json!({"status": "0x1", "logs": [auth_used(1, 0), next.clone()]});
            assert!(
                matches!(pairing(&r, &p), Ok(Some(Use::Elsewhere))),
                "{next}"
            );
        }
        // A failed transaction proves nothing.
        let failed =
            json!({"status": "0x0", "logs": [auth_used(1, 0), transfer(&REVENUE, 2_500, 1)]});
        assert!(matches!(pairing(&failed, &p), Ok(None)));
    }
}
